//! 在已经打开的会话上读取库表。这里不新开数据库连接。
//!
//! 这里是引擎无关的门面：各引擎的元数据 SQL（metadata_sql、kind_sql）与会话读取写在
//! `adapters/<engine>/catalog*.rs`，经 [`catalog_of`] 取得目录方言后调用。

use crate::adapters::catalog_of;
use crate::catalog::model::{DatabaseNode, Engine, KindReport, ObjectKind, ObjectRow, SchemaNode, DEFAULT_OBJECT_PAGE};
use crate::connections::CatalogSession;

#[cfg(test)]
pub use crate::adapters::mysql::catalog_read::read_completion_objects as read_mysql_completion_objects;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectWindow {
    pub database: String,
    pub schema: String,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionBody {
    pub error: Option<String>,
    pub foreign_keys: Option<bool>,
    pub databases: Vec<DatabaseNode>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectPageBody {
    pub objects: Vec<ObjectRow>,
    pub total: usize,
    pub error: Option<String>,
    pub kind_reports: Vec<KindReport>,
}

pub(crate) fn read_limit(requested: usize, page: usize) -> usize {
    let page = if page == 0 { DEFAULT_OBJECT_PAGE } else { page };
    if requested == 0 { page } else { requested }
}

pub(crate) fn requested_limit(database: &str, schema: &str, windows: &[ObjectWindow], page: usize) -> usize {
    let requested = windows
        .iter()
        .filter(|item| item.database == database && item.schema == schema)
        .map(|item| item.limit)
        .filter(|limit| *limit > 0)
        .max()
        .unwrap_or(0);
    read_limit(requested, page)
}

pub(crate) fn read_failure(err: impl Into<crate::error_text::Reason>) -> String {
    let err = err.into();
    format!("读取库表失败：{err}")
}

pub(crate) fn definition_failure(err: impl Into<crate::error_text::Reason>) -> String {
    let err = err.into();
    format!("读取定义失败：{err}")
}

fn scripted_message() -> String {
    read_failure("这个连接没有连着可查询的数据库")
}

pub(crate) fn other_database_message(current: &str, name: &str) -> String {
    format!("当前会话连在 {current}，没有另开连接读取 {name}")
}

pub(crate) fn empty_page(message: impl Into<String>) -> ObjectPageBody {
    ObjectPageBody {
        objects: Vec::new(),
        total: 0,
        error: Some(message.into()),
        kind_reports: Vec::new(),
    }
}

fn scripted_body() -> ConnectionBody {
    ConnectionBody {
        error: Some(scripted_message()),
        foreign_keys: None,
        databases: Vec::new(),
    }
}

/// 库、模式与每层第一页对象。
pub async fn read_catalog_session(engine: Engine, session: &CatalogSession, page: usize, windows: &[ObjectWindow]) -> ConnectionBody {
    if session.is_scripted() {
        return scripted_body();
    }
    catalog_of(engine).read_connection(session, page, windows).await
}

/// 只读取库名；分模式的引擎再读取当前库的模式名。对象留给 catalog_page。
pub async fn read_catalog_outline(engine: Engine, session: &CatalogSession) -> ConnectionBody {
    if session.is_scripted() {
        return scripted_body();
    }
    catalog_of(engine).read_outline(session).await
}

/// 只重读一种对象：局部失败的分组重试时不碰兄弟类型。
pub async fn read_object_kind_page(
    session: &CatalogSession,
    engine: Engine,
    database: &str,
    schema: &str,
    kind: ObjectKind,
    offset: usize,
    limit: usize,
) -> ObjectPageBody {
    let limit = read_limit(limit, 0);
    if session.is_scripted() {
        return empty_page(scripted_message());
    }
    catalog_of(engine).read_kind_page(session, database, schema, kind, offset, limit).await
}

/// 导航只需要一个窗口和是否还有下一页。返回 (页, 总数是否确知)；
/// 能省掉计数与统计读取的引擎由适配器给出更轻的读法，其它引擎保留原计数语义。
pub async fn read_navigation_kind_page(
    session: &CatalogSession, engine: Engine, database: &str, schema: &str,
    kind: ObjectKind, offset: usize, limit: usize,
) -> (ObjectPageBody, bool) {
    if session.is_scripted() {
        return (empty_page(scripted_message()), true);
    }
    catalog_of(engine).read_navigation_kind_page(session, database, schema, kind, offset, limit).await
}

fn escape_like(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('%');
    for ch in value.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('%');
    out
}

/// 侧栏搜索每个库最多先给出的命中数；超出的库带截断标记，用户展开时再按库分页取回。
pub const SEARCH_PER_DATABASE: usize = 200;
/// 一个连接一次搜索最多给出的命中数（所有库合计）。
pub const SEARCH_PER_CONNECTION: usize = 2_000;

/// 一个连接的一次侧栏搜索结果。命中、截断和失败分开报告：出错或超时不再变成“没有命中”。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CatalogSearch {
    /// (库, 模式, 对象)。
    pub hits: Vec<(String, String, ObjectRow)>,
    /// 命中超过名额、还有更多没给出的库：(库, 还剩多少；None 表示数量未知)。
    pub truncated: Vec<(String, Option<usize>)>,
    /// 按库报告的失败：(库, 原因)；库为空表示整个连接的搜索失败。
    pub failures: Vec<(String, String)>,
    /// 这次没有搜索到的库（例如会话只能读自己所在的库），由界面说明。
    pub unsearched: Vec<String>,
}

impl CatalogSearch {
    pub fn failed(database: &str, message: impl Into<String>) -> Self {
        Self { failures: vec![(database.to_owned(), message.into())], ..Self::default() }
    }
}

/// 按库分配名额：库按名字顺序，每库最多 `per_database`，合计最多 `per_connection`。
/// 输入是每个库的命中总数，返回每个库这次取多少条。
pub fn search_quota(counts: &[(String, usize)], per_database: usize, per_connection: usize) -> Vec<(String, usize)> {
    let mut left = per_connection;
    counts
        .iter()
        .map(|(database, count)| {
            let take = (*count).min(per_database).min(left);
            left -= take;
            (database.clone(), take)
        })
        .collect()
}

/// 资源管理器全局搜索：跨未展开的库与超过第一页的对象直接向数据库检索匹配名字，
/// 解决库里有几百上千张表时“前 100 张以外的表搜不出来”的问题。
pub async fn search_catalog_objects(session: &CatalogSession, engine: Engine, query: &str) -> CatalogSearch {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return CatalogSearch::default();
    }
    let pattern = escape_like(trimmed);
    catalog_of(engine).search_objects(session, &pattern).await
}

