//! 共享事务控件（ADR-5）：同连接快照、控制命令入口、保存点/隔离选项与关闭保护状态机。
//!
//! 控件只读 [`Service::transaction_snapshot`]，从不在标签或页面里另存事务状态：查询、网格与结构
//! 工作区画的是同一份快照，任一入口开始、提交、回滚或执行手写控制语句之后，其他地方立即看到同一结果。
//! 命令在执行完成、数据库给出结果之后才推进状态；保存点、隔离级别和事务模式的能力边界来自方言能力表。
//!
//! 本模块不依赖 GPUI：动作 id、选项、提示键和关闭保护都在这里可独立测试；真正的绘制在 `chrome`，
//! 异步执行由 `Workbench` 取走 [`TransactionRequest`] 后调用 [`execute`]。

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::connections::transaction::ControlCommand;
use crate::connections::Service;
use crate::query::exec::LiveDriver;
use crate::query::transaction::{attempt, StatementPlan};
use crate::transaction::{
    ControlSql, SavepointName, SettingChange, TransactionCapabilities, TransactionEffect, TransactionSnapshot, TransactionState, UnknownResolution,
};

/// 没有打开的业务会话：不另开连接，也不改用元数据会话。
const NOT_OPEN: &str = "这条连接还没有打开，没有另开新的连接。";

// ---------------------------------------------------------------- 动作 id

pub const BEGIN: &str = "tx-begin";
pub const COMMIT: &str = "tx-commit";
pub const ROLLBACK: &str = "tx-rollback";
pub const SAVEPOINT_MENU: &str = "tx-savepoint-menu";
pub const ISOLATION_MENU: &str = "tx-isolation-menu";
pub const MODE_MENU: &str = "tx-mode-menu";
pub const RESOLVE_COMMITTED: &str = "tx-resolve:committed";
pub const RESOLVE_ROLLED_BACK: &str = "tx-resolve:rolled_back";
pub const GUARD_CANCEL: &str = "tx-guard-cancel";
pub const GUARD_COMMIT: &str = "tx-guard-commit";
pub const GUARD_ROLLBACK: &str = "tx-guard-rollback";

const SAVEPOINT_CREATE_PREFIX: &str = "tx-savepoint-create:";
const SAVEPOINT_ROLLBACK_PREFIX: &str = "tx-savepoint-rollback:";
const SAVEPOINT_RELEASE_PREFIX: &str = "tx-savepoint-release:";
const ISOLATION_PREFIX: &str = "tx-isolation:";
const MODE_PREFIX: &str = "tx-mode:";

/// 一条连接上的业务标签（查询/网格/结构）共享的事务目标。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransactionTarget {
    pub connection_id: String,
    pub connection_name: String,
    pub engine: String,
    pub database: String,
}

/// 用户在事务控件上选择的命令。它只是意图：真正改变状态的是数据库确认后的登记结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransactionCommand {
    Begin,
    Commit,
    Rollback,
    Savepoint(String),
    RollbackToSavepoint(String),
    ReleaseSavepoint(String),
    /// `None` 表示恢复服务器默认隔离级别。
    Isolation(Option<String>),
    /// `None` 表示恢复方言默认事务模式。
    Mode(Option<String>),
    /// 独立核对后关闭旧事务的未知记录；不重放任何写入。
    Resolve(UnknownResolution),
}

impl TransactionCommand {
    /// 把控件动作 id 解析成命令。未知 id 返回 `None`，调用方保留原样，不猜测动作。
    pub fn parse(id: &str) -> Option<Self> {
        match id {
            BEGIN => Some(Self::Begin),
            COMMIT => Some(Self::Commit),
            ROLLBACK => Some(Self::Rollback),
            RESOLVE_COMMITTED => Some(Self::Resolve(UnknownResolution::Committed)),
            RESOLVE_ROLLED_BACK => Some(Self::Resolve(UnknownResolution::RolledBack)),
            _ => {
                if let Some(name) = id.strip_prefix(SAVEPOINT_CREATE_PREFIX) {
                    return Some(Self::Savepoint(name.to_owned()));
                }
                if let Some(name) = id.strip_prefix(SAVEPOINT_ROLLBACK_PREFIX) {
                    return Some(Self::RollbackToSavepoint(name.to_owned()));
                }
                if let Some(name) = id.strip_prefix(SAVEPOINT_RELEASE_PREFIX) {
                    return Some(Self::ReleaseSavepoint(name.to_owned()));
                }
                if let Some(value) = id.strip_prefix(ISOLATION_PREFIX) {
                    return Some(Self::Isolation(option_of(value)));
                }
                if let Some(value) = id.strip_prefix(MODE_PREFIX) {
                    return Some(Self::Mode(option_of(value)));
                }
                None
            }
        }
    }

    pub fn savepoint_create_id(name: &str) -> String {
        format!("{SAVEPOINT_CREATE_PREFIX}{name}")
    }

    pub fn savepoint_rollback_id(name: &str) -> String {
        format!("{SAVEPOINT_ROLLBACK_PREFIX}{name}")
    }

