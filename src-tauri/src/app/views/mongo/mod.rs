//! MongoDB 集合文档。过滤、新增、替换和删除都先校验；未确认不写库，失败时编辑内容还在。

use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, ClipboardItem, Context, Entity, FocusHandle, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, TestSupportExt as _, Window, div, px,
};

use crate::ui::kit::{self, FieldSpec, Fields, Palette, Radius, TypeScale};
use crate::ui::{EditMode, FormatViewer, FormatViewerEvent, TruncationState, ViewerSpec};

use crate::application::mongo::{DeleteOutcome, InsertOutcome, ReplaceOutcome};
use crate::application::AppServices;
use crate::i18n::{self, Locale};
use crate::mongo::docs::{CollectionSummary, DatabaseSummary, DocumentsPage, MongoDoc};

const PAGE_LIMIT: u64 = 20;
/// 文档表每列的最小宽度：窄窗口靠横向滚动，不把列压到只剩省略号。
const DOC_COL_MIN: f32 = 120.;
/// 行尾查看/复制/编辑/删除四个图标列的宽度。
const DOC_ACTIONS_W: f32 = 124.;
/// 表格视图最多显示的列数；其余字段仍能在整篇文档与 JSON 树里看到。
const DOC_COL_MAX: usize = 7;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Field {
    Filter,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditorMode {
    Insert,
    Replace,
}

/// 文档区六态：没目标、读取中、读取失败、空集合、筛选无命中、有行。
/// 空集合（无数据）与筛选无命中（用户操作后无结果）是两种语义，错误也不再伪装成"没有文档"。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DocsState {
    NeedTarget,
    Loading,
    Error,
    EmptyCollection,
    NoMatch,
    Rows,
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
    /// 数据区错误态的标题键：跟着失败来源走，读文档以外的失败不再套"读取文档失败"。
    error_title: &'static str,
    /// 文档读取本身失败：上一页的行还在也画错误态（带重试），
    /// 不拿旧行或"集合为空"顶替一次读失败。
    read_failed: bool,
    busy: bool,
    generation: u64,
    copied: String,
    focused: Option<Field>,
    fields: Fields<Field>,
    /// None 跟随窗口宽度；Some 是用户手动折叠或展开。
    nav_choice: Option<bool>,
    doc_tree: bool,
    crumb_db: bool,
    crumb_coll: bool,
    viewer: Option<Entity<FormatViewer>>,
    viewer_sub: Option<Subscription>,
    viewer_editable: bool,
    viewport_w: f32,
    /// 文档表格整体只占一个 Tab 停靠点：方向键在格子间移动，Enter 打开当前格。
    cell_focus: Option<FocusHandle>,
    /// 键盘光标所在单元格（行、列下标），鼠标点格子也会同步到这里。
    focused_cell: Option<(usize, usize)>,
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
            error_title: "mongo.tab.loadFailed",
            read_failed: false,
            busy: false,
            generation: 0,
            copied: String::new(),
            focused: None,
            fields: Fields::default(),
            nav_choice: None,
            doc_tree: false,
            crumb_db: false,
            crumb_coll: false,
            viewer: None,
            viewer_sub: None,
            viewer_editable: false,
            viewport_w: 1200.,
            cell_focus: None,
            focused_cell: None,
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

    /// 错误态的标题键，测试用它确认标题跟着失败来源走。
    #[cfg(test)]
    pub fn error_title(&self) -> &'static str {
        self.error_title
    }

    /// 这次错误是否来自文档读取本身。
    #[cfg(test)]
    pub fn read_failed(&self) -> bool {
        self.read_failed
    }

    pub fn copied(&self) -> &str {
        &self.copied
    }

    pub fn view_json(&self) -> &str {
        &self.view_json
    }

    pub fn view_open(&self) -> bool {
        self.view_open
    }

    #[cfg(test)]
    pub fn focused_cell(&self) -> Option<(usize, usize)> {
        self.focused_cell
    }

    #[cfg(test)]
    pub fn docs_state_name(&self) -> &'static str {
        match self.docs_state() {
            DocsState::NeedTarget => "need-target",
            DocsState::Loading => "loading",
            DocsState::Error => "error",
            DocsState::EmptyCollection => "empty",
            DocsState::NoMatch => "no-match",
            DocsState::Rows => "rows",
        }
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
        self.error_title = "mongo.tab.loadFailed";
        self.read_failed = false;
        self.copied.clear();
        self.fields = Fields::default();
        self.nav_choice = None;
        self.doc_tree = false;
        self.crumb_db = false;
        self.crumb_coll = false;
        self.viewer = None;
        self.viewer_sub = None;
        self.viewer_editable = false;
        self.focused_cell = None;
    }

    fn nav_collapsed(&self, width: f32) -> bool {
        self.nav_choice.unwrap_or(width < kit::NARROW_PAGE_WIDTH)
    }

    /// 文档区六态：先分清"没目标 / 读取中 / 读取失败 / 空集合 / 筛选无命中 / 有行"，
    /// 再决定画哪一种；错误不再和"这个集合没有文档"共用同一块。
    fn docs_state(&self) -> DocsState {
        if self.connection_id.is_empty() || self.database.is_empty() || self.collection.is_empty() {
            return DocsState::NeedTarget;
        }
        if self.busy && self.documents.is_empty() {
            return DocsState::Loading;
        }
        // 读文档失败不再分"有没有旧行"：一次失败的读取既不能让上一页的行冒充当前结果，
        // 也不能退化成"集合为空"。
        if !self.error.is_empty() && (self.read_failed || self.documents.is_empty()) {
            return DocsState::Error;
        }
        if self.documents.is_empty() {
            return if self.applied_filter.trim().is_empty() { DocsState::EmptyCollection } else { DocsState::NoMatch };
        }
        DocsState::Rows
    }

    /// 表格视图显示的列：最多 DOC_COL_MAX 列。
    fn shown_columns(&self) -> Vec<String> {
        self.columns.iter().take(DOC_COL_MAX).cloned().collect()
    }

    /// 重试跟随当前目标层级：还没选到集合时重读集合清单，否则重读当前页。
    fn docs_retry_command(&self) -> &'static str {
        if self.database.is_empty() || self.collection.is_empty() { "mongo-refresh-db" } else { "mongo-refresh" }
    }

    /// 文档表格的键盘等价操作：方向键移动光标格，Home/End 到首末列，Enter/空格打开当前格，
    /// Esc 只收起只读查看（编辑中的草稿不能因为一次 Esc 丢掉）。返回是否吃掉了这次按键。
    fn cell_key(&mut self, key: &str, _cx: &mut Context<Self>) -> bool {
        if self.documents.is_empty() {
            return false;
        }
        let rows = self.documents.len();
        let columns = self.shown_columns().len().max(1);
        let (row, column) = self.focused_cell.unwrap_or((0, 0));
        let row = row.min(rows - 1);
        let column = column.min(columns - 1);
        match key {
            "up" => self.focused_cell = Some((row.saturating_sub(1), column)),
            "down" => self.focused_cell = Some(((row + 1).min(rows - 1), column)),
            "left" => self.focused_cell = Some((row, column.saturating_sub(1))),
            "right" => self.focused_cell = Some((row, (column + 1).min(columns - 1))),
            "home" => self.focused_cell = Some((row, 0)),
            "end" => self.focused_cell = Some((row, columns - 1)),
            "enter" | "space" => {
                let Some(column) = self.shown_columns().get(column).cloned() else { return false };
                self.open_cell(&row.to_string(), &column);
            }
            "escape" => {
                if !self.view_open && !self.editor_open {
                    self.focused_cell = None;
                } else if self.view_open && !self.editor_open {
                    self.view_open = false;
                } else {
                    return false;
                }
            }
            _ => return false,
        }
        true
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
        let local = matches!(id, "mongo-cancel" | "mongo-nav-toggle" | "mongo-view-table" | "mongo-view-tree" | "mongo-crumb-db" | "mongo-crumb-coll");
        if self.busy && !local {
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
            if self.viewer_editable {
                if let Some(viewer) = &self.viewer {
                    self.editor_text = viewer.read(cx).draft().to_owned();
                }
            }
            self.ask_save();
        } else if let Some(index) = id.strip_prefix("mongo-delete:") {
            self.ask_delete(index);
        } else if id == "mongo-confirm" {
            self.run_confirm(cx);
        } else if id == "mongo-cancel" {
            self.confirm_id = None;
            self.confirm_write = None;
            self.confirm_prompt.clear();
        } else if id == "mongo-nav-toggle" {
            self.nav_choice = Some(!self.nav_collapsed(self.viewport_w));
        } else if id == "mongo-view-table" {
            self.doc_tree = false;
        } else if id == "mongo-view-tree" {
            self.doc_tree = true;
        } else if id == "mongo-crumb-db" {
            self.crumb_db = !self.crumb_db;
            self.crumb_coll = false;
        } else if id == "mongo-crumb-coll" {
            self.crumb_coll = !self.crumb_coll;
            self.crumb_db = false;
        } else if let Some(index) = id.strip_prefix("mongo-crumb-db:") {
            self.crumb_db = false;
            self.pick_database(index, cx);
        } else if let Some(index) = id.strip_prefix("mongo-crumb-coll:") {
            self.crumb_coll = false;
            self.pick_collection(index, cx);
        } else if let Some(rest) = id.strip_prefix("mongo-cell:") {
            let mut parts = rest.splitn(2, ':');
            let row = parts.next().unwrap_or("");
            let column = parts.next().unwrap_or("");
            self.open_cell(row, column);
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
            Err(message) => self.show_error_as("mongo.tab.listDatabasesFailed", message),
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
            Err(message) => self.show_error_as("mongo.tab.listCollectionsFailed", message),
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
                self.error.clear();
            }
            Err(message) => {
                // 只留一处呈现：数据区的错误态（带重试）。同一句错误不再同时写进筛选条旁边的红条，
                // 否则一次失败会出现两条说明和两个重试入口。
                self.show_error_as("mongo.tab.loadFailed", message);
            }
        }
    }

    fn open_cell(&mut self, index: &str, column: &str) {
        let Some(doc) = self.doc_at(index) else { return };
        let Some(text) = cell_fragment(&doc.json, column) else { return };
        self.view_json = text;
        self.view_open = true;
        self.error.clear();
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
            self.show_error_as("mongo.docs.saveFailed", message);
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
                    self.show_error_as("mongo.docs.saveFailed", self.text("mongo.docs.notMatched"));
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
                self.show_error_as("mongo.docs.saveFailed", message);
            }
        }
    }

    fn finish_replace(&mut self, result: Result<ReplaceOutcome, String>, cx: &mut Context<Self>) {
        match result {
            Ok(outcome) if outcome.matched == 0 => {
                let message = self.text("mongo.docs.notMatched");
                self.editor_error = message.clone();
                self.show_error_as("mongo.docs.saveFailed", message);
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
                self.show_error_as("mongo.docs.saveFailed", message);
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
            Err(message) => self.show_error_as("mongo.docs.deleteFailed", message),
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
        self.show_error_as("mongo.tab.loadFailed", message);
    }

    /// 错误同时进状态条和数据区错误态；标题按失败来源给，
    /// 否则一次写入或清单失败也会被标成"读取文档失败"。
    fn show_error_as(&mut self, title: &'static str, message: String) {
        self.error = message;
        self.error_title = title;
        // 只有"读文档失败"才把数据区整块换成错误态；写入、删除、清单失败不动已有行。
        self.read_failed = title == "mongo.tab.loadFailed";
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
        self.read_failed = false;
        if clear_note {
            self.note.clear();
        }
        self.generation
    }

    pub fn status_line(&self) -> String {
        let columns = self.columns.join(",");
        let names = self.documents.iter().take(8).map(|doc| doc.json.replace('\n', " ")).collect::<Vec<_>>().join(" || ");
        format!(
            "{name} db={db} coll={coll} filter={filter} applied={applied} skip={skip} total={total} columns={columns} note={note} error={error} error_title={error_title} editor={editor} confirm={confirm} copied={copied} view={view} docs={names}",
            name = self.connection_name,
            db = self.database,
            coll = self.collection,
            filter = self.filter_input,
            applied = self.applied_filter,
            skip = self.skip,
            total = self.total,
            note = self.note,
            error = self.error,
            error_title = self.error_title,
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

/// 嵌套对象和数组用 `{N}` / `[N]` 徽标，标量没有徽标。
pub fn cell_badge(json: &str, column: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(json).ok()?;
    match value.get(column)? {
        serde_json::Value::Object(map) => Some(format!("{{{}}}", map.len())),
        serde_json::Value::Array(items) => Some(format!("[{}]", items.len())),
        _ => None,
    }
}

pub fn cell_fragment(json: &str, column: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(json).ok()?;
    let value = value.get(column)?.clone();
    Some(pretty_json(&serde_json::to_string(&value).unwrap_or_default()))
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
        self.viewport_w = f32::from(window.viewport_size().width);
        let collapsed = self.nav_collapsed(self.viewport_w);
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
        let mut nav = div().id("mongo-nav-list").flex().flex_col().gap(px(1.)).flex_1().min_h(px(0.)).overflow_y_scroll().p(px(6.));
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
        let nav_w = m.header_h() * 4. + m.pane_pad();
        // 窄窗口里左栏是覆盖内容区的抽屉：展开时不挤压文档表，页面本身也不会被撑出横向滚动。
        let overlay_nav = !collapsed && self.viewport_w < kit::NARROW_PAGE_WIDTH;
        let panel_w = if overlay_nav { nav_w.min((self.viewport_w - 32.).max(160.)) } else { nav_w };
        let nav_column = div()
            .id("mongo-nav")
            .test_support()
            .flex_none()
            .w(px(if collapsed { 0. } else { panel_w }))
            .overflow_hidden()
            .h_full()
            .flex()
            .flex_col()
            .bg(p.chrome)
            .border_r_1()
            .border_color(p.line)
            .when(overlay_nav, |this| this.absolute().top_0().left_0().bottom_0().shadow_lg())
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
            .child(mongo_icon(
                "mongo-nav-toggle",
                IconName::PanelLeft,
                &self.text(if collapsed { "mongo.nav.expand" } else { "mongo.nav.collapse" }),
                &entity,
            ))
            .child(mongo_button("mongo-view-table", None, &self.text("mongo.view.table"), Kind::Tool, &entity))
            .child(mongo_button("mongo-view-tree", None, &self.text("mongo.view.tree"), Kind::Tool, &entity))
            .child(
                // 面包屑自身允许换行与截断：长库名在 640 宽度下也不会把工具条撑出页面。
                div()
                    .id("mongo-crumb")
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(4.))
                    .min_w(px(0.))
                    .max_w_full()
                    .text_size(px(TypeScale::MD))
                    .child(mongo_button("mongo-crumb-db", None, &self.text("mongo.nav.breadcrumb"), Kind::Tool, &entity))
                    .child(
                        div().max_w(px(220.)).truncate().text_color(p.text_muted).child(SharedString::from(if self.database.is_empty() {
                            self.text("mongo.tab.databases")
                        } else {
                            self.database.clone()
                        })),
                    )
                    .child(kit::icon(IconName::ChevronRight, 12., p.text_faint))
                    .child(mongo_button("mongo-crumb-coll", None, &self.text("mongo.tab.collections"), Kind::Tool, &entity))
                    .child(
                        div()
                            .max_w(px(220.))
                            .truncate()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(SharedString::from(self.collection.clone())),
                    ),
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

        // ---- 文档数据区：状态分派在这里，页面本身不参与横向滚动。
        let state = self.docs_state();
        let cell_focus = self.cell_focus.get_or_insert_with(|| cx.focus_handle().tab_stop(true)).clone();
        let docs_focused = cell_focus.is_focused(window);
        let documents = self.docs_body(state, docs_focused, &mono, &p, &m, &entity, cx);

        let mut right = div().flex_1().min_w(px(0.)).h_full().flex().flex_col().relative().child(toolbar);
        if self.crumb_db || self.crumb_coll {
            let mut menu = div().id("mongo-crumb-menu").flex().flex_col().max_h(px(m.header_h() * 6.)).overflow_y_scroll().p(px(m.pad)).border_b_1().border_color(p.line).bg(p.chrome);
            if self.crumb_db {
                for (index, database) in self.databases.iter().take(60).enumerate() {
                    menu = menu.child(nav_row(
                        format!("mongo-crumb-db:{index}"),
                        format!("{} {}", self.text("mongo.tab.databases"), database.name),
                        IconName::Database,
                        database.name.clone(),
                        database.collections.to_string(),
                        database.name == self.database,
                        &p,
                        m,
                        &entity,
                    ));
                }
            } else {
                for (index, collection) in self.collections.iter().take(120).enumerate() {
                    let count = collection.estimated_documents.map(|count| count.to_string()).unwrap_or_else(|| "—".into());
                    menu = menu.child(nav_row(
                        format!("mongo-crumb-coll:{index}"),
                        format!("{} {}", self.text("mongo.tab.collections"), collection.name),
                        IconName::FolderOpen,
                        collection.name.clone(),
                        count,
                        collection.name == self.collection,
                        &p,
                        m,
                        &entity,
                    ));
                }
            }
            right = right.child(menu);
        }
        right = right.child(documents);

        // ---- 抽屉：查看 / 编辑
        if self.view_open || self.editor_open {
            let editable = self.editor_open;
            let source = if editable { self.editor_text.clone() } else { self.view_json.clone() };
            let viewer = self.ensure_document_viewer(&source, editable, window, cx);
            let caption = if self.editor_mode == EditorMode::Insert { self.text("mongo.tab.saveInsert") } else { self.text("mongo.tab.saveReplace") };
            let close_entity = entity.clone();
            let mut drawer = div()
                .flex_none()
                .h(px(m.header_h() * 6.))
                .flex()
                .flex_col()
                .gap(px(m.pad))
                .p(px(m.card_pad()))
                .border_t_1()
                .border_color(p.line)
                .bg(p.chrome)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(kit::eyebrow(SharedString::from(self.text(if editable { "mongo.tab.editorLabel" } else { "mongo.tab.viewLabel" })), &p))
                        .child(mongo_button("mongo-view-close", Some(IconName::X), &self.text("mongo.tab.close"), Kind::Tool, &entity))
                        .when(editable, |this| {
                            this.child(kit::icon_button("mongo-editor-close", IconName::X, SharedString::from(self.text("mongo.tab.close")), move |_, _, cx| {
                                close_entity.update(cx, |pane, cx| {
                                    pane.editor_open = false;
                                    cx.notify();
                                })
                            }))
                        }),
                )
                .child(div().id("mongo-view-json").flex_1().min_h(px(0.)).overflow_hidden().child(viewer));
            if editable {
                drawer = drawer
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
        // 状态条：错误优先，其次是操作回执，读取中显示进度文案；读取进度在数据区还有局部/骨架两档。
        let status = if !self.error.is_empty() {
            self.error.clone()
        } else if !self.note.is_empty() {
            self.note.clone()
        } else if self.busy {
            self.text("mongo.tab.loading")
        } else {
            String::new()
        };
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

        // 页面根只负责分栏：不滚横向、不撑宽，滚动全部交给左栏或文档数据区自己。
        let mut root = div().id("mongo-pane").test_support().relative().size_full().flex().min_w(px(0.)).overflow_hidden();
        if overlay_nav {
            // 窄窗口的抽屉：先画内容，再压一层遮罩与左栏，点遮罩收回导航。
            let dismiss = entity.clone();
            root = root.child(right).child(
                div()
                    .id("mongo-nav-backdrop")
                    .test_support()
                    .absolute()
                    .inset_0()
                    .occlude()
                    .bg(gpui_kit::hsla(0., 0., 0., 0.22))
                    .on_click(move |_, _, cx| {
                        dismiss.update(cx, |pane, cx| {
                            pane.nav_choice = Some(true);
                            cx.notify();
                        });
                    }),
            ).child(nav_column);
        } else {
            root = root.child(nav_column).child(right);
        }
        root
    }
}

impl MongoPane {
    /// 文档数据区：上面是错误/刷新提示条，下面按六态分派；滚动只发生在这一块内部
    /// （纵向常驻，横向只在"有行"且列宽不足时出现），页面本身不滚横向。
    #[allow(clippy::too_many_arguments)]
    fn docs_body(
        &mut self,
        state: DocsState,
        docs_focused: bool,
        mono: &SharedString,
        p: &Palette,
        m: &kit::Metrics,
        entity: &Entity<Self>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let cell_focus = self.cell_focus.clone();
        let mut area = div()
            .id("mongo-docs")
            .test_support()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .min_w(px(0.))
            .on_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, _, cx| {
                let key = match event.keystroke.key.as_str() {
                    "arrowup" => "up",
                    "arrowdown" => "down",
                    "arrowleft" => "left",
                    "arrowright" => "right",
                    other => other,
                };
                if this.cell_key(key, cx) {
                    cx.notify();
                    cx.stop_propagation();
                }
            }));
        if let Some(handle) = cell_focus {
            area = area.track_focus(&handle);
        }
        area = if matches!(state, DocsState::Rows) { area.overflow_y_scroll().overflow_x_scroll() } else { area.overflow_hidden() };
        // 读失败只由下面的错误态呈现：这里不再另画一条同样的红条和第二个重试按钮。
        // 已有内容时的局部刷新：一行局部加载，不把整块换成首屏骨架。
        if self.busy && matches!(state, DocsState::Rows) {
            area = area.child(
                div()
                    .id("mongo-docs-busy")
                    .test_support()
                    .flex_none()
                    .px(px(12.))
                    .py(px(6.))
                    .border_b_1()
                    .border_color(p.line)
                    .child(kit::inline_loading(self.text("mongo.tab.loading"), m, p)),
            );
        }
        if matches!(state, DocsState::Rows) {
            area = area.child(if self.doc_tree { self.document_tree(p, m, entity) } else { self.docs_table(docs_focused, mono, p, m, entity) });
        } else {
            area = area.child(
                div()
                    .id("mongo-docs-state")
                    .test_support()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.))
                    .min_w(px(0.))
                    .items_start()
                    .justify_center()
                    .px(px(m.pane_pad()))
                    .py(px(m.pad))
                    .child(self.docs_state_block(state, p, m, entity)),
            );
        }
        area.into_any_element()
    }

    /// 六态里除"有行"之外的呈现：说明 + 下一步；主操作进状态块的动作位，不另起一行居中追加。
    fn docs_state_block(&self, state: DocsState, p: &Palette, m: &kit::Metrics, entity: &Entity<Self>) -> AnyElement {
        match state {
            DocsState::NeedTarget if self.connection_id.is_empty() => kit::empty_state_with_action(
                IconName::Database,
                self.text("mongo.tab.needConnection"),
                self.text("mongo.tab.needConnectionHint"),
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(kit::kbd(kit::shortcut("Ctrl+B"), p))
                    .child(div().text_size(px(TypeScale::SM)).text_color(p.text_muted).child(self.text("shell.command.toggleSidebar"))),
                p,
            )
            .into_any_element(),
            DocsState::NeedTarget => kit::empty_state_with_action(
                IconName::FolderOpen,
                self.text("mongo.tab.needCollection"),
                self.text("mongo.tab.needCollectionHint"),
                mongo_button_for("mongo-docs-reload", "mongo-refresh-db", Some(IconName::RefreshCw), &self.text("mongo.tab.refreshDatabases"), Kind::Secondary, entity),
                p,
            )
            .into_any_element(),
            DocsState::Loading => kit::skeleton_state(6, m, p).into_any_element(),
            DocsState::Error => kit::error_state_with_action(
                self.text(self.error_title),
                self.error.clone(),
                mongo_button_for("mongo-docs-retry", self.docs_retry_command(), Some(IconName::RefreshCw), &self.text("common.retry"), Kind::Secondary, entity),
                p,
            )
            .into_any_element(),
            DocsState::EmptyCollection => kit::empty_state_with_action(
                IconName::Braces,
                self.text("mongo.tab.emptyCollection"),
                self.text("mongo.tab.emptyCollectionHint"),
                mongo_button_for("mongo-empty-insert", "mongo-insert", Some(IconName::Plus), &self.text("mongo.tab.insert"), Kind::Primary, entity),
                p,
            )
            .into_any_element(),
            DocsState::NoMatch => {
                let detail = i18n::fill(&self.text("mongo.tab.emptyFilteredHint"), &[("filter", &self.applied_filter)]);
                kit::no_match_state_with_action(
                    IconName::Funnel,
                    self.text("mongo.tab.emptyFiltered"),
                    detail,
                    mongo_button_for("mongo-no-match-clear", "mongo-clear-filter", None, &self.text("mongo.tab.clearFilter"), Kind::Secondary, entity),
                    p,
                )
                .into_any_element()
            }
            DocsState::Rows => div().into_any_element(),
        }
    }

    /// 表格视图：表头与行共用同一组列宽，行尾四个图标仍是查看/复制/编辑/删除。
    fn docs_table(&self, docs_focused: bool, mono: &SharedString, p: &Palette, m: &kit::Metrics, entity: &Entity<Self>) -> AnyElement {
        let shown = self.shown_columns();
        // 窄窗口下每列至少 120：不够宽就在数据区内部横向滚动，不把列压成全是省略号。
        let row_min_w = shown.len() as f32 * DOC_COL_MIN + DOC_ACTIONS_W + m.pane_pad();
        let focus_handle = self.cell_focus.clone();
        let mut table = div().id("mongo-doc-table").test_support().flex().flex_col().flex_none().min_w(px(row_min_w));
        let mut head = div()
            .flex()
            .flex_none()
            .min_w(px(row_min_w))
            .h(px(m.bar_h() - 2.))
            .px(px(10.))
            .items_center()
            .bg(p.chrome)
            .border_b_1()
            .border_color(p.line)
            .text_size(px(TypeScale::XS))
            .text_color(p.text_faint);
        for name in &shown {
            head = head.child(div().flex_1().flex_basis(px(0.)).min_w(px(0.)).truncate().pr(px(8.)).child(SharedString::from(name.clone())));
        }
        head = head.child(div().flex_none().w(px(DOC_ACTIONS_W)));
        table = table.child(head);
        for (index, doc) in self.documents.iter().take(PAGE_LIMIT as usize).enumerate() {
            let mut row = div()
                .id(SharedString::from(format!("mongo-row-{index}")))
                .test_support()
                .group("mongo-row")
                .flex()
                .flex_none()
                .min_w(px(row_min_w))
                .min_h(px(m.row_h + 2.))
                .px(px(10.))
                .items_center()
                .border_b_1()
                .border_color(p.line_soft)
                .hover(|style| style.bg(p.hover));
            for (column_index, name) in shown.iter().enumerate() {
                let focused = docs_focused && self.focused_cell == Some((index, column_index));
                row = row.child(doc_cell(
                    index,
                    column_index,
                    name,
                    cell_text(&doc.json, name),
                    cell_badge(&doc.json, name),
                    mono,
                    focus_handle.clone(),
                    focused,
                    p,
                    entity,
                ));
            }
            row = row.child(
                div()
                    .flex_none()
                    .w(px(DOC_ACTIONS_W))
                    .flex()
                    .justify_end()
                    .gap(px(1.))
                    .child(doc_icon(format!("mongo-view:{index}"), IconName::Eye, format!("{} {index}", self.text("mongo.tab.view")), entity))
                    .child(doc_icon(format!("mongo-copy:{index}"), IconName::Copy, format!("{} {index}", self.text("mongo.tab.copy")), entity))
                    .child(doc_icon(format!("mongo-edit:{index}"), IconName::Pencil, format!("{} {index}", self.text("mongo.tab.edit")), entity))
                    .child(doc_icon(format!("mongo-delete:{index}"), IconName::Trash, format!("{} {index}", self.text("mongo.tab.remove")), entity)),
            );
            table = table.child(row);
        }
        table.into_any_element()
    }
}

