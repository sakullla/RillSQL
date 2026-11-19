//! SQL Server 的查询执行：TDS 收行集或拿受影响行数、SHOWPLAN 批次与排序、TDS 类型名与单元文本、
//! 服务器错误行号，以及 TOP 行数上限改写。

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::adapters::exec::looks_like_rows;
use crate::query::exec::{
    limit_target, peel_trailing_semi, read_u64, replace_count, reveal_rows, scan_top, skip_noise, AutoRewrite, CappedSql, CountSpan, ErrorAt, Failure, RowSink,
    StatementValue,
};
use crate::query::split;

/// SQL Server 的服务器错误带行号（批次内从 1 开始）；0 表示服务器没给行号。
pub(crate) fn sqlserver_error_line(line: u32) -> Option<usize> {
    if line == 0 {
        None
    } else {
        Some(line as usize)
    }
}

/// SQL Server 服务器错误的位置。错误发生在存储过程/触发器/函数里时（procedure 非空），
/// 行号属于那个模块的定义，不对应用户写的语句，此时不给位置，由编辑器整条语句标错。
pub(crate) fn sqlserver_error_at(line: u32, procedure: &str) -> Option<ErrorAt> {
    if !procedure.trim().is_empty() {
        return None;
    }
    sqlserver_error_line(line).map(ErrorAt::Line)
}

/// 从 tiberius 错误里取行号；不是服务器报错时没有位置。
fn tds_error_at(err: &tiberius::error::Error) -> Option<ErrorAt> {
    match err {
        tiberius::error::Error::Server(token) => sqlserver_error_at(token.line(), token.procedure()),
        _ => None,
    }
}

/// SQL Server 服务器错误号（如 2627 主键冲突）；不是服务器报错时没有错误号。
fn tds_error_code(err: &tiberius::error::Error) -> Option<String> {
    match err {
        tiberius::error::Error::Server(token) => Some(token.code().to_string()),
        _ => None,
    }
}

fn tds_failure(err: &tiberius::error::Error) -> Failure {
    Failure { message: crate::error_text::chain(err), at: tds_error_at(err), code: tds_error_code(err) }
}

pub(in crate::adapters) async fn sqlserver_current(client: &std::sync::Arc<tokio::sync::Mutex<crate::connections::drivers::TdsClient>>) -> Result<String, String> {
    let (_names, rows) = super::grid_read::sqlserver_rows(client, "SELECT DB_NAME()").await?;
    match rows.first().and_then(|row| row.first()) {
        Some(crate::grid::Cell::Text(text)) => Ok(text.clone()),
        _ => Err("没有当前数据库".to_owned()),
    }
}

/// TDS 执行：会返回行的语句走 simple_query 收行集，其余走 execute 拿受影响行数。
/// SET 类语句（含 SET SHOWPLAN_ALL）一律走 simple_query：execute 是 sp_executesql，
/// 其中的 SET 只作用于内层批次、结束后即还原，外层会话上的 SHOWPLAN_ALL 不会生效。
pub(in crate::adapters) async fn sqlserver_run(
    client: &std::sync::Arc<tokio::sync::Mutex<crate::connections::drivers::TdsClient>>,
    bounded: &str,
    limit: usize,
    cancel: &Arc<AtomicBool>,
    sink: &dyn RowSink,
    index: usize,
) -> Result<StatementValue, Failure> {
    // 读取上限已由 LiveDriver 改写过，这里发出去的就是实际语句。
    let mut locked = client.lock().await;
    if !sqlserver_wants_rows(bounded) {
        let done = locked.execute(bounded.to_owned(), &[]).await.map_err(|err| tds_failure(&err))?;
        let value = StatementValue {
            columns: Vec::new(),
            column_types: Vec::new(),
            rows: Vec::new(),
            truncated: false,
            affected: Some(done.total()),
        };
        sink.publish(index, &value).await;
        return Ok(value);
    }
    let mut stream = locked.simple_query(bounded).await.map_err(|err| tds_failure(&err))?;
    let mut columns: Vec<String> = Vec::new();
    let mut column_types: Vec<String> = Vec::new();
    let mut rows: Vec<Vec<Option<String>>> = Vec::new();
    let mut truncated = false;
    {
        use futures::TryStreamExt;
        while let Some(item) = stream.try_next().await.map_err(|err| tds_failure(&err))? {
            if let Some(meta) = item.as_metadata() {
                if columns.is_empty() {
                    columns = meta.columns().iter().map(|column| column.name().to_owned()).collect();
                    column_types = meta.columns().iter().map(|column| tds_type_name(column.column_type()).to_owned()).collect();
                }
                continue;
            }
            if let Some(row) = item.into_row() {
                if columns.is_empty() {
                    columns = (0..row.len()).map(|column| format!("col{column}")).collect();
                    column_types = vec!["类型未知".to_owned(); columns.len()];
                }
                if rows.len() >= limit {
                    truncated = true;
                    break;
                }
                let cells = (0..columns.len()).map(|cell| tds_string_cell(&row, cell)).collect();
                rows.push(cells);
            }
        }
    }
    drop(stream);
    drop(locked);
    // SHOWPLAN_ALL 行集按 NodeId 排成稳定的父先子后顺序（层次有效性经 showplan_outline 验证）。
    let rows = order_showplan_results(&columns, rows);
    if columns.is_empty() {
        let value = StatementValue {
            columns,
            column_types,
            rows,
            truncated: false,
            affected: None,
        };
        sink.publish(index, &value).await;
        return Ok(value);
    }
    reveal_rows(sink, index, columns, column_types, rows, truncated, None, limit, cancel).await.map_err(Failure::from)
}

