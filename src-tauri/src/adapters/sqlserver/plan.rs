//! SQL Server 的执行计划：`SET SHOWPLAN_ALL ON` 行集 (NodeId, Parent, PhysicalOp…)。

use crate::query::plan::{assemble, cell, number, Issue, PlanNode};

pub(super) fn parse_showplan_all(columns: &[String], rows: &[Vec<Option<String>>]) -> Option<Vec<PlanNode>> {
    let at = |name: &str| columns.iter().position(|column| column.eq_ignore_ascii_case(name));
    let (node_at, parent_at, physical_at, logical_at, argument_at) = (at("NodeId"), at("Parent"), at("PhysicalOp"), at("LogicalOp"), at("Argument"));
    let (rows_at, cost_at, warnings_at, type_at) = (at("EstimateRows"), at("TotalSubtreeCost"), at("Warnings"), at("Type"));
    let items = rows
        .iter()
        .filter(|row| cell(row, type_at).is_none_or(|kind| kind.eq_ignore_ascii_case("PLAN_ROW")))
        .filter_map(|row| {
            let id = number(cell(row, node_at)?)? as i64;
            let parent = number(cell(row, parent_at).unwrap_or("0")).unwrap_or(0.) as i64;
            let physical = cell(row, physical_at)?.trim().to_owned();
            let logical = cell(row, logical_at).unwrap_or("").trim().to_owned();
            let mut node = PlanNode::new(if logical.is_empty() || logical == physical { physical.clone() } else { format!("{physical} ({logical})") });
            let argument = cell(row, argument_at).unwrap_or("").to_owned();
            node.object = showplan_object(&argument);
            node.detail = argument.chars().take(160).collect();
            node.rows = cell(row, rows_at).and_then(number);
            node.cost = cell(row, cost_at).and_then(number);
            for (index, column) in columns.iter().enumerate() {
                if let Some(value) = cell(row, Some(index)).filter(|value| !value.is_empty()) {
                    node.props.push((column.clone(), value.to_owned()));
                }
            }
            if physical == "Table Scan" || physical == "Clustered Index Scan" || physical == "Index Scan" {
                node.issues.push(Issue::FullScan);
            }
            if physical == "Sort" {
                node.issues.push(Issue::TempSort);
            }
            if physical.contains("Spool") {
                node.issues.push(Issue::TempTable);
            }
            if cell(row, warnings_at).is_some_and(|warnings| !warnings.trim().is_empty()) {
                node.issues.push(Issue::EngineWarning);
            }
            Some((id, parent, node))
        })
        .collect();
    Some(assemble(items))
}

/// `OBJECT:([db].[dbo].[orders].[ix_customer] AS [o])` → `orders.ix_customer`（去掉库与架构）。
fn showplan_object(argument: &str) -> Option<String> {
    let start = argument.find("OBJECT:(")? + "OBJECT:(".len();
    let rest = &argument[start..];
    let end = rest.find(')').unwrap_or(rest.len());
    let parts: Vec<String> = rest[..end].split(" AS ").next()?.split('.').map(|part| part.trim().trim_matches(['[', ']']).to_owned()).filter(|part| !part.is_empty()).collect();
    match parts.len() {
        0 => None,
        1 | 2 => parts.last().cloned(),
        _ => Some(parts[2..].join(".")),
    }
}
