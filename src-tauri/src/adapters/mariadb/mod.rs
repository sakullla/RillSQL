//! MariaDB 适配器：独立入口，内部复用 MySQL 家族的共享实现。

use crate::adapters::mysql;
use crate::adapters::{Capabilities, EngineAdapter, EngineMeta, SqlSyntax, Workbench};
use crate::connections::model::Engine;

pub(in crate::adapters) struct MariadbAdapter;

impl EngineMeta for MariadbAdapter {
    fn engine(&self) -> Engine {
        Engine::Mariadb
    }

    fn id(&self) -> &'static str {
        "mariadb"
    }

    fn label(&self) -> &'static str {
        "MariaDB"
    }

    fn default_port(&self) -> Option<u16> {
        Some(3306)
    }

    fn file_based(&self) -> bool {
        false
    }

    fn workbench(&self) -> Workbench {
        Workbench::Relational
    }
}

impl EngineAdapter for MariadbAdapter {
    fn meta(&self) -> &dyn EngineMeta {
        self
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities { relational: true, sql_syntax_group: true, ..Capabilities::default() }
    }

    fn syntax(&self) -> Option<&dyn SqlSyntax> {
        Some(&mysql::syntax::MysqlSyntax)
    }
}