/// SHOWPLAN 批次判定：以 SET SHOWPLAN_ALL 开头的语句即使整批到达也按收行路径执行。
pub(crate) fn is_showplan_statement(sql: &str) -> bool {
    let mut words = sql.split_whitespace();
    matches!(
        (words.next(), words.next()),
        (Some(first), Some(second)) if first.eq_ignore_ascii_case("SET") && second.eq_ignore_ascii_case("SHOWPLAN_ALL")
    )
}

/// TDS 的收行路由：普通 SELECT 族、以及全部 SET 开头的会话设置语句。
fn sqlserver_wants_rows(sql: &str) -> bool {
    is_showplan_statement(sql) || split::first_keyword(sql) == "SET" || looks_like_rows(sql)
}

/// 把已收到的 SHOWPLAN_ALL 行集按 NodeId 升序排成父先子后。
/// 先把行集还原成 (NodeId, Parent, 操作) 三元组走 showplan_outline 验证层次完整
/// （父节点缺失或 NodeId 非数字时按原序返回，不猜）；有效层次才重排。
pub(crate) fn order_showplan_results(columns: &[String], rows: Vec<Vec<Option<String>>>) -> Vec<Vec<Option<String>>> {
    let names: Vec<String> = columns.iter().map(|column| column.trim().to_ascii_lowercase()).collect();
    // 只对 SHOWPLAN_ALL 形状应用：除 NodeId/Parent 外还须有 PhysicalOp/LogicalOp/StmtId 等
    // SHOWPLAN 专属列同现——普通查询恰好有 NodeId+Parent 数字列时不重排、保持原序。
    if !names.iter().any(|name| matches!(name.as_str(), "physicalop" | "logicalop" | "stmtid" | "estimaterows" | "totalsubtreecost")) {
        return rows;
    }
    let (Some(node_at), Some(parent_at)) = (names.iter().position(|name| name == "nodeid"), names.iter().position(|name| name == "parent")) else {
        return rows;
    };
    let text = |row: &Vec<Option<String>>, index: usize| -> String {
        row.get(index).and_then(|cell| cell.as_deref()).unwrap_or("").trim().to_owned()
    };
    let mut triples = Vec::with_capacity(rows.len());
    for (position, row) in rows.iter().enumerate() {
        let (Some(node), Some(parent)) = (text(row, node_at).parse::<i64>().ok(), text(row, parent_at).parse::<i64>().ok()) else {
            return rows;
        };
        triples.push((node, parent, position));
    }
    if showplan_outline(&triples.iter().map(|(node, parent, _)| (*node, *parent, String::new(), String::new())).collect::<Vec<_>>()).len() != rows.len() {
        return rows;
    }
    let mut indexed = triples;
    indexed.sort_by_key(|(node, _, _)| *node);
    indexed.into_iter().map(|(_, _, position)| rows[position].clone()).collect()
}

