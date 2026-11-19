//! SQL Server 的类型规则：可手输的类型、N'' 与当前时间函数默认值、常用字段类型。

use crate::adapters::ddl::{PresetTypes, TypeRules};
use crate::ddl::sql::is_quoted;

pub(in crate::adapters) struct SqlserverTypes;

impl TypeRules for SqlserverTypes {
    fn valid_type_base(&self, base: &str) -> bool {
        SQLSERVER_TYPES.contains(&base)
    }

    /// T-SQL 的 Unicode 字符串默认值 N'...'；GETDATE()/SYSDATETIME() 是常见默认值，GETUTCDATE() 同属当前时间函数。
    fn extra_default(&self, text: &str, upper: &str) -> bool {
        if (text.starts_with("N'") || text.starts_with("n'")) && is_quoted(&text[1..]) {
            return true;
        }
        matches!(upper, "GETDATE()" | "GETDATE" | "SYSDATETIME()" | "GETUTCDATE()" | "SYSUTCDATETIME()")
    }

    fn type_choices(&self) -> &'static [&'static str] {
        &[
            "bigint",
            "int",
            "smallint",
            "tinyint",
            "bit",
            "nvarchar(255)",
            "varchar(255)",
            "decimal(18,2)",
            "datetime2",
            "date",
            "uniqueidentifier",
            "varbinary(max)",
        ]
    }

    fn default_choices(&self) -> &'static [&'static str] {
        &["0", "''", "N''", "GETDATE()", "SYSDATETIME()", "GETUTCDATE()"]
    }

    fn default_column_type(&self) -> &'static str {
        "nvarchar(255)"
    }

    fn id_column_type(&self) -> &'static str {
        "bigint"
    }

    fn preset_types(&self) -> PresetTypes {
        PresetTypes {
            timestamp: "datetime2",
            timestamp_default: "SYSDATETIME()",
            short_text: "nvarchar(32)",
            note: "nvarchar(255)",
            uuid: "uniqueidentifier",
        }
    }
}

const SQLSERVER_TYPES: &[&str] = &[
    "bigint", "int", "smallint", "tinyint", "bit", "decimal", "numeric", "money", "smallmoney",
    "float", "real", "date", "datetime", "datetime2", "datetimeoffset", "smalldatetime", "time",
    "char", "varchar", "text", "nchar", "nvarchar", "ntext", "binary", "varbinary", "image",
    "uniqueidentifier", "xml",
];
