//! 执行前风险分类：按语句的**实际后果**决定要不要在执行前确认。
//!
//! 这里只回答「这条语句会造成什么后果」，不生成提示文案、不执行、不碰会话：
//! 切分复用 split.rs（不重复实现语句切分），结论交给 query/desk 的执行前决策，
//! 文案由视图按界面语言渲染。只读语句不产生任何后果，直接直通。
//!
//! 方言差异只经过适配器已经暴露的词法特征判断（crate::adapters::query），不写死引擎名与方言关键字：
//! 适配器边界见 adapters/boundary_tests.rs。

use super::split;

/// 后果分类。按后果而不是关键词分档：同一条语句可能同时命中多项
/// （例如会隐式提交的方言里，删表既丢数据又提交事务）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RiskKind {
    /// 数据丢失：删表/清空表、没有 WHERE 的 DELETE、删除列或分区。
    DataLoss,
    /// 对象替换：按行替换、就地重建视图/函数，或同一批里先删后建同名对象。
    ObjectReplace,
    /// 大量重写：整表 UPDATE、需要重建表的 ALTER、重写整库的维护命令。
    MassRewrite,
    /// 隐式提交：该方言的 DDL 会提交当前事务，已执行的步骤无法回滚。
    ImplicitCommit,
    /// 外部调度状态：服务器按时间自动执行的任务或调度器开关（事件、调度器变量）。
    ExternalSchedule,
    /// 其它服务端/连接状态：权限、服务端变量、会话中断、索引与统计信息等。
    ServerState,
}

/// 一条语句上的一项后果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Consequence {
    pub kind: RiskKind,
    /// 受影响的对象或服务端变量名；判断不出来时为空。
    pub object: Option<String>,
}

/// 需要确认的一条语句。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RiskStatement {
    /// 在这批 SQL 里的序号（0 基，按切分顺序），与执行消息页的「第几条」一致。
    pub index: usize,
    pub sql: String,
    pub consequences: Vec<Consequence>,
}

/// 一批 SQL 的风险评估结果。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RiskReport {
    /// 需要确认的语句，按出现顺序；只读和无风险语句不在这里。
    pub statements: Vec<RiskStatement>,
    /// 直通执行的语句数（只读，或没有可判定的后果）。
    pub passthrough: usize,
    /// 评估过程中的说明（切分被拒绝、多步 DDL 的隐式提交等）。
    pub notices: Vec<String>,
}

impl RiskReport {
    /// 有任何一条语句需要确认。
    pub fn needs_confirmation(&self) -> bool {
        !self.statements.is_empty()
    }
}

/// 该方言的 DDL 是否会隐式提交当前事务。
///
/// 只从适配器暴露的词法特征推导：哈希行注释与可执行注释属于同一族方言，
/// 而这一族的 DDL 会隐式提交、逐条执行无法整体回滚。查询层不写死引擎名。
fn ddl_implicitly_commits(engine: &str) -> bool {
    let lexis = crate::adapters::query::lexis_named(engine);
    lexis.hash_comments && lexis.executable_comments
}

/// 评估一批 SQL。
///
/// 切分沿用 split_sql：多语句脚本逐条评估，只读语句计入直通数。
/// 切分被拒绝时按整段评估（宁可多确认一次，也不放过没看清的语句）。
pub fn assess(engine: &str, sql: &str) -> RiskReport {
    let mut report = RiskReport::default();
    let split_report = split::split_sql(engine, sql);
    let refused = split_report.refusal.is_some();
    let pieces: Vec<String> = if let Some(refusal) = split_report.refusal.clone() {
        report.notices.push(format!("切分被拒绝（{refusal}），按整段语句评估风险。"));
        vec![sql.to_owned()]
    } else {
        split_report.pieces.into_iter().map(|piece| piece.text).collect()
    };
    if pieces.is_empty() {
        return report;
    }
    let mut consequences: Vec<Vec<Consequence>> = pieces.iter().map(|text| classify(engine, text)).collect();
    mark_replacements(engine, &pieces, &mut consequences);
    let ddl_batch = ddl_implicitly_commits(engine)
        && !refused
        && pieces.len() > 1
        && pieces.iter().any(|text| is_ddl(&split::first_keyword(text)));
    for (index, (text, items)) in pieces.iter().zip(consequences).enumerate() {
        if items.is_empty() {
            report.passthrough += 1;
        } else {
            report.statements.push(RiskStatement { index, sql: text.clone(), consequences: items });
        }
    }
    if ddl_batch {
        report
            .notices
            .push("这批语句逐条执行；该方言的 DDL 会隐式提交前面的语句，中途失败不能整体回滚。".to_owned());
    }
    report
}

