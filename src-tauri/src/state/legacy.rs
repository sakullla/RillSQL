//! 只读导入旧 WebView localStorage。不删除、不改写旧文件。
//! Windows 读 WebView2/Chromium leveldb 目录；macOS 读 WKWebView `.localstorage`；
//! Linux 读 WebKitGTK localStorage SQLite。导入失败保留旧文件，也不覆盖已有 `app-state.json`。

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sqlx::Row;

use super::atomic::write_atomic_off_ui;
use super::document::{self, shell_from_legacy, session_from_legacy, AppState, StateError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Windows,
    Macos,
    Linux,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportReport {
    pub imported: bool,
    pub wrote_state: bool,
    pub warning: Option<String>,
}

impl ImportReport {
    fn imported() -> Self {
        Self { imported: true, wrote_state: true, warning: None }
    }

    fn skipped(warning: impl Into<String>) -> Self {
        Self { imported: false, wrote_state: false, warning: Some(warning.into()) }
    }
}

pub async fn import_legacy(state_path: &Path, legacy_root: &Path, platform: Platform) -> ImportReport {
    let state_path_check = state_path.to_path_buf();
    let existing = match tokio::task::spawn_blocking(move || existing_state(&state_path_check)).await {
        Ok(report) => report,
        Err(err) => return ImportReport::skipped(format!("读取已有状态失败：{err} 未覆盖，也未改写旧 localStorage。")),
    };
    if let Some(report) = existing {
        return report;
    }
    let values = match read_legacy_values(legacy_root, platform).await {
        Ok(values) => values,
        Err(err) => return ImportReport::skipped(err),
    };
    let state = match state_from_values(&values) {
        Ok(state) => state,
        Err(err) => return ImportReport::skipped(err),
    };
    let bytes = match document::to_bytes(&state) {
        Ok(bytes) => bytes,
        Err(err) => return ImportReport::skipped(err.to_string()),
    };
    match write_atomic_off_ui(state_path.to_path_buf(), bytes).await {
        Ok(()) => ImportReport::imported(),
        Err(err) => ImportReport::skipped(err),
    }
}

fn existing_state(path: &Path) -> Option<ImportReport> {
    if !path.exists() {
        return None;
    }
    Some(match fs::read(path) {
        Ok(bytes) => match document::parse_stored(&bytes) {
            Ok(_) => ImportReport::skipped("已有 app-state.json，未覆盖，也未改写旧 localStorage。"),
            Err(StateError::UnknownVersion(version)) => {
                ImportReport::skipped(format!("已有未知版本 {version} 的 app-state.json，未覆盖，也未改写旧 localStorage。"))
            }
            Err(_) => ImportReport::skipped("已有无法识别的 app-state.json，未覆盖，也未改写旧 localStorage。"),
        },
        Err(err) => ImportReport::skipped(format!("已有 app-state.json 但读不了（{err}），未覆盖，也未改写旧 localStorage。")),
    })
}

fn state_from_values(values: &BTreeMap<String, String>) -> Result<AppState, String> {
    let session_raw = values.get("rillsql.session").ok_or_else(|| "旧 localStorage 里没有 rillsql.session，没有写入新状态。".to_owned())?;
    let shell_raw = values.get("rillsql.shell").ok_or_else(|| "旧 localStorage 里没有 rillsql.shell，没有写入新状态。".to_owned())?;
    let session_json: Value = serde_json::from_str(session_raw).map_err(|_| "rillsql.session 不是合法 JSON，没有写入新状态。".to_owned())?;
    let shell_json: Value = serde_json::from_str(shell_raw).map_err(|_| "rillsql.shell 不是合法 JSON，没有写入新状态。".to_owned())?;
    let session = session_from_legacy(&session_json)?;
    let shell = shell_from_legacy(&shell_json)?;
    Ok(AppState::new(shell, session))
}

pub async fn read_legacy_values(root: &Path, platform: Platform) -> Result<BTreeMap<String, String>, String> {
    if !root.exists() {
        return Err("找不到旧 WebView 数据目录，没有写入新状态。".to_owned());
    }
    match platform {
        Platform::Windows => {
            let root = root.to_path_buf();
            tokio::task::spawn_blocking(move || read_chromium_tree(&root))
                .await
                .map_err(|err| format!("读取 Chromium localStorage 失败：{err}"))?
        }
        Platform::Macos | Platform::Linux => read_webkit_tree(root, platform).await,
    }
}

fn read_chromium_tree(root: &Path) -> Result<BTreeMap<String, String>, String> {
    let dir = chromium_leveldb_dir(root).ok_or_else(|| "找不到 WebView2/Chromium Local Storage/leveldb，没有写入新状态。".to_owned())?;
    let values = super::leveldb::latest_values(&dir)?;
    if !values.contains_key("rillsql.session") && !values.contains_key("rillsql.shell") {
        return Err("旧 localStorage 里没有 rillsql.session 或 rillsql.shell，没有写入新状态。".to_owned());
    }
    Ok(values)
}

async fn read_webkit_tree(root: &Path, platform: Platform) -> Result<BTreeMap<String, String>, String> {
    let root = root.to_path_buf();
    let files = tokio::task::spawn_blocking(move || webkit_files(&root, platform))
        .await
        .map_err(|err| format!("枚举 WebKit localStorage 失败：{err} 旧文件未改写。"))?;
    if files.is_empty() {
        return Err(match platform {
            Platform::Macos => "找不到 WKWebView/WebKit localStorage，没有写入新状态。".to_owned(),
            _ => "找不到 WebKitGTK localStorage，没有写入新状态。".to_owned(),
        });
    }
    let mut values = BTreeMap::new();
    for path in files {
        let found = read_webkit_items(&path).await.map_err(|err| format!("读取 {} 失败：{err} 旧文件未改写。", path.display()))?;
        values.extend(found);
    }
    if !values.contains_key("rillsql.session") && !values.contains_key("rillsql.shell") {
        return Err("旧 localStorage 里没有 rillsql.session 或 rillsql.shell，没有写入新状态。".to_owned());
    }
    Ok(values)
}

fn chromium_leveldb_dir(root: &Path) -> Option<PathBuf> {
    let candidates = [
        root.join("EBWebView").join("Default").join("Local Storage").join("leveldb"),
        root.join("Default").join("Local Storage").join("leveldb"),
        root.join("Local Storage").join("leveldb"),
    ];
    candidates.into_iter().find(|path| path.is_dir())
}

fn webkit_files(root: &Path, platform: Platform) -> Vec<PathBuf> {
    let mut found = Vec::new();
    walk(root, 8, &mut found);
    found.retain(|path| is_webkit_store(path, platform));
    found.sort_by_key(|path| path.metadata().and_then(|meta| meta.modified()).ok());
    found
}

fn is_webkit_store(path: &Path, platform: Platform) -> bool {
    let text = path.to_string_lossy().replace('\\', "/").to_ascii_lowercase();
    let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("").to_ascii_lowercase();
    match platform {
        Platform::Macos => text.contains("localstorage") || name.ends_with(".localstorage"),
        Platform::Linux => text.contains("localstorage") || name.ends_with(".localstorage") || name.contains("localstorage"),
        Platform::Windows => false,
    }
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth == 0 || !dir.is_dir() {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, depth - 1, out);
        } else if path.is_file() {
            out.push(path);
        }
    }
}

