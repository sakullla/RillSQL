//! MariaDB 适配器：独立入口，内部复用 MySQL 家族的共享实现。

use crate::adapters::mysql;
use crate::adapters::{Capabilities, Connector, EngineAdapter, EngineMeta, SqlSyntax, UrlScheme, Workbench};
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
        Capabilities { relational: true, sql_syntax_group: true, connector_group: true, ..Capabilities::default() }
    }

    fn syntax(&self) -> Option<&dyn SqlSyntax> {
        Some(&mysql::syntax::MysqlSyntax)
    }

    fn connector(&self) -> Option<&dyn Connector> {
        Some(&mysql::connect::MysqlConnector)
    }
}
