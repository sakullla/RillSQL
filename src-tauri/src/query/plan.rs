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

/// 节点在画布上的类型（参照 MySQL Workbench Visual Explain）：表是方框、JOIN 是胶囊、
/// 排序/分组等操作是圆角框、子查询是带框区域。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NodeKind {
    /// 读一张表（或派生表引用）：标题是表名加别名，访问类型作为彩色标签。
    Table,
    /// 连接：Nested loop / Hash join 等，标注产出行数。
    Join,
    /// 排序、分组、过滤、物化等其它操作。
    #[default]
    Operation,
    /// 子查询或查询块（DERIVED / SUBQUERY / UNION / Select #n）。
    Subquery,
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
    /// 实际耗时（毫秒，EXPLAIN ANALYZE / MariaDB ANALYZE）。
    pub actual_time_ms: Option<f64>,
    /// 实际返回行数（EXPLAIN ANALYZE / MariaDB ANALYZE）。
    pub actual_rows: Option<f64>,
    /// 循环执行次数（EXPLAIN ANALYZE / MariaDB ANALYZE）。
    pub loops: Option<u64>,
    /// 过滤率百分比（0..=100）。
    pub filtered: Option<f64>,
    /// 访问类型（如 const / eq_ref / ref / range / index / ALL）。
    pub access_type: Option<String>,
    /// 实际使用的索引名。
    pub key_used: Option<String>,
    /// 是否命中覆盖索引（Using index / Covering index）。
    pub covering_index: bool,
    /// 引擎给出的全部原始属性，详情面板逐条列出。
    pub props: Vec<(String, String)>,
    pub issues: Vec<Issue>,
    pub children: Vec<PlanNode>,
    /// 解析器明确知道的节点类型；None 时按操作名与对象推断（见 [`PlanNode::node_kind`]）。
    pub category: Option<NodeKind>,
}

impl PlanNode {
    pub(crate) fn new(op: impl Into<String>) -> Self {
        Self { op: op.into(), ..Self::default() }
    }

    pub(crate) fn with_category(mut self, category: NodeKind) -> Self {
        self.category = Some(category);
        self
    }

    /// 节点类型：解析器给定的优先；否则 JOIN 类操作是 Join，带对象的扫描/查找是 Table，
    /// 查询块、派生表、子查询是 Subquery，其余是 Operation。
    pub fn node_kind(&self) -> NodeKind {
        if let Some(category) = self.category {
            return category;
        }
        let op = self.op.to_ascii_lowercase();
        match self.kind() {
            OpKind::Join => NodeKind::Join,
            _ if op.contains("subquery") || op.contains("derived") || op.starts_with("select #") || op.starts_with("query block") || op.starts_with("subplan") || op.starts_with("initplan") => {
                NodeKind::Subquery
            }
            OpKind::Scan | OpKind::Seek if self.object.is_some() => NodeKind::Table,
            _ => NodeKind::Operation,
        }
    }

    /// 卡片标题：表节点用表名（加别名），其它节点用操作名。
    pub fn title(&self) -> String {
        match (self.node_kind(), &self.object) {
            (NodeKind::Table, Some(object)) if !object.trim().is_empty() => object.clone(),
            _ => self.op.clone(),
        }
    }

