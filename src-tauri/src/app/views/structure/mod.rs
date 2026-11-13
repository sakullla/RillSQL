//! 对象结构。预览和执行只走 application::ddl，取消预览不执行。

use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState, InputEvent};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, Entity, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, TestSupportExt as _, Window, div, px, relative,
};

use crate::ui::{DataFormat, EditMode, FormatViewer, TruncationState, ViewerSpec};
use crate::ui::kit::{self, FieldSpec, Fields, Palette, Radius, Tip as _, TypeScale};

use crate::application::AppServices;
use crate::app::views::grid::{operation_banner, GridTarget};
use crate::ddl::english::{localize, localize_missing_item};
use crate::ddl::{preview_plan, Action, ColumnDraft, ForeignKeyDraft, IndexColumnDraft, Plan, Request, Snapshot};
use crate::i18n::{self, Locale};

/// 与 `ddl::model::fk_actions` 相同的级联动作。该函数在 ddl 私有模块里，结构页按引擎原样列出。
fn fk_action_choices(engine: &str) -> &'static [&'static str] {
    match engine {
        "mysql" | "mariadb" => &["RESTRICT", "CASCADE", "SET NULL", "NO ACTION"],
        "redis" | "mongodb" => &[],
        _ => &["RESTRICT", "CASCADE", "SET NULL", "SET DEFAULT", "NO ACTION"],
    }
}

fn type_choices(engine: &str) -> &'static [&'static str] {
    match engine {
        "postgres" => &["bigint", "integer", "text", "boolean", "numeric", "timestamp", "jsonb", "uuid"],
        "mysql" | "mariadb" => &["bigint", "int", "varchar(255)", "text", "datetime", "decimal(10,2)", "json"],
        "sqlserver" => &["bigint", "int", "nvarchar(255)", "datetime2", "bit", "decimal(18,2)"],
        _ => &["INTEGER", "TEXT", "REAL", "BLOB", "NUMERIC"],
    }
}

fn default_id(engine: &str) -> ColumnDraft {
    let type_name = match engine {
        "postgres" | "mysql" | "mariadb" | "sqlserver" => "bigint",
        _ => "INTEGER",
    };
    ColumnDraft {
        original: None,
        name: "id".into(),
        type_name: type_name.into(),
        nullable: false,
        default_sql: String::new(),
        primary: true,
        auto: true,
        dropped: false,
    }
}

fn column_drafts(snapshot: &Snapshot) -> Vec<ColumnDraft> {
    snapshot
        .columns
        .iter()
        .map(|column| ColumnDraft {
            original: Some(column.name.clone()),
            name: column.name.clone(),
            type_name: column.type_name.clone(),
            nullable: column.nullable,
            default_sql: column.default_sql.clone().unwrap_or_default(),
            primary: column.primary,
            auto: column.auto,
            dropped: false,
        })
        .collect()
}

fn shows_ref_schema(engine: &str) -> bool {
    matches!(engine, "postgres" | "sqlserver")
}

fn definition_text(snapshot: &Snapshot) -> (String, Vec<String>) {
    if snapshot.kind != "table" {
        return (snapshot.table_sql.clone(), Vec::new());
    }
    if !snapshot.table_sql.trim().is_empty() {
        return (snapshot.table_sql.clone(), Vec::new());
    }
    crate::ddl::rebuilt_definition(snapshot)
}

/// 字段、索引、外键三张列表共用一份几何（S8.4：复用结果网格的表头、行高与列宽规范）。
/// 行高与表头高都取 `Metrics::row_h`，随密度变化；左右内边距三张表相同，
/// 不再各自拼 `row_h + 2` 与不同的 `px(14.)`。
#[derive(Clone, Copy, Debug, PartialEq)]
struct TableSpec {
    header_h: f32,
    row_h: f32,
    pad_x: f32,
}

impl TableSpec {
    const PAD_X: f32 = 12.;

    fn of(m: kit::Metrics) -> Self {
        Self { header_h: m.row_h, row_h: m.row_h, pad_x: Self::PAD_X }
    }
}

/// 一列的语义、伸缩权重与最小宽度。最小宽度之和要在 640 宽的窗口里放得下（R6）。
#[derive(Clone, Copy, Debug, PartialEq)]
struct ColumnSpec {
    key: &'static str,
    weight: f32,
    min_w: f32,
}

const FIELDS_TABLE: [ColumnSpec; 4] = [
    ColumnSpec { key: "structure.field.column", weight: 2., min_w: 96. },
    ColumnSpec { key: "structure.field.type", weight: 1.4, min_w: 76. },
    ColumnSpec { key: "ui.structure.nullable", weight: 0.6, min_w: 52. },
    ColumnSpec { key: "ui.structure.default", weight: 1.4, min_w: 80. },
];

const INDEXES_TABLE: [ColumnSpec; 3] = [
    ColumnSpec { key: "structure.field.index", weight: 2., min_w: 96. },
    ColumnSpec { key: "structure.section.columns", weight: 2., min_w: 96. },
    ColumnSpec { key: "ui.structure.unique", weight: 0.6, min_w: 52. },
];

const FOREIGN_TABLE: [ColumnSpec; 3] = [
    ColumnSpec { key: "structure.section.foreign", weight: 1.4, min_w: 84. },
    ColumnSpec { key: "structure.section.columns", weight: 1.4, min_w: 84. },
    ColumnSpec { key: "ui.structure.references", weight: 2., min_w: 108. },
];

fn table_columns(section: Section) -> &'static [ColumnSpec] {
    match section {
        Section::Indexes => &INDEXES_TABLE,
        Section::ForeignKeys => &FOREIGN_TABLE,
        _ => &FIELDS_TABLE,
    }
}

/// 列表三态与内容态的判定，供渲染与测试共用：加载优先于错误，错误优先于空态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ListState {
    Loading,
    Error,
    Empty,
    Rows,
}

fn list_state(loading: bool, error: &str, rows: usize) -> ListState {
    if loading {
        return ListState::Loading;
    }
    if !error.is_empty() {
        return ListState::Error;
    }
    if rows == 0 {
        return ListState::Empty;
    }
    ListState::Rows
}

