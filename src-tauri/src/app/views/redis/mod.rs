//! Redis 键浏览器。浏览只走 SCAN；支持命名空间树、DB0–DB15 实时切换、结构化格式化查看（JSON/XML/Hex）、
//! 右键上下文菜单、一键复制、命令与键名自动补全，以及写删前二次确认。

use std::collections::BTreeSet;
use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::input::InputState;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use crate::ui::kit::{self, stack_frame, FieldSpec, Fields, Palette, Radius, Tip as _, TypeScale};
use crate::ui::{ApplyBlock, CommitResult, EditMode, FormatViewer, FormatViewerEvent, TruncationState, ViewerSpec};
use gpui_kit::{
    anchored, deferred, div, px, Anchor, AnyElement, AppContext as _, ClickEvent, ClipboardItem, Context, Entity, FontWeight,
    InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels,
    Point, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, TestSupportExt as _, Window,
};

use crate::application::redis::{DeleteOutcome, ExecOutcome, KeyspaceDb, RenameOutcome, TtlOutcome};
use crate::application::AppServices;
use crate::i18n::{self, Locale};
use crate::redis::complete::{self, RedisCompletion};
use crate::redis::exec::{self, CommandRisk, RespReply};
use crate::redis::keys::{self, RedisInitialValue, RedisKeyDetail, RedisKeyInfo, RedisMutation, RedisValue, ScanPage};

const SIDEBAR_DEFAULT_W: f32 = 304.;
const SIDEBAR_MIN_W: f32 = 220.;
const SIDEBAR_MAX_W: f32 = 560.;

#[allow(dead_code, clippy::enum_variant_names)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Field {
    Pattern,
    Ttl,
    Rename,
    Command,
    NewKey,
    NewTtl,
    NewField,
    NewValue,
    NewScore,
    EntryField,
    EntryValue,
    EntryMember,
    EntryScore,
    EntryIndex,
    StreamId,
    StreamField,
    StreamValue,
    HashQuery,
    SetQuery,
}

#[derive(Clone, Debug, PartialEq)]
enum Confirm {
    Delete { key: String },
    Rename { from: String, to: String },
    Exec { command: String, args: Vec<String>, line: String },
    Ttl { key: String, seconds: Option<i64> },
    Mutation { mutation: RedisMutation, key: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Follow {
    None,
    Read,
}

/// 失败来源。错误块按它落在自己的上下文里，不再把一切错误都塞进底部一行红字。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ErrorSource {
    /// SCAN 读键列表（含未打开连接）。
    Scan,
    /// 读取选中键的内容。
    Detail,
    /// 命令行 / 写删命令。
    Command,
}

/// 键列表的呈现状态：加载 → 错误 → 无命中 → 无数据 → 有行。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ListState {
    Loading,
    Error,
    NoMatch,
    Empty,
    Rows,
}

fn list_state(busy: bool, scan_failed: bool, total: usize, visible: usize, pattern_active: bool, done: bool) -> ListState {
    if busy && total == 0 {
        return ListState::Loading;
    }
    if scan_failed && total == 0 {
        return ListState::Error;
    }
    if total == 0 {
        if !done {
            return ListState::Rows;
        }
        if pattern_active {
            return ListState::NoMatch;
        }
        return ListState::Empty;
    }
    if visible == 0 {
        return ListState::NoMatch;
    }
    ListState::Rows
}

/// 命名空间树与列表的统一渲染行：树模式下文件夹与叶子键按真实层级穿插排列，绝不重复堆叠扁平列表。
#[derive(Clone, Debug, PartialEq, Eq)]
enum KeyRow {
    Folder {
        prefix: String,
        segment: String,
        depth: usize,
        count: usize,
        open: bool,
    },
    Key {
        index: usize,
        display: String,
        depth: usize,
    },
}

#[derive(Clone, Debug)]
struct ContextItem {
    id: String,
    icon: IconName,
    label: String,
    danger: bool,
}

#[derive(Clone, Debug)]
struct ContextMenuState {
    at: Point<Pixels>,
    items: Vec<ContextItem>,
}

pub struct RedisPane {
    services: Arc<AppServices>,
    locale: Locale,
    connection_id: String,
    connection_name: String,
    database: String,
    db_picker_open: bool,
    db_counts: Vec<KeyspaceDb>,
    sidebar_w: f32,
    sidebar_drag_start: Option<(f32, f32)>,
    pattern: String,
    keys: Vec<RedisKeyInfo>,
    cursor: u64,
    done: bool,
    selected: String,
    detail: Option<Arc<RedisKeyDetail>>,
    ttl_input: String,
    ttl_open: bool,
    rename_to: String,
    rename_open: bool,
    command_input: String,
    reply: String,
    pretty_reply: String,
    note: String,
    error: String,
    confirm: Option<Confirm>,
    confirm_prompt: String,
    rename_landed: String,
    busy: bool,
    generation: u64,
    copied: String,
    scan_reset: bool,
    follow: Follow,
    pending_ttl: Option<i64>,
    ttl_was_persist: bool,
    focused: Option<Field>,
    fields: Fields<Field>,
    type_filter: String,
    tree_view: bool,
    nav_choice: Option<bool>,
    viewport_w: f32,
    error_source: ErrorSource,
    expanded: BTreeSet<String>,
    console_open: bool,
    complete_open: bool,
    complete_index: usize,
    history: Vec<String>,
    history_at: Option<usize>,
    history_draft: String,
    new_open: bool,
    new_type: String,
    new_key: String,
    new_ttl: String,
    new_field: String,
    new_value: String,
    new_score: String,
    new_overwrite: bool,
    entry_field: String,
    entry_value: String,
    entry_member: String,
    entry_score: String,
    entry_index: String,
    stream_id: String,
    stream_field: String,
    stream_value: String,
    hash_query: String,
    set_query: String,
    stream_focus: Option<usize>,
    string_viewer: Option<Entity<FormatViewer>>,
    string_sub: Option<Subscription>,
    viewer_key: String,
    reload_viewer: bool,
    /// 集合条目级格式化查看器（Hash / List / Set / ZSet / Stream 选中项自动识别 JSON/XML/Hex）。
    entry_focus: Option<usize>,
    entry_viewer: Option<Entity<FormatViewer>>,
    entry_viewer_tag: String,
    context_menu: Option<ContextMenuState>,
}

impl RedisPane {
    pub fn new(services: Arc<AppServices>) -> Self {
        Self {
            services,
            locale: Locale::ZhCn,
            connection_id: String::new(),
            connection_name: String::new(),
            database: "0".into(),
            db_picker_open: false,
            db_counts: Vec::new(),
            sidebar_w: SIDEBAR_DEFAULT_W,
            sidebar_drag_start: None,
            pattern: String::new(),
            keys: Vec::new(),
            cursor: 0,
            done: false,
            selected: String::new(),
            detail: None,
            ttl_input: String::new(),
            ttl_open: false,
            rename_to: String::new(),
            rename_open: false,
            command_input: String::new(),
            reply: String::new(),
            pretty_reply: String::new(),
            note: String::new(),
            error: String::new(),
            confirm: None,
            confirm_prompt: String::new(),
            rename_landed: String::new(),
            busy: false,
            generation: 0,
            copied: String::new(),
            scan_reset: true,
            follow: Follow::None,
            pending_ttl: None,
            ttl_was_persist: false,
            focused: None,
            fields: Fields::default(),
            type_filter: String::new(),
            tree_view: false,
            nav_choice: None,
            viewport_w: 1200.,
            error_source: ErrorSource::Command,
            expanded: BTreeSet::new(),
            console_open: true,
            complete_open: false,
            complete_index: 0,
            history: Vec::new(),
            history_at: None,
            history_draft: String::new(),
            new_open: false,
            new_type: "string".into(),
            new_key: String::new(),
            new_ttl: String::new(),
            new_field: String::new(),
            new_value: String::new(),
            new_score: String::new(),
            new_overwrite: false,
            entry_field: String::new(),
            entry_value: String::new(),
            entry_member: String::new(),
            entry_score: String::new(),
            entry_index: String::new(),
            stream_id: String::new(),
            stream_field: String::new(),
            stream_value: String::new(),
            hash_query: String::new(),
            set_query: String::new(),
            stream_focus: None,
            string_viewer: None,
            string_sub: None,
            viewer_key: String::new(),
            reload_viewer: false,
            entry_focus: None,
            entry_viewer: None,
            entry_viewer_tag: String::new(),
            context_menu: None,
        }
    }

    pub fn set_locale(&mut self, locale: Locale) {
        self.locale = locale;
    }

    pub fn running(&self) -> bool {
        self.busy
    }

    pub fn set_pattern(&mut self, value: &str) {
        self.pattern = value.to_owned();
    }

    pub fn set_ttl(&mut self, value: &str) {
        self.ttl_input = value.to_owned();
        self.ttl_open = true;
    }

    pub fn set_rename(&mut self, value: &str) {
        self.rename_to = value.to_owned();
        self.rename_open = true;
    }

    pub fn set_command(&mut self, value: &str) {
        self.command_input = value.to_owned();
    }

    pub fn pattern(&self) -> &str {
        &self.pattern
    }

    pub fn command_input(&self) -> &str {
        &self.command_input
    }

    pub fn ttl_input(&self) -> &str {
        &self.ttl_input
    }

    pub fn rename_to(&self) -> &str {
        &self.rename_to
    }

    pub fn selected(&self) -> &str {
        &self.selected
    }

    pub fn database(&self) -> &str {
        &self.database
    }

    pub fn keys(&self) -> &[RedisKeyInfo] {
        &self.keys
    }

    pub fn error(&self) -> &str {
        &self.error
    }

    pub fn note(&self) -> &str {
        &self.note
    }

    pub fn reply(&self) -> &str {
        &self.reply
    }

    pub fn scan_done(&self) -> bool {
        self.done
    }

    pub fn detail_ttl(&self) -> Option<i64> {
        self.detail.as_ref().and_then(|detail| detail.ttl_ms)
    }

    #[cfg(test)]
    pub fn plant_key(&mut self, name: &str) {
        self.connection_id = "missing".into();
        self.connection_name = "缓存".into();
        self.database = "0".into();
        self.keys = vec![RedisKeyInfo { name: name.into(), key_type: "string".into(), ttl_ms: Some(-1) }];
        self.selected = name.into();
        self.detail = Some(Arc::new(RedisKeyDetail {
            key_type: "string".into(),
            ttl_ms: Some(-1),
            value: RedisValue::String { text: "hello".into() },
        }));
    }

    pub fn copied(&self) -> &str {
        &self.copied
    }

    pub fn confirm_open(&self) -> bool {
        self.confirm.is_some()
    }

    pub fn confirm_prompt(&self) -> &str {
        &self.confirm_prompt
    }

    pub fn detail_text(&self) -> String {
        self.detail.as_ref().map(|detail| format!("{} {}", detail.key_type, format_value(&detail.value, self.locale))).unwrap_or_default()
    }

    fn reset_target_state(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.busy = false;
        self.connection_id.clear();
        self.connection_name.clear();
        self.database = "0".to_owned();
        self.db_picker_open = false;
        self.db_counts.clear();
        self.pattern.clear();
        self.keys.clear();
        self.cursor = 0;
        self.done = false;
        self.selected.clear();
        self.detail = None;
        self.ttl_input.clear();
        self.ttl_open = false;
        self.rename_to.clear();
        self.rename_open = false;
        self.command_input.clear();
        self.reply.clear();
        self.pretty_reply.clear();
        self.copied.clear();
        self.note.clear();
        self.error.clear();
        self.confirm = None;
        self.confirm_prompt.clear();
        self.rename_landed.clear();
        self.pending_ttl = None;
        self.ttl_was_persist = false;
        self.scan_reset = true;
        self.follow = Follow::None;
        self.fields = Fields::default();
        self.type_filter.clear();
        self.tree_view = false;
        self.nav_choice = None;
        self.error_source = ErrorSource::Command;
        self.expanded.clear();
        self.console_open = true;
        self.complete_open = false;
        self.complete_index = 0;
        self.history.clear();
        self.history_at = None;
        self.history_draft.clear();
        self.new_open = false;
        self.new_type = "string".into();
        self.new_key.clear();
        self.new_ttl.clear();
        self.new_field.clear();
        self.new_value.clear();
        self.new_score.clear();
        self.new_overwrite = false;
        self.clear_entry_drafts();
        self.hash_query.clear();
        self.set_query.clear();
        self.stream_focus = None;
        self.string_viewer = None;
        self.string_sub = None;
        self.viewer_key.clear();
        self.reload_viewer = false;
        self.entry_focus = None;
        self.entry_viewer = None;
        self.entry_viewer_tag.clear();
        self.context_menu = None;
    }

    fn clear_entry_drafts(&mut self) {
        self.entry_field.clear();
        self.entry_value.clear();
        self.entry_member.clear();
        self.entry_score.clear();
        self.entry_index.clear();
        self.stream_id.clear();
        self.stream_field.clear();
        self.stream_value.clear();
    }

    pub fn open_spec(&mut self, spec: &str, cx: &mut Context<Self>) {
        self.reset_target_state();
        let Some((id, name, database)) = parse_spec(spec) else {
            self.show_error_from(ErrorSource::Scan, self.text("redis.tab.needConnection"));
            cx.notify();
            return;
        };
        self.connection_id = id;
        self.connection_name = name;
        self.database = database.trim_start_matches("db").to_owned();
        if self.database.is_empty() {
            self.database = "0".into();
        }
        self.refresh_keyspace_counts(cx);
        self.scan_from_start(cx);
    }

