//! 语句结构化 SQL 补全。
//!
//! 这里只做纯计算：当前语句边界、子句与来源（含别名、CTE、ON 两侧、UPDATE/INSERT 目标）、
//! 候选计划、前缀/子序列匹配、排序去重、替换范围与按引擎引用标识符。
//! 编辑器和 Desk 只把快照传进来，这里不读写任何草稿、票据或 UI 状态。

use std::collections::{HashMap, HashSet};

use crate::i18n::{self, Locale};

use super::assist::{
    dialect_words, known_engine, quote_ident, CatalogObject, CompletionEntry, CompletionScope, CompletionSource, MetadataState,
};

/// 只回看这么多字节找当前语句；更长的语句用窗口起点兜底，解析仍然可用。
const WINDOW: usize = 8 * 1024;
/// 一次最多给出的候选数；分组上限见 Gather::push。
pub const MAX_ITEMS: usize = 120;

/// 当前语句的子句；解析不到时是 None，调用方回退到通用场景。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Clause {
    Select,
    From,
    Join,
    On,
    Where,
    Group,
    Order,
    Having,
    Set,
    Values,
    Use,
}

impl Clause {
    fn of(word: &str) -> Option<Self> {
        Some(match word {
            "SELECT" => Clause::Select,
            "FROM" => Clause::From,
            "JOIN" => Clause::Join,
            "ON" => Clause::On,
            "WHERE" => Clause::Where,
            "GROUP" => Clause::Group,
            "ORDER" => Clause::Order,
            "HAVING" => Clause::Having,
            "SET" => Clause::Set,
            "VALUES" => Clause::Values,
            "USE" => Clause::Use,
            _ => return None,
        })
    }

    fn field_first(self) -> bool {
        matches!(self, Clause::Select | Clause::Where | Clause::Group | Clause::Order | Clause::Having)
    }

    fn needs_columns(self) -> bool {
        self.field_first() || matches!(self, Clause::On | Clause::Set | Clause::Values)
    }
}

/// FROM/JOIN/UPDATE/INSERT 里的一处来源。alias 是语句里实际使用的名字。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TableRef {
    pub alias: String,
    pub schema: String,
    pub name: String,
    pub depth: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StatementScope {
    pub clause: Option<Clause>,
    pub tables: Vec<TableRef>,
    pub ctes: Vec<String>,
    pub target: Option<TableRef>,
    /// ON 两侧参与比较的来源别名（最近一次 JOIN 之前的全部来源）。
    pub on_tables: Vec<String>,
}

/// 候选被接受时替换的字节范围。只覆盖当前词，不动限定符以外的文本。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub label: String,
    pub insert: String,
    pub kind: &'static str,
    pub detail: String,
    /// 分组顺序来自子句场景，见 group 模块。
    pub group: u8,
    pub keyword: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub candidates: Vec<Candidate>,
    /// 元数据状态说明（已翻译）。失败、部分失败、空目录都不能显示成成功的空列表。
    pub note: Option<String>,
    pub span: Span,
    pub clause: Option<Clause>,
}

impl Plan {
    /// Desk 侧使用的条目：保留标签、类型、详情和插入文本。
    pub fn entries(&self) -> Vec<CompletionEntry> {
        self.candidates
            .iter()
            .map(|item| CompletionEntry {
                label: item.label.clone(),
                kind: item.kind.to_owned(),
                detail: Some(item.detail.clone()),
                insert: item.insert.clone(),
            })
            .collect()
    }
}

/// 子句里的候选分组顺序；数字越小越靠前。
mod group {
    pub const CONDITION: u8 = 0;
    pub const COLUMN: u8 = 1;
    pub const OBJECT: u8 = 2;
    pub const CTE: u8 = 3;
    pub const DATABASE: u8 = 4;
    pub const SCHEMA: u8 = 5;
    pub const FUNCTION: u8 = 6;
    pub const KEYWORD: u8 = 7;
    pub const SNIPPET: u8 = 8;
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Tok {
    text: String,
    word: bool,
    start: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Lexed {
    /// 游标落在未闭合的字符串、引用标识符或注释里。
    open: bool,
    /// 窗口里最后一个语句分隔符；字符串和注释里的分号已经跳过。
    last_mark: Option<usize>,
}

/// 语句词法：跳过字符串、引用标识符和行/块注释，只保留判断子句需要的记号。
fn lex(text: &str) -> (Vec<Tok>, Lexed) {
    let mut out = Vec::new();
    let mut info = Lexed::default();
    let bytes = text.as_bytes();
    let mut at = 0usize;
    while at < bytes.len() {
        let ch = text[at..].chars().next().unwrap_or('\u{fffd}');
        if ch.is_whitespace() {
            at += ch.len_utf8();
            continue;
        }
        if ch == '-' && bytes.get(at + 1) == Some(&b'-') {
            match text[at..].find('\n') {
                Some(found) => at += found + 1,
                None => {
                    info.open = true;
                    break;
                }
            }
            continue;
        }
        if ch == '/' && bytes.get(at + 1) == Some(&b'*') {
            match text[at + 2..].find("*/") {
                Some(found) => at = at + 2 + found + 2,
                None => {
                    info.open = true;
                    break;
                }
            }
            continue;
        }
        if ch == '\'' {
            let mut end = at + 1;
            let mut closed = false;
            while end < bytes.len() {
                if bytes[end] == b'\'' {
                    if bytes.get(end + 1) == Some(&b'\'') {
                        end += 2;
                        continue;
                    }
                    end += 1;
                    closed = true;
                    break;
                }
                end += 1;
            }
            if !closed {
                info.open = true;
                break;
            }
            at = end;
            continue;
        }
        if ch == '"' || ch == '\u{60}' {
            let quote = ch;
            let mut end = at + quote.len_utf8();
            let mut closed = false;
            while end < bytes.len() {
                let next = text[end..].chars().next().unwrap_or('\u{fffd}');
                if next == quote {
                    let after = end + next.len_utf8();
                    if text[after..].starts_with(quote) {
                        end = after + quote.len_utf8();
                        continue;
                    }
                    end = after;
                    closed = true;
                    break;
                }
                end += next.len_utf8();
            }
            if !closed {
                info.open = true;
                break;
            }
            let raw = &text[at + 1..end - 1];
            let doubled = format!("{quote}{quote}");
            let name = raw.replace(&doubled, &quote.to_string());
            out.push(Tok { text: name, word: true, start: at });
            at = end;
            continue;
        }
        if ch == '[' {
            match text[at + 1..].find(']') {
                Some(found) => {
                    out.push(Tok { text: text[at + 1..at + 1 + found].to_owned(), word: true, start: at });
                    at = at + 1 + found + 1;
                }
                None => {
                    info.open = true;
                    break;
                }
            }
            continue;
        }
        if ch.is_ascii_digit() {
            let mut end = at;
            while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b'.') {
                end += 1;
            }
            at = end;
            continue;
        }
        if ch.is_alphanumeric() || ch == '_' {
            let mut end = at;
            while end < bytes.len() {
                let byte = bytes[end];
                if byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$' {
                    end += 1;
                    continue;
                }
                let next = text[end..].chars().next().unwrap_or('\u{fffd}');
                if !next.is_ascii() && next.is_alphanumeric() {
                    end += next.len_utf8();
                    continue;
                }
                break;
            }
            out.push(Tok { text: text[at..end].to_owned(), word: true, start: at });
            at = end;
            continue;
        }
        if matches!(ch, '.' | ',' | '(' | ')' | ';') {
            out.push(Tok { text: ch.to_string(), word: false, start: at });
            if ch == ';' {
                info.last_mark = Some(at);
            }
        }
        at += ch.len_utf8();
    }
    (out, info)
}

