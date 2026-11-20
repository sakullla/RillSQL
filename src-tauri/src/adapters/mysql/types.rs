//! MySQL 家族的类型规则：可手输的类型、默认值候选、常用字段类型与外键动作（没有 SET DEFAULT）。

use crate::adapters::ddl::{generic_parse_default, ParamType, PresetTypes, TypeRules};
use crate::ddl::model::DefaultSpec;

pub(in crate::adapters) struct MysqlTypes;

impl TypeRules for MysqlTypes {
    fn fk_actions(&self) -> &'static [&'static str] {
        &["RESTRICT", "CASCADE", "SET NULL", "NO ACTION"]
    }

    fn valid_type_base(&self, base: &str) -> bool {
        MYSQL_TYPES.contains(&base)
    }

    /// `CURRENT_TIMESTAMP(n)`（0–6 位小数秒，配 datetime(n)/timestamp(n)）与带括号的 `(CURRENT_DATE)`。
    /// 读回时 MySQL 8 把 `(CURRENT_DATE)` 报成 `curdate()`，结构快照包成 `(curdate())`，同样接受。
    fn extra_default(&self, _text: &str, upper: &str) -> bool {
        current_timestamp_fsp(upper).is_some() || upper == "(CURRENT_DATE)" || upper == "(CURDATE())"
    }

    /// MySQL 8 不接受裸写的 CURRENT_DATE / CURRENT_TIME（要写成带括号的表达式）；
    /// datetime/timestamp 列的当前时间默认值小数秒位数必须与列一致，否则服务端报 ERROR 1067。
    fn expression_default_problem(&self, type_name: &str, upper: &str) -> Option<String> {
        if upper == "CURRENT_DATE" || upper == "CURRENT_TIME" {
            return Some(format!("MySQL 要写成带括号的表达式 ({upper})。"));
        }
        let expression_fsp = current_timestamp_fsp(upper)?;
        let column = type_name.trim().to_ascii_lowercase();
        let base = crate::adapters::ddl::type_base(&column);
        if base != "datetime" && base != "timestamp" {
            return None;
        }
        let column_fsp = column
            .split_once('(')
            .and_then(|(_, rest)| rest.split_once(')'))
            .and_then(|(digits, _)| digits.trim().parse::<u8>().ok())
            .unwrap_or(0);
        (column_fsp != expression_fsp).then(|| {
            let wanted = if column_fsp == 0 { "CURRENT_TIMESTAMP".to_owned() } else { format!("CURRENT_TIMESTAMP({column_fsp})") };
            format!("列类型 {} 的当前时间默认值要写 {wanted}（小数秒位数必须一致）。", type_name.trim())
        })
    }

    /// MySQL/MariaDB 的 BOOLEAN 落地为 `tinyint(1)`（可带 unsigned/zerofill）；其他宽度的 tinyint 是普通整数。
    fn flag_type(&self, type_name: &str) -> bool {
        let compact: String = type_name.chars().filter(|ch| !ch.is_whitespace()).collect::<String>().to_ascii_lowercase();
        compact.strip_prefix("tinyint(1)").is_some_and(|rest| rest.is_empty() || rest == "unsigned" || rest == "zerofill" || rest == "unsignedzerofill")
            || compact == "bool"
            || compact == "boolean"
    }

    fn type_choices(&self) -> &'static [&'static str] {
        &[
            "bigint",
            "int",
            "tinyint(1)",
            "varchar(255)",
            "varchar(64)",
            "text",
            "decimal(10,2)",
            "datetime",
            "timestamp",
            "date",
            "json",
            "blob",
        ]
    }

    fn param_types(&self) -> &'static [ParamType] {
        &[
            ParamType { label: "varchar(n)", insert: "varchar(255)" },
            ParamType { label: "char(n)", insert: "char(36)" },
            ParamType { label: "decimal(p,s)", insert: "decimal(10,2)" },
            ParamType { label: "varbinary(n)", insert: "varbinary(255)" },
            ParamType { label: "datetime(fsp)", insert: "datetime(3)" },
            ParamType { label: "timestamp(fsp)", insert: "timestamp(3)" },
        ]
    }

    fn expression_choices(&self) -> &'static [&'static str] {
        &["CURRENT_TIMESTAMP", "CURRENT_TIMESTAMP(3)", "(CURRENT_DATE)"]
    }

    /// information_schema 的 COLUMN_DEFAULT：MySQL 给出不带引号的字面量原文，MariaDB 给出带引号的字面量与 `NULL`。
    /// 带括号或当前时间关键字的是表达式；其余不带引号的原文按字面量。
    fn parse_default(&self, raw: &str) -> DefaultSpec {
        let text = raw.trim();
        let parsed = generic_parse_default(text);
        let DefaultSpec::Expression(_) = parsed else {
            return parsed;
        };
        let upper = text.to_ascii_uppercase();
        if text.contains('(') || text.contains('\'') || upper.starts_with("CURRENT_") || upper.starts_with("LOCALTIME") || upper == "NOW()" {
            return parsed;
        }
        DefaultSpec::Literal(text.to_owned())
    }

    fn text_default(&self, _type_name: &str, value: &str) -> String {
        super::ddl::mysql_text(value)
    }

    fn default_column_type(&self) -> &'static str {
        "varchar(255)"
    }

    fn id_column_type(&self) -> &'static str {
        "bigint"
    }

    fn preset_types(&self) -> PresetTypes {
        PresetTypes { timestamp: "datetime", timestamp_default: "CURRENT_TIMESTAMP", short_text: "varchar(32)", note: "varchar(255)", uuid: "char(36)" }
    }
}

