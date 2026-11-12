//! SQL 方言辅助。编辑器只负责文本和显示；切分、格式化、lint、补全、大纲和计划在这里。
//! 失败时返回原 SQL，不在这里改写调用方的草稿。

use std::collections::{HashMap, HashSet};
use std::path::Path;

use super::exec::showplan_batch;
use super::split::{self, Piece, SplitReport};

pub const MAX_SQL_FILE_BYTES: u64 = 2 * 1024 * 1024;

const SQL_KEYWORDS: &[&str] = &[
    "SELECT", "FROM", "WHERE", "AND", "OR", "NOT", "NULL", "IS", "IN", "EXISTS", "INSERT", "INTO", "VALUES", "UPDATE", "SET", "DELETE", "CREATE", "ALTER", "DROP", "TABLE", "VIEW", "INDEX", "JOIN", "LEFT", "RIGHT", "INNER", "OUTER", "ON", "GROUP", "BY", "ORDER", "LIMIT", "OFFSET", "AS", "DISTINCT", "HAVING", "UNION", "ALL", "CASE", "WHEN", "THEN", "ELSE", "END", "WITH", "BEGIN", "COMMIT", "ROLLBACK",
];

const CLAUSE: &[&str] = &["SELECT", "FROM", "WHERE", "GROUP", "ORDER", "HAVING", "LIMIT", "OFFSET", "UNION", "INSERT", "UPDATE", "DELETE", "VALUES", "SET", "WITH", "RETURNING"];
const SUB: &[&str] = &["AND", "OR", "JOIN", "LEFT", "RIGHT", "INNER", "FULL", "CROSS"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunScope {
    Auto,
    Current,
    Script,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunPick {
    pub sql: String,
    pub mode: &'static str,
    pub from: usize,
    pub to: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Located {
    pub piece: Option<Piece>,
    pub refusal: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextResult {
    pub ok: bool,
    pub sql: String,
    pub reason_key: Option<&'static str>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LintFinding {
    pub start: usize,
    pub end: usize,
    pub message_key: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VariableInput {
    pub key: String,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutlineItem {
    pub index: usize,
    pub start: usize,
    pub end: usize,
    pub line: usize,
    pub text: String,
    pub preview: String,
    pub status: String,
    pub rows: usize,
    pub affected: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionEntry {
    pub label: String,
    pub kind: String,
    /// 已翻译的详情（类型、签名或来源）；没有详情时为 None。
    pub detail: Option<String>,
    pub insert: String,
    /// 结构化的说明数据（列类型/可空/注释、对象类型与模式、函数签名），供详情文档和悬停使用。
    /// 只放实际读到的元数据；没有时为 None。
    pub info: Option<EntryInfo>,
}

/// 一列的元数据。type_name / comment 读不到时为空串，nullable 读不到时为 None，不编造。
/// primary / foreign_key 只在目录或外键清单确实标到时为真。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ColumnMeta {
    pub name: String,
    pub type_name: String,
    pub nullable: Option<bool>,
    pub comment: String,
    pub primary: bool,
    pub foreign_key: bool,
}

impl ColumnMeta {
    /// 只知道列名时的元数据：类型、可空和注释都留空。
    pub fn named(name: impl Into<String>) -> Self {
        Self { name: name.into(), ..Self::default() }
    }
}

/// 补全候选与悬停共用的结构化说明。文本由界面按语言排版，这里不放翻译后的句子。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntryInfo {
    /// 来源（表或视图，按语句里的写法）里的一列。
    Column { source: String, column: ColumnMeta },
    /// 目录对象：模式、名字、类型和注释（读不到时为空）。
    Object { schema: String, name: String, kind: String, comment: String },
    /// 内置函数的一行签名。
    Function { signature: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionSource {
    pub alias: String,
    pub label: String,
    pub columns: Vec<ColumnMeta>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogObject {
    pub name: String,
    pub schema: String,
    pub kind: String,
    /// 对象注释；没有读到时为空。
    pub comment: String,
}

/// 补全所需的元数据状态。失败、部分失败和空目录都必须能显示出来，
/// 不能渲染成“补全成功但没有命中”。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum MetadataState {
    #[default]
    Ready,
    NoTarget,
    NoProvider,
    Empty,
    Failed(String),
    Partial(String),
    Stale(String),
}

/// 真实外键：ON 条件只在实际读到外键时标注为外键，否则只给命名推断。
/// 连接、库和两侧模式一起构成身份；同名表在不同库或模式里不能互相匹配。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ForeignKey {
    pub connection_id: String,
    pub database: String,
    pub from_schema: String,
    pub from_table: String,
    pub from_columns: Vec<String>,
    pub to_schema: String,
    pub to_table: String,
    pub to_columns: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeObject {
    pub connection_id: String,
    pub connection_name: String,
    pub engine: String,
    pub database: String,
    pub schema: String,
    pub name: String,
    pub kind: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileLoad {
    pub sql: String,
    pub execute: bool,
}

pub fn known_engine(engine: &str) -> bool {
    matches!(engine, "postgres" | "mysql" | "mariadb" | "sqlite" | "sqlserver")
}

pub fn locate_statement(engine: &str, sql: &str, cursor: usize) -> Located {
    let report = split::split_sql(engine, sql);
    if let Some(found) = report.pieces.iter().find(|piece| cursor >= piece.start && cursor <= piece.end) {
        return Located { piece: Some(found.clone()), refusal: None };
    }
    if let (Some(reason), Some(from)) = (report.refusal.clone(), report.refusal_from) {
        if cursor >= from {
            return Located { piece: None, refusal: Some(reason) };
        }
    }
    if let Some(previous) = report.pieces.iter().rev().find(|piece| piece.end <= cursor) {
        return Located { piece: Some(previous.clone()), refusal: None };
    }
    Located { piece: None, refusal: report.refusal }
}

/// 选区执行使用精确切片。空选区或“当前语句”才按光标定位。
pub fn sql_to_run(sql: &str, from: usize, to: usize, head: usize, scope: RunScope, engine: &str) -> RunPick {
    if scope == RunScope::Script {
        return RunPick { sql: sql.to_owned(), mode: "script", from: 0, to: sql.len() };
    }
    if scope == RunScope::Current || from == to {
        let located = locate_statement(engine, sql, head);
        let (text, start, end) = match located.piece {
            Some(piece) => (piece.text, piece.start, piece.end),
            None => (String::new(), head, head),
        };
        return RunPick { sql: text, mode: "current", from: start, to: end };
    }
    let (from, to) = if from <= to { (from, to) } else { (to, from) };
    let end = to.min(sql.len());
    let start = from.min(end);
    RunPick { sql: sql.get(start..end).unwrap_or("").to_owned(), mode: "selection", from: start, to: end }
}

pub fn prepare_send(sql: &str, pick: &RunPick, engine: &str) -> Result<SplitReport, String> {
    if pick.mode == "current" {
        let located = locate_statement(engine, sql, pick.from);
        if let Some(reason) = located.refusal {
            return Err(reason);
        }
        if located.piece.is_none() {
            let document = split::split_sql(engine, sql);
            if let Some(reason) = document.refusal {
                return Err(reason);
            }
            if !document.notices.is_empty() {
                return Err(document.notices.join(""));
            }
            return Err(key_text("query.errors.blankOnly"));
        }
    }
    let inspected = if pick.mode == "script" { split::split_sql(engine, sql) } else { split::split_sql(engine, &pick.sql) };
    if let Some(reason) = inspected.refusal.clone() {
        return Err(reason);
    }
    if inspected.pieces.is_empty() {
        if !inspected.notices.is_empty() {
            return Err(inspected.notices.join(""));
        }
        return Err(key_text("query.errors.blankOnly"));
    }
    Ok(inspected)
}

fn key_text(key: &str) -> String {
    crate::i18n::text(crate::i18n::Locale::ZhCn, key)
}

pub fn format_sql(sql: &str) -> TextResult {
    if let Some(open) = open_delimiter(sql) {
        return TextResult { ok: false, sql: sql.to_owned(), reason_key: Some(open_key(open)) };
    }
    let laid = layout(&tokenize(sql));
    if !laid.0 || !keeps_adjacent(sql, &laid.1) {
        return TextResult { ok: false, sql: sql.to_owned(), reason_key: Some("query.format.adjacent") };
    }
    TextResult { ok: true, sql: laid.1, reason_key: None }
}

pub fn format_document(sql: &str, from: usize, to: usize) -> TextResult {
    let selected = from != to;
    let (from, to) = ordered(sql, from, to);
    let formatted = format_sql(sql.get(from..to).unwrap_or(""));
    if !formatted.ok {
        return TextResult { ok: false, sql: sql.to_owned(), reason_key: formatted.reason_key };
    }
    if !selected && from == 0 && to == sql.len() {
        return formatted;
    }
    let next = format!("{}{}{}", sql.get(..from).unwrap_or(""), formatted.sql, sql.get(to..).unwrap_or(""));
    if line_comment_broken(sql, &next) {
        return TextResult { ok: false, sql: sql.to_owned(), reason_key: Some("query.format.commentBreak") };
    }
    if edge_whitespace_removed(sql, from, to, &formatted.sql) {
        return TextResult { ok: false, sql: sql.to_owned(), reason_key: Some("query.format.edgeGlue") };
    }
    TextResult { ok: true, sql: next, reason_key: None }
}

pub fn guard_rewrite(engine: &str, provider: bool, sql: &str) -> Option<TextResult> {
    if !known_engine(engine) {
        return Some(TextResult { ok: false, sql: sql.to_owned(), reason_key: Some("query.errors.unknownDialect") });
    }
    if !provider {
        return Some(TextResult { ok: false, sql: sql.to_owned(), reason_key: Some("query.errors.noProvider") });
    }
    None
}

pub fn lint_sql(sql: &str, engine: &str) -> Vec<LintFinding> {
    let hash = engine == "mysql" || engine == "mariadb";
    let mut findings = Vec::new();
    let mut parens = Vec::new();
    let mut i = 0usize;
    let mut quote: Option<char> = None;
    let mut quote_at = 0usize;
    let mut dollar: Option<String> = None;
    let mut dollar_at = 0usize;
    let mut comment = false;
    let mut comment_at = 0usize;
    while i < sql.len() {
        if let Some(tag) = dollar.clone() {
            let marker = format!("${tag}$");
            if sql[i..].starts_with(&marker) {
                i += marker.len();
                dollar = None;
                continue;
            }
            i = step(sql, i);
            continue;
        }
        if let Some(q) = quote {
            let ch = ch_at(sql, i);
            let next = ch_at(sql, step(sql, i));
            if ch == Some(q) && next == Some(q) {
                i = step(sql, step(sql, i));
                continue;
            }
            if ch == Some(q) {
                quote = None;
            }
            i = step(sql, i);
            continue;
        }
        if comment {
            if sql[i..].starts_with("*/") {
                comment = false;
                i += 2;
                continue;
            }
            i = step(sql, i);
            continue;
        }
        if hash && ch_at(sql, i) == Some('#') {
            i = eol(sql, step(sql, i));
            continue;
        }
        if sql[i..].starts_with("--") {
            i = eol(sql, i + 2);
            continue;
        }
        if sql[i..].starts_with("/*") {
            comment = true;
            comment_at = i;
            i += 2;
            continue;
        }
        if matches!(ch_at(sql, i), Some('\'' | '"' | '`')) {
            quote = ch_at(sql, i);
            quote_at = i;
            i = step(sql, i);
            continue;
        }
        if ch_at(sql, i) == Some('$') {
            if let Some(tag) = dollar_tag(sql, i) {
                dollar = Some(tag.clone());
                dollar_at = i;
                i += tag.len() + 2;
                continue;
            }
        }
        match ch_at(sql, i) {
            Some('(') => parens.push(i),
            Some(')') => {
                parens.pop();
            }
            _ => {}
        }
        i = step(sql, i);
    }
    if quote.is_some() {
        findings.push(LintFinding { start: quote_at, end: line_end(sql, quote_at), message_key: "query.split.quoteUnclosed" });
    }
    if comment {
        findings.push(LintFinding { start: comment_at, end: line_end(sql, comment_at), message_key: "query.split.blockCommentUnclosed" });
    }
    if dollar.is_some() {
        findings.push(LintFinding { start: dollar_at, end: line_end(sql, dollar_at), message_key: "query.split.dollarUnclosed" });
    }
    if let Some(start) = parens.first().copied() {
        findings.push(LintFinding { start, end: line_end(sql, start), message_key: "query.split.parenUnclosed" });
    }
    findings.sort_by_key(|item| item.start);
    findings
}

pub fn find_variables(sql: &str) -> Vec<VariableInput> {
    let mut inputs = Vec::new();
    let mut seen = HashSet::new();
    for spot in scan_spots(sql) {
        if !seen.insert(spot.key.clone()) {
            continue;
        }
        let label = if spot.key.starts_with('?') { spot.key.clone() } else { format!(":{}", spot.key) };
        inputs.push(VariableInput { key: spot.key, label });
    }
    inputs
}

pub fn substitute_variables(sql: &str, values: &HashMap<String, String>) -> String {
    let spots = scan_spots(sql);
    if spots.is_empty() {
        return sql.to_owned();
    }
    let mut out = String::new();
    let mut last = 0usize;
    for spot in spots {
        out.push_str(sql.get(last..spot.start).unwrap_or(""));
        out.push_str(&literal(values.get(&spot.key).map(String::as_str)));
        last = spot.end;
    }
    out.push_str(sql.get(last..).unwrap_or(""));
    out
}

pub fn outline_statements(sql: &str, engine: &str) -> Vec<OutlineItem> {
    split::split_sql(engine, sql)
        .pieces
        .into_iter()
        .enumerate()
        .map(|(position, piece)| {
            let head = piece.text.lines().next().unwrap_or("").split_whitespace().collect::<Vec<_>>().join(" ");
            let preview = if head.chars().count() > 72 {
                format!("{}…", head.chars().take(72).collect::<String>())
            } else {
                head
            };
            OutlineItem {
                index: position + 1,
                line: count_lines(sql, piece.start),
                start: piece.start,
                end: piece.end,
                text: piece.text,
                preview,
                status: "none".to_owned(),
                rows: 0,
                affected: None,
            }
        })
        .collect()
}

pub fn merge_outline(list: Vec<OutlineItem>, statements: &[(String, String, usize, Option<u64>)]) -> Vec<OutlineItem> {
    let mut queues: HashMap<String, Vec<(String, usize, Option<u64>)>> = HashMap::new();
    for (sql, status, rows, affected) in statements {
        queues.entry(sql.trim().to_owned()).or_default().push((status.clone(), *rows, *affected));
    }
    list.into_iter()
        .map(|mut item| {
            let key = item.text.trim().to_owned();
            let Some(queue) = queues.get_mut(&key) else {
                return item;
            };
            if queue.is_empty() {
                return item;
            }
            let (status, rows, affected) = queue.remove(0);
            item.status = status;
            item.rows = rows;
            item.affected = affected;
            item
        })
        .collect()
}

#[allow(dead_code)]
pub fn statement_index_at(list: &[OutlineItem], offset: usize) -> isize {
    if list.is_empty() {
        return -1;
    }
    for (index, item) in list.iter().enumerate() {
        if offset < item.start {
            return if index == 0 { 0 } else { index as isize - 1 };
        }
        if offset <= item.end {
            return index as isize;
        }
    }
    list.len() as isize - 1
}

pub fn fold_line_ranges(sql: &str, engine: &str) -> Vec<(usize, usize)> {
    outline_statements(sql, engine)
        .into_iter()
        .filter_map(|item| {
            let start = item.line.saturating_sub(1);
            let end = count_lines(sql, item.end).saturating_sub(1);
            if end > start { Some((start, end)) } else { None }
        })
        .collect()
}

pub fn quote_ident(engine: &str, name: &str) -> String {
    if engine == "sqlserver" {
        return format!("[{}]", name.replace(']', "]]"));
    }
    let quote = if engine == "mysql" || engine == "mariadb" { '`' } else { '"' };
    format!("{quote}{}{quote}", name.replace(quote, &format!("{quote}{quote}")))
}

pub fn qualified_name(engine: &str, schema: &str, name: &str) -> String {
    let table = quote_ident(engine, name);
    if schema.is_empty() {
        table
    } else {
        format!("{}.{}", quote_ident(engine, schema), table)
    }
}

pub fn browse_sql(engine: &str, schema: &str, name: &str) -> String {
    format!("SELECT *\nFROM {}\nLIMIT 100;", qualified_name(engine, schema, name))
}

pub fn expand_star(engine: &str, sql: &str, sources: &[CompletionSource], provider: bool) -> TextResult {
    if let Some(blocked) = guard_rewrite(engine, provider, sql) {
        return blocked;
    }
    if sources.iter().all(|source| source.columns.is_empty()) {
        return TextResult { ok: false, sql: sql.to_owned(), reason_key: Some("query.errors.noProvider") };
    }
    let Some(rewritten) = rewrite_stars(sql, sources) else {
        return TextResult { ok: false, sql: sql.to_owned(), reason_key: Some("query.errors.noProvider") };
    };
    if rewritten == sql {
        return TextResult { ok: false, sql: sql.to_owned(), reason_key: Some("query.errors.noProvider") };
    }
    TextResult { ok: true, sql: rewritten, reason_key: None }
}

/// 编辑器补全用的元数据快照：方言、当前库对象与列、库/模式、词频、状态与外键。
/// 语句文本和光标由编辑器在请求时另外传入，这里不保存草稿。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompletionScope {
    pub engine: String,
    pub objects: Vec<CatalogObject>,
    pub sources: Vec<CompletionSource>,
    pub provider: bool,
    /// 当前连接的库清单，只在 USE 和通用场景出现。
    pub databases: Vec<String>,
    /// 当前库的模式清单。
    pub schemas: Vec<String>,
    /// 本地词频（小写标识符 → 次数）；只参与同分排序，不参与筛选。
    pub frequency: Vec<(String, u32)>,
    /// 元数据状态；失败和部分失败必须能显示出来。
    pub state: MetadataState,
    /// 已读到的真实外键；为空时 ON 条件只做命名推断。
    pub foreign_keys: Vec<ForeignKey>,
}

impl CompletionScope {
    /// 没有语句文本时的通用补全；编辑器请求会带上语句和光标。
    pub fn complete(&self, prefix: &str, locale: crate::i18n::Locale) -> Vec<CompletionEntry> {
        super::complete::plan(self, prefix, prefix.len(), locale).entries()
    }
}

pub fn plan_statement(sql: &str, from: usize, to: usize, head: usize, engine: &str) -> Result<(String, usize, usize), String> {
    if !known_engine(engine) {
        return Err(key_text("query.errors.unknownDialect"));
    }
    let pick = if from == to {
        let located = locate_statement(engine, sql, head);
        if let Some(reason) = located.refusal {
            return Err(reason);
        }
        let Some(piece) = located.piece else {
            return Err(key_text("query.plan.empty"));
        };
        (piece.text, piece.start, piece.end)
    } else {
        let (start, end) = ordered(sql, from, to);
        (sql.get(start..end).unwrap_or("").to_owned(), start, end)
    };
    let report = split::split_sql(engine, &pick.0);
    if let Some(reason) = report.refusal {
        return Err(reason);
    }
    if report.pieces.is_empty() {
        return Err(key_text("query.plan.empty"));
    }
    if report.pieces.len() > 1 {
        return Err(key_text("query.plan.multiple"));
    }
    Ok((report.pieces[0].text.clone(), pick.1, pick.2))
}

pub fn explain_sql(engine: &str, statement: &str) -> Result<String, String> {
    if !known_engine(engine) {
        return Err(key_text("query.errors.unknownDialect"));
    }
    let body = statement.trim().trim_end_matches(';').trim();
    if body.is_empty() {
        return Err(key_text("query.plan.empty"));
    }
    if has_analyze(body, engine) {
        return Err(key_text("query.plan.noAnalyze"));
    }
    if engine == "sqlserver" {
        return Ok(showplan_batch(body));
    }
    let upper = body.to_ascii_uppercase();
    if upper.starts_with("EXPLAIN") {
        if engine == "sqlite" && !upper.starts_with("EXPLAIN QUERY PLAN") {
            let rest = body.split_once(char::is_whitespace).map(|(_, rest)| rest).unwrap_or("");
            return Ok(format!("EXPLAIN QUERY PLAN {rest}"));
        }
        return Ok(body.to_owned());
    }
    if engine == "sqlite" {
        Ok(format!("EXPLAIN QUERY PLAN {body}"))
    } else if engine == "mysql" {
        Ok(format!("EXPLAIN FORMAT=TREE {body}"))
    } else if engine == "mariadb" {
        Ok(format!("EXPLAIN FORMAT=JSON {body}"))
    } else {
        Ok(format!("EXPLAIN {body}"))
    }
}

pub fn sql_save_name() -> String {
    format!("query-{}.sql", chrono::Local::now().format("%Y-%m-%d"))
}

pub fn sql_file_problem(size: u64) -> Option<&'static str> {
    if size > MAX_SQL_FILE_BYTES { Some("query.fileIo.tooLarge") } else { None }
}

pub fn read_sql_file(path: &Path) -> Result<FileLoad, String> {
    let meta = std::fs::metadata(path).map_err(|err| format!("文件没有读出来：{err}"))?;
    if sql_file_problem(meta.len()).is_some() {
        return Err(key_text("query.fileIo.tooLarge"));
    }
    let sql = std::fs::read_to_string(path).map_err(|err| format!("文件没有读出来：{err}"))?;
    Ok(FileLoad { sql, execute: false })
}

pub fn write_sql_file(path: &Path, sql: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|err| format!("无法保存文件：{err}"))?;
        }
    }
    let tmp = path.with_extension("partial");
    std::fs::write(&tmp, sql.as_bytes()).map_err(|err| format!("无法保存文件：{err}"))?;
    std::fs::rename(&tmp, path).map_err(|err| {
        let _ = std::fs::remove_file(&tmp);
        format!("无法保存文件：{err}")
    })
}

fn rewrite_stars(sql: &str, sources: &[CompletionSource]) -> Option<String> {
    let bytes = sql.as_bytes();
    let mut out = String::new();
    let mut i = 0usize;
    let mut quote: Option<u8> = None;
    let mut replaced = false;
    while i < bytes.len() {
        if let Some(q) = quote {
            out.push(bytes[i] as char);
            if bytes[i] == q {
                if q == b'\'' && i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                    out.push('\'');
                    i += 2;
                    continue;
                }
                quote = None;
            }
            i += 1;
            continue;
        }
        if bytes[i] == b'\'' || bytes[i] == b'"' || bytes[i] == b'`' {
            quote = Some(bytes[i]);
            out.push(bytes[i] as char);
            i += 1;
            continue;
        }
        if bytes[i] == b'-' && i + 1 < bytes.len() && bytes[i + 1] == b'-' {
            let end = eol(sql, i);
            out.push_str(&sql[i..end]);
            i = end;
            continue;
        }
        if bytes[i] == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            let end = sql[i + 2..].find("*/").map(|at| i + 2 + at + 2).unwrap_or(sql.len());
            out.push_str(&sql[i..end]);
            i = end;
            continue;
        }
        if bytes[i] == b'*' {
            // COUNT(*) 和其它函数调用里的星号保持原样，只展开选择列表里的 *。
            if preceded_by_paren(sql, i) {
                out.push('*');
                i += 1;
                continue;
            }
            let alias = alias_before(sql, i);
            let columns = columns_for(sources, alias.as_deref());
            if columns.is_empty() {
                return None;
            }
            if let Some(alias_at) = alias_span(sql, i) {
                out.truncate(alias_at);
            }
            out.push_str(&columns.join(", "));
            replaced = true;
            i += 1;
            continue;
        }
        out.push(sql[i..].chars().next().unwrap_or('\u{fffd}'));
        i = step(sql, i);
    }
    if quote.is_some() || !replaced {
        None
    } else {
        Some(out)
    }
}

fn preceded_by_paren(sql: &str, star: usize) -> bool {
    sql.get(..star).is_some_and(|head| head.trim_end().ends_with('('))
}

fn alias_before(sql: &str, star: usize) -> Option<String> {
    let head = sql.get(..star)?;
    let trimmed = head.trim_end();
    if !trimmed.ends_with('.') {
        return None;
    }
    let bare = trimmed.trim_end_matches('.').trim_end();
    let name: String = bare.chars().rev().take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_').collect::<String>().chars().rev().collect();
    if name.is_empty() { None } else { Some(name) }
}

fn alias_span(sql: &str, star: usize) -> Option<usize> {
    let alias = alias_before(sql, star)?;
    let trimmed = sql.get(..star)?.trim_end();
    let without_dot = trimmed.trim_end_matches('.').trim_end();
    Some(without_dot.len() - alias.len())
}

fn columns_for(sources: &[CompletionSource], alias: Option<&str>) -> Vec<String> {
    match alias {
        Some(alias) => sources
            .iter()
            .find(|source| source.alias.eq_ignore_ascii_case(alias))
            .map(|source| source.columns.iter().map(|column| column.name.clone()).collect())
            .unwrap_or_default(),
        None => {
            let mut seen = HashSet::new();
            let mut columns = Vec::new();
            for source in sources {
                for column in &source.columns {
                    if seen.insert(column.name.to_ascii_lowercase()) {
                        columns.push(column.name.clone());
                    }
                }
            }
            columns
        }
    }
}

pub(crate) fn dialect_words(engine: &str) -> &'static [&'static str] {
    match engine {
        "postgres" => &["ILIKE", "RETURNING", "LATERAL", "SIMILAR", "TABLESAMPLE", "STRING_AGG"],
        "mysql" | "mariadb" => &["REGEXP", "STRAIGHT_JOIN", "IFNULL", "GROUP_CONCAT", "UNSIGNED"],
        "sqlite" => &["PRAGMA", "VACUUM", "WITHOUT", "ROWID", "AUTOINCREMENT", "IFNULL", "GLOB"],
        "sqlserver" => &["TOP", "NOLOCK", "ISNULL", "OFFSET", "FETCH", "IDENTITY"],
        _ => &[],
    }
}

