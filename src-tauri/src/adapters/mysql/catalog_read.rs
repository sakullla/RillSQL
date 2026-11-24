//! MySQL 家族在已打开会话上的目录读取：库清单、分类分页、导航窗口、搜索、补全清单与只读定义。

use mysql_async::prelude::Queryable;

use crate::catalog::model::{known_row_estimate, parse_kind, DatabaseNode, ObjectKind, ObjectRow, SchemaNode};
use crate::catalog::read::{definition_failure, finish_kinds, read_failure, read_limit, requested_limit, ObjectPageBody, ObjectWindow};
use crate::connections::model::Engine;

use super::catalog::{kind_sql, listed_kinds, metadata_sql};

pub(super) async fn read_mysql(pool: &mysql_async::Pool, engine: Engine, page: usize, windows: &[ObjectWindow]) -> Result<Vec<DatabaseNode>, String> {
    let sql = metadata_sql();
    let mut conn = pool.get_conn().await.map_err(read_failure)?;
    let rows: Vec<mysql_async::Row> = conn.query(sql.databases).await.map_err(read_failure)?;
    let mut names = Vec::new();
    for row in &rows {
        names.push(mysql_string(row, 0)?);
    }
    let mut databases = Vec::new();
    for name in names {
        let limit = requested_limit(&name, "", windows, page);
        databases.push(read_mysql_database(&mut conn, engine, &name, 0, limit).await);
    }
    Ok(databases)
}

pub(super) async fn read_mysql_outline(pool: &mysql_async::Pool) -> Result<Vec<DatabaseNode>, String> {
    let sql = metadata_sql();
    let mut conn = pool.get_conn().await.map_err(read_failure)?;
    let rows: Vec<mysql_async::Row> = conn.query(sql.databases).await.map_err(read_failure)?;
    let mut databases = Vec::new();
    for row in &rows {
        databases.push(DatabaseNode {
            name: mysql_string(row, 0)?,
            error: None,
            schemas: vec![SchemaNode::empty("")],
        });
    }
    Ok(databases)
}

/// 展开一个库：各类对象分别借一条元数据连接并发读取。远端高延迟时，
/// 六类对象 × (计数 + 列表) 串行要走十几次往返；并发后只等最慢的那一类。
pub(super) async fn read_mysql_page(pool: &mysql_async::Pool, engine: Engine, database: &str, offset: usize, limit: usize) -> ObjectPageBody {
    let window = offset.saturating_add(limit);
    let reads = listed_kinds(engine).iter().map(|kind| async move {
        let result = match pool.get_conn().await {
            Ok(mut conn) => read_mysql_kind(&mut conn, engine, database, *kind, window).await,
            Err(err) => Err(read_failure(err)),
        };
        (*kind, result)
    });
    let parts = futures::future::join_all(reads).await;
    let (objects, total, reports) = finish_kinds(parts, offset, limit);
    ObjectPageBody { objects, total, error: None, kind_reports: reports }
}

async fn read_mysql_database(conn: &mut mysql_async::Conn, engine: Engine, database: &str, offset: usize, limit: usize) -> DatabaseNode {
    let schema = read_mysql_objects(conn, engine, database, offset, limit).await;
    DatabaseNode {
        name: database.to_owned(),
        error: None,
        schemas: vec![schema],
    }
}

async fn read_mysql_objects(conn: &mut mysql_async::Conn, engine: Engine, database: &str, offset: usize, limit: usize) -> SchemaNode {
    let window = offset.saturating_add(limit);
    let mut parts = Vec::new();
    for kind in listed_kinds(engine) {
        parts.push((*kind, read_mysql_kind(conn, engine, database, *kind, window).await));
    }
    let (objects, total, reports) = finish_kinds(parts, offset, limit);
    SchemaNode {
        name: String::new(),
        objects,
        total,
        error: None,
        kind_reports: reports,
    }
}