/// 空态的列表不保留表头骨架：表头只属于有行的表。
/// 判定与用例共用；渲染路径里"只有有行才画表头"由 `structure_table` 的调用点保证。
#[allow(dead_code)]
fn shows_table_header(state: ListState) -> bool {
    state == ListState::Rows
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Columns,
    Indexes,
    ForeignKeys,
    View,
    Ddl,
    Danger,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StructureMode {
    Existing,
    Create,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DangerKind {
    Drop,
    Truncate,
    Rename,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditIntent {
    Apply,
    DropIndex { name: String },
    DropForeignKey { name: String },
}

/// 结构页提示。界面文字存 i18n 键，执行层/应用层的原文按条保存；显示时按当前语言给出，切换语言立即生效。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Note {
    #[default]
    Empty,
    Key(&'static str),
    Raw(Vec<String>),
}

impl Note {
    fn raw(message: String) -> Self {
        Note::Raw(vec![message])
    }

    pub fn text(&self, locale: Locale) -> String {
        match self {
            Note::Empty => String::new(),
            Note::Key(key) => i18n::text(locale, key),
            Note::Raw(parts) => parts.iter().map(|part| localize(part, locale)).collect::<Vec<_>>().join(" "),
        }
    }

    pub fn is_empty(&self) -> bool {
        match self {
            Note::Empty => true,
            Note::Key(_) => false,
            Note::Raw(parts) => parts.iter().all(|part| part.is_empty()),
        }
    }
}

pub struct StructureSession {
    pub target: GridTarget,
    pub mode: StructureMode,
    pub section: Section,
    pub object_kind: String,
    pub table_name: String,
    pub column_name: String,
    pub column_type: String,
    /// 非空时字段预览使用完整草稿。已有列带 original，alter_precheck 才允许改。
    pub kept_columns: Vec<ColumnDraft>,
    pub index_name: String,
    pub index_column: String,
    pub index_unique: bool,
    pub index_columns: Vec<IndexColumnDraft>,
    pub fk_name: String,
    pub fk_column: String,
    pub fk_ref_schema: String,
    pub fk_ref_table: String,
    pub fk_ref_column: String,
    pub fk_on_delete: String,
    pub fk_on_update: String,
    pub view_sql: String,
    pub danger: DangerKind,
    pub rename_to: String,
    /// 生产环境确认前要输入的对象名。不写入 SQL。
    pub danger_name: String,
    pub intent: EditIntent,
    preview: Option<Plan>,
    live: bool,
    open: bool,
    /// 危险计划先进入待确认，第二次确认才执行。
    armed: bool,
    pub locale: Locale,
    note: Note,
}

impl StructureSession {
    pub fn new(target: GridTarget) -> Self {
        Self {
            target,
            mode: StructureMode::Existing,
            section: Section::Columns,
            object_kind: String::new(),
            table_name: String::new(),
            column_name: String::new(),
            column_type: "TEXT".to_owned(),
            kept_columns: Vec::new(),
            index_name: String::new(),
            index_column: String::new(),
            index_unique: false,
            index_columns: Vec::new(),
            fk_name: String::new(),
            fk_column: String::new(),
            fk_ref_schema: String::new(),
            fk_ref_table: String::new(),
            fk_ref_column: String::new(),
            fk_on_delete: "NO ACTION".to_owned(),
            fk_on_update: "NO ACTION".to_owned(),
            view_sql: String::new(),
            danger: DangerKind::Drop,
            rename_to: String::new(),
            danger_name: String::new(),
            intent: EditIntent::Apply,
            preview: None,
            live: false,
            open: false,
            armed: false,
            locale: Locale::ZhCn,
            note: Note::Empty,
        }
    }

    /// 当前提示，按界面语言给出。
    pub fn note(&self) -> String {
        self.note.text(self.locale)
    }

    pub fn banner(&self) -> String {
        operation_banner(&self.target)
    }

    pub fn request(&self) -> Result<Request, String> {
        crate::application::ddl::request(self.target.database.clone(), self.target.schema.clone(), &self.target.engine, self.action())
    }

    pub fn visible_sections(&self) -> Vec<Section> {
        if self.mode == StructureMode::Create {
            return vec![Section::Columns];
        }
        if self.object_kind == "view" {
            return vec![Section::Columns, Section::View, Section::Danger];
        }
        vec![Section::Columns, Section::Indexes, Section::ForeignKeys, Section::Ddl, Section::Danger]
    }

    pub fn production(&self) -> bool {
        matches!(self.target.environment.as_str(), "prod" | "production")
    }

    pub fn danger_name_ok(&self) -> bool {
        !self.production() || self.danger_name.trim() == self.target.table
    }

    /// 确认层按危险色呈现的条件：危险操作页签，或计划里本身有 danger 级风险。
    /// 只影响配色与图标，二次确认仍由 `requires_second_confirm` 决定。
    pub fn preview_dangerous(&self) -> bool {
        self.section == Section::Danger
            || self
                .preview
                .as_ref()
                .is_some_and(|plan| plan.risks.iter().any(|risk| risk.level == "danger"))
    }

    /// 生产环境危险操作必须先输入对象名；未满足时确认按钮保持禁用。
    pub fn confirm_blocked(&self) -> bool {
        self.section == Section::Danger && !self.danger_name_ok()
    }

    /// 确认层标题里的对象名：已有对象用表名，新建表用输入的表名。
    pub fn object_label(&self) -> String {
        if self.mode == StructureMode::Create { self.table_name.clone() } else { self.target.table.clone() }
    }


    fn column_drafts_for_action(&self) -> Vec<ColumnDraft> {
        let mut columns = self.kept_columns.clone();
        if !self.column_name.is_empty() && columns.iter().all(|column| column.name != self.column_name) {
            columns.push(ColumnDraft {
                original: None,
                name: self.column_name.clone(),
                type_name: self.column_type.clone(),
                nullable: true,
                default_sql: String::new(),
                primary: false,
                auto: false,
                dropped: false,
            });
        }
        if columns.is_empty() {
            columns.push(ColumnDraft {
                original: None,
                name: self.column_name.clone(),
                type_name: self.column_type.clone(),
                nullable: true,
                default_sql: String::new(),
                primary: false,
                auto: false,
                dropped: false,
            });
        }
        columns
    }

    fn index_drafts(&self) -> Vec<IndexColumnDraft> {
        if !self.index_columns.is_empty() {
            return self.index_columns.clone();
        }
        vec![IndexColumnDraft { name: self.index_column.clone(), descending: false }]
    }

    fn foreign_key_draft(&self) -> ForeignKeyDraft {
        let on_delete = if self.fk_on_delete.trim().is_empty() { "NO ACTION".to_owned() } else { self.fk_on_delete.clone() };
        let on_update = if self.fk_on_update.trim().is_empty() { "NO ACTION".to_owned() } else { self.fk_on_update.clone() };
        ForeignKeyDraft {
            name: self.fk_name.clone(),
            columns: vec![self.fk_column.clone()],
            ref_schema: self.fk_ref_schema.clone(),
            ref_table: self.fk_ref_table.clone(),
            ref_columns: vec![self.fk_ref_column.clone()],
            on_delete,
            on_update,
        }
    }

    fn danger_action(&self) -> Action {
        let table = self.target.table.clone();
        match self.danger {
            DangerKind::Truncate => Action::TruncateTable { table },
            DangerKind::Rename => Action::RenameTable { table, new_name: self.rename_to.clone() },
            DangerKind::Drop if self.object_kind == "view" => Action::DropView { view: table },
            DangerKind::Drop => Action::DropTable { table },
        }
    }

    pub fn action(&self) -> Action {
        let table = self.target.table.clone();
        match self.section {
            Section::Columns if self.mode == StructureMode::Create => Action::CreateTable { name: self.table_name.clone(), columns: self.column_drafts_for_action() },
            Section::Columns | Section::Ddl => Action::AlterColumns { table, columns: self.column_drafts_for_action() },
            Section::Indexes => match &self.intent {
                EditIntent::DropIndex { name } => Action::DropIndex { table, name: name.clone() },
                _ => Action::CreateIndex { table, name: self.index_name.clone(), unique: self.index_unique, columns: self.index_drafts() },
            },
            Section::ForeignKeys => match &self.intent {
                EditIntent::DropForeignKey { name } => Action::DropForeignKey { table, name: name.clone() },
                _ => Action::CreateForeignKey { table, foreign_key: self.foreign_key_draft() },
            },
            Section::View => Action::ReplaceView { table, definition: self.view_sql.clone() },
            Section::Danger => self.danger_action(),
        }
    }

    pub fn arm_live(&mut self, plan: Plan) {
        self.note = Note::Empty;
        self.preview = Some(plan);
        self.live = true;
        self.open = true;
        self.armed = false;
    }

    pub fn arm_fixture(&mut self, snapshot: &Snapshot) {
        let plan = preview_plan(snapshot, &self.action());
        self.note = Note::Key("admin.preview.unconnected");
        self.preview = Some(plan);
        self.live = false;
        self.open = true;
        self.armed = false;
    }

    pub fn preview_open(&self) -> bool {
        self.open
    }

    /// 危险计划在真正执行前还要再确认一次；预览关闭、重做或取消都会重置这个状态。
    pub fn requires_second_confirm(&self) -> bool {
        self.open
            && !self.armed
            && self
                .preview
                .as_ref()
                .is_some_and(|plan| plan.risks.iter().any(|risk| risk.level == "danger"))
    }

    pub fn arm_confirm(&mut self) {
        if self.requires_second_confirm() {
            self.armed = true;
            self.note = Note::Key("structure.confirm.second");
        }
    }

    pub fn armed(&self) -> bool {
        self.armed
    }

    pub fn blocked(&self) -> Vec<String> {
        self.preview.as_ref().map(|plan| plan.blocked.iter().map(|text| localize(text, self.locale)).collect()).unwrap_or_default()
    }

    pub fn preview_sql(&self) -> Vec<String> {
        self.preview.as_ref().map(|plan| plan.steps.iter().map(|step| step.sql.clone()).collect()).unwrap_or_default()
    }

    pub fn preview_risks(&self) -> Vec<(String, String)> {
        self.preview
            .as_ref()
            .map(|plan| plan.risks.iter().map(|risk| (risk.level.clone(), localize(&risk.text, self.locale))).collect())
            .unwrap_or_default()
    }

    pub fn preview_rule(&self) -> String {
        self.preview.as_ref().map(|plan| localize(&plan.rule, self.locale)).unwrap_or_default()
    }

    pub fn preview_target(&self) -> String {
        self.preview.as_ref().map(|plan| plan.target.clone()).unwrap_or_default()
    }

    pub fn cancel(&mut self) {
        self.open = false;
        self.armed = false;
    }

    pub fn confirm_sql(&self) -> Result<Vec<String>, String> {
        self.confirm_steps().map_err(|note| note.text(self.locale))
    }

    /// 确认前的检查；不能执行时给出要显示的提示（保留原文，显示时再本地化）。
    fn confirm_steps(&self) -> Result<Vec<String>, Note> {
        if !self.open {
            return Err(Note::Key("structure.preview.need"));
        }
        if !self.live {
            return Err(Note::Key("admin.preview.unconnected"));
        }
        if self.section == Section::Danger && !self.danger_name_ok() {
            return Err(Note::Key("structure.danger.typeName"));
        }
        let plan = self.preview.as_ref().ok_or(Note::Key("structure.preview.need"))?;
        if !plan.blocked.is_empty() {
            return Err(Note::Raw(plan.blocked.clone()));
        }
        Ok(plan.steps.iter().map(|step| step.sql.clone()).collect())
    }
}

pub struct StructurePane {
    pub session: StructureSession,
    services: Arc<AppServices>,
    fields: Fields<&'static str>,
    draft_fields: Fields<u32>,
    snapshot: Option<Snapshot>,
    loaded_for: String,
    /// 快照列已经写进 kept_columns。重置前不再覆盖用户草稿。
    drafts_ready: bool,
    loading: bool,
    load_error: String,
    /// 当前绑定到的工作项 id；关闭后重开会拿到新 id，必须重置草稿。
    item_id: String,
    /// 刚删除成功的表；外壳取走后让对象树跟着变。
    dropped: Option<GridTarget>,
    /// 建表成功后的新表。外壳取走后刷新对象树并切到这张表。
    created: Option<GridTarget>,
    fk_tables: Vec<String>,
    fk_schemas: Vec<String>,
    fk_columns: Vec<String>,
    fk_key: String,
    ddl_view: Option<gpui_kit::Entity<FormatViewer>>,
    ddl_text: String,
    view_editor: Option<gpui_kit::Entity<EditorState>>,
    view_editor_sync: String,
    view_sub: Option<Subscription>,
}

impl StructurePane {
    pub fn new(services: Arc<AppServices>) -> Self {
        Self {
            session: StructureSession::new(GridTarget::default()),
            services,
            fields: Fields::default(),
            draft_fields: Fields::default(),
            snapshot: None,
            loaded_for: String::new(),
            drafts_ready: false,
            loading: false,
            load_error: String::new(),
            item_id: String::new(),
            dropped: None,
            created: None,
            fk_tables: Vec::new(),
            fk_schemas: Vec::new(),
            fk_columns: Vec::new(),
            fk_key: String::new(),
            ddl_view: None,
            ddl_text: String::new(),
            view_editor: None,
            view_editor_sync: String::new(),
            view_sub: None,
        }
    }

    /// 外壳取走“刚建好的表”。
    pub fn take_created(&mut self) -> Option<GridTarget> {
        self.created.take()
    }

    /// 对象树“新建表”：表名留空，字段草稿从 id 开始。
    pub fn begin_create(&mut self, mut target: GridTarget) {
        target.table.clear();
        let item = format!("create:{}", target.connection_id);
        self.bind_item(target, &item);
        self.enter_create();
    }

    fn enter_create(&mut self) {
        self.session.mode = StructureMode::Create;
        self.session.section = Section::Columns;
        self.session.object_kind.clear();
        self.session.table_name.clear();
        self.session.kept_columns = vec![default_id(&self.session.target.engine)];
        self.drafts_ready = true;
    }

    /// 外壳取走“刚删除成功的表”。
    pub fn take_dropped(&mut self) -> Option<GridTarget> {
        self.dropped.take()
    }

    /// 兼容调用：不区分工作项实例，只按目标绑定。
    pub fn bind(&mut self, target: GridTarget) {
        self.bind_item(target, "");
    }

    /// 同一工作项切工具不丢草稿；关闭后重开（新 item id）或换目标才重建。
    pub fn bind_item(&mut self, target: GridTarget, item: &str) {
        if self.session.target == target && self.item_id == item {
            return;
        }
        self.item_id = item.to_owned();
        let locale = self.session.locale;
        self.session = StructureSession::new(target.clone());
        self.session.locale = locale;
        self.fields = Fields::default();
        self.draft_fields = Fields::default();
        self.snapshot = None;
        self.loaded_for.clear();
        self.drafts_ready = false;
        self.load_error.clear();
        self.loading = false;
        self.fk_tables.clear();
        self.fk_schemas.clear();
        self.fk_columns.clear();
        self.fk_key.clear();
        self.ddl_view = None;
        self.ddl_text.clear();
        self.view_editor = None;
        self.view_editor_sync.clear();
        self.view_sub = None;
        if target.table.is_empty() && !target.connection_id.is_empty() {
            self.enter_create();
        }
    }

    pub fn set_locale(&mut self, locale: Locale) {
        self.session.locale = locale;
    }

    /// 读取结构失败的原因，按当前界面语言给出（模型里留原文）。
    pub fn load_error_text(&self) -> String {
        localize(&self.load_error, self.session.locale)
    }

    pub fn type_into(&mut self, field: &str, text: &str) {
        let keep_preview = field == "danger_name";
        let slot = match field {
            "column_name" => &mut self.session.column_name,
            "column_type" => &mut self.session.column_type,
            "index_name" => &mut self.session.index_name,
            "index_column" => &mut self.session.index_column,
            "fk_name" => &mut self.session.fk_name,
            "fk_column" => &mut self.session.fk_column,
            "fk_ref_schema" => &mut self.session.fk_ref_schema,
            "fk_ref_table" => &mut self.session.fk_ref_table,
            "fk_ref_column" => &mut self.session.fk_ref_column,
            "view_sql" => &mut self.session.view_sql,
            "table_name" => &mut self.session.table_name,
            "rename_to" => &mut self.session.rename_to,
            "danger_name" => &mut self.session.danger_name,
            _ => return,
        };
        if text == "backspace" {
            slot.pop();
        } else if field == "column_type" && slot.as_str() == "TEXT" {
            slot.clear();
            slot.push_str(text);
        } else {
            slot.push_str(text);
        }
        if field == "index_column" {
            self.session.index_columns.clear();
        }
        if !keep_preview {
            self.session.cancel();
        }
    }

    pub fn preview(&mut self, cx: &mut Context<Self>) {
        let Ok(request) = self.session.request() else {
            self.session.note = Note::Key("structure.preview.need");
            cx.notify();
            return;
        };
        let services = Arc::clone(&self.services);
        let target = self.session.target.clone();
        let connection = self.session.target.connection_id.clone();
        let view = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::app::views::grid::runtime_for_views().spawn(async move {
                let result = crate::application::ddl::preview_request(&services.connections, &connection, &request).await;
                let _ = tx.send(result);
            });
            if let Ok(result) = rx.await {
                cx.update(|cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |this, cx| {
                            if this.session.target != target {
                                this.session.note = Note::Key("structure.preview.stale");
                                cx.notify();
                                return;
                            }
                            match result {
                                Ok(plan) => this.session.arm_live(plan),
                                Err(message) => this.session.note = Note::raw(message),
                            }
                            cx.notify();
                        });
                    }
                });
            }
        })
        .detach();
    }

    pub fn confirm(&mut self, cx: &mut Context<Self>) {
        if self.session.section == Section::Danger && !self.session.danger_name_ok() {
            self.session.note = Note::Key("structure.danger.typeName");
            cx.notify();
            return;
        }
        if self.session.requires_second_confirm() {
            self.session.arm_confirm();
            cx.notify();
            return;
        }
        let sql = match self.session.confirm_steps() {
            Ok(sql) => sql,
            Err(note) => {
                self.session.note = note;
                cx.notify();
                return;
            }
        };
        let Ok(request) = self.session.request() else {
            return;
        };
        let drops_table = matches!(request.action, Action::DropTable { .. });
        let created_name = match &request.action {
            Action::CreateTable { name, .. } => Some(name.clone()),
            _ => None,
        };
        let services = Arc::clone(&self.services);
        let target = self.session.target.clone();
        let connection = self.session.target.connection_id.clone();
        let view = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::app::views::grid::runtime_for_views().spawn(async move {
                let result = crate::application::ddl::execute_request(&services.connections, &connection, &request, &sql, true).await;
                let _ = tx.send(result);
            });
            if let Ok(result) = rx.await {
                cx.update(|cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |this, cx| {
                            if this.session.target != target {
                                this.session.note = Note::Key("structure.execute.stale");
                                cx.notify();
                                return;
                            }
                            this.session.note = match result {
                                Ok(receipt) if receipt.blocked.is_empty() => {
                                    this.session.open = false;
                                    this.session.armed = false;
                                    // 删表成功：对象树也要去掉这张表。失败的语句不算成功。
                                    let all_ok = receipt.steps.iter().all(|step| step.status == crate::ddl::StepStatus::Ok);
                                    if drops_table && all_ok && !receipt.steps.is_empty() {
                                        this.dropped = Some(this.session.target.clone());
                                    }
                                    if let Some(name) = created_name.clone() {
                                        if all_ok && !receipt.steps.is_empty() && !name.trim().is_empty() {
                                            let mut created = this.session.target.clone();
                                            created.table = name;
                                            this.created = Some(created.clone());
                                            this.session.mode = StructureMode::Existing;
                                            this.session.target = created;
                                            this.session.object_kind = "table".into();
                                            this.session.table_name.clear();
                                            this.drafts_ready = false;
                                            this.loaded_for.clear();
                                            this.snapshot = None;
                                            this.ddl_view = None;
                                            this.ddl_text.clear();
                                        }
                                    }
                                    Note::Key("structure.preview.done")
                                }
                                Ok(receipt) => {
                                    this.session.armed = false;
                                    Note::Raw(receipt.blocked)
                                }
                                Err(message) => {
                                    // 执行失败留在预览里，原因和可重试的脚本都还在。
                                    this.session.armed = false;
                                    Note::raw(message)
                                }
                            };
                            cx.notify();
                        });
                    }
                });
            }
        })
        .detach();
    }
}

