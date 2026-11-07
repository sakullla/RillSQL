//! 已加载导出和拖放预览。文件读写走异步 IO，不在调用方里同步读写整文件。

use std::path::Path;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::dropped::{DroppedPreview, PREVIEW_LIMIT_BYTES};

pub async fn write_loaded(path: String, contents: String) -> Result<(), String> {
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
    }
    .await;
    if let Err(error) = result {
        let _ = tokio::fs::remove_file(&staging).await;
        return Err(format!("无法保存文件：{error}。目标文件未被替换。"));
    }
    Ok(())
}

pub async fn read_dropped_preview(path: String, limit: Option<u64>) -> Result<DroppedPreview, String> {
    let format = detect_format(&path)?;
    let target = Path::new(&path);
    if !target.is_absolute() {
        return Err("拖放的路径不完整，无法读取。".to_owned());
    }
    let file = tokio::fs::File::open(target).await.map_err(|error| format!("无法读取拖放的文件：{error}。"))?;
    let file_size = file.metadata().await.map_err(|error| format!("无法读取拖放的文件信息：{error}。"))?.len();
    let cap = limit.unwrap_or(PREVIEW_LIMIT_BYTES).clamp(1, PREVIEW_LIMIT_BYTES);
    let mut buffer = Vec::with_capacity(cap.min(file_size) as usize);
    let read = file.take(cap).read_to_end(&mut buffer).await.map_err(|error| format!("无法读取拖放的文件：{error}。"))? as u64;
    let truncated = file_size > read;
    if truncated {
        if let Some(cut) = buffer.iter().rposition(|byte| *byte == b'\n') {
            buffer.truncate(cut + 1);
        }
    }
    let text = String::from_utf8_lossy(&buffer).into_owned();
    Ok(DroppedPreview { path, format: format.to_owned(), text, file_size, read_bytes: read, truncated })
}

fn detect_format(path: &str) -> Result<&'static str, String> {
    let extension = Path::new(path).extension().and_then(|raw| raw.to_str()).map(|raw| raw.to_ascii_lowercase());
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