async fn read_mysql_kind(conn: &mut mysql_async::Conn, engine: Engine, database: &str, kind: ObjectKind, window: usize) -> Result<(usize, Vec<ObjectRow>), String> {
    read_mysql_kind_slice(conn, engine, database, kind, 0, window).await
}

/// 对象编辑器读取单个事件/例程/触发器的完整状态（R5 / R6 / R7）。
///
/// 读的是对象自己的定义与状态，不读某张表的结构；读不到就返回原因，编辑器据此阻断编辑，
/// 不用空值覆盖服务器对象。桥接方向是从这里到 object_read：对象读回仍只写在 object_read 一份。
pub(crate) async fn read_object_detail(
    pool: &mysql_async::Pool,
    database: &str,
    kind: ObjectKind,
    name: &str,
) -> Result<crate::catalog::objects::ObjectDetail, String> {
    use crate::catalog::objects::ObjectDetail;
    use crate::ddl::model::RoutineKind;
    match kind {
        ObjectKind::Event => Ok(ObjectDetail::Event(super::object_read::load_event(pool, database, name).await?)),
        ObjectKind::Function => {
            Ok(ObjectDetail::Routine(super::object_read::load_routine(pool, database, RoutineKind::Function, name).await?))
        }
        ObjectKind::Procedure => {
            Ok(ObjectDetail::Routine(super::object_read::load_routine(pool, database, RoutineKind::Procedure, name).await?))
        }
        ObjectKind::Trigger => Ok(ObjectDetail::Trigger(super::object_read::load_trigger(pool, database, name).await?)),
        other => Err(format!("{} 不是可以在这里编辑的对象。", crate::catalog::model::kind_name(other))),
    }
}

/// 会话守卫 + 读回：把“哪类会话读哪类对象”的判断留在适配器层，主代码只做通用拼装。
///
/// 不是 MySQL 家族的会话直接给出原因，不编造空对象。
pub(crate) async fn read_object_detail_session(
    session: &crate::connections::CatalogSession,
    engine: Engine,
    database: &str,
    kind: ObjectKind,
    name: &str,
) -> Result<crate::catalog::objects::ObjectDetail, String> {
    let crate::connections::CatalogSession::Mysql(pool) = session else {
        return Err(format!("{} 没有事件、函数、存储过程与触发器，这里不读。", engine.as_str()));
    };
    read_object_detail(pool, database, kind, name).await
}

pub(super) async fn read_mysql_kind_slice(
    conn: &mut mysql_async::Conn,
    engine: Engine,
    database: &str,
    kind: ObjectKind,
    offset: usize,
    window: usize,
) -> Result<(usize, Vec<ObjectRow>), String> {
    let sql = kind_sql(engine, kind);
    let count_row: Option<mysql_async::Row> = conn.exec_first(sql.count, (database.to_owned(),)).await.map_err(read_failure)?;
    let total = match count_row.as_ref().and_then(|row| mysql_i64(row, 0)) {
        Some(value) => usize::try_from(value).map_err(|_| read_failure("对象数量无效"))?,
        None => return Err(read_failure("没有对象数量")),
    };
    let rows: Vec<mysql_async::Row> = conn
        .exec(
            format!("{} OFFSET ?", sql.list),
            (database.to_owned(), i64::try_from(window).unwrap_or(i64::MAX), i64::try_from(offset).unwrap_or(i64::MAX)),
        )
        .await
        .map_err(read_failure)?;
    let mut objects = Vec::new();
    for row in &rows {
        objects.push(mysql_object(row)?);
    }
    Ok((total, objects))
}

