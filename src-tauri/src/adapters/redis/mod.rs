//! Redis 适配器：没有 SQL 面。
//!
//! 键浏览器后端（R11）：SCAN 分页、类型化值读取、TTL 修改、删除/重命名与命令执行通道。
//! 键空间浏览一律走 SCAN 游标（禁 KEYS）；写删类操作由前端确认后携带 confirmed 标记执行。
//! 会话经 `Session::as_redis()` 专属接口取得。

pub(crate) mod catalog;
pub mod commands;
pub(crate) mod complete;
pub(crate) mod connect;
pub(crate) mod console;
mod english;
pub(crate) mod exec;
pub(crate) mod keys;

use crate::adapters::{FieldHint, Capabilities, CatalogDialect, Connector, EngineAdapter, EngineMeta, FormField, UrlScheme, Workbench};
use crate::connections::model::{Engine, SavedConnection, TlsMode};

pub fn module_id() -> &'static str {
    let _linked = (
        std::any::type_name_of_val(&commands::redis_scan_keys),
        std::any::type_name_of_val(&commands::redis_read_key),
        std::any::type_name_of_val(&exec::run_command),
        std::any::type_name::<keys::ScanPage>(),
    );
    "redis"
}

pub(in crate::adapters) struct RedisAdapter;

impl EngineMeta for RedisAdapter {
    fn engine(&self) -> Engine {
        Engine::Redis
    }

    fn id(&self) -> &'static str {
        "redis"
    }

    fn label(&self) -> &'static str {
        "Redis"
    }

    fn default_port(&self) -> Option<u16> {
        Some(6379)
    }

    fn file_based(&self) -> bool {
        false
    }

    fn workbench(&self) -> Workbench {
        Workbench::Redis
    }

    fn mark(&self) -> (&'static str, u32) {
        ("RD", 0xdc3d35)
    }

    fn url_example(&self) -> &'static str {
        "redis://host:6379/0"
    }

    fn messages_en(&self) -> &'static [(&'static str, &'static str)] {
        english::MESSAGES_EN
    }

    fn missing_items_en(&self) -> &'static [(&'static str, &'static str)] {
        english::MISSING_ITEMS_EN
    }

    fn database_hint_key(&self) -> &'static str {
        "ui.conn.redisDb"
    }

    fn username_hint(&self) -> FieldHint {
        FieldHint::Key("ui.conn.redisAclHint")
    }

    fn defaults_to_local(&self) -> bool {
        true
    }

    /// 库索引从 0 起。
    fn default_database(&self) -> &'static str {
        "0"
    }

    /// 允许无 ACL 用户名（密码认证）。
    fn username_optional(&self) -> bool {
        true
    }

    fn database_is_index(&self) -> bool {
        true
    }

    /// 库索引排在凭据前面：连 Redis 先选库再认证是最常见的填写顺序。
    fn general_fields(&self) -> &'static [FormField] {
        &[FormField::Host, FormField::Port, FormField::Database, FormField::Password, FormField::Username, FormField::SavePassword]
    }

    /// 允许无密码直连（本地开发常见）：空密码不是“凭据缺失”，照常建连。
    fn password_optional(&self, _username: &str) -> bool {
        true
    }

    fn reconnect_needs_saved_password(&self) -> bool {
        false
    }

    fn auth_stage_note(&self, config: &SavedConnection) -> String {
        let user = config.username.clone().unwrap_or_default();
        if user.is_empty() {
            "密码认证".to_owned()
        } else {
            format!("ACL 用户 {user}")
        }
    }

    fn url_schemes(&self) -> &'static [UrlScheme] {
        const SCHEMES: &[UrlScheme] = &[UrlScheme { name: "rediss", implies_tls: true, discovered_port: false }, UrlScheme::plain("redis")];
        SCHEMES
    }

    fn url_tls(&self, param: &dyn Fn(&str) -> Option<String>) -> Option<TlsMode> {
        tls_flag(param)
    }
}

/// `tls=` / `ssl=` 开关参数（Redis 与 MongoDB 连接串共用的写法）。
pub(in crate::adapters) fn tls_flag(param: &dyn Fn(&str) -> Option<String>) -> Option<TlsMode> {
    let flag = param("tls").or_else(|| param("ssl"))?.to_ascii_lowercase();
    Some(if matches!(flag.as_str(), "true" | "yes" | "1") { TlsMode::Require } else { TlsMode::Off })
}

impl EngineAdapter for RedisAdapter {
    fn meta(&self) -> &dyn EngineMeta {
        self
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            relational: false,
            sql_syntax_group: false,
            connector_group: true,
            catalog_group: true,
            shows_databases: true,
            ..Capabilities::default()
        }
    }

    fn catalog(&self) -> Option<&dyn CatalogDialect> {
        Some(&catalog::RedisCatalog)
    }

    fn connector(&self) -> Option<&dyn Connector> {
        Some(&connect::RedisConnector)
    }
}

#[cfg(test)]
mod tests;
