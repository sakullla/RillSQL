//! MySQL / MariaDB 的执行计划：MySQL 8 的 `FORMAT=TREE` / `EXPLAIN ANALYZE` / `FORMAT=JSON`、
//! 传统表格格式与 MariaDB 的 `EXPLAIN FORMAT=JSON` / `ANALYZE FORMAT=JSON`。

use serde_json::{Map, Value};

use crate::query::plan::{arrow_tree, cell, json_inline, number, stat, Issue, PlanNode};

pub(super) fn parse_mysql_family(columns: &[String], rows: &[Vec<Option<String>>]) -> Option<Vec<PlanNode>> {
    // 传统表格格式：每张表一行（id, select_type, table, type, key, rows, filtered, Extra…）。
    if columns.iter().any(|column| column.eq_ignore_ascii_case("select_type")) {
        return Some(mysql_table_rows(columns, rows));
    }
    let text: Vec<&str> = rows.iter().filter_map(|row| row.first().and_then(|value| value.as_deref())).collect();
    let joined = text.join("\n");
    let trimmed = joined.trim_start();
    if trimmed.starts_with('{') {
        let value: Value = serde_json::from_str(trimmed).ok()?;
        return Some(mysql_json(&value));
    }
    if trimmed.starts_with("->") {
        return Some(arrow_tree(&joined, mysql_tree_node));
    }
    None
}

// ---------------------------------------------------------------- MySQL 8 FORMAT=TREE / EXPLAIN ANALYZE

/// 拆出 MySQL TREE / EXPLAIN ANALYZE 行尾的统计括号组：
/// `Table scan on o  (cost=500.25 rows=5000) (actual time=0.03..4.56 rows=5000 loops=1)`
fn split_mysql_tree_stats(line: &str) -> (String, Vec<String>) {
    let trimmed = line.trim();
    let mut split_at = None;
    for (idx, _) in trimmed.match_indices("  (") {
        let rest = &trimmed[idx + 3..];
        let lower = rest.to_ascii_lowercase();
        if lower.starts_with("cost=") || lower.starts_with("actual ") || lower.starts_with("never executed") || lower.starts_with("rows=") {
            split_at = Some(idx);
            break;
        }
    }
    let Some(idx) = split_at else {
        return (trimmed.to_owned(), Vec::new());
    };
    let head = trimmed[..idx].trim().to_owned();
    let tail = trimmed[idx + 2..].trim();
    let mut groups = Vec::new();
    let mut depth = 0usize;
    let mut start = None;
    for (pos, ch) in tail.char_indices() {
        match ch {
            '(' => {
                if depth == 0 {
                    start = Some(pos + 1);
                }
                depth += 1;
            }
            ')' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    if let Some(s) = start.take() {
                        let content = tail[s..pos].trim();
                        if !content.is_empty() {
                            groups.push(content.to_owned());
                        }
                    }
                }
            }
            _ => {}
        }
    }
    (head, groups)
}