/// 导航窗口：多取一行判断是否还有下一页，总数按已读到的位置推算。
pub(super) async fn read_navigation_page(
    pool: &mysql_async::Pool,
    engine: Engine,
    database: &str,
    kind: ObjectKind,
    offset: usize,
    limit: usize,
) -> (ObjectPageBody, bool) {
    let limit = read_limit(limit, 0);
    let result = async {
        let mut conn = pool.get_conn().await.map_err(read_failure)?;
        let sql = mysql_navigation_page_sql(engine, database, kind, offset, limit);
        let rows: Vec<mysql_async::Row> = conn.query(sql).await.map_err(read_failure)?;
        rows.iter().map(mysql_object).collect::<Result<Vec<_>, _>>()
    }
    .await;
    match result {
        Ok(mut objects) => {
            let has_more = objects.len() > limit;
            objects.truncate(limit);
            let total = offset.saturating_add(objects.len()).saturating_add(usize::from(has_more));
            (ObjectPageBody { objects, total, error: None, kind_reports: Vec::new() }, !has_more)
        }
        Err(error) => (crate::catalog::read::empty_page(error), false),
    }
}

// 十六进制字符串不受 NO_BACKSLASH_ESCAPES / ANSI_QUOTES 影响，也不把库名当 SQL。
fn mysql_catalog_literal(value: &str) -> String {
    use std::fmt::Write;
    let mut hex = String::with_capacity(value.len() * 2);
    for byte in value.as_bytes() {
        write!(&mut hex, "{byte:02x}").expect("write string");
    }
    format!("CONVERT(X'{hex}' USING utf8mb4)")
}

fn mysql_navigation_page_sql(engine: Engine, database: &str, kind: ObjectKind, offset: usize, limit: usize) -> String {
    // 只替换模板中的两个参数；替换值只含十六进制字符。
    let list = kind_sql(engine, kind).list.replace("table_rows", "NULL");
    let list = list.replacen('?', &mysql_catalog_literal(database), 1).replacen('?', &limit.saturating_add(1).to_string(), 1);
    format!("{list} OFFSET {offset}")
}

/// 补全用的轻量名字清单。一次读取所选库，不逐类 COUNT、不读统计或定义，
/// 也不把 5,000 个对象的截断页错误地记作完整清单。
pub async fn read_completion_objects(pool: &mysql_async::Pool, database: &str) -> Result<Vec<ObjectRow>, String> {
    let mut conn = pool.get_conn().await.map_err(read_failure)?;
    let db = mysql_catalog_literal(database);
    let sql = format!("SELECT table_name, CASE table_type WHEN 'VIEW' THEN 'view' WHEN 'SEQUENCE' THEN 'sequence' ELSE 'table' END AS kind, NULL, '', '' FROM information_schema.tables WHERE table_schema = {db} AND table_type IN ('BASE TABLE', 'VIEW', 'SEQUENCE') \
        UNION ALL SELECT routine_name, LOWER(routine_type), NULL, '', '' FROM information_schema.routines WHERE routine_schema = {db} \
        UNION ALL SELECT trigger_name, 'trigger', NULL, '', event_object_table FROM information_schema.triggers WHERE trigger_schema = {db} \
        UNION ALL SELECT event_name, 'event', NULL, '', '' FROM information_schema.events WHERE event_schema = {db}");
    let rows: Vec<mysql_async::Row> = conn.query(sql).await.map_err(read_failure)?;
    rows.iter().map(mysql_object).collect()
}

/// 资源管理器全局搜索：跨库检索表、视图、序列与例程的名字。
pub(super) async fn search(pool: &mysql_async::Pool, pattern: &str) -> Vec<(String, String, ObjectRow)> {
    const LIMIT: usize = 300;
    let Ok(mut conn) = pool.get_conn().await else {
        return Vec::new();
    };
    let pat = mysql_catalog_literal(&pattern.to_lowercase());
    let sql = format!(
        "SELECT table_schema, table_name, CASE table_type WHEN 'VIEW' THEN 'view' WHEN 'SEQUENCE' THEN 'sequence' ELSE 'table' END AS kind, '' AS sig, '' AS rel \
         FROM information_schema.tables \
         WHERE table_schema NOT IN ('information_schema', 'performance_schema', 'mysql', 'sys') \
           AND LOWER(table_name) LIKE {pat} ESCAPE '\\\\' \
         UNION ALL \
         SELECT routine_schema, routine_name, LOWER(routine_type), '', '' \
         FROM information_schema.routines \
         WHERE routine_schema NOT IN ('information_schema', 'performance_schema', 'mysql', 'sys') \
           AND LOWER(routine_name) LIKE {pat} ESCAPE '\\\\' \
         ORDER BY 1, 2 LIMIT {LIMIT}"
    );
    let Ok(rows) = conn.query::<mysql_async::Row, _>(sql).await else {
        return Vec::new();
    };
    let mut hits = Vec::with_capacity(rows.len());
    for row in &rows {
        let Ok(db) = mysql_string(row, 0) else { continue };
        let Ok(name) = mysql_string(row, 1) else { continue };
        let Ok(kind) = mysql_string(row, 2) else { continue };
        hits.push((
            db,
            String::new(),
            ObjectRow {
                kind: parse_kind(&kind),
                name,
                estimated_rows: None,
                stale: false,
                signature: mysql_text(row, 3),
                relation: mysql_text(row, 4),
                materialized: false,
            },
        ));
    }
    hits
}

