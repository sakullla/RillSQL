//! MongoDB 适配器：没有 SQL 面。
//!
//! 集合文档浏览器后端（R12）：库/集合清单、文档分页（JSON 过滤带定位提示）、文档 CRUD 与删除确认。
//! 不做聚合管道构建器、GridFS 与索引管理 UI；JSON 编辑一律先校验后落库。
//! 会话经 `Session::as_mongo()` 专属接口取得。

pub mod commands;
pub(crate) mod connect;
pub(crate) mod docs;

use crate::adapters::{Capabilities, Connector, EngineAdapter, EngineMeta, FormField, UrlScheme, UrlStyle, Workbench};
use crate::connections::model::{Engine, SavedConnection, TlsMode};

pub fn module_id() -> &'static str {
    let _linked = (
        std::any::type_name_of_val(&commands::mongo_list_databases),
        std::any::type_name_of_val(&commands::mongo_find_documents),
        std::any::type_name_of_val(&docs::find_documents),
        std::any::type_name::<docs::DocumentsPage>(),
        std::any::type_name::<docs::CountMode>(),
    );
    "mongo"
}

pub(in crate::adapters) struct MongodbAdapter;

impl EngineMeta for MongodbAdapter {
    fn engine(&self) -> Engine {
        Engine::Mongodb
    }

    fn id(&self) -> &'static str {
        "mongodb"
    }

    fn label(&self) -> &'static str {
        "MongoDB"
    }

    fn default_port(&self) -> Option<u16> {
        Some(27017)
    }

    fn file_based(&self) -> bool {
        false
    }

    fn workbench(&self) -> Workbench {
        Workbench::Mongo
    }

    fn defaults_to_local(&self) -> bool {
        true
    }

    /// 允许无认证直连（本地开发）。
    fn username_optional(&self) -> bool {
        true
    }

    fn host_is_uri(&self, host: &str) -> bool {
        connect::is_uri(host)
    }

    fn uses_replica_set(&self) -> bool {
        true
    }

    fn general_fields(&self) -> &'static [FormField] {
        &[
            FormField::Host,
            FormField::Port,
            FormField::Username,
            FormField::Password,
            FormField::Database,
            FormField::ReplicaSet,
            FormField::SavePassword,
        ]
    }

    /// 无用户名时是无认证直连；有用户名而没有密码仍按凭据缺失报错。
    fn password_optional(&self, username: &str) -> bool {
        username.trim().is_empty()
    }

    fn reconnect_needs_saved_password(&self) -> bool {
        false
    }

    fn auth_stage_note(&self, config: &SavedConnection) -> String {
        let user = config.username.clone().unwrap_or_default();
        let source = config
            .database
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "admin".to_owned());
        if user.is_empty() {
            "无认证直连".to_owned()
        } else {
            format!("SCRAM 用户 {user}（认证库 {source}）")
        }
    }

    fn url_schemes(&self) -> &'static [UrlScheme] {
        const SCHEMES: &[UrlScheme] = &[UrlScheme { name: "mongodb+srv", implies_tls: true, discovered_port: true }, UrlScheme::plain("mongodb")];
        SCHEMES
    }

    fn url_style(&self) -> UrlStyle {
        UrlStyle::FullUri
    }

    fn url_tls(&self, param: &dyn Fn(&str) -> Option<String>) -> Option<TlsMode> {
        crate::adapters::redis::tls_flag(param)
    }
}

impl EngineAdapter for MongodbAdapter {
    fn meta(&self) -> &dyn EngineMeta {
        self
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities { relational: false, sql_syntax_group: false, connector_group: true, ..Capabilities::default() }
    }

    fn connector(&self) -> Option<&dyn Connector> {
        Some(&connect::MongoConnector)
    }
}

#[cfg(test)]
mod tests;
