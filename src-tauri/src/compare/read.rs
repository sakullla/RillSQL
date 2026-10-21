//! 在已打开的会话上读取两侧。PostgreSQL 的另一个库只另开临时会话，不改连、不写配置。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use mysql_async::prelude::Queryable;

use crate::catalog::{read_definition, read_object_page, Engine as CatalogEngine, ObjectKind, ObjectRow};
use crate::connections::{CatalogSession, Service};
use crate::ddl::{load, Action, DdlEngine, Request, Snapshot};
use crate::grid::{describe, qualified_name, quote_ident, select_list, Cell, TableSchema};

use super::data::diff_data;
use super::model::{
    needs_ephemeral_postgres, Category, ColumnShape, DataColumn, DataInput, DataReport, DataRequest, DataTable, ForeignKeyShape, IndexShape, Kind, ObjectDef, Side,
    SideRequest, StructureReport, StructureRequest, TableShape,
};
use super::structure::{diff_structure, keys_from_shape};

static CANCELLED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

pub fn begin_cancel(id: &str) {
    if !id.is_empty() {
        CANCELLED.lock().expect("cancel").remove(id);
    }
}

pub fn request_cancel(id: &str) {
    if !id.is_empty() {
        CANCELLED.lock().expect("cancel").insert(id.to_owned());
    }
}

pub fn is_cancelled(id: &str) -> bool {
    !id.is_empty() && CANCELLED.lock().expect("cancel").contains(id)
}

struct Held {
    _ephemeral: Option<crate::connections::EphemeralRead>,
    session: CatalogSession,
}

pub async fn read_structure(service: &Service, request: &StructureRequest) -> StructureReport {
    begin_cancel(&request.id);
    let baseline = load_side(service, &request.baseline, &request.id).await;
    let target = load_side(service, &request.target, &request.id).await;
    diff_structure(&baseline, &target)
}

pub async fn read_data(service: &Service, request: &DataRequest) -> DataReport {
    begin_cancel(&request.id);
    if request.tables.is_empty() {
        return diff_data(&DataInput { baseline_engine: request.baseline.engine.clone(), target_engine: request.target.engine.clone(), tables: Vec::new(), cancelled: false });
    }
    let baseline = load_side(service, &request.baseline, &request.id).await;
    let target = load_side(service, &request.target, &request.id).await;
    if let Some(message) = refuse_loaded(&baseline, &target) {
        return DataReport { refused: Some(message), script: String::new(), incomplete: false, notes: Vec::new(), diffs: Vec::new() };
    }
    let mut tables = Vec::new();
    let mut cancelled = is_cancelled(&request.id) || baseline.cancelled || target.cancelled;
    for wanted in &request.tables {
        if is_cancelled(&request.id) {
            cancelled = true;
            tables.push(DataTable {
                name: wanted.name.clone(),
                qualify: String::new(),
                columns: Vec::new(),
                keys: Vec::new(),
                chosen_key: wanted.key.clone(),
                baseline: Vec::new(),
                target: Vec::new(),
                read_error: Some("对比已取消".to_owned()),
                only_side: None,
            });
            continue;
        }
        tables.push(load_data_table(service, &request.baseline, &request.target, &baseline, &target, &wanted.name, wanted.key.clone(), &request.id).await);
        if is_cancelled(&request.id) {
            cancelled = true;
        }
    }
    diff_data(&DataInput { baseline_engine: request.baseline.engine.clone(), target_engine: request.target.engine.clone(), tables, cancelled })
}

fn refuse_loaded(baseline: &Side, target: &Side) -> Option<String> {
    if super::model::Engine::parse(&baseline.engine).ok() != super::model::Engine::parse(&target.engine).ok() {
        return Some("两侧引擎不同。MySQL 与 MariaDB 是不同引擎，不生成脚本。".to_owned());
    }
    baseline.fatal.clone().or(target.fatal.clone()).map(|message| format!("{message} 不生成脚本。"))
}

