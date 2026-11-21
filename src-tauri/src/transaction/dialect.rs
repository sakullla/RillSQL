//! 事务控制的方言接口：能力边界、控制语句 SQL 与手写语句分类。（ADR-2、ADR-3）
//!
//! 这里只描述“方言允许什么、生成什么 SQL、一句话的意图是什么”；语句执行、错误分类和状态观测
//! 由适配器与协调器完成。能力表是界面入口、模型校验和适配器实现的同一份事实来源：
//! 未列出的隔离级别、保存点释放、隐式提交与链式事务都必须明确拒绝，不能静默替换。

use super::model::{ControlStatement, SavepointName};

/// 标准隔离级别的稳定 id。
pub const READ_UNCOMMITTED: &str = "read_uncommitted";
pub const READ_COMMITTED: &str = "read_committed";
pub const REPEATABLE_READ: &str = "repeatable_read";
pub const SERIALIZABLE: &str = "serializable";
/// SQL Server 的快照隔离。
pub const SNAPSHOT: &str = "snapshot";
/// MySQL/MariaDB 的事务访问模式。
pub const READ_ONLY: &str = "read_only";
pub const READ_WRITE: &str = "read_write";

/// 一个可选的隔离级别或事务模式。`id` 是稳定标识（持久化与比较），`label` 是英文回退文案，
/// 界面文案由 i18n 按 `id` 提供。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingOption {
    pub id: &'static str,
    pub label: &'static str,
}

/// 某类设置在什么时机允许改变。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingChange {
    /// 方言不支持这类设置。
    Unsupported,
    /// 只能在没有活动事务时设置。
    WhileIdle,
    /// 活动事务中也可以设置，由方言保证不会隐式提交当前事务。
    AnyTime,
}

/// 一类设置的可用选项与设置时机。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingSupport {
    pub options: &'static [SettingOption],
    pub change: SettingChange,
}

impl SettingSupport {
    /// 完全不支持这类设置。
    pub const NONE: Self = Self { options: &[], change: SettingChange::Unsupported };
}

/// 隐式提交能力。只有 MySQL/MariaDB 家族会把 DDL 等语句变成隐式提交。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImplicitCommitSupport {
    /// 该方言不会隐式提交事务。
    NotApplicable,
    /// MySQL/MariaDB 家族：按语句实际类型判断是否隐式提交。
    MysqlFamily,
}

/// 活动事务中再次执行开始语句时数据库的真实行为。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DuplicateBegin {
    /// 隐式提交旧事务并开始新事务（MySQL/MariaDB 的 `BEGIN`、`START TRANSACTION`）。
    EndsPrevious,
    /// 增加事务层数，一次提交不会结束外层事务（SQL Server 的 `@@TRANCOUNT`）。
    Nests,
    /// 警告并忽略，事务身份不变（PostgreSQL）。
    Ignored,
    /// 明确拒绝（SQLite 的 “cannot start a transaction within a transaction”）。
    Rejected,
}

/// 语句错误后事务进入中止状态时的恢复能力。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbortedRecovery {
    /// 该方言不会把事务置为中止状态（MySQL/MariaDB、SQLite）。
    NotApplicable,
    /// 中止后可以回滚到保存点恢复（PostgreSQL）。
    SavepointRecovery,
    /// 中止后只能整体回滚（SQL Server 的 `XACT_STATE() = -1`）。
    RollbackOnly,
}

/// 方言提供的事务能力边界。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransactionCapabilities {
    /// 连接是否提供显式事务控制。关系型为 true；没有事务控制的方言为 false。
    pub explicit_control: bool,
    pub savepoints: bool,
    /// 是否提供 `RELEASE SAVEPOINT`（SQL Server 只有 `SAVE TRANSACTION`，没有释放）。
    pub release_savepoint: bool,
    /// 是否支持 `COMMIT/ROLLBACK AND CHAIN`。
    pub chained_transactions: bool,
    pub duplicate_begin: DuplicateBegin,
    pub aborted: AbortedRecovery,
    pub isolation: SettingSupport,
    pub mode: SettingSupport,
    pub implicit_commit: ImplicitCommitSupport,
}

