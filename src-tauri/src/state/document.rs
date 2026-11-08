//! 版本化 `app-state.json`。只保存 shell 设置、活动标签、草稿和目标。
//! 不保存密码、SSH passphrase 或未提交的网格变更。

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const APP_STATE_VERSION: u32 = 1;
pub const APP_STATE_FILE: &str = "app-state.json";

const TAB_KINDS: &[&str] = &["query", "table", "structure", "definition", "diagram", "keys", "docs", "database"];
const CONNECTION_STATUSES: &[&str] = &["connected", "disconnected", "connecting", "error", "lost"];
const DENSITIES: &[&str] = &["compact", "standard", "relaxed"];
const EDITOR_THEMES: &[&str] = &["auto", "contrast", "warm", "cool", "mono"];
const NULL_COPIES: &[&str] = &["NULL", "empty", "N"];
const FORBIDDEN_KEYS: &[&str] = &[
    "password",
    "sshPassword",
    "sshPassphrase",
    "passphrase",
    "pendingChanges",
    "uncommittedChanges",
];

#[derive(Debug)]
pub enum StateError {
    Malformed,
    UnknownVersion(u64),
    Forbidden(String),
}

impl std::fmt::Display for StateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StateError::Malformed => write!(f, "应用状态无法识别，原文件未改动。"),
            StateError::UnknownVersion(version) => write!(f, "不支持的应用状态版本 {version}，原文件未改动。"),
            StateError::Forbidden(key) => write!(f, "应用状态不能保存 {key}。"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellSettings {
    pub theme: String,
    pub density: String,
    pub editor_font: i64,
    pub editor_theme: String,
    pub locale: String,
    pub page_size: i64,
    pub preview_kb: i64,
    pub object_page: i64,
    pub null_copy: String,
    pub query_timeout_sec: i64,
    pub auto_check_updates: bool,
}

impl Default for ShellSettings {
    fn default() -> Self {
        Self {
            theme: "system".to_owned(),
            density: "compact".to_owned(),
            editor_font: 13,
            editor_theme: "auto".to_owned(),
            locale: "zh-CN".to_owned(),
            page_size: 100,
            preview_kb: 1,
            object_page: 100,
            null_copy: "NULL".to_owned(),
            query_timeout_sec: 0,
            auto_check_updates: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DefinitionTarget {
    pub schema: String,
    pub name: String,
    pub kind: String,
    pub signature: String,
    pub relation: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TitleKey {
    pub key: String,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub params: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTab {
    pub id: String,
    pub title: String,
    pub kind: String,
    /// 表 / 结构 / 专用页面的目标规格；旧状态没有该字段时按无目标处理。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<String>,
    pub draft: String,
    pub connection_id: Option<String>,
    pub connection_name: Option<String>,
    pub database: Option<String>,
    pub engine: Option<String>,
    pub connection_status: Option<String>,
    pub target_text: Option<String>,
    pub target_notice: Option<String>,
    pub path: Option<String>,
    pub last_executed_sql: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table_pane: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structure_section: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition: Option<DefinitionTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagram_schema: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redis_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc_database: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc_collection: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admin_schema: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_key: Option<TitleKey>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(Default)]
pub struct SessionState {
    pub tabs: Vec<SessionTab>,
    pub active_id: Option<String>,
}


#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppState {
    pub version: u32,
    pub shell: ShellSettings,
    pub session: SessionState,
}

impl AppState {
    pub fn new(shell: ShellSettings, session: SessionState) -> Self {
        Self { version: APP_STATE_VERSION, shell, session }
    }
}

pub fn parse_stored(bytes: &[u8]) -> Result<AppState, StateError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| StateError::Malformed)?;
    let version = value.get("version").and_then(Value::as_u64).ok_or(StateError::Malformed)?;
    if version != u64::from(APP_STATE_VERSION) {
        return Err(StateError::UnknownVersion(version));
    }
    let state: AppState = serde_json::from_value(value).map_err(|_| StateError::Malformed)?;
    reject_forbidden(&state)?;
    Ok(state)
}

pub fn to_bytes(state: &AppState) -> Result<Vec<u8>, StateError> {
    if state.version != APP_STATE_VERSION {
        return Err(StateError::UnknownVersion(u64::from(state.version)));
    }
    reject_forbidden(state)?;
    let mut bytes = serde_json::to_vec_pretty(state).map_err(|_| StateError::Malformed)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn reject_forbidden(state: &AppState) -> Result<(), StateError> {
    let value = serde_json::to_value(state).map_err(|_| StateError::Malformed)?;
    let mut found = Vec::new();
    walk_keys(&value, &mut found);
    if let Some(key) = found.into_iter().next() {
        return Err(StateError::Forbidden(key));
    }
    Ok(())
}

fn walk_keys(value: &Value, found: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if FORBIDDEN_KEYS.contains(&key.as_str()) {
                    found.push(key.clone());
                }
                walk_keys(child, found);
            }
        }
        Value::Array(items) => {
            for item in items {
                walk_keys(item, found);
            }
        }
        _ => {}
    }
}

/// 旧 `rillsql.shell`。未知密度和配色按现有前端规则归一；密码字段直接丢掉。
pub fn shell_from_legacy(value: &Value) -> Result<ShellSettings, String> {
    let object = value.as_object().ok_or_else(|| "rillsql.shell 不是 JSON 对象，没有写入新状态。".to_owned())?;
    let mut settings = ShellSettings::default();
    if let Some(theme) = optional_string(object, "theme")? {
        settings.theme = theme;
    }
    if let Some(density) = object.get("density") {
        settings.density = normalize_density(density);
    }
    if let Some(font) = object.get("editorFont") {
        settings.editor_font = finite_i64(font).unwrap_or(settings.editor_font);
    }
    if let Some(theme) = object.get("editorTheme") {
        settings.editor_theme = normalize_editor_theme(theme);
    }
    if let Some(locale) = object.get("locale") {
        settings.locale = normalize_locale(locale);
    }
    if let Some(page) = object.get("pageSize").and_then(finite_i64) {
        settings.page_size = page;
    }
    if let Some(preview) = object.get("previewKB").and_then(finite_i64) {
        settings.preview_kb = preview;
    }
    if let Some(page) = object.get("objectPage").and_then(finite_i64) {
        settings.object_page = page;
    }
    if let Some(copy) = optional_string(object, "nullCopy")? {
        if NULL_COPIES.contains(&copy.as_str()) {
            settings.null_copy = copy;
        }
    }
    if let Some(timeout) = object.get("queryTimeoutSec").and_then(finite_i64) {
        settings.query_timeout_sec = timeout;
    }
    if let Some(auto) = object.get("autoCheckUpdates").and_then(Value::as_bool) {
        settings.auto_check_updates = auto;
    }
    Ok(settings)
}

/// 旧 `rillsql.session`。形状不合法时整体拒绝，不写成空会话。
pub fn session_from_legacy(value: &Value) -> Result<SessionState, String> {
    let object = value.as_object().ok_or_else(|| "rillsql.session 不是 JSON 对象，没有写入新状态。".to_owned())?;
    let tabs_value = object.get("tabs").and_then(Value::as_array).ok_or_else(|| "rillsql.session 缺少标签列表，没有写入新状态。".to_owned())?;
    let active = match object.get("activeId") {
        None | Some(Value::Null) => None,
        Some(Value::String(id)) => Some(id.clone()),
        Some(_) => return Err("rillsql.session 的活动标签无效，没有写入新状态。".to_owned()),
    };
    let mut tabs = Vec::with_capacity(tabs_value.len());
    for item in tabs_value {
        tabs.push(tab_from_legacy(item)?);
    }
    let active_id = if tabs.iter().any(|tab| Some(tab.id.as_str()) == active.as_deref()) {
        active
    } else {
        tabs.last().map(|tab| tab.id.clone())
    };
    Ok(SessionState { tabs, active_id })
}

fn tab_from_legacy(value: &Value) -> Result<SessionTab, String> {
    let tab = value.as_object().ok_or_else(|| "标签不是对象，没有写入新状态。".to_owned())?;
    let id = required_string(tab, "id")?;
    if id.is_empty() {
        return Err("标签缺少 id，没有写入新状态。".to_owned());
    }
    let title = required_string(tab, "title")?;
    let draft = required_string(tab, "draft")?;
    let kind = match tab.get("kind") {
        None => "query".to_owned(),
        Some(Value::String(kind)) if TAB_KINDS.contains(&kind.as_str()) => kind.clone(),
        _ => return Err(format!("标签 {id} 的类型无效，没有写入新状态。")),
    };
    let connection_status = match tab.get("connectionStatus") {
        None | Some(Value::Null) => None,
        Some(Value::String(status)) if CONNECTION_STATUSES.contains(&status.as_str()) => Some(status.clone()),
        _ => return Err(format!("标签 {id} 的连接状态无效，没有写入新状态。")),
    };
    Ok(SessionTab {
        id,
        title,
        kind,
        spec: optional_text(tab, "spec")?,
        draft,
        connection_id: optional_text(tab, "connectionId")?,
        connection_name: optional_text(tab, "connectionName")?,
        database: optional_text(tab, "database")?,
        engine: optional_text(tab, "engine")?,
        connection_status,
        target_text: optional_text(tab, "targetText")?,
        target_notice: optional_text(tab, "targetNotice")?,
        path: optional_text(tab, "path")?,
        last_executed_sql: optional_text(tab, "lastExecutedSql")?,
        table_pane: optional_enum(tab, "tablePane", &["data", "structure"])?,
        structure_section: optional_enum(tab, "structureSection", &["columns", "indexes", "foreign", "drop"])?,
        definition: definition_from(tab.get("definition"))?,
        diagram_schema: optional_text(tab, "diagramSchema")?,
        redis_key: optional_text(tab, "redisKey")?,
        doc_database: optional_text(tab, "docDatabase")?,
        doc_collection: optional_text(tab, "docCollection")?,
        admin_schema: optional_text(tab, "adminSchema")?,
        title_key: title_key_from(tab.get("titleKey"))?,
    })
}

fn definition_from(value: Option<&Value>) -> Result<Option<DefinitionTarget>, String> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(item)) => {
            Ok(Some(DefinitionTarget {
                schema: required_string(item, "schema")?,
                name: required_string(item, "name")?,
                kind: required_string(item, "kind")?,
                signature: required_string(item, "signature")?,
                relation: required_string(item, "relation")?,
            }))
        }
        Some(_) => Err("定义标签的目标无效，没有写入新状态。".to_owned()),
    }
}