impl StructurePane {
    fn ensure_snapshot(&mut self, cx: &mut Context<Self>) {
        let target = &self.session.target;
        if target.connection_id.is_empty() || target.table.is_empty() {
            return;
        }
        let key = format!("{}|{}|{}|{}", target.connection_id, target.database, target.schema, target.table);
        if self.loaded_for == key {
            return;
        }
        self.loaded_for = key.clone();
        self.snapshot = None;
        self.load_error.clear();
        self.loading = true;
        let services = Arc::clone(&self.services);
        let target = target.clone();
        let view = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::app::views::grid::runtime_for_views().spawn(async move {
                let result = crate::application::ddl::describe_saved(&services.connections, &target.connection_id, target.database.clone(), target.schema.clone(), &target.engine, &target.table).await;
                let _ = tx.send(result);
            });
            if let Ok(result) = rx.await {
                cx.update(|cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |this, cx| {
                            if this.loaded_for != key {
                                return;
                            }
                            this.loading = false;
                            match result {
                                Ok(snapshot) => {
                                    this.adopt_snapshot(&snapshot);
                                    this.snapshot = Some(snapshot);
                                }
                                // 留原文，显示时按当前语言给出。
                                Err(message) => this.load_error = message,
                            }
                            cx.notify();
                        });
                    }
                });
            }
        })
        .detach();
    }

    fn adopt_snapshot(&mut self, snapshot: &Snapshot) {
        self.session.object_kind = snapshot.kind.clone();
        if self.session.mode == StructureMode::Create {
            return;
        }
        if !self.drafts_ready {
            self.drafts_ready = true;
            self.session.kept_columns = column_drafts(snapshot);
            if self.session.view_sql.is_empty() {
                self.session.view_sql = snapshot.table_sql.clone();
            }
        }
        let sections = self.session.visible_sections();
        if !sections.contains(&self.session.section) {
            self.session.section = Section::Columns;
            self.session.cancel();
        }
    }

    fn set_field(&mut self, field: &'static str, value: String) {
        let keep_preview = field == "danger_name";
        let slot = match field {
            "column_name" => &mut self.session.column_name,
            "column_type" => &mut self.session.column_type,
            "index_name" => &mut self.session.index_name,
            "index_column" => &mut self.session.index_column,
            "fk_name" => &mut self.session.fk_name,
            "fk_column" => &mut self.session.fk_column,
            "fk_ref_schema" => &mut self.session.fk_ref_schema,
            "fk_ref_table" => &mut self.session.fk_ref_table,
            "fk_ref_column" => &mut self.session.fk_ref_column,
            "view_sql" => &mut self.session.view_sql,
            "table_name" => &mut self.session.table_name,
            "rename_to" => &mut self.session.rename_to,
            "danger_name" => &mut self.session.danger_name,
            _ => return,
        };
        if *slot != value {
            *slot = value;
            if field == "index_column" {
                self.session.index_columns.clear();
            }
            if field == "fk_ref_schema" || field == "fk_ref_table" {
                self.fk_key.clear();
            }
            if !keep_preview {
                self.session.cancel();
            }
        }
    }

    fn field_value(&self, field: &'static str) -> String {
        match field {
            "column_name" => self.session.column_name.clone(),
            "column_type" => self.session.column_type.clone(),
            "index_name" => self.session.index_name.clone(),
            "index_column" => self.session.index_column.clone(),
            "fk_name" => self.session.fk_name.clone(),
            "fk_column" => self.session.fk_column.clone(),
            "fk_ref_schema" => self.session.fk_ref_schema.clone(),
            "fk_ref_table" => self.session.fk_ref_table.clone(),
            "fk_ref_column" => self.session.fk_ref_column.clone(),
            "view_sql" => self.session.view_sql.clone(),
            "table_name" => self.session.table_name.clone(),
            "rename_to" => self.session.rename_to.clone(),
            "danger_name" => self.session.danger_name.clone(),
            _ => String::new(),
        }
    }

    fn write_draft(&mut self, key: u32, value: String) {
        let group = key / 10_000;
        let index = (key % 10_000) as usize;
        let changed = match group {
            0 => match key {
                4 => replace_text(&mut self.session.table_name, value),
                5 => replace_text(&mut self.session.rename_to, value),
                6 => replace_text(&mut self.session.danger_name, value),
                _ => false,
            },
            1 => self.edit_column(index, |column| replace_text(&mut column.name, value)),
            2 => self.edit_column(index, |column| replace_text(&mut column.type_name, value)),
            3 => self.edit_column(index, |column| replace_text(&mut column.default_sql, value)),
            _ => false,
        };
        if changed && key != 6 {
            self.session.cancel();
        }
    }

    fn edit_column(&mut self, index: usize, edit: impl FnOnce(&mut ColumnDraft) -> bool) -> bool {
        let Some(column) = self.session.kept_columns.get_mut(index) else {
            return false;
        };
        edit(column)
    }

    fn toggle_column(&mut self, index: usize, which: &str) {
        let create = self.session.mode == StructureMode::Create;
        let changed = self.edit_column(index, |column| match which {
            "null" => {
                column.nullable = !column.nullable;
                true
            }
            "primary" if create => {
                column.primary = !column.primary;
                if column.primary {
                    column.nullable = false;
                }
                true
            }
            "auto" if create => {
                column.auto = !column.auto;
                true
            }
            "drop" => {
                column.dropped = !column.dropped;
                true
            }
            _ => false,
        });
        if changed {
            self.session.cancel();
        }
    }

    fn move_column(&mut self, index: usize, delta: isize) {
        if self.session.mode != StructureMode::Create {
            return;
        }
        let next = index as isize + delta;
        if next < 0 || next >= self.session.kept_columns.len() as isize {
            return;
        }
        self.session.kept_columns.swap(index, next as usize);
        self.session.cancel();
    }

    fn add_column(&mut self) {
        let name = if self.session.column_name.trim().is_empty() {
            format!("col_{}", self.session.kept_columns.len() + 1)
        } else {
            self.session.column_name.trim().to_owned()
        };
        if self.session.kept_columns.iter().any(|column| column.name == name) {
            return;
        }
        self.session.kept_columns.push(ColumnDraft {
            original: None,
            name,
            type_name: self.session.column_type.clone(),
            nullable: true,
            default_sql: String::new(),
            primary: false,
            auto: false,
            dropped: false,
        });
        self.session.column_name.clear();
        self.session.cancel();
    }

    fn reset_draft(&mut self) {
        self.session.cancel();
        self.session.column_name.clear();
        self.session.column_type = "TEXT".into();
        self.session.intent = EditIntent::Apply;
        self.session.index_unique = false;
        self.session.index_columns.clear();
        self.session.index_name.clear();
        self.session.index_column.clear();
        if self.session.mode == StructureMode::Create {
            self.session.table_name.clear();
            self.session.kept_columns = vec![default_id(&self.session.target.engine)];
            return;
        }
        if let Some(snapshot) = &self.snapshot {
            self.session.kept_columns = column_drafts(snapshot);
            self.session.view_sql = snapshot.table_sql.clone();
        }
    }

    fn toggle_index_column(&mut self, name: &str) {
        if let Some(index) = self.session.index_columns.iter().position(|column| column.name == name) {
            self.session.index_columns.remove(index);
        } else {
            self.session.index_columns.push(IndexColumnDraft { name: name.to_owned(), descending: false });
        }
        self.session.index_column = self.session.index_columns.first().map(|column| column.name.clone()).unwrap_or_default();
        self.session.intent = EditIntent::Apply;
        self.session.cancel();
    }

    fn toggle_index_order(&mut self, name: &str) {
        if let Some(column) = self.session.index_columns.iter_mut().find(|column| column.name == name) {
            column.descending = !column.descending;
            self.session.intent = EditIntent::Apply;
            self.session.cancel();
        }
    }

    fn choose_fk(&mut self, field: &'static str, value: String) {
        self.set_field(field, value);
    }

    fn field(&mut self, field: &'static str, key: &str, placeholder: &str, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let value = self.field_value(field);
        let state = self.fields.bind(field, &value, FieldSpec { placeholder, masked: false }, window, cx, |this: &mut Self, field, value, _| this.set_field(field, value));
        kit::form_row(
            SharedString::from(i18n::text(self.session.locale, key)),
            kit::text_input(SharedString::from(format!("structure-field-{field}")), &state, i18n::text(self.session.locale, key)),
            &p,
        )
    }

    fn ensure_references(&mut self, cx: &mut Context<Self>) {
        if self.session.section != Section::ForeignKeys || self.session.mode == StructureMode::Create {
            return;
        }
        let target = &self.session.target;
        if target.connection_id.is_empty() {
            return;
        }
        let schema = if self.session.fk_ref_schema.is_empty() { target.schema.clone() } else { self.session.fk_ref_schema.clone() };
        let key = format!("{}|{}|{}|{}", target.connection_id, target.database, schema, self.session.fk_ref_table);
        if self.fk_key == key {
            return;
        }
        self.fk_key = key.clone();
        let services = Arc::clone(&self.services);
        let target = target.clone();
        let ref_table = self.session.fk_ref_table.clone();
        let want_schemas = shows_ref_schema(&target.engine) && self.fk_schemas.is_empty();
        let view = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::app::views::grid::runtime_for_views().spawn(async move {
                let loaded = load_fk_choices(&services.connections, &target, &schema, &ref_table, want_schemas).await;
                let _ = tx.send(loaded);
            });
            if let Ok(loaded) = rx.await {
                cx.update(|cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |this, cx| {
                            if this.fk_key != key {
                                return;
                            }
                            if let Some(schemas) = loaded.schemas {
                                this.fk_schemas = schemas;
                            }
                            this.fk_tables = loaded.tables;
                            this.fk_columns = loaded.columns;
                            cx.notify();
                        });
                    }
                });
            }
        })
        .detach();
    }

    fn ddl_viewer(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) -> gpui_kit::Entity<FormatViewer> {
        if let Some(view) = self.ddl_view.clone() {
            if self.ddl_text != text {
                self.ddl_text = text.to_owned();
                let locale = self.session.locale;
                let text = text.to_owned();
                view.update(cx, |viewer, cx| {
                    viewer.replace_full(text, None, window, cx);
                    viewer.set_manual_format(Some(DataFormat::Sql), window, cx);
                    viewer.set_locale(locale, window, cx);
                });
            }
            return view;
        }
        self.ddl_text = text.to_owned();
        let spec = ViewerSpec {
            text: text.to_owned(),
            column_hint: None,
            type_hint: Some("sql".into()),
            bytes: None,
            truncation: TruncationState::Full,
            mode: EditMode::ReadOnly,
            locale: self.session.locale,
        };
        let view = cx.new(|cx| {
            let mut viewer = FormatViewer::new(spec, window, cx);
            viewer.set_manual_format(Some(DataFormat::Sql), window, cx);
            viewer
        });
        self.ddl_view = Some(view.clone());
        view
    }

    fn ensure_view_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.view_editor.is_some() {
            return;
        }
        let initial = self.session.view_sql.clone();
        let editor = cx.new(|cx| EditorState::new(window, cx).language("sql").line_number(true).soft_wrap(true).default_value(initial));
        self.view_editor_sync = self.session.view_sql.clone();
        self.view_sub = Some(cx.subscribe(&editor, |this, state, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let text = state.read(cx).value().to_string();
                if this.session.view_sql != text {
                    this.session.view_sql = text;
                    this.session.cancel();
                    cx.notify();
                }
            }
        }));
        self.view_editor = Some(editor);
    }

    /// “当前结构”卡片：列表走统一的加载/失败/空/有行四态；空列表不再画表头加破折号冒充空态。
    /// “当前结构”卡片：加载 / 读取失败 / 空 / 有内容四态对**所有页签**同一套判定。
    /// 列表页签的"空"看行数，定义类页签（视图 / DDL / 危险操作）的"空"看有没有 SQL 定义——
    /// 读取失败不能退化成"这个对象没有可显示的 SQL 定义"，那样既没原因也没有重试入口。
    fn overview(&self, entity: &Entity<StructurePane>, p: &Palette, cx: &App) -> AnyElement {
        let locale = self.session.locale;
        let metrics = kit::Metrics::of(cx);
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let section = self.session.section;
        let list = matches!(section, Section::Columns | Section::Indexes | Section::ForeignKeys);
        let rows = if list { self.list_len(section) } else { usize::from(self.has_definition()) };
        match list_state(self.loading, &self.load_error, rows) {
            ListState::Loading => return kit::skeleton_state(6, &metrics, p).into_any_element(),
            ListState::Error => return self.load_error_state(entity, p, locale),
            ListState::Empty if !list => {
                return kit::empty_state(IconName::Braces, t("ui.structure.none"), t("structure.empty.definition"), p).into_any_element()
            }
            ListState::Empty => {
                let (icon, heading, detail) = match section {
                    Section::Indexes => (IconName::Hash, "structure.empty.indexes", "structure.empty.indexesHint"),
                    Section::ForeignKeys => (IconName::Key, "structure.empty.foreignKeys", "structure.empty.foreignKeysHint"),
                    _ => (IconName::Table, "structure.empty.columns", "structure.empty.columnsHint"),
                };
                return kit::empty_state(icon, t(heading), t(detail), p).into_any_element();
            }
            ListState::Rows => {}
        }
        if list {
            return self.list_card(entity, section, p, cx);
        }
        // 视图 / DDL / 危险操作页签：显示对象的 SQL 定义（重建定义与 DDL 页签一致）。
        let (sql, _missing) = self.snapshot.as_ref().map(definition_text).unwrap_or((String::new(), Vec::new()));
        div()
            .id("structure-definition")
            .max_h(px(260.))
            .overflow_y_scroll()
            .p(px(metrics.card_pad()))
            .font_family(cx.theme().mono_font_family.clone())
            .text_size(px(TypeScale::SM))
            .text_color(p.text_muted)
            .child(SharedString::from(sql))
            .into_any_element()
    }

    /// 读取失败的统一呈现：原因 + 重试（重试不清用户草稿）。所有页签共用同一块。
    fn load_error_state(&self, entity: &Entity<StructurePane>, p: &Palette, locale: Locale) -> AnyElement {
        let owner = entity.clone();
        let retry = kit::secondary_button("structure-retry", None, SharedString::from(i18n::text(locale, "common.retry")), move |_, _, cx| {
            owner.update(cx, |pane, cx| pane.retry_load(cx));
        })
        .into_any_element();
        kit::error_state_with_action(
            SharedString::from(i18n::text(locale, "structure.error.heading")),
            SharedString::from(self.load_error_text()),
            retry,
            p,
        )
        .into_any_element()
    }

    /// 定义类页签有没有可显示的定义（DDL 页签会用重建定义，所以与 table_sql 是否为空不完全一致）。
    fn has_definition(&self) -> bool {
        self.snapshot.as_ref().map(|snapshot| !definition_text(snapshot).0.trim().is_empty()).unwrap_or(false)
    }

    /// 有行时的列表：表头与行共用 TableSpec；行带右键菜单，菜单项仍只改草稿，要预览确认才执行。
    fn list_card(&self, entity: &Entity<StructurePane>, section: Section, p: &Palette, cx: &App) -> AnyElement {
        let locale = self.session.locale;
        let spec = TableSpec::of(kit::Metrics::of(cx));
        let mono = cx.theme().mono_font_family.clone();
        let rows = self.list_rows(section, locale, &mono, p);
        structure_table(entity, table_columns(section), rows, spec, p, locale)
    }

    fn list_rows(&self, section: Section, locale: Locale, mono: &SharedString, p: &Palette) -> Vec<RowSpec> {
        let text = |key: &str| i18n::text(locale, key);
        let Some(snapshot) = &self.snapshot else {
            return Vec::new();
        };
        match section {
            Section::Columns => snapshot
                .columns
                .iter()
                .enumerate()
                .map(|(index, column)| {
                    let dropped = self.session.kept_columns.get(index).is_some_and(|draft| draft.dropped);
                    RowSpec {
                        id: format!("columns-{index}"),
                        dim: dropped,
                        cells: vec![
                            cell_main(column.name.clone(), column.primary.then_some(IconName::Key), p),
                            cell_mono(column.type_name.clone(), mono, p),
                            cell_text(if column.nullable { "✓".into() } else { "—".into() }, p),
                            cell_mono(column.default_sql.clone().unwrap_or_default(), mono, p),
                        ],
                        menu: vec![
                            MenuEntry {
                                id: format!("copy-column-{index}"),
                                label: text("structure.menu.copyName"),
                                action: RowAction::Copy(column.name.clone()),
                                disabled: false,
                                reason: None,
                            },
                            MenuEntry {
                                id: format!("drop-column-{index}"),
                                label: text(if dropped { "structure.menu.restoreColumn" } else { "structure.menu.dropColumn" }),
                                action: RowAction::ToggleColumnDrop(index),
                                disabled: false,
                                reason: None,
                            },
                        ],
                    }
                })
                .collect(),
            Section::Indexes => snapshot
                .indexes
                .iter()
                .enumerate()
                .map(|(index, row)| {
                    let reason = localize(&row.reason, locale);
                    RowSpec {
                        id: format!("indexes-{index}"),
                        dim: false,
                        cells: vec![
                            cell_main(row.name.clone(), row.unique.then_some(IconName::Hash), p),
                            cell_mono(
                                row.columns
                                    .iter()
                                    .map(|column| if column.descending { format!("{} DESC", column.name) } else { column.name.clone() })
                                    .collect::<Vec<_>>()
                                    .join(", "),
                                mono,
                                p,
                            ),
                            cell_text(if row.unique { "✓".into() } else { "—".into() }, p),
                        ],
                        menu: vec![
                            MenuEntry {
                                id: format!("copy-index-{index}"),
                                label: text("structure.menu.copyName"),
                                action: RowAction::Copy(row.name.clone()),
                                disabled: false,
                                reason: None,
                            },
                            MenuEntry {
                                id: format!("drop-index-{index}"),
                                label: text("structure.index.delete"),
                                action: RowAction::DropIndex(row.name.clone()),
                                disabled: row.protected,
                                reason: (row.protected && !reason.is_empty()).then_some(reason),
                            },
                        ],
                    }
                })
                .collect(),
            Section::ForeignKeys => snapshot
                .foreign_keys
                .iter()
                .enumerate()
                .map(|(index, key)| {
                    let reference = if key.ref_schema.is_empty() { key.ref_table.clone() } else { format!("{}.{}", key.ref_schema, key.ref_table) };
                    let name = if key.name.is_empty() { format!("fk_{index}") } else { key.name.clone() };
                    RowSpec {
                        id: format!("foreign-{index}"),
                        dim: false,
                        cells: vec![
                            cell_main(name.clone(), None, p),
                            cell_mono(key.columns.join(", "), mono, p),
                            cell_mono(format!("{reference}({})", key.ref_columns.join(", ")), mono, p),
                        ],
                        menu: vec![
                            MenuEntry {
                                id: format!("copy-fk-{index}"),
                                label: text("structure.menu.copyName"),
                                action: RowAction::Copy(name.clone()),
                                disabled: false,
                                reason: None,
                            },
                            MenuEntry {
                                id: format!("drop-fk-{index}"),
                                label: text("structure.fk.delete"),
                                action: RowAction::DropForeignKey(name),
                                disabled: key.name.is_empty(),
                                reason: None,
                            },
                        ],
                    }
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    fn list_len(&self, section: Section) -> usize {
        let Some(snapshot) = &self.snapshot else {
            return 0;
        };
        match section {
            Section::Columns => snapshot.columns.len(),
            Section::Indexes => snapshot.indexes.len(),
            Section::ForeignKeys => snapshot.foreign_keys.len(),
            _ => 0,
        }
    }

    /// 读取失败后的重试：清掉失败原因与已读标记，下一帧重新读取（不清用户草稿）。
    pub fn retry_load(&mut self, cx: &mut Context<Self>) {
        self.load_error.clear();
        self.loaded_for.clear();
        self.loading = false;
        cx.notify();
    }

    /// 读取中或读取失败时不显示新增表单与主操作：先让用户看到加载/失败态与重试。
    fn form_visible(&self) -> bool {
        self.session.mode == StructureMode::Create || (!self.loading && self.load_error.is_empty())
    }

    /// 主操作（预览 SQL）固定显示的条件。它与内容滚动分开，长字段列表下也始终可达。
    fn preview_action_visible(&self) -> bool {
        let section = self.session.section;
        let readonly_view = self.session.object_kind == "view" && self.session.mode != StructureMode::Create;
        self.form_visible() && !matches!(section, Section::Ddl) && !(section == Section::Columns && readonly_view)
    }

    /// 表名 + 字段/索引数量的摘要行（S8.4）。
    fn summary_text(&self) -> String {
        if self.session.mode == StructureMode::Create {
            return String::new();
        }
        let Some(snapshot) = &self.snapshot else {
            return String::new();
        };
        let columns = snapshot.columns.len().to_string();
        let indexes = snapshot.indexes.len().to_string();
        i18n::fill(&i18n::text(self.session.locale, "structure.summary"), &[("columns", &columns), ("indexes", &indexes)])
    }

    fn apply_row_action(&mut self, action: RowAction, cx: &mut Context<Self>) {
        match action {
            RowAction::Copy(text) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
            RowAction::ToggleColumnDrop(index) => self.toggle_column(index, "drop"),
            RowAction::DropIndex(name) => self.preview_drop_index(name, cx),
            RowAction::DropForeignKey(name) => self.preview_drop_fk(name, cx),
        }
    }

    /// 主操作按钮：页签内容之外的一条固定动作栏，三档宽度都不被滚动吞掉。
    fn preview_footer(&self, entity: &Entity<StructurePane>, p: &Palette, locale: Locale, metrics: kit::Metrics) -> Option<AnyElement> {
        if !self.preview_action_visible() {
            return None;
        }
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let section = self.session.section;
        let owner = entity.clone();
        let click = move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
            owner.update(cx, |pane, cx| pane.preview_current(cx));
        };
        let button = if section == Section::Danger {
            kit::danger_button("structure-preview", Some(IconName::Eye), t("structure.action.preview"), click).into_any_element()
        } else {
            kit::primary_button("structure-preview", Some(IconName::Eye), t("structure.action.preview"), click).into_any_element()
        };
        Some(
            div()
                .id("structure-actions")
                .test_support()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(metrics.page_pad))
                .py(px(8.))
                .border_t_1()
                .border_color(p.line)
                .bg(p.surface)
                .child(div().flex_1().min_w(px(0.)).child(kit::muted(t("structure.action.hint"), p)))
                .child(button)
                .into_any_element(),
        )
    }
}

fn selected_button(button: gpui_kit::component::button::Button, selected: bool) -> gpui_kit::component::button::Button {
    if selected { button.primary() } else { button }
}

fn replace_text(slot: &mut String, value: String) -> bool {
    if *slot == value {
        return false;
    }
    *slot = value;
    true
}

fn on_draft(this: &mut StructurePane, key: u32, value: String, _: &mut Context<StructurePane>) {
    this.write_draft(key, value);
}

struct FkChoices {
    schemas: Option<Vec<String>>,
    tables: Vec<String>,
    columns: Vec<String>,
}

async fn load_fk_choices(service: &crate::connections::Service, target: &GridTarget, schema: &str, ref_table: &str, want_schemas: bool) -> FkChoices {
    let Some(session) = service.catalog_session(&target.connection_id) else {
        return FkChoices { schemas: None, tables: Vec::new(), columns: Vec::new() };
    };
    let Ok(engine) = crate::connections::model::Engine::parse(&target.engine) else {
        return FkChoices { schemas: None, tables: Vec::new(), columns: Vec::new() };
    };
    let schemas = if want_schemas {
        let body = crate::catalog::read::read_catalog_outline(engine, &session).await;
        let names = body
            .databases
            .iter()
            .find(|database| database.name == target.database)
            .map(|database| database.schemas.iter().map(|schema| schema.name.clone()).filter(|name| !name.is_empty()).collect())
            .unwrap_or_default();
        Some(names)
    } else {
        None
    };
    let page = crate::catalog::read::read_object_kind_page(&session, engine, &target.database, schema, crate::catalog::model::ObjectKind::Table, 0, 200).await;
    let mut tables = page.objects.into_iter().map(|row| row.name).collect::<Vec<_>>();
    tables.sort();
    tables.dedup();
    let mut columns = Vec::new();
    if !ref_table.is_empty() {
        if let Ok(snapshot) = crate::application::ddl::describe_saved(service, &target.connection_id, target.database.clone(), schema.to_owned(), &target.engine, ref_table).await {
            columns = snapshot.columns.into_iter().map(|column| column.name).collect();
        }
    }
    FkChoices { schemas, tables, columns }
}

/// 列表一行：单元格、右键菜单与“已标记删除”的淡化状态。
struct RowSpec {
    /// 行 id 片段（`columns-0` 之类），行的元素 id 是 `structure-row-{id}`。
    id: String,
    dim: bool,
    cells: Vec<AnyElement>,
    menu: Vec<MenuEntry>,
}

/// 右键菜单项。动作只改草稿或发起预览，不直接执行 SQL。
struct MenuEntry {
    id: String,
    label: String,
    action: RowAction,
    disabled: bool,
    reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RowAction {
    Copy(String),
    ToggleColumnDrop(usize),
    DropIndex(String),
    DropForeignKey(String),
}

/// 一张结构列表：表头与行共用一份 TableSpec（行高、表头高、左右内边距）。
fn structure_table(entity: &Entity<StructurePane>, columns: &[ColumnSpec], rows: Vec<RowSpec>, spec: TableSpec, p: &Palette, locale: Locale) -> AnyElement {
    let mut head = div()
        .id("structure-table-head")
        .test_support()
        .flex()
        .flex_none()
        .px(px(spec.pad_x))
        .h(px(spec.header_h))
        .items_center()
        .bg(p.chrome)
        .border_b_1()
        .border_color(p.line)
        .text_size(px(TypeScale::XS))
        .text_color(p.text_faint);
    for column in columns {
        head = head.child(table_cell(*column, div().truncate().child(SharedString::from(i18n::text(locale, column.key)))));
    }
    let mut body = div().flex().flex_col();
    for row in rows {
        let owner = entity.clone();
        let entries = row.menu;
        // 每行一个稳定的元素 id：右键菜单的状态按元素 id 保存，共用同一处代码位置会让所有行共享一个菜单。
        let mut cells = div()
            .id(SharedString::from(format!("structure-row-cells-{}", row.id)))
            .flex()
            .flex_1()
            .min_w(px(0.))
            .items_center();
        for (cell, column) in row.cells.into_iter().zip(columns.iter()) {
            cells = cells.child(table_cell(*column, cell));
        }
        let cells = cells.context_menu(move |menu, _, _| row_menu(&owner, &entries, menu));
        let line = div()
            .id(SharedString::from(format!("structure-row-{}", row.id)))
            .test_support()
            .flex()
            .px(px(spec.pad_x))
            .h(px(spec.row_h))
            .items_center()
            .border_b_1()
            .border_color(p.line_soft)
            .when(row.dim, |this| this.opacity(0.5))
            .hover(|style| style.bg(p.hover))
            .child(cells);
        body = body.child(line);
    }
    div().flex().flex_col().child(head).child(body).into_any_element()
}

fn table_cell(cell: ColumnSpec, child: impl IntoElement) -> gpui_kit::Div {
    div()
        .flex_grow(cell.weight)
        .flex_shrink(1.)
        .flex_basis(px(0.))
        .min_w(px(cell.min_w))
        .pr(px(8.))
        .child(child)
}

fn row_menu(entity: &Entity<StructurePane>, entries: &[MenuEntry], mut menu: PopupMenu) -> PopupMenu {
    for entry in entries {
        menu = menu.item(menu_entry(entity, entry));
    }
    menu
}

/// 菜单项带稳定 id（`structure-menu-{id}`），禁用项留在菜单里并说明原因。
fn menu_entry(entity: &Entity<StructurePane>, entry: &MenuEntry) -> PopupMenuItem {
    let owner = entity.downgrade();
    let element_id = SharedString::from(format!("structure-menu-{}", entry.id));
    let label = SharedString::from(entry.label.clone());
    let reason = entry.reason.clone();
    let disabled = entry.disabled;
    let item = PopupMenuItem::element(move |_, cx| {
        let p = Palette::of(cx);
        div()
            .id(element_id.clone())
            .test_support()
            .w_full()
            .flex()
            .items_center()
            .text_size(px(TypeScale::SM))
            .when(disabled, |this| this.text_color(p.text_faint))
            .child(label.clone())
            .when_some(reason.clone(), |this, reason| this.tooltip_text(SharedString::from(reason)))
    });
    if entry.disabled {
        return item.disabled(true);
    }
    let action = entry.action.clone();
    item.on_click(move |_: &gpui_kit::ClickEvent, _, cx| {
        if let Some(owner) = owner.upgrade() {
            owner.update(cx, |pane, cx| {
                pane.apply_row_action(action.clone(), cx);
                cx.notify();
            });
        }
    })
}

fn cell_main(text: String, icon: Option<IconName>, p: &Palette) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(6.))
        .min_w(px(0.))
        .child(div().truncate().font_weight(FontWeight::MEDIUM).child(SharedString::from(text)))
        .when_some(icon, |this, icon| this.child(kit::icon(icon, 12., p.warn)))
        .into_any_element()
}