/// 单元格：hover / 按下 / 焦点环 / 光标统一走底座，点击与 Enter 都按字段名打开片段。
/// 单元格不逐个进 Tab 顺序（文档表整体是一个停靠点），键盘用方向键在格子间移动。
#[allow(clippy::too_many_arguments)]
fn doc_cell(
    row: usize,
    column_index: usize,
    column: &str,
    text: String,
    badge: Option<String>,
    mono: &SharedString,
    focus_handle: Option<FocusHandle>,
    focused: bool,
    p: &Palette,
    entity: &Entity<MongoPane>,
) -> AnyElement {
    let id = format!("mongo-cell:{row}:{column}");
    let faint = text == "null" || text.is_empty();
    let mut cell = div()
        .id(SharedString::from(id.clone()))
        .test_support()
        .role(gpui_kit::Role::Button)
        .aria_label(SharedString::from(format!("{column} {text}")))
        .flex_1()
        .flex_basis(px(0.))
        .min_w(px(0.))
        .pr(px(8.))
        .h_full()
        .flex()
        .items_center()
        .gap(px(4.))
        .rounded(px(Radius::CONTROL))
        .text_size(px(TypeScale::SM))
        .when(column == "_id", |this| this.font_family(mono.clone()).text_color(p.text_muted))
        .when(faint && badge.is_none(), |this| this.text_color(p.text_faint));
    if let Some(badge) = badge {
        cell = cell.child(kit::pill(SharedString::from(badge), p.accent_text, p.accent_soft));
    }
    cell = cell.child(div().flex_1().min_w(px(0.)).truncate().child(SharedString::from(text)));
    let mut cell = kit::interactive(cell, p, false).tab_stop(false);
    if focused {
        cell = cell.shadow(vec![kit::focus_ring(p.accent, kit::FOCUS_RING_W)]);
    }
    let owner = entity.clone();
    let command = id;
    cell.on_click(move |_, window, cx| {
        if let Some(handle) = focus_handle.clone() {
            window.focus(&handle, cx);
        }
        let command = command.clone();
        owner.update(cx, |pane, cx| {
            pane.focused_cell = Some((row, column_index));
            pane.command(&command, cx);
        });
    })
    .into_any_element()
}

