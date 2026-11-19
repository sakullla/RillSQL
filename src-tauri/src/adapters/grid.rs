//! 表网格的会话读写（ADR-0 / R22，A4）：描述表结构、读一页、计数、逐条执行、外键与流式导出。
//!
//! 主代码（`grid/live.rs`）只调用这里的 [`CatalogSession`] 方法；各引擎的驱动读写与目录 SQL
//! 写在 `adapters/<engine>/grid_read.rs`。都在已经打开的会话上进行，不新开数据库连接。

use crate::connections::CatalogSession;
use crate::grid::{Cell, ForeignKey, TableSchema};

use super::{mysql, postgres, sqlite, sqlserver};

pub(crate) fn db_err(err: impl Into<crate::error_text::Reason>) -> String {
    let err = err.into();
    err.to_string()
}

/// 标识符已由结构化编译器加引号，字面量也已转义。这里只执行那条 SQL，不接收手写片段。
pub(crate) fn audited(statement: &str) -> sqlx::AssertSqlSafe<String> {
    sqlx::AssertSqlSafe(statement.to_owned())
}

pub(crate) fn other_database(current: &str, name: &str) -> String {
    format!("当前连接打开的是 {current}，没有另开连接读取 {name}")
}

pub(crate) fn cell_text(value: Option<String>) -> Cell {
    match value {
        Some(text) => Cell::Text(text),
        None => Cell::Null,
    }
}

fn no_channel() -> String {
    "这个连接没有连着可查询的数据库".to_owned()
}

/// 导出逐行写出的接收方。返回 false 表示已经停下（结果记在接收方里）。
pub trait ExportSink: Send {
    /// 读到列名后开始写（建文件、写表头）。
    fn start(&mut self, names: &[String]) -> bool;
    /// 读下一行之前的检查（取消、测试注入的失败）。
    fn proceed(&mut self) -> bool;
    /// 写一行。
    fn row(&mut self, cells: &[Cell]) -> bool;
    /// 读取失败：开始写之前与之后的处理不同（之后保留不完整的文件）。
    fn fail(&mut self, message: &str);
    /// 全部读完。
    fn finish(&mut self);
}

impl CatalogSession {
    /// 表或视图的结构：列、类型、主键/唯一键与自增列。`engine_hint` 区分共用会话类型的引擎。
    pub async fn grid_describe(&self, database: &str, schema: &str, table: &str, engine_hint: &str) -> Result<TableSchema, String> {
        match self {
            Self::Scripted => Err(no_channel()),
            // Redis 是键值专项引擎（R11）：不经 TableTarget→grid_describe 的 SQL 通路。
            Self::Redis(_) => Err("Redis 没有表数据网格；请使用键浏览器。".to_owned()),
            Self::Mongo(_) => Err("MongoDB 没有表数据网格；请使用集合文档浏览器。".to_owned()),
            Self::Sqlite { pool, .. } => sqlite::grid_read::describe(pool, database, table).await,
            Self::Mysql(pool) => mysql::grid_read::describe(pool, database, table, engine_hint).await,
            Self::Postgres(client) => postgres::grid_read::describe(client, database, schema, table).await,
            Self::Sqlserver(client) => sqlserver::grid_read::describe(client, database, schema, table).await,
        }
    }

    /// 按列数把一页行读成 Cell 矩阵。
    pub async fn grid_fetch_cells(&self, sql: &str, width: usize) -> Result<Vec<Vec<Cell>>, String> {
        match self {
            Self::Scripted => Err(no_channel()),
            Self::Redis(_) => Err("Redis 没有 SQL 行读取；数据经键浏览专项通路。".to_owned()),
            Self::Mongo(_) => Err("MongoDB 没有 SQL 行读取；文档经集合浏览器分页。".to_owned()),
            Self::Sqlite { pool, .. } => sqlite::grid_read::fetch_cells(pool, sql, width).await,
            Self::Mysql(pool) => mysql::grid_read::fetch_cells(pool, sql).await,
            Self::Postgres(client) => postgres::grid_read::fetch_cells(client, sql, width).await,
            Self::Sqlserver(client) => sqlserver::grid_read::fetch_cells(client, sql).await,
        }
    }

