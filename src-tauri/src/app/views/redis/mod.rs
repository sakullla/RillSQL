//! Redis 键浏览器。浏览只走 SCAN；写删和危险命令先在界面确认，失败时草稿还在。

use std::sync::Arc;

use gpui_kit::{
    AppContext as _, ClipboardItem, Context, Entity, FocusHandle, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div, px,
};

use crate::application::redis::{DeleteOutcome, ExecOutcome, RenameOutcome, TtlOutcome};
use crate::application::AppServices;
use crate::i18n::{self, Locale};
use crate::redis::exec::{self, CommandRisk, RespReply};
use crate::redis::keys::{RedisKeyDetail, RedisKeyInfo, RedisValue, ScanPage};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Pattern,
    Ttl,
    Rename,
    Command,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Confirm {
    Delete,
    Rename,
    Exec,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Follow {
    None,
    Read,
}

pub struct RedisPane {
    services: Arc<AppServices>,
    locale: Locale,
    connection_id: String,
    connection_name: String,
    database: String,
    pattern: String,
    keys: Vec<RedisKeyInfo>,
    cursor: u64,
    done: bool,
    selected: String,
    detail: Option<RedisKeyDetail>,
    ttl_input: String,
    rename_to: String,
    command_input: String,
    reply: String,
    note: String,
    error: String,
    confirm: Option<Confirm>,
    confirm_prompt: String,
    busy: bool,
    generation: u64,
    copied: String,
    scan_reset: bool,
    follow: Follow,
    pending_ttl: Option<i64>,
    ttl_was_persist: bool,
    focused: Option<Field>,
    pattern_focus: Option<FocusHandle>,
    ttl_focus: Option<FocusHandle>,
    rename_focus: Option<FocusHandle>,
    command_focus: Option<FocusHandle>,
}

impl RedisPane {
    pub fn new(services: Arc<AppServices>) -> Self {
        Self {
            services,
            locale: Locale::ZhCn,
            connection_id: String::new(),
            connection_name: String::new(),
            database: "0".into(),
            pattern: String::new(),
            keys: Vec::new(),
            cursor: 0,
            done: false,
            selected: String::new(),
            detail: None,
            ttl_input: String::new(),
            rename_to: String::new(),
            command_input: String::new(),
            reply: String::new(),
            note: String::new(),
            error: String::new(),
            confirm: None,
            confirm_prompt: String::new(),
            busy: false,
            generation: 0,
            copied: String::new(),
            scan_reset: true,
            follow: Follow::None,
            pending_ttl: None,
            ttl_was_persist: false,
            focused: None,
            pattern_focus: None,
            ttl_focus: None,
            rename_focus: None,
            command_focus: None,
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
    }

    pub fn set_rename(&mut self, value: &str) {
        self.rename_to = value.to_owned();
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
        self.detail = Some(RedisKeyDetail {
            key_type: "string".into(),
            ttl_ms: Some(-1),
            value: RedisValue::String { text: "hello".into() },
        });
    }

    pub fn copied(&self) -> &str {
        &self.copied
    }

    pub fn confirm_open(&self) -> bool {
        self.confirm.is_some()
    }

    pub fn detail_text(&self) -> String {
        self.detail.as_ref().map(|detail| format!("{} {}", detail.key_type, format_value(&detail.value, self.locale))).unwrap_or_default()
    }

    pub fn open_spec(&mut self, spec: &str, cx: &mut Context<Self>) {
        let Some((id, name, database)) = parse_spec(spec) else {
            self.connection_id.clear();
            self.error = self.text("redis.tab.needConnection");
            self.note.clear();
            cx.notify();
            return;
        };
        self.connection_id = id;
        self.connection_name = name;
        self.database = database;
        self.keys.clear();
        self.cursor = 0;
        self.done = false;
        self.selected.clear();
        self.detail = None;
        self.confirm = None;
        self.note.clear();
        self.error.clear();
        self.scan_from_start(cx);
    }

    pub fn command(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.busy && !matches!(id, "redis-cancel") {
            self.error = self.text("redis.tab.busy");
            self.note.clear();
            cx.notify();
            return;
        }
        if id == "redis-filter" || id == "redis-refresh" {
            self.scan_from_start(cx);
        } else if id == "redis-more" {
            if !self.done {
                self.scan_reset = false;
                self.follow = Follow::None;
                self.load_keys(cx);
            }
        } else if let Some(index) = id.strip_prefix("redis-select:") {
            if let Ok(index) = index.parse::<usize>() {
                if let Some(key) = self.keys.get(index) {
                    let name = key.name.clone();
                    self.select_key(name, cx);
                }
            }
        } else if id == "redis-ttl" {
            self.save_ttl(cx);
        } else if id == "redis-rename" {
            self.ask_rename();
        } else if id == "redis-delete" {
            self.ask_delete();
        } else if id == "redis-exec" {
            self.ask_exec(cx);
        } else if id == "redis-confirm" {
            self.run_confirm(cx);
        } else if id == "redis-cancel" {
            self.confirm = None;
            self.confirm_prompt.clear();
        } else if id == "redis-copy" {
            self.copy_value(cx);
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
            self.error = self.text("redis.tab.needConnection");
            self.note.clear();
            return;
        }
        let connections = Arc::clone(&self.services.connections);
        let connection_id = self.connection_id.clone();
        let cursor = if self.scan_reset { 0 } else { self.cursor };
        let pattern = {
            let trimmed = self.pattern.trim();
            if trimmed.is_empty() { None } else { Some(trimmed.to_owned()) }
        };
        self.spawn(cx, false, async move { crate::application::redis::scan_keys(connections.as_ref(), &connection_id, Some(cursor), pattern, None).await }, Self::finish_scan);
    }

    fn finish_scan(&mut self, result: Result<ScanPage, String>, cx: &mut Context<Self>) {
        match result {
            Ok(page) => {
                let base = if self.scan_reset { Vec::new() } else { self.keys.clone() };
                self.keys = merge_keys(&base, &page.keys);
                self.cursor = page.cursor;
                self.done = page.done;
                self.error.clear();
                if self.follow == Follow::Read && !self.selected.is_empty() {
                    let name = self.selected.clone();
                    self.select_key(name, cx);
                }
            }
            Err(message) => self.show_error(message),
        }
    }

    fn select_key(&mut self, name: String, cx: &mut Context<Self>) {
        self.selected = name.clone();
        self.detail = None;
        if self.connection_id.is_empty() {
            self.show_error(self.text("redis.tab.needConnection"));
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
                self.detail = Some(detail);
                self.error.clear();
            }
            Err(message) => self.show_error(message),
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
        let connections = Arc::clone(&self.services.connections);
        let connection_id = self.connection_id.clone();
        let key = self.selected.clone();
        self.spawn(
            cx,
            false,
            async move { crate::application::redis::set_ttl(connections.as_ref(), &connection_id, key, seconds).await },
            Self::finish_ttl,
        );
    }

    fn finish_ttl(&mut self, result: Result<TtlOutcome, String>, cx: &mut Context<Self>) {
        match result {
            Ok(outcome) if outcome.applied == 0 => self.show_error(self.text("redis.tab.ttlMissed")),
            Ok(_) => {
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
        self.confirm = Some(Confirm::Delete);
        self.confirm_prompt = i18n::fill(&self.text("redis.tab.deletePrompt"), &[("key", &self.selected)]);
        self.note.clear();
        self.error.clear();
    }

    fn ask_rename(&mut self) {
        if self.selected.is_empty() {
            self.show_error(self.text("redis.tab.needKey"));
            return;
        }
        let target = self.rename_to.trim().to_owned();
        if target.is_empty() {
            self.show_error(self.text("redis.tab.renameNeedName"));
            return;
        }
        self.confirm = Some(Confirm::Rename);
        self.confirm_prompt = i18n::fill(&self.text("redis.tab.renamePrompt"), &[("key", &self.selected), ("target", &target)]);
        self.note.clear();
        self.error.clear();
    }

    fn ask_exec(&mut self, cx: &mut Context<Self>) {
        let (command, _) = split_commandline(&self.command_input);
        if command.is_empty() {
            self.show_error(self.text("redis.tab.emptyCommand"));
            return;
        }
        match exec::command_risk(&command) {
            CommandRisk::Blocked => self.show_error(self.text("redis.tab.keysRefused")),
            CommandRisk::Dangerous => {
                self.confirm = Some(Confirm::Exec);
                self.confirm_prompt = danger_prompt(self.locale, &command);
                self.note.clear();
                self.error.clear();
            }
            CommandRisk::Readonly => self.run_exec(false, cx),
        }
    }

    fn run_confirm(&mut self, cx: &mut Context<Self>) {
        let action = self.confirm.take();
        self.confirm_prompt.clear();
        match action {
            Some(Confirm::Delete) => self.run_delete(cx),
            Some(Confirm::Rename) => self.run_rename(cx),
            Some(Confirm::Exec) => self.run_exec(true, cx),
            None => {}
        }
    }

    fn run_delete(&mut self, cx: &mut Context<Self>) {
        let key = self.selected.clone();
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

    fn run_rename(&mut self, cx: &mut Context<Self>) {
        let from = self.selected.clone();
        let to = self.rename_to.trim().to_owned();
        if from.is_empty() || to.is_empty() {
            self.show_error(self.text("redis.tab.renameNeedName"));
            return;
        }
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
                let target = self.rename_to.trim().to_owned();
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
            Err(message) => self.show_error(message),
        }
    }

    fn run_exec(&mut self, confirmed: bool, cx: &mut Context<Self>) {
        let (command, args) = split_commandline(&self.command_input);
        if command.is_empty() {
            self.show_error(self.text("redis.tab.emptyCommand"));
            return;
        }
        if let Err(message) = exec::guard_command(&command, confirmed) {
            let refused = if exec::command_risk(&command) == CommandRisk::Blocked { self.text("redis.tab.keysRefused") } else { message };
            self.show_error(refused);
            return;
        }
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
                self.note = self.text("redis.tab.executed");
                self.error.clear();
                self.scan_reset = true;
                self.cursor = 0;
                self.follow = if self.selected.is_empty() { Follow::None } else { Follow::Read };
                self.load_keys(cx);
            }
            Err(message) => self.show_error(message),
        }
    }

    fn copy_value(&mut self, cx: &mut Context<Self>) {
        let text = self.detail_text();
        if text.is_empty() {
            self.show_error(self.text("redis.tab.pickKey"));
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
        self.copied = text;
        self.error.clear();
    }

    fn show_error(&mut self, message: String) {
        self.error = message;
        self.note.clear();
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
                let _ = cx.update(|cx| {
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

    fn focus_of(&mut self, field: Field, cx: &mut Context<Self>) -> FocusHandle {
        let slot = match field {
            Field::Pattern => &mut self.pattern_focus,
            Field::Ttl => &mut self.ttl_focus,
            Field::Rename => &mut self.rename_focus,
            Field::Command => &mut self.command_focus,
        };
        if slot.is_none() {
            *slot = Some(cx.focus_handle());
        }
        slot.clone().unwrap()
    }

    fn apply_key(&mut self, field: Field, action: KeyAction, cx: &mut Context<Self>) {
        if self.focused != Some(field) {
            return;
        }
        let slot = match field {
            Field::Pattern => &mut self.pattern,
            Field::Ttl => &mut self.ttl_input,
            Field::Rename => &mut self.rename_to,
            Field::Command => &mut self.command_input,
        };
        match action {
            KeyAction::Ignore => {}
            KeyAction::Backspace => {
                slot.pop();
            }
            KeyAction::Type(text) => slot.push_str(&text),
            KeyAction::Commit => {
                let id = match field {
                    Field::Pattern => "redis-filter",
                    Field::Ttl => "redis-ttl",
                    Field::Rename => "redis-rename",
                    Field::Command => "redis-exec",
                };
                self.command(id, cx);
            }
        }
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
    let mut iter = tokens.into_iter();
    (iter.next().unwrap_or_default(), iter.collect())
}

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

fn merge_keys(loaded: &[RedisKeyInfo], page: &[RedisKeyInfo]) -> Vec<RedisKeyInfo> {
    let mut out = loaded.to_vec();
    for item in page {
        if let Some(existing) = out.iter_mut().find(|key| key.name == item.name) {
            *existing = item.clone();
        } else {
            out.push(item.clone());
        }
    }
    out
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

fn describe_ttl(locale: Locale, ttl_ms: Option<i64>) -> String {
    match ttl_ms {
        None => "—".to_owned(),
        Some(-1) => i18n::text(locale, "redis.keys.neverExpire"),
        Some(-2) => i18n::text(locale, "redis.keys.missing"),
        Some(ms) => format!("{ms} ms"),
    }
}

enum KeyAction {
    Ignore,
    Backspace,
    Type(String),
    Commit,
}

fn key_action(event: &gpui_kit::KeyDownEvent) -> KeyAction {
    if event.keystroke.modifiers.control || event.keystroke.modifiers.platform || event.keystroke.modifiers.alt {
        return KeyAction::Ignore;
    }
    match event.keystroke.key.as_str() {
        "backspace" => KeyAction::Backspace,
        "enter" => KeyAction::Commit,
        "escape" | "tab" | "up" | "down" | "left" | "right" => KeyAction::Ignore,
        _ => event.keystroke.key_char.clone().filter(|text| !text.is_empty()).map(KeyAction::Type).unwrap_or(KeyAction::Ignore),
    }
}

impl Render for RedisPane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity().clone();
        let pattern_focus = self.focus_of(Field::Pattern, cx);
        let ttl_focus = self.focus_of(Field::Ttl, cx);
        let rename_focus = self.focus_of(Field::Rename, cx);
        let command_focus = self.focus_of(Field::Command, cx);
        let mut column = div().id("redis-pane").flex().flex_col();
        column = column.child(SharedString::from(format!("{} / {}", self.connection_name, self.database)));
        column = column.child(input_box("redis-pattern", &self.text("redis.tab.patternLabel"), &self.pattern, pattern_focus, Field::Pattern, entity.clone()));
        column = column.child(button("redis-filter", &self.text("redis.tab.filter"), entity.clone()));
        column = column.child(button("redis-refresh", &self.text("redis.tab.refresh"), entity.clone()));
        column = column.child(button("redis-more", &self.text("redis.tab.loadMore"), entity.clone()));
        if self.done {
            column = column.child(SharedString::from(self.text("redis.tab.scanDone")));
        }
        if self.keys.is_empty() {
            column = column.child(SharedString::from(self.text("redis.tab.emptyList")));
        }
        for (index, key) in self.keys.iter().take(200).enumerate() {
            let label = format!("{} {} {}", key.key_type, key.name, describe_ttl(self.locale, key.ttl_ms));
            column = column.child(button_owned(format!("redis-select:{index}"), label, entity.clone()));
        }
        column = column.child(SharedString::from(if self.selected.is_empty() { self.text("redis.tab.pickKey") } else { self.detail_text() }));
        column = column.child(input_box("redis-ttl-field", &self.text("redis.tab.ttlLabel"), &self.ttl_input, ttl_focus, Field::Ttl, entity.clone()));
        column = column.child(button("redis-ttl", &self.text("redis.tab.setTtl"), entity.clone()));
        column = column.child(input_box("redis-rename-field", &self.text("redis.tab.rename"), &self.rename_to, rename_focus, Field::Rename, entity.clone()));
        column = column.child(button("redis-rename", &self.text("redis.tab.rename"), entity.clone()));
        column = column.child(button("redis-delete", &self.text("redis.tab.remove"), entity.clone()));
        column = column.child(button("redis-copy", &self.text("redis.value.value"), entity.clone()));
        column = column.child(input_box("redis-command", &self.text("redis.tab.commandLabel"), &self.command_input, command_focus, Field::Command, entity.clone()));
        column = column.child(button("redis-exec", &self.text("redis.tab.execute"), entity.clone()));
        if self.confirm.is_some() {
            column = column.child(SharedString::from(self.confirm_prompt.clone()));
            column = column.child(button("redis-confirm", &self.text("redis.tab.confirmExecute"), entity.clone()));
            column = column.child(button("redis-cancel", &i18n::text(self.locale, "common.cancel"), entity.clone()));
        }
        column = column.child(
            div()
                .id("redis-status")
                .test_support()
                .role(gpui_kit::Role::Status)
                .aria_label(SharedString::from(self.status_line()))
                .child(SharedString::from(if self.error.is_empty() { self.note.clone() } else { self.error.clone() })),
        );
        column
    }
}

fn button(id: &'static str, label: &str, entity: Entity<RedisPane>) -> impl IntoElement {
    let label = SharedString::from(label.to_owned());
    div()
        .id(id)
        .test_support()
        .role(gpui_kit::Role::Button)
        .aria_label(label.clone())
        .child(label)
        .on_click(move |_, _, cx| {
            entity.update(cx, |pane, cx| pane.command(id, cx));
        })
}

fn button_owned(id: String, label: String, entity: Entity<RedisPane>) -> impl IntoElement {
    let command = SharedString::from(id.clone());
    let label = SharedString::from(label);
    div()
        .id(SharedString::from(id))
        .test_support()
        .role(gpui_kit::Role::Button)
        .aria_label(label.clone())
        .child(label)
        .on_click(move |_, _, cx| {
            let command = command.to_string();
            entity.update(cx, |pane, cx| pane.command(&command, cx));
        })
}

fn input_box(id: &'static str, caption: &str, value: &str, focus: FocusHandle, field: Field, entity: Entity<RedisPane>) -> impl IntoElement {
    let label = SharedString::from(format!("{caption} {value}"));
    div()
        .id(id)
        .test_support()
        .track_focus(&focus)
        .min_h(px(28.))
        .w(px(360.))
        .role(gpui_kit::Role::TextInput)
        .aria_label(label.clone())
        .child(label)
        .on_mouse_down(gpui_kit::MouseButton::Left, {
            let entity = entity.clone();
            let focus = focus.clone();
            move |_, window, cx| {
                window.focus(&focus, cx);
                entity.update(cx, |pane, cx| {
                    pane.focused = Some(field);
                    cx.notify();
                });
            }
        })
        .on_key_down(move |event: &gpui_kit::KeyDownEvent, _, cx| {
            let action = key_action(event);
            entity.update(cx, |pane, cx| pane.apply_key(field, action, cx));
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_parsing_ttl_and_keys_merge_stay_local() {
        assert_eq!(split_commandline("SET key \"hello world\""), ("SET".into(), vec!["key".into(), "hello world".into()]));
        assert_eq!(parse_ttl("").unwrap(), None);
        assert_eq!(parse_ttl("15").unwrap(), Some(15));
        assert!(parse_ttl("-1").is_err());
        assert!(parse_ttl("1.5").is_err());
        let merged = merge_keys(
            &[RedisKeyInfo { name: "a".into(), key_type: "string".into(), ttl_ms: Some(-1) }],
            &[RedisKeyInfo { name: "a".into(), key_type: "hash".into(), ttl_ms: Some(10) }, RedisKeyInfo { name: "b".into(), key_type: "list".into(), ttl_ms: None }],
        );
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].key_type, "hash");
        assert_eq!(parse_spec("cache|缓存|0").unwrap().0, "cache");
        assert!(parse_spec("|缓存|0").is_none());
        let prompt = danger_prompt(Locale::ZhCn, "flushall");
        assert!(prompt.contains("FLUSHALL"), "{prompt}");
        assert_ne!(i18n::text(Locale::En, "redis.tab.keysRefused"), "redis.tab.keysRefused");
    }
}

#[cfg(test)]
mod live;
