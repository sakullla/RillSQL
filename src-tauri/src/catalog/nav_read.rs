//! 导航只读层级概要；远端对象按展开的种类分页，慢连接不阻挡其它连接。

use std::sync::{Arc, Mutex, Weak};
use std::collections::HashMap;
use std::time::Duration;
use futures::{stream, StreamExt};
use crate::catalog::hierarchy::NavConnection;
use crate::catalog::model::{ConnStatus, Engine};
use crate::catalog::read::{read_catalog_outline, read_catalog_session};
use crate::connections::{Service, SessionCondition};

/// 全局最多四个读取，每条连接最多两个；先等连接名额，避免慢连接占满全局队列。
pub struct MetadataGate {
    global: Arc<tokio::sync::Semaphore>,
    connections: Mutex<HashMap<String, Weak<tokio::sync::Semaphore>>>,
}

impl MetadataGate {
    pub fn new() -> Self {
        Self { global: Arc::new(tokio::sync::Semaphore::new(4)), connections: Mutex::new(HashMap::new()) }
    }

    fn connection(&self, id: &str) -> Arc<tokio::sync::Semaphore> {
        let mut connections = self.connections.lock().expect("metadata gates");
        connections.retain(|_, gate| gate.strong_count() > 0);
        if let Some(gate) = connections.get(id).and_then(Weak::upgrade) { return gate; }
        let gate = Arc::new(tokio::sync::Semaphore::new(2));
        connections.insert(id.to_owned(), Arc::downgrade(&gate));
        gate
    }
}

impl Default for MetadataGate {
    fn default() -> Self { Self::new() }
}

pub const METADATA_TIMEOUT: Duration = Duration::from_secs(30);
pub const METADATA_TIMEOUT_MESSAGE: &str = "catalog.tree.readTimeout";

/// 包括等待会话连接的时间；调用方的并发队列在这段超时之外。
pub async fn metadata_read<T>(read: impl std::future::Future<Output = T>) -> Result<T, String> {
    metadata_read_with_timeout(METADATA_TIMEOUT, read).await
}

async fn metadata_read_with_timeout<T>(timeout: Duration, read: impl std::future::Future<Output = T>) -> Result<T, String> {
    tokio::time::timeout(timeout, read).await.map_err(|_| METADATA_TIMEOUT_MESSAGE.to_owned())
}

pub enum NavigationEvent {
    Started(Vec<NavConnection>),
    Connection(NavConnection),
    Finished(Vec<String>),
    Failed(String),
}

pub async fn stream_navigation(service: &Service, page: usize, connection_id: Option<&str>, gate: Arc<MetadataGate>, sender: tokio::sync::mpsc::Sender<NavigationEvent>) {
    let saved = match metadata_read(service.list()).await {
        Ok(Ok(saved)) => saved,
        Ok(Err(error)) | Err(error) => { let _ = sender.send(NavigationEvent::Failed(error)).await; return; }
    };
    let saved: Vec<_> = saved.into_iter().filter(|record| connection_id.is_none_or(|id| id == record.saved.id)).collect();
    let ids = saved.iter().map(|record| record.saved.id.clone()).collect();
    let roots = saved.iter().map(|record| {
        let status = match service.session_condition(&record.saved.id) {
            Some(SessionCondition::Connected) => ConnStatus::Connected,
            Some(SessionCondition::Lost { .. }) => ConnStatus::Lost,
            None => ConnStatus::Disconnected,
        };
        let mut root = NavConnection::new(record.saved.id.clone(), record.saved.name.clone(), record.saved.engine, status);
        root.default_database = record.saved.database.clone();
        root.outline_only = matches!(record.saved.engine, Engine::Mysql | Engine::Mariadb | Engine::Postgres | Engine::Sqlserver);
        root
    }).collect();
    if sender.send(NavigationEvent::Started(roots)).await.is_err() { return; }
    let reads = stream::iter(saved).map(|record| { let gate = Arc::clone(&gate); async move {
        let engine = record.saved.engine;
        let condition = service.session_condition(&record.saved.id);
        let (status, error) = match condition {
            Some(SessionCondition::Lost { reason }) => (ConnStatus::Lost, reason),
            Some(SessionCondition::Connected) => (ConnStatus::Connected, None),
            None => (ConnStatus::Disconnected, None),
        };
        let mut connection = NavConnection::new(record.saved.id.clone(), record.saved.name, engine, status);
        connection.error = error;
        connection.default_database = record.saved.database;
        connection.outline_only = matches!(engine, Engine::Mysql | Engine::Mariadb | Engine::Postgres | Engine::Sqlserver);
        if let Some(session) = service.catalog_session(&record.saved.id) {
            let result = limited_metadata(gate, &record.saved.id, async {
                if connection.outline_only { read_catalog_outline(engine, &session).await }
                else { read_catalog_session(engine, &session, page, &[]).await }
            }).await;
            match result {
                Ok(body) => { connection.error = body.error.or(connection.error); connection.databases = body.databases; }
                Err(error) => connection.error = Some(error),
            }
        }
        connection
    }});
    publish_connections(reads, &sender).await;
    let _ = sender.send(NavigationEvent::Finished(ids)).await;
}

