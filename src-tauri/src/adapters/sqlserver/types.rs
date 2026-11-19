//! SQL Server 的类型规则：可手输的类型、N'' 与当前时间函数默认值、常用字段类型。

use crate::adapters::ddl::{generic_parse_default, quote_text, type_base, ParamType, PresetTypes, TypeRules};
use crate::ddl::model::DefaultSpec;
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

    fn param_types(&self) -> &'static [ParamType] {
        &[
            ParamType { label: "nvarchar(n)", insert: "nvarchar(255)" },
            ParamType { label: "varchar(n)", insert: "varchar(255)" },
            ParamType { label: "nchar(n)", insert: "nchar(36)" },
            ParamType { label: "decimal(p,s)", insert: "decimal(18,2)" },
            ParamType { label: "datetime2(p)", insert: "datetime2(3)" },
            ParamType { label: "varbinary(n)", insert: "varbinary(255)" },
        ]
    }

    fn expression_choices(&self) -> &'static [&'static str] {
        &["GETDATE()", "SYSDATETIME()", "GETUTCDATE()", "SYSUTCDATETIME()", "CURRENT_TIMESTAMP"]
    }

    /// sys.default_constraints 的定义带外层括号（`((0))`、`(N'open')`、`(getdate())`），字符串可带 N 前缀。
    fn parse_default(&self, raw: &str) -> DefaultSpec {
        let mut text = raw.trim();
        while let Some(inner) = text.strip_prefix('(').and_then(|rest| rest.strip_suffix(')')) {
            if !balanced(inner) {
                break;
            }
            text = inner.trim();
        }
        if (text.starts_with("N'") || text.starts_with("n'")) && is_quoted(&text[1..]) {
            return generic_parse_default(&text[1..]);
        }
        generic_parse_default(text)
    }

    /// Unicode 字符类型（nchar/nvarchar/ntext）的字符串写成 N'…'。
    fn text_default(&self, type_name: &str, value: &str) -> String {
        let quoted = quote_text(value);
        if type_base(type_name).starts_with('n') { format!("N{quoted}") } else { quoted }
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

/// 括号成对且不在中途闭合（`(a)+(b)` 去掉外层后不成对）。
fn balanced(text: &str) -> bool {
    let mut depth = 0i32;
    for ch in text.chars() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => {}
        }
    }
    depth == 0
}
