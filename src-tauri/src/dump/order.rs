//! 共享排序核心：kind 秩 + Kahn 拓扑，以及为排序供边的 SQL 定义依赖分析。
//! compare 的结构脚本与 dump 的全库导出共用这里的顺序决策；本模块不连接数据库，
//! 也不认识任何一侧的业务模型（Side / DumpObject 都在各自调用方）。

use std::collections::{BTreeMap, BTreeSet};

/// 参与拓扑排序的一个节点。
/// key 是对象标识，也是 ready 集的比较键；同 key 的成员由调用方自己归组。
#[derive(Clone)]
pub(crate) struct DepNode {
    pub key: String,
    /// kind 秩：同时可取时的优先级（dump 的支撑对象按 枚举→域→序列→函数→过程）。
    /// 不分秩的调用方传 0，退化为纯标识序。
    pub rank: u8,
    /// 它引用的同批对象标识。指向批外名字的引用在这里被忽略。
    pub references: Vec<String>,
}

/// Kahn 拓扑 + kind 秩的排序核心。
/// `dependents_first` 为真是 DROP 序（依赖方在前，先删引用别人的一方）；
/// 为假是 CREATE/INSERT 序（被依赖方在前，先建被引用的一方）。
/// 同时可取的节点按 `(rank, key)` 最小者优先；环上的节点不死锁，按同序放到最后。
/// 返回去重后的 key 顺序，同 key 的成员保持在传入时的相对顺序，由调用方展开。
pub(crate) fn order_dep_keys(nodes: Vec<DepNode>, dependents_first: bool) -> Vec<String> {
    let mut rank_of: BTreeMap<String, u8> = BTreeMap::new();
    let mut known: BTreeSet<String> = BTreeSet::new();
    for node in &nodes {
        rank_of.insert(node.key.clone(), node.rank);
        known.insert(node.key.clone());
    }
    let mut forward: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut indegree: BTreeMap<String, usize> = BTreeMap::new();
    for key in &known {
        forward.insert(key.clone(), BTreeSet::new());
        indegree.insert(key.clone(), 0);
    }
    for node in &nodes {
        for referenced in &node.references {
            if referenced.eq_ignore_ascii_case(&node.key) || !known.contains(referenced.as_str()) {
                continue;
            }
            let (from, to) = if dependents_first { (&node.key, referenced) } else { (referenced, &node.key) };
            if forward.get_mut(from).expect("from").insert(to.clone()) {
                *indegree.get_mut(to).expect("to") += 1;
            }
        }
    }
    let rank = |key: &str| (rank_of.get(key).copied().unwrap_or(0), key.to_owned());
    let mut ready: BTreeSet<(u8, String)> = indegree.iter().filter(|(_, degree)| **degree == 0).map(|(key, _)| rank(key)).collect();
    let mut ordered = Vec::new();
    while let Some(entry) = ready.iter().next().cloned() {
        ready.remove(&entry);
        let key = entry.1;
        ordered.push(key.clone());
        for next in forward.get(&key).into_iter().flatten() {
            let degree = indegree.get_mut(next).expect("next");
            *degree -= 1;
            if *degree == 0 {
                ready.insert(rank(next));
            }
        }
    }
    let mut cyclic = indegree
        .iter()
        .filter(|(key, degree)| **degree > 0 && !ordered.iter().any(|item| item == *key))
        .map(|(key, _)| rank(key))
        .collect::<Vec<_>>();
    cyclic.sort();
    ordered.extend(cyclic.into_iter().map(|(_, key)| key));
    ordered
}

/// 定义文本里点到的同批对象名是依赖（支撑对象排序用）。
/// 美元引号里的函数体也要扫，否则函数间的调用顺序只能退回名称序。
pub(crate) fn definition_references(name: &str, definition: &str, names: &BTreeSet<String>) -> Vec<String> {
    let mut refs = BTreeSet::new();
    for token in sql_tokens(definition) {
        if !is_ident_token(&token) {
            continue;
        }
        if let Some(known) = names.iter().find(|item| token.text.eq_ignore_ascii_case(item) && !item.eq_ignore_ascii_case(name)) {
            refs.insert(known.clone());
        }
    }
    refs.into_iter().collect()
}

