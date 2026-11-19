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
use crate::adapters::redis::complete::{self, RedisCompletion};
use crate::adapters::redis::exec::{self, CommandRisk, RespReply};
use crate::adapters::redis::keys::{self, RedisInitialValue, RedisKeyDetail, RedisKeyInfo, RedisMutation, RedisValue, ScanPage};

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

/// 结构化查看器里可以直接写回的条目：Hash 字段值、List 指定下标的元素。其余条目只读。
#[derive(Clone, Debug, PartialEq, Eq)]
enum EntryTarget {
    Hash(String),
    List(i64),
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
    /// 集合条目的添加 / 编辑表单。平时收起，不占值区高度。
    entry_form_open: bool,
    entry_target: Option<EntryTarget>,
    entry_viewer_sub: Option<Subscription>,
    /// 下一帧要聚焦的输入框（打开 TTL / 重命名 / 新建键 / 条目表单时把光标放进去）。
    focus_request: Option<Field>,
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
            entry_form_open: false,
            entry_target: None,
            entry_viewer_sub: None,
            focus_request: None,
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
        self.entry_form_open = false;
        self.entry_target = None;
        self.entry_viewer_sub = None;
        self.focus_request = None;
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

    /// 复制一段值。空字符串也是值（R20）：剪贴板得到空文本，提示说清复制的是空字符串。
    fn copy_text(&mut self, text: String, note_key: &str, cx: &mut Context<Self>) {
        if text.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(String::new()));
            self.copied = String::new();
            self.note = self.text("redis.copied.empty");
            self.error.clear();
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
                | "redis-entry-form"
                | "redis-entry-form-close"
                | "redis-new-close"
                | "redis-db-close"
        ) || id.starts_with("redis-type-")
            || id.starts_with("redis-hash-edit:")
            || id.starts_with("redis-zset-edit:")
            || id.starts_with("redis-list-edit:")
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
            if self.ttl_open {
                self.focus_request = Some(Field::Ttl);
            }
        } else if id == "redis-ttl-close" {
            self.ttl_open = false;
        } else if id == "redis-ttl" {
            // 头部按钮只负责打开编辑条；编辑条里的同名按钮才提交，避免误点就把剩余秒数原样写回去。
            if !self.ttl_open {
                self.ttl_open = true;
                self.rename_open = false;
                self.focus_request = Some(Field::Ttl);
            } else {
                self.save_ttl(cx);
            }
        } else if let Some(preset) = id.strip_prefix("redis-ttl-preset:") {
            self.ttl_input = if preset == "persist" { String::new() } else { preset.to_owned() };
            self.ttl_open = true;
            self.save_ttl(cx);
        } else if id == "redis-rename-toggle" {
            self.rename_open = !self.rename_open;
            self.ttl_open = false;
            if self.rename_open {
                if self.rename_to.is_empty() {
                    self.rename_to = self.selected.clone();
                }
                self.focus_request = Some(Field::Rename);
            }
        } else if id == "redis-rename-close" {
            self.rename_open = false;
        } else if id == "redis-rename" {
            if !self.rename_open {
                self.rename_open = true;
                self.ttl_open = false;
                if self.rename_to.trim().is_empty() {
                    self.rename_to = self.selected.clone();
                }
                self.focus_request = Some(Field::Rename);
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
        } else if id == "redis-entry-form" {
            self.entry_form_open = !self.entry_form_open;
            if self.entry_form_open {
                // 从「添加」进来是新条目：清掉上一次编辑留下的下标，提交时不会误写回旧位置。
                self.clear_entry_drafts();
                self.focus_request = Some(self.entry_first_field());
            }
        } else if id == "redis-entry-form-close" {
            self.entry_form_open = false;
            self.clear_entry_drafts();
        } else if id == "redis-new-close" {
            self.new_open = false;
            self.new_overwrite = false;
            if self.error_source == ErrorSource::Command {
                self.error.clear();
            }
        } else if id == "redis-db-close" {
            self.db_picker_open = false;
        } else if let Some(index) = id.strip_prefix("redis-list-edit:") {
            if let Some(val) = self.list_item(index) {
                if let Ok(ix) = index.parse::<usize>() {
                    self.entry_value = val;
                    self.entry_index = ix.to_string();
                    self.entry_focus = Some(ix);
                    self.entry_form_open = true;
                    self.focus_request = Some(Field::EntryValue);
                }
            }
        } else if let Some(index) = id.strip_prefix("redis-zset-edit:") {
            if let Ok(ix) = index.parse::<usize>() {
                if let Some(RedisKeyDetail { value: RedisValue::ZSet { entries }, .. }) = self.detail.as_deref() {
                    if let Some(entry) = entries.get(ix) {
                        self.entry_member = entry.member.clone();
                        self.entry_score = entry.score.clone();
                        self.entry_focus = Some(ix);
                        self.entry_form_open = true;
                        self.focus_request = Some(Field::EntryScore);
                    }
                }
            }
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
            self.focus_request = Some(Field::NewKey);
        } else if id == "redis-history-older" {
            self.complete_open = false;
            self.history_step(true);
        } else if id == "redis-history-newer" {
            self.complete_open = false;
            self.history_step(false);
        } else if id == "redis-new-key" || id == "redis-new-key-value" {
            self.nav_choice = Some(true);
            self.new_open = !self.new_open;
            self.db_picker_open = false;
            if self.new_open {
                self.focus_request = Some(Field::NewKey);
            }
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
            // 表单开着时这是“写回”：清空后提交就是写入空字符串（R20），不能当成“重新载入原值”。
            if list_set_fills(self.entry_form_open, &self.entry_value) {
                if let Some(val) = self.list_item(index) {
                    self.entry_value = val;
                    if let Ok(ix) = index.parse::<usize>() {
                        self.entry_focus = Some(ix);
                        self.entry_index = ix.to_string();
                    }
                    self.entry_form_open = true;
                    self.focus_request = Some(Field::EntryValue);
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
        if self.selected != name {
            // 换键时收起条目表单、清掉条目筛选：上一个键的草稿不能被误写到新键上。
            self.entry_form_open = false;
            self.clear_entry_drafts();
            self.hash_query.clear();
            self.set_query.clear();
        }
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
                self.entry_form_open = false;
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
        let Some(raw) = self.detail.as_ref().and_then(|detail| value_copy_text(&detail.value, self.locale)) else {
            self.show_error(self.text("redis.tab.pickKey"));
            return;
        };
        if raw.is_empty() {
            self.copy_text(raw, "redis.copied.value", cx);
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
                self.entry_form_open = true;
                self.focus_request = Some(Field::EntryValue);
            }
        }
    }

    /// 条目表单打开时光标先落在哪个输入框。
    fn entry_first_field(&self) -> Field {
        match self.detail.as_deref().map(|detail| &detail.value) {
            Some(RedisValue::Hash { .. }) => Field::EntryField,
            Some(RedisValue::Set { .. }) => Field::EntryMember,
            Some(RedisValue::ZSet { .. }) => Field::EntryScore,
            Some(RedisValue::Stream { .. }) => Field::StreamField,
            _ => Field::EntryValue,
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

/// 值区域里一格要怎么画（R20）：空字符串画成弱化的“空”提示，不当成普通数据，也不和“没有内容”混淆。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ValueCell {
    Empty,
    Text(String),
}

pub(crate) fn value_cell(text: &str) -> ValueCell {
    if text.is_empty() { ValueCell::Empty } else { ValueCell::Text(one_line(text)) }
}

/// “复制值”得到的文本：字符串原样（空字符串就是空文本，不是“这个键没有内容”这句说明）；
/// 集合按行展开；键不存在时没有可复制的值。
pub(crate) fn value_copy_text(value: &RedisValue, locale: Locale) -> Option<String> {
    match value {
        RedisValue::None {} => None,
        RedisValue::String { text } => Some(text.clone()),
        other => Some(format_value(other, locale)),
    }
}

/// 列表条目的“设置”按钮：表单没打开时先载入原值；表单打开时就是提交，空文本也照样写回。
pub(crate) fn list_set_fills(form_open: bool, draft: &str) -> bool {
    !form_open && draft.trim().is_empty()
}

fn value_empty(value: &RedisValue) -> bool {
    match value {
        // 空字符串是一个值，不是“没有内容”。
        RedisValue::String { .. } => false,
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
        Some(ms) if ms >= 0 => i18n::fill(&i18n::text(locale, "redis.ttl.left"), &[("t", &long_ttl(ms))]),
        Some(_) => "—".to_owned(),
    }
}

/// 键列表行尾的紧凑剩余时间：`45s` / `12m` / `3h` / `2d`。
pub(crate) fn short_ttl(ms: i64) -> String {
    let secs = ms / 1000;
    if ms < 1000 {
        format!("{}ms", ms.max(0))
    } else if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

/// 详情头部的剩余时间：取两档单位，`1h 23m`、`5m 08s`、`42s`。
pub(crate) fn long_ttl(ms: i64) -> String {
    if ms < 1000 {
        return format!("{}ms", ms.max(0));
    }
    let secs = ms / 1000;
    let (d, h, m, s) = (secs / 86_400, secs % 86_400 / 3600, secs % 3600 / 60, secs % 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m:02}m")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}

/// 宽度不足时把键名从中间省略：开头的前缀和结尾的 ID 都留着。
/// Redis 键名的区分信息往往在末尾（`order:209831:1269…`），只在尾部截断会让一屏行长得一样。
/// 宽度按等宽单位计：ASCII 算 1，其余（中文等）算 2。
pub(crate) fn middle_ellipsis(text: &str, max_units: usize) -> String {
    fn units(ch: char) -> usize {
        if ch.is_ascii() { 1 } else { 2 }
    }
    let total: usize = text.chars().map(units).sum();
    if total <= max_units || max_units < 8 {
        return text.to_owned();
    }
    let budget = max_units - 1;
    let head_budget = budget * 2 / 5;
    let tail_budget = budget - head_budget;
    let mut head = String::new();
    let mut used = 0;
    for ch in text.chars() {
        if used + units(ch) > head_budget {
            break;
        }
        used += units(ch);
        head.push(ch);
    }
    let mut tail: Vec<char> = Vec::new();
    used = 0;
    for ch in text.chars().rev() {
        if used + units(ch) > tail_budget {
            break;
        }
        used += units(ch);
        tail.push(ch);
    }
    tail.reverse();
    format!("{head}…{}", tail.into_iter().collect::<String>())
}

/// 列表单元格里的一行预览：换行、制表符折成空格，超长值只取开头，完整内容留给结构化查看器。
pub(crate) fn one_line(text: &str) -> String {
    const MAX: usize = 240;
    let mut out = String::with_capacity(text.len().min(MAX + 1));
    for (count, ch) in text.chars().enumerate() {
        if count >= MAX {
            out.push('…');
            break;
        }
        out.push(if matches!(ch, '\n' | '\r' | '\t') { ' ' } else { ch });
    }
    out
}

mod view;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_parsing_ttl_and_keys_merge_stay_local() {
        assert_eq!(split_commandline("SET key \"hello world\""), ("SET".into(), vec!["key".into(), "hello world".into()]));
        assert_eq!(split_commandline("CONFIG SET save 60"), ("CONFIG SET".into(), vec!["save".into(), "60".into()]));
        assert_eq!(split_commandline("ACL SETUSER default"), ("ACL SETUSER".into(), vec!["default".into()]));
        assert_eq!(short_ttl(45_000), "45s");
        assert_eq!(short_ttl(3_600_000), "1h");
        assert_eq!(long_ttl(5_000_000), "1h 23m");
        assert_eq!(long_ttl(308_000), "5m 08s");
        let name = "lastCustomArea209831:1269820826354053120";
        let short = middle_ellipsis(name, 24);
        assert!(short.starts_with("lastCust") && short.ends_with("4053120") && short.contains('…'), "{short}");
        assert!(short.chars().count() <= 24, "{short}");
        assert_eq!(middle_ellipsis("user:1", 24), "user:1");
        assert_eq!(one_line("a\nb\tc"), "a b c");
    }

    /// R20：Redis 值查看与网格同一套规则——空字符串单独分类、复制得到空文本、清空提交就是写入空字符串。
    #[test]
    fn empty_strings_are_classified_copied_and_written_as_values() {
        assert_eq!(value_cell(""), ValueCell::Empty);
        assert_eq!(value_cell("a\nb"), ValueCell::Text("a b".to_owned()));
        assert_eq!(value_cell(" "), ValueCell::Text(" ".to_owned()), "空白不是空字符串");
        let empty = RedisValue::String { text: String::new() };
        assert_eq!(value_copy_text(&empty, Locale::ZhCn).as_deref(), Some(""), "复制空字符串得到空文本");
        assert_eq!(format_value(&empty, Locale::ZhCn), "", "不再把空字符串说成“没有内容”");
        assert_eq!(value_copy_text(&RedisValue::None {}, Locale::ZhCn), None);
        assert_eq!(value_copy_text(&RedisValue::List { items: vec!["a".into(), String::new()] }, Locale::ZhCn).as_deref(), Some("a\n"));
        // 列表“设置”：表单没开时载入原值；表单开着时空文本也直接提交。
        assert!(list_set_fills(false, ""));
        assert!(!list_set_fills(true, ""));
        assert!(!list_set_fills(false, "x"));
        let plan = crate::adapters::redis::keys::prepare_mutation(&RedisMutation::ListSet { key: "k".into(), index: 0, value: String::new() }, true).expect("空字符串可以写入");
        assert_eq!(plan.steps[0].word, "LSET");
        assert_eq!(plan.steps[0].args, vec!["k".to_owned(), "0".to_owned(), String::new()]);
        let plan = crate::adapters::redis::keys::prepare_mutation(
            &RedisMutation::HashSet { key: "h".into(), field: "f".into(), value: String::new() },
            true,
        )
        .expect("哈希值可以是空字符串");
        assert_eq!(plan.steps[0].args.last().map(String::as_str), Some(""));
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
mod live_tests;

#[cfg(test)]
mod states;