impl TransactionCapabilities {
    /// 非事务方言（Redis、MongoDB 或引擎未知）：什么都不支持。
    pub const UNSUPPORTED: Self = Self {
        explicit_control: false,
        savepoints: false,
        release_savepoint: false,
        chained_transactions: false,
        duplicate_begin: DuplicateBegin::Rejected,
        aborted: AbortedRecovery::NotApplicable,
        isolation: SettingSupport::NONE,
        mode: SettingSupport::NONE,
        implicit_commit: ImplicitCommitSupport::NotApplicable,
    };
}

/// 控制语句 SQL 的生成结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlSql {
    /// 需要下发的语句。
    Statement(String),
    /// 不需要下发语句：选择与当前状态一致，或由方言在开始事务时表达。
    NoStatement,
    /// 该方言不支持这个操作。
    Unsupported,
}

/// 手写语句的分类结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlMatch {
    /// 明确的事务控制语句，执行后需要按 [`ControlStatement`] 确认状态。
    Control(ControlStatement),
    /// 不是事务控制语句，按普通语句执行。
    Ordinary,
    /// 以事务控制关键字开头但形态超出可确认范围（多条语句、过程、动态 SQL、未识别修饰语等）：
    /// 执行后必须按未知处理，不能当普通语句忽略，也不能据此推断成功。
    Ambiguous,
}

/// 隐式提交判定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImplicitCommitVerdict {
    /// 该方言不会隐式提交，或不在本需求范围内。
    NotApplicable,
    /// 该语句会隐式提交当前事务。
    Yes,
    /// 可能隐式提交（过程体、动态 SQL、未分类语句）：不宣称发生，必要时按未知处理。
    Uncertain,
}

/// 各方言共有的控制语句词汇差异。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlSyntax {
    /// 单独的 `BEGIN` 是开始事务。T-SQL 的 `BEGIN` 是块开始，必须写 `BEGIN TRAN`。
    pub bare_begin: bool,
    /// 接受 `START TRANSACTION`（MySQL 家族、PostgreSQL）。
    pub start_transaction: bool,
    /// 接受 `END` / `END TRANSACTION` 作为提交（PostgreSQL、SQLite）。
    pub end_commits: bool,
    /// 接受 `ABORT` 作为回滚（PostgreSQL）。
    pub abort_rolls_back: bool,
    /// 接受 `AND [NO] CHAIN`。
    pub chained: bool,
    /// `BEGIN` 后可直接使用的模式词（SQLite 的 `DEFERRED`/`IMMEDIATE`/`EXCLUSIVE`）。
    pub begin_modes: &'static [&'static str],
    /// `ROLLBACK TRAN[SACTION] <name>` 是回滚到保存点（T-SQL）。
    pub rollback_transaction_name: bool,
    /// `#` 起行注释（MySQL 家族）。
    pub hash_comments: bool,
    /// 引号标识符的开闭符号（标准 SQL 只有双引号；方言由适配器补充反引号、方括号）。
    pub identifier_quotes: &'static [(char, char)],
}

impl ControlSyntax {
    /// 标准 SQL：`BEGIN` 开始，没有 T-SQL 与 MySQL 专有写法。
    pub const STANDARD: Self = Self {
        bare_begin: true,
        start_transaction: true,
        end_commits: true,
        abort_rolls_back: false,
        chained: false,
        begin_modes: &[],
        rollback_transaction_name: false,
        hash_comments: false,
        identifier_quotes: &[('"', '"')],
    };
}

/// 一个方言对事务控制的全部认识。
pub trait TransactionDialect: Sync {
    fn capabilities(&self) -> TransactionCapabilities;

    fn syntax(&self) -> ControlSyntax;

    /// 标识符引用（保存点名称）。不能用值绑定代替标识符。
    fn quote_identifier(&self, name: &str) -> String;

    fn begin_sql(&self, mode: Option<&str>) -> String;

    fn commit_sql(&self, chain: bool) -> String;

    fn rollback_sql(&self, chain: bool) -> String;

    /// 创建保存点的语句。必须由方言实现：标准写法与 `SAVE TRANSACTION` 不能互相替代。
    fn savepoint_sql(&self, name: &SavepointName) -> String;

    /// 回滚到保存点的语句。必须由方言实现（T-SQL 的 `ROLLBACK TRANSACTION <name>`）。
    fn rollback_to_savepoint_sql(&self, name: &SavepointName) -> String;

