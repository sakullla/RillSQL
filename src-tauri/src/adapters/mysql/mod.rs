//! MySQL 适配器。共享实现（语法等）以 `pub(in crate::adapters)` 暴露给 MariaDB 复用。

pub(crate) mod catalog;
pub(crate) mod catalog_read;
pub(crate) mod connect;
pub(in crate::adapters) mod ddl;
pub(in crate::adapters) mod ddl_read;
pub(crate) mod diagram;
pub(crate) mod extras;
pub(crate) mod exec;
mod functions;
pub(crate) mod grid_read;
mod plan;
pub(in crate::adapters) mod syntax;
pub(in crate::adapters) mod types;

use crate::adapters::{Capabilities, CatalogDialect, Connector, DdlDialect, EngineAdapter, EngineMeta, SqlSyntax, TypeRules, UrlScheme, Workbench};
use crate::connections::model::{Engine, TlsMode};

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

    fn url_schemes(&self) -> &'static [UrlScheme] {
        const SCHEMES: &[UrlScheme] = &[UrlScheme::plain("mysql")];
        SCHEMES
    }

    fn url_tls(&self, param: &dyn Fn(&str) -> Option<String>) -> Option<TlsMode> {
        connect::url_tls(param)
    }
}

impl EngineAdapter for MysqlAdapter {
    fn meta(&self) -> &dyn EngineMeta {
        self
    }

    fn capabilities(&self) -> Capabilities {
        family_capabilities()
    }

    fn syntax(&self) -> Option<&dyn SqlSyntax> {
        Some(&syntax::MYSQL_SYNTAX)
    }

    fn types(&self) -> Option<&'static dyn TypeRules> {
        Some(&types::MysqlTypes)
    }

    fn ddl(&self) -> Option<&'static dyn DdlDialect> {
        Some(&ddl::MYSQL_DDL)
    }

    fn catalog(&self) -> Option<&dyn CatalogDialect> {
        Some(&catalog::MYSQL_CATALOG)
    }

    fn connector(&self) -> Option<&dyn Connector> {
        Some(&connect::MysqlConnector)
    }
}

/// MySQL 家族共用的能力声明：按库分的对象树，先读库名、对象按分组分页。
pub(in crate::adapters) fn family_capabilities() -> Capabilities {
    Capabilities {
        relational: true,
        sql_syntax_group: true,
        connector_group: true,
        catalog_group: true,
        type_rules_group: true,
        ddl_group: true,
        shows_databases: true,
        outline_navigation: true,
        ..Capabilities::default()
    }
}
