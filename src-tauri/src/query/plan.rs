//! 执行计划：把各引擎 EXPLAIN 的结果解析成同一棵计划树，供结果区画成可读的计划视图。
//!
//! | 引擎 | 来源 | 结构 |
//! | --- | --- | --- |
//! | SQLite | `EXPLAIN QUERY PLAN` | (id, parent, notused, detail) 行，按 parent 建树 |
//! | PostgreSQL | `EXPLAIN (FORMAT JSON)`；手写的文本格式也认 | JSON 节点树 / `->` 缩进树 |
//! | MySQL | `EXPLAIN FORMAT=TREE`；手写的表格格式也认 | `->` 缩进树 / 每表一行 |
//! | MariaDB | `EXPLAIN FORMAT=JSON` | query_block 嵌套对象 |
//! | SQL Server | `SET SHOWPLAN_ALL ON` 行集 | (NodeId, Parent, PhysicalOp…) 行 |
//!
//! 解析只做“看得懂”：操作名、对象、预估代价与行数、原始属性，以及几类常见问题
//! （全表扫描、临时排序、临时表）。认不出的格式返回 None，界面退回原始结果网格。

use serde_json::Value;

/// 计划里值得提醒的问题。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Issue {
    /// 整表扫描：过滤/连接条件没有用上索引。
    FullScan,
    /// 为排序/分组/去重额外建了临时结构（SQLite temp B-tree、MySQL filesort、外部排序）。
    TempSort,
    /// 物化了临时表。
    TempTable,
    /// 引擎自己报出的警告（SQL Server 的 Warnings 列）。
    EngineWarning,
}

impl Issue {
    pub fn label_key(self) -> &'static str {
        match self {
            Issue::FullScan => "query.plan.issue.fullScan",
            Issue::TempSort => "query.plan.issue.tempSort",
            Issue::TempTable => "query.plan.issue.tempTable",
            Issue::EngineWarning => "query.plan.issue.warning",
        }
    }

    pub fn hint_key(self) -> &'static str {
        match self {
            Issue::FullScan => "query.plan.hint.fullScan",
            Issue::TempSort => "query.plan.hint.tempSort",
            Issue::TempTable => "query.plan.hint.tempTable",
            Issue::EngineWarning => "query.plan.hint.warning",
        }
    }
}

/// 操作的大类：决定图标与颜色，不影响内容。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpKind {
    Scan,
    Seek,
    Join,
    Sort,
    Aggregate,
    Other,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlanNode {
    /// 操作名（引擎原文，如 Seq Scan / SEARCH / Table scan on）。
    pub op: String,
    /// 作用对象：表、别名或索引。
    pub object: Option<String>,
    /// 一行补充说明：用到的索引、条件等。
    pub detail: String,
    /// 预估代价（引擎自己的单位；SQLite 没有）。
    pub cost: Option<f64>,
    /// 预估行数。
    pub rows: Option<f64>,
    /// 引擎给出的全部原始属性，详情面板逐条列出。
    pub props: Vec<(String, String)>,
    pub issues: Vec<Issue>,
    pub children: Vec<PlanNode>,
}

impl PlanNode {
    pub(crate) fn new(op: impl Into<String>) -> Self {
        Self { op: op.into(), ..Self::default() }
    }