    pub fn savepoint_release_id(name: &str) -> String {
        format!("{SAVEPOINT_RELEASE_PREFIX}{name}")
    }

    pub fn isolation_id(value: Option<&str>) -> String {
        format!("{ISOLATION_PREFIX}{}", value.unwrap_or("default"))
    }

    pub fn mode_id(value: Option<&str>) -> String {
        format!("{MODE_PREFIX}{}", value.unwrap_or("default"))
    }

    /// 这条命令是否只是在登记设置（不会向数据库发语句或只发设置语句）。
    pub fn is_setting(&self) -> bool {
        matches!(self, Self::Isolation(_) | Self::Mode(_))
    }
}

fn option_of(value: &str) -> Option<String> {
    (value != "default").then(|| value.to_owned())
}

// ---------------------------------------------------------------- 可用动作

/// 当前快照下哪些入口可到达。文案与禁用原因都由调用方按方言能力给出。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransactionActions {
    pub begin: bool,
    pub commit: bool,
    pub rollback: bool,
    pub savepoints: bool,
    /// 中止事务允许回滚到保存点（PostgreSQL 类）。
    pub savepoint_recovery: bool,
    /// 未知结果可以由使用者独立核对后关闭记录。
    pub resolve: bool,
    /// 隔离级别入口是否可点（不支持时整项不画）。
    pub isolation: bool,
    pub mode: bool,
}

impl TransactionActions {
    /// 按状态与方言能力给出可用动作。没有显式事务控制的方言没有任何动作。
    pub fn of(snapshot: &TransactionSnapshot, capabilities: TransactionCapabilities) -> Self {
        if !capabilities.explicit_control {
            return Self { begin: false, commit: false, rollback: false, savepoints: false, savepoint_recovery: false, resolve: false, isolation: false, mode: false };
        }
        let mut actions = Self {
            begin: false,
            commit: false,
            rollback: false,
            savepoints: false,
            savepoint_recovery: false,
            resolve: false,
            isolation: capabilities.isolation.change != SettingChange::Unsupported,
            mode: capabilities.mode.change != SettingChange::Unsupported,
        };
        match snapshot.state {
            TransactionState::Idle => actions.begin = true,
            TransactionState::Active => {
                actions.commit = true;
                actions.rollback = true;
                actions.savepoints = capabilities.savepoints;
            }
            TransactionState::Aborted => {
                // 中止事务里提交与数据库规则矛盾：只提供回滚，或方言允许的回滚到保存点。
                actions.rollback = true;
                actions.savepoint_recovery = capabilities.savepoints && capabilities.aborted == crate::transaction::AbortedRecovery::SavepointRecovery;
            }
            TransactionState::Unknown => {
                // 未知状态不提供开始：先用回滚或独立核对把结果说清楚。
                actions.rollback = true;
                actions.resolve = true;
            }
        }
        actions
    }
}

// ---------------------------------------------------------------- 文案

pub fn state_key(state: TransactionState) -> &'static str {
    match state {
        TransactionState::Idle => "transaction.state.idle",
        TransactionState::Active => "transaction.state.active",
        TransactionState::Aborted => "transaction.state.aborted",
        TransactionState::Unknown => "transaction.state.unknown",
    }
}

pub fn state_hint_key(state: TransactionState) -> Option<&'static str> {
    match state {
        TransactionState::Idle => None,
        TransactionState::Active => Some("transaction.state.activeHint"),
        TransactionState::Aborted => Some("transaction.state.abortedHint"),
        TransactionState::Unknown => Some("transaction.state.unknownHint"),
    }
}

pub fn effect_key(effect: TransactionEffect) -> Option<&'static str> {
    match effect {
        TransactionEffect::Began => Some("transaction.effect.began"),
        TransactionEffect::StillActive => Some("transaction.effect.stillActive"),
        TransactionEffect::Committed => Some("transaction.effect.committed"),
        TransactionEffect::RolledBack => Some("transaction.effect.rolledBack"),
        TransactionEffect::Aborted => Some("transaction.effect.aborted"),
        TransactionEffect::ImplicitCommit => Some("transaction.effect.implicitCommit"),
        TransactionEffect::Unknown => Some("transaction.effect.unknown"),
        TransactionEffect::Unchanged => None,
    }
}

/// 命令执行后显示的那一句：设置类命令在自动提交下报“已生效/下一次生效”，不再借用事务效果。
pub fn command_effect_key(command: &TransactionCommand, effect: TransactionEffect) -> Option<&'static str> {
    if effect == TransactionEffect::Unchanged {
        return command.is_setting().then_some("transaction.settings.applied");
    }
    effect_key(effect)
}

pub fn isolation_label_key(id: &str) -> String {
    format!("transaction.isolation.{id}")
}

pub fn mode_label_key(id: &str) -> String {
    format!("transaction.mode.{id}")
}

/// 界面推荐的下一个保存点名：`sp1`、`sp2`……跳过当前事务里已有的名字。
/// 保存点名称仍由用户经手写 SQL 决定；控件只提供一个不重名的默认值。
pub fn suggested_savepoint(snapshot: &TransactionSnapshot) -> String {
    for serial in 1u32.. {
        let name = format!("sp{serial}");
        if snapshot.savepoints.iter().all(|existing| existing.as_str() != name) {
            return name;
        }
    }
    unreachable!("u32 范围内的保存点名称不会被用尽")
}