fn reserved(word: &str) -> bool {
    matches!(
        word,
        "SELECT"
            | "FROM"
            | "WHERE"
            | "AND"
            | "OR"
            | "NOT"
            | "NULL"
            | "IS"
            | "IN"
            | "EXISTS"
            | "INSERT"
            | "INTO"
            | "VALUES"
            | "UPDATE"
            | "SET"
            | "DELETE"
            | "CREATE"
            | "ALTER"
            | "DROP"
            | "TABLE"
            | "VIEW"
            | "INDEX"
            | "JOIN"
            | "LEFT"
            | "RIGHT"
            | "INNER"
            | "OUTER"
            | "FULL"
            | "CROSS"
            | "ON"
            | "GROUP"
            | "BY"
            | "ORDER"
            | "LIMIT"
            | "OFFSET"
            | "AS"
            | "DISTINCT"
            | "HAVING"
            | "UNION"
            | "ALL"
            | "CASE"
            | "WHEN"
            | "THEN"
            | "ELSE"
            | "END"
            | "WITH"
            | "USING"
            | "RETURNING"
            | "ASC"
            | "DESC"
            | "NULLS"
            | "FIRST"
            | "LAST"
            | "USE"
    )
}

/// 解析当前语句：子句、FROM/JOIN 来源与别名、WITH 定义的 CTE、UPDATE/INSERT 目标。
pub fn parse_scope(statement: &str) -> StatementScope {
    let (tokens, _) = lex(statement);
    let mut scope = StatementScope::default();
    let mut depth = 0usize;
    let mut here = 0usize;
    let mut index = 0usize;
    while index < tokens.len() {
        let token = &tokens[index];
        if !token.word {
            match token.text.as_str() {
                "(" => depth += 1,
                ")" => depth = depth.saturating_sub(1),
                _ => {}
            }
            here = depth;
            index += 1;
            continue;
        }
        let upper = token.text.to_ascii_uppercase();
        if upper == "WITH" {
            index = read_ctes(&tokens, index + 1, &mut scope.ctes);
            continue;
        }
        if upper == "UPDATE" || (upper == "INTO" && scope.target.is_none()) {
            let (table, next) = read_target(&tokens, index + 1, here);
            if table.is_some() {
                scope.target = table;
            }
            index = next;
            continue;
        }
        if upper == "FROM" || upper == "JOIN" {
            scope.clause = Some(if upper == "FROM" { Clause::From } else { Clause::Join });
            let next = read_tables(&tokens, index + 1, &mut scope.tables, here);
            if upper == "JOIN" {
                scope.on_tables = scope.tables.iter().map(|table| table.alias.clone()).collect();
            }
            index = next;
            continue;
        }
        if let Some(clause) = Clause::of(&upper) {
            scope.clause = Some(clause);
        }
        index += 1;
    }
    scope.tables.retain(|table| table.depth <= depth);
    scope
}

fn read_target(tokens: &[Tok], start: usize, depth: usize) -> (Option<TableRef>, usize) {
    let Some(first) = tokens.get(start) else {
        return (None, start);
    };
    if !first.word || reserved(&first.text.to_ascii_uppercase()) {
        return (None, start);
    }
    let mut schema = String::new();
    let mut name = first.text.clone();
    let mut next = start + 1;
    if tokens.get(next).is_some_and(|token| token.text == ".") {
        if let Some(second) = tokens.get(next + 1).filter(|token| token.word) {
            schema = first.text.clone();
            name = second.text.clone();
            next += 2;
        }
    }
    let mut alias = name.clone();
    if let Some(after) = tokens.get(next) {
        let upper = after.text.to_ascii_uppercase();
        if after.word && upper == "AS" {
            if let Some(named) = tokens.get(next + 1).filter(|token| token.word) {
                alias = named.text.clone();
                next += 2;
            }
        } else if after.word && !reserved(&upper) {
            alias = after.text.clone();
            next += 1;
        }
    }
    (Some(TableRef { alias, schema, name, depth }), next)
}

fn read_ctes(tokens: &[Tok], start: usize, ctes: &mut Vec<String>) -> usize {
    let mut index = start;
    loop {
        let Some(name) = tokens.get(index) else {
            return index;
        };
        if !name.word || reserved(&name.text.to_ascii_uppercase()) {
            return index;
        }
        let Some(marker) = tokens.get(index + 1) else {
            return index;
        };
        if !marker.text.eq_ignore_ascii_case("AS") {
            return index;
        }
        ctes.push(name.text.clone());
        index += 2;
        if tokens.get(index).is_some_and(|token| token.text == "(") {
            let mut depth = 0usize;
            while index < tokens.len() {
                match tokens[index].text.as_str() {
                    "(" => depth += 1,
                    ")" => {
                        depth = depth.saturating_sub(1);
                        if depth == 0 {
                            index += 1;
                            break;
                        }
                    }
                    _ => {}
                }
                index += 1;
            }
        }
        if tokens.get(index).is_some_and(|token| token.text == ",") {
            index += 1;
            continue;
        }
        return index;
    }
}

fn read_tables(tokens: &[Tok], start: usize, tables: &mut Vec<TableRef>, depth: usize) -> usize {
    let mut index = start;
    loop {
        let Some(first) = tokens.get(index) else {
            return index;
        };
        if !first.word && first.text == "(" {
            let mut level = 0usize;
            while index < tokens.len() {
                match tokens[index].text.as_str() {
                    "(" => level += 1,
                    ")" => {
                        level = level.saturating_sub(1);
                        if level == 0 {
                            index += 1;
                            break;
                        }
                    }
                    _ => {}
                }
                index += 1;
            }
            if let Some(derived) = tokens.get(index).filter(|token| token.word) {
                let alias = derived.text.clone();
                tables.push(TableRef { alias: alias.clone(), schema: String::new(), name: alias, depth });
                index += 1;
            }
            if tokens.get(index).is_some_and(|token| token.text == ",") {
                index += 1;
                continue;
            }
            return index;
        }
        if !first.word {
            return index;
        }
        let upper = first.text.to_ascii_uppercase();
        if reserved(&upper) {
            return index;
        }
        let mut schema = String::new();
        let mut name = first.text.clone();
        let mut next = index + 1;
        if tokens.get(next).is_some_and(|token| token.text == ".") {
            if let Some(second) = tokens.get(next + 1).filter(|token| token.word) {
                schema = first.text.clone();
                name = second.text.clone();
                next += 2;
            }
        }
        let mut alias = name.clone();
        if let Some(after) = tokens.get(next) {
            let after_upper = after.text.to_ascii_uppercase();
            if after.word && after_upper == "AS" {
                if let Some(named) = tokens.get(next + 1).filter(|token| token.word) {
                    alias = named.text.clone();
                    next += 2;
                }
            } else if after.word && !reserved(&after_upper) {
                alias = after.text.clone();
                next += 1;
            }
        }
        tables.push(TableRef { alias, schema, name, depth });
        if tokens.get(next).is_some_and(|token| token.text == ",") {
            index = next + 1;
            continue;
        }
        return next;
    }
}

/// 语句里已经解析出来、并且能在当前元数据里找到列的来源。
#[derive(Clone, Debug, PartialEq, Eq)]
struct Source {
    alias: String,
    label: String,
    name: String,
    /// 来源所在模式；解析不到时为空。外键匹配按它区分同名表。
    schema: String,
    columns: Vec<String>,
}

fn source_columns<'a>(scope: &'a CompletionScope, table: &TableRef) -> Option<&'a CompletionSource> {
    let name = table.name.to_lowercase();
    let tail = format!(".{name}");
    scope.sources.iter().find(|source| {
        let label = source.label.to_lowercase();
        if table.schema.is_empty() {
            label == name || label.ends_with(&tail)
        } else {
            label == format!("{}.{}", table.schema.to_lowercase(), name) || (label == name && !label.contains('.'))
        }
    })
}

fn schema_of(label: &str) -> String {
    match label.rsplit_once('.') {
        Some((schema, _)) => schema.to_owned(),
        None => String::new(),
    }
}

fn resolve_sources(scope: &CompletionScope, tables: &[TableRef]) -> Vec<Source> {
    tables
        .iter()
        .map(|table| {
            let found = source_columns(scope, table);
            let schema = if table.schema.is_empty() {
                found.map(|source| schema_of(&source.label)).unwrap_or_default()
            } else {
                table.schema.clone()
            };
            Source {
                alias: table.alias.clone(),
                label: if table.schema.is_empty() { table.name.clone() } else { format!("{}.{}", table.schema, table.name) },
                name: table.name.clone(),
                schema,
                columns: found.map(|source| source.columns.clone()).unwrap_or_default(),
            }
        })
        .collect()
}

