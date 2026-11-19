//! SQL Server 适配器。

pub(crate) mod connect;
mod syntax;

use crate::adapters::{Capabilities, Connector, EngineAdapter, EngineMeta, SqlSyntax, UrlScheme, UrlStyle, Workbench};
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
        Capabilities { relational: true, sql_syntax_group: true, connector_group: true, ..Capabilities::default() }
    }

    fn syntax(&self) -> Option<&dyn SqlSyntax> {
        Some(&syntax::SqlserverSyntax)
    }

    fn connector(&self) -> Option<&dyn Connector> {
        Some(&connect::SqlserverConnector)
    }
}