/// MySQL 8 的 FORMAT=TREE 与 EXPLAIN ANALYZE。
fn mysql_tree_node(line: &str) -> PlanNode {
    let (head, groups) = split_mysql_tree_stats(line);
    let mut node = PlanNode::new(head.clone());
    let lower = head.to_ascii_lowercase();

    if let Some((op, rest)) = head.split_once(" on ") {
        node.op = op.trim().to_owned();
        let object = rest.split_whitespace().next().unwrap_or("").to_owned();
        node.detail = rest.trim()[object.len()..].trim().to_owned();
        node.object = (!object.is_empty()).then_some(object);
    }

    if lower.contains("covering index") {
        node.covering_index = true;
    }

    if let Some((_, after_using)) = head.split_once(" using ") {
        if let Some(token) = after_using.split_whitespace().next() {
            let key = token.trim_matches(|ch: char| matches!(ch, '(' | ')' | ',' | ';'));
            if !key.is_empty() && !key.eq_ignore_ascii_case("temporary") && !key.eq_ignore_ascii_case("filesort") {
                node.key_used = Some(key.to_owned());
            }
        }
    }

    node.access_type = infer_tree_access_type(&lower).map(str::to_owned);

    for group in groups {
        let g_lower = group.to_ascii_lowercase();
        if g_lower == "never executed" {
            node.loops = Some(0);
            node.actual_rows = Some(0.0);
            node.actual_time_ms = Some(0.0);
            node.props.push(("actual".into(), group));
        } else if g_lower.starts_with("actual ") {
            if let Some(time) = stat(&group, "time").and_then(|t| t.rsplit("..").next().and_then(number)) {
                node.actual_time_ms = Some(time);
            }
            if let Some(rows) = stat(&group, "rows").and_then(|r| number(&r)) {
                node.actual_rows = Some(rows);
            }
            if let Some(loops) = stat(&group, "loops").and_then(|l| l.trim().parse::<u64>().ok()) {
                node.loops = Some(loops);
            }
            node.props.push(("actual".into(), group));
        } else {
            if let Some(cost) = stat(&group, "cost").and_then(|c| c.rsplit("..").next().and_then(number)) {
                node.cost = Some(cost);
            }
            if let Some(rows) = stat(&group, "rows").and_then(|r| number(&r)) {
                node.rows = Some(rows);
            }
            node.props.push(("estimate".into(), group));
        }
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

fn infer_tree_access_type(lower: &str) -> Option<&'static str> {
    if lower.starts_with("table scan") {
        Some("ALL")
    } else if lower.starts_with("single-row index lookup") {
        Some("eq_ref")
    } else if lower.starts_with("constant") || lower.starts_with("zero rows") {
        Some("const")
    } else if lower.starts_with("index range scan") || lower.starts_with("covering index range scan") {
        Some("range")
    } else if lower.starts_with("index lookup") || lower.starts_with("covering index lookup") {
        Some("ref")
    } else if lower.starts_with("index scan") || lower.starts_with("covering index scan") {
        Some("index")
    } else if lower.starts_with("full-text index") {
        Some("fulltext")
    } else {
        None
    }
}

// ---------------------------------------------------------------- 传统表格 EXPLAIN

struct TableRow {
    id: String,
    select_type: String,
    node: PlanNode,
}

fn mysql_table_rows(columns: &[String], rows: &[Vec<Option<String>>]) -> Vec<PlanNode> {
    let at = |name: &str| columns.iter().position(|column| column.eq_ignore_ascii_case(name));
    let parsed: Vec<TableRow> = rows
        .iter()
        .map(|row| {
            let id = cell(row, at("id")).unwrap_or("").trim().to_owned();
            let select_type = cell(row, at("select_type")).unwrap_or("SIMPLE").trim().to_owned();
            let access = cell(row, at("type")).unwrap_or("").trim().to_owned();
            let mut node = PlanNode::new(if access.is_empty() { select_type.clone() } else { access.clone() });
            node.access_type = (!access.is_empty()).then_some(access.clone());
            node.object = cell(row, at("table")).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
            let key_raw = cell(row, at("key")).map(str::trim).filter(|s| !s.is_empty());
            node.key_used = key_raw.map(str::to_owned);
            let key = key_raw.map(|k| format!("key {k}"));
            let extra_raw = cell(row, at("Extra")).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
            node.detail = [key, extra_raw.clone()].into_iter().flatten().collect::<Vec<_>>().join(" · ");
            node.rows = cell(row, at("rows")).and_then(number);
            node.filtered = cell(row, at("filtered")).and_then(number);
            for (index, column) in columns.iter().enumerate() {
                if let Some(value) = cell(row, Some(index)) {
                    node.props.push((column.clone(), value.to_owned()));
                }
            }
            if access.eq_ignore_ascii_case("ALL") {
                node.issues.push(Issue::FullScan);
            }
            if let Some(extra) = &extra_raw {
                let extra_lower = extra.to_ascii_lowercase();
                if extra_lower.contains("filesort") {
                    node.issues.push(Issue::TempSort);
                }
                if extra_lower.contains("temporary") {
                    node.issues.push(Issue::TempTable);
                }
                if extra.split(';').any(|part| part.trim().eq_ignore_ascii_case("using index")) {
                    node.covering_index = true;
                }
            }
            TableRow { id, select_type, node }
        })
        .collect();

    // 按 (id, select_type) 分组；只有单块（例如单 id 的 SIMPLE）时保持扁平列表。
    let mut blocks: Vec<(String, String, Vec<PlanNode>)> = Vec::new();
    for item in parsed {
        if let Some(existing) = blocks.iter_mut().find(|(id, st, _)| *id == item.id && *st == item.select_type) {
            existing.2.push(item.node);
        } else {
            blocks.push((item.id, item.select_type, vec![item.node]));
        }
    }

    if blocks.len() <= 1 {
        return blocks.into_iter().flat_map(|(_, _, nodes)| nodes).collect();
    }

    // 多块执行计划（PRIMARY / DERIVED / SUBQUERY / UNION / UNION RESULT）构造成清晰的层次树。
    struct BlockNode {
        id: String,
        select_type: String,
        node: PlanNode,
    }

    let mut block_nodes: Vec<BlockNode> = blocks
        .into_iter()
        .map(|(id, select_type, mut children)| {
            let st_upper = select_type.to_ascii_uppercase();
            let node = if st_upper == "UNION RESULT" && children.len() == 1 {
                let mut single = children.remove(0);
                single.op = select_type.clone();
                single
            } else {
                let op = if id.is_empty() { select_type.clone() } else { format!("{select_type} #{id}") };
                let mut block = PlanNode::new(op);
                block.children = children;
                block
            };
            BlockNode { id, select_type: st_upper, node }
        })
        .collect();

    // 把 DERIVED / MATERIALIZED / SUBQUERY 挂到引用它的父表节点或 PRIMARY 块下；UNION RESULT 作为总根。
    let union_result_idx = block_nodes.iter().position(|b| b.select_type == "UNION RESULT");
    let primary_idx = block_nodes.iter().position(|b| b.select_type == "PRIMARY").unwrap_or(0);

    // 先处理能精确匹配 `<derivedN>` / `<subqueryN>` 的块（倒序摘除，避免索引偏移）。
    let mut deferred_children: Vec<(usize, PlanNode)> = Vec::new();
    let mut idx = block_nodes.len();
    while idx > 0 {
        idx -= 1;
        if Some(idx) == union_result_idx || idx == primary_idx {
            continue;
        }
        let st = block_nodes[idx].select_type.as_str();
        let is_sub = st.contains("DERIVED") || st.contains("SUBQUERY") || st == "MATERIALIZED";
        if !is_sub {
            continue;
        }
        let block = block_nodes.remove(idx);
        let derived_tag = format!("<derived{}>", block.id);
        let subq_tag = format!("<subquery{}>", block.id);
        let mut attached = false;
        for candidate in &mut block_nodes {
            if attach_to_table_ref(&mut candidate.node, &derived_tag, &subq_tag, &block.node) {
                attached = true;
                break;
            }
        }
        if !attached {
            // 挂到 PRIMARY 块（若索引变动则动态查找）。
            let target = block_nodes.iter().position(|b| b.select_type == "PRIMARY").unwrap_or(0);
            deferred_children.push((target, block.node));
        }
    }
    // deferred_children 是倒序收集的，反转回来保持原始出现顺序。
    for (target, child) in deferred_children.into_iter().rev() {
        if let Some(parent) = block_nodes.get_mut(target) {
            parent.node.children.push(child);
        }
    }

    if let Some(ur_pos) = block_nodes.iter().position(|b| b.select_type == "UNION RESULT") {
        let mut union_root = block_nodes.remove(ur_pos).node;
        for other in block_nodes {
            union_root.children.push(other.node);
        }
        return vec![union_root];
    }

    block_nodes.into_iter().map(|b| b.node).collect()
}

fn attach_to_table_ref(root: &mut PlanNode, derived_tag: &str, subq_tag: &str, child: &PlanNode) -> bool {
    if root.object.as_deref().is_some_and(|obj| obj.eq_ignore_ascii_case(derived_tag) || obj.eq_ignore_ascii_case(subq_tag)) {
        root.children.push(child.clone());
        return true;
    }
    for sub in &mut root.children {
        if attach_to_table_ref(sub, derived_tag, subq_tag, child) {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------- MySQL 8 & MariaDB FORMAT=JSON / ANALYZE FORMAT=JSON

fn json_f64(value: &Value) -> Option<f64> {
    value.as_f64().or_else(|| value.as_str().and_then(number))
}

fn json_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_f64().filter(|n| n.is_finite() && *n >= 0.0).map(|n| n.round() as u64))
        .or_else(|| value.as_str().and_then(|s| s.trim().parse::<u64>().ok()))
}

fn is_scalar_prop_key(key: &str) -> bool {
    matches!(
        key,
        "used_key_parts" | "possible_keys" | "ref" | "used_columns" | "partitions" | "sort_key" | "key_length" | "message"
    )
}

fn apply_cost_info(map: &Map<String, Value>, node: &mut PlanNode) {
    let Some(Value::Object(ci)) = map.get("cost_info") else {
        return;
    };
    if node.cost.is_none() {
        node.cost = ci
            .get("query_cost")
            .and_then(json_f64)
            .or_else(|| ci.get("prefix_cost").and_then(json_f64))
            .or_else(|| ci.get("sort_cost").and_then(json_f64))
            .or_else(|| match (ci.get("read_cost").and_then(json_f64), ci.get("eval_cost").and_then(json_f64)) {
                (Some(read), Some(eval)) => Some(read + eval),
                (Some(read), None) => Some(read),
                (None, Some(eval)) => Some(eval),
                (None, None) => None,
            });
    }
    for (k, v) in ci {
        node.props.push((format!("cost_info.{k}"), json_inline(v)));
    }
}

fn apply_mariadb_analyze_metrics(map: &Map<String, Value>, node: &mut PlanNode) {
    if node.actual_time_ms.is_none() {
        node.actual_time_ms = map.get("r_total_time_ms").and_then(json_f64);
    }
    if node.actual_rows.is_none() {
        node.actual_rows = map.get("r_rows").and_then(json_f64).or_else(|| map.get("r_output_rows").and_then(json_f64));
    }
    if node.loops.is_none() {
        node.loops = map.get("r_loops").and_then(json_u64);
    }
    if node.filtered.is_none() {
        node.filtered = map.get("r_filtered").and_then(json_f64).or_else(|| map.get("filtered").and_then(json_f64));
    }
}

fn push_issue_once(node: &mut PlanNode, issue: Issue) {
    if !node.issues.contains(&issue) {
        node.issues.push(issue);
    }
}

/// MySQL 8 与 MariaDB 的 `EXPLAIN FORMAT=JSON` / `ANALYZE FORMAT=JSON`。
fn mysql_json(value: &Value) -> Vec<PlanNode> {
    fn walk(key: &str, value: &Value) -> Vec<PlanNode> {
        match value {
            Value::Array(items) => items.iter().flat_map(|item| walk(key, item)).collect(),
            Value::Object(map) => {
                // attached_subqueries / query_specifications 的每一项都是 { dependent, cacheable, query_block } 包装。
                if (key == "attached_subqueries" || key == "query_specifications") && map.contains_key("query_block") {
                    let mut out = walk("query_block", &map["query_block"]);
                    if let Some(first) = out.first_mut() {
                        for (meta_k, meta_v) in map {
                            if meta_k != "query_block" {
                                first.props.push((meta_k.clone(), json_inline(meta_v)));
                            }
                        }
                    }
                    return out;
                }

                let mut node = match key {
                    "query_block" => PlanNode::new(format!("Query block #{}", map.get("select_id").map(json_inline).unwrap_or_default())),
                    "table" => {
                        let access = map.get("access_type").map(json_inline).unwrap_or_default();
                        let mut node = PlanNode::new(if access.is_empty() { "table".to_owned() } else { access.clone() });
                        node.access_type = (!access.is_empty()).then_some(access.clone());
                        node.object = map.get("table_name").map(json_inline);
                        let key_raw = map.get("key").map(json_inline).filter(|k| !k.is_empty() && k != "null");
                        node.key_used = key_raw.clone();
                        let key_part = key_raw.map(|k| format!("key {k}"));
                        let condition = map.get("attached_condition").map(json_inline);
                        node.detail = [key_part, condition].into_iter().flatten().collect::<Vec<_>>().join(" · ");
                        node.rows = map
                            .get("rows_examined_per_scan")
                            .and_then(json_f64)
                            .or_else(|| map.get("rows").and_then(json_f64))
                            .or_else(|| map.get("rows_produced_per_join").and_then(json_f64));
                        node.covering_index = map.get("using_index").and_then(Value::as_bool).unwrap_or(false);
                        if access.eq_ignore_ascii_case("ALL") {
                            push_issue_once(&mut node, Issue::FullScan);
                        }
                        node
                    }
                    "ordering_operation" => PlanNode::new("ordering operation"),
                    "grouping_operation" => PlanNode::new("grouping operation"),
                    "duplicates_removal" => PlanNode::new("duplicates removal"),
                    "buffer_result" => PlanNode::new("buffer result"),
                    "union_result" => {
                        let mut node = PlanNode::new("union result");
                        node.object = map.get("table_name").map(json_inline);
                        let access = map.get("access_type").map(json_inline).unwrap_or_default();
                        node.access_type = (!access.is_empty()).then_some(access);
                        node
                    }
                    "materialized_from_subquery" => PlanNode::new("materialized subquery"),
                    "filesort" => {
                        let mut node = PlanNode::new("filesort");
                        node.detail = map.get("sort_key").map(json_inline).unwrap_or_default();
                        push_issue_once(&mut node, Issue::TempSort);
                        node
                    }
                    "temporary_table" => {
                        let mut node = PlanNode::new("temporary table");
                        push_issue_once(&mut node, Issue::TempTable);
                        node
                    }
                    "read_sorted_file" => PlanNode::new("read sorted file"),
                    "table_scan" => PlanNode::new("table scan"),
                    "nested_loop" | "attached_subqueries" | "query_specifications" => PlanNode::new(key),
                    other => PlanNode::new(other),
                };

                if map.get("using_filesort").and_then(Value::as_bool) == Some(true) {
                    push_issue_once(&mut node, Issue::TempSort);
                }
                if map.get("using_temporary_table").and_then(Value::as_bool) == Some(true) {
                    push_issue_once(&mut node, Issue::TempTable);
                }

                node.cost = map.get("cost").and_then(json_f64).or_else(|| map.get("query_cost").and_then(json_f64));
                apply_cost_info(map, &mut node);
                apply_mariadb_analyze_metrics(map, &mut node);

                for (child_key, child) in map {
                    if child_key == "cost_info" {
                        continue;
                    }
                    match child {
                        Value::Object(_) | Value::Array(_) if !is_scalar_prop_key(child_key) => {
                            node.children.extend(walk(child_key, child));
                        }
                        Value::Object(_) | Value::Array(_) => node.props.push((child_key.clone(), json_inline(child))),
                        scalar => node.props.push((child_key.clone(), json_inline(scalar))),
                    }
                }

                // nested_loop 等容器节点不单独占一层，直接返回其子节点。
                if matches!(key, "nested_loop" | "attached_subqueries" | "query_specifications") {
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
