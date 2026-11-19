//! MariaDB 适配器：独立入口，内部复用 MySQL 家族的共享实现。

use crate::adapters::mysql;
use crate::adapters::{FieldHint, 
    Capabilities, CatalogDialect, Connector, DdlDialect, EngineAdapter, EngineMeta, ErrorRules, SqlSyntax, TransferDialect, TypeRules, UrlScheme, Workbench,
};
use crate::connections::model::{Engine, TlsMode};

pub(in crate::adapters) struct MariadbAdapter;

impl EngineMeta for MariadbAdapter {
    fn engine(&self) -> Engine {
        Engine::Mariadb
    }

    fn id(&self) -> &'static str {
        "mariadb"
    }

    fn label(&self) -> &'static str {
        "MariaDB"
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
        ("MA", 0xb46346)
    }

    fn url_example(&self) -> &'static str {
        "mariadb://user@host:3306/db"
    }

    fn messages_en(&self) -> &'static [(&'static str, &'static str)] {
        mysql::english::MESSAGES_EN
    }

    fn missing_items_en(&self) -> &'static [(&'static str, &'static str)] {
        mysql::english::MISSING_ITEMS_EN
    }

    fn database_hint_key(&self) -> &'static str {
        "ui.conn.dbPlaceholderMysql"
    }

    fn username_hint(&self) -> FieldHint {
        FieldHint::Text("root")
    }

    fn url_schemes(&self) -> &'static [UrlScheme] {
        const SCHEMES: &[UrlScheme] = &[UrlScheme::plain("mariadb")];
        SCHEMES
    }

    fn url_tls(&self, param: &dyn Fn(&str) -> Option<String>) -> Option<TlsMode> {
        mysql::connect::url_tls(param)
    }
}

impl EngineAdapter for MariadbAdapter {
    fn meta(&self) -> &dyn EngineMeta {
        self
    }

    fn capabilities(&self) -> Capabilities {
        mysql::family_capabilities()
    }

    fn syntax(&self) -> Option<&dyn SqlSyntax> {
        Some(&mysql::syntax::MARIADB_SYNTAX)
    }

    fn types(&self) -> Option<&'static dyn TypeRules> {
        Some(&mysql::types::MysqlTypes)
    }

    fn ddl(&self) -> Option<&'static dyn DdlDialect> {
        Some(&mysql::ddl::MARIADB_DDL)
    }

    fn catalog(&self) -> Option<&dyn CatalogDialect> {
        Some(&mysql::catalog::MARIADB_CATALOG)
    }

    fn connector(&self) -> Option<&dyn Connector> {
        Some(&mysql::connect::MysqlConnector)
    }

    fn errors(&self) -> Option<&dyn ErrorRules> {
        Some(&mysql::transfer::MysqlErrors)
    }

    fn transfer(&self) -> Option<&'static dyn TransferDialect> {
        Some(&mysql::transfer::MARIADB_TRANSFER)
    }
}