fn matched(label: &str, prefix: &str) -> bool {
    match_kind(label, prefix) >= 0
}

/// 语句里可计数的标识符（小写）。关键字、字符串、引用符和注释都不计入。
pub fn identifiers(sql: &str) -> Vec<String> {
    let (tokens, _) = lex(sql);
    tokens
        .iter()
        .filter(|token| token.word)
        .map(|token| token.text.to_ascii_lowercase())
        .filter(|word| !word.is_empty() && !reserved(&word.to_ascii_uppercase()))
        .filter(|word| !BASE_KEYWORDS.contains(&word.to_ascii_uppercase().as_str()))
        .collect()
}

/// 0：前缀命中；1：子序列命中；-1：不命中。大小写不敏感。
pub fn match_kind(label: &str, prefix: &str) -> i32 {
    if prefix.is_empty() {
        return 0;
    }
    let haystack = label.to_lowercase();
    let needle = prefix.to_lowercase();
    if haystack.starts_with(&needle) {
        return 0;
    }
    let chars: Vec<char> = needle.chars().collect();
    let mut index = 0usize;
    for ch in haystack.chars() {
        if index < chars.len() && ch == chars[index] {
            index += 1;
        }
        if index == chars.len() {
            return 1;
        }
    }
    -1
}

fn plain_ident(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '$')
}

/// PostgreSQL 会把没加引号的标识符折叠成小写，CamelCase 名字必须引用才指得回原对象。
fn folds_case(engine: &str, name: &str) -> bool {
    engine == "postgres" && name.chars().any(|ch| ch.is_uppercase())
}

/// 需要引号时才加引号，保留原大小写；引擎决定引号字符。
pub fn ident(engine: &str, name: &str) -> String {
    if plain_ident(name) && !folds_case(engine, name) {
        name.to_owned()
    } else {
        quote_ident(engine, name)
    }
}

fn qualified_ident(engine: &str, schema: &str, name: &str) -> String {
    if schema.is_empty() {
        ident(engine, name)
    } else {
        format!("{}.{}", ident(engine, schema), ident(engine, name))
    }
}

/// 目录里的对象类型是动态字符串，这里映射成可静态保存的类型标签。
fn static_kind(kind: &str) -> &'static str {
    match kind {
        "table" | "collection" => "table",
        "view" => "view",
        "function" => "function",
        "procedure" => "procedure",
        "trigger" => "trigger",
        "sequence" => "sequence",
        _ => "object",
    }
}

fn kind_key(kind: &str) -> &'static str {
    match kind {
        "table" | "collection" => "query.complete.kind.table",
        "view" => "query.complete.kind.view",
        "function" => "query.complete.kind.function",
        "procedure" => "query.complete.kind.procedure",
        "trigger" => "query.complete.kind.trigger",
        "sequence" => "query.complete.kind.sequence",
        _ => "query.complete.kind.object",
    }
}

fn function_signature(name: &str) -> Option<&'static str> {
    Some(match name.to_ascii_lowercase().as_str() {
        "count" => "count(expr) → bigint",
        "sum" => "sum(expr) → numeric",
        "avg" => "avg(expr) → numeric",
        "min" => "min(expr) → same",
        "max" => "max(expr) → same",
        "coalesce" => "coalesce(v1, v2, …) → value",
        "nullif" => "nullif(a, b) → value",
        "concat" => "concat(s1, s2, …) → text",
        "length" => "length(text) → int",
        "lower" => "lower(text) → text",
        "upper" => "upper(text) → text",
        "substring" => "substring(text, start, len) → text",
        "trim" => "trim(text) → text",
        "replace" => "replace(text, from, to) → text",
        "round" => "round(value, digits) → numeric",
        "floor" => "floor(value) → numeric",
        "ceil" => "ceil(value) → numeric",
        "abs" => "abs(value) → numeric",
        "now" => "now() → timestamp",
        "current_date" => "current_date → date",
        "cast" => "cast(expr as type) → type",
        "row_number" => "row_number() over (…) → bigint",
        "rank" => "rank() over (…) → bigint",
        "lag" => "lag(expr, offset) over (…) → value",
        "lead" => "lead(expr, offset) over (…) → value",
        "date_trunc" => "date_trunc(unit, value) → timestamp",
        "extract" => "extract(field from value) → numeric",
        "to_char" => "to_char(value, format) → text",
        "date_part" => "date_part(field, value) → double",
        "generate_series" => "generate_series(from, to, step) → set",
        "array_agg" => "array_agg(expr) → array",
        "string_agg" => "string_agg(expr, sep) → text",
        "jsonb_build_object" => "jsonb_build_object(key, value, …) → jsonb",
        "jsonb_agg" => "jsonb_agg(expr) → jsonb",
        "make_interval" => "make_interval(days, hours, …) → interval",
        "date_format" => "date_format(value, format) → text",
        "ifnull" => "ifnull(a, b) → value",
        "group_concat" => "group_concat(expr, sep) → text",
        "json_extract" => "json_extract(json, path) → value",
        "date_add" => "date_add(value, interval) → timestamp",
        "datediff" => "datediff(a, b) → int",
        "strftime" => "strftime(format, time, modifier) → text",
        "datetime" => "datetime(time, modifier) → text",
        "unixepoch" => "unixepoch(time) → integer",
        "json_group_array" => "json_group_array(value) → text",
        _ => return None,
    })
}

fn functions(engine: &str) -> Vec<&'static str> {
    let mut list = vec![
        "count", "sum", "avg", "min", "max", "coalesce", "nullif", "concat", "length", "lower", "upper", "substring", "trim",
        "replace", "round", "floor", "ceil", "abs", "now", "current_date", "cast", "row_number", "rank", "lag", "lead",
    ];
    match engine {
        "sqlite" => list.extend(["strftime", "datetime", "unixepoch", "json_extract", "json_group_array"]),
        "mysql" | "mariadb" => list.extend(["date_format", "ifnull", "group_concat", "json_extract", "date_add", "datediff"]),
        _ => list.extend([
            "string_agg",
            "date_trunc",
            "extract",
            "to_char",
            "date_part",
            "generate_series",
            "array_agg",
            "jsonb_build_object",
            "jsonb_agg",
            "make_interval",
        ]),
    }
    list.sort_unstable();
    list.dedup();
    list
}

const SNIPPETS: &[(&str, &str)] = &[
    ("select-from", "SELECT  FROM "),
    ("join-on", "JOIN  ON "),
    ("insert-values", "INSERT INTO  () VALUES ()"),
    ("update-set", "UPDATE  SET  WHERE "),
    ("create-index", "CREATE INDEX  ON  ()"),
    ("alter-table", "ALTER TABLE  ADD COLUMN "),
];

const BASE_KEYWORDS: &[&str] = &[
    "SELECT", "FROM", "WHERE", "AND", "OR", "NOT", "NULL", "IS", "IN", "EXISTS", "INSERT", "INTO", "VALUES", "UPDATE", "SET",
    "DELETE", "CREATE", "ALTER", "DROP", "TABLE", "VIEW", "INDEX", "JOIN", "LEFT", "RIGHT", "INNER", "OUTER", "FULL", "CROSS",
    "ON", "GROUP", "BY", "ORDER", "LIMIT", "OFFSET", "AS", "DISTINCT", "HAVING", "UNION", "ALL", "CASE", "WHEN", "THEN",
    "ELSE", "END", "WITH", "USING", "RETURNING", "ASC", "DESC", "NULLS", "FIRST", "LAST", "USE",
];

struct Gather<'a> {
    engine: &'a str,
    prefix: &'a str,
    locale: Locale,
    items: Vec<(usize, Candidate)>,
    order: usize,
}

impl<'a> Gather<'a> {
    fn new(engine: &'a str, prefix: &'a str, locale: Locale) -> Self {
        Self { engine, prefix, locale, items: Vec::new(), order: 0 }
    }