fn tds_string_cell(row: &tiberius::Row, index: usize) -> Option<String> {
    if let Ok(Some(text)) = row.try_get::<&str, _>(index) {
        return Some(text.to_owned());
    }
    if let Ok(Some(value)) = row.try_get::<i64, _>(index) {
        return Some(value.to_string());
    }
    if let Ok(Some(value)) = row.try_get::<i32, _>(index) {
        return Some(value.to_string());
    }
    if let Ok(Some(value)) = row.try_get::<i16, _>(index) {
        return Some(value.to_string());
    }
    if let Ok(Some(value)) = row.try_get::<u8, _>(index) {
        return Some(value.to_string());
    }
    if let Ok(Some(value)) = row.try_get::<f64, _>(index) {
        return Some(value.to_string());
    }
    if let Ok(Some(value)) = row.try_get::<bool, _>(index) {
        return Some(if value { "1" } else { "0" }.to_owned());
    }
    if let Ok(Some(bytes)) = row.try_get::<&[u8], _>(index) {
        return Some(bytes.iter().map(|byte| format!("{byte:02x}")).collect());
    }
    // decimal / numeric / money：Numeric 自带定点文本（含小数位）。
    if let Ok(Some(value)) = row.try_get::<tiberius::numeric::Numeric, _>(index) {
        return Some(tds_numeric_text(value));
    }
    // uniqueidentifier。
    if let Ok(Some(value)) = row.try_get::<uuid::Uuid, _>(index) {
        return Some(tds_uuid_text(value));
    }
    // xml（文档原文）：XmlData 只实现 FromSqlOwned，经 cells() 按 ColumnData 直接取。
    if let Some((_, tiberius::ColumnData::Xml(Some(xml)))) = row.cells().nth(index) {
        return Some(xml.to_string());
    }
    // datetime / datetime2 / smalldatetime → 本地日期时间文本。
    if let Ok(Some(value)) = row.try_get::<chrono::NaiveDateTime, _>(index) {
        return Some(tds_naive_datetime_text(value));
    }
    // date。
    if let Ok(Some(value)) = row.try_get::<chrono::NaiveDate, _>(index) {
        return Some(tds_naive_date_text(value));
    }
    // time(n)。
    if let Ok(Some(value)) = row.try_get::<chrono::NaiveTime, _>(index) {
        return Some(tds_naive_time_text(value));
    }
    // datetimeoffset（带时区偏移）。
    if let Ok(Some(value)) = row.try_get::<chrono::DateTime<chrono::Utc>, _>(index) {
        return Some(tds_datetime_offset_text(value));
    }
    None
}

/// 以下格式化器把 TDS 专用类型落成与其它引擎一致的网格文本；
/// 单独成函数是为了不经数据库就能单测格式口径。
pub(crate) fn tds_numeric_text(value: tiberius::numeric::Numeric) -> String {
    value.to_string()
}

pub(crate) fn tds_uuid_text(value: uuid::Uuid) -> String {
    value.to_string()
}

pub(crate) fn tds_naive_datetime_text(value: chrono::NaiveDateTime) -> String {
    // chrono Display：无小数 "2024-01-02 15:04:05"，有则补到 3/6/9 位。
    value.to_string()
}

pub(crate) fn tds_naive_date_text(value: chrono::NaiveDate) -> String {
    value.format("%Y-%m-%d").to_string()
}

pub(crate) fn tds_naive_time_text(value: chrono::NaiveTime) -> String {
    value.to_string()
}

pub(crate) fn tds_datetime_offset_text(value: chrono::DateTime<chrono::Utc>) -> String {
    value.to_rfc3339()
}

pub(crate) fn tds_type_name(kind: tiberius::ColumnType) -> &'static str {
    use tiberius::ColumnType::*;
    match kind {
        Bit | Bitn => "bit",
        Int1 => "tinyint",
        Int2 => "smallint",
        Int4 => "int",
        Int8 | Intn => "bigint",
        Datetime4 | Datetime | Datetimen | Datetime2 => "datetime",
        DatetimeOffsetn => "datetimeoffset",
        Daten => "date",
        Timen => "time",
        Float4 | Floatn => "real",
        Float8 => "float",
        Money | Money4 | Decimaln | Numericn => "decimal",
        Guid => "uniqueidentifier",
        BigVarBin | BigBinary | Image => "varbinary",
        BigVarChar | BigChar | Text => "varchar",
        NVarchar | NChar | NText => "nvarchar",
        Xml => "xml",
        Udt | SSVariant | Null => "类型未知",
    }
}