/// information_schema.columns 的 COLUMN_DEFAULT（`raw`，SQL NULL 为 None）与 EXTRA → 结构快照里的默认值写法，
/// 让对比与转储拼出的 `DEFAULT ...` 在 MySQL 8 与 MariaDB 上都能执行：
/// - EXTRA 标 DEFAULT_GENERATED 的是 MySQL 8 表达式默认值，读回不带括号（`(CURRENT_DATE)` 读回 `curdate()`），
///   除当前时间写法外补上外层括号；
/// - 非 NULL 的空串是 `DEFAULT ''`，写成 `''`（不再与“无默认值”混同）；
/// - 含单引号却不以引号开头、也不像函数调用或 `b'…'`/`x'…'` 的，是 MySQL 不带引号的文本字面量，补引号并翻倍内部引号。
///
/// 其余（MariaDB 已带引号的字面量、数字、NULL、当前时间、无标记的旧版写法）原样保留，交给方言的 parse_default。
pub(in crate::adapters) fn reported_default(raw: Option<&str>, extra: &str) -> Option<String> {
    let raw = raw?;
    let text = raw.trim();
    if extra.to_ascii_uppercase().contains("DEFAULT_GENERATED") && !text.is_empty() {
        let upper = text.to_ascii_uppercase();
        if current_timestamp_fsp(&upper).is_some() || wrapped_in_parens(text) {
            return Some(text.to_owned());
        }
        return Some(format!("({text})"));
    }
    if raw.is_empty() {
        return Some("''".to_owned());
    }
    let bit_or_hex = text.len() >= 2 && text.as_bytes()[1] == b'\'' && matches!(text.as_bytes()[0], b'b' | b'B' | b'x' | b'X');
    if raw.contains('\'') && !text.starts_with('\'') && !text.contains('(') && !bit_or_hex {
        return Some(crate::adapters::ddl::quote_text(raw));
    }
    Some(raw.to_owned())
}

/// 整段被一对外层括号包住（第一个左括号的配对右括号就是最后一个字符）。
fn wrapped_in_parens(text: &str) -> bool {
    if !text.starts_with('(') {
        return false;
    }
    let mut depth = 0usize;
    for (at, ch) in text.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return at + 1 == text.len();
                }
            }
            _ => {}
        }
    }
    false
}

