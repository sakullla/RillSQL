//! MySQL 适配器。共享实现（语法等）以 `pub(in crate::adapters)` 暴露给 MariaDB 复用。

pub(crate) mod connect;
pub(in crate::adapters) mod syntax;

use crate::adapters::{Capabilities, Connector, EngineAdapter, EngineMeta, SqlSyntax, UrlScheme, Workbench};
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
        Capabilities { relational: true, sql_syntax_group: true, connector_group: true, ..Capabilities::default() }
    }

    fn syntax(&self) -> Option<&dyn SqlSyntax> {
        Some(&syntax::MysqlSyntax)
    }

    fn connector(&self) -> Option<&dyn Connector> {
        Some(&connect::MysqlConnector)
    }
}
