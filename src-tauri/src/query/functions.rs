//! 各引擎常用内置函数的签名表（聚合、字符串、日期、转换、窗口等）。
//!
//! 只做纯查表：补全详情、签名提示和悬停都从这里取。表里没有的函数返回 None，
//! 不猜参数；目录里的用户函数只按名字参与补全，没有参数信息时不显示签名提示。

use super::complete::{self, Tok};

/// 一个内置函数的签名。params 里用方括号标出可省略的参数；variadic 表示最后一个参数可以重复。
/// bare 表示调用时不写括号（如 current_date），不弹签名提示。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FunctionSig {
    pub name: &'static str,
    pub params: &'static [&'static str],
    pub variadic: bool,
    pub returns: &'static str,
    pub bare: bool,
}

const fn f(name: &'static str, params: &'static [&'static str], returns: &'static str) -> FunctionSig {
    FunctionSig { name, params, variadic: false, returns, bare: false }
}

const fn v(name: &'static str, params: &'static [&'static str], returns: &'static str) -> FunctionSig {
    FunctionSig { name, params, variadic: true, returns, bare: false }
}

const fn b(name: &'static str, returns: &'static str) -> FunctionSig {
    FunctionSig { name, params: &[], variadic: false, returns, bare: true }
}

const POSTGRES: &[FunctionSig] = &[
    // 聚合
    f("count", &["expression"], "bigint"),
    f("sum", &["expression"], "numeric"),
    f("avg", &["expression"], "numeric"),
    f("min", &["expression"], "same as input"),
    f("max", &["expression"], "same as input"),
    f("string_agg", &["value", "delimiter"], "text"),
    f("array_agg", &["expression"], "array"),
    f("json_agg", &["expression"], "json"),
    f("jsonb_agg", &["expression"], "jsonb"),
    f("bool_and", &["expression"], "boolean"),
    f("bool_or", &["expression"], "boolean"),
    // 条件
    v("coalesce", &["value", "value"], "same as input"),
    f("nullif", &["value1", "value2"], "same as input"),
    v("greatest", &["value", "value"], "same as input"),
    v("least", &["value", "value"], "same as input"),
    // 字符串
    f("length", &["text"], "integer"),
    f("lower", &["text"], "text"),
    f("upper", &["text"], "text"),
    f("substr", &["string", "start", "[count]"], "text"),
    f("substring", &["string FROM start FOR count"], "text"),
    f("trim", &["[LEADING | TRAILING | BOTH] [characters] FROM string"], "text"),
    f("btrim", &["string", "[characters]"], "text"),
    f("ltrim", &["string", "[characters]"], "text"),
    f("rtrim", &["string", "[characters]"], "text"),
    f("replace", &["string", "from", "to"], "text"),
    v("concat", &["value", "value"], "text"),
    v("concat_ws", &["separator", "value", "value"], "text"),
    f("left", &["string", "n"], "text"),
    f("right", &["string", "n"], "text"),
    f("position", &["substring IN string"], "integer"),
    f("split_part", &["string", "delimiter", "n"], "text"),
    f("regexp_replace", &["string", "pattern", "replacement", "[flags]"], "text"),
    f("lpad", &["string", "length", "[fill]"], "text"),
    f("rpad", &["string", "length", "[fill]"], "text"),
    f("md5", &["text"], "text"),
    v("format", &["formatstr", "value"], "text"),
    // 日期
    f("now", &[], "timestamp with time zone"),
    b("current_date", "date"),
    b("current_timestamp", "timestamp with time zone"),
    f("date_trunc", &["field", "source"], "timestamp"),
    f("date_part", &["field", "source"], "double precision"),
    f("extract", &["field FROM source"], "numeric"),
    f("age", &["timestamp", "[timestamp]"], "interval"),
    f("make_interval", &["[years]", "[months]", "[weeks]", "[days]", "[hours]", "[mins]", "[secs]"], "interval"),
    f("generate_series", &["start", "stop", "[step]"], "setof"),
    // 转换
    f("cast", &["expression AS type"], "type"),
    f("to_char", &["value", "format"], "text"),
    f("to_date", &["text", "format"], "date"),
    f("to_timestamp", &["text", "format"], "timestamp with time zone"),
    f("to_number", &["text", "format"], "numeric"),
    f("to_json", &["value"], "json"),
    f("to_jsonb", &["value"], "jsonb"),
    v("json_build_object", &["key", "value"], "json"),
    v("jsonb_build_object", &["key", "value"], "jsonb"),
    v("jsonb_extract_path", &["from_json", "path"], "jsonb"),
    f("array_length", &["array", "dimension"], "integer"),
    f("unnest", &["array"], "setof"),
    // 数学
    f("abs", &["x"], "same as input"),
    f("round", &["x", "[digits]"], "numeric"),
    f("ceil", &["x"], "same as input"),
    f("floor", &["x"], "same as input"),
    f("mod", &["y", "x"], "same as input"),
    f("power", &["a", "b"], "double precision"),
    f("sqrt", &["x"], "double precision"),
    f("random", &[], "double precision"),
    // 窗口
    f("row_number", &[], "bigint"),
    f("rank", &[], "bigint"),
    f("dense_rank", &[], "bigint"),
    f("ntile", &["num_buckets"], "integer"),
    f("lag", &["value", "[offset]", "[default]"], "same as input"),
    f("lead", &["value", "[offset]", "[default]"], "same as input"),
    f("first_value", &["value"], "same as input"),
    f("last_value", &["value"], "same as input"),
    // 其它
    f("gen_random_uuid", &[], "uuid"),
    f("pg_size_pretty", &["bigint"], "text"),
    f("current_setting", &["setting_name"], "text"),
];

const MYSQL: &[FunctionSig] = &[
    // 聚合
    f("count", &["expr"], "bigint"),
    f("sum", &["expr"], "numeric"),
    f("avg", &["expr"], "numeric"),
    f("min", &["expr"], "same as input"),
    f("max", &["expr"], "same as input"),
    f("group_concat", &["expr [ORDER BY …] [SEPARATOR sep]"], "text"),
    f("json_arrayagg", &["expr"], "json"),
    // 条件
    v("coalesce", &["value", "value"], "same as input"),
    f("ifnull", &["expr1", "expr2"], "same as input"),
    f("nullif", &["expr1", "expr2"], "same as input"),
    f("if", &["condition", "value_if_true", "value_if_false"], "same as input"),
    v("greatest", &["value", "value"], "same as input"),
    v("least", &["value", "value"], "same as input"),
    // 字符串
    v("concat", &["str", "str"], "text"),
    v("concat_ws", &["separator", "str", "str"], "text"),
    f("length", &["str"], "int"),
    f("char_length", &["str"], "int"),
    f("lower", &["str"], "text"),
    f("upper", &["str"], "text"),
    f("substring", &["str", "pos", "[len]"], "text"),
    f("substring_index", &["str", "delim", "count"], "text"),
    f("left", &["str", "len"], "text"),
    f("right", &["str", "len"], "text"),
    f("trim", &["str"], "text"),
    f("ltrim", &["str"], "text"),
    f("rtrim", &["str"], "text"),
    f("replace", &["str", "from_str", "to_str"], "text"),
    f("locate", &["substr", "str", "[pos]"], "int"),
    f("instr", &["str", "substr"], "int"),
    f("lpad", &["str", "len", "padstr"], "text"),
    f("rpad", &["str", "len", "padstr"], "text"),
    f("repeat", &["str", "count"], "text"),
    f("reverse", &["str"], "text"),
    f("format", &["x", "d", "[locale]"], "text"),
    // 日期
    f("now", &[], "datetime"),
    f("curdate", &[], "date"),
    f("curtime", &[], "time"),
    b("current_date", "date"),
    b("current_timestamp", "datetime"),
    f("date_format", &["date", "format"], "text"),
    f("str_to_date", &["str", "format"], "datetime"),
    f("date_add", &["date", "INTERVAL expr unit"], "datetime"),
    f("date_sub", &["date", "INTERVAL expr unit"], "datetime"),
    f("datediff", &["expr1", "expr2"], "int"),
    f("timestampdiff", &["unit", "datetime_expr1", "datetime_expr2"], "bigint"),
    f("year", &["date"], "int"),
    f("month", &["date"], "int"),
    f("day", &["date"], "int"),
    f("unix_timestamp", &["[date]"], "bigint"),
    f("from_unixtime", &["unix_timestamp", "[format]"], "datetime"),
    // 转换
    f("cast", &["expr AS type"], "type"),
    f("convert", &["expr", "type"], "type"),
    v("json_extract", &["json_doc", "path"], "json"),
    v("json_object", &["key", "val"], "json"),
    f("json_unquote", &["json_val"], "text"),
    // 数学
    f("abs", &["x"], "same as input"),
    f("round", &["x", "[d]"], "numeric"),
    f("ceil", &["x"], "bigint"),
    f("floor", &["x"], "bigint"),
    f("mod", &["n", "m"], "numeric"),
    f("power", &["x", "y"], "double"),
    f("rand", &["[seed]"], "double"),
    // 窗口
    f("row_number", &[], "bigint"),
    f("rank", &[], "bigint"),
    f("dense_rank", &[], "bigint"),
    f("lag", &["expr", "[n]", "[default]"], "same as input"),
    f("lead", &["expr", "[n]", "[default]"], "same as input"),
    // 其它
    f("uuid", &[], "text"),
    f("last_insert_id", &[], "bigint"),
    f("database", &[], "text"),
];

const SQLITE: &[FunctionSig] = &[
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

const SQLSERVER: &[FunctionSig] = &[
    // 聚合
    f("count", &["expression"], "int"),
    f("count_big", &["expression"], "bigint"),
    f("sum", &["expression"], "numeric"),
    f("avg", &["expression"], "numeric"),
    f("min", &["expression"], "same as input"),
    f("max", &["expression"], "same as input"),
    f("string_agg", &["expression", "separator"], "nvarchar"),
    // 条件
    v("coalesce", &["expression", "expression"], "same as input"),
    f("isnull", &["check_expression", "replacement_value"], "same as input"),
    f("nullif", &["expression1", "expression2"], "same as input"),
    f("iif", &["boolean_expression", "true_value", "false_value"], "same as input"),
    v("choose", &["index", "val"], "same as input"),
    // 字符串
    f("len", &["string_expression"], "int"),
    f("datalength", &["expression"], "int"),
    f("lower", &["character_expression"], "nvarchar"),
    f("upper", &["character_expression"], "nvarchar"),
    f("substring", &["expression", "start", "length"], "nvarchar"),
    f("left", &["character_expression", "integer_expression"], "nvarchar"),
    f("right", &["character_expression", "integer_expression"], "nvarchar"),
    f("ltrim", &["character_expression"], "nvarchar"),
    f("rtrim", &["character_expression"], "nvarchar"),
    f("trim", &["string"], "nvarchar"),
    f("replace", &["string_expression", "string_pattern", "string_replacement"], "nvarchar"),
    f("charindex", &["expressionToFind", "expressionToSearch", "[start_location]"], "int"),
    f("patindex", &["pattern", "expression"], "int"),
    v("concat", &["string_value", "string_value"], "nvarchar"),
    v("concat_ws", &["separator", "argument", "argument"], "nvarchar"),
    f("replicate", &["string_expression", "integer_expression"], "nvarchar"),
    f("reverse", &["string_expression"], "nvarchar"),
    f("stuff", &["character_expression", "start", "length", "replace_with_expression"], "nvarchar"),
    f("format", &["value", "format", "[culture]"], "nvarchar"),
    // 日期
    f("getdate", &[], "datetime"),
    f("getutcdate", &[], "datetime"),
    f("sysdatetime", &[], "datetime2"),
    b("current_timestamp", "datetime"),
    f("dateadd", &["datepart", "number", "date"], "same as date"),
    f("datediff", &["datepart", "startdate", "enddate"], "int"),
    f("datepart", &["datepart", "date"], "int"),
    f("datename", &["datepart", "date"], "nvarchar"),
    f("eomonth", &["start_date", "[month_to_add]"], "date"),
    f("year", &["date"], "int"),
    f("month", &["date"], "int"),
    f("day", &["date"], "int"),
    // 转换
    f("cast", &["expression AS data_type"], "data_type"),
    f("try_cast", &["expression AS data_type"], "data_type"),
    f("convert", &["data_type", "expression", "[style]"], "data_type"),
    f("try_convert", &["data_type", "expression", "[style]"], "data_type"),
    f("json_value", &["expression", "path"], "nvarchar"),
    f("json_query", &["expression", "[path]"], "nvarchar"),
    f("isjson", &["expression"], "int"),
    // 数学
    f("abs", &["numeric_expression"], "same as input"),
    f("round", &["numeric_expression", "length", "[function]"], "same as input"),
    f("ceiling", &["numeric_expression"], "same as input"),
    f("floor", &["numeric_expression"], "same as input"),
    f("power", &["float_expression", "y"], "float"),
    f("sqrt", &["float_expression"], "float"),
    f("rand", &["[seed]"], "float"),
    // 窗口
    f("row_number", &[], "bigint"),
    f("rank", &[], "bigint"),
    f("dense_rank", &[], "bigint"),
    f("ntile", &["integer_expression"], "bigint"),
    f("lag", &["scalar_expression", "[offset]", "[default]"], "same as input"),
    f("lead", &["scalar_expression", "[offset]", "[default]"], "same as input"),
    // 其它
    f("newid", &[], "uniqueidentifier"),
    f("scope_identity", &[], "numeric"),
    f("object_id", &["object_name"], "int"),
    f("db_name", &["[database_id]"], "nvarchar"),
];

fn table(engine: &str) -> &'static [FunctionSig] {
    match engine {
        "postgres" => POSTGRES,
        "mysql" | "mariadb" => MYSQL,
        "sqlite" => SQLITE,
        "sqlserver" => SQLSERVER,
        _ => &[],
    }
}

/// 这个引擎签名表里的全部函数名，按字母排序。未知引擎为空。
pub fn names(engine: &str) -> Vec<&'static str> {
    let mut list: Vec<&'static str> = table(engine).iter().map(|sig| sig.name).collect();
    list.sort_unstable();
    list.dedup();
    list
}

/// 按名字（大小写不敏感）查签名；表里没有返回 None。
pub fn lookup(engine: &str, name: &str) -> Option<&'static FunctionSig> {
    table(engine).iter().find(|sig| sig.name.eq_ignore_ascii_case(name))
}