/// 单条语句的后果。
fn classify(engine: &str, text: &str) -> Vec<Consequence> {
    if readonly(engine, text) {
        return Vec::new();
    }
    let words = Words::of(engine, text);
    let lead = words.lead();
    // EXPLAIN ANALYZE 与 WITH ... (DELETE ...) 会真的执行写操作：按内层语句的后果算。
    if lead == "EXPLAIN" || lead == "WITH" {
        return match inner_write(engine, text) {
            Some((inner, at)) => classify_at(engine, &inner, &text[at..]),
            None => Vec::new(),
        };
    }
    classify_at(engine, &lead, text)
}

/// 按开头关键字与顶层词分档。inner 是参与判断的语句文本（内层写语句时为它的后半段）。
fn classify_at(engine: &str, lead: &str, inner: &str) -> Vec<Consequence> {
    let words = Words::of(engine, inner);
    let text = inner;
    let mut out: Vec<Consequence> = Vec::new();
    match lead {
        "DROP" => {
            let kind = words.get(1).unwrap_or_default().to_owned();
            let name = object_after(text, &words, 1, OBJECT_PREFIX_SKIP);
            // 删索引/统计信息不改数据；删账号/角色改的是服务端权限。
            if matches!(kind.as_str(), "INDEX" | "STATISTICS" | "USER" | "LOGIN" | "ROLE" | "SERVER") {
                out.push(consequence(RiskKind::ServerState, name));
            } else {
                out.push(consequence(RiskKind::DataLoss, name));
            }
        }
        "TRUNCATE" => {
            let index = if words.get(1) == Some("TABLE") { 1 } else { 0 };
            out.push(consequence(RiskKind::DataLoss, object_after(text, &words, index, OBJECT_PREFIX_SKIP)));
        }
        "DELETE" => {
            // 没有 WHERE 的 DELETE 清掉整张表；带 WHERE 的按行删除属于常规写入。
            if !words.has("WHERE") {
                let index = if words.get(1) == Some("FROM") { 1 } else { 0 };
                out.push(consequence(RiskKind::DataLoss, object_after(text, &words, index, OBJECT_PREFIX_SKIP)));
            }
        }
        "UPDATE" => {
            if !words.has("WHERE") {
                out.push(consequence(RiskKind::MassRewrite, object_after(text, &words, 0, OBJECT_PREFIX_SKIP)));
            }
        }
        "REPLACE" => out.push(consequence(RiskKind::ObjectReplace, object_after(text, &words, 0, OBJECT_PREFIX_SKIP))),
        "MERGE" => out.push(consequence(RiskKind::MassRewrite, object_after(text, &words, 0, OBJECT_PREFIX_SKIP))),
        "RENAME" => out.push(consequence(RiskKind::ServerState, object_after(text, &words, 0, OBJECT_PREFIX_SKIP))),
        "LOCK" | "UNLOCK" => out.push(consequence(RiskKind::ServerState, None)),
        "ALTER" => out.extend(alter_consequences(&words)),
        "CREATE" => {
            // CREATE OR REPLACE / CREATE OR ALTER 都是就地替换已有对象。
            if words.has("OR") && (words.has("REPLACE") || words.has("ALTER")) {
                let index = words.index_of("REPLACE").or_else(|| words.index_of("ALTER")).unwrap_or(0);
                out.push(consequence(RiskKind::ObjectReplace, object_after(text, &words, index, OBJECT_PREFIX_SKIP)));
            }
            if words.get(1) == Some("EVENT") {
                out.push(consequence(RiskKind::ExternalSchedule, object_after(text, &words, 1, OBJECT_PREFIX_SKIP)));
            }
        }
        "SET" => out.extend(set_consequences(text, &words)),
        "GRANT" | "REVOKE" => out.push(consequence(RiskKind::ServerState, None)),
        // VACUUM 会重写或回收整表/整库的数据文件：维护命令，不是普通查询。
        "VACUUM" => out.push(consequence(RiskKind::MassRewrite, object_after(text, &words, 0, OBJECT_PREFIX_SKIP))),
        "OPTIMIZE" => out.push(consequence(RiskKind::MassRewrite, object_after(text, &words, 0, OBJECT_PREFIX_SKIP))),
        "CLUSTER" => out.push(consequence(RiskKind::MassRewrite, object_after(text, &words, 0, OBJECT_PREFIX_SKIP))),
        "REINDEX" => out.push(consequence(RiskKind::ServerState, object_after(text, &words, 0, OBJECT_PREFIX_SKIP))),
        "ATTACH" | "DETACH" => out.push(consequence(RiskKind::ServerState, object_after(text, &words, 0, OBJECT_PREFIX_SKIP))),
        "EXEC" | "EXECUTE" => {
            let proc = object_after(text, &words, 0, OBJECT_PREFIX_SKIP);
            if !proc.as_deref().is_some_and(read_only_procedure) {
                out.push(consequence(RiskKind::ServerState, proc));
            }
        }
        "CALL" => out.push(consequence(RiskKind::ServerState, object_after(text, &words, 0, OBJECT_PREFIX_SKIP))),
        "KILL" | "SHUTDOWN" | "FLUSH" | "PURGE" | "RESET" | "DBCC" | "RECONFIGURE" | "CHECKPOINT" | "BACKUP" | "RESTORE" => {
            out.push(consequence(RiskKind::ServerState, None));
        }
        _ => {}
    }
    // DDL 逐条执行且会隐式提交的方言：中途失败时前面已提交的步骤不会回滚。
    if ddl_implicitly_commits(engine) && is_ddl(lead) && !out.is_empty() {
        out.push(consequence(RiskKind::ImplicitCommit, None));
    }
    dedupe(out)
}

