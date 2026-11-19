//! SQL Server 适配器。

mod syntax;

use crate::adapters::{Capabilities, EngineAdapter, EngineMeta, SqlSyntax, Workbench};
use crate::connections::model::Engine;

pub(in crate::adapters) struct SqlserverAdapter;

impl EngineMeta for SqlserverAdapter {
    fn engine(&self) -> Engine {
        Engine::Sqlserver
    }

    fn id(&self) -> &'static str {
        "sqlserver"
    }

    fn label(&self) -> &'static str {
        "SQL Server"
    }

    fn default_port(&self) -> Option<u16> {
        Some(1433)
    }

    fn file_based(&self) -> bool {
        false
    }

    fn workbench(&self) -> Workbench {
        Workbench::Relational
    }
}

impl EngineAdapter for SqlserverAdapter {
    fn meta(&self) -> &dyn EngineMeta {
        self
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities { relational: true, sql_syntax_group: true, ..Capabilities::default() }
    }

    fn syntax(&self) -> Option<&dyn SqlSyntax> {
        Some(&syntax::SqlserverSyntax)
    }
}