/// 写了括号才会调用的函数才有签名提示；不写括号的（current_date）和未知函数都没有。
pub fn call_signature(engine: &str, name: &str) -> Option<&'static FunctionSig> {
    lookup(engine, name).filter(|sig| !sig.bare)
}

impl FunctionSig {
    /// 参数列表文本，可变参数在末尾加 “, …”。
    pub fn params_text(&self) -> String {
        let mut text = self.params.join(", ");
        if self.variadic {
            text.push_str(", …");
        }
        text
    }

    /// 一行签名：`name(params) → returns`；不写括号的函数是 `name → returns`。
    pub fn text(&self) -> String {
        if self.bare {
            format!("{} → {}", self.name, self.returns)
        } else {
            format!("{}({}) → {}", self.name, self.params_text(), self.returns)
        }
    }

    /// 第 argument 个实参（从 0 开始）对应的形参下标。可变参数停在最后一个；
    /// 超出固定参数个数或函数没有参数时为 None。
    pub fn active_param(&self, argument: usize) -> Option<usize> {
        if self.params.is_empty() {
            return None;
        }
        if argument < self.params.len() {
            return Some(argument);
        }
        if self.variadic {
            Some(self.params.len() - 1)
        } else {
            None
        }
    }
}

/// 光标所在的函数调用：函数名、左括号的字节偏移、当前是第几个实参（从 0 开始）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallContext {
    pub name: String,
    pub open: usize,
    pub argument: usize,
}

