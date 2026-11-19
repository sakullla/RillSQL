//! MySQL 家族的类型规则：可手输的类型、默认值候选、常用字段类型与外键动作（没有 SET DEFAULT）。

use crate::adapters::ddl::{PresetTypes, TypeRules};

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

    fn default_choices(&self) -> &'static [&'static str] {
        &["0", "''", "CURRENT_TIMESTAMP", "CURRENT_DATE"]
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
