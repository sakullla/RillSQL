mod catalog;
mod compare;
mod connections;
mod ddl;
mod diagram;
mod dropped;
mod dump;
mod grid;
mod migrate;
mod mongo;
mod native_export;
mod product;
mod query;
mod redis;

use serde::Serialize;

#[derive(Serialize)]
struct AppInfo {
    title: &'static str,
    name_zh: &'static str,
    name_en: &'static str,
    license: &'static str,
    data_dir: String,
}

#[tauri::command]
fn app_info() -> AppInfo {
    AppInfo {
        title: product::TITLE,
        name_zh: product::NAME_ZH,
        name_en: product::NAME_EN,
        license: product::LICENSE,
        data_dir: local_data_dir(),
    }
}

fn local_data_dir() -> String {
    directories::ProjectDirs::from("app", "RillSQL", "RillSQL")
        .map(|dirs| dirs.data_dir().to_string_lossy().into_owned())
        .unwrap_or_else(|| "RillSQL".to_owned())
}

fn wire<R: tauri::Runtime>(builder: tauri::Builder<R>, state: connections::commands::ConnectionsState) -> tauri::Builder<R> {
    let query_state = query::QueryState::from_config_path(state.service.config_path());
    builder.manage(state).manage(query_state).invoke_handler(tauri::generate_handler![
        app_info,
        native_export::export_write_loaded,
        dropped::read_dropped_preview,
        connections::commands::connections_list,
        connections::commands::connections_save,
        connections::commands::connections_delete,
        connections::commands::connections_test,
        connections::commands::connections_open,
        connections::commands::connections_focus_database,
        connections::commands::connections_disconnect,
        connections::commands::connections_confirm_host_key,
        connections::commands::connections_forget_host_key,
        connections::commands::connections_export,
        connections::commands::connections_import_file,
        connections::commands::env_tags_list,
        connections::commands::env_tags_save,
        connections::commands::env_tags_delete,
        catalog::commands::catalog_sessions,
        catalog::commands::catalog_refresh,
        catalog::commands::catalog_page,
        catalog::commands::catalog_definition,
        compare::commands::compare_structure,
        compare::commands::compare_data,
        compare::commands::compare_cancel,
        compare::commands::compare_schemas,
        grid::commands::grid_describe,
        grid::commands::grid_page,
        grid::commands::grid_count,
        grid::commands::grid_commit,
        grid::commands::grid_export,
        grid::commands::grid_export_cancel,
        dump::commands::database_dump,
        dump::commands::database_dump_cancel,
        migrate::commands::migrate_data,
        migrate::commands::migrate_cancel,
        redis::commands::redis_scan_keys,
        redis::commands::redis_read_key,
        redis::commands::redis_set_ttl,
        redis::commands::redis_delete_keys,
        redis::commands::redis_rename_key,
        redis::commands::redis_exec,
        mongo::commands::mongo_list_databases,
        mongo::commands::mongo_list_collections,
        mongo::commands::mongo_find_documents,
        mongo::commands::mongo_read_document,
        mongo::commands::mongo_replace_document,
        mongo::commands::mongo_insert_document,
        mongo::commands::mongo_delete_document,
        ddl::commands::ddl_describe,
        ddl::commands::ddl_preview,
        ddl::commands::ddl_execute,
        diagram::commands::diagram_read,
        query::commands::query_start,
        query::commands::query_poll,
        query::commands::query_release,
        query::commands::query_cancel,
        query::commands::query_history,
        query::commands::query_history_clear,
        query::commands::query_history_delete,
        query::commands::query_saved_list,
        query::commands::query_saved_save,
        query::commands::query_saved_rename,
        query::commands::query_saved_delete,
    ])
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let _ = fixed_dependency_markers();
    wire(tauri::Builder::default(), connections::commands::ConnectionsState::new())
        .plugin(tauri_plugin_dialog::init())
        .run(tauri::generate_context!())
        .expect("启动澄库 · RillSQL 失败");
}