async fn load_side(service: &Service, request: &SideRequest, cancel_id: &str) -> Side {
    let database = if request.engine == "sqlite" && request.database.is_empty() { "main".to_owned() } else { request.database.clone() };
    let mut side = Side {
        engine: request.engine.clone(),
        database: database.clone(),
        schema: request.schema.clone(),
        connected: false,
        fatal: None,
        objects: Vec::new(),
        category_errors: Vec::new(),
        cancelled: false,
        listing_complete: false,
    };
    if super::model::Engine::parse(&request.engine).is_err() {
        side.fatal = Some(format!("不支持的数据库 {}", request.engine));
        return side;
    }
    if request.engine != "sqlite" && database.trim().is_empty() {
        side.fatal = Some("需要数据库名".to_owned());
        return side;
    }
    let Some(existing) = service.catalog_session(&request.connection_id) else {
        side.fatal = Some("这一侧还没有打开".to_owned());
        return side;
    };
    if !session_matches(&existing, &request.engine) {
        side.fatal = Some("会话类型和目标引擎不一致".to_owned());
        return side;
    }
    side.connected = true;
    let held = match hold_session(service, &existing, request, &database).await {
        Ok(held) => held,
        Err(message) => {
            side.fatal = Some(message);
            return side;
        }
    };
    if let Err(message) = ensure_scope(&held.session, &request.engine, &database, &request.schema).await {
        side.fatal = Some(message);
        return side;
    }
    let Ok(catalog_engine) = catalog_engine(&request.engine) else {
        side.fatal = Some(format!("不支持的数据库 {}", request.engine));
        return side;
    };
    let mut offset = 0usize;
    let mut seen = 0usize;
    loop {
        if is_cancelled(cancel_id) {
            side.cancelled = true;
            side.listing_complete = false;
            break;
        }
        let page = read_object_page(&held.session, catalog_engine, &database, &request.schema, offset, 200).await;
        if let Some(message) = page.error {
            side.fatal = Some(message);
            return side;
        }
        for report in page.kind_reports {
            if let Some(message) = report.error {
                side.category_errors.push((category_of(report.kind), message));
            }
        }
        let count = page.objects.len();
        for object in page.objects {
            if let Some(def) = bare_object(object) {
                side.objects.push(def);
            }
        }
        seen += count;
        if count == 0 || seen >= page.total {
            side.listing_complete = true;
            break;
        }
        offset = seen;
    }
    side.category_errors.sort();
    side.category_errors.dedup();
    if side.listing_complete {
        for object in &mut side.objects {
            if is_cancelled(cancel_id) {
                side.cancelled = true;
                object.read_error = Some("对比已取消".to_owned());
                continue;
            }
            enrich(&held.session, request, &database, object).await;
        }
    }
    side
}