pub(super) async fn read_mysql_definition(pool: &mysql_async::Pool, engine: Engine, database: &str, name: &str, kind: &str) -> Result<String, String> {
    let statement = match kind {
        "function" => "FUNCTION",
        "procedure" => "PROCEDURE",
        "trigger" => "TRIGGER",
        // 事件的事件体只读：这里只给 SHOW CREATE EVENT 的原文。
        "event" => "EVENT",
        // 表与视图的 SHOW CREATE 原文在索引 1。
        "table" => "TABLE",
        "view" => "VIEW",
        "sequence" if engine == Engine::Mariadb => "SEQUENCE",
        "sequence" | "enum" | "domain" => {
            return Err(definition_failure(if engine == Engine::Mariadb {
                "MariaDB 没有独立的枚举或域"
            } else {
                "MySQL 没有独立的序列、枚举或域"
            }));
        }
        _ => return Err(definition_failure("只有表、视图、函数、存储过程、触发器和事件可以读取只读定义")),
    };
    if database.is_empty() {
        return Err(definition_failure("需要数据库名"));
    }
    // 函数与存储过程：SHOW CREATE 读不到时回退元数据重建，仍读不到给出权限原因（R6）。
    match statement {
        "FUNCTION" => return super::routine_definition::read_routine_definition(pool, engine, database, name, crate::ddl::model::RoutineKind::Function).await,
        "PROCEDURE" => return super::routine_definition::read_routine_definition(pool, engine, database, name, crate::ddl::model::RoutineKind::Procedure).await,
        _ => {}
    }
    let sql = format!("SHOW CREATE {statement} {}.{}", quote_mysql(database), quote_mysql(name));
    let mut conn = pool.get_conn().await.map_err(definition_failure)?;
    let row: Option<mysql_async::Row> = conn.query_first(sql).await.map_err(definition_failure)?;
    let row = row.ok_or_else(|| definition_failure("没有读到定义"))?;
    // SHOW CREATE SEQUENCE 的语句在索引 1；函数、过程和触发器在索引 2。
    // SHOW CREATE EVENT 的列名是 Create Event（不同版本列序会变），按列名定位，读不到时退回索引 3。
    let column = match statement {
        "SEQUENCE" | "TABLE" | "VIEW" => 1,
        "EVENT" => mysql_column_index(&row, "Create Event").unwrap_or(3),
        _ => 2,
    };
    mysql_create_sql(&row, column)
}

/// 按列名定位 SHOW CREATE 的语句列；列名大小写按服务器返回值。
fn mysql_column_index(row: &mysql_async::Row, label: &str) -> Option<usize> {
    row.columns_ref().iter().position(|column| column.name_str().eq_ignore_ascii_case(label))
}

fn quote_mysql(name: &str) -> String {
    use crate::adapters::SqlSyntax;
    super::syntax::MYSQL_SYNTAX.quote_always(name)
}

fn mysql_create_sql(row: &mysql_async::Row, index: usize) -> Result<String, String> {
    let statement = mysql_string(row, index).ok();
    let mut columns = vec![None; index + 1];
    columns[index] = statement.as_deref();
    mysql_show_create_at(&columns, index)
}

