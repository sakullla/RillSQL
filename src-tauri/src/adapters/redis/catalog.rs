//! Redis 的目录面：没有库/模式/对象层级，键空间由键浏览器经 SCAN 分页读取。
//! 这里只给目录入口一致的回答，不编造对象。

use crate::adapters::catalog::{mismatch, mismatch_body, CatalogDialect};
use crate::catalog::model::ObjectKind;
use crate::catalog::read::{definition_failure, empty_page, ConnectionBody, ObjectPageBody, ObjectWindow};
use crate::connections::CatalogSession;

pub(in crate::adapters) struct RedisCatalog;

const KEYSPACE_PAGING: &str = "Redis 键空间经键浏览器分页读取，不走对象分页。";

#[async_trait::async_trait]
impl CatalogDialect for RedisCatalog {
    async fn read_connection(&self, session: &CatalogSession, _page: usize, _windows: &[ObjectWindow]) -> ConnectionBody {
        match session {
            CatalogSession::Redis(_) => ConnectionBody { error: None, foreign_keys: None, databases: Vec::new() },
            _ => mismatch_body(),
        }
    }

    async fn read_kind_page(&self, session: &CatalogSession, _database: &str, _schema: &str, _kind: ObjectKind, _offset: usize, _limit: usize) -> ObjectPageBody {
        match session {
            CatalogSession::Redis(_) => empty_page(KEYSPACE_PAGING),
            _ => empty_page(mismatch()),
        }
    }

    async fn read_object_page(&self, session: &CatalogSession, _database: &str, _schema: &str, _offset: usize, _limit: usize) -> ObjectPageBody {
        match session {
            CatalogSession::Redis(_) => empty_page(KEYSPACE_PAGING),
            _ => empty_page(mismatch()),
        }
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
            CatalogSession::Redis(_) => Err(definition_failure("Redis 没有对象定义")),
            _ => Err(definition_failure("连接类型和引擎不一致")),
        }
    }
}
