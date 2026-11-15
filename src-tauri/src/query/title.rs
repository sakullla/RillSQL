//! 从语句快照生成稳定的本地标题：操作类型 · 主要对象。
//!
//! 只读执行时留下的 SQL 文本，不看之后的草稿，也不调用外部模型；无法可靠识别对象时
//! 降级成“操作 · 序号”，识别不到操作时返回 None，由调用方回退到既有的“结果 N”文案。
//! 词法复用 split 的顶层词与 complete 的语句范围解析，避免另写一套 SQL 解析。

use std::ops::Range;

use super::complete::{parse_scope, TableRef};
use super::split::top_level_words;

/// 标题里的分隔符。中英文都用这一种写法，不再按语言改标点。
const SEP: &str = " · ";

/// 主语句的操作类型：跳过 WITH 的 CTE 定义后取第一个顶层词。
pub fn operation(engine: &str, sql: &str) -> Option<String> {
    let (words, _) = top_level_words(engine, sql);
    let index = main_verb_index(&words);
    words.get(index).map(|word| word.0.clone())
}

/// WITH [RECURSIVE] name [(列)] AS [NOT] [MATERIALIZED] ( ... ) [, ...] 之后的下标。
fn main_verb_index(words: &[(String, usize, usize)]) -> usize {
    let Some(first) = words.first() else {
        return 0;
    };
    if first.0 != "WITH" {
        return 0;
    }
    let mut index = 1;
    while index < words.len() {
        let word = words[index].0.as_str();
        if matches!(word, "RECURSIVE" | "AS" | "NOT" | "MATERIALIZED") {
            index += 1;
            continue;
        }
        // 一个 CTE 定义：名字之后必须是 AS，否则就是主语句开始了。
        index += 1;
        if words.get(index).map(|word| word.0.as_str()) != Some("AS") {
            return index - 1;
        }
        index += 1;
        while matches!(words.get(index).map(|word| word.0.as_str()), Some("NOT") | Some("MATERIALIZED")) {
            index += 1;
        }
    }
    index
}

/// 语句的主要对象：DML 目标优先，其次首个可证明且不是 CTE 的顶层来源。
///
/// 不枚举 JOIN 的全部来源，也不把 CTE 别名当成物理对象；只取一条能证明的，
/// 取不到就返回 None。
pub fn object(engine: &str, sql: &str) -> Option<String> {
    let scope = parse_scope(sql);
    if let Some(target) = scope.target.as_ref() {
        if !is_cte(&scope.ctes, target) {
            return Some(qualified(target));
        }
    }
    // 主体第一个来源是派生表（FROM (SELECT ...) t）时跳过它的别名：
    // 那是子查询的名字，不是可证明的物理对象。
    let skip_derived = first_source_is_derived(engine, sql);
    if let Some(table) = first_source(&scope.tables, &scope.ctes, skip_derived) {
        return Some(qualified(table));
    }
    // 主体只读 CTE（WITH cte AS (SELECT ... FROM real) SELECT * FROM cte）：
    // 物理对象在 CTE 体里，仍然只取第一条能证明的。
    for body in cte_bodies(engine, sql) {
        let inner = parse_scope(&sql[body.clone()]);
        if let Some(table) = first_source(&inner.tables, &inner.ctes, false) {
            return Some(qualified(table));
        }
    }
    None
}

/// 首个不属于 CTE 的来源；同一层里按语句里的出现顺序，不按 JOIN 展开。
fn first_source<'a>(tables: &'a [TableRef], ctes: &[String], skip_derived: bool) -> Option<&'a TableRef> {
    let mut sources = tables.iter().filter(|table| !is_cte(ctes, table));
    if skip_derived {
        sources.next();
    }
    sources.min_by_key(|table| table.depth)
}

/// 第一个顶层 FROM/JOIN 的来源是不是括号里的子查询。
fn first_source_is_derived(engine: &str, sql: &str) -> bool {
    let (words, _) = top_level_words(engine, sql);
    for word in &words {
        if word.0 == "FROM" || word.0 == "JOIN" {
            return sql[skip_noise(sql, word.2)..].starts_with('(');
        }
    }
    false
}

fn is_cte(ctes: &[String], table: &TableRef) -> bool {
    ctes.iter().any(|cte| cte.eq_ignore_ascii_case(&table.name) || cte.eq_ignore_ascii_case(&table.alias))
}

fn qualified(table: &TableRef) -> String {
    let name = table.name.trim_end_matches('.');
    if table.schema.is_empty() {
        name.to_owned()
    } else {
        format!("{}.{}", table.schema, name)
    }
}