fn has_analyze(sql: &str, engine: &str) -> bool {
    let (words, _) = split::top_level_words(engine, sql);
    words.iter().any(|(word, _, _)| word == "ANALYZE" || word == "ANALYSE")
}

struct Spot {
    key: String,
    start: usize,
    end: usize,
}

fn scan_spots(sql: &str) -> Vec<Spot> {
    let mut spots = Vec::new();
    let mut question = 0usize;
    let mut i = 0usize;
    let mut quote: Option<char> = None;
    let mut dollar: Option<String> = None;
    let mut comment = false;
    while i < sql.len() {
        if let Some(tag) = dollar.clone() {
            let marker = format!("${tag}$");
            if sql[i..].starts_with(&marker) {
                i += marker.len();
                dollar = None;
                continue;
            }
            i = step(sql, i);
            continue;
        }
        if let Some(q) = quote {
            let ch = ch_at(sql, i);
            if ch == Some(q) && ch_at(sql, step(sql, i)) == Some(q) {
                i = step(sql, step(sql, i));
                continue;
            }
            if ch == Some(q) {
                quote = None;
            }
            i = step(sql, i);
            continue;
        }
        if comment {
            if sql[i..].starts_with("*/") {
                comment = false;
                i += 2;
                continue;
            }
            i = step(sql, i);
            continue;
        }
        if sql[i..].starts_with("--") {
            i = eol(sql, i + 2);
            continue;
        }
        if sql[i..].starts_with("/*") {
            comment = true;
            i += 2;
            continue;
        }
        match ch_at(sql, i) {
            Some('\'' | '"' | '`') => {
                quote = ch_at(sql, i);
                i = step(sql, i);
            }
            Some('$') => {
                if let Some(tag) = dollar_tag(sql, i) {
                    dollar = Some(tag.clone());
                    i += tag.len() + 2;
                } else {
                    i = step(sql, i);
                }
            }
            Some(':') => {
                if sql[i..].starts_with("::") {
                    i += 2;
                    continue;
                }
                let mut end = step(sql, i);
                while end < sql.len() && ident_cont(ch_at(sql, end).unwrap_or(' ')) {
                    end = step(sql, end);
                }
                if end > step(sql, i) {
                    spots.push(Spot { key: sql[step(sql, i)..end].to_owned(), start: i, end });
                    i = end;
                } else {
                    i = step(sql, i);
                }
            }
            Some('?') => {
                question += 1;
                spots.push(Spot { key: format!("?{question}"), start: i, end: i + 1 });
                i += 1;
            }
            _ => i = step(sql, i),
        }
    }
    spots
}

