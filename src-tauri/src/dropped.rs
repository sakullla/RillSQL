use serde::Serialize;
use std::path::Path;
use tokio::io::AsyncReadExt;

/// 拖放预览一次读取的硬上限：与前端载入 SQL 文件的 2 MB 同一档（src/query/file-io.ts 先例）。
pub const PREVIEW_LIMIT_BYTES: u64 = 2 * 1024 * 1024;

/// read_dropped_preview 的返回：text 是按行边界截断后的文本；truncated 表示文件还有未读内容。
#[derive(Debug, Serialize)]
pub struct DroppedPreview {
    pub path: String,
    pub format: String,
    pub text: String,
    pub file_size: u64,
    pub read_bytes: u64,
    pub truncated: bool,
}

/// 只接受 CSV 与 JSON；其余扩展名给出含格式名的明确拒绝（R9：XLSX/Parquet 不支持拖放）。
fn detect_format(path: &str) -> Result<&'static str, String> {
    let extension = Path::new(path)
        .extension()
        .and_then(|raw| raw.to_str())
        .map(|raw| raw.to_ascii_lowercase());
    match extension.as_deref() {
        Some("csv") => Ok("csv"),
        Some("json") => Ok("json"),
        Some(known) => Err(format!(
            "不支持拖放 .{known} 文件：拖放预览只支持 .csv 与 .json。{}",
            if known == "xlsx" || known == "xls" { "Excel 文件仍可以从数据网格的导入对话框进入。" } else { "" }
        )),
        None => Err("拖放的文件没有扩展名，识别不出格式：拖放预览只支持 .csv 与 .json。".to_owned()),
    }
}

/// 读取拖放文件的前段文本用于预览：只读、限量（调用方上限与 2 MB 硬上限取小）、超限按行截断并标记。
#[tauri::command]
pub async fn read_dropped_preview(path: String, limit: Option<u64>) -> Result<DroppedPreview, String> {
    let format = detect_format(&path)?;
    let target = Path::new(&path);
    if !target.is_absolute() {
        return Err("拖放的路径不完整，无法读取。".to_owned());
    }
    let file = tokio::fs::File::open(target)
        .await
        .map_err(|error| format!("无法读取拖放的文件：{error}。"))?;
    let file_size = file
        .metadata()
        .await
        .map_err(|error| format!("无法读取拖放的文件信息：{error}。"))?
        .len();
    let cap = limit.unwrap_or(PREVIEW_LIMIT_BYTES).clamp(1, PREVIEW_LIMIT_BYTES);
    let mut buffer = Vec::with_capacity(cap.min(file_size) as usize);
    let read = file
        .take(cap)
        .read_to_end(&mut buffer)
        .await
        .map_err(|error| format!("无法读取拖放的文件：{error}。"))? as u64;
    let truncated = file_size > read;
    if truncated {
        // 丢弃被截断的最后半行，预览只交出完整行（CSV 按行解析；JSON 截断只能靠前端解析失败提示）。
        if let Some(cut) = buffer.iter().rposition(|byte| *byte == b'\n') {
            buffer.truncate(cut + 1);
        }
    }
    let text = String::from_utf8_lossy(&buffer).into_owned();
    Ok(DroppedPreview {
        path,
        format: format.to_owned(),
        text,
        file_size,
        read_bytes: read,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> String {
        std::env::temp_dir()
            .join(format!("rillsql-dropped-{}-{name}", uuid::Uuid::new_v4()))
            .to_string_lossy()
            .into_owned()
    }

    #[tokio::test]
    async fn rejects_unsupported_extensions_with_the_format_named() {
        for name in ["orders.xlsx", "events.parquet", "notes.txt"] {
            let rejected = read_dropped_preview(temp_path(name), None).await;
            let message = rejected.unwrap_err();
            let extension = name.rsplit('.').next().unwrap();
            assert!(message.contains(&format!(".{extension}")), "{name}: {message}");
            assert!(message.contains("不支持"), "{name}: {message}");
        }
        let xlsx = read_dropped_preview(temp_path("orders.xlsx"), None).await.unwrap_err();
        assert!(xlsx.contains("导入对话框"), "{xlsx}");
        let parquet = read_dropped_preview(temp_path("events.parquet"), None).await.unwrap_err();
        assert!(!parquet.contains("导入对话框"), "{parquet}");
        let bare = read_dropped_preview(temp_path("no-extension"), None).await.unwrap_err();
        assert!(bare.contains("扩展名"), "{bare}");
    }

    #[tokio::test]
    async fn reads_the_whole_file_when_under_the_limit() {
        let path = temp_path("rows.json");
        std::fs::write(&path, "[{\"name\":\"甲\"}]").unwrap();
        let preview = read_dropped_preview(path.clone(), None).await.unwrap();
        assert_eq!(preview.format, "json");
        assert_eq!(preview.text, "[{\"name\":\"甲\"}]");
        assert_eq!(preview.file_size, 16);
        assert_eq!(preview.read_bytes, 16);
        assert!(!preview.truncated);
        std::fs::remove_file(&path).unwrap();
    }

    #[tokio::test]
    async fn truncates_on_a_line_boundary_and_reports_the_read_size() {
        // "id,name\n" 8 字节 + "1,甲\n" 6 字节（甲 3 字节）+ "2,乙\n" 6 字节。
        let path = temp_path("rows.csv");
        std::fs::write(&path, "id,name\n1,甲\n2,乙\n").unwrap();
        let preview = read_dropped_preview(path.clone(), Some(12)).await.unwrap();
        assert_eq!(preview.format, "csv");
        assert_eq!(preview.read_bytes, 12);
        assert!(preview.truncated);
        // 12 字节落在“甲”的多字节序列中间：退回到上一个完整行边界。
        assert_eq!(preview.text, "id,name\n");
        std::fs::remove_file(&path).unwrap();
    }

    #[tokio::test]
    async fn caps_caller_limits_at_a_minimum_of_one_byte() {
        let path = temp_path("rows.csv");
        std::fs::write(&path, "id\n1\n").unwrap();
        let preview = read_dropped_preview(path.clone(), Some(0)).await.unwrap();
        assert_eq!(preview.read_bytes, 1);
        assert!(preview.truncated);
        assert_eq!(preview.text, "i");
        std::fs::remove_file(&path).unwrap();
    }

    #[tokio::test]
    async fn reports_missing_and_relative_paths_explicitly() {
        let missing = read_dropped_preview(temp_path("absent.csv"), None).await.unwrap_err();
        assert!(missing.contains("无法读取"), "{missing}");
        let relative = read_dropped_preview("rows.csv".to_owned(), None).await.unwrap_err();
        assert!(relative.contains("路径不完整"), "{relative}");
    }
}
