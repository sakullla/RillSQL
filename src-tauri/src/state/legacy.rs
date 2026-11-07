//! 只读导入旧 WebView localStorage。不删除、不改写旧文件。
//! Windows 读 WebView2/Chromium leveldb 目录；macOS 读 WKWebView `.localstorage`；
//! Linux 读 WebKitGTK localStorage SQLite。导入失败保留旧文件，也不覆盖已有 `app-state.json`。

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sqlx::Row;

use super::atomic::write_atomic_off_ui;
use super::document::{self, shell_from_legacy, session_from_legacy, AppState, StateError};

const SCAN_LIMIT: u64 = 64 * 1024 * 1024;

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
    if state_path.exists() {
        return match fs::read(state_path) {
            Ok(bytes) => match document::parse_stored(&bytes) {
                Ok(_) => ImportReport::skipped("已有 app-state.json，未覆盖，也未改写旧 localStorage。"),
                Err(StateError::UnknownVersion(version)) => {
                    ImportReport::skipped(format!("已有未知版本 {version} 的 app-state.json，未覆盖，也未改写旧 localStorage。"))
                }
                Err(_) => ImportReport::skipped("已有无法识别的 app-state.json，未覆盖，也未改写旧 localStorage。"),
            },
            Err(err) => ImportReport::skipped(format!("已有 app-state.json 但读不了（{err}），未覆盖，也未改写旧 localStorage。")),
        };
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
    let mut values = BTreeMap::new();
    for path in dir_files(&dir) {
        if values.contains_key("rillsql.session") && values.contains_key("rillsql.shell") {
            break;
        }
        let bytes = read_capped(&path)?;
        merge_found(&mut values, extract_chromium_values(&bytes));
    }
    if !values.contains_key("rillsql.session") && !values.contains_key("rillsql.shell") {
        return Err("旧 localStorage 里没有 rillsql.session 或 rillsql.shell，没有写入新状态。".to_owned());
    }
    Ok(values)
}

async fn read_webkit_tree(root: &Path, platform: Platform) -> Result<BTreeMap<String, String>, String> {
    let files = webkit_files(root, platform);
    if files.is_empty() {
        return Err(match platform {
            Platform::Macos => "找不到 WKWebView/WebKit localStorage，没有写入新状态。".to_owned(),
            _ => "找不到 WebKitGTK localStorage，没有写入新状态。".to_owned(),
        });
    }
    let mut values = BTreeMap::new();
    for path in files {
        if values.contains_key("rillsql.session") && values.contains_key("rillsql.shell") {
            break;
        }
        let found = read_webkit_items(&path).await.map_err(|err| format!("读取 {} 失败：{err} 旧文件未改写。", path.display()))?;
        merge_found(&mut values, found);
    }
    if !values.contains_key("rillsql.session") && !values.contains_key("rillsql.shell") {
        return Err("旧 localStorage 里没有 rillsql.session 或 rillsql.shell，没有写入新状态。".to_owned());
    }
    Ok(values)
}

fn merge_found(values: &mut BTreeMap<String, String>, found: BTreeMap<String, String>) {
    for (key, value) in found {
        values.entry(key).or_insert(value);
    }
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
    found.sort();
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

fn dir_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

fn read_capped(path: &Path) -> Result<Vec<u8>, String> {
    let file = File::open(path).map_err(|err| format!("读不了 {}：{err}", path.display()))?;
    let mut reader = file.take(SCAN_LIMIT);
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).map_err(|err| format!("读不了 {}：{err}", path.display()))?;
    Ok(bytes)
}

/// Chromium/WebView2 leveldb 不是稳定公开格式。这里只从记录里抽出两个已知键的 JSON，
/// 同时接受 UTF-8 与 UTF-16LE。抽不出来就当导入失败，不改旧文件。
pub fn extract_chromium_values(bytes: &[u8]) -> BTreeMap<String, String> {
    let mut found = BTreeMap::new();
    for key in ["rillsql.session", "rillsql.shell"] {
        if let Some(value) = json_after_key(bytes, key) {
            found.insert(key.to_owned(), value);
        }
    }
    found
}

fn json_after_key(bytes: &[u8], key: &str) -> Option<String> {
    let utf8 = key.as_bytes();
    let utf16 = encode_utf16_le(key);
    for (needle, wide) in [(utf8, false), (utf16.as_slice(), true)] {
        let mut offset = 0;
        while let Some(found) = find_at(&bytes[offset..], needle) {
            let after = offset + found + needle.len();
            if let Some(json) = json_near(&bytes[after..], wide) {
                return Some(json);
            }
            offset = after.max(offset + 1);
            if offset >= bytes.len() {
                break;
            }
        }
    }
    None
}

fn json_near(bytes: &[u8], prefer_utf16: bool) -> Option<String> {
    let window = bytes.len().min(128);
    let head = &bytes[..window];
    if prefer_utf16 {
        if let Some(index) = find_at(head, &[b'{', 0]) {
            return take_utf16_json(&bytes[index..]);
        }
    }
    if let Some(index) = head.iter().position(|byte| *byte == b'{') {
        if let Some(slice) = take_json_object(&bytes[index..]) {
            if let Ok(text) = std::str::from_utf8(slice) {
                if serde_json::from_str::<Value>(text).is_ok() {
                    return Some(text.to_owned());
                }
            }
        }
    }
    if let Some(index) = find_at(head, &[b'{', 0]) {
        return take_utf16_json(&bytes[index..]);
    }
    None
}

fn take_json_object(bytes: &[u8]) -> Option<&[u8]> {
    if bytes.first() != Some(&b'{') {
        return None;
    }
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape = false;
    for (index, byte) in bytes.iter().enumerate() {
        if in_string {
            if escape {
                escape = false;
                continue;
            }
            match byte {
                b'\\' => escape = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&bytes[..=index]);
                }
            }
            _ => {}
        }
    }
    None
}

fn take_utf16_json(bytes: &[u8]) -> Option<String> {
    if bytes.len() < 2 || bytes[0] != b'{' || bytes[1] != 0 {
        return None;
    }
    let units: Vec<u16> = bytes.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).collect();
    let text = String::from_utf16_lossy(&units);
    let slice = take_json_str(&text)?;
    serde_json::from_str::<Value>(slice).ok()?;
    Some(slice.to_owned())
}

fn take_json_str(text: &str) -> Option<&str> {
    let slice = take_json_object(text.as_bytes())?;
    std::str::from_utf8(slice).ok()
}

pub fn encode_utf16_le(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(|unit| unit.to_le_bytes()).collect()
}

pub fn decode_storage_blob(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes.len() % 2 == 0 {
        let units: Vec<u16> = bytes.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).collect();
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

fn find_at(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|window| window == needle)
}