/// 一条语句的自动标题；语句里没有可证明的操作时返回 None。
///
/// index 是这条语句在本次执行里的序号（从 1 开始），对象不可靠时用它区分。
pub fn title(engine: &str, sql: &str, index: usize) -> Option<String> {
    let operation = operation(engine, sql)?;
    match object(engine, sql) {
        Some(object) => Some(format!("{operation}{SEP}{object}")),
        None => Some(format!("{operation}{SEP}{index}")),
    }
}

/// 一批语句的自动标题，与输入一一对应；同批里重复的标题附上语句序号消歧。
pub fn titles<T: AsRef<str>>(engine: &str, statements: &[T]) -> Vec<Option<String>> {
    let mut titles: Vec<Option<String>> = statements
        .iter()
        .enumerate()
        .map(|(position, sql)| title(engine, sql.as_ref(), position + 1))
        .collect();
    let repeated: Vec<String> = titles
        .iter()
        .filter_map(|title| title.clone())
        .filter(|title| titles.iter().filter(|other| other.as_deref() == Some(title.as_str())).count() > 1)
        .collect();
    for (position, title) in titles.iter_mut().enumerate() {
        if let Some(current) = title.as_deref() {
            if repeated.iter().any(|other| other == current) {
                *title = Some(format!("{current} ({})", position + 1));
            }
        }
    }
    titles
}

/// WITH 里每个 CTE 括号体的字节范围（不含括号本身）。
fn cte_bodies(engine: &str, sql: &str) -> Vec<Range<usize>> {
    let (words, _) = top_level_words(engine, sql);
    let mut bodies = Vec::new();
    if words.first().map(|word| word.0.as_str()) != Some("WITH") {
        return bodies;
    }
    let mut index = 1;
    while index < words.len() {
        let word = words[index].0.as_str();
        if matches!(word, "RECURSIVE" | "AS" | "NOT" | "MATERIALIZED") {
            index += 1;
            continue;
        }
        index += 1;
        if words.get(index).map(|word| word.0.as_str()) != Some("AS") {
            break;
        }
        let mut at = words[index].2;
        index += 1;
        while matches!(words.get(index).map(|word| word.0.as_str()), Some("NOT") | Some("MATERIALIZED")) {
            at = words[index].2;
            index += 1;
        }
        let start = skip_noise(sql, at);
        if !sql[start..].starts_with('(') {
            break;
        }
        let Some(close) = matching_paren(sql, start) else { break };
        bodies.push(start + 1..close);
        if !sql[close + 1..].trim_start().starts_with(',') {
            break;
        }
    }
    bodies
}

/// 跳过空白与注释，落在下一个真正的字符上。
fn skip_noise(sql: &str, mut at: usize) -> usize {
    loop {
        let rest = &sql[at..];
        let trimmed = rest.trim_start();
        at += rest.len() - trimmed.len();
        if at >= sql.len() {
            return sql.len();
        }
        if sql[at..].starts_with("--") {
            at = sql[at..].find('\n').map(|offset| at + offset + 1).unwrap_or(sql.len());
            continue;
        }
        if sql[at..].starts_with("/*") {
            match sql[at + 2..].find("*/") {
                Some(offset) => {
                    at = at + 2 + offset + 2;
                    continue;
                }
                None => return sql.len(),
            }
        }
        return at;
    }
}

/// 从左括号开始找到与它配对的右括号；跳过字符串、引用标识符和注释里的括号。
fn matching_paren(sql: &str, open: usize) -> Option<usize> {
    let bytes = sql.as_bytes();
    let mut depth = 0usize;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' | b'"' | 0x60 => {
                let quote = bytes[i];
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == quote {
                        if quote == b'\'' && bytes.get(i + 1) == Some(&b'\'') {
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += step(sql, i);
                }
            }
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += step(sql, i);
                }
                i = (i + 2).min(bytes.len());
            }
            b'(' => {
                depth += 1;
                i += 1;
            }
            b')' => {
                depth = depth.saturating_sub(1);
                i += 1;
                if depth == 0 {
                    return Some(i - 1);
                }
            }
            _ => i += step(sql, i),
        }
    }
    None
}

