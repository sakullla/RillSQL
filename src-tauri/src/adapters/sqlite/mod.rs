//! SQLite 适配器（文件型）。

mod syntax;

use crate::adapters::{Capabilities, EngineAdapter, EngineMeta, SqlSyntax, Workbench};
use crate::connections::model::Engine;

pub(in crate::adapters) struct SqliteAdapter;

impl EngineMeta for SqliteAdapter {
    fn engine(&self) -> Engine {
        Engine::Sqlite
    }

    fn id(&self) -> &'static str {
        "sqlite"
    }

    fn label(&self) -> &'static str {
        "SQLite"
    }

    fn default_port(&self) -> Option<u16> {
        None
    }

    fn file_based(&self) -> bool {
        true
    }

    fn workbench(&self) -> Workbench {
        Workbench::Relational
    }
}

impl EngineAdapter for SqliteAdapter {
    fn meta(&self) -> &dyn EngineMeta {
        self
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities { relational: true, sql_syntax_group: true, ..Capabilities::default() }
    }

    fn syntax(&self) -> Option<&dyn SqlSyntax> {
        Some(&syntax::SqliteSyntax)
    }
}