// ---------------------------------------------------------------- 控件状态

/// 事务控件上的一次性界面状态：展开的菜单与最近一条回执。
///
/// 这里没有事务状态本身：状态只从共享快照读，切换连接或收到其他标签的回执时都会对齐同一份事实。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TransactionBarState {
    /// 上一次绘制/操作针对的连接；切到别的连接时清掉旧连接的回执与菜单。
    pub connection: Option<String>,
    pub menu: Option<TransactionMenu>,
    pub busy: bool,
    pub effect: Option<TransactionEffect>,
    pub message_key: Option<&'static str>,
    pub message_detail: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransactionMenu {
    Savepoints,
    Isolation,
    Mode,
}

impl TransactionBarState {
    /// 对齐到当前连接：换连接时不把上一个连接的回执或菜单带过去。
    pub fn align(&mut self, connection: &str) {
        if self.connection.as_deref() != Some(connection) {
            self.connection = Some(connection.to_owned());
            self.menu = None;
            self.busy = false;
            self.effect = None;
            self.message_key = None;
            self.message_detail.clear();
        }
    }

    pub fn toggle_menu(&mut self, menu: TransactionMenu) {
        self.menu = (self.menu != Some(menu)).then_some(menu);
    }

    pub fn close_menu(&mut self) {
        self.menu = None;
    }

    pub fn begin_request(&mut self) {
        self.busy = true;
        self.menu = None;
        self.message_key = None;
        self.message_detail.clear();
    }

    /// 一次命令的可见回执。
    pub fn note(&mut self, effect: TransactionEffect, key: Option<&'static str>, detail: String) {
        self.busy = false;
        self.effect = Some(effect);
        self.message_key = key;
        self.message_detail = detail;
    }

    pub fn note_error(&mut self, detail: String) {
        self.busy = false;
        self.effect = None;
        self.message_key = Some("transaction.command.failed");
        self.message_detail = detail;
    }
}

/// `Workbench` 取走的一次事务命令。`guard` 非空表示它属于关闭保护流程。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransactionRequest {
    pub target: TransactionTarget,
    pub command: TransactionCommand,
    pub guard: Option<GuardDecision>,
}

// ---------------------------------------------------------------- 关闭保护

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuardCause {
    Quit,
    Update,
    Disconnect,
}

impl GuardCause {
    pub fn title_key(self) -> &'static str {
        match self {
            Self::Quit => "transaction.guard.quitTitle",
            Self::Update => "transaction.guard.updateTitle",
            Self::Disconnect => "transaction.guard.disconnectTitle",
        }
    }
}

/// 一条连接的事务事实（视图从 `Service` 读出后交给模型；模型不持有服务）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuardFact {
    pub connection_id: String,
    pub connection_name: String,
    pub engine: String,
    pub database: String,
    pub state: TransactionState,
}

impl GuardFact {
    pub fn target(&self) -> TransactionTarget {
        TransactionTarget {
            connection_id: self.connection_id.clone(),
            connection_name: self.connection_name.clone(),
            engine: self.engine.clone(),
            database: self.database.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuardDecision {
    Commit,
    Rollback,
}

impl GuardDecision {
    pub fn command(self) -> TransactionCommand {
        match self {
            Self::Commit => TransactionCommand::Commit,
            Self::Rollback => TransactionCommand::Rollback,
        }
    }

    /// 只有数据库确认了这个方向的结局才算成功；其他效果（含未知）一律中止关闭。
    pub fn confirmed(self) -> TransactionEffect {
        match self {
            Self::Commit => TransactionEffect::Committed,
            Self::Rollback => TransactionEffect::RolledBack,
        }
    }
}

/// 关闭保护：逐条连接询问，当前这条得到确认结果后才轮到下一条。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransactionGuard {
    pub cause: GuardCause,
    pub current: GuardFact,
    pub queue: Vec<GuardFact>,
    pub pending: Option<GuardDecision>,
    pub busy: bool,
    pub error: Option<String>,
    /// 断开连接时原本要断开的那条连接；提交/回滚确认成功后完成这次断开。
    pub disconnect: Option<String>,
}

/// 关闭保护完成后要继续的流程。只有确认成功才会产生它。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GuardFollowup {
    Quit,
    Update,
    Disconnect(String),
}

/// 当前这条连接的提交/回滚结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GuardStep {
    /// 结果确认，但还有连接要问；当前项已经前进。
    Next,
    /// 所有连接都处理完，继续原来的关闭/更新/断开流程。
    Complete(GuardFollowup),
    /// 效果不是“已按选择结束”（失败、未知、仍活动）：关闭必须中止。
    Mismatch,
}

/// 关闭保护收到一条连接结果后的去向。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GuardOutcome {
    /// 当前连接已处理，等待下一条或正在执行。
    Pending,
    /// 所有连接都确认结束，继续原来的流程。
    Continue(GuardFollowup),
    /// 失败或未知：关闭已中止，对话框保留。
    Stopped,
}

