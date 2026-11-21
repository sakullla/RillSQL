//! 连接级事务状态机：快照、效果与状态转换。（ADR-2）
//!
//! 模型不依赖驱动、GPUI 或具体方言实现：方言只提供能力表（[`TransactionCapabilities`]），
//! 状态只在数据库确认执行结果之后推进。手写 SQL 只能先分类成 [`ControlStatement`]（意图），
//! 分类本身不改变任何状态；没有活动事务时的提交/回滚、控制结果无法确认以及无法识别的效果
//! 都不会被报告为成功。
//!
//! 代次与未知信息的规则：
//!
//! - `generation` 在事务开始、结束（提交/回滚/隐式提交/被新的开始语句取代）、首次进入未知、
//!   此前存在事务时的重连，以及探针确认事务存在性变化时递增；中止状态、保存点、设置与层数变化不改变它。
//! - `unresolved` 只在两种情况消失：直接确认了当前这个未知事务的结局，或
//!   [`TransactionMachine::resolve_unknown`] 给出独立核对结论。新事务的开始/提交/回滚不会清除旧记录（R10）。
//! - `state == Unknown` 时 `unresolved` 必定存在，且它的 `state` 是失去确认前的最后状态。

use std::fmt;

use serde::{Deserialize, Serialize};

use super::dialect::{AbortedRecovery, DuplicateBegin, ImplicitCommitSupport, SettingChange, SettingSupport, TransactionCapabilities};

/// 连接级事务状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransactionState {
    /// 没有活动事务（自动提交）。
    Idle,
    /// 活动事务，可以继续执行语句。
    Active,
    /// 事务被数据库置为中止状态：只能回滚，或在方言允许时回滚到保存点。
    Aborted,
    /// 无法确认数据库中的事务状态；不得据此推断 Idle 或 Active。
    Unknown,
}

impl TransactionState {
    /// 普通语句是否可以继续下发。`Aborted` 只允许数据库认可的恢复操作，`Unknown` 不允许推测。
    pub fn allows_statements(self) -> bool {
        matches!(self, Self::Idle | Self::Active)
    }

    /// 是否必须先用回滚或回滚到保存点恢复。
    pub fn needs_recovery(self) -> bool {
        matches!(self, Self::Aborted)
    }
}

/// 一次语句或控制操作对事务的效果，供查询语句、网格和结构回执统一报告。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransactionEffect {
    /// 事务由本次操作开始。
    Began,
    /// 事务仍然活动，没有结束。
    StillActive,
    /// 数据库确认事务已提交。
    Committed,
    /// 数据库确认事务已回滚。
    RolledBack,
    /// 事务被数据库置为中止状态。
    Aborted,
    /// 数据库确认语句导致隐式提交（MySQL/MariaDB）。
    ImplicitCommit,
    /// 无法确认实际结果。
    Unknown,
    /// 语句成功，但事务状态没有变化（例如无活动事务时设置隔离级别）。
    Unchanged,
}

impl TransactionEffect {
    pub fn is_unknown(self) -> bool {
        matches!(self, Self::Unknown)
    }

    /// 该效果表示事务已经结束（含隐式提交）。
    pub fn ends_transaction(self) -> bool {
        matches!(self, Self::Committed | Self::RolledBack | Self::ImplicitCommit)
    }
}

/// 无法确认事务结果的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnknownReason {
    /// 事务期间连接断开，旧事务的最终结果无法确认。
    ConnectionLost,
    /// 提交、回滚或恢复等控制语句的结果没有返回。
    ControlUnconfirmed,
    /// 语句或控制发生在过程、动态 SQL 等无法静态确认的位置，实际效果无法识别。
    UnrecognizedEffect,
    /// 状态观测探针本身无法给出结论，或探针发现事务在没有确认结果的情况下结束。
    ObservationFailed,
}

/// 最近一个结果无法确认的事务。`state == Unknown` 时它就是当前事务；
/// 重连或探针确认当前状态后仍保留为旧事务信息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnknownTransaction {
    pub reason: UnknownReason,
    /// 失去确认时的事务代次。
    pub generation: u64,
    /// 失去确认前的最后状态；`Idle` 表示无法确认是否已经开始了事务。
    pub state: TransactionState,
}

/// 独立核对（用户确认或另一条连接上的查询结果）给出的旧事务结局。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnknownResolution {
    Committed,
    RolledBack,
}

/// 方言探针观测到的真实事务状态。探针与模型不一致时以数据库观测为准。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransactionProbe {
    /// 探针确认没有活动事务。
    Idle,
    /// 探针确认事务活动；`depth` 是数据库报告的层数（T-SQL 的 `@@TRANCOUNT`），不分层的方言为 1。
    Active { depth: u32 },
    /// 探针确认事务已中止。
    Aborted,
}

/// 保存点名称：非空、去掉两侧空白，且不含控制字符。引用与转义由方言负责。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SavepointName(String);