    /// 释放保存点。方言在能力表里没有释放入口时返回 [`ControlSql::Unsupported`]。
    fn release_savepoint_sql(&self, name: &SavepointName) -> ControlSql {
        if !self.capabilities().release_savepoint {
            return ControlSql::Unsupported;
        }
        ControlSql::Statement(format!("RELEASE SAVEPOINT {}", self.quote_identifier(name.as_str())))
    }

    /// 设置隔离级别的语句。`None` 表示恢复服务器默认。
    fn isolation_sql(&self, level: Option<&str>) -> ControlSql;

    /// 设置事务模式的语句。`None` 表示恢复方言默认。
    fn mode_sql(&self, mode: Option<&str>) -> ControlSql;

    /// 一条控制命令对应的 SQL。能力表之外的操作返回 [`ControlSql::Unsupported`]，
    /// 调用方不得为了继续执行而改用别的会话或静默降级。
    fn sql_for(&self, statement: &ControlStatement) -> ControlSql {
        let capabilities = self.capabilities();
        // 没有显式事务控制的方言不生成任何控制语句。
        if !capabilities.explicit_control {
            return ControlSql::Unsupported;
        }
        match statement {
            ControlStatement::Begin { mode } => match mode {
                Some(mode) if !supports_option(capabilities.mode, mode) => ControlSql::Unsupported,
                _ => ControlSql::Statement(self.begin_sql(mode.as_deref())),
            },
            ControlStatement::Commit { chain } => {
                if *chain && !capabilities.chained_transactions {
                    return ControlSql::Unsupported;
                }
                ControlSql::Statement(self.commit_sql(*chain))
            }
            ControlStatement::Rollback { chain } => {
                if *chain && !capabilities.chained_transactions {
                    return ControlSql::Unsupported;
                }
                ControlSql::Statement(self.rollback_sql(*chain))
            }
            ControlStatement::Savepoint(name) => {
                if capabilities.savepoints {
                    ControlSql::Statement(self.savepoint_sql(name))
                } else {
                    ControlSql::Unsupported
                }
            }
            ControlStatement::RollbackToSavepoint(name) => {
                if capabilities.savepoints {
                    ControlSql::Statement(self.rollback_to_savepoint_sql(name))
                } else {
                    ControlSql::Unsupported
                }
            }
            ControlStatement::ReleaseSavepoint(name) => self.release_savepoint_sql(name),
            ControlStatement::SetIsolation { level } => setting_sql(capabilities.isolation, level.as_deref(), |level| self.isolation_sql(level)),
            ControlStatement::SetMode { mode } => setting_sql(capabilities.mode, mode.as_deref(), |mode| self.mode_sql(mode)),
        }
    }

    /// 分类一条手写语句。调用方负责按方言切分出单条语句；传入多条语句时返回
    /// [`ControlMatch::Ambiguous`]，不能当成单条控制语句处理。
    fn classify_control(&self, body: &str) -> ControlMatch {
        classify_control(body, self.syntax())
    }

    /// 判断一条语句是否会隐式提交。默认只按能力表保守回答，真实分类由适配器覆盖。
    fn implicit_commit(&self, _body: &str) -> ImplicitCommitVerdict {
        match self.capabilities().implicit_commit {
            ImplicitCommitSupport::NotApplicable => ImplicitCommitVerdict::NotApplicable,
            ImplicitCommitSupport::MysqlFamily => ImplicitCommitVerdict::Uncertain,
        }
    }
}

fn supports_option(support: SettingSupport, value: &str) -> bool {
    support.change != SettingChange::Unsupported && support.options.iter().any(|option| option.id == value)
}

/// 设置语句只在方言声明了这类设置、且值在选项内时生成。
fn setting_sql(support: SettingSupport, value: Option<&str>, sql: impl FnOnce(Option<&str>) -> ControlSql) -> ControlSql {
    if support.change == SettingChange::Unsupported {
        return ControlSql::Unsupported;
    }
    if let Some(value) = value {
        if !supports_option(support, value) {
            return ControlSql::Unsupported;
        }
    }
    sql(value)
}