    fn push(&mut self, label: String, insert: String, kind: &'static str, detail: String, group: u8, keyword: bool) {
        let cap = match group {
            group::CONDITION => 10,
            group::SNIPPET => 6,
            group::CTE | group::DATABASE | group::SCHEMA => 20,
            _ => 40,
        };
        if self.items.iter().filter(|(_, item)| item.group == group).count() >= cap {
            return;
        }
        if !matched(&label, self.prefix) {
            return;
        }
        self.order += 1;
        self.items.push((self.order, Candidate { label, insert, kind, detail, group, keyword }));
    }

    fn note(&self, key: &str) -> String {
        i18n::text(self.locale, key)
    }

    /// 这一组是否已经有候选；没有就说明这一组的元数据还没读到。
    fn has_group(&self, group: u8) -> bool {
        self.items.iter().any(|(_, item)| item.group == group)
    }

    /// 一个对象只留一条候选：限定名就是身份，不再同时推裸名占限额。
    fn push_object(&mut self, object: &CatalogObject, with_schema: bool) {
        let detail = i18n::text(self.locale, kind_key(&object.kind));
        let schema = if with_schema { object.schema.as_str() } else { "" };
        let label = if schema.is_empty() { object.name.clone() } else { format!("{}.{}", schema, object.name) };
        let insert = qualified_ident(self.engine, schema, &object.name);
        self.push(label, insert, static_kind(&object.kind), detail, group::OBJECT, false);
    }

    fn push_columns(&mut self, sources: &[Source]) {
        for source in sources {
            for column in &source.columns {
                self.push(
                    column.clone(),
                    ident(self.engine, column),
                    "column",
                    source.label.clone(),
                    group::COLUMN,
                    false,
                );
            }
        }
    }