impl SavepointName {
    pub fn parse(raw: &str) -> Result<Self, TransactionError> {
        let name = raw.trim();
        if name.is_empty() || name.chars().any(char::is_control) {
            return Err(TransactionError::InvalidSavepointName(raw.to_owned()));
        }
        Ok(Self(name.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 保存点边界比较。未引用的标识符在各方言中大小写不敏感（PostgreSQL 折叠小写，
    /// MySQL/SQLite/SQL Server 本身不区分），因此同一个保存点不因大小写不同而被当成未知。
    pub fn matches(&self, other: &Self) -> bool {
        self.0.eq_ignore_ascii_case(&other.0)
    }
}

impl fmt::Display for SavepointName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// 数据库确认生效的事务特性。`None` 表示服务器/方言默认，没有下发过设置。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransactionSettings {
    pub isolation: Option<String>,
    pub mode: Option<String>,
}

/// 同连接所有业务标签共享的事务快照。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransactionSnapshot {
    /// 连接 id（与 `connections` 中的条目一致）。
    pub connection: String,
    pub state: TransactionState,
    /// 事务代次：事务开始/结束（含被新的开始语句取代）、首次进入未知、此前存在事务时的重连，
    /// 以及探针确认事务存在性变化时递增；中止、保存点、设置与层数变化不改变它。
    pub generation: u64,
    /// 快照修订：任何可见变化都递增，供界面判断是否需要刷新。
    pub revision: u64,
    /// 数据库内部事务层数（T-SQL 的 `@@TRANCOUNT`）。`Idle` 为 0；界面不把它展示为嵌套事务，
    /// 只用于判断一次提交是否真的结束事务。
    pub depth: u32,
    pub settings: TransactionSettings,
    /// 按登记顺序排列的保存点；同名保存点只保留最新边界。
    pub savepoints: Vec<SavepointName>,
    pub last_effect: TransactionEffect,
    /// 最近一个结果无法确认的事务；`state == Unknown` 时它就是当前事务。
    pub unresolved: Option<UnknownTransaction>,
}

/// 事务模型拒绝的操作。错误只说明“状态没有被推进”，不代表数据库一定没有变化。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransactionError {
    #[error("该方言不支持显式事务控制")]
    TransactionsUnsupported,
    #[error("当前没有活动事务")]
    NoActiveTransaction,
    #[error("事务已经活动；需要部分回滚时请使用保存点")]
    AlreadyActive,
    #[error("事务已中止，只能回滚或回滚到保存点")]
    TransactionAborted,
    #[error("该方言的中止事务只能整体回滚")]
    RecoveryRequiresFullRollback,
    #[error("事务结果未知，不能据此推进状态")]
    UnknownOutcome,
    #[error("数据库没有确认该控制语句，事务状态保持不变")]
    ControlRejected,
    #[error("数据库没有确认该操作的结果")]
    ControlUnconfirmed,
    #[error("保存点名称不合法：{0}")]
    InvalidSavepointName(String),
    #[error("保存点 {0} 不在当前事务中")]
    UnknownSavepoint(String),
    #[error("该方言不支持保存点")]
    SavepointsUnsupported,
    #[error("该方言不支持释放保存点")]
    ReleaseSavepointUnsupported,
    #[error("该方言不支持链式事务（AND CHAIN）")]
    ChainedTransactionUnsupported,
    #[error("该方言不支持隔离级别 {0}")]
    IsolationUnsupported(String),
    #[error("该方言不支持事务模式 {0}")]
    ModeUnsupported(String),
    #[error("当前事务状态不允许改变{0}")]
    SettingLocked(&'static str),
    #[error("该方言不会隐式提交，不能报告隐式提交结果")]
    ImplicitCommitUnsupported,
}

/// 事务控制语句（来自按钮或手写 SQL）。它只是意图，本身不改变状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlStatement {
    Begin { mode: Option<String> },
    Commit { chain: bool },
    Rollback { chain: bool },
    Savepoint(SavepointName),
    RollbackToSavepoint(SavepointName),
    ReleaseSavepoint(SavepointName),
    /// `None` 表示恢复服务器默认隔离级别。
    SetIsolation { level: Option<String> },
    /// `None` 表示恢复方言默认事务模式。
    SetMode { mode: Option<String> },
}

/// 数据库对一次控制语句的确认结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlOutcome {
    /// 数据库确认语句成功。
    Confirmed,
    /// 数据库把提交实际执行成回滚（PostgreSQL 中止事务的 `COMMIT`）。
    RolledBack,
    /// 数据库拒绝语句，外层事务状态保持不变。
    Rejected,
    /// 语句失败并把事务置为中止状态。
    Aborted,
    /// 传输中断或结果没有返回：事务结果未知。
    Unconfirmed(UnknownReason),
    /// 语句在过程、动态 SQL 等位置执行，实际效果无法识别：按未知处理。
    Unrecognized,
}

/// 普通语句（查询、DML、DDL）的执行结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatementOutcome {
    Succeeded,
    Failed(StatementFailure),
    /// 连接断开或结果没有返回。
    Unconfirmed(UnknownReason),
}

/// 语句失败后事务的可用性，由方言按错误判定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatementFailure {
    /// 语句被拒绝，事务仍可继续使用（含取消后事务仍可用的情况）。
    TransactionUsable,
    /// 语句失败并把事务置为中止状态（PostgreSQL 类），只能回滚或回滚到保存点。
    TransactionAborted,
    /// 连接在语句期间断开。
    ConnectionLost,
}

/// 连接级事务状态机：持有当前快照，只按数据库确认的结果推进。
#[derive(Debug, Clone)]
pub struct TransactionMachine {
    capabilities: TransactionCapabilities,
    snapshot: TransactionSnapshot,
}