    pub fn kind(&self) -> OpKind {
        let op = self.op.to_ascii_lowercase();
        if op.contains("join") || op.contains("nested loop") || op.contains("merge") && !op.contains("append") {
            OpKind::Join
        } else if op.contains("sort") || op.contains("temp b-tree") || op.contains("filesort") {
            OpKind::Sort
        } else if op.contains("aggregate") || op.contains("group") || op.contains("hash match") {
            OpKind::Aggregate
        } else if op.contains("search") || op.contains("seek") || op.contains("lookup") || op.contains("index scan") || op.contains("index only") || op.contains("ref") || op.contains("eq_ref") || op.contains("const") {
            OpKind::Seek
        } else if op.contains("scan") || op == "all" {
            OpKind::Scan
        } else {
            OpKind::Other
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub engine: String,
    pub roots: Vec<PlanNode>,
}

/// 计划树的一行（展平后）：深度、节点、是否还有子节点。
pub struct PlanLine<'a> {
    pub depth: usize,
    pub node: &'a PlanNode,
}

impl Plan {
    /// 前序展平：界面按这个顺序画缩进树，详情面板按下标取节点。
    pub fn lines(&self) -> Vec<PlanLine<'_>> {
        fn walk<'a>(node: &'a PlanNode, depth: usize, out: &mut Vec<PlanLine<'a>>) {
            out.push(PlanLine { depth, node });
            for child in &node.children {
                walk(child, depth + 1, out);
            }
        }
        let mut out = Vec::new();
        for root in &self.roots {
            walk(root, 0, &mut out);
        }
        out
    }

    /// 整个计划的代价：根节点代价之和（代价是累计值，子节点已含在父节点里）。
    pub fn total_cost(&self) -> Option<f64> {
        let costs: Vec<f64> = self.roots.iter().filter_map(|node| node.cost).collect();
        (!costs.is_empty()).then(|| costs.iter().sum())
    }

    /// 最大的单节点代价：代价条按它归一。
    pub fn max_cost(&self) -> Option<f64> {
        self.lines().iter().filter_map(|line| line.node.cost).fold(None, |best: Option<f64>, cost| Some(best.map_or(cost, |best| best.max(cost))))
    }

    /// 根节点的预估行数（结果大概有多少行）。
    pub fn result_rows(&self) -> Option<f64> {
        self.roots.first().and_then(|root| root.rows)
    }

    /// 全部问题：（问题, 所在节点的对象）。同一对象同一问题只列一次。
    pub fn findings(&self) -> Vec<(Issue, Option<String>)> {
        let mut out: Vec<(Issue, Option<String>)> = Vec::new();
        for line in self.lines() {
            for issue in &line.node.issues {
                let entry = (*issue, line.node.object.clone());
                if !out.contains(&entry) {
                    out.push(entry);
                }
            }
        }
        out
    }
}

/// 这条语句的结果是不是执行计划；是就解析，不是或认不出返回 None。各引擎的结果形态由适配器认。
pub fn parse(engine: &str, sql: &str, columns: &[String], rows: &[Vec<Option<String>>]) -> Option<Plan> {
    let upper = sql.trim_start().to_ascii_uppercase();
    let explained = upper.starts_with("EXPLAIN") || upper.starts_with("SET SHOWPLAN");
    let roots = crate::adapters::query::syntax_named(engine)?.parse_plan(sql, explained, columns, rows)?;
    let plan = Plan { engine: engine.to_owned(), roots };
    if plan.roots.is_empty() {
        return None;
    }
    Some(plan)
}

pub(crate) fn cell(row: &[Option<String>], index: Option<usize>) -> Option<&str> {
    index.and_then(|index| row.get(index)).and_then(|value| value.as_deref())
}

pub(crate) fn number(text: &str) -> Option<f64> {
    text.trim().parse::<f64>().ok().filter(|value| value.is_finite())
}

/// 多个节点挂成树：(id, parent, node)，parent 不在集合里的就是根。
pub(crate) fn assemble(mut items: Vec<(i64, i64, PlanNode)>) -> Vec<PlanNode> {
    fn take_children(parent: i64, items: &mut Vec<(i64, i64, PlanNode)>, depth: usize) -> Vec<PlanNode> {
        if depth > 64 {
            return Vec::new();
        }
        let mut children = Vec::new();
        let mut index = 0;
        while index < items.len() {
            if items[index].1 == parent && items[index].0 != parent {
                let (id, _, mut node) = items.remove(index);
                node.children = take_children(id, items, depth + 1);
                children.push(node);
            } else {
                index += 1;
            }
        }
        children
    }
    let ids: Vec<i64> = items.iter().map(|(id, _, _)| *id).collect();
    let mut roots = Vec::new();
    let mut index = 0;
    while index < items.len() {
        if !ids.contains(&items[index].1) || items[index].1 == items[index].0 {
            let (id, _, mut node) = items.remove(index);
            node.children = take_children(id, &mut items, 0);
            roots.push(node);
        } else {
            index += 1;
        }
    }
    // 成环或断链的剩余节点也不能丢：按根挂出来。
    roots.extend(items.into_iter().map(|(_, _, node)| node));
    roots
}