fn alter_consequences(words: &Words) -> Vec<Consequence> {
    let text = words.text;
    let kind = words.get(1).unwrap_or_default().to_owned();
    let mut out = Vec::new();
    match kind.as_str() {
        "EVENT" => out.push(consequence(RiskKind::ExternalSchedule, object_after(text, words, 1, OBJECT_PREFIX_SKIP))),
        "SYSTEM" | "DATABASE" | "SCHEMA" | "USER" | "LOGIN" | "ROLE" | "SERVER" => {
            out.push(consequence(RiskKind::ServerState, object_after(text, words, 1, OBJECT_PREFIX_SKIP)))
        }
        "TABLE" => {
            let name = object_after(text, words, 1, OBJECT_PREFIX_SKIP);
            if words.has("DROP") && (words.has("PARTITION") || words.has("COLUMN")) {
                out.push(consequence(RiskKind::DataLoss, name.clone()));
            }
            // 明确指出 INSTANT（不改行格式的原地变更）时不按「重建整表」提醒。
            let instant = words.has("INSTANT");
            let rewrites = !instant
                && (words.has_any(&["MODIFY", "CHANGE", "CONVERT", "ENGINE", "FORCE", "REORGANIZE", "COALESCE", "REBUILD"])
                    || words.adjacent("ALTER", "COLUMN")
                    || (words.has("ADD") && words.has_any(&["PRIMARY", "UNIQUE", "CONSTRAINT", "FOREIGN"])));
            if rewrites {
                out.push(consequence(RiskKind::MassRewrite, name.clone()));
            }
            if words.has("RENAME") {
                out.push(consequence(RiskKind::ServerState, name));
            }
        }
        _ => {}
    }
    out
}

fn set_consequences(text: &str, words: &Words) -> Vec<Consequence> {
    let mut out = Vec::new();
    // 会话级 SET 只影响当前连接；只有服务端级设置才算改动服务器状态。
    let upper = text.to_ascii_uppercase();
    let global = words.has("GLOBAL") || words.has("PERSIST") || upper.contains("@@GLOBAL.");
    if global {
        let scheduler = upper.contains("EVENT_SCHEDULER");
        let kind = if scheduler { RiskKind::ExternalSchedule } else { RiskKind::ServerState };
        out.push(consequence(kind, object_after(text, words, 0, OBJECT_PREFIX_SKIP)));
    }
    out
}

fn consequence(kind: RiskKind, object: Option<String>) -> Consequence {
    Consequence { kind, object }
}