/// 关系名来自 FROM / JOIN，以及 INSERT、UPDATE 的目标。字符串、注释、别名和 WITH 里的 CTE 名不算引用。
pub(crate) fn referenced_relations(sql: &str) -> Vec<String> {
    let tokens = sql_tokens(sql);
    let mut refs = Vec::new();
    let mut ctes = Vec::new();
    collect_relations(&tokens, &mut refs, &mut ctes);
    refs
}

fn collect_relations(tokens: &[SqlToken], refs: &mut Vec<String>, ctes: &mut Vec<String>) {
    let mut index = 0;
    let scope = ctes.len();
    while index < tokens.len() {
        if is_word(&tokens[index], "WITH") {
            index = consume_with(tokens, index, refs, ctes);
            continue;
        }
        if is_word(&tokens[index], "INSERT") {
            index = consume_insert(tokens, index, refs, ctes);
            continue;
        }
        if is_word(&tokens[index], "UPDATE") && is_table_update(tokens, index) {
            index = consume_update(tokens, index, refs, ctes);
            continue;
        }
        if is_word(&tokens[index], "FROM") || is_word(&tokens[index], "JOIN") {
            let next = parse_table_list(tokens, index + 1, refs, ctes);
            index = if next > index { next } else { index + 1 };
            continue;
        }
        // WHERE EXISTS 和标量子查询里的 FROM 也是引用，不能只扫本层。
        if tokens[index].text == "(" {
            let end = matching_paren(tokens, index);
            let mut nested = ctes.clone();
            collect_relations(&tokens[index + 1..end], refs, &mut nested);
            index = end.saturating_add(1);
            continue;
        }
        index += 1;
    }
    ctes.truncate(scope);
}

/// FOR UPDATE、ON UPDATE、DO UPDATE 不是改表。BEFORE/AFTER UPDATE 是触发器时机。
fn is_table_update(tokens: &[SqlToken], index: usize) -> bool {
    if index == 0 {
        return true;
    }
    let previous = &tokens[index - 1];
    !is_word(previous, "FOR") && !is_word(previous, "ON") && !is_word(previous, "DO") && !is_word(previous, "BEFORE") && !is_word(previous, "AFTER")
}

fn consume_insert(tokens: &[SqlToken], insert_at: usize, refs: &mut Vec<String>, ctes: &[String]) -> usize {
    let mut index = insert_at + 1;
    if index < tokens.len() && is_word(&tokens[index], "INTO") {
        index += 1;
    }
    if !starts_dml_target(tokens, index) {
        return insert_at + 1;
    }
    parse_dml_target(tokens, index, refs, ctes)
}

fn consume_update(tokens: &[SqlToken], update_at: usize, refs: &mut Vec<String>, ctes: &[String]) -> usize {
    let mut index = update_at + 1;
    if index < tokens.len() && is_word(&tokens[index], "ONLY") {
        index += 1;
    }
    if !starts_dml_target(tokens, index) {
        return update_at + 1;
    }
    parse_dml_target(tokens, index, refs, ctes)
}

fn starts_dml_target(tokens: &[SqlToken], index: usize) -> bool {
    if index >= tokens.len() {
        return false;
    }
    if tokens[index].text == "(" {
        return true;
    }
    is_ident_token(&tokens[index]) && !is_from_stop(&tokens[index].text) && !is_word(&tokens[index], "SET") && !is_word(&tokens[index], "VALUES") && !is_word(&tokens[index], "SELECT") && !is_word(&tokens[index], "DEFAULT")
}

fn parse_dml_target(tokens: &[SqlToken], mut index: usize, refs: &mut Vec<String>, ctes: &[String]) -> usize {
    if index < tokens.len() && (is_word(&tokens[index], "ONLY") || is_word(&tokens[index], "LATERAL")) {
        index += 1;
    }
    if index >= tokens.len() {
        return index;
    }
    if tokens[index].text == "(" {
        let end = matching_paren(tokens, index);
        let mut nested = ctes.to_vec();
        collect_relations(&tokens[index + 1..end], refs, &mut nested);
        return skip_alias(tokens, end.saturating_add(1));
    }
    if !starts_dml_target(tokens, index) {
        return index;
    }
    let (name, next) = qualified_relation(tokens, index);
    if !name.is_empty() && !ctes.iter().any(|cte| cte.eq_ignore_ascii_case(&name)) {
        refs.push(name);
    }
    index = next;
    if index < tokens.len() && tokens[index].text == "(" {
        index = matching_paren(tokens, index).saturating_add(1);
    }
    skip_alias(tokens, index)
}

