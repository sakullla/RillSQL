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
    fn extra_default(&self, _text: &str, upper: &str) -> bool {
        current_timestamp_fsp(upper).is_some() || upper == "(CURRENT_DATE)"
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
