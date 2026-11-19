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

    fn extra_default(&self, _text: &str, upper: &str) -> bool {
        upper == "CURRENT_TIMESTAMP()"
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
        &["CURRENT_TIMESTAMP", "CURRENT_TIMESTAMP()", "CURRENT_DATE"]
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
