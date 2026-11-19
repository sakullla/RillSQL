//! 在已打开的会话上读取两侧。PostgreSQL 的另一个库只另开临时会话，不改连、不写配置。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use crate::adapters::transfer::{relational_engine, transfer_of};
use crate::adapters::{adapter, catalog_of, engine_of_session, fixed_database_named, session_matches};
use crate::catalog::{read_definition, read_object_page, ObjectKind, ObjectRow};
use crate::connections::{CatalogSession, Service};
use crate::ddl::{load_partial, Action, DdlEngine, Request, Snapshot};
use crate::grid::{describe, fetch_cells, qualified_name, quote_ident, select_list, Cell, TableSchema};

use super::data::diff_data;
use super::model::{
    needs_ephemeral_postgres, Category, ColumnShape, DataColumn, DataInput, DataReport, DataRequest, DataTable, ForeignKeyShape, IndexShape, Kind, ObjectDef, Side,
    Engine, SideRequest, StructureReport, StructureRequest, TableShape,
};
use super::structure::{diff_structure, table_data_keys};

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
                keys_unknown: false,
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
        return Some(crate::adapters::transfer::ENGINE_MISMATCH.to_owned());
    }
    baseline.fatal.clone().or(target.fatal.clone()).map(|message| format!("{message} 不生成脚本。"))
}

async fn load_side(service: &Service, request: &SideRequest, cancel_id: &str) -> Side {
    let fixed = fixed_database_named(&request.engine);
    let database = match fixed {
        Some(fixed) if request.database.is_empty() => fixed.to_owned(),
        _ => request.database.clone(),
    };
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
    let Ok(engine) = super::model::Engine::parse(&request.engine) else {
        side.fatal = Some(format!("不支持的数据库 {}", request.engine));
        return side;
    };
    if fixed.is_none() && database.trim().is_empty() {
        side.fatal = Some("需要数据库名".to_owned());
        return side;
    }
    let Some(existing) = service.catalog_session(&request.connection_id) else {
        side.fatal = Some("这一侧还没有打开".to_owned());
        return side;
    };
    if !session_matches(engine, &existing) {
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
    if let Err(message) = transfer_of(engine).compare_scope(&held.session, &database, &request.schema).await {
        side.fatal = Some(message);
        return side;
    }
    let mut offset = 0usize;
    let mut seen = 0usize;
    loop {
        if is_cancelled(cancel_id) {
            side.cancelled = true;
            side.listing_complete = false;
            break;
        }
        let page = read_object_page(&held.session, engine, &database, &request.schema, offset, 200).await;
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
    if side.listing_complete {
        for object in &mut side.objects {
            if is_cancelled(cancel_id) {
                side.cancelled = true;
                object.read_error = Some("对比已取消".to_owned());
                continue;
            }
            side.category_errors.extend(enrich(&held.session, request, &database, object).await);
        }
    }
    side.category_errors.sort();
    side.category_errors.dedup();
    side
}

#[allow(clippy::too_many_arguments)]
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
            keys_unknown: false,
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
    let left_shape = left.and_then(|object| object.table.as_ref());
    let right_shape = right.and_then(|object| object.table.as_ref());
    if left_shape.or(right_shape).is_none() {
        return failed_table(name, chosen_key, "没有读到表结构".to_owned());
    }
    let Ok(engine) = Engine::parse(&target_request.engine) else {
        return failed_table(name, chosen_key, format!("不支持的数据库 {}", target_request.engine));
    };
    let Ok(keys) = table_data_keys(engine, left_shape, right_shape) else {
        return unread_keys_table(name, chosen_key);
    };
    let Some(base_session) = service.catalog_session(&baseline_request.connection_id) else {
        return failed_table(name, chosen_key, "基准连接还没有打开".to_owned());
    };
    let Some(target_session) = service.catalog_session(&target_request.connection_id) else {
        return failed_table(name, chosen_key, "对照连接还没有打开".to_owned());
    };
    let side_database = |request: &SideRequest| fixed_database_named(&request.engine).map(str::to_owned).unwrap_or_else(|| request.database.clone());
    let base_database = side_database(baseline_request);
    let target_database = side_database(target_request);
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
        keys_unknown: false,
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
        keys_unknown: false,
        chosen_key,
        baseline: Vec::new(),
        target: Vec::new(),
        read_error: Some(message),
        only_side: None,
    }
}

