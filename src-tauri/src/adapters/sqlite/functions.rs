//! SQLite 常用内置函数的签名表（补全详情、签名提示与悬停）。

use crate::query::functions::{b, f, v, FunctionSig};

pub(super) const SQLITE: &[FunctionSig] = &[
    // 聚合
    f("count", &["X"], "integer"),
    f("sum", &["X"], "numeric"),
    f("total", &["X"], "real"),
    f("avg", &["X"], "real"),
    f("min", &["X"], "same as input"),
    f("max", &["X"], "same as input"),
    f("group_concat", &["X", "[Y]"], "text"),
    f("json_group_array", &["value"], "text"),
    f("json_group_object", &["name", "value"], "text"),
    // 条件
    v("coalesce", &["X", "Y"], "same as input"),
    f("ifnull", &["X", "Y"], "same as input"),
    f("nullif", &["X", "Y"], "same as input"),
    f("iif", &["X", "Y", "Z"], "same as input"),
    // 字符串
    f("length", &["X"], "integer"),
    f("lower", &["X"], "text"),
    f("upper", &["X"], "text"),
    f("substr", &["X", "Y", "[Z]"], "text"),
    f("substring", &["X", "Y", "[Z]"], "text"),
    f("trim", &["X", "[Y]"], "text"),
    f("ltrim", &["X", "[Y]"], "text"),
    f("rtrim", &["X", "[Y]"], "text"),
    f("replace", &["X", "Y", "Z"], "text"),
    f("instr", &["X", "Y"], "integer"),
    v("printf", &["FORMAT", "arg"], "text"),
    v("format", &["FORMAT", "arg"], "text"),
    f("hex", &["X"], "text"),
    f("quote", &["X"], "text"),
    f("unicode", &["X"], "integer"),
    v("char", &["X1", "X2"], "text"),
    f("typeof", &["X"], "text"),
    // 日期
    v("date", &["time_value", "modifier"], "text"),
    v("time", &["time_value", "modifier"], "text"),
    v("datetime", &["time_value", "modifier"], "text"),
    v("julianday", &["time_value", "modifier"], "real"),
    v("unixepoch", &["time_value", "modifier"], "integer"),
    v("strftime", &["format", "time_value", "modifier"], "text"),
    b("current_date", "text"),
    b("current_timestamp", "text"),
    // 转换
    f("cast", &["expr AS type"], "type"),
    f("json", &["json"], "text"),
    v("json_extract", &["json", "path"], "value"),
    v("json_object", &["label", "value"], "text"),
    v("json_array", &["value"], "text"),
    f("json_each", &["json", "[path]"], "table"),
    // 数学
    f("abs", &["X"], "same as input"),
    f("round", &["X", "[Y]"], "real"),
    f("ceil", &["X"], "same as input"),
    f("floor", &["X"], "same as input"),
    f("random", &[], "integer"),
    // 窗口
    f("row_number", &[], "integer"),
    f("rank", &[], "integer"),
    f("dense_rank", &[], "integer"),
    f("lag", &["expr", "[offset]", "[default]"], "same as input"),
    f("lead", &["expr", "[offset]", "[default]"], "same as input"),
    // 其它
    f("last_insert_rowid", &[], "integer"),
    f("changes", &[], "integer"),
];