/// `CURRENT_TIMESTAMP`、`CURRENT_TIMESTAMP()`、`CURRENT_TIMESTAMP(n)`（n 为 0–6）的小数秒位数；其他写法为 None。
/// 带外层括号的 `(CURRENT_TIMESTAMP)` 是表达式默认值，不在此列。
fn current_timestamp_fsp(upper: &str) -> Option<u8> {
    let rest = upper.strip_prefix("CURRENT_TIMESTAMP")?;
    if rest.is_empty() {
        return Some(0);
    }
    let inner = rest.strip_prefix('(')?.strip_suffix(')')?.trim();
    if inner.is_empty() {
        return Some(0);
    }
    inner.parse::<u8>().ok().filter(|fsp| *fsp <= 6 && inner.len() == 1)
}

const MYSQL_TYPES: &[&str] = &[
    "tinyint",
    "smallint",
    "mediumint",
    "int",
    "integer",
    "bigint",
    "decimal",
    "numeric",
    "float",
    "double",
    "real",
    "bit",
    "char",
    "varchar",
    "binary",
    "varbinary",
    "tinytext",
    "text",
    "mediumtext",
    "longtext",
    "tinyblob",
    "blob",
    "mediumblob",
    "longblob",
    "date",
    "datetime",
    "timestamp",
    "time",
    "year",
    "json",
    "boolean",
    "bool",
];

#[cfg(test)]
mod default_expression_tests {
    use crate::adapters::ddl::types_named;

    /// datetime(n)/timestamp(n) 的当前时间默认值要带同样的小数秒位数；CURRENT_DATE 要加括号。
    #[test]
    fn fractional_seconds_and_current_date_follow_mysql_rules() {
        for engine in ["mysql", "mariadb"] {
            let types = types_named(engine);
            for accepted in ["CURRENT_TIMESTAMP()", "CURRENT_TIMESTAMP(3)", "CURRENT_TIMESTAMP(6)", "(CURRENT_DATE)"] {
                assert!(types.extra_default(accepted, accepted), "{engine}: {accepted}");
            }
            for rejected in ["CURRENT_TIMESTAMP(7)", "CURRENT_TIMESTAMP(33)", "CURRENT_TIMESTAMP(x)", "CURRENT_TIMESTAMP(3))"] {
                assert!(!types.extra_default(rejected, rejected), "{engine}: {rejected}");
            }
            assert_eq!(types.expression_default_problem("datetime(3)", "CURRENT_TIMESTAMP(3)"), None);
            assert_eq!(types.expression_default_problem("datetime", "CURRENT_TIMESTAMP"), None);
            assert_eq!(types.expression_default_problem("timestamp", "CURRENT_TIMESTAMP()"), None);
            assert_eq!(types.expression_default_problem("TIMESTAMP(6)", "CURRENT_TIMESTAMP(6)"), None);
            assert_eq!(types.expression_default_problem("date", "(CURRENT_DATE)"), None);
            let mismatch = types.expression_default_problem("datetime(3)", "CURRENT_TIMESTAMP").expect("fsp mismatch");
            assert!(mismatch.contains("CURRENT_TIMESTAMP(3)"), "{mismatch}");
            let mismatch = types.expression_default_problem("datetime", "CURRENT_TIMESTAMP(3)").expect("fsp mismatch");
            assert!(mismatch.contains("CURRENT_TIMESTAMP"), "{mismatch}");
            assert!(types.expression_default_problem("date", "CURRENT_DATE").is_some_and(|reason| reason.contains("(CURRENT_DATE)")));
            for choice in types.expression_choices() {
                assert!(choice.starts_with("CURRENT_TIMESTAMP") || types.extra_default(choice, choice), "{engine}: {choice}");
                assert_ne!(*choice, "CURRENT_DATE", "{engine}: 裸写 CURRENT_DATE 会被 MySQL 8 拒绝");
            }
        }
    }
}

#[cfg(test)]
mod reported_default_tests {
    use super::reported_default;
    use crate::adapters::ddl::types_named;
    use crate::ddl::model::{DefaultSpec, Engine};
    use crate::ddl::sql::{existing_default, existing_default_sql};