    fn refresh_keyspace_counts(&mut self, cx: &mut Context<Self>) {
        if self.connection_id.is_empty() {
            return;
        }
        let connections = Arc::clone(&self.services.connections);
        let connection_id = self.connection_id.clone();
        let generation = self.generation;
        let view = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::app::views::grid::runtime_for_views().spawn(async move {
                let _ = tx.send(crate::application::redis::list_keyspaces(connections.as_ref(), &connection_id).await);
            });
            if let Ok(Ok(counts)) = rx.await {
                cx.update(|cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |pane, cx| {
                            if pane.generation == generation {
                                pane.db_counts = counts;
                                cx.notify();
                            }
                        });
                    }
                });
            }
        })
        .detach();
    }

    fn switch_db(&mut self, db: u8, cx: &mut Context<Self>) {
        self.db_picker_open = false;
        self.context_menu = None;
        let target_db = db.to_string();
        if self.connection_id.is_empty() {
            self.database = target_db;
            return;
        }
        self.database = target_db;
        self.keys.clear();
        self.cursor = 0;
        self.done = false;
        self.selected.clear();
        self.detail = None;
        self.entry_focus = None;
        self.entry_viewer = None;
        self.entry_viewer_tag.clear();
        let connections = Arc::clone(&self.services.connections);
        let connection_id = self.connection_id.clone();
        self.spawn(
            cx,
            true,
            async move { crate::application::redis::select_db(connections.as_ref(), &connection_id, db).await },
            Self::finish_switch_db,
        );
    }

    fn finish_switch_db(&mut self, result: Result<Vec<KeyspaceDb>, String>, cx: &mut Context<Self>) {
        match result {
            Ok(counts) => {
                self.db_counts = counts;
                self.scan_from_start(cx);
            }
            Err(message) => self.show_error_from(ErrorSource::Scan, message),
        }
    }

    fn copy_text(&mut self, text: String, note_key: &str, cx: &mut Context<Self>) {
        if text.is_empty() {
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
        self.copied = text;
        self.note = self.text(note_key);
        self.error.clear();
    }

    fn current_completions(&self) -> (usize, usize, Vec<RedisCompletion>) {
        if !self.console_open || !self.complete_open || self.command_input.trim().is_empty() {
            return (0, 0, Vec::new());
        }
        let known_keys: Vec<String> = self.keys.iter().map(|k| k.name.clone()).collect();
        let (start, end, items) =
            complete::complete_command_line(&self.command_input, self.command_input.len(), &known_keys, self.locale);
        // 若唯一候选与当前输入完全一致，则无需再弹出补全浮层。
        if items.len() == 1 && self.command_input[start..end].eq_ignore_ascii_case(&items[0].insert) {
            return (start, end, Vec::new());
        }
        (start, end, items)
    }

    fn accept_completion(&mut self, index: usize) -> bool {
        let (start, end, items) = self.current_completions();
        let Some(chosen) = items.get(index) else {
            return false;
        };
        let mut next = String::with_capacity(self.command_input.len() + chosen.insert.len() + 1);
        next.push_str(&self.command_input[..start]);
        next.push_str(&chosen.insert);
        let rest = &self.command_input[end..];
        if rest.is_empty() {
            next.push(' ');
        } else {
            next.push_str(rest);
        }
        self.command_input = next;
        self.complete_index = 0;
        // 接受命令名后保留补全开关，方便紧接着补全键名。
        self.complete_open = chosen.kind == "command";
        true
    }

    pub fn command(&mut self, id: &str, cx: &mut Context<Self>) {
        self.context_menu = None;
        let local = matches!(
            id,
            "redis-cancel"
                | "redis-console"
                | "redis-console-clear"
                | "redis-console-copy"
                | "redis-view-flat"
                | "redis-view-tree"
                | "redis-new-key"
                | "redis-new-key-value"
                | "redis-entry-filter"
                | "redis-nav-toggle"
                | "redis-error-dismiss"
                | "redis-db-toggle"
                | "redis-ttl-toggle"
                | "redis-ttl-close"
                | "redis-rename-toggle"
                | "redis-rename-close"
                | "redis-copy-key"
                | "redis-use-scan"
        ) || id.starts_with("redis-type-")
            || id.starts_with("redis-history-")
            || id.starts_with("redis-folder:")
            || id.starts_with("redis-folder-filter:")
            || id.starts_with("redis-folder-copy:")
            || id.starts_with("redis-folder-new:")
            || id.starts_with("redis-key-copy-name:")
            || id.starts_with("redis-key-fill-cmd:")
            || id.starts_with("redis-hash-copy:")
            || id.starts_with("redis-hash-copy-field:")
            || id.starts_with("redis-list-copy:")
            || id.starts_with("redis-set-copy:")
            || id.starts_with("redis-zset-copy:")
            || id.starts_with("redis-stream-copy:")
            || id.starts_with("redis-stream-copy-id:")
            || id.starts_with("redis-entry-inspect:")
            || id.starts_with("redis-complete-pick:")
            || id.starts_with("redis-xopen:");

        if self.busy && !local {
            self.show_error_from(ErrorSource::Command, self.text("redis.tab.busy"));
            cx.notify();
            return;
        }

        if id == "redis-filter" || id == "redis-refresh" || id == "redis-retry-scan" {
            self.scan_from_start(cx);
        } else if id == "redis-use-scan" {
            let (_, args) = split_commandline(&self.command_input);
            let pat = args.first().map(String::as_str).unwrap_or("*");
            self.pattern = if pat == "*" { String::new() } else { pat.to_owned() };
            self.error.clear();
            self.scan_from_start(cx);
        } else if id == "redis-db-toggle" {
            self.db_picker_open = !self.db_picker_open;
            if self.db_picker_open {
                self.refresh_keyspace_counts(cx);
            }
        } else if let Some(db_str) = id.strip_prefix("redis-db-pick:") {
            if let Ok(db) = db_str.parse::<u8>() {
                self.switch_db(db, cx);
            }
        } else if id == "redis-clear-filter" {
            self.type_filter.clear();
            self.pattern.clear();
            self.scan_from_start(cx);
        } else if id == "redis-retry-read" || id == "redis-refresh-key" {
            if !self.selected.is_empty() {
                let name = self.selected.clone();
                self.select_key(name, cx);
            }
        } else if id == "redis-error-dismiss" {
            self.error.clear();
            self.error_source = ErrorSource::Command;
        } else if id == "redis-nav-toggle" {
            self.nav_choice = Some(!self.keys_visible(self.viewport_w));
        } else if id == "redis-more" {
            if !self.done {
                self.scan_reset = false;
                self.follow = Follow::None;
                self.load_keys(cx);
            }
        } else if let Some(index) = id.strip_prefix("redis-select:") {
            if self.confirm.is_none() {
                if let Ok(index) = index.parse::<usize>() {
                    if let Some(key) = self.keys.get(index) {
                        let name = key.name.clone();
                        self.select_key(name, cx);
                    }
                }
            }
        } else if id == "redis-ttl-toggle" {
            self.ttl_open = !self.ttl_open;
            self.rename_open = false;
        } else if id == "redis-ttl-close" {
            self.ttl_open = false;
        } else if id == "redis-ttl" {
            if !self.ttl_open && self.ttl_input.is_empty() && self.detail_ttl() == Some(-1) {
                self.ttl_open = true;
                self.rename_open = false;
            } else {
                self.save_ttl(cx);
            }
        } else if id == "redis-rename-toggle" {
            self.rename_open = !self.rename_open;
            self.ttl_open = false;
            if self.rename_open && self.rename_to.is_empty() {
                self.rename_to = self.selected.clone();
            }
        } else if id == "redis-rename-close" {
            self.rename_open = false;
        } else if id == "redis-rename" {
            if !self.rename_open && self.rename_to.trim().is_empty() {
                self.rename_open = true;
                self.ttl_open = false;
                self.rename_to = self.selected.clone();
            } else {
                self.ask_rename();
            }
        } else if id == "redis-delete" {
            self.ask_delete();
        } else if let Some(index) = id.strip_prefix("redis-key-delete:") {
            if let Ok(index) = index.parse::<usize>() {
                if let Some(key) = self.keys.get(index) {
                    let name = key.name.clone();
                    self.selected = name.clone();
                    self.confirm_prompt = i18n::fill(&self.text("redis.tab.deletePrompt"), &[("key", &name)]);
                    self.confirm = Some(Confirm::Delete { key: name });
                    self.note.clear();
                    self.error.clear();
                }
            }
        } else if let Some(index) = id.strip_prefix("redis-key-copy-name:") {
            if let Ok(index) = index.parse::<usize>() {
                if let Some(key) = self.keys.get(index) {
                    let name = key.name.clone();
                    self.copy_text(name, "redis.copied.keyName", cx);
                }
            }
        } else if let Some(index) = id.strip_prefix("redis-key-fill-cmd:") {
            if let Ok(index) = index.parse::<usize>() {
                if let Some(key) = self.keys.get(index) {
                    let quoted = if key.name.chars().any(char::is_whitespace) {
                        format!("\"{}\"", key.name)
                    } else {
                        key.name.clone()
                    };
                    let cmd_str = match key.key_type.as_str() {
                        "hash" => format!("HGETALL {quoted}"),
                        "list" => format!("LRANGE {quoted} 0 -1"),
                        "set" => format!("SMEMBERS {quoted}"),
                        "zset" => format!("ZRANGE {quoted} 0 -1 WITHSCORES"),
                        "stream" => format!("XRANGE {quoted} - + COUNT 50"),
                        _ => format!("GET {quoted}"),
                    };
                    self.console_open = true;
                    self.command_input = cmd_str;
                    self.complete_open = false;
                }
            }
        } else if id == "redis-copy-key" {
            if !self.selected.is_empty() {
                let key = self.selected.clone();
                self.copy_text(key, "redis.copied.keyName", cx);
            }
        } else if id == "redis-exec" {
            self.complete_open = false;
            self.ask_exec(cx);
        } else if let Some(idx_str) = id.strip_prefix("redis-complete-pick:") {
            if let Ok(idx) = idx_str.parse::<usize>() {
                let _ = self.accept_completion(idx);
            }
        } else if id == "redis-confirm" {
            self.run_confirm(cx);
        } else if id == "redis-cancel" {
            self.confirm = None;
            self.confirm_prompt.clear();
        } else if id == "redis-copy" {
            self.copy_value(cx);
        } else if id == "redis-console" {
            self.console_open = !self.console_open;
        } else if id == "redis-console-clear" {
            self.reply.clear();
            self.pretty_reply.clear();
            self.note.clear();
        } else if id == "redis-console-copy" {
            let out = if !self.pretty_reply.is_empty() { self.pretty_reply.clone() } else { self.reply.clone() };
            self.copy_text(out, "redis.copied.value", cx);
        } else if id == "redis-view-flat" {
            self.tree_view = false;
        } else if id == "redis-view-tree" {
            self.tree_view = true;
            if self.expanded.is_empty() {
                self.expand_top_level();
            }
        } else if let Some(kind) = id.strip_prefix("redis-type-") {
            self.type_filter = if kind == "all" { String::new() } else { kind.to_owned() };
        } else if let Some(prefix) = id.strip_prefix("redis-folder:") {
            if !self.expanded.remove(prefix) {
                self.expanded.insert(prefix.to_owned());
            }
        } else if let Some(prefix) = id.strip_prefix("redis-folder-filter:") {
            self.pattern = format!("{prefix}:*");
            self.scan_from_start(cx);
        } else if let Some(prefix) = id.strip_prefix("redis-folder-copy:") {
            self.copy_text(prefix.to_owned(), "redis.copied.keyName", cx);
        } else if let Some(prefix) = id.strip_prefix("redis-folder-new:") {
            self.nav_choice = Some(true);
            self.new_open = true;
            self.new_key = format!("{prefix}:");
        } else if id == "redis-history-older" {
            self.complete_open = false;
            self.history_step(true);
        } else if id == "redis-history-newer" {
            self.complete_open = false;
            self.history_step(false);
        } else if id == "redis-new-key" || id == "redis-new-key-value" {
            self.nav_choice = Some(true);
            self.new_open = !self.new_open;
        } else if let Some(kind) = id.strip_prefix("redis-new-type-") {
            self.new_type = kind.to_owned();
            self.new_open = true;
        } else if id == "redis-create" {
            self.ask_create();
        } else if id == "redis-overwrite" {
            self.new_overwrite = true;
            self.ask_create();
        } else if id == "redis-string-save" {
            self.ask_string(cx);
        } else if let Some(idx_str) = id.strip_prefix("redis-entry-inspect:") {
            if let Ok(idx) = idx_str.parse::<usize>() {
                self.entry_focus = Some(idx);
            }
        } else if id == "redis-hash-add" {
            self.ask_hash_set();
        } else if let Some(index) = id.strip_prefix("redis-hash-edit:") {
            self.fill_hash(index);
        } else if let Some(index) = id.strip_prefix("redis-hash-copy:") {
            self.copy_hash(index, cx);
        } else if let Some(index) = id.strip_prefix("redis-hash-copy-field:") {
            if let Some(field) = self.hash_field(index) {
                self.copy_text(field, "redis.copied.keyName", cx);
            }
        } else if let Some(index) = id.strip_prefix("redis-hash-del:") {
            self.ask_hash_del(index);
        } else if id == "redis-list-lpush" {
            self.ask_list_push(true);
        } else if id == "redis-list-rpush" {
            self.ask_list_push(false);
        } else if let Some(index) = id.strip_prefix("redis-list-copy:") {
            if let Some(val) = self.list_item(index) {
                self.copy_text(val, "redis.copied.value", cx);
            }
        } else if let Some(index) = id.strip_prefix("redis-list-set:") {
            if self.entry_value.trim().is_empty() {
                if let Some(val) = self.list_item(index) {
                    self.entry_value = val;
                    if let Ok(ix) = index.parse::<usize>() {
                        self.entry_focus = Some(ix);
                    }
                }
            } else {
                self.ask_list_set(index);
            }
        } else if let Some(index) = id.strip_prefix("redis-list-rem:") {
            self.ask_list_rem(index);
        } else if id == "redis-set-add" {
            self.ask_set_add();
        } else if let Some(index) = id.strip_prefix("redis-set-copy:") {
            if let Some(member) = self.set_member(index) {
                self.copy_text(member, "redis.copied.value", cx);
            }
        } else if let Some(index) = id.strip_prefix("redis-set-rem:") {
            self.ask_set_rem(index);
        } else if id == "redis-zadd" {
            self.ask_zadd();
        } else if let Some(index) = id.strip_prefix("redis-zset-copy:") {
            if let Some(member) = self.zset_member(index) {
                self.copy_text(member, "redis.copied.value", cx);
            }
        } else if let Some(index) = id.strip_prefix("redis-zrem:") {
            self.ask_zrem(index);
        } else if id == "redis-xadd" {
            self.ask_xadd();
        } else if let Some(index) = id.strip_prefix("redis-stream-copy:") {
            if let Some(payload) = self.stream_payload_json(index) {
                self.copy_text(payload, "redis.copied.value", cx);
            }
        } else if let Some(index) = id.strip_prefix("redis-stream-copy-id:") {
            if let Some(msg_id) = self.stream_message(index) {
                self.copy_text(msg_id, "redis.copied.keyName", cx);
            }
        } else if let Some(index) = id.strip_prefix("redis-xopen:") {
            self.open_stream(index);
        } else if let Some(index) = id.strip_prefix("redis-xdel:") {
            self.ask_xdel(index);
        }
        cx.notify();
    }

    fn scan_from_start(&mut self, cx: &mut Context<Self>) {
        self.cursor = 0;
        self.done = false;
        self.scan_reset = true;
        self.follow = Follow::None;
        self.note.clear();
        self.load_keys(cx);
    }

    fn load_keys(&mut self, cx: &mut Context<Self>) {
        if self.connection_id.is_empty() {
            self.show_error_from(ErrorSource::Scan, self.text("redis.tab.needConnection"));
            return;
        }
        let connections = Arc::clone(&self.services.connections);
        let connection_id = self.connection_id.clone();
        let cursor = if self.scan_reset { 0 } else { self.cursor };
        let pattern = {
            let trimmed = self.pattern.trim();
            if trimmed.is_empty() { None } else { Some(trimmed.to_owned()) }
        };
        self.spawn(
            cx,
            false,
            async move { crate::application::redis::scan_keys(connections.as_ref(), &connection_id, Some(cursor), pattern, None).await },
            Self::finish_scan,
        );
    }

    fn finish_scan(&mut self, result: Result<ScanPage, String>, cx: &mut Context<Self>) {
        match result {
            Ok(page) => {
                if self.scan_reset {
                    self.keys.clear();
                }
                self.keys = merge_keys(std::mem::take(&mut self.keys), page.keys);
                self.cursor = page.cursor;
                self.done = page.done;
                self.error.clear();
                if self.tree_view && self.expanded.is_empty() {
                    self.expand_top_level();
                }
                if self.follow == Follow::Read && !self.selected.is_empty() {
                    let name = self.selected.clone();
                    self.select_key(name, cx);
                }
            }
            Err(message) => self.show_error_from(ErrorSource::Scan, message),
        }
    }

    fn select_key(&mut self, name: String, cx: &mut Context<Self>) {
        self.selected = name.clone();
        self.detail = None;
        self.stream_focus = None;
        self.entry_focus = None;
        self.entry_viewer_tag.clear();
        self.ttl_open = false;
        self.rename_open = false;
        self.rename_to.clear();
        if self.viewport_w < kit::NARROW_PAGE_WIDTH {
            self.nav_choice = Some(false);
        }
        if self.connection_id.is_empty() {
            self.show_error_from(ErrorSource::Detail, self.text("redis.tab.needConnection"));
            return;
        }
        let connections = Arc::clone(&self.services.connections);
        let connection_id = self.connection_id.clone();
        self.spawn(
            cx,
            false,
            async move { crate::application::redis::read_key(connections.as_ref(), &connection_id, name).await },
            Self::finish_read,
        );
    }

    fn finish_read(&mut self, result: Result<RedisKeyDetail, String>, _: &mut Context<Self>) {
        match result {
            Ok(detail) => {
                self.ttl_input = match detail.ttl_ms {
                    Some(ms) if ms >= 0 => (ms / 1000).to_string(),
                    _ => String::new(),
                };
                self.entry_focus = match &detail.value {
                    RedisValue::Hash { entries } if !entries.is_empty() => Some(0),
                    RedisValue::List { items } if !items.is_empty() => Some(0),
                    RedisValue::Set { items } if !items.is_empty() => Some(0),
                    RedisValue::ZSet { entries } if !entries.is_empty() => Some(0),
                    RedisValue::Stream { messages } if !messages.is_empty() => Some(0),
                    _ => None,
                };
                self.detail = Some(Arc::new(detail));
                self.reload_viewer = true;
                self.entry_viewer_tag.clear();
                self.error.clear();
            }
            Err(message) => self.show_error_from(ErrorSource::Detail, message),
        }
    }

    fn save_ttl(&mut self, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            self.show_error(self.text("redis.tab.needKey"));
            return;
        }
        let seconds = match parse_ttl(&self.ttl_input) {
            Ok(seconds) => seconds,
            Err(()) => {
                self.show_error(self.text("redis.tab.ttlInvalid"));
                return;
            }
        };
        self.pending_ttl = seconds;
        self.ttl_was_persist = seconds.is_none();
        let key = self.selected.clone();
        let preview = match seconds {
            Some(seconds) => format!("EXPIRE {key} {seconds}"),
            None => format!("PERSIST {key}"),
        };
        self.confirm_prompt = i18n::fill(&self.text("redis.confirm.pending"), &[("command", &preview)]);
        self.confirm = Some(Confirm::Ttl { key, seconds });
        self.note.clear();
        self.error.clear();
        let _ = cx;
    }

    fn finish_ttl(&mut self, result: Result<TtlOutcome, String>, cx: &mut Context<Self>) {
        match result {
            Ok(outcome) if outcome.applied == 0 => self.show_error(self.text("redis.tab.ttlMissed")),
            Ok(_) => {
                self.ttl_open = false;
                self.note = if self.ttl_was_persist {
                    self.text("redis.tab.ttlRemoved")
                } else {
                    i18n::fill(&self.text("redis.tab.ttlSet"), &[("n", &self.pending_ttl.unwrap_or(0).to_string())])
                };
                self.error.clear();
                self.scan_reset = true;
                self.cursor = 0;
                self.follow = Follow::Read;
                self.load_keys(cx);
            }
            Err(message) => self.show_error(message),
        }
    }

    fn ask_delete(&mut self) {
        if self.selected.is_empty() {
            self.show_error(self.text("redis.tab.needKey"));
            return;
        }
        let key = self.selected.clone();
        self.confirm_prompt = i18n::fill(&self.text("redis.tab.deletePrompt"), &[("key", &key)]);
        self.confirm = Some(Confirm::Delete { key });
        self.note.clear();
        self.error.clear();
    }

    fn ask_rename(&mut self) {
        if self.selected.is_empty() {
            self.show_error(self.text("redis.tab.needKey"));
            return;
        }
        let from = self.selected.clone();
        let to = self.rename_to.trim().to_owned();
        if to.is_empty() {
            self.show_error(self.text("redis.tab.renameNeedName"));
            return;
        }
        self.confirm_prompt = i18n::fill(&self.text("redis.tab.renamePrompt"), &[("key", &from), ("target", &to)]);
        self.confirm = Some(Confirm::Rename { from, to });
        self.note.clear();
        self.error.clear();
    }

    fn ask_exec(&mut self, cx: &mut Context<Self>) {
        let (command, args) = split_commandline(&self.command_input);
        if command.is_empty() {
            self.show_error(self.text("redis.tab.emptyCommand"));
            return;
        }
        match exec::command_risk(&command) {
            CommandRisk::Blocked => self.show_error(self.text("redis.tab.keysRefused")),
            CommandRisk::Dangerous => {
                let line = self.command_input.clone();
                self.confirm_prompt = danger_prompt(self.locale, &command);
                self.confirm = Some(Confirm::Exec { command, args, line });
                self.note.clear();
                self.error.clear();
            }
            CommandRisk::Readonly => self.exec_command(command, args, false, self.command_input.clone(), cx),
        }
    }

    fn run_confirm(&mut self, cx: &mut Context<Self>) {
        let action = self.confirm.take();
        self.confirm_prompt.clear();
        match action {
            Some(Confirm::Delete { key }) => self.run_delete(key, cx),
            Some(Confirm::Rename { from, to }) => self.run_rename(from, to, cx),
            Some(Confirm::Exec { command, args, line }) => self.exec_command(command, args, true, line, cx),
            Some(Confirm::Ttl { key, seconds }) => self.run_ttl(key, seconds, cx),
            Some(Confirm::Mutation { mutation, key }) => self.run_mutation(mutation, key, cx),
            None => {}
        }
    }

    fn run_ttl(&mut self, key: String, seconds: Option<i64>, cx: &mut Context<Self>) {
        let connections = Arc::clone(&self.services.connections);
        let connection_id = self.connection_id.clone();
        self.spawn(
            cx,
            false,
            async move { crate::application::redis::set_ttl(connections.as_ref(), &connection_id, key, seconds, true).await },
            Self::finish_ttl,
        );
    }

    fn run_mutation(&mut self, mutation: RedisMutation, key: String, cx: &mut Context<Self>) {
        self.selected = key;
        let connections = Arc::clone(&self.services.connections);
        let connection_id = self.connection_id.clone();
        self.spawn(
            cx,
            false,
            async move { crate::application::redis::mutate(connections.as_ref(), &connection_id, mutation, true).await },
            Self::finish_mutate,
        );
    }

    fn finish_mutate(&mut self, result: Result<i64, String>, cx: &mut Context<Self>) {
        match result {
            Ok(_) => {
                self.note = self.text("redis.tab.executed");
                self.error.clear();
                self.new_open = false;
                self.new_overwrite = false;
                self.clear_entry_drafts();
                self.scan_reset = true;
                self.cursor = 0;
                self.follow = Follow::Read;
                self.load_keys(cx);
            }
            Err(message) => self.show_error(message),
        }
    }

    fn stage_mutation(&mut self, mutation: RedisMutation) {
        let preview = keys::mutation_preview(&mutation);
        let key = mutation_key(&mutation);
        self.confirm_prompt = i18n::fill(&self.text("redis.confirm.pending"), &[("command", &preview)]);
        self.confirm = Some(Confirm::Mutation { mutation, key });
        self.note.clear();
        self.error.clear();
    }

    fn run_delete(&mut self, key: String, cx: &mut Context<Self>) {
        if key.is_empty() {
            self.show_error(self.text("redis.tab.needKey"));
            return;
        }
        let connections = Arc::clone(&self.services.connections);
        let connection_id = self.connection_id.clone();
        self.spawn(
            cx,
            false,
            async move { crate::application::redis::delete_keys(connections.as_ref(), &connection_id, vec![key], true).await },
            Self::finish_delete,
        );
    }

    fn finish_delete(&mut self, result: Result<DeleteOutcome, String>, cx: &mut Context<Self>) {
        match result {
            Ok(outcome) => {
                self.note = i18n::fill(&self.text("redis.tab.deletedCount"), &[("n", &outcome.deleted.to_string())]);
                self.error.clear();
                self.selected.clear();
                self.detail = None;
                self.scan_reset = true;
                self.cursor = 0;
                self.follow = Follow::None;
                self.load_keys(cx);
            }
            Err(message) => self.show_error(message),
        }
    }

    fn run_rename(&mut self, from: String, to: String, cx: &mut Context<Self>) {
        if from.is_empty() || to.is_empty() {
            self.show_error(self.text("redis.tab.renameNeedName"));
            return;
        }
        self.rename_landed = to.clone();
        let connections = Arc::clone(&self.services.connections);
        let connection_id = self.connection_id.clone();
        self.spawn(
            cx,
            false,
            async move { crate::application::redis::rename_key(connections.as_ref(), &connection_id, from, to, true).await },
            Self::finish_rename,
        );
    }

    fn finish_rename(&mut self, result: Result<RenameOutcome, String>, cx: &mut Context<Self>) {
        match result {
            Ok(_) => {
                let target = std::mem::take(&mut self.rename_landed);
                self.rename_open = false;
                self.note = i18n::fill(&self.text("redis.tab.renamedNote"), &[("target", &target)]);
                self.error.clear();
                self.selected = target;
                self.rename_to.clear();
                self.detail = None;
                self.scan_reset = true;
                self.cursor = 0;
                self.follow = Follow::Read;
                self.load_keys(cx);
            }
            Err(message) => {
                self.rename_landed.clear();
                self.show_error(message);
            }
        }
    }

    fn exec_command(&mut self, command: String, args: Vec<String>, confirmed: bool, line: String, cx: &mut Context<Self>) {
        if command.is_empty() {
            self.show_error(self.text("redis.tab.emptyCommand"));
            return;
        }
        if let Err(message) = exec::guard_invocation(&command, &args, confirmed) {
            let refused = if exec::command_risk(&exec::invocation_word(&command, &args)) == CommandRisk::Blocked {
                self.text("redis.tab.keysRefused")
            } else {
                message
            };
            self.show_error(refused);
            return;
        }
        if command.eq_ignore_ascii_case("SELECT") {
            if let Some(arg) = args.first() {
                if let Ok(db) = arg.parse::<u8>() {
                    self.database = db.to_string();
                }
            }
        }
        self.remember_command(line);
        let connections = Arc::clone(&self.services.connections);
        let connection_id = self.connection_id.clone();
        self.spawn(
            cx,
            false,
            async move { crate::application::redis::exec(connections.as_ref(), &connection_id, command, args, confirmed).await },
            Self::finish_exec,
        );
    }

    fn finish_exec(&mut self, result: Result<ExecOutcome, String>, cx: &mut Context<Self>) {
        match result {
            Ok(outcome) => {
                self.reply = reply_text(&outcome.reply);
                self.pretty_reply = reply_pretty(&outcome.reply, 0);
                self.note = self.text("redis.tab.executed");
                self.error.clear();
                self.scan_reset = true;
                self.cursor = 0;
                self.follow = if self.selected.is_empty() { Follow::None } else { Follow::Read };
                self.refresh_keyspace_counts(cx);
                self.load_keys(cx);
            }
            Err(message) => self.show_error(message),
        }
    }

    fn copy_value(&mut self, cx: &mut Context<Self>) {
        let raw = self
            .detail
            .as_ref()
            .map(|detail| format_value(&detail.value, self.locale))
            .unwrap_or_default();
        if raw.is_empty() {
            self.show_error(self.text("redis.tab.pickKey"));
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(raw.clone()));
        self.copied = raw;
        self.note = self.text("redis.copied.value");
        self.error.clear();
    }

    fn remember_command(&mut self, line: String) {
        let line = line.trim().to_owned();
        if line.is_empty() {
            return;
        }
        if self.history.last().is_some_and(|item| item == &line) {
            self.history_at = None;
            return;
        }
        self.history.push(line);
        self.history_at = None;
    }

    fn history_step(&mut self, older: bool) {
        if self.confirm.is_some() || self.history.is_empty() {
            return;
        }
        if self.history_at.is_none() {
            self.history_draft = self.command_input.clone();
        }
        if older {
            let next = self.history_at.map(|index| index.saturating_sub(1)).unwrap_or(self.history.len() - 1);
            self.history_at = Some(next);
            self.command_input = self.history[next].clone();
            return;
        }
        match self.history_at {
            Some(index) if index + 1 < self.history.len() => {
                self.history_at = Some(index + 1);
                self.command_input = self.history[index + 1].clone();
            }
            Some(_) => {
                self.history_at = None;
                self.command_input = std::mem::take(&mut self.history_draft);
            }
            None => {}
        }
    }

    /// 构建列表或命名空间树行：列表模式只渲染键；树模式按 `:` 拆分层级并穿插渲染文件夹与叶子键。
    fn build_key_rows(&self) -> Vec<KeyRow> {
        let filtered: Vec<(usize, &RedisKeyInfo)> = self
            .keys
            .iter()
            .enumerate()
            .filter(|(_, key)| self.type_filter.is_empty() || key.key_type == self.type_filter)
            .take(500)
            .collect();

        if !self.tree_view {
            return filtered
                .into_iter()
                .map(|(index, key)| KeyRow::Key { index, display: key.name.clone(), depth: 0 })
                .collect();
        }

        // 统计每个前缀下的键数量并按字典序构建真实层级树。
        let mut sorted = filtered;
        sorted.sort_by(|a, b| a.1.name.cmp(&b.1.name));

        let mut folder_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for (_, key) in &sorted {
            let parts: Vec<&str> = key.name.split(':').collect();
            if parts.len() >= 2 {
                let mut prefix = String::new();
                for part in &parts[..parts.len() - 1] {
                    if !prefix.is_empty() {
                        prefix.push(':');
                    }
                    prefix.push_str(part);
                    *folder_counts.entry(prefix.clone()).or_insert(0) += 1;
                }
            }
        }

        let mut rows = Vec::new();
        let mut emitted_folders = BTreeSet::new();
        for (index, key) in sorted {
            let parts: Vec<&str> = key.name.split(':').collect();
            if parts.len() < 2 {
                rows.push(KeyRow::Key { index, display: key.name.clone(), depth: 0 });
                continue;
            }
            let mut prefix = String::new();
            let mut ancestors_open = true;
            for (depth, &segment) in parts[..parts.len() - 1].iter().enumerate() {
                if !prefix.is_empty() {
                    prefix.push(':');
                }
                prefix.push_str(segment);
                let open = self.expanded.contains(&prefix);
                if ancestors_open && emitted_folders.insert(prefix.clone()) {
                    let count = folder_counts.get(&prefix).copied().unwrap_or(0);
                    rows.push(KeyRow::Folder {
                        prefix: prefix.clone(),
                        segment: segment.to_owned(),
                        depth,
                        count,
                        open,
                    });
                }
                if !open {
                    ancestors_open = false;
                }
            }
            if ancestors_open {
                let leaf = (*parts.last().unwrap_or(&key.name.as_str())).to_owned();
                let display = if leaf.is_empty() { key.name.clone() } else { leaf };
                rows.push(KeyRow::Key {
                    index,
                    display,
                    depth: parts.len() - 1,
                });
            }
        }
        rows
    }

    fn expand_top_level(&mut self) {
        for key in &self.keys {
            if let Some((top, _)) = key.name.split_once(':') {
                if !top.is_empty() {
                    self.expanded.insert(top.to_owned());
                }
            }
        }
    }

    fn ask_create(&mut self) {
        let key = self.new_key.trim().to_owned();
        if key.is_empty() {
            self.show_error(self.text("redis.key.name"));
            return;
        }
        let ttl = match parse_ttl(&self.new_ttl) {
            Ok(ttl) => ttl,
            Err(()) => {
                self.show_error(self.text("redis.tab.ttlInvalid"));
                return;
            }
        };
        let initial = match self.new_type.as_str() {
            "hash" => RedisInitialValue::Hash { field: self.new_field.clone(), value: self.new_value.clone() },
            "list" => RedisInitialValue::List(self.new_value.clone()),
            "set" => RedisInitialValue::Set(self.new_value.clone()),
            "zset" => {
                let score = match self.new_score.trim().parse::<f64>() {
                    Ok(score) if score.is_finite() => score,
                    _ => {
                        self.show_error(self.text("redis.zset.scoreInvalid"));
                        return;
                    }
                };
                RedisInitialValue::ZSet { score, member: self.new_value.clone() }
            }
            "stream" => RedisInitialValue::Stream {
                id: self.stream_id.clone(),
                fields: vec![(self.new_field.clone(), self.new_value.clone())],
            },
            _ => RedisInitialValue::String(self.new_value.clone()),
        };
        let mutation = RedisMutation::CreateKey { key: key.clone(), ttl_secs: ttl, initial, overwrite: self.new_overwrite };
        if let Err(message) = keys::prepare_mutation(&mutation, true) {
            self.show_error(message);
            return;
        }
        self.stage_mutation(mutation);
    }

    fn ask_string(&mut self, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            self.show_error(self.text("redis.tab.needKey"));
            return;
        }
        let Some(viewer) = self.string_viewer.clone() else {
            self.show_error(self.text("redis.tab.pickKey"));
            return;
        };
        let result = viewer.update(cx, |viewer, cx| viewer.commit(cx));
        match result {
            CommitResult::Applied(value) => {
                self.stage_mutation(RedisMutation::SetString { key: self.selected.clone(), value });
            }
            CommitResult::Blocked { block, .. } => {
                let message = match block {
                    ApplyBlock::InvalidJson(error) => i18n::fill(&self.text("mongo.docs.syntaxError"), &[("message", &error.message)]),
                    _ => self.text("redis.value.none"),
                };
                self.show_error(message);
            }
        }
    }

    fn ask_hash_set(&mut self) {
        self.stage_if_key(RedisMutation::HashSet {
            key: self.selected.clone(),
            field: self.entry_field.clone(),
            value: self.entry_value.clone(),
        });
    }

    fn ask_hash_del(&mut self, index: &str) {
        let Some(field) = self.hash_field(index) else { return };
        self.stage_if_key(RedisMutation::HashDel { key: self.selected.clone(), fields: vec![field] });
    }

    fn ask_list_push(&mut self, head: bool) {
        self.stage_if_key(RedisMutation::ListPush { key: self.selected.clone(), head, value: self.entry_value.clone() });
    }

    fn ask_list_set(&mut self, index: &str) {
        let Ok(index) = index.parse::<i64>() else { return };
        self.entry_index = index.to_string();
        self.stage_if_key(RedisMutation::ListSet { key: self.selected.clone(), index, value: self.entry_value.clone() });
    }

    fn ask_list_rem(&mut self, index: &str) {
        let Some(value) = self.list_item(index) else { return };
        self.stage_if_key(RedisMutation::ListRem { key: self.selected.clone(), count: 1, value });
    }

    fn ask_set_add(&mut self) {
        self.stage_if_key(RedisMutation::SetAdd { key: self.selected.clone(), member: self.entry_member.clone() });
    }

    fn ask_set_rem(&mut self, index: &str) {
        let Some(member) = self.set_member(index) else { return };
        self.stage_if_key(RedisMutation::SetRem { key: self.selected.clone(), member });
    }

    fn ask_zadd(&mut self) {
        let score = match self.entry_score.trim().parse::<f64>() {
            Ok(score) if score.is_finite() => score,
            _ => {
                self.show_error(self.text("redis.zset.scoreInvalid"));
                return;
            }
        };
        self.stage_if_key(RedisMutation::ZSetAdd { key: self.selected.clone(), score, member: self.entry_member.clone() });
    }

    fn ask_zrem(&mut self, index: &str) {
        let Some(member) = self.zset_member(index) else { return };
        self.stage_if_key(RedisMutation::ZSetRem { key: self.selected.clone(), member });
    }

    fn ask_xadd(&mut self) {
        self.stage_if_key(RedisMutation::StreamAdd {
            key: self.selected.clone(),
            id: self.stream_id.clone(),
            fields: vec![(self.stream_field.clone(), self.stream_value.clone())],
        });
    }

    fn ask_xdel(&mut self, index: &str) {
        let Some(id) = self.stream_message(index) else { return };
        self.stage_if_key(RedisMutation::StreamDel { key: self.selected.clone(), id });
    }

    fn stage_if_key(&mut self, mutation: RedisMutation) {
        if self.selected.is_empty() {
            self.show_error(self.text("redis.tab.needKey"));
            return;
        }
        if let Err(message) = keys::prepare_mutation(&mutation, true) {
            self.show_error(message);
            return;
        }
        self.stage_mutation(mutation);
    }

    fn copy_hash(&mut self, index: &str, cx: &mut Context<Self>) {
        let Ok(index) = index.parse::<usize>() else { return };
        let Some(RedisKeyDetail { value: RedisValue::Hash { entries }, .. }) = self.detail.as_deref() else { return };
        let Some(entry) = entries.get(index) else { return };
        let text = entry.value.clone();
        self.copy_text(text, "redis.copied.value", cx);
    }

    fn open_stream(&mut self, index: &str) {
        let Ok(index) = index.parse::<usize>() else { return };
        self.stream_focus = if self.stream_focus == Some(index) { None } else { Some(index) };
        self.entry_focus = Some(index);
        self.error.clear();
    }

    fn fill_hash(&mut self, index: &str) {
        let Ok(index) = index.parse::<usize>() else { return };
        if let Some(RedisKeyDetail { value: RedisValue::Hash { entries }, .. }) = self.detail.as_deref() {
            if let Some(entry) = entries.get(index) {
                self.entry_field = entry.field.clone();
                self.entry_value = entry.value.clone();
                self.entry_focus = Some(index);
            }
        }
    }

    fn hash_field(&self, index: &str) -> Option<String> {
        let index = index.parse::<usize>().ok()?;
        match self.detail.as_deref() {
            Some(RedisKeyDetail { value: RedisValue::Hash { entries }, .. }) => entries.get(index).map(|entry| entry.field.clone()),
            _ => None,
        }
    }

    fn list_item(&self, index: &str) -> Option<String> {
        let index = index.parse::<usize>().ok()?;
        match self.detail.as_deref() {
            Some(RedisKeyDetail { value: RedisValue::List { items }, .. }) => items.get(index).cloned(),
            _ => None,
        }
    }

    fn set_member(&self, index: &str) -> Option<String> {
        let index = index.parse::<usize>().ok()?;
        match self.detail.as_deref() {
            Some(RedisKeyDetail { value: RedisValue::Set { items }, .. }) => items.get(index).cloned(),
            _ => None,
        }
    }

    fn zset_member(&self, index: &str) -> Option<String> {
        let index = index.parse::<usize>().ok()?;
        match self.detail.as_deref() {
            Some(RedisKeyDetail { value: RedisValue::ZSet { entries }, .. }) => entries.get(index).map(|entry| entry.member.clone()),
            _ => None,
        }
    }

    fn stream_message(&self, index: &str) -> Option<String> {
        let index = index.parse::<usize>().ok()?;
        match self.detail.as_deref() {
            Some(RedisKeyDetail { value: RedisValue::Stream { messages }, .. }) => messages.get(index).map(|message| message.id.clone()),
            _ => None,
        }
    }

    fn stream_payload_json(&self, index: &str) -> Option<String> {
        let index = index.parse::<usize>().ok()?;
        match self.detail.as_deref() {
            Some(RedisKeyDetail { value: RedisValue::Stream { messages }, .. }) => {
                let message = messages.get(index)?;
                let mut map = serde_json::Map::new();
                for field in &message.fields {
                    let parsed = serde_json::from_str::<serde_json::Value>(&field.value)
                        .unwrap_or_else(|_| serde_json::Value::String(field.value.clone()));
                    map.insert(field.field.clone(), parsed);
                }
                serde_json::to_string_pretty(&serde_json::Value::Object(map)).ok()
            }
            _ => None,
        }
    }

    fn show_error(&mut self, message: String) {
        self.show_error_from(ErrorSource::Command, message);
    }

    fn show_error_from(&mut self, source: ErrorSource, message: String) {
        self.error = message;
        self.error_source = source;
        self.note.clear();
    }

    fn keys_visible(&self, width: f32) -> bool {
        if width >= kit::NARROW_PAGE_WIDTH {
            return true;
        }
        self.nav_choice.unwrap_or(self.selected.is_empty())
    }

    fn text(&self, key: &str) -> String {
        i18n::text(self.locale, key)
    }

    fn spawn<T: Send + 'static>(
        &mut self,
        cx: &mut Context<Self>,
        clear_note: bool,
        work: impl std::future::Future<Output = Result<T, String>> + Send + 'static,
        finish: fn(&mut Self, Result<T, String>, &mut Context<Self>),
    ) {
        let generation = self.begin(clear_note);
        let view = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::app::views::grid::runtime_for_views().spawn(async move {
                let _ = tx.send(work.await);
            });
            if let Ok(result) = rx.await {
                cx.update(|cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |pane, cx| {
                            if pane.generation != generation {
                                return;
                            }
                            pane.busy = false;
                            finish(pane, result, cx);
                            cx.notify();
                        });
                    }
                });
            }
        })
        .detach();
    }

    fn begin(&mut self, clear_note: bool) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.busy = true;
        self.error.clear();
        if clear_note {
            self.note.clear();
        }
        self.generation
    }

    pub fn status_line(&self) -> String {
        let keys = self.keys.iter().map(|key| format!("{} {}", key.key_type, key.name)).collect::<Vec<_>>().join(" | ");
        format!(
            "{name} db={db} pattern={pattern} ttl={ttl} rename={rename} command={command} selected={selected} note={note} error={error} confirm={confirm} reply={reply} detail={detail} keys={keys}",
            name = self.connection_name,
            db = self.database,
            pattern = self.pattern,
            ttl = self.ttl_input,
            rename = self.rename_to,
            command = self.command_input,
            selected = self.selected,
            note = self.note,
            error = self.error,
            confirm = self.confirm_prompt,
            reply = self.reply,
            detail = self.detail_text().replace('\n', " "),
        )
    }

    fn open_key_context_menu(&mut self, index: usize, at: Point<Pixels>, cx: &mut Context<Self>) {
        if self.keys.get(index).is_none() {
            return;
        }
        self.context_menu = Some(ContextMenuState {
            at,
            items: vec![
                ContextItem { id: format!("redis-select:{index}"), icon: IconName::Eye, label: self.text("redis.menu.open"), danger: false },
                ContextItem { id: format!("redis-key-copy-name:{index}"), icon: IconName::Copy, label: self.text("redis.copy.keyName"), danger: false },
                ContextItem { id: format!("redis-key-fill-cmd:{index}"), icon: IconName::SquareTerminal, label: self.text("redis.menu.fillCommand"), danger: false },
                ContextItem { id: format!("redis-key-delete:{index}"), icon: IconName::Trash, label: self.text("redis.tab.remove"), danger: true },
            ],
        });
        cx.notify();
    }

    fn open_folder_context_menu(&mut self, prefix: String, at: Point<Pixels>, cx: &mut Context<Self>) {
        self.context_menu = Some(ContextMenuState {
            at,
            items: vec![
                ContextItem { id: format!("redis-folder:{prefix}"), icon: IconName::FolderOpen, label: self.text("redis.menu.expandCollapse"), danger: false },
                ContextItem { id: format!("redis-folder-filter:{prefix}"), icon: IconName::ListFilter, label: self.text("redis.menu.filterPrefix"), danger: false },
                ContextItem { id: format!("redis-folder-copy:{prefix}"), icon: IconName::Copy, label: self.text("redis.copy.prefix"), danger: false },
                ContextItem { id: format!("redis-folder-new:{prefix}"), icon: IconName::Plus, label: self.text("redis.menu.newInPrefix"), danger: false },
            ],
        });
        cx.notify();
    }

    fn open_entry_context_menu(&mut self, items: Vec<ContextItem>, at: Point<Pixels>, cx: &mut Context<Self>) {
        self.context_menu = Some(ContextMenuState { at, items });
        cx.notify();
    }
}

