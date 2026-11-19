//! 结构变更用到的命名与校验门面：限定名、类型与默认值校验经适配器的 DDL 方言与类型规则；
//! 名字合法性与字面量识别是引擎无关的。

use crate::adapters::ddl::{ddl_of, no_structure_editing, types_of};
use crate::adapters::{registry, adapter};

use super::model::Engine;

/// 按方言最小引号，统一由适配器 SqlSyntax 判定。
pub use crate::adapters::quote_ident;

/// 结构变更目标的限定名。没有结构编辑的引擎、缺库/模式或不是当前库时给出原因。
pub fn qualified_table(engine: Engine, database: &str, schema: &str, table: &str) -> Result<String, String> {
    match ddl_of(engine) {
        Some(dialect) => dialect.qualified_table(database, schema, table),
        None => Err(no_structure_editing(engine)),
    }
}

/// 默认值只接受字面量和少量当前时间函数，避免把用户输入拼成任意语句。各方言额外接受的写法见类型规则。
pub fn valid_default(engine: Engine, raw: &str) -> bool {
    let text = raw.trim();
    if text.is_empty() || text.eq_ignore_ascii_case("null") {
        return text.eq_ignore_ascii_case("null");
    }
    if is_number(text) || is_quoted(text) {
        return true;
    }
    let upper = text.to_ascii_uppercase();
    if matches!(upper.as_str(), "CURRENT_TIMESTAMP" | "CURRENT_DATE" | "CURRENT_TIME" | "(CURRENT_TIMESTAMP)") {
        return true;
    }
    types_of(engine).is_some_and(|types| types.extra_default(text, &upper))
}

pub fn non_null_default(raw: &str) -> bool {
    let text = raw.trim();
    !text.is_empty() && !text.eq_ignore_ascii_case("null")
}

pub fn valid_type(engine: Engine, raw: &str) -> bool {
    let text = raw.trim();
    if text.is_empty() || text.len() > 80 {
        return false;
    }
    let lower = text.to_ascii_lowercase();
    if !lower.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, ' ' | '_' | '(' | ')' | ',' | '[' | ']')) {
        return false;
    }
    let base = lower.split('(').next().unwrap_or("").trim();
    // 没有结构编辑的引擎没有列类型。
    types_of(engine).is_some_and(|types| types.valid_type_base(base))
}

pub fn valid_name(name: &str) -> bool {
    let text = name.trim();
    !text.is_empty() && text.len() <= 128 && !text.chars().any(|ch| ch.is_control() || ch == '\u{0}')
}

pub fn mentions(sql: &str, name: &str) -> bool {
    let needle = name.trim();
    if needle.is_empty() {
        return false;
    }
    // 各方言的引号写法都算提到（双引号、反引号、方括号）；方括号另认不转义 `]` 的写法。
    let mut quoted: Vec<String> = registry::ENGINES.into_iter().filter_map(|engine| adapter(engine).syntax()).map(|syntax| syntax.quote_always(needle)).collect();
    quoted.push(format!("[{needle}]"));
    if quoted.iter().any(|item| sql.contains(item)) {
        return true;
    }
    sql.split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
        .any(|word| word.eq_ignore_ascii_case(needle))
}

fn is_number(value: &str) -> bool {
    let mut chars = value.chars().peekable();
    if chars.peek() == Some(&'+') || chars.peek() == Some(&'-') {
        chars.next();
    }
    let mut digits = 0;
    let mut dot = false;
    for ch in chars {
        if ch == '.' && !dot {
            dot = true;
            continue;
        }
        if ch.is_ascii_digit() {
            digits += 1;
            continue;
        }
        return false;
    }
    digits > 0
}

/// 单引号字符串字面量（内部单引号已加倍）。
pub(crate) fn is_quoted(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() < 2 || bytes[0] != b'\'' || bytes[bytes.len() - 1] != b'\'' {
        return false;
    }
    let inner = &value[1..value.len() - 1];
    let mut chars = inner.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\''
            && chars.next() != Some('\'') {
                return false;
            }
    }
    true
}