async fn load_data_table(
    service: &Service,
    baseline_request: &SideRequest,
    target_request: &SideRequest,
    baseline: &Side,
    target: &Side,
    name: &str,
    chosen_key: Option<String>,
    cancel_id: &str,
) -> DataTable {
    let left = baseline.objects.iter().find(|object| object.kind == Kind::Table && object.name == name);
    let right = target.objects.iter().find(|object| object.kind == Kind::Table && object.name == name);
    if left.is_none() || right.is_none() {
        let only_side = if left.is_some() { "基准" } else { "对照" };
        return DataTable {
            name: name.to_owned(),
            qualify: String::new(),
            columns: Vec::new(),
            keys: Vec::new(),
            chosen_key,
            baseline: Vec::new(),
            target: Vec::new(),
            read_error: None,
            only_side: Some(only_side.to_owned()),
        };
    }
    if let Some(message) = left.and_then(|object| object.read_error.clone()).or(right.and_then(|object| object.read_error.clone())) {
        return failed_table(name, chosen_key, message);
    }
    let shape = right.and_then(|object| object.table.as_ref()).or(left.and_then(|object| object.table.as_ref()));
    let Some(shape) = shape else {
        return failed_table(name, chosen_key, "没有读到表结构".to_owned());
    };
    let keys = keys_from_shape(shape);
    let Some(base_session) = service.catalog_session(&baseline_request.connection_id) else {
        return failed_table(name, chosen_key, "基准连接还没有打开".to_owned());
    };
    let Some(target_session) = service.catalog_session(&target_request.connection_id) else {
        return failed_table(name, chosen_key, "对照连接还没有打开".to_owned());
    };
    let base_database = if baseline_request.engine == "sqlite" { "main".to_owned() } else { baseline_request.database.clone() };
    let target_database = if target_request.engine == "sqlite" { "main".to_owned() } else { target_request.database.clone() };
    let base_held = match hold_session(service, &base_session, baseline_request, &base_database).await {
        Ok(held) => held,
        Err(message) => return failed_table(name, chosen_key, message),
    };
    let target_held = match hold_session(service, &target_session, target_request, &target_database).await {
        Ok(held) => held,
        Err(message) => return failed_table(name, chosen_key, message),
    };
    let described = match describe(&target_held.session, &target_database, &target_request.schema, name, &target_request.engine).await {
        Ok(schema) => schema,
        Err(message) => return failed_table(name, chosen_key, message),
    };
    let mut table = DataTable {
        name: name.to_owned(),
        qualify: described.schema.clone(),
        columns: described.columns.iter().map(|column| DataColumn { name: column.name.clone(), type_name: column.type_name.clone(), nullable: column.nullable }).collect(),
        keys,
        chosen_key,
        baseline: Vec::new(),
        target: Vec::new(),
        read_error: None,
        only_side: None,
    };
    let (baseline_rows, baseline_error) = read_all_rows(&base_held.session, &base_database, &baseline_request.schema, name, &baseline_request.engine, cancel_id).await;
    let (target_rows, target_error) = read_all_rows(&target_held.session, &target_database, &target_request.schema, name, &target_request.engine, cancel_id).await;
    table.baseline = baseline_rows;
    table.target = target_rows;
    table.read_error = baseline_error.or(target_error);
    table
}

fn failed_table(name: &str, chosen_key: Option<String>, message: String) -> DataTable {
    DataTable {
        name: name.to_owned(),
        qualify: String::new(),
        columns: Vec::new(),
        keys: Vec::new(),
        chosen_key,
        baseline: Vec::new(),
        target: Vec::new(),
        read_error: Some(message),
        only_side: None,
    }
}

async fn read_all_rows(session: &CatalogSession, database: &str, schema: &str, table: &str, engine: &str, cancel_id: &str) -> (Vec<BTreeMap<String, Cell>>, Option<String>) {
    let described = match describe(session, database, schema, table, engine).await {
        Ok(schema) => schema,
        Err(message) => return (Vec::new(), Some(message)),
    };
    let mut offset = 0u64;
    let limit = 200u64;
    let mut rows = Vec::new();
    loop {
        if is_cancelled(cancel_id) {
            return (rows, Some("对比已取消".to_owned()));
        }
        match fetch_page(session, &described, offset, limit).await {
            Ok(page) => {
                let count = page.len() as u64;
                rows.extend(page);
                if count < limit {
                    break;
                }
                offset += count;
            }
            Err(message) => return (rows, Some(message)),
        }
    }
    (rows, None)
}

