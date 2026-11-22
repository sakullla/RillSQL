//! 版本化 `app-state.json`。只保存 shell 设置、活动标签、草稿和目标。
//! 不保存密码、SSH passphrase 或未提交的网格变更。

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::ddl::templates::TemplateStore;

/// 2：新增 `columnTemplates`（常用字段模板）。版本 1 的文件读入时迁移为 2（没有模板）。
pub const APP_STATE_VERSION: u32 = 2;
/// 仍能读入并迁移的旧版本。
const MIGRATABLE_VERSIONS: &[u64] = &[1];
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
    /// 空字符串在网格里怎么画（R20）：`hint` 弱化提示（默认），`blank` 完全留空。旧文件没有这一项时取默认。
    #[serde(default = "default_empty_display")]
    pub empty_display: String,
    /// 索引命名模板（R8）。旧文件没有这一项时取默认，与原来的固定命名一致。
    #[serde(default)]
    pub index_naming: crate::ddl::naming::IndexNaming,
    /// 事务默认方式：true 时连接空闲后自动开启手动事务，提交或回滚后继续开启下一个。
    #[serde(default)]
    pub transaction_manual: bool,
    /// 在事务栏显示保存点、隔离级别与事务模式等高级入口。
    #[serde(default)]
    pub transaction_advanced: bool,
}

fn default_empty_display() -> String {
    "hint".to_owned()
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
            object_page: 500,
            null_copy: "NULL".to_owned(),
            query_timeout_sec: 0,
            auto_check_updates: true,
            empty_display: default_empty_display(),
            index_naming: crate::ddl::naming::IndexNaming::default(),
            transaction_manual: false,
            transaction_advanced: false,
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
    /// 固定标签的 id（排在左侧、批量关闭时跳过）。旧会话没有这一项时按都未固定读取；没有固定时不写出。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pinned: Vec<String>,
}


#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppState {
    pub version: u32,
    pub shell: ShellSettings,
    pub session: SessionState,
    /// 用户自定义的常用字段模板与被隐藏的内置模板。
    #[serde(default)]
    pub column_templates: TemplateStore,
}

impl AppState {
    pub fn new(shell: ShellSettings, session: SessionState) -> Self {
        Self { version: APP_STATE_VERSION, shell, session, column_templates: TemplateStore::default() }
    }

    pub fn with_templates(mut self, templates: TemplateStore) -> Self {
        self.column_templates = templates;
        self
    }
}

pub fn parse_stored(bytes: &[u8]) -> Result<AppState, StateError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| StateError::Malformed)?;
    let version = value.get("version").and_then(Value::as_u64).ok_or(StateError::Malformed)?;
    if version != u64::from(APP_STATE_VERSION) && !MIGRATABLE_VERSIONS.contains(&version) {
        return Err(StateError::UnknownVersion(version));
    }
    let mut state: AppState = serde_json::from_value(value).map_err(|_| StateError::Malformed)?;
    // 版本 1 没有模板字段（按空模板读入），升到当前版本；下次写盘即为新版本。
    state.version = APP_STATE_VERSION;
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
    let pinned = object
        .get("pinned")
        .and_then(Value::as_array)
        .map(|ids| ids.iter().filter_map(Value::as_str).filter(|id| tabs.iter().any(|tab| tab.id == *id)).map(str::to_owned).collect())
        .unwrap_or_default();
    Ok(SessionState { tabs, active_id, pinned })
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

#[cfg(test)]
mod pinned_tests {
    use super::*;

    fn tab(id: &str) -> SessionTab {
        SessionTab {
            id: id.into(),
            title: id.into(),
            kind: "query".into(),
            spec: None,
            draft: String::new(),
            connection_id: None,
            connection_name: None,
            database: None,
            engine: None,
            connection_status: None,
            target_text: None,
            target_notice: None,
            path: None,
            last_executed_sql: None,
            table_pane: None,
            structure_section: None,
            definition: None,
            diagram_schema: None,
            redis_key: None,
            doc_database: None,
            doc_collection: None,
            admin_schema: None,
            title_key: None,
        }
    }

    /// 固定标签随会话保存；旧会话没有 pinned 时按都未固定读取，不判为损坏；没有固定时不写出这一项。
    #[test]
    fn pinned_tabs_round_trip_and_old_sessions_still_parse() {
        let session = SessionState { tabs: vec![tab("a"), tab("b")], active_id: Some("a".into()), pinned: vec!["b".into()] };
        let state = AppState::new(ShellSettings::default(), session);
        let bytes = to_bytes(&state).unwrap();
        let parsed = parse_stored(&bytes).unwrap();
        assert_eq!(parsed.session.pinned, ["b"]);

        let mut value: Value = serde_json::from_slice(&bytes).unwrap();
        value["session"].as_object_mut().unwrap().remove("pinned");
        let old = parse_stored(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(old.session.pinned.is_empty());
        let rewritten = String::from_utf8(to_bytes(&old).unwrap()).unwrap();
        assert!(!rewritten.contains("pinned"), "{rewritten}");
    }

    /// 旧 rillsql.session 迁移：有 pinned 列表时只保留存在的标签，没有时为空。
    #[test]
    fn legacy_sessions_read_pinned_ids_when_present() {
        let legacy = serde_json::json!({
            "tabs": [{"id": "a", "title": "A", "draft": ""}, {"id": "b", "title": "B", "draft": ""}],
            "activeId": "a",
            "pinned": ["b", "gone"],
        });
        assert_eq!(session_from_legacy(&legacy).unwrap().pinned, ["b"]);
        let mut without = legacy.clone();
        without.as_object_mut().unwrap().remove("pinned");
        assert!(session_from_legacy(&without).unwrap().pinned.is_empty());
    }
}
