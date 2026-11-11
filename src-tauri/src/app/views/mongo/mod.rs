//! MongoDB 集合文档。过滤、新增、替换和删除都先校验；未确认不写库，失败时编辑内容还在。

use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Editor, EditorState, InputEvent};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, ClipboardItem, Context, Entity, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div, px, relative,
};

use crate::ui::kit::{self, FieldSpec, Fields, Palette, Radius, TypeScale};

use crate::application::mongo::{DeleteOutcome, InsertOutcome, ReplaceOutcome};
use crate::application::AppServices;
use crate::i18n::{self, Locale};
use crate::mongo::docs::{CollectionSummary, DatabaseSummary, DocumentsPage, MongoDoc};

const PAGE_LIMIT: u64 = 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Field {
    Filter,
    Editor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditorMode {
    Insert,
    Replace,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AfterFind {
    Stay,
    ApplyFilter,
}

/// 文档写入也先进入待确认；确认条展示目标库、集合和要写入的 JSON。
#[derive(Clone, Debug, PartialEq, Eq)]
enum PendingWrite {
    Insert { json: String },
    Replace { id_json: String, json: String },
}

pub struct MongoPane {
    services: Arc<AppServices>,
    locale: Locale,
    connection_id: String,
    connection_name: String,
    databases: Vec<DatabaseSummary>,
    database: String,
    collections: Vec<CollectionSummary>,
    collection: String,
    filter_input: String,
    applied_filter: String,
    pending_filter: String,
    after_find: AfterFind,
    skip: u64,
    documents: Vec<MongoDoc>,
    total: i64,
    columns: Vec<String>,
    view_open: bool,
    view_json: String,
    editor_open: bool,
    editor_mode: EditorMode,
    editor_text: String,
    editor_id: String,
    editor_error: String,
    confirm_id: Option<String>,
    confirm_write: Option<PendingWrite>,
    confirm_prompt: String,
    note: String,
    error: String,
    filter_error: String,
    busy: bool,
    generation: u64,
    copied: String,
    focused: Option<Field>,
    fields: Fields<Field>,
    editor_state: Option<(Entity<EditorState>, String, gpui_kit::Subscription)>,
}

impl MongoPane {
    pub fn new(services: Arc<AppServices>) -> Self {
        Self {
            services,
            locale: Locale::ZhCn,
            connection_id: String::new(),
            connection_name: String::new(),
            databases: Vec::new(),
            database: String::new(),
            collections: Vec::new(),
            collection: String::new(),
            filter_input: String::new(),
            applied_filter: String::new(),
            pending_filter: String::new(),
            after_find: AfterFind::Stay,
            skip: 0,
            documents: Vec::new(),
            total: 0,
            columns: Vec::new(),
            view_open: false,
            view_json: String::new(),
            editor_open: false,
            editor_mode: EditorMode::Insert,
            editor_text: String::new(),
            editor_id: String::new(),
            editor_error: String::new(),
            confirm_id: None,
            confirm_write: None,
            confirm_prompt: String::new(),
            note: String::new(),
            error: String::new(),
            filter_error: String::new(),
            busy: false,
            generation: 0,
            copied: String::new(),
            focused: None,
            fields: Fields::default(),
            editor_state: None,
        }
    }

    pub fn set_locale(&mut self, locale: Locale) {
        self.locale = locale;
    }

    pub fn running(&self) -> bool {
        self.busy
    }

    pub fn set_filter(&mut self, value: &str) {
        self.filter_input = value.to_owned();
    }

    pub fn set_editor(&mut self, value: &str) {
        self.editor_text = value.to_owned();
        self.editor_open = true;
    }

    pub fn filter_input(&self) -> &str {
        &self.filter_input
    }

    pub fn applied_filter(&self) -> &str {
        &self.applied_filter
    }

    pub fn editor_text(&self) -> &str {
        &self.editor_text
    }

    pub fn editor_open(&self) -> bool {
        self.editor_open
    }

    pub fn documents(&self) -> &[MongoDoc] {
        &self.documents
    }

    pub fn columns(&self) -> &[String] {
        &self.columns
    }

    pub fn total(&self) -> i64 {
        self.total
    }

    pub fn skip(&self) -> u64 {
        self.skip
    }

    pub fn database(&self) -> &str {
        &self.database
    }

    pub fn collection(&self) -> &str {
        &self.collection
    }

    pub fn databases(&self) -> &[DatabaseSummary] {
        &self.databases
    }

    pub fn collections(&self) -> &[CollectionSummary] {
        &self.collections
    }

    pub fn error(&self) -> &str {
        &self.error
    }

    pub fn note(&self) -> &str {
        &self.note
    }

    pub fn filter_error(&self) -> &str {
        &self.filter_error
    }

    pub fn copied(&self) -> &str {
        &self.copied
    }

    pub fn view_json(&self) -> &str {
        &self.view_json
    }

    pub fn confirm_open(&self) -> bool {
        self.confirm_id.is_some() || self.confirm_write.is_some()
    }

    pub fn confirm_prompt(&self) -> &str {
        &self.confirm_prompt
    }

    fn confirm_is_delete(&self) -> bool {
        self.confirm_id.is_some()
    }

    fn confirm_action_text(&self) -> String {
        if self.confirm_is_delete() {
            self.text("mongo.tab.confirmDelete")
        } else if self.locale == Locale::En {
            "Confirm write".to_owned()
        } else {
            "确认写入".to_owned()
        }
    }

    #[cfg(test)]
    pub fn plant_documents(&mut self, database: &str, collection: &str, docs: Vec<MongoDoc>) {
        self.connection_id = "missing".into();
        self.connection_name = "文档".into();
        self.database = database.into();
        self.collection = collection.into();
        self.documents = docs;
        self.total = self.documents.len() as i64;
        self.columns = columns_of(&self.documents);
    }

    fn clear_confirm(&mut self) {
        self.confirm_id = None;
        self.confirm_write = None;
        self.confirm_prompt.clear();
    }

    fn reset_target_state(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.busy = false;
        self.connection_id.clear();
        self.connection_name.clear();
        self.databases.clear();
        self.database.clear();
        self.collections.clear();
        self.collection.clear();
        self.filter_input.clear();
        self.applied_filter.clear();
        self.pending_filter.clear();
        self.after_find = AfterFind::Stay;
        self.skip = 0;
        self.documents.clear();
        self.total = 0;
        self.columns.clear();
        self.view_open = false;
        self.view_json.clear();
        self.editor_open = false;
        self.editor_mode = EditorMode::Insert;
        self.editor_text.clear();
        self.editor_id.clear();
        self.editor_error.clear();
        self.confirm_id = None;
        self.confirm_write = None;
        self.confirm_prompt.clear();
        self.note.clear();
        self.error.clear();
        self.filter_error.clear();
        self.copied.clear();
        self.fields = Fields::default();
        self.editor_state = None;
    }

    pub fn open_spec(&mut self, spec: &str, cx: &mut Context<Self>) {
        self.reset_target_state();
        let Some((id, name, database, collection)) = parse_spec(spec) else {
            self.show_error(self.text("mongo.tab.needConnection"));
            cx.notify();
            return;
        };
        self.connection_id = id;
        self.connection_name = name;
        self.database = database;
        self.collection = collection;
        self.refresh_databases(cx);
    }

    pub fn command(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.busy && !matches!(id, "mongo-cancel") {
            self.show_error(self.text("mongo.tab.loading"));
            cx.notify();
            return;
        }
        if id == "mongo-refresh-db" {
            self.refresh_databases(cx);
        } else if let Some(index) = id.strip_prefix("mongo-database:") {
            self.pick_database(index, cx);
        } else if let Some(index) = id.strip_prefix("mongo-collection:") {
            self.pick_collection(index, cx);
        } else if id == "mongo-filter" {
            self.skip = 0;
            self.load_page(AfterFind::ApplyFilter, cx);
        } else if id == "mongo-clear-filter" {
            self.filter_input.clear();
            self.applied_filter.clear();
            self.pending_filter.clear();
            self.skip = 0;
            self.load_page(AfterFind::Stay, cx);
        } else if id == "mongo-refresh" {
            self.load_page(AfterFind::Stay, cx);
        } else if id == "mongo-prev" {
            if self.skip > 0 {
                self.skip = next_skip(self.skip, PAGE_LIMIT, -1);
                self.load_page(AfterFind::Stay, cx);
            }
        } else if id == "mongo-next" {
            if self.can_next() {
                self.skip = next_skip(self.skip, PAGE_LIMIT, 1);
                self.load_page(AfterFind::Stay, cx);
            }
        } else if let Some(index) = id.strip_prefix("mongo-view:") {
            self.open_view(index);
        } else if id == "mongo-view-close" {
            self.view_open = false;
        } else if let Some(index) = id.strip_prefix("mongo-copy:") {
            self.copy_doc(index, cx);
        } else if let Some(index) = id.strip_prefix("mongo-edit:") {
            self.open_edit(index);
        } else if id == "mongo-insert" {
            self.editor_mode = EditorMode::Insert;
            self.editor_id.clear();
            self.editor_text = "{\n  \"status\": \"active\"\n}".to_owned();
            self.editor_error.clear();
            self.editor_open = true;
            self.note.clear();
            self.error.clear();
        } else if id == "mongo-save" {
            self.ask_save();
        } else if let Some(index) = id.strip_prefix("mongo-delete:") {
            self.ask_delete(index);
        } else if id == "mongo-confirm" {
            self.run_confirm(cx);
        } else if id == "mongo-cancel" {
            self.confirm_id = None;
            self.confirm_write = None;
            self.confirm_prompt.clear();
        }
        cx.notify();
    }

    fn pick_database(&mut self, index: &str, cx: &mut Context<Self>) {
        let Ok(index) = index.parse::<usize>() else { return };
        let Some(database) = self.databases.get(index) else { return };
        self.database = database.name.clone();
        self.collection.clear();
        self.clear_confirm();
        self.documents.clear();
        self.columns.clear();
        self.total = 0;
        self.load_collections(cx);
    }

    fn pick_collection(&mut self, index: &str, cx: &mut Context<Self>) {
        let Ok(index) = index.parse::<usize>() else { return };
        let Some(collection) = self.collections.get(index) else { return };
        self.collection = collection.name.clone();
        self.clear_confirm();
        self.skip = 0;
        self.applied_filter.clear();
        self.filter_input.clear();
        self.load_page(AfterFind::Stay, cx);
    }

    fn refresh_databases(&mut self, cx: &mut Context<Self>) {
        if self.connection_id.is_empty() {
            self.show_error(self.text("mongo.tab.needConnection"));
            return;
        }
        let connections = Arc::clone(&self.services.connections);
        let connection_id = self.connection_id.clone();
        self.spawn(cx, true, async move { crate::application::mongo::list_databases(connections.as_ref(), &connection_id).await }, Self::finish_databases);
    }

    fn finish_databases(&mut self, result: Result<Vec<DatabaseSummary>, String>, cx: &mut Context<Self>) {
        match result {
            Ok(rows) => {
                self.databases = rows;
                self.error.clear();
                if !self.database.is_empty() {
                    self.load_collections(cx);
                }
            }
            Err(message) => self.show_error(message),
        }
    }

    fn load_collections(&mut self, cx: &mut Context<Self>) {
        if self.database.is_empty() {
            self.show_error(self.text("mongo.tab.needCollection"));
            return;
        }
        let connections = Arc::clone(&self.services.connections);
        let connection_id = self.connection_id.clone();
        let database = self.database.clone();
        self.spawn(
            cx,
            false,
            async move { crate::application::mongo::list_collections(connections.as_ref(), &connection_id, &database).await },
            Self::finish_collections,
        );
    }

    fn finish_collections(&mut self, result: Result<Vec<CollectionSummary>, String>, cx: &mut Context<Self>) {
        match result {
            Ok(rows) => {
                self.collections = rows;
                self.error.clear();
                if !self.collection.is_empty() {
                    self.load_page(AfterFind::Stay, cx);
                }
            }
            Err(message) => self.show_error(message),
        }
    }

    fn load_page(&mut self, after: AfterFind, cx: &mut Context<Self>) {
        if self.connection_id.is_empty() {
            self.show_error(self.text("mongo.tab.needConnection"));
            return;
        }
        if self.database.is_empty() || self.collection.is_empty() {
            self.show_error(self.text("mongo.tab.needCollection"));
            return;
        }
        self.after_find = after;
        let filter = match after {
            AfterFind::ApplyFilter => self.filter_input.trim().to_owned(),
            AfterFind::Stay => self.applied_filter.clone(),
        };
        self.pending_filter = filter.clone();
        let filter_arg = if filter.is_empty() { None } else { Some(filter) };
        let connections = Arc::clone(&self.services.connections);
        let connection_id = self.connection_id.clone();
        let database = self.database.clone();
        let collection = self.collection.clone();
        let skip = self.skip;
        self.spawn(
            cx,
            false,
            async move {
                crate::application::mongo::find_documents(connections.as_ref(), &connection_id, &database, &collection, Some(skip), Some(PAGE_LIMIT), filter_arg).await
            },
            Self::finish_find,
        );
    }

    fn finish_find(&mut self, result: Result<DocumentsPage, String>, _: &mut Context<Self>) {
        match result {
            Ok(page) => {
                if self.after_find == AfterFind::ApplyFilter {
                    self.applied_filter = self.pending_filter.clone();
                }
                self.documents = page.documents;
                self.total = page.total;
                self.columns = columns_of(&self.documents);
                self.filter_error.clear();
                self.error.clear();
            }
            Err(message) => {
                self.filter_error = message.clone();
                self.show_error(message);
            }
        }
    }

    fn open_view(&mut self, index: &str) {
        let Some(doc) = self.doc_at(index) else { return };
        self.view_json = pretty_json(&doc.json);
        self.view_open = true;
        self.error.clear();
    }

    fn copy_doc(&mut self, index: &str, cx: &mut Context<Self>) {
        let Some(doc) = self.doc_at(index) else { return };
        let text = pretty_json(&doc.json);
        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
        self.copied = text;
        self.note = self.text("mongo.docs.copiedJson");
        self.error.clear();
    }

    fn open_edit(&mut self, index: &str) {
        let Some(doc) = self.doc_at(index) else { return };
        self.editor_mode = EditorMode::Replace;
        self.editor_id = doc.id_json.clone();
        self.editor_text = pretty_json(&doc.json);
        self.editor_error.clear();
        self.editor_open = true;
        self.view_json = self.editor_text.clone();
        self.view_open = true;
        self.error.clear();
    }

    /// 保存按钮只生成待确认写入，不直接落库。确认条上带目标库、集合和 JSON。
    fn ask_save(&mut self) {
        if let Some(message) = quick_validate(&self.editor_text, self.locale) {
            self.editor_error = message.clone();
            self.show_error(message);
            return;
        }
        self.editor_error.clear();
        if self.connection_id.is_empty() || self.database.is_empty() || self.collection.is_empty() {
            self.show_error(self.text("mongo.tab.needCollection"));
            return;
        }
        let target = format!("{}.{}", self.database, self.collection);
        let json = self.editor_text.clone();
        let preview = json.replace('\n', " ");
        self.confirm_id = None;
        match self.editor_mode {
            EditorMode::Insert => {
                self.confirm_prompt = if self.locale == Locale::En {
                    format!("Insert into {target}: {preview}. Confirm write?")
                } else {
                    format!("将向 {target} 新增文档：{preview}。确认写入？")
                };
                self.confirm_write = Some(PendingWrite::Insert { json });
            }
            EditorMode::Replace => {
                let id_json = self.editor_id.clone();
                if id_json.is_empty() {
                    self.show_error(self.text("mongo.docs.notMatched"));
                    return;
                }
                self.confirm_prompt = if self.locale == Locale::En {
                    format!("Replace _id={id_json} in {target}: {preview}. Confirm write?")
                } else {
                    format!("将覆盖 {target} 中 _id={id_json} 的文档：{preview}。确认写入？")
                };
                self.confirm_write = Some(PendingWrite::Replace { id_json, json });
            }
        }
        self.note.clear();
        self.error.clear();
    }

    fn run_confirm(&mut self, cx: &mut Context<Self>) {
        if self.confirm_id.is_some() {
            self.run_delete(cx);
        } else if let Some(write) = self.confirm_write.take() {
            self.confirm_prompt.clear();
            self.run_write(write, cx);
        }
    }

    fn run_write(&mut self, write: PendingWrite, cx: &mut Context<Self>) {
        let connections = Arc::clone(&self.services.connections);
        let connection_id = self.connection_id.clone();
        let database = self.database.clone();
        let collection = self.collection.clone();
        match write {
            PendingWrite::Insert { json } => self.spawn(
                cx,
                false,
                async move { crate::application::mongo::insert_document(connections.as_ref(), &connection_id, &database, &collection, &json).await },
                Self::finish_insert,
            ),
            PendingWrite::Replace { id_json, json } => self.spawn(
                cx,
                false,
                async move { crate::application::mongo::replace_document(connections.as_ref(), &connection_id, &database, &collection, &id_json, &json).await },
                Self::finish_replace,
            ),
        }
    }

    fn finish_insert(&mut self, result: Result<InsertOutcome, String>, cx: &mut Context<Self>) {
        match result {
            Ok(outcome) => {
                self.note = i18n::fill(&self.text("mongo.docs.insertedNote"), &[("id", &outcome.inserted_id_json)]);
                self.error.clear();
                self.editor_error.clear();
                self.editor_open = false;
                self.skip = 0;
                self.load_page(AfterFind::Stay, cx);
            }
            Err(message) => {
                self.editor_error = message.clone();
                self.show_error(message);
            }
        }
    }

    fn finish_replace(&mut self, result: Result<ReplaceOutcome, String>, cx: &mut Context<Self>) {
        match result {
            Ok(outcome) if outcome.matched == 0 => {
                let message = self.text("mongo.docs.notMatched");
                self.editor_error = message.clone();
                self.show_error(message);
            }
            Ok(_) => {
                self.note = self.text("mongo.docs.replacedNote");
                self.error.clear();
                self.editor_error.clear();
                self.editor_open = false;
                self.load_page(AfterFind::Stay, cx);
            }
            Err(message) => {
                self.editor_error = message.clone();
                self.show_error(message);
            }
        }
    }

    fn ask_delete(&mut self, index: &str) {
        let Some(doc) = self.doc_at(index) else { return };
        let id_json = doc.id_json.clone();
        self.confirm_prompt = i18n::fill(&self.text("mongo.docs.deletePrompt"), &[("id", &id_json)]);
        self.confirm_id = Some(id_json);
        self.confirm_write = None;
        self.note.clear();
        self.error.clear();
    }

    fn run_delete(&mut self, cx: &mut Context<Self>) {
        let Some(id_json) = self.confirm_id.clone() else { return };
        self.confirm_id = None;
        self.confirm_write = None;
        self.confirm_prompt.clear();
        let connections = Arc::clone(&self.services.connections);
        let connection_id = self.connection_id.clone();
        let database = self.database.clone();
        let collection = self.collection.clone();
        self.spawn(
            cx,
            false,
            async move { crate::application::mongo::delete_document(connections.as_ref(), &connection_id, &database, &collection, &id_json, true).await },
            Self::finish_delete,
        );
    }

    fn finish_delete(&mut self, result: Result<DeleteOutcome, String>, cx: &mut Context<Self>) {
        match result {
            Ok(outcome) => {
                self.note = i18n::fill(&self.text("mongo.docs.deletedNote"), &[("n", &outcome.deleted.to_string())]);
                self.error.clear();
                if self.skip > 0 && self.documents.len() <= 1 {
                    self.skip = next_skip(self.skip, PAGE_LIMIT, -1);
                }
                self.load_page(AfterFind::Stay, cx);
            }
            Err(message) => self.show_error(message),
        }
    }

    fn doc_at(&self, index: &str) -> Option<MongoDoc> {
        let index = index.parse::<usize>().ok()?;
        self.documents.get(index).cloned()
    }

    fn can_next(&self) -> bool {
        self.total > 0 && self.skip.saturating_add(self.documents.len() as u64) < self.total as u64
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
        let columns = self.columns.join(",");
        let names = self.documents.iter().take(8).map(|doc| doc.json.replace('\n', " ")).collect::<Vec<_>>().join(" || ");
        format!(
            "{name} db={db} coll={coll} filter={filter} applied={applied} skip={skip} total={total} columns={columns} note={note} error={error} filter_error={filter_error} editor={editor} confirm={confirm} copied={copied} view={view} docs={names}",
            name = self.connection_name,
            db = self.database,
            coll = self.collection,
            filter = self.filter_input,
            applied = self.applied_filter,
            skip = self.skip,
            total = self.total,
            note = self.note,
            error = self.error,
            filter_error = self.filter_error,
            editor = self.editor_text.replace('\n', " "),
            confirm = self.confirm_prompt,
            copied = self.copied.replace('\n', " "),
            view = self.view_json.replace('\n', " "),
        )
    }
}

pub fn columns_of(documents: &[MongoDoc]) -> Vec<String> {
    let mut columns = vec!["_id".to_owned()];
    for doc in documents {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&doc.json) else { continue };
        let Some(object) = value.as_object() else { continue };
        for key in object.keys() {
            if !columns.iter().any(|column| column == key) {
                columns.push(key.clone());
            }
        }
    }
    columns
}

pub fn cell_text(json: &str, column: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else { return String::new() };
    let Some(object) = value.as_object() else { return String::new() };
    let Some(value) = object.get(column) else { return String::new() };
    if value.is_null() {
        return "null".to_owned();
    }
    if let Some(text) = value.as_str() {
        return text.to_owned();
    }
    serde_json::to_string(value).unwrap_or_default()
}

pub fn pretty_json(json: &str) -> String {
    serde_json::from_str::<serde_json::Value>(json).ok().and_then(|value| serde_json::to_string_pretty(&value).ok()).unwrap_or_else(|| json.to_owned())
}

pub fn quick_validate(text: &str, locale: Locale) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Some(i18n::text(locale, "mongo.docs.required"));
    }
    match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(value) if value.is_object() => None,
        Ok(_) => Some(i18n::text(locale, "mongo.docs.mustBeObject")),
        Err(err) => Some(i18n::fill(&i18n::text(locale, "mongo.docs.syntaxError"), &[("message", &err.to_string())])),
    }
}

