//! PostgreSQL 的执行计划：`EXPLAIN (FORMAT JSON)` 的节点树，手写的文本格式按 `->` 缩进树认。

use serde_json::Value;

use crate::query::plan::{arrow_tree, cell, json_inline, number, split_stats, stat, Issue, PlanNode};

pub(super) fn parse_postgres(columns: &[String], rows: &[Vec<Option<String>>]) -> Option<Vec<PlanNode>> {
    let at = columns.iter().position(|column| column.eq_ignore_ascii_case("QUERY PLAN")).or(Some(0));
    let text: Vec<&str> = rows.iter().filter_map(|row| cell(row, at)).collect();
    let joined = text.join("\n");
    let trimmed = joined.trim_start();
    if trimmed.starts_with('[') || trimmed.starts_with('{') {
        let value: Value = serde_json::from_str(trimmed).ok()?;
        let plans: Vec<&Value> = match &value {
            Value::Array(items) => items.iter().filter_map(|item| item.get("Plan")).collect(),
            Value::Object(_) => value.get("Plan").into_iter().collect(),
            _ => Vec::new(),
        };
        return Some(plans.into_iter().map(pg_json_node).collect());
    }
    Some(arrow_tree(&joined, pg_text_node))
}

fn pg_json_node(value: &Value) -> PlanNode {
    let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
    let op = match (text("Node Type"), text("Join Type")) {
        (Some(kind), Some(join)) if kind.contains("Join") || kind == "Nested Loop" => format!("{kind} ({join})"),
        (Some(kind), _) => kind,
        _ => "Plan".into(),
    };
    let mut node = PlanNode::new(op.clone());
    node.object = match (text("Relation Name"), text("Alias"), text("Index Name")) {
        (Some(relation), Some(alias), _) if alias != relation => Some(format!("{relation} {alias}")),
        (Some(relation), _, _) => Some(relation),
        (None, _, Some(index)) => Some(index),
        _ => None,
    };
    let index = text("Index Name").filter(|_| text("Relation Name").is_some()).map(|index| format!("using {index}"));
    let condition = ["Index Cond", "Hash Cond", "Merge Cond", "Join Filter", "Filter", "Recheck Cond", "Sort Key", "Group Key"]
        .iter()
        .find_map(|key| value.get(*key).map(|item| format!("{key}: {}", json_inline(item))));
    node.detail = [index, condition].into_iter().flatten().collect::<Vec<_>>().join(" · ");
    node.cost = value.get("Total Cost").and_then(Value::as_f64);
    node.rows = value.get("Plan Rows").and_then(Value::as_f64);
    if let Some(map) = value.as_object() {
        for (key, item) in map {
            if key != "Plans" {
                node.props.push((key.clone(), json_inline(item)));
            }
        }
    }
    let filtered = value.get("Filter").is_some();
    if op == "Seq Scan" && (filtered || node.rows.unwrap_or(0.) >= 10_000.) {
        node.issues.push(Issue::FullScan);
    }
    if value.get("Sort Space Type").and_then(Value::as_str) == Some("Disk") {
        node.issues.push(Issue::TempSort);
    }
    node.children = value.get("Plans").and_then(Value::as_array).map(|plans| plans.iter().map(pg_json_node).collect()).unwrap_or_default();
    node
}

/// PostgreSQL 文本计划的一行：`Seq Scan on t  (cost=0.00..35.50 rows=2550 width=4)`。
fn pg_text_node(line: &str) -> PlanNode {
    let (head, stats) = split_stats(line);
    let mut node = PlanNode::new(head.split(" on ").next().unwrap_or(&head).trim().to_owned());
    if let Some((_, object)) = head.split_once(" on ") {
        let object = object.trim();
        let (object, index) = match object.split_once(" using ") {
            Some((table, index)) => (table.trim().to_owned(), format!("using {}", index.trim())),
            None => (object.to_owned(), String::new()),
        };
        node.object = Some(object);
        node.detail = index;
    }
    node.cost = stat(&stats, "cost").and_then(|cost| cost.rsplit("..").next().and_then(number));
    node.rows = stat(&stats, "rows").and_then(|rows| number(&rows));
    if node.op.starts_with("Using ") {
        // “Index Scan using idx on t” 的另一种写法：操作在前、索引在后。
        node.op = head.clone();
    }
    if !stats.is_empty() {
        node.props.push(("estimate".into(), stats));
    }
    if node.op == "Seq Scan" && node.rows.unwrap_or(0.) >= 10_000. {
        node.issues.push(Issue::FullScan);
    }
    node
}
