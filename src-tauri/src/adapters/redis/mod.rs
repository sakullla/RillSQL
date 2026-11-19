//! Redis 适配器：没有 SQL 面；A1 起迁入 `redis/` 后端并经 `as_redis()` 专属接口访问。

use crate::adapters::{Capabilities, EngineAdapter, EngineMeta, Workbench};
use crate::connections::model::Engine;

pub(in crate::adapters) struct RedisAdapter;

impl EngineMeta for RedisAdapter {
    fn engine(&self) -> Engine {
        Engine::Redis
    }

    fn id(&self) -> &'static str {
        "redis"
    }

    fn label(&self) -> &'static str {
        "Redis"
    }

    fn default_port(&self) -> Option<u16> {
        Some(6379)
    }

    fn file_based(&self) -> bool {
        false
    }

    fn workbench(&self) -> Workbench {
        Workbench::Redis
    }
}

impl EngineAdapter for RedisAdapter {
    fn meta(&self) -> &dyn EngineMeta {
        self
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities { relational: false, sql_syntax_group: false, ..Capabilities::default() }
    }
}
