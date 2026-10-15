use std::path::Path;
use tokio::io::AsyncWriteExt;

/// Save the loaded snapshot without issuing another database query.
#[tauri::command]
pub async fn export_write_loaded(path: String, contents: String) -> Result<(), String> {
    let target = Path::new(&path);
    if !target.is_absolute() || target.file_name().is_none() {
        return Err("请选择完整的文件保存路径".to_owned());
    }
    let staging = target.with_file_name(format!(".rillsql-{}.partial", uuid::Uuid::new_v4()));
    let result = async {
        let mut file = tokio::fs::OpenOptions::new().write(true).create_new(true).open(&staging).await?;
        file.write_all(contents.as_bytes()).await?;
        file.sync_all().await?;
        drop(file);
        tokio::fs::rename(&staging, target).await
    }.await;
    if let Err(error) = result {
        let _ = tokio::fs::remove_file(&staging).await;
        return Err(format!("无法保存 CSV：{error}。目标文件未被替换。"));
    }
    Ok(())
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