impl TransactionGuard {
    pub fn remaining(&self) -> usize {
        self.queue.len()
    }

    /// 当前这条连接上允许的选择。中止事务不能提交；未知状态在打开保护前就会被拦下。
    pub fn allows(&self, decision: GuardDecision) -> bool {
        match decision {
            GuardDecision::Commit => self.current.state == TransactionState::Active,
            GuardDecision::Rollback => matches!(self.current.state, TransactionState::Active | TransactionState::Aborted),
        }
    }

    pub fn pending_decision(&self) -> Option<GuardDecision> {
        self.pending
    }

    /// 把当前连接的确认效果交给流程：只有请求方向被数据库确认才算成功。
    pub fn accept(&mut self, effect: TransactionEffect) -> GuardStep {
        let Some(pending) = self.pending.take() else {
            return GuardStep::Mismatch;
        };
        if effect != pending.confirmed() {
            return GuardStep::Mismatch;
        }
        self.error = None;
        self.busy = false;
        match self.queue.first().cloned() {
            Some(next) => {
                self.queue.remove(0);
                self.current = next;
                GuardStep::Next
            }
            None => GuardStep::Complete(self.followup()),
        }
    }

    fn followup(&self) -> GuardFollowup {
        match self.cause {
            GuardCause::Quit => GuardFollowup::Quit,
            GuardCause::Update => GuardFollowup::Update,
            GuardCause::Disconnect => GuardFollowup::Disconnect(self.disconnect.clone().unwrap_or_else(|| self.current.connection_id.clone())),
        }
    }
}

// ---------------------------------------------------------------- 异步执行

/// 执行一次控件命令：取得业务会话与租约，按方言映射出 SQL，数据库确认后登记到共享协调器。
///
/// 一条命令只在一个租约里执行；活动事务期间不会并发驱动同一条事务连接。提交/回滚的结果没有返回时
/// 返回 [`TransactionEffect::Unknown`]，调用方据此中止关闭并保留可见状态，绝不重放。
pub async fn execute(service: &Arc<Service>, target: &TransactionTarget, command: &TransactionCommand) -> Result<TransactionEffect, String> {
    let coordinator = service.transaction_coordinator(&target.connection_id).ok_or_else(|| NOT_OPEN.to_owned())?;
    if !coordinator.is_transactional() {
        return Err("这条连接没有事务控制".to_owned());
    }
    // 独立核对不发送任何语句：它只是关闭旧事务的未知记录。
    if let TransactionCommand::Resolve(resolution) = command {
        return coordinator
            .resolve_unknown(*resolution)
            .map(|_| match resolution {
                UnknownResolution::Committed => TransactionEffect::Committed,
                UnknownResolution::RolledBack => TransactionEffect::RolledBack,
            })
            .ok_or_else(|| "没有需要独立核对的旧事务".to_owned());
    }
    let control = control_command(coordinator.as_ref(), command)?;
    if matches!(control.sql, ControlSql::NoStatement) {
        return coordinator
            .acknowledge(&control.statement, crate::transaction::ControlOutcome::Confirmed)
            .map_err(|error| error.to_string());
    }
    let sql = match &control.sql {
        ControlSql::Statement(sql) => sql.clone(),
        ControlSql::Unsupported => return Err("这条事务控制在当前数据库上没有对应的执行方式".to_owned()),
        ControlSql::NoStatement => unreachable!("上面已经处理过不下发语句的设置"),
    };
    let query = service.query_session(&target.connection_id).await?.ok_or_else(|| NOT_OPEN.to_owned())?;
    // 租约持有到这条命令结束；业务会话与查询批次共用同一条事务连接。
    let lease = query.lease;
    let transport = service.cancel_transport(&target.connection_id).unwrap_or_default();
    let driver = LiveDriver::attach(query.session, &target.engine, &target.database, transport, None).await?;
    let cancel = Arc::new(AtomicBool::new(false));
    let plan = StatementPlan::Control { statement: control.statement.clone(), sql: control.sql.clone() };
    let trial = attempt(Some(coordinator.as_ref()), plan, &driver, 1, &cancel, &sql).await;
    let _ = driver.finish().await;
    drop(lease);
    match (trial.status, trial.transaction) {
        ("ok", Some(effect)) => Ok(effect),
        // 结果没有返回：事务结局未知，交给调用方中止关闭并显示未知。
        ("unknown", Some(effect)) => Ok(effect),
        _ => Err(if trial.message.trim().is_empty() { "事务命令没有完成".to_owned() } else { trial.message }),
    }
}