fn dedupe(items: Vec<Consequence>) -> Vec<Consequence> {
    let mut out: Vec<Consequence> = Vec::new();
    for item in items {
        if !out.contains(&item) {
            out.push(item);
        }
    }
    out
}

/// 同一批里 DROP 之后又 CREATE 同名对象：DROP 那一条补上「对象替换」。
fn mark_replacements(engine: &str, pieces: &[String], consequences: &mut [Vec<Consequence>]) {
    let shapes: Vec<(String, Option<String>)> = pieces.iter().map(|text| drop_shape(engine, text)).collect();
    let created: Vec<String> = pieces.iter().filter_map(|text| create_shape(engine, text)).collect();
    for (index, (lead, name)) in shapes.into_iter().enumerate() {
        if lead != "DROP" {
            continue;
        }
        let Some(name) = name else { continue };
        if created.iter().any(|candidate| same_object(candidate, &name)) {
            consequences[index].push(consequence(RiskKind::ObjectReplace, Some(name)));
            consequences[index] = dedupe(std::mem::take(&mut consequences[index]));
        }
    }
}

fn drop_shape(engine: &str, text: &str) -> (String, Option<String>) {
    let words = Words::of(engine, text);
    let lead = words.lead();
    if lead != "DROP" {
        return (lead, None);
    }
    let kind = words.get(1).unwrap_or_default();
    if matches!(kind, "INDEX" | "STATISTICS" | "USER" | "LOGIN" | "ROLE" | "SERVER") {
        return (lead, None);
    }
    (lead, object_after(text, &words, 1, OBJECT_PREFIX_SKIP))
}

fn create_shape(engine: &str, text: &str) -> Option<String> {
    let words = Words::of(engine, text);
    if words.lead() != "CREATE" {
        return None;
    }
    let mut index = 1;
    while matches!(words.get(index), Some("OR") | Some("REPLACE") | Some("TEMPORARY") | Some("UNIQUE")) {
        index += 1;
    }
    if words.get(index) == Some("INDEX") {
        return None;
    }
    object_after(text, &words, index, OBJECT_PREFIX_SKIP)
}

fn same_object(left: &str, right: &str) -> bool {
    let normalize = |value: &str| {
        let trimmed = value.trim_matches(|ch| ch == '"' || ch == '\u{60}' || ch == '[' || ch == ']');
        let last = trimmed.rsplit('.').next().unwrap_or(trimmed);
        last.to_ascii_lowercase()
    };
    !left.is_empty() && normalize(left) == normalize(right)
}

fn is_ddl(lead: &str) -> bool {
    matches!(lead, "DROP" | "ALTER" | "TRUNCATE" | "RENAME" | "OPTIMIZE")
}

/// 这条语句是否只读（不产生任何后果，直接执行）。
fn readonly(engine: &str, text: &str) -> bool {
    let lead = split::first_keyword(text);
    if lead.is_empty() {
        return true;
    }
    // 会话设置、事务控制与只读元数据语句不产生数据后果。
    match lead.as_str() {
        "BEGIN" | "START" | "COMMIT" | "ROLLBACK" | "SAVEPOINT" | "RELEASE" | "ANALYZE" | "HELP" | "USE" => return true,
        "EXPLAIN" => {
            // 只有 EXPLAIN ANALYZE（会真的执行）才继续往内层看。
            let words = Words::of(engine, text);
            if !words.has("ANALYZE") {
                return true;
            }
        }
        "SET" => {
            let words = Words::of(engine, text);
            let upper = text.to_ascii_uppercase();
            let global = words.has("GLOBAL") || words.has("PERSIST") || upper.contains("@@GLOBAL.");
            if !global {
                return true;
            }
        }
        "EXEC" | "EXECUTE" => {
            let words = Words::of(engine, text);
            if object_after(text, &words, 0, OBJECT_PREFIX_SKIP).as_deref().is_some_and(read_only_procedure) {
                return true;
            }
        }
        _ => {}
    }
    if split::is_clearly_read_only(engine, text) {
        return true;
    }
    // 各引擎共有的只读查询写法：VALUES / TABLE 后面不会跟着写入动作。
    if !matches!(lead.as_str(), "VALUES" | "TABLE") {
        return false;
    }
    let dialect = crate::adapters::query::side_effect_words_named(engine);
    let (words, _) = split::top_level_words(engine, text);
    !words.iter().any(|(word, _, _)| {
        split::WRITE_WORDS.contains(&word.as_str()) || dialect.contains(&word.as_str())
    })
}

