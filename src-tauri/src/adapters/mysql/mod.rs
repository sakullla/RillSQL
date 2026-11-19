//! MySQL 适配器。共享实现（语法等）以 `pub(in crate::adapters)` 暴露给 MariaDB 复用。

pub(in crate::adapters) mod syntax;

use crate::adapters::{Capabilities, EngineAdapter, EngineMeta, SqlSyntax, Workbench};
use crate::connections::model::Engine;

pub(in crate::adapters) struct MysqlAdapter;

impl EngineMeta for MysqlAdapter {
    fn engine(&self) -> Engine {
        Engine::Mysql
    }

    fn id(&self) -> &'static str {
        "mysql"
    }

    fn label(&self) -> &'static str {
        "MySQL"
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

impl EngineAdapter for MysqlAdapter {
    fn meta(&self) -> &dyn EngineMeta {
        self
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities { relational: true, sql_syntax_group: true, ..Capabilities::default() }
    }

    fn syntax(&self) -> Option<&dyn SqlSyntax> {
        Some(&syntax::MysqlSyntax)
    }
}
