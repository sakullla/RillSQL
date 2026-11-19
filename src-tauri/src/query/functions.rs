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

pub(crate) const fn f(name: &'static str, params: &'static [&'static str], returns: &'static str) -> FunctionSig {
    FunctionSig { name, params, variadic: false, returns, bare: false }
}

pub(crate) const fn v(name: &'static str, params: &'static [&'static str], returns: &'static str) -> FunctionSig {
    FunctionSig { name, params, variadic: true, returns, bare: false }
}

pub(crate) const fn b(name: &'static str, returns: &'static str) -> FunctionSig {
    FunctionSig { name, params: &[], variadic: false, returns, bare: true }
}

/// 引擎的签名表由适配器提供；未知引擎为空。
fn table(engine: &str) -> &'static [FunctionSig] {
    crate::adapters::query::syntax_named(engine).map(|syntax| syntax.functions()).unwrap_or(&[])
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
