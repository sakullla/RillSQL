//! PostgreSQL 的类型规则：可手输的类型、默认值候选与布尔/now() 默认值、常用字段类型。

use crate::adapters::ddl::{PresetTypes, TypeRules};

pub(in crate::adapters) struct PostgresTypes;

impl TypeRules for PostgresTypes {
    fn valid_type_base(&self, base: &str) -> bool {
        POSTGRES_TYPES.contains(&base)
    }

    fn extra_default(&self, _text: &str, upper: &str) -> bool {
        matches!(upper, "TRUE" | "FALSE" | "NOW()")
    }

    fn type_choices(&self) -> &'static [&'static str] {
        &[
            "bigint",
            "integer",
            "smallint",
            "bigserial",
            "serial",
            "varchar(255)",
            "text",
            "boolean",
            "numeric(10,2)",
            "timestamptz",
            "timestamp",
            "date",
            "jsonb",
            "uuid",
            "bytea",
        ]
    }

    fn default_choices(&self) -> &'static [&'static str] {
        &["0", "''", "TRUE", "FALSE", "CURRENT_TIMESTAMP", "NOW()"]
    }

    fn default_column_type(&self) -> &'static str {
        "text"
    }

    fn id_column_type(&self) -> &'static str {
        "bigint"
    }

    fn preset_types(&self) -> PresetTypes {
        PresetTypes { timestamp: "timestamptz", timestamp_default: "CURRENT_TIMESTAMP", short_text: "varchar(32)", note: "text", uuid: "uuid" }
    }
}

const POSTGRES_TYPES: &[&str] = &[
    "smallint",
    "integer",
    "bigint",
    "int",
    "int2",
    "int4",
    "int8",
    "real",
    "double precision",
    "numeric",
    "decimal",
    "text",
    "varchar",
    "character varying",
    "char",
    "character",
    "boolean",
    "bool",
    "date",
    "timestamp",
    "timestamptz",
    "timestamp with time zone",
    "timestamp without time zone",
    "time",
    "time with time zone",
    "time without time zone",
    "json",
    "jsonb",
    "uuid",
    "bytea",
    "serial",
    "bigserial",
];