fn unread_keys_table(name: &str, chosen_key: Option<String>) -> DataTable {
    DataTable {
        name: name.to_owned(),
        qualify: String::new(),
        columns: Vec::new(),
        keys: Vec::new(),
        keys_unknown: true,
        chosen_key,
        baseline: Vec::new(),
        target: Vec::new(),
        read_error: None,
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
    let tail = crate::adapters::query::sql_syntax(schema.engine).page_tail(limit, offset, !order.is_empty());
    let sql = format!("SELECT {list} FROM {}{order}{tail}", qualified_name(schema));
    // 与网格、导出同一套逐格读取（各引擎的取值规则在适配器里）。
    let matrix = fetch_cells(session, &sql, schema.columns.len()).await?;
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

async fn enrich(session: &CatalogSession, request: &SideRequest, database: &str, object: &mut ObjectDef) -> Vec<(Category, String)> {
    match object.kind {
        Kind::Table | Kind::View => {
            let action = Action::DropTable { table: object.name.clone() };
            let ddl_request = Request { database: database.to_owned(), schema: request.schema.clone(), engine: match DdlEngine::parse(&request.engine) {
                Ok(engine) => engine,
                Err(message) => {
                    object.read_error = Some(message);
                    return Vec::new();
                }
            }, action };
            match load_partial(session, &ddl_request, &object.name).await {
                Ok(snapshot) => absorb_snapshot(object, &snapshot),
                Err(message) => {
                    object.read_error = Some(message);
                    Vec::new()
                }
            }
        }
        _ => {
            let Ok(engine) = relational_engine(&request.engine) else {
                object.read_error = Some("引擎不一致".to_owned());
                return Vec::new();
            };
            match read_definition(session, engine, database, &request.schema, &object.name, kind_str(object.kind), &object.signature, &object.relation).await {
                Ok(sql) => {
                    object.definition = sql;
                    // serial / identity 的序列由列拥有：relation 记成 `表\u{1}列`，结构脚本不再无条件 DROP SEQUENCE。
                    if object.kind == Kind::Sequence {
                        if let Some(owner) = transfer_of(engine).sequence_owner(session, &request.schema, &object.name).await {
                            object.relation = owner;
                        }
                    }
                }
                Err(message) => object.read_error = Some(message),
            }
            Vec::new()
        }
    }
}

pub(super) fn absorb_snapshot(object: &mut ObjectDef, snapshot: &Snapshot) -> Vec<(Category, String)> {
    let mut errors = Vec::new();
    if let Some(message) = snapshot.index_error.clone() {
        errors.push((Category::Index, message));
    }
    if let Some(message) = snapshot.foreign_key_error.clone() {
        errors.push((Category::ForeignKey, message));
    }
    apply_snapshot(object, snapshot);
    errors
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
                default_constraint: column.default_constraint.clone().unwrap_or_default(),
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
        indexes_read: snapshot.index_error.is_none(),
        foreign_keys_read: snapshot.foreign_key_error.is_none(),
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
        // 扩展与事件不是可比较的结构对象，比较侧不列它们。
        ObjectKind::Extension | ObjectKind::Event => return None,
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

/// 会话绑定单库的引擎（PostgreSQL）要读的库不是当前库时另开临时会话，不改连、不写配置。
async fn hold_session(service: &Service, existing: &CatalogSession, request: &SideRequest, database: &str) -> Result<Held, String> {
    let Ok(engine) = relational_engine(&request.engine) else {
        return Ok(Held { _ephemeral: None, session: existing.clone() });
    };
    if !adapter(engine).capabilities().database_scoped_session {
        return Ok(Held { _ephemeral: None, session: existing.clone() });
    }
    let current = transfer_of(engine).session_database(existing).await?;
    if !needs_ephemeral_postgres(&request.engine, &current, database.trim()) {
        return Ok(Held { _ephemeral: None, session: existing.clone() });
    }
    let ephemeral = service.open_ephemeral_postgres(&request.connection_id, database.trim()).await?;
    let session = ephemeral.catalog();
    Ok(Held { _ephemeral: Some(ephemeral), session })
}

/// 列出所选 PostgreSQL / SQL Server 库的 schema。PostgreSQL 的另一个库走临时会话，不改连、不写配置。
pub async fn list_schemas(service: &Service, connection_id: &str, database: &str) -> Result<Vec<String>, String> {
    let database = database.trim();
    if database.is_empty() {
        return Err("需要数据库名".to_owned());
    }
    let Some(existing) = service.catalog_session(connection_id) else {
        return Err("这一侧还没有打开".to_owned());
    };
    let Some(engine) = engine_of_session(&existing).filter(|engine| adapter(*engine).capabilities().shows_schemas) else {
        return Err(crate::adapters::transfer::schema_engines_note());
    };
    let request = SideRequest {
        connection_id: connection_id.to_owned(),
        database: database.to_owned(),
        schema: String::new(),
        engine: adapter(engine).meta().id().to_owned(),
    };
    let held = hold_session(service, &existing, &request, database).await?;
    let sql = catalog_of(engine)
        .metadata_sql()
        .schemas
        .ok_or_else(|| format!("{} 缺少 schema 查询", adapter(engine).meta().label()))?;
    transfer_of(engine).schema_names(&held.session, sql).await
}