pub fn split_commandline(line: &str) -> (String, Vec<String>) {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for ch in line.trim().chars() {
        match quote {
            Some(mark) if ch == mark => quote = None,
            Some(_) => current.push(ch),
            None if ch == '"' || ch == '\'' => quote = Some(ch),
            None if ch.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            None => current.push(ch),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    if tokens.len() >= 2 && exec::is_compound_command(&tokens[0], &tokens[1]) {
        let command = format!("{} {}", tokens[0], tokens[1]);
        return (command, tokens.into_iter().skip(2).collect());
    }
    let mut iter = tokens.into_iter();
    (iter.next().unwrap_or_default(), iter.collect())
}

#[allow(clippy::result_unit_err)]
pub fn parse_ttl(input: &str) -> Result<Option<i64>, ()> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if !trimmed.chars().all(|ch| ch.is_ascii_digit()) {
        return Err(());
    }
    let seconds: i64 = trimmed.parse().map_err(|_| ())?;
    if seconds > 2_147_483_647 {
        return Err(());
    }
    Ok(Some(seconds))
}

fn parse_spec(spec: &str) -> Option<(String, String, String)> {
    let mut parts = spec.splitn(3, '|');
    let id = parts.next()?.trim().to_owned();
    let name = parts.next().unwrap_or("").trim().to_owned();
    let database = parts.next().unwrap_or("0").trim().to_owned();
    if id.is_empty() {
        return None;
    }
    Some((id, name, if database.is_empty() { "0".into() } else { database }))
}

fn merge_keys(mut loaded: Vec<RedisKeyInfo>, page: Vec<RedisKeyInfo>) -> Vec<RedisKeyInfo> {
    let mut positions: std::collections::HashMap<String, usize> = loaded
        .iter()
        .enumerate()
        .map(|(index, key)| (key.name.clone(), index))
        .collect();
    for item in page {
        if let Some(index) = positions.get(&item.name).copied() {
            loaded[index] = item;
        } else {
            positions.insert(item.name.clone(), loaded.len());
            loaded.push(item);
        }
    }
    loaded
}

fn mutation_key(mutation: &RedisMutation) -> String {
    match mutation {
        RedisMutation::CreateKey { key, .. }
        | RedisMutation::SetString { key, .. }
        | RedisMutation::HashSet { key, .. }
        | RedisMutation::HashDel { key, .. }
        | RedisMutation::ListPush { key, .. }
        | RedisMutation::ListSet { key, .. }
        | RedisMutation::ListRem { key, .. }
        | RedisMutation::SetAdd { key, .. }
        | RedisMutation::SetRem { key, .. }
        | RedisMutation::ZSetAdd { key, .. }
        | RedisMutation::ZSetRem { key, .. }
        | RedisMutation::StreamAdd { key, .. }
        | RedisMutation::StreamDel { key, .. }
        | RedisMutation::SetTtl { key, .. } => key.clone(),
    }
}

fn danger_prompt(locale: Locale, word: &str) -> String {
    let upper = word.to_ascii_uppercase();
    let key = match upper.as_str() {
        "FLUSHDB" => "redis.danger.flushdb",
        "FLUSHALL" => "redis.danger.flushall",
        "SHUTDOWN" => "redis.danger.shutdown",
        _ => "redis.danger.generic",
    };
    i18n::fill(&i18n::text(locale, key), &[("word", &upper)])
}

fn format_value(value: &RedisValue, locale: Locale) -> String {
    if matches!(value, RedisValue::None {}) {
        return i18n::text(locale, "redis.value.none");
    }
    if value_empty(value) {
        return i18n::text(locale, "redis.value.empty");
    }
    match value {
        RedisValue::String { text } => text.clone(),
        RedisValue::Hash { entries } => entries.iter().map(|entry| format!("{}={}", entry.field, entry.value)).collect::<Vec<_>>().join("\n"),
        RedisValue::List { items } | RedisValue::Set { items } => items.join("\n"),
        RedisValue::ZSet { entries } => entries.iter().map(|entry| format!("{} {}", entry.member, entry.score)).collect::<Vec<_>>().join("\n"),
        RedisValue::Stream { messages } => messages
            .iter()
            .map(|message| {
                let fields = message.fields.iter().map(|field| format!("{}={}", field.field, field.value)).collect::<Vec<_>>().join(" ");
                format!("{} {fields}", message.id)
            })
            .collect::<Vec<_>>()
            .join("\n"),
        RedisValue::None {} => String::new(),
    }
}

fn value_empty(value: &RedisValue) -> bool {
    match value {
        RedisValue::String { text } => text.is_empty(),
        RedisValue::Hash { entries } => entries.is_empty(),
        RedisValue::List { items } | RedisValue::Set { items } => items.is_empty(),
        RedisValue::ZSet { entries } => entries.is_empty(),
        RedisValue::Stream { messages } => messages.is_empty(),
        RedisValue::None {} => false,
    }
}

fn reply_text(reply: &RespReply) -> String {
    match reply.kind {
        "nil" => "nil".to_owned(),
        "int" => reply.int.unwrap_or_default().to_string(),
        "array" => reply.items.as_ref().map(|items| items.iter().map(reply_text).collect::<Vec<_>>().join(" ")).unwrap_or_default(),
        _ => reply.text.clone().unwrap_or_default(),
    }
}

fn reply_pretty(reply: &RespReply, indent: usize) -> String {
    let pad = "  ".repeat(indent);
    match reply.kind {
        "nil" => format!("{pad}(nil)"),
        "int" => format!("{pad}(integer) {}", reply.int.unwrap_or_default()),
        "array" => {
            let Some(items) = reply.items.as_ref() else {
                return format!("{pad}(empty array)");
            };
            if items.is_empty() {
                return format!("{pad}(empty array)");
            }
            items
                .iter()
                .enumerate()
                .map(|(ix, item)| {
                    if item.kind == "array" {
                        format!("{pad}{}) \n{}", ix + 1, reply_pretty(item, indent + 1))
                    } else {
                        format!("{pad}{}) {}", ix + 1, reply_pretty(item, 0))
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
        _ => {
            let raw = reply.text.clone().unwrap_or_default();
            if raw.trim_start().starts_with('{') || raw.trim_start().starts_with('[') {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw) {
                    if let Ok(pretty) = serde_json::to_string_pretty(&val) {
                        return pretty;
                    }
                }
            }
            format!("{pad}{raw}")
        }
    }
}

fn describe_ttl(locale: Locale, ttl_ms: Option<i64>) -> String {
    match ttl_ms {
        None => "—".to_owned(),
        Some(-1) => i18n::text(locale, "redis.keys.neverExpire"),
        Some(-2) => i18n::text(locale, "redis.keys.missing"),
        Some(ms) if ms >= 1000 => format!("{} s", ms / 1000),
        Some(ms) => format!("{ms} ms"),
    }
}

impl Render for RedisPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::of(cx);
        let m = kit::Metrics::of(cx);
        let entity = cx.entity();
        let mono = cx.theme().mono_font_family.clone();
        let pattern = self.redis_input(Field::Pattern, "redis-pattern", "redis.tab.patternLabel", "user:*", window, cx);
        let ttl = self.redis_input(Field::Ttl, "redis-ttl-field", "redis.tab.ttlLabel", "3600", window, cx);
        let rename = self.redis_input(Field::Rename, "redis-rename-field", "redis.tab.rename", "new:key", window, cx);
        let command = self.redis_input(Field::Command, "redis-command", "redis.tab.commandLabel", "GET key", window, cx);
        let key_rows = self.build_key_rows();
        let type_bar = self.type_filters(&p, &entity);
        let new_form = if self.new_open { Some(self.new_key_form(&p, &entity, window, cx)) } else { None };
        let db_panel = if self.db_picker_open { Some(self.db_picker_panel(&p, &entity)) } else { None };

        let viewport_w = f32::from(window.viewport_size().width);
        self.viewport_w = viewport_w;
        let narrow = viewport_w < kit::NARROW_PAGE_WIDTH;
        let show_keys = self.keys_visible(viewport_w);
        let scan_failed = self.error_source == ErrorSource::Scan && !self.error.is_empty();
        let pattern_active = !self.pattern.trim().is_empty();
        let visible = key_rows.len();
        let state = list_state(self.busy, scan_failed, self.keys.len(), visible, pattern_active, self.done);

        let list = stack_frame(|| {
            let mut list = div()
                .id("redis-keys")
                .test_support()
                .flex()
                .flex_col()
                .flex_1()
                .min_h(px(0.))
                .overflow_y_scroll()
                .p(px(m.pad))
                .gap(px(1.));
            match state {
                ListState::Loading => list = list.child(kit::skeleton_state(6, &m, &p)),
                ListState::Error => {
                    let owner = entity.clone();
                    let retry = kit::secondary_button(
                        "redis-retry-scan",
                        Some(IconName::RefreshCw),
                        SharedString::from(self.text("common.retry")),
                        move |_, _, cx| owner.update(cx, |pane, cx| pane.command("redis-retry-scan", cx)),
                    )
                    .into_any_element();
                    list = list.child(kit::error_state_with_action(
                        SharedString::from(self.text("redis.error.scan.heading")),
                        SharedString::from(self.error.clone()),
                        retry,
                        &p,
                    ));
                }
                ListState::NoMatch => {
                    let owner = entity.clone();
                    let clear = kit::secondary_button(
                        "redis-clear-filter",
                        Some(IconName::ListFilter),
                        SharedString::from(self.text("redis.noMatch.clear")),
                        move |_, _, cx| owner.update(cx, |pane, cx| pane.command("redis-clear-filter", cx)),
                    )
                    .into_any_element();
                    list = list.child(kit::no_match_state_with_action(
                        IconName::Search,
                        SharedString::from(self.text("redis.noMatch.heading")),
                        SharedString::from(self.text("redis.noMatch.detail")),
                        clear,
                        &p,
                    ));
                }
                ListState::Empty => {
                    let owner = entity.clone();
                    let create = kit::primary_button(
                        "redis-new-key",
                        Some(IconName::Plus),
                        SharedString::from(self.text("redis.action.newKey")),
                        move |_, _, cx| owner.update(cx, |pane, cx| pane.command("redis-new-key", cx)),
                    )
                    .into_any_element();
                    list = list.child(kit::empty_state_with_action(
                        IconName::Key,
                        SharedString::from(self.text("redis.empty.heading")),
                        SharedString::from(self.text("redis.empty.detail")),
                        create,
                        &p,
                    ));
                }
                ListState::Rows => {
                    if self.busy {
                        list = list.child(kit::inline_loading(SharedString::from(self.text("redis.list.busy")), &m, &p));
                    } else if visible == 0 {
                        list = list.child(
                            div()
                                .id("redis-keys-blank")
                                .test_support()
                                .px(px(m.pane_pad()))
                                .py(px(4.))
                                .text_size(px(TypeScale::XS))
                                .text_color(p.text_faint)
                                .child(SharedString::from(self.text("redis.tab.emptyList"))),
                        );
                    }
                }
            }
            for row in key_rows {
                match row {
                    KeyRow::Folder { prefix, segment, depth, count, open } => {
                        let id = format!("redis-folder:{prefix}");
                        let menu_prefix = prefix.clone();
                        let click_entity = entity.clone();
                        let menu_entity = entity.clone();
                        list = list.child(
                            kit::interactive_states(div().id(SharedString::from(id.clone())), &p, false)
                                .test_support()
                                .role(gpui_kit::Role::Button)
                                .aria_label(SharedString::from(prefix.clone()))
                                .h(px(m.tree_row_h))
                                .pl(px(8. + depth as f32 * 14.))
                                .pr(px(8.))
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .rounded(px(Radius::CONTROL))
                                .text_size(px(TypeScale::SM))
                                .text_color(p.text)
                                .hover(|style| style.bg(p.hover))
                                .child(kit::icon(if open { IconName::ChevronDown } else { IconName::ChevronRight }, 12., p.text_faint))
                                .child(kit::icon(if open { IconName::FolderOpen } else { IconName::Folder }, 13., p.accent_text))
                                .child(div().flex_1().min_w(px(0.)).truncate().font_family(mono.clone()).font_weight(FontWeight::MEDIUM).child(SharedString::from(segment)))
                                .child(
                                    div()
                                        .flex_none()
                                        .px(px(5.))
                                        .rounded(px(Radius::TAG))
                                        .bg(p.sunken)
                                        .text_size(px(TypeScale::XS))
                                        .text_color(p.text_muted)
                                        .child(SharedString::from(count.to_string())),
                                )
                                .on_mouse_down(MouseButton::Right, move |event: &MouseDownEvent, _, cx| {
                                    cx.stop_propagation();
                                    let pfx = menu_prefix.clone();
                                    let pos = event.position;
                                    menu_entity.update(cx, |pane, cx| pane.open_folder_context_menu(pfx, pos, cx));
                                })
                                .on_click(move |_, _, cx| {
                                    let id = id.clone();
                                    click_entity.update(cx, |pane, cx| pane.command(&id, cx));
                                }),
                        );
                    }
                    KeyRow::Key { index, display, depth } => {
                        let key = &self.keys[index];
                        let selected = key.name == self.selected;
                        let id = format!("redis-select:{index}");
                        let label = format!("{} {} {}", key.key_type, key.name, describe_ttl(self.locale, key.ttl_ms));
                        let (fg, bg) = type_colors(&key.key_type, &p);
                        let click_entity = entity.clone();
                        let menu_entity = entity.clone();
                        let full_name = key.name.clone();
                        list = list.child(
                            kit::interactive_states(div().id(SharedString::from(id.clone())), &p, false)
                                .test_support()
                                .role(gpui_kit::Role::Button)
                                .aria_label(SharedString::from(label))
                                .aria_selected(selected)
                                .tooltip_text(SharedString::from(full_name))
                                .h(px(m.tree_row_h + 2.))
                                .pl(px(8. + depth as f32 * 14.))
                                .pr(px(8.))
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .rounded(px(Radius::CONTROL))
                                .when(selected, |this| this.bg(p.selection))
                                .when(!selected, |this| this.hover(|style| style.bg(p.hover)))
                                .child(div().flex_none().w(px(42.)).child(kit::pill(SharedString::from(key.key_type.clone()), fg, bg)))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .truncate()
                                        .font_family(mono.clone())
                                        .text_size(px(TypeScale::SM))
                                        .child(SharedString::from(display)),
                                )
                                .when(key.ttl_ms.is_some_and(|ttl| ttl >= 0), |this| this.child(kit::icon(IconName::Timer, 12., p.text_faint)))
                                .on_mouse_down(MouseButton::Right, move |event: &MouseDownEvent, _, cx| {
                                    cx.stop_propagation();
                                    let pos = event.position;
                                    menu_entity.update(cx, |pane, cx| pane.open_key_context_menu(index, pos, cx));
                                })
                                .on_click(move |_, _, cx| {
                                    let id = id.clone();
                                    click_entity.update(cx, |pane, cx| pane.command(&id, cx));
                                }),
                        );
                    }
                }
            }
            list.into_any_element()
        });

        let left = stack_frame(|| {
            let max_side = (viewport_w * 0.5).clamp(SIDEBAR_MIN_W, SIDEBAR_MAX_W);
            let side_w = self.sidebar_w.clamp(SIDEBAR_MIN_W, max_side);
            let db_toggle_entity = entity.clone();
            let db_open = self.db_picker_open;
            let db_button = kit::interactive_states(div().id("redis-db-toggle"), &p, false)
                .test_support()
                .role(gpui_kit::Role::Button)
                .aria_label(SharedString::from(format!("{} db{}", self.text("redis.db.switch"), self.database)))
                .aria_expanded(db_open)
                .tooltip_text(SharedString::from(self.text("redis.db.switch")))
                .h(px(24.))
                .px(px(7.))
                .flex()
                .items_center()
                .gap(px(4.))
                .rounded(px(Radius::CONTROL))
                .border_1()
                .when(db_open, |this| this.border_color(p.accent).bg(p.accent_soft).text_color(p.accent_text))
                .when(!db_open, |this| {
                    this.border_color(p.line)
                        .bg(if p.dark { p.raised } else { p.surface })
                        .text_color(p.text)
                        .hover(|s| s.bg(p.hover).border_color(p.line_strong))
                })
                .text_size(px(TypeScale::XS))
                .font_weight(FontWeight::MEDIUM)
                .child(SharedString::from(format!("db {}", self.database)))
                .child(kit::icon(IconName::ChevronDown, 11., if db_open { p.accent_text } else { p.text_muted }))
                .on_click(move |_, _, cx| db_toggle_entity.update(cx, |pane, cx| pane.command("redis-db-toggle", cx)));

            let left = div()
                .h_full()
                .flex()
                .flex_col()
                .bg(p.chrome)
                .when(narrow, |this| this.w_full().min_w(px(0.)))
                .when(!narrow, |this| this.flex_none().w(px(side_w)))
                .child(
                    div()
                        .p(px(10.))
                        .flex()
                        .flex_col()
                        .gap(px(7.))
                        .border_b_1()
                        .border_color(p.line_soft)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(7.))
                                .child(kit::engine_badge("redis", 20.))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .truncate()
                                        .text_size(px(TypeScale::SM))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(SharedString::from(self.connection_name.clone())),
                                )
                                .child(db_button),
                        )
                        .children(db_panel)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(4.))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .child(
                                            kit::text_input("redis-pattern", &pattern, self.text("redis.tab.patternLabel"))
                                                .small()
                                                .cleanable(true)
                                                .prefix(kit::input_prefix(IconName::Search, 13., p.text_faint)),
                                        ),
                                )
                                .child(redis_button("redis-filter", Some(IconName::ListFilter), &self.text("redis.tab.filter"), Kind::Tool, &entity))
                                .child(redis_icon("redis-refresh", IconName::RefreshCw, &self.text("redis.tab.refresh"), &entity)),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap(px(4.))
                                .child({
                                    let flat_entity = entity.clone();
                                    let tree_entity = entity.clone();
                                    kit::segmented_sized(
                                        vec![
                                            (
                                                SharedString::from("redis-view-flat"),
                                                SharedString::from(self.text("redis.view.flat")),
                                                !self.tree_view,
                                                kit::click(move |_, _, cx| flat_entity.update(cx, |pane, cx| pane.command("redis-view-flat", cx))),
                                            ),
                                            (
                                                SharedString::from("redis-view-tree"),
                                                SharedString::from(self.text("redis.view.tree")),
                                                self.tree_view,
                                                kit::click(move |_, _, cx| tree_entity.update(cx, |pane, cx| pane.command("redis-view-tree", cx))),
                                            ),
                                        ],
                                        &p,
                                        m.control_h - 4.,
                                    )
                                })
                                .child(redis_button("redis-new-key", Some(IconName::Plus), &self.text("redis.action.newKey"), Kind::Primary, &entity).small()),
                        )
                        .child(type_bar)
                        .children(new_form),
                )
                .child(list)
                .child(if scan_failed && !self.keys.is_empty() {
                    div()
                        .id("redis-list-error")
                        .test_support()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .px(px(10.))
                        .py(px(6.))
                        .border_t_1()
                        .border_color(p.line)
                        .child(kit::icon(IconName::CircleAlert, 13., p.danger))
                        .child(div().flex_1().min_w(px(0.)).truncate().text_size(px(TypeScale::XS)).text_color(p.danger).child(SharedString::from(self.error.clone())))
                        .child(redis_button("redis-retry-scan", Some(IconName::RefreshCw), &self.text("common.retry"), Kind::Tool, &entity))
                        .into_any_element()
                } else {
                    let footer_count = if self.done {
                        format!("{} · {}", i18n::fill(&self.text("redis.db.keysCount"), &[("n", &self.keys.len().to_string())]), self.text("redis.tab.scanDone"))
                    } else {
                        i18n::fill(&self.text("redis.db.keysCount"), &[("n", &self.keys.len().to_string())])
                    };
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .px(px(10.))
                        .py(px(6.))
                        .border_t_1()
                        .border_color(p.line)
                        .child(div().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(SharedString::from(footer_count)))
                        .child(redis_button("redis-more", Some(IconName::ChevronsDown), &self.text("redis.tab.loadMore"), Kind::Tool, &entity).disabled(self.done || self.busy))
                        .into_any_element()
                });
            left.into_any_element()
        });

        // ---- 右侧：键详情与结构化格式化查看器
        let detail_error = self.error_source == ErrorSource::Detail && !self.error.is_empty();
        let loaded_value = if self.detail.is_some() {
            Some(stack_frame(|| self.value_body(&p, &entity, window, cx)))
        } else {
            None
        };
        let detail = stack_frame(|| {
            let value_panel = if let Some(value) = loaded_value {
                Some(value)
            } else if detail_error {
                let owner = entity.clone();
                let retry = kit::secondary_button(
                    "redis-retry-read",
                    Some(IconName::RefreshCw),
                    SharedString::from(self.text("redis.retry.read")),
                    move |_, _, cx| owner.update(cx, |pane, cx| pane.command("redis-retry-read", cx)),
                )
                .into_any_element();
                Some(
                    kit::error_state_with_action(
                        SharedString::from(self.text("redis.error.detail.heading")),
                        SharedString::from(self.error.clone()),
                        retry,
                        &p,
                    )
                    .into_any_element(),
                )
            } else if self.busy {
                Some(kit::skeleton_state(4, &m, &p).into_any_element())
            } else {
                Some(
                    div()
                        .text_size(px(TypeScale::SM))
                        .text_color(p.text_muted)
                        .child(SharedString::from(self.text("redis.tab.pickKey")))
                        .into_any_element(),
                )
            };
            let mut detail = div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .p(px(12.))
                .flex_1()
                .min_w(px(0.))
                .min_h(px(0.));
            if self.selected.is_empty() {
                let owner = entity.clone();
                let create = kit::primary_button(
                    "redis-new-key-value",
                    Some(IconName::Plus),
                    SharedString::from(self.text("redis.action.newKey")),
                    move |_, _, cx| owner.update(cx, |pane, cx| pane.command("redis-new-key-value", cx)),
                )
                .into_any_element();
                detail = detail.child(
                    div().flex_1().min_w(px(0.)).child(kit::empty_state_with_action(
                        IconName::Key,
                        SharedString::from(self.text("redis.empty.pick.heading")),
                        SharedString::from(self.text("redis.tab.pickKey")),
                        create,
                        &p,
                    )),
                );
                detail = detail.child(
                    div()
                        .hidden()
                        .child(redis_button("redis-copy", None, &self.text("redis.value.value"), Kind::Tool, &entity))
                        .child(redis_button("redis-delete", None, &self.text("redis.tab.remove"), Kind::Danger, &entity))
                        .child(redis_button("redis-ttl", None, &self.text("redis.tab.setTtl"), Kind::Secondary, &entity))
                        .child(redis_button("redis-rename", None, &self.text("redis.tab.rename"), Kind::Secondary, &entity))
                        .child(kit::text_input("redis-ttl-field", &ttl, self.text("redis.tab.ttlLabel")))
                        .child(kit::text_input("redis-rename-field", &rename, self.text("redis.tab.rename"))),
                );
            } else {
                detail = stack_frame(|| {
                    let mut detail = detail;
                    let (type_name, ttl_text) = self
                        .detail
                        .as_ref()
                        .map(|d| (d.key_type.clone(), describe_ttl(self.locale, d.ttl_ms)))
                        .unwrap_or_default();
                    let (fg, bg) = type_colors(&type_name, &p);
                    let copy_key_entity = entity.clone();

                    let mut header_row = div()
                        .flex_none()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(8.))
                        .child(kit::pill(SharedString::from(type_name), fg, bg))
                        .child(
                            kit::interactive_states(div().id("redis-copy-key"), &p, false)
                                .test_support()
                                .role(gpui_kit::Role::Button)
                                .aria_label(SharedString::from(self.selected.clone()))
                                .tooltip_text(SharedString::from(format!("{} · {}", self.text("redis.copy.keyName"), self.selected)))
                                .flex_1()
                                .min_w(px(120.))
                                .px(px(6.))
                                .py(px(2.))
                                .rounded(px(Radius::CONTROL))
                                .hover(|s| s.bg(p.hover))
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .child(
                                    div()
                                        .min_w(px(0.))
                                        .truncate()
                                        .font_family(mono.clone())
                                        .text_size(px(TypeScale::MD))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(SharedString::from(self.selected.clone())),
                                )
                                .child(kit::icon(IconName::Copy, 12., p.text_faint))
                                .on_click(move |_, _, cx| copy_key_entity.update(cx, |pane, cx| pane.command("redis-copy-key", cx))),
                        )
                        .child(
                            div()
                                .flex_none()
                                .px(px(7.))
                                .py(px(2.))
                                .rounded(px(Radius::TAG))
                                .bg(p.sunken)
                                .flex()
                                .items_center()
                                .gap(px(4.))
                                .text_size(px(TypeScale::XS))
                                .text_color(p.text_muted)
                                .child(kit::icon(IconName::Timer, 12., p.text_faint))
                                .child(SharedString::from(ttl_text)),
                        );

                    if !self.ttl_open {
                        header_row = header_row.child(redis_button("redis-ttl", Some(IconName::Timer), &self.text("redis.tab.setTtl"), Kind::Tool, &entity).small());
                    } else {
                        header_row = header_row.child(redis_button("redis-ttl-toggle", Some(IconName::Timer), &self.text("redis.tab.setTtl"), Kind::Secondary, &entity).small());
                    }
                    if !self.rename_open {
                        header_row = header_row.child(redis_button("redis-rename", Some(IconName::Pencil), &self.text("redis.tab.rename"), Kind::Tool, &entity).small());
                    } else {
                        header_row = header_row.child(redis_button("redis-rename-toggle", Some(IconName::Pencil), &self.text("redis.tab.rename"), Kind::Secondary, &entity).small());
                    }
                    header_row = header_row
                        .child(redis_button("redis-copy", Some(IconName::Copy), &self.text("redis.value.value"), Kind::Tool, &entity).small())
                        .child(redis_icon("redis-refresh-key", IconName::RefreshCw, &self.text("redis.tab.refresh"), &entity))
                        .child(redis_button("redis-delete", Some(IconName::Trash), &self.text("redis.tab.remove"), Kind::Danger, &entity).small());

                    detail = detail.child(header_row);

                    // 按需展开的紧凑单行 TTL / 重命名编辑条（平时 0 高度，绝不挤占键值展示区）。
                    if self.ttl_open {
                        detail = detail.child(
                            div()
                                .flex_none()
                                .px(px(10.))
                                .py(px(6.))
                                .rounded(px(Radius::CONTROL))
                                .bg(p.sunken)
                                .border_1()
                                .border_color(p.line_soft)
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .child(div().text_size(px(TypeScale::XS)).text_color(p.text_muted).child(SharedString::from(self.text("redis.tab.ttlLabel"))))
                                .child(div().w(px(160.)).child(kit::text_input("redis-ttl-field", &ttl, self.text("redis.tab.ttlLabel")).small()))
                                .child(redis_button("redis-ttl", Some(IconName::Check), &self.text("redis.tab.setTtl"), Kind::Primary, &entity).small())
                                .child(redis_icon("redis-ttl-close", IconName::X, &self.text("common.cancel"), &entity)),
                        );
                    } else {
                        detail = detail.child(div().hidden().child(kit::text_input("redis-ttl-field", &ttl, self.text("redis.tab.ttlLabel"))));
                    }

                    if self.rename_open {
                        detail = detail.child(
                            div()
                                .flex_none()
                                .px(px(10.))
                                .py(px(6.))
                                .rounded(px(Radius::CONTROL))
                                .bg(p.sunken)
                                .border_1()
                                .border_color(p.line_soft)
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .child(div().text_size(px(TypeScale::XS)).text_color(p.text_muted).child(SharedString::from(self.text("redis.tab.rename"))))
                                .child(div().flex_1().min_w(px(160.)).child(kit::text_input("redis-rename-field", &rename, self.text("redis.tab.rename")).small()))
                                .child(redis_button("redis-rename", Some(IconName::Check), &self.text("redis.tab.rename"), Kind::Primary, &entity).small())
                                .child(redis_icon("redis-rename-close", IconName::X, &self.text("common.cancel"), &entity)),
                        );
                    } else {
                        detail = detail.child(div().hidden().child(kit::text_input("redis-rename-field", &rename, self.text("redis.tab.rename"))));
                    }

                    detail = detail.child(
                        div()
                            .id("redis-value")
                            .test_support()
                            .flex_1()
                            .min_h(px(120.))
                            .overflow_y_scroll()
                            .p(px(12.))
                            .rounded(px(Radius::PANEL))
                            .bg(p.chrome)
                            .border_1()
                            .border_color(p.line)
                            .font_family(mono.clone())
                            .text_size(px(TypeScale::SM))
                            .flex()
                            .flex_col()
                            .children(value_panel),
                    );
                    detail
                });
            }
            detail.into_any_element()
        });

        // ---- 底部：命令控制台（含可见回复面板、语法签名提示与命令/键名自动补全）
        let console = stack_frame(|| {
            let (_, _, completions) = self.current_completions();
            let active_comp = if completions.is_empty() { 0 } else { self.complete_index.min(completions.len() - 1) };
            let cmd_spec = complete::lookup_command(&self.command_input);

            let mut console = div()
                .flex_none()
                .px(px(12.))
                .py(px(8.))
                .flex()
                .flex_col()
                .gap(px(6.))
                .border_t_1()
                .border_color(p.line)
                .bg(p.chrome)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(redis_button("redis-console", Some(IconName::SquareTerminal), &self.text("redis.console.toggle"), Kind::Tool, &entity).small())
                        .child(redis_icon("redis-history-older", IconName::ChevronUp, &self.text("redis.console.prev"), &entity))
                        .child(redis_icon("redis-history-newer", IconName::ChevronDown, &self.text("redis.console.next"), &entity))
                        .when_some(cmd_spec, |this, spec| {
                            let (mode_fg, mode_bg, mode_txt) = if spec.is_write() {
                                (p.warn, p.warn_soft, "WRITE")
                            } else {
                                (p.ok, p.ok_soft, "READ")
                            };
                            this.child(kit::pill(SharedString::from(mode_txt), mode_fg, mode_bg)).child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .truncate()
                                    .px(px(6.))
                                    .font_family(mono.clone())
                                    .text_size(px(TypeScale::XS))
                                    .text_color(p.text_muted)
                                    .child(SharedString::from(format!("{} — {}", spec.syntax, spec.summary(self.locale)))),
                            )
                        })
                        .when(cmd_spec.is_none(), |this| this.child(div().flex_1()))
                        .when(!self.reply.is_empty() && self.console_open, |this| {
                            this.child(redis_icon("redis-console-copy", IconName::Copy, &self.text("redis.console.copyOutput"), &entity))
                                .child(redis_icon("redis-console-clear", IconName::Trash, &self.text("redis.console.clear"), &entity))
                        }),
                );

            if self.console_open {
                if !self.reply.is_empty() {
                    let shown_reply = if self.pretty_reply.is_empty() { self.reply.clone() } else { self.pretty_reply.clone() };
                    console = console.child(
                        div()
                            .id("redis-console-output")
                            .test_support()
                            .max_h(px(140.))
                            .overflow_y_scroll()
                            .px(px(10.))
                            .py(px(7.))
                            .rounded(px(Radius::CONTROL))
                            .bg(p.sunken)
                            .border_1()
                            .border_color(p.line_soft)
                            .font_family(mono.clone())
                            .text_size(px(TypeScale::XS))
                            .text_color(p.text)
                            .child(SharedString::from(shown_reply)),
                    );
                }

                if !completions.is_empty() {
                    let mut popup = div()
                        .id("redis-complete-list")
                        .test_support()
                        .max_h(px(168.))
                        .overflow_y_scroll()
                        .p(px(4.))
                        .rounded(px(Radius::PANEL))
                        .bg(p.raised)
                        .border_1()
                        .border_color(p.line_strong)
                        .shadow_sm()
                        .flex()
                        .flex_col()
                        .gap(px(1.));
                    for (ix, item) in completions.iter().take(12).enumerate() {
                        let pick_id = format!("redis-complete-pick:{ix}");
                        let pick_entity = entity.clone();
                        let selected = ix == active_comp;
                        let (badge_fg, badge_bg, badge_label) = match item.kind {
                            "key" => (p.ok, p.ok_soft, "KEY"),
                            "database" => (p.warn, p.warn_soft, "DB"),
                            _ => (p.accent_text, p.accent_soft, "CMD"),
                        };
                        popup = popup.child(
                            kit::interactive_states(div().id(SharedString::from(pick_id.clone())), &p, false)
                                .test_support()
                                .role(gpui_kit::Role::ListItem)
                                .aria_selected(selected)
                                .h(px(26.))
                                .px(px(8.))
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .rounded(px(Radius::CONTROL))
                                .when(selected, |s| s.bg(p.selection))
                                .when(!selected, |s| s.hover(|h| h.bg(p.hover)))
                                .child(kit::pill(SharedString::from(badge_label), badge_fg, badge_bg))
                                .child(
                                    div()
                                        .font_family(mono.clone())
                                        .text_size(px(TypeScale::SM))
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(SharedString::from(item.label.clone())),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .truncate()
                                        .text_size(px(TypeScale::XS))
                                        .text_color(p.text_muted)
                                        .child(SharedString::from(format!("{} · {}", item.detail, item.summary))),
                                )
                                .on_click(move |_, _, cx| {
                                    let cmd = pick_id.clone();
                                    pick_entity.update(cx, |pane, cx| pane.command(&cmd, cx));
                                }),
                        );
                    }
                    popup = popup.child(
                        div()
                            .px(px(8.))
                            .pt(px(2.))
                            .text_size(px(TypeScale::XS))
                            .text_color(p.text_faint)
                            .child(SharedString::from(self.text("redis.console.completeHint"))),
                    );
                    console = console.child(popup);
                }

                console = console.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(
                            div()
                                .flex_1()
                                .child(
                                    kit::text_input("redis-command", &command, self.text("redis.tab.commandLabel"))
                                        .small()
                                        .cleanable(true)
                                        .prefix(kit::input_prefix(IconName::ChevronRight, 13., p.accent_text)),
                                ),
                        )
                        .child(redis_button("redis-exec", Some(IconName::Play), &self.text("redis.tab.execute"), Kind::Primary, &entity).small()),
                );
            } else {
                console = console.child(
                    div()
                        .hidden()
                        .child(kit::text_input("redis-command", &command, self.text("redis.tab.commandLabel")))
                        .child(redis_button("redis-exec", Some(IconName::Play), &self.text("redis.tab.execute"), Kind::Primary, &entity)),
                );
            }

            if self.confirm.is_some() {
                console = console.child(
                    div()
                        .px(px(12.))
                        .py(px(8.))
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .rounded(px(Radius::PANEL))
                        .bg(p.danger_soft)
                        .text_color(p.danger)
                        .child(kit::icon(IconName::TriangleAlert, 15., p.danger))
                        .child(div().flex_1().min_w(px(0.)).text_size(px(TypeScale::SM)).child(SharedString::from(self.confirm_prompt.clone())))
                        .child(redis_button("redis-cancel", None, &i18n::text(self.locale, "common.cancel"), Kind::Secondary, &entity).small())
                        .child(redis_button("redis-confirm", Some(IconName::Check), &self.text("redis.tab.confirmExecute"), Kind::Danger, &entity).small()),
                );
            }
            if self.error_source == ErrorSource::Command && !self.error.is_empty() {
                let keys_blocked = self.error.contains("KEYS");
                let dismiss_owner = entity.clone();
                let scan_owner = entity.clone();
                let actions = div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .when(keys_blocked, |this| {
                        this.child(kit::primary_button(
                            "redis-use-scan",
                            Some(IconName::Search),
                            SharedString::from(self.text("redis.console.useScan")),
                            move |_, _, cx| scan_owner.update(cx, |pane, cx| pane.command("redis-use-scan", cx)),
                        ))
                    })
                    .child(kit::secondary_button(
                        "redis-error-dismiss",
                        Some(IconName::X),
                        SharedString::from(self.text("redis.error.dismiss")),
                        move |_, _, cx| dismiss_owner.update(cx, |pane, cx| pane.command("redis-error-dismiss", cx)),
                    ))
                    .into_any_element();
                console = console.child(kit::error_state_with_action(
                    SharedString::from(self.text("redis.error.command.heading")),
                    SharedString::from(self.error.clone()),
                    actions,
                    &p,
                ));
            }
            console = console.child(
                div()
                    .id("redis-status")
                    .test_support()
                    .role(gpui_kit::Role::Status)
                    .aria_label(SharedString::from(self.status_line()))
                    .min_h(px(16.))
                    .text_size(px(TypeScale::XS))
                    .text_color(p.text_muted)
                    .child(SharedString::from(self.note.clone())),
            );
            console.into_any_element()
        });

        let menu_layer = self.render_context_menu(&p, &entity);

        stack_frame(|| {
            let mut root = div()
                .id("redis-pane")
                .test_support()
                .size_full()
                .flex()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        if this.context_menu.take().is_some() {
                            cx.notify();
                        }
                    }),
                )
                .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                    if let Some((start_x, start_w)) = this.sidebar_drag_start {
                        let delta = f32::from(event.position.x) - start_x;
                        let max_w = (this.viewport_w * 0.5).clamp(SIDEBAR_MIN_W, SIDEBAR_MAX_W);
                        this.sidebar_w = (start_w + delta).clamp(SIDEBAR_MIN_W, max_w);
                        cx.notify();
                    }
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseUpEvent, _, cx| {
                        if this.sidebar_drag_start.take().is_some() {
                            cx.notify();
                        }
                    }),
                )
                .on_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, _, cx| {
                    let key = match event.keystroke.key.as_str() {
                        "arrowup" => "up",
                        "arrowdown" => "down",
                        other => other,
                    };
                    if key == "escape" {
                        if this.context_menu.take().is_some() || this.complete_open || this.db_picker_open {
                            this.complete_open = false;
                            this.db_picker_open = false;
                            cx.stop_propagation();
                            cx.notify();
                        }
                        return;
                    }
                    if this.focused != Some(Field::Command) {
                        return;
                    }
                    let (_, _, completions) = this.current_completions();
                    let comp_count = completions.len().min(12);
                    if comp_count > 0 && key == "tab" {
                        let idx = this.complete_index.min(comp_count - 1);
                        if this.accept_completion(idx) {
                            cx.stop_propagation();
                            cx.notify();
                            return;
                        }
                    }
                    if key == "up" || key == "down" {
                        if comp_count > 0 {
                            if key == "down" {
                                this.complete_index = (this.complete_index + 1) % comp_count;
                            } else {
                                this.complete_index = if this.complete_index == 0 { comp_count - 1 } else { this.complete_index - 1 };
                            }
                        } else {
                            this.history_step(key == "up");
                        }
                        cx.stop_propagation();
                        cx.notify();
                    }
                }));
            if narrow {
                let (toggle_icon, toggle_label) = if show_keys {
                    (IconName::ChevronRight, self.text("redis.nav.value"))
                } else {
                    (IconName::ChevronLeft, self.text("redis.nav.keys"))
                };
                let strip = div()
                    .id("redis-nav-strip")
                    .test_support()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(m.pad))
                    .px(px(m.pane_pad()))
                    .py(px(4.))
                    .border_b_1()
                    .border_color(p.line)
                    .bg(p.chrome)
                    .child(redis_button("redis-nav-toggle", Some(toggle_icon), &toggle_label, Kind::Tool, &entity))
                    .when(!show_keys, |this| {
                        this.child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .truncate()
                                .text_size(px(TypeScale::SM))
                                .text_color(p.text_muted)
                                .child(SharedString::from(self.selected.clone())),
                        )
                    });
                let content = if show_keys { left } else { detail };
                root = root
                    .flex_col()
                    .child(strip)
                    .child(div().id("redis-narrow-body").flex_1().min_h(px(0.)).flex().child(content))
                    .child(console)
                    .children(menu_layer);
            } else {
                let dragging = self.sidebar_drag_start.is_some();
                let splitter = div()
                    .id("redis-sidebar-splitter")
                    .test_support()
                    .flex_none()
                    .w(px(5.))
                    .h_full()
                    .cursor_col_resize()
                    .bg(if dragging { p.accent } else { p.line })
                    .hover(|s| s.bg(p.accent.opacity(0.65)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            if event.click_count >= 2 {
                                this.sidebar_w = SIDEBAR_DEFAULT_W;
                                this.sidebar_drag_start = None;
                            } else {
                                this.sidebar_drag_start = Some((f32::from(event.position.x), this.sidebar_w));
                            }
                            cx.notify();
                        }),
                    );
                root = root
                    .child(left)
                    .child(splitter)
                    .child(div().flex_1().min_w(px(0.)).h_full().flex().flex_col().child(detail).child(console))
                    .children(menu_layer);
            }
            root.into_any_element()
        })
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Primary,
    Secondary,
    Tool,
    Danger,
}