#[derive(Clone, Copy)]
enum Kind {
    Primary,
    Secondary,
    Tool,
    Danger,
}

impl MongoPane {
    /// 查看用只读 FormatViewer，新增和替换用可编辑的。应用只进入确认，不直接写库。
    fn ensure_document_viewer(&mut self, text: &str, editable: bool, window: &mut Window, cx: &mut Context<Self>) -> Entity<FormatViewer> {
        let mode = if editable { EditMode::Editable } else { EditMode::ReadOnly };
        if self.viewer.is_none() || self.viewer_editable != editable {
            let spec = ViewerSpec {
                text: text.to_owned(),
                column_hint: Some("document".into()),
                type_hint: Some("json".into()),
                bytes: None,
                truncation: TruncationState::Full,
                mode,
                locale: self.locale,
            };
            let viewer = cx.new(|cx| FormatViewer::new(spec, window, cx));
            let sub = cx.subscribe(&viewer, |this, _, event, cx| {
                if let FormatViewerEvent::Commit(body) = event {
                    if !this.viewer_editable {
                        return;
                    }
                    this.editor_text = body.clone();
                    this.ask_save();
                    cx.notify();
                }
            });
            self.viewer_sub = Some(sub);
            self.viewer = Some(viewer);
            self.viewer_editable = editable;
        }
        let viewer = self.viewer.clone().expect("document viewer");
        let source = viewer.read(cx).source().to_owned();
        let draft = viewer.read(cx).draft().to_owned();
        if editable && text == source && draft != text {
            self.editor_text = draft;
        } else         if text != source && text != draft {
            viewer.update(cx, |viewer, cx| viewer.replace_full(text.to_owned(), None, window, cx));
        }
        viewer
    }