pub fn next_skip(current: u64, page: u64, direction: i32) -> u64 {
    if direction < 0 { current.saturating_sub(page) } else { current.saturating_add(page) }
}

fn parse_spec(spec: &str) -> Option<(String, String, String, String)> {
    let mut parts = spec.splitn(4, '|');
    let id = parts.next()?.trim().to_owned();
    if id.is_empty() {
        return None;
    }
    let name = parts.next().unwrap_or("").trim().to_owned();
    let database = parts.next().unwrap_or("").trim().to_owned();
    let collection = parts.next().unwrap_or("").trim().to_owned();
    Some((id, name, database, collection))
}

impl Render for MongoPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::of(cx);
        let m = kit::Metrics::of(cx);
        let entity = cx.entity();
        let mono = cx.theme().mono_font_family.clone();
        let filter = self.fields.bind_enter(
            Field::Filter,
            &self.filter_input.clone(),
            FieldSpec { placeholder: r#"{"status": "active"}"#, masked: false },
            window,
            cx,
            |this: &mut Self, _, value, _| {
                this.focused = Some(Field::Filter);
                this.filter_input = value;
            },
            Some(|this: &mut Self, _, cx| this.command("mongo-filter", cx)),
        );

        // ---- 左栏：库与集合
        let mut nav = div().id("mongo-nav").flex().flex_col().gap(px(1.)).flex_1().min_h(px(0.)).overflow_y_scroll().p(px(6.));
        nav = nav.child(div().px(px(8.)).pt(px(6.)).pb(px(4.)).child(kit::eyebrow(SharedString::from(self.text("mongo.tab.databases")), &p)));
        for (index, database) in self.databases.iter().take(60).enumerate() {
            let selected = database.name == self.database;
            nav = nav.child(
                nav_row(
                    format!("mongo-database:{index}"),
                    format!("{} {}", self.text("mongo.tab.databases"), database.name),
                    IconName::Database,
                    database.name.clone(),
                    database.collections.to_string(),
                    selected,
                    &p,
                    m,
                    &entity,
                ),
            );
        }
        if !self.collections.is_empty() {
            nav = nav.child(div().px(px(8.)).pt(px(12.)).pb(px(4.)).child(kit::eyebrow(SharedString::from(self.text("mongo.tab.collections")), &p)));
        }
        for (index, collection) in self.collections.iter().take(120).enumerate() {
            let count = collection.estimated_documents.map(|count| count.to_string()).unwrap_or_else(|| "—".into());
            let selected = collection.name == self.collection;
            nav = nav.child(nav_row(
                format!("mongo-collection:{index}"),
                format!("{} {} {count}", self.text("mongo.tab.collections"), collection.name),
                IconName::FolderOpen,
                collection.name.clone(),
                count,
                selected,
                &p,
                m,
                &entity,
            ));
        }
        let left = div()
            .flex_none()
            .w(px(250.))
            .h_full()
            .flex()
            .flex_col()
            .bg(p.chrome)
            .border_r_1()
            .border_color(p.line)
            .child(
                div()
                    .h(px(m.toolbar_h() + 2.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .border_b_1()
                    .border_color(p.line)
                    .child(kit::engine_badge("mongodb", 20.))
                    .child(div().flex_1().min_w(px(0.)).truncate().font_weight(FontWeight::SEMIBOLD).child(SharedString::from(self.connection_name.clone())))
                    .child(mongo_icon("mongo-refresh-db", IconName::RefreshCw, &self.text("mongo.tab.databases"), &entity)),
            )
            .child(nav);

        // ---- 工具条
        let page_text = if self.total >= 0 {
            format!("{}–{} / {}", if self.documents.is_empty() { 0 } else { self.skip + 1 }, self.skip + self.documents.len() as u64, self.total)
        } else {
            format!("{}–{}", self.skip + 1, self.skip + self.documents.len() as u64)
        };
        let toolbar = div()
            .flex_none()
            .px(px(10.))
            .py(px(8.))
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(6.))
            .border_b_1()
            .border_color(p.line)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .text_size(px(TypeScale::MD))
                    .child(div().text_color(p.text_muted).child(SharedString::from(self.database.clone())))
                    .child(kit::icon(IconName::ChevronRight, 12., p.text_faint))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(SharedString::from(self.collection.clone()))),
            )
            .child(div().w(px(8.)))
            .child(div().flex_1().min_w(px(200.)).child(kit::text_input("mongo-filter-field", &filter, self.text("mongo.tab.filterLabel")).small().prefix(kit::icon(IconName::Funnel, 13., p.text_faint))))
            .child(mongo_button("mongo-filter", None, &self.text("mongo.tab.apply"), Kind::Secondary, &entity))
            .child(mongo_button("mongo-clear-filter", None, &self.text("mongo.tab.clearFilter"), Kind::Tool, &entity))
            .child(kit::vline(&p))
            .child(mongo_icon("mongo-refresh", IconName::RefreshCw, &self.text("mongo.tab.refresh"), &entity))
            .child(mongo_icon("mongo-prev", IconName::ChevronLeft, &self.text("mongo.tab.prevPage"), &entity))
            .child(div().text_size(px(TypeScale::SM)).text_color(p.text_muted).child(SharedString::from(page_text)))
            .child(mongo_icon("mongo-next", IconName::ChevronRight, &self.text("mongo.tab.nextPage"), &entity))
            .child(mongo_button("mongo-insert", Some(IconName::Plus), &self.text("mongo.tab.insert"), Kind::Primary, &entity));

        // ---- 文档表
        let shown: Vec<String> = self.columns.iter().take(7).cloned().collect();
        let mut table = div().id("mongo-docs").flex().flex_col().flex_1().min_h(px(0.)).overflow_y_scroll();
        if !self.filter_error.is_empty() {
            table = table.child(div().mx(px(10.)).mt(px(10.)).px(px(12.)).py(px(8.)).rounded(px(Radius::PANEL)).bg(p.danger_soft).text_color(p.danger).text_size(px(TypeScale::SM)).child(SharedString::from(self.filter_error.clone())));
        }
        if self.documents.is_empty() {
            let empty = if self.applied_filter.is_empty() { self.text("mongo.tab.emptyCollection") } else { self.text("mongo.tab.emptyFiltered") };
            let detail = if self.collection.is_empty() { self.text("mongo.tab.needCollection") } else { String::new() };
            table = table.child(div().flex_1().flex().items_center().justify_center().child(kit::empty_state(IconName::Braces, SharedString::from(empty), SharedString::from(detail), &p)));
        } else {
            let mut head = div().flex().h(px(m.bar_h() - 2.)).px(px(10.)).items_center().bg(p.chrome).border_b_1().border_color(p.line).text_size(px(TypeScale::XS)).text_color(p.text_faint);
            for name in &shown {
                head = head.child(div().flex_1().flex_basis(px(0.)).min_w(px(0.)).truncate().pr(px(8.)).child(SharedString::from(name.clone())));
            }
            head = head.child(div().flex_none().w(px(124.)));
            table = table.child(head);
            for (index, doc) in self.documents.iter().take(PAGE_LIMIT as usize).enumerate() {
                let mut row = div()
                    .id(SharedString::from(format!("mongo-row-{index}")))
                    .group("mongo-row")
                    .flex()
                    .min_h(px(m.row_h + 2.))
                    .px(px(10.))
                    .items_center()
                    .border_b_1()
                    .border_color(p.line_soft)
                    .hover(|style| style.bg(p.hover));
                for name in &shown {
                    let text = cell_text(&doc.json, name);
                    let faint = text == "null" || text.is_empty();
                    row = row.child(
                        div()
                            .flex_1()
                            .flex_basis(px(0.))
                            .min_w(px(0.))
                            .truncate()
                            .pr(px(8.))
                            .text_size(px(TypeScale::SM))
                            .when(name == "_id", |this| this.font_family(mono.clone()).text_color(p.text_muted))
                            .when(faint, |this| this.text_color(p.text_faint))
                            .child(SharedString::from(text)),
                    );
                }
                row = row.child(
                    div()
                        .flex_none()
                        .w(px(124.))
                        .flex()
                        .justify_end()
                        .gap(px(1.))
                        .child(doc_icon(format!("mongo-view:{index}"), IconName::Eye, format!("{} {index}", self.text("mongo.tab.view")), &entity))
                        .child(doc_icon(format!("mongo-copy:{index}"), IconName::Copy, format!("{} {index}", self.text("mongo.tab.copy")), &entity))
                        .child(doc_icon(format!("mongo-edit:{index}"), IconName::Pencil, format!("{} {index}", self.text("mongo.tab.edit")), &entity))
                        .child(doc_icon(format!("mongo-delete:{index}"), IconName::Trash, format!("{} {index}", self.text("mongo.tab.remove")), &entity)),
                );
                table = table.child(row);
            }
        }

        let mut right = div().flex_1().min_w(px(0.)).h_full().flex().flex_col().relative().child(toolbar).child(table);

        // ---- 抽屉：查看 / 编辑
        if self.view_open || self.editor_open {
            let mut drawer = div()
                .flex_none()
                .max_h(relative(0.55))
                .flex()
                .flex_col()
                .gap(px(10.))
                .p(px(12.))
                .border_t_1()
                .border_color(p.line)
                .bg(p.chrome);
            if self.view_open {
                let invalid = serde_json::from_str::<serde_json::Value>(&self.view_json).is_err();
                let body = if invalid { format!("{} {}", self.text("mongo.docJson.invalid"), self.view_json) } else { self.view_json.clone() };
                drawer = drawer
                    .child(div().flex().items_center().justify_between().child(kit::eyebrow(SharedString::from(self.text("mongo.tab.viewLabel")), &p)).child(mongo_button("mongo-view-close", Some(IconName::X), &self.text("mongo.tab.close"), Kind::Tool, &entity)))
                    .child(div().id("mongo-view-json").flex_1().min_h(px(0.)).overflow_y_scroll().p(px(12.)).rounded(px(Radius::PANEL)).bg(p.surface).border_1().border_color(p.line).font_family(mono.clone()).text_size(px(TypeScale::SM)).child(SharedString::from(body)));
            }
            if self.editor_open {
                let editor = self.json_editor(window, cx);
                let caption = if self.editor_mode == EditorMode::Insert { self.text("mongo.tab.saveInsert") } else { self.text("mongo.tab.saveReplace") };
                let close_entity = entity.clone();
                drawer = drawer
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(kit::eyebrow(SharedString::from(self.text("mongo.tab.editorLabel")), &p))
                            .child(kit::icon_button("mongo-editor-close", IconName::X, SharedString::from(self.text("mongo.tab.close")), move |_, _, cx| {
                                close_entity.update(cx, |pane, cx| {
                                    pane.editor_open = false;
                                    cx.notify();
                                })
                            })),
                    )
                    .child(div().id("mongo-editor").h(px(220.)).rounded(px(Radius::PANEL)).border_1().border_color(p.line).bg(p.surface).overflow_hidden().child(Editor::new(&editor).h(relative(1.)).bordered(false).aria_label(SharedString::from(self.text("mongo.tab.editorLabel")))))
                    .when(!self.editor_error.is_empty(), |this| this.child(div().text_size(px(TypeScale::SM)).text_color(p.danger).child(SharedString::from(self.editor_error.clone()))))
                    .child(div().flex().justify_end().child(mongo_button("mongo-save", Some(IconName::Check), &caption, Kind::Primary, &entity)));
            }
            right = right.child(drawer);
        }
        if self.confirm_open() {
            let delete = self.confirm_is_delete();
            right = right.child(
                div()
                    .flex_none()
                    .mx(px(10.))
                    .mb(px(8.))
                    .px(px(12.))
                    .py(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .rounded(px(Radius::PANEL))
                    .bg(if delete { p.danger_soft } else { p.warn_soft })
                    .text_color(if delete { p.danger } else { p.warn })
                    .child(kit::icon(IconName::TriangleAlert, 15., if delete { p.danger } else { p.warn }))
                    .child(div().flex_1().min_w(px(0.)).text_size(px(TypeScale::SM)).child(SharedString::from(self.confirm_prompt.clone())))
                    .child(mongo_button("mongo-cancel", None, &i18n::text(self.locale, "common.cancel"), Kind::Secondary, &entity))
                    .child(mongo_button(
                        "mongo-confirm",
                        Some(if delete { IconName::Trash } else { IconName::Check }),
                        &self.confirm_action_text(),
                        if delete { Kind::Danger } else { Kind::Primary },
                        &entity,
                    )),
            );
        }
        let status = if self.error.is_empty() { self.note.clone() } else { self.error.clone() };
        right = right.child(
            div()
                .id("mongo-status")
                .test_support()
                .role(gpui_kit::Role::Status)
                .aria_label(SharedString::from(self.status_line()))
                .flex_none()
                .h(px(m.tree_row_h))
                .px(px(12.))
                .flex()
                .items_center()
                .gap(px(8.))
                .border_t_1()
                .border_color(p.line)
                .text_size(px(TypeScale::SM))
                .text_color(if self.error.is_empty() { p.text_muted } else { p.danger })
                .when(self.busy, |this| this.child(Spinner::new().xsmall().color(p.accent)))
                .child(SharedString::from(status)),
        );

        div().id("mongo-pane").size_full().flex().child(left).child(right)
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Primary,
    Secondary,
    Tool,
    Danger,
}