    /// 读一个计数。
    pub async fn grid_fetch_count(&self, sql: &str) -> Result<i64, String> {
        match self {
            Self::Scripted => Err(no_channel()),
            Self::Redis(_) => Err("Redis 没有 SQL 计数；键空间计数走专项通路。".to_owned()),
            Self::Mongo(_) => Err("MongoDB 没有 SQL 计数；文档计数走 estimatedDocumentCount。".to_owned()),
            Self::Sqlite { pool, .. } => sqlite::grid_read::fetch_count(pool, sql).await,
            Self::Mysql(pool) => mysql::grid_read::fetch_count(pool, sql).await,
            Self::Postgres(client) => postgres::grid_read::fetch_count(client, sql).await,
            Self::Sqlserver(client) => sqlserver::grid_read::fetch_count(client, sql).await,
        }
    }

    /// 执行一条写入，返回受影响行数。
    pub async fn grid_execute(&self, sql: &str) -> Result<u64, String> {
        match self {
            Self::Scripted => Err(no_channel()),
            Self::Redis(_) => Err("Redis 不执行 SQL；命令经 redis_exec 专项通道。".to_owned()),
            Self::Mongo(_) => Err("MongoDB 不执行 SQL；文档操作经 mongo_* 专项通道。".to_owned()),
            Self::Sqlite { pool, .. } => sqlite::grid_read::execute(pool, sql).await,
            Self::Mysql(pool) => mysql::grid_read::execute(pool, sql).await,
            Self::Postgres(client) => postgres::grid_read::execute(client, sql).await,
            Self::Sqlserver(client) => sqlserver::grid_read::execute(client, sql).await,
        }
    }

    /// 表级出向外键；键值/文档引擎与探针连接没有外键，空集不是失败。
    pub async fn grid_foreign_keys(&self, database: &str, schema: &str, table: &str) -> Result<Vec<ForeignKey>, String> {
        match self {
            Self::Scripted | Self::Redis(_) | Self::Mongo(_) => Ok(Vec::new()),
            Self::Sqlite { pool, .. } => sqlite::grid_read::foreign_keys(pool, database, table).await,
            Self::Mysql(pool) => mysql::grid_read::foreign_keys(pool, database, table).await,
            Self::Postgres(client) => postgres::grid_read::foreign_keys(client, database, schema, table).await,
            Self::Sqlserver(client) => sqlserver::grid_read::foreign_keys(client, database, schema, table).await,
        }
    }

    /// 补全 ON 条件用的外键：SQL Server 按目录语句直接读（不校验当前库），其余与网格同一套读取。
    pub async fn completion_foreign_keys(&self, database: &str, schema: &str, table: &str) -> Result<Vec<ForeignKey>, String> {
        match self {
            Self::Sqlserver(_) => {
                let rows = self.text_rows(&sqlserver::grid_read::foreign_key_sql(schema, table)).await?;
                let rows = rows
                    .into_iter()
                    .map(|row| row.into_iter().map(cell_text).collect::<Vec<_>>())
                    .collect::<Vec<_>>();
                Ok(crate::grid::assemble_foreign_keys(&rows))
            }
            _ => self.grid_foreign_keys(database, schema, table).await,
        }
    }

    /// 流式导出一条语句的结果。`known_names` 是已知的列名（表导出），否则按结果集的列名。
    pub async fn grid_export(&self, sql: &str, known_names: Option<Vec<String>>, sink: &mut dyn ExportSink) {
        match self {
            Self::Scripted => sink.fail(&no_channel()),
            // Redis 不参与五格式导出（capability 门控）；误入时明确拒绝。
            Self::Redis(_) => sink.fail("Redis 没有表数据导出；请使用键浏览器。"),
            Self::Mongo(_) => sink.fail("MongoDB 没有表数据导出；请使用集合文档浏览器。"),
            Self::Sqlite { pool, .. } => sqlite::grid_read::stream(pool, sql, known_names, sink).await,
            Self::Mysql(pool) => mysql::grid_read::stream(pool, sql, known_names, sink).await,
            Self::Postgres(client) => postgres::grid_read::stream(client, sql, known_names, sink).await,
            Self::Sqlserver(client) => sqlserver::grid_read::stream(client, sql, known_names, sink).await,
        }
    }
}