fn control_command(
    coordinator: &crate::connections::transaction::TransactionCoordinator,
    command: &TransactionCommand,
) -> Result<ControlCommand, String> {
    let parsed = match command {
        TransactionCommand::Begin => return coordinator.begin(None).map_err(|error| error.to_string()),
        TransactionCommand::Commit => return coordinator.commit().map_err(|error| error.to_string()),
        TransactionCommand::Rollback => return coordinator.rollback().map_err(|error| error.to_string()),
        TransactionCommand::Savepoint(name) => Some(ControlStatementKind::Savepoint(name)),
        TransactionCommand::RollbackToSavepoint(name) => Some(ControlStatementKind::RollbackToSavepoint(name)),
        TransactionCommand::ReleaseSavepoint(name) => Some(ControlStatementKind::ReleaseSavepoint(name)),
        TransactionCommand::Isolation(_) | TransactionCommand::Mode(_) | TransactionCommand::Resolve(_) => None,
    };
    if let Some(parsed) = parsed {
        let name = SavepointName::parse(parsed.name()).map_err(|error| error.to_string())?;
        return match parsed {
            ControlStatementKind::Savepoint(_) => coordinator.savepoint(&name),
            ControlStatementKind::RollbackToSavepoint(_) => coordinator.rollback_to_savepoint(&name),
            ControlStatementKind::ReleaseSavepoint(_) => coordinator.release_savepoint(&name),
        }
        .map_err(|error| error.to_string());
    }
    match command {
        TransactionCommand::Isolation(level) => coordinator.set_isolation(level.as_deref()).map_err(|error| error.to_string()),
        TransactionCommand::Mode(mode) => coordinator.set_mode(mode.as_deref()).map_err(|error| error.to_string()),
        _ => Err("不支持的事务命令".to_owned()),
    }
}