impl RedisPane {
    fn db_picker_panel(&self, p: &Palette, entity: &Entity<Self>) -> AnyElement {
        let mut grid = div()
            .id("redis-db-picker")
            .test_support()
            .p(px(6.))
            .rounded(px(Radius::PANEL))
            .bg(p.surface)
            .border_1()
            .border_color(p.line_strong)
            .flex()
            .flex_wrap()
            .gap(px(4.));
        for db in 0..16u8 {
            let db_str = db.to_string();
            let active = self.database == db_str;
            let count = self.db_counts.iter().find(|item| item.db == db).map(|item| item.keys);
            let label = match count {
                Some(n) if n > 0 => format!("db{db} ({n})"),
                _ => format!("db{db}"),
            };
            let cmd = format!("redis-db-pick:{db}");
            let owner = entity.clone();
            grid = grid.child(
                kit::interactive_states(div().id(SharedString::from(cmd.clone())), p, false)
                    .test_support()
                    .role(gpui_kit::Role::Button)
                    .aria_label(SharedString::from(label.clone()))
                    .aria_selected(active)
                    .h(px(24.))
                    .px(px(7.))
                    .flex()
                    .items_center()
                    .rounded(px(Radius::CONTROL))
                    .border_1()
                    .text_size(px(TypeScale::XS))
                    .when(active, |this| this.border_color(p.accent).bg(p.accent_soft).text_color(p.accent_text).font_weight(FontWeight::SEMIBOLD))
                    .when(!active && count.is_some_and(|n| n > 0), |this| {
                        this.border_color(p.line).text_color(p.text).hover(|s| s.bg(p.hover))
                    })
                    .when(!active && !count.is_some_and(|n| n > 0), |this| {
                        this.border_color(p.line_soft).text_color(p.text_muted).hover(|s| s.bg(p.hover).text_color(p.text))
                    })
                    .child(SharedString::from(label))
                    .on_click(move |_, _, cx| {
                        let cmd = cmd.clone();
                        owner.update(cx, |pane, cx| pane.command(&cmd, cx));
                    }),
            );
        }
        grid.into_any_element()
    }

