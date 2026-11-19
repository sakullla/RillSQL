//! 对象结构工作台：字段设计器、索引管理、外键与被引用关系、视图与 DDL 定义、表级危险操作与安全变更预览。
//! 预览和执行只走 `application::ddl`，取消预览不发送任何 SQL。

use std::collections::HashSet;
use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState, InputEvent};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, Entity, FocusHandle, FontWeight,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, TestSupportExt as _, Window, div, px, relative,
};

use crate::app::views::grid::{GridTarget, group_digits, operation_banner};
use crate::application::AppServices;
use crate::ddl::english::{localize, localize_missing_item};
use crate::ddl::sql::{non_null_default, quote_ident, valid_default, valid_name, valid_type};
use crate::ddl::{
    Action, ColumnDraft, DdlEngine, ForeignKeyDraft, IndexColumnDraft, Plan, Request, Snapshot,
    fk_sql_for_display, index_sql_for_display, preview_plan,
};
use crate::i18n::{self, Locale};
use crate::ui::kit::{self, FieldSpec, Fields, Palette, Radius, Tip as _, TypeScale, stack_frame};
use crate::ui::{DataFormat, EditMode, FormatViewer, TruncationState, ViewerSpec};

/// 与 `ddl::model::fk_actions` 相同的级联动作。结构页按引擎原样列出。
fn fk_action_choices(engine: &str) -> &'static [&'static str] {
    match engine {
        "mysql" | "mariadb" => &["RESTRICT", "CASCADE", "SET NULL", "NO ACTION"],
        "redis" | "mongodb" => &[],
        _ => &["RESTRICT", "CASCADE", "SET NULL", "SET DEFAULT", "NO ACTION"],
    }
}

fn type_choices(engine: &str) -> &'static [&'static str] {
    match engine {
        "postgres" => &[
            "bigint",
            "integer",
            "smallint",
            "bigserial",
            "serial",
            "varchar(255)",
            "text",
            "boolean",
            "numeric(10,2)",
            "timestamptz",
            "timestamp",
            "date",
            "jsonb",
            "uuid",
            "bytea",
        ],
        "mysql" | "mariadb" => &[
            "bigint",
            "int",
            "tinyint(1)",
            "varchar(255)",
            "varchar(64)",
            "text",
            "decimal(10,2)",
            "datetime",
            "timestamp",
            "date",
            "json",
            "blob",
        ],
        "sqlserver" => &[
            "bigint",
            "int",
            "smallint",
            "tinyint",
            "bit",
            "nvarchar(255)",
            "varchar(255)",
            "decimal(18,2)",
            "datetime2",
            "date",
            "uniqueidentifier",
            "varbinary(max)",
        ],
        _ => &["INTEGER", "TEXT", "REAL", "NUMERIC", "BLOB"],
    }
}

fn default_choices(engine: &str, nullable: bool) -> Vec<&'static str> {
    let mut items = Vec::new();
    if nullable {
        items.push("NULL");
    }
    match engine {
        "postgres" => items.extend(["0", "''", "TRUE", "FALSE", "CURRENT_TIMESTAMP", "NOW()"]),
        "mysql" | "mariadb" => items.extend(["0", "''", "CURRENT_TIMESTAMP", "CURRENT_DATE"]),
        "sqlserver" => items.extend(["0", "''", "N''", "GETDATE()", "SYSDATETIME()", "GETUTCDATE()"]),
        _ => items.extend(["0", "''", "CURRENT_TIMESTAMP", "CURRENT_DATE"]),
    }
    items
}

