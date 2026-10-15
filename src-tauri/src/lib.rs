mod catalog;
mod connections;
mod ddl;
mod grid;
mod native_export;
mod product;
mod query;

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
        connections::commands::connections_list,
        connections::commands::connections_save,
        connections::commands::connections_delete,
        connections::commands::connections_test,
        connections::commands::connections_open,
        connections::commands::connections_focus_database,
        connections::commands::connections_disconnect,
        connections::commands::connections_confirm_host_key,
        connections::commands::connections_forget_host_key,
        connections::commands::env_tags_list,
        connections::commands::env_tags_save,
        connections::commands::env_tags_delete,
        catalog::commands::catalog_sessions,
        catalog::commands::catalog_refresh,
        catalog::commands::catalog_page,
        grid::commands::grid_describe,
        grid::commands::grid_page,
        grid::commands::grid_count,
        grid::commands::grid_commit,
        grid::commands::grid_export,
        grid::commands::grid_export_cancel,
        ddl::commands::ddl_describe,
        ddl::commands::ddl_preview,
        ddl::commands::ddl_execute,
        query::commands::query_start,
        query::commands::query_poll,
        query::commands::query_cancel,
        query::commands::query_history,
        query::commands::query_history_clear,
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
        "env_tags_list",
        "env_tags_save",
        "env_tags_delete",
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
        assert!(super::fixed_dependency_markers() >= 12);
        assert!(!super::local_data_dir().is_empty());
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
            super::connections::commands::env_tags_list,
            super::connections::commands::env_tags_save,
            super::connections::commands::env_tags_delete,
            super::connections::commands::ConnectionsState::new,
            super::connections::commands::ConnectionsState::with_path,
        );
        let _ = _keep_public;
    }
}