/// 展开截断的库：在一个库里从 `offset` 起再取一页命中。
pub async fn search_catalog_database(session: &CatalogSession, engine: Engine, query: &str, database: &str, offset: usize) -> CatalogSearch {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return CatalogSearch::default();
    }
    let pattern = escape_like(trimmed);
    catalog_of(engine).search_database_objects(session, database, &pattern, offset, SEARCH_PER_DATABASE).await
}

pub async fn read_object_page(session: &CatalogSession, engine: Engine, database: &str, schema: &str, offset: usize, limit: usize) -> ObjectPageBody {
    let limit = read_limit(limit, 0);
    if session.is_scripted() {
        return empty_page(scripted_message());
    }
    catalog_of(engine).read_object_page(session, database, schema, offset, limit).await
}

#[allow(clippy::too_many_arguments)]
pub async fn read_definition(
    session: &CatalogSession,
    engine: Engine,
    database: &str,
    schema: &str,
    name: &str,
    kind: &str,
    signature: &str,
    relation: &str,
) -> Result<String, String> {
    if name.is_empty() {
        return Err(definition_failure("缺少对象名"));
    }
    if session.is_scripted() {
        return Err(definition_failure("这个连接没有连着可查询的数据库"));
    }
    catalog_of(engine).read_definition(session, database, schema, name, kind, signature, relation).await
}

/// 单个库的 schema 清单（不含对象）：库节点展开时按库读一次。
/// 会话只连着一个库的引擎由调用方另开连到目标库的临时会话（见 Service::open_ephemeral_postgres）；
/// 能跨库读目录的引擎在原会话上限定到目标库读。对象仍按层的分组展开时读取。
pub async fn read_database_outline(engine: Engine, session: &CatalogSession, database: &str) -> Result<Vec<SchemaNode>, String> {
    if database.trim().is_empty() {
        return Err(read_failure("需要数据库名"));
    }
    catalog_of(engine).read_database_outline(session, database).await
}

/// 会话当前库以外的库：只有一个库名，没有错误也不是“读不到”——它的模式与对象
/// 等到用户展开这个库时按库另读（PostgreSQL 另开临时会话，SQL Server 用三段名）。
pub(crate) fn unloaded_database(name: &str) -> DatabaseNode {
    DatabaseNode {
        name: name.to_owned(),
        schemas: Vec::new(),
        error: None,
    }
}

pub(crate) fn assemble_postgres(names: &[String], current: &str, schemas: Result<Vec<SchemaNode>, String>) -> Vec<DatabaseNode> {
    names
        .iter()
        .map(|name| {
            if name == current {
                match &schemas {
                    Ok(nodes) => DatabaseNode {
                        name: name.clone(),
                        schemas: nodes.clone(),
                        error: None,
                    },
                    Err(message) => DatabaseNode {
                        name: name.clone(),
                        schemas: Vec::new(),
                        error: Some(message.clone()),
                    },
                }
            } else {
                unloaded_database(name)
            }
        })
        .collect()
}

#[allow(clippy::type_complexity)]
pub(crate) fn finish_kinds(parts: Vec<(ObjectKind, Result<(usize, Vec<ObjectRow>), String>)>, offset: usize, limit: usize) -> (Vec<ObjectRow>, usize, Vec<KindReport>) {
    let mut reports = Vec::new();
    let mut rows = Vec::new();
    let mut total = 0usize;
    for (kind, result) in parts {
        match result {
            Ok((kind_total, objects)) => {
                total = total.saturating_add(kind_total);
                rows.extend(objects);
                reports.push(KindReport {
                    kind,
                    total: kind_total,
                    error: None,
                });
            }
            Err(message) => reports.push(KindReport {
                kind,
                total: 0,
                error: Some(message),
            }),
        }
    }
    rows.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.signature.cmp(&right.signature))
            .then_with(|| left.relation.cmp(&right.relation))
            .then_with(|| left.kind_rank().cmp(&right.kind_rank()))
    });
    let objects = rows.into_iter().skip(offset).take(limit).collect();
    (objects, total, reports)
}

trait KindRank {
    fn kind_rank(&self) -> u8;
}

impl KindRank for ObjectRow {
    fn kind_rank(&self) -> u8 {
        match self.kind {
            ObjectKind::Table => 0,
            ObjectKind::View => 1,
            ObjectKind::Function => 2,
            ObjectKind::Procedure => 3,
            ObjectKind::Trigger => 4,
            ObjectKind::Sequence => 5,
            ObjectKind::Enum => 6,
            ObjectKind::Domain => 7,
            ObjectKind::Extension => 8,
            ObjectKind::Event => 9,
        }
    }
}