/// 读完 WITH 绑定的 CTE，并让这些名字在后续主查询里可见。返回主查询起点。
fn consume_with(tokens: &[SqlToken], with_at: usize, refs: &mut Vec<String>, ctes: &mut Vec<String>) -> usize {
    let mut index = with_at + 1;
    if index < tokens.len() && is_word(&tokens[index], "RECURSIVE") {
        index += 1;
    }
    loop {
        if index >= tokens.len() || !is_ident_token(&tokens[index]) || is_from_stop(&tokens[index].text) {
            break;
        }
        let name = tokens[index].text.clone();
        let mut next = index + 1;
        if next < tokens.len() && tokens[next].text == "(" {
            let after = matching_paren(tokens, next).saturating_add(1);
            if after < tokens.len() && is_word(&tokens[after], "AS") {
                next = after;
            }
        }
        if next >= tokens.len() || !is_word(&tokens[next], "AS") {
            break;
        }
        next += 1;
        if next < tokens.len() && is_word(&tokens[next], "NOT") {
            next += 1;
        }
        if next < tokens.len() && is_word(&tokens[next], "MATERIALIZED") {
            next += 1;
        }
        if next >= tokens.len() || tokens[next].text != "(" {
            break;
        }
        let end = matching_paren(tokens, next);
        ctes.push(name);
        collect_relations(&tokens[next + 1..end], refs, ctes);
        index = end.saturating_add(1);
        if index < tokens.len() && tokens[index].text == "," {
            index += 1;
            continue;
        }
        break;
    }
    index
}

fn parse_table_list(tokens: &[SqlToken], mut index: usize, refs: &mut Vec<String>, ctes: &[String]) -> usize {
    while index < tokens.len() {
        if is_word(&tokens[index], "ONLY") || is_word(&tokens[index], "LATERAL") {
            index += 1;
            continue;
        }
        if tokens[index].text == "(" {
            let end = matching_paren(tokens, index);
            let mut nested = ctes.to_vec();
            collect_relations(&tokens[index + 1..end], refs, &mut nested);
            index = end.saturating_add(1);
            index = skip_alias(tokens, index);
        } else if is_ident_token(&tokens[index]) {
            let (name, next) = qualified_relation(tokens, index);
            if next < tokens.len() && tokens[next].text == "(" {
                index = matching_paren(tokens, next).saturating_add(1);
            } else {
                if !name.is_empty() && !ctes.iter().any(|cte| cte.eq_ignore_ascii_case(&name)) {
                    refs.push(name);
                }
                index = next;
            }
            index = skip_alias(tokens, index);
        } else {
            break;
        }
        if index < tokens.len() && tokens[index].text == "," {
            index += 1;
            continue;
        }
        break;
    }
    index
}

pub(crate) fn qualified_relation(tokens: &[SqlToken], start: usize) -> (String, usize) {
    let parts = qualified_parts(&tokens[start..]);
    if parts.is_empty() {
        return (String::new(), start);
    }
    let mut index = start + 1;
    let mut seen = 1;
    while seen < parts.len() {
        if index + 1 < tokens.len() && tokens[index].text == "." {
            index += 2;
            seen += 1;
        } else {
            break;
        }
    }
    (parts.last().cloned().unwrap_or_default(), index)
}

pub(crate) fn matching_paren(tokens: &[SqlToken], open: usize) -> usize {
    let mut depth = 0;
    for (index, token) in tokens.iter().enumerate().skip(open) {
        if token.text == "(" {
            depth += 1;
        } else if token.text == ")" {
            depth -= 1;
            if depth == 0 {
                return index;
            }
        }
    }
    tokens.len().saturating_sub(1)
}

fn skip_alias(tokens: &[SqlToken], mut index: usize) -> usize {
    if index < tokens.len() && is_word(&tokens[index], "AS") {
        index += 1;
    }
    if index < tokens.len() && is_ident_token(&tokens[index]) && !is_from_stop(&tokens[index].text) {
        index += 1;
    }
    index
}

