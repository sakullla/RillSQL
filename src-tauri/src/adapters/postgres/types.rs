//! PostgreSQL 的类型规则：可手输的类型、默认值候选与布尔/now() 默认值、常用字段类型。

use crate::adapters::ddl::{generic_parse_default, ParamType, PresetTypes, TypeRules};
use crate::ddl::model::DefaultSpec;

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

    fn param_types(&self) -> &'static [ParamType] {
        &[
            ParamType { label: "varchar(n)", insert: "varchar(255)" },
            ParamType { label: "char(n)", insert: "char(36)" },
            ParamType { label: "numeric(p,s)", insert: "numeric(10,2)" },
            ParamType { label: "timestamp(p)", insert: "timestamp(3)" },
            ParamType { label: "timestamptz(p)", insert: "timestamptz(3)" },
        ]
    }

    fn expression_choices(&self) -> &'static [&'static str] {
        &["CURRENT_TIMESTAMP", "NOW()", "CURRENT_DATE"]
    }

    /// `'paid'::text`、`NULL::character varying` 这类带类型转换的默认值：去掉转换再按通用规则解析。
    fn parse_default(&self, raw: &str) -> DefaultSpec {
        let text = raw.trim();
        if let Some(head) = strip_cast(text) {
            let inner = generic_parse_default(head);
            if !matches!(inner, DefaultSpec::Expression(_)) {
                return inner;
            }
        }
        generic_parse_default(text)
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

/// 末尾的 `::type`（可带空格与括号参数）之前的部分；前面是带引号字面量或 NULL 时才算。
fn strip_cast(text: &str) -> Option<&str> {
    let at = text.rfind("::")?;
    let head = text[..at].trim();
    let tail = text[at + 2..].trim();
    if tail.is_empty() || !tail.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, ' ' | '_' | '(' | ')' | ',' | '[' | ']')) {
        return None;
    }
    let head = head.strip_prefix('(').and_then(|inner| inner.strip_suffix(')')).unwrap_or(head).trim();
    (head.starts_with('\'') || head.eq_ignore_ascii_case("null") || head.starts_with('-')).then_some(head)
}