async fn fetch_page(session: &CatalogSession, schema: &TableSchema, offset: u64, limit: u64) -> Result<Vec<BTreeMap<String, Cell>>, String> {
    let list = select_list(schema, true);
    let order = if schema.key.is_empty() {
        String::new()
    } else {
        let columns = schema.key.iter().map(|name| format!("{} ASC", quote_ident(schema.engine, name))).collect::<Vec<_>>().join(", ");
        format!(" ORDER BY {columns}")
    };
    let sql = format!("SELECT {list} FROM {}{order} LIMIT {limit} OFFSET {offset}", qualified_name(schema));
    let width = schema.columns.len();
    let matrix = match session {
        CatalogSession::Scripted => return Err("这个连接没有连着可查询的数据库".to_owned()),
        CatalogSession::Sqlite { pool, .. } => fetch_sqlite(pool, &sql, width).await?,
        CatalogSession::Mysql(pool) => fetch_mysql(pool, &sql).await?,
        CatalogSession::Postgres(client) => fetch_postgres(client, &sql, width).await?,
    };
    let mut rows = Vec::new();
    for cells in matrix {
        let mut map = BTreeMap::new();
        for (column, cell) in schema.columns.iter().zip(cells) {
            map.insert(column.name.clone(), cell);
        }
        rows.push(map);
    }
    Ok(rows)
}

async fn fetch_sqlite(pool: &sqlx::SqlitePool, sql: &str, width: usize) -> Result<Vec<Vec<Cell>>, String> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(sql.to_owned())).fetch_all(pool).await.map_err(|err| err.to_string())?;
    rows.iter().map(|row| read_sqlite_row(row, width)).collect()
}

fn read_sqlite_row(row: &sqlx::sqlite::SqliteRow, width: usize) -> Result<Vec<Cell>, String> {
    use sqlx::Row;
    let mut cells = Vec::new();
    for index in 0..width {
        if let Ok(value) = row.try_get::<Option<String>, _>(index) {
            cells.push(cell_text(value));
            continue;
        }
        if let Ok(value) = row.try_get::<Option<i64>, _>(index) {
            cells.push(cell_text(value.map(|number| number.to_string())));
            continue;
        }
        if let Ok(value) = row.try_get::<Option<f64>, _>(index) {
            cells.push(cell_text(value.map(|number| number.to_string())));
            continue;
        }
        return Err(format!("读不到第 {index} 列"));
    }
    Ok(cells)
}

async fn fetch_postgres(client: &tokio_postgres::Client, sql: &str, width: usize) -> Result<Vec<Vec<Cell>>, String> {
    let rows = client.query(sql, &[]).await.map_err(|err| err.to_string())?;
    let mut matrix = Vec::new();
    for row in &rows {
        let mut cells = Vec::new();
        for index in 0..width {
            let value = row.try_get::<_, Option<String>>(index).map_err(|err| err.to_string())?;
            cells.push(cell_text(value));
        }
        matrix.push(cells);
    }
    Ok(matrix)
}

async fn fetch_mysql(pool: &mysql_async::Pool, sql: &str) -> Result<Vec<Vec<Cell>>, String> {
    let mut conn = pool.get_conn().await.map_err(|err| err.to_string())?;
    let mut result = conn.query_iter(sql).await.map_err(|err| err.to_string())?;
    let width = result.columns_ref().len();
    let mut matrix = Vec::new();
    while let Some(row) = result.next().await.map_err(|err| err.to_string())? {
        let cells = (0..width).map(|index| mysql_cell(row.as_ref(index).unwrap_or(&mysql_async::Value::NULL))).collect();
        matrix.push(cells);
    }
    Ok(matrix)
}

fn mysql_cell(value: &mysql_async::Value) -> Cell {
    match value {
        mysql_async::Value::NULL => Cell::Null,
        mysql_async::Value::Bytes(bytes) => Cell::Text(String::from_utf8_lossy(bytes).into_owned()),
        mysql_async::Value::Int(number) => Cell::Text(number.to_string()),
        mysql_async::Value::UInt(number) => Cell::Text(number.to_string()),
        mysql_async::Value::Float(number) => Cell::Text(number.to_string()),
        mysql_async::Value::Double(number) => Cell::Text(number.to_string()),
        mysql_async::Value::Date(year, month, day, hour, minute, second, micros) => Cell::Text(format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}.{micros:06}")),
        mysql_async::Value::Time(negative, days, hour, minute, second, micros) => {
            Cell::Text(format!("{}{days} {hour:02}:{minute:02}:{second:02}.{micros:06}", if *negative { "-" } else { "" }))
        }
    }
}