/// 从光标往回找还没闭合的左括号，括号前紧跟函数名时给出调用上下文。
/// 实参序号按这对括号内顶层的逗号计数；字符串、注释、引用标识符和内层括号里的逗号都不算。
/// 光标在字符串或注释里、在括号外、或者括号前不是名字时为 None。
pub fn call_context(text: &str, cursor: usize) -> Option<CallContext> {
    let mut cursor = cursor.min(text.len());
    while cursor > 0 && !text.is_char_boundary(cursor) {
        cursor -= 1;
    }
    let start = complete::statement_start(text, cursor);
    let head = &text[start..cursor];
    let (tokens, open) = complete::tokens(head);
    if open {
        return None;
    }
    let mut depth = 0usize;
    let mut commas = 0usize;
    for (index, token) in tokens.iter().enumerate().rev() {
        if token.word {
            continue;
        }
        match token.text.as_str() {
            ")" => depth += 1,
            "(" if depth > 0 => depth -= 1,
            "(" => return call_name(&tokens, index).map(|name| CallContext { name, open: start + token.start, argument: commas }),
            "," if depth == 0 => commas += 1,
            _ => {}
        }
    }
    None
}

fn call_name(tokens: &[Tok], open: usize) -> Option<String> {
    let before = tokens.get(open.checked_sub(1)?)?;
    if !before.word || before.quoted || before.end != tokens[open].start {
        return None;
    }
    Some(before.text.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_engine_has_a_real_table_and_unknown_engines_have_none() {
        for engine in ["postgres", "mysql", "mariadb", "sqlite", "sqlserver"] {
            let list = names(engine);
            assert!(list.len() >= 40, "{engine} 只有 {} 个函数", list.len());
            for name in &list {
                let sig = lookup(engine, name).expect("名字来自同一张表");
                assert_eq!(&sig.name, name);
            }
        }
        assert!(names("redis").is_empty());
        assert!(lookup("mongodb", "count").is_none());
    }

    #[test]
    fn known_functions_return_their_parameters_and_unknown_ones_return_none() {
        let substr = lookup("postgres", "SUBSTR").expect("大小写不敏感");
        assert_eq!(substr.params, &["string", "start", "[count]"]);
        assert_eq!(substr.text(), "substr(string, start, [count]) → text");
        let len = lookup("sqlserver", "len").expect("SQL Server 的 LEN");
        assert_eq!(len.params, &["string_expression"]);
        assert!(lookup("postgres", "len").is_none(), "PostgreSQL 没有 LEN");
        assert!(lookup("sqlite", "date_format").is_none(), "SQLite 没有 DATE_FORMAT");
        assert_eq!(lookup("mysql", "date_format").expect("MySQL").params, &["date", "format"]);
        assert!(lookup("postgres", "no_such_function").is_none());
        let coalesce = lookup("sqlite", "coalesce").expect("可变参数");
        assert_eq!(coalesce.params_text(), "X, Y, …");
        assert!(call_signature("postgres", "current_date").is_none(), "不写括号的函数没有签名提示");
        assert_eq!(lookup("postgres", "current_date").expect("仍可补全").text(), "current_date → date");
    }

    #[test]
    fn active_parameter_follows_commas_and_stops_at_the_last_variadic_one() {
        let replace = lookup("postgres", "replace").unwrap();
        assert_eq!(replace.active_param(0), Some(0));
        assert_eq!(replace.active_param(2), Some(2));
        assert_eq!(replace.active_param(3), None, "固定参数用完以后不再高亮");
        let concat = lookup("postgres", "concat").unwrap();
        assert_eq!(concat.active_param(5), Some(1));
        assert_eq!(lookup("postgres", "now").unwrap().active_param(0), None);
    }

    #[test]
    fn call_context_counts_top_level_commas_only() {
        let sql = "SELECT replace(name, 'a,b', ";
        let found = call_context(sql, sql.len()).expect("在 replace 括号里");
        assert_eq!(found.name, "replace");
        assert_eq!(found.open, sql.find('(').unwrap());
        assert_eq!(found.argument, 2, "字符串里的逗号不算");

        let nested = "SELECT coalesce(round(x, 2), ";
        let found = call_context(nested, nested.len()).expect("外层 coalesce");
        assert_eq!(found.name, "coalesce");
        assert_eq!(found.argument, 1, "内层括号里的逗号不算");

        let inner = "SELECT coalesce(round(x, ";
        let found = call_context(inner, inner.len()).expect("内层 round");
        assert_eq!(found.name, "round");
        assert_eq!(found.argument, 1);

        let first = "SELECT count(";
        assert_eq!(call_context(first, first.len()).unwrap().argument, 0);
    }

    #[test]
    fn leaving_the_parentheses_or_typing_inside_strings_has_no_call() {
        let closed = "SELECT count(id) ";
        assert!(call_context(closed, closed.len()).is_none(), "括号已经闭合");
        let grouped = "SELECT (1, ";
        assert!(call_context(grouped, grouped.len()).is_none(), "括号前不是函数名");
        let quoted = "SELECT replace(name, 'a, ";
        assert!(call_context(quoted, quoted.len()).is_none(), "光标在字符串里");
        let spaced = "SELECT count (";
        assert!(call_context(spaced, spaced.len()).is_none(), "名字和括号之间有空白时不当作调用");
        let previous = "SELECT count(1;\nSELECT ";
        assert!(call_context(previous, previous.len()).is_none(), "上一条语句的括号不算");
        let middle = "SELECT replace(a, b, c)";
        let found = call_context(middle, "SELECT replace(a, ".len()).expect("光标在第二个参数前");
        assert_eq!(found.argument, 1);
    }
}