    fn document_tree(&self, p: &Palette, m: &kit::Metrics, entity: &Entity<Self>) -> AnyElement {
        // 空/加载/错误已在文档数据区的六态里处理，树视图只负责"有行"时的排版。
        let mut tree = div().id("mongo-doc-tree").test_support().flex().flex_col().flex_1().min_h(px(0.)).min_w(px(0.)).overflow_y_scroll();
        for (index, doc) in self.documents.iter().take(PAGE_LIMIT as usize).enumerate() {
            let parsed = serde_json::from_str::<serde_json::Value>(&doc.json).ok();
            let nodes = parsed.as_ref().map(|value| crate::ui::visible_json_nodes(value, &std::collections::BTreeSet::new(), "")).unwrap_or_default();
            let mut block = div()
                .id(SharedString::from(format!("mongo-row-{index}")))
                .test_support()
                .flex()
                .flex_col()
                .gap(px(2.))
                .px(px(m.pane_pad()))
                .py(px(m.pad))
                .border_b_1()
                .border_color(p.line_soft);
            block = block.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .child(div().flex_1().truncate().text_size(px(TypeScale::SM)).child(SharedString::from(cell_text(&doc.json, "_id"))))
                    .child(doc_icon(format!("mongo-view:{index}"), IconName::Eye, format!("{} {index}", self.text("mongo.tab.view")), entity))
                    .child(doc_icon(format!("mongo-copy:{index}"), IconName::Copy, format!("{} {index}", self.text("mongo.tab.copy")), entity))
                    .child(doc_icon(format!("mongo-edit:{index}"), IconName::Pencil, format!("{} {index}", self.text("mongo.tab.edit")), entity))
                    .child(doc_icon(format!("mongo-delete:{index}"), IconName::Trash, format!("{} {index}", self.text("mongo.tab.remove")), entity)),
            );
            for node in nodes.into_iter().take(40) {
                let badge = node.badge.clone().unwrap_or_default();
                let label = if node.scalar.as_ref().is_some() {
                    format!("{} {}", node.key, node.scalar.unwrap_or_default())
                } else {
                    format!("{} {badge}", node.key)
                };
                block = block.child(
                    div()
                        .pl(px(m.pane_pad() * node.depth as f32))
                        .text_size(px(TypeScale::SM))
                        .text_color(p.text_muted)
                        .child(SharedString::from(label)),
                );
            }
            tree = tree.child(block);
        }
        tree.into_any_element()
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
    mongo_button_for(id, id, icon, label, kind, entity)
}