fn literal(value: Option<&str>) -> String {
    match value {
        None | Some("") => "NULL".to_owned(),
        Some(value) => format!("'{}'", value.replace('\'', "''")),
    }
}

fn count_lines(sql: &str, offset: usize) -> usize {
    sql.get(..offset.min(sql.len())).unwrap_or("").bytes().filter(|byte| *byte == b'\n').count() + 1
}

fn line_end(sql: &str, index: usize) -> usize {
    eol(sql, index)
}

fn ordered(sql: &str, from: usize, to: usize) -> (usize, usize) {
    let (from, to) = if from <= to { (from, to) } else { (to, from) };
    let end = to.min(sql.len());
    (from.min(end), end)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OpenKind {
    Quote,
    Comment,
    Dollar,
}

fn open_key(kind: OpenKind) -> &'static str {
    match kind {
        OpenKind::Quote => "query.format.quote",
        OpenKind::Comment => "query.format.comment",
        OpenKind::Dollar => "query.format.dollar",
    }
}

fn open_delimiter(sql: &str) -> Option<OpenKind> {
    let mut i = 0usize;
    let mut quote: Option<char> = None;
    let mut dollar: Option<String> = None;
    let mut comment = false;
    while i < sql.len() {
        if let Some(tag) = dollar.clone() {
            let marker = format!("${tag}$");
            if sql[i..].starts_with(&marker) {
                i += marker.len();
                dollar = None;
                continue;
            }
            i = step(sql, i);
            continue;
        }
        if let Some(q) = quote {
            if ch_at(sql, i) == Some(q) && ch_at(sql, step(sql, i)) == Some(q) {
                i = step(sql, step(sql, i));
                continue;
            }
            if ch_at(sql, i) == Some(q) {
                quote = None;
            }
            i = step(sql, i);
            continue;
        }
        if comment {
            if sql[i..].starts_with("*/") {
                comment = false;
                i += 2;
                continue;
            }
            i = step(sql, i);
            continue;
        }
        if ch_at(sql, i) == Some('#') {
            i = eol(sql, step(sql, i));
            continue;
        }
        if sql[i..].starts_with("--") {
            i = eol(sql, i + 2);
            continue;
        }
        if sql[i..].starts_with("/*") {
            comment = true;
            i += 2;
            continue;
        }
        if matches!(ch_at(sql, i), Some('\'' | '"' | '`')) {
            quote = ch_at(sql, i);
            i = step(sql, i);
            continue;
        }
        if ch_at(sql, i) == Some('$') {
            if let Some(tag) = dollar_tag(sql, i) {
                dollar = Some(tag.clone());
                i += tag.len() + 2;
                continue;
            }
        }
        i = step(sql, i);
    }
    if quote.is_some() {
        Some(OpenKind::Quote)
    } else if comment {
        Some(OpenKind::Comment)
    } else if dollar.is_some() {
        Some(OpenKind::Dollar)
    } else {
        None
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TokKind {
    Word,
    String,
    Number,
    Comment,
    Punct,
    Dollar,
}

struct Tok {
    kind: TokKind,
    text: String,
    gap: String,
    upper: Option<String>,
}

fn tokenize(sql: &str) -> Vec<Tok> {
    let mut tokens = Vec::new();
    let mut i = 0usize;
    while i < sql.len() {
        let gap_start = i;
        while i < sql.len() && ch_at(sql, i).is_some_and(char::is_whitespace) {
            i = step(sql, i);
        }
        if i >= sql.len() {
            break;
        }
        let gap = sql[gap_start..i].to_owned();
        let ch = ch_at(sql, i).unwrap_or(' ');
        if sql[i..].starts_with("--") || ch == '#' {
            let mut j = if ch == '#' { step(sql, i) } else { i + 2 };
            j = eol(sql, j);
            push(&mut tokens, TokKind::Comment, &sql[i..j], gap, None);
            i = j;
            continue;
        }
        if sql[i..].starts_with("/*") {
            let j = sql[i + 2..].find("*/").map(|at| i + 2 + at + 2).unwrap_or(sql.len());
            push(&mut tokens, TokKind::Comment, &sql[i..j], gap, None);
            i = j;
            continue;
        }
        if ch == '\'' || ch == '"' || ch == '`' {
            let mut j = step(sql, i);
            while j < sql.len() {
                if ch_at(sql, j) == Some(ch) && ch_at(sql, step(sql, j)) == Some(ch) {
                    j = step(sql, step(sql, j));
                    continue;
                }
                if ch_at(sql, j) == Some(ch) {
                    j = step(sql, j);
                    break;
                }
                j = step(sql, j);
            }
            push(&mut tokens, TokKind::String, &sql[i..j], gap, None);
            i = j;
            continue;
        }
        if ch == '$' {
            if let Some(tag) = dollar_tag(sql, i) {
                let marker = format!("${tag}$");
                let j = sql[i + marker.len()..].find(&marker).map(|at| i + marker.len() + at + marker.len()).unwrap_or(sql.len());
                push(&mut tokens, TokKind::Dollar, &sql[i..j], gap, None);
                i = j;
                continue;
            }
            if ch_at(sql, step(sql, i)).is_some_and(|next| next.is_ascii_digit()) {
                let mut j = step(sql, step(sql, i));
                while ch_at(sql, j).is_some_and(|next| next.is_ascii_digit()) {
                    j = step(sql, j);
                }
                push(&mut tokens, TokKind::Word, &sql[i..j], gap, None);
                i = j;
                continue;
            }
        }
        if matches!(ch, 'x' | 'X' | 'b' | 'B') && ch_at(sql, step(sql, i)) == Some('\'') {
            let j = skip_quoted(sql, step(sql, i));
            push(&mut tokens, TokKind::Number, &sql[i..j], gap, None);
            i = j;
            continue;
        }
        if let Some(number_end) = scan_number(sql, i) {
            push(&mut tokens, TokKind::Number, &sql[i..number_end], gap, None);
            i = number_end;
            continue;
        }
        if let Some((text, end)) = sql_word_at(sql, i) {
            let upper = text.to_uppercase();
            push(&mut tokens, TokKind::Word, &text, gap, Some(upper));
            i = end;
            continue;
        }
        if sql[i..].starts_with("->>") {
            push(&mut tokens, TokKind::Punct, "->>", gap, None);
            i += 3;
            continue;
        }
        let two = sql.get(i..i + 2).unwrap_or("");
        if matches!(two, "::" | "->" | "<>" | "<=" | ">=" | "!=" | "||") {
            push(&mut tokens, TokKind::Punct, two, gap, None);
            i += 2;
            continue;
        }
        if (ch == ':' || ch == '@') && two != "::" {
            if let Some((_, end)) = sql_word_at(sql, step(sql, i)) {
                push(&mut tokens, TokKind::Word, &sql[i..end], gap, None);
                i = end;
                continue;
            }
        }
        let next = step(sql, i);
        push(&mut tokens, TokKind::Punct, &sql[i..next], gap, None);
        i = next;
    }
    tokens
}

fn push(tokens: &mut Vec<Tok>, kind: TokKind, text: &str, gap: String, upper: Option<String>) {
    tokens.push(Tok { kind, text: text.to_owned(), gap, upper });
}

fn holds(prev: &Tok, tok: &Tok) -> bool {
    let (Some(left), Some(right)) = (prev.upper.as_deref(), tok.upper.as_deref()) else {
        return false;
    };
    matches!(
        (left, right),
        ("SELECT", "DISTINCT")
            | ("GROUP", "BY")
            | ("ORDER", "BY")
            | ("INSERT", "INTO")
            | ("UNION", "ALL")
            | ("LEFT", "OUTER" | "JOIN")
            | ("RIGHT", "OUTER" | "JOIN")
            | ("INNER", "JOIN")
            | ("FULL", "OUTER" | "JOIN")
            | ("CROSS", "JOIN")
            | ("OUTER", "JOIN")
            | ("IS", "NOT" | "NULL")
            | ("NOT", "NULL" | "IN" | "EXISTS")
    )
}

fn break_kind(tok: &Tok, prev: Option<&Tok>) -> Option<&'static str> {
    let upper = tok.upper.as_deref()?;
    if tok.kind != TokKind::Word {
        return None;
    }
    if prev.is_some_and(|prev| holds(prev, tok)) {
        return None;
    }
    if CLAUSE.contains(&upper) {
        Some("clause")
    } else if SUB.contains(&upper) {
        Some("sub")
    } else {
        None
    }
}

fn render_tok(tok: &Tok) -> String {
    if tok.kind == TokKind::Word {
        if let Some(upper) = tok.upper.as_deref() {
            if SQL_KEYWORDS.contains(&upper) {
                return upper.to_owned();
            }
        }
    }
    tok.text.clone()
}

fn layout(tokens: &[Tok]) -> (bool, String) {
    let mut out = String::new();
    let mut depth = 0i32;
    let mut break_body = false;
    let mut prev: Option<&Tok> = None;
    for tok in tokens {
        let kind = break_kind(tok, prev);
        let line_comment = prev.is_some_and(|prev| prev.kind == TokKind::Comment && (prev.text.starts_with("--") || prev.text.starts_with('#')));
        let mut placed = "none";
        let mut level = 0usize;
        let mut next_break = break_body;
        if depth == 0 && kind == Some("clause") {
            if !out.trim().is_empty() {
                placed = "newline";
                level = 0;
            }
            next_break = true;
        } else if depth == 0 && kind == Some("sub") {
            if !out.trim().is_empty() {
                placed = "newline";
                level = 1;
            }
        } else if line_comment && !out.trim().is_empty() {
            placed = "newline";
            level = 1 + depth as usize;
            next_break = false;
        } else if break_body && depth == 0 && !prev.is_some_and(|prev| holds(prev, tok)) {
            placed = "newline";
            level = 1;
            next_break = false;
        } else if prev.is_some_and(|prev| prev.kind == TokKind::Punct && prev.text == ",") && depth == 0 && tok.kind != TokKind::Comment {
            placed = "newline";
            level = 1;
        } else if !tight(tok, prev) {
            placed = "space";
        }
        if placed == "newline" {
            if tok.gap.is_empty() {
                return (false, String::new());
            }
            newline(&mut out, level);
        } else if placed == "space" && !tok.gap.is_empty() {
            space(&mut out);
        }
        out.push_str(&render_tok(tok));
        if tok.kind == TokKind::Punct && tok.text == "(" {
            depth += 1;
        }
        if tok.kind == TokKind::Punct && tok.text == ")" {
            depth = depth.saturating_sub(1);
        }
        break_body = next_break;
        prev = Some(tok);
    }
    (true, out.trim_end_matches([' ', '\t']).to_owned())
}

fn newline(out: &mut String, level: usize) {
    while out.ends_with(' ') || out.ends_with('\t') {
        out.pop();
    }
    if !out.is_empty() {
        out.push('\n');
    }
    for _ in 0..level {
        out.push_str("  ");
    }
}

fn space(out: &mut String) {
    if out.is_empty() {
        return;
    }
    let last = out.chars().last().unwrap_or(' ');
    if last.is_whitespace() || last == '.' || last == '(' {
        return;
    }
    out.push(' ');
}

fn tight(tok: &Tok, prev: Option<&Tok>) -> bool {
    if tok.kind == TokKind::Punct && matches!(tok.text.as_str(), "," | ";" | "." | ")") {
        return true;
    }
    if tok.kind == TokKind::Punct && tok.text == "(" {
        if let Some(prev) = prev {
            if prev.kind == TokKind::Word {
                if let Some(upper) = prev.upper.as_deref() {
                    if !CLAUSE.contains(&upper) && !SUB.contains(&upper) {
                        return true;
                    }
                }
            }
        }
    }
    prev.is_some_and(|prev| prev.kind == TokKind::Punct && (prev.text == "(" || prev.text == "."))
}

fn keeps_adjacent(source: &str, formatted: &str) -> bool {
    let left: Vec<char> = source.chars().collect();
    let right: Vec<char> = formatted.chars().collect();
    let mut i = 0usize;
    let mut j = 0usize;
    while i < left.len() && j < right.len() {
        if left[i].is_whitespace() && right[j].is_whitespace() {
            while i < left.len() && left[i].is_whitespace() {
                i += 1;
            }
            while j < right.len() && right[j].is_whitespace() {
                j += 1;
            }
            continue;
        }
        if left[i].is_whitespace() {
            while i < left.len() && left[i].is_whitespace() {
                i += 1;
            }
            continue;
        }
        if right[j].is_whitespace() {
            return false;
        }
        if left[i].to_uppercase().to_string() != right[j].to_uppercase().to_string() {
            return false;
        }
        i += 1;
        j += 1;
    }
    while i < left.len() && left[i].is_whitespace() {
        i += 1;
    }
    while j < right.len() && right[j].is_whitespace() {
        j += 1;
    }
    i == left.len() && j == right.len()
}

fn line_comment_broken(source: &str, formatted: &str) -> bool {
    let marked = line_comment_marks(source);
    let source: Vec<char> = source.chars().collect();
    let formatted: Vec<char> = formatted.chars().collect();
    let mut i = 0usize;
    let mut j = 0usize;
    while i < source.len() && j < formatted.len() {
        if source[i].is_whitespace() && formatted[j].is_whitespace() {
            let source_next = skip_blank(&source, i);
            let formatted_next = skip_blank(&formatted, j);
            if formatted_next.1 && comment_continues(&marked, source_next.0) {
                return true;
            }
            i = source_next.0;
            j = formatted_next.0;
            continue;
        }
        if source[i].is_whitespace() {
            i = skip_blank(&source, i).0;
            continue;
        }
        if formatted[j].is_whitespace() {
            let formatted_next = skip_blank(&formatted, j);
            if formatted_next.1 && comment_continues(&marked, i) {
                return true;
            }
            j = formatted_next.0;
            continue;
        }
        i += 1;
        j += 1;
    }
    false
}

fn comment_continues(marked: &[bool], index: usize) -> bool {
    index > 0 && index < marked.len() && marked[index] && marked[index - 1]
}

fn skip_blank(text: &[char], index: usize) -> (usize, bool) {
    let mut next = index;
    let mut newline = false;
    while next < text.len() && text[next].is_whitespace() {
        if text[next] == '\n' || text[next] == '\r' {
            newline = true;
        }
        next += 1;
    }
    (next, newline)
}

fn line_comment_marks(sql: &str) -> Vec<bool> {
    let chars: Vec<char> = sql.chars().collect();
    let mut marked = vec![false; chars.len()];
    let mut i = 0usize;
    let mut quote: Option<char> = None;
    let mut dollar: Option<String> = None;
    let mut block = false;
    while i < chars.len() {
        if let Some(tag) = dollar.clone() {
            let marker: Vec<char> = format!("${tag}$").chars().collect();
            if chars[i..].starts_with(&marker) {
                i += marker.len();
                dollar = None;
                continue;
            }
            i += 1;
            continue;
        }
        if let Some(q) = quote {
            if chars[i] == q && chars.get(i + 1) == Some(&q) {
                i += 2;
                continue;
            }
            if chars[i] == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        if block {
            if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                block = false;
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        if chars[i] == '#' || (chars[i] == '-' && chars.get(i + 1) == Some(&'-')) {
            let start = i;
            i += if chars[i] == '#' { 1 } else { 2 };
            while i < chars.len() && chars[i] != '\n' && chars[i] != '\r' {
                i += 1;
            }
            for slot in &mut marked[start..i] {
                *slot = true;
            }
            continue;
        }
        if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
            block = true;
            i += 2;
            continue;
        }
        if matches!(chars[i], '\'' | '"' | '`') {
            quote = Some(chars[i]);
            i += 1;
            continue;
        }
        if chars[i] == '$' {
            let rest: String = chars[i..].iter().collect();
            if let Some(tag) = dollar_tag(&rest, 0) {
                dollar = Some(tag.clone());
                i += tag.chars().count() + 2;
                continue;
            }
        }
        i += 1;
    }
    marked
}

fn edge_whitespace_removed(sql: &str, from: usize, to: usize, formatted: &str) -> bool {
    let prefix = sql.get(..from).unwrap_or("");
    let selected = sql.get(from..to).unwrap_or("");
    let suffix = sql.get(to..).unwrap_or("");
    if selected.is_empty() || (prefix.is_empty() && suffix.is_empty()) {
        return false;
    }
    let selected_chars: Vec<char> = selected.chars().collect();
    let formatted_chars: Vec<char> = formatted.chars().collect();
    if selected_chars.first().is_some_and(|ch| ch.is_whitespace()) && formatted_chars.first().is_none_or(|ch| !ch.is_whitespace()) && outside_touches(prefix, formatted, true) {
        return true;
    }
    if selected_chars.last().is_some_and(|ch| ch.is_whitespace()) && formatted_chars.last().is_none_or(|ch| !ch.is_whitespace()) && outside_touches(suffix, formatted, false) {
        return true;
    }
    if selected.chars().all(char::is_whitespace) && formatted.is_empty() {
        let left = prefix.chars().last();
        let right = suffix.chars().next();
        return left.is_some_and(|ch| !ch.is_whitespace()) && right.is_some_and(|ch| !ch.is_whitespace());
    }
    false
}

fn outside_touches(outside: &str, inside: &str, outside_is_before: bool) -> bool {
    if outside.is_empty() || inside.is_empty() {
        return false;
    }
    let outside_char = if outside_is_before { outside.chars().last() } else { outside.chars().next() };
    let inside_char = if outside_is_before { inside.chars().next() } else { inside.chars().last() };
    outside_char.is_some_and(|ch| !ch.is_whitespace()) && inside_char.is_some_and(|ch| !ch.is_whitespace())
}

fn dollar_tag(sql: &str, start: usize) -> Option<String> {
    let rest = sql.get(start + 1..)?;
    let end = rest.find('$')?;
    let tag = &rest[..end];
    if tag.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_') { Some(tag.to_owned()) } else { None }
}

fn sql_word_at(sql: &str, start: usize) -> Option<(String, usize)> {
    let first = ch_at(sql, start)?;
    if !ident_start(first) {
        return None;
    }
    let mut end = step(sql, start);
    while end < sql.len() && ch_at(sql, end).is_some_and(ident_cont) {
        end = step(sql, end);
    }
    Some((sql[start..end].to_owned(), end))
}

fn ident_start(ch: char) -> bool {
    ch == '_' || ch.is_alphabetic()
}

fn ident_cont(ch: char) -> bool {
    ch == '_' || ch == '$' || ch.is_alphanumeric()
}

fn skip_quoted(sql: &str, start: usize) -> usize {
    let quote = ch_at(sql, start).unwrap_or('\'');
    let mut j = step(sql, start);
    while j < sql.len() {
        if ch_at(sql, j) == Some(quote) && ch_at(sql, step(sql, j)) == Some(quote) {
            j = step(sql, step(sql, j));
            continue;
        }
        if ch_at(sql, j) == Some(quote) {
            return step(sql, j);
        }
        j = step(sql, j);
    }
    sql.len()
}

fn eat_digits(sql: &str, index: usize) -> usize {
    let mut j = index;
    while j < sql.len() {
        if ch_at(sql, j).is_some_and(|ch| ch.is_ascii_digit()) {
            j = step(sql, j);
            continue;
        }
        if ch_at(sql, j) == Some('_') && ch_at(sql, step(sql, j)).is_some_and(|ch| ch.is_ascii_digit()) {
            j = step(sql, step(sql, j));
            continue;
        }
        break;
    }
    j
}

fn scan_exponent(sql: &str, index: usize) -> Option<usize> {
    let mark = ch_at(sql, index)?;
    if mark != 'e' && mark != 'E' {
        return None;
    }
    let mut k = step(sql, index);
    if matches!(ch_at(sql, k), Some('+' | '-')) {
        k = step(sql, k);
    }
    if !ch_at(sql, k).is_some_and(|ch| ch.is_ascii_digit()) {
        return None;
    }
    while ch_at(sql, k).is_some_and(|ch| ch.is_ascii_digit()) {
        k = step(sql, k);
    }
    Some(k)
}

fn scan_number(sql: &str, start: usize) -> Option<usize> {
    let head = ch_at(sql, start)?;
    if !head.is_ascii_digit() {
        return None;
    }
    let mark = ch_at(sql, step(sql, start)).unwrap_or(' ');
    if head == '0' && (mark == 'x' || mark == 'X') && ch_at(sql, step(sql, step(sql, start))).is_some_and(|ch| ch.is_ascii_hexdigit()) {
        let mut j = step(sql, step(sql, step(sql, start)));
        while ch_at(sql, j).is_some_and(|ch| ch.is_ascii_hexdigit()) {
            j = step(sql, j);
        }
        return Some(j);
    }
    if head == '0' && (mark == 'b' || mark == 'B') && matches!(ch_at(sql, step(sql, step(sql, start))), Some('0' | '1')) {
        let mut j = step(sql, step(sql, step(sql, start)));
        while matches!(ch_at(sql, j), Some('0' | '1')) {
            j = step(sql, j);
        }
        return Some(j);
    }
    let mut j = eat_digits(sql, start);
    if ch_at(sql, j) == Some('.') {
        if let Some(dotted) = scan_exponent(sql, step(sql, j)) {
            if !ch_at(sql, step(sql, j)).is_some_and(|ch| ch.is_ascii_digit()) {
                return Some(dotted);
            }
        }
        if ch_at(sql, step(sql, j)).is_some_and(|ch| ch.is_ascii_digit()) {
            j = eat_digits(sql, step(sql, j));
        }
    }
    Some(scan_exponent(sql, j).unwrap_or(j))
}

fn ch_at(sql: &str, i: usize) -> Option<char> {
    sql.get(i..)?.chars().next()
}

fn step(sql: &str, i: usize) -> usize {
    i + ch_at(sql, i).map(|ch| ch.len_utf8()).unwrap_or(0)
}

fn eol(sql: &str, mut i: usize) -> usize {
    while i < sql.len() && ch_at(sql, i) != Some('\n') && ch_at(sql, i) != Some('\r') {
        i = step(sql, i);
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_keeps_strings_comments_and_rejects_open_quotes() {
        let sample = "select   status , 'O pen'  -- keep me\nfrom   orders\nwhere status='open' and note = 'it''s'";
        let formatted = format_sql(sample);
        assert!(formatted.ok, "{formatted:?}");
        assert!(formatted.sql.contains("'O pen'"));
        assert!(formatted.sql.contains("-- keep me"));
        assert!(formatted.sql.contains("'it''s'"));
        assert!(formatted.sql.starts_with("SELECT\n"));
        assert_eq!(format_sql(&formatted.sql).sql, formatted.sql);
        for sql in ["select 'abc", "select 1 /* hi", "select $tag$ hi"] {
            let result = format_sql(sql);
            assert!(!result.ok);
            assert_eq!(result.sql, sql);
        }
    }

    #[test]
    fn selection_is_exact_and_unclosed_sql_is_not_rewritten() {
        let sql = "SELECT 1;\nSELECT 2;";
        let from = sql.find("ELECT 2").unwrap();
        let pick = sql_to_run(sql, from, from + "ELECT 2".len(), from, RunScope::Auto, "sqlite");
        assert_eq!(pick.mode, "selection");
        assert_eq!(pick.sql, "ELECT 2");
        let current = sql_to_run(sql, 0, 0, 0, RunScope::Current, "sqlite");
        assert_eq!(current.sql.trim(), "SELECT 1;");
        let open = "SELECT 'abc";
        assert!(!format_sql(open).ok);
        assert_eq!(format_sql(open).sql, open);
        assert!(prepare_send(open, &sql_to_run(open, 0, 0, 0, RunScope::Script, "sqlite"), "sqlite").is_err());
        assert_eq!(expand_star("nope", "SELECT * FROM t", &[], true).sql, "SELECT * FROM t");
        let unknown = CompletionScope { engine: "nope".to_owned(), provider: true, ..Default::default() };
        assert!(unknown.complete("sel", crate::i18n::Locale::ZhCn).is_empty());
    }

    #[test]
    fn variables_outline_star_and_plan_keep_the_documented_shape() {
        let inputs = find_variables("select :name, :name, ?, 'not :here'");
        assert_eq!(inputs.iter().map(|item| item.key.as_str()).collect::<Vec<_>>(), vec!["name", "?1"]);
        let mut values = HashMap::new();
        values.insert("name".to_owned(), "O'Hare".to_owned());
        values.insert("?1".to_owned(), String::new());
        assert_eq!(substitute_variables("select :name, ?", &values), "select 'O''Hare', NULL");
        let outline = outline_statements("select 1;\nselect 2;", "sqlite");
        assert_eq!(outline.len(), 2);
        let merged = merge_outline(outline, &[("select 1;".to_owned(), "ok".to_owned(), 1, None)]);
        assert_eq!(merged[0].status, "ok");
        assert_eq!(merged[1].status, "none");
        let sources = vec![CompletionSource { alias: String::new(), label: "t".to_owned(), columns: vec![ColumnMeta::named("id"), ColumnMeta::named("name")] }];
        let expanded = expand_star("sqlite", "SELECT * FROM t", &sources, true);
        assert_eq!(expanded.sql, "SELECT id, name FROM t");
        assert_eq!(expand_star("sqlite", "SELECT *, COUNT(*) FROM t", &sources, true).sql, "SELECT id, name, COUNT(*) FROM t");
        assert_eq!(expand_star("sqlite", "SELECT COUNT(*) FROM t", &sources, true).sql, "SELECT COUNT(*) FROM t");
        assert_eq!(expand_star("sqlite", "SELECT * FROM t", &[], true).sql, "SELECT * FROM t");
        let plan = explain_sql("sqlite", "SELECT * FROM t").unwrap();
        assert!(plan.starts_with("EXPLAIN QUERY PLAN"));
        assert!(explain_sql("sqlite", "EXPLAIN ANALYZE SELECT 1").is_err());
        let lint = lint_sql("select 'abc", "sqlite");
        assert_eq!(lint[0].message_key, "query.split.quoteUnclosed");
    }

    #[test]
    fn sql_file_load_does_not_execute_and_rejects_oversize() {
        let dir = std::env::temp_dir().join(format!("rillsql-sql-file-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("draft.sql");
        write_sql_file(&path, "SELECT 1;").unwrap();
        let loaded = read_sql_file(&path).unwrap();
        assert_eq!(loaded.sql, "SELECT 1;");
        assert!(!loaded.execute);
        assert!(sql_file_problem(MAX_SQL_FILE_BYTES + 1).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