impl MongoPane {
    fn json_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<EditorState> {
        let value = self.editor_text.clone();
        if let Some((state, last, _)) = self.editor_state.as_mut() {
            let state = state.clone();
            kit::sync_editor(&state, last, &value, window, cx);
            return state;
        }
        let state = cx.new(|cx| EditorState::new(window, cx).language("json").line_number(true).default_value(value.clone()));
        let subscription = cx.subscribe(&state, |this, state, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.focused = Some(Field::Editor);
                this.editor_text = state.read(cx).value().to_string();
                cx.notify();
            }
        });
        self.editor_state = Some((state.clone(), value, subscription));
        state
    }
}

#[allow(clippy::too_many_arguments)]
fn nav_row(id: String, label: String, icon: IconName, text: String, count: String, selected: bool, p: &Palette, m: kit::Metrics, entity: &Entity<MongoPane>) -> AnyElement {
    let entity = entity.clone();
    let command = id.clone();
    kit::interactive_states(div().id(SharedString::from(id)), p, false)
        .test_support()
        .role(gpui_kit::Role::Button)
        .aria_label(SharedString::from(label))
        .aria_selected(selected)
        .h(px(m.tree_row_h))
        .px(px(8.))
        .flex()
        .items_center()
        .gap(px(8.))
        .rounded(px(Radius::CONTROL))
        .when(selected, |this| this.bg(p.selection))
        .when(!selected, |this| this.hover(|style| style.bg(p.hover)))
        .child(kit::icon(icon, 13., if selected { p.accent } else { p.text_faint }))
        .child(div().flex_1().min_w(px(0.)).truncate().text_size(px(TypeScale::SM)).child(SharedString::from(text)))
        .child(div().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(SharedString::from(count)))
        .on_click(move |_, _, cx| {
            let command = command.clone();
            entity.update(cx, |pane, cx| pane.command(&command, cx));
        })
        .into_any_element()
}