fn cell_text(value: Option<String>) -> Cell {
    match value {
        Some(text) => Cell::Text(text),
        None => Cell::Null,
    }
}

async fn enrich(session: &CatalogSession, request: &SideRequest, database: &str, object: &mut ObjectDef) {
    match object.kind {
        Kind::Table | Kind::View => {
            let action = Action::DropTable { table: object.name.clone() };
            let ddl_request = Request { database: database.to_owned(), schema: request.schema.clone(), engine: match DdlEngine::parse(&request.engine) {
                Ok(engine) => engine,
                Err(message) => {
                    object.read_error = Some(message);
                    return;
                }
            }, action };
            match load(session, &ddl_request, &object.name).await {
                Ok(snapshot) => apply_snapshot(object, &snapshot),
                Err(message) => object.read_error = Some(message),
            }
        }
        _ => {
            let Ok(engine) = catalog_engine(&request.engine) else {
                object.read_error = Some("引擎不一致".to_owned());
                return;
            };
            match read_definition(session, engine, database, &request.schema, &object.name, kind_str(object.kind), &object.signature, &object.relation).await {
                Ok(sql) => object.definition = sql,
                Err(message) => object.read_error = Some(message),
            }
        }
    }
}

fn apply_snapshot(object: &mut ObjectDef, snapshot: &Snapshot) {
    if snapshot.kind == "view" || object.kind == Kind::View {
        object.kind = Kind::View;
        object.materialized = snapshot.materialized;
        if let Some(message) = &snapshot.definition_error {
            object.read_error = Some(message.clone());
            return;
        }
        if snapshot.table_sql.trim().is_empty() || snapshot.table_sql.trim().starts_with("--") {
            object.read_error = Some("没有读到视图定义".to_owned());
            return;
        }
        object.definition = snapshot.table_sql.clone();
        return;
    }
    object.table = Some(TableShape {
        columns: snapshot
            .columns
            .iter()
            .map(|column| ColumnShape {
                name: column.name.clone(),
                type_name: column.type_name.clone(),
                nullable: column.nullable,
                default_sql: column.default_sql.clone().unwrap_or_default(),
                primary: column.primary,
            })
            .collect(),
        indexes: snapshot
            .indexes
            .iter()
            .map(|index| IndexShape {
                name: index.name.clone(),
                unique: index.unique,
                columns: index.columns.iter().map(|column| (column.name.clone(), column.descending)).collect(),
                protected: index.protected,
                reason: index.reason.clone(),
                partial: index.partial,
                expression: index.expression,
                sql: index.sql.clone().unwrap_or_default(),
            })
            .collect(),
        foreign_keys: snapshot
            .foreign_keys
            .iter()
            .map(|key| ForeignKeyShape {
                name: key.name.clone(),
                columns: key.columns.clone(),
                ref_schema: key.ref_schema.clone(),
                ref_table: key.ref_table.clone(),
                ref_columns: key.ref_columns.clone(),
                on_delete: key.on_delete.clone(),
                on_update: key.on_update.clone(),
            })
            .collect(),
    });
}

fn bare_object(row: ObjectRow) -> Option<ObjectDef> {
    Some(ObjectDef {
        kind: map_kind(row.kind)?,
        name: row.name,
        signature: row.signature,
        relation: row.relation,
        definition: String::new(),
        materialized: row.materialized,
        table: None,
        read_error: None,
    })
}

fn map_kind(kind: ObjectKind) -> Option<Kind> {
    Some(match kind {
        ObjectKind::Table => Kind::Table,
        ObjectKind::View => Kind::View,
        ObjectKind::Function => Kind::Function,
        ObjectKind::Procedure => Kind::Procedure,
        ObjectKind::Trigger => Kind::Trigger,
        ObjectKind::Sequence => Kind::Sequence,
        ObjectKind::Enum => Kind::Enum,
        ObjectKind::Domain => Kind::Domain,
    })
}