/// 元素 id 与命令分开：同一个命令在工具条和状态块里各有一个入口，元素 id 不能重复。
fn mongo_button_for(
    id: &'static str,
    command: &'static str,
    icon: Option<IconName>,
    label: &str,
    kind: Kind,
    entity: &Entity<MongoPane>,
) -> gpui_kit::component::button::Button {
    let entity = entity.clone();
    let click = move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut gpui_kit::App| entity.update(cx, |pane, cx| pane.command(command, cx));
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
    use gpui_kit::test::TestWindowExt as _;

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
        let nested = r#"{"_id":"1","地址":{"城市":"杭州","区":"西湖"},"标签":["加急"]}"#;
        assert_eq!(cell_badge(nested, "地址").as_deref(), Some("{2}"));
        assert_eq!(cell_badge(nested, "标签").as_deref(), Some("[1]"));
        assert!(cell_badge(nested, "_id").is_none());
        assert!(cell_fragment(nested, "地址").unwrap().contains("杭州"));
        assert_eq!(next_skip(20, 20, -1), 0);
        assert_eq!(next_skip(0, 20, 1), 20);
        assert!(pretty_json(r#"{"a":1}"#).contains('\n'));
        assert_ne!(i18n::text(Locale::En, "mongo.tab.insert"), "mongo.tab.insert");
    }

    /// 窄窗口下文档表不能把每列压成省略号：列宽有下限，超出部分横向滚动。
    #[gpui_kit::test]
    fn narrow_pane_keeps_document_columns_readable(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::test::TestWindowExt;
        use gpui_kit::AppContext as _;
        let dir = std::env::temp_dir().join(format!("rillsql-mongo-narrow-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let services = std::sync::Arc::new(crate::application::AppServices::new(std::sync::Arc::new(
            crate::connections::Service::new_for_test(dir.join("connections.sqlite")),
        )));
        cx.update(gpui_kit::init);
        let (window, pane) = cx.update(|cx| {
            let bounds = gpui_kit::Bounds {
                origin: gpui_kit::Point::default(),
                size: gpui_kit::size(gpui_kit::px(640.), gpui_kit::px(900.)),
            };
            let (window, pane) = gpui_kit::open_window(
                gpui_kit::WindowOptions {
                    window_bounds: Some(gpui_kit::WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                cx,
                |_window, cx| cx.new(|_| MongoPane::new(services)),
            )
            .expect("mongo pane");
            pane.update(cx, |pane, cx| {
                pane.connection_id = "docs".into();
                pane.connection_name = "文档".into();
                pane.database = "app".into();
                pane.collection = "orders".into();
                pane.columns = vec!["_id".into(), "customer".into(), "status".into(), "total".into(), "memo".into(), "created".into(), "note".into()];
                pane.documents = vec![MongoDoc {
                    id_json: "\"1\"".into(),
                    json: r#"{"_id":"1","customer":"c1","status":"active","total":17.5,"memo":null,"created":"2026-11-11","note":null}"#.into(),
                }];
                cx.notify();
            });
            (window, pane)
        });
        let row_w = cx
            .update_window(window, |_, window, cx| {
                window.render_frame(cx);
                f32::from(window.find("mongo-row-0").bounds().size.width)
            })
            .unwrap();
        let wanted = 7. * DOC_COL_MIN + DOC_ACTIONS_W;
        assert!(row_w > 640., "窄窗口里一行要比窗口宽，交给横向滚动：{row_w}");
        assert!(row_w >= wanted, "每列至少 {DOC_COL_MIN}、动作列 {DOC_ACTIONS_W}：{row_w} < {wanted}");
        // 横向滚动只发生在文档数据区内部：页面根与数据区都不超过窗口宽。
        let (page, area, nav_hidden) = cx
            .update_window(window, |_, window, _| {
                (
                    f32::from(window.find("mongo-pane").bounds().size.width),
                    f32::from(window.find("mongo-docs").bounds().size.width),
                    f32::from(window.find("mongo-nav").bounds().size.width) <= 1.5,
                )
            })
            .unwrap();
        assert!(page <= 640.5, "页面本身不能出现横向滚动：{page}");
        assert!(area <= 640.5, "文档数据区按窗口宽裁切：{area}");
        assert!(nav_hidden, "窄窗口默认收起左栏");
        let _ = pane;
        let _ = std::fs::remove_dir_all(&dir);
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

    fn pane_services(tag: &str) -> (std::path::PathBuf, std::sync::Arc<crate::application::AppServices>) {
        let dir = std::env::temp_dir().join(format!("rillsql-mongo-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let services = std::sync::Arc::new(crate::application::AppServices::new(std::sync::Arc::new(
            crate::connections::Service::new_for_test(dir.join("connections.sqlite")),
        )));
        (dir, services)
    }

    fn open_pane_at(
        cx: &mut gpui_kit::TestAppContext,
        services: std::sync::Arc<crate::application::AppServices>,
        width: f32,
    ) -> (gpui_kit::AnyWindowHandle, gpui_kit::Entity<MongoPane>) {
        use gpui_kit::AppContext as _;
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            let bounds = gpui_kit::Bounds {
                origin: gpui_kit::Point::default(),
                size: gpui_kit::size(gpui_kit::px(width), gpui_kit::px(900.)),
            };
            gpui_kit::open_window(
                gpui_kit::WindowOptions { window_bounds: Some(gpui_kit::WindowBounds::Windowed(bounds)), ..Default::default() },
                cx,
                |_window, cx| cx.new(|_| MongoPane::new(services)),
            )
            .expect("mongo pane")
        })
    }

    fn doc(id: &str, customer: &str, status: &str) -> MongoDoc {
        MongoDoc {
            id_json: format!("\"{id}\""),
            json: format!(r#"{{"_id":"{id}","customer":"{customer}","status":"{status}"}}"#),
        }
    }

    fn render(window: gpui_kit::AnyWindowHandle, cx: &mut gpui_kit::TestAppContext) {
        cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
    }

    /// 单元格的 hover / 按下 / 焦点环 / 禁用取值来自底座，不是页面自造；
    /// 单元格不进 Tab 顺序，整张文档表是一个停靠点（见 docs_body）。
    #[test]
    fn document_cells_take_their_states_from_the_kit() {
        for p in [Palette::light(), Palette::dark()] {
            let style = kit::interaction_style(&p, false);
            assert_eq!(style.hover_bg, Some(p.hover));
            assert_eq!(style.active_bg, Some(p.active));
            assert_eq!(style.ring, Some(p.accent), "焦点环用强调色");
            assert_eq!(style.ring_width, kit::FOCUS_RING_W, "焦点环 2px");
            assert!(style.focusable);
            let disabled = kit::interaction_style(&p, true);
            assert!(disabled.hover_bg.is_none() && disabled.active_bg.is_none() && disabled.ring.is_none() && !disabled.focusable);
        }
        assert_eq!(DOC_COL_MAX, 7, "表格最多七列，其余字段在整篇文档里看");
    }

    #[gpui_kit::test]
    fn docs_states_separate_empty_no_match_and_error(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::AppContext as _;
        let (dir, services) = pane_services("states");
        let pane = cx.update(|cx| cx.new(|_| MongoPane::new(services)));
        cx.update(|cx| {
            pane.update(cx, |pane, _| {
                assert_eq!(pane.docs_state_name(), "need-target", "没连接时先给出下一步");
                pane.connection_id = "docs".into();
                pane.database = "app".into();
                pane.collection = "users".into();
                assert_eq!(pane.docs_state_name(), "empty");
                pane.applied_filter = r#"{"status":"active"}"#.into();
                assert_eq!(pane.docs_state_name(), "no-match", "筛选无命中与空集合必须分开");
                pane.busy = true;
                assert_eq!(pane.docs_state_name(), "loading", "读取中优先于空态");
                pane.busy = false;
                pane.show_error("连接已断开".into());
                assert_eq!(pane.docs_state_name(), "error", "读取失败不能退化成空集合");
                pane.error.clear();
                pane.plant_documents("app", "users", vec![doc("1", "c1", "active")]);
                assert_eq!(pane.docs_state_name(), "rows");
                assert_eq!(pane.focused_cell(), None, "换目标后键盘光标回到未定位");
            });
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[gpui_kit::test]
    fn error_state_replaces_the_body_and_offers_retry(cx: &mut gpui_kit::TestAppContext) {
        let (dir, services) = pane_services("error-state");
        let (window, pane) = open_pane_at(cx, services, 900.);
        cx.update(|cx| {
            pane.update(cx, |pane, _| {
                pane.connection_id = "docs".into();
                pane.connection_name = "文档".into();
                pane.database = "app".into();
                pane.collection = "users".into();
                pane.show_error("连接已断开".into());
            });
        });
        render(window, cx);
        let (error, empty, retry) = cx
            .update_window(window, |_, window, _| {
                (
                    window.try_find("error-state").is_some(),
                    window.try_find("empty-state").is_some(),
                    window.try_find("mongo-docs-retry").is_some(),
                )
            })
            .unwrap();
        assert!(error, "读取失败要画错误态");
        assert!(!empty, "错误态不能同时画成空集合");
        assert!(retry, "错误态的动作位是重试");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 一次读失败只出现一次：数据区错误态的那个重试是唯一入口，筛选条旁不再另画一条同样的红条。
    /// 标题跟着失败来源走，写入/删除失败不会被标成"读取文档失败"。
    #[gpui_kit::test]
    fn a_find_failure_is_reported_once_with_a_source_title(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::test::TestQueryExt as _;
        let (dir, services) = pane_services("find-failed");
        let (window, pane) = open_pane_at(cx, services, 900.);
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                pane.plant_documents("app", "orders", vec![doc("1", "c1", "active")]);
                // 真实的读文档失败路径：读取回调带 Err 回来，而不是直接往面板里塞一句错误。
                pane.finish_find(Err("过滤条件语法有误".into()), cx);
            });
        });
        render(window, cx);
        let (error, retry, retries, bars, detail) = cx
            .update_window(window, |_, window, _| {
                (
                    window.try_find("error-state").is_some(),
                    window.try_find("mongo-docs-retry").is_some(),
                    window.find_all_by_label("重试").len(),
                    window.try_find("mongo-filter-error").is_some(),
                    window.try_find("error-state-detail").is_some(),
                )
            })
            .unwrap();
        assert!(error && retry, "读失败要画错误态并给出重试");
        assert_eq!(retries, 1, "一次失败只能有一个重试入口");
        assert!(!bars, "读失败不再另写一条筛选条红条");
        // 错误原文只有错误态一处：面板里没有第二个字段再存它一遍。
        assert!(detail, "错误原文放在错误态的说明位里");
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert!(pane.read_failed(), "读失败要记在读取来源上");
            assert_eq!(pane.error_title(), "mongo.tab.loadFailed", "读失败标题仍是读文档");
            assert_eq!(pane.status_line().matches("过滤条件语法有误").count(), 1, "{}", pane.status_line());
        });
        // 同一条错误通道，失败来源不是读取时标题跟着换：删除失败不能叫"读取文档失败"。
        cx.update(|cx| {
            pane.update(cx, |pane, cx| pane.finish_delete(Err("连接已断开".into()), cx));
        });
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert_eq!(pane.error_title(), "mongo.docs.deleteFailed", "{}", pane.status_line());
            assert!(!pane.read_failed(), "删除失败不是读文档失败");
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[gpui_kit::test]
    fn filtered_collection_without_match_offers_clearing_the_filter(cx: &mut gpui_kit::TestAppContext) {
        let (dir, services) = pane_services("no-match");
        let (window, pane) = open_pane_at(cx, services, 900.);
        cx.update(|cx| {
            pane.update(cx, |pane, _| {
                pane.connection_id = "docs".into();
                pane.connection_name = "文档".into();
                pane.database = "app".into();
                pane.collection = "users".into();
                pane.applied_filter = r#"{"status":"blocked"}"#.into();
            });
        });
        render(window, cx);
        let (no_match, clear, empty) = cx
            .update_window(window, |_, window, _| {
                (
                    window.try_find("no-match-state").is_some(),
                    window.try_find("mongo-no-match-clear").is_some(),
                    window.try_find("empty-state").is_some(),
                )
            })
            .unwrap();
        assert!(no_match, "筛选无命中用无命中空态");
        assert!(clear, "无命中的下一步是清除过滤");
        assert!(!empty, "无命中不是无数据空态");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[gpui_kit::test]
    fn clicking_a_cell_opens_that_column_value(cx: &mut gpui_kit::TestAppContext) {
        let (dir, services) = pane_services("cell-click");
        let (window, pane) = open_pane_at(cx, services, 900.);
        cx.update(|cx| {
            pane.update(cx, |pane, _| {
                pane.plant_documents("app", "orders", vec![doc("1", "c1", "active")]);
            });
        });
        render(window, cx);
        cx.update_window(window, |_, window, cx| window.click("mongo-cell:0:customer", cx)).unwrap();
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert!(pane.view_json().contains("c1"), "点单元格要看到这条字段的值：{}", pane.view_json());
            assert!(pane.view_open(), "点单元格直接打开值，不是点了没反应");
            assert_eq!(pane.focused_cell(), Some((0, 1)), "鼠标点格子同步键盘光标");
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[gpui_kit::test]
    fn keyboard_moves_between_cells_and_enter_opens_the_focused_one(cx: &mut gpui_kit::TestAppContext) {
        let (dir, services) = pane_services("cell-keys");
        let (window, pane) = open_pane_at(cx, services, 900.);
        cx.update(|cx| {
            pane.update(cx, |pane, _| {
                pane.plant_documents("app", "orders", vec![doc("1", "c1", "active"), doc("2", "c2", "idle")]);
            });
        });
        render(window, cx);
        let handle = cx.update(|cx| pane.read(cx).cell_focus.clone()).expect("文档区焦点句柄");
        cx.update_window(window, |_, window, cx| window.focus(&handle, cx)).unwrap();
        cx.update_window(window, |_, window, cx| window.press("down", cx)).unwrap();
        cx.update(|cx| assert_eq!(pane.read(cx).focused_cell(), Some((1, 0))));
        cx.update_window(window, |_, window, cx| window.press("right", cx)).unwrap();
        cx.update(|cx| assert_eq!(pane.read(cx).focused_cell(), Some((1, 1))));
        cx.update_window(window, |_, window, cx| window.press("end", cx)).unwrap();
        cx.update(|cx| assert_eq!(pane.read(cx).focused_cell(), Some((1, 2)), "End 到最后一列"));
        cx.update_window(window, |_, window, cx| window.press("left", cx)).unwrap();
        cx.update_window(window, |_, window, cx| window.press("enter", cx)).unwrap();
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert!(pane.view_json().contains("c2"), "Enter 打开当前格：{}", pane.view_json());
            assert!(pane.view_open());
        });
        cx.update_window(window, |_, window, cx| window.press("escape", cx)).unwrap();
        cx.update(|cx| assert!(!pane.read(cx).view_open(), "Esc 先收起只读查看"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 窄窗口展开左栏不能挤压文档表：左栏覆盖内容区，页面本身仍然没有横向滚动。
    #[gpui_kit::test]
    fn narrow_navigation_overlays_instead_of_squeezing_the_table(cx: &mut gpui_kit::TestAppContext) {
        let (dir, services) = pane_services("nav-overlay");
        let (window, pane) = open_pane_at(cx, services, 640.);
        cx.update(|cx| {
            pane.update(cx, |pane, _| {
                pane.plant_documents("app", "orders", vec![doc("1", "c1", "active")]);
            });
        });
        render(window, cx);
        let (page, before) = cx
            .update_window(window, |_, window, _| {
                (
                    f32::from(window.find("mongo-pane").bounds().size.width),
                    f32::from(window.find("mongo-docs").bounds().size.width),
                )
            })
            .unwrap();
        assert!((page - 640.).abs() < 1., "页面根不随内容撑宽：{page}");
        cx.update_window(window, |_, window, cx| window.click("mongo-nav-toggle", cx)).unwrap();
        render(window, cx);
        let (overlay, backdrop, after) = cx
            .update_window(window, |_, window, _| {
                (
                    window.try_find("mongo-nav").is_some(),
                    window.try_find("mongo-nav-backdrop").is_some(),
                    f32::from(window.find("mongo-docs").bounds().size.width),
                )
            })
            .unwrap();
        assert!(overlay && backdrop, "窄窗口展开左栏是带遮罩的抽屉");
        let label = cx.update_window(window, |_, window, _| window.find("mongo-nav-toggle").label().map(|text| text.to_owned())).unwrap();
        assert_eq!(label.as_deref(), Some("折叠导航"), "左栏展开后按钮说明下一步是折叠");
        assert!((after - before).abs() < 2., "抽屉覆盖内容区，文档区宽度不变：{before} → {after}");
        cx.update_window(window, |_, window, cx| window.click("mongo-nav-backdrop", cx)).unwrap();
        render(window, cx);
        cx.update_window(window, |_, window, _| assert!(window.try_find("mongo-nav-backdrop").is_none(), "点遮罩收回导航")).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 一个宽度 × 浅/深色 × 紧凑/标准：六态各自的下一步、单元格四态与键盘等价、页面不横向滚动。
    fn fill_docs(pane: &gpui_kit::Entity<MongoPane>, state: &str, cx: &mut gpui_kit::App) {
        pane.update(cx, |pane, _| {
            pane.documents.clear();
            pane.columns.clear();
            pane.total = 0;
            pane.busy = false;
            pane.error.clear();
            pane.error_title = "mongo.tab.loadFailed";
            pane.read_failed = false;
            pane.applied_filter.clear();
            pane.filter_input.clear();
            pane.view_open = false;
            pane.editor_open = false;
            pane.confirm_id = None;
            pane.confirm_write = None;
            pane.confirm_prompt.clear();
            pane.focused_cell = None;
            pane.connection_id = "docs".into();
            pane.connection_name = "文档".into();
            pane.database = "app".into();
            pane.collection = "orders".into();
            match state {
                "rows" => {
                    pane.documents = vec![doc("1", "c1", "active"), doc("2", "c2", "idle")];
                    pane.columns = vec![
                        "_id".into(),
                        "customer".into(),
                        "status".into(),
                        "total".into(),
                        "memo".into(),
                        "created".into(),
                        "note".into(),
                    ];
                    pane.total = 2;
                }
                "no-match" => pane.applied_filter = r#"{"status":"blocked"}"#.into(),
                "error" => pane.show_error("连接已断开".into()),
                "loading" => pane.busy = true,
                _ => {}
            }
        });
    }

    fn docs_hold_at(cx: &mut gpui_kit::TestAppContext, width: f32) {
        let (dir, services) = pane_services("matrix");
        let (window, pane) = open_pane_at(cx, services, width);
        let narrow = width < kit::NARROW_PAGE_WIDTH;
        for (dark, density) in [(false, "compact"), (false, "standard"), (true, "compact"), (true, "standard")] {
            cx.update(|cx| {
                kit::apply_theme(dark, cx);
                let metrics = if density == "compact" { kit::Metrics::compact() } else { kit::Metrics::standard() };
                metrics.publish(cx);
            });
            let tag = format!("{width}/{}/{density}", if dark { "dark" } else { "light" });

            // 有行：表格在数据区里，页面根与数据区都不超过窗口宽。
            cx.update(|cx| fill_docs(&pane, "rows", cx));
            render(window, cx);
            let (page, area, nav, table, cell) = cx
                .update_window(window, |_, window, _| {
                    (
                        f32::from(window.find("mongo-pane").bounds().size.width),
                        f32::from(window.find("mongo-docs").bounds().size.width),
                        f32::from(window.find("mongo-nav").bounds().size.width),
                        window.try_find("mongo-row-0").map(|found| f32::from(found.bounds().size.width)).unwrap_or(0.),
                        window.try_find("mongo-cell:0:customer").is_some(),
                    )
                })
                .unwrap();
            assert!((page - width).abs() < 2., "{tag}：页面本身不能被内容撑出横向滚动：{page}");
            assert!(area <= width + 0.5, "{tag}：数据区按窗口宽裁切：{area}");
            assert!(cell, "{tag}：有行时文档单元格要在");
            assert_eq!(nav > 8., !narrow, "{tag}：窄窗口默认收起左栏，宽窗口保持展开");
            // 列宽下限放不进数据区时，多出来的部分必须是数据区内部的横向滚动，页面本身不超宽。
            let wanted = 7. * DOC_COL_MIN + DOC_ACTIONS_W;
            if wanted > area {
                assert!(table >= wanted, "{tag}：数据区放不下时一行仍保持列宽下限：{table} < {wanted}");
            } else {
                assert!(table <= area + 0.5, "{tag}：放得下时一行不超出数据区：{table} > {area}");
            }

            // 单元格点击 = 打开这一列的值；键盘光标同步。
            cx.update_window(window, |_, window, cx| window.click("mongo-cell:0:customer", cx)).unwrap();
            cx.update(|cx| {
                let pane = pane.read(cx);
                assert!(pane.view_open() && pane.view_json().contains("c1"), "{tag}：点单元格要看到这一列的值");
                assert_eq!(pane.focused_cell(), Some((0, 1)), "{tag}：鼠标点格子同步键盘光标");
            });

            // 键盘等价：Tab 进数据区，方向键移格，Enter 打开当前格，Esc 收起只读查看。
            let handle = cx.update(|cx| pane.read(cx).cell_focus.clone()).expect("文档区焦点句柄");
            cx.update_window(window, |_, window, cx| window.focus(&handle, cx)).unwrap();
            cx.update_window(window, |_, window, cx| window.press("down", cx)).unwrap();
            cx.update(|cx| assert_eq!(pane.read(cx).focused_cell(), Some((1, 1)), "{tag}：方向键在格子间移动"));
            cx.update_window(window, |_, window, cx| window.press("enter", cx)).unwrap();
            cx.update(|cx| assert!(pane.read(cx).view_json().contains("c2"), "{tag}：Enter 打开当前格"));
            cx.update_window(window, |_, window, cx| window.press("escape", cx)).unwrap();
            cx.update(|cx| assert!(!pane.read(cx).view_open(), "{tag}：Esc 收起只读查看"));

            // 空集合：无数据空态 + 新增入口，不是错误态。
            cx.update(|cx| fill_docs(&pane, "empty", cx));
            render(window, cx);
            let (empty, insert, error, no_match) = cx
                .update_window(window, |_, window, _| {
                    (
                        window.try_find("empty-state").is_some(),
                        window.try_find("mongo-empty-insert").is_some(),
                        window.try_find("error-state").is_some(),
                        window.try_find("no-match-state").is_some(),
                    )
                })
                .unwrap();
            assert!(empty && insert, "{tag}：空集合用无数据空态并给出新增入口");
            assert!(!error && !no_match, "{tag}：空集合不能画成错误或无命中");

            // 筛选无命中：无命中空态 + 清除条件。
            cx.update(|cx| fill_docs(&pane, "no-match", cx));
            render(window, cx);
            let (no_match, clear, empty) = cx
                .update_window(window, |_, window, _| {
                    (
                        window.try_find("no-match-state").is_some(),
                        window.try_find("mongo-no-match-clear").is_some(),
                        window.try_find("empty-state").is_some(),
                    )
                })
                .unwrap();
            assert!(no_match && clear, "{tag}：无命中给清除条件");
            assert!(!empty, "{tag}：无命中不能和无数据共用一块");

            // 读取失败：错误态 + 重试。
            cx.update(|cx| fill_docs(&pane, "error", cx));
            render(window, cx);
            let (error, retry, empty) = cx
                .update_window(window, |_, window, _| {
                    (
                        window.try_find("error-state").is_some(),
                        window.try_find("mongo-docs-retry").is_some(),
                        window.try_find("empty-state").is_some(),
                    )
                })
                .unwrap();
            assert!(error && retry, "{tag}：读取失败给错误态和重试");
            assert!(!empty, "{tag}：读取失败不能退化成空集合");

            // 读取中：首屏骨架优先于空态。
            cx.update(|cx| fill_docs(&pane, "loading", cx));
            render(window, cx);
            let (skeleton, empty) = cx
                .update_window(window, |_, window, _| (window.try_find("skeleton-state").is_some(), window.try_find("empty-state").is_some()))
                .unwrap();
            assert!(skeleton, "{tag}：读取中用骨架");
            assert!(!empty, "{tag}：读取中不能同时画空态");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[gpui_kit::test]
    fn docs_states_and_cells_hold_at_1360(cx: &mut gpui_kit::TestAppContext) {
        docs_hold_at(cx, 1360.);
    }

    #[gpui_kit::test]
    fn docs_states_and_cells_hold_at_900(cx: &mut gpui_kit::TestAppContext) {
        docs_hold_at(cx, 900.);
    }

    #[gpui_kit::test]
    fn docs_states_and_cells_hold_at_640(cx: &mut gpui_kit::TestAppContext) {
        docs_hold_at(cx, 640.);
    }
}

#[cfg(test)]
mod live;