impl TransactionMachine {
    pub fn new(connection: impl Into<String>, capabilities: TransactionCapabilities) -> Self {
        Self {
            capabilities,
            snapshot: TransactionSnapshot {
                connection: connection.into(),
                state: TransactionState::Idle,
                generation: 0,
                revision: 0,
                depth: 0,
                settings: TransactionSettings::default(),
                savepoints: Vec::new(),
                last_effect: TransactionEffect::Unchanged,
                unresolved: None,
            },
        }
    }

    pub fn snapshot(&self) -> &TransactionSnapshot {
        &self.snapshot
    }

    pub fn state(&self) -> TransactionState {
        self.snapshot.state
    }

    pub fn capabilities(&self) -> TransactionCapabilities {
        self.capabilities
    }

    /// 执行前的预检：只判断“这个操作是否可以发给数据库”，不改变任何状态。
    ///
    /// 与 [`Self::acknowledge`] 分开是因为数据库可能已经执行了语句：预检拒绝的操作不应下发，
    /// 而已经确认的结果必须按真实效果登记（例如 SQL Server 的嵌套 `BEGIN`）。
    pub fn precheck(&self, statement: &ControlStatement) -> Result<(), TransactionError> {
        if !self.capabilities.explicit_control {
            return Err(TransactionError::TransactionsUnsupported);
        }
        match statement {
            ControlStatement::Begin { mode } => {
                if let Some(mode) = mode {
                    self.require_mode(mode)?;
                }
                match self.snapshot.state {
                    TransactionState::Idle | TransactionState::Unknown => Ok(()),
                    TransactionState::Active => match self.capabilities.duplicate_begin {
                        // 数据库会隐式提交旧事务并开始新事务：允许，但结果必须报告隐式提交。
                        DuplicateBegin::EndsPrevious => Ok(()),
                        // 其余方言的重复开始只会被拒绝、被忽略或增加层数，界面一律先引导到保存点。
                        DuplicateBegin::Nests | DuplicateBegin::Ignored | DuplicateBegin::Rejected => Err(TransactionError::AlreadyActive),
                    },
                    TransactionState::Aborted => Err(TransactionError::TransactionAborted),
                }
            }
            ControlStatement::Commit { chain } => {
                if *chain && !self.capabilities.chained_transactions {
                    return Err(TransactionError::ChainedTransactionUnsupported);
                }
                match self.snapshot.state {
                    TransactionState::Idle => Err(TransactionError::NoActiveTransaction),
                    TransactionState::Aborted => Err(TransactionError::TransactionAborted),
                    // 提交是未知结果的明确核对路径之一。
                    TransactionState::Active | TransactionState::Unknown => Ok(()),
                }
            }
            ControlStatement::Rollback { chain } => {
                if *chain && !self.capabilities.chained_transactions {
                    return Err(TransactionError::ChainedTransactionUnsupported);
                }
                match self.snapshot.state {
                    TransactionState::Idle => Err(TransactionError::NoActiveTransaction),
                    TransactionState::Active | TransactionState::Aborted | TransactionState::Unknown => Ok(()),
                }
            }
            ControlStatement::Savepoint(_) => {
                if !self.capabilities.savepoints {
                    return Err(TransactionError::SavepointsUnsupported);
                }
                match self.snapshot.state {
                    TransactionState::Active => Ok(()),
                    TransactionState::Idle => Err(TransactionError::NoActiveTransaction),
                    TransactionState::Aborted => Err(TransactionError::TransactionAborted),
                    TransactionState::Unknown => Err(TransactionError::UnknownOutcome),
                }
            }
            ControlStatement::RollbackToSavepoint(name) => {
                if !self.capabilities.savepoints {
                    return Err(TransactionError::SavepointsUnsupported);
                }
                match self.snapshot.state {
                    TransactionState::Active => self.known_savepoint(name),
                    TransactionState::Aborted => match self.capabilities.aborted {
                        AbortedRecovery::SavepointRecovery => self.known_savepoint(name),
                        AbortedRecovery::NotApplicable | AbortedRecovery::RollbackOnly => Err(TransactionError::RecoveryRequiresFullRollback),
                    },
                    TransactionState::Idle => Err(TransactionError::NoActiveTransaction),
                    TransactionState::Unknown => Err(TransactionError::UnknownOutcome),
                }
            }
            ControlStatement::ReleaseSavepoint(name) => {
                if !self.capabilities.savepoints {
                    return Err(TransactionError::SavepointsUnsupported);
                }
                if !self.capabilities.release_savepoint {
                    return Err(TransactionError::ReleaseSavepointUnsupported);
                }
                match self.snapshot.state {
                    TransactionState::Active => self.known_savepoint(name),
                    TransactionState::Idle => Err(TransactionError::NoActiveTransaction),
                    TransactionState::Aborted => Err(TransactionError::TransactionAborted),
                    TransactionState::Unknown => Err(TransactionError::UnknownOutcome),
                }
            }
            ControlStatement::SetIsolation { level } => self.check_setting(SettingKind::Isolation, level.as_deref()),
            ControlStatement::SetMode { mode } => self.check_setting(SettingKind::Mode, mode.as_deref()),
        }
    }