/// 按 [`ControlSyntax`] 分类一条手写语句。
///
/// 只做保守的前缀识别：识别不了的形态返回 [`ControlMatch::Ambiguous`]，普通 SQL 返回
/// [`ControlMatch::Ordinary`]。分类结果不改变任何事务状态——必须执行并确认后才能推进。
pub fn classify_control(body: &str, syntax: ControlSyntax) -> ControlMatch {
    let code = strip_comments(body, syntax.hash_comments, syntax.identifier_quotes);
    let code = code.trim();
    // 去掉所有开头/结尾的空语句与分号（`COMMIT;;`、`; COMMIT` 都是单条语句）；
    // 中间的分号说明传入了多条语句。
    let code = code.trim_start_matches(|ch: char| ch == ';' || ch.is_whitespace());
    let code = code.trim_end_matches(|ch: char| ch == ';' || ch.is_whitespace());
    let (tokens, multiple) = tokenize(code, syntax.identifier_quotes);
    if multiple {
        return ControlMatch::Ambiguous;
    }
    let Some(first) = tokens.first() else { return ControlMatch::Ordinary };
    if first.quoted || first.text.is_empty() {
        return ControlMatch::Ordinary;
    }
    let head = first.upper.as_str();
    let rest = &tokens[1..];
    match head {
        "BEGIN" => classify_begin(rest, syntax),
        "START" if syntax.start_transaction && keyword(rest.first(), "TRANSACTION") => match rest.len() {
            1 => ControlMatch::Control(ControlStatement::Begin { mode: None }),
            _ => ControlMatch::Ambiguous,
        },
        "COMMIT" => match finish_tail(rest, syntax) {
            Some(chain) => ControlMatch::Control(ControlStatement::Commit { chain }),
            None => ControlMatch::Ambiguous,
        },
        "END" if syntax.end_commits => match finish_tail(rest, syntax) {
            Some(chain) => ControlMatch::Control(ControlStatement::Commit { chain }),
            None => ControlMatch::Ambiguous,
        },
        "ABORT" if syntax.abort_rolls_back => match finish_tail(rest, syntax) {
            Some(chain) => ControlMatch::Control(ControlStatement::Rollback { chain }),
            None => ControlMatch::Ambiguous,
        },
        "ROLLBACK" => classify_rollback(rest, syntax),
        "SAVEPOINT" => match savepoint_name(rest) {
            Some(name) => ControlMatch::Control(ControlStatement::Savepoint(name)),
            None => ControlMatch::Ambiguous,
        },
        "RELEASE" => {
            let name = if keyword(rest.first(), "SAVEPOINT") { savepoint_name(&rest[1..]) } else { savepoint_name(rest) };
            match name {
                Some(name) => ControlMatch::Control(ControlStatement::ReleaseSavepoint(name)),
                None => ControlMatch::Ambiguous,
            }
        }
        "SET" => classify_set(rest),
        _ => ControlMatch::Ordinary,
    }
}

fn classify_begin(rest: &[Token], syntax: ControlSyntax) -> ControlMatch {
    let Some(first) = rest.first() else {
        return if syntax.bare_begin { ControlMatch::Control(ControlStatement::Begin { mode: None }) } else { ControlMatch::Ordinary };
    };
    if first.quoted {
        return ControlMatch::Ordinary;
    }
    match first.upper.as_str() {
        "WORK" | "TRANSACTION" | "TRAN" if rest.len() == 1 => ControlMatch::Control(ControlStatement::Begin { mode: None }),
        "WORK" | "TRANSACTION" | "TRAN" => ControlMatch::Ambiguous,
        mode if syntax.bare_begin && syntax.begin_modes.iter().any(|known| known.eq_ignore_ascii_case(mode)) => {
            if rest.len() == 1 {
                ControlMatch::Control(ControlStatement::Begin { mode: Some(mode.to_ascii_lowercase()) })
            } else {
                ControlMatch::Ambiguous
            }
        }
        // `BEGIN ISOLATION LEVEL ...` 等工作单元选项超出可确认范围，不能只按“成功”推进。
        _ if syntax.bare_begin => ControlMatch::Ambiguous,
        // T-SQL 的 BEGIN…END 块等不是事务控制。
        _ => ControlMatch::Ordinary,
    }
}