    fn render_context_menu(&self, p: &Palette, entity: &Entity<Self>) -> Option<AnyElement> {
        let menu = self.context_menu.as_ref()?;
        let mut col = div()
            .id("redis-context-menu")
            .test_support()
            .occlude()
            .min_w(px(176.))
            .p(px(4.))
            .rounded(px(Radius::PANEL))
            .bg(p.raised)
            .border_1()
            .border_color(p.line_strong)
            .shadow_lg()
            .flex()
            .flex_col()
            .gap(px(1.))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation());

        for item in &menu.items {
            let cmd = item.id.clone();
            let owner = entity.clone();
            let fg = if item.danger { p.danger } else { p.text };
            let icon_fg = if item.danger { p.danger } else { p.text_muted };
            col = col.child(
                kit::interactive_states(div().id(SharedString::from(format!("ctx-{}", item.id))), p, false)
                    .test_support()
                    .role(gpui_kit::Role::MenuItem)
                    .aria_label(SharedString::from(item.label.clone()))
                    .h(px(28.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .rounded(px(Radius::CONTROL))
                    .text_size(px(TypeScale::SM))
                    .text_color(fg)
                    .hover(|s| if item.danger { s.bg(p.danger_soft) } else { s.bg(p.hover) })
                    .child(kit::icon(item.icon, 13., icon_fg))
                    .child(SharedString::from(item.label.clone()))
                    .on_click(move |_, _, cx| {
                        let cmd = cmd.clone();
                        owner.update(cx, |pane, cx| pane.command(&cmd, cx));
                    }),
            );
        }

        Some(
            deferred(
                anchored()
                    .position(menu.at)
                    .anchor(Anchor::TopLeft)
                    .snap_to_window_with_margin(px(8.))
                    .child(col),
            )
            .with_priority(20)
            .into_any_element(),
        )
    }

    fn type_filters(&self, p: &Palette, entity: &Entity<Self>) -> AnyElement {
        let mut bar = div().flex().flex_wrap().gap(px(3.));
        for (id, filter_val, label_key) in [
            ("redis-type-all", "", "redis.type.all"),
            ("redis-type-string", "string", "redis.type.string"),
            ("redis-type-hash", "hash", "redis.type.hash"),
            ("redis-type-list", "list", "redis.type.list"),
            ("redis-type-set", "set", "redis.type.set"),
            ("redis-type-zset", "zset", "redis.type.zset"),
            ("redis-type-stream", "stream", "redis.type.stream"),
        ] {
            let active = self.type_filter == filter_val;
            let owner = entity.clone();
            let text = self.text(label_key);
            bar = bar.child(
                kit::interactive_states(div().id(id), p, false)
                    .test_support()
                    .role(gpui_kit::Role::Button)
                    .aria_label(SharedString::from(text.clone()))
                    .aria_selected(active)
                    .h(px(22.))
                    .px(px(6.))
                    .flex()
                    .items_center()
                    .rounded(px(Radius::TAG))
                    .text_size(px(TypeScale::XS))
                    .when(active, |s| s.bg(p.accent_soft).text_color(p.accent_text).font_weight(FontWeight::MEDIUM))
                    .when(!active, |s| s.text_color(p.text_muted).hover(|h| h.bg(p.hover).text_color(p.text)))
                    .child(SharedString::from(text))
                    .on_click(move |_, _, cx| owner.update(cx, |pane, cx| pane.command(id, cx))),
            );
        }
        bar.into_any_element()
    }

    fn new_key_form(&mut self, p: &Palette, entity: &Entity<Self>, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let key = self.redis_input(Field::NewKey, "redis-new-name", "redis.key.name", "user:1", window, cx);
        let ttl = self.redis_input(Field::NewTtl, "redis-new-ttl", "redis.tab.ttlLabel", "", window, cx);
        let field = self.redis_input(Field::NewField, "redis-new-field", "redis.value.field", "field", window, cx);
        let value = self.redis_input(Field::NewValue, "redis-new-value", "redis.value.value", "value", window, cx);
        let score = self.redis_input(Field::NewScore, "redis-new-score", "redis.value.score", "1", window, cx);
        let pad = kit::Metrics::of(cx).pad;
        let mut form = div()
            .flex()
            .flex_col()
            .gap(px(pad))
            .p(px(pad))
            .rounded(px(Radius::PANEL))
            .bg(p.surface)
            .border_1()
            .border_color(p.line)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(4.))
                    .child(redis_button("redis-new-type-string", None, &self.text("redis.type.string"), Kind::Tool, entity).xsmall())
                    .child(redis_button("redis-new-type-hash", None, &self.text("redis.type.hash"), Kind::Tool, entity).xsmall())
                    .child(redis_button("redis-new-type-list", None, &self.text("redis.type.list"), Kind::Tool, entity).xsmall())
                    .child(redis_button("redis-new-type-set", None, &self.text("redis.type.set"), Kind::Tool, entity).xsmall())
                    .child(redis_button("redis-new-type-zset", None, &self.text("redis.type.zset"), Kind::Tool, entity).xsmall())
                    .child(redis_button("redis-new-type-stream", None, &self.text("redis.type.stream"), Kind::Tool, entity).xsmall()),
            )
            .child(kit::text_input("redis-new-name", &key, self.text("redis.key.name")).small())
            .child(kit::text_input("redis-new-ttl", &ttl, self.text("redis.tab.ttlLabel")).small());
        if matches!(self.new_type.as_str(), "hash" | "stream") {
            form = form.child(kit::text_input("redis-new-field", &field, self.text("redis.value.field")).small());
        }
        if self.new_type == "zset" {
            form = form.child(kit::text_input("redis-new-score", &score, self.text("redis.value.score")).small());
        }
        form = form
            .child(kit::text_input("redis-new-value", &value, self.text("redis.value.value")).small())
            .child(redis_button("redis-create", Some(IconName::Plus), &self.text("redis.action.newKey"), Kind::Primary, entity).small());
        if self.error.contains("已存在") {
            form = form.child(redis_button("redis-overwrite", None, &self.text("redis.tab.confirmExecute"), Kind::Danger, entity).small());
        }
        form.into_any_element()
    }

    fn ensure_string_viewer(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) -> Entity<FormatViewer> {
        let key = self.selected.clone();
        if self.string_viewer.is_none() || self.viewer_key != key {
            let spec = ViewerSpec {
                text: text.to_owned(),
                column_hint: Some(key.clone()),
                type_hint: Some("string".into()),
                bytes: None,
                truncation: TruncationState::Full,
                mode: EditMode::Editable,
                locale: self.locale,
            };
            let viewer = cx.new(|cx| FormatViewer::new(spec, window, cx));
            let sub = cx.subscribe(&viewer, |this, _, event, cx| {
                if let FormatViewerEvent::Commit(value) = event {
                    if this.selected.is_empty() {
                        return;
                    }
                    let value = value.clone();
                    this.stage_mutation(RedisMutation::SetString { key: this.selected.clone(), value });
                    cx.notify();
                }
            });
            self.string_sub = Some(sub);
            self.string_viewer = Some(viewer);
            self.viewer_key = key;
            self.reload_viewer = false;
        } else if self.reload_viewer {
            if let Some(viewer) = &self.string_viewer {
                let same = viewer.read(cx).draft() == viewer.read(cx).source();
                if same {
                    viewer.update(cx, |viewer, cx| viewer.replace_full(text.to_owned(), None, window, cx));
                }
            }
            self.reload_viewer = false;
        }
        self.string_viewer.clone().expect("string viewer")
    }

    /// 集合条目级结构化查看器（自动识别 JSON / XML / YAML / Hex 并支持一键复制与树状展开）。
    fn ensure_entry_viewer(&mut self, tag: &str, title_hint: &str, text: &str, window: &mut Window, cx: &mut Context<Self>) -> Entity<FormatViewer> {
        if self.entry_viewer.is_none() || self.entry_viewer_tag != tag {
            let spec = ViewerSpec {
                text: text.to_owned(),
                column_hint: Some(title_hint.to_owned()),
                type_hint: Some("text".into()),
                bytes: None,
                truncation: TruncationState::Full,
                mode: EditMode::ReadOnly,
                locale: self.locale,
            };
            let viewer = cx.new(|cx| FormatViewer::new(spec, window, cx));
            self.entry_viewer = Some(viewer);
            self.entry_viewer_tag = tag.to_owned();
        }
        self.entry_viewer.clone().expect("entry viewer")
    }

    fn render_entry_inspector(
        &mut self,
        selected_entry: Option<(usize, String, String)>,
        p: &Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some((idx, label, raw)) = selected_entry else {
            return div()
                .px(px(10.))
                .py(px(8.))
                .rounded(px(Radius::PANEL))
                .bg(p.surface)
                .border_1()
                .border_color(p.line_soft)
                .text_size(px(TypeScale::XS))
                .text_color(p.text_faint)
                .child(SharedString::from(self.text("redis.inspector.pickEntry")))
                .into_any_element();
        };
        let tag = format!("{}:{}:{}", self.selected, idx, raw.len());
        let viewer = self.ensure_entry_viewer(&tag, &label, &raw, window, cx);
        div()
            .id("redis-entry-inspector")
            .test_support()
            .flex_none()
            .h(px(230.))
            .mt(px(6.))
            .p(px(10.))
            .rounded(px(Radius::PANEL))
            .bg(p.surface)
            .border_1()
            .border_color(p.line)
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().text_size(px(TypeScale::XS)).font_weight(FontWeight::SEMIBOLD).text_color(p.text_muted).child(SharedString::from(self.text("redis.inspector.title"))))
                    .child(div().flex_1().min_w(px(0.)).truncate().text_size(px(TypeScale::XS)).text_color(p.accent_text).child(SharedString::from(label))),
            )
            .child(div().flex_1().min_h(px(0.)).child(viewer))
            .into_any_element()
    }

    fn value_body(&mut self, p: &Palette, entity: &Entity<Self>, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(detail) = self.detail.clone() else {
            return div().text_size(px(TypeScale::SM)).child(SharedString::from(self.text("redis.tab.pickKey"))).into_any_element();
        };
        let pad = kit::Metrics::of(cx).pad;
        match &detail.value {
            RedisValue::String { text } => stack_frame(|| {
                let viewer = self.ensure_string_viewer(text, window, cx);
                div()
                    .flex_1()
                    .min_h(px(220.))
                    .flex()
                    .flex_col()
                    .gap(px(pad))
                    .child(div().flex_1().min_h(px(180.)).child(viewer))
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .justify_end()
                            .child(redis_button("redis-string-save", Some(IconName::Check), &self.text("redis.string.save"), Kind::Primary, entity).small()),
                    )
                    .into_any_element()
            }),
            RedisValue::Hash { entries } => stack_frame(|| {
                let query = self.redis_input(Field::HashQuery, "redis-hash-filter", "redis.value.field", "", window, cx);
                let field = self.redis_input(Field::EntryField, "redis-hash-field", "redis.value.field", "field", window, cx);
                let entry_value = self.redis_input(Field::EntryValue, "redis-hash-value", "redis.value.value", "value", window, cx);
                let needle = self.hash_query.trim().to_ascii_lowercase();
                let mut rows_col = div().flex().flex_col().gap(px(2.));
                for (index, entry) in entries.iter().enumerate() {
                    if !needle.is_empty()
                        && !entry.field.to_ascii_lowercase().contains(&needle)
                        && !entry.value.to_ascii_lowercase().contains(&needle)
                    {
                        continue;
                    }
                    let focused = self.entry_focus == Some(index);
                    let inspect_cmd = format!("redis-entry-inspect:{index}");
                    let click_entity = entity.clone();
                    let menu_entity = entity.clone();
                    let copy_f_lbl = self.text("redis.copy.field");
                    let copy_v_lbl = self.text("redis.copy.keyValue");
                    let fmt_lbl = self.text("redis.menu.formatView");
                    let edit_lbl = self.text("redis.hash.add");
                    let del_lbl = self.text("redis.hash.delete");
                    rows_col = rows_col.child(
                        kit::interactive_states(div().id(SharedString::from(format!("redis-hash-row:{index}"))), p, false)
                            .test_support()
                            .px(px(8.))
                            .py(px(4.))
                            .rounded(px(Radius::CONTROL))
                            .when(focused, |s| s.bg(p.selection))
                            .when(!focused, |s| s.hover(|h| h.bg(p.hover)))
                            .flex()
                            .items_center()
                            .gap(px(pad))
                            .child(div().w(px(180.)).flex_none().truncate().font_weight(FontWeight::MEDIUM).text_size(px(TypeScale::SM)).child(SharedString::from(entry.field.clone())))
                            .child(div().flex_1().min_w(px(0.)).truncate().text_size(px(TypeScale::SM)).text_color(p.text_muted).child(SharedString::from(entry.value.clone())))
                            .child(row_button(format!("redis-hash-copy-field:{index}"), IconName::Tag, copy_f_lbl.clone(), entity))
                            .child(row_button(format!("redis-hash-copy:{index}"), IconName::Copy, self.text("common.copy"), entity))
                            .child(row_button(format!("redis-hash-edit:{index}"), IconName::Pencil, edit_lbl.clone(), entity))
                            .child(row_button(format!("redis-hash-del:{index}"), IconName::Trash, del_lbl.clone(), entity))
                            .on_mouse_down(MouseButton::Right, move |event: &MouseDownEvent, _, cx| {
                                cx.stop_propagation();
                                let pos = event.position;
                                let items = vec![
                                    ContextItem { id: format!("redis-entry-inspect:{index}"), icon: IconName::Eye, label: fmt_lbl.clone(), danger: false },
                                    ContextItem { id: format!("redis-hash-copy:{index}"), icon: IconName::Copy, label: copy_v_lbl.clone(), danger: false },
                                    ContextItem { id: format!("redis-hash-copy-field:{index}"), icon: IconName::Tag, label: copy_f_lbl.clone(), danger: false },
                                    ContextItem { id: format!("redis-hash-edit:{index}"), icon: IconName::Pencil, label: edit_lbl.clone(), danger: false },
                                    ContextItem { id: format!("redis-hash-del:{index}"), icon: IconName::Trash, label: del_lbl.clone(), danger: true },
                                ];
                                menu_entity.update(cx, |pane, cx| pane.open_entry_context_menu(items, pos, cx));
                            })
                            .on_click(move |_, _, cx| {
                                let cmd = inspect_cmd.clone();
                                click_entity.update(cx, |pane, cx| pane.command(&cmd, cx));
                            }),
                    );
                }
                let selected_entry = self
                    .entry_focus
                    .and_then(|ix| entries.get(ix).map(|e| (ix, e.field.clone(), e.value.clone())));
                let inspector = self.render_entry_inspector(selected_entry, p, window, cx);
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .flex_col()
                    .gap(px(pad))
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .flex_wrap()
                            .gap(px(pad))
                            .child(
                                div().w(px(200.)).child(
                                    kit::text_input("redis-hash-filter", &query, self.text("redis.tab.filter"))
                                        .small()
                                        .cleanable(true)
                                        .prefix(kit::input_prefix(IconName::Search, 13., p.text_faint)),
                                ),
                            )
                            .child(div().flex_1().min_w(px(120.)).child(kit::text_input("redis-hash-field", &field, self.text("redis.value.field")).small()))
                            .child(div().flex_1().min_w(px(140.)).child(kit::text_input("redis-hash-value", &entry_value, self.text("redis.value.value")).small()))
                            .child(redis_button("redis-hash-add", Some(IconName::Plus), &self.text("redis.hash.add"), Kind::Primary, entity).small()),
                    )
                    .child(div().id("redis-entries-scroll").flex_1().min_h(px(90.)).overflow_y_scroll().child(rows_col))
                    .child(inspector)
                    .into_any_element()
            }),
            RedisValue::List { items } => stack_frame(|| {
                let entry_value = self.redis_input(Field::EntryValue, "redis-list-value", "redis.value.value", "value", window, cx);
                let mut rows_col = div().flex().flex_col().gap(px(2.));
                for (index, item) in items.iter().enumerate() {
                    let focused = self.entry_focus == Some(index);
                    let inspect_cmd = format!("redis-entry-inspect:{index}");
                    let click_entity = entity.clone();
                    let menu_entity = entity.clone();
                    let fmt_lbl = self.text("redis.menu.formatView");
                    let copy_lbl = self.text("common.copy");
                    let set_lbl = self.text("redis.list.set");
                    let rem_lbl = self.text("redis.list.remove");
                    rows_col = rows_col.child(
                        kit::interactive_states(div().id(SharedString::from(format!("redis-list-row:{index}"))), p, false)
                            .test_support()
                            .px(px(8.))
                            .py(px(4.))
                            .rounded(px(Radius::CONTROL))
                            .when(focused, |s| s.bg(p.selection))
                            .when(!focused, |s| s.hover(|h| h.bg(p.hover)))
                            .flex()
                            .items_center()
                            .gap(px(pad))
                            .child(div().w(px(44.)).flex_none().text_size(px(TypeScale::XS)).text_color(p.text_muted).child(SharedString::from(format!("#{index}"))))
                            .child(div().flex_1().min_w(px(0.)).truncate().text_size(px(TypeScale::SM)).child(SharedString::from(item.clone())))
                            .child(row_button(format!("redis-list-copy:{index}"), IconName::Copy, copy_lbl.clone(), entity))
                            .child(row_button(format!("redis-list-set:{index}"), IconName::Pencil, set_lbl.clone(), entity))
                            .child(row_button(format!("redis-list-rem:{index}"), IconName::Trash, rem_lbl.clone(), entity))
                            .on_mouse_down(MouseButton::Right, move |event: &MouseDownEvent, _, cx| {
                                cx.stop_propagation();
                                let pos = event.position;
                                let items = vec![
                                    ContextItem { id: format!("redis-entry-inspect:{index}"), icon: IconName::Eye, label: fmt_lbl.clone(), danger: false },
                                    ContextItem { id: format!("redis-list-copy:{index}"), icon: IconName::Copy, label: copy_lbl.clone(), danger: false },
                                    ContextItem { id: format!("redis-list-set:{index}"), icon: IconName::Pencil, label: set_lbl.clone(), danger: false },
                                    ContextItem { id: format!("redis-list-rem:{index}"), icon: IconName::Trash, label: rem_lbl.clone(), danger: true },
                                ];
                                menu_entity.update(cx, |pane, cx| pane.open_entry_context_menu(items, pos, cx));
                            })
                            .on_click(move |_, _, cx| {
                                let cmd = inspect_cmd.clone();
                                click_entity.update(cx, |pane, cx| pane.command(&cmd, cx));
                            }),
                    );
                }
                let selected_entry = self
                    .entry_focus
                    .and_then(|ix| items.get(ix).map(|v| (ix, format!("#{ix}"), v.clone())));
                let inspector = self.render_entry_inspector(selected_entry, p, window, cx);
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .flex_col()
                    .gap(px(pad))
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .gap(px(pad))
                            .child(div().flex_1().child(kit::text_input("redis-list-value", &entry_value, self.text("redis.value.value")).small()))
                            .child(redis_button("redis-list-lpush", None, &self.text("redis.list.lpush"), Kind::Secondary, entity).small())
                            .child(redis_button("redis-list-rpush", None, &self.text("redis.list.rpush"), Kind::Primary, entity).small()),
                    )
                    .child(div().id("redis-entries-scroll").flex_1().min_h(px(90.)).overflow_y_scroll().child(rows_col))
                    .child(inspector)
                    .into_any_element()
            }),
            RedisValue::Set { items } => stack_frame(|| {
                let query = self.redis_input(Field::SetQuery, "redis-set-filter", "redis.value.member", "", window, cx);
                let member = self.redis_input(Field::EntryMember, "redis-set-member", "redis.value.member", "member", window, cx);
                let needle = self.set_query.trim().to_ascii_lowercase();
                let mut rows_col = div().flex().flex_col().gap(px(2.));
                for (index, item) in items.iter().enumerate() {
                    if !needle.is_empty() && !item.to_ascii_lowercase().contains(&needle) {
                        continue;
                    }
                    let focused = self.entry_focus == Some(index);
                    let inspect_cmd = format!("redis-entry-inspect:{index}");
                    let click_entity = entity.clone();
                    let menu_entity = entity.clone();
                    let fmt_lbl = self.text("redis.menu.formatView");
                    let copy_lbl = self.text("redis.copy.member");
                    let rem_lbl = self.text("redis.set.remove");
                    rows_col = rows_col.child(
                        kit::interactive_states(div().id(SharedString::from(format!("redis-set-row:{index}"))), p, false)
                            .test_support()
                            .px(px(8.))
                            .py(px(4.))
                            .rounded(px(Radius::CONTROL))
                            .when(focused, |s| s.bg(p.selection))
                            .when(!focused, |s| s.hover(|h| h.bg(p.hover)))
                            .flex()
                            .items_center()
                            .gap(px(pad))
                            .child(div().flex_1().min_w(px(0.)).truncate().text_size(px(TypeScale::SM)).child(SharedString::from(item.clone())))
                            .child(row_button(format!("redis-set-copy:{index}"), IconName::Copy, copy_lbl.clone(), entity))
                            .child(row_button(format!("redis-set-rem:{index}"), IconName::Trash, rem_lbl.clone(), entity))
                            .on_mouse_down(MouseButton::Right, move |event: &MouseDownEvent, _, cx| {
                                cx.stop_propagation();
                                let pos = event.position;
                                let items = vec![
                                    ContextItem { id: format!("redis-entry-inspect:{index}"), icon: IconName::Eye, label: fmt_lbl.clone(), danger: false },
                                    ContextItem { id: format!("redis-set-copy:{index}"), icon: IconName::Copy, label: copy_lbl.clone(), danger: false },
                                    ContextItem { id: format!("redis-set-rem:{index}"), icon: IconName::Trash, label: rem_lbl.clone(), danger: true },
                                ];
                                menu_entity.update(cx, |pane, cx| pane.open_entry_context_menu(items, pos, cx));
                            })
                            .on_click(move |_, _, cx| {
                                let cmd = inspect_cmd.clone();
                                click_entity.update(cx, |pane, cx| pane.command(&cmd, cx));
                            }),
                    );
                }
                let selected_entry = self
                    .entry_focus
                    .and_then(|ix| items.get(ix).map(|v| (ix, format!("member #{ix}"), v.clone())));
                let inspector = self.render_entry_inspector(selected_entry, p, window, cx);
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .flex_col()
                    .gap(px(pad))
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .gap(px(pad))
                            .child(
                                div().w(px(200.)).child(
                                    kit::text_input("redis-set-filter", &query, self.text("redis.tab.filter"))
                                        .small()
                                        .cleanable(true)
                                        .prefix(kit::input_prefix(IconName::Search, 13., p.text_faint)),
                                ),
                            )
                            .child(div().flex_1().child(kit::text_input("redis-set-member", &member, self.text("redis.value.member")).small()))
                            .child(redis_button("redis-set-add", Some(IconName::Plus), &self.text("redis.set.add"), Kind::Primary, entity).small()),
                    )
                    .child(div().id("redis-entries-scroll").flex_1().min_h(px(90.)).overflow_y_scroll().child(rows_col))
                    .child(inspector)
                    .into_any_element()
            }),
            RedisValue::ZSet { entries } => stack_frame(|| {
                let score = self.redis_input(Field::EntryScore, "redis-zset-score", "redis.value.score", "1", window, cx);
                let member = self.redis_input(Field::EntryMember, "redis-zset-member", "redis.value.member", "member", window, cx);
                let mut rows_col = div().flex().flex_col().gap(px(2.));
                for (index, entry) in entries.iter().enumerate() {
                    let focused = self.entry_focus == Some(index);
                    let inspect_cmd = format!("redis-entry-inspect:{index}");
                    let click_entity = entity.clone();
                    let menu_entity = entity.clone();
                    let fmt_lbl = self.text("redis.menu.formatView");
                    let copy_lbl = self.text("redis.copy.member");
                    let rem_lbl = self.text("redis.zset.remove");
                    rows_col = rows_col.child(
                        kit::interactive_states(div().id(SharedString::from(format!("redis-zset-row:{index}"))), p, false)
                            .test_support()
                            .px(px(8.))
                            .py(px(4.))
                            .rounded(px(Radius::CONTROL))
                            .when(focused, |s| s.bg(p.selection))
                            .when(!focused, |s| s.hover(|h| h.bg(p.hover)))
                            .flex()
                            .items_center()
                            .gap(px(pad))
                            .child(div().w(px(78.)).flex_none().text_size(px(TypeScale::SM)).text_color(p.accent_text).child(SharedString::from(entry.score.clone())))
                            .child(div().flex_1().min_w(px(0.)).truncate().text_size(px(TypeScale::SM)).child(SharedString::from(entry.member.clone())))
                            .child(row_button(format!("redis-zset-copy:{index}"), IconName::Copy, copy_lbl.clone(), entity))
                            .child(row_button(format!("redis-zrem:{index}"), IconName::Trash, rem_lbl.clone(), entity))
                            .on_mouse_down(MouseButton::Right, move |event: &MouseDownEvent, _, cx| {
                                cx.stop_propagation();
                                let pos = event.position;
                                let items = vec![
                                    ContextItem { id: format!("redis-entry-inspect:{index}"), icon: IconName::Eye, label: fmt_lbl.clone(), danger: false },
                                    ContextItem { id: format!("redis-zset-copy:{index}"), icon: IconName::Copy, label: copy_lbl.clone(), danger: false },
                                    ContextItem { id: format!("redis-zrem:{index}"), icon: IconName::Trash, label: rem_lbl.clone(), danger: true },
                                ];
                                menu_entity.update(cx, |pane, cx| pane.open_entry_context_menu(items, pos, cx));
                            })
                            .on_click(move |_, _, cx| {
                                let cmd = inspect_cmd.clone();
                                click_entity.update(cx, |pane, cx| pane.command(&cmd, cx));
                            }),
                    );
                }
                let selected_entry = self
                    .entry_focus
                    .and_then(|ix| entries.get(ix).map(|e| (ix, format!("score={}", e.score), e.member.clone())));
                let inspector = self.render_entry_inspector(selected_entry, p, window, cx);
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .flex_col()
                    .gap(px(pad))
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .gap(px(pad))
                            .child(div().w(px(96.)).child(kit::text_input("redis-zset-score", &score, self.text("redis.value.score")).small()))
                            .child(div().flex_1().child(kit::text_input("redis-zset-member", &member, self.text("redis.value.member")).small()))
                            .child(redis_button("redis-zadd", Some(IconName::Plus), &self.text("redis.zset.add"), Kind::Primary, entity).small()),
                    )
                    .child(div().id("redis-entries-scroll").flex_1().min_h(px(90.)).overflow_y_scroll().child(rows_col))
                    .child(inspector)
                    .into_any_element()
            }),
            RedisValue::Stream { messages } => stack_frame(|| {
                let id = self.redis_input(Field::StreamId, "redis-stream-id", "redis.value.field", "*", window, cx);
                let field = self.redis_input(Field::StreamField, "redis-stream-field", "redis.value.field", "field", window, cx);
                let entry_value = self.redis_input(Field::StreamValue, "redis-stream-value", "redis.value.value", "value", window, cx);
                let focus = self.stream_focus;
                let mut rows_col = div().flex().flex_col().gap(px(2.));
                for (index, message) in messages.iter().enumerate() {
                    let summary = message.fields.iter().map(|f| format!("{}={}", f.field, f.value)).collect::<Vec<_>>().join(" ");
                    let focused = self.entry_focus == Some(index);
                    let inspect_cmd = format!("redis-entry-inspect:{index}");
                    let click_entity = entity.clone();
                    let menu_entity = entity.clone();
                    let fmt_lbl = self.text("redis.menu.formatView");
                    let copy_v_lbl = self.text("redis.copy.keyValue");
                    let copy_id_lbl = self.text("redis.copy.id");
                    let del_lbl = self.text("redis.stream.delete");
                    let mut row = div()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .px(px(8.))
                        .py(px(4.))
                        .rounded(px(Radius::CONTROL))
                        .when(focused, |s| s.bg(p.selection))
                        .child(
                            kit::interactive_states(div().id(SharedString::from(format!("redis-stream-row:{index}"))), p, false)
                                .test_support()
                                .flex()
                                .items_center()
                                .gap(px(pad))
                                .child(div().flex_none().text_size(px(TypeScale::SM)).text_color(p.accent_text).child(SharedString::from(message.id.clone())))
                                .child(div().flex_1().min_w(px(0.)).truncate().text_size(px(TypeScale::SM)).text_color(p.text_muted).child(SharedString::from(summary)))
                                .child(row_button(format!("redis-stream-copy:{index}"), IconName::Copy, copy_v_lbl.clone(), entity))
                                .child(row_button(format!("redis-xopen:{index}"), IconName::Eye, self.text("mongo.tab.view"), entity))
                                .child(row_button(format!("redis-xdel:{index}"), IconName::Trash, del_lbl.clone(), entity))
                                .on_mouse_down(MouseButton::Right, move |event: &MouseDownEvent, _, cx| {
                                    cx.stop_propagation();
                                    let pos = event.position;
                                    let items = vec![
                                        ContextItem { id: format!("redis-entry-inspect:{index}"), icon: IconName::Eye, label: fmt_lbl.clone(), danger: false },
                                        ContextItem { id: format!("redis-stream-copy:{index}"), icon: IconName::Copy, label: copy_v_lbl.clone(), danger: false },
                                        ContextItem { id: format!("redis-stream-copy-id:{index}"), icon: IconName::Tag, label: copy_id_lbl.clone(), danger: false },
                                        ContextItem { id: format!("redis-xdel:{index}"), icon: IconName::Trash, label: del_lbl.clone(), danger: true },
                                    ];
                                    menu_entity.update(cx, |pane, cx| pane.open_entry_context_menu(items, pos, cx));
                                })
                                .on_click(move |_, _, cx| {
                                    let cmd = inspect_cmd.clone();
                                    click_entity.update(cx, |pane, cx| pane.command(&cmd, cx));
                                }),
                        );
                    if focus == Some(index) {
                        for field in &message.fields {
                            row = row.child(
                                div()
                                    .flex()
                                    .gap(px(pad))
                                    .pl(px(pad))
                                    .child(div().w(px(160.)).flex_none().truncate().text_size(px(TypeScale::XS)).child(SharedString::from(field.field.clone())))
                                    .child(div().flex_1().min_w(px(0.)).truncate().text_size(px(TypeScale::XS)).text_color(p.text_muted).child(SharedString::from(field.value.clone()))),
                            );
                        }
                    }
                    rows_col = rows_col.child(row);
                }
                let selected_entry = self.entry_focus.and_then(|ix| {
                    let msg = messages.get(ix)?;
                    let json = self.stream_payload_json(&ix.to_string())?;
                    Some((ix, msg.id.clone(), json))
                });
                let inspector = self.render_entry_inspector(selected_entry, p, window, cx);
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .flex_col()
                    .gap(px(pad))
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .gap(px(pad))
                            .child(div().w(px(110.)).child(kit::text_input("redis-stream-id", &id, self.text("redis.value.field")).small()))
                            .child(div().flex_1().child(kit::text_input("redis-stream-field", &field, self.text("redis.value.field")).small()))
                            .child(div().flex_1().child(kit::text_input("redis-stream-value", &entry_value, self.text("redis.value.value")).small()))
                            .child(redis_button("redis-xadd", Some(IconName::Plus), &self.text("redis.stream.add"), Kind::Primary, entity).small()),
                    )
                    .child(div().id("redis-entries-scroll").flex_1().min_h(px(90.)).overflow_y_scroll().child(rows_col))
                    .child(inspector)
                    .into_any_element()
            }),
            RedisValue::None {} => div().text_size(px(TypeScale::SM)).text_color(p.text_muted).child(SharedString::from(self.text("redis.value.none"))).into_any_element(),
        }
    }

    fn redis_input(&mut self, field: Field, _id: &'static str, label_key: &str, placeholder: &str, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        let value = match field {
            Field::Pattern => self.pattern.clone(),
            Field::Ttl => self.ttl_input.clone(),
            Field::Rename => self.rename_to.clone(),
            Field::Command => self.command_input.clone(),
            Field::NewKey => self.new_key.clone(),
            Field::NewTtl => self.new_ttl.clone(),
            Field::NewField => self.new_field.clone(),
            Field::NewValue => self.new_value.clone(),
            Field::NewScore => self.new_score.clone(),
            Field::EntryField => self.entry_field.clone(),
            Field::EntryValue => self.entry_value.clone(),
            Field::EntryMember => self.entry_member.clone(),
            Field::EntryScore => self.entry_score.clone(),
            Field::EntryIndex => self.entry_index.clone(),
            Field::StreamId => self.stream_id.clone(),
            Field::StreamField => self.stream_field.clone(),
            Field::StreamValue => self.stream_value.clone(),
            Field::HashQuery => self.hash_query.clone(),
            Field::SetQuery => self.set_query.clone(),
        };
        let _ = label_key;
        self.fields.bind_enter(
            field,
            &value,
            FieldSpec { placeholder, masked: false },
            window,
            cx,
            |this: &mut Self, field, value, _| {
                if this.confirm.is_some() && matches!(field, Field::Rename | Field::Command | Field::Ttl) {
                    return;
                }
                this.focused = Some(field);
                match field {
                    Field::Pattern => this.pattern = value,
                    Field::Ttl => this.ttl_input = value,
                    Field::Rename => this.rename_to = value,
                    Field::Command => {
                        this.complete_open = !value.trim().is_empty();
                        this.complete_index = 0;
                        this.command_input = value;
                        this.history_at = None;
                    }
                    Field::NewKey => this.new_key = value,
                    Field::NewTtl => this.new_ttl = value,
                    Field::NewField => this.new_field = value,
                    Field::NewValue => this.new_value = value,
                    Field::NewScore => this.new_score = value,
                    Field::EntryField => this.entry_field = value,
                    Field::EntryValue => this.entry_value = value,
                    Field::EntryMember => this.entry_member = value,
                    Field::EntryScore => this.entry_score = value,
                    Field::EntryIndex => this.entry_index = value,
                    Field::StreamId => this.stream_id = value,
                    Field::StreamField => this.stream_field = value,
                    Field::StreamValue => this.stream_value = value,
                    Field::HashQuery => this.hash_query = value,
                    Field::SetQuery => this.set_query = value,
                }
            },
            Some(|this: &mut Self, field, cx| {
                let id = match field {
                    Field::Pattern => "redis-filter",
                    Field::Ttl => "redis-ttl",
                    Field::Rename => "redis-rename",
                    Field::Command => "redis-exec",
                    Field::NewKey | Field::NewTtl | Field::NewField | Field::NewValue | Field::NewScore => "redis-create",
                    Field::EntryField | Field::EntryValue => {
                        if this.detail.as_ref().is_some_and(|detail| detail.key_type == "list") {
                            "redis-list-rpush"
                        } else {
                            "redis-hash-add"
                        }
                    }
                    Field::EntryMember | Field::EntryScore => {
                        if this.detail.as_ref().is_some_and(|detail| detail.key_type == "set") {
                            "redis-set-add"
                        } else {
                            "redis-zadd"
                        }
                    }
                    Field::EntryIndex => "redis-list-rpush",
                    Field::StreamId | Field::StreamField | Field::StreamValue => "redis-xadd",
                    Field::HashQuery | Field::SetQuery => "redis-entry-filter",
                };
                this.command(id, cx);
            }),
        )
    }
}

