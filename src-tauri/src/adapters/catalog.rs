//! 目录方言（ADR-0 / R22，A2）：元数据/对象列表/搜索/定义/列与索引/扩展信息/关系图的 SQL 与读取。
//!
//! 主代码（`catalog/`、`diagram/`、`app/shell/nav.rs`）只经 [`catalog_of`] 取得方言再调用；
//! 各引擎的 SQL 文本与会话读取写在 `adapters/<engine>/catalog*.rs`、`extras.rs`、`diagram.rs`。
//! 不提供的读取走这里的默认实现：会话与引擎不一致时给出同一句原因，不编造结果。

use crate::catalog::hierarchy::{ColumnInfo, IndexInfo};
use crate::catalog::model::{KindSql, MetadataSql, ObjectKind, ObjectRow, SchemaNode};
use crate::catalog::objects::ExtrasBody;
use crate::catalog::read::{empty_page, read_failure, ConnectionBody, ObjectPageBody, ObjectWindow};
use crate::connections::CatalogSession;
use crate::diagram::Diagram;
use crate::query::assist::ColumnMeta;

use super::adapter;
use crate::connections::model::Engine;

/// 会话与引擎对不上时的原因（读取库表）。
pub(crate) fn mismatch() -> String {
    read_failure("连接类型和引擎不一致")
}

/// 会话与引擎对不上时的库表结果。
pub(crate) fn mismatch_body() -> ConnectionBody {
    ConnectionBody { error: Some(mismatch()), foreign_keys: None, databases: Vec::new() }
}

/// 关系图读取的会话错误：探针会话没有数据库，其余是会话与引擎不一致。
pub(crate) fn diagram_session_error(session: &CatalogSession) -> String {
    if session.is_scripted() {
        "这个连接没有连着可查询的数据库".to_owned()
    } else {
        "连接类型和引擎不一致".to_owned()
    }
}

/// SQL 字面量：单引号翻倍。目录读取只接受已读到的名字，这里仍然转义，避免对象名里的引号跑出语句。
pub(crate) fn literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// information_schema 的主键列查询（MySQL 家族、PostgreSQL、SQL Server 共用）。`scope` 是已转义的库/模式字面量。
pub(crate) fn information_schema_primary_key_sql(scope: &str, table: &str) -> String {
    format!(
        "SELECT kcu.column_name FROM information_schema.table_constraints tc JOIN information_schema.key_column_usage kcu ON kcu.constraint_name = tc.constraint_name AND kcu.table_schema = tc.table_schema AND kcu.table_name = tc.table_name WHERE tc.constraint_type = 'PRIMARY KEY' AND tc.table_schema = {scope} AND tc.table_name = {}",
        literal(table)
    )
}

pub(crate) fn cell(rows: &[Vec<Option<String>>], row: usize, column: usize) -> String {
    rows.get(row).and_then(|row| row.get(column)).and_then(|cell| cell.clone()).unwrap_or_default()
}

pub(crate) fn yes(text: &str) -> bool {
    matches!(text.trim().to_ascii_uppercase().as_str(), "YES" | "TRUE" | "1" | "T")
}

/// 索引行（name, column, unique, primary, ordinal）按索引名聚合，列按序号排好。
pub(crate) fn group_index_rows(rows: impl IntoIterator<Item = (String, String, bool, bool, i64)>) -> Vec<IndexInfo> {
    let mut indexes: Vec<IndexInfo> = Vec::new();
    let mut positions: Vec<Vec<(i64, String)>> = Vec::new();
    for (name, column, unique, primary, ordinal) in rows {
        if name.is_empty() {
            continue;
        }
        match indexes.iter().position(|item| item.name == name) {
            Some(at) => {
                if !column.is_empty() {
                    positions[at].push((ordinal, column));
                }
                indexes[at].unique |= unique;
                indexes[at].primary |= primary;
            }
            None => {
                indexes.push(IndexInfo { name, columns: Vec::new(), unique, primary });
                positions.push(if column.is_empty() { Vec::new() } else { vec![(ordinal, column)] });
            }
        }
    }
    for (at, mut items) in positions.into_iter().enumerate() {
        items.sort_by_key(|(ordinal, _)| *ordinal);
        indexes[at].columns = items.into_iter().map(|(_, column)| column).collect();
    }
    indexes
}