fn classify_rollback(rest: &[Token], syntax: ControlSyntax) -> ControlMatch {
    if rest.is_empty() || (rest.len() == 1 && is_work_unit(&rest[0])) {
        return ControlMatch::Control(ControlStatement::Rollback { chain: false });
    }
    if keyword(rest.first(), "TO") {
        let tail = if keyword(rest.get(1), "SAVEPOINT") { &rest[2..] } else { &rest[1..] };
        return match savepoint_name(tail) {
            Some(name) => ControlMatch::Control(ControlStatement::RollbackToSavepoint(name)),
            None => ControlMatch::Ambiguous,
        };
    }
    if keyword(rest.first(), "AND") {
        return match finish_tail(rest, syntax) {
            Some(chain) => ControlMatch::Control(ControlStatement::Rollback { chain }),
            None => ControlMatch::Ambiguous,
        };
    }
    if syntax.rollback_transaction_name && (keyword(rest.first(), "TRANSACTION") || keyword(rest.first(), "TRAN")) {
        return match savepoint_name(&rest[1..]) {
            Some(name) => ControlMatch::Control(ControlStatement::RollbackToSavepoint(name)),
            None => ControlMatch::Control(ControlStatement::Rollback { chain: false }),
        };
    }
    // 其余形态（含带名字但方言不认的写法）不能确认实际效果。
    ControlMatch::Ambiguous
}

/// `COMMIT`/`ROLLBACK`/`ABORT` 之后的修饰语：只有工作单元词与 `AND [NO] CHAIN` 可确认。
fn finish_tail(rest: &[Token], syntax: ControlSyntax) -> Option<bool> {
    match rest {
        [] => Some(false),
        [single] if is_work_unit(single) => Some(false),
        [and, chain] if syntax.chained && keyword(Some(and), "AND") && keyword(Some(chain), "CHAIN") => Some(true),
        [and, no, chain] if syntax.chained && keyword(Some(and), "AND") && keyword(Some(no), "NO") && keyword(Some(chain), "CHAIN") => {
            Some(false)
        }
        _ => None,
    }
}

fn classify_set(rest: &[Token]) -> ControlMatch {
    let words: Vec<&str> = rest.iter().map(|token| token.upper.as_str()).collect();
    let level_tail = match words.as_slice() {
        ["SESSION", "CHARACTERISTICS", "AS", "TRANSACTION", "ISOLATION", "LEVEL", ..] => Some(&words[6..]),
        ["SESSION", "TRANSACTION", "ISOLATION", "LEVEL", ..] => Some(&words[4..]),
        ["TRANSACTION", "ISOLATION", "LEVEL", ..] => Some(&words[3..]),
        _ => None,
    };
    if let Some(tail) = level_tail {
        return match isolation_id(tail) {
            Some(level) => ControlMatch::Control(ControlStatement::SetIsolation { level }),
            // 未列出的级别、缺少级别或写法超出可确认范围：执行后不能宣称已生效。
            None => ControlMatch::Ambiguous,
        };
    }
    let mode = match words.as_slice() {
        ["SESSION", "TRANSACTION", "READ", "ONLY"] | ["TRANSACTION", "READ", "ONLY"] => Some(READ_ONLY),
        ["SESSION", "TRANSACTION", "READ", "WRITE"] | ["TRANSACTION", "READ", "WRITE"] => Some(READ_WRITE),
        _ => None,
    };
    if let Some(mode) = mode {
        return ControlMatch::Control(ControlStatement::SetMode { mode: Some(mode.to_owned()) });
    }
    // 改变事务边界（autocommit、隐式事务）的设置不在本控制模型内：执行后只能按未知处理。
    let changes_boundary = rest.iter().any(|token| {
        !token.quoted && (token.upper.contains("AUTOCOMMIT") || token.upper == "IMPLICIT_TRANSACTIONS")
    });
    if changes_boundary {
        ControlMatch::Ambiguous
    } else {
        ControlMatch::Ordinary
    }
}

fn isolation_id(words: &[&str]) -> Option<Option<String>> {
    match words {
        ["DEFAULT"] => Some(None),
        ["READ", "UNCOMMITTED"] => Some(Some(READ_UNCOMMITTED.to_owned())),
        ["READ", "COMMITTED"] => Some(Some(READ_COMMITTED.to_owned())),
        ["REPEATABLE", "READ"] => Some(Some(REPEATABLE_READ.to_owned())),
        ["SERIALIZABLE"] => Some(Some(SERIALIZABLE.to_owned())),
        ["SNAPSHOT"] => Some(Some(SNAPSHOT.to_owned())),
        _ => None,
    }
}

fn is_work_unit(token: &Token) -> bool {
    !token.quoted && matches!(token.upper.as_str(), "WORK" | "TRANSACTION" | "TRAN")
}

fn keyword(token: Option<&Token>, word: &str) -> bool {
    matches!(token, Some(token) if !token.quoted && token.upper == word)
}