fn type_colors(kind: &str, p: &Palette) -> (gpui_kit::Hsla, gpui_kit::Hsla) {
    match kind {
        "string" => (p.info, p.accent_soft),
        "hash" => (p.ok, p.ok_soft),
        "list" => (p.warn, p.warn_soft),
        "set" => (p.accent_text, p.accent_soft),
        "zset" => (p.danger, p.danger_soft),
        _ => (p.text_muted, p.hover),
    }
}

fn row_button(id: String, icon: IconName, label: String, entity: &Entity<RedisPane>) -> kit::IconButton {
    let entity = entity.clone();
    let command = id.clone();
    kit::icon_button(SharedString::from(id), icon, SharedString::from(label), move |_, _, cx| {
        let command = command.clone();
        entity.update(cx, |pane, cx| pane.command(&command, cx));
    })
    .xsmall()
}

fn redis_button(id: &'static str, icon: Option<IconName>, label: &str, kind: Kind, entity: &Entity<RedisPane>) -> kit::ActionButton {
    let entity = entity.clone();
    let click = move |_: &ClickEvent, _: &mut Window, cx: &mut gpui_kit::App| entity.update(cx, |pane, cx| pane.command(id, cx));
    let label = SharedString::from(label.to_owned());
    match kind {
        Kind::Primary => kit::primary_button(id, icon, label, click),
        Kind::Secondary => kit::secondary_button(id, icon, label, click),
        Kind::Tool => kit::tool_button(id, icon, label, click),
        Kind::Danger => kit::danger_button(id, icon, label, click),
    }
}

