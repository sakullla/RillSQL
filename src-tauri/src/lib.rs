mod catalog;
mod connections;
mod ddl;
mod grid;
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let _ = fixed_dependency_markers();
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![app_info])
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
}