async fn publish_connections<S, F>(reads: S, sender: &tokio::sync::mpsc::Sender<NavigationEvent>)
where S: futures::Stream<Item = F>, F: std::future::Future<Output = NavConnection> {
    let reads = reads.buffer_unordered(4);
    futures::pin_mut!(reads);
    loop {
        tokio::select! {
            _ = sender.closed() => return,
            connection = reads.next() => match connection {
                Some(connection) => if sender.send(NavigationEvent::Connection(connection)).await.is_err() { return; },
                None => return,
            }
        }
    }
}

/// 兼容需要完整返回值的只读调用；GUI 使用流式入口逐条呈现。
pub async fn read_navigation(service: &Service, page: usize, connection_id: Option<&str>) -> Result<Vec<NavConnection>, String> {
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let producer = stream_navigation(service, page, connection_id, Arc::new(MetadataGate::new()), tx);
    let consumer = async {
        let mut connections = Vec::new();
        while let Some(event) = rx.recv().await {
            match event {
                NavigationEvent::Started(_) => {},
                NavigationEvent::Connection(connection) => connections.push(connection),
                NavigationEvent::Failed(error) => return Err(error),
                NavigationEvent::Finished(ids) => {
                    let order: std::collections::HashMap<_, _> = ids.into_iter().enumerate().map(|(index, id)| (id, index)).collect();
                    connections.sort_by_key(|connection| order.get(&connection.id).copied().unwrap_or(usize::MAX));
                    break;
                }
            }
        }
        Ok(connections)
    };
    let (_, result) = tokio::join!(producer, consumer);
    result
}

/// 节点读取限流：排队不占用数据库连接，拿到名额后才开始计算读取超时。
pub async fn limited_metadata<T>(gate: Arc<MetadataGate>, connection: &str, read: impl std::future::Future<Output = T>) -> Result<T, String> {
    let connection = gate.connection(connection);
    let _connection = connection.acquire().await.map_err(|_| "目录读取已停止".to_owned())?;
    let _permit = gate.global.acquire().await.map_err(|_| "目录读取已停止".to_owned())?;
    metadata_read(read).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn one_slow_connection_does_not_take_all_global_capacity() {
        let gate = Arc::new(MetadataGate::new());
        let (ready, mut started) = tokio::sync::mpsc::channel(6);
        let mut tasks = Vec::new();
        for _ in 0..6 {
            let gate = Arc::clone(&gate);
            let ready = ready.clone();
            tasks.push(tokio::spawn(async move {
                limited_metadata(gate, "slow", async move {
                    ready.send(()).await.unwrap();
                    std::future::pending::<()>().await;
                }).await
            }));
        }
        started.recv().await.unwrap();
        started.recv().await.unwrap();
        tokio::task::yield_now().await;
        assert_eq!(gate.global.available_permits(), 2, "排队的慢连接不提前占全局名额");
        let fast = tokio::time::timeout(Duration::from_secs(5), limited_metadata(Arc::clone(&gate), "fast", async { 42 })).await.unwrap().unwrap();
        assert_eq!(fast, 42);
        for task in &tasks { task.abort(); }
        for task in tasks { assert!(task.await.unwrap_err().is_cancelled()); }
        assert_eq!(gate.global.available_permits(), 4);
    }

    #[tokio::test]
    async fn slow_connection_does_not_hold_back_fast_connection_and_timeout_is_visible() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let reads = stream::iter([0, 1]).map(|id| async move {
            let mut connection = NavConnection::new(id.to_string(), "test", Engine::Mysql, ConnStatus::Connected);
            if id == 0 {
                connection.error = metadata_read_with_timeout(Duration::from_millis(30), std::future::pending::<()>()).await.err();
            }
            connection
        });
        let producer = publish_connections(reads, &tx);
        let consumer = async {
            let Some(NavigationEvent::Connection(first)) = rx.recv().await else { panic!("first") };
            assert_eq!(first.id, "1", "快连接应该先显示，不等慢连接结束");
            let Some(NavigationEvent::Connection(slow)) = rx.recv().await else { panic!("slow") };
            assert_eq!(slow.id, "0");
            assert_eq!(slow.error.as_deref(), Some(METADATA_TIMEOUT_MESSAGE));
        };
        tokio::join!(producer, consumer);
    }

    #[tokio::test]
    async fn metadata_work_is_limited_and_releases_capacity_after_timeout() {
        let gate = Arc::new(MetadataGate::new());
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let work = (0..24).map(|index| {
            let gate = Arc::clone(&gate);
            let active = Arc::clone(&active);
            let peak = Arc::clone(&peak);
            async move {
                limited_metadata(gate, &format!("connection-{}", index % 3), async {
                    let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(count, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(2)).await;
                    active.fetch_sub(1, Ordering::SeqCst);
                }).await.unwrap();
            }
        });
        futures::future::join_all(work).await;
        assert_eq!(peak.load(Ordering::SeqCst), 4);
        assert_eq!(gate.global.available_permits(), 4);
        let cancelled = metadata_read_with_timeout(Duration::from_millis(10), limited_metadata(Arc::clone(&gate), "cancelled", std::future::pending::<()>())).await;
        assert!(cancelled.is_err());
        assert_eq!(gate.global.available_permits(), 4, "取消读取后归还并发名额");
    }
}