/// 一个字符占的字节数；只在字符边界上调用。
fn step(sql: &str, at: usize) -> usize {
    sql[at..].chars().next().map_or(1, char::len_utf8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn title_of(sql: &str) -> String {
        title("", sql, 1).unwrap_or_else(|| "<none>".to_owned())
    }

    #[test]
    fn read_and_write_statements_name_their_operation_and_object() {
        assert_eq!(title_of("SELECT * FROM customers"), "SELECT · customers");
        assert_eq!(title_of("select id from public.users"), "SELECT · public.users");
        assert_eq!(title_of("INSERT INTO audit_log (id) VALUES (1)"), "INSERT · audit_log");
        assert_eq!(title_of("UPDATE users SET name = 'x'"), "UPDATE · users");
        assert_eq!(title_of("DELETE FROM logs WHERE id = 1"), "DELETE · logs");
    }

    #[test]
    fn a_statement_without_a_provable_object_falls_back_to_its_sequence() {
        assert_eq!(title("", "SELECT 1", 1).as_deref(), Some("SELECT · 1"));
        assert_eq!(title("", "SELECT now()", 4).as_deref(), Some("SELECT · 4"));
        // 只有子查询来源：不把内层表伪装成主要对象。
        assert_eq!(title_of("SELECT * FROM (SELECT * FROM orders) t"), "SELECT · 1");
        // 没有可证明的操作：交给调用方回退到“结果 N”。
        assert_eq!(title("", "", 1), None);
        assert_eq!(title("", "-- 只有注释\n", 2), None);
    }

    #[test]
    fn joins_and_subqueries_take_only_the_first_provable_source() {
        assert_eq!(
            title_of("SELECT * FROM customers c JOIN orders o ON c.id = o.customer_id"),
            "SELECT · customers"
        );
        assert_eq!(
            title_of("SELECT * FROM orders LEFT JOIN customers c ON c.id = orders.customer_id"),
            "SELECT · orders"
        );
    }

    #[test]
    fn a_cte_alias_is_never_reported_as_a_physical_object() {
        let sql = "WITH recent AS (SELECT * FROM orders WHERE id > 1) SELECT * FROM recent";
        assert_eq!(title_of(sql), "SELECT · orders", "物理来源在 CTE 体里");
        // CTE 与物理表并存：取物理表，不取别名。
        let sql = "WITH c AS (SELECT 1) SELECT * FROM c JOIN customers ON customers.id = c.id";
        assert_eq!(title_of(sql), "SELECT · customers");
        // 多个 CTE 也能跳过定义找到主操作。
        let sql = "WITH a AS (SELECT 1), b AS (SELECT * FROM invoices) SELECT * FROM b";
        assert_eq!(title_of(sql), "SELECT · invoices", "取第一个能证明的 CTE 体来源");
        assert_eq!(
            operation("", "WITH RECURSIVE tree AS (SELECT 1) SELECT * FROM tree"),
            Some("SELECT".to_owned())
        );
    }

    #[test]
    fn quoted_and_long_names_keep_the_name_they_were_given() {
        assert_eq!(title_of("SELECT * FROM \"customers\""), "SELECT · customers");
        assert_eq!(title_of("SELECT * FROM \"sales\".\"orders\""), "SELECT · sales.orders");
        // 反引号限定名同样能取到库与对象，不留悬空的分隔点。
        let backticked = format!("SELECT * FROM {0}sales{0}.{0}orders{0}", 96 as char);
        assert_eq!(title_of(&backticked), "SELECT · sales.orders");
        let long = "a".repeat(80);
        assert_eq!(title_of(&format!("SELECT * FROM {long}")), format!("SELECT · {long}"));
    }

    #[test]
    fn a_batch_keeps_statements_apart_and_numbers_repeated_names() {
        let batch = ["SELECT * FROM customers", "SELECT * FROM orders"];
        assert_eq!(
            titles("", &batch),
            vec![Some("SELECT · customers".to_owned()), Some("SELECT · orders".to_owned())]
        );
        let batch = ["SELECT 1", "SELECT 1"];
        assert_eq!(
            titles("", &batch),
            vec![Some("SELECT · 1".to_owned()), Some("SELECT · 2".to_owned())],
            "无对象的语句靠序号区分，不硬塞表名"
        );
        let batch = ["SELECT * FROM t", "SELECT * FROM t"];
        assert_eq!(
            titles("", &batch),
            vec![Some("SELECT · t (1)".to_owned()), Some("SELECT · t (2)".to_owned())],
            "同批重复名称附加序号消歧"
        );
        // 单条结果没有重复，不编号。
        assert_eq!(titles("", &["SELECT * FROM t"]), vec![Some("SELECT · t".to_owned())]);
    }

    #[test]
    fn the_operation_skips_leading_comments_and_whitespace() {
        assert_eq!(operation("", "\n  -- 说明\n  SELECT 1"), Some("SELECT".to_owned()));
        assert_eq!(operation("", "/* 批量 */ INSERT INTO t VALUES (1)"), Some("INSERT".to_owned()));
        assert_eq!(title("", "  select 1", 2).as_deref(), Some("SELECT · 2"), "操作词统一大写");
    }
}