/// 只读的元数据存储过程：这些名字只读元数据，不修改数据。
/// 其余 EXEC 无法在客户端判断过程体后果，按服务端状态处理。
fn read_only_procedure(name: &str) -> bool {
    let bare = name
        .trim_matches(|ch| ch == '"' || ch == '\u{60}' || ch == '[' || ch == ']')
        .rsplit('.')
        .next()
        .unwrap_or(name)
        .to_ascii_lowercase();
    matches!(
        bare.as_str(),
        "sp_help"
            | "sp_helptext"
            | "sp_helpdb"
            | "sp_helpindex"
            | "sp_helpconstraint"
            | "sp_columns"
            | "sp_tables"
            | "sp_who"
            | "sp_who2"
            | "sp_lock"
            | "sp_spaceused"
            | "sp_databases"
            | "sp_server_info"
            | "xp_msver"
            | "sp_helpfile"
            | "sp_helpuser"
            | "sp_statistics"
    )
}

/// 内层写语句：EXPLAIN ANALYZE DELETE ...、WITH x AS (DELETE ...) SELECT ...
fn inner_write(engine: &str, text: &str) -> Option<(String, usize)> {
    let code = split::all_code_words(engine, text);
    let lead = code.first()?.0.clone();
    if lead != "EXPLAIN" && lead != "WITH" {
        return None;
    }
    code.iter()
        .find(|(word, _, _)| matches!(word.as_str(), "INSERT" | "UPDATE" | "DELETE" | "MERGE" | "REPLACE"))
        .map(|(word, _, start)| (word.clone(), *start))
}

/// 跳过这些前缀词后再读对象名（IF EXISTS / ONLY / CONCURRENTLY ...）。
const OBJECT_PREFIX_SKIP: &[&str] = &[
    "IF",
    "EXISTS",
    "ONLY",
    "CONCURRENTLY",
    "TEMPORARY",
    "TEMP",
    "UNIQUE",
    "FULLTEXT",
    "SPATIAL",
    "INTO",
    "FROM",
    "TABLE",
    "VIEW",
    "INDEX",
    "SEQUENCE",
    "TYPE",
    "TRIGGER",
    "EVENT",
    "FUNCTION",
    "PROCEDURE",
    "DATABASE",
    "SCHEMA",
    "USER",
    "LOGIN",
    "ROLE",
    "SERVER",
    "STATISTICS",
];

/// 顶层代码词（括号外），保留原文与字节区间。
struct Words<'a> {
    text: &'a str,
    list: Vec<(String, usize, usize)>,
}

impl<'a> Words<'a> {
    fn of(engine: &str, text: &'a str) -> Self {
        Self { text, list: split::top_level_words(engine, text).0 }
    }

    fn get(&self, index: usize) -> Option<&str> {
        self.list.get(index).map(|(word, _, _)| word.as_str())
    }

    fn lead(&self) -> String {
        self.get(0).unwrap_or_default().to_owned()
    }

    fn has(&self, word: &str) -> bool {
        self.list.iter().any(|(item, _, _)| item == word)
    }

    fn has_any(&self, words: &[&str]) -> bool {
        words.iter().any(|word| self.has(word))
    }

    fn index_of(&self, word: &str) -> Option<usize> {
        self.list.iter().position(|(item, _, _)| item == word)
    }

    /// 两个词是否紧挨着出现（ALTER COLUMN 与 ADD COLUMN 要分开看）。
    fn adjacent(&self, first: &str, second: &str) -> bool {
        self.list.windows(2).any(|pair| pair[0].0 == first && pair[1].0 == second)
    }
}

/// 第 index 个词之后的对象名（可带模式前缀）。跳过前缀词、注释与多余空白。
fn object_after(text: &str, words: &Words, index: usize, skip: &[&str]) -> Option<String> {
    let (_, _, end) = *words.list.get(index)?;
    let mut at = end;
    for _ in 0..8 {
        at = skip_blank(text, at);
        let (token, next) = read_token(text, at)?;
        if skip.iter().any(|word| token.eq_ignore_ascii_case(word)) {
            at = next;
            continue;
        }
        let (mut name, mut cursor) = (token, next);
        // 模式前缀：schema.object
        while let Some(after_dot) = dotted(text, cursor) {
            let (part, next) = read_token(text, after_dot)?;
            name.push('.');
            name.push_str(&part);
            cursor = next;
        }
        return Some(name);
    }
    None
}