fn default_column_type(engine: &str) -> &'static str {
    match engine {
        "postgres" => "text",
        "mysql" | "mariadb" => "varchar(255)",
        "sqlserver" => "nvarchar(255)",
        _ => "TEXT",
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColumnPreset {
    Timestamps,
    SoftDelete,
    StatusRemark,
    Uuid,
}

fn preset_columns(engine: &str, preset: ColumnPreset) -> Vec<ColumnDraft> {
    let ts_type = match engine {
        "postgres" => "timestamptz",
        "mysql" | "mariadb" => "datetime",
        "sqlserver" => "datetime2",
        _ => "TEXT",
    };
    let ts_default = match engine {
        "sqlserver" => "SYSDATETIME()",
        _ => "CURRENT_TIMESTAMP",
    };
    let str_short = match engine {
        "postgres" => "varchar(32)",
        "mysql" | "mariadb" => "varchar(32)",
        "sqlserver" => "nvarchar(32)",
        _ => "TEXT",
    };
    let str_note = match engine {
        "postgres" => "text",
        "mysql" | "mariadb" => "varchar(255)",
        "sqlserver" => "nvarchar(255)",
        _ => "TEXT",
    };
    let uuid_type = match engine {
        "postgres" => "uuid",
        "sqlserver" => "uniqueidentifier",
        "mysql" | "mariadb" => "char(36)",
        _ => "TEXT",
    };
    let col = |name: &str, kind: &str, nullable: bool, default_sql: &str| ColumnDraft {
        original: None,
        name: name.into(),
        type_name: kind.into(),
        nullable,
        default_sql: default_sql.into(),
        primary: false,
        auto: false,
        dropped: false,
    };
    match preset {
        ColumnPreset::Timestamps => vec![
            col("created_at", ts_type, false, ts_default),
            col("updated_at", ts_type, false, ts_default),
        ],
        ColumnPreset::SoftDelete => vec![col("deleted_at", ts_type, true, "")],
        ColumnPreset::StatusRemark => vec![
            col("status", str_short, false, "'active'"),
            col("remark", str_note, true, ""),
        ],
        ColumnPreset::Uuid => vec![col("uuid", uuid_type, false, "")],
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
    // 没有服务器原文时读取只留下“-- 库.表”一行占位；它不是定义，改用重建定义。
    if !is_placeholder_definition(&snapshot.table_sql) {
        return (snapshot.table_sql.clone(), Vec::new());
    }
    crate::ddl::rebuilt_definition(snapshot)
}

fn is_placeholder_definition(sql: &str) -> bool {
    sql.lines().map(str::trim).filter(|line| !line.is_empty()).all(|line| line.starts_with("--"))
}

fn suggested_index_name(table: &str, unique: bool, columns: &[IndexColumnDraft]) -> String {
    if columns.is_empty() {
        return String::new();
    }
    let prefix = if unique { "uk" } else { "idx" };
    let base = if table.trim().is_empty() { "table" } else { table.trim() };
    let cols = columns.iter().map(|col| col.name.trim()).filter(|name| !name.is_empty()).collect::<Vec<_>>().join("_");
    if cols.is_empty() {
        return String::new();
    }
    let candidate = format!("{prefix}_{base}_{cols}");
    if candidate.len() <= 60 { candidate } else { candidate.chars().take(60).collect() }
}

fn suggested_fk_name(table: &str, column: &str, ref_table: &str) -> String {
    let col = column.trim();
    let target = ref_table.trim();
    if col.is_empty() || target.is_empty() {
        return String::new();
    }
    let base = if table.trim().is_empty() { "table" } else { table.trim() };
    let candidate = format!("fk_{base}_{col}");
    if candidate.len() <= 60 { candidate } else { candidate.chars().take(60).collect() }
}

fn select_sample_sql(target: &GridTarget) -> String {
    let Ok(engine) = DdlEngine::parse(&target.engine) else {
        return format!("SELECT * FROM {} LIMIT 100;", target.table);
    };
    let qualified = match engine {
        DdlEngine::Sqlite => quote_ident(engine, &target.table),
        DdlEngine::Mysql | DdlEngine::Mariadb if !target.database.is_empty() => {
            format!("{}.{}", quote_ident(engine, &target.database), quote_ident(engine, &target.table))
        }
        DdlEngine::Postgres | DdlEngine::Sqlserver if !target.schema.is_empty() => {
            format!("{}.{}", quote_ident(engine, &target.schema), quote_ident(engine, &target.table))
        }
        _ => quote_ident(engine, &target.table),
    };
    if engine == DdlEngine::Sqlserver {
        format!("SELECT TOP 100 * FROM {qualified};")
    } else {
        format!("SELECT * FROM {qualified} LIMIT 100;")
    }
}

/// 从 `CREATE INDEX ... ON tbl (expr)` 里取出括号段，供表达式索引没有普通列名时展示。
fn extract_index_target_expr(sql: Option<&str>) -> String {
    let Some(raw) = sql.map(str::trim).filter(|text| !text.is_empty() && !text.starts_with("--")) else {
        return String::new();
    };
    let Some(open) = raw.find('(') else {
        return raw.to_owned();
    };
    let end = raw.rfind(')').unwrap_or(raw.len());
    if end > open + 1 { raw[open + 1..end].trim().to_owned() } else { raw.to_owned() }
}

/// 字段、索引、外键三张列表共用一份几何（S8.4：复用结果网格的表头、行高与列宽规范）。
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

/// 可直接编辑的字段表列宽。表头与行共用同一份：宽窗口用宽档，窄内容列整体换紧凑档，
/// 行尾动作在 640 宽侧栏开启时也不会被裁切（R6 / R7）。
#[derive(Clone, Copy, Debug, PartialEq)]
struct FieldWidths {
    name_min: f32,
    kind: f32,
    nullable: f32,
    default: f32,
    primary: f32,
    auto: f32,
    moves: f32,
    drop: f32,
}

impl FieldWidths {
    const GAP: f32 = 6.;
    const PAD_LEFT: f32 = 12.;
    const PAD_RIGHT: f32 = 8.;

    fn wide() -> Self {
        Self {
            name_min: 140.,
            kind: 174.,
            nullable: 52.,
            default: 156.,
            primary: 44.,
            auto: 44.,
            moves: 52.,
            drop: 46.,
        }
    }

    /// 紧凑档要放进最窄的真实内容列（640 - 52 活动栏 - 2 * 26 页边距 - 2 卡片边线 = 534）。
    /// 新建表有 7 个间隙与主键/自增/移动列，行尾只放删除（24px）；已有表放还原+删除（44px）。
    fn compact_for(create: bool) -> Self {
        Self {
            name_min: 96.,
            kind: 108.,
            nullable: 40.,
            default: 84.,
            primary: 36.,
            auto: 36.,
            moves: 40.,
            drop: if create { 24. } else { 44. },
        }
    }

    #[cfg(test)]
    fn compact() -> Self {
        Self::compact_for(true)
    }

    fn min_total(&self, create: bool) -> f32 {
        let mut total = self.name_min + self.kind + self.nullable + self.default + self.drop;
        let mut columns = 4;
        if create {
            total += self.primary + self.auto + self.moves;
            columns += 3;
        }
        total + Self::GAP * columns as f32 + Self::PAD_LEFT + Self::PAD_RIGHT
    }

    fn of(content_w: f32, create: bool) -> Self {
        let mut wide = Self::wide();
        if create {
            wide.drop = 28.;
        }
        let slack = content_w - wide.min_total(create);
        if slack < 0. {
            return Self::compact_for(create);
        }
        wide.default += (slack * 0.4).min(260.);
        wide
    }
}

const FIELDS_TABLE: [ColumnSpec; 4] = [
    ColumnSpec { key: "structure.field.column", weight: 2., min_w: 96. },
    ColumnSpec { key: "structure.field.type", weight: 1.4, min_w: 76. },
    ColumnSpec { key: "ui.structure.nullable", weight: 0.6, min_w: 52. },
    ColumnSpec { key: "ui.structure.default", weight: 1.4, min_w: 80. },
];

const INDEXES_TABLE: [ColumnSpec; 3] = [
    ColumnSpec { key: "structure.field.index", weight: 1.8, min_w: 96. },
    ColumnSpec { key: "structure.section.columns", weight: 2.2, min_w: 104. },
    ColumnSpec { key: "structure.index.kind", weight: 0.9, min_w: 68. },
];

const FOREIGN_TABLE: [ColumnSpec; 4] = [
    ColumnSpec { key: "structure.section.foreign", weight: 1.3, min_w: 84. },
    ColumnSpec { key: "structure.section.columns", weight: 1.1, min_w: 72. },
    ColumnSpec { key: "ui.structure.references", weight: 1.8, min_w: 108. },
    ColumnSpec { key: "structure.fk.rules", weight: 1.3, min_w: 96. },
];

const INCOMING_FK_TABLE: [ColumnSpec; 3] = [
    ColumnSpec { key: "structure.fk.incomingChild", weight: 1.8, min_w: 104. },
    ColumnSpec { key: "structure.fk.incomingTarget", weight: 1.4, min_w: 88. },
    ColumnSpec { key: "structure.section.foreign", weight: 1.4, min_w: 88. },
];

fn table_columns(section: Section) -> &'static [ColumnSpec] {
    match section {
        Section::Indexes => &INDEXES_TABLE,
        Section::ForeignKeys => &FOREIGN_TABLE,
        _ => &FIELDS_TABLE,
    }
}

/// 列表三态与内容态的判定：加载优先于错误，错误优先于空态。
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

/// 结构页提示。界面文字存 i18n 键，执行层/应用层的原文按条保存；显示时按当前语言给出。
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
        let initial_type = default_column_type(&target.engine).to_owned();
        Self {
            target,
            mode: StructureMode::Existing,
            section: Section::Columns,
            object_kind: String::new(),
            table_name: String::new(),
            column_name: String::new(),
            column_type: initial_type,
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

    pub fn note(&self) -> String {
        self.note.text(self.locale)
    }

    pub fn banner(&self) -> String {
        operation_banner(&self.target)
    }

    pub fn display_banner(&self) -> String {
        crate::app::views::grid::display_banner(&self.target, self.locale)
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

    pub fn preview_dangerous(&self) -> bool {
        self.section == Section::Danger
            || self
                .preview
                .as_ref()
                .is_some_and(|plan| plan.risks.iter().any(|risk| risk.level == "danger"))
    }

    pub fn confirm_blocked(&self) -> bool {
        self.section == Section::Danger && !self.danger_name_ok()
    }

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
        if self.index_column.trim().is_empty() {
            return Vec::new();
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
            Section::Columns if self.mode == StructureMode::Create => {
                Action::CreateTable { name: self.table_name.clone(), columns: self.column_drafts_for_action() }
            }
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

    pub fn preview_unchanged(&self) -> bool {
        self.preview.as_ref().is_some_and(|plan| {
            plan.steps.is_empty() && !plan.blocked.is_empty() && plan.blocked.iter().all(|text| crate::ddl::is_no_change(text))
        })
    }

    pub fn blocked(&self) -> Vec<String> {
        self.preview
            .as_ref()
            .map(|plan| plan.blocked.iter().map(|text| localize(text, self.locale)).collect())
            .unwrap_or_default()
    }

    pub fn preview_sql(&self) -> Vec<String> {
        self.preview.as_ref().map(|plan| plan.steps.iter().map(|step| step.sql.clone()).collect()).unwrap_or_default()
    }

    pub fn preview_steps(&self) -> Vec<(String, String)> {
        self.preview
            .as_ref()
            .map(|plan| {
                plan.steps
                    .iter()
                    .map(|step| (localize(&step.label, self.locale), step.sql.clone()))
                    .collect()
            })
            .unwrap_or_default()
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

type DataSink = Arc<dyn Fn(&mut App)>;
type QuerySink = Arc<dyn Fn(GridTarget, String, &mut App)>;
type StructureNavSink = Arc<dyn Fn(GridTarget, &mut App)>;

pub struct StructurePane {
    pub session: StructureSession,
    services: Arc<AppServices>,
    focus: Option<FocusHandle>,
    fields: Fields<&'static str>,
    draft_fields: Fields<u32>,
    snapshot: Option<Snapshot>,
    loaded_for: String,
    drafts_ready: bool,
    loading: bool,
    refreshing: bool,
    previewing: bool,
    executing: bool,
    load_error: String,
    item_id: String,
    /// 刚删除成功的表或视图 `(target, is_view)`。
    dropped: Option<(GridTarget, bool)>,
    /// 建表成功后的新表。
    created: Option<GridTarget>,
    /// 改表名成功后的 `(旧目标, 新表名)`。
    renamed: Option<(GridTarget, String)>,
    /// 结构修改（字段/索引/外键/视图定义）执行成功后的 `(target, is_view)`。
    modified: Option<(GridTarget, bool)>,
    fk_tables: Vec<String>,
    fk_schemas: Vec<String>,
    fk_columns: Vec<String>,
    fk_key: String,
    ddl_view: Option<Entity<FormatViewer>>,
    ddl_text: String,
    ddl_copied: bool,
    preview_copied: bool,
    view_editor: Option<Entity<EditorState>>,
    view_editor_sync: String,
    view_sub: Option<Subscription>,
    composer_open: bool,
    /// 字段筛选关键字（大表按列名或类型快速定位）。
    column_filter: String,
    /// 仅显示有未提交变更的字段行。
    only_changed_columns: bool,
    /// 新建索引名称是否跟随所选列自动生成（用户手动输入后转为 false，清空或点自动命名恢复 true）。
    index_name_auto: bool,
    /// 新建外键名称是否跟随所选列自动生成。
    fk_name_auto: bool,
    /// 下一帧要自动聚焦的 draft_fields 键（如建表时的表名框、刚新增的字段名框）。
    pending_focus_draft: Option<u32>,
    connect_request: Option<String>,
    content_width: f32,
    data_sink: Option<DataSink>,
    query_sink: Option<QuerySink>,
    structure_nav_sink: Option<StructureNavSink>,
}

impl StructurePane {
    pub fn new(services: Arc<AppServices>) -> Self {
        Self {
            session: StructureSession::new(GridTarget::default()),
            services,
            focus: None,
            fields: Fields::default(),
            draft_fields: Fields::default(),
            snapshot: None,
            loaded_for: String::new(),
            drafts_ready: false,
            loading: false,
            refreshing: false,
            previewing: false,
            executing: false,
            load_error: String::new(),
            item_id: String::new(),
            dropped: None,
            created: None,
            renamed: None,
            modified: None,
            fk_tables: Vec::new(),
            fk_schemas: Vec::new(),
            fk_columns: Vec::new(),
            fk_key: String::new(),
            ddl_view: None,
            ddl_text: String::new(),
            ddl_copied: false,
            preview_copied: false,
            view_editor: None,
            view_editor_sync: String::new(),
            view_sub: None,
            composer_open: false,
            column_filter: String::new(),
            only_changed_columns: false,
            index_name_auto: true,
            fk_name_auto: true,
            pending_focus_draft: None,
            connect_request: None,
            content_width: 0.,
            data_sink: None,
            query_sink: None,
            structure_nav_sink: None,
        }
    }

    pub fn set_data_sink(&mut self, sink: DataSink) {
        self.data_sink = Some(sink);
    }

    pub fn set_query_sink(&mut self, sink: QuerySink) {
        self.query_sink = Some(sink);
    }

    pub fn set_structure_nav_sink(&mut self, sink: StructureNavSink) {
        self.structure_nav_sink = Some(sink);
    }

    pub fn set_content_width(&mut self, width: f32) -> bool {
        if (self.content_width - width).abs() < 0.5 {
            return false;
        }
        self.content_width = width;
        true
    }

    #[cfg(test)]
    pub(crate) fn content_width(&self) -> f32 {
        self.content_width
    }

    #[cfg(test)]
    pub(crate) fn install_snapshot_for_test(&mut self, columns: usize) {
        let mut snapshot = crate::ddl::Snapshot::bare(crate::ddl::DdlEngine::Postgres, "app", "public", "orders");
        snapshot.kind = "table".into();
        for index in 0..columns {
            snapshot.columns.push(crate::ddl::Column {
                name: format!("col_{index}"),
                type_name: if index == 0 { "bigint" } else { "integer" }.into(),
                nullable: index != 0,
                default_sql: None,
                primary: index == 0,
                auto: false,
                generated: false,
                default_constraint: None,
            });
        }
        self.session.object_kind = snapshot.kind.clone();
        self.session.kept_columns = column_drafts(&snapshot);
        self.session.index_columns.clear();
        self.drafts_ready = true;
        self.snapshot = Some(snapshot);
    }

    pub fn take_created(&mut self) -> Option<GridTarget> {
        self.created.take()
    }

    pub fn take_dropped(&mut self) -> Option<GridTarget> {
        self.dropped.take().map(|(target, _)| target)
    }

    pub fn take_dropped_object(&mut self) -> Option<(GridTarget, bool)> {
        self.dropped.take()
    }

    pub fn take_renamed(&mut self) -> Option<(GridTarget, String)> {
        self.renamed.take()
    }

    pub fn take_modified(&mut self) -> Option<(GridTarget, bool)> {
        self.modified.take()
    }

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
        self.pending_focus_draft = Some(4);
    }

    pub fn bind(&mut self, target: GridTarget) {
        self.bind_item(target, "");
    }

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
        self.refreshing = false;
        self.previewing = false;
        self.executing = false;
        self.fk_tables.clear();
        self.fk_schemas.clear();
        self.fk_columns.clear();
        self.fk_key.clear();
        self.ddl_view = None;
        self.ddl_text.clear();
        self.ddl_copied = false;
        self.preview_copied = false;
        self.view_editor = None;
        self.view_editor_sync.clear();
        self.view_sub = None;
        self.composer_open = false;
        self.column_filter.clear();
        self.only_changed_columns = false;
        self.index_name_auto = true;
        self.fk_name_auto = true;
        self.pending_focus_draft = None;
        if target.table.is_empty() && !target.connection_id.is_empty() {
            self.enter_create();
        }
    }

    pub fn set_locale(&mut self, locale: Locale) {
        self.session.locale = locale;
    }

    pub fn load_error_text(&self) -> String {
        localize(&self.load_error, self.session.locale)
    }

    pub fn column_filter(&self) -> &str {
        &self.column_filter
    }

    pub fn set_column_filter(&mut self, value: String) {
        self.column_filter = value;
    }

    pub fn clear_column_filter(&mut self) {
        self.column_filter.clear();
        self.only_changed_columns = false;
    }

    pub fn type_into(&mut self, field: &str, text: &str) {
        let keep_preview = field == "danger_name";
        if field == "index_name" {
            self.index_name_auto = false;
        } else if field == "fk_name" {
            self.fk_name_auto = false;
        }
        let default_kind = default_column_type(&self.session.target.engine);
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
        } else if field == "column_type" && (slot.as_str() == "TEXT" || slot.as_str() == default_kind) {
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
        if self.previewing {
            return;
        }
        let Ok(request) = self.session.request() else {
            self.session.note = Note::Key("structure.preview.need");
            cx.notify();
            return;
        };
        self.previewing = true;
        self.preview_copied = false;
        cx.notify();
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
                            this.previewing = false;
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
        if self.executing {
            return;
        }
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
        let drop_kind = match &request.action {
            Action::DropTable { .. } => Some(false),
            Action::DropView { .. } => Some(true),
            _ => None,
        };
        let created_name = match &request.action {
            Action::CreateTable { name, .. } => Some(name.trim().to_owned()),
            _ => None,
        };
        let renamed_to = match &request.action {
            Action::RenameTable { new_name, .. } => Some(new_name.trim().to_owned()),
            _ => None,
        };
        let is_view = self.session.object_kind == "view";
        self.executing = true;
        cx.notify();
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
                            this.executing = false;
                            if this.session.target != target {
                                this.session.note = Note::Key("structure.execute.stale");
                                cx.notify();
                                return;
                            }
                            this.session.note = match result {
                                Ok(receipt) if receipt.blocked.is_empty() => {
                                    this.session.open = false;
                                    this.session.armed = false;
                                    let all_ok = receipt.steps.iter().all(|step| step.status == crate::ddl::StepStatus::Ok);
                                    if all_ok && !receipt.steps.is_empty() {
                                        if let Some(dropped_view) = drop_kind {
                                            this.dropped = Some((this.session.target.clone(), dropped_view));
                                        } else if let Some(new_name) = renamed_to.filter(|name| !name.is_empty()) {
                                            let old = this.session.target.clone();
                                            this.session.target.table = new_name.clone();
                                            this.session.rename_to.clear();
                                            this.session.danger_name.clear();
                                            this.session.section = Section::Columns;
                                            this.renamed = Some((old, new_name));
                                            this.reload_after_execute();
                                        } else if let Some(name) = created_name.filter(|name| !name.is_empty()) {
                                            let mut created = this.session.target.clone();
                                            created.table = name;
                                            this.created = Some(created.clone());
                                            this.session.mode = StructureMode::Existing;
                                            this.session.target = created;
                                            this.session.object_kind = "table".into();
                                            this.session.table_name.clear();
                                            this.reload_after_execute();
                                        } else {
                                            this.modified = Some((this.session.target.clone(), is_view));
                                            this.composer_open = false;
                                            this.reset_composer_drafts();
                                            this.reload_after_execute();
                                        }
                                    }
                                    Note::Key("structure.preview.done")
                                }
                                Ok(receipt) => {
                                    this.session.armed = false;
                                    Note::Raw(receipt.blocked)
                                }
                                Err(message) => {
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
        self.load_error.clear();
        if self.snapshot.is_some() {
            self.refreshing = true;
        } else {
            self.loading = true;
        }
        let services = Arc::clone(&self.services);
        let target = target.clone();
        let view = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::app::views::grid::runtime_for_views().spawn(async move {
                let result = crate::application::ddl::describe_saved(
                    &services.connections,
                    &target.connection_id,
                    target.database.clone(),
                    target.schema.clone(),
                    &target.engine,
                    &target.table,
                )
                .await;
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
                            this.refreshing = false;
                            match result {
                                Ok(snapshot) => {
                                    this.adopt_snapshot(&snapshot);
                                    this.snapshot = Some(snapshot);
                                }
                                Err(message) => {
                                    this.snapshot = None;
                                    this.load_error = message;
                                }
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

    /// 手动刷新结构：没有未应用的字段修改时同步重读最新列草稿；已有未保存编辑时只更新底层快照对照。
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.loading || self.refreshing || self.session.mode == StructureMode::Create {
            return;
        }
        if self.pending_changes() == 0 {
            self.drafts_ready = false;
            self.session.view_sql.clear();
        }
        self.fk_key.clear();
        self.ddl_view = None;
        self.ddl_text.clear();
        self.retry_load(cx);
    }

    fn reset_composer_drafts(&mut self) {
        self.session.index_name.clear();
        self.session.index_column.clear();
        self.session.index_unique = false;
        self.session.index_columns.clear();
        self.index_name_auto = true;
        self.session.fk_name.clear();
        self.session.fk_column.clear();
        self.session.fk_ref_table.clear();
        self.session.fk_ref_column.clear();
        self.session.fk_on_delete = "NO ACTION".to_owned();
        self.session.fk_on_update = "NO ACTION".to_owned();
        self.fk_name_auto = true;
    }

    fn set_field(&mut self, field: &'static str, value: String) {
        if field == "column_filter" {
            self.column_filter = value;
            return;
        }
        if field == "index_name" {
            if value.trim().is_empty() {
                self.index_name_auto = true;
            } else {
                let auto = suggested_index_name(&self.session.target.table, self.session.index_unique, &self.session.index_columns);
                if value != auto {
                    self.index_name_auto = false;
                }
            }
        } else if field == "fk_name" {
            if value.trim().is_empty() {
                self.fk_name_auto = true;
            } else {
                let auto = suggested_fk_name(&self.session.target.table, &self.session.fk_column, &self.session.fk_ref_table);
                if value != auto {
                    self.fk_name_auto = false;
                }
            }
        }
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
            "column_filter" => self.column_filter.clone(),
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

    pub fn revert_column(&mut self, index: usize) {
        let Some(original_name) = self.session.kept_columns.get(index).and_then(|col| col.original.clone()) else {
            return;
        };
        let Some(before) = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.columns.iter().find(|col| col.name == original_name))
            .cloned()
        else {
            return;
        };
        if let Some(draft) = self.session.kept_columns.get_mut(index) {
            draft.name = before.name;
            draft.type_name = before.type_name;
            draft.nullable = before.nullable;
            draft.default_sql = before.default_sql.unwrap_or_default();
            draft.primary = before.primary;
            draft.auto = before.auto;
            draft.dropped = false;
            self.session.cancel();
        }
    }

    fn toggle_column(&mut self, index: usize, which: &str) {
        let create = self.session.mode == StructureMode::Create;
        if which == "drop" {
            let is_unsaved_new = self.session.kept_columns.get(index).is_some_and(|col| col.original.is_none());
            if is_unsaved_new && (!create || self.session.kept_columns.len() > 1) {
                self.session.kept_columns.remove(index);
                self.session.cancel();
                return;
            }
        }
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
                if column.auto {
                    column.nullable = false;
                }
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

    pub fn add_column(&mut self) {
        let name = if self.session.column_name.trim().is_empty() {
            let mut seq = self.session.kept_columns.len() + 1;
            loop {
                let candidate = format!("col_{seq}");
                if self.session.kept_columns.iter().all(|col| !col.name.eq_ignore_ascii_case(&candidate)) {
                    break candidate;
                }
                seq += 1;
            }
        } else {
            self.session.column_name.trim().to_owned()
        };
        if self.session.kept_columns.iter().any(|column| column.name == name) {
            return;
        }
        let new_index = self.session.kept_columns.len();
        self.session.kept_columns.push(ColumnDraft {
            original: None,
            name,
            type_name: default_column_type(&self.session.target.engine).to_owned(),
            nullable: true,
            default_sql: String::new(),
            primary: false,
            auto: false,
            dropped: false,
        });
        self.session.column_name.clear();
        self.only_changed_columns = false;
        self.pending_focus_draft = Some(10_000 + new_index as u32);
        self.session.cancel();
    }

    pub fn apply_column_preset(&mut self, preset: ColumnPreset) {
        let templates = preset_columns(&self.session.target.engine, preset);
        let mut added = false;
        for draft in templates {
            if self.session.kept_columns.iter().all(|col| !col.name.eq_ignore_ascii_case(&draft.name)) {
                self.session.kept_columns.push(draft);
                added = true;
            }
        }
        if added {
            self.session.cancel();
        }
    }

    fn reset_draft(&mut self) {
        self.session.cancel();
        self.session.column_name.clear();
        self.session.column_type = default_column_type(&self.session.target.engine).into();
        self.session.intent = EditIntent::Apply;
        self.reset_composer_drafts();
        self.only_changed_columns = false;
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

    fn sync_auto_index_name(&mut self) {
        if self.index_name_auto || self.session.index_name.trim().is_empty() {
            self.session.index_name = suggested_index_name(&self.session.target.table, self.session.index_unique, &self.session.index_columns);
            self.index_name_auto = true;
        }
    }

    pub fn apply_auto_index_name(&mut self) {
        self.index_name_auto = true;
        self.session.index_name = suggested_index_name(&self.session.target.table, self.session.index_unique, &self.session.index_columns);
        self.session.cancel();
    }

    fn toggle_index_column(&mut self, name: &str) {
        if let Some(index) = self.session.index_columns.iter().position(|column| column.name == name) {
            self.session.index_columns.remove(index);
        } else {
            self.session.index_columns.push(IndexColumnDraft { name: name.to_owned(), descending: false });
        }
        self.session.index_column = self.session.index_columns.first().map(|column| column.name.clone()).unwrap_or_default();
        self.sync_auto_index_name();
        self.session.intent = EditIntent::Apply;
        self.session.cancel();
    }

    pub fn move_index_column(&mut self, index: usize, delta: isize) {
        let next = index as isize + delta;
        if next < 0 || next >= self.session.index_columns.len() as isize {
            return;
        }
        self.session.index_columns.swap(index, next as usize);
        self.session.index_column = self.session.index_columns.first().map(|column| column.name.clone()).unwrap_or_default();
        self.sync_auto_index_name();
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
        if matches!(field, "fk_column" | "fk_ref_table") && (self.fk_name_auto || self.session.fk_name.trim().is_empty()) {
            let suggested = suggested_fk_name(&self.session.target.table, &self.session.fk_column, &self.session.fk_ref_table);
            if !suggested.is_empty() {
                self.session.fk_name = suggested;
                self.fk_name_auto = true;
            }
        }
    }

    fn field(&mut self, field: &'static str, key: &str, placeholder: &str, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let value = self.field_value(field);
        let state = self.fields.bind(field, &value, FieldSpec { placeholder, masked: false }, window, cx, |this: &mut Self, field, value, _| {
            this.set_field(field, value)
        });
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
                            // 刚选定引用表且引用列为空时，优先预填主键或 id 列，省去再点一次下拉。
                            if this.session.fk_ref_column.trim().is_empty() && !this.fk_columns.is_empty() {
                                if let Some(pk) = loaded.primary_column.or_else(|| {
                                    this.fk_columns.iter().find(|col| col.eq_ignore_ascii_case("id")).cloned().or_else(|| {
                                        (this.fk_columns.len() == 1).then(|| this.fk_columns[0].clone())
                                    })
                                }) {
                                    this.session.fk_ref_column = pk;
                                }
                            }
                            cx.notify();
                        });
                    }
                });
            }
        })
        .detach();
    }

    fn ddl_viewer(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) -> Entity<FormatViewer> {
        if let Some(view) = self.ddl_view.clone() {
            if self.ddl_text != text {
                self.ddl_text = text.to_owned();
                self.ddl_copied = false;
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
            viewer.set_code_only(true, cx);
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
                return kit::empty_state(IconName::Braces, t("ui.structure.none"), t("structure.empty.definition"), p).into_any_element();
            }
            ListState::Empty => {
                let (icon, heading, detail) = match section {
                    Section::Indexes => (IconName::Hash, "structure.empty.indexes", "structure.empty.indexesHint"),
                    Section::ForeignKeys => (IconName::Key, "structure.empty.foreignKeys", "structure.empty.foreignKeysHint"),
                    _ => (IconName::Table, "structure.empty.columns", "structure.empty.columnsHint"),
                };
                if matches!(section, Section::Indexes | Section::ForeignKeys) && !self.composer_open {
                    let owner = entity.clone();
                    let cta_label = if section == Section::Indexes { t("structure.index.new") } else { t("structure.fk.new") };
                    let cta = kit::primary_button("structure-empty-create", Some(IconName::Plus), cta_label, move |_, _, cx| {
                        owner.update(cx, |pane, cx| {
                            pane.composer_open = true;
                            pane.session.cancel();
                            cx.notify();
                        });
                    });
                    return kit::empty_state_with_action(icon, t(heading), t(detail), cta, p).into_any_element();
                }
                return kit::empty_state(icon, t(heading), t(detail), p).into_any_element();
            }
            ListState::Rows => {}
        }
        if list {
            return self.list_card(entity, section, p, cx);
        }
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

    fn load_error_state(&self, entity: &Entity<StructurePane>, p: &Palette, locale: Locale) -> AnyElement {
        if self.connection_closed() {
            let owner = entity.clone();
            let connect = kit::primary_button(
                "structure-retry",
                Some(IconName::Plug),
                SharedString::from(i18n::text(locale, "grid.action.connectRead")),
                move |_, _, cx| {
                    owner.update(cx, |pane, cx| pane.retry_load(cx));
                },
            )
            .into_any_element();
            return kit::empty_state_with_action(
                IconName::Plug,
                SharedString::from(i18n::text(locale, "grid.state.offlineTitle")),
                SharedString::from(i18n::text(locale, "grid.state.offlineHint")),
                connect,
                p,
            )
            .into_any_element();
        }
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

    fn has_definition(&self) -> bool {
        self.snapshot.as_ref().map(|snapshot| !definition_text(snapshot).0.trim().is_empty()).unwrap_or(false)
    }

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
                    let mut menu = vec![MenuEntry {
                        id: format!("copy-column-{index}"),
                        label: text("structure.menu.copyName"),
                        action: RowAction::Copy(column.name.clone()),
                        disabled: false,
                        reason: None,
                    }];
                    if self.session.object_kind != "view" {
                        menu.push(MenuEntry {
                            id: format!("drop-column-{index}"),
                            label: text(if dropped { "structure.menu.restoreColumn" } else { "structure.menu.dropColumn" }),
                            action: RowAction::ToggleColumnDrop(index),
                            disabled: false,
                            reason: None,
                        });
                    }
                    RowSpec {
                        id: format!("columns-{index}"),
                        dim: dropped,
                        cells: vec![
                            cell_main(column.name.clone(), column.primary.then_some(IconName::Key), p),
                            cell_mono(column.type_name.clone(), mono, p),
                            cell_text(if column.nullable { "✓".into() } else { "—".into() }, p),
                            cell_mono(column.default_sql.clone().unwrap_or_else(|| "—".into()), mono, p),
                        ],
                        menu,
                    }
                })
                .collect(),
            Section::Indexes => snapshot
                .indexes
                .iter()
                .enumerate()
                .map(|(index, row)| {
                    let reason = localize(&row.reason, locale);
                    let is_pk = row.protected && row.reason == "由主键约束维护";
                    let cols_display = if !row.columns.is_empty() {
                        row.columns
                            .iter()
                            .map(|column| if column.descending { format!("{} DESC", column.name) } else { column.name.clone() })
                            .collect::<Vec<_>>()
                            .join(", ")
                    } else {
                        extract_index_target_expr(row.sql.as_deref())
                    };
                    let mut kind_badges = div().flex().items_center().gap(px(4.)).min_w(px(0.));
                    if is_pk {
                        kind_badges = kind_badges.child(kit::pill(text("structure.index.primaryBadge"), p.warn, p.warn_soft));
                    } else if row.unique {
                        kind_badges = kind_badges.child(kit::pill(text("structure.index.uniqueBadge"), p.accent_text, p.accent_soft));
                    } else {
                        kind_badges = kind_badges.child(kit::pill(text("structure.index.normalBadge"), p.text_muted, p.hover));
                    }
                    if row.partial {
                        kind_badges = kind_badges.child(kit::pill(text("structure.index.partialBadge"), p.info, p.accent_soft));
                    } else if row.expression {
                        kind_badges = kind_badges.child(kit::pill(text("structure.index.exprBadge"), p.info, p.accent_soft));
                    }
                    let name_cell = div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .min_w(px(0.))
                        .child(div().truncate().font_weight(FontWeight::MEDIUM).child(SharedString::from(row.name.clone())))
                        .when(row.protected, |this| {
                            let tip = if reason.is_empty() { text("structure.index.primaryBadge") } else { reason.clone() };
                            this.child(
                                div()
                                    .id(SharedString::from(format!("structure-index-lock-{index}")))
                                    .test_support()
                                    .flex_none()
                                    .tooltip_text(SharedString::from(tip))
                                    .child(kit::icon(if is_pk { IconName::Key } else { IconName::Lock }, 12., if is_pk { p.warn } else { p.text_faint })),
                            )
                        })
                        .into_any_element();
                    let mut menu = vec![MenuEntry {
                        id: format!("copy-index-{index}"),
                        label: text("structure.menu.copyName"),
                        action: RowAction::Copy(row.name.clone()),
                        disabled: false,
                        reason: None,
                    }];
                    if let Some(sql) = index_sql_for_display(snapshot, row) {
                        menu.push(MenuEntry {
                            id: format!("copy-index-sql-{index}"),
                            label: text("structure.index.copySql"),
                            action: RowAction::Copy(sql),
                            disabled: false,
                            reason: None,
                        });
                    }
                    menu.push(MenuEntry {
                        id: format!("drop-index-{index}"),
                        label: text("structure.index.delete"),
                        action: RowAction::DropIndex(row.name.clone()),
                        disabled: row.protected,
                        reason: (row.protected && !reason.is_empty()).then_some(reason),
                    });
                    RowSpec {
                        id: format!("indexes-{index}"),
                        dim: false,
                        cells: vec![name_cell, cell_mono(cols_display, mono, p), kind_badges.into_any_element()],
                        menu,
                    }
                })
                .collect(),
            Section::ForeignKeys => snapshot
                .foreign_keys
                .iter()
                .enumerate()
                .map(|(index, key)| {
                    let reference = if key.ref_schema.is_empty() {
                        key.ref_table.clone()
                    } else {
                        format!("{}.{}", key.ref_schema, key.ref_table)
                    };
                    let name = if key.name.is_empty() { format!("fk_{index}") } else { key.name.clone() };
                    let target_schema = if key.ref_schema.is_empty() { self.session.target.schema.clone() } else { key.ref_schema.clone() };
                    let mut menu = vec![MenuEntry {
                        id: format!("copy-fk-{index}"),
                        label: text("structure.menu.copyName"),
                        action: RowAction::Copy(name.clone()),
                        disabled: false,
                        reason: None,
                    }];
                    if !key.ref_table.is_empty() && self.structure_nav_sink.is_some() {
                        menu.push(MenuEntry {
                            id: format!("open-fk-ref-{index}"),
                            label: text("structure.fk.openRef"),
                            action: RowAction::OpenTable { schema: target_schema.clone(), table: key.ref_table.clone() },
                            disabled: false,
                            reason: None,
                        });
                    }
                    if let Some(sql) = fk_sql_for_display(snapshot, key) {
                        menu.push(MenuEntry {
                            id: format!("copy-fk-sql-{index}"),
                            label: text("structure.fk.copySql"),
                            action: RowAction::Copy(sql),
                            disabled: false,
                            reason: None,
                        });
                    }
                    menu.push(MenuEntry {
                        id: format!("drop-fk-{index}"),
                        label: text("structure.fk.delete"),
                        action: RowAction::DropForeignKey(name.clone()),
                        disabled: key.name.is_empty(),
                        reason: None,
                    });
                    let ref_display = format!("{reference}({})", key.ref_columns.join(", "));
                    let ref_cell = if !key.ref_table.is_empty() && self.structure_nav_sink.is_some() {
                        let nav_sink = self.structure_nav_sink.clone();
                        let mut ref_target = self.session.target.clone();
                        ref_target.schema = target_schema;
                        ref_target.table = key.ref_table.clone();
                        kit::interactive(
                            div().id(SharedString::from(format!("structure-fk-jump-{index}"))),
                            p,
                            false,
                        )
                        .test_support()
                        .px(px(4.))
                        .mx(px(-4.))
                        .rounded(px(Radius::TAG))
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .min_w(px(0.))
                        .tooltip_text(SharedString::from(text("structure.fk.openRef")))
                        .child(
                            div()
                                .truncate()
                                .font_family(mono.clone())
                                .text_size(px(TypeScale::SM))
                                .text_color(p.accent_text)
                                .child(SharedString::from(ref_display)),
                        )
                        .child(kit::icon(IconName::ArrowUpRight, 12., p.accent_text))
                        .on_click(move |_, _, cx| {
                            if let Some(sink) = &nav_sink {
                                sink(ref_target.clone(), cx);
                            }
                        })
                        .into_any_element()
                    } else {
                        cell_mono(ref_display, mono, p)
                    };
                    RowSpec {
                        id: format!("foreign-{index}"),
                        dim: false,
                        cells: vec![
                            cell_main(name, Some(IconName::Key), p),
                            cell_mono(key.columns.join(", "), mono, p),
                            ref_cell,
                            fk_rules_cell(&key.on_delete, &key.on_update, p),
                        ],
                        menu,
                    }
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    fn incoming_fk_card(&self, entity: &Entity<StructurePane>, p: &Palette, cx: &App) -> Option<AnyElement> {
        let snapshot = self.snapshot.as_ref()?;
        if snapshot.incoming.is_empty() || self.loading || !self.load_error.is_empty() {
            return None;
        }
        let locale = self.session.locale;
        let text = |key: &str| i18n::text(locale, key);
        let mono = cx.theme().mono_font_family.clone();
        let spec = TableSpec::of(kit::Metrics::of(cx));
        let rows: Vec<RowSpec> = snapshot
            .incoming
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let child_qualified = if item.schema.is_empty() {
                    item.table.clone()
                } else {
                    format!("{}.{}", item.schema, item.table)
                };
                let child_display = format!("{child_qualified}({})", item.columns.join(", "));
                let child_schema = if item.schema.is_empty() { self.session.target.schema.clone() } else { item.schema.clone() };
                let child_cell = if !item.table.is_empty() && self.structure_nav_sink.is_some() {
                    let nav_sink = self.structure_nav_sink.clone();
                    let mut child_target = self.session.target.clone();
                    child_target.schema = child_schema.clone();
                    child_target.table = item.table.clone();
                    kit::interactive(
                        div().id(SharedString::from(format!("structure-incoming-jump-{index}"))),
                        p,
                        false,
                    )
                    .test_support()
                    .px(px(4.))
                    .mx(px(-4.))
                    .rounded(px(Radius::TAG))
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .min_w(px(0.))
                    .tooltip_text(SharedString::from(text("structure.fk.openChild")))
                    .child(
                        div()
                            .truncate()
                            .font_family(mono.clone())
                            .text_size(px(TypeScale::SM))
                            .text_color(p.accent_text)
                            .child(SharedString::from(child_display)),
                    )
                    .child(kit::icon(IconName::ArrowUpRight, 12., p.accent_text))
                    .on_click(move |_, _, cx| {
                        if let Some(sink) = &nav_sink {
                            sink(child_target.clone(), cx);
                        }
                    })
                    .into_any_element()
                } else {
                    cell_mono(child_display, &mono, p)
                };
                let fk_label = if item.name.is_empty() { format!("fk_in_{index}") } else { item.name.clone() };
                let mut menu = vec![MenuEntry {
                    id: format!("copy-incoming-{index}"),
                    label: text("structure.menu.copyName"),
                    action: RowAction::Copy(item.table.clone()),
                    disabled: false,
                    reason: None,
                }];
                if !item.table.is_empty() && self.structure_nav_sink.is_some() {
                    menu.push(MenuEntry {
                        id: format!("open-incoming-{index}"),
                        label: text("structure.fk.openChild"),
                        action: RowAction::OpenTable { schema: child_schema, table: item.table.clone() },
                        disabled: false,
                        reason: None,
                    });
                }
                RowSpec {
                    id: format!("incoming-{index}"),
                    dim: false,
                    cells: vec![
                        child_cell,
                        cell_mono(format!("{}({})", self.session.target.table, item.ref_columns.join(", ")), &mono, p),
                        cell_text(fk_label, p),
                    ],
                    menu,
                }
            })
            .collect();
        let heading = SharedString::from(i18n::fill(
            &i18n::text(locale, "structure.fk.incomingTitle"),
            &[("count", &snapshot.incoming.len().to_string())],
        ));
        Some(
            div()
                .id("structure-incoming-fks")
                .test_support()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(kit::eyebrow(heading, p))
                .child(kit::card(p).overflow_hidden().child(structure_table(entity, &INCOMING_FK_TABLE, rows, spec, p, locale)))
                .into_any_element(),
        )
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

    fn connection_closed(&self) -> bool {
        let connection = &self.session.target.connection_id;
        !connection.is_empty() && self.services.connections.catalog_session(connection).is_none()
    }

    fn reload_after_execute(&mut self) {
        self.drafts_ready = false;
        self.loaded_for.clear();
        self.snapshot = None;
        self.ddl_view = None;
        self.ddl_text.clear();
    }

    pub fn retry_load(&mut self, cx: &mut Context<Self>) {
        let connection = self.session.target.connection_id.clone();
        if !connection.is_empty() && self.services.connections.catalog_session(&connection).is_none() {
            self.connect_request = Some(connection);
            cx.notify();
            return;
        }
        self.load_error.clear();
        self.loaded_for.clear();
        self.loading = false;
        self.refreshing = false;
        cx.notify();
    }

    pub fn take_connect_request(&mut self) -> Option<String> {
        self.connect_request.take()
    }

    pub fn retry_after_connect(&mut self, connection_id: &str, cx: &mut Context<Self>) {
        if self.session.target.connection_id == connection_id && !self.load_error.is_empty() {
            self.retry_load(cx);
        }
    }

    fn form_visible(&self) -> bool {
        self.session.mode == StructureMode::Create || (!self.loading && self.load_error.is_empty())
    }

    fn preview_action_visible(&self) -> bool {
        let section = self.session.section;
        let readonly_view = self.session.object_kind == "view" && self.session.mode != StructureMode::Create;
        let composing = !matches!(section, Section::Indexes | Section::ForeignKeys) || self.composer_open;
        self.form_visible() && composing && !matches!(section, Section::Ddl) && !(section == Section::Columns && readonly_view)
    }

    fn apply_row_action(&mut self, action: RowAction, cx: &mut Context<Self>) {
        match action {
            RowAction::Copy(text) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
            RowAction::ToggleColumnDrop(index) => self.toggle_column(index, "drop"),
            RowAction::RevertColumn(index) => self.revert_column(index),
            RowAction::AddColumn => self.add_column(),
            RowAction::DropIndex(name) => self.preview_drop_index(name, cx),
            RowAction::DropForeignKey(name) => self.preview_drop_fk(name, cx),
            RowAction::OpenTable { schema, table } => {
                if let Some(sink) = self.structure_nav_sink.clone() {
                    let mut target = self.session.target.clone();
                    target.schema = schema;
                    target.table = table;
                    sink(target, cx);
                }
            }
        }
    }

    /// 草稿前置诊断：在点击“预览 SQL”之前即时标出重名、空列名、非法类型/默认值或非空无默认值提示。
    pub fn draft_diagnostic(&self) -> Option<String> {
        let locale = self.session.locale;
        if self.session.section != Section::Columns {
            return None;
        }
        let engine = DdlEngine::parse(&self.session.target.engine).ok();
        let active: Vec<(usize, &ColumnDraft)> = self
            .session
            .kept_columns
            .iter()
            .enumerate()
            .filter(|(_, col)| !col.dropped)
            .collect();
        let mut seen = HashSet::new();
        for (row_idx, col) in &active {
            let trimmed = col.name.trim();
            if trimmed.is_empty() {
                return Some(i18n::fill(&i18n::text(locale, "structure.diag.emptyCol"), &[("index", &(row_idx + 1).to_string())]));
            }
            let lower = trimmed.to_lowercase();
            if !seen.insert(lower) {
                return Some(i18n::fill(&i18n::text(locale, "structure.diag.duplicateCol"), &[("name", trimmed)]));
            }
            if let Some(eng) = engine {
                // 已有字段如果没动过类型，按原样放行（例如数据库读出的带显示宽度或自定义类型）。
                let untouched_type = col
                    .original
                    .as_deref()
                    .and_then(|orig| self.snapshot.as_ref().and_then(|snap| snap.columns.iter().find(|item| item.name == orig)))
                    .is_some_and(|before| before.type_name.eq_ignore_ascii_case(col.type_name.trim()));
                if !untouched_type && !valid_type(eng, &col.type_name) {
                    return Some(i18n::fill(
                        &i18n::text(locale, "structure.diag.invalidType"),
                        &[("name", trimmed), ("kind", col.type_name.trim())],
                    ));
                }
                let untouched_default = col
                    .original
                    .as_deref()
                    .and_then(|orig| self.snapshot.as_ref().and_then(|snap| snap.columns.iter().find(|item| item.name == orig)))
                    .is_some_and(|before| before.default_sql.as_deref().unwrap_or("") == col.default_sql.as_str());
                if !untouched_default && !col.default_sql.trim().is_empty() && !valid_default(eng, &col.default_sql) {
                    return Some(i18n::fill(&i18n::text(locale, "structure.diag.invalidDefault"), &[("name", trimmed)]));
                }
            }
            if self.session.mode == StructureMode::Existing
                && col.original.is_none()
                && !col.nullable
                && !non_null_default(&col.default_sql)
                && (self.snapshot.as_ref().and_then(|snap| snap.row_count).unwrap_or(0) > 0 || engine == Some(DdlEngine::Sqlite))
            {
                return Some(i18n::fill(&i18n::text(locale, "structure.diag.notNullNeedsDefault"), &[("name", trimmed)]));
            }
        }
        None
    }

    fn changes_breakdown_text(&self, locale: Locale) -> (usize, String) {
        if self.session.mode == StructureMode::Create {
            return (0, String::new());
        }
        let (mut added, mut modified, mut dropped) = (0usize, 0usize, 0usize);
        for column in &self.session.kept_columns {
            match self.column_change(column) {
                ColumnChange::Added => added += 1,
                ColumnChange::Modified => modified += 1,
                ColumnChange::Dropped => dropped += 1,
                ColumnChange::Clean => {}
            }
        }
        let total = added + modified + dropped;
        if total == 0 {
            return (0, String::new());
        }
        let mut parts = Vec::new();
        if added > 0 {
            parts.push(i18n::fill(&i18n::text(locale, "structure.changes.added"), &[("n", &added.to_string())]));
        }
        if modified > 0 {
            parts.push(i18n::fill(&i18n::text(locale, "structure.changes.modified"), &[("n", &modified.to_string())]));
        }
        if dropped > 0 {
            parts.push(i18n::fill(&i18n::text(locale, "structure.changes.dropped"), &[("n", &dropped.to_string())]));
        }
        let sep = match locale {
            Locale::ZhCn => " · ",
            Locale::En => ", ",
        };
        let summary = i18n::fill(
            &i18n::text(locale, "structure.changes.breakdown"),
            &[("count", &total.to_string()), ("detail", &parts.join(sep))],
        );
        (total, summary)
    }

    fn preview_footer(&self, entity: &Entity<StructurePane>, p: &Palette, locale: Locale, metrics: kit::Metrics) -> Option<AnyElement> {
        if !self.preview_action_visible() {
            return None;
        }
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let section = self.session.section;
        let (pending, breakdown) = if section == Section::Columns {
            self.changes_breakdown_text(locale)
        } else {
            (0, String::new())
        };
        let diagnostic = self.draft_diagnostic();
        let create_summary = (section == Section::Columns && self.session.mode == StructureMode::Create).then(|| {
            let active: Vec<&ColumnDraft> = self.session.kept_columns.iter().filter(|col| !col.dropped).collect();
            let pks: Vec<&str> = active.iter().filter(|col| col.primary).map(|col| col.name.trim()).filter(|n| !n.is_empty()).collect();
            if pks.is_empty() {
                (
                    true,
                    i18n::fill(&i18n::text(locale, "structure.create.noPkWarn"), &[("columns", &active.len().to_string())]),
                )
            } else {
                (
                    false,
                    i18n::fill(
                        &i18n::text(locale, "structure.create.summary"),
                        &[("columns", &active.len().to_string()), ("pk", &pks.join(", "))],
                    ),
                )
            }
        });
        let owner = entity.clone();
        let click = move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
            owner.update(cx, |pane, cx| pane.preview_current(cx));
        };
        let tip = SharedString::from(format!("{} · {}", i18n::text(locale, "structure.action.preview"), kit::shortcut("Mod+Enter")));
        let button = if section == Section::Danger {
            kit::danger_button("structure-preview", Some(IconName::Eye), t("structure.action.preview"), click)
                .loading(self.previewing)
                .tooltip(tip)
                .into_any_element()
        } else {
            kit::primary_button("structure-preview", Some(IconName::Eye), t("structure.action.preview"), click)
                .loading(self.previewing)
                .tooltip(tip)
                .into_any_element()
        };
        let status_node: AnyElement = if let Some(diag) = diagnostic {
            div()
                .id("structure-draft-diagnostic")
                .test_support()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .items_center()
                .gap(px(6.))
                .child(kit::icon(IconName::TriangleAlert, 13., p.warn))
                .child(div().min_w(px(0.)).truncate().text_size(px(TypeScale::SM)).text_color(p.warn).font_weight(FontWeight::MEDIUM).child(SharedString::from(diag)))
                .into_any_element()
        } else if pending > 0 {
            div()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .items_center()
                .gap(px(6.))
                .child(kit::icon(IconName::CircleDot, 13., p.warn))
                .child(div().truncate().text_size(px(TypeScale::SM)).font_weight(FontWeight::MEDIUM).child(SharedString::from(breakdown)))
                .into_any_element()
        } else if let Some((no_pk, text)) = create_summary {
            div()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .items_center()
                .gap(px(6.))
                .when(no_pk, |this| this.child(kit::icon(IconName::TriangleAlert, 13., p.warn)))
                .child(
                    div()
                        .min_w(px(0.))
                        .truncate()
                        .text_size(px(TypeScale::SM))
                        .text_color(if no_pk { p.warn } else { p.text_muted })
                        .child(SharedString::from(text)),
                )
                .into_any_element()
        } else {
            div().flex_1().min_w(px(0.)).child(kit::muted(t("structure.action.hint"), p)).into_any_element()
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
                .bg(if pending > 0 { p.warn_soft } else { p.surface })
                .child(status_node)
                .when(pending > 0, |this| {
                    let owner = entity.clone();
                    this.child(kit::tool_button("structure-reset-draft", Some(IconName::Undo2), t("structure.changes.discard"), move |_, _, cx| {
                        owner.update(cx, |pane, cx| {
                            pane.reset_draft();
                            cx.notify();
                        });
                    }))
                })
                .child(button)
                .into_any_element(),
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ColumnChange {
    Clean,
    Added,
    Modified,
    Dropped,
}

fn check_box(id: SharedString, on: bool, enabled: bool, label: SharedString, p: &Palette) -> gpui_kit::Stateful<gpui_kit::Div> {
    kit::interactive_states(div().id(id), p, !enabled)
        .role(gpui_kit::Role::CheckBox)
        .aria_label(label.clone())
        .aria_selected(on)
        .tooltip_text(label)
        .size(px(16.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(Radius::TAG))
        .border_1()
        .when(on, |this| this.bg(p.accent).border_color(p.accent).child(kit::icon(IconName::Check, 12., p.on_accent)))
        .when(!on, |this| this.border_color(p.line_strong).bg(p.surface))
}

fn fk_rules_cell(on_delete: &str, on_update: &str, p: &Palette) -> AnyElement {
    let del = if on_delete.trim().is_empty() { "NO ACTION" } else { on_delete.trim() };
    let upd = if on_update.trim().is_empty() { "NO ACTION" } else { on_update.trim() };
    let badge = |prefix: &str, rule: &str| -> AnyElement {
        let upper = rule.to_ascii_uppercase();
        let (fg, bg) = if upper == "CASCADE" {
            (p.warn, p.warn_soft)
        } else if upper.starts_with("SET ") {
            (p.accent_text, p.accent_soft)
        } else {
            (p.text_muted, p.hover)
        };
        kit::pill(SharedString::from(format!("{prefix}: {upper}")), fg, bg)
    };
    div()
        .flex()
        .items_center()
        .gap(px(4.))
        .min_w(px(0.))
        .overflow_hidden()
        .child(badge("DEL", del))
        .when(!upd.eq_ignore_ascii_case("NO ACTION") && !upd.eq_ignore_ascii_case("RESTRICT"), |this| {
            this.child(badge("UPD", upd))
        })
        .into_any_element()
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
    primary_column: Option<String>,
}

async fn load_fk_choices(
    service: &crate::connections::Service,
    target: &GridTarget,
    schema: &str,
    ref_table: &str,
    want_schemas: bool,
) -> FkChoices {
    let Some(session) = service.catalog_session(&target.connection_id) else {
        return FkChoices { schemas: None, tables: Vec::new(), columns: Vec::new(), primary_column: None };
    };
    let Ok(engine) = crate::connections::model::Engine::parse(&target.engine) else {
        return FkChoices { schemas: None, tables: Vec::new(), columns: Vec::new(), primary_column: None };
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
    let mut primary_column = None;
    if !ref_table.is_empty() {
        if let Ok(snapshot) = crate::application::ddl::describe_saved(
            service,
            &target.connection_id,
            target.database.clone(),
            schema.to_owned(),
            &target.engine,
            ref_table,
        )
        .await
        {
            let pks: Vec<&str> = snapshot.columns.iter().filter(|col| col.primary).map(|col| col.name.as_str()).collect();
            if pks.len() == 1 {
                primary_column = Some(pks[0].to_owned());
            }
            columns = snapshot.columns.into_iter().map(|column| column.name).collect();
        }
    }
    FkChoices { schemas, tables, columns, primary_column }
}

struct RowSpec {
    id: String,
    dim: bool,
    cells: Vec<AnyElement>,
    menu: Vec<MenuEntry>,
}

#[derive(Clone)]
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
    RevertColumn(usize),
    AddColumn,
    DropIndex(String),
    DropForeignKey(String),
    OpenTable { schema: String, table: String },
}

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
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(p.text_faint);
    for column in columns {
        head = head.child(table_cell(*column, div().truncate().child(SharedString::from(i18n::text(locale, column.key)))));
    }
    let quick_of = |entries: &[MenuEntry]| {
        entries
            .iter()
            .find(|entry| matches!(entry.action, RowAction::DropIndex(_) | RowAction::DropForeignKey(_)))
            .cloned()
    };
    let has_quick = rows.iter().any(|row| quick_of(&row.menu).is_some());
    if has_quick {
        head = head.child(div().w(px(28.)).flex_none());
    }
    let mut body = div().flex().flex_col();
    for row in rows {
        let owner = entity.clone();
        let quick = quick_of(&row.menu);
        let entries = row.menu;
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
            .child(cells)
            .when_some(quick, |this, entry| {
                let owner = entity.clone();
                let action = entry.action.clone();
                let button = kit::icon_button(
                    SharedString::from(format!("structure-quick-{}", entry.id)),
                    IconName::Trash,
                    SharedString::from(entry.label.clone()),
                    move |_, _, cx| {
                        owner.update(cx, |pane, cx| {
                            pane.apply_row_action(action.clone(), cx);
                            cx.notify();
                        });
                    },
                )
                .xsmall()
                .disabled(entry.disabled);
                let button = match entry.reason.clone() {
                    Some(reason) if entry.disabled => button.tooltip(SharedString::from(reason)),
                    _ => button,
                };
                this.child(div().w(px(28.)).flex_none().flex().justify_center().child(button))
            });
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

#[derive(Debug, PartialEq, Eq)]
enum ChoiceMatch<'a> {
    Unavailable,
    NoMatch,
    Items(Vec<&'a String>),
}

fn match_choices<'a>(choices: &'a [String], current: &str) -> ChoiceMatch<'a> {
    if choices.is_empty() {
        return ChoiceMatch::Unavailable;
    }
    let current = current.trim();
    let needle = current.to_lowercase();
    if needle.is_empty() || choices.iter().any(|choice| choice == current) {
        return ChoiceMatch::Items(choices.iter().collect());
    }
    let hits: Vec<&String> = choices.iter().filter(|choice| choice.to_lowercase().contains(&needle)).collect();
    if hits.is_empty() { ChoiceMatch::NoMatch } else { ChoiceMatch::Items(hits) }
}

fn choice_menu(
    menu: PopupMenu,
    choices: &[String],
    current: &str,
    locale: Locale,
    pick: impl Fn(String, &mut App) + Clone + 'static,
) -> PopupMenu {
    let mut menu = menu.min_w(px(220.)).max_w(px(420.)).max_h(px(360.)).scrollable(true);
    match match_choices(choices, current) {
        ChoiceMatch::Unavailable => menu.item(PopupMenuItem::label(i18n::text(locale, "structure.choice.empty"))),
        ChoiceMatch::NoMatch => menu
            .item(PopupMenuItem::label(i18n::text(locale, "structure.choice.noMatch")))
            .item(PopupMenuItem::label(i18n::text(locale, "structure.choice.noMatchHint"))),
        ChoiceMatch::Items(filtered) => {
            for choice in filtered {
                let value = choice.clone();
                let pick = pick.clone();
                menu = menu.item(
                    PopupMenuItem::new(choice.clone())
                        .checked(choice == current.trim())
                        .on_click(move |_, _, cx| pick(value.clone(), cx)),
                );
            }
            menu
        }
    }
}

fn row_menu(entity: &Entity<StructurePane>, entries: &[MenuEntry], mut menu: PopupMenu) -> PopupMenu {
    for entry in entries {
        menu = menu.item(menu_entry(entity, entry));
    }
    menu
}

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
    div()
        .truncate()
        .font_family(mono.clone())
        .text_size(px(TypeScale::SM))
        .text_color(p.text_muted)
        .child(SharedString::from(text))
        .into_any_element()
}

fn cell_text(text: String, p: &Palette) -> AnyElement {
    div().truncate().text_color(p.text_muted).child(SharedString::from(text)).into_any_element()
}

impl StructurePane {
    pub fn handle_escape(&mut self) -> bool {
        if self.session.preview_open() && !self.executing {
            self.session.cancel();
            return true;
        }
        if self.composer_open {
            self.composer_open = false;
            return true;
        }
        false
    }

    pub fn trigger_preview_shortcut(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.session.preview_open() && self.preview_action_visible() && !self.previewing {
            self.preview_current(cx);
            return true;
        }
        false
    }

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

    fn draft_input(&mut self, key: u32, value: &str, placeholder: &str, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let state = self.draft_fields.bind(key, value, FieldSpec { placeholder, masked: false }, window, cx, on_draft);
        kit::text_input(SharedString::from(format!("structure-draft-{key}")), &state, placeholder.to_owned()).small().into_any_element()
    }

    fn column_change(&self, column: &ColumnDraft) -> ColumnChange {
        let Some(original) = column.original.as_deref() else {
            return if column.dropped { ColumnChange::Clean } else { ColumnChange::Added };
        };
        if column.dropped {
            return ColumnChange::Dropped;
        }
        let Some(before) = self.snapshot.as_ref().and_then(|snapshot| snapshot.columns.iter().find(|item| item.name == original)) else {
            return ColumnChange::Clean;
        };
        let changed = before.name != column.name
            || !before.type_name.eq_ignore_ascii_case(&column.type_name)
            || before.nullable != column.nullable
            || before.default_sql.clone().unwrap_or_default() != column.default_sql;
        if changed { ColumnChange::Modified } else { ColumnChange::Clean }
    }

    pub fn pending_changes(&self) -> usize {
        if self.session.mode == StructureMode::Create {
            return 0;
        }
        self.session.kept_columns.iter().filter(|column| self.column_change(column) != ColumnChange::Clean).count()
    }

    fn column_matches_filter(&self, column: &ColumnDraft, change: ColumnChange) -> bool {
        if self.only_changed_columns && change == ColumnChange::Clean {
            return false;
        }
        let needle = self.column_filter.trim().to_lowercase();
        if needle.is_empty() {
            return true;
        }
        column.name.to_lowercase().contains(&needle)
            || column.type_name.to_lowercase().contains(&needle)
            || column.default_sql.to_lowercase().contains(&needle)
            || column.original.as_deref().is_some_and(|orig| orig.to_lowercase().contains(&needle))
    }

    /// 实时生成建表、新建索引或新建外键的预览 SQL（纯内存推导，不发起网络请求）。
    fn live_composer_sql(&self) -> Option<String> {
        let engine = DdlEngine::parse(&self.session.target.engine).ok()?;
        match (self.session.mode, self.session.section) {
            (StructureMode::Create, Section::Columns) => {
                if !valid_name(&self.session.table_name) {
                    return None;
                }
                let mut snap = Snapshot::bare(engine, &self.session.target.database, &self.session.target.schema, &self.session.table_name);
                snap.object_exists = false;
                let plan = preview_plan(&snap, &self.session.action());
                plan.steps.first().map(|step| step.sql.clone())
            }
            (StructureMode::Existing, Section::Indexes) => {
                let snap = self.snapshot.as_ref()?;
                let plan = preview_plan(snap, &self.session.action());
                plan.steps.first().map(|step| step.sql.clone())
            }
            (StructureMode::Existing, Section::ForeignKeys) => {
                let snap = self.snapshot.as_ref()?;
                let draft = self.session.foreign_key_draft();
                if !valid_name(&draft.name)
                    || draft.columns.first().is_none_or(|c| c.trim().is_empty())
                    || draft.ref_table.trim().is_empty()
                    || draft.ref_columns.first().is_none_or(|c| c.trim().is_empty())
                {
                    return None;
                }
                let key = crate::ddl::ForeignKey {
                    name: draft.name,
                    columns: draft.columns,
                    ref_schema: draft.ref_schema,
                    ref_table: draft.ref_table,
                    ref_columns: draft.ref_columns,
                    on_delete: draft.on_delete,
                    on_update: draft.on_update,
                };
                fk_sql_for_display(snap, &key)
            }
            _ => None,
        }
    }

    #[inline(never)]
    fn column_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let locale = self.session.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let entity = cx.entity();
        let metrics = kit::Metrics::of(cx);
        let create = self.session.mode == StructureMode::Create;
        let readonly_view = self.session.object_kind == "view" && !create;
        if readonly_view {
            return div().into_any_element();
        }
        let engine = self.session.target.engine.clone();
        let row_h = metrics.row_h;
        let content_w = (self.content_width - 2. * metrics.page_pad).max(0.);
        let widths = FieldWidths::of(content_w, create);
        let mut editor = div().flex().flex_col().gap(px(metrics.section_gap()));

        if create {
            let name_placeholder = i18n::text(locale, "structure.create.tableNamePlaceholder");
            let name = self.draft_input(4, &self.session.table_name.clone(), &name_placeholder, window, cx);
            let preset_entity = entity.clone();
            let add_top_entity = entity.clone();
            let presets_btn = Button::new("structure-column-presets")
                .ghost()
                .small()
                .icon(IconName::Sparkles)
                .label(t("structure.field.presets"))
                .dropdown_menu(move |menu, _, _| {
                    let items = [
                        ("structure.preset.timestamps", ColumnPreset::Timestamps),
                        ("structure.preset.softDelete", ColumnPreset::SoftDelete),
                        ("structure.preset.statusRemark", ColumnPreset::StatusRemark),
                        ("structure.preset.uuid", ColumnPreset::Uuid),
                    ];
                    let mut m = menu.min_w(px(220.));
                    for (key, preset) in items {
                        let owner = preset_entity.clone();
                        m = m.item(PopupMenuItem::new(i18n::text(locale, key)).on_click(move |_, _, cx| {
                            owner.update(cx, |pane, cx| {
                                pane.apply_column_preset(preset);
                                cx.notify();
                            });
                        }));
                    }
                    m
                });
            editor = editor.child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_end()
                    .gap(px(12.))
                    .child(div().flex_1().min_w(px(200.)).max_w(px(380.)).child(kit::form_row(t("structure.action.createTable"), name, &p)))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(presets_btn)
                            .child(kit::secondary_button("structure-add-column-top", Some(IconName::Plus), t("structure.field.addColumn"), move |_, _, cx| {
                                add_top_entity.update(cx, |pane, cx| {
                                    pane.add_column();
                                    cx.notify();
                                });
                            })),
                    ),
            );
        } else {
            let total_cols = self.session.kept_columns.len();
            let pending = self.pending_changes();
            let filter_val = self.column_filter.clone();
            let filter_placeholder = i18n::text(locale, "structure.columns.filter");
            let filter_state = self.fields.bind(
                "column_filter",
                &filter_val,
                FieldSpec { placeholder: &filter_placeholder, masked: false },
                window,
                cx,
                |this: &mut Self, field, value, _| this.set_field(field, value),
            );
            let show_toolbar = total_cols >= 5 || !filter_val.is_empty() || self.only_changed_columns || pending > 0;
            if show_toolbar {
                let clear_entity = entity.clone();
                let changed_entity = entity.clone();
                let add_top_entity = entity.clone();
                let preset_entity = entity.clone();
                let visible_count = self
                    .session
                    .kept_columns
                    .iter()
                    .filter(|col| self.column_matches_filter(col, self.column_change(col)))
                    .count();
                let count_label = if !filter_val.trim().is_empty() || self.only_changed_columns {
                    i18n::fill(
                        &i18n::text(locale, "structure.columns.filteredCount"),
                        &[("shown", &visible_count.to_string()), ("total", &total_cols.to_string())],
                    )
                } else {
                    i18n::fill(&i18n::text(locale, "structure.columns.count"), &[("count", &total_cols.to_string())])
                };
                let presets_btn = Button::new("structure-column-presets")
                    .ghost()
                    .small()
                    .icon(IconName::Sparkles)
                    .label(t("structure.field.presets"))
                    .dropdown_menu(move |menu, _, _| {
                        let items = [
                            ("structure.preset.timestamps", ColumnPreset::Timestamps),
                            ("structure.preset.softDelete", ColumnPreset::SoftDelete),
                            ("structure.preset.statusRemark", ColumnPreset::StatusRemark),
                        ];
                        let mut m = menu.min_w(px(220.));
                        for (key, preset) in items {
                            let owner = preset_entity.clone();
                            m = m.item(PopupMenuItem::new(i18n::text(locale, key)).on_click(move |_, _, cx| {
                                owner.update(cx, |pane, cx| {
                                    pane.apply_column_preset(preset);
                                    cx.notify();
                                });
                            }));
                        }
                        m
                    });
                editor = editor.child(
                    div()
                        .id("structure-columns-toolbar")
                        .test_support()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(8.))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(160.))
                                .max_w(px(280.))
                                .h(px(metrics.control_h))
                                .px(px(8.))
                                .rounded(px(Radius::CONTROL))
                                .border_1()
                                .border_color(if !filter_val.is_empty() { p.accent } else { p.line })
                                .bg(p.surface)
                                .flex()
                                .items_center()
                                .child(kit::input_prefix(IconName::Search, 13., p.text_faint))
                                .child(div().flex_1().min_w(px(0.)).child(kit::borderless_input("structure-column-filter", &filter_state, filter_placeholder)))
                                .when(!filter_val.is_empty(), |this| {
                                    this.child(
                                        kit::icon_button("structure-column-filter-clear", IconName::X, t("structure.columns.filterClear"), move |_, _, cx| {
                                            clear_entity.update(cx, |pane, cx| {
                                                pane.column_filter.clear();
                                                cx.notify();
                                            });
                                        })
                                        .xsmall(),
                                    )
                                }),
                        )
                        .when(pending > 0 || self.only_changed_columns, |this| {
                            let label = SharedString::from(i18n::fill(
                                &i18n::text(locale, "structure.columns.onlyChanged"),
                                &[("count", &pending.to_string())],
                            ));
                            this.child(
                                kit::tool_button("structure-only-changed", Some(IconName::CircleDot), label, move |_, _, cx| {
                                    changed_entity.update(cx, |pane, cx| {
                                        pane.only_changed_columns = !pane.only_changed_columns;
                                        cx.notify();
                                    });
                                })
                                .selected(self.only_changed_columns),
                            )
                        })
                        .child(div().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(SharedString::from(count_label)))
                        .child(div().flex_1())
                        .when(content_w >= 560., |this| this.child(presets_btn))
                        .child(kit::secondary_button("structure-add-column-top", Some(IconName::Plus), t("structure.field.addColumn"), move |_, _, cx| {
                            add_top_entity.update(cx, |pane, cx| {
                                pane.add_column();
                                cx.notify();
                            });
                        })),
                );
            }
        }

        let head = div()
            .id("structure-table-head")
            .test_support()
            .flex()
            .flex_none()
            .h(px(row_h))
            .items_center()
            .gap(px(FieldWidths::GAP))
            .pl(px(FieldWidths::PAD_LEFT))
            .pr(px(FieldWidths::PAD_RIGHT))
            .bg(p.chrome)
            .border_b_1()
            .border_color(p.line)
            .text_size(px(TypeScale::XS))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(p.text_faint)
            .child(div().flex_1().min_w(px(widths.name_min)).pl(px(16.)).child(t("structure.field.column")))
            .child(div().w(px(widths.kind)).flex_none().child(t("structure.field.type")))
            .child(div().w(px(widths.nullable)).flex_none().flex().justify_center().child(t("ui.structure.nullable")))
            .child(div().w(px(widths.default)).flex_none().child(t("ui.structure.default")))
            .when(create, |this| {
                this.child(div().w(px(widths.primary)).flex_none().flex().justify_center().child(t("structure.field.primary")))
                    .child(div().w(px(widths.auto)).flex_none().flex().justify_center().child(t("structure.field.auto")))
                    .child(div().w(px(widths.moves)).flex_none())
            })
            .child(div().w(px(widths.drop)).flex_none());

        let columns = self.session.kept_columns.clone();
        let visible_indices: Vec<(usize, ColumnDraft, ColumnChange)> = columns
            .into_iter()
            .enumerate()
            .filter_map(|(index, column)| {
                let change = self.column_change(&column);
                self.column_matches_filter(&column, change).then_some((index, column, change))
            })
            .collect();

        if visible_indices.is_empty() && (!self.column_filter.trim().is_empty() || self.only_changed_columns) {
            let clear_owner = entity.clone();
            let clear_btn = kit::secondary_button("structure-clear-col-filter", None, t("structure.columns.filterClear"), move |_, _, cx| {
                clear_owner.update(cx, |pane, cx| {
                    pane.clear_column_filter();
                    cx.notify();
                });
            });
            editor = editor.child(
                kit::card(&p).overflow_hidden().child(kit::no_match_state_with_action(
                    IconName::Search,
                    t("structure.columns.noMatch"),
                    t("structure.columns.noMatchHint"),
                    clear_btn,
                    &p,
                )),
            );
            return editor.into_any_element();
        }

        let mut rows = div().flex().flex_col();
        for (index, column, change) in &visible_indices {
            rows = rows.child(stack_frame(|| {
                self.column_row(*index, column, *change, create, &engine, row_h, widths, &p, &entity, window, cx)
            }));
        }
        let add_entity = entity.clone();
        let add = div()
            .id("structure-add-column-row")
            .flex()
            .items_center()
            .h(px(row_h + 4.))
            .px(px(6.))
            .bg(p.surface)
            .child(kit::tool_button("structure-add-column", Some(IconName::Plus), t("structure.field.addColumn"), move |_, _, cx| {
                add_entity.update(cx, |pane, cx| {
                    pane.add_column();
                    cx.notify();
                });
            }));
        editor = editor.child(kit::card(&p).overflow_hidden().flex().flex_col().child(head).child(rows).child(add));

        if create {
            let live_sql = self.live_composer_sql();
            let mono = cx.theme().mono_font_family.clone();
            editor = editor.child(
                div()
                    .id("structure-create-live-sql")
                    .test_support()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(kit::eyebrow(t("structure.create.sqlPreview"), &p))
                    .child(
                        kit::sunken(&p)
                            .id("structure-create-live-sql-scroll")
                            .p(px(12.))
                            .max_h(px(180.))
                            .overflow_y_scroll()
                            .font_family(mono)
                            .text_size(px(TypeScale::SM))
                            .text_color(if live_sql.is_some() { p.text } else { p.text_faint })
                            .child(SharedString::from(live_sql.unwrap_or_else(|| i18n::text(locale, "structure.create.nameHint")))),
                    ),
            );
        }

        editor.into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    #[inline(never)]
    fn column_row(
        &mut self,
        index: usize,
        column: &ColumnDraft,
        change: ColumnChange,
        create: bool,
        engine: &str,
        row_h: f32,
        widths: FieldWidths,
        p: &Palette,
        entity: &Entity<StructurePane>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let locale = self.session.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));

        let snapshot_col = column
            .original
            .as_deref()
            .and_then(|orig| self.snapshot.as_ref().and_then(|snap| snap.columns.iter().find(|c| c.name == orig)));
        let orig_primary = !create && snapshot_col.is_some_and(|c| c.primary);
        let orig_generated = !create && snapshot_col.is_some_and(|c| c.generated);
        let orig_auto = !create && snapshot_col.is_some_and(|c| c.auto);
        let col_comment = column
            .original
            .as_deref()
            .and_then(|orig| self.snapshot.as_ref().and_then(|snap| snap.column_comments.get(orig)))
            .cloned();
        let fk_target = column.original.as_deref().and_then(|orig| {
            self.snapshot.as_ref().and_then(|snap| {
                snap.foreign_keys
                    .iter()
                    .find(|fk| fk.columns.iter().any(|c| c == orig))
                    .map(|fk| format!("{}.{}", fk.ref_table, fk.ref_columns.join(",")))
            })
        });
        let changed_detail = (change == ColumnChange::Modified).then(|| {
            snapshot_col
                .map(|before| {
                    let null_str = if before.nullable { "NULL" } else { "NOT NULL" };
                    let def_str = before.default_sql.as_deref().map(|d| format!(" DEFAULT {d}")).unwrap_or_default();
                    i18n::fill(
                        &i18n::text(locale, "structure.field.changedFrom"),
                        &[("detail", &format!("{} {} {null_str}{def_str}", before.name, before.type_name))],
                    )
                })
                .unwrap_or_default()
        });

        let name = self.draft_input(10_000 + index as u32, &column.name, "name", window, cx);
        let kind = self.draft_input(20_000 + index as u32, &column.type_name, default_column_type(engine), window, cx);
        let default_value = self.draft_input(30_000 + index as u32, &column.default_sql, "NULL", window, cx);

        let types = type_choices(engine).iter().map(|item| (*item).to_owned()).collect::<Vec<_>>();
        let current_type = types
            .iter()
            .find(|item| item.eq_ignore_ascii_case(column.type_name.trim()))
            .cloned()
            .unwrap_or_default();
        let type_entity = entity.clone();
        let type_locked = orig_primary || orig_generated;
        let type_menu = Button::new(SharedString::from(format!("structure-types-{index}")))
            .ghost()
            .xsmall()
            .icon(IconName::ChevronDown)
            .disabled(type_locked)
            .tooltip(t("structure.field.pickType"))
            .dropdown_menu(move |menu, _, _| {
                let entity = type_entity.clone();
                choice_menu(menu, &types, &current_type, locale, move |choice, cx| {
                    entity.update(cx, |pane, cx| {
                        pane.write_draft(20_000 + index as u32, choice);
                        cx.notify();
                    });
                })
            });

        let defaults = default_choices(engine, column.nullable);
        let default_entity = entity.clone();
        let cur_default = column.default_sql.trim().to_owned();
        let clear_default_label = i18n::text(locale, "structure.field.defaultNone");
        let default_menu = Button::new(SharedString::from(format!("structure-defaults-{index}")))
            .ghost()
            .xsmall()
            .icon(IconName::ChevronDown)
            .disabled(orig_generated || (create && column.auto))
            .tooltip(t("structure.field.pickDefault"))
            .dropdown_menu(move |mut menu, _, _| {
                let owner_clear = default_entity.clone();
                menu = menu.min_w(px(180.)).item(
                    PopupMenuItem::new(clear_default_label.clone())
                        .checked(cur_default.is_empty())
                        .on_click(move |_, _, cx| {
                            owner_clear.update(cx, |pane, cx| {
                                pane.write_draft(30_000 + index as u32, String::new());
                                cx.notify();
                            });
                        }),
                );
                for preset in &defaults {
                    let value = (*preset).to_owned();
                    let owner = default_entity.clone();
                    menu = menu.item(
                        PopupMenuItem::new(value.clone())
                            .checked(cur_default.eq_ignore_ascii_case(&value))
                            .on_click(move |_, _, cx| {
                                owner.update(cx, |pane, cx| {
                                    pane.write_draft(30_000 + index as u32, value.clone());
                                    cx.notify();
                                });
                            }),
                    );
                }
                menu
            });

        let toggle = |id: String, on: bool, which: &'static str, label: SharedString, enabled: bool| {
            let owner = entity.clone();
            check_box(SharedString::from(id), on, enabled, label, p).on_click(move |_, _, cx| {
                owner.update(cx, |pane, cx| {
                    pane.toggle_column(index, which);
                    cx.notify();
                });
            })
        };
        let (bar, row_tint) = match change {
            ColumnChange::Added => (Some(p.ok), Some(p.ok_soft.opacity(0.45))),
            ColumnChange::Modified => (Some(p.warn), Some(p.warn_soft.opacity(0.45))),
            ColumnChange::Dropped => (Some(p.danger), Some(p.danger_soft.opacity(0.35))),
            ColumnChange::Clean => (None, None),
        };
        let dropped = change == ColumnChange::Dropped;
        let is_unsaved_new = column.original.is_none();
        let drop_locked = orig_generated || (create && is_unsaved_new && self.session.kept_columns.len() <= 1);
        let drop_label = if dropped {
            t("structure.field.undoDelete")
        } else if is_unsaved_new && !create {
            t("structure.field.removeDraft")
        } else {
            t("structure.field.delete")
        };
        let drop_tip = if orig_primary {
            Some(t("structure.field.pkLocked"))
        } else if orig_generated {
            Some(t("structure.field.genLocked"))
        } else {
            None
        };
        let drop_entity = entity.clone();
        let drop_btn = kit::icon_button(
            SharedString::from(format!("structure-drop-col-{index}")),
            if dropped {
                IconName::Undo2
            } else if is_unsaved_new && !create {
                IconName::X
            } else {
                IconName::Trash
            },
            drop_label.clone(),
            move |_, _, cx| {
                drop_entity.update(cx, |pane, cx| {
                    pane.toggle_column(index, "drop");
                    cx.notify();
                });
            },
        )
        .xsmall()
        .disabled(drop_locked);
        let drop_btn = match drop_tip {
            Some(reason) => drop_btn.tooltip(reason),
            None => drop_btn,
        };

        let revert_btn = (!create && change == ColumnChange::Modified).then(|| {
            let revert_entity = entity.clone();
            let tip = changed_detail
                .clone()
                .filter(|d| !d.is_empty())
                .map(|d| SharedString::from(format!("{} · {d}", i18n::text(locale, "structure.field.revert"))))
                .unwrap_or_else(|| t("structure.field.revert"));
            kit::icon_button(
                SharedString::from(format!("structure-revert-col-{index}")),
                IconName::Undo2,
                t("structure.field.revert"),
                move |_, _, cx| {
                    revert_entity.update(cx, |pane, cx| {
                        pane.revert_column(index);
                        cx.notify();
                    });
                },
            )
            .xsmall()
            .tooltip(tip)
        });

        let mut row_menu_entries = vec![MenuEntry {
            id: format!("edit-copy-col-{index}"),
            label: i18n::text(locale, "structure.menu.copyName"),
            action: RowAction::Copy(column.name.clone()),
            disabled: column.name.trim().is_empty(),
            reason: None,
        }];
        if !create && change == ColumnChange::Modified {
            row_menu_entries.push(MenuEntry {
                id: format!("edit-revert-col-{index}"),
                label: i18n::text(locale, "structure.field.revert"),
                action: RowAction::RevertColumn(index),
                disabled: false,
                reason: None,
            });
        }
        row_menu_entries.push(MenuEntry {
            id: format!("edit-add-col-{index}"),
            label: i18n::text(locale, "structure.field.addColumn"),
            action: RowAction::AddColumn,
            disabled: false,
            reason: None,
        });
        row_menu_entries.push(MenuEntry {
            id: format!("edit-drop-col-{index}"),
            label: drop_label.to_string(),
            action: RowAction::ToggleColumnDrop(index),
            disabled: drop_locked,
            reason: if orig_primary {
                Some(i18n::text(locale, "structure.field.pkLocked"))
            } else if orig_generated {
                Some(i18n::text(locale, "structure.field.genLocked"))
            } else {
                None
            },
        });

        let slot_icon: Option<(IconName, gpui_kit::Hsla, String)> = if column.primary {
            let mut tip = i18n::text(locale, "structure.field.primary");
            if orig_auto || (create && column.auto) {
                tip = format!("{tip} · {}", i18n::text(locale, "structure.field.auto"));
            }
            if let Some(comment) = &col_comment {
                tip = format!("{tip} — {comment}");
            }
            Some((IconName::Key, p.warn, tip))
        } else if let Some(target) = &fk_target {
            let mut tip = i18n::fill(&i18n::text(locale, "structure.field.badgeFk"), &[("target", target)]);
            if let Some(comment) = &col_comment {
                tip = format!("{tip} — {comment}");
            }
            Some((IconName::Key, p.info, tip))
        } else if orig_generated {
            Some((IconName::Lock, p.text_faint, i18n::text(locale, "structure.field.genLocked")))
        } else {
            col_comment
                .as_ref()
                .map(|comment| (IconName::Info, p.text_faint, format!("{}: {comment}", i18n::text(locale, "structure.field.comment"))))
        };

        let show_inline_badge = !create && widths.name_min >= 130. && (orig_auto || orig_generated);
        let menu_owner = entity.clone();

        let mut row = div()
            .id(SharedString::from(format!("structure-row-columns-{index}")))
            .test_support()
            .flex()
            .items_center()
            .gap(px(FieldWidths::GAP))
            .relative()
            .h(px(row_h))
            .pl(px(FieldWidths::PAD_LEFT))
            .pr(px(FieldWidths::PAD_RIGHT))
            .border_b_1()
            .border_color(p.line_soft)
            .when_some(row_tint, |this, tint| this.bg(tint))
            .hover(|style| style.bg(p.hover))
            .context_menu(move |menu, _, _| row_menu(&menu_owner, &row_menu_entries, menu))
            .when_some(bar, |this, color| {
                let indicator = div().absolute().left_0().top_0().bottom_0().w(px(3.)).bg(color);
                this.child(indicator)
            })
            .child(
                div()
                    .flex_1()
                    .min_w(px(widths.name_min))
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .child(
                        div()
                            .w(px(12.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .when_some(slot_icon, |this, (ic, color, tip)| {
                                this.child(
                                    div()
                                        .id(SharedString::from(format!("structure-col-mark-{index}")))
                                        .tooltip_text(SharedString::from(tip))
                                        .child(kit::icon(ic, 12., color)),
                                )
                            }),
                    )
                    .child(div().flex_1().min_w(px(0.)).when(dropped, |this| this.opacity(0.5).line_through()).child(name))
                    .when(show_inline_badge, |this| {
                        let key = if orig_generated { "structure.field.badgeGen" } else { "structure.field.badgeAuto" };
                        this.child(kit::pill(t(key), p.text_muted, p.hover))
                    }),
            )
            .child(
                div()
                    .w(px(widths.kind))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(2.))
                    .when(dropped, |this| this.opacity(0.5))
                    .child(div().flex_1().min_w(px(0.)).child(kind))
                    .child(type_menu),
            )
            .child(
                div()
                    .w(px(widths.nullable))
                    .flex_none()
                    .flex()
                    .justify_center()
                    .child(toggle(
                        format!("structure-null-{index}"),
                        column.nullable,
                        "null",
                        t("ui.structure.nullable"),
                        !dropped && !column.primary && !orig_generated,
                    )),
            )
            .child(
                div()
                    .w(px(widths.default))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(2.))
                    .when(dropped, |this| this.opacity(0.5))
                    .child(div().flex_1().min_w(px(0.)).child(default_value))
                    .child(default_menu),
            );

        if create {
            let up_entity = entity.clone();
            let down_entity = entity.clone();
            let is_first = index == 0;
            let is_last = index + 1 >= self.session.kept_columns.len();
            row = row
                .child(
                    div()
                        .w(px(widths.primary))
                        .flex_none()
                        .flex()
                        .justify_center()
                        .child(toggle(format!("structure-pk-{index}"), column.primary, "primary", t("structure.field.primary"), true)),
                )
                .child(
                    div()
                        .w(px(widths.auto))
                        .flex_none()
                        .flex()
                        .justify_center()
                        .child(toggle(format!("structure-auto-{index}"), column.auto, "auto", t("structure.field.auto"), true)),
                )
                .child(
                    div()
                        .w(px(widths.moves))
                        .flex_none()
                        .flex()
                        .justify_center()
                        .child(
                            kit::icon_button(
                                SharedString::from(format!("structure-up-{index}")),
                                IconName::ChevronUp,
                                t("structure.field.moveUp"),
                                move |_, _, cx| {
                                    up_entity.update(cx, |pane, cx| {
                                        pane.move_column(index, -1);
                                        cx.notify();
                                    });
                                },
                            )
                            .xsmall()
                            .disabled(is_first),
                        )
                        .child(
                            kit::icon_button(
                                SharedString::from(format!("structure-down-{index}")),
                                IconName::ChevronDown,
                                t("structure.field.moveDown"),
                                move |_, _, cx| {
                                    down_entity.update(cx, |pane, cx| {
                                        pane.move_column(index, 1);
                                        cx.notify();
                                    });
                                },
                            )
                            .xsmall()
                            .disabled(is_last),
                        ),
                );
        }
        row.child(
            div()
                .w(px(widths.drop))
                .flex_none()
                .flex()
                .items_center()
                .justify_end()
                .gap(px(2.))
                .when_some(revert_btn, |this, btn| this.child(btn))
                .child(drop_btn),
        )
        .into_any_element()
    }

    fn section_form(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let locale = self.session.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let section = self.session.section;
        let entity = cx.entity();
        let metrics = kit::Metrics::of(cx);
        let form = div().flex().flex_col().gap(px(metrics.section_gap()));
        let readonly_view = self.session.object_kind == "view" && self.session.mode != StructureMode::Create;
        let form = match section {
            Section::Columns if readonly_view => {
                let edit_entity = entity.clone();
                form.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(div().flex_1().min_w(px(0.)).child(kit::muted(t("structure.view.readonlyColumns"), &p)))
                        .child(kit::secondary_button("structure-switch-view-def", Some(IconName::SquarePen), t("structure.view.editDef"), move |_, _, cx| {
                            edit_entity.update(cx, |pane, cx| {
                                pane.session.section = Section::View;
                                pane.session.cancel();
                                cx.notify();
                            });
                        })),
                )
            }
            Section::Columns => form.child(self.column_editor(window, cx)),
            Section::Indexes => stack_frame(|| {
                let mut form = form;
                let names = self
                    .snapshot
                    .as_ref()
                    .map(|snapshot| snapshot.columns.iter().map(|column| column.name.clone()).collect::<Vec<_>>())
                    .unwrap_or_else(|| {
                        self.session.kept_columns.iter().filter(|column| !column.dropped).map(|column| column.name.clone()).collect()
                    });
                let selected = self.session.index_columns.clone();
                let unique = self.session.index_unique;
                let name = self.field("index_name", "structure.field.index", "idx_table_column", window, cx);
                let unique_entity = entity.clone();
                let auto_entity = entity.clone();
                form = form.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_end()
                        .gap(px(12.))
                        .child(div().flex_1().min_w(px(200.)).max_w(px(360.)).child(name))
                        .when(!self.index_name_auto && !selected.is_empty(), |this| {
                            this.child(kit::tool_button("structure-index-autoname", Some(IconName::Sparkles), t("structure.index.autoName"), move |_, _, cx| {
                                auto_entity.update(cx, |pane, cx| {
                                    pane.apply_auto_index_name();
                                    cx.notify();
                                });
                            }))
                        })
                        .child(
                            div()
                                .id("structure-index-unique")
                                .h(px(metrics.control_h))
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .cursor_pointer()
                                .child(check_box(SharedString::from("structure-index-unique-box"), unique, true, t("structure.index.unique"), &p))
                                .child(div().text_size(px(TypeScale::SM)).child(t("structure.index.unique")))
                                .on_click(move |_, _, cx| {
                                    unique_entity.update(cx, |pane, cx| {
                                        pane.session.index_unique = !pane.session.index_unique;
                                        pane.sync_auto_index_name();
                                        pane.session.intent = EditIntent::Apply;
                                        pane.session.cancel();
                                        cx.notify();
                                    });
                                }),
                        ),
                );
                let mut chips = div().flex().flex_wrap().items_center().gap(px(6.));
                if selected.is_empty() {
                    chips = chips.child(div().text_size(px(TypeScale::SM)).text_color(p.text_faint).child(t("structure.index.pickHint")));
                }
                let selected_len = selected.len();
                for (chip_idx, column) in selected.iter().enumerate() {
                    let order_entity = entity.clone();
                    let remove_entity = entity.clone();
                    let left_entity = entity.clone();
                    let right_entity = entity.clone();
                    let order_name = column.name.clone();
                    let remove_name = column.name.clone();
                    chips = chips.child(
                        div()
                            .id(SharedString::from(format!("structure-index-chip-{}", column.name)))
                            .flex()
                            .items_center()
                            .gap(px(4.))
                            .h(px(26.))
                            .pl(px(8.))
                            .pr(px(3.))
                            .rounded(px(Radius::CONTROL))
                            .bg(p.accent_soft)
                            .text_color(p.accent_text)
                            .text_size(px(TypeScale::SM))
                            .font_family(cx.theme().mono_font_family.clone())
                            .when(selected_len > 1, |this| {
                                this.child(
                                    div()
                                        .text_size(px(TypeScale::XS))
                                        .text_color(p.text_faint)
                                        .child(SharedString::from(format!("{}.", chip_idx + 1))),
                                )
                            })
                            .child(SharedString::from(column.name.clone()))
                            .child(
                                div()
                                    .id(SharedString::from(format!("structure-index-order-{}", column.name)))
                                    .px(px(4.))
                                    .rounded(px(Radius::TAG))
                                    .cursor_pointer()
                                    .hover(|style| style.bg(p.surface))
                                    .tooltip_text(t("structure.index.toggleOrder"))
                                    .child(if column.descending { t("structure.index.desc") } else { t("structure.index.asc") })
                                    .on_click(move |_, _, cx| {
                                        let name = order_name.clone();
                                        order_entity.update(cx, |pane, cx| {
                                            pane.toggle_index_order(&name);
                                            cx.notify();
                                        });
                                    }),
                            )
                            .when(selected_len > 1 && chip_idx > 0, |this| {
                                this.child(
                                    kit::icon_button(
                                        SharedString::from(format!("structure-index-left-{}", column.name)),
                                        IconName::ChevronLeft,
                                        t("structure.index.moveLeft"),
                                        move |_, _, cx| {
                                            left_entity.update(cx, |pane, cx| {
                                                pane.move_index_column(chip_idx, -1);
                                                cx.notify();
                                            });
                                        },
                                    )
                                    .xsmall(),
                                )
                            })
                            .when(selected_len > 1 && chip_idx + 1 < selected_len, |this| {
                                this.child(
                                    kit::icon_button(
                                        SharedString::from(format!("structure-index-right-{}", column.name)),
                                        IconName::ChevronRight,
                                        t("structure.index.moveRight"),
                                        move |_, _, cx| {
                                            right_entity.update(cx, |pane, cx| {
                                                pane.move_index_column(chip_idx, 1);
                                                cx.notify();
                                            });
                                        },
                                    )
                                    .xsmall(),
                                )
                            })
                            .child(
                                kit::icon_button(
                                    SharedString::from(format!("structure-index-remove-{}", column.name)),
                                    IconName::X,
                                    t("structure.index.removeColumn"),
                                    move |_, _, cx| {
                                        let name = remove_name.clone();
                                        remove_entity.update(cx, |pane, cx| {
                                            pane.toggle_index_column(&name);
                                            cx.notify();
                                        });
                                    },
                                )
                                .xsmall(),
                            ),
                    );
                }
                let remaining: Vec<String> = names.into_iter().filter(|name| !selected.iter().any(|column| &column.name == name)).collect();
                let add_entity = entity.clone();
                chips = chips.child(
                    Button::new("structure-index-add-column")
                        .ghost()
                        .small()
                        .icon(IconName::Plus)
                        .label(t("structure.index.addColumn"))
                        .disabled(remaining.is_empty())
                        .dropdown_menu(move |menu, _, _| {
                            let entity = add_entity.clone();
                            choice_menu(menu, &remaining, "", locale, move |name, cx| {
                                entity.update(cx, |pane, cx| {
                                    pane.toggle_index_column(&name);
                                    cx.notify();
                                });
                            })
                        }),
                );
                form = form.child(kit::form_row(t("structure.section.columns"), chips, &p));
                if let Some(sql) = self.live_composer_sql() {
                    form = form.child(
                        kit::sunken(&p)
                            .px(px(10.))
                            .py(px(8.))
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_size(px(TypeScale::SM))
                            .text_color(p.text_muted)
                            .child(SharedString::from(sql)),
                    );
                }
                form
            }),
            Section::ForeignKeys => stack_frame(|| {
                let mut form = form;
                self.ensure_references(cx);
                let name = self.field("fk_name", "structure.section.foreign", "fk_table_column", window, cx);
                let column = self.field("fk_column", "structure.field.column", "parent_id", window, cx);
                let table = self.field("fk_ref_table", "ui.structure.refTable", "parents", window, cx);
                let target = self.field("fk_ref_column", "ui.structure.refColumn", "id", window, cx);
                let tables = self.fk_tables.clone();
                let columns = self.fk_columns.clone();
                let current_fk_column = self.session.fk_column.clone();
                let current_fk_ref_table = self.session.fk_ref_table.clone();
                let current_fk_ref_column = self.session.fk_ref_column.clone();
                let local: Vec<String> = self
                    .snapshot
                    .as_ref()
                    .map(|snapshot| snapshot.columns.iter().map(|column| column.name.clone()).collect())
                    .unwrap_or_default();
                let table_entity = entity.clone();
                let column_entity = entity.clone();
                let ref_entity = entity.clone();
                let local_button = stack_frame(|| {
                    Button::new("structure-fk-local")
                        .ghost()
                        .small()
                        .icon(IconName::ChevronDown)
                        .tooltip(t("structure.field.column"))
                        .accessibility_label(t("structure.field.column"))
                        .dropdown_menu(move |menu, _, _| {
                            let column_entity = column_entity.clone();
                            choice_menu(menu, &local, &current_fk_column, locale, move |choice, cx| {
                                column_entity.update(cx, |pane, cx| {
                                    pane.choose_fk("fk_column", choice);
                                    cx.notify();
                                });
                            })
                        })
                        .into_any_element()
                });
                let tables_button = stack_frame(|| {
                    Button::new("structure-fk-tables")
                        .ghost()
                        .small()
                        .icon(IconName::ChevronDown)
                        .tooltip(t("ui.structure.refTable"))
                        .accessibility_label(t("ui.structure.refTable"))
                        .dropdown_menu(move |menu, _, _| {
                            let table_entity = table_entity.clone();
                            choice_menu(menu, &tables, &current_fk_ref_table, locale, move |choice, cx| {
                                table_entity.update(cx, |pane, cx| {
                                    pane.choose_fk("fk_ref_table", choice);
                                    cx.notify();
                                });
                            })
                        })
                        .into_any_element()
                });
                let columns_button = stack_frame(|| {
                    Button::new("structure-fk-ref-cols")
                        .ghost()
                        .small()
                        .icon(IconName::ChevronDown)
                        .tooltip(t("ui.structure.refColumn"))
                        .accessibility_label(t("ui.structure.refColumn"))
                        .dropdown_menu(move |menu, _, _| {
                            let ref_entity = ref_entity.clone();
                            choice_menu(menu, &columns, &current_fk_ref_column, locale, move |choice, cx| {
                                ref_entity.update(cx, |pane, cx| {
                                    pane.choose_fk("fk_ref_column", choice);
                                    cx.notify();
                                });
                            })
                        })
                        .into_any_element()
                });
                form = form.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_end()
                        .gap(px(8.))
                        .child(div().flex_1().min_w(px(160.)).child(name))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(160.))
                                .flex()
                                .items_end()
                                .gap(px(4.))
                                .child(div().flex_1().min_w(px(0.)).child(column))
                                .child(local_button),
                        ),
                );
                if shows_ref_schema(&self.session.target.engine) {
                    let schema = self.field("fk_ref_schema", "structure.fk.refSchema", "public", window, cx);
                    let schemas = self.fk_schemas.clone();
                    let current_fk_ref_schema = self.session.fk_ref_schema.clone();
                    let schema_entity = entity.clone();
                    form = form.child(
                        div()
                            .flex()
                            .items_end()
                            .gap(px(4.))
                            .child(div().flex_1().min_w(px(0.)).child(schema))
                            .child(
                                Button::new("structure-fk-schemas")
                                    .ghost()
                                    .small()
                                    .icon(IconName::ChevronDown)
                                    .tooltip(t("structure.fk.refSchema"))
                                    .accessibility_label(t("structure.fk.refSchema"))
                                    .dropdown_menu(move |menu, _, _| {
                                        let schema_entity = schema_entity.clone();
                                        choice_menu(menu, &schemas, &current_fk_ref_schema, locale, move |choice, cx| {
                                            schema_entity.update(cx, |pane, cx| {
                                                pane.choose_fk("fk_ref_schema", choice);
                                                cx.notify();
                                            });
                                        })
                                    }),
                            ),
                    );
                }
                form = form.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_end()
                        .gap(px(8.))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(160.))
                                .flex()
                                .items_end()
                                .gap(px(4.))
                                .child(div().flex_1().min_w(px(0.)).child(table))
                                .child(tables_button),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(160.))
                                .flex()
                                .items_end()
                                .gap(px(4.))
                                .child(div().flex_1().min_w(px(0.)).child(target))
                                .child(columns_button),
                        ),
                );
                let actions = fk_action_choices(&self.session.target.engine);
                let delete_now = self.session.fk_on_delete.clone();
                let update_now = self.session.fk_on_update.clone();
                let delete_entity = entity.clone();
                let update_entity = entity.clone();
                let delete_checked = delete_now.clone();
                let update_checked = update_now.clone();
                let delete_button = stack_frame(|| {
                    Button::new("structure-fk-on-delete")
                        .outline()
                        .small()
                        .icon(IconName::ChevronDown)
                        .label(SharedString::from(format!("{}: {delete_now}", i18n::text(locale, "structure.fk.onDelete"))))
                        .dropdown_menu(move |mut menu, _, _| {
                            for choice in actions {
                                let delete_entity = delete_entity.clone();
                                let choice = (*choice).to_owned();
                                let is_cur = delete_checked.eq_ignore_ascii_case(&choice);
                                menu = menu.item(PopupMenuItem::new(choice.clone()).checked(is_cur).on_click(move |_, _, cx| {
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
                        })
                        .into_any_element()
                });
                let update_button = stack_frame(|| {
                    let actions = fk_action_choices(&self.session.target.engine);
                    Button::new("structure-fk-on-update")
                        .outline()
                        .small()
                        .icon(IconName::ChevronDown)
                        .label(SharedString::from(format!("{}: {update_now}", i18n::text(locale, "structure.fk.onUpdate"))))
                        .dropdown_menu(move |mut menu, _, _| {
                            for choice in actions {
                                let update_entity = update_entity.clone();
                                let choice = (*choice).to_owned();
                                let is_cur = update_checked.eq_ignore_ascii_case(&choice);
                                menu = menu.item(PopupMenuItem::new(choice.clone()).checked(is_cur).on_click(move |_, _, cx| {
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
                        })
                        .into_any_element()
                });
                form = form.child(div().flex().flex_wrap().gap(px(8.)).child(delete_button).child(update_button));
                if let Some(sql) = self.live_composer_sql() {
                    form = form.child(
                        kit::sunken(&p)
                            .px(px(10.))
                            .py(px(8.))
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_size(px(TypeScale::SM))
                            .text_color(p.text_muted)
                            .child(SharedString::from(sql)),
                    );
                }
                form
            }),
            Section::View => stack_frame(|| {
                let mut form = form;
                self.ensure_view_editor(window, cx);
                let orig_sql = self.snapshot.as_ref().map(|s| s.table_sql.as_str()).unwrap_or("");
                let view_dirty = !orig_sql.is_empty() && self.session.view_sql.trim() != orig_sql.trim();
                let lines = self.session.view_sql.lines().count().max(1);
                let available = (f32::from(window.viewport_size().height) - 340.).max(220.);
                let copy_sql = self.session.view_sql.clone();
                let open_sql = self.session.view_sql.clone();
                let revert_owner = entity.clone();
                let query_sink = self.query_sink.clone();
                let target_for_query = self.session.target.clone();
                let toolbar = div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .text_size(px(TypeScale::SM))
                            .text_color(p.text_muted)
                            .child(SharedString::from(format!(
                                "{} · {}",
                                i18n::text(locale, "structure.section.viewDef"),
                                i18n::text(locale, "ui.structure.ddlLines").replace("{count}", &lines.to_string())
                            )))
                            .when(view_dirty, |this| this.child(kit::pill(t("structure.view.modified"), p.warn, p.warn_soft))),
                    )
                    .when(view_dirty, |this| {
                        this.child(kit::tool_button("structure-view-revert", Some(IconName::Undo2), t("structure.view.revert"), move |_, _, cx| {
                            revert_owner.update(cx, |pane, cx| {
                                if let Some(snap) = &pane.snapshot {
                                    pane.session.view_sql = snap.table_sql.clone();
                                    pane.session.cancel();
                                    cx.notify();
                                }
                            });
                        }))
                    })
                    .when_some(query_sink, |this, sink| {
                        this.child(kit::secondary_button("structure-view-open-query", Some(IconName::SquarePen), t("structure.ddl.openInQuery"), move |_, _, cx| {
                            sink(target_for_query.clone(), open_sql.clone(), cx);
                        }))
                    })
                    .child(kit::secondary_button("structure-view-copy", Some(IconName::Copy), t("common.copy"), move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy_sql.clone()));
                    }));
                form = form.child(toolbar);
                if let Some(editor) = self.view_editor.clone() {
                    kit::sync_editor(&editor, &mut self.view_editor_sync, &self.session.view_sql, window, cx);
                    form = form.child(
                        div()
                            .h(px(available))
                            .rounded(px(Radius::CONTROL))
                            .border_1()
                            .border_color(if view_dirty { p.warn } else { p.line })
                            .overflow_hidden()
                            .child(Editor::new(&editor).h(relative(1.)).bordered(false).aria_label(t("structure.section.viewDef"))),
                    );
                }
                form
            }),
            Section::Ddl => stack_frame(|| {
                let mut form = form;
                let (sql, missing) = self.snapshot.as_ref().map(definition_text).unwrap_or_else(|| (String::new(), Vec::new()));
                let rebuilt = !missing.is_empty();
                let viewer = self.ddl_viewer(&sql, window, cx);
                let copy_sql = sql.clone();
                let open_sql = sql.clone();
                let lines = sql.lines().count().max(1) as f32;
                let available = (f32::from(window.viewport_size().height) - 300.).max(240.);
                let height = (lines * 20. + 24.).clamp(160., available);
                let copied = self.ddl_copied;
                let owner = cx.entity();
                let query_sink = self.query_sink.clone();
                let target_for_query = self.session.target.clone();
                let toolbar = div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(140.))
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .text_size(px(TypeScale::SM))
                            .text_color(p.text_muted)
                            .child(SharedString::from(format!(
                                "{} · {}",
                                i18n::text(locale, "structure.section.ddl"),
                                i18n::text(locale, "ui.structure.ddlLines").replace("{count}", &(lines as usize).to_string())
                            )))
                            .when(rebuilt, |this| this.child(kit::pill(t("structure.meta.rebuilt"), p.warn, p.warn_soft))),
                    )
                    .when_some(query_sink, |this, sink| {
                        this.child(kit::secondary_button(
                            "structure-open-ddl-query",
                            Some(IconName::SquarePen),
                            t("structure.ddl.openInQuery"),
                            move |_, _, cx| {
                                sink(target_for_query.clone(), open_sql.clone(), cx);
                            },
                        ))
                    })
                    .child(kit::secondary_button(
                        "structure-copy-ddl",
                        Some(if copied { IconName::Check } else { IconName::Copy }),
                        if copied { t("ui.copied") } else { t("common.copy") },
                        move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(copy_sql.clone()));
                            let reset = owner.downgrade();
                            owner.update(cx, |pane, cx| {
                                pane.ddl_copied = true;
                                cx.notify();
                                cx.spawn(async move |_, cx| {
                                    cx.background_executor().timer(std::time::Duration::from_millis(1500)).await;
                                    cx.update(|cx| {
                                        if let Some(pane) = reset.upgrade() {
                                            pane.update(cx, |pane, cx| {
                                                pane.ddl_copied = false;
                                                cx.notify();
                                            });
                                        }
                                    });
                                })
                                .detach();
                            });
                        },
                    ));
                form = form
                    .child(toolbar)
                    .child(div().h(px(height)).rounded(px(Radius::CONTROL)).border_1().border_color(p.line).overflow_hidden().child(viewer));
                if !missing.is_empty() {
                    let sep = match locale {
                        Locale::ZhCn => "、",
                        Locale::En => ", ",
                    };
                    let localized_items = missing.iter().map(|item| localize_missing_item(item, locale)).collect::<Vec<_>>().join(sep);
                    let notice = i18n::fill(&i18n::text(locale, "structure.meta.rebuiltHint"), &[("items", &localized_items)]);
                    form = form.child(
                        div()
                            .flex()
                            .items_start()
                            .gap(px(8.))
                            .px(px(12.))
                            .py(px(8.))
                            .rounded(px(Radius::PANEL))
                            .bg(p.hover)
                            .text_size(px(TypeScale::XS))
                            .text_color(p.text_muted)
                            .child(kit::icon(IconName::Info, 13., p.text_muted))
                            .child(div().flex_1().min_w(px(0.)).child(SharedString::from(notice))),
                    );
                }
                if let Some(snap) = &self.snapshot {
                    let mono = cx.theme().mono_font_family.clone();
                    if !snap.triggers.is_empty() {
                        let heading = SharedString::from(i18n::fill(
                            &i18n::text(locale, "structure.ddl.triggers"),
                            &[("count", &snap.triggers.len().to_string())],
                        ));
                        let mut trig_list = div().flex().flex_col().gap(px(6.));
                        for (idx, trig) in snap.triggers.iter().enumerate() {
                            let trig_sql = trig.sql.clone();
                            trig_list = trig_list.child(
                                kit::card(&p)
                                    .p(px(10.))
                                    .flex()
                                    .flex_col()
                                    .gap(px(4.))
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(px(6.))
                                            .child(div().flex_1().font_weight(FontWeight::MEDIUM).text_size(px(TypeScale::SM)).child(SharedString::from(trig.name.clone())))
                                            .when(!trig_sql.trim().is_empty(), |this| {
                                                this.child(
                                                    kit::icon_button(
                                                        SharedString::from(format!("structure-copy-trigger-{idx}")),
                                                        IconName::Copy,
                                                        t("common.copy"),
                                                        move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(trig_sql.clone())),
                                                    )
                                                    .xsmall(),
                                                )
                                            }),
                                    )
                                    .when(!trig.sql.trim().is_empty(), |this| {
                                        this.child(
                                            div()
                                                .id(SharedString::from(format!("structure-trigger-sql-{idx}")))
                                                .max_h(px(120.))
                                                .overflow_y_scroll()
                                                .font_family(mono.clone())
                                                .text_size(px(TypeScale::XS))
                                                .text_color(p.text_muted)
                                                .child(SharedString::from(trig.sql.clone())),
                                        )
                                    }),
                            );
                        }
                        form = form.child(div().flex().flex_col().gap(px(6.)).child(kit::eyebrow(heading, &p)).child(trig_list));
                    }
                    if !snap.views.is_empty() {
                        let heading = SharedString::from(i18n::fill(
                            &i18n::text(locale, "structure.ddl.dependentViews"),
                            &[("count", &snap.views.len().to_string())],
                        ));
                        let mut view_chips = div().flex().flex_wrap().gap(px(6.));
                        for dep in &snap.views {
                            view_chips = view_chips.child(kit::pill(SharedString::from(dep.name.clone()), p.text, p.hover));
                        }
                        form = form.child(div().flex().flex_col().gap(px(6.)).child(kit::eyebrow(heading, &p)).child(view_chips));
                    }
                }
                form
            }),
            Section::Danger => stack_frame(|| {
                let mut form = form;
                let view = self.session.object_kind == "view";
                let back_owner = entity.clone();
                let mut mode_bar = div().flex().flex_wrap().items_center().gap(px(6.));
                if !view {
                    for (kind, key) in [
                        (DangerKind::Rename, "catalog.tree.renameTable"),
                        (DangerKind::Truncate, "catalog.tree.truncateTable"),
                        (DangerKind::Drop, "catalog.tree.dropTable"),
                    ] {
                        let owner = entity.clone();
                        let active = self.session.danger == kind;
                        mode_bar = mode_bar.child(
                            kit::tool_button(
                                SharedString::from(format!("structure-danger-kind-{kind:?}")),
                                None,
                                t(key),
                                move |_, _, cx| {
                                    owner.update(cx, |pane, cx| {
                                        pane.session.danger = kind;
                                        pane.session.cancel();
                                        cx.notify();
                                    });
                                },
                            )
                            .selected(active),
                        );
                    }
                }
                mode_bar = mode_bar.child(div().flex_1()).child(kit::tool_button(
                    "structure-danger-back",
                    Some(IconName::ArrowLeft),
                    t("structure.danger.backColumns"),
                    move |_, _, cx| {
                        back_owner.update(cx, |pane, cx| {
                            pane.session.section = Section::Columns;
                            pane.session.cancel();
                            cx.notify();
                        });
                    },
                ));
                form = form.child(mode_bar);

                let (title_key, detail_key) = match (self.session.danger, view) {
                    (_, true) => ("catalog.tree.dropView", "ui.structure.dropDetail"),
                    (DangerKind::Rename, false) => ("catalog.tree.renameTable", "structure.danger.renameDetail"),
                    (DangerKind::Truncate, false) => ("catalog.tree.truncateTable", "structure.danger.truncateDetail"),
                    (DangerKind::Drop, false) => ("catalog.tree.dropTable", "ui.structure.dropDetail"),
                };
                let rename = self.session.danger == DangerKind::Rename && !view;
                let tone = if rename { p.warn } else { p.danger };
                let tone_soft = if rename { p.warn_soft } else { p.danger_soft };
                let impact_line = self.snapshot.as_ref().and_then(|snap| {
                    let mut facts = Vec::new();
                    if let Some(rows) = snap.row_count.filter(|_| !rename && !view) {
                        facts.push(i18n::fill(&i18n::text(locale, "structure.meta.rows"), &[("count", &group_digits(rows))]));
                    }
                    if !snap.incoming.is_empty() {
                        facts.push(i18n::fill(
                            &i18n::text(locale, "structure.danger.incomingCount"),
                            &[("count", &snap.incoming.len().to_string())],
                        ));
                    }
                    let deps = snap.triggers.len() + snap.views.len();
                    if deps > 0 {
                        facts.push(i18n::fill(&i18n::text(locale, "structure.danger.dependentCount"), &[("count", &deps.to_string())]));
                    }
                    (!facts.is_empty()).then(|| {
                        let sep = match locale {
                            Locale::ZhCn => " · ",
                            Locale::En => ", ",
                        };
                        i18n::fill(&i18n::text(locale, "structure.danger.impact"), &[("detail", &facts.join(sep))])
                    })
                });
                form = form.child(
                    div()
                        .flex()
                        .gap(px(10.))
                        .p(px(metrics.card_pad()))
                        .rounded(px(Radius::PANEL))
                        .bg(tone_soft)
                        .text_color(tone)
                        .child(kit::icon(IconName::TriangleAlert, 16., tone))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .flex()
                                .flex_col()
                                .gap(px(3.))
                                .child(
                                    div()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(SharedString::from(format!("{} {}", i18n::text(locale, title_key), self.session.target.table))),
                                )
                                .child(div().text_size(px(TypeScale::SM)).child(t(detail_key)))
                                .when_some(impact_line, |this, line| {
                                    this.child(div().pt(px(2.)).text_size(px(TypeScale::XS)).font_weight(FontWeight::MEDIUM).child(SharedString::from(line)))
                                }),
                        ),
                );
                if rename {
                    let input = self.field("rename_to", "catalog.danger.newName", "new_name", window, cx);
                    form = form.child(div().max_w(px(360.)).child(input));
                }
                if self.session.production() && !self.session.preview_open() {
                    let typed = self.draft_input(6, &self.session.danger_name.clone(), &self.session.target.table.clone(), window, cx);
                    form = form.child(kit::form_row(t("structure.danger.typeName"), div().max_w(px(360.)).child(typed), &p));
                }
                form
            }),
        };
        form.into_any_element()
    }
}

