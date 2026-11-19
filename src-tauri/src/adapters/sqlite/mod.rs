//! SQLite 适配器（文件型）。

pub(crate) mod connect;
mod syntax;

use crate::adapters::{Capabilities, Connector, EngineAdapter, EngineMeta, SqlSyntax, UrlScheme, UrlStyle, Workbench};
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

    fn defaults_to_local(&self) -> bool {
        true
    }

    fn reconnect_needs_saved_password(&self) -> bool {
        false
    }

    fn url_schemes(&self) -> &'static [UrlScheme] {
        const SCHEMES: &[UrlScheme] = &[UrlScheme::plain("sqlite")];
        SCHEMES
    }

    fn url_style(&self) -> UrlStyle {
        UrlStyle::FilePath
    }
}

impl EngineAdapter for SqliteAdapter {
    fn meta(&self) -> &dyn EngineMeta {
        self
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities { relational: true, sql_syntax_group: true, connector_group: true, ..Capabilities::default() }
    }

    fn syntax(&self) -> Option<&dyn SqlSyntax> {
        Some(&syntax::SqliteSyntax)
    }

    fn connector(&self) -> Option<&dyn Connector> {
        Some(&connect::SqliteConnector)
    }
}