fn skip_blank(text: &str, from: usize) -> usize {
    let bytes = text.as_bytes();
    let mut at = from;
    while at < bytes.len() {
        let ch = bytes[at] as char;
        if ch.is_whitespace() || ch == ',' {
            at += 1;
            continue;
        }
        if ch == '-' && bytes.get(at + 1) == Some(&b'-') {
            while at < bytes.len() && bytes[at] != b'\n' {
                at += 1;
            }
            continue;
        }
        if ch == '/' && bytes.get(at + 1) == Some(&b'*') {
            at += 2;
            while at + 1 < bytes.len() && !(bytes[at] == b'*' && bytes[at + 1] == b'/') {
                at += 1;
            }
            at = (at + 2).min(bytes.len());
            continue;
        }
        break;
    }
    at
}

/// 读一个标识符：裸词或引号形式；返回原文和结束位置。
fn read_token(text: &str, from: usize) -> Option<(String, usize)> {
    let bytes = text.as_bytes();
    let mut at = from;
    if at >= bytes.len() {
        return None;
    }
    let open = bytes[at] as char;
    if open == '"' || open == '\u{60}' {
        at += 1;
        while at < bytes.len() && bytes[at] as char != open {
            at += 1;
        }
        let end = (at + 1).min(bytes.len());
        return Some((text[from..end].to_owned(), end));
    }
    if open == '[' {
        at += 1;
        while at < bytes.len() && bytes[at] as char != ']' {
            at += 1;
        }
        let end = (at + 1).min(bytes.len());
        return Some((text[from..end].to_owned(), end));
    }
    if !(open.is_ascii_alphabetic() || open == '_' || open == '@') {
        return None;
    }
    while at < bytes.len() {
        let ch = bytes[at] as char;
        if !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '$' || ch == '@' || ch == '#') {
            break;
        }
        at += 1;
    }
    Some((text[from..at].to_owned(), at))
}