fn is_from_stop(word: &str) -> bool {
    matches!(
        word.to_ascii_uppercase().as_str(),
        "WHERE" | "GROUP" | "HAVING" | "WINDOW" | "ORDER" | "LIMIT" | "OFFSET" | "UNION" | "EXCEPT" | "INTERSECT" | "FETCH" | "FOR" | "ON" | "USING" | "LEFT" | "RIGHT" | "INNER" | "OUTER" | "FULL" | "CROSS" | "NATURAL" | "JOIN" | "SELECT" | "SET" | "WHEN" | "THEN" | "ELSE" | "END" | "AND" | "OR"
    )
}

/// 列名、别名和同名标识不是调用。只有 name( 才算视图或定义正文对例程的调用。
pub(crate) fn calls_routine(sql: &str, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let tokens = sql_tokens(sql);
    let mut index = 0;
    while index < tokens.len() {
        if is_word(&tokens[index], "CREATE") {
            let next = skip_created_signature(&tokens, index);
            if next > index {
                index = next;
                continue;
            }
        }
        if is_ident_token(&tokens[index]) {
            let (found, next) = qualified_relation(&tokens, index);
            if next > index && next < tokens.len() && tokens[next].text == "(" && found.eq_ignore_ascii_case(name) {
                return true;
            }
            if next > index {
                index = next;
                continue;
            }
        }
        index += 1;
    }
    false
}

fn skip_created_signature(tokens: &[SqlToken], create_at: usize) -> usize {
    let mut index = create_at + 1;
    if index < tokens.len() && is_word(&tokens[index], "OR") {
        index += 1;
        if index < tokens.len() && is_word(&tokens[index], "REPLACE") {
            index += 1;
        }
    }
    let mut guard = 0;
    while index < tokens.len() && guard < 6 {
        if matches_created_keyword(&tokens[index]) {
            break;
        }
        if is_word(&tokens[index], "TEMP") || is_word(&tokens[index], "TEMPORARY") {
            index += 1;
            guard += 1;
            continue;
        }
        return create_at + 1;
    }
    if index >= tokens.len() {
        return create_at + 1;
    }
    if is_word(&tokens[index], "MATERIALIZED") {
        index += 1;
    }
    if index < tokens.len() && matches_created_keyword(&tokens[index]) && !is_word(&tokens[index], "MATERIALIZED") {
        index += 1;
    } else if index >= tokens.len() || !is_word(&tokens[index], "VIEW") {
        return create_at + 1;
    }
    if index >= tokens.len() || !is_ident_token(&tokens[index]) {
        return index;
    }
    let (_, next) = qualified_relation(tokens, index);
    index = if next > index { next } else { index + 1 };
    if index < tokens.len() && tokens[index].text == "(" {
        index = matching_paren(tokens, index).saturating_add(1);
    }
    index
}

fn matches_created_keyword(token: &SqlToken) -> bool {
    is_word(token, "VIEW") || is_word(token, "FUNCTION") || is_word(token, "PROCEDURE") || is_word(token, "TRIGGER") || is_word(token, "MATERIALIZED")
}

pub(crate) struct SqlToken {
    pub(crate) text: String,
    pub(crate) quoted: bool,
}

pub(crate) fn sql_tokens(sql: &str) -> Vec<SqlToken> {
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < sql.len() {
        let ch = sql[index..].chars().next().unwrap();
        // 函数体在美元引号里。整段跳过会看不见其中的调用，创建顺序就退回名称序。
        if ch == '$' {
            if let Some(end) = skip_dollar(sql, index) {
                let delimiter_len = dollar_delimiter_len(sql, index).unwrap_or(0);
                let body_start = index + delimiter_len;
                let body_end = end.saturating_sub(delimiter_len);
                if body_end > body_start {
                    tokens.extend(sql_tokens(&sql[body_start..body_end]));
                }
                index = end;
                continue;
            }
        }
        if let Some(next) = skip_delimited(sql, index) {
            let ch = sql[index..].chars().next().unwrap();
            if ch == '"' || ch == '`' || ch == '[' {
                tokens.push(SqlToken { text: unquote(&sql[index..next]), quoted: true });
            }
            index = next;
            continue;
        }
        if ch.is_whitespace() {
            index += ch.len_utf8();
            continue;
        }
        if is_ident_start(ch) {
            let end = ident_continue(sql, index);
            tokens.push(SqlToken { text: sql[index..end].to_owned(), quoted: false });
            index = end;
            continue;
        }
        tokens.push(SqlToken { text: ch.to_string(), quoted: false });
        index += 1;
    }
    tokens
}

