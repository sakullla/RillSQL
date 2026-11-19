//! 结构内省：读表/视图快照、库级动作的状态与会话状态。只在已打开的会话上读，不另开连接；
//! 各引擎的系统表查询在适配器的 [`DdlDialect`] 里，这里只做引擎无关的流程。

use crate::adapters::ddl::{ddl_of, DdlDialect};
use crate::connections::CatalogSession;

use super::model::{Action, Engine, Request, Snapshot};
use super::sql::{qualified_table, quote_ident};

/// 会话句柄与请求的引擎对不上。
pub(crate) const SESSION_MISMATCH: &str = "会话类型和目标数据库不一致，没有另开连接。";

const NO_DATABASE: &str = "这个连接没有连着可查询的数据库。";

pub async fn load(session: &CatalogSession, request: &Request, table: &str) -> Result<Snapshot, String> {
    let snapshot = load_partial(session, request, table).await?;
    // 结构编辑仍把索引或外键失败当成整次读取失败，避免把没读到的索引当成不存在。
    if let Some(message) = snapshot.index_error.clone().or(snapshot.foreign_key_error.clone()) {
        return Err(message);
    }
    Ok(snapshot)
}

pub async fn load_partial(session: &CatalogSession, request: &Request, table: &str) -> Result<Snapshot, String> {
    let dialect = ensure_session(session, request.engine)?;
    dialect.load_snapshot(session, request, table).await
}

/// 读取的某一部分失败时记下第一条原因，返回空结果，让快照的其它部分照常读完。
pub(crate) fn keep_rows<T>(error: &mut Option<String>, result: Result<Vec<T>, String>) -> Vec<T> {
    match result {
        Ok(rows) => rows,
        Err(message) => {
            if error.is_none() {
                *error = Some(message);
            }
            Vec::new()
        }
    }
}

pub async fn enrich(session: &CatalogSession, snapshot: &mut Snapshot, action: &Action) -> Result<(), String> {
    if let Action::RenameTable { new_name, .. } = action {
        let wanted = new_name.trim();
        if !wanted.is_empty() {
            snapshot.name_taken = Some(name_taken(session, snapshot, wanted).await?);
        }
        return Ok(());
    }
    let Action::AlterColumns { columns, .. } = action else {
        return Ok(());
    };
    for draft in columns {
        let Some(original) = draft.original.as_deref() else {
            continue;
        };
        let Some(column) = snapshot.columns.iter().find(|column| column.name == original) else {
            continue;
        };
        if !draft.dropped && !draft.nullable && column.nullable {
            let count = count_nulls(session, snapshot, original).await?;
            snapshot.null_counts.insert(original.to_owned(), count);
        }
    }
    Ok(())
}

/// 数据库级动作（序列、枚举、域、事件、PRAGMA、附加库、维护）不针对某张表，
/// 走 object_snapshot 读它们自己的状态。
pub fn is_object_action(action: &Action) -> bool {
    matches!(
        action,
        Action::AlterSequence { .. }
            | Action::AddEnumValue { .. }
            | Action::AlterDomain { .. }
            | Action::SetEventEnabled { .. }
            | Action::SetPragma { .. }
            | Action::AttachDatabase { .. }
            | Action::Vacuum
            | Action::Reindex { .. }
    )
}

pub fn table_of(action: &Action) -> Option<&str> {
    match action {
        Action::CreateTable { .. } | Action::SetForeignKeys { .. } => None,
        // 数据库级动作没有目标表；plan 只根据 object_snapshot 读到的状态生成语句。
        Action::AlterSequence { .. }
        | Action::AddEnumValue { .. }
        | Action::AlterDomain { .. }
        | Action::SetEventEnabled { .. }
        | Action::SetPragma { .. }
        | Action::AttachDatabase { .. }
        | Action::Vacuum
        | Action::Reindex { .. } => None,
        Action::AlterColumns { table, .. }
        | Action::CreateIndex { table, .. }
        | Action::DropIndex { table, .. }
        | Action::CreateForeignKey { table, .. }
        | Action::DropForeignKey { table, .. }
        | Action::DropTable { table }
        | Action::TruncateTable { table }
        | Action::RenameTable { table, .. }
        | Action::ReplaceView { table, .. } => Some(table.as_str()),
        Action::DropView { view } => Some(view.as_str()),
    }
}

/// 改名前查新名字是否已被同一命名空间里的对象占用（各引擎的命名空间规则见适配器）。
/// 被改名的表自身（按原名逐字节相同）不算占用，这样只改大小写的改名不会被当成名字冲突。
pub async fn name_taken(session: &CatalogSession, snapshot: &Snapshot, name: &str) -> Result<bool, String> {
    let dialect = ensure_session(session, snapshot.engine)?;
    dialect.name_taken(session, snapshot, name).await
}

/// 读表与列的注释，只给“从元数据重建定义”用。查询失败只记在 comment_error，不让整份定义读取失败。
pub async fn load_comments(session: &CatalogSession, snapshot: &mut Snapshot) {
    let Some(dialect) = ddl_of(snapshot.engine) else {
        return;
    };
    let Some(result) = dialect.load_comments(session, &snapshot.schema, &snapshot.name).await else {
        return;
    };
    match result {
        Ok((table, columns)) => {
            snapshot.table_comment = table;
            snapshot.column_comments = columns;
        }
        Err(message) => snapshot.comment_error = Some(message),
    }
}

