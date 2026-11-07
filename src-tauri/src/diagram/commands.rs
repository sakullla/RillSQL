use crate::connections::commands::ConnectionsState;
use crate::diagram::model::Diagram;

pub async fn diagram_read(state: &ConnectionsState, connection_id: String, database: String, schema: String) -> Result<Diagram, String> {
    crate::application::work::read_diagram(&state.service, &connection_id, &database, &schema).await
}