    /// 确认一条控制语句的执行结果并推进状态。按钮路径与手写 SQL 路径共用这一个入口。
    ///
    /// 无法确认的结果只会进入 `Unknown`；被数据库拒绝的结果返回错误并保持原状态。
    pub fn acknowledge(&mut self, statement: &ControlStatement, outcome: ControlOutcome) -> Result<TransactionEffect, TransactionError> {
        if !self.capabilities.explicit_control {
            return Err(TransactionError::TransactionsUnsupported);
        }
        match statement {
            ControlStatement::Begin { mode } => self.begin(mode.as_deref(), outcome),
            ControlStatement::Commit { chain } => self.finish(true, *chain, outcome),
            ControlStatement::Rollback { chain } => self.finish(false, *chain, outcome),
            ControlStatement::Savepoint(name) => self.create_savepoint(name, outcome),
            ControlStatement::RollbackToSavepoint(name) => self.rollback_to_savepoint(name, outcome),
            ControlStatement::ReleaseSavepoint(name) => self.release_savepoint(name, outcome),
            ControlStatement::SetIsolation { level } => self.set_setting(SettingKind::Isolation, level.as_deref(), outcome),
            ControlStatement::SetMode { mode } => self.set_setting(SettingKind::Mode, mode.as_deref(), outcome),
        }
    }

    /// 报告一条普通语句的结果。失败不会自动回滚事务，也不会被当成成功。
    pub fn observe_statement(&mut self, outcome: StatementOutcome) -> TransactionEffect {
        match self.snapshot.state {
            // 自动提交：语句结果不构成事务，失败也不产生需要确认的事务。
            TransactionState::Idle => TransactionEffect::Unchanged,
            TransactionState::Active => match outcome {
                StatementOutcome::Succeeded | StatementOutcome::Failed(StatementFailure::TransactionUsable) => {
                    self.record(TransactionEffect::StillActive);
                    TransactionEffect::StillActive
                }
                StatementOutcome::Failed(StatementFailure::TransactionAborted) => self.abort_in_place(),
                StatementOutcome::Failed(StatementFailure::ConnectionLost) => {
                    self.lose_confirmation(UnknownReason::ConnectionLost);
                    TransactionEffect::Unknown
                }
                StatementOutcome::Unconfirmed(reason) => {
                    self.lose_confirmation(reason);
                    TransactionEffect::Unknown
                }
            },
            TransactionState::Aborted => match outcome {
                StatementOutcome::Succeeded => {
                    // 中止事务里普通语句成功与数据库规则矛盾：不据此认为事务已恢复可用。
                    self.lose_confirmation(UnknownReason::UnrecognizedEffect);
                    TransactionEffect::Unknown
                }
                StatementOutcome::Failed(StatementFailure::TransactionUsable | StatementFailure::TransactionAborted) => TransactionEffect::Aborted,
                StatementOutcome::Failed(StatementFailure::ConnectionLost) => {
                    self.lose_confirmation(UnknownReason::ConnectionLost);
                    TransactionEffect::Unknown
                }
                StatementOutcome::Unconfirmed(reason) => {
                    self.lose_confirmation(reason);
                    TransactionEffect::Unknown
                }
            },
            TransactionState::Unknown => match outcome {
                StatementOutcome::Unconfirmed(reason) => {
                    self.lose_confirmation(reason);
                    TransactionEffect::Unknown
                }
                _ => TransactionEffect::Unknown,
            },
        }
    }

    /// 报告本连接上的隐式提交（MySQL/MariaDB）。只有方言确认该语句会隐式提交时才调用。
    pub fn observe_implicit_commit(&mut self) -> Result<TransactionEffect, TransactionError> {
        if self.capabilities.implicit_commit == ImplicitCommitSupport::NotApplicable {
            return Err(TransactionError::ImplicitCommitUnsupported);
        }
        if self.snapshot.state == TransactionState::Idle {
            // 自动提交下没有可被隐式提交的事务，不报告“隐式提交”。
            self.record(TransactionEffect::Unchanged);
            return Ok(TransactionEffect::Unchanged);
        }
        self.confirm_current_unknown();
        self.end_transaction(TransactionEffect::ImplicitCommit, false);
        Ok(TransactionEffect::ImplicitCommit)
    }

    /// 方言探针观测到的真实状态。探针与模型不一致时以数据库为准；事务在没有确认结果的情况下消失时，
    /// 记录为未知而不是猜测提交或回滚。
    pub fn observe_probe(&mut self, probe: TransactionProbe) -> TransactionEffect {
        match (probe, self.snapshot.state) {
            (TransactionProbe::Idle, TransactionState::Idle) => {
                self.record(TransactionEffect::Unchanged);
                TransactionEffect::Unchanged
            }
            // 事务在没有确认结果的情况下结束：提交还是回滚无法从“没有活动事务”推出。
            (TransactionProbe::Idle, previous) => {
                if previous != TransactionState::Unknown {
                    self.snapshot.unresolved = Some(UnknownTransaction { reason: UnknownReason::ObservationFailed, generation: self.snapshot.generation, state: previous });
                }
                self.snapshot.state = TransactionState::Idle;
                self.snapshot.depth = 0;
                self.snapshot.savepoints.clear();
                self.snapshot.generation = self.snapshot.generation.saturating_add(1);
                self.record(TransactionEffect::Unknown);
                TransactionEffect::Unknown
            }
            (TransactionProbe::Active { depth }, state) => {
                let depth = depth.max(1);
                let was_active = matches!(state, TransactionState::Active | TransactionState::Aborted);
                self.snapshot.state = TransactionState::Active;
                self.snapshot.depth = depth;
                if !was_active {
                    self.snapshot.generation = self.snapshot.generation.saturating_add(1);
                    self.snapshot.savepoints.clear();
                }
                self.record(TransactionEffect::StillActive);
                TransactionEffect::StillActive
            }
            (TransactionProbe::Aborted, TransactionState::Idle) => {
                // 没有事务却报告中止：观测自相矛盾，不能据此推出可用状态。
                self.lose_confirmation(UnknownReason::ObservationFailed);
                TransactionEffect::Unknown
            }
            (TransactionProbe::Aborted, _) => {
                self.snapshot.state = TransactionState::Aborted;
                self.record(TransactionEffect::Aborted);
                TransactionEffect::Aborted
            }
        }
    }