#[derive(thiserror::Error, Debug)]
#[allow(dead_code)]
enum FixedDependencyError {
    #[error("占位错误，后续模块替换为真实失败")]
    Placeholder,
}

#[async_trait::async_trait]
#[allow(dead_code)]
trait FixedDependencyEntry: Send {
    async fn ready(&self) -> bool;
}

fn fixed_dependency_markers() -> usize {
    let markers: &[&str] = &[
        std::any::type_name::<sqlx::sqlite::SqliteConnectOptions>(),
        std::any::type_name::<tokio_postgres::Config>(),
        std::any::type_name::<tokio_postgres_rustls::MakeRustlsConnect>(),
        std::any::type_name::<mysql_async::Opts>(),
        std::any::type_name::<tiberius::Config>(),
        std::any::type_name::<tiberius::EncryptionLevel>(),
        std::any::type_name::<::redis::Client>(),
        std::any::type_name::<::redis::aio::MultiplexedConnection>(),
        std::any::type_name::<::mongodb::Client>(),
        std::any::type_name::<mongodb::options::ClientOptions>(),
        std::any::type_name::<tokio_util::compat::Compat<tokio::net::TcpStream>>(),
        std::any::type_name::<keyring::Entry>(),
        std::any::type_name::<russh::client::Config>(),
        std::any::type_name::<csv::Writer<Vec<u8>>>(),
        std::any::type_name::<chrono::DateTime<chrono::Utc>>(),
        std::any::type_name::<uuid::Uuid>(),
        std::any::type_name::<tracing::Level>(),
        std::any::type_name::<futures::future::Ready<()>>(),
        std::any::type_name::<FixedDependencyError>(),
        std::any::type_name::<&dyn FixedDependencyEntry>(),
        std::any::type_name::<tokio::runtime::Runtime>(),
        connections::module_id(),
        catalog::module_id(),
        query::module_id(),
        grid::module_id(),
        ddl::module_id(),
        diagram::module_id(),
        compare::module_id(),
        dump::module_id(),
        migrate::module_id(),
        redis::module_id(),
        mongo::module_id(),
    ];
    markers.len()
}

#[cfg(test)]
mod tests {
    use super::product;

    const CONNECTION_COMMANDS: &[&str] = &[
        "connections_list",
        "connections_save",
        "connections_delete",
        "connections_test",
        "connections_open",
        "connections_focus_database",
        "connections_disconnect",
        "connections_confirm_host_key",
        "connections_forget_host_key",
        "connections_export",
        "connections_import_file",
        "env_tags_list",
        "env_tags_save",
        "env_tags_delete",
    ];

    /// 全库导出（R7）的命令也要三件套齐备：lib.rs 注册、权限清单、能力文件。
    const DUMP_COMMANDS: &[&str] = &["database_dump", "database_dump_cancel"];

    /// 跨库迁移（R8）的命令同样要三件套齐备：lib.rs 注册、权限清单、能力文件。
    const MIGRATE_COMMANDS: &[&str] = &["migrate_data", "migrate_cancel"];

    /// 拖放预览（R9）的命令同样要三件套齐备：lib.rs 注册、权限清单、能力文件。
    const DROPPED_COMMANDS: &[&str] = &["read_dropped_preview"];

    /// Redis 键浏览器（R11）的命令同样要三件套齐备：lib.rs 注册、权限清单、能力文件。
    const REDIS_COMMANDS: &[&str] = &[
        "redis_scan_keys",
        "redis_read_key",
        "redis_set_ttl",
        "redis_delete_keys",
        "redis_rename_key",
        "redis_exec",
    ];

    /// MongoDB 集合文档浏览器（R12）的命令同样要三件套齐备：lib.rs 注册、权限清单、能力文件。
    const MONGO_COMMANDS: &[&str] = &[
        "mongo_list_databases",
        "mongo_list_collections",
        "mongo_find_documents",
        "mongo_read_document",
        "mongo_replace_document",
        "mongo_insert_document",
        "mongo_delete_document",
    ];

