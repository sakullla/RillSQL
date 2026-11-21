//! MySQL 适配器。共享实现（语法等）以 `pub(in crate::adapters)` 暴露给 MariaDB 复用。

pub(crate) mod catalog;
pub(crate) mod catalog_read;
pub(crate) mod connect;
pub(in crate::adapters) mod ddl;
pub(in crate::adapters) mod ddl_read;
pub(crate) mod diagram;
pub(in crate::adapters) mod english;
pub(crate) mod extras;
pub(crate) mod exec;
mod functions;
pub(crate) mod grid_read;
pub(in crate::adapters) mod literal;
pub(in crate::adapters) mod object_ddl;
pub(in crate::adapters) mod object_read;
mod plan;
pub(in crate::adapters) mod profile;
pub(in crate::adapters) mod table_ddl;
pub(in crate::adapters) mod table_read;
#[cfg(test)]
mod table_live_tests;
pub(in crate::adapters) mod syntax;
pub(in crate::adapters) mod transfer;
pub(in crate::adapters) mod types;

use crate::adapters::{
    Capabilities, CatalogDialect, Connector, DdlDialect, EngineAdapter, EngineMeta, ErrorRules, FieldHint, SqlSyntax, TransferDialect, TypeRules, UrlScheme, Workbench,
};
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

    fn mark(&self) -> (&'static str, u32) {
        ("MY", 0x0284a8)
    }

    fn url_example(&self) -> &'static str {
        "mysql://user@host:3306/db"
    }

    fn messages_en(&self) -> &'static [(&'static str, &'static str)] {
        english::MESSAGES_EN
    }

    fn missing_items_en(&self) -> &'static [(&'static str, &'static str)] {
        english::MISSING_ITEMS_EN
    }

    fn database_hint_key(&self) -> &'static str {
        "ui.conn.dbPlaceholderMysql"
    }

    fn username_hint(&self) -> FieldHint {
        FieldHint::Text("root")
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

    fn errors(&self) -> Option<&dyn ErrorRules> {
        Some(&transfer::MysqlErrors)
    }

    fn transfer(&self) -> Option<&'static dyn TransferDialect> {
        Some(&transfer::MYSQL_TRANSFER)
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
        error_rules_group: true,
        transfer_group: true,
        shows_databases: true,
        outline_navigation: true,
        ..Capabilities::default()
    }
}