    /// 独立核对确认旧事务的最终结果：清除未知记录。当前会话仍在未知时，被核对的事务已经结束，
    /// `state` 回到 `Idle`。不重放任何写入或事务控制语句。
    pub fn resolve_unknown(&mut self, resolution: UnknownResolution) -> Option<UnknownTransaction> {
        let resolved = self.snapshot.unresolved.take()?;
        if self.snapshot.state == TransactionState::Unknown {
            self.snapshot.state = TransactionState::Idle;
            self.snapshot.depth = 0;
            self.snapshot.savepoints.clear();
            self.snapshot.generation = self.snapshot.generation.saturating_add(1);
        }
        self.snapshot.last_effect = match resolution {
            UnknownResolution::Committed => TransactionEffect::Committed,
            UnknownResolution::RolledBack => TransactionEffect::RolledBack,
        };
        self.touch();
        Some(resolved)
    }

    /// 重新连接会话：新会话从 `Idle` 开始，设置回到服务器默认，旧事务结果保留为未知信息。
    /// 该操作不重放任何写入或事务控制语句。
    pub fn reconnect(&mut self, reason: UnknownReason) -> TransactionEffect {
        let previous = self.snapshot.state;
        let effect = if previous == TransactionState::Idle { TransactionEffect::Unchanged } else { TransactionEffect::Unknown };
        if previous != TransactionState::Idle && self.snapshot.unresolved.is_none() {
            self.snapshot.unresolved = Some(UnknownTransaction { reason, generation: self.snapshot.generation, state: previous });
        }
        if previous != TransactionState::Idle {
            self.snapshot.generation = self.snapshot.generation.saturating_add(1);
        }
        self.snapshot.state = TransactionState::Idle;
        self.snapshot.depth = 0;
        self.snapshot.savepoints.clear();
        self.snapshot.settings = TransactionSettings::default();
        self.record(effect);
        effect
    }

    fn begin(&mut self, mode: Option<&str>, outcome: ControlOutcome) -> Result<TransactionEffect, TransactionError> {
        if let Some(mode) = mode {
            self.require_mode(mode)?;
        }
        match outcome {
            ControlOutcome::Unconfirmed(reason) => {
                self.lose_confirmation(reason);
                Ok(TransactionEffect::Unknown)
            }
            ControlOutcome::Unrecognized => {
                self.lose_confirmation(UnknownReason::UnrecognizedEffect);
                Ok(TransactionEffect::Unknown)
            }
            ControlOutcome::Confirmed => match self.snapshot.state {
                TransactionState::Active => self.restarted(mode),
                // 中止事务里确认开始与数据库规则矛盾：不据此认为事务已恢复可用。
                TransactionState::Aborted => {
                    self.lose_confirmation(UnknownReason::UnrecognizedEffect);
                    Ok(TransactionEffect::Unknown)
                }
                TransactionState::Idle | TransactionState::Unknown => {
                    self.start(mode);
                    Ok(TransactionEffect::Began)
                }
            },
            ControlOutcome::Rejected => match self.snapshot.state {
                TransactionState::Active => Err(TransactionError::AlreadyActive),
                TransactionState::Aborted => Err(TransactionError::TransactionAborted),
                TransactionState::Idle | TransactionState::Unknown => Err(self.rejection()),
            },
            ControlOutcome::Aborted => match self.snapshot.state {
                TransactionState::Active => Ok(self.abort_in_place()),
                TransactionState::Aborted => Err(TransactionError::TransactionAborted),
                TransactionState::Unknown => Ok(TransactionEffect::Unknown),
                TransactionState::Idle => Err(TransactionError::ControlRejected),
            },
            ControlOutcome::RolledBack => Err(TransactionError::ControlRejected),
        }
    }

    /// 数据库已经确认了重复的开始语句：按方言的真实效果登记，不假装旧事务仍然原样活动。
    fn restarted(&mut self, mode: Option<&str>) -> Result<TransactionEffect, TransactionError> {
        match self.capabilities.duplicate_begin {
            DuplicateBegin::EndsPrevious => {
                // 旧事务被隐式提交，新事务立即开始；两者合起来只推进一次代次。
                self.snapshot.state = TransactionState::Active;
                self.snapshot.depth = 1;
                self.snapshot.savepoints.clear();
                if let Some(mode) = mode {
                    self.snapshot.settings.mode = Some(mode.to_owned());
                }
                self.snapshot.generation = self.snapshot.generation.saturating_add(1);
                self.snapshot.last_effect = TransactionEffect::ImplicitCommit;
                self.touch();
                Ok(TransactionEffect::ImplicitCommit)
            }
            DuplicateBegin::Nests => {
                // T-SQL：@@TRANCOUNT 加一，一次 COMMIT 不会结束外层事务。
                self.snapshot.depth = self.snapshot.depth.max(1).saturating_add(1);
                self.record(TransactionEffect::StillActive);
                Ok(TransactionEffect::StillActive)
            }
            DuplicateBegin::Ignored => {
                // PostgreSQL：警告并忽略，事务身份与层数都不变。
                self.record(TransactionEffect::StillActive);
                Ok(TransactionEffect::StillActive)
            }
            DuplicateBegin::Rejected => {
                // 方言应当拒绝重复开始；数据库却确认成功：无法解释，按未知处理。
                self.lose_confirmation(UnknownReason::UnrecognizedEffect);
                Ok(TransactionEffect::Unknown)
            }
        }
    }

