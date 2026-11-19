//! PostgreSQL 适配器。

pub(crate) mod catalog;
pub(crate) mod catalog_read;
pub(crate) mod connect;
pub(crate) mod diagram;
mod ddl;
pub(in crate::adapters) mod ddl_read;
pub(crate) mod extras;
mod syntax;
mod types;

use crate::adapters::{Capabilities, CatalogDialect, Connector, DdlDialect, EngineAdapter, EngineMeta, SqlSyntax, TypeRules, UrlScheme, Workbench};
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
            catalog_group: true,
            type_rules_group: true,
            ddl_group: true,
            shows_databases: true,
            shows_schemas: true,
            outline_navigation: true,
            ..Capabilities::default()
        }
    }

    fn syntax(&self) -> Option<&dyn SqlSyntax> {
        Some(&syntax::PostgresSyntax)
    }

    fn types(&self) -> Option<&'static dyn TypeRules> {
        Some(&types::PostgresTypes)
    }

    fn ddl(&self) -> Option<&'static dyn DdlDialect> {
        Some(&ddl::PostgresDdl)
    }

    fn catalog(&self) -> Option<&dyn CatalogDialect> {
        Some(&catalog::PostgresCatalog)
    }

    fn connector(&self) -> Option<&dyn Connector> {
        Some(&connect::PostgresConnector)
    }
}