pub(crate) fn unquote(raw: &str) -> String {
    let closer = if raw.starts_with('[') { ']' } else { raw.chars().next().unwrap_or('"') };
    let open = if closer == ']' { '[' } else { closer };
    let inner = raw.trim_start_matches(open).trim_end_matches(closer);
    inner.replace(&format!("{closer}{closer}"), &closer.to_string())
}

pub(crate) fn is_word(token: &SqlToken, word: &str) -> bool {
    !token.quoted && token.text.eq_ignore_ascii_case(word)
}

pub(crate) fn is_ident_token(token: &SqlToken) -> bool {
    token.quoted || token.text.chars().next().is_some_and(is_ident_start)
}

pub(crate) fn statement_mentions(sql: &str, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    sql_tokens(sql).into_iter().any(|token| if token.quoted { token.text == name } else { token.text.eq_ignore_ascii_case(name) })
}

pub(crate) fn qualified_parts(tokens: &[SqlToken]) -> Vec<String> {
    if tokens.is_empty() || !is_ident_token(&tokens[0]) {
        return Vec::new();
    }
    let mut parts = vec![tokens[0].text.clone()];
    let mut index = 1;
    while index + 1 < tokens.len() && tokens[index].text == "." && is_ident_token(&tokens[index + 1]) {
        parts.push(tokens[index + 1].text.clone());
        index += 2;
    }
    parts
}

pub(crate) fn skip_delimited(sql: &str, index: usize) -> Option<usize> {
    let rest = sql.get(index..)?;
    if rest.starts_with("--") {
        return Some(index + rest.find('\n').map(|at| at + 1).unwrap_or(rest.len()));
    }
    if rest.starts_with("/*") {
        return Some(skip_block_comment(sql, index));
    }
    let ch = rest.chars().next()?;
    if ch == '\'' || ch == '"' || ch == '`' {
        return skip_quoted(sql, index, ch);
    }
    if ch == '$' {
        return skip_dollar(sql, index);
    }
    None
}

pub(crate) fn skip_block_comment(sql: &str, start: usize) -> usize {
    let mut index = start + 2;
    let mut depth = 1;
    while index < sql.len() {
        if sql[index..].starts_with("/*") {
            depth += 1;
            index += 2;
            continue;
        }
        if sql[index..].starts_with("*/") {
            depth -= 1;
            index += 2;
            if depth == 0 {
                return index;
            }
            continue;
        }
        index += sql[index..].chars().next().unwrap().len_utf8();
    }
    sql.len()
}

pub(crate) fn skip_quoted(sql: &str, start: usize, closer: char) -> Option<usize> {
    let mut index = start + closer.len_utf8();
    if closer == ']' {
        index = start + 1;
    }
    while index < sql.len() {
        let ch = sql[index..].chars().next()?;
        if ch == closer {
            let next = index + ch.len_utf8();
            if sql[next..].starts_with(closer) {
                index = next + ch.len_utf8();
                continue;
            }
            return Some(next);
        }
        index += ch.len_utf8();
    }
    Some(sql.len())
}

pub(crate) fn dollar_delimiter_len(sql: &str, start: usize) -> Option<usize> {
    let rest = sql.get(start..)?;
    let tag_end = rest[1..].find('$')?;
    let delimiter = &rest[..=tag_end + 1];
    if !delimiter[1..delimiter.len() - 1].chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
        return None;
    }
    Some(delimiter.len())
}

pub(crate) fn skip_dollar(sql: &str, start: usize) -> Option<usize> {
    let delimiter_len = dollar_delimiter_len(sql, start)?;
    let delimiter = &sql[start..start + delimiter_len];
    let body = start + delimiter_len;
    sql[body..].find(delimiter).map(|at| body + at + delimiter.len())
}

pub(crate) fn is_ident_start(ch: char) -> bool {
    ch.is_alphabetic() || ch == '_'
}

pub(crate) fn is_ident_continue(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_' || ch == '$'
}

