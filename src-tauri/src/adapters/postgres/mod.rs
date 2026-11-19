//! PostgreSQL 适配器。

pub(crate) mod connect;
mod syntax;

use crate::adapters::{Capabilities, Connector, EngineAdapter, EngineMeta, SqlSyntax, UrlScheme, Workbench};
use crate::connections::model::Engine;

pub(in crate::adapters) struct PostgresAdapter;

impl EngineMeta for PostgresAdapter {
    fn engine(&self) -> Engine {
        Engine::Postgres
    }

    fn id(&self) -> &'static str {
        "postgres"
    }

    fn label(&self) -> &'static str {
        "PostgreSQL"
    }

    fn default_port(&self) -> Option<u16> {
        Some(5432)
    }

    fn file_based(&self) -> bool {
        false
    }

    fn workbench(&self) -> Workbench {
        Workbench::Relational
    }

    fn url_schemes(&self) -> &'static [UrlScheme] {
        const SCHEMES: &[UrlScheme] = &[UrlScheme::plain("postgresql"), UrlScheme::plain("postgres")];
        SCHEMES
    }
}

impl EngineAdapter for PostgresAdapter {
    fn meta(&self) -> &dyn EngineMeta {
        self
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            relational: true,
            sql_syntax_group: true,
            connector_group: true,
            database_scoped_session: true,
            ..Capabilities::default()
        }
    }

    fn syntax(&self) -> Option<&dyn SqlSyntax> {
        Some(&syntax::PostgresSyntax)
    }

    fn connector(&self) -> Option<&dyn Connector> {
        Some(&connect::PostgresConnector)
    }
}