fn mongo_button(id: &'static str, icon: Option<IconName>, label: &str, kind: Kind, entity: &Entity<MongoPane>) -> gpui_kit::component::button::Button {
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

fn mongo_icon(id: &'static str, icon: IconName, label: &str, entity: &Entity<MongoPane>) -> gpui_kit::component::button::Button {
    let entity = entity.clone();
    kit::icon_button(id, icon, SharedString::from(label.to_owned()), move |_, _, cx| entity.update(cx, |pane, cx| pane.command(id, cx)))
}

fn doc_icon(id: String, icon: IconName, label: String, entity: &Entity<MongoPane>) -> gpui_kit::component::button::Button {
    let entity = entity.clone();
    let command = id.clone();
    kit::icon_button(SharedString::from(id), icon, SharedString::from(label), move |_, _, cx| {
        let command = command.clone();
        entity.update(cx, |pane, cx| pane.command(&command, cx));
    })
    .xsmall()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_follow_fields_and_invalid_json_stays_local() {
        let docs = vec![
            MongoDoc { id_json: "\"1\"".into(), json: r#"{"_id":"1","姓名":"林夏"}"#.into() },
            MongoDoc { id_json: "\"2\"".into(), json: r#"{"_id":"2","姓名":"周宁","t8flag":true}"#.into() },
        ];
        assert_eq!(columns_of(&docs), vec!["_id", "姓名", "t8flag"]);
        assert_eq!(cell_text(&docs[1].json, "姓名"), "周宁");
        assert_eq!(cell_text(&docs[0].json, "t8flag"), "");
        assert!(quick_validate("{", Locale::ZhCn).unwrap().contains("语法"));
        assert!(quick_validate("[]", Locale::En).unwrap().to_ascii_lowercase().contains("object"));
        assert!(quick_validate("", Locale::ZhCn).is_some());
        assert!(quick_validate(r#"{"status":"active"}"#, Locale::ZhCn).is_none());
        assert_eq!(next_skip(20, 20, -1), 0);
        assert_eq!(next_skip(0, 20, 1), 20);
        assert!(pretty_json(r#"{"a":1}"#).contains('\n'));
        assert_ne!(i18n::text(Locale::En, "mongo.tab.insert"), "mongo.tab.insert");
    }

    #[gpui_kit::test]
    fn mongo_write_is_previewed_and_target_switch_drops_it(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::AppContext as _;
        let dir = std::env::temp_dir().join(format!("rillsql-mongo-reset-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let services = std::sync::Arc::new(crate::application::AppServices::new(std::sync::Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
        let pane = cx.update(|cx| cx.new(|_| MongoPane::new(services)));
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                pane.connection_id = "docs".into();
                pane.connection_name = "文档".into();
                pane.database = "app".into();
                pane.collection = "users".into();
                pane.command("mongo-insert", cx);
                pane.set_editor(r#"{"name":"临时"}"#);
                pane.command("mongo-save", cx);
                assert!(pane.confirm_open(), "新增文档必须先预览确认");
                assert!(pane.confirm_prompt().contains("app.users"), "{}", pane.confirm_prompt());
                assert!(pane.confirm_prompt().contains("临时"), "{}", pane.confirm_prompt());
                pane.reset_target_state();
                pane.database = "app".into();
                pane.collection = "orders".into();
                assert!(!pane.confirm_open(), "换集合不能保留旧写入预览");
                assert!(!pane.editor_open());
                assert!(pane.editor_text().is_empty());
                assert!(pane.documents().is_empty());
            });
        });
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod live;