pub async fn attach_reference(session: &CatalogSession, request: &Request, snapshot: &mut Snapshot, action: &Action) -> Result<(), String> {
    let Action::CreateForeignKey { foreign_key, .. } = action else {
        return Ok(());
    };
    let mut target_request = request.clone();
    if let Some(schema) = ddl_of(request.engine).and_then(|dialect| dialect.reference_scope(&foreign_key.ref_schema)) {
        target_request.schema = schema.to_owned();
    }
    let referenced = load(session, &target_request, &foreign_key.ref_table).await?;
    if referenced.kind != "table" {
        return Err(format!("目标 {} 不是基础表。", foreign_key.ref_table));
    }
    snapshot.reference = Some(Box::new(referenced));
    Ok(())
}

/// 外键开关动作的快照：只有会话状态（外键开关、版本），没有对象。
pub async fn session_snapshot(session: &CatalogSession, request: &Request) -> Result<Snapshot, String> {
    let dialect = ensure_session(session, request.engine)?;
    let mut snapshot = Snapshot::bare(request.engine, &request.database, &request.schema, "");
    if !dialect.session_foreign_keys() {
        // 拒绝原因由外键开关动作所属的方言给出。
        let refused = crate::adapters::ddl::plan_object_action(&snapshot, &request.action);
        return Err(refused.map(|plan| plan.blocked.join(" ")).unwrap_or_else(|| SESSION_MISMATCH.to_owned()));
    }
    snapshot.kind = "session".to_owned();
    snapshot.object_exists = false;
    snapshot.table_sql.clear();
    dialect.read_session_state(session, &mut snapshot).await?;
    Ok(snapshot)
}

/// 建表时目标表还不存在：只读会话状态，作为“缺失”快照交给计划。
pub async fn missing_snapshot(session: &CatalogSession, request: &Request, name: &str) -> Result<Snapshot, String> {
    let mut snapshot = Snapshot::bare(request.engine, &request.database, &request.schema, name);
    snapshot.kind = "missing".to_owned();
    snapshot.object_exists = false;
    snapshot.table_sql.clear();
    if let Some(dialect) = ddl_of(request.engine) {
        dialect.read_session_state(session, &mut snapshot).await?;
    }
    Ok(snapshot)
}

/// 会话要连着数据库，且句柄属于请求的引擎；返回该引擎的方言。
fn ensure_session(session: &CatalogSession, engine: Engine) -> Result<&'static dyn DdlDialect, String> {
    if session.is_scripted() {
        return Err(NO_DATABASE.to_owned());
    }
    ddl_of(engine).filter(|dialect| dialect.matches_session(session)).ok_or_else(|| SESSION_MISMATCH.to_owned())
}

/// 读数据库级动作自己的状态：序列参数、枚举取值、域定义、事件状态、PRAGMA 与附加库。
/// 读不到就返回原因，plan 不会用猜出来的当前值判断“没有变化”。
pub async fn object_snapshot(session: &CatalogSession, request: &Request) -> Result<Snapshot, String> {
    let dialect = ensure_session(session, request.engine)?;
    let mut snapshot = Snapshot::bare(request.engine, &request.database, &request.schema, "");
    snapshot.table_sql.clear();
    match &request.action {
        Action::AlterSequence { sequence, .. } => {
            snapshot.kind = "sequence".to_owned();
            snapshot.name = sequence.clone();
        }
        Action::AddEnumValue { enum_name, .. } => {
            snapshot.kind = "enum".to_owned();
            snapshot.name = enum_name.clone();
        }
        Action::AlterDomain { domain, .. } => {
            snapshot.kind = "domain".to_owned();
            snapshot.name = domain.clone();
        }
        Action::SetEventEnabled { event, .. } => {
            snapshot.kind = "event".to_owned();
            snapshot.name = event.clone();
        }
        Action::SetPragma { .. } | Action::AttachDatabase { .. } | Action::Vacuum | Action::Reindex { .. } => {
            snapshot.kind = "database".to_owned();
        }
        _ => return Err("这个动作需要指定表，不是数据库级动作。".to_owned()),
    }
    // 只有提供该动作的方言会读；其它引擎留空，计划给出“不属于这个引擎”的原因。
    dialect.read_object_state(session, request, &mut snapshot).await?;
    Ok(snapshot)
}

/// 维护动作完成后的结果反馈。读不到就只给一句“已完成”，不编造数字。
pub async fn maintenance_note(session: &CatalogSession, engine: Engine, action: &Action) -> Option<String> {
    ddl_of(engine)?.maintenance_note(session, action).await
}

async fn count_nulls(session: &CatalogSession, snapshot: &Snapshot, column: &str) -> Result<i64, String> {
    let qualified = qualified_table(snapshot.engine, &snapshot.database, &snapshot.schema, &snapshot.name)?;
    let sql = format!("SELECT COUNT(*) FROM {qualified} WHERE {} IS NULL", quote_ident(snapshot.engine, column));
    let dialect = ensure_session(session, snapshot.engine)?;
    dialect.count(session, &sql).await
}