fn cell_mono(text: String, mono: &SharedString, p: &Palette) -> AnyElement {
    div().truncate().font_family(mono.clone()).text_size(px(TypeScale::SM)).text_color(p.text_muted).child(SharedString::from(text)).into_any_element()
}

fn cell_text(text: String, p: &Palette) -> AnyElement {
    div().truncate().text_color(p.text_muted).child(SharedString::from(text)).into_any_element()
}

impl StructurePane {
    fn preview_current(&mut self, cx: &mut Context<Self>) {
        self.session.intent = EditIntent::Apply;
        self.preview(cx);
    }

    fn preview_drop_index(&mut self, name: String, cx: &mut Context<Self>) {
        self.session.intent = EditIntent::DropIndex { name };
        self.session.section = Section::Indexes;
        self.preview(cx);
    }

    fn preview_drop_fk(&mut self, name: String, cx: &mut Context<Self>) {
        self.session.intent = EditIntent::DropForeignKey { name };
        self.session.section = Section::ForeignKeys;
        self.preview(cx);
    }

    fn preview_danger(&mut self, kind: DangerKind, cx: &mut Context<Self>) {
        self.session.danger = kind;
        self.session.intent = EditIntent::Apply;
        self.session.section = Section::Danger;
        self.preview(cx);
    }

    fn draft_input(&mut self, key: u32, value: &str, placeholder: &str, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let state = self.draft_fields.bind(key, value, FieldSpec { placeholder, masked: false }, window, cx, on_draft);
        kit::text_input(SharedString::from(format!("structure-draft-{key}")), &state, placeholder.to_owned()).small().into_any_element()
    }

