//! MySQL / MariaDB 的执行计划：MySQL 8 的 `FORMAT=TREE`、传统表格格式与 MariaDB 的 `FORMAT=JSON`。

use serde_json::Value;

use crate::query::plan::{arrow_tree, cell, json_inline, number, split_stats, stat, Issue, PlanNode};

pub(super) fn parse_mysql_family(columns: &[String], rows: &[Vec<Option<String>>]) -> Option<Vec<PlanNode>> {
    // 传统表格格式：每张表一行（id, select_type, table, type, key, rows, Extra…）。
    if columns.iter().any(|column| column.eq_ignore_ascii_case("select_type")) {
        return Some(mysql_table_rows(columns, rows));
    }
    let text: Vec<&str> = rows.iter().filter_map(|row| row.first().and_then(|value| value.as_deref())).collect();
    let joined = text.join("\n");
    let trimmed = joined.trim_start();
    if trimmed.starts_with('{') {
        let value: Value = serde_json::from_str(trimmed).ok()?;
        return Some(mariadb_json(&value));
    }
    if trimmed.starts_with("->") {
        return Some(arrow_tree(&joined, mysql_tree_node));
    }
    None
}

/// MySQL 8 的 FORMAT=TREE：`-> Table scan on t  (cost=0.35 rows=1)`。
fn mysql_tree_node(line: &str) -> PlanNode {
    let (head, stats) = split_stats(line);
    let mut node = PlanNode::new(head.clone());
    let lower = head.to_ascii_lowercase();
    for marker in [" on ", " using "] {
        if let Some((op, rest)) = head.split_once(marker) {
            if marker == " on " {
                node.op = op.trim().to_owned();
                let object = rest.split_whitespace().next().unwrap_or("").to_owned();
                node.detail = rest.trim()[object.len()..].trim().to_owned();
                node.object = (!object.is_empty()).then_some(object);
                break;
            }
        }
    }
    node.cost = stat(&stats, "cost").and_then(|cost| cost.rsplit("..").next().and_then(number));
    node.rows = stat(&stats, "rows").and_then(|rows| number(&rows));
    if !stats.is_empty() {
        node.props.push(("estimate".into(), stats));
    }
    if lower.starts_with("table scan on") {
        node.issues.push(Issue::FullScan);
    }
    if lower.starts_with("sort:") || lower.contains("filesort") {
        node.issues.push(Issue::TempSort);
    }
    if lower.contains("temporary table") || lower.starts_with("materialize") {
        node.issues.push(Issue::TempTable);
    }
    node
}

fn mysql_table_rows(columns: &[String], rows: &[Vec<Option<String>>]) -> Vec<PlanNode> {
    let at = |name: &str| columns.iter().position(|column| column.eq_ignore_ascii_case(name));
    rows.iter()
        .map(|row| {
            let access = cell(row, at("type")).unwrap_or("").to_owned();
            let mut node = PlanNode::new(if access.is_empty() { cell(row, at("select_type")).unwrap_or("SELECT").to_owned() } else { access.clone() });
            node.object = cell(row, at("table")).map(str::to_owned);
            let key = cell(row, at("key")).map(|key| format!("key {key}"));
            let extra = cell(row, at("Extra")).map(str::to_owned);
            node.detail = [key, extra.clone()].into_iter().flatten().collect::<Vec<_>>().join(" · ");
            node.rows = cell(row, at("rows")).and_then(number);
            for (index, column) in columns.iter().enumerate() {
                if let Some(value) = cell(row, Some(index)) {
                    node.props.push((column.clone(), value.to_owned()));
                }
            }
            if access.eq_ignore_ascii_case("ALL") {
                node.issues.push(Issue::FullScan);
            }
            let extra = extra.unwrap_or_default().to_ascii_lowercase();
            if extra.contains("filesort") {
                node.issues.push(Issue::TempSort);
            }
            if extra.contains("temporary") {
                node.issues.push(Issue::TempTable);
            }
            node
        })
        .collect()
}

/// MariaDB 的 FORMAT=JSON：query_block 里嵌着 table / nested_loop / filesort / temporary_table。
fn mariadb_json(value: &Value) -> Vec<PlanNode> {
    fn walk(key: &str, value: &Value) -> Vec<PlanNode> {
        match value {
            Value::Array(items) => items.iter().flat_map(|item| walk(key, item)).collect(),
            Value::Object(map) => {
                let mut node = match key {
                    "query_block" => PlanNode::new(format!("Query block #{}", map.get("select_id").map(json_inline).unwrap_or_default())),
                    "table" => {
                        let access = map.get("access_type").map(json_inline).unwrap_or_default();
                        let mut node = PlanNode::new(if access.is_empty() { "table".to_owned() } else { access.clone() });
                        node.object = map.get("table_name").map(json_inline);
                        let key = map.get("key").map(|key| format!("key {}", json_inline(key)));
                        let condition = map.get("attached_condition").map(json_inline);
                        node.detail = [key, condition].into_iter().flatten().collect::<Vec<_>>().join(" · ");
                        node.rows = map.get("rows").and_then(Value::as_f64);
                        if access == "ALL" {
                            node.issues.push(Issue::FullScan);
                        }
                        node
                    }
                    "filesort" => {
                        let mut node = PlanNode::new("filesort");
                        node.detail = map.get("sort_key").map(json_inline).unwrap_or_default();
                        node.issues.push(Issue::TempSort);
                        node
                    }
                    "temporary_table" => {
                        let mut node = PlanNode::new("temporary table");
                        node.issues.push(Issue::TempTable);
                        node
                    }
                    "nested_loop" => PlanNode::new("nested loop"),
                    other => PlanNode::new(other),
                };
                node.cost = map.get("cost").and_then(Value::as_f64).or_else(|| map.get("query_cost").and_then(Value::as_f64));
                for (child_key, child) in map {
                    match child {
                        Value::Object(_) | Value::Array(_) if child_key != "used_key_parts" && child_key != "possible_keys" && child_key != "ref" => {
                            node.children.extend(walk(child_key, child));
                        }
                        Value::Object(_) | Value::Array(_) => node.props.push((child_key.clone(), json_inline(child))),
                        scalar => node.props.push((child_key.clone(), json_inline(scalar))),
                    }
                }
                // nested_loop 自己只是容器：直接用它的子节点，层级更浅。
                if key == "nested_loop" {
                    return std::mem::take(&mut node.children);
                }
                vec![node]
            }
            _ => Vec::new(),
        }
    }
    match value.get("query_block") {
        Some(block) => walk("query_block", block),
        None => walk("plan", value),
    }
}
