//! SQL Server 适配器。

pub(crate) mod catalog;
pub(crate) mod catalog_read;
pub(crate) mod connect;
pub(crate) mod diagram;
pub(crate) mod ddl;
pub(crate) mod ddl_read;
mod syntax;
mod types;

use crate::adapters::{Capabilities, CatalogDialect, Connector, DdlDialect, EngineAdapter, EngineMeta, SqlSyntax, TypeRules, UrlScheme, UrlStyle, Workbench};
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

    fn url_schemes(&self) -> &'static [UrlScheme] {
        const SCHEMES: &[UrlScheme] = &[UrlScheme::plain("sqlserver"), UrlScheme::plain("mssql")];
        SCHEMES
    }

    fn url_style(&self) -> UrlStyle {
        UrlStyle::Jdbc
    }
}

impl EngineAdapter for SqlserverAdapter {
    fn meta(&self) -> &dyn EngineMeta {
        self
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            relational: true,
            sql_syntax_group: true,
            connector_group: true,
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
        Some(&syntax::SqlserverSyntax)
    }

    fn types(&self) -> Option<&'static dyn TypeRules> {
        Some(&types::SqlserverTypes)
    }

    fn ddl(&self) -> Option<&'static dyn DdlDialect> {
        Some(&ddl::SqlserverDdl)
    }

    fn catalog(&self) -> Option<&dyn CatalogDialect> {
        Some(&catalog::SqlserverCatalog)
    }

    fn connector(&self) -> Option<&dyn Connector> {
        Some(&connect::SqlserverConnector)
    }
}