    fn section_form(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let locale = self.session.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let section = self.session.section;
        let entity = cx.entity();
        let metrics = kit::Metrics::of(cx);
        let mut form = div().flex().flex_col().gap(px(metrics.section_gap()));
        let readonly_view = self.session.object_kind == "view" && self.session.mode != StructureMode::Create;
        match section {
            Section::Columns if readonly_view => {
                form = form.child(kit::muted(t("structure.section.viewDef"), &p));
            }
            Section::Columns => {
                if self.session.mode == StructureMode::Create {
                    let name = self.draft_input(4, &self.session.table_name.clone(), "table_name", window, cx);
                    form = form.child(kit::form_row(t("structure.action.createTable"), name, &p));
                }
                let columns = self.session.kept_columns.clone();
                let create = self.session.mode == StructureMode::Create;
                let engine = self.session.target.engine.clone();
                for (index, column) in columns.iter().enumerate() {
                    let name = self.draft_input(10_000 + index as u32, &column.name, "name", window, cx);
                    let kind = self.draft_input(20_000 + index as u32, &column.type_name, "TEXT", window, cx);
                    let default_value = self.draft_input(30_000 + index as u32, &column.default_sql, "DEFAULT", window, cx);
                    let entity = entity.clone();
                    let types = type_choices(&engine).iter().map(|item| (*item).to_owned()).collect::<Vec<_>>();
                    let type_menu = {
                        let entity = entity.clone();
                        Button::new(SharedString::from(format!("structure-types-{index}")))
                            .ghost()
                            .small()
                            .label(t("structure.field.type"))
                            .dropdown_menu(move |mut menu, _, _| {
                                for choice in types.clone() {
                                    let entity = entity.clone();
                                    let choice = choice.clone();
                                    menu = menu.item(PopupMenuItem::new(choice.clone()).on_click(move |_, _, cx| {
                                        let choice = choice.clone();
                                        entity.update(cx, |pane, cx| {
                                            pane.write_draft(20_000 + index as u32, choice);
                                            cx.notify();
                                        });
                                    }));
                                }
                                menu
                            })
                            .into_any_element()
                    };
                    let null_entity = entity.clone();
                    let null_on = column.nullable;
                    let null_button = selected_button(
                        kit::secondary_button(SharedString::from(format!("structure-null-{index}")), None, t("ui.structure.nullable"), move |_, _, cx| {
                            null_entity.update(cx, |pane, cx| {
                                pane.toggle_column(index, "null");
                                cx.notify();
                            });
                        }),
                        null_on,
                    );
                    let mut row = div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(8.))
                        .when(column.dropped, |row| row.opacity(0.45))
                        .child(div().flex_1().min_w(px(120.)).child(name))
                        .child(div().w(px(140.)).child(kind))
                        .child(type_menu)
                        .child(div().w(px(140.)).child(default_value))
                        .child(null_button);
                    if create {
                        let primary_entity = entity.clone();
                        let primary_on = column.primary;
                        row = row.child(selected_button(
                            kit::secondary_button(SharedString::from(format!("structure-pk-{index}")), Some(IconName::Key), t("structure.field.primary"), move |_, _, cx| {
                                primary_entity.update(cx, |pane, cx| {
                                    pane.toggle_column(index, "primary");
                                    cx.notify();
                                });
                            }),
                            primary_on,
                        ));
                        let auto_entity = entity.clone();
                        let auto_on = column.auto;
                        row = row.child(selected_button(
                            kit::secondary_button(SharedString::from(format!("structure-auto-{index}")), None, t("structure.field.auto"), move |_, _, cx| {
                                auto_entity.update(cx, |pane, cx| {
                                    pane.toggle_column(index, "auto");
                                    cx.notify();
                                });
                            }),
                            auto_on,
                        ));
                        let up_entity = entity.clone();
                        row = row.child(kit::icon_button(SharedString::from(format!("structure-up-{index}")), IconName::ChevronUp, t("structure.field.moveUp"), move |_, _, cx| {
                            up_entity.update(cx, |pane, cx| {
                                pane.move_column(index, -1);
                                cx.notify();
                            });
                        }));
                        let down_entity = entity.clone();
                        row = row.child(kit::icon_button(SharedString::from(format!("structure-down-{index}")), IconName::ChevronDown, t("structure.field.moveDown"), move |_, _, cx| {
                            down_entity.update(cx, |pane, cx| {
                                pane.move_column(index, 1);
                                cx.notify();
                            });
                        }));
                    }
                    let drop_entity = entity.clone();
                    row = row.child(kit::icon_button(SharedString::from(format!("structure-drop-col-{index}")), IconName::Trash, t("structure.field.delete"), move |_, _, cx| {
                        drop_entity.update(cx, |pane, cx| {
                            pane.toggle_column(index, "drop");
                            cx.notify();
                        });
                    }));
                    form = form.child(row);
                }
                let name = self.field("column_name", "structure.field.column", "new_column", window, cx);
                let kind = self.field("column_type", "structure.field.type", "TEXT", window, cx);
                let add_entity = entity.clone();
                let reset_entity = entity.clone();
                form = form
                    .child(div().flex().gap(px(8.)).child(div().flex_1().child(name)).child(div().w(px(160.)).child(kind)))
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap(px(8.))
                            .child(kit::secondary_button("structure-add-column", Some(IconName::Plus), t("ui.structure.add"), move |_, _, cx| {
                                add_entity.update(cx, |pane, cx| {
                                    pane.add_column();
                                    cx.notify();
                                });
                            }))
                            .child(kit::secondary_button("structure-reset-draft", None, t("structure.field.reset"), move |_, _, cx| {
                                reset_entity.update(cx, |pane, cx| {
                                    pane.reset_draft();
                                    cx.notify();
                                });
                            })),
                    );
            }
            Section::Indexes => {
                let indexes = self.snapshot.as_ref().map(|snapshot| snapshot.indexes.clone()).unwrap_or_default();
                for (index, index_row) in indexes.iter().enumerate() {
                    let protected = index_row.protected;
                    let reason = localize(&index_row.reason, locale);
                    let name = index_row.name.clone();
                    let drop_entity = entity.clone();
                    let button = kit::secondary_button(SharedString::from(format!("structure-drop-index-{index}")), Some(IconName::Trash), t("structure.index.delete"), move |_, _, cx| {
                        let name = name.clone();
                        drop_entity.update(cx, |pane, cx| pane.preview_drop_index(name, cx));
                    })
                    .disabled(protected);
                    let button = if protected && !reason.is_empty() { button.tooltip(SharedString::from(reason)).into_any_element() } else { button.into_any_element() };
                    form = form.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(div().flex_1().truncate().child(SharedString::from(index_row.name.clone())))
                            .child(button),
                    );
                }
                let names = self.snapshot.as_ref().map(|snapshot| snapshot.columns.iter().map(|column| column.name.clone()).collect::<Vec<_>>()).unwrap_or_else(|| {
                    self.session.kept_columns.iter().filter(|column| !column.dropped).map(|column| column.name.clone()).collect()
                });
                let selected = self.session.index_columns.clone();
                for name in &names {
                    let picked = selected.iter().find(|column| column.name == *name);
                    let on = picked.is_some();
                    let descending = picked.is_some_and(|column| column.descending);
                    let pick_entity = entity.clone();
                    let column_name = name.clone();
                    let order_entity = entity.clone();
                    let order_name = name.clone();
                    form = form.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(selected_button(
                                kit::secondary_button(SharedString::from(format!("structure-index-col-{name}")), None, SharedString::from(name.clone()), move |_, _, cx| {
                                    let column_name = column_name.clone();
                                    pick_entity.update(cx, |pane, cx| {
                                        pane.toggle_index_column(&column_name);
                                        cx.notify();
                                    });
                                }),
                                on,
                            ))
                            .child(
                                kit::secondary_button(
                                    SharedString::from(format!("structure-index-order-{name}")),
                                    None,
                                    if descending { t("structure.index.desc") } else { t("structure.index.asc") },
                                    move |_, _, cx| {
                                        let order_name = order_name.clone();
                                        order_entity.update(cx, |pane, cx| {
                                            pane.toggle_index_order(&order_name);
                                            cx.notify();
                                        });
                                    },
                                )
                                .disabled(!on),
                            ),
                    );
                }
                let unique_entity = entity.clone();
                let unique = self.session.index_unique;
                let name = self.field("index_name", "structure.field.index", "ix_table_column", window, cx);
                form = form.child(name).child(selected_button(
                    kit::secondary_button("structure-index-unique", None, t("structure.index.unique"), move |_, _, cx| {
                        unique_entity.update(cx, |pane, cx| {
                            pane.session.index_unique = !pane.session.index_unique;
                            pane.session.intent = EditIntent::Apply;
                            pane.session.cancel();
                            cx.notify();
                        });
                    }),
                    unique,
                ));
            }
            Section::ForeignKeys => {
                self.ensure_references(cx);
                let keys = self.snapshot.as_ref().map(|snapshot| snapshot.foreign_keys.clone()).unwrap_or_default();
                for (index, key) in keys.iter().enumerate() {
                    let name = if key.name.is_empty() { format!("fk_{index}") } else { key.name.clone() };
                    let drop_entity = entity.clone();
                    let drop_name = name.clone();
                    form = form.child(
                        div().flex().items_center().gap(px(8.)).child(div().flex_1().truncate().child(SharedString::from(name))).child(kit::secondary_button(
                            SharedString::from(format!("structure-drop-fk-{index}")),
                            Some(IconName::Trash),
                            t("structure.fk.delete"),
                            move |_, _, cx| {
                                let drop_name = drop_name.clone();
                                drop_entity.update(cx, |pane, cx| pane.preview_drop_fk(drop_name, cx));
                            },
                        )),
                    );
                }
                let name = self.field("fk_name", "structure.section.foreign", "fk_name", window, cx);
                let column = self.field("fk_column", "structure.field.column", "parent_id", window, cx);
                let table = self.field("fk_ref_table", "ui.structure.refTable", "parents", window, cx);
                let target = self.field("fk_ref_column", "ui.structure.refColumn", "id", window, cx);
                form = form.child(div().flex().gap(px(8.)).child(div().flex_1().child(name)).child(div().flex_1().child(column)));
                if shows_ref_schema(&self.session.target.engine) {
                    let schema = self.field("fk_ref_schema", "structure.fk.refSchema", "public", window, cx);
                    let schemas = self.fk_schemas.clone();
                    let schema_entity = entity.clone();
                    form = form.child(div().flex().gap(px(8.)).child(div().flex_1().child(schema)).child(
                        Button::new("structure-fk-schemas")
                            .ghost()
                            .small()
                            .label(t("structure.fk.refSchema"))
                            .dropdown_menu(move |mut menu, _, _| {
                                for choice in schemas.clone() {
                                    let schema_entity = schema_entity.clone();
                                    let choice = choice.clone();
                                    menu = menu.item(PopupMenuItem::new(choice.clone()).on_click(move |_, _, cx| {
                                        let choice = choice.clone();
                                        schema_entity.update(cx, |pane, cx| {
                                            pane.choose_fk("fk_ref_schema", choice);
                                            cx.notify();
                                        });
                                    }));
                                }
                                menu
                            }),
                    ));
                }
                let tables = self.fk_tables.clone();
                let columns = self.fk_columns.clone();
                let local = self.snapshot.as_ref().map(|snapshot| snapshot.columns.iter().map(|column| column.name.clone()).collect()).unwrap_or_else(Vec::new);
                let table_entity = entity.clone();
                let column_entity = entity.clone();
                let ref_entity = entity.clone();
                form = form.child(div().flex().gap(px(8.)).child(div().flex_1().child(table)).child(div().flex_1().child(target))).child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(8.))
                        .child(
                            Button::new("structure-fk-local")
                                .ghost()
                                .small()
                                .label(t("structure.field.column"))
                                .dropdown_menu(move |mut menu, _, _| {
                                    for choice in local.clone() {
                                        let column_entity = column_entity.clone();
                                        let choice = choice.clone();
                                        menu = menu.item(PopupMenuItem::new(choice.clone()).on_click(move |_, _, cx| {
                                            let choice = choice.clone();
                                            column_entity.update(cx, |pane, cx| {
                                                pane.choose_fk("fk_column", choice);
                                                cx.notify();
                                            });
                                        }));
                                    }
                                    menu
                                }),
                        )
                        .child(
                            Button::new("structure-fk-tables")
                                .ghost()
                                .small()
                                .label(t("ui.structure.refTable"))
                                .dropdown_menu(move |mut menu, _, _| {
                                    for choice in tables.clone() {
                                        let table_entity = table_entity.clone();
                                        let choice = choice.clone();
                                        menu = menu.item(PopupMenuItem::new(choice.clone()).on_click(move |_, _, cx| {
                                            let choice = choice.clone();
                                            table_entity.update(cx, |pane, cx| {
                                                pane.choose_fk("fk_ref_table", choice);
                                                cx.notify();
                                            });
                                        }));
                                    }
                                    menu
                                }),
                        )
                        .child(
                            Button::new("structure-fk-ref-cols")
                                .ghost()
                                .small()
                                .label(t("ui.structure.refColumn"))
                                .dropdown_menu(move |mut menu, _, _| {
                                    for choice in columns.clone() {
                                        let ref_entity = ref_entity.clone();
                                        let choice = choice.clone();
                                        menu = menu.item(PopupMenuItem::new(choice.clone()).on_click(move |_, _, cx| {
                                            let choice = choice.clone();
                                            ref_entity.update(cx, |pane, cx| {
                                                pane.choose_fk("fk_ref_column", choice);
                                                cx.notify();
                                            });
                                        }));
                                    }
                                    menu
                                }),
                        ),
                );
                let actions = fk_action_choices(&self.session.target.engine);
                let delete_now = self.session.fk_on_delete.clone();
                let update_now = self.session.fk_on_update.clone();
                let delete_entity = entity.clone();
                let update_entity = entity.clone();
                form = form.child(
                    div()
                        .flex()
                        .gap(px(8.))
                        .child(
                            Button::new("structure-fk-on-delete")
                                .ghost()
                                .small()
                                .label(SharedString::from(format!("{} {delete_now}", i18n::text(locale, "structure.fk.onDelete"))))
                                .dropdown_menu(move |mut menu, _, _| {
                                    for choice in actions {
                                        let delete_entity = delete_entity.clone();
                                        let choice = (*choice).to_owned();
                                        menu = menu.item(PopupMenuItem::new(choice.clone()).on_click(move |_, _, cx| {
                                            let choice = choice.clone();
                                            delete_entity.update(cx, |pane, cx| {
                                                pane.session.fk_on_delete = choice;
                                                pane.session.intent = EditIntent::Apply;
                                                pane.session.cancel();
                                                cx.notify();
                                            });
                                        }));
                                    }
                                    menu
                                }),
                        )
                        .child(
                            Button::new("structure-fk-on-update")
                                .ghost()
                                .small()
                                .label(SharedString::from(format!("{} {update_now}", i18n::text(locale, "structure.fk.onUpdate"))))
                                .dropdown_menu(move |mut menu, _, _| {
                                    for choice in actions {
                                        let update_entity = update_entity.clone();
                                        let choice = (*choice).to_owned();
                                        menu = menu.item(PopupMenuItem::new(choice.clone()).on_click(move |_, _, cx| {
                                            let choice = choice.clone();
                                            update_entity.update(cx, |pane, cx| {
                                                pane.session.fk_on_update = choice;
                                                pane.session.intent = EditIntent::Apply;
                                                pane.session.cancel();
                                                cx.notify();
                                            });
                                        }));
                                    }
                                    menu
                                }),
                        ),
                );
            }
            Section::View => {
                self.ensure_view_editor(window, cx);
                if let Some(editor) = self.view_editor.clone() {
                    kit::sync_editor(&editor, &mut self.view_editor_sync, &self.session.view_sql, window, cx);
                    form = form.child(
                        div().h(px(220.)).child(
                            Editor::new(&editor).h(relative(1.)).bordered(false).aria_label(t("structure.section.viewDef")),
                        ),
                    );
                }
            }
            Section::Ddl => {
                let (sql, missing) = self.snapshot.as_ref().map(definition_text).unwrap_or_else(|| (String::new(), Vec::new()));
                let viewer = self.ddl_viewer(&sql, window, cx);
                let copy_sql = sql.clone();
                form = form
                    .child(div().h(px(280.)).child(viewer))
                    .child(kit::secondary_button("structure-copy-ddl", Some(IconName::Copy), t("common.copy"), move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy_sql.clone()));
                    }));
                if !missing.is_empty() {
                    let mut notes = div().flex().flex_col().gap(px(4.));
                    for item in missing {
                        notes = notes.child(kit::muted(SharedString::from(localize_missing_item(&item, locale)), &p));
                    }
                    form = form.child(notes);
                }
            }
            Section::Danger => {
                let view = self.session.object_kind == "view";
                form = form.child(
                    div()
                        .flex()
                        .gap(px(10.))
                        .p(px(metrics.card_pad()))
                        .rounded(px(Radius::PANEL))
                        .bg(p.danger_soft)
                        .text_color(p.danger)
                        .child(kit::icon(IconName::TriangleAlert, 16., p.danger))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(2.))
                                .child(div().font_weight(FontWeight::SEMIBOLD).child(if view { t("structure.section.view") } else { t("ui.structure.dropTitle") }))
                                .child(div().text_size(px(TypeScale::SM)).child(t("ui.structure.dropDetail"))),
                        ),
                );
                if !view {
                    let rename = self.field("rename_to", "catalog.danger.newName", "new_name", window, cx);
                    let rename_entity = entity.clone();
                    let truncate_entity = entity.clone();
                    form = form.child(rename).child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(px(8.))
                            .child(kit::secondary_button("structure-rename", None, t("catalog.tree.renameTable"), move |_, _, cx| {
                                rename_entity.update(cx, |pane, cx| pane.preview_danger(DangerKind::Rename, cx));
                            }))
                            .child(kit::danger_button("structure-truncate", None, t("catalog.tree.truncateTable"), move |_, _, cx| {
                                truncate_entity.update(cx, |pane, cx| pane.preview_danger(DangerKind::Truncate, cx));
                            })),
                    );
                }
                if self.session.production() && !self.session.preview_open() {
                    let typed = self.draft_input(6, &self.session.danger_name.clone(), &self.session.target.table.clone(), window, cx);
                    form = form.child(kit::form_row(t("structure.danger.typeName"), typed, &p));
                }
            }
        }
        form.into_any_element()
    }
}