enum ControlStatementKind<'a> {
    Savepoint(&'a str),
    RollbackToSavepoint(&'a str),
    ReleaseSavepoint(&'a str),
}

impl ControlStatementKind<'_> {
    fn name(&self) -> &str {
        match self {
            Self::Savepoint(name) | Self::RollbackToSavepoint(name) | Self::ReleaseSavepoint(name) => name,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::adapter;
    use crate::connections::model::Engine;
    use crate::transaction::{AbortedRecovery, DuplicateBegin, ImplicitCommitSupport, SettingOption, TransactionSettings};

    fn capabilities(engine: Engine) -> TransactionCapabilities {
        adapter(engine).transaction().map(|dialect| dialect.capabilities()).unwrap_or(TransactionCapabilities::UNSUPPORTED)
    }

    fn snapshot(state: TransactionState) -> TransactionSnapshot {
        TransactionSnapshot {
            connection: "c1".to_owned(),
            state,
            generation: 1,
            revision: 1,
            depth: Some(1),
            settings: TransactionSettings::default(),
            savepoints: Vec::new(),
            last_effect: TransactionEffect::Unchanged,
            unresolved: None,
        }
    }

    #[test]
    fn every_control_action_has_a_stable_id_round_trip() {
        assert_eq!(TransactionCommand::parse(BEGIN), Some(TransactionCommand::Begin));
        assert_eq!(TransactionCommand::parse(COMMIT), Some(TransactionCommand::Commit));
        assert_eq!(TransactionCommand::parse(ROLLBACK), Some(TransactionCommand::Rollback));
        assert_eq!(TransactionCommand::parse(RESOLVE_COMMITTED), Some(TransactionCommand::Resolve(UnknownResolution::Committed)));
        assert_eq!(TransactionCommand::parse(RESOLVE_ROLLED_BACK), Some(TransactionCommand::Resolve(UnknownResolution::RolledBack)));
        let rollback = TransactionCommand::savepoint_rollback_id("sp 1");
        assert_eq!(rollback, "tx-savepoint-rollback:sp 1");
        assert_eq!(TransactionCommand::parse(&rollback), Some(TransactionCommand::RollbackToSavepoint("sp 1".to_owned())));
        let release = TransactionCommand::savepoint_release_id("sp1");
        assert_eq!(TransactionCommand::parse(&release), Some(TransactionCommand::ReleaseSavepoint("sp1".to_owned())));
        assert_eq!(TransactionCommand::parse(&TransactionCommand::isolation_id(Some("serializable"))), Some(TransactionCommand::Isolation(Some("serializable".to_owned()))));
        assert_eq!(
            TransactionCommand::parse(&TransactionCommand::isolation_id(None)),
            Some(TransactionCommand::Isolation(None)),
            "默认项用稳定标记表示服务器默认"
        );
        assert_eq!(TransactionCommand::parse(&TransactionCommand::mode_id(None)), Some(TransactionCommand::Mode(None)));
        assert_eq!(TransactionCommand::parse("tx-savepoint"), None, "菜单动作不是命令");
        assert_eq!(TransactionCommand::parse("not-a-command"), None);
    }

    #[test]
    fn idle_and_active_actions_follow_the_shared_state() {
        let pg = capabilities(Engine::Postgres);
        let idle = TransactionActions::of(&snapshot(TransactionState::Idle), pg);
        assert!(idle.begin && !idle.commit && !idle.rollback, "自动提交下没有可提交/回滚的事务");
        assert!(idle.isolation && idle.mode, "PostgreSQL 的隔离级别与只读/读写模式都可选");
        let sqlserver = capabilities(Engine::Sqlserver);
        let idle = TransactionActions::of(&snapshot(TransactionState::Idle), sqlserver);
        assert!(idle.isolation && !idle.mode, "SQL Server 没有事务模式选项");

        let active = TransactionActions::of(&snapshot(TransactionState::Active), pg);
        assert!(!active.begin, "活动事务中不再提供开始入口（部分回滚用保存点）");
        assert!(active.commit && active.rollback && active.savepoints);

        let aborted = TransactionActions::of(&snapshot(TransactionState::Aborted), pg);
        assert!(!aborted.commit, "中止事务不能提交");
        assert!(aborted.rollback && aborted.savepoint_recovery, "PostgreSQL 可以回滚到保存点恢复");

        let unknown = TransactionActions::of(&snapshot(TransactionState::Unknown), pg);
        assert!(!unknown.begin && !unknown.commit, "未知状态不据此开始或提交");
        assert!(unknown.rollback && unknown.resolve, "未知状态下可以回滚或独立核对");
    }

    #[test]
    fn savepoint_release_and_mode_entries_follow_the_dialect() {
        let sqlserver = capabilities(Engine::Sqlserver);
        let active = TransactionActions::of(&snapshot(TransactionState::Active), sqlserver);
        assert!(active.savepoints, "SQL Server 有原生保存点");
        assert!(!sqlserver.release_savepoint, "SQL Server 没有释放保存点");

        let sqlite = capabilities(Engine::Sqlite);
        let idle = TransactionActions::of(&snapshot(TransactionState::Idle), sqlite);
        assert!(idle.mode, "SQLite 的事务模式入口");
        let mysql = capabilities(Engine::Mysql);
        let idle = TransactionActions::of(&snapshot(TransactionState::Idle), mysql);
        assert!(idle.mode, "MySQL 的只读/读写事务模式入口");

        let mongo = TransactionCapabilities::UNSUPPORTED;
        let none = TransactionActions::of(&snapshot(TransactionState::Idle), mongo);
        assert!(!none.begin && !none.isolation && !none.mode && !none.resolve);
    }

    #[test]
    fn aborted_recovery_is_only_offered_where_the_dialect_allows_it() {
        let sqlserver = TransactionCapabilities {
            aborted: AbortedRecovery::RollbackOnly,
            ..capabilities(Engine::Sqlserver)
        };
        let actions = TransactionActions::of(&snapshot(TransactionState::Aborted), sqlserver);
        assert!(!actions.savepoint_recovery, "SQL Server 中止后只能整体回滚");

        let mysql = capabilities(Engine::Mysql);
        assert_eq!(mysql.aborted, AbortedRecovery::NotApplicable);
        assert_eq!(mysql.duplicate_begin, DuplicateBegin::EndsPrevious);
        assert_eq!(mysql.implicit_commit, ImplicitCommitSupport::MysqlFamily);
    }

    #[test]
    fn state_and_effect_labels_never_echo_the_key() {
        for state in [TransactionState::Idle, TransactionState::Active, TransactionState::Aborted, TransactionState::Unknown] {
            for locale in [crate::i18n::Locale::ZhCn, crate::i18n::Locale::En] {
                let key = state_key(state);
                let text = crate::i18n::text(locale, key);
                assert_ne!(text, key, "{key} {locale:?}");
                assert!(!text.is_empty(), "{key}");
            }
        }
        for effect in [
            TransactionEffect::Began,
            TransactionEffect::StillActive,
            TransactionEffect::Committed,
            TransactionEffect::RolledBack,
            TransactionEffect::Aborted,
            TransactionEffect::ImplicitCommit,
            TransactionEffect::Unknown,
        ] {
            let key = effect_key(effect).expect("效果文案");
            for locale in [crate::i18n::Locale::ZhCn, crate::i18n::Locale::En] {
                assert_ne!(crate::i18n::text(locale, key), key, "{key} {locale:?}");
            }
        }
    }

    #[test]
    fn every_setting_option_has_translated_labels() {
        let engines = [Engine::Postgres, Engine::Mysql, Engine::Mariadb, Engine::Sqlite, Engine::Sqlserver];
        for engine in engines {
            let capabilities = capabilities(engine);
            let groups = [(isolation_label_key as fn(&str) -> String, capabilities.isolation), (mode_label_key, capabilities.mode)];
            for (label_key, support) in groups {
                for SettingOption { id, .. } in support.options {
                    let key = label_key(id);
                    for locale in [crate::i18n::Locale::ZhCn, crate::i18n::Locale::En] {
                        assert_ne!(crate::i18n::text(locale, &key), key, "{key} {engine:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn bar_state_keeps_receipts_with_their_own_connection() {
        let mut bar = TransactionBarState::default();
        bar.align("c1");
        bar.note(TransactionEffect::Committed, Some("transaction.effect.committed"), String::new());
        bar.toggle_menu(TransactionMenu::Savepoints);
        bar.align("c1");
        assert_eq!(bar.effect, Some(TransactionEffect::Committed), "同一连接继续显示回执");
        assert_eq!(bar.menu, Some(TransactionMenu::Savepoints));
        bar.align("c2");
        assert_eq!(bar.effect, None, "切换连接不把上一连接的回执挂到新连接上");
        assert_eq!(bar.menu, None);
    }

    #[test]
    fn the_suggested_savepoint_skips_names_already_in_the_transaction() {
        let mut snapshot = snapshot(TransactionState::Active);
        snapshot.savepoints = vec![SavepointName::parse("sp1").unwrap(), SavepointName::parse("sp2").unwrap()];
        assert_eq!(suggested_savepoint(&snapshot), "sp3");
        snapshot.savepoints.clear();
        assert_eq!(suggested_savepoint(&snapshot), "sp1");
    }

    #[test]
    fn the_model_blocks_closing_unknown_transactions_and_asks_one_connection_at_a_time() {
        use crate::app::Shell;
        use crate::app::shell::WorkbenchModel;

        let mut model = WorkbenchModel::new(Shell::default());
        let unknown = vec![fact("a", TransactionState::Unknown)];
        assert!(model.open_transaction_guard(GuardCause::Quit, unknown), "未知事务也要拦下关闭");
        assert!(model.transaction_guard.is_none(), "未知状态不打开可以提交/回滚绕过的对话框");
        assert_eq!(model.notice_key, Some("transaction.guard.unknown"));
        assert_eq!(model.notice_detail, "a");

        let facts = vec![fact("idle", TransactionState::Idle), fact("b", TransactionState::Active), fact("c", TransactionState::Aborted)];
        assert!(model.open_transaction_guard(GuardCause::Quit, facts), "活动与中止事务都要保护");
        assert_eq!(model.transaction_guard.as_ref().map(|guard| guard.current.connection_id.as_str()), Some("b"));
        assert_eq!(model.transaction_guard.as_ref().map(TransactionGuard::remaining), Some(1));

        assert!(model.dispatch("tx-guard-commit").is_some());
        let request = model.transaction_request.take().expect("提交请求");
        assert_eq!(request.guard, Some(GuardDecision::Commit));
        assert_eq!(model.guard_finished(Err("connection lost".to_owned())), GuardOutcome::Stopped);
        assert!(model.guard_open(), "失败后对话框保留");
        assert_eq!(
            model.transaction_guard.as_ref().and_then(|guard| guard.error.clone()).as_deref(),
            Some("connection lost")
        );

        assert!(model.dispatch("tx-guard-commit").is_some());
        let request = model.transaction_request.take().expect("重试提交");
        assert_eq!(request.guard, Some(GuardDecision::Commit));
        assert_eq!(model.guard_finished(Ok(TransactionEffect::Committed)), GuardOutcome::Pending, "确认后轮到下一条连接");
        assert_eq!(model.transaction_guard.as_ref().map(|guard| guard.current.connection_id.as_str()), Some("c"));
        assert!(!model.transaction_guard.as_ref().unwrap().allows(GuardDecision::Commit), "中止连接只能回滚后继续");

        assert!(model.dispatch("tx-guard-rollback").is_some());
        let request = model.transaction_request.take().expect("回滚请求");
        assert_eq!(request.guard, Some(GuardDecision::Rollback));
        assert_eq!(model.guard_finished(Ok(TransactionEffect::RolledBack)), GuardOutcome::Continue(GuardFollowup::Quit));
        assert!(!model.guard_open());
    }

    #[test]
    fn the_guard_does_not_close_when_the_control_outcome_is_unknown() {
        use crate::app::Shell;
        use crate::app::shell::WorkbenchModel;

        let mut model = WorkbenchModel::new(Shell::default());
        assert!(model.open_transaction_guard(GuardCause::Disconnect, vec![fact("a", TransactionState::Active)]));
        assert!(model.dispatch("tx-guard-rollback").is_some());
        let _request = model.transaction_request.take().expect("回滚请求");
        assert_eq!(model.guard_finished(Ok(TransactionEffect::Unknown)), GuardOutcome::Stopped, "结果未知不显示正常结束");
        assert!(model.guard_open());
        assert!(model.dispatch("tx-guard-cancel").is_some());
        assert!(!model.guard_open(), "取消后保持原状");
        assert!(model.transaction_request.is_none(), "取消不发任何事务命令");
    }

    #[test]
    fn guard_requests_only_continue_on_a_confirmed_outcome() {
        assert_eq!(GuardDecision::Commit.confirmed(), TransactionEffect::Committed);
        assert_eq!(GuardDecision::Rollback.confirmed(), TransactionEffect::RolledBack);
        // 未知、仍活动、中止与隐式提交都不是“已按选择结束”：关闭保护必须中止。
        for effect in [
            TransactionEffect::Unknown,
            TransactionEffect::StillActive,
            TransactionEffect::Aborted,
            TransactionEffect::ImplicitCommit,
            TransactionEffect::Began,
            TransactionEffect::Unchanged,
        ] {
            assert_ne!(effect, GuardDecision::Commit.confirmed(), "{effect:?}");
            assert_ne!(effect, GuardDecision::Rollback.confirmed(), "{effect:?}");
        }
    }

    fn fact(connection: &str, state: TransactionState) -> GuardFact {
        GuardFact {
            connection_id: connection.to_owned(),
            connection_name: connection.to_owned(),
            engine: "sqlite".to_owned(),
            database: "main".to_owned(),
            state,
        }
    }

    fn guard(cause: GuardCause, current: GuardFact, queue: Vec<GuardFact>) -> TransactionGuard {
        TransactionGuard {
            cause,
            current,
            queue,
            pending: None,
            busy: false,
            error: None,
            disconnect: None,
        }
    }

    #[test]
    fn the_guard_walks_connections_one_by_one_and_only_continues_after_confirmation() {
        let mut flow = guard(GuardCause::Quit, fact("a", TransactionState::Active), vec![fact("b", TransactionState::Active)]);
        assert_eq!(flow.remaining(), 1);
        flow.pending = Some(GuardDecision::Commit);
        assert_eq!(flow.accept(TransactionEffect::StillActive), GuardStep::Mismatch, "未结束的事务不能继续关闭");
        flow.pending = Some(GuardDecision::Commit);
        assert_eq!(flow.accept(TransactionEffect::Unknown), GuardStep::Mismatch, "结果未知不显示正常结束");
        flow.pending = Some(GuardDecision::Commit);
        assert_eq!(flow.accept(TransactionEffect::Committed), GuardStep::Next, "第一条连接确认后轮到下一条");
        assert_eq!(flow.current.connection_id, "b");
        flow.pending = Some(GuardDecision::Rollback);
        assert_eq!(flow.accept(TransactionEffect::RolledBack), GuardStep::Complete(GuardFollowup::Quit));
    }

    #[test]
    fn an_aborted_transaction_can_only_roll_back_out_of_the_guard() {
        let flow = guard(GuardCause::Disconnect, fact("a", TransactionState::Aborted), Vec::new());
        assert!(flow.allows(GuardDecision::Rollback));
        assert!(!flow.allows(GuardDecision::Commit), "中止事务里提交与数据库规则矛盾");
        assert_eq!(flow.remaining(), 0);
        let mut flow = flow;
        flow.pending = Some(GuardDecision::Rollback);
        assert_eq!(flow.accept(TransactionEffect::RolledBack), GuardStep::Complete(GuardFollowup::Disconnect("a".to_owned())));
    }

    /// 真实 SQLite 文件：开始、保存点、回滚到保存点、提交都经控件入口执行并登记同一快照。
    #[tokio::test]
    async fn control_commands_execute_on_the_shared_business_session() {
        let dir = std::env::temp_dir().join(format!("rillsql-tx-ui-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let database = dir.join("notes.sqlite");
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).create_if_missing(true))
            .await
            .unwrap();
        sqlx::query("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)").execute(&pool).await.unwrap();
        pool.close().await;
        let service = Arc::new(Service::new_for_test(dir.join("connections.sqlite")));
        service.open_sqlite_for_test("notes", "笔记", &database).await.unwrap();
        let target = TransactionTarget {
            connection_id: "notes".to_owned(),
            connection_name: "笔记".to_owned(),
            engine: "sqlite".to_owned(),
            database: "main".to_owned(),
        };

        assert_eq!(execute(&service, &target, &TransactionCommand::Begin).await.unwrap(), TransactionEffect::Began);
        let snapshot = service.transaction_snapshot("notes").unwrap();
        assert_eq!(snapshot.state, TransactionState::Active);
        assert_eq!(snapshot.generation, 1);

        assert_eq!(
            execute(&service, &target, &TransactionCommand::Savepoint("sp1".to_owned())).await.unwrap(),
            TransactionEffect::StillActive
        );
        assert_eq!(service.transaction_snapshot("notes").unwrap().savepoints.len(), 1);

        assert_eq!(
            execute(&service, &target, &TransactionCommand::RollbackToSavepoint("sp1".to_owned())).await.unwrap(),
            TransactionEffect::StillActive
        );
        assert_eq!(execute(&service, &target, &TransactionCommand::Rollback).await.unwrap(), TransactionEffect::RolledBack);
        let snapshot = service.transaction_snapshot("notes").unwrap();
        assert_eq!(snapshot.state, TransactionState::Idle);
        assert!(snapshot.savepoints.is_empty());

        // 事务模式设置：SQLite 的模式随下一次 BEGIN 生效，不发语句但登记到共享快照。
        assert_eq!(
            execute(&service, &target, &TransactionCommand::Mode(Some("immediate".to_owned()))).await.unwrap(),
            TransactionEffect::Unchanged
        );
        assert_eq!(service.transaction_snapshot("notes").unwrap().settings.mode.as_deref(), Some("immediate"));
        assert_eq!(execute(&service, &target, &TransactionCommand::Begin).await.unwrap(), TransactionEffect::Began);
        assert_eq!(service.transaction_snapshot("notes").unwrap().settings.mode.as_deref(), Some("immediate"));
        let _ = execute(&service, &target, &TransactionCommand::Rollback).await;

        // 没有活动事务时提交不得伪造成功；控件显示失败而快照保持 Idle。
        let refused = execute(&service, &target, &TransactionCommand::Commit).await;
        assert!(refused.is_err(), "无活动事务时提交必须失败：{refused:?}");
        assert_eq!(service.transaction_snapshot("notes").unwrap().state, TransactionState::Idle);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