    pub fn kind(&self) -> OpKind {
        let op = self.op.to_ascii_lowercase();
        if op.contains("join") || op.contains("nested loop") || (op.contains("merge") && !op.contains("append") && !op.contains("index_merge")) {
            OpKind::Join
        } else if op.contains("sort") || op.contains("temp b-tree") || op.contains("filesort") || op.contains("ordering") {
            OpKind::Sort
        } else if op.contains("aggregate") || op.contains("group") || op.contains("hash match") || op.contains("duplicates removal") {
            OpKind::Aggregate
        } else if op.contains("search")
            || op.contains("seek")
            || op.contains("lookup")
            || op.contains("index scan")
            || op.contains("index only")
            || op.contains("covering index")
            || op.contains("index range")
            || op.contains("ref")
            || op.contains("eq_ref")
            || op.contains("const")
            || op == "system"
            || op == "range"
            || op == "index"
            || op == "index_merge"
            || op == "fulltext"
        {
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

    /// 根节点的实际返回行数（EXPLAIN ANALYZE / MariaDB ANALYZE）。
    pub fn result_actual_rows(&self) -> Option<f64> {
        self.roots.first().and_then(|root| root.actual_rows)
    }

    /// 整个计划的实际耗时（毫秒）：根节点之和；根节点没有时退回最大单节点耗时。
    pub fn total_actual_time_ms(&self) -> Option<f64> {
        let times: Vec<f64> = self.roots.iter().filter_map(|node| node.actual_time_ms).collect();
        if !times.is_empty() {
            Some(times.iter().sum())
        } else {
            self.max_actual_time_ms()
        }
    }

    /// 最大的单节点实际耗时（毫秒）：耗时占比条按它归一。
    pub fn max_actual_time_ms(&self) -> Option<f64> {
        self.lines()
            .iter()
            .filter_map(|line| line.node.actual_time_ms)
            .fold(None, |best: Option<f64>, time| Some(best.map_or(time, |best| best.max(time))))
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

fn starts_with_keyword(upper: &str, keyword: &str) -> bool {
    upper
        .strip_prefix(keyword)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(|ch: char| ch.is_ascii_whitespace() || ch == '('))
}

/// 这条语句的结果是不是执行计划；是就解析，不是或认不出返回 None。各引擎的结果形态由适配器认。
pub fn parse(engine: &str, sql: &str, columns: &[String], rows: &[Vec<Option<String>>]) -> Option<Plan> {
    let upper = sql.trim_start().to_ascii_uppercase();
    let explained = starts_with_keyword(&upper, "EXPLAIN")
        || upper.starts_with("SET SHOWPLAN")
        || (starts_with_keyword(&upper, "ANALYZE") && !upper.starts_with("ANALYZE TABLE"))
        || starts_with_keyword(&upper, "DESC")
        || starts_with_keyword(&upper, "DESCRIBE");
    let mut roots = crate::adapters::query::syntax_named(engine)?.parse_plan(sql, explained, columns, rows)?;
    // 有的引擎在计划里只写别名（table: o）：从语句里对回表名，表节点标题写成 “orders o”。
    // 已经带表名的对象（“orders o”“o（orders）”“orders.PK_orders”）不会与单个别名相等，不受影响。
    let aliases = alias_map(sql);
    if !aliases.is_empty() {
        for root in &mut roots {
            name_tables(root, &aliases);
        }
    }
    let plan = Plan { engine: engine.to_owned(), roots };
    if plan.roots.is_empty() {
        return None;
    }
    Some(plan)
}

/// 表节点的对象如果是语句里的别名，改写成 “表名 别名”。
fn name_tables(node: &mut PlanNode, aliases: &[(String, String)]) {
    if node.node_kind() == NodeKind::Table {
        if let Some(object) = node.object.as_mut() {
            if let Some((_, table)) = aliases.iter().find(|(alias, table)| alias.eq_ignore_ascii_case(object) && !table.eq_ignore_ascii_case(object)) {
                *object = format!("{table} {object}");
            }
        }
    }
    for child in &mut node.children {
        name_tables(child, aliases);
    }
}

/// 标识符两侧的引号或括号（各方言的引用写法都去掉）：既不是字母数字也不是 `_` / `$` 的 ASCII 标点。
fn is_quote(ch: char) -> bool {
    ch.is_ascii_punctuation() && ch != '_' && ch != '$'
}

/// 语句里的 “表 [AS] 别名”（FROM / JOIN 之后，以及 FROM 列表里逗号之后）。逗号结束一个表引用项，
/// 只在同一项内配对，绝不跨逗号把下一张表当成别名。只认简单形态，认不出就不改名。
pub(crate) fn alias_map(sql: &str) -> Vec<(String, String)> {
    const STOP: [&str; 18] =
        ["WHERE", "ON", "JOIN", "INNER", "LEFT", "RIGHT", "FULL", "CROSS", "GROUP", "ORDER", "LIMIT", "USING", "NATURAL", "OUTER", "UNION", "HAVING", "STRAIGHT_JOIN", "WINDOW"];
    /// 这些关键字结束 FROM 列表：之后的逗号不再分隔表引用。
    const END_FROM: [&str; 13] = ["WHERE", "ON", "USING", "GROUP", "ORDER", "LIMIT", "HAVING", "UNION", "WINDOW", "SELECT", "SET", "VALUES", "RETURNING"];
    // 逗号与括号各自单独成为一个记号：逗号是表引用项的边界，括号界定子查询的范围。
    let mut tokens: Vec<String> = Vec::new();
    let push_piece = |piece: &str, tokens: &mut Vec<String>| {
        for (n, part) in piece.split(',').enumerate() {
            if n > 0 {
                tokens.push(",".to_owned());
            }
            let part = part.trim_matches(is_quote);
            if !part.is_empty() {
                tokens.push(part.to_owned());
            }
        }
    };
    let mut rest = sql;
    while let Some(at) = rest.find(|ch: char| ch.is_whitespace() || ch == '(' || ch == ')' || ch == ';') {
        push_piece(&rest[..at], &mut tokens);
        let ch = rest[at..].chars().next().unwrap_or(' ');
        if ch == '(' || ch == ')' {
            tokens.push(ch.to_string());
        }
        rest = &rest[at + ch.len_utf8()..];
    }
    push_piece(rest, &mut tokens);
    let is_paren = |token: &str| token == "(" || token == ")";
    let mut out = Vec::new();
    let mut in_from = false;
    // 进入括号时保存外层的 FROM 状态，括号里从头判断；出括号时恢复。
    // 这样 SELECT 列表里标量子查询的 FROM 不会让外层后面的逗号被当成表引用项。
    let mut outer: Vec<bool> = Vec::new();
    for index in 0..tokens.len() {
        let keyword = tokens[index].to_ascii_uppercase();
        let starts_item = match keyword.as_str() {
            "(" => {
                outer.push(in_from);
                in_from = false;
                false
            }
            ")" => {
                in_from = outer.pop().unwrap_or(false);
                false
            }
            "FROM" | "JOIN" | "STRAIGHT_JOIN" => {
                in_from = true;
                true
            }
            "," => in_from,
            other => {
                if END_FROM.contains(&other) {
                    in_from = false;
                }
                false
            }
        };
        if !starts_item {
            continue;
        }
        let Some(table) = tokens.get(index + 1) else { continue };
        if table == "," || is_paren(table) || table.eq_ignore_ascii_case("SELECT") || STOP.contains(&table.to_ascii_uppercase().as_str()) {
            continue;
        }
        let mut next = index + 2;
        if tokens.get(next).is_some_and(|token| token.eq_ignore_ascii_case("AS")) {
            next += 1;
        }
        if let Some(alias) = tokens.get(next).filter(|alias| *alias != "," && !is_paren(alias) && !STOP.contains(&alias.to_ascii_uppercase().as_str())) {
            let table = table.rsplit('.').next().unwrap_or(table).trim_matches(is_quote).to_owned();
            out.push((alias.clone(), table));
        }
    }
    out
}

/// 数字的紧凑文本：整数不带小数，其余最多两位小数。
pub(crate) fn trim_number(value: f64) -> String {
    if value.fract().abs() < f64::EPSILON {
        format!("{value:.0}")
    } else {
        let text = format!("{value:.2}");
        text.trim_end_matches('0').trim_end_matches('.').to_owned()
    }
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
    // 用栈建树：每个节点挂到栈里缩进比它小的最近节点下；缩进回退到比第一层还浅时成为新的根，
    // 不会提前结束而丢掉后面的行。
    let mut roots: Vec<PlanNode> = Vec::new();
    let mut stack: Vec<(usize, PlanNode)> = Vec::new();
    fn pop_into(stack: &mut Vec<(usize, PlanNode)>, roots: &mut Vec<PlanNode>) {
        if let Some((_, node)) = stack.pop() {
            match stack.last_mut() {
                Some((_, parent)) => parent.children.push(node),
                None => roots.push(node),
            }
        }
    }
    for (indent, node) in flat {
        while stack.last().is_some_and(|(top, _)| *top >= indent) {
            pop_into(&mut stack, &mut roots);
        }
        stack.push((indent, node));
    }
    while !stack.is_empty() {
        pop_into(&mut stack, &mut roots);
    }
    roots
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

    fn pairs(sql: &str) -> Vec<(String, String)> {
        alias_map(sql)
    }

    fn pair(alias: &str, table: &str) -> (String, String) {
        (alias.to_owned(), table.to_owned())
    }

    /// 逗号结束一个表引用项：不带别名的逗号连接不改名，带别名的逗号连接逐项配对；JOIN 写法不变。
    #[test]
    fn alias_map_never_pairs_across_a_comma() {
        assert!(pairs("EXPLAIN select * from orders, customers where orders.customer_id = customers.id").is_empty(), "逗号连接的两张表各用自己的名字");
        assert!(pairs("EXPLAIN select * from orders,customers").is_empty());
        assert_eq!(pairs("EXPLAIN select * from orders o, customers c where c.id = o.customer_id"), vec![pair("o", "orders"), pair("c", "customers")]);
        assert_eq!(pairs("EXPLAIN select * from shop.orders AS o,shop.customers AS c"), vec![pair("o", "orders"), pair("c", "customers")]);
        assert_eq!(pairs("EXPLAIN select * from orders o, customers where c.id = 1"), vec![pair("o", "orders")], "无别名的项不借用下一项");
        assert_eq!(
            pairs("EXPLAIN select c.name, o.total from shop.orders o join shop.customers c on c.id = o.customer_id join shop.order_lines AS l on l.order_id = o.id"),
            vec![pair("o", "orders"), pair("c", "customers"), pair("l", "order_lines")]
        );
        assert!(pairs("EXPLAIN select a, b from orders join customers on f(a, b) = 1 where x in (1, 2)").is_empty(), "ON / WHERE 之后的逗号不是表引用项");
        // SELECT 列表里的标量子查询：它的 FROM 只在括号里有效，外层后面的逗号不是表引用项。
        assert!(pairs("SELECT (SELECT max(x) FROM u), name AS customers FROM customers").is_empty(), "列别名不会让表节点改名");
        assert_eq!(
            pairs("EXPLAIN select (select max(l.qty) from order_lines l where l.order_id = o.id), o.total AS orders from orders o"),
            vec![pair("l", "order_lines"), pair("o", "orders")]
        );
        assert_eq!(pairs("EXPLAIN select * from (select id from orders) s, customers c"), vec![pair("c", "customers")], "派生表之后的逗号仍分隔表引用项");
        let cols = columns(&["id", "select_type", "table", "type", "key", "rows", "filtered", "Extra"]);
        let data = rows(&[&["1", "PRIMARY", "customers", "ALL", "", "3", "100", ""], &["2", "SUBQUERY", "u", "ALL", "", "5", "100", ""]]);
        let plan = parse("mysql", "EXPLAIN SELECT (SELECT max(x) FROM u), name AS customers FROM customers", &cols, &data).expect("plan");
        let titles: Vec<String> = plan.lines().iter().filter(|line| line.node.node_kind() == NodeKind::Table).map(|line| line.node.title()).collect();
        assert!(titles.contains(&"customers".to_owned()) && !titles.iter().any(|title| title.contains("name")), "{titles:?}");
        let cols = columns(&["id", "select_type", "table", "type", "key", "rows", "filtered", "Extra"]);
        let data = rows(&[&["1", "SIMPLE", "orders", "ALL", "", "5", "100", ""], &["1", "SIMPLE", "customers", "ALL", "", "3", "100", ""]]);
        let plan = parse("mysql", "EXPLAIN select * from orders, customers", &cols, &data).expect("plan");
        let titles: Vec<String> = plan.lines().iter().filter(|line| line.node.node_kind() == NodeKind::Table).map(|line| line.node.title()).collect();
        assert_eq!(titles, vec!["orders", "customers"]);
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
        assert_eq!(plan.roots.len(), 1, "同一块的两张表连成一个 JOIN");
        assert_eq!(plan.roots[0].node_kind(), NodeKind::Join);
        let join = &plan.roots[0].children;
        assert_eq!(join[0].issues, vec![Issue::FullScan, Issue::TempSort, Issue::TempTable]);
        assert!(join[1].issues.is_empty());
        assert_eq!(join[1].detail, "key PRIMARY");
    }

    #[test]
    fn mariadb_json_walks_query_blocks() {
        let json = r#"{"query_block": {"select_id": 1, "filesort": {"sort_key": "count(0) desc", "temporary_table": {"nested_loop": [
            {"table": {"table_name": "o", "access_type": "ALL", "rows": 5000, "filtered": 100}},
            {"table": {"table_name": "c", "access_type": "eq_ref", "key": "PRIMARY", "used_key_parts": ["id"], "rows": 1}}]}}}}"#;
        let plan = parse("mariadb", "EXPLAIN FORMAT=JSON select", &columns(&["EXPLAIN"]), &rows(&[&[json]])).expect("plan");
        let lines = plan.lines();
        let ops: Vec<&str> = lines.iter().map(|line| line.node.op.as_str()).collect();
        assert_eq!(ops, vec!["Query block #1", "filesort", "temporary table", "Nested loop", "ALL", "eq_ref"]);
        assert_eq!(lines[3].node.node_kind(), NodeKind::Join, "nested_loop 保留为 JOIN 节点");
        assert_eq!(lines[4].node.issues, vec![Issue::FullScan]);
        assert_eq!(lines[5].node.detail, "key PRIMARY");
        assert_eq!((lines[4].depth, lines[5].depth), (4, 4), "两张表是 JOIN 的左右输入");
        assert_eq!(lines[4].node.title(), "o");
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

    #[test]
    fn mysql8_json_plan_extracts_cost_info_without_spurious_child_nodes() {
        let json = r#"{
          "query_block": {
            "select_id": 1,
            "cost_info": { "query_cost": "125.50" },
            "ordering_operation": {
              "using_temporary_table": true,
              "using_filesort": true,
              "cost_info": { "sort_cost": "12.00" },
              "nested_loop": [
                {
                  "table": {
                    "table_name": "orders",
                    "access_type": "range",
                    "possible_keys": ["idx_created"],
                    "key": "idx_created",
                    "used_key_parts": ["created_at"],
                    "rows_examined_per_scan": 400,
                    "rows_produced_per_join": 200,
                    "filtered": "50.00",
                    "using_index": true,
                    "attached_condition": "(orders.status = 'paid')",
                    "cost_info": { "read_cost": "40.25", "eval_cost": "20.00", "prefix_cost": "60.25" },
                    "used_columns": ["id", "customer_id", "status"]
                  }
                },
                {
                  "table": {
                    "table_name": "customers",
                    "access_type": "eq_ref",
                    "key": "PRIMARY",
                    "used_key_parts": ["id"],
                    "rows_examined_per_scan": 1,
                    "filtered": "100.00",
                    "cost_info": { "read_cost": "50.00", "eval_cost": "15.25", "prefix_cost": "125.50" }
                  }
                }
              ]
            }
          }
        }"#;
        let plan = parse("mysql", "EXPLAIN FORMAT=JSON SELECT * FROM orders JOIN customers", &columns(&["EXPLAIN"]), &rows(&[&[json]])).expect("plan");
        let lines = plan.lines();
        assert!(!lines.iter().any(|line| line.node.op == "cost_info"), "cost_info 不能作为子节点出现");
        assert!(!lines.iter().any(|line| line.node.op == "used_columns"), "used_columns 不能作为子节点出现");
        assert_eq!(plan.total_cost(), Some(125.50));
        assert_eq!(lines[0].node.op, "Query block #1");
        assert_eq!(lines[0].node.cost, Some(125.50));
        assert!(lines[0].node.props.iter().any(|(k, v)| k == "cost_info.query_cost" && v == "125.50"));

        assert_eq!(lines[1].node.op, "ordering operation");
        assert_eq!(lines[1].node.cost, Some(12.0));
        assert_eq!(lines[1].node.issues, vec![Issue::TempSort, Issue::TempTable]);

        let join = lines[2].node;
        assert_eq!(join.op, "Nested loop");
        assert_eq!(join.cost, Some(125.50), "JOIN 的代价是右表的累计 prefix_cost");
        let orders = lines[3].node;
        assert_eq!(orders.object.as_deref(), Some("orders"));
        assert_eq!(orders.access_type.as_deref(), Some("range"));
        assert_eq!(orders.key_used.as_deref(), Some("idx_created"));
        assert!(orders.covering_index);
        assert_eq!(orders.rows, Some(400.0));
        assert_eq!(orders.filtered, Some(50.0));
        assert_eq!(orders.cost, Some(60.25));
        assert_eq!(orders.title(), "orders");

        let customers = lines[4].node;
        assert_eq!(customers.object.as_deref(), Some("customers"));
        assert_eq!(customers.access_type.as_deref(), Some("eq_ref"));
        assert_eq!(customers.key_used.as_deref(), Some("PRIMARY"));
        assert!(!customers.covering_index);
        assert_eq!(customers.cost, Some(65.25), "表自身的代价是 read_cost + eval_cost");
    }

    #[test]
    fn mysql8_explain_analyze_parses_actual_metrics_and_never_executed() {
        let tree = "\
-> Nested loop inner join  (cost=2250.25 rows=5000) (actual time=0.120..18.450 rows=420 loops=1)
    -> Table scan on o  (cost=500.25 rows=5000) (actual time=0.032..4.567 rows=5000 loops=1)
    -> Covering index lookup on c using idx_cust (id=o.customer_id)  (cost=0.25 rows=1) (actual time=0.002..0.003 rows=1 loops=420)
    -> Index lookup on d using idx_dead (id=o.id)  (cost=1.10 rows=2) (never executed)";
        let plan = parse("mysql", "EXPLAIN ANALYZE SELECT * FROM o JOIN c", &columns(&["EXPLAIN"]), &rows(&[&[tree]])).expect("plan");
        assert_eq!(plan.total_cost(), Some(2250.25));
        assert_eq!(plan.total_actual_time_ms(), Some(18.45));
        assert_eq!(plan.max_actual_time_ms(), Some(18.45));
        assert_eq!(plan.result_actual_rows(), Some(420.0));

        let lines = plan.lines();
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0].node.loops, Some(1));

        let scan = lines[1].node;
        assert_eq!(scan.access_type.as_deref(), Some("ALL"));
        assert_eq!(scan.actual_time_ms, Some(4.567));
        assert_eq!(scan.actual_rows, Some(5000.0));
        assert_eq!(scan.loops, Some(1));
        assert_eq!(scan.issues, vec![Issue::FullScan]);

        let cov = lines[2].node;
        assert_eq!(cov.access_type.as_deref(), Some("ref"));
        assert_eq!(cov.key_used.as_deref(), Some("idx_cust"));
        assert!(cov.covering_index);
        assert_eq!(cov.actual_time_ms, Some(0.003));
        assert_eq!(cov.actual_rows, Some(1.0));
        assert_eq!(cov.loops, Some(420));

        let dead = lines[3].node;
        assert_eq!(dead.cost, Some(1.10));
        assert_eq!(dead.rows, Some(2.0));
        assert_eq!(dead.loops, Some(0));
        assert_eq!(dead.actual_rows, Some(0.0));
        assert_eq!(dead.actual_time_ms, Some(0.0));
    }

    #[test]
    fn mariadb_analyze_json_parses_runtime_metrics() {
        let json = r#"{
          "query_block": {
            "select_id": 1,
            "r_loops": 1,
            "r_total_time_ms": 9.82,
            "nested_loop": [
              {
                "table": {
                  "table_name": "orders",
                  "access_type": "index",
                  "key": "idx_status",
                  "rows": 1200,
                  "r_loops": 1,
                  "r_rows": 980,
                  "r_total_time_ms": 6.45,
                  "filtered": 100,
                  "r_filtered": 42.5,
                  "using_index": true,
                  "attached_condition": "orders.status = 'open'"
                }
              }
            ]
          }
        }"#;
        let plan = parse("mariadb", "ANALYZE FORMAT=JSON SELECT * FROM orders", &columns(&["ANALYZE"]), &rows(&[&[json]])).expect("plan");
        assert_eq!(plan.total_actual_time_ms(), Some(9.82));
        let lines = plan.lines();
        assert_eq!(lines.len(), 2);
        let tbl = lines[1].node;
        assert_eq!(tbl.object.as_deref(), Some("orders"));
        assert_eq!(tbl.access_type.as_deref(), Some("index"));
        assert_eq!(tbl.key_used.as_deref(), Some("idx_status"));
        assert!(tbl.covering_index);
        assert_eq!(tbl.rows, Some(1200.0));
        assert_eq!(tbl.actual_rows, Some(980.0));
        assert_eq!(tbl.loops, Some(1));
        assert_eq!(tbl.actual_time_ms, Some(6.45));
        assert_eq!(tbl.filtered, Some(42.5));
    }

    #[test]
    fn mysql_traditional_multiblock_explain_builds_hierarchy() {
        let cols = columns(&["id", "select_type", "table", "type", "possible_keys", "key", "rows", "filtered", "Extra"]);
        let data = vec![
            vec![Some("1".into()), Some("PRIMARY".into()), Some("<derived2>".into()), Some("ALL".into()), None, None, Some("100".into()), Some("100.00".into()), None],
            vec![Some("1".into()), Some("PRIMARY".into()), Some("c".into()), Some("eq_ref".into()), Some("PRIMARY".into()), Some("PRIMARY".into()), Some("1".into()), Some("100.00".into()), Some("Using index".into())],
            vec![Some("2".into()), Some("DERIVED".into()), Some("orders".into()), Some("range".into()), Some("idx_dt".into()), Some("idx_dt".into()), Some("100".into()), Some("25.00".into()), Some("Using index condition".into())],
            vec![Some("3".into()), Some("SUBQUERY".into()), Some("vip".into()), Some("ref".into()), Some("idx_vip".into()), Some("idx_vip".into()), Some("10".into()), Some("50.00".into()), Some("Using index".into())],
        ];
        let plan = parse("mysql", "DESC SELECT * FROM (SELECT * FROM orders) d JOIN c", &cols, &data).expect("plan");
        assert_eq!(plan.roots.len(), 1);
        assert_eq!(plan.roots[0].op, "PRIMARY #1");
        let lines = plan.lines();
        // PRIMARY #1 -> Nested loop -> (<derived2> -> DERIVED #2 -> orders; c); SUBQUERY #3 -> vip
        assert_eq!(lines[1].node.op, "Nested loop");
        assert_eq!(lines[2].node.object.as_deref(), Some("<derived2>"));
        assert_eq!(lines[3].node.op, "DERIVED #2");
        assert_eq!(lines[3].node.node_kind(), NodeKind::Subquery);
        assert_eq!(lines[3].depth, 3);
        assert_eq!(lines[4].node.object.as_deref(), Some("orders"));
        assert_eq!(lines[4].node.filtered, Some(25.0));
        assert!(!lines[4].node.covering_index, "Using index condition 不是覆盖索引");
        let c_node = lines.iter().find(|l| l.node.object.as_deref() == Some("c")).unwrap().node;
        assert!(c_node.covering_index, "Using index 是覆盖索引");
        assert_eq!(c_node.key_used.as_deref(), Some("PRIMARY"));
        assert_eq!(c_node.access_type.as_deref(), Some("eq_ref"));
        assert!(lines.iter().any(|l| l.node.op == "SUBQUERY #3" && l.depth == 1));
    }
}
