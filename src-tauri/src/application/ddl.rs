//! 结构变更。未确认不执行；确认过的 SQL 仍必须和当前预览一致。

use crate::connections::model::Engine;
use crate::connections::Service;
use crate::ddl::{preview, Action, Plan, Receipt, Request, Snapshot};

pub fn require_execute(confirmed: bool) -> Result<(), String> {
    if !confirmed {
        return Err("结构变更需要先确认预览中的 SQL，未确认不会执行。".to_owned());
    }
    Ok(())
}

fn open_session(service: &Service, connection_id: &str) -> Result<crate::connections::CatalogSession, String> {
    service.catalog_session(connection_id).ok_or_else(|| "这条连接还没有打开，没有另开新的连接。".to_owned())
}

pub async fn describe_saved(service: &Service, connection_id: &str, database: String, schema: String, engine: &str, table: &str) -> Result<Snapshot, String> {
    let request = request(database, schema, engine, Action::DropTable { table: table.to_owned() })?;
    let session = open_session(service, connection_id)?;
    crate::ddl::load(&session, &request, table).await
}

pub async fn preview_request(service: &Service, connection_id: &str, request: &Request) -> Result<Plan, String> {
    let session = open_session(service, connection_id)?;
    preview(&session, request).await
}

pub async fn execute_request(service: &Service, connection_id: &str, request: &Request, confirmed_sql: &[String], confirmed: bool) -> Result<Receipt, String> {
    require_execute(confirmed)?;
    crate::ddl::commands::execute_saved(service, connection_id, request, confirmed_sql).await
}

pub fn request(database: String, schema: String, engine: &str, action: Action) -> Result<Request, String> {
    Ok(Request {
        database,
        schema,
        engine: Engine::parse(engine)?,
        action,
    })
}
