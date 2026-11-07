//! MongoDB 集合文档。过滤、新增、替换和删除都先校验；未确认不写库，失败时编辑内容还在。

use std::sync::Arc;

use gpui_kit::{
    ClipboardItem, Context, Entity, FocusHandle, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div, px,
};

use crate::application::mongo::{DeleteOutcome, InsertOutcome, ReplaceOutcome};
use crate::application::AppServices;
use crate::i18n::{self, Locale};
use crate::mongo::docs::{CollectionSummary, DatabaseSummary, DocumentsPage, MongoDoc};

const PAGE_LIMIT: u64 = 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    confirm_prompt: String,
    note: String,
    error: String,
    filter_error: String,
    busy: bool,
    generation: u64,
    copied: String,
    focused: Option<Field>,
    filter_focus: Option<FocusHandle>,
    editor_focus: Option<FocusHandle>,
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
            confirm_prompt: String::new(),
            note: String::new(),
            error: String::new(),
            filter_error: String::new(),
            busy: false,
            generation: 0,
            copied: String::new(),
            focused: None,
            filter_focus: None,
            editor_focus: None,
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
        self.confirm_id.is_some()
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

    pub fn open_spec(&mut self, spec: &str, cx: &mut Context<Self>) {
        let Some((id, name, database, collection)) = parse_spec(spec) else {
            self.connection_id.clear();
            self.show_error(self.text("mongo.tab.needConnection"));
            cx.notify();
            return;
        };
        self.connection_id = id;
        self.connection_name = name;
        self.database = database;
        self.collection = collection;
        self.documents.clear();
        self.note.clear();
        self.error.clear();
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
            self.save_editor(cx);
        } else if let Some(index) = id.strip_prefix("mongo-delete:") {
            self.ask_delete(index);
        } else if id == "mongo-confirm" {
            self.run_delete(cx);
        } else if id == "mongo-cancel" {
            self.confirm_id = None;
            self.confirm_prompt.clear();
        }
        cx.notify();
    }

    fn pick_database(&mut self, index: &str, cx: &mut Context<Self>) {
        let Ok(index) = index.parse::<usize>() else { return };
        let Some(database) = self.databases.get(index) else { return };
        self.database = database.name.clone();
        self.collection.clear();
        self.documents.clear();
        self.columns.clear();
        self.total = 0;
        self.load_collections(cx);
    }

    fn pick_collection(&mut self, index: &str, cx: &mut Context<Self>) {
        let Ok(index) = index.parse::<usize>() else { return };
        let Some(collection) = self.collections.get(index) else { return };
        self.collection = collection.name.clone();
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

    fn save_editor(&mut self, cx: &mut Context<Self>) {
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
        let connections = Arc::clone(&self.services.connections);
        let connection_id = self.connection_id.clone();
        let database = self.database.clone();
        let collection = self.collection.clone();
        let json = self.editor_text.clone();
        match self.editor_mode {
            EditorMode::Insert => self.spawn(
                cx,
                false,
                async move { crate::application::mongo::insert_document(connections.as_ref(), &connection_id, &database, &collection, &json).await },
                Self::finish_insert,
            ),
            EditorMode::Replace => {
                let id_json = self.editor_id.clone();
                if id_json.is_empty() {
                    self.show_error(self.text("mongo.docs.notMatched"));
                    return;
                }
                self.spawn(
                    cx,
                    false,
                    async move { crate::application::mongo::replace_document(connections.as_ref(), &connection_id, &database, &collection, &id_json, &json).await },
                    Self::finish_replace,
                )
            }
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
        self.note.clear();
        self.error.clear();
    }

    fn run_delete(&mut self, cx: &mut Context<Self>) {
        let Some(id_json) = self.confirm_id.clone() else { return };
        self.confirm_id = None;
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

    fn focus_of(&mut self, field: Field, cx: &mut Context<Self>) -> FocusHandle {
        let slot = match field {
            Field::Filter => &mut self.filter_focus,
            Field::Editor => &mut self.editor_focus,
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
            Field::Filter => &mut self.filter_input,
            Field::Editor => &mut self.editor_text,
        };
        match action {
            KeyAction::Ignore => {}
            KeyAction::Backspace => {
                slot.pop();
            }
            KeyAction::Type(text) => slot.push_str(&text),
            KeyAction::Commit => {
                let id = if field == Field::Filter { "mongo-filter" } else { "mongo-save" };
                self.command(id, cx);
            }
        }
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

impl Render for MongoPane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity().clone();
        let filter_focus = self.focus_of(Field::Filter, cx);
        let editor_focus = self.focus_of(Field::Editor, cx);
        let mut column = div().id("mongo-pane").flex().flex_col();
        column = column.child(SharedString::from(format!("{} / {} / {}", self.connection_name, self.database, self.collection)));
        column = column.child(button("mongo-refresh-db", &self.text("mongo.tab.databases"), entity.clone()));
        for (index, database) in self.databases.iter().take(40).enumerate() {
            column = column.child(button_owned(format!("mongo-database:{index}"), format!("{} {}", self.text("mongo.tab.databases"), database.name), entity.clone()));
        }
        for (index, collection) in self.collections.iter().take(80).enumerate() {
            let count = collection.estimated_documents.map(|count| count.to_string()).unwrap_or_else(|| "—".into());
            column = column.child(button_owned(
                format!("mongo-collection:{index}"),
                format!("{} {} {count}", self.text("mongo.tab.collections"), collection.name),
                entity.clone(),
            ));
        }
        column = column.child(input_box("mongo-filter-field", &self.text("mongo.tab.filterLabel"), &self.filter_input, filter_focus, Field::Filter, entity.clone()));
        column = column.child(button("mongo-filter", &self.text("mongo.tab.apply"), entity.clone()));
        column = column.child(button("mongo-clear-filter", &self.text("mongo.tab.clearFilter"), entity.clone()));
        column = column.child(button("mongo-refresh", &self.text("mongo.tab.refresh"), entity.clone()));
        column = column.child(button("mongo-prev", &self.text("mongo.tab.prevPage"), entity.clone()));
        column = column.child(button("mongo-next", &self.text("mongo.tab.nextPage"), entity.clone()));
        column = column.child(button("mongo-insert", &self.text("mongo.tab.insert"), entity.clone()));
        if self.view_open {
            let invalid = serde_json::from_str::<serde_json::Value>(&self.view_json).is_err();
            let body = if invalid { format!("{} {}", self.text("mongo.docJson.invalid"), self.view_json) } else { self.view_json.clone() };
            column = column.child(SharedString::from(body));
            column = column.child(button("mongo-view-close", &self.text("mongo.tab.close"), entity.clone()));
        }
        if self.editor_open {
            let caption = if self.editor_mode == EditorMode::Insert { self.text("mongo.tab.saveInsert") } else { self.text("mongo.tab.saveReplace") };
            column = column.child(input_box("mongo-editor", &self.text("mongo.tab.editorLabel"), &self.editor_text, editor_focus, Field::Editor, entity.clone()));
            column = column.child(button("mongo-save", &caption, entity.clone()));
        }
        if self.confirm_id.is_some() {
            column = column.child(SharedString::from(self.confirm_prompt.clone()));
            column = column.child(button("mongo-confirm", &self.text("mongo.tab.confirmDelete"), entity.clone()));
            column = column.child(button("mongo-cancel", &i18n::text(self.locale, "common.cancel"), entity.clone()));
        }
        column = column.child(SharedString::from(self.columns.join(" | ")));
        if self.documents.is_empty() {
            let empty = if self.applied_filter.is_empty() { self.text("mongo.tab.emptyCollection") } else { self.text("mongo.tab.emptyFiltered") };
            column = column.child(SharedString::from(empty));
        }
        for (index, doc) in self.documents.iter().take(PAGE_LIMIT as usize).enumerate() {
            let cells = self.columns.iter().map(|column| cell_text(&doc.json, column)).collect::<Vec<_>>().join(" | ");
            column = column.child(SharedString::from(cells));
            column = column.child(button_owned(format!("mongo-view:{index}"), format!("{} {index}", self.text("mongo.tab.view")), entity.clone()));
            column = column.child(button_owned(format!("mongo-copy:{index}"), format!("{} {index}", self.text("mongo.tab.copy")), entity.clone()));
            column = column.child(button_owned(format!("mongo-edit:{index}"), format!("{} {index}", self.text("mongo.tab.edit")), entity.clone()));
            column = column.child(button_owned(format!("mongo-delete:{index}"), format!("{} {index}", self.text("mongo.tab.remove")), entity.clone()));
        }
        column = column.child(
            div()
                .id("mongo-status")
                .test_support()
                .role(gpui_kit::Role::Status)
                .aria_label(SharedString::from(self.status_line()))
                .child(SharedString::from(if self.error.is_empty() { self.note.clone() } else { self.error.clone() })),
        );
        column
    }
}

fn button(id: &'static str, label: &str, entity: Entity<MongoPane>) -> impl IntoElement {
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

fn button_owned(id: String, label: String, entity: Entity<MongoPane>) -> impl IntoElement {
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

fn input_box(id: &'static str, caption: &str, value: &str, focus: FocusHandle, field: Field, entity: Entity<MongoPane>) -> impl IntoElement {
    let shown = value.replace('\n', " ");
    let label = SharedString::from(format!("{caption} {shown}"));
    div()
        .id(id)
        .test_support()
        .track_focus(&focus)
        .min_h(px(28.))
        .w(px(420.))
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
}

#[cfg(test)]
mod live;
