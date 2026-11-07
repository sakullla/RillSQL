/// Save the loaded snapshot without issuing another database query.
pub async fn export_write_loaded(path: String, contents: String) -> Result<(), String> {
    crate::application::files::write_loaded(path, contents).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn saves_exact_csv_and_replaces_existing_file() {
        let dir = std::env::temp_dir().join(format!("rillsql-native-export-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir(&dir).await.unwrap();
        let path = dir.join("客户.csv");
        tokio::fs::write(&path, "old").await.unwrap();
        let csv = "\u{feff}\"id\",\"name\"\r\n\"1\",\"林夏\"\r\n\"2\",\"周衡\"";
        export_write_loaded(path.to_string_lossy().into_owned(), csv.to_owned()).await.unwrap();
        assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), csv);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        let failure = export_write_loaded(dir.to_string_lossy().into_owned(), "new".to_owned()).await;
        assert!(failure.is_err());
        assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), csv);
        tokio::fs::remove_dir_all(dir).await.unwrap();
    }

    #[tokio::test]
    async fn rejects_relative_and_missing_paths() {
        assert!(export_write_loaded("customers.csv".to_owned(), "data".to_owned()).await.is_err());
        let path = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string()).join("out.csv");
        assert!(export_write_loaded(path.to_string_lossy().into_owned(), "data".to_owned()).await.is_err());
        assert!(!path.exists());
    }
}