fn redis_icon(id: &'static str, icon: IconName, label: &str, entity: &Entity<RedisPane>) -> kit::IconButton {
    let entity = entity.clone();
    kit::icon_button(id, icon, SharedString::from(label.to_owned()), move |_, _, cx| entity.update(cx, |pane, cx| pane.command(id, cx))).small()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_parsing_ttl_and_keys_merge_stay_local() {
        assert_eq!(split_commandline("SET key \"hello world\""), ("SET".into(), vec!["key".into(), "hello world".into()]));
        assert_eq!(split_commandline("CONFIG SET save 60"), ("CONFIG SET".into(), vec!["save".into(), "60".into()]));
        assert_eq!(split_commandline("ACL SETUSER default"), ("ACL SETUSER".into(), vec!["default".into()]));
        assert_eq!(parse_ttl("").unwrap(), None);
        assert_eq!(parse_ttl("15").unwrap(), Some(15));
        assert!(parse_ttl("-1").is_err());
        assert!(parse_ttl("1.5").is_err());
        let merged = merge_keys(
            vec![RedisKeyInfo { name: "a".into(), key_type: "string".into(), ttl_ms: Some(-1) }],
            vec![
                RedisKeyInfo { name: "a".into(), key_type: "hash".into(), ttl_ms: Some(10) },
                RedisKeyInfo { name: "b".into(), key_type: "list".into(), ttl_ms: None },
            ],
        );
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].key_type, "hash");
        assert_eq!(parse_spec("cache|缓存|0").unwrap().0, "cache");
        assert!(parse_spec("|缓存|0").is_none());
        let prompt = danger_prompt(Locale::ZhCn, "flushall");
        assert!(prompt.contains("FLUSHALL"), "{prompt}");
        assert_ne!(i18n::text(Locale::En, "redis.tab.keysRefused"), "redis.tab.keysRefused");
    }

    #[test]
    fn tree_mode_interleaves_folders_and_leaf_keys_without_duplicating_flat_list() {
        let dir = std::env::temp_dir().join(format!("rillsql-redis-tree-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let services = Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
        let mut pane = RedisPane::new(services);
        pane.keys = vec![
            RedisKeyInfo { name: "user:1001".into(), key_type: "hash".into(), ttl_ms: None },
            RedisKeyInfo { name: "user:1002".into(), key_type: "hash".into(), ttl_ms: None },
            RedisKeyInfo { name: "global_flag".into(), key_type: "string".into(), ttl_ms: None },
        ];
        pane.tree_view = false;
        let flat = pane.build_key_rows();
        assert_eq!(flat.len(), 3);
        assert!(flat.iter().all(|row| matches!(row, KeyRow::Key { depth: 0, .. })));

        pane.tree_view = true;
        // 未展开 user 文件夹时：只出现 global_flag 与 user 文件夹（count=2），两个子键收起隐藏。
        let collapsed = pane.build_key_rows();
        assert_eq!(collapsed.len(), 2, "{collapsed:?}");
        assert!(collapsed.iter().any(|r| matches!(r, KeyRow::Folder { prefix, count: 2, open: false, .. } if prefix == "user")));

        // 展开 user 文件夹后：user 文件夹正下方紧跟缩进后的叶子名 1001、1002。
        pane.expanded.insert("user".into());
        let expanded = pane.build_key_rows();
        assert_eq!(expanded.len(), 4, "{expanded:?}");
        assert!(expanded.iter().any(|r| matches!(r, KeyRow::Key { display, depth: 1, .. } if display == "1001")));
        assert!(expanded.iter().any(|r| matches!(r, KeyRow::Key { display, depth: 1, .. } if display == "1002")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn large_key_pages_move_existing_storage_and_replace_duplicates_in_order() {
        let keys: Vec<_> = (0..10_000)
            .map(|index| RedisKeyInfo { name: format!("key-{index}"), key_type: "string".into(), ttl_ms: None })
            .collect();
        let first_name = keys[0].name.as_ptr();
        let merged = merge_keys(
            keys,
            vec![
                RedisKeyInfo { name: "key-9999".into(), key_type: "hash".into(), ttl_ms: Some(10) },
                RedisKeyInfo { name: "new".into(), key_type: "list".into(), ttl_ms: None },
                RedisKeyInfo { name: "new".into(), key_type: "set".into(), ttl_ms: Some(20) },
            ],
        );
        assert_eq!(merged.len(), 10_001);
        assert_eq!(merged[0].name.as_ptr(), first_name, "旧键字符串没有复制");
        assert_eq!(merged[9999].key_type, "hash");
        assert_eq!(merged[10_000].key_type, "set");
    }

    #[gpui_kit::test]
    fn switching_redis_target_clears_key_command_and_confirmation(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::AppContext as _;
        let dir = std::env::temp_dir().join(format!("rillsql-redis-reset-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let services = std::sync::Arc::new(crate::application::AppServices::new(std::sync::Arc::new(
            crate::connections::Service::new_for_test(dir.join("connections.sqlite")),
        )));
        let pane = cx.update(|cx| cx.new(|_| RedisPane::new(services)));
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                pane.plant_key("old-key");
                pane.set_command("DEL old-key");
                pane.set_ttl("60");
                pane.set_rename("renamed");
                pane.command("redis-delete", cx);
                assert!(pane.confirm_open());
                pane.reset_target_state();
                pane.connection_id = "cache".into();
                pane.connection_name = "缓存".into();
                pane.database = "0".into();
            });
        });
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert!(!pane.confirm_open(), "换 Redis 目标不能保留旧删除确认");
            assert!(pane.selected().is_empty());
            assert!(pane.command_input().is_empty());
            assert!(pane.ttl_input().is_empty());
            assert!(pane.rename_to().is_empty());
            assert!(pane.reply().is_empty());
            assert!(!pane.status_line().contains("old-key"), "{}", pane.status_line());
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[gpui_kit::test]
    fn config_set_ttl_score_and_history_stay_local(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::AppContext as _;
        let dir = std::env::temp_dir().join(format!("rillsql-redis-gate-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let services = std::sync::Arc::new(crate::application::AppServices::new(std::sync::Arc::new(
            crate::connections::Service::new_for_test(dir.join("connections.sqlite")),
        )));
        let pane = cx.update(|cx| cx.new(|_| RedisPane::new(services)));
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                pane.plant_key("kept");
                pane.set_command("CONFIG SET save 60");
                pane.command("redis-exec", cx);
                assert!(pane.confirm_open(), "{}", pane.status_line());
                assert!(pane.confirm_prompt().to_ascii_uppercase().contains("CONFIG"), "{}", pane.confirm_prompt());
                pane.command("redis-cancel", cx);
                assert!(!pane.confirm_open());
                assert_eq!(pane.command_input(), "CONFIG SET save 60");
                pane.set_ttl("30");
                pane.command("redis-ttl", cx);
                assert!(pane.confirm_open(), "TTL 必须先确认");
                assert_eq!(pane.ttl_input(), "30");
                pane.command("redis-cancel", cx);
                assert_eq!(pane.ttl_input(), "30");
                pane.detail = Some(Arc::new(RedisKeyDetail {
                    key_type: "zset".into(),
                    ttl_ms: Some(-1),
                    value: RedisValue::ZSet { entries: Vec::new() },
                }));
                pane.entry_score = "nope".into();
                pane.entry_member = "张伟".into();
                pane.command("redis-zadd", cx);
                assert!(!pane.confirm_open(), "非法分值不能进入确认");
                assert_eq!(pane.entry_score, "nope");
                assert!(pane.error().contains("分值") || pane.error().to_ascii_lowercase().contains("score"), "{}", pane.error());
                pane.history = vec!["GET a".into(), "GET b".into()];
                pane.command_input = "PING".into();
                pane.history_step(true);
                assert_eq!(pane.command_input(), "GET b");
                pane.history_step(true);
                assert_eq!(pane.command_input(), "GET a");
                pane.history_step(false);
                assert_eq!(pane.command_input(), "GET b");
                pane.history_step(false);
                assert_eq!(pane.command_input(), "PING");
                assert!(pane.console_open);
                pane.command("redis-console", cx);
                assert!(!pane.console_open);
            });
        });
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod live;

#[cfg(test)]
mod states;
