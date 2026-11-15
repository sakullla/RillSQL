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
    fn new(op: impl Into<String>) -> Self {
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

/// 这条语句的结果是不是执行计划；是就解析，不是或认不出返回 None。
pub fn parse(engine: &str, sql: &str, columns: &[String], rows: &[Vec<Option<String>>]) -> Option<Plan> {
    let upper = sql.trim_start().to_ascii_uppercase();
    let explained = upper.starts_with("EXPLAIN") || upper.starts_with("SET SHOWPLAN");
    let column = |name: &str| columns.iter().position(|column| column.eq_ignore_ascii_case(name));
    let roots = match engine {
        "sqlite" if column("detail").is_some() && column("parent").is_some() => parse_sqlite(columns, rows),
        "sqlserver" if column("NodeId").is_some() && column("PhysicalOp").is_some() => parse_showplan_all(columns, rows),
        _ if !explained => return None,
        "postgres" => parse_postgres(columns, rows),
        "mysql" | "mariadb" => parse_mysql_family(columns, rows),
        _ => None,
    }?;
    let mut plan = Plan { engine: engine.to_owned(), roots };
    if plan.roots.is_empty() {
        return None;
    }
    // SQLite 在计划里写的是别名（SCAN o）：从语句里把别名对回表名，写成 “o（orders）”。
    if engine == "sqlite" {
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
        for root in &mut plan.roots {
            rename(root, &aliases);
        }
    }
    Some(plan)
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

fn cell(row: &[Option<String>], index: Option<usize>) -> Option<&str> {
    index.and_then(|index| row.get(index)).and_then(|value| value.as_deref())
}

fn number(text: &str) -> Option<f64> {
    text.trim().parse::<f64>().ok().filter(|value| value.is_finite())
}

/// 多个节点挂成树：(id, parent, node)，parent 不在集合里的就是根。
fn assemble(mut items: Vec<(i64, i64, PlanNode)>) -> Vec<PlanNode> {
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

// ---------------------------------------------------------------- SQLite

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

// ---------------------------------------------------------------- PostgreSQL

fn parse_postgres(columns: &[String], rows: &[Vec<Option<String>>]) -> Option<Vec<PlanNode>> {
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

fn json_inline(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(items) => items.iter().map(json_inline).collect::<Vec<_>>().join(", "),
        other => other.to_string(),
    }
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

// ---------------------------------------------------------------- MySQL / MariaDB

fn parse_mysql_family(columns: &[String], rows: &[Vec<Option<String>>]) -> Option<Vec<PlanNode>> {
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

// ---------------------------------------------------------------- SQL Server

fn parse_showplan_all(columns: &[String], rows: &[Vec<Option<String>>]) -> Option<Vec<PlanNode>> {
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

// ---------------------------------------------------------------- 共用：`->` 缩进树

/// PostgreSQL 文本计划与 MySQL TREE 共用：`->` 开头、按缩进分层；不带 `->` 的行是上一节点的属性。
fn arrow_tree(text: &str, node_of: fn(&str) -> PlanNode) -> Vec<PlanNode> {
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
fn split_stats(line: &str) -> (String, String) {
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
fn stat(stats: &str, key: &str) -> Option<String> {
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