    /// 读回的默认值经快照规整后，对比/转储拼出的 DEFAULT 子句都是 MySQL 8 能执行的写法。
    #[test]
    fn reported_defaults_become_executable_default_clauses() {
        let clause = |raw: Option<&str>, extra: &str, type_name: &str| {
            let snapshot = reported_default(raw, extra);
            existing_default_sql(Engine::Mysql, type_name, snapshot.as_deref().unwrap_or_default())
        };
        // 表达式默认值：MySQL 8 读回不带括号，补括号；当前时间写法保持原样。
        assert_eq!(clause(Some("curdate()"), "DEFAULT_GENERATED", "date"), "(curdate())");
        assert_eq!(clause(Some("(rand() * 10)"), "DEFAULT_GENERATED", "double"), "(rand() * 10)");
        assert_eq!(clause(Some("(1) + (2)"), "DEFAULT_GENERATED", "int"), "((1) + (2))");
        assert_eq!(clause(Some("CURRENT_TIMESTAMP"), "DEFAULT_GENERATED on update CURRENT_TIMESTAMP", "datetime"), "CURRENT_TIMESTAMP");
        assert_eq!(clause(Some("CURRENT_TIMESTAMP(3)"), "DEFAULT_GENERATED", "datetime(3)"), "CURRENT_TIMESTAMP(3)");
        // 空串默认值不再塌缩为“无默认值”；SQL NULL 才是无默认值。
        assert_eq!(clause(Some(""), "", "varchar(10)"), "''");
        assert_eq!(reported_default(None, ""), None);
        // 含单引号的文本字面量补引号并翻倍。
        assert_eq!(clause(Some("it's"), "", "varchar(10)"), "'it''s'");
        // 不带引号的普通文本字面量、数字、MariaDB 已带引号的字面量与表达式维持原有处理。
        assert_eq!(clause(Some("new"), "", "varchar(10)"), "'new'");
        assert_eq!(clause(Some("0"), "", "int"), "0");
        assert_eq!(clause(Some("'it''s'"), "", "varchar(10)"), "'it''s'");
        assert_eq!(clause(Some("curdate()"), "", "date"), "curdate()");
        assert_eq!(clause(Some("b'101'"), "", "bit(3)"), "b'101'");
    }

    /// 结构页读到的四态与规整一致：空串是空字面量，含引号文本是字面量，补括号的 curdate() 仍是可接受的表达式。
    #[test]
    fn reported_defaults_parse_into_the_structure_states() {
        let parse = |raw: Option<&str>, extra: &str| existing_default(Engine::Mysql, reported_default(raw, extra).as_deref());
        assert_eq!(parse(Some(""), ""), DefaultSpec::Literal(String::new()));
        assert_eq!(parse(Some("it's"), ""), DefaultSpec::Literal("it's".to_owned()));
        assert_eq!(parse(None, ""), DefaultSpec::None);
        assert_eq!(parse(Some("curdate()"), "DEFAULT_GENERATED"), DefaultSpec::Expression("(curdate())".to_owned()));
        assert!(types_named("mysql").extra_default("(curdate())", "(CURDATE())"));
    }
}

#[cfg(test)]
mod flag_type_tests {
    use crate::adapters::ddl::types_named;

    /// MySQL/MariaDB 的 BOOLEAN 落地为 tinyint(1)：网格为它给开关；其他宽度和其他引擎不算。
    #[test]
    fn tinyint_one_is_a_flag_only_in_the_mysql_family() {
        for engine in ["mysql", "mariadb"] {
            let types = types_named(engine);
            for name in ["tinyint(1)", "TINYINT(1)", "tinyint(1) unsigned", "tinyint( 1 )", "boolean"] {
                assert!(types.flag_type(name), "{engine}: {name}");
            }
            for name in ["tinyint", "tinyint(4)", "int(1)", "tinyint(10)", "bit(1)", "varchar(1)"] {
                assert!(!types.flag_type(name), "{engine}: {name}");
            }
        }
        for engine in ["postgres", "sqlite", "sqlserver"] {
            assert!(!types_named(engine).flag_type("tinyint(1)"), "{engine}");
        }
    }
}
