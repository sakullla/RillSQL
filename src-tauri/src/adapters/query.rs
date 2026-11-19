//! 网格与查询的方言门面（ADR-0 / R22，A4）。
//!
//! 主代码以 `Engine` 或引擎 id 字符串调用，这里经注册表找到该引擎的 [`SqlSyntax`]；
//! 没有 SQL 语法组的引擎（Redis/MongoDB）落到 [`NoSqlSyntax`]，保持原来的占位写法。

use std::sync::OnceLock;

use super::{adapter, identifier_quotes, registry, standard_quote, Lexis, SqlSyntax, Workbench};
use crate::connections::model::Engine;

/// 没有 SQL 生成面的引擎：网格/查询不会把它们送进 SQL 编译（描述表结构时已拒绝），
/// 万一被调用时按原样投影、标准 SQL 双引号与通用写法，不编造方言。
struct NoSqlSyntax;

impl SqlSyntax for NoSqlSyntax {
    fn quote_always(&self, name: &str) -> String {
        standard_quote(name)
    }

    fn reserved_words(&self) -> &'static [&'static str] {
        &[]
    }

    fn quote_ident(&self, name: &str) -> String {
        standard_quote(name)
    }

    fn text_cast(&self, ident: &str) -> String {
        ident.to_owned()
    }

    fn text_preview(&self, plain: &str, _cap: usize) -> String {
        plain.to_owned()
    }

    fn binary_length(&self, ident: &str) -> String {
        format!("CASE WHEN {ident} IS NULL THEN NULL ELSE {ident} END")
    }

    fn binary_hex(&self, ident: &str) -> String {
        ident.to_owned()
    }

    fn lexis(&self) -> Lexis {
        Lexis::UNKNOWN
    }
}

/// 引擎的 SQL 语法；没有语法组的引擎给占位实现。
pub fn sql_syntax(engine: Engine) -> &'static dyn SqlSyntax {
    adapter(engine).syntax().unwrap_or(&NoSqlSyntax)
}

/// 按引擎 id 取 SQL 语法；未知引擎或没有 SQL 语法组时为 None。
pub fn syntax_named(engine: &str) -> Option<&'static dyn SqlSyntax> {
    registry::parse(engine).and_then(|engine| adapter(engine).syntax())
}

/// 这个引擎 id 是有 SQL 方言的引擎（编辑器的格式化、计划、补全据此启用）。
pub fn known_sql_engine(engine: &str) -> bool {
    syntax_named(engine).is_some()
}

/// 引擎 id 的词法特征；未知引擎保守处理。
pub fn lexis_named(engine: &str) -> Lexis {
    syntax_named(engine).map(|syntax| syntax.lexis()).unwrap_or(Lexis::UNKNOWN)
}

/// 引擎 id 的显示名；未知引擎为空。
pub fn label_named(engine: &str) -> &'static str {
    registry::parse(engine).map(|engine| adapter(engine).meta().label()).unwrap_or_default()
}

/// 引擎 id 的工作台类型；未知引擎为 None。
pub fn workbench_named(engine: &str) -> Option<Workbench> {
    registry::parse(engine).map(|engine| adapter(engine).meta().workbench())
}

/// 用于切分/大纲/高亮的方言 id：有 SQL 方言的引擎原样返回，其余（未绑定、未知、键值/文档引擎）按未绑定方言。
pub fn sql_dialect_id(engine: &str) -> &'static str {
    adapter(registry::sql_engine(engine)).meta().id()
}

/// 所有方言里系统内部对象的名字前缀（小写）。
pub fn system_object_prefixes() -> impl Iterator<Item = &'static str> {
    registry::ENGINES
        .into_iter()
        .filter_map(|engine| adapter(engine).syntax())
        .flat_map(|syntax| syntax.system_object_prefixes().iter().copied())
}

/// 所有方言专有的语句关键字（大写），筛选表达式等只接受条件的地方据此拒绝。
pub fn dialect_statement_keywords() -> impl Iterator<Item = &'static str> {
    registry::ENGINES
        .into_iter()
        .filter_map(|engine| adapter(engine).syntax())
        .flat_map(|syntax| syntax.statement_keywords().iter().copied())
}

/// 通用词法认的引号字符：字符串单引号，加上各方言开闭相同的标识符引号。
fn quote_chars() -> &'static [char] {
    static CHARS: OnceLock<Vec<char>> = OnceLock::new();
    CHARS.get_or_init(|| {
        let mut chars = vec!['\''];
        for (open, close) in identifier_quotes() {
            if open == close && !chars.contains(&open) {
                chars.push(open);
            }
        }
        chars
    })
}

/// 任一方言标识符引号的开、闭字符。
fn identifier_quote_chars() -> &'static [char] {
    static CHARS: OnceLock<Vec<char>> = OnceLock::new();
    CHARS.get_or_init(|| {
        let mut chars = Vec::new();
        for (open, close) in identifier_quotes() {
            for ch in [open, close] {
                if !chars.contains(&ch) {
                    chars.push(ch);
                }
            }
        }
        chars
    })
}

/// 这个字符会开始一段引号文本（字符串或开闭相同的引用标识符）。
pub fn is_quote_char(ch: char) -> bool {
    quote_chars().contains(&ch)
}

/// [`is_quote_char`] 的字节形态（多字节字符的字节一律不是引号）。
pub fn is_quote_byte(byte: u8) -> bool {
    byte.is_ascii() && is_quote_char(byte as char)
}

/// 这个字符是某个方言的标识符引号（开或闭）。
pub fn is_identifier_quote(ch: char) -> bool {
    identifier_quote_chars().contains(&ch)
}
