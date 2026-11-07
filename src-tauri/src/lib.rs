pub mod app;
mod application;
mod catalog;
mod compare;
mod connections;
mod ddl;
mod diagram;
mod dropped;
mod dump;
mod export;
mod grid;
mod migrate;
mod mongo;
mod native_export;
mod i18n;
mod product;
mod query;
pub mod release;
mod redis;
mod state;
mod ui;

use serde::Serialize;

#[derive(Serialize)]
struct AppInfo {
    title: &'static str,
    name_en: &'static str,
    license: &'static str,
    data_dir: String,
}

fn app_info() -> AppInfo {
    AppInfo {
        title: product::TITLE,
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

fn command_surface() -> usize {
    let linked = (
        std::any::type_name_of_val(&app_info),
        std::any::type_name_of_val(&native_export::export_write_loaded),
        std::any::type_name_of_val(&dropped::read_dropped_preview),
        std::any::type_name_of_val(&connections::commands::connections_list),
        std::any::type_name_of_val(&connections::commands::connections_save),
        std::any::type_name_of_val(&connections::commands::connections_delete),
        std::any::type_name_of_val(&connections::commands::connections_test),
        std::any::type_name_of_val(&connections::commands::connections_open),
        std::any::type_name_of_val(&connections::commands::connections_focus_database),
        std::any::type_name_of_val(&connections::commands::connections_disconnect),
        std::any::type_name_of_val(&connections::commands::connections_confirm_host_key),
        std::any::type_name_of_val(&connections::commands::connections_forget_host_key),
        std::any::type_name_of_val(&connections::commands::connections_export),
        std::any::type_name_of_val(&connections::commands::connections_import_file),
        std::any::type_name_of_val(&connections::commands::env_tags_list),
        std::any::type_name_of_val(&connections::commands::env_tags_save),
        std::any::type_name_of_val(&connections::commands::env_tags_delete),
        std::any::type_name_of_val(&catalog::commands::catalog_sessions),
        std::any::type_name_of_val(&catalog::commands::catalog_refresh),
        std::any::type_name_of_val(&catalog::commands::catalog_page),
        std::any::type_name_of_val(&catalog::commands::catalog_definition),
        std::any::type_name_of_val(&compare::commands::compare_structure),
        std::any::type_name_of_val(&compare::commands::compare_data),
        std::any::type_name_of_val(&compare::commands::compare_cancel),
        std::any::type_name_of_val(&compare::commands::compare_schemas),
        std::any::type_name_of_val(&grid::commands::grid_describe),
        std::any::type_name_of_val(&grid::commands::grid_page),
        std::any::type_name_of_val(&grid::commands::grid_count),
        std::any::type_name_of_val(&grid::commands::grid_commit),
        std::any::type_name_of_val(&grid::commands::grid_export),
        std::any::type_name_of_val(&grid::commands::grid_export_cancel),
        std::any::type_name_of_val(&dump::commands::database_dump),
        std::any::type_name_of_val(&dump::commands::database_dump_cancel),
        std::any::type_name_of_val(&migrate::commands::migrate_data),
        std::any::type_name_of_val(&migrate::commands::migrate_cancel),
        std::any::type_name_of_val(&redis::commands::redis_scan_keys),
        std::any::type_name_of_val(&redis::commands::redis_read_key),
        std::any::type_name_of_val(&redis::commands::redis_set_ttl),
        std::any::type_name_of_val(&redis::commands::redis_delete_keys),
        std::any::type_name_of_val(&redis::commands::redis_rename_key),
        std::any::type_name_of_val(&redis::commands::redis_exec),
        std::any::type_name_of_val(&mongo::commands::mongo_list_databases),
        std::any::type_name_of_val(&mongo::commands::mongo_list_collections),
        std::any::type_name_of_val(&mongo::commands::mongo_find_documents),
        std::any::type_name_of_val(&mongo::commands::mongo_read_document),
        std::any::type_name_of_val(&mongo::commands::mongo_replace_document),
        std::any::type_name_of_val(&mongo::commands::mongo_insert_document),
        std::any::type_name_of_val(&mongo::commands::mongo_delete_document),
        std::any::type_name_of_val(&ddl::commands::ddl_describe),
        std::any::type_name_of_val(&ddl::commands::ddl_preview),
        std::any::type_name_of_val(&ddl::commands::ddl_execute),
        std::any::type_name_of_val(&diagram::commands::diagram_read),
        std::any::type_name_of_val(&query::commands::query_start),
        std::any::type_name_of_val(&query::commands::query_poll),
        std::any::type_name_of_val(&query::commands::query_release),
        std::any::type_name_of_val(&query::commands::query_cancel),
        std::any::type_name_of_val(&query::commands::query_history),
        std::any::type_name_of_val(&query::commands::query_history_clear),
        std::any::type_name_of_val(&query::commands::query_history_delete),
        std::any::type_name_of_val(&query::commands::query_saved_list),
        std::any::type_name_of_val(&query::commands::query_saved_save),
        std::any::type_name_of_val(&query::commands::query_saved_rename),
        std::any::type_name_of_val(&query::commands::query_saved_delete),
    );
    std::mem::size_of_val(&linked)
}

pub fn run() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let _ = fixed_dependency_markers();
    let _ = command_surface();
    app::run();
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

    #[test]
    fn title_names_the_product() {
        assert_eq!(product::TITLE, "RillSQL");
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

    fn source(relative: &str) -> String {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative);
        std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
    }

    fn tauri_ipc_manifests_are_absent() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        for relative in [
            "capabilities/default.json",
            "permissions/app-commands.toml",
            "gen/schemas/acl-manifests.json",
            "gen/schemas/capabilities.json",
            "gen/schemas/desktop-schema.json",
            "gen/schemas/windows-schema.json",
        ] {
            assert!(!root.join(relative).exists(), "{relative}");
        }
    }

    #[test]
    fn gpui_export_calls_dump_and_migrate() {
        tauri_ipc_manifests_are_absent();
        let view = source("src/app/views/export/mod.rs");
        let desk = source("src/export/desk.rs");
        assert!(view.contains("watch_dump("));
        assert!(view.contains("watch_migrate("));
        assert!(desk.contains("dump_database("));
        assert!(desk.contains("migrate_data("));
    }

    #[test]
    fn gpui_drop_reads_a_preview_without_ipc_manifests() {
        tauri_ipc_manifests_are_absent();
        assert!(source("src/export/desk.rs").contains("read_dropped_preview("));
        assert!(source("src/app/shell/view.rs").contains("accept_drop_path("));
    }

    #[test]
    fn gpui_mongo_calls_document_commands() {
        tauri_ipc_manifests_are_absent();
        let view = source("src/app/views/mongo/mod.rs");
        for name in ["list_databases", "list_collections", "find_documents", "replace_document", "insert_document", "delete_document"] {
            assert!(view.contains(&format!("application::mongo::{name}(")), "{name}");
        }
        assert_eq!(super::mongo::module_id(), "mongo");
    }

    #[test]
    fn gpui_redis_calls_key_commands() {
        tauri_ipc_manifests_are_absent();
        let view = source("src/app/views/redis/mod.rs");
        for name in ["scan_keys", "read_key", "set_ttl", "delete_keys", "rename_key", "exec"] {
            assert!(view.contains(&format!("application::redis::{name}(")), "{name}");
        }
        assert_eq!(super::redis::module_id(), "redis");
    }

    #[test]
    fn gpui_connections_call_the_service_and_do_not_start_tauri() {
        tauri_ipc_manifests_are_absent();
        let view = source("src/app/views/connections/mod.rs");
        for name in ["fn list_rows(", "fn save_draft(", "fn delete_row(", "fn test_row(", "fn open_row(", "fn disconnect_row("] {
            assert!(view.contains(name), "{name}");
        }
        let registration = source("src/lib.rs");
        assert!(registration.contains("app::run()"));
        let tauri_path = format!("{}{}", "tauri", "::");
        assert!(!registration.contains(&tauri_path));
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