/// 目录方言。SQL 生成是纯函数；读取在已打开的会话上进行，不新开连接。
#[async_trait::async_trait]
pub trait CatalogDialect: Sync {
    // ---- 树形与能力 ----

    /// 引擎真正拥有的独立对象种类（列和索引不算）。
    fn listed_kinds(&self) -> &'static [ObjectKind] {
        &[]
    }

    /// 复制/插入用限定名的容器段：默认是模式；按库分的引擎是库；单文件引擎没有。
    fn name_qualifier<'a>(&self, _database: &'a str, schema: &'a str) -> &'a str {
        schema
    }

    /// 名字的第一段是库（而不是模式）：`库.表` 指向另一个库。
    fn qualifier_is_database(&self) -> bool {
        false
    }

    /// 没有指定模式时对象所在的默认模式。
    fn default_schema(&self) -> Option<&'static str> {
        None
    }

    /// 这种对象能不能读到定义（菜单据此决定“查看 DDL”是否可点）。
    fn definition_available(&self, _kind: ObjectKind) -> bool {
        false
    }

    /// 表定义按元数据重建（数据库没有保存建表原文）。
    fn table_is_rebuilt(&self) -> bool {
        false
    }

    // ---- 元数据 SQL ----

    fn metadata_sql(&self) -> MetadataSql {
        MetadataSql { databases: "", schemas: None, objects: "", foreign_keys: None }
    }

    fn object_count_sql(&self) -> &'static str {
        ""
    }

    /// 每一类单独计数、单独取窗口，这样某一类失败不会清掉其他类。
    fn kind_sql(&self, _kind: ObjectKind) -> KindSql {
        KindSql { count: "", list: "" }
    }

    /// 列查询：name, type, nullable, default, comment（单文件引擎按自己的行形状，见 `columns_from_rows`）。
    fn columns_sql(&self, _database: &str, _schema: &str, _table: &str) -> Option<String> {
        None
    }

    /// 一个库/模式里有注释的表和视图：(name, comment)。
    fn table_comments_sql(&self, _database: &str, _schema: &str) -> Option<String> {
        None
    }

    /// `table_comments_sql` 里对象名所在的列，按单个对象过滤时用。
    fn table_comment_name_column(&self) -> &'static str {
        "table_name"
    }

    /// 主键列查询。没有主键就是空集，不是失败。
    fn primary_key_sql(&self, _database: &str, _schema: &str, _table: &str) -> Option<String> {
        None
    }

    fn indexes_sql(&self, _database: &str, _schema: &str, _table: &str) -> Option<String> {
        None
    }

    /// 索引清单不带列的引擎：逐个索引读列的查询（列名在第 3 列）。
    fn index_columns_sql(&self, _index: &str) -> Option<String> {
        None
    }

    /// 读非当前库时把目录查询限定到目标库（同一条会话、不发 USE）。默认原样。
    fn scope_sql(&self, _database: &str, sql: &str) -> String {
        sql.to_owned()
    }

    /// 列查询结果：默认是 information_schema 的位置列 name, type, nullable, default。
    fn columns_from_rows(&self, rows: &[Vec<Option<String>>]) -> Vec<ColumnInfo> {
        (0..rows.len())
            .map(|index| ColumnInfo {
                name: cell(rows, index, 0),
                data_type: cell(rows, index, 1),
                nullable: yes(&cell(rows, index, 2)),
                primary: false,
            })
            .filter(|column| !column.name.is_empty())
            .collect()
    }

    /// 补全和悬停用的列元数据：在列查询的位置约定上再取注释（第 5 列）。读不到的类型和注释留空，不编造。
    fn column_meta_from_rows(&self, rows: &[Vec<Option<String>>]) -> Vec<ColumnMeta> {
        (0..rows.len())
            .filter_map(|index| {
                let name = cell(rows, index, 0);
                if name.is_empty() {
                    return None;
                }
                let flag = cell(rows, index, 2);
                let nullable = if flag.trim().is_empty() { None } else { Some(yes(&flag)) };
                Some(ColumnMeta {
                    name,
                    type_name: cell(rows, index, 1),
                    nullable,
                    comment: cell(rows, index, 4),
                    primary: false,
                    foreign_key: false,
                })
            })
            .collect()
    }

    /// 索引查询结果：默认位置列 name, column, unique, primary, ordinal。
    fn indexes_from_rows(&self, rows: &[Vec<Option<String>>]) -> Vec<IndexInfo> {
        group_index_rows((0..rows.len()).map(|index| {
            (
                cell(rows, index, 0),
                cell(rows, index, 1),
                yes(&cell(rows, index, 2)),
                yes(&cell(rows, index, 3)),
                cell(rows, index, 4).parse::<i64>().unwrap_or(0),
            )
        }))
    }

    // ---- 会话读取 ----

    /// 库、模式与每层第一页对象。
    async fn read_connection(&self, _session: &CatalogSession, _page: usize, _windows: &[ObjectWindow]) -> ConnectionBody {
        mismatch_body()
    }

    /// 只读库名（及当前库的模式名）；对象留给按页读取。
    async fn read_outline(&self, _session: &CatalogSession) -> ConnectionBody {
        mismatch_body()
    }

    /// 单个库的模式清单（不含对象）：库节点展开时按库读一次。
    async fn read_database_outline(&self, _session: &CatalogSession, _database: &str) -> Result<Vec<SchemaNode>, String> {
        Err(read_failure("这个引擎的库层级已经随连接一次读出，不需要按库再读"))
    }

    /// 只读一种对象的一页。`limit` 已按默认页大小补齐。
    #[allow(clippy::too_many_arguments)]
    async fn read_kind_page(
        &self,
        _session: &CatalogSession,
        _database: &str,
        _schema: &str,
        _kind: ObjectKind,
        _offset: usize,
        _limit: usize,
    ) -> ObjectPageBody {
        empty_page(mismatch())
    }

    /// 导航用的一页：返回 (页, 总数是否确知)。默认按带计数的分类页读取。`limit` 未补齐。
    #[allow(clippy::too_many_arguments)]
    async fn read_navigation_kind_page(
        &self,
        session: &CatalogSession,
        database: &str,
        schema: &str,
        kind: ObjectKind,
        offset: usize,
        limit: usize,
    ) -> (ObjectPageBody, bool) {
        let limit = crate::catalog::read::read_limit(limit, 0);
        (self.read_kind_page(session, database, schema, kind, offset, limit).await, true)
    }

    /// 一个库/模式的对象一页（各类合并排序）。`limit` 已按默认页大小补齐。
    async fn read_object_page(&self, _session: &CatalogSession, _database: &str, _schema: &str, _offset: usize, _limit: usize) -> ObjectPageBody {
        empty_page(mismatch())
    }

    /// 补全用的轻量对象名单（一次读完整个库）。None 表示这个引擎按对象分页读取。
    async fn read_completion_objects(&self, _session: &CatalogSession, _database: &str) -> Option<Result<Vec<ObjectRow>, String>> {
        None
    }

    /// 全局搜索：`pattern` 是已转义、两端带 `%` 的 LIKE 模式。返回 (库, 模式, 对象)。
    async fn search_objects(&self, _session: &CatalogSession, _pattern: &str) -> Vec<(String, String, ObjectRow)> {
        Vec::new()
    }

    /// 只读定义原文。
    #[allow(clippy::too_many_arguments)]
    async fn read_definition(
        &self,
        _session: &CatalogSession,
        _database: &str,
        _schema: &str,
        _name: &str,
        _kind: &str,
        _signature: &str,
        _relation: &str,
    ) -> Result<String, String> {
        Err(crate::catalog::read::definition_failure("连接类型和引擎不一致"))
    }

    /// 连接级对象详情（扩展、序列、事件、分区、PRAGMA、附加库）。不适用的节为空清单且不报错。
    async fn read_extras(&self, _session: &CatalogSession, _database: &str, _schema: &str) -> ExtrasBody {
        ExtrasBody::default()
    }

    /// 关系图：只读基表、列与外键。
    async fn read_diagram(&self, session: &CatalogSession, _database: &str, _schema: &str) -> Result<Diagram, String> {
        Err(diagram_session_error(session))
    }

    /// 关系图使用的全部语句（只读校验用）。
    fn diagram_statements(&self) -> &'static [&'static str] {
        &[]
    }
}

/// 没有目录能力的兜底（目前每个引擎都提供目录方言；保留它让门面不必 unwrap）。
struct NoCatalog;

impl CatalogDialect for NoCatalog {}

/// 门面：引擎的目录方言。
pub fn catalog_of(engine: Engine) -> &'static dyn CatalogDialect {
    adapter(engine).catalog().unwrap_or(&NoCatalog)
}