fn title_key_from(value: Option<&Value>) -> Result<Option<TitleKey>, String> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(item)) => {
            let key = item.get("key").and_then(Value::as_str).unwrap_or("");
            if key.is_empty() {
                return Ok(None);
            }
            let mut params = Map::new();
            if let Some(raw) = item.get("params") {
                let object = raw.as_object().ok_or_else(|| "标签标题参数无效，没有写入新状态。".to_owned())?;
                for (name, param) in object {
                    if !matches!(param, Value::String(_) | Value::Number(_)) {
                        return Err("标签标题参数无效，没有写入新状态。".to_owned());
                    }
                    params.insert(name.clone(), param.clone());
                }
            }
            Ok(Some(TitleKey { key: key.to_owned(), params }))
        }
        Some(_) => Ok(None),
    }
}

fn normalize_density(value: &Value) -> String {
    match value.as_str() {
        Some("comfortable") => "standard".to_owned(),
        Some(density) if DENSITIES.contains(&density) => density.to_owned(),
        _ => ShellSettings::default().density,
    }
}

fn normalize_editor_theme(value: &Value) -> String {
    match value.as_str() {
        Some(theme) if EDITOR_THEMES.contains(&theme) => theme.to_owned(),
        _ => ShellSettings::default().editor_theme,
    }
}