impl Render for StructurePane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_snapshot(cx);
        let form = if self.form_visible() { Some(self.section_form(window, cx)) } else { None };
        if let Some(key) = self.pending_focus_draft.take() {
            if self.draft_fields.contains(key) {
                self.draft_fields.focus(key, window, cx);
            }
        }
        let p = Palette::of(cx);
        let metrics = kit::Metrics::of(cx);
        let section = self.session.section;
        let entity = cx.entity();
        let preview = self.render_preview_dialog(&entity, &p, metrics, section, window, cx);
        self.render_page(form, preview, window, cx)
    }
}

impl StructurePane {
    #[inline(never)]
    fn render_page(
        &mut self,
        form: Option<AnyElement>,
        preview: Option<AnyElement>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let p = Palette::of(cx);
        let metrics = kit::Metrics::of(cx);
        let locale = self.session.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let section = self.session.section;
        let entity = cx.entity();

        let counts = self.snapshot.as_ref().map(|snapshot| {
            (snapshot.columns.len(), snapshot.indexes.len(), snapshot.foreign_keys.len())
        });
        let pending_cols = self.pending_changes();
        let mut tabs = div().id("structure-tabs").role(gpui_kit::Role::TabList).flex().items_end().gap(px(4.));
        for value in self.session.visible_sections() {
            if value == Section::Danger && section != Section::Danger {
                continue;
            }
            let key = match value {
                Section::Columns => "structure.section.columns",
                Section::Indexes => "structure.section.indexes",
                Section::ForeignKeys => "structure.section.foreign",
                Section::View => "structure.section.viewDef",
                Section::Ddl => "structure.section.ddl",
                Section::Danger => "structure.section.danger",
            };
            let count = match (value, counts) {
                (Section::Columns, Some((columns, _, _))) => Some(columns),
                (Section::Indexes, Some((_, indexes, _))) => Some(indexes),
                (Section::ForeignKeys, Some((_, _, keys))) => Some(keys),
                _ => None,
            };
            let selected = section == value;
            let danger = value == Section::Danger;
            let dirty_dot = value == Section::Columns && pending_cols > 0;
            let owner = entity.clone();
            tabs = tabs.child(
                kit::interactive_states(div().id(SharedString::from(key)), &p, false)
                    .test_support()
                    .role(gpui_kit::Role::Tab)
                    .aria_label(t(key))
                    .aria_selected(selected)
                    .h(px(metrics.tabs_h))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .border_b_2()
                    .text_size(px(TypeScale::SM))
                    .when(selected, |this| {
                        this.border_color(if danger { p.danger } else { p.accent })
                            .text_color(if danger { p.danger } else { p.text })
                            .font_weight(FontWeight::MEDIUM)
                    })
                    .when(!selected, |this| {
                        this.border_color(gpui_kit::hsla(0., 0., 0., 0.))
                            .text_color(p.text_muted)
                            .hover(|style| style.text_color(p.text))
                    })
                    .child(t(key))
                    .when_some(count, |this, count| {
                        this.child(
                            div()
                                .px(px(5.))
                                .rounded(px(Radius::TAG))
                                .bg(if dirty_dot { p.warn_soft } else { p.hover })
                                .text_size(px(TypeScale::XS))
                                .text_color(if dirty_dot { p.warn } else { p.text_muted })
                                .child(SharedString::from(count.to_string())),
                        )
                    })
                    .when(dirty_dot, |this| this.child(kit::status_dot(p.warn)))
                    .on_click(move |_, _, cx| {
                        owner.update(cx, |pane, cx| {
                            pane.session.section = value;
                            pane.session.intent = EditIntent::Apply;
                            pane.session.cancel();
                            pane.composer_open = false;
                            cx.notify();
                        });
                    }),
            );
        }

        let title = if self.session.mode == StructureMode::Create {
            t("structure.action.createTable")
        } else if self.session.target.table.is_empty() {
            t("structure.section.columns")
        } else {
            SharedString::from(self.session.target.table.clone())
        };

        let existing = self.session.mode != StructureMode::Create && !self.session.target.table.is_empty();
        let qualified = {
            let target = &self.session.target;
            [target.database.as_str(), target.schema.as_str(), target.table.as_str()]
                .iter()
                .filter(|part| !part.is_empty())
                .copied()
                .collect::<Vec<_>>()
                .join(".")
        };

        let more = (self.session.mode != StructureMode::Create && self.session.visible_sections().contains(&Section::Danger)).then(|| {
            let owner = entity.clone();
            let view = self.session.object_kind == "view";
            let query_sink = self.query_sink.clone();
            let target_for_menu = self.session.target.clone();
            let qualified_for_menu = qualified.clone();
            let ddl_for_menu = self.snapshot.as_ref().map(|snap| definition_text(snap).0).unwrap_or_default();
            Button::new("structure-more")
                .ghost()
                .small()
                .icon(IconName::Ellipsis)
                .label(t("structure.action.more"))
                .dropdown_menu(move |mut menu, _, cx| {
                    let pal = Palette::of(cx);
                    menu = menu.min_w(px(200.));
                    if let Some(sink) = query_sink.clone() {
                        let tgt = target_for_menu.clone();
                        let sql = select_sample_sql(&tgt);
                        let label = i18n::text(locale, "structure.action.openQuery");
                        menu = menu.item(
                            PopupMenuItem::element(move |_, cx| {
                                kit::popup_menu_row(Some(IconName::SquarePen), label.clone(), None, false, &Palette::of(cx))
                            })
                            .on_click(move |_, _, cx| sink(tgt.clone(), sql.clone(), cx)),
                        );
                    }
                    if !qualified_for_menu.is_empty() {
                        let q = qualified_for_menu.clone();
                        let label = i18n::text(locale, "structure.action.copyQualified");
                        menu = menu.item(
                            PopupMenuItem::element(move |_, cx| {
                                kit::popup_menu_row(Some(IconName::Copy), label.clone(), None, false, &Palette::of(cx))
                            })
                            .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(q.clone()))),
                        );
                    }
                    if !ddl_for_menu.trim().is_empty() {
                        let sql = ddl_for_menu.clone();
                        let label = i18n::text(locale, "structure.action.copyDdl");
                        menu = menu.item(
                            PopupMenuItem::element(move |_, cx| {
                                kit::popup_menu_row(Some(IconName::Code), label.clone(), None, false, &Palette::of(cx))
                            })
                            .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(sql.clone()))),
                        );
                    }
                    menu = menu.separator();
                    let entries: Vec<(&str, DangerKind, IconName, bool)> = if view {
                        vec![("catalog.tree.dropView", DangerKind::Drop, IconName::Trash, true)]
                    } else {
                        vec![
                            ("catalog.tree.renameTable", DangerKind::Rename, IconName::Pencil, false),
                            ("catalog.tree.truncateTable", DangerKind::Truncate, IconName::Trash, true),
                            ("catalog.tree.dropTable", DangerKind::Drop, IconName::Trash, true),
                        ]
                    };
                    let _ = pal;
                    for (key, kind, ic, is_danger) in entries {
                        let owner = owner.clone();
                        let label = i18n::text(locale, key);
                        let item = PopupMenuItem::element(move |_, cx| {
                            kit::popup_menu_row(Some(ic), label.clone(), None, is_danger, &Palette::of(cx))
                        })
                        .on_click(move |_, _, cx| {
                            owner.update(cx, |pane, cx| {
                                pane.session.section = Section::Danger;
                                pane.session.danger = kind;
                                pane.session.intent = EditIntent::Apply;
                                pane.session.cancel();
                                cx.notify();
                            });
                        });
                        menu = menu.item(item);
                    }
                    menu
                })
        });

        let show_meta_pills = existing && (self.content_width == 0. || self.content_width >= 680.);
        let meta_pills: Vec<AnyElement> = if show_meta_pills {
            let mut pills = Vec::new();
            let env = self.session.target.environment.trim();
            if !env.is_empty() && env != "none" && env != "local" {
                pills.push(kit::env_tag(env, SharedString::from(env.to_uppercase()), &p));
            }
            if let Some(snap) = &self.snapshot {
                if snap.kind == "view" {
                    let key = if snap.materialized { "structure.meta.materializedView" } else { "structure.meta.view" };
                    pills.push(kit::pill(t(key), p.accent_text, p.accent_soft));
                } else {
                    if let Some(storage) = snap.storage.as_deref().filter(|s| !s.is_empty()) {
                        pills.push(kit::pill(SharedString::from(storage.to_owned()), p.text_muted, p.hover));
                    }
                    if snap.strict {
                        pills.push(kit::pill("STRICT", p.text_muted, p.hover));
                    }
                    if snap.without_rowid {
                        pills.push(kit::pill("WITHOUT ROWID", p.text_muted, p.hover));
                    }
                    if let Some(rows) = snap.row_count {
                        let label = i18n::fill(&i18n::text(locale, "structure.meta.rows"), &[("count", &group_digits(rows))]);
                        pills.push(kit::pill(SharedString::from(label), p.text_muted, p.hover));
                    }
                }
            }
            pills
        } else {
            Vec::new()
        };

        let banner_text = SharedString::from(self.session.display_banner());
        let title_row: AnyElement = if existing {
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .min_w(px(0.))
                .child(kit::copyable(
                    "structure-copy-name",
                    self.session.target.table.clone(),
                    kit::title(title.clone(), TypeScale::LG, &p),
                    i18n::text(locale, "ui.copy.name"),
                    &p,
                    window,
                    cx,
                ))
                .children(meta_pills)
                .into_any_element()
        } else {
            kit::title(title.clone(), TypeScale::LG, &p)
        };

        let table_comment = self.snapshot.as_ref().and_then(|snap| snap.table_comment.clone()).filter(|c| !c.trim().is_empty());
        let banner_row: AnyElement = if existing {
            div()
                .id("structure-banner")
                .flex()
                .items_center()
                .gap(px(6.))
                .min_w(px(0.))
                .text_size(px(TypeScale::SM))
                .text_color(p.text_muted)
                .child(kit::copyable(
                    "structure-copy-path",
                    qualified.clone(),
                    banner_text.clone(),
                    i18n::fill(&i18n::text(locale, "ui.copy.path"), &[("name", &qualified)]),
                    &p,
                    window,
                    cx,
                ))
                .when_some(table_comment, |this, comment| {
                    this.child(div().text_color(p.text_faint).child("·")).child(
                        div()
                            .id("structure-table-comment")
                            .min_w(px(0.))
                            .truncate()
                            .text_color(p.text_faint)
                            .tooltip_text(SharedString::from(comment.clone()))
                            .child(SharedString::from(comment)),
                    )
                })
                .into_any_element()
        } else {
            div()
                .id("structure-banner")
                .text_size(px(TypeScale::SM))
                .text_color(p.text_muted)
                .truncate()
                .child(banner_text)
                .into_any_element()
        };

        let refresh_btn = existing.then(|| {
            let owner = entity.clone();
            if self.refreshing {
                div()
                    .id("structure-refresh")
                    .size(px(kit::HitArea::CONTROL))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(Spinner::new().xsmall().color(p.accent))
                    .into_any_element()
            } else {
                kit::icon_button("structure-refresh", IconName::RefreshCw, t("structure.action.refresh"), move |_, _, cx| {
                    owner.update(cx, |pane, cx| pane.refresh(cx));
                })
                .disabled(self.loading)
                .into_any_element()
            }
        });

        let focus = self.focus.get_or_insert_with(|| cx.focus_handle()).clone();
        let mut column = div()
            .id("structure-pane")
            .test_support()
            .key_context("Structure")
            .track_focus(&focus)
            .on_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, _, cx| {
                let key = event.keystroke.key.as_str();
                if key == "escape" && this.handle_escape() {
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                if key == "enter"
                    && (event.keystroke.modifiers.control || event.keystroke.modifiers.platform)
                    && this.trigger_preview_shortcut(cx)
                {
                    cx.stop_propagation();
                }
            }))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .px(px(metrics.page_pad))
                    .pt(px(metrics.page_pad_y))
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .border_b_1()
                    .border_color(p.line)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(kit::engine_badge(&self.session.target.engine, 28.))
                            .child(div().flex_1().min_w(px(120.)).flex().flex_col().child(title_row).child(banner_row))
                            .children(refresh_btn)
                            .when_some(
                                self.data_sink
                                    .clone()
                                    .filter(|_| self.session.mode != StructureMode::Create && !self.session.target.table.is_empty()),
                                |this, sink| {
                                    this.child(kit::secondary_button("structure-open-data", Some(IconName::Table), t("ui.table.switchData"), move |_, _, cx| {
                                        sink(cx);
                                    }))
                                },
                            )
                            .children(more),
                    )
                    .child(tabs),
            );

        let mut body = div()
            .id("structure-body")
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .p(px(metrics.page_pad))
            .flex()
            .flex_col()
            .gap(px(metrics.section_gap()));
        let readonly_view = self.session.object_kind == "view" && self.session.mode != StructureMode::Create;
        let editing_columns = section == Section::Columns && self.form_visible() && !readonly_view;
        let composer_section = matches!(section, Section::Indexes | Section::ForeignKeys);
        if editing_columns {
            if let Some(form) = form {
                body = body.child(form);
            }
        } else {
            if composer_section && self.form_visible() && (self.list_len(section) > 0 || self.composer_open) {
                let owner = entity.clone();
                let label = if section == Section::Indexes { t("structure.index.new") } else { t("structure.fk.new") };
                body = body.child(
                    div().flex().items_center().child(div().flex_1()).child(kit::secondary_button(
                        "structure-composer-toggle",
                        Some(if self.composer_open { IconName::X } else { IconName::Plus }),
                        if self.composer_open { t("common.cancel") } else { label },
                        move |_, _, cx| {
                            owner.update(cx, |pane, cx| {
                                pane.composer_open = !pane.composer_open;
                                pane.session.cancel();
                                cx.notify();
                            });
                        },
                    )),
                );
            }
            let ddl_direct = section == Section::Ddl && form.is_some() && self.has_definition();
            let danger_direct = section == Section::Danger && form.is_some() && !self.loading && self.load_error.is_empty();
            let mut form = form;
            if ddl_direct || danger_direct {
                if let Some(form) = form.take() {
                    body = body.child(div().id("structure-composer").test_support().child(form));
                }
            } else {
                body = body.child(kit::card(&p).overflow_hidden().child(self.overview(&entity, &p, cx)));
            }
            if section == Section::Columns && readonly_view {
                if let Some(form) = form.take() {
                    body = body.child(form);
                }
            } else if let Some(form) = form.filter(|_| !composer_section || self.composer_open) {
                let heading = match section {
                    Section::Indexes => t("structure.index.new"),
                    Section::ForeignKeys => t("structure.fk.new"),
                    Section::Danger => t("structure.section.danger"),
                    _ => t("structure.section.definition"),
                };
                body = body.child(
                    div()
                        .id("structure-composer")
                        .test_support()
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .child(kit::eyebrow(heading, &p))
                        .child(kit::card(&p).p(px(metrics.card_pad())).child(form)),
                );
            }
            if section == Section::ForeignKeys {
                if let Some(incoming) = self.incoming_fk_card(&entity, &p, cx) {
                    body = body.child(incoming);
                }
            }
        }
        if !self.session.note.is_empty() {
            body = body.child(
                div()
                    .px(px(12.))
                    .py(px(8.))
                    .rounded(px(Radius::PANEL))
                    .bg(p.hover)
                    .text_size(px(TypeScale::SM))
                    .child(SharedString::from(self.session.note())),
            );
        }
        column = column.child(body.test_support());
        if let Some(actions) = self.preview_footer(&entity, &p, locale, metrics) {
            column = column.child(actions);
        }
        if let Some(preview) = preview {
            column = column.child(preview);
        }
        column
    }
}

