//! Redis 键浏览器。浏览只走 SCAN；写删和危险命令先在界面确认，失败时草稿还在。

use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::input::InputState;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use crate::ui::kit::{self, FieldSpec, Fields, Palette, Radius, TypeScale};
use gpui_kit::{
    ClipboardItem, Context, Entity, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div, px,
};

use crate::application::redis::{DeleteOutcome, ExecOutcome, RenameOutcome, TtlOutcome};
use crate::application::AppServices;
use crate::i18n::{self, Locale};
use crate::redis::exec::{self, CommandRisk, RespReply};
use crate::redis::keys::{RedisKeyDetail, RedisKeyInfo, RedisValue, ScanPage};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Field {
    Pattern,
    Ttl,
    Rename,
    Command,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Confirm {
    Delete { key: String },
    Rename { from: String, to: String },
    Exec { command: String, args: Vec<String> },
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

    pub fn confirm_prompt(&self) -> &str {
        &self.confirm_prompt
    }

    pub fn detail_text(&self) -> String {
        self.detail.as_ref().map(|detail| format!("{} {}", detail.key_type, format_value(&detail.value, self.locale))).unwrap_or_default()
    }

    fn reset_target_state(&mut self) {
        // 切换 Redis 工作项时丢弃上一目标的键、命令、确认和回复；
        // 迟到响应靠代次失效，不能把旧库的结果写进新库。
        self.generation = self.generation.wrapping_add(1);
        self.busy = false;
        self.connection_id.clear();
        self.connection_name.clear();
        self.database = "0".to_owned();
        self.pattern.clear();
        self.keys.clear();
        self.cursor = 0;
        self.done = false;
        self.selected.clear();
        self.detail = None;
        self.ttl_input.clear();
        self.rename_to.clear();
        self.command_input.clear();
        self.reply.clear();
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
    }

    pub fn open_spec(&mut self, spec: &str, cx: &mut Context<Self>) {
        self.reset_target_state();
        let Some((id, name, database)) = parse_spec(spec) else {
            self.error = self.text("redis.tab.needConnection");
            cx.notify();
            return;
        };
        self.connection_id = id;
        self.connection_name = name;
        self.database = database;
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
            // 确认已经钉住打开时的键，改选只留在列表上，不换这次删除或重命名的目标。
            if self.confirm.is_none() {
                if let Ok(index) = index.parse::<usize>() {
                    if let Some(key) = self.keys.get(index) {
                        let name = key.name.clone();
                        self.select_key(name, cx);
                    }
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
                self.confirm_prompt = danger_prompt(self.locale, &command);
                self.confirm = Some(Confirm::Exec { command, args });
                self.note.clear();
                self.error.clear();
            }
            CommandRisk::Readonly => self.exec_command(command, args, false, cx),
        }
    }

    fn run_confirm(&mut self, cx: &mut Context<Self>) {
        let action = self.confirm.take();
        self.confirm_prompt.clear();
        match action {
            Some(Confirm::Delete { key }) => self.run_delete(key, cx),
            Some(Confirm::Rename { from, to }) => self.run_rename(from, to, cx),
            Some(Confirm::Exec { command, args }) => self.exec_command(command, args, true, cx),
            None => {}
        }
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

    fn exec_command(&mut self, command: String, args: Vec<String>, confirmed: bool, cx: &mut Context<Self>) {
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

        // ---- 左侧：键列表
        let mut list = div().id("redis-keys").flex().flex_col().flex_1().min_h(px(0.)).overflow_y_scroll().p(px(6.));
        if self.keys.is_empty() {
            list = list.child(div().p(px(14.)).text_size(px(TypeScale::SM)).text_color(p.text_faint).child(SharedString::from(self.text("redis.tab.emptyList"))));
        }
        for (index, key) in self.keys.iter().take(500).enumerate() {
            let selected = key.name == self.selected;
            let id = format!("redis-select:{index}");
            let label = format!("{} {} {}", key.key_type, key.name, describe_ttl(self.locale, key.ttl_ms));
            let (fg, bg) = type_colors(&key.key_type, &p);
            let entity = entity.clone();
            list = list.child(
                kit::interactive_states(div().id(SharedString::from(id.clone())), &p, false)
                    .test_support()
                    .role(gpui_kit::Role::Button)
                    .aria_label(SharedString::from(label))
                    .aria_selected(selected)
                    .h(px(m.tree_row_h + 2.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .rounded(px(Radius::CONTROL))
                    .when(selected, |this| this.bg(p.selection))
                    .when(!selected, |this| this.hover(|style| style.bg(p.hover)))
                    .child(div().flex_none().w(px(44.)).child(kit::pill(SharedString::from(key.key_type.clone()), fg, bg)))
                    .child(div().flex_1().min_w(px(0.)).truncate().font_family(mono.clone()).text_size(px(TypeScale::SM)).child(SharedString::from(key.name.clone())))
                    .when(key.ttl_ms.is_some_and(|ttl| ttl >= 0), |this| this.child(kit::icon(IconName::Timer, 12., p.text_faint)))
                    .on_click(move |_, _, cx| {
                        let id = id.clone();
                        entity.update(cx, |pane, cx| pane.command(&id, cx));
                    }),
            );
        }
        let left = div()
            .flex_none()
            .w(px(320.))
            .h_full()
            .flex()
            .flex_col()
            .bg(p.chrome)
            .border_r_1()
            .border_color(p.line)
            .child(
                div()
                    .p(px(10.))
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(kit::engine_badge("redis", 20.))
                            .child(div().flex_1().min_w(px(0.)).truncate().font_weight(FontWeight::SEMIBOLD).child(SharedString::from(self.connection_name.clone())))
                            .child(kit::pill(SharedString::from(format!("db {}", self.database)), p.text_muted, p.hover)),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(4.))
                            .child(div().flex_1().child(kit::text_input("redis-pattern", &pattern, self.text("redis.tab.patternLabel")).small().prefix(kit::icon(IconName::Search, 13., p.text_faint))))
                            .child(redis_button("redis-filter", Some(IconName::ListFilter), &self.text("redis.tab.filter"), Kind::Tool, &entity))
                            .child(redis_icon("redis-refresh", IconName::RefreshCw, &self.text("redis.tab.refresh"), &entity)),
                    ),
            )
            .child(list)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(px(10.))
                    .py(px(6.))
                    .border_t_1()
                    .border_color(p.line)
                    .child(div().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(SharedString::from(if self.done { self.text("redis.tab.scanDone") } else { format!("{} keys", self.keys.len()) })))
                    .child(redis_button("redis-more", Some(IconName::ChevronsDown), &self.text("redis.tab.loadMore"), Kind::Tool, &entity)),
            );

        // ---- 右侧：键详情
        let mut detail = div().flex().flex_col().gap(px(14.)).p(px(18.)).flex_1().min_h(px(0.));
        if self.selected.is_empty() {
            detail = detail.child(div().flex_1().flex().items_center().justify_center().child(kit::empty_state(IconName::Key, SharedString::from(self.text("redis.open")), SharedString::from(self.text("redis.tab.pickKey")), &p)));
        } else {
            let (type_name, ttl_text) = self
                .detail
                .as_ref()
                .map(|detail| (detail.key_type.clone(), describe_ttl(self.locale, detail.ttl_ms)))
                .unwrap_or_default();
            let (fg, bg) = type_colors(&type_name, &p);
            detail = detail
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .child(kit::pill(SharedString::from(type_name.clone()), fg, bg))
                        .child(div().flex_1().min_w(px(0.)).truncate().font_family(mono.clone()).text_size(px(TypeScale::LG)).font_weight(FontWeight::SEMIBOLD).child(SharedString::from(self.selected.clone())))
                        .child(div().flex().items_center().gap(px(4.)).text_size(px(TypeScale::SM)).text_color(p.text_muted).child(kit::icon(IconName::Timer, 13., p.text_faint)).child(SharedString::from(ttl_text)))
                        .child(redis_button("redis-copy", Some(IconName::Copy), &self.text("redis.value.value"), Kind::Tool, &entity))
                        .child(redis_button("redis-delete", Some(IconName::Trash), &self.text("redis.tab.remove"), Kind::Danger, &entity)),
                )
                .child(
                    div()
                        .id("redis-value")
                        .flex_1()
                        .min_h(px(120.))
                        .overflow_y_scroll()
                        .p(px(14.))
                        .rounded(px(Radius::PANEL))
                        .bg(p.chrome)
                        .border_1()
                        .border_color(p.line)
                        .font_family(mono.clone())
                        .text_size(px(TypeScale::SM))
                        .child(SharedString::from(self.detail.as_ref().map(|detail| format_value(&detail.value, self.locale)).unwrap_or_default())),
                )
                .child(
                    div()
                        .flex()
                        .gap(px(12.))
                        .child(
                            div()
                                .flex_1()
                                .flex()
                                .items_end()
                                .gap(px(6.))
                                .child(div().flex_1().child(kit::form_row(SharedString::from(self.text("redis.tab.ttlLabel")), kit::text_input("redis-ttl-field", &ttl, self.text("redis.tab.ttlLabel")).small(), &p)))
                                .child(redis_button("redis-ttl", Some(IconName::Timer), &self.text("redis.tab.setTtl"), Kind::Secondary, &entity)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .flex()
                                .items_end()
                                .gap(px(6.))
                                .child(div().flex_1().child(kit::form_row(SharedString::from(self.text("redis.tab.rename")), kit::text_input("redis-rename-field", &rename, self.text("redis.tab.rename")).small(), &p)))
                                .child(redis_button("redis-rename", Some(IconName::Pencil), &self.text("redis.tab.rename"), Kind::Secondary, &entity)),
                        ),
                );
        }
        if self.selected.is_empty() {
            // 操作按钮常驻，便于键盘与测试定位；没有选中键时它们只给出提示。
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
        }

        // ---- 底部：命令行与状态
        let mut console = div()
            .flex_none()
            .p(px(10.))
            .flex()
            .flex_col()
            .gap(px(8.))
            .border_t_1()
            .border_color(p.line)
            .bg(p.chrome)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(kit::icon(IconName::SquareTerminal, 15., p.text_muted))
                    .child(div().flex_1().child(kit::text_input("redis-command", &command, self.text("redis.tab.commandLabel")).small()))
                    .child(redis_button("redis-exec", Some(IconName::Play), &self.text("redis.tab.execute"), Kind::Primary, &entity)),
            );
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
                    .child(redis_button("redis-cancel", None, &i18n::text(self.locale, "common.cancel"), Kind::Secondary, &entity))
                    .child(redis_button("redis-confirm", Some(IconName::Check), &self.text("redis.tab.confirmExecute"), Kind::Danger, &entity)),
            );
        }
        let status_text = if self.error.is_empty() { self.note.clone() } else { self.error.clone() };
        console = console.child(
            div()
                .id("redis-status")
                .test_support()
                .role(gpui_kit::Role::Status)
                .aria_label(SharedString::from(self.status_line()))
                .min_h(px(16.))
                .text_size(px(TypeScale::SM))
                .text_color(if self.error.is_empty() { p.text_muted } else { p.danger })
                .child(SharedString::from(status_text)),
        );

        div()
            .id("redis-pane")
            .size_full()
            .flex()
            .child(left)
            .child(div().flex_1().min_w(px(0.)).h_full().flex().flex_col().child(detail).child(console))
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
    fn redis_input(&mut self, field: Field, _id: &'static str, label_key: &str, placeholder: &str, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        let value = match field {
            Field::Pattern => self.pattern.clone(),
            Field::Ttl => self.ttl_input.clone(),
            Field::Rename => self.rename_to.clone(),
            Field::Command => self.command_input.clone(),
        };
        let _ = label_key;
        self.fields.bind_enter(
            field,
            &value,
            FieldSpec { placeholder, masked: false },
            window,
            cx,
            |this: &mut Self, field, value, _| {
                // 确认期间重命名框和命令框保持打开时的文本，避免确认落到后改的目标上。
                if this.confirm.is_some() && matches!(field, Field::Rename | Field::Command) {
                    return;
                }
                this.focused = Some(field);
                match field {
                    Field::Pattern => this.pattern = value,
                    Field::Ttl => this.ttl_input = value,
                    Field::Rename => this.rename_to = value,
                    Field::Command => this.command_input = value,
                }
            },
            Some(|this: &mut Self, field, cx| {
                let id = match field {
                    Field::Pattern => "redis-filter",
                    Field::Ttl => "redis-ttl",
                    Field::Rename => "redis-rename",
                    Field::Command => "redis-exec",
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

fn redis_button(id: &'static str, icon: Option<IconName>, label: &str, kind: Kind, entity: &Entity<RedisPane>) -> gpui_kit::component::button::Button {
    let entity = entity.clone();
    let click = move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut gpui_kit::App| entity.update(cx, |pane, cx| pane.command(id, cx));
    let label = SharedString::from(label.to_owned());
    match kind {
        Kind::Primary => kit::primary_button(id, icon, label, click),
        Kind::Secondary => kit::secondary_button(id, icon, label, click),
        Kind::Tool => kit::tool_button(id, icon, label, click),
        Kind::Danger => kit::danger_button(id, icon, label, click),
    }
}

fn redis_icon(id: &'static str, icon: IconName, label: &str, entity: &Entity<RedisPane>) -> gpui_kit::component::button::Button {
    let entity = entity.clone();
    kit::icon_button(id, icon, SharedString::from(label.to_owned()), move |_, _, cx| entity.update(cx, |pane, cx| pane.command(id, cx)))
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

    #[gpui_kit::test]
    fn switching_redis_target_clears_key_command_and_confirmation(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::AppContext as _;
        let dir = std::env::temp_dir().join(format!("rillsql-redis-reset-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let services = std::sync::Arc::new(crate::application::AppServices::new(std::sync::Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
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
}

#[cfg(test)]
mod live;
