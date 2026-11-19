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
