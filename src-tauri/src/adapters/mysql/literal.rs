//! MySQL 家族的字面量编码与解析：单引号字符串、ENUM/SET 候选值列表。
//!
//! ENUM/SET 的候选值可以是空串、含单引号、反斜杠、逗号或中文，序列化与解析必须能够互相还原，
//! 且写进语句的写法要被服务器接受。反斜杠是否为转义符取决于会话 sql_mode 的 NO_BACKSLASH_ESCAPES，
//! 因此编码与解析都带上 LiteralQuoting，不假设用户界面里已经转义过。

use crate::ddl::model::{EnumSetKind, EnumSetState, LiteralQuoting};

/// 候选值里单个元素的最大字符数（MySQL 与 MariaDB 的 ENUM/SET 元素上限都是 255 个字符）。
pub(in crate::adapters) const MAX_ELEMENT_CHARS: usize = 255;

/// 单引号字符串字面量。反斜杠转义模式下反斜杠与单引号都翻倍；NO_BACKSLASH_ESCAPES 下只把单引号写成两个。
pub(in crate::adapters) fn text(value: &str, quoting: LiteralQuoting) -> String {
    let escaped = if quoting.backslash_escapes {
        value.replace('\\', "\\\\").replace('\'', "''")
    } else {
        value.replace('\'', "''")
    };
    format!("'{escaped}'")
}

/// ENUM/SET 的完整类型写法，如 enum('a','b')、set('x','y,z')。候选值为空或元素过长时给出原因。
pub(in crate::adapters) fn enum_set_type(kind: EnumSetKind, values: &[String], quoting: LiteralQuoting) -> Result<String, String> {
    if values.is_empty() {
        return Err(format!("{} 至少要有一个候选值。", kind.keyword().to_uppercase()));
    }
    let mut rendered = Vec::with_capacity(values.len());
    for value in values {
        if value.chars().count() > MAX_ELEMENT_CHARS {
            return Err(format!(
                "候选值 {}… 超过 {MAX_ELEMENT_CHARS} 个字符，服务器会拒绝这个定义。",
                value.chars().take(12).collect::<String>()
            ));
        }
        rendered.push(text(value, quoting));
    }
    Ok(format!("{}({})", kind.keyword(), rendered.join(",")))
}

/// 读回的 COLUMN_TYPE 转成 ENUM/SET 候选值。不是 enum/set 时返回 None（调用方按普通类型处理）。
pub(in crate::adapters) fn parse_enum_set(type_name: &str, quoting: LiteralQuoting) -> Option<EnumSetState> {
    let text = type_name.trim();
    let lower = text.to_ascii_lowercase();
    let (kind, body) = if lower.starts_with("enum(") {
        (EnumSetKind::Enum, text.get(5..)?)
    } else if lower.starts_with("set(") {
        (EnumSetKind::Set, text.get(4..)?)
    } else {
        return None;
    };
    let body = body.strip_suffix(')')?;
    let values = parse_elements(body, quoting)?;
    Some(EnumSetState { kind, values })
}