/// SHOW CREATE FUNCTION、PROCEDURE、TRIGGER 的语句都在索引 2。
/// 索引 0 是对象名，名称里的 CREATE 不是定义。
#[cfg(test)]
fn mysql_show_create_statement(columns: &[Option<&str>]) -> Result<String, String> {
    mysql_show_create_at(columns, 2)
}

fn mysql_show_create_at(columns: &[Option<&str>], index: usize) -> Result<String, String> {
    let text = columns.get(index).and_then(|item| *item).unwrap_or("");
    if text.trim().is_empty() {
        Err(definition_failure("没有读到定义"))
    } else {
        Ok(text.to_owned())
    }
}

fn mysql_object(row: &mysql_async::Row) -> Result<ObjectRow, String> {
    Ok(ObjectRow {
        name: mysql_string(row, 0)?,
        kind: parse_kind(&mysql_string(row, 1)?),
        estimated_rows: known_row_estimate(mysql_i64(row, 2)),
        stale: false,
        signature: mysql_text(row, 3),
        relation: mysql_text(row, 4),
        materialized: false,
    })
}

fn mysql_string(row: &mysql_async::Row, index: usize) -> Result<String, String> {
    let value = row.as_ref(index).ok_or_else(|| read_failure("元数据列缺失"))?;
    match value {
        mysql_async::Value::Bytes(bytes) => String::from_utf8(bytes.clone()).map_err(read_failure),
        mysql_async::Value::NULL => Err(read_failure("元数据列是空值")),
        other => Ok(other.as_sql(true)),
    }
}

fn mysql_text(row: &mysql_async::Row, index: usize) -> String {
    mysql_string(row, index).unwrap_or_default()
}

fn mysql_i64(row: &mysql_async::Row, index: usize) -> Option<i64> {
    let value = row.as_ref(index)?;
    match value {
        mysql_async::Value::NULL => None,
        mysql_async::Value::Int(number) => Some(*number),
        mysql_async::Value::UInt(number) => i64::try_from(*number).ok(),
        mysql_async::Value::Bytes(bytes) => std::str::from_utf8(bytes).ok().and_then(|text| text.parse().ok()),
        _ => None,
    }
}

fn detail_cell(value: Option<&mysql_async::Value>) -> Option<String> {
    match value? {
        mysql_async::Value::NULL => None,
        mysql_async::Value::Bytes(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
        mysql_async::Value::Int(number) => Some(number.to_string()),
        mysql_async::Value::UInt(number) => Some(number.to_string()),
        mysql_async::Value::Float(number) => Some(number.to_string()),
        mysql_async::Value::Double(number) => Some(number.to_string()),
        other => Some(format!("{other:?}")),
    }
}

/// 在目录会话上执行一条只读语句，把每格转成文本（对象详情与补全外键读取共用）。
pub(in crate::adapters) async fn text_rows(pool: &mysql_async::Pool, sql: &str) -> Result<Vec<Vec<Option<String>>>, String> {
    use crate::catalog::detail::failure;
    let mut conn = pool.get_conn().await.map_err(failure)?;
    let rows: Vec<mysql_async::Row> = conn.query(sql).await.map_err(failure)?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let width = row.columns_ref().len().max(row.len());
            (0..width).map(|index| detail_cell(row.as_ref(index))).collect()
        })
        .collect())
}

#[cfg(test)]
mod definition_column_tests {
    use super::mysql_show_create_statement;