fn savepoint_name(tokens: &[Token]) -> Option<SavepointName> {
    match tokens {
        [token] => SavepointName::parse(&token.text).ok(),
        _ => None,
    }
}

/// 控制分类用的代码词。括号、引号标识符与字符串字面量都按引用标记，不当作关键字。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Token {
    /// 大写的词（关键字比较用）。
    upper: String,
    /// 原始文本（引号标识符去掉引号）。
    text: String,
    /// 引号标识符、字符串字面量与括号：不是关键字。
    quoted: bool,
}

/// 切出代码词，并报告是否出现语句分隔符（说明传入了多条语句）。
///
/// 循环保证每次都消耗至少一个字符：分隔符被显式消费，无法识别的字符按单个词消耗。
fn tokenize(code: &str, quotes: &[(char, char)]) -> (Vec<Token>, bool) {
    let mut tokens = Vec::new();
    let mut multiple = false;
    let mut rest = code.trim_start();
    while !rest.is_empty() {
        let first = rest.chars().next().unwrap();
        if let Some((open, close)) = quotes.iter().copied().find(|(open, _)| *open == first) {
            let (value, used) = read_quoted(&rest[open.len_utf8()..], close);
            tokens.push(Token { upper: value.to_uppercase(), text: value, quoted: true });
            rest = &rest[open.len_utf8()..][used..];
        } else if first == '\'' {
            let (value, used) = read_quoted(&rest[1..], '\'');
            tokens.push(Token { upper: value.to_uppercase(), text: value, quoted: true });
            rest = &rest[1..][used..];
        } else if first == ';' {
            multiple = true;
            rest = &rest[1..];
        } else if matches!(first, '(' | ')' | ',' | '=') {
            tokens.push(Token { upper: first.to_string(), text: first.to_string(), quoted: true });
            rest = &rest[first.len_utf8()..];
        } else {
            let end = rest.find(|ch: char| ch.is_whitespace() || matches!(ch, ';' | '(' | ')' | ',' | '=')).unwrap_or(rest.len());
            if end == 0 {
                // 理论上不会发生（上面的分支已覆盖分隔符）；仍然显式消费一个字符，保证不挂死。
                tokens.push(Token { upper: first.to_string(), text: first.to_string(), quoted: true });
                rest = &rest[first.len_utf8()..];
            } else {
                let value = &rest[..end];
                tokens.push(Token { upper: value.to_ascii_uppercase(), text: value.to_owned(), quoted: false });
                rest = &rest[end..];
            }
        }
        rest = rest.trim_start();
    }
    (tokens, multiple)
}

/// 读取引号内文本，返回去转义后的值与消耗的字节数（含结束引号）。
fn read_quoted(inner: &str, close: char) -> (String, usize) {
    let mut value = String::new();
    let mut chars = inner.char_indices();
    while let Some((index, ch)) = chars.next() {
        if ch == close {
            if let Some((next_index, next)) = chars.next() {
                if next == close {
                    value.push(close);
                    continue;
                }
                return (value, next_index);
            }
            return (value, index + ch.len_utf8());
        }
        value.push(ch);
    }
    // 没有结束引号：整段都算引号内容，调用方按无法确认处理。
    (value, inner.len())
}

/// 去掉注释（字符串与引号标识符内保留）。分类只关心事务控制前缀，注释不构成语义。
fn strip_comments(code: &str, hash: bool, quotes: &[(char, char)]) -> String {
    let mut out = String::with_capacity(code.len());
    let mut chars = code.char_indices();
    while let Some((index, ch)) = chars.next() {
        let close = if ch == '\'' { Some('\'') } else { quotes.iter().copied().find(|(open, _)| *open == ch).map(|(_, close)| close) };
        if let Some(close) = close {
            out.push(ch);
            for (_, inner) in chars.by_ref() {
                out.push(inner);
                if inner == close {
                    break;
                }
            }
        } else if code[index..].starts_with("--") || (hash && ch == '#') {
            // 行注释：换行保留，其余丢弃。
            for (_, inner) in chars.by_ref() {
                if inner == '\n' {
                    out.push('\n');
                    break;
                }
            }
        } else if code[index..].starts_with("/*") {
            chars.next();
            let mut previous = '\0';
            for (_, inner) in chars.by_ref() {
                if previous == '*' && inner == '/' {
                    break;
                }
                previous = inner;
            }
            out.push(' ');
        } else {
            out.push(ch);
        }
    }
    out
}