    fn finish(&mut self, commit: bool, chain: bool, outcome: ControlOutcome) -> Result<TransactionEffect, TransactionError> {
        if chain && !self.capabilities.chained_transactions {
            return Err(TransactionError::ChainedTransactionUnsupported);
        }
        match outcome {
            ControlOutcome::Unconfirmed(reason) => {
                self.lose_confirmation(reason);
                Ok(TransactionEffect::Unknown)
            }
            ControlOutcome::Unrecognized => {
                self.lose_confirmation(UnknownReason::UnrecognizedEffect);
                Ok(TransactionEffect::Unknown)
            }
            ControlOutcome::Rejected => Err(self.rejection()),
            ControlOutcome::Aborted => match self.snapshot.state {
                TransactionState::Active => Ok(self.abort_in_place()),
                TransactionState::Aborted => Err(TransactionError::TransactionAborted),
                TransactionState::Unknown => Ok(TransactionEffect::Unknown),
                TransactionState::Idle => Err(TransactionError::NoActiveTransaction),
            },
            ControlOutcome::Confirmed | ControlOutcome::RolledBack => {
                let committed = commit && outcome == ControlOutcome::Confirmed;
                let effect = if committed { TransactionEffect::Committed } else { TransactionEffect::RolledBack };
                match self.snapshot.state {
                    // 没有活动事务时提交/回滚不得伪造成功结果。
                    TransactionState::Idle => Err(TransactionError::NoActiveTransaction),
                    // 中止事务里的成功提交与数据库规则矛盾：只能按未知处理。
                    TransactionState::Aborted if committed => {
                        self.lose_confirmation(UnknownReason::UnrecognizedEffect);
                        Ok(TransactionEffect::Unknown)
                    }
                    // 嵌套事务里的一次 COMMIT 只减少层数，外层事务仍然活动。
                    TransactionState::Active if committed && self.snapshot.depth > 1 => {
                        self.snapshot.depth -= 1;
                        self.record(TransactionEffect::StillActive);
                        Ok(TransactionEffect::StillActive)
                    }
                    TransactionState::Active | TransactionState::Aborted | TransactionState::Unknown => {
                        // 未知状态下直接确认了当前事务的结局，这是观测而不是推测。
                        self.confirm_current_unknown();
                        self.end_transaction(effect, chain);
                        Ok(effect)
                    }
                }
            }
        }
    }

    fn create_savepoint(&mut self, name: &SavepointName, outcome: ControlOutcome) -> Result<TransactionEffect, TransactionError> {
        if !self.capabilities.savepoints {
            return Err(TransactionError::SavepointsUnsupported);
        }
        match outcome {
            ControlOutcome::Unconfirmed(reason) => {
                self.lose_confirmation(reason);
                Ok(TransactionEffect::Unknown)
            }
            ControlOutcome::Unrecognized => {
                self.lose_confirmation(UnknownReason::UnrecognizedEffect);
                Ok(TransactionEffect::Unknown)
            }
            ControlOutcome::Confirmed => match self.snapshot.state {
                TransactionState::Active => {
                    self.register_savepoint(name);
                    Ok(TransactionEffect::StillActive)
                }
                TransactionState::Idle => Err(TransactionError::NoActiveTransaction),
                TransactionState::Aborted => Err(TransactionError::TransactionAborted),
                TransactionState::Unknown => Ok(TransactionEffect::Unknown),
            },
            ControlOutcome::Aborted => match self.snapshot.state {
                TransactionState::Active => Ok(self.abort_in_place()),
                TransactionState::Aborted => Err(TransactionError::TransactionAborted),
                TransactionState::Unknown => Ok(TransactionEffect::Unknown),
                TransactionState::Idle => Err(TransactionError::NoActiveTransaction),
            },
            ControlOutcome::Rejected => Err(self.rejection()),
            ControlOutcome::RolledBack => Err(TransactionError::ControlRejected),
        }
    }