fn category_of(kind: ObjectKind) -> Category {
    Category::from_kind(map_kind(kind).unwrap_or(Kind::Table))
}

fn kind_str(kind: Kind) -> &'static str {
    match kind {
        Kind::Table => "table",
        Kind::View => "view",
        Kind::Function => "function",
        Kind::Procedure => "procedure",
        Kind::Trigger => "trigger",
        Kind::Sequence => "sequence",
        Kind::Enum => "enum",
        Kind::Domain => "domain",
    }
}

fn catalog_engine(value: &str) -> Result<CatalogEngine, String> {
    match value {
        "postgres" => Ok(CatalogEngine::Postgres),
        "mysql" => Ok(CatalogEngine::Mysql),
        "mariadb" => Ok(CatalogEngine::Mariadb),
        "sqlite" => Ok(CatalogEngine::Sqlite),
        other => Err(format!("不支持的数据库 {other}")),
    }
}

fn session_matches(session: &CatalogSession, engine: &str) -> bool {
    match (session, engine) {
        (CatalogSession::Sqlite { .. }, "sqlite") => true,
        (CatalogSession::Mysql(_), "mysql" | "mariadb") => true,
        (CatalogSession::Postgres(_), "postgres") => true,
        _ => false,
    }
}

async fn hold_session(service: &Service, existing: &CatalogSession, request: &SideRequest, database: &str) -> Result<Held, String> {
    if request.engine != "postgres" {
        return Ok(Held { _ephemeral: None, session: existing.clone() });
    }
    let current = postgres_current(existing).await?;
    if !needs_ephemeral_postgres(&request.engine, &current, database.trim()) {
        return Ok(Held { _ephemeral: None, session: existing.clone() });
    }
    let ephemeral = service.open_ephemeral_postgres(&request.connection_id, database.trim()).await?;
    let session = ephemeral.catalog();
    Ok(Held { _ephemeral: Some(ephemeral), session })
}

async fn postgres_current(session: &CatalogSession) -> Result<String, String> {
    let CatalogSession::Postgres(client) = session else {
        return Ok(String::new());
    };
    let row = client.query_one("SELECT current_database()", &[]).await.map_err(|err| err.to_string())?;
    let name: Option<String> = row.try_get(0).map_err(|err| err.to_string())?;
    Ok(name.unwrap_or_default())
}

async fn ensure_scope(session: &CatalogSession, engine: &str, database: &str, schema: &str) -> Result<(), String> {
    match engine {
        "sqlite" => {
            if !database.is_empty() && database != "main" {
                return Err("SQLite 只对比该连接的 main，不附加其他文件".to_owned());
            }
            Ok(())
        }
        "postgres" => {
            if schema.trim().is_empty() {
                return Err("PostgreSQL 需要指定 schema".to_owned());
            }
            let CatalogSession::Postgres(client) = session else {
                return Err("会话类型和目标引擎不一致".to_owned());
            };
            let rows = client.query("SELECT 1 FROM pg_namespace WHERE nspname = $1", &[&schema]).await.map_err(|err| err.to_string())?;
            if rows.is_empty() {
                return Err(format!("schema {schema} 不存在"));
            }
            Ok(())
        }
        "mysql" | "mariadb" => {
            let CatalogSession::Mysql(pool) = session else {
                return Err("会话类型和目标引擎不一致".to_owned());
            };
            let mut conn = pool.get_conn().await.map_err(|err| err.to_string())?;
            let row: Option<mysql_async::Row> = conn
                .exec_first("SELECT SCHEMA_NAME FROM information_schema.SCHEMATA WHERE SCHEMA_NAME = ?", (database.to_owned(),))
                .await
                .map_err(|err| err.to_string())?;
            if row.is_none() {
                return Err(format!("数据库 {database} 不存在"));
            }
            Ok(())
        }
        _ => Err(format!("不支持的数据库 {engine}")),
    }
}