impl Render for StructurePane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_snapshot(cx);
        let p = Palette::of(cx);
        let metrics = kit::Metrics::of(cx);
        let locale = self.session.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let section = self.session.section;
        let entity = cx.entity();
        let mut items = Vec::new();
        for value in self.session.visible_sections() {
            let key = match value {
                Section::Columns => "structure.section.columns",
                Section::Indexes => "structure.section.indexes",
                Section::ForeignKeys => "structure.section.foreign",
                Section::View => "structure.section.viewDef",
                Section::Ddl => "structure.section.ddl",
                Section::Danger => "structure.section.danger",
            };
            let entity = entity.clone();
            items.push((
                SharedString::from(key),
                t(key),
                section == value,
                kit::click(move |_, _, cx| {
                    entity.update(cx, |pane, cx| {
                        pane.session.section = value;
                        pane.session.intent = EditIntent::Apply;
                        pane.session.cancel();
                        cx.notify();
                    });
                }),
            ));
        }
        let form = if self.form_visible() { Some(self.section_form(window, cx)) } else { None };
        let summary = self.summary_text();
        let title = if self.session.mode == StructureMode::Create {
            t("structure.action.createTable")
        } else if self.session.target.table.is_empty() {
            t("structure.section.columns")
        } else {
            SharedString::from(self.session.target.table.clone())
        };
        let mut column = div()
            .id("structure-pane")
            .test_support()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .px(px(metrics.page_pad))
                    .pt(px(metrics.page_pad_y))
                    .pb(px(metrics.pane_pad()))
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(14.))
                    .border_b_1()
                    .border_color(p.line)
                    .child(kit::engine_badge(&self.session.target.engine, 30.))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(160.))
                            .flex()
                            .flex_col()
                            .child(kit::title(title, TypeScale::LG, &p))
                            .child(div().id("structure-banner").text_size(px(TypeScale::SM)).text_color(p.text_muted).truncate().child(SharedString::from(self.session.banner())))
                            .when(!summary.is_empty(), |this| {
                                this.child(
                                    div()
                                        .id("structure-summary")
                                        .test_support()
                                        .text_size(px(TypeScale::XS))
                                        .text_color(p.text_faint)
                                        .truncate()
                                        .child(SharedString::from(summary)),
                                )
                            }),
                    )
                    .child(kit::segmented_sized(items, &p, metrics.control_h)),
            );
        let mut body = div().id("structure-body").flex_1().min_h(px(0.)).overflow_y_scroll().p(px(metrics.page_pad)).flex().flex_col().gap(px(metrics.section_gap()));
        body = body.child(
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(kit::eyebrow(t("ui.structure.existing"), &p))
                .child(kit::card(&p).overflow_hidden().child(self.overview(&entity, &p, cx))),
        );
        if let Some(form) = form {
            body = body.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(kit::eyebrow(t(if section == Section::Danger { "structure.section.danger" } else { "ui.structure.add" }), &p))
                    .child(kit::card(&p).p(px(metrics.card_pad())).child(form)),
            );
        }
        if !self.session.note.is_empty() {
            body = body.child(div().px(px(12.)).py(px(8.)).rounded(px(Radius::PANEL)).bg(p.hover).text_size(px(TypeScale::SM)).child(SharedString::from(self.session.note())));
        }
        column = column.child(body.test_support());
        if let Some(actions) = self.preview_footer(&entity, &p, locale, metrics) {
            column = column.child(actions);
        }
        if self.session.preview_open() {
            // 危险操作确认层（S12.5）：标题行带对象名与危险图标，只有正文滚动，
            // 页脚动作固定可见，640/900/1360 三档都能点到取消与确认。
            let dangerous = self.session.preview_dangerous();
            let armed = self.session.armed();
            let confirm_blocked = self.session.confirm_blocked();
            let object = SharedString::from(self.session.object_label());
            let sql = self.session.preview_sql().join("\n");
            let banner = SharedString::from(self.session.banner());
            let target = SharedString::from(self.session.preview_target());
            let rule = self.session.preview_rule();
            let note_text = self.session.note();
            let risks = self.session.preview_risks();
            let blocked_text = self.session.blocked().join(" ");
            let preview_title = t("structure.preview.title");
            let confirm_label = if armed { t("structure.action.confirmAgain") } else { t("structure.action.confirm") };
            let cancel_label = t("structure.action.cancel");
            let typed_hint =
                (section == Section::Danger && self.session.production() && confirm_blocked).then(|| t("structure.danger.typeHint"));
            let danger_field = if section == Section::Danger && self.session.production() {
                Some(kit::form_row(
                    t("structure.danger.typeName"),
                    self.draft_input(6, &self.session.danger_name.clone(), &self.session.target.table.clone(), window, cx),
                    &p,
                ))
            } else {
                None
            };
            let confirm = {
                let entity = entity.clone();
                move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| entity.update(cx, |pane, cx| pane.confirm(cx))
            };
            let cancel = {
                let entity = entity.clone();
                move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                    entity.update(cx, |pane, cx| {
                        pane.session.cancel();
                        cx.notify();
                    })
                }
            };
            column = column.child(
                div().absolute().inset_0().bg(gpui_kit::hsla(0., 0., 0., 0.25)).flex().items_center().justify_center().child(
                    div()
                        .id("structure-preview-dialog")
                        .test_support()
                        .role(gpui_kit::Role::Dialog)
                        .aria_label(preview_title.clone())
                        .occlude()
                        .w(px(620.))
                        .max_w(relative(0.92))
                        .max_h(relative(0.92))
                        .flex()
                        .flex_col()
                        .overflow_hidden()
                        .bg(p.raised)
                        .border_1()
                        .border_color(p.line)
                        .rounded(px(Radius::PANEL))
                        .shadow_lg()
                        .child(
                            div()
                                .flex_none()
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .px(px(metrics.card_pad()))
                                .pt(px(metrics.card_pad()))
                                .pb(px(10.))
                                .border_b_1()
                                .border_color(p.line_soft)
                                .when(dangerous, |this| this.child(kit::icon(IconName::TriangleAlert, 16., p.danger)))
                                .child(kit::title(preview_title, TypeScale::LG, &p))
                                .child(div().flex_1().min_w(px(0.)))
                                .child(
                                    div()
                                        .id("structure-preview-object")
                                        .test_support()
                                        .min_w(px(0.))
                                        .text_size(px(TypeScale::SM))
                                        .text_color(p.text_muted)
                                        .truncate()
                                        .child(object),
                                ),
                        )
                        .child(
                            div()
                                .id("structure-preview-body")
                                .test_support()
                                .flex_1()
                                .min_h(px(0.))
                                .overflow_y_scroll()
                                .px(px(metrics.card_pad()))
                                .py(px(10.))
                                .flex()
                                .flex_col()
                                .gap(px(10.))
                                .child(kit::muted(banner, &p))
                                .child(
                                    div()
                                        .id("structure-preview-sql")
                                        .max_h(px(240.))
                                        .overflow_y_scroll()
                                        .p(px(12.))
                                        .rounded(px(Radius::PANEL))
                                        .bg(p.chrome)
                                        .border_1()
                                        .border_color(p.line)
                                        .font_family(cx.theme().mono_font_family.clone())
                                        .text_size(px(TypeScale::SM))
                                        .child(SharedString::from(sql)),
                                )
                                .child(kit::muted(target, &p))
                                .when(!risks.is_empty(), |this| {
                                    let mut list = div().flex().flex_col().gap(px(4.));
                                    for (level, text) in risks {
                                        let (fg, bg) = if level == "danger" { (p.danger, p.danger_soft) } else if level == "warn" { (p.warn, p.warn_soft) } else { (p.text_muted, p.hover) };
                                        list = list.child(div().px(px(10.)).py(px(6.)).rounded(px(Radius::PANEL)).bg(bg).text_color(fg).text_size(px(TypeScale::SM)).child(SharedString::from(text)));
                                    }
                                    this.child(list)
                                })
                                .when(!rule.is_empty(), |this| this.child(kit::muted(SharedString::from(rule), &p)))
                                .when(!blocked_text.is_empty(), |this| {
                                    this.child(
                                        div()
                                            .px(px(12.))
                                            .py(px(8.))
                                            .rounded(px(Radius::PANEL))
                                            .bg(p.danger_soft)
                                            .text_color(p.danger)
                                            .text_size(px(TypeScale::SM))
                                            .child(SharedString::from(blocked_text.clone())),
                                    )
                                })
                                .when(!note_text.is_empty(), |this| this.child(kit::muted(SharedString::from(note_text), &p)))
                                .when_some(danger_field, |this, field| this.child(field))
                                .when_some(typed_hint, |this, hint| this.child(div().text_size(px(TypeScale::XS)).text_color(p.warn).child(hint))),
                        )
                        .child(
                            div()
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_end()
                                .gap(px(8.))
                                .px(px(metrics.card_pad()))
                                .py(px(10.))
                                .border_t_1()
                                .border_color(p.line_soft)
                                .child(kit::secondary_button("structure-cancel", None, cancel_label, cancel))
                                .child(if dangerous || armed {
                                    kit::danger_button("structure-confirm", Some(IconName::Check), confirm_label, confirm).disabled(confirm_blocked).into_any_element()
                                } else {
                                    kit::primary_button("structure-confirm", Some(IconName::Check), confirm_label, confirm).into_any_element()
                                }),
                        ),
                ),
            );
        }
        column
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ddl::{Column, ColumnDraft, DdlEngine, Snapshot};

    fn column(name: &str, type_name: &str, primary: bool) -> Column {
        Column {
            name: name.to_owned(),
            type_name: type_name.to_owned(),
            nullable: !primary,
            default_sql: None,
            primary,
            auto: false,
            generated: false,
            default_constraint: None,
        }
    }

    fn target(engine: &str, table: &str) -> GridTarget {
        GridTarget {
            connection_id: "notes".into(),
            connection_name: "笔记".into(),
            database: "app".into(),
            schema: "public".into(),
            table: table.into(),
            engine: engine.into(),
            environment: "dev".into(),
        }
    }

    #[test]
    fn fixtures_preview_columns_indexes_keys_views_and_danger_without_a_live_engine() {
        let mut session = StructureSession::new(target("postgres", "orders"));
        session.section = Section::Indexes;
        session.index_name = "ix_note".into();
        session.index_column = "note".into();
        let mut snapshot = Snapshot::bare(DdlEngine::Postgres, "app", "public", "orders");
        snapshot.columns.push(column("note", "text", false));
        session.arm_fixture(&snapshot);
        assert!(session.banner().contains("笔记") && session.banner().contains("dev") && session.banner().contains("orders"), "{}", session.banner());
        assert!(session.preview_sql().iter().any(|sql| sql.to_ascii_uppercase().contains("INDEX")), "{:?}", session.preview_sql());
        session.cancel();
        assert!(session.confirm_sql().is_err());

        session.section = Section::ForeignKeys;
        session.fk_name = "orders_parent".into();
        session.fk_column = "parent_id".into();
        session.fk_ref_schema = "public".into();
        session.fk_ref_table = "parents".into();
        session.fk_ref_column = "id".into();
        let mut child = Snapshot::bare(DdlEngine::Postgres, "app", "public", "orders");
        child.columns.push(column("parent_id", "integer", false));
        let mut parent = Snapshot::bare(DdlEngine::Postgres, "app", "public", "parents");
        parent.columns.push(column("id", "integer", true));
        child.reference = Some(Box::new(parent));
        session.arm_fixture(&child);
        assert!(session.blocked().is_empty(), "{:?}", session.blocked());
        let foreign = session.preview_sql().join("\n").to_ascii_uppercase();
        assert!(foreign.contains("FOREIGN") || foreign.contains("REFERENCES"), "{foreign}");
        assert!(session.confirm_sql().unwrap_err().contains("没有连接"));

        session.target = target("postgres", "v");
        session.section = Section::View;
        session.view_sql = "CREATE VIEW public.v AS SELECT 2".into();
        let mut view = Snapshot::bare(DdlEngine::Postgres, "app", "public", "v");
        view.kind = "view".into();
        view.table_sql = "CREATE VIEW public.v AS SELECT 1".into();
        session.arm_fixture(&view);
        assert!(session.blocked().is_empty(), "{:?}", session.blocked());
        assert!(session.preview_sql().join("\n").to_ascii_uppercase().contains("VIEW"), "{:?}", session.preview_sql());
        assert!(session.confirm_sql().is_err());

        session.target = target("sqlite", "customers");
        session.target.database = "main".into();
        session.target.schema.clear();
        session.section = Section::Danger;
        let danger = Snapshot::bare(DdlEngine::Sqlite, "main", "", "customers");
        session.arm_fixture(&danger);
        assert!(session.banner().contains("customers") && session.banner().contains("sqlite") && session.banner().contains("dev"), "{}", session.banner());
        assert!(session.preview_sql().iter().any(|sql| sql.to_ascii_uppercase().contains("DROP")), "{:?}", session.preview_sql());
        session.cancel();
        assert!(session.confirm_sql().is_err());
    }

    #[test]
    fn dangerous_ddl_needs_a_second_confirm_and_target_change_drops_the_draft() {
        let mut session = StructureSession::new(target("sqlite", "customers"));
        session.target.database = "main".into();
        session.target.schema.clear();
        session.section = Section::Danger;
        let snapshot = Snapshot::bare(DdlEngine::Sqlite, "main", "", "customers");
        session.arm_fixture(&snapshot);
        assert!(session.requires_second_confirm(), "DROP 必须进入二次确认");
        assert!(session.preview_risks().iter().any(|(level, _)| level == "danger"));
        assert!(session.preview_rule().contains("逐条执行"));
        assert!(session.preview_target().contains("customers"), "{}", session.preview_target());
        session.arm_confirm();
        assert!(session.armed());
        assert!(!session.requires_second_confirm(), "第二次确认后不再拦");
        session.cancel();
        assert!(!session.armed(), "取消后必须重置二次确认");

        let dir = std::env::temp_dir().join(format!("rillsql-structure-reset-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let services = std::sync::Arc::new(crate::application::AppServices::new(std::sync::Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
        let mut pane = StructurePane::new(services);
        pane.bind(target("sqlite", "customers"));
        pane.type_into("column_name", "nickname");
        pane.session.section = Section::Indexes;
        pane.session.arm_fixture(&snapshot);
        assert!(pane.session.preview_open());
        pane.bind(target("sqlite", "orders"));
        assert_eq!(pane.session.target.table, "orders");
        assert!(pane.session.column_name.is_empty(), "换表不能带走上一张表的列名");
        assert_eq!(pane.session.section, Section::Columns);
        assert!(!pane.session.preview_open(), "换表不能带走上一张表的预览");
        assert!(pane.session.note().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 提示与读取错误留原文，显示时按当前语言给出：错误出现后再切换语言，文字立即跟着变。
    #[test]
    fn notes_and_load_errors_follow_a_later_locale_switch() {
        let cjk = crate::ddl::english::has_cjk;
        let mut session = StructureSession::new(target("sqlite", "customers"));
        session.note = Note::raw("这条连接还没有打开，没有另开新的连接。".to_owned());
        assert!(session.note().contains("没有打开"), "{}", session.note());
        session.locale = Locale::En;
        assert!(session.note().contains("isn't open") && !cjk(&session.note()), "{}", session.note());
        session.locale = Locale::ZhCn;
        assert!(session.note().contains("没有打开"), "{}", session.note());

        // 执行结果里的多条阻断逐条翻译后再连起来。
        session.note = Note::Raw(vec!["生成计划失败".to_owned(), "找不到表或视图 orders".to_owned()]);
        session.locale = Locale::En;
        assert_eq!(session.note(), "Generating the plan failed Table or view orders was not found");

        // 二次确认的提示与未连接提示也跟着语言变。
        session.target.database = "main".into();
        session.target.schema.clear();
        session.section = Section::Danger;
        session.locale = Locale::ZhCn;
        session.arm_fixture(&Snapshot::bare(DdlEngine::Sqlite, "main", "", "customers"));
        assert_eq!(session.note(), i18n::text(Locale::ZhCn, "admin.preview.unconnected"));
        session.arm_confirm();
        assert!(session.note().contains("再次点击"), "{}", session.note());
        session.locale = Locale::En;
        assert!(session.note().contains("Confirm again") && !cjk(&session.note()), "{}", session.note());
        assert!(!cjk(&session.confirm_sql().unwrap_err()), "未连接的拒绝原因按英文给出");

        let dir = std::env::temp_dir().join(format!("rillsql-structure-locale-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let services = std::sync::Arc::new(crate::application::AppServices::new(std::sync::Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
        let mut pane = StructurePane::new(services);
        pane.bind(target("sqlite", "customers"));
        pane.load_error = "找不到表或视图 customers".to_owned();
        assert!(pane.load_error_text().contains("找不到"), "{}", pane.load_error_text());
        pane.set_locale(Locale::En);
        assert_eq!(pane.load_error_text(), "Table or view customers was not found");
        pane.set_locale(Locale::ZhCn);
        assert!(pane.load_error_text().contains("找不到"), "{}", pane.load_error_text());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn typed_structure_fields_are_the_preview_draft() {
        let dir = std::env::temp_dir().join(format!("rillsql-structure-draft-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let services = std::sync::Arc::new(crate::application::AppServices::new(std::sync::Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
        let mut pane = StructurePane::new(services);
        pane.bind(target("sqlite", "customers"));
        pane.type_into("column_name", "nickname");
        match pane.session.action() {
            crate::ddl::Action::AlterColumns { columns, .. } => assert_eq!(columns[0].name, "nickname"),
            other => panic!("{other:?}"),
        }
        pane.session.section = Section::Indexes;
        pane.type_into("index_name", "ix_nick");
        pane.type_into("index_column", "nickname");
        match pane.session.action() {
            crate::ddl::Action::CreateIndex { name, columns, .. } => {
                assert_eq!(name, "ix_nick");
                assert_eq!(columns[0].name, "nickname");
            }
            other => panic!("{other:?}"),
        }
        pane.session.section = Section::ForeignKeys;
        pane.type_into("fk_name", "fk_parent");
        pane.type_into("fk_column", "parent_id");
        pane.type_into("fk_ref_table", "parents");
        pane.type_into("fk_ref_column", "id");
        match pane.session.action() {
            crate::ddl::Action::CreateForeignKey { foreign_key, .. } => {
                assert_eq!(foreign_key.name, "fk_parent");
                assert_eq!(foreign_key.columns, vec!["parent_id".to_owned()]);
                assert_eq!(foreign_key.ref_table, "parents");
            }
            other => panic!("{other:?}"),
        }
        pane.session.section = Section::View;
        pane.type_into("view_sql", "select 1");
        match pane.session.action() {
            crate::ddl::Action::ReplaceView { definition, .. } => assert_eq!(definition, "select 1"),
            other => panic!("{other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn sqlite_structure_cancel_leaves_the_file_and_confirm_applies_the_preview() {
        let dir = std::env::temp_dir().join(format!("rillsql-structure-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let database = dir.join("notes.sqlite");
        let setup = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).create_if_missing(true))
            .await
            .unwrap();
        sqlx::query("CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT)").execute(&setup).await.unwrap();
        sqlx::query("CREATE TABLE scratch (id INTEGER PRIMARY KEY)").execute(&setup).await.unwrap();
        setup.close().await;
        let service = std::sync::Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")));
        service.open_sqlite_for_test("notes", "笔记", &database).await.unwrap();
        let mut session = StructureSession::new(GridTarget {
            connection_id: "notes".into(),
            connection_name: "笔记".into(),
            database: "main".into(),
            schema: String::new(),
            table: "customers".into(),
            engine: "sqlite".into(),
            environment: "dev".into(),
        });
        session.section = Section::Columns;
        session.column_name = "extra".into();
        session.column_type = "TEXT".into();
        let blocked = crate::application::ddl::preview_request(&service, "notes", &session.request().unwrap()).await.unwrap();
        assert!(!blocked.blocked.is_empty(), "缺字段的草稿必须停在预览");
        session.arm_live(blocked);
        assert!(session.banner().contains("笔记") && session.banner().contains("dev") && session.banner().contains("customers"), "{}", session.banner());
        assert!(session.confirm_sql().is_err());
        session.kept_columns = vec![
            ColumnDraft { original: Some("id".into()), name: "id".into(), type_name: "INTEGER".into(), nullable: false, default_sql: String::new(), primary: true, auto: true, dropped: false },
            ColumnDraft { original: Some("name".into()), name: "name".into(), type_name: "TEXT".into(), nullable: true, default_sql: String::new(), primary: false, auto: false, dropped: false },
            ColumnDraft { original: None, name: "extra".into(), type_name: "TEXT".into(), nullable: true, default_sql: String::new(), primary: false, auto: false, dropped: false },
        ];
        let request = session.request().unwrap();
        let plan = crate::application::ddl::preview_request(&service, "notes", &request).await.unwrap();
        assert!(plan.blocked.is_empty(), "{:?}", plan.blocked);
        session.arm_live(plan);
        let sql = session.confirm_sql().unwrap();
        session.cancel();
        assert!(session.confirm_sql().is_err());
        let refused = match crate::application::ddl::execute_request(&service, "notes", &request, &sql, false).await {
            Err(message) => message,
            Ok(_) => panic!("未确认的结构变更应当被拒绝"),
        };
        assert!(refused.contains("确认"), "{refused}");
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).read_only(true))
            .await
            .unwrap();
        let extra: i64 = sqlx::query_scalar("SELECT count(*) FROM pragma_table_info('customers') WHERE name = 'extra'").fetch_one(&pool).await.unwrap();
        pool.close().await;
        assert_eq!(extra, 0);
        session.open = true;
        let done = crate::application::ddl::execute_request(&service, "notes", &request, &sql, true).await.unwrap();
        assert!(done.blocked.is_empty(), "{}", done.blocked.join("; "));
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).read_only(true))
            .await
            .unwrap();
        let extra: i64 = sqlx::query_scalar("SELECT count(*) FROM pragma_table_info('customers') WHERE name = 'extra'").fetch_one(&pool).await.unwrap();
        pool.close().await;
        assert_eq!(extra, 1);

        session.section = Section::Indexes;
        session.index_name = "ix_name".into();
        session.index_column = "name".into();
        let request = session.request().unwrap();
        let plan = crate::application::ddl::preview_request(&service, "notes", &request).await.unwrap();
        assert!(plan.blocked.is_empty(), "{:?}", plan.blocked);
        session.arm_live(plan);
        let index_sql = session.confirm_sql().unwrap();
        session.cancel();
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).read_only(true))
            .await
            .unwrap();
        let indexes: i64 = sqlx::query_scalar("SELECT count(*) FROM sqlite_master WHERE name = 'ix_name'").fetch_one(&pool).await.unwrap();
        pool.close().await;
        assert_eq!(indexes, 0);
        session.open = true;
        crate::application::ddl::execute_request(&service, "notes", &request, &index_sql, true).await.unwrap();
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).read_only(true))
            .await
            .unwrap();
        let indexes: i64 = sqlx::query_scalar("SELECT count(*) FROM sqlite_master WHERE name = 'ix_name'").fetch_one(&pool).await.unwrap();
        pool.close().await;
        assert_eq!(indexes, 1);

        session.target.table = "scratch".into();
        session.section = Section::Danger;
        let request = session.request().unwrap();
        let plan = crate::application::ddl::preview_request(&service, "notes", &request).await.unwrap();
        assert!(plan.steps.iter().any(|step| step.sql.to_ascii_uppercase().contains("DROP")), "{:?}", plan.blocked);
        session.arm_live(plan);
        let drop_sql = session.confirm_sql().unwrap();
        session.cancel();
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).read_only(true))
            .await
            .unwrap();
        let present: i64 = sqlx::query_scalar("SELECT count(*) FROM sqlite_master WHERE name = 'scratch'").fetch_one(&pool).await.unwrap();
        pool.close().await;
        assert_eq!(present, 1);
        session.open = true;
        crate::application::ddl::execute_request(&service, "notes", &request, &drop_sql, true).await.unwrap();
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).read_only(true))
            .await
            .unwrap();
        let present: i64 = sqlx::query_scalar("SELECT count(*) FROM sqlite_master WHERE name = 'scratch'").fetch_one(&pool).await.unwrap();
        pool.close().await;
        assert_eq!(present, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tabs_follow_the_object_and_original_names_let_alter_precheck_proceed() {
        let mut session = StructureSession::new(target("postgres", "orders"));
        assert_eq!(
            session.visible_sections(),
            vec![Section::Columns, Section::Indexes, Section::ForeignKeys, Section::Ddl, Section::Danger]
        );
        session.object_kind = "view".into();
        assert_eq!(session.visible_sections(), vec![Section::Columns, Section::View, Section::Danger]);
        assert!(!session.visible_sections().contains(&Section::Ddl));
        session.mode = StructureMode::Create;
        assert_eq!(session.visible_sections(), vec![Section::Columns]);

        let mut snapshot = Snapshot::bare(DdlEngine::Postgres, "app", "public", "orders");
        snapshot.kind = "table".into();
        snapshot.columns.push(column("name", "text", false));
        let mut drafts = column_drafts(&snapshot);
        assert_eq!(drafts[0].original.as_deref(), Some("name"));
        drafts[0].name = "title".into();
        let plan = preview_plan(&snapshot, &Action::AlterColumns { table: "orders".into(), columns: drafts });
        assert!(!plan.blocked.iter().any(|item| item.contains("没有出现")), "{:?}", plan.blocked);

        let mut created = StructureSession::new(target("sqlite", "ignored"));
        created.mode = StructureMode::Create;
        created.table_name = "notes".into();
        created.kept_columns = vec![default_id("sqlite")];
        match created.action() {
            Action::CreateTable { name, columns } => {
                assert_eq!(name, "notes");
                assert_eq!(columns[0].name, "id");
                assert!(columns[0].primary && columns[0].original.is_none());
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn indexes_and_foreign_keys_preview_drops_unique_order_and_actions() {
        let mut session = StructureSession::new(target("postgres", "orders"));
        session.section = Section::Indexes;
        session.index_name = "ix_note".into();
        session.index_unique = true;
        session.index_columns = vec![
            IndexColumnDraft { name: "note".into(), descending: false },
            IndexColumnDraft { name: "id".into(), descending: true },
        ];
        match session.action() {
            Action::CreateIndex { unique, columns, .. } => {
                assert!(unique);
                assert_eq!(columns.len(), 2);
                assert!(columns[1].descending);
            }
            other => panic!("{other:?}"),
        }
        session.intent = EditIntent::DropIndex { name: "orders_pkey".into() };
        assert!(matches!(session.action(), Action::DropIndex { name, .. } if name == "orders_pkey"));

        session.section = Section::ForeignKeys;
        session.intent = EditIntent::Apply;
        session.fk_name = "orders_parent".into();
        session.fk_column = "parent_id".into();
        session.fk_ref_schema = "public".into();
        session.fk_ref_table = "parents".into();
        session.fk_ref_column = "id".into();
        session.fk_on_delete = "CASCADE".into();
        session.fk_on_update = "RESTRICT".into();
        match session.action() {
            Action::CreateForeignKey { foreign_key, .. } => {
                assert_eq!(foreign_key.ref_schema, "public");
                assert_eq!(foreign_key.on_delete, "CASCADE");
                assert_eq!(foreign_key.on_update, "RESTRICT");
            }
            other => panic!("{other:?}"),
        }
        session.intent = EditIntent::DropForeignKey { name: "orders_parent".into() };
        assert!(matches!(session.action(), Action::DropForeignKey { .. }));
        assert!(fk_action_choices("postgres").contains(&"SET DEFAULT"));
        assert!(!fk_action_choices("mysql").contains(&"SET DEFAULT"));
        assert_eq!(fk_action_choices("mysql"), fk_action_choices("mariadb"));
    }

    #[test]
    fn production_danger_needs_the_object_name_and_a_failed_note_keeps_the_draft() {
        let mut session = StructureSession::new(target("sqlite", "customers"));
        session.target.environment = "prod".into();
        session.section = Section::Danger;
        assert!(!session.danger_name_ok());
        session.danger_name = "customers".into();
        assert!(session.danger_name_ok());
        session.danger_name = "other".into();
        assert!(!session.danger_name_ok());

        let dir = std::env::temp_dir().join(format!("rillsql-structure-keep-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let services = std::sync::Arc::new(crate::application::AppServices::new(std::sync::Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
        let mut pane = StructurePane::new(services);
        pane.begin_create(target("sqlite", "customers"));
        assert_eq!(pane.session.mode, StructureMode::Create);
        assert_eq!(pane.session.kept_columns[0].name, "id");
        pane.type_into("table_name", "notes");
        pane.type_into("column_name", "nickname");
        pane.session.note = Note::raw("生成计划失败".into());
        assert_eq!(pane.session.table_name, "notes");
        assert_eq!(pane.session.column_name, "nickname");
        assert_eq!(pane.session.kept_columns[0].name, "id");
        match pane.session.action() {
            Action::CreateTable { name, columns } => {
                assert_eq!(name, "notes");
                assert!(columns.iter().any(|column| column.name == "nickname"));
            }
            other => panic!("{other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn snapshot_with(columns: usize) -> Snapshot {
        let mut snapshot = Snapshot::bare(DdlEngine::Postgres, "app", "public", "orders");
        snapshot.kind = "table".into();
        for index in 0..columns {
            snapshot.columns.push(column(&format!("col_{index}"), if index == 0 { "bigint" } else { "integer" }, index == 0));
        }
        snapshot
    }

    /// 没有连接 id 的目标：渲染时不会发起读取，快照由用例直接注入。
    fn offline_target(table: &str) -> GridTarget {
        GridTarget {
            connection_id: String::new(),
            connection_name: String::new(),
            database: "app".into(),
            schema: "public".into(),
            table: table.into(),
            engine: "postgres".into(),
            environment: "dev".into(),
        }
    }

    fn pane_with_snapshot(services: std::sync::Arc<crate::application::AppServices>, columns: usize) -> StructurePane {
        let mut pane = StructurePane::new(services);
        pane.bind(offline_target("orders"));
        let snapshot = snapshot_with(columns);
        pane.session.object_kind = snapshot.kind.clone();
        pane.session.kept_columns = column_drafts(&snapshot);
        pane.session.index_columns.clear();
        pane.drafts_ready = true;
        pane.snapshot = Some(snapshot);
        pane
    }

    fn offline_services(name: &str) -> (std::sync::Arc<crate::application::AppServices>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("rillsql-structure-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let services = std::sync::Arc::new(crate::application::AppServices::new(std::sync::Arc::new(
            crate::connections::Service::new_for_test(dir.join("connections.sqlite")),
        )));
        (services, dir)
    }

    /// 字段、索引、外键三张列表共用一份行高、表头高与内边距，最小列宽在 640 宽窗口里放得下（S8.4 / R6）。
    #[test]
    fn structure_lists_share_one_row_height_and_fit_the_narrowest_window() {
        for metrics in [kit::Metrics::compact(), kit::Metrics::standard(), kit::Metrics::relaxed()] {
            let spec = TableSpec::of(metrics);
            assert_eq!(spec.header_h, metrics.row_h, "表头高取 row_h");
            assert_eq!(spec.row_h, metrics.row_h, "行高取 row_h，不再各表 +2");
            assert_eq!(spec.pad_x, TableSpec::PAD_X, "三张表共用同一左右内边距");
            for section in [Section::Columns, Section::Indexes, Section::ForeignKeys] {
                let columns = table_columns(section);
                assert!(columns.len() >= 3, "{section:?} 至少要能显示标识、内容与状态列");
                let total: f32 = columns.iter().map(|column| column.min_w).sum();
                let available = 640. - 2. * metrics.page_pad - 2. * spec.pad_x;
                assert!(total <= available, "{section:?}: 最小列宽之和 {total} 超过 640 宽可用的 {available}");
                assert!(columns.iter().all(|column| column.weight > 0.), "{section:?}: 每列都要能伸缩");
            }
        }
        // 反向：行高不能是个与密度无关的常量。
        assert_ne!(TableSpec::of(kit::Metrics::compact()).row_h, TableSpec::of(kit::Metrics::standard()).row_h);
    }

    /// 空列表不画表头冒充空态；加载优先于错误，错误优先于空态。
    #[test]
    fn empty_lists_have_no_header_and_loading_wins_over_error() {
        assert_eq!(list_state(false, "", 0), ListState::Empty);
        assert!(!shows_table_header(ListState::Empty), "空列表不能保留表头加破折号");
        assert_eq!(list_state(false, "", 3), ListState::Rows);
        assert!(shows_table_header(ListState::Rows));
        assert_eq!(list_state(true, "", 0), ListState::Loading);
        assert_eq!(list_state(true, "找不到表或视图 orders", 3), ListState::Loading, "加载优先于错误");
        assert_eq!(list_state(false, "找不到表或视图 orders", 0), ListState::Error);
    }

    /// 生产环境的危险操作在输入对象名之前不能确认；图标/配色按危险呈现。
    #[test]
    fn production_warning_name_gates_the_confirm_button() {
        let mut session = StructureSession::new(target("postgres", "orders"));
        session.target.environment = "prod".into();
        session.section = Section::Danger;
        session.arm_fixture(&snapshot_with(1));
        assert!(session.confirm_blocked(), "生产环境未输入对象名时确认必须禁用");
        assert!(session.preview_dangerous());
        session.danger_name = "orders".into();
        assert!(!session.confirm_blocked());
        session.section = Section::Columns;
        assert!(!session.confirm_blocked(), "非危险页签不受对象名约束");
        assert_eq!(session.object_label(), "orders");

        session.mode = StructureMode::Create;
        session.table_name = "notes".into();
        assert_eq!(session.object_label(), "notes", "新建表确认层标题用输入的表名");
    }

    /// 主操作固定在滚动内容之外：字段很多、窗口只有 640 宽时也直接可见可点（R6）。
    #[gpui_kit::test]
    fn primary_action_stays_outside_the_scrolled_body_at_every_width(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::test::TestWindowExt;
        use gpui_kit::AppContext as _;
        let (services, dir) = offline_services("actions");
        cx.update(gpui_kit::init);
        for width in [1360., 900., 640.] {
            let (window, _pane) = cx
                .update(|cx| {
                    let bounds = gpui_kit::Bounds {
                        origin: gpui_kit::Point::default(),
                        size: gpui_kit::size(gpui_kit::px(width), gpui_kit::px(900.)),
                    };
                    let (window, pane) = gpui_kit::open_window(
                        gpui_kit::WindowOptions { window_bounds: Some(gpui_kit::WindowBounds::Windowed(bounds)), ..Default::default() },
                        cx,
                        |_window, cx| cx.new(|_| pane_with_snapshot(services.clone(), 26)),
                    )
                    .expect("structure pane");
                    (window, pane)
                });
            let (button, body, pane_bounds, row_h) = cx
                .update_window(window, |_, window, cx| {
                    window.render_frame(cx);
                    (
                        window.find("structure-preview").bounds(),
                        window.find("structure-body").bounds(),
                        window.find("structure-pane").bounds(),
                        window.find("structure-row-columns-0").bounds(),
                    )
                })
                .unwrap();
            let bottom = f32::from(button.origin.y + button.size.height);
            assert!(bottom <= f32::from(pane_bounds.size.height) + 0.5, "{width}: 主操作被裁在窗口外（底 {bottom}）");
            assert!(
                f32::from(button.origin.y) >= f32::from(body.origin.y + body.size.height) - 0.5,
                "{width}: 主操作不能落在滚动内容里"
            );
            let expected = cx.update(|cx| kit::Metrics::of(cx).row_h);
            assert!((f32::from(row_h.size.height) - expected).abs() < 0.5, "{width}: 行高应为 Metrics::row_h {expected}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 行右键菜单的语义：复制只写剪贴板，"标记删除"只改草稿，任何菜单动作都不执行 SQL。
    ///
    /// 说明：ContextMenu 弹层在 headless 测试窗口里不展开（它用 hitbox.is_hovered 判断按下是否属于自己，
    /// 没有真实指针时恒为假），所以这里验证菜单项内容与它们触发的动作；行的渲染与行高由
    /// primary_action_stays_outside_the_scrolled_body_at_every_width 覆盖。
    #[gpui_kit::test]
    fn row_menu_actions_only_edit_the_draft_and_never_execute(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::AppContext as _;
        let (services, dir) = offline_services("menu");
        let pane = cx.update(|cx| cx.new(|_| pane_with_snapshot(services, 2)));
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                let mono = SharedString::from("mono");
                let rows = pane.list_rows(Section::Columns, Locale::ZhCn, &mono, &Palette::light());
                assert_eq!(rows.len(), 2, "两列就该有两个可右键的行");
                let ids = rows[1].menu.iter().map(|entry| entry.id.as_str()).collect::<Vec<_>>();
                assert_eq!(ids, vec!["copy-column-1", "drop-column-1"], "字段行菜单：复制名称 + 标记删除");
                assert!(rows[1].menu.iter().all(|entry| !entry.disabled));

                pane.apply_row_action(RowAction::Copy("col_1".into()), cx);
                pane.apply_row_action(RowAction::ToggleColumnDrop(1), cx);
                assert!(pane.session.kept_columns[1].dropped, "标记删除落在草稿上");
                assert!(!pane.session.preview_open(), "菜单动作不能绕过预览");
                assert!(!pane.session.armed(), "也不能把计划推到可执行状态");
                // 同一个菜单项再点一次是取消标记。
                pane.apply_row_action(RowAction::ToggleColumnDrop(1), cx);
                assert!(!pane.session.kept_columns[1].dropped, "再点一次取消删除标记");
            });
        });
        let copied = cx.update(|cx| cx.read_from_clipboard().and_then(|item| item.text()));
        assert_eq!(copied.as_deref(), Some("col_1"), "复制名称进剪贴板");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 浅/深色 × 紧凑/标准：行高与表头高随密度取 `row_h`，主题只换配色，主操作与空态位置不变（S9 / S12.2）。
    #[gpui_kit::test]
    fn structure_lists_follow_density_in_both_themes(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::test::TestWindowExt;
        use gpui_kit::AppContext as _;
        let (services, dir) = offline_services("density");
        for dark in [false, true] {
            for metrics in [kit::Metrics::compact(), kit::Metrics::standard()] {
                let expected = metrics.row_h;
                cx.update(|cx| {
                    gpui_kit::init(cx);
                    kit::apply_theme(dark, cx);
                    metrics.publish(cx);
                });
                let (window, _pane) = cx
                    .update(|cx| {
                        let bounds = gpui_kit::Bounds {
                            origin: gpui_kit::Point::default(),
                            size: gpui_kit::size(gpui_kit::px(900.), gpui_kit::px(900.)),
                        };
                        let (window, pane) = gpui_kit::open_window(
                            gpui_kit::WindowOptions { window_bounds: Some(gpui_kit::WindowBounds::Windowed(bounds)), ..Default::default() },
                            cx,
                            |_window, cx| cx.new(|_| pane_with_snapshot(services.clone(), 3)),
                        )
                        .expect("structure pane");
                        (window, pane)
                    });
                let (row, head, action) = cx
                    .update_window(window, |_, window, cx| {
                        window.render_frame(cx);
                        (
                            window.find("structure-row-columns-0").bounds(),
                            window.find("structure-table-head").bounds(),
                            window.find("structure-preview").visible(),
                        )
                    })
                    .unwrap();
                let theme = if dark { "深色" } else { "浅色" };
                assert!((f32::from(row.size.height) - expected).abs() < 0.5, "{theme}: 行高 {} 应为 {expected}", f32::from(row.size.height));
                assert!((f32::from(head.size.height) - expected).abs() < 0.5, "{theme}: 表头高应为 {expected}");
                assert!(action, "{theme}: 主操作在两种密度两种主题下都在位");
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 空索引列表整块换成空态：既没有表头，也没有"表头 + 破折号"的占位行。
    #[gpui_kit::test]
    fn empty_index_list_replaces_the_table_with_an_empty_state(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::test::TestWindowExt;
        use gpui_kit::AppContext as _;
        let (services, dir) = offline_services("empty-indexes");
        cx.update(gpui_kit::init);
        let (window, _pane) = cx.update(|cx| {
            let bounds = gpui_kit::Bounds {
                origin: gpui_kit::Point::default(),
                size: gpui_kit::size(gpui_kit::px(900.), gpui_kit::px(900.)),
            };
            let (window, pane) = gpui_kit::open_window(
                gpui_kit::WindowOptions { window_bounds: Some(gpui_kit::WindowBounds::Windowed(bounds)), ..Default::default() },
                cx,
                |_window, cx| {
                    cx.new(|_| {
                        let mut pane = pane_with_snapshot(services.clone(), 2);
                        pane.session.section = Section::Indexes;
                        pane
                    })
                },
            )
            .expect("structure pane");
            (window, pane)
        });
        let (head, empty, dash_rows) = cx
            .update_window(window, |_, window, cx| {
                window.render_frame(cx);
                (
                    window.try_find("structure-table-head").map(|head| head.visible()),
                    window.try_find("empty-state").map(|block| block.visible()),
                    window.find_all("structure-row-indexes-0").len(),
                )
            })
            .unwrap();
        assert!(head.is_none(), "空索引列表不能保留表头");
        assert_eq!(empty, Some(true), "空索引列表要换成空态块");
        assert_eq!(dash_rows, 0, "空列表不能留占位行");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 危险确认层在 640×640 里也放得下：只有正文滚动，取消与确认始终可见。
    #[gpui_kit::test]
    fn danger_dialog_keeps_its_actions_visible_in_a_short_narrow_window(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::test::TestWindowExt;
        use gpui_kit::AppContext as _;
        let (services, dir) = offline_services("dialog");
        cx.update(gpui_kit::init);
        let (window, _pane) = cx.update(|cx| {
            let bounds = gpui_kit::Bounds {
                origin: gpui_kit::Point::default(),
                size: gpui_kit::size(gpui_kit::px(640.), gpui_kit::px(640.)),
            };
            let (window, pane) = gpui_kit::open_window(
                gpui_kit::WindowOptions { window_bounds: Some(gpui_kit::WindowBounds::Windowed(bounds)), ..Default::default() },
                cx,
                |_window, cx| {
                    cx.new(|_| {
                        let mut pane = pane_with_snapshot(services.clone(), 8);
                        pane.session.section = Section::Danger;
                        let snapshot = snapshot_with(8);
                        pane.session.arm_fixture(&snapshot);
                        pane
                    })
                },
            )
            .expect("structure pane");
            (window, pane)
        });
        let (dialog, body, confirm, cancel, window_size) = cx
            .update_window(window, |_, window, cx| {
                window.render_frame(cx);
                (
                    window.find("structure-preview-dialog").bounds(),
                    window.find("structure-preview-body").bounds(),
                    window.find("structure-confirm").bounds(),
                    window.find("structure-cancel").bounds(),
                    window.find("structure-pane").bounds(),
                )
            })
            .unwrap();
        let limit = f32::from(window_size.size.height) * 0.92 + 1.;
        assert!(f32::from(dialog.size.height) <= limit, "确认层高 {} 超出窗口可用高度 {limit}", f32::from(dialog.size.height));
        assert!(f32::from(confirm.size.height) > 0., "确认按钮要真的画出来");
        for (name, bounds) in [("确认", confirm), ("取消", cancel)] {
            let bottom = f32::from(bounds.origin.y + bounds.size.height);
            assert!(bottom <= f32::from(window_size.size.height) + 0.5, "{name}按钮被裁在窗口外（底 {bottom}）");
        }
        assert!(
            f32::from(body.size.height) < f32::from(dialog.size.height),
            "正文与页脚分工：正文滚动，页脚固定"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 源码里出现的每一个结构页文案键都要在字典里。
    ///
    /// \`i18n::text\` 对未知键原样返回键名（曾把 \`structure.error.retry\` 这个不存在的键直接显示到两种语言里），
    /// 而 \`i18n\` 自己的测试只从"键→语言"方向检查，抓不到"用了不存在的键"。这里反向扫描本文件。
    #[test]
    fn every_structure_copy_key_used_in_the_source_exists_in_the_catalog() {
        let catalog = i18n::catalog_keys();
        let prefixes = ["structure.", "ui.structure.", "common.", "admin.", "catalog."];
        let mut checked = 0;
        for literal in string_literals(include_str!("mod.rs")) {
            if prefixes.contains(&literal.as_str()) || !prefixes.iter().any(|prefix| literal.starts_with(prefix)) {
                continue;
            }
            // 元素 id 用连字符（structure-row-…），键用点号；带占位符或空格的不是文案键。
            if literal.contains('{') || literal.contains(' ') {
                continue;
            }
            assert!(catalog.contains(&literal.as_str()), "{literal:?} 被当作文案键使用，但字典里没有它");
            checked += 1;
        }
        assert!(checked >= 20, "只扫到 {checked} 个文案键，扫描本身可能失效了");
    }

    /// 取出源码里的所有双引号字符串字面量（跳过 \`\\"\` 转义）。
    fn string_literals(source: &str) -> Vec<String> {
        let mut literals = Vec::new();
        let mut chars = source.chars();
        while let Some(ch) = chars.next() {
            if ch != '"' {
                continue;
            }
            let mut literal = String::new();
            let mut escaped = false;
            for inner in chars.by_ref() {
                if escaped {
                    escaped = false;
                    literal.push(inner);
                    continue;
                }
                match inner {
                    '\\' => escaped = true,
                    '"' => break,
                    _ => literal.push(inner),
                }
            }
            literals.push(literal);
        }
        literals
    }

    /// 读取失败与读取中在定义类页签（DDL / 危险操作）也要给原因、重试与骨架，
    /// 不能退化成"这个对象没有可显示的 SQL 定义"。
    #[gpui_kit::test]
    fn definition_tabs_share_the_loading_and_load_failure_states(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::test::TestWindowExt;
        use gpui_kit::AppContext as _;
        let (services, dir) = offline_services("definition-tabs");
        cx.update(gpui_kit::init);
        for section in [Section::Ddl, Section::Danger, Section::View] {
            // 读取中：骨架，不是空态文案。
            let (window, _pane) = cx.update(|cx| {
                let bounds = gpui_kit::Bounds {
                    origin: gpui_kit::Point::default(),
                    size: gpui_kit::size(gpui_kit::px(900.), gpui_kit::px(900.)),
                };
                gpui_kit::open_window(
                    gpui_kit::WindowOptions { window_bounds: Some(gpui_kit::WindowBounds::Windowed(bounds)), ..Default::default() },
                    cx,
                    |_window, cx| {
                        cx.new(|_| {
                            let mut pane = pane_with_snapshot(services.clone(), 2);
                            pane.session.section = section;
                            pane.snapshot = None;
                            pane.loading = true;
                            pane
                        })
                    },
                )
                .expect("structure pane")
            });
            let (skeleton, empty) = cx
                .update_window(window, |_, window, cx| {
                    window.render_frame(cx);
                    (window.try_find("skeleton-state").map(|block| block.visible()), window.try_find("empty-state").is_some())
                })
                .unwrap();
            assert_eq!(skeleton, Some(true), "{section:?}: 读取中要给骨架");
            assert!(!empty, "{section:?}: 读取中不能闪「没有可显示的 SQL 定义」");

            // 读取失败：错误态 + 可点的重试，按钮文案是翻译过的（不是键名）。
            let (window, pane) = cx.update(|cx| {
                let bounds = gpui_kit::Bounds {
                    origin: gpui_kit::Point::default(),
                    size: gpui_kit::size(gpui_kit::px(900.), gpui_kit::px(900.)),
                };
                gpui_kit::open_window(
                    gpui_kit::WindowOptions { window_bounds: Some(gpui_kit::WindowBounds::Windowed(bounds)), ..Default::default() },
                    cx,
                    |_window, cx| {
                        cx.new(|_| {
                            let mut pane = pane_with_snapshot(services.clone(), 2);
                            pane.session.section = section;
                            pane.snapshot = None;
                            pane.load_error = "找不到表或视图 orders".to_owned();
                            pane
                        })
                    },
                )
                .expect("structure pane")
            });
            let (error, empty, label, detail) = cx
                .update_window(window, |_, window, cx| {
                    window.render_frame(cx);
                    (
                        window.try_find("error-state").map(|block| block.visible()),
                        window.try_find("empty-state").is_some(),
                        window.find("structure-retry").label().map(str::to_owned),
                        window.find("error-state-detail").label().map(str::to_owned),
                    )
                })
                .unwrap();
            assert_eq!(error, Some(true), "{section:?}: 读取失败要给错误态");
            assert!(!empty, "{section:?}: 读取失败不能退化成「没有可显示的 SQL 定义」");
            assert_eq!(label.as_deref(), Some("重试"), "{section:?}: 重试按钮文案要翻译过，不能是键名");
            assert!(detail.is_none() || detail.as_deref() != Some("structure.empty.definition"));
            cx.update_window(window, |_, window, cx| window.click("structure-retry", cx)).unwrap();
            assert!(cx.update(|cx| pane.read(cx).load_error_text().is_empty()), "{section:?}: 重试要清掉失败原因");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