    fn rollback_to_savepoint(&mut self, name: &SavepointName, outcome: ControlOutcome) -> Result<TransactionEffect, TransactionError> {
        if !self.capabilities.savepoints {
            return Err(TransactionError::SavepointsUnsupported);
        }
        match outcome {
            ControlOutcome::Unconfirmed(reason) => {
                self.lose_confirmation(reason);
                Ok(TransactionEffect::Unknown)
            }
            ControlOutcome::Unrecognized => {
                self.lose_confirmation(UnknownReason::UnrecognizedEffect);
                Ok(TransactionEffect::Unknown)
            }
            ControlOutcome::Confirmed => match self.snapshot.state {
                TransactionState::Active | TransactionState::Aborted => match self.savepoint_index(name) {
                    Some(index) => {
                        // 回滚到保存点保留该保存点本身及其之前的状态，外层事务继续活动。
                        self.snapshot.savepoints.truncate(index + 1);
                        self.snapshot.state = TransactionState::Active;
                        self.record(TransactionEffect::StillActive);
                        Ok(TransactionEffect::StillActive)
                    }
                    // 数据库确认了一个没有登记的保存点：保存点位置不可知。
                    None => {
                        self.lose_confirmation(UnknownReason::UnrecognizedEffect);
                        Ok(TransactionEffect::Unknown)
                    }
                },
                TransactionState::Idle => Err(TransactionError::NoActiveTransaction),
                TransactionState::Unknown => Ok(TransactionEffect::Unknown),
            },
            ControlOutcome::Aborted => match self.snapshot.state {
                TransactionState::Active => Ok(self.abort_in_place()),
                TransactionState::Aborted => Err(TransactionError::TransactionAborted),
                TransactionState::Unknown => Ok(TransactionEffect::Unknown),
                TransactionState::Idle => Err(TransactionError::NoActiveTransaction),
            },
            ControlOutcome::Rejected => {
                if self.savepoint_index(name).is_none() && self.snapshot.state != TransactionState::Idle {
                    Err(TransactionError::UnknownSavepoint(name.to_string()))
                } else {
                    Err(self.rejection())
                }
            }
            ControlOutcome::RolledBack => Err(TransactionError::ControlRejected),
        }
    }

    fn release_savepoint(&mut self, name: &SavepointName, outcome: ControlOutcome) -> Result<TransactionEffect, TransactionError> {
        if !self.capabilities.savepoints {
            return Err(TransactionError::SavepointsUnsupported);
        }
        if !self.capabilities.release_savepoint {
            // 不支持释放的方言不提供伪造成功的入口。
            return Err(TransactionError::ReleaseSavepointUnsupported);
        }
        match outcome {
            ControlOutcome::Unconfirmed(reason) => {
                self.lose_confirmation(reason);
                Ok(TransactionEffect::Unknown)
            }
            ControlOutcome::Unrecognized => {
                self.lose_confirmation(UnknownReason::UnrecognizedEffect);
                Ok(TransactionEffect::Unknown)
            }
            ControlOutcome::Confirmed => match self.snapshot.state {
                TransactionState::Active => match self.savepoint_index(name) {
                    Some(index) => {
                        // 释放该保存点及其之后建立的保存点，外层事务继续活动。
                        self.snapshot.savepoints.truncate(index);
                        self.record(TransactionEffect::StillActive);
                        Ok(TransactionEffect::StillActive)
                    }
                    None => {
                        self.lose_confirmation(UnknownReason::UnrecognizedEffect);
                        Ok(TransactionEffect::Unknown)
                    }
                },
                TransactionState::Idle => Err(TransactionError::NoActiveTransaction),
                TransactionState::Aborted => Err(TransactionError::TransactionAborted),
                TransactionState::Unknown => Ok(TransactionEffect::Unknown),
            },
            ControlOutcome::Aborted => match self.snapshot.state {
                TransactionState::Active => Ok(self.abort_in_place()),
                TransactionState::Aborted => Err(TransactionError::TransactionAborted),
                TransactionState::Unknown => Ok(TransactionEffect::Unknown),
                TransactionState::Idle => Err(TransactionError::NoActiveTransaction),
            },
            ControlOutcome::Rejected => {
                if self.savepoint_index(name).is_none() && self.snapshot.state != TransactionState::Idle {
                    Err(TransactionError::UnknownSavepoint(name.to_string()))
                } else {
                    Err(self.rejection())
                }
            }
            ControlOutcome::RolledBack => Err(TransactionError::ControlRejected),
        }
    }

    fn set_setting(&mut self, kind: SettingKind, value: Option<&str>, outcome: ControlOutcome) -> Result<TransactionEffect, TransactionError> {
        self.check_setting(kind, value)?;
        match outcome {
            ControlOutcome::Confirmed => {
                let effect = if self.snapshot.state == TransactionState::Active { TransactionEffect::StillActive } else { TransactionEffect::Unchanged };
                let changed = {
                    let slot = match kind {
                        SettingKind::Isolation => &mut self.snapshot.settings.isolation,
                        SettingKind::Mode => &mut self.snapshot.settings.mode,
                    };
                    let changed = slot.as_deref() != value;
                    *slot = value.map(str::to_owned);
                    changed
                };
                if changed {
                    self.touch();
                }
                self.record(effect);
                Ok(effect)
            }
            ControlOutcome::Unconfirmed(reason) => self.setting_unconfirmed(reason),
            ControlOutcome::Unrecognized => self.setting_unconfirmed(UnknownReason::UnrecognizedEffect),
            ControlOutcome::Rejected => Err(match self.snapshot.state {
                TransactionState::Aborted => TransactionError::TransactionAborted,
                TransactionState::Unknown => TransactionError::UnknownOutcome,
                // 设置被拒绝不是“没有活动事务”，而是数据库没有接受这次设置。
                _ => TransactionError::ControlRejected,
            }),
            ControlOutcome::RolledBack => Err(TransactionError::ControlRejected),
            ControlOutcome::Aborted => match self.snapshot.state {
                TransactionState::Active => Ok(self.abort_in_place()),
                TransactionState::Unknown => Ok(TransactionEffect::Unknown),
                TransactionState::Aborted => Err(TransactionError::TransactionAborted),
                TransactionState::Idle => Err(TransactionError::ControlRejected),
            },
        }
    }