/// SQL Server 的执行计划批次：SET SHOWPLAN_ALL ON 后服务器只编译不执行，读完行集再关掉。
/// 与前端 src/query/plan.ts 的 sqlserver 形态互为镜像；这里钉住批次文本的契约。
pub(crate) fn showplan_batch(statement: &str) -> String {
    let body = statement.trim().trim_end_matches(';');
    format!("SET SHOWPLAN_ALL ON;\n{body};\nSET SHOWPLAN_ALL OFF")
}

/// SHOWPLAN_ALL 行集的层次展开（纯函数）：(NodeId, Parent, 操作, 估算行数) 按父节点深度排成缩进大纲。
/// 根节点的 Parent 不在结果里时深度为 0；父节点缺失按根处理，成环按根处理。
pub(crate) fn showplan_outline(rows: &[(i64, i64, String, String)]) -> Vec<(usize, String, String)> {
    let ids: Vec<i64> = rows.iter().map(|(node, _, _, _)| *node).collect();
    let depth_of = |node: i64| -> usize {
        let mut seen = std::collections::HashSet::new();
        let mut depth = 0usize;
        let mut current = node;
        loop {
            if !seen.insert(current) {
                return depth;
            }
            let Some((_, parent, _, _)) = rows.iter().find(|(candidate, _, _, _)| *candidate == current) else {
                return depth;
            };
            if *parent == current || !ids.contains(parent) {
                return depth;
            }
            depth += 1;
            current = *parent;
        }
    };
    rows.iter()
        .map(|(node, _, operation, est_rows)| (depth_of(*node), operation.clone(), est_rows.clone()))
        .collect()
}

/// T-SQL 的行数上限：SELECT 直接改写为 SELECT TOP (n)；FETCH NEXT 已有时只改数字；
/// TOP 已有时只改数字；WITH 开头的语句不改写（TOP 需要落在内层 SELECT，盲改会破坏语义）。
pub(super) fn cap_rows_with_top(sql: &str, cap: usize) -> CappedSql {
    let probe = cap.saturating_add(1);
    let (body, suffix) = peel_trailing_semi(sql);
    if !limit_target(body) {
        return CappedSql::unchanged(sql);
    }
    let scan = scan_top("sqlserver", body);
    if scan.has_into {
        return CappedSql::unchanged(sql);
    }
    if let Some(fetch) = scan.fetch {
        if fetch.value <= probe as u64 {
            return CappedSql::unchanged(sql);
        }
        return replace_count(body, suffix, fetch.span, probe, "fetch");
    }
    if let Some(top) = scan_top_n(body) {
        if top.value <= probe as u64 {
            return CappedSql::unchanged(sql);
        }
        return replace_count(body, suffix, top.span, probe, "top");
    }
    if split::first_keyword(body) != "SELECT" {
        return CappedSql::unchanged(sql);
    }
    // TOP 紧跟在第一个 SELECT 之后：插入点之前的文本原样保留，之后的全部后移。
    let cut = body.to_ascii_uppercase().find("SELECT").unwrap_or(0) + "SELECT".len();
    let mut added = String::from(" TOP (");
    added.push_str(&probe.to_string());
    added.push(')');
    let mut rewritten = String::new();
    rewritten.push_str(&body[..cut]);
    rewritten.push_str(&added);
    rewritten.push_str(&body[cut..]);
    rewritten.push_str(suffix);
    let rewrite = AutoRewrite { kind: "top".to_owned(), at: cut, replaced: 0, added };
    CappedSql { sql: rewritten, rewrite: Some(rewrite) }
}

/// 顶层 TOP n 的数量区间。TOP 与数量之间允许括号。
fn scan_top_n(sql: &str) -> Option<CountSpan> {
    let (words, _) = split::top_level_words("sqlserver", sql);
    let mut index = 0usize;
    while index < words.len() {
        if words[index].0 == "TOP" {
            let mut at = skip_noise(sql, words[index].2);
            let bytes = sql.as_bytes();
            if at < bytes.len() && bytes[at] == b'(' {
                at = skip_noise(sql, at + 1);
            }
            let (value, end) = read_u64(sql, at)?;
            return Some(CountSpan { value, span: (at, end) });
        }
        index += 1;
    }
    None
}
