//! 把已经在跑的 catalog 读取收成外壳可直接用的导航快照。
//! 只读、不新开连接；库/模式/对象类型的局部失败原样带到节点上。

use crate::catalog::hierarchy::NavConnection;
use crate::catalog::model::ConnStatus;
use crate::catalog::read::read_catalog_session;
use crate::connections::{Service, SessionCondition};

/// 读取每条已保存连接的层级。connection_id 只收窄到一条，用于局部重试。
pub async fn read_navigation(service: &Service, page: usize, connection_id: Option<&str>) -> Result<Vec<NavConnection>, String> {
    let saved = service.list().await?;
    let mut connections = Vec::with_capacity(saved.len());
    for record in saved {
        if connection_id.is_some_and(|id| id != record.saved.id) {
            continue;
        }
        let engine = record.saved.engine;
        let condition = service.session_condition(&record.saved.id);
        let session = if matches!(condition, Some(SessionCondition::Connected)) {
            service.catalog_session(&record.saved.id)
        } else {
            None
        };
        let (status, mut error) = match &condition {
            Some(SessionCondition::Lost { reason }) => (ConnStatus::Lost, reason.clone()),
            Some(SessionCondition::Connected) => (ConnStatus::Connected, None),
            None => (ConnStatus::Disconnected, None),
        };
        let databases = match session {
            Some(session) => {
                let body = read_catalog_session(engine, &session, page, &[]).await;
                if error.is_none() {
                    error = body.error;
                }
                body.databases
            }
            None => Vec::new(),
        };
        let mut connection = NavConnection::new(record.saved.id.clone(), record.saved.name.clone(), engine, status);
        connection.error = error;
        connection.default_database = record.saved.database.clone();
        connection.databases = databases;
        connections.push(connection);
    }
    Ok(connections)
}