    /// 没有活动事务时设置失败不改变已生效设置；有事务时失去确认按未知处理。
    fn setting_unconfirmed(&mut self, reason: UnknownReason) -> Result<TransactionEffect, TransactionError> {
        if self.snapshot.state == TransactionState::Idle {
            return Err(TransactionError::ControlUnconfirmed);
        }
        self.lose_confirmation(reason);
        Ok(TransactionEffect::Unknown)
    }

    fn check_setting(&self, kind: SettingKind, value: Option<&str>) -> Result<(), TransactionError> {
        let support = kind.support(self.capabilities);
        if support.change == SettingChange::Unsupported {
            return Err(kind.unsupported(value.unwrap_or("default")));
        }
        if let Some(value) = value {
            if !support.options.iter().any(|option| option.id == value) {
                return Err(kind.unsupported(value));
            }
        }
        if self.snapshot.state != TransactionState::Idle && support.change != SettingChange::AnyTime {
            // 不为应用设置而提交或结束当前事务。
            return Err(TransactionError::SettingLocked(kind.label()));
        }
        Ok(())
    }

    fn require_mode(&self, mode: &str) -> Result<(), TransactionError> {
        let support = self.capabilities.mode;
        if support.change == SettingChange::Unsupported || !support.options.iter().any(|option| option.id == mode) {
            return Err(TransactionError::ModeUnsupported(mode.to_owned()));
        }
        Ok(())
    }

    fn known_savepoint(&self, name: &SavepointName) -> Result<(), TransactionError> {
        match self.savepoint_index(name) {
            Some(_) => Ok(()),
            None => Err(TransactionError::UnknownSavepoint(name.to_string())),
        }
    }

    fn rejection(&self) -> TransactionError {
        match self.snapshot.state {
            TransactionState::Idle => TransactionError::NoActiveTransaction,
            TransactionState::Aborted => TransactionError::TransactionAborted,
            TransactionState::Unknown => TransactionError::UnknownOutcome,
            TransactionState::Active => TransactionError::ControlRejected,
        }
    }

    fn start(&mut self, mode: Option<&str>) {
        self.snapshot.state = TransactionState::Active;
        self.snapshot.generation = self.snapshot.generation.saturating_add(1);
        self.snapshot.depth = 1;
        self.snapshot.savepoints.clear();
        if let Some(mode) = mode {
            self.snapshot.settings.mode = Some(mode.to_owned());
        }
        self.snapshot.last_effect = TransactionEffect::Began;
        self.touch();
    }

    fn end_transaction(&mut self, effect: TransactionEffect, chain: bool) {
        self.snapshot.generation = self.snapshot.generation.saturating_add(1);
        self.snapshot.savepoints.clear();
        self.snapshot.state = if chain { TransactionState::Active } else { TransactionState::Idle };
        self.snapshot.depth = if chain { 1 } else { 0 };
        self.snapshot.last_effect = effect;
        self.touch();
    }

    /// 直接确认了当前未知事务的结局：清除未知记录。旧事务的记录不会被新事务的结束清除。
    fn confirm_current_unknown(&mut self) {
        if self.snapshot.state == TransactionState::Unknown {
            self.snapshot.unresolved = None;
        }
    }

    /// 语句失败把事务置为中止：保存点保留，仍可按方言能力回滚到保存点恢复。
    fn abort_in_place(&mut self) -> TransactionEffect {
        if self.snapshot.state != TransactionState::Aborted {
            self.snapshot.state = TransactionState::Aborted;
            self.snapshot.last_effect = TransactionEffect::Aborted;
            self.touch();
        }
        TransactionEffect::Aborted
    }

    /// 失去确认：第一次转为未知时记下原因与事务代次，之后保持不变直到被确认结果或核对解决。
    fn lose_confirmation(&mut self, reason: UnknownReason) {
        if self.snapshot.state != TransactionState::Unknown {
            let state = self.snapshot.state;
            self.snapshot.unresolved = Some(UnknownTransaction { reason, generation: self.snapshot.generation, state });
            self.snapshot.generation = self.snapshot.generation.saturating_add(1);
            self.snapshot.state = TransactionState::Unknown;
        }
        self.record(TransactionEffect::Unknown);
    }

    fn register_savepoint(&mut self, name: &SavepointName) {
        // 同名保存点只保留最新边界：回滚与释放都只作用于它。
        self.snapshot.savepoints.retain(|existing| !existing.matches(name));
        self.snapshot.savepoints.push(name.clone());
        self.touch();
    }

    fn savepoint_index(&self, name: &SavepointName) -> Option<usize> {
        self.snapshot.savepoints.iter().rposition(|existing| existing.matches(name))
    }

    fn record(&mut self, effect: TransactionEffect) {
        if self.snapshot.last_effect != effect {
            self.snapshot.last_effect = effect;
            self.touch();
        }
    }

    fn touch(&mut self) {
        self.snapshot.revision = self.snapshot.revision.saturating_add(1);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingKind {
    Isolation,
    Mode,
}

impl SettingKind {
    fn support(self, capabilities: TransactionCapabilities) -> SettingSupport {
        match self {
            Self::Isolation => capabilities.isolation,
            Self::Mode => capabilities.mode,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Isolation => "隔离级别",
            Self::Mode => "事务模式",
        }
    }

    fn unsupported(self, value: &str) -> TransactionError {
        match self {
            Self::Isolation => TransactionError::IsolationUnsupported(value.to_owned()),
            Self::Mode => TransactionError::ModeUnsupported(value.to_owned()),
        }
    }
}