pub(crate) fn json_inline(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(items) => items.iter().map(json_inline).collect::<Vec<_>>().join(", "),
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------- 共用：`->` 缩进树

/// PostgreSQL 文本计划与 MySQL TREE 共用：`->` 开头、按缩进分层；不带 `->` 的行是上一节点的属性。
pub(crate) fn arrow_tree(text: &str, node_of: fn(&str) -> PlanNode) -> Vec<PlanNode> {
    let mut flat: Vec<(usize, PlanNode)> = Vec::new();
    for raw in text.lines() {
        if raw.trim().is_empty() {
            continue;
        }
        let indent = raw.len() - raw.trim_start().len();
        let trimmed = raw.trim();
        if let Some(body) = trimmed.strip_prefix("->") {
            flat.push((indent, node_of(body.trim())));
        } else if flat.is_empty() {
            // 第一行没有箭头（PostgreSQL 的根节点）。
            flat.push((indent.saturating_sub(1), node_of(trimmed)));
        } else if let Some((_, node)) = flat.last_mut() {
            // “Filter: …”“Sort Key: …” 这类属性行挂到上一个节点上。
            match trimmed.split_once(": ") {
                Some((key, value)) => {
                    if node.detail.is_empty() {
                        node.detail = trimmed.to_owned();
                    }
                    if key == "Filter" && node.op == "Seq Scan" && !node.issues.contains(&Issue::FullScan) {
                        node.issues.push(Issue::FullScan);
                    }
                    if key == "Sort Method" && value.contains("external") {
                        node.issues.push(Issue::TempSort);
                    }
                    node.props.push((key.to_owned(), value.to_owned()));
                }
                None => node.props.push(("note".into(), trimmed.to_owned())),
            }
        }
    }
    fn build(flat: &[(usize, PlanNode)], start: usize) -> (Vec<PlanNode>, usize) {
        let mut out = Vec::new();
        let mut index = start;
        let level = flat.get(start).map(|item| item.0).unwrap_or(0);
        while index < flat.len() && flat[index].0 >= level {
            if flat[index].0 > level {
                let (children, next) = build(flat, index);
                if let Some(parent) = out.last_mut() {
                    let parent: &mut PlanNode = parent;
                    parent.children.extend(children);
                }
                index = next;
                continue;
            }
            out.push(flat[index].1.clone());
            index += 1;
        }
        (out, index)
    }
    build(&flat, 0).0
}

/// `head  (cost=… rows=… width=…)` → (head, 括号里的统计)；没有统计时第二项为空。
pub(crate) fn split_stats(line: &str) -> (String, String) {
    let line = line.trim();
    if let Some(open) = line.rfind("  (") {
        let stats = line[open + 3..].trim_end_matches(')').to_owned();
        if stats.contains('=') {
            return (line[..open].trim().to_owned(), stats);
        }
    }
    (line.to_owned(), String::new())
}

/// 在 “cost=0.35 rows=1 width=4” 里取一个键的值（遇到第一个括号或空白结束）。
pub(crate) fn stat(stats: &str, key: &str) -> Option<String> {
    stats
        .split(['(', ')', ' '])
        .find_map(|part| part.strip_prefix(&format!("{key}=")).map(str::to_owned))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(items: &[&[&str]]) -> Vec<Vec<Option<String>>> {
        items.iter().map(|row| row.iter().map(|cell| Some((*cell).to_owned())).collect()).collect()
    }

    fn columns(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    /// 真实的 SQLite 3.4x 输出：别名、整数主键查找、临时 B 树、子查询挂在父节点下。
    #[test]
    fn sqlite_rows_become_a_tree_with_issues() {
        let cols = columns(&["id", "parent", "notused", "detail"]);
        let data = rows(&[
            &["8", "0", "0", "SCAN o"],
            &["10", "0", "0", "SEARCH c USING INTEGER PRIMARY KEY (rowid=?)"],
            &["13", "0", "0", "USE TEMP B-TREE FOR GROUP BY"],
            &["55", "0", "0", "USE TEMP B-TREE FOR ORDER BY"],
        ]);
        let plan = parse("sqlite", "EXPLAIN QUERY PLAN select c.city from orders o join customers AS c on c.id = o.customer_id group by c.city", &cols, &data).expect("plan");
        assert_eq!(plan.roots.len(), 4);
        assert_eq!(plan.roots[0].op, "SCAN");
        assert_eq!(plan.roots[0].object.as_deref(), Some("o（orders）"), "别名对回表名");
        assert_eq!(plan.roots[1].object.as_deref(), Some("c（customers）"));
        assert_eq!(plan.roots[0].issues, vec![Issue::FullScan]);
        assert_eq!(plan.roots[1].op, "SEARCH");
        assert!(plan.roots[1].issues.is_empty(), "按主键查找不是问题");
        assert_eq!(plan.roots[1].detail, "USING INTEGER PRIMARY KEY (rowid=?)");
        assert_eq!(plan.roots[2].issues, vec![Issue::TempSort]);
        assert_eq!(plan.findings().len(), 2, "同一问题同一对象只列一次：{:?}", plan.findings());

        let nested = rows(&[&["2", "0", "0", "SEARCH customers USING INTEGER PRIMARY KEY (rowid=?)"], &["6", "0", "0", "LIST SUBQUERY 1"], &["8", "6", "0", "SCAN orders"]]);
        let plan = parse("sqlite", "EXPLAIN QUERY PLAN select", &cols, &nested).unwrap();
        assert_eq!(plan.roots.len(), 2);
        assert_eq!(plan.roots[1].children[0].object.as_deref(), Some("orders"), "子查询里的扫描挂在 LIST SUBQUERY 下");
        let depths: Vec<usize> = plan.lines().iter().map(|line| line.depth).collect();
        assert_eq!(depths, vec![0, 0, 1]);

        // 用了覆盖索引的扫描不报全表扫描；旧版 “SCAN TABLE t” 也认。
        let covered = rows(&[&["3", "0", "0", "SCAN orders USING COVERING INDEX idx_orders_customer"], &["4", "0", "0", "SCAN TABLE legacy"]]);
        let plan = parse("sqlite", "EXPLAIN QUERY PLAN x", &cols, &covered).unwrap();
        assert!(plan.roots[0].issues.is_empty());
        assert_eq!(plan.roots[1].object.as_deref(), Some("legacy"));
        assert_eq!(plan.roots[1].issues, vec![Issue::FullScan]);
    }

    #[test]
    fn postgres_json_plan_keeps_costs_rows_and_conditions() {
        let json = r#"[{"Plan": {"Node Type": "Sort", "Startup Cost": 120.5, "Total Cost": 125.0, "Plan Rows": 1800, "Plan Width": 40, "Sort Key": ["o.total DESC"],
            "Plans": [{"Node Type": "Hash Join", "Join Type": "Inner", "Total Cost": 98.0, "Plan Rows": 1800, "Hash Cond": "(o.customer_id = c.id)",
              "Plans": [{"Node Type": "Seq Scan", "Relation Name": "orders", "Alias": "o", "Total Cost": 80.0, "Plan Rows": 5000, "Filter": "(total > '100'::numeric)"},
                        {"Node Type": "Hash", "Total Cost": 15.0, "Plan Rows": 1200, "Plans": [{"Node Type": "Index Only Scan", "Relation Name": "customers", "Alias": "customers", "Index Name": "customers_pkey", "Total Cost": 12.0, "Plan Rows": 1200}]}]}]}}]"#;
        let plan = parse("postgres", "EXPLAIN (FORMAT JSON) select", &columns(&["QUERY PLAN"]), &rows(&[&[json]])).expect("plan");
        assert_eq!(plan.total_cost(), Some(125.0));
        assert_eq!(plan.result_rows(), Some(1800.));
        let lines = plan.lines();
        assert_eq!(lines.len(), 5);
        assert_eq!(lines[1].node.op, "Hash Join (Inner)");
        assert_eq!(lines[1].node.kind(), OpKind::Join);
        assert_eq!(lines[2].node.object.as_deref(), Some("orders o"));
        assert_eq!(lines[2].node.issues, vec![Issue::FullScan], "带过滤条件的顺序扫描要提醒");
        assert!(lines[2].node.detail.contains("Filter"));
        assert_eq!(lines[4].node.object.as_deref(), Some("customers"));
        assert!(lines[4].node.detail.contains("customers_pkey"));
        assert_eq!(lines[4].node.kind(), OpKind::Seek);
        assert!(lines[0].node.props.iter().any(|(key, _)| key == "Sort Key"));
    }

    #[test]
    fn postgres_text_plan_uses_indentation() {
        let lines = [
            "Hash Join  (cost=30.50..90.00 rows=1800 width=40)",
            "  Hash Cond: (o.customer_id = c.id)",
            "  ->  Seq Scan on orders o  (cost=0.00..55.00 rows=5000 width=20)",
            "        Filter: (total > '100'::numeric)",
            "  ->  Hash  (cost=15.00..15.00 rows=1200 width=20)",
            "        ->  Index Scan using customers_pkey on customers c  (cost=0.28..12.00 rows=1200 width=20)",
        ];
        let data: Vec<Vec<Option<String>>> = lines.iter().map(|line| vec![Some((*line).to_owned())]).collect();
        let plan = parse("postgres", "EXPLAIN select", &columns(&["QUERY PLAN"]), &data).expect("plan");
        assert_eq!(plan.roots.len(), 1);
        assert_eq!(plan.roots[0].cost, Some(90.0));
        assert_eq!(plan.roots[0].children.len(), 2);
        let scan = &plan.roots[0].children[0];
        assert_eq!(scan.op, "Seq Scan");
        assert_eq!(scan.object.as_deref(), Some("orders o"));
        assert_eq!(scan.issues, vec![Issue::FullScan]);
        assert_eq!(plan.roots[0].children[1].children[0].rows, Some(1200.));
    }

    #[test]
    fn mysql_tree_and_table_formats() {
        let tree = "-> Sort: n DESC  (actual time=0.1..0.1 rows=7)\n    -> Table scan on <temporary>\n        -> Aggregate using temporary table\n            -> Nested loop inner join  (cost=2250.25 rows=5000)\n                -> Table scan on o  (cost=500.25 rows=5000)\n                -> Single-row index lookup on c using PRIMARY (id=o.customer_id)  (cost=0.25 rows=1)";
        let plan = parse("mysql", "EXPLAIN FORMAT=TREE select", &columns(&["EXPLAIN"]), &rows(&[&[tree]])).expect("plan");
        let lines = plan.lines();
        assert_eq!(lines.len(), 6);
        assert_eq!(lines[0].node.issues, vec![Issue::TempSort]);
        assert_eq!(lines[3].node.cost, Some(2250.25));
        assert_eq!(lines[4].node.op, "Table scan");
        assert_eq!(lines[4].node.object.as_deref(), Some("o"));
        assert_eq!(lines[4].node.issues, vec![Issue::FullScan]);
        assert_eq!(lines[5].depth, 4);
        assert_eq!(lines[5].node.object.as_deref(), Some("c"));

        let cols = columns(&["id", "select_type", "table", "type", "possible_keys", "key", "rows", "filtered", "Extra"]);
        let data = vec![
            vec![Some("1".into()), Some("SIMPLE".into()), Some("o".into()), Some("ALL".into()), None, None, Some("5000".into()), Some("100.00".into()), Some("Using temporary; Using filesort".into())],
            vec![Some("1".into()), Some("SIMPLE".into()), Some("c".into()), Some("eq_ref".into()), Some("PRIMARY".into()), Some("PRIMARY".into()), Some("1".into()), Some("100.00".into()), None],
        ];
        let plan = parse("mysql", "EXPLAIN select", &cols, &data).unwrap();
        assert_eq!(plan.roots[0].issues, vec![Issue::FullScan, Issue::TempSort, Issue::TempTable]);
        assert!(plan.roots[1].issues.is_empty());
        assert_eq!(plan.roots[1].detail, "key PRIMARY");
    }

    #[test]
    fn mariadb_json_walks_query_blocks() {
        let json = r#"{"query_block": {"select_id": 1, "filesort": {"sort_key": "count(0) desc", "temporary_table": {"nested_loop": [
            {"table": {"table_name": "o", "access_type": "ALL", "rows": 5000, "filtered": 100}},
            {"table": {"table_name": "c", "access_type": "eq_ref", "key": "PRIMARY", "used_key_parts": ["id"], "rows": 1}}]}}}}"#;
        let plan = parse("mariadb", "EXPLAIN FORMAT=JSON select", &columns(&["EXPLAIN"]), &rows(&[&[json]])).expect("plan");
        let lines = plan.lines();
        let ops: Vec<&str> = lines.iter().map(|line| line.node.op.as_str()).collect();
        assert_eq!(ops, vec!["Query block #1", "filesort", "temporary table", "ALL", "eq_ref"]);
        assert_eq!(lines[3].node.issues, vec![Issue::FullScan]);
        assert_eq!(lines[4].node.detail, "key PRIMARY");
        assert_eq!(lines[3].depth, 3, "nested_loop 只是容器，不单独占一层");
    }

    #[test]
    fn sqlserver_showplan_all_rows() {
        let cols = columns(&["StmtText", "StmtId", "NodeId", "Parent", "PhysicalOp", "LogicalOp", "Argument", "EstimateRows", "TotalSubtreeCost", "Warnings", "Type"]);
        let data = vec![
            vec![Some("SELECT * FROM orders WHERE total > 100".into()), Some("1".into()), Some("1".into()), Some("0".into()), None, None, None, Some("1800".into()), Some("0.42".into()), None, Some("SELECT".into())],
            vec![Some("|--Clustered Index Scan".into()), Some("1".into()), Some("2".into()), Some("1".into()), Some("Clustered Index Scan".into()), Some("Clustered Index Scan".into()),
                 Some("OBJECT:([shop].[dbo].[orders].[PK_orders] AS [o]), WHERE:([o].[total]>(100))".into()), Some("1800".into()), Some("0.42".into()), None, Some("PLAN_ROW".into())],
        ];
        let plan = parse("sqlserver", "SET SHOWPLAN_ALL ON", &cols, &data).expect("plan");
        assert_eq!(plan.roots.len(), 1, "语句行不是计划节点");
        let scan = &plan.roots[0];
        assert_eq!(scan.op, "Clustered Index Scan");
        assert_eq!(scan.object.as_deref(), Some("orders.PK_orders"));
        assert_eq!(scan.cost, Some(0.42));
        assert_eq!(scan.issues, vec![Issue::FullScan]);
    }

    /// 普通查询结果不是计划：不误认成计划视图。
    #[test]
    fn ordinary_results_are_not_plans() {
        assert!(parse("postgres", "SELECT 1", &columns(&["?column?"]), &rows(&[&["1"]])).is_none());
        assert!(parse("mysql", "SELECT 'x'", &columns(&["EXPLAIN"]), &rows(&[&["-> Table scan on t"]])).is_none());
        assert!(parse("sqlite", "SELECT 1", &columns(&["a"]), &rows(&[&["1"]])).is_none());
        assert!(parse("postgres", "EXPLAIN select", &columns(&["QUERY PLAN"]), &rows(&[&["{not json"]])).is_none());
    }
}