/// 位置处是点号时返回点之后的位置。
fn dotted(text: &str, at: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let cursor = skip_blank(text, at);
    if bytes.get(cursor) == Some(&b'.') {
        Some(skip_blank(text, cursor + 1))
    } else {
        None
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    /// 一批 SQL 里需要确认的语句数。
    fn risky(engine: &str, sql: &str) -> usize {
        assess(engine, sql).statements.len()
    }

    /// 这批 SQL 切出的语句总数（需要确认的 + 直通的）。
    fn total(report: &RiskReport) -> usize {
        report.statements.len() + report.passthrough
    }

    /// 唯一一条风险语句上的后果分类（按出现顺序）。
    fn kinds(engine: &str, sql: &str) -> Vec<RiskKind> {
        let report = assess(engine, sql);
        assert_eq!(report.statements.len(), 1, "{engine} / {sql}: {:?}", report.statements);
        report.statements[0].consequences.iter().map(|item| item.kind).collect()
    }

    #[test]
    fn read_only_queries_pass_through_on_every_sql_engine() {
        let cases: &[(&str, &[&str])] = &[
            ("postgres", &["SELECT 1", "EXPLAIN SELECT 1", "SHOW server_version", "VALUES (1)", "TABLE t", "BEGIN", "SET search_path = public"]),
            ("mysql", &["SELECT 1", "SHOW TABLES", "DESCRIBE t", "EXPLAIN SELECT 1", "VALUES ROW(1)", "SET @x = 1", "SET SESSION sql_mode = ''"]),
            ("mariadb", &["SELECT 1", "SHOW CREATE TABLE t", "DESC t", "SET @x = 1"]),
            ("sqlite", &["SELECT 1", "EXPLAIN QUERY PLAN SELECT 1", "PRAGMA table_info(t)", "PRAGMA integrity_check"]),
            ("sqlserver", &["SELECT 1", "SET NOCOUNT ON", "EXEC sp_help"]),
        ];
        for (engine, statements) in cases {
            for sql in *statements {
                let report = assess(engine, sql);
                assert!(!report.needs_confirmation(), "{engine} / {sql}: {:?}", report.statements);
                assert_eq!(report.passthrough, 1, "{engine} / {sql}");
            }
        }
    }

    #[test]
    fn data_loss_carries_implicit_commit_only_where_the_dialect_commits() {
        assert_eq!(kinds("mysql", "DROP TABLE orders"), vec![RiskKind::DataLoss, RiskKind::ImplicitCommit]);
        assert_eq!(kinds("mariadb", "TRUNCATE TABLE orders"), vec![RiskKind::DataLoss, RiskKind::ImplicitCommit]);
        assert_eq!(kinds("postgres", "DROP TABLE orders"), vec![RiskKind::DataLoss]);
        assert_eq!(kinds("postgres", "TRUNCATE orders"), vec![RiskKind::DataLoss]);
        assert_eq!(kinds("sqlite", "DROP TABLE orders"), vec![RiskKind::DataLoss]);
        // 可以回滚的 DDL 方言没有隐式提交这一项。
        assert_eq!(kinds("sqlserver", "TRUNCATE TABLE orders"), vec![RiskKind::DataLoss]);
        // 删索引不改数据，只影响服务端状态。
        assert_eq!(kinds("postgres", "DROP INDEX ix_orders"), vec![RiskKind::ServerState]);
    }

    #[test]
    fn delete_and_update_without_where_are_flagged_but_row_scoped_ones_pass() {
        assert_eq!(kinds("postgres", "DELETE FROM orders"), vec![RiskKind::DataLoss]);
        assert_eq!(kinds("mysql", "DELETE FROM orders"), vec![RiskKind::DataLoss]);
        assert_eq!(kinds("postgres", "UPDATE orders SET total = 0"), vec![RiskKind::MassRewrite]);
        assert_eq!(risky("postgres", "DELETE FROM orders WHERE id = 1"), 0);
        assert_eq!(risky("mysql", "UPDATE orders SET total = 0 WHERE id = 1"), 0);
        assert_eq!(risky("postgres", "INSERT INTO orders (id) VALUES (1)"), 0);
    }

    #[test]
    fn object_replacement_covers_replace_or_replace_and_drop_create_pairs() {
        assert_eq!(kinds("mysql", "REPLACE INTO orders (id) VALUES (1)"), vec![RiskKind::ObjectReplace]);
        assert_eq!(kinds("postgres", "CREATE OR REPLACE VIEW v AS SELECT 1"), vec![RiskKind::ObjectReplace]);
        assert_eq!(kinds("sqlserver", "CREATE OR ALTER VIEW v AS SELECT 1"), vec![RiskKind::ObjectReplace]);
        // 先删后建同名对象：删表那一条同时是数据丢失与对象替换。
        let report = assess("postgres", "DROP TABLE orders;\nCREATE TABLE orders (id int)");
        assert_eq!(report.statements.len(), 1, "{:?}", report.statements);
        let found: Vec<RiskKind> = report.statements[0].consequences.iter().map(|item| item.kind).collect();
        assert_eq!(found, vec![RiskKind::DataLoss, RiskKind::ObjectReplace]);
        assert_eq!(report.passthrough, 1);
        // 换了名字就不是替换。
        assert_eq!(kinds("postgres", "DROP TABLE old_orders;\nCREATE TABLE orders (id int)"), vec![RiskKind::DataLoss]);
    }

    #[test]
    fn mass_rewrite_covers_alter_rewrites_and_whole_table_scans() {
        assert_eq!(
            kinds("mysql", "ALTER TABLE orders MODIFY COLUMN total BIGINT"),
            vec![RiskKind::MassRewrite, RiskKind::ImplicitCommit]
        );
        // 明确 INSTANT 的原地变更不按重建整表提醒：没有后果就不拦。
        assert_eq!(risky("mysql", "ALTER TABLE orders ADD COLUMN note TEXT, ALGORITHM=INSTANT"), 0);
        assert_eq!(risky("postgres", "ALTER TABLE orders ADD COLUMN note TEXT"), 0);
        assert_eq!(kinds("postgres", "VACUUM FULL orders"), vec![RiskKind::MassRewrite]);
        assert_eq!(kinds("sqlite", "VACUUM"), vec![RiskKind::MassRewrite]);
        assert_eq!(
            kinds("sqlserver", "MERGE orders AS t USING src AS s ON t.id = s.id WHEN MATCHED THEN DELETE;"),
            vec![RiskKind::MassRewrite]
        );
        assert_eq!(kinds("mysql", "ALTER TABLE orders DROP COLUMN note"), vec![RiskKind::DataLoss, RiskKind::ImplicitCommit]);
    }

    #[test]
    fn external_schedule_state_covers_events_and_the_scheduler_switch() {
        assert_eq!(kinds("mysql", "SET GLOBAL event_scheduler = ON"), vec![RiskKind::ExternalSchedule]);
        assert_eq!(kinds("mariadb", "SET @@GLOBAL.event_scheduler = OFF"), vec![RiskKind::ExternalSchedule]);
        assert_eq!(
            kinds("mysql", "ALTER EVENT nightly DISABLE"),
            vec![RiskKind::ExternalSchedule, RiskKind::ImplicitCommit]
        );
        assert_eq!(
            kinds("mysql", "CREATE EVENT nightly ON SCHEDULE EVERY 1 HOUR DO DELETE FROM orders"),
            vec![RiskKind::ExternalSchedule]
        );
        // 服务端变量不是调度：只算服务端状态。
        assert_eq!(kinds("mysql", "SET GLOBAL max_connections = 50"), vec![RiskKind::ServerState]);
    }

    #[test]
    fn server_state_covers_privileges_and_sessions() {
        assert_eq!(kinds("postgres", "GRANT SELECT ON orders TO analyst"), vec![RiskKind::ServerState]);
        assert_eq!(kinds("postgres", "ALTER SYSTEM SET work_mem = '8MB'"), vec![RiskKind::ServerState]);
        assert_eq!(kinds("sqlserver", "EXEC sp_configure 'max degree', 4"), vec![RiskKind::ServerState]);
        assert_eq!(kinds("sqlserver", "KILL 57"), vec![RiskKind::ServerState]);
        assert_eq!(kinds("sqlite", "ATTACH DATABASE 'other.db' AS other"), vec![RiskKind::ServerState]);
        assert_eq!(kinds("mysql", "RENAME TABLE a TO b"), vec![RiskKind::ServerState, RiskKind::ImplicitCommit]);
        assert_eq!(kinds("mysql", "CALL refresh_rollups()"), vec![RiskKind::ServerState]);
    }

    #[test]
    fn a_mixed_script_only_lists_the_statements_with_consequences() {
        let report = assess("mysql", "SELECT 1;\nDROP TABLE orders;\nSELECT 2");
        assert_eq!(total(&report), 3);
        assert_eq!(report.passthrough, 2);
        assert_eq!(report.statements.len(), 1);
        assert_eq!(report.statements[0].index, 1);
        assert_eq!(report.statements[0].sql.trim().trim_end_matches(';'), "DROP TABLE orders");
        assert!(report.notices.iter().any(|notice| notice.contains("隐式提交")), "{:?}", report.notices);
        // 只读脚本没有阻断。
        assert!(!assess("mysql", "SELECT 1; SELECT 2").needs_confirmation());
    }

    #[test]
    fn explain_analyze_and_cte_writes_are_judged_by_the_inner_statement() {
        assert_eq!(kinds("postgres", "EXPLAIN ANALYZE DELETE FROM orders"), vec![RiskKind::DataLoss]);
        assert_eq!(
            kinds("postgres", "WITH gone AS (DELETE FROM orders RETURNING id) SELECT count(*) FROM gone"),
            vec![RiskKind::DataLoss]
        );
        assert_eq!(risky("postgres", "EXPLAIN SELECT 1"), 0);
    }

    #[test]
    fn objects_are_named_for_the_confirmation_panel() {
        let report = assess("mysql", "DROP TABLE IF EXISTS sales.orders");
        assert_eq!(report.statements[0].consequences[0].object.as_deref(), Some("sales.orders"));
        let report = assess("sqlite", "DELETE FROM main.orders");
        assert_eq!(report.statements[0].consequences[0].object.as_deref(), Some("main.orders"));
        // 引号形式保持原样，不猜用户想删的是哪个标识符。
        let report = assess("postgres", "DROP TABLE \"Orders\"");
        assert_eq!(report.statements[0].consequences[0].object.as_deref(), Some("\"Orders\""));
    }

    #[test]
    fn refusal_is_assessed_as_one_statement_instead_of_being_skipped() {
        let report = assess("mysql", "DROP TABLE orders WHERE");
        assert_eq!(total(&report), 1);
        assert_eq!(report.passthrough, 0);
    }
}