    fn push_conditions(&mut self, conditions: &[(String, &'static str)]) {
        for (text, key) in conditions {
            let detail = self.note(key);
            self.push(text.clone(), text.clone(), "condition", detail, group::CONDITION, false);
        }
    }

    fn push_keywords(&mut self) {
        // 常用词在前：分组上限截断时不能把 SELECT 这样的词挤掉。
        let mut words: Vec<String> = BASE_KEYWORDS.iter().map(|word| (*word).to_owned()).collect();
        for word in dialect_words(self.engine) {
            let upper = (*word).to_uppercase();
            if !words.iter().any(|have| have == &upper) {
                words.push(upper);
            }
        }
        let detail = self.note("query.complete.detail.keyword");
        for word in words {
            if word.eq_ignore_ascii_case(self.prefix) {
                continue;
            }
            self.push(word.clone(), word.clone(), "keyword", detail.clone(), group::KEYWORD, true);
        }
    }

    fn push_functions(&mut self) {
        for name in functions(self.engine) {
            let detail = function_signature(name).map(str::to_owned).unwrap_or_else(|| self.note("query.complete.detail.function"));
            self.push(name.to_owned(), name.to_owned(), "function", detail, group::FUNCTION, false);
        }
    }

    fn push_snippets(&mut self) {
        let prefix = self.prefix.to_ascii_lowercase();
        for (label, body) in SNIPPETS {
            if label.to_ascii_lowercase().starts_with(&prefix) {
                self.push((*label).to_owned(), (*body).to_owned(), "snippet", (*body).to_owned(), group::SNIPPET, false);
            }
        }
    }

    fn push_databases(&mut self, scope: &CompletionScope) {
        let detail = self.note("query.complete.detail.database");
        for name in &scope.databases {
            self.push(name.clone(), ident(self.engine, name), "database", detail.clone(), group::DATABASE, false);
        }
    }

    fn push_schemas(&mut self, scope: &CompletionScope) {
        let detail = self.note("query.complete.detail.schema");
        for name in &scope.schemas {
            self.push(name.clone(), ident(self.engine, name), "schema", detail.clone(), group::SCHEMA, false);
        }
    }

    fn push_ctes(&mut self, scope: &StatementScope) {
        let detail = self.note("query.complete.detail.cte");
        for name in &scope.ctes {
            self.push(name.clone(), ident(self.engine, name), "cte", detail.clone(), group::CTE, false);
        }
    }

    fn push_objects(&mut self, scope: &CompletionScope) {
        for object in &scope.objects {
            self.push_object(object, true);
        }
    }
}

fn weight(frequency: &HashMap<String, u32>, label: &str) -> u32 {
    let lower = label.to_lowercase();
    let direct = frequency.get(&lower).copied().unwrap_or(0);
    match lower.rsplit_once('.') {
        Some((_, tail)) => direct.max(frequency.get(tail).copied().unwrap_or(0)),
        None => direct,
    }
}

fn type_rank(kind: &str) -> u8 {
    match kind {
        "table" | "collection" => 0,
        "view" => 1,
        _ => 2,
    }
}

fn finalize(mut gather: Gather<'_>, frequency: &HashMap<String, u32>) -> Vec<Candidate> {
    let prefix = gather.prefix.to_owned();
    gather.items.sort_by(|left, right| {
        // 顺序：子句分组 → 匹配精度 → 对象类型 → 本地词频 → 出现顺序。
        // 词频只在同一分组、同一精度、同一类型之间才起作用。
        let key = |(order, item): &(usize, Candidate)| {
            (
                item.group,
                match_kind(&item.label, &prefix),
                type_rank(item.kind),
                std::cmp::Reverse(weight(frequency, &item.label)),
                *order,
            )
        };
        key(left).cmp(&key(right))
    });
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (_, item) in gather.items {
        if !seen.insert(format!("{}:{}", item.kind, item.label.to_lowercase())) {
            continue;
        }
        out.push(item);
        if out.len() >= MAX_ITEMS {
            break;
        }
    }
    out
}

fn state_note(state: &MetadataState, locale: Locale) -> Option<String> {
    let key = match state {
        MetadataState::Ready => return None,
        MetadataState::NoTarget => "query.complete.state.noTarget",
        MetadataState::NoProvider => "query.complete.state.noProvider",
        MetadataState::Empty => "query.complete.state.empty",
        MetadataState::Failed(_) => "query.complete.state.failed",
        MetadataState::Partial(_) => "query.complete.state.partial",
        MetadataState::Stale(_) => "query.complete.state.stale",
    };
    let detail = match state {
        MetadataState::Failed(detail) | MetadataState::Partial(detail) | MetadataState::Stale(detail) => detail.as_str(),
        _ => "",
    };
    let template = i18n::text(locale, key);
    Some(i18n::fill(&template, &[("detail", detail)]))
}

fn floor_boundary(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while at > 0 && !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

fn ident_start(text: &str, at: usize) -> usize {
    let mut start = at.min(text.len());
    while start > 0 {
        let ch = text[..start].chars().next_back().unwrap_or(' ');
        if ch.is_alphanumeric() || ch == '_' || ch == '$' {
            start -= ch.len_utf8();
        } else {
            break;
        }
    }
    start
}

/// 当前词在光标之后的结尾；光标停在词中间时整词替换，不留下半截。
fn ident_end(text: &str, at: usize) -> usize {
    let mut end = at.min(text.len());
    while end < text.len() {
        let ch = text[end..].chars().next().unwrap_or(' ');
        if ch.is_alphanumeric() || ch == '_' || ch == '$' {
            end += ch.len_utf8();
        } else {
            break;
        }
    }
    end
}

fn qualifier_before(text: &str, word_start: usize) -> Option<(String, usize)> {
    let head = text.get(..word_start)?;
    let trimmed = head.trim_end();
    let rest = trimmed.strip_suffix('.')?;
    let start = ident_start(rest, rest.len());
    if start == rest.len() {
        return None;
    }
    Some((rest[start..].to_owned(), start))
}

fn inferred_conditions(engine: &str, left: &Source, right: &Source) -> Vec<String> {
    let mut found = Vec::new();
    for (near, far) in [(left, right), (right, left)] {
        for column in &near.columns {
            let lower = column.to_ascii_lowercase();
            let Some(stem) = lower.strip_suffix("_id") else {
                continue;
            };
            let plural = format!("{stem}s");
            if stem.is_empty() || !(far.name.eq_ignore_ascii_case(stem) || far.name.eq_ignore_ascii_case(&plural)) {
                continue;
            }
            if !far.columns.iter().any(|name| name.eq_ignore_ascii_case("id")) {
                continue;
            }
            found.push(format!("{}.{} = {}.id", near.alias, ident(engine, column), far.alias));
        }
    }
    found
}

/// 外键的两侧是否就是这两个来源。模式已知时按模式比对，避免同名表串到别的模式。
fn same_table(schema: &str, name: &str, source: &Source) -> bool {
    if !name.eq_ignore_ascii_case(&source.name) {
        return false;
    }
    schema.is_empty() || source.schema.is_empty() || schema.eq_ignore_ascii_case(&source.schema)
}

fn fk_pair<'a>(fk: &super::assist::ForeignKey, left: &'a Source, right: &'a Source) -> Option<(&'a Source, &'a Source)> {
    if same_table(&fk.from_schema, &fk.from_table, left) && same_table(&fk.to_schema, &fk.to_table, right) {
        return Some((left, right));
    }
    if same_table(&fk.from_schema, &fk.from_table, right) && same_table(&fk.to_schema, &fk.to_table, left) {
        return Some((right, left));
    }
    None
}

/// 这一对来源是否已经有真实外键；有就不再给命名推断。
fn pair_has_foreign_key(scope: &CompletionScope, left: &Source, right: &Source) -> bool {
    scope.foreign_keys.iter().any(|fk| fk_pair(fk, left, right).is_some())
}

fn foreign_key_conditions(scope: &CompletionScope, engine: &str, left: &Source, right: &Source) -> Vec<String> {
    let mut found = Vec::new();
    for fk in &scope.foreign_keys {
        let Some((from, to)) = fk_pair(fk, left, right) else {
            continue;
        };
        if fk.from_columns.len() != fk.to_columns.len() {
            continue;
        }
        for (left_column, right_column) in fk.from_columns.iter().zip(fk.to_columns.iter()) {
            found.push(format!(
                "{}.{} = {}.{}",
                from.alias,
                ident(engine, left_column),
                to.alias,
                ident(engine, right_column)
            ));
        }
    }
    found
}

fn labels_of(sources: &[Source]) -> String {
    let names: Vec<String> = sources.iter().map(|source| source.label.clone()).collect();
    if names.is_empty() {
        String::new()
    } else {
        names.join(", ")
    }
}

/// 来源里确实读到过列。前缀没命中列不能说明元数据缺失，只有列集合真的为空才算。
fn any_columns_loaded(sources: &[Source]) -> bool {
    sources.iter().any(|source| !source.columns.is_empty())
}

/// 列拿不到时的回退：仍然给对象、CTE、函数和关键字，不留成功空列表。
fn fallback_objects(gather: &mut Gather<'_>, scope: &CompletionScope, statement: &StatementScope, metadata_ok: bool) {
    if metadata_ok {
        gather.push_objects(scope);
        gather.push_ctes(statement);
        gather.push_functions();
    }
    gather.push_keywords();
}

/// 计算一次补全计划。text 是编辑器全文，cursor 是光标字节偏移。
pub fn plan(scope: &CompletionScope, text: &str, cursor: usize, locale: Locale) -> Plan {
    let end = floor_boundary(text, cursor);
    let mut plan = Plan { span: Span { start: end, end }, ..Default::default() };
    if !scope.provider || !known_engine(&scope.engine) {
        plan.note = Some(i18n::text(locale, "query.complete.state.noProvider"));
        return plan;
    }
    let window_start = floor_boundary(text, end.saturating_sub(WINDOW));
    let (_, head) = lex(&text[window_start..end]);
    if head.open {
        // 字符串、引用标识符或注释里没有可补全的标识符。
        return plan;
    }
    let statement_start = head.last_mark.map(|at| window_start + at + 1).unwrap_or(window_start);
    let local = &text[statement_start..end];
    let word_start = ident_start(local, local.len());
    // 光标可能停在词中间：替换范围和匹配前缀都按整个词算。
    let word_end = ident_end(&text[statement_start..], end - statement_start);
    let prefix = text[statement_start + word_start..statement_start + word_end].to_owned();
    let qualifier = qualifier_before(local, word_start);
    plan.span = Span { start: statement_start + word_start, end: statement_start + word_end };
    if prefix.is_empty() {
        // IME 组合或中文正文：不打断输入，不弹候选。
        if let Some(prev) = local[..word_start].chars().next_back() {
            if !prev.is_ascii() && prev.is_alphanumeric() {
                return plan;
            }
        }
    }

    let statement = parse_scope(local);
    plan.clause = statement.clause;
    let clause = statement.clause;
    let sources = resolve_sources(scope, &statement.tables);
    let metadata_ok = !matches!(
        scope.state,
        MetadataState::NoTarget | MetadataState::NoProvider | MetadataState::Failed(_)
    );
    let frequency: HashMap<String, u32> = scope.frequency.iter().cloned().collect();
    let mut gather = Gather::new(&scope.engine, &prefix, locale);

    // 限定符：别名/表名只出该来源的列；模式只出该模式的对象；都不是就不回退无关列。
    if let Some((name, _)) = qualifier {
        if let Some(source) = sources
            .iter()
            .find(|source| source.alias.eq_ignore_ascii_case(&name) && !source.columns.is_empty())
        {
            gather.push_columns(std::slice::from_ref(source));
            plan.candidates = finalize(gather, &frequency);
            return plan;
        }
        if metadata_ok && matches!(clause, Some(Clause::From) | Some(Clause::Join) | None) {
            let schema_objects: Vec<&CatalogObject> =
                scope.objects.iter().filter(|object| object.schema.eq_ignore_ascii_case(&name)).collect();
            if !schema_objects.is_empty() {
                for object in schema_objects {
                    gather.push_object(object, false);
                }
                plan.candidates = finalize(gather, &frequency);
                return plan;
            }
        }
        if let Some(source) = sources
            .iter()
            .find(|source| source.alias.eq_ignore_ascii_case(&name) || source.name.eq_ignore_ascii_case(&name))
        {
            // 别名认出来了但列还没读到：说明原因，不回退到无关列。
            plan.note = Some(i18n::fill(
                &i18n::text(locale, "query.complete.state.noColumns"),
                &[("source", &source.label)],
            ));
            return plan;
        }
        plan.note = Some(i18n::fill(&i18n::text(locale, "query.complete.state.unknownQualifier"), &[("name", &name)]));
        return plan;
    }

    let target = statement.target.clone();
    // 需要列的来源确实没有任何列时的说明范围；只由来源本身决定，不看被前缀过滤后的候选。
    // 回退候选（对象/关键字）不受它限制：前缀没命中列时也要有正常候选。
    let mut column_gap: Option<String> = None;
    match clause {
        Some(Clause::Use) => gather.push_databases(scope),
        Some(Clause::On) => {
            let mut scoped: Vec<Source> = Vec::new();
            if metadata_ok {
                let on_tables: HashSet<String> = statement.on_tables.iter().map(|alias| alias.to_lowercase()).collect();
                scoped = if on_tables.is_empty() {
                    sources.clone()
                } else {
                    sources
                        .iter()
                        .filter(|source| {
                            on_tables.contains(&source.alias.to_lowercase()) || on_tables.contains(&source.name.to_lowercase())
                        })
                        .cloned()
                        .collect()
                };
                let mut conditions: Vec<(String, &'static str)> = Vec::new();
                for (index, left) in scoped.iter().enumerate() {
                    for right in scoped.iter().skip(index + 1) {
                        for text in foreign_key_conditions(scope, &scope.engine, left, right) {
                            conditions.push((text, "query.complete.detail.joinCondition"));
                        }
                        // 只有这一对来源确实没有外键时才做命名推断，无关外键不再关掉全部推断。
                        if !pair_has_foreign_key(scope, left, right) {
                            for text in inferred_conditions(&scope.engine, left, right) {
                                conditions.push((text, "query.complete.detail.inferredCondition"));
                            }
                        }
                    }
                }
                gather.push_conditions(&conditions);
                gather.push_columns(&scoped);
            }
            if !gather.has_group(group::COLUMN) {
                if metadata_ok && !any_columns_loaded(&scoped) {
                    let labels = labels_of(&scoped);
                    if !labels.is_empty() {
                        column_gap = Some(labels);
                    }
                }
                fallback_objects(&mut gather, scope, &statement, metadata_ok);
            }
        }
        Some(Clause::Set) | Some(Clause::Values) => {
            let mut picked: Vec<Source> = Vec::new();
            if metadata_ok {
                picked = match target {
                    Some(table) => resolve_sources(scope, std::slice::from_ref(&table)),
                    None => sources.clone(),
                };
                if picked.is_empty() {
                    picked = sources.clone();
                }
                gather.push_columns(&picked);
            }
            if !gather.has_group(group::COLUMN) {
                if metadata_ok && !any_columns_loaded(&picked) {
                    let labels = labels_of(&picked);
                    if !labels.is_empty() {
                        column_gap = Some(labels);
                    }
                }
                fallback_objects(&mut gather, scope, &statement, metadata_ok);
            }
        }
        Some(Clause::From) | Some(Clause::Join) => {
            if metadata_ok {
                gather.push_objects(scope);
            }
            gather.push_ctes(&statement);
            gather.push_keywords();
        }
        Some(clause) if clause.field_first() => {
            if metadata_ok {
                gather.push_columns(&sources);
                gather.push_objects(scope);
                gather.push_databases(scope);
                gather.push_schemas(scope);
            }
            gather.push_ctes(&statement);
            gather.push_functions();
            gather.push_keywords();
            gather.push_snippets();
            if metadata_ok && !sources.is_empty() && !any_columns_loaded(&sources) {
                let labels = labels_of(&sources);
                if !labels.is_empty() {
                    column_gap = Some(labels);
                }
            }
        }
        // 其余子句（SELECT/WHERE/GROUP/ORDER/HAVING）已在上面按 field_first 处理。
        Some(_) => {}
        None => {
            gather.push_databases(scope);
            gather.push_schemas(scope);
            if metadata_ok {
                gather.push_objects(scope);
                gather.push_columns(&sources);
            }
            gather.push_ctes(&statement);
            gather.push_functions();
            gather.push_keywords();
            gather.push_snippets();
        }
    }

    plan.candidates = finalize(gather, &frequency);
    let needs_columns = clause.is_some_and(Clause::needs_columns);
    // 说明只在来源列集合确实为空时出现；前缀没命中列是正常无候选，不能报成元数据缺失。
    let column_note = if needs_columns {
        column_gap.map(|labels| {
            i18n::fill(&i18n::text(locale, "query.complete.state.noColumns"), &[("source", &labels)])
        })
    } else {
        None
    };
    plan.note = state_note(&scope.state, locale).or(column_note);
    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::assist::ForeignKey;

    fn object(name: &str, schema: &str, kind: &str) -> CatalogObject {
        CatalogObject { name: name.to_owned(), schema: schema.to_owned(), kind: kind.to_owned() }
    }

    fn source(label: &str, columns: &[&str]) -> CompletionSource {
        CompletionSource {
            alias: String::new(),
            label: label.to_owned(),
            columns: columns.iter().map(|name| (*name).to_owned()).collect(),
        }
    }

    fn scope(engine: &str) -> CompletionScope {
        CompletionScope {
            engine: engine.to_owned(),
            objects: vec![object("orders", "public", "table"), object("customers", "public", "table")],
            sources: vec![
                source("public.orders", &["id", "customer_id", "status"]),
                source("public.customers", &["id", "name"]),
            ],
            provider: true,
            databases: vec!["atlas_dev".to_owned(), "atlas_test".to_owned()],
            schemas: vec!["public".to_owned(), "analytics".to_owned()],
            frequency: Vec::new(),
            state: MetadataState::Ready,
            foreign_keys: Vec::new(),
        }
    }

    fn labels(planned: &Plan) -> Vec<String> {
        planned.candidates.iter().map(|item| item.label.clone()).collect()
    }

    fn at(sql: &str) -> Plan {
        plan(&scope("postgres"), sql, sql.len(), Locale::ZhCn)
    }

    #[test]
    fn clause_detection_skips_strings_and_comments() {
        assert_eq!(parse_scope("SELECT id, name FROM customers ").clause, Some(Clause::From));
        assert_eq!(parse_scope("SELECT id FROM customers WHERE name = 'from dual' ").clause, Some(Clause::Where));
        assert_eq!(parse_scope("SELECT 'select from dual' ").clause, Some(Clause::Select));
        assert_eq!(parse_scope("SELECT id FROM t /* from x */ WHERE ").clause, Some(Clause::Where));
        assert_eq!(parse_scope("SELECT id FROM t -- from x\nGROUP ").clause, Some(Clause::Group));
        assert_eq!(parse_scope("SELECT 1 FROM \"from\" \"where\" ORDER ").clause, Some(Clause::Order));
        assert_eq!(parse_scope("UPDATE orders SET ").clause, Some(Clause::Set));
        assert_eq!(parse_scope("INSERT INTO orders VALUES ").clause, Some(Clause::Values));
        assert_eq!(parse_scope("USE ").clause, Some(Clause::Use));
    }

    #[test]
    fn aliases_ctes_and_targets_come_from_the_current_statement() {
        let parsed = parse_scope("SELECT o.customer_id FROM public.orders AS o JOIN customers c ON c.id = o.customer_id WHERE ");
        assert_eq!(parsed.clause, Some(Clause::Where));
        assert_eq!(parsed.tables[0].alias, "o");
        assert_eq!(parsed.tables[0].name, "orders");
        assert_eq!(parsed.tables[1].alias, "c");
        assert_eq!(parsed.tables[1].name, "customers");
        assert_eq!(parsed.on_tables, vec!["o".to_owned(), "c".to_owned()]);

        let cte = parse_scope("WITH recent AS (SELECT 1) SELECT ");
        assert_eq!(cte.ctes, vec!["recent".to_owned()]);
        assert_eq!(cte.clause, Some(Clause::Select));

        let target = parse_scope("UPDATE public.orders o SET ");
        assert_eq!(target.target.as_ref().map(|table| table.name.as_str()), Some("orders"));
        let insert = parse_scope("INSERT INTO customers VALUES ");
        assert_eq!(insert.target.as_ref().map(|table| table.name.as_str()), Some("customers"));
    }

    #[test]
    fn from_offers_objects_not_columns() {
        let planned = at("SELECT * FROM ");
        assert!(planned.candidates.iter().any(|item| item.kind == "table"));
        assert!(planned.candidates.iter().any(|item| item.kind == "keyword"));
        assert!(!planned.candidates.iter().any(|item| item.kind == "column"));
    }

    #[test]
    fn select_puts_source_columns_before_objects_and_keywords() {
        let planned = at("SELECT id FROM orders o WHERE ");
        let columns = planned.candidates.iter().position(|item| item.kind == "column").expect("column");
        let tables = planned.candidates.iter().position(|item| item.kind == "table").expect("table");
        let keywords = planned.candidates.iter().position(|item| item.kind == "keyword").expect("keyword");
        assert!(columns < tables, "{:?}", labels(&planned));
        assert!(columns < keywords);
    }

    #[test]
    fn alias_qualifier_only_offers_that_table() {
        let planned = at("SELECT * FROM public.orders u WHERE u.");
        let names = labels(&planned);
        assert!(names.contains(&"customer_id".to_owned()), "{names:?}");
        assert!(!names.contains(&"name".to_owned()), "{names:?}");
        assert!(!names.contains(&"SELECT".to_owned()), "{names:?}");
    }

    #[test]
    fn unknown_qualifier_does_not_fall_back_to_unrelated_columns() {
        let planned = at("SELECT * FROM public.orders u WHERE z.");
        assert!(planned.candidates.is_empty(), "{:?}", labels(&planned));
        assert!(planned.note.as_deref().is_some_and(|note| note.contains('z')));
    }

    #[test]
    fn on_offers_both_sides_and_real_foreign_keys_before_naming_inference() {
        let mut exact = scope("postgres");
        exact.foreign_keys = vec![ForeignKey {
            connection_id: "pg".to_owned(),
            database: "shop".to_owned(),
            from_schema: "public".to_owned(),
            from_table: "orders".to_owned(),
            from_columns: vec!["customer_id".to_owned()],
            to_schema: "public".to_owned(),
            to_table: "customers".to_owned(),
            to_columns: vec!["id".to_owned()],
        }];
        let sql = "SELECT * FROM orders o JOIN customers c ON ";
        let planned = plan(&exact, sql, sql.len(), Locale::ZhCn);
        let first = planned.candidates.first().expect("condition");
        assert_eq!(first.kind, "condition");
        assert_eq!(first.label, "o.customer_id = c.id");
        assert!(first.detail.contains("外键"), "{}", first.detail);
        assert!(!planned.candidates.iter().any(|item| item.detail.contains("推断")), "真实外键存在时不再给命名推断");
        assert!(planned.candidates.iter().any(|item| item.label == "status"));

        let inferred = plan(&scope("postgres"), sql, sql.len(), Locale::ZhCn);
        let condition = inferred.candidates.iter().find(|item| item.kind == "condition").expect("inferred");
        assert_eq!(condition.label, "o.customer_id = c.id");
        assert!(condition.detail.contains("推断"), "{}", condition.detail);
    }

    #[test]
    fn on_never_offers_columns_when_the_join_is_unknown() {
        let mut empty = scope("postgres");
        empty.sources.clear();
        let sql = "SELECT * FROM other x JOIN unknown y ON ";
        let planned = plan(&empty, sql, sql.len(), Locale::ZhCn);
        assert!(!planned.candidates.iter().any(|item| item.kind == "column"), "{:?}", labels(&planned));
    }

    #[test]
    fn set_and_values_only_offer_target_columns() {
        let set = at("UPDATE orders SET ");
        let names = labels(&set);
        assert!(names.contains(&"status".to_owned()), "{names:?}");
        assert!(!names.contains(&"name".to_owned()), "{names:?}");
        assert!(set.candidates.iter().all(|item| item.kind == "column"));

        let values = at("INSERT INTO customers VALUES (");
        assert_eq!(labels(&values), vec!["id".to_owned(), "name".to_owned()]);
        assert!(values.candidates.iter().all(|item| item.kind == "column"));
    }

    #[test]
    fn use_only_offers_databases() {
        let planned = at("USE ");
        assert!(!planned.candidates.is_empty());
        assert!(planned.candidates.iter().all(|item| item.kind == "database"), "{:?}", labels(&planned));
        assert!(planned.candidates.iter().any(|item| item.label == "atlas_test"));
    }

    #[test]
    fn prefix_and_subsequence_matching_work() {
        let planned = at("SELECT * FROM ord");
        assert!(labels(&planned).contains(&"public.orders".to_owned()), "{:?}", labels(&planned));
        let fuzzy = at("SELECT * FROM ors");
        assert!(labels(&fuzzy).contains(&"public.orders".to_owned()), "{:?}", labels(&fuzzy));
    }

    #[test]
    fn ranking_keeps_keywords_ahead_of_objects_and_uses_frequency_on_ties() {
        let planned = at("SEL");
        assert_eq!(planned.candidates.first().map(|item| item.label.as_str()), Some("SELECT"));
    }

    #[test]
    fn local_frequency_breaks_matching_ties() {
        let mut frequent = scope("postgres");
        frequent.sources = vec![source("public.metrics", &["state", "status"])];
        let sql = "SELECT st FROM metrics m WHERE ";
        let plain = plan(&frequent, sql, sql.len(), Locale::ZhCn);
        let names = labels(&plain);
        let status = names.iter().position(|name| name == "status").expect("status");
        let state = names.iter().position(|name| name == "state").expect("state");
        assert!(state < status, "同分时保持来源顺序：{names:?}");

        frequent.frequency = vec![("status".to_owned(), 7)];
        let weighted = plan(&frequent, sql, sql.len(), Locale::ZhCn);
        let names = labels(&weighted);
        let status = names.iter().position(|name| name == "status").expect("status");
        let state = names.iter().position(|name| name == "state").expect("state");
        assert!(status < state, "本地词频只在同分时提权：{names:?}");
    }

    #[test]
    fn candidates_are_deduped_and_quoted_per_engine() {
        let mut odd = scope("mysql");
        odd.objects = vec![object("order items", "public", "table")];
        odd.sources = vec![source("public.order items", &["order id"])];
        let sql = "SELECT * FROM ";
        let planned = plan(&odd, sql, sql.len(), Locale::ZhCn);
        let item = planned.candidates.iter().find(|item| item.label == "public.order items").expect("object");
        assert_eq!(item.insert, "public.\u{60}order items\u{60}");
        let names = labels(&planned);
        let unique: HashSet<&String> = names.iter().collect();
        assert_eq!(unique.len(), names.len(), "{names:?}");

        let pg = plan(&scope("postgres"), sql, sql.len(), Locale::ZhCn);
        let plain = pg.candidates.iter().find(|item| item.label == "public.orders").expect("object");
        assert_eq!(plain.insert, "public.orders");
        assert_eq!(ident("postgres", "order items"), "\"order items\"");
        assert_eq!(ident("sqlserver", "order items"), "[order items]");
    }

    #[test]
    fn cursor_inside_string_or_comment_offers_nothing() {
        assert!(at("SELECT 'ab").candidates.is_empty());
        assert!(at("SELECT 1 -- note").candidates.is_empty());
        assert!(at("SELECT 1 /* note").candidates.is_empty());
    }

    #[test]
    fn ime_text_does_not_open_candidates() {
        assert!(at("SELECT 中文").candidates.is_empty());
    }

    #[test]
    fn metadata_failures_are_readable_and_still_leave_keywords() {
        let mut failed = scope("postgres");
        failed.state = MetadataState::Failed("读取库表失败：连接中断".to_owned());
        let sql = "SELECT * FROM ";
        let planned = plan(&failed, sql, sql.len(), Locale::ZhCn);
        assert!(planned.note.as_deref().is_some_and(|note| note.contains("连接中断")), "{:?}", planned.note);
        assert!(planned.candidates.iter().any(|item| item.kind == "keyword"));
        assert!(!planned.candidates.iter().any(|item| item.kind == "table"));

        let mut partial = scope("postgres");
        partial.state = MetadataState::Partial("analytics (权限不足)".to_owned());
        let kept = plan(&partial, sql, sql.len(), Locale::ZhCn);
        assert!(kept.candidates.iter().any(|item| item.kind == "table"));
        assert!(kept.note.as_deref().is_some_and(|note| note.contains("权限不足")));
    }

    #[test]
    fn missing_columns_are_explained_instead_of_an_empty_list() {
        let mut unknown = scope("postgres");
        unknown.sources = vec![source("public.orders", &[])];
        let sql = "SELECT * FROM orders o WHERE ";
        let planned = plan(&unknown, sql, sql.len(), Locale::ZhCn);
        assert!(!planned.candidates.is_empty());
        assert!(planned.note.as_deref().is_some_and(|note| note.contains("orders")), "{:?}", planned.note);
    }

    #[test]
    fn prefixes_that_miss_every_column_do_not_claim_missing_columns() {
        let loaded = scope("postgres");
        // WHERE an：列已读到但没命中，说明必须为空，关键字 AND 照常给出。
        let where_an = "SELECT id FROM customers c WHERE an";
        let planned = plan(&loaded, where_an, where_an.len(), Locale::ZhCn);
        assert!(planned.note.is_none(), "{:?}", planned.note);
        assert!(labels(&planned).contains(&"AND".to_owned()), "{:?}", labels(&planned));
        assert!(!planned.candidates.iter().any(|item| item.kind == "column"), "{:?}", labels(&planned));

        // WHERE AND：关键字已经打全，只是没有可补的内容，同样不是元数据失败。
        let where_and = "SELECT id FROM customers c WHERE AND";
        let planned = plan(&loaded, where_and, where_and.len(), Locale::ZhCn);
        assert!(planned.note.is_none(), "{:?}", planned.note);

        // VALUES lu：目标列已读到，前缀没命中也不回退成假失败说明。
        let values = "INSERT INTO customers VALUES lu";
        let planned = plan(&loaded, values, values.len(), Locale::ZhCn);
        assert!(planned.note.is_none(), "{:?}", planned.note);
        assert!(!planned.candidates.iter().any(|item| item.kind == "column"), "{:?}", labels(&planned));

        // ON lu：两侧列都读到了，没命中也不报列缺失。
        let on = "SELECT * FROM orders o JOIN customers c ON lu";
        let planned = plan(&loaded, on, on.len(), Locale::ZhCn);
        assert!(planned.note.is_none(), "{:?}", planned.note);

        // 同一来源真正有列时，命中的前缀照旧给出列。
        let hit = "SELECT id FROM customers c WHERE na";
        let planned = plan(&loaded, hit, hit.len(), Locale::ZhCn);
        assert!(planned.note.is_none(), "{:?}", planned.note);
        assert!(labels(&planned).contains(&"name".to_owned()), "{:?}", labels(&planned));
    }

    #[test]
    fn schema_qualifier_narrows_objects_to_that_schema() {
        let mut multi = scope("postgres");
        multi.objects.push(object("daily_metrics", "analytics", "view"));
        let sql = "SELECT * FROM analytics.";
        let planned = plan(&multi, sql, sql.len(), Locale::ZhCn);
        assert_eq!(labels(&planned), vec!["daily_metrics".to_owned()]);
    }

    #[test]
    fn ranking_keeps_group_and_type_before_local_frequency() {
        // 执行过 SELECT 之后词频很高，也不能把关键字排到列前面。
        let mut weighted = scope("postgres");
        weighted.frequency = vec![("select".to_owned(), 99), ("public.orders".to_owned(), 50)];
        let sql = "SELECT * FROM orders o WHERE st";
        let planned = plan(&weighted, sql, sql.len(), Locale::ZhCn);
        let names = labels(&planned);
        let column = names.iter().position(|name| name == "status").expect("status");
        let keyword = names.iter().position(|name| name == "SELECT").expect("SELECT");
        assert!(column < keyword, "分组先于词频：{names:?}");

        // 同分组里表类型先于视图，词频只在同类型之间提权。
        let mut mixed = scope("postgres");
        mixed.objects = vec![object("orders", "public", "view"), object("order_items", "public", "table")];
        mixed.frequency = vec![("public.orders".to_owned(), 9)];
        let sql = "SELECT * FROM order";
        let planned = plan(&mixed, sql, sql.len(), Locale::ZhCn);
        let names = labels(&planned);
        let table = names.iter().position(|name| name == "public.order_items").expect("table");
        let view = names.iter().position(|name| name == "public.orders").expect("view");
        assert!(table < view, "同分组内表先于视图：{names:?}");
    }

    #[test]
    fn one_object_is_listed_once_even_with_a_schema() {
        let planned = at("SELECT * FROM ");
        let names = labels(&planned);
        assert!(names.contains(&"public.orders".to_owned()), "{names:?}");
        assert!(!names.contains(&"orders".to_owned()), "同一对象不再推裸名：{names:?}");
    }

    #[test]
    fn foreign_keys_with_another_schema_or_pair_do_not_take_over() {
        let mut other_schema = scope("postgres");
        other_schema.foreign_keys = vec![ForeignKey {
            connection_id: "pg".to_owned(),
            database: "shop".to_owned(),
            from_schema: "analytics".to_owned(),
            from_table: "orders".to_owned(),
            from_columns: vec!["customer_id".to_owned()],
            to_schema: "analytics".to_owned(),
            to_table: "customers".to_owned(),
            to_columns: vec!["id".to_owned()],
        }];
        let sql = "SELECT * FROM orders o JOIN customers c ON ";
        let planned = plan(&other_schema, sql, sql.len(), Locale::ZhCn);
        assert!(planned.candidates.iter().all(|item| item.detail != "外键"), "别的模式的外键不能串过来");
        let inferred = planned.candidates.iter().find(|item| item.kind == "condition").expect("推断条件");
        assert_eq!(inferred.label, "o.customer_id = c.id");
        assert!(inferred.detail.contains("推断"), "{}", inferred.detail);

        // 已经有一对外键时，别的表对仍然保留命名推断。
        let mut mixed = scope("postgres");
        mixed.sources.push(source("public.order_items", &["id", "order_id"]));
        mixed.foreign_keys = vec![ForeignKey {
            connection_id: "pg".to_owned(),
            database: "shop".to_owned(),
            from_schema: "public".to_owned(),
            from_table: "orders".to_owned(),
            from_columns: vec!["customer_id".to_owned()],
            to_schema: "public".to_owned(),
            to_table: "customers".to_owned(),
            to_columns: vec!["id".to_owned()],
        }];
        let sql = "SELECT * FROM orders o JOIN order_items i ON ";
        let planned = plan(&mixed, sql, sql.len(), Locale::ZhCn);
        let inferred = planned.candidates.iter().find(|item| item.kind == "condition").expect("这一对没有外键");
        assert_eq!(inferred.label, "i.order_id = o.id");
        assert!(inferred.detail.contains("推断"), "{}", inferred.detail);
    }

    #[test]
    fn set_and_on_without_columns_fall_back_to_keywords_and_objects() {
        let mut unknown = scope("postgres");
        unknown.sources.clear();
        let set_sql = "UPDATE missing SET ";
        let set = plan(&unknown, set_sql, set_sql.len(), Locale::ZhCn);
        assert!(!set.candidates.is_empty(), "缺列时不能给成功空列表");
        assert!(set.candidates.iter().any(|item| item.kind == "keyword"), "{:?}", labels(&set));
        assert!(set.candidates.iter().any(|item| item.kind == "table"), "{:?}", labels(&set));
        assert!(set.note.as_deref().is_some_and(|note| note.contains("missing")), "{:?}", set.note);

        let values_sql = "INSERT INTO missing VALUES (";
        let values = plan(&unknown, values_sql, values_sql.len(), Locale::ZhCn);
        assert!(values.candidates.iter().any(|item| item.kind == "keyword"), "{:?}", labels(&values));
        assert!(values.note.is_some());

        let on_sql = "SELECT * FROM missing m JOIN other o ON ";
        let on = plan(&unknown, on_sql, on_sql.len(), Locale::ZhCn);
        assert!(on.candidates.iter().any(|item| item.kind == "keyword"), "{:?}", labels(&on));
        assert!(on.note.is_some());
    }

    #[test]
    fn cursor_inside_a_word_replaces_the_whole_word() {
        let sql = "SELECT * FROM orders";
        let planned = plan(&scope("postgres"), sql, 17, Locale::ZhCn);
        assert_eq!(&sql[planned.span.start..planned.span.end], "orders");
        let qualified = "SELECT * FROM orders o WHERE o.cust_id";
        let planned = plan(&scope("postgres"), qualified, 33, Locale::ZhCn);
        assert_eq!(&qualified[planned.span.start..planned.span.end], "cust_id");
    }

    #[test]
    fn postgres_camel_case_names_are_quoted_and_other_engines_are_not() {
        assert_eq!(ident("postgres", "OrderItems"), "\"OrderItems\"");
        assert_eq!(ident("postgres", "orders"), "orders");
        assert_eq!(ident("mysql", "OrderItems"), "OrderItems");
        let mut camel = scope("postgres");
        camel.objects = vec![object("OrderItems", "public", "table")];
        let planned = plan(&camel, "SELECT * FROM order", 18, Locale::ZhCn);
        let item = planned.candidates.iter().find(|item| item.label == "public.OrderItems").expect("camel object");
        assert_eq!(item.insert, "public.\"OrderItems\"");
    }

    #[test]
    fn span_only_replaces_the_current_word() {
        let sql = "SELECT * FROM ord";
        let planned = at(sql);
        assert_eq!(planned.span.end, sql.len());
        assert_eq!(planned.span.start, sql.len() - 3);

        let qualified = "SELECT * FROM orders o WHERE o.cu";
        let planned = at(qualified);
        assert_eq!(&qualified[planned.span.start..planned.span.end], "cu");
    }
}