    #[test]
    fn show_create_uses_the_statement_column_when_the_name_contains_create() {
        let function = [
            Some("create_order"),
            Some("ONLY_FULL_GROUP_BY"),
            Some("CREATE FUNCTION `create_order`() RETURNS int\nBEGIN\n  RETURN 1;\nEND"),
        ];
        let sql = mysql_show_create_statement(&function).expect("statement column");
        assert!(sql.starts_with("CREATE FUNCTION"));
        assert_ne!(sql, "create_order");

        let trigger = [
            Some("created_at"),
            Some(""),
            Some("CREATE TRIGGER created_at BEFORE INSERT ON t FOR EACH ROW SET NEW.a = 1"),
        ];
        assert!(mysql_show_create_statement(&trigger).unwrap().starts_with("CREATE TRIGGER"));
        assert!(mysql_show_create_statement(&[Some("create_order"), Some("mode")]).is_err());
        assert!(mysql_show_create_statement(&[Some("create_order"), Some("mode"), Some("   ")]).is_err());

        let sequence = [Some("orders_id_seq"), Some("CREATE SEQUENCE `orders_id_seq` START WITH 1")];
        let sequence_sql = super::mysql_show_create_at(&sequence, 1).expect("sequence column");
        assert!(sequence_sql.starts_with("CREATE SEQUENCE"));
        assert_ne!(sequence_sql, "orders_id_seq");
    }
}

#[cfg(test)]
mod latency_fixture_tests {
    use super::*;
    use crate::catalog::read::{read_catalog_outline, read_object_kind_page};
    use crate::connections::CatalogSession;

    #[test]
    fn mysql_navigation_uses_one_bounded_query_without_statistics() {
        for engine in [Engine::Mysql, Engine::Mariadb] {
            for kind in listed_kinds(engine) {
                let database = "库'\\?; DROP DATABASE x; --";
                let sql = mysql_navigation_page_sql(engine, database, *kind, 900, 100);
                assert!(sql.ends_with("LIMIT 101 OFFSET 900"), "{sql}");
                assert!(!sql.to_lowercase().contains("count("));
                assert!(!sql.to_lowercase().contains("table_rows"));
                assert!(!sql.contains(database));
                assert!(!sql.contains('?'));
                assert!(sql.contains(&mysql_catalog_literal(database)));
            }
        }
    }

    /// Uses scripts/verify-catalog-mysql.py, which rejects growing-prefix/eager reads.
    #[tokio::test]
    #[ignore = "requires local MySQL latency protocol fixture on port 33067"]
    async fn mysql_catalog_latency_fixture() {
        let pool = mysql_async::Pool::new(
            mysql_async::OptsBuilder::default().ip_or_hostname("127.0.0.1").tcp_port(33067).user(Some("fixture")).pass(Some("fixture")),
        );
        let session = CatalogSession::Mysql(pool.clone());
        let outline = crate::catalog::nav_read::metadata_read(read_catalog_outline(Engine::Mysql, &session)).await.unwrap();
        assert!(outline.error.is_none(), "{:?}", outline.error);
        assert_eq!(outline.databases.len(), 2);
        assert!(outline.databases.iter().flat_map(|db| &db.schemas).all(|schema| schema.objects.is_empty()));
        for offset in [0, 100, 99_900] {
            let page = crate::catalog::nav_read::metadata_read(read_object_kind_page(&session, Engine::Mysql, "shop", "", ObjectKind::Table, offset, 100))
                .await
                .unwrap();
            assert!(page.error.is_none(), "{:?}", page.error);
            assert_eq!(page.total, 100_000);
            assert_eq!(page.objects.len(), 100);
            assert_eq!(page.objects[0].name, format!("table_{offset:06}"));
            assert_eq!(page.objects[99].name, format!("table_{:06}", offset + 99));
        }
        // 与目录 deadline 相同的 future 丢弃路径，缩短到 50 ms 以验证客户端恢复。
        let timed_out = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            read_object_kind_page(&session, Engine::Mysql, "shop", "", ObjectKind::Table, 200, 100),
        )
        .await;
        assert!(timed_out.is_err());
        let recovered = crate::catalog::nav_read::metadata_read(read_object_kind_page(&session, Engine::Mysql, "shop", "", ObjectKind::Table, 200, 100))
            .await
            .unwrap();
        assert!(recovered.error.is_none(), "{:?}", recovered.error);
        assert_eq!(recovered.objects[0].name, "table_000200");
        drop(session);
        pool.disconnect().await.unwrap();
    }
}