impl StructurePane {
    #[inline(never)]
    fn render_preview_dialog(
        &mut self,
        entity: &Entity<Self>,
        p: &Palette,
        metrics: kit::Metrics,
        section: Section,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.session.preview_open() {
            return None;
        }
        let locale = self.session.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let dangerous = self.session.preview_dangerous();
        let armed = self.session.armed();
        let confirm_blocked = self.session.confirm_blocked();
        let executing = self.executing;
        let object = SharedString::from(self.session.object_label());
        let steps = self.session.preview_steps();
        let sql_raw = self.session.preview_sql().join("\n");
        let sql_display = if steps.len() > 1 {
            steps
                .iter()
                .enumerate()
                .map(|(i, (label, sql))| format!("-- {}. {label}\n{sql}", i + 1))
                .collect::<Vec<_>>()
                .join("\n\n")
        } else {
            sql_raw.clone()
        };
        let banner = SharedString::from(self.session.display_banner());
        let target = SharedString::from(self.session.preview_target());
        let rule = self.session.preview_rule();
        let note_text = self.session.note();
        let risks = self.session.preview_risks();
        let blocked_text = self.session.blocked().join(" ");
        let nothing_to_run = sql_raw.trim().is_empty();
        let only_unchanged = nothing_to_run && self.session.preview_unchanged();
        let preview_title = t("structure.preview.title");
        let confirm_label = if armed { t("structure.action.confirmAgain") } else { t("structure.action.confirm") };
        let cancel_label = t("structure.action.cancel");
        let typed_hint = (section == Section::Danger && self.session.production() && confirm_blocked).then(|| t("structure.danger.typeHint"));
        let danger_field = if section == Section::Danger && self.session.production() {
            Some(kit::form_row(
                t("structure.danger.typeName"),
                self.draft_input(6, &self.session.danger_name.clone(), &self.session.target.table.clone(), window, cx),
                p,
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
                    if !pane.executing {
                        pane.session.cancel();
                        cx.notify();
                    }
                })
            }
        };
        let close_header = {
            let entity = entity.clone();
            move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                entity.update(cx, |pane, cx| {
                    if !pane.executing {
                        pane.session.cancel();
                        cx.notify();
                    }
                })
            }
        };
        let scrim_entity = entity.clone();
        let copy_entity = entity.clone();
        let copy_sql_text = sql_raw.clone();
        let copied = self.preview_copied;
        let steps_heading = SharedString::from(i18n::fill(
            &i18n::text(locale, "structure.preview.steps"),
            &[("count", &steps.len().to_string())],
        ));

        let dialog = div()
            .id("structure-preview-scrim")
            .absolute()
            .inset_0()
            .bg(gpui_kit::hsla(0., 0., 0., if p.dark { 0.45 } else { 0.25 }))
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                scrim_entity.update(cx, |pane, cx| {
                    if !pane.executing {
                        pane.session.cancel();
                        cx.notify();
                    }
                });
            })
            .child(
                div()
                    .id("structure-preview-dialog")
                    .test_support()
                    .role(gpui_kit::Role::Dialog)
                    .aria_label(preview_title.clone())
                    .occlude()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .w(px(640.))
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
                            .when(!dangerous, |this| this.child(kit::icon(IconName::Code, 16., p.accent)))
                            .child(kit::title(preview_title, TypeScale::LG, p))
                            .child(div().flex_1().min_w(px(0.)))
                            .child(
                                div()
                                    .id("structure-preview-object")
                                    .test_support()
                                    .min_w(px(0.))
                                    .font_family(cx.theme().mono_font_family.clone())
                                    .text_size(px(TypeScale::SM))
                                    .text_color(p.text_muted)
                                    .truncate()
                                    .child(object),
                            )
                            .child(kit::icon_button("structure-preview-close", IconName::X, t("common.close"), close_header).xsmall().disabled(executing)),
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
                            .child(kit::muted(banner, p))
                            .when(!nothing_to_run, |this| {
                                this.child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap(px(6.))
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .child(div().flex_1().child(kit::eyebrow(steps_heading, p)))
                                                .child(
                                                    kit::tool_button(
                                                        "structure-preview-copy",
                                                        Some(if copied { IconName::Check } else { IconName::Copy }),
                                                        if copied { t("ui.copied") } else { t("structure.preview.copySql") },
                                                        move |_, _, cx| {
                                                            cx.write_to_clipboard(ClipboardItem::new_string(copy_sql_text.clone()));
                                                            let reset = copy_entity.downgrade();
                                                            copy_entity.update(cx, |pane, cx| {
                                                                pane.preview_copied = true;
                                                                cx.notify();
                                                                cx.spawn(async move |_, cx| {
                                                                    cx.background_executor().timer(std::time::Duration::from_millis(1500)).await;
                                                                    cx.update(|cx| {
                                                                        if let Some(pane) = reset.upgrade() {
                                                                            pane.update(cx, |pane, cx| {
                                                                                pane.preview_copied = false;
                                                                                cx.notify();
                                                                            });
                                                                        }
                                                                    });
                                                                })
                                                                .detach();
                                                            });
                                                        },
                                                    )
                                                    .xsmall(),
                                                ),
                                        )
                                        .child(
                                            div()
                                                .id("structure-preview-sql")
                                                .max_h(px(240.))
                                                .overflow_y_scroll()
                                                .p(px(12.))
                                                .rounded(px(Radius::PANEL))
                                                .bg(p.sunken)
                                                .border_1()
                                                .border_color(p.line)
                                                .font_family(cx.theme().mono_font_family.clone())
                                                .text_size(px(TypeScale::SM))
                                                .child(SharedString::from(sql_display)),
                                        ),
                                )
                            })
                            .child(kit::muted(target, p))
                            .when(!risks.is_empty(), |this| {
                                let mut list = div().flex().flex_col().gap(px(4.));
                                for (level, text) in risks {
                                    let (fg, bg, ic) = if level == "danger" {
                                        (p.danger, p.danger_soft, IconName::TriangleAlert)
                                    } else if level == "warn" {
                                        (p.warn, p.warn_soft, IconName::CircleAlert)
                                    } else {
                                        (p.text_muted, p.hover, IconName::Info)
                                    };
                                    list = list.child(
                                        div()
                                            .flex()
                                            .items_start()
                                            .gap(px(8.))
                                            .px(px(10.))
                                            .py(px(6.))
                                            .rounded(px(Radius::PANEL))
                                            .bg(bg)
                                            .text_color(fg)
                                            .text_size(px(TypeScale::SM))
                                            .child(div().pt(px(2.)).flex_none().child(kit::icon(ic, 13., fg)))
                                            .child(div().flex_1().min_w(px(0.)).child(SharedString::from(text))),
                                    );
                                }
                                this.child(list)
                            })
                            .when(!rule.is_empty() && !nothing_to_run, |this| this.child(kit::muted(SharedString::from(rule), p)))
                            .when(!blocked_text.is_empty(), |this| {
                                let (fg, bg) = if only_unchanged { (p.text_muted, p.hover) } else { (p.danger, p.danger_soft) };
                                this.child(
                                    div()
                                        .id("structure-preview-blocked")
                                        .px(px(12.))
                                        .py(px(8.))
                                        .rounded(px(Radius::PANEL))
                                        .bg(bg)
                                        .text_color(fg)
                                        .text_size(px(TypeScale::SM))
                                        .child(SharedString::from(blocked_text.clone())),
                                )
                            })
                            .when(!note_text.is_empty(), |this| {
                                if armed {
                                    this.child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(px(8.))
                                            .px(px(10.))
                                            .py(px(8.))
                                            .rounded(px(Radius::PANEL))
                                            .bg(p.danger_soft)
                                            .text_color(p.danger)
                                            .text_size(px(TypeScale::SM))
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(kit::icon(IconName::TriangleAlert, 14., p.danger))
                                            .child(div().flex_1().min_w(px(0.)).child(SharedString::from(note_text))),
                                    )
                                } else {
                                    this.child(kit::muted(SharedString::from(note_text), p))
                                }
                            })
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
                            .child(kit::secondary_button("structure-cancel", None, cancel_label, cancel).disabled(executing))
                            .child(if dangerous || armed {
                                kit::danger_button("structure-confirm", Some(IconName::Check), confirm_label, confirm)
                                    .disabled(confirm_blocked || nothing_to_run)
                                    .loading(executing)
                                    .into_any_element()
                            } else {
                                kit::primary_button("structure-confirm", Some(IconName::Check), confirm_label, confirm)
                                    .disabled(nothing_to_run)
                                    .loading(executing)
                                    .into_any_element()
                            }),
                    ),
            )
            .into_any_element();
        Some(dialog)
    }
}

#[cfg(test)]
mod tests;
