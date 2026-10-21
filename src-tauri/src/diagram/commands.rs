use crate::connections::commands::ConnectionsState;
use crate::diagram::model::Diagram;
use crate::diagram::read::read_diagram;

#[tauri::command]
pub async fn diagram_read(state: tauri::State<'_, ConnectionsState>, connection_id: String, database: String, schema: String) -> Result<Diagram, String> {
    let session = state
        .service
        .catalog_session(&connection_id)
        .ok_or_else(|| "这条连接还没有打开，无法读取关系图".to_owned())?;
    let saved = state.service.list().await?;
    let engine = saved
        .into_iter()
        .find(|item| item.saved.id == connection_id)
        .map(|item| item.saved.engine)
        .ok_or_else(|| "找不到这条连接".to_owned())?;
    read_diagram(engine, &session, &database, &schema).await
}