pub(crate) fn ident_continue(sql: &str, start: usize) -> usize {
    let mut index = start;
    let mut first = true;
    while index < sql.len() {
        let ch = sql[index..].chars().next().unwrap();
        let ok = if first { is_ident_start(ch) } else { is_ident_continue(ch) };
        if !ok {
            break;
        }
        first = false;
        index += ch.len_utf8();
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(key: &str, rank: u8, references: &[&str]) -> DepNode {
        DepNode { key: key.to_owned(), rank, references: references.iter().map(|item| (*item).to_owned()).collect() }
    }

    fn keys(items: Vec<DepNode>, dependents_first: bool) -> Vec<String> {
        order_dep_keys(items, dependents_first)
    }

    #[test]
    fn create_order_puts_referenced_support_objects_first_by_rank() {
        // domain 引用 enum，procedure 调用 function，function 又用到 domain：按秩分层的拓扑先出枚举。
        let items = vec![
            node("proc_a", 4, &["fn_b"]),
            node("fn_b", 3, &["dom_c"]),
            node("dom_c", 1, &["color"]),
            node("color", 0, &[]),
        ];
        assert_eq!(keys(items, false), vec!["color", "dom_c", "fn_b", "proc_a"]);
    }

    #[test]
    fn drop_order_reverses_to_dependents_first() {
        let items = vec![node("child", 0, &["parent"]), node("parent", 0, &[])];
        assert_eq!(keys(items.clone(), true), vec!["child", "parent"]);
        assert_eq!(keys(items, false), vec!["parent", "child"]);
    }

    #[test]
    fn equal_ready_nodes_come_out_in_rank_then_name_order() {
        let items = vec![node("zz_enum", 0, &[]), node("aa_proc", 4, &[]), node("mm_seq", 2, &[])];
        assert_eq!(keys(items, false), vec!["zz_enum", "mm_seq", "aa_proc"]);
    }

    #[test]
    fn cyclic_nodes_do_not_deadlock_and_go_last() {
        let items = vec![node("a", 0, &["b"]), node("b", 0, &["a"]), node("free", 0, &[])];
        assert_eq!(keys(items, false), vec!["free", "a", "b"]);
    }

    #[test]
    fn references_to_unknown_names_are_ignored() {
        let items = vec![node("a", 0, &["missing"]), node("b", 0, &["a"])];
        assert_eq!(keys(items, false), vec!["a", "b"]);
    }

    #[test]
    fn duplicate_keys_share_one_vertex() {
        // 同名重载函数是一条边上的同一顶点，输出只出现一次。
        let items = vec![node("fn", 3, &["enum_x"]), node("fn", 3, &[]), node("enum_x", 0, &[])];
        assert_eq!(keys(items, false), vec!["enum_x", "fn"]);
    }

    #[test]
    fn definition_references_scan_dollar_quoted_bodies_and_skip_self() {
        let mut names = BTreeSet::new();
        names.insert("helper".to_owned());
        names.insert("self_fn".to_owned());
        let definition = "CREATE FUNCTION self_fn() RETURNS int LANGUAGE plpgsql AS $$ BEGIN RETURN helper(); END $$";
        assert_eq!(definition_references("self_fn", definition, &names), vec!["helper".to_owned()]);
        assert!(definition_references("other", "SELECT 1", &names).is_empty());
    }

    #[test]
    fn referenced_relations_come_from_from_join_and_ignore_ctes() {
        let sql = "CREATE VIEW v AS WITH base AS (SELECT id FROM raw) SELECT b.id FROM base b JOIN customers c ON c.id = b.id";
        let mut found = referenced_relations(sql);
        found.sort();
        assert_eq!(found, vec!["customers", "raw"]);
        assert!(referenced_relations("SELECT 'customers' AS literal").is_empty());
    }

    #[test]
    fn calls_routine_requires_a_call_shape() {
        let definition = "CREATE VIEW v AS SELECT helper() AS value, helper_name FROM helper";
        assert!(calls_routine(definition, "helper"));
        assert!(!calls_routine(definition, "helper_name"));
    }

    #[test]
    fn statement_mentions_matches_quoted_names_exactly() {
        assert!(statement_mentions("ALTER TABLE \"Order\" DROP COLUMN id", "Order"));
        // 加引号的名字区分大小写：小写 order 不点名 \"Order\"。
        assert!(!statement_mentions("ALTER TABLE \"Order\" DROP COLUMN id", "order"));
        assert!(!statement_mentions("ALTER TABLE `Order` DROP COLUMN id", "order"));
        assert!(statement_mentions("ALTER TABLE `Order` DROP COLUMN id", "Order"));
    }
}