#[allow(dead_code)]
pub fn encode_utf16_le(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(|unit| unit.to_le_bytes()).collect()
}

pub fn decode_storage_blob(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes.len().is_multiple_of(2) {
        let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|pair| u16::from_le_bytes([pair[0], pair[1]])).collect();
        if let Ok(text) = String::from_utf16(&units) {
            let trimmed = text.trim_matches('\u{0}').trim();
            if trimmed.starts_with('{') || trimmed.starts_with('[') {
                return trimmed.to_owned();
            }
        }
    }
    String::from_utf8_lossy(bytes).trim_matches('\u{0}').trim().to_owned()
}

async fn read_webkit_items(path: &Path) -> Result<BTreeMap<String, String>, String> {
    let options = sqlx::sqlite::SqliteConnectOptions::new().filename(path).read_only(true).create_if_missing(false);
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .map_err(|err| err.to_string())?;
    let rows = sqlx::query("SELECT key, CAST(value AS BLOB) FROM ItemTable")
        .fetch_all(&pool)
        .await
        .map_err(|err| err.to_string())?;
    let mut found = BTreeMap::new();
    for row in rows {
        let key: String = row.try_get(0).map_err(|err| err.to_string())?;
        if key != "rillsql.session" && key != "rillsql.shell" {
            continue;
        }
        let blob: Vec<u8> = row.try_get(1).map_err(|err| err.to_string())?;
        found.insert(key, decode_storage_blob(&blob));
    }
    pool.close().await;
    Ok(found)
}
