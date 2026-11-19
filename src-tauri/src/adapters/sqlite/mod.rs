//! SQLite 适配器（文件型）。

pub(crate) mod catalog;
pub(crate) mod catalog_read;
pub(crate) mod connect;
mod ddl;
pub(in crate::adapters) mod ddl_read;
pub(crate) mod diagram;
mod english;
pub(crate) mod extras;
pub(crate) mod exec;
mod functions;
pub(crate) mod grid_read;
mod plan;
pub(crate) mod file_page;
mod syntax;
pub(in crate::adapters) mod transfer;
mod types;

use crate::adapters::{Capabilities, CatalogDialect, Connector, DdlDialect, EngineAdapter, EngineMeta, ErrorRules, SqlSyntax, TransferDialect, TypeRules, UrlScheme, UrlStyle, Workbench};
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

    fn mark(&self) -> (&'static str, u32) {
        ("SL", 0x1d82d6)
    }

    fn url_example(&self) -> &'static str {
        "sqlite:///path/to/file.db"
    }

    fn messages_en(&self) -> &'static [(&'static str, &'static str)] {
        english::MESSAGES_EN
    }

    fn missing_items_en(&self) -> &'static [(&'static str, &'static str)] {
        english::MISSING_ITEMS_EN
    }

    fn file_extensions(&self) -> &'static [&'static str] {
        &["sqlite", "sqlite3", "db", "db3", "s3db"]
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
        Capabilities {
            relational: true,
            sql_syntax_group: true,
            connector_group: true,
            catalog_group: true,
            type_rules_group: true,
            ddl_group: true,
            error_rules_group: true,
            transfer_group: true,
            shows_foreign_keys: true,
            ..Capabilities::default()
        }
    }

    fn syntax(&self) -> Option<&dyn SqlSyntax> {
        Some(&syntax::SqliteSyntax)
    }

    fn types(&self) -> Option<&'static dyn TypeRules> {
        Some(&types::SqliteTypes)
    }

    fn ddl(&self) -> Option<&'static dyn DdlDialect> {
        Some(&ddl::SqliteDdl)
    }

    fn catalog(&self) -> Option<&dyn CatalogDialect> {
        Some(&catalog::SqliteCatalog)
    }

    fn connector(&self) -> Option<&dyn Connector> {
        Some(&connect::SqliteConnector)
    }

    fn errors(&self) -> Option<&dyn ErrorRules> {
        Some(&transfer::SqliteErrors)
    }

    fn transfer(&self) -> Option<&'static dyn TransferDialect> {
        Some(&transfer::SqliteTransfer)
    }
}