    #[test]
    fn title_names_the_product() {
        assert_eq!(product::TITLE, "澄库 · RillSQL");
        assert!(product::TITLE.contains(product::NAME_ZH));
        assert!(product::TITLE.contains(product::NAME_EN));
        assert_eq!(product::LICENSE, "GPL-3.0-only");
    }

    #[test]
    fn module_entries_are_linked() {
        assert_eq!(super::connections::module_id(), "connections");
        assert_eq!(super::catalog::module_id(), "catalog");
        assert_eq!(super::query::module_id(), "query");
        assert_eq!(super::grid::module_id(), "grid");
        assert_eq!(super::ddl::module_id(), "ddl");
        assert_eq!(super::diagram::module_id(), "diagram");
        assert_eq!(super::compare::module_id(), "compare");
        assert_eq!(super::dump::module_id(), "dump");
        assert_eq!(super::migrate::module_id(), "migrate");
        assert!(super::fixed_dependency_markers() >= 12);
        assert!(!super::local_data_dir().is_empty());
    }

    #[test]
    fn main_window_registers_dump_commands() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let capability: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(root.join("capabilities/default.json")).unwrap()).unwrap();
        let permissions = capability["permissions"].as_array().unwrap();
        let acl = std::fs::read_to_string(root.join("permissions/app-commands.toml")).unwrap();
        let registration = std::fs::read_to_string(root.join("src/lib.rs")).unwrap();
        let wire = registration.split("fn wire").nth(1).unwrap_or("");
        let wire = wire.split("pub fn run").next().unwrap_or("");
        for command in DUMP_COMMANDS {
            let allow = format!("allow-{}", command.replace('_', "-"));
            assert!(permissions.iter().any(|item| item.as_str() == Some(allow.as_str())), "{allow}");
            assert!(acl.contains(&format!("commands.allow = [\"{command}\"]")), "{command}");
            assert!(wire.contains(&format!("dump::commands::{command}")), "{command}");
        }
    }

    #[test]
    fn main_window_registers_migrate_commands() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let capability: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(root.join("capabilities/default.json")).unwrap()).unwrap();
        let permissions = capability["permissions"].as_array().unwrap();
        let acl = std::fs::read_to_string(root.join("permissions/app-commands.toml")).unwrap();
        let registration = std::fs::read_to_string(root.join("src/lib.rs")).unwrap();
        let wire = registration.split("fn wire").nth(1).unwrap_or("");
        let wire = wire.split("pub fn run").next().unwrap_or("");
        for command in MIGRATE_COMMANDS {
            let allow = format!("allow-{}", command.replace('_', "-"));
            assert!(permissions.iter().any(|item| item.as_str() == Some(allow.as_str())), "{allow}");
            assert!(acl.contains(&format!("commands.allow = [\"{command}\"]")), "{command}");
            assert!(wire.contains(&format!("migrate::commands::{command}")), "{command}");
        }
    }

    #[test]
    fn main_window_registers_dropped_commands() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let capability: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(root.join("capabilities/default.json")).unwrap()).unwrap();
        let permissions = capability["permissions"].as_array().unwrap();
        let acl = std::fs::read_to_string(root.join("permissions/app-commands.toml")).unwrap();
        let registration = std::fs::read_to_string(root.join("src/lib.rs")).unwrap();
        let wire = registration.split("fn wire").nth(1).unwrap_or("");
        let wire = wire.split("pub fn run").next().unwrap_or("");
        for command in DROPPED_COMMANDS {
            let allow = format!("allow-{}", command.replace('_', "-"));
            assert!(permissions.iter().any(|item| item.as_str() == Some(allow.as_str())), "{allow}");
            assert!(acl.contains(&format!("commands.allow = [\"{command}\"]")), "{command}");
            assert!(wire.contains(&format!("dropped::{command}")), "{command}");
        }
    }

    #[test]
    fn main_window_registers_mongo_commands() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let capability: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(root.join("capabilities/default.json")).unwrap()).unwrap();
        let permissions = capability["permissions"].as_array().unwrap();
        let acl = std::fs::read_to_string(root.join("permissions/app-commands.toml")).unwrap();
        let registration = std::fs::read_to_string(root.join("src/lib.rs")).unwrap();
        let wire = registration.split("fn wire").nth(1).unwrap_or("");
        let wire = wire.split("pub fn run").next().unwrap_or("");
        for command in MONGO_COMMANDS {
            let allow = format!("allow-{}", command.replace('_', "-"));
            assert!(permissions.iter().any(|item| item.as_str() == Some(allow.as_str())), "{allow}");
            assert!(acl.contains(&format!("commands.allow = [\"{command}\"]")), "{command}");
            assert!(wire.contains(&format!("mongo::commands::{command}")), "{command}");
        }
        assert_eq!(super::mongo::module_id(), "mongo");
    }

    #[test]
    fn main_window_registers_redis_commands() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let capability: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(root.join("capabilities/default.json")).unwrap()).unwrap();
        let permissions = capability["permissions"].as_array().unwrap();
        let acl = std::fs::read_to_string(root.join("permissions/app-commands.toml")).unwrap();
        let registration = std::fs::read_to_string(root.join("src/lib.rs")).unwrap();
        let wire = registration.split("fn wire").nth(1).unwrap_or("");
        let wire = wire.split("pub fn run").next().unwrap_or("");
        for command in REDIS_COMMANDS {
            let allow = format!("allow-{}", command.replace('_', "-"));
            assert!(permissions.iter().any(|item| item.as_str() == Some(allow.as_str())), "{allow}");
            assert!(acl.contains(&format!("commands.allow = [\"{command}\"]")), "{command}");
            assert!(wire.contains(&format!("redis::commands::{command}")), "{command}");
        }
        assert_eq!(super::redis::module_id(), "redis");
    }

    #[test]
    fn main_window_registers_connection_commands() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let capability: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(root.join("capabilities/default.json")).unwrap()).unwrap();
        let permissions = capability["permissions"].as_array().unwrap();
        assert_eq!(capability["windows"][0], "main");
        let acl = std::fs::read_to_string(root.join("permissions/app-commands.toml")).unwrap();
        let registration = std::fs::read_to_string(root.join("src/lib.rs")).unwrap();
        let wire = registration.split("fn wire").nth(1).unwrap_or("");
        let wire = wire.split("pub fn run").next().unwrap_or("");
        assert!(wire.contains(".manage(state)"));
        assert!(registration.contains("wire(tauri::Builder::default(), connections::commands::ConnectionsState::new())"));
        for command in CONNECTION_COMMANDS {
            let allow = format!("allow-{}", command.replace('_', "-"));
            assert!(permissions.iter().any(|item| item.as_str() == Some(allow.as_str())), "{allow}");
            assert!(acl.contains(&format!("commands.allow = [\"{command}\"]")), "{command}");
            assert!(wire.contains(&format!("connections::commands::{command}")), "{command}");
        }
        let _keep_public = (
            super::connections::commands::connections_list,
            super::connections::commands::connections_save,
            super::connections::commands::connections_delete,
            super::connections::commands::connections_test,
            super::connections::commands::connections_open,
            super::connections::commands::connections_focus_database,
            super::connections::commands::connections_disconnect,
            super::connections::commands::connections_confirm_host_key,
            super::connections::commands::connections_forget_host_key,
            super::connections::commands::connections_export,
            super::connections::commands::connections_import_file,
            super::connections::commands::env_tags_list,
            super::connections::commands::env_tags_save,
            super::connections::commands::env_tags_delete,
            super::connections::commands::ConnectionsState::new,
            super::connections::commands::ConnectionsState::with_path,
        );
        let _ = _keep_public;
    }
}
