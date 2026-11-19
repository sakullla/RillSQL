//! MongoDB 适配器：没有 SQL 面；A1 起迁入 `mongo/` 后端并经 `as_mongo()` 专属接口访问。

use crate::adapters::{Capabilities, EngineAdapter, EngineMeta, Workbench};
use crate::connections::model::Engine;

pub(in crate::adapters) struct MongodbAdapter;

impl EngineMeta for MongodbAdapter {
    fn engine(&self) -> Engine {
        Engine::Mongodb
    }

    fn id(&self) -> &'static str {
        "mongodb"
    }

    fn label(&self) -> &'static str {
        "MongoDB"
    }

    fn default_port(&self) -> Option<u16> {
        Some(27017)
    }

    fn file_based(&self) -> bool {
        false
    }

    fn workbench(&self) -> Workbench {
        Workbench::Mongo
    }
}

impl EngineAdapter for MongodbAdapter {
    fn meta(&self) -> &dyn EngineMeta {
        self
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities { relational: false, sql_syntax_group: false, ..Capabilities::default() }
    }
}
