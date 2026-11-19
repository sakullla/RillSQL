//! MongoDB 的目录面（R12）：库列表 + 每库集合清单（estimatedDocumentCount 作为行估算），
//! 复用“库/无模式”的树形状；集合映射成表一种，树按“集合”渲染。

use crate::adapters::catalog::{mismatch, mismatch_body, CatalogDialect};
use crate::catalog::model::{known_row_estimate, DatabaseNode, KindReport, ObjectKind, ObjectRow, SchemaNode};
use crate::catalog::read::{definition_failure, empty_page, read_failure, requested_limit, ConnectionBody, ObjectPageBody, ObjectWindow};
use crate::connections::CatalogSession;

pub(in crate::adapters) struct MongoCatalog;

#[async_trait::async_trait]
impl CatalogDialect for MongoCatalog {
    fn listed_kinds(&self) -> &'static [ObjectKind] {
        &[ObjectKind::Table]
    }

    async fn read_connection(&self, session: &CatalogSession, page: usize, windows: &[ObjectWindow]) -> ConnectionBody {
        let CatalogSession::Mongo(client) = session else { return mismatch_body() };
        match read_mongo(client, page, windows).await {
            Ok(databases) => ConnectionBody { error: None, foreign_keys: None, databases },
            Err(message) => ConnectionBody { error: Some(message), foreign_keys: None, databases: Vec::new() },
        }
    }

    async fn read_outline(&self, session: &CatalogSession) -> ConnectionBody {
        let CatalogSession::Mongo(client) = session else { return mismatch_body() };
        match read_mongo_outline(client).await {
            Ok(databases) => ConnectionBody { error: None, foreign_keys: None, databases },
            Err(message) => ConnectionBody { error: Some(message), foreign_keys: None, databases: Vec::new() },
        }
    }

    async fn read_kind_page(&self, session: &CatalogSession, database: &str, _schema: &str, _kind: ObjectKind, offset: usize, limit: usize) -> ObjectPageBody {
        let CatalogSession::Mongo(client) = session else { return empty_page(mismatch()) };
        read_mongo_page(client, database, offset, limit).await
    }

    async fn read_object_page(&self, session: &CatalogSession, database: &str, _schema: &str, offset: usize, limit: usize) -> ObjectPageBody {
        let CatalogSession::Mongo(client) = session else { return empty_page(mismatch()) };
        read_mongo_page(client, database, offset, limit).await
    }

    async fn read_definition(
        &self,
        session: &CatalogSession,
        _database: &str,
        _schema: &str,
        _name: &str,
        _kind: &str,
        _signature: &str,
        _relation: &str,
    ) -> Result<String, String> {
        match session {
            CatalogSession::Mongo(_) => Err(definition_failure("MongoDB 集合没有 SQL 定义；文档在集合浏览器查看")),
            _ => Err(definition_failure("连接类型和引擎不一致")),
        }
    }
}

async fn database_names(client: &mongodb::Client) -> Result<Vec<String>, String> {
    let mut names = client.list_database_names().await.map_err(read_failure)?;
    names.retain(|name| !super::docs::catalog_hidden(name));
    names.sort();
    Ok(names)
}

async fn read_mongo(client: &mongodb::Client, page: usize, windows: &[ObjectWindow]) -> Result<Vec<DatabaseNode>, String> {
    let mut databases = Vec::new();
    for name in database_names(client).await? {
        let limit = requested_limit(&name, "", windows, page);
        let schema = read_mongo_objects(client, &name, 0, limit).await;
        databases.push(DatabaseNode { name, error: None, schemas: vec![schema] });
    }
    Ok(databases)
}

async fn read_mongo_outline(client: &mongodb::Client) -> Result<Vec<DatabaseNode>, String> {
    Ok(database_names(client)
        .await?
        .into_iter()
        .map(|name| DatabaseNode {
            name,
            error: None,
            schemas: vec![SchemaNode::empty("")],
        })
        .collect())
}

async fn read_mongo_page(client: &mongodb::Client, database: &str, offset: usize, limit: usize) -> ObjectPageBody {
    let schema = read_mongo_objects(client, database, offset, limit).await;
    ObjectPageBody {
        objects: schema.objects,
        total: schema.total,
        error: schema.error,
        kind_reports: schema.kind_reports,
    }
}

/// 集合清单 + estimatedDocumentCount 计数（树上的行估算）。计数失败不拖垮清单，只让估算留空。
async fn read_mongo_objects(client: &mongodb::Client, database: &str, offset: usize, limit: usize) -> SchemaNode {
    let mut names = match client.database(database).list_collection_names().await {
        Ok(names) => names,
        Err(message) => {
            let error = read_failure(message);
            return SchemaNode {
                name: String::new(),
                objects: Vec::new(),
                total: 0,
                error: Some(error),
                kind_reports: Vec::new(),
            };
        }
    };
    names.sort();
    let total = names.len();
    let window = offset.saturating_add(limit);
    let mut objects = Vec::new();
    for name in names.into_iter().take(window).skip(offset) {
        let estimated = client
            .database(database)
            .collection::<mongodb::bson::Document>(&name)
            .estimated_document_count()
            .await
            .ok()
            .and_then(|count| i64::try_from(count).ok());
        objects.push(ObjectRow {
            name,
            kind: ObjectKind::Table,
            estimated_rows: known_row_estimate(estimated),
            stale: false,
            signature: String::new(),
            relation: String::new(),
            materialized: false,
        });
    }
    SchemaNode {
        name: String::new(),
        objects,
        total,
        error: None,
        kind_reports: vec![KindReport {
            kind: ObjectKind::Table,
            total,
            error: None,
        }],
    }
}
