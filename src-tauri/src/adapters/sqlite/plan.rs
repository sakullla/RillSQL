//! SQLite 的执行计划：`EXPLAIN QUERY PLAN` 的 (id, parent, notused, detail) 行按 parent 建树。

use crate::query::plan::{assemble, cell, number, Issue, PlanNode};

/// 有 detail 与 parent 列就是计划行集。SQLite 在计划里写的是别名（SCAN o）：从语句里把别名对回表名，写成 “o（orders）”。
pub(super) fn parse_plan(sql: &str, columns: &[String], rows: &[Vec<Option<String>>]) -> Option<Vec<PlanNode>> {
    let column = |name: &str| columns.iter().position(|column| column.eq_ignore_ascii_case(name));
    if column("detail").is_none() || column("parent").is_none() {
        return None;
    }
    let mut roots = parse_sqlite(columns, rows)?;
    let aliases = alias_map(sql);
    fn rename(node: &mut PlanNode, aliases: &[(String, String)]) {
        if let Some(object) = node.object.as_mut() {
            if let Some((_, table)) = aliases.iter().find(|(alias, table)| alias.eq_ignore_ascii_case(object) && !table.eq_ignore_ascii_case(object)) {
                *object = format!("{object}（{table}）");
            }
        }
        for child in &mut node.children {
            rename(child, aliases);
        }
    }
    for root in &mut roots {
        rename(root, &aliases);
    }
    Some(roots)
}

/// 语句里的 “表 [AS] 别名”（FROM / JOIN 之后）。只认简单形态，认不出就不改名。
fn alias_map(sql: &str) -> Vec<(String, String)> {
    const STOP: [&str; 16] = ["WHERE", "ON", "JOIN", "INNER", "LEFT", "RIGHT", "FULL", "CROSS", "GROUP", "ORDER", "LIMIT", "USING", "NATURAL", "OUTER", "UNION", "HAVING"];
    let tokens: Vec<String> = sql
        .split(|ch: char| ch.is_whitespace() || ch == ',' || ch == '(' || ch == ')' || ch == ';')
        .filter(|token| !token.is_empty())
        .map(|token| token.trim_matches(['"', '`', '[', ']']).to_owned())
        .collect();
    let mut out = Vec::new();
    for index in 0..tokens.len() {
        let keyword = tokens[index].to_ascii_uppercase();
        if keyword != "FROM" && keyword != "JOIN" {
            continue;
        }
        let Some(table) = tokens.get(index + 1) else { continue };
        let mut next = index + 2;
        if tokens.get(next).is_some_and(|token| token.eq_ignore_ascii_case("AS")) {
            next += 1;
        }
        if let Some(alias) = tokens.get(next).filter(|alias| !STOP.contains(&alias.to_ascii_uppercase().as_str())) {
            let table = table.rsplit('.').next().unwrap_or(table).to_owned();
            out.push((alias.clone(), table));
        }
    }
    out
}

fn parse_sqlite(columns: &[String], rows: &[Vec<Option<String>>]) -> Option<Vec<PlanNode>> {
    let at = |name: &str| columns.iter().position(|column| column.eq_ignore_ascii_case(name));
    let (id_at, parent_at, detail_at) = (at("id"), at("parent"), at("detail"));
    let items = rows
        .iter()
        .filter_map(|row| {
            let id = number(cell(row, id_at)?)? as i64;
            let parent = number(cell(row, parent_at).unwrap_or("0")).unwrap_or(0.) as i64;
            let detail = cell(row, detail_at)?.trim().to_owned();
            Some((id, parent, sqlite_node(&detail)))
        })
        .collect();
    Some(assemble(items))
}

fn sqlite_node(detail: &str) -> PlanNode {
    let upper = detail.to_ascii_uppercase();
    let mut words = detail.split_whitespace();
    let head = words.next().unwrap_or("").to_owned();
    let mut node = PlanNode::new(head.clone());
    node.props.push(("detail".into(), detail.to_owned()));
    match head.to_ascii_uppercase().as_str() {
        "SCAN" | "SEARCH" => {
            // 旧版写 “SCAN TABLE t”，新版写 “SCAN t”。
            let mut object = words.next().unwrap_or("").to_owned();
            if object.eq_ignore_ascii_case("TABLE") {
                object = words.next().unwrap_or("").to_owned();
            }
            let rest = detail.split_once(&object).map(|(_, rest)| rest.trim()).unwrap_or("");
            node.object = (!object.is_empty()).then_some(object);
            node.detail = rest.to_owned();
            let uses_index = upper.contains("INDEX") || upper.contains("PRIMARY KEY");
            let virtual_scan = upper.contains("SUBQUERY") || upper.contains("CONSTANT ROW") || upper.contains("CO-ROUTINE") || upper.contains("VIRTUAL TABLE");
            if head.eq_ignore_ascii_case("SCAN") && !uses_index && !virtual_scan {
                node.issues.push(Issue::FullScan);
            }
        }
        "USE" if upper.contains("TEMP B-TREE") => {
            node.op = "TEMP B-TREE".into();
            node.detail = detail.split_once("FOR").map(|(_, rest)| format!("FOR{rest}")).unwrap_or_default();
            node.issues.push(Issue::TempSort);
        }
        _ => {
            node.op = detail.to_owned();
        }
    }
    node
}
