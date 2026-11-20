//! 结构变更用到的命名与校验门面：限定名、类型与默认值校验经适配器的 DDL 方言与类型规则；
//! 名字合法性与字面量识别是引擎无关的。

use crate::adapters::ddl::{ddl_of, no_structure_editing, types_of};
use crate::adapters::{registry, adapter};

use super::model::{ColumnDraft, DefaultSpec, Engine};

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

/// 读入列的默认值原文 → 可以直接拼进 `DEFAULT ...` 的 SQL。数据库报回的若是不带引号的文本字面量
/// （information_schema.COLUMN_DEFAULT 的写法），按方言与列类型补上引号；已带引号、括号、数字、NULL 与表达式原样保留。
pub fn existing_default_sql(engine: Engine, type_name: &str, raw: &str) -> String {
    let text = raw.trim();
    if text.is_empty() || text.starts_with('\'') || text.starts_with('(') || is_number(text) {
        return text.to_owned();
    }
    match (existing_default(engine, Some(text)), types_of(engine)) {
        (DefaultSpec::Literal(value), Some(types)) => types.literal_default(type_name, &value),
        _ => text.to_owned(),
    }
}

/// 读入列的默认值原文（数据库报回的写法）→ 四态。缺失为无默认值；解析由方言的类型规则完成。
pub fn existing_default(engine: Engine, raw: Option<&str>) -> DefaultSpec {
    let Some(raw) = raw else {
        return DefaultSpec::None;
    };
    match types_of(engine) {
        Some(types) => types.parse_default(raw),
        None => crate::adapters::ddl::generic_parse_default(raw),
    }
}

/// 默认值要写进语句的 SQL：无默认为 None；NULL 为 `NULL`；字面量按方言与列类型加引号；表达式原样。
/// 不合规则的值给出原因。
pub fn default_sql(engine: Engine, draft: &ColumnDraft) -> Result<Option<String>, String> {
    if let Some(problem) = default_problem(engine, draft) {
        return Err(problem);
    }
    Ok(match &draft.default {
        DefaultSpec::None => None,
        DefaultSpec::Null => Some("NULL".to_owned()),
        DefaultSpec::Literal(value) => Some(match types_of(engine) {
            Some(types) => types.literal_default(&draft.type_name, value),
            None => crate::adapters::ddl::quote_text(value),
        }),
        DefaultSpec::Expression(text) => Some(text.trim().to_owned()),
    })
}

/// 默认值不能生成的原因：非空列默认 NULL、数值列的字面量不是数字、表达式为空或不在白名单里。
pub fn default_problem(engine: Engine, draft: &ColumnDraft) -> Option<String> {
    let name = draft.name.trim();
    match &draft.default {
        DefaultSpec::None => None,
        DefaultSpec::Null if !draft.nullable => Some(format!("字段 {name} 不允许 NULL，默认值不能是 NULL。")),
        DefaultSpec::Null => None,
        DefaultSpec::Literal(value) => {
            let base = crate::adapters::ddl::type_base(&draft.type_name);
            let numeric = types_of(engine).is_some_and(|types| types.numeric_type(&base));
            (numeric && !is_number(value.trim())).then(|| format!("字段 {name} 是数值类型，默认值 {} 不是数字。", value.trim()))
        }
        DefaultSpec::Expression(text) if text.trim().is_empty() => Some(format!("字段 {name} 的默认值表达式为空。")),
        DefaultSpec::Expression(text) => {
            if !valid_default(engine, text) {
                return Some(format!("字段 {name} 的默认值不是允许的字面量或当前时间函数。"));
            }
            let upper = text.trim().to_ascii_uppercase();
            types_of(engine)
                .and_then(|types| types.expression_default_problem(&draft.type_name, &upper))
                .map(|reason| format!("字段 {name}：{reason}"))
        }
    }
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

pub(crate) fn is_number(value: &str) -> bool {
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