fn normalize_locale(value: &Value) -> String {
    match value.as_str() {
        Some("en") => "en".to_owned(),
        Some("zh-CN") => "zh-CN".to_owned(),
        _ => ShellSettings::default().locale,
    }
}

fn finite_i64(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| value.as_u64().and_then(|item| i64::try_from(item).ok())).or_else(|| {
        value.as_f64().and_then(|item| if item.is_finite() && item.fract() == 0.0 { i64::try_from(item as i128).ok() } else { None })
    })
}

fn required_string(object: &Map<String, Value>, key: &str) -> Result<String, String> {
    object.get(key).and_then(Value::as_str).map(str::to_owned).ok_or_else(|| format!("缺少 {key}，没有写入新状态。"))
}

fn optional_string(object: &Map<String, Value>, key: &str) -> Result<Option<String>, String> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(format!("{key} 不是文本，没有写入新状态。")),
    }
}

fn optional_text(object: &Map<String, Value>, key: &str) -> Result<Option<String>, String> {
    if !object.contains_key(key) {
        return Ok(None);
    }
    optional_string(object, key)
}

fn optional_enum(object: &Map<String, Value>, key: &str, allowed: &[&str]) -> Result<Option<String>, String> {
    match object.get(key) {
        None => Ok(None),
        Some(Value::String(value)) if allowed.contains(&value.as_str()) => Ok(Some(value.clone())),
        Some(_) => Err(format!("标签字段 {key} 无效，没有写入新状态。")),
    }
}