/// 候选值列表本体（不带括号）转成各个值。写法不完整时返回 None，调用方不要当成“没有候选值”。
fn parse_elements(body: &str, quoting: LiteralQuoting) -> Option<Vec<String>> {
    let mut values = Vec::new();
    let mut chars = body.chars().peekable();
    loop {
        // 跳过值之间的空白与逗号。
        while chars.peek().is_some_and(|ch| ch.is_whitespace() || *ch == ',') {
            chars.next();
        }
        if chars.peek().is_none() {
            return Some(values);
        }
        if chars.next() != Some('\'') {
            return None;
        }
        let mut value = String::new();
        loop {
            let ch = chars.next()?;
            if ch == '\'' {
                if chars.peek() == Some(&'\'') {
                    chars.next();
                    value.push('\'');
                    continue;
                }
                break;
            }
            if ch == '\\' && quoting.backslash_escapes {
                let next = chars.next()?;
                match next {
                    '0' => value.push('\0'),
                    'n' => value.push('\n'),
                    'r' => value.push('\r'),
                    't' => value.push('\t'),
                    'b' => value.push('\u{8}'),
                    'Z' => value.push('\u{1a}'),
                    other => value.push(other),
                }
                continue;
            }
            value.push(ch);
        }
        values.push(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quoted(backslash_escapes: bool) -> LiteralQuoting {
        LiteralQuoting { backslash_escapes }
    }

    /// 空串、单引号、反斜杠、逗号与中文都要能编码再解析回原值，顺序保持不变。
    #[test]
    fn enum_set_values_round_trip_in_both_quoting_modes() {
        let values = vec![
            String::new(),
            "it's".to_owned(),
            "back\\slash".to_owned(),
            "a,b".to_owned(),
            "中文值".to_owned(),
            "\\'".to_owned(),
        ];
        for quoting in [quoted(true), quoted(false)] {
            for kind in [EnumSetKind::Enum, EnumSetKind::Set] {
                let rendered = enum_set_type(kind, &values, quoting).expect("rendered");
                let parsed = parse_enum_set(&rendered, quoting).expect("parsed");
                assert_eq!(parsed.kind, kind);
                assert_eq!(parsed.values, values, "quoting={quoting:?} rendered={rendered}");
            }
        }
    }

    /// 反斜杠转义模式与 NO_BACKSLASH_ESCAPES 的写法不同：前者把反斜杠翻倍，后者原样保留。
    #[test]
    fn quoting_mode_changes_the_rendered_literal() {
        let value = vec!["a\\b".to_owned()];
        assert_eq!(enum_set_type(EnumSetKind::Enum, &value, quoted(true)).expect("sql"), "enum('a\\\\b')");
        assert_eq!(enum_set_type(EnumSetKind::Enum, &value, quoted(false)).expect("sql"), "enum('a\\b')");
        assert_eq!(text("it's", quoted(true)), "'it''s'");
        assert_eq!(text("it's", quoted(false)), "'it''s'");
    }

    /// 服务器读回的写法：MySQL 报 enum('a','b''c')；反斜杠转义与加倍单引号两种读回都能解析。
    #[test]
    fn read_back_column_type_is_parsed_in_defined_order() {
        let state = parse_enum_set("enum('draft','it''s','done')", quoted(true)).expect("mysql read back");
        assert_eq!(state.kind, EnumSetKind::Enum);
        assert_eq!(state.values, vec!["draft".to_owned(), "it's".to_owned(), "done".to_owned()]);
        let state = parse_enum_set("SET('a','b,c')", quoted(true)).expect("set read back");
        assert_eq!(state.kind, EnumSetKind::Set);
        assert_eq!(state.values, vec!["a".to_owned(), "b,c".to_owned()]);
        assert_eq!(parse_enum_set("enum('a\\'b')", quoted(true)).expect("escaped").values, vec!["a'b".to_owned()]);
        assert_eq!(parse_enum_set("enum('a''b')", quoted(false)).expect("doubled").values, vec!["a'b".to_owned()]);
    }

    /// 不是 enum/set、写法不完整都要能区分；空候选值列表解析出来是空列表，不是“解析失败”。
    #[test]
    fn non_enum_types_and_broken_definitions_are_not_mistaken_for_values() {
        assert!(parse_enum_set("varchar(10)", quoted(true)).is_none());
        assert!(parse_enum_set("enum", quoted(true)).is_none());
        assert!(parse_enum_set("enum('a'", quoted(true)).is_none());
        assert!(parse_enum_set("enum(a)", quoted(true)).is_none());
        assert_eq!(parse_enum_set("enum()", quoted(true)).map(|state| state.values), Some(Vec::new()));
        assert!(enum_set_type(EnumSetKind::Enum, &[], quoted(true)).is_err());
        let long = "x".repeat(MAX_ELEMENT_CHARS + 1);
        assert!(enum_set_type(EnumSetKind::Enum, &[long], quoted(true)).is_err());
    }

    /// 会话 sql_mode 选择转义模式；没读到或没有 NO_BACKSLASH_ESCAPES 时按反斜杠转义。
    #[test]
    fn sql_mode_selects_the_quoting_mode() {
        assert!(LiteralQuoting::from_sql_mode("").backslash_escapes);
        assert!(LiteralQuoting::from_sql_mode("STRICT_TRANS_TABLES,NO_ENGINE_SUBSTITUTION").backslash_escapes);
        assert!(!LiteralQuoting::from_sql_mode("STRICT_TRANS_TABLES,NO_BACKSLASH_ESCAPES").backslash_escapes);
    }
}
