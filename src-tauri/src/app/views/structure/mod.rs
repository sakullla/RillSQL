//! 对象结构工作台：字段设计器、索引管理、外键与被引用关系、视图与 DDL 定义、表级危险操作与安全变更预览。
//! 预览和执行只走 `application::ddl`，取消预览不发送任何 SQL。

use std::collections::HashSet;
use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::base::FocusTrapElement as _;
use gpui_kit::component::input::{Editor, EditorState, InputEvent};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, Entity, FocusHandle, FontWeight,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, TestSupportExt as _, UniformListScrollHandle, Window, div, px, relative,
};

use crate::app::views::grid::{GridTarget, group_digits, operation_banner};
use crate::application::AppServices;
use crate::ddl::english::{localize, localize_missing_item};
use crate::ddl::naming::{IndexNaming, index_name_limit};
use crate::adapters::ddl::types_named;
use crate::ddl::sql::{default_problem, existing_default, valid_name, valid_type};
use crate::ddl::model::{EnumSetDraft, EnumSetKind, GeneratedColumnDraft, PartitionDraft, PartitionMethod};
use crate::ddl::templates::{ColumnTemplate, TemplateStore};
use crate::ddl::{
    Action, ColumnDraft, DdlEngine, DefaultSpec, ForeignKeyDraft, IndexColumnDraft, IndexDraft, IndexKind, Plan, Request, Snapshot, TableOptions,
    fk_sql_for_display, index_sql_for_display, preview_plan,
};
use crate::i18n::{self, Locale};
use crate::ui::kit::{self, ControlSize, FieldSpec, Fields, Palette, Radius, Tip as _, TypeScale, stack_frame};
use crate::ui::kit::ScrollAreaExt as _;
use crate::ui::picker::Picker;
use crate::ui::{DataFormat, EditMode, FormatViewer, TruncationState, ViewerSpec};

/// 与 `ddl::model::fk_actions` 相同的级联动作。结构页按引擎原样列出（由适配器的类型规则给出）。
fn fk_action_choices(engine: &str) -> &'static [&'static str] {
    crate::adapters::ddl::fk_actions_named(engine)
}

fn default_column_type(engine: &str) -> &'static str {
    types_named(engine).default_column_type()
}

fn default_id(engine: &str) -> ColumnDraft {
    ColumnDraft {
        original: None,
        name: "id".into(),
        type_name: types_named(engine).id_column_type().into(),
        nullable: false,
        default: DefaultSpec::None,
        comment: String::new(),
        primary: true,
        auto: true,
        dropped: false,
        ..ColumnDraft::default()
    }
}

fn column_drafts(snapshot: &Snapshot) -> Vec<ColumnDraft> {
    let mut drafts: Vec<ColumnDraft> = snapshot
        .columns
        .iter()
        .map(|column| ColumnDraft {
            original: Some(column.name.clone()),
            name: column.name.clone(),
            type_name: column.type_name.clone(),
            nullable: column.nullable,
            default: existing_default(snapshot.engine, column.default_sql.as_deref()),
            comment: String::new(),
            primary: column.primary,
            pk_order: None,
            auto: column.auto,
            dropped: false,
            // 专有形态按读回的实际定义起稿：只改候选值、表达式或存储方式时草稿里也有完整定义，
            // 生成语句不会被写成一条缺定义的普通列。
            enum_set: snapshot.enum_sets.get(&column.name).map(|state| EnumSetDraft { kind: state.kind, values: state.values.clone() }),
            generated: snapshot
                .generation
                .get(&column.name)
                .map(|state| GeneratedColumnDraft { expression: state.expression.clone(), storage: state.storage }),
        })
        .collect();
    crate::ddl::plan::assign_pk_order(snapshot, &mut drafts);
    drafts
}

/// 外键可以引用另一个模式里的表时，结构页显示目标模式。
fn shows_ref_schema(engine: &str) -> bool {
    crate::adapters::ddl::ddl_named(engine).is_some_and(|dialect| dialect.references_cross_schema())
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

/// 按设置里的命名模板生成索引名，并按方言的长度上限截断（超长时末尾加 `_` 和 6 位哈希）。
fn suggested_index_name(naming: &IndexNaming, engine: &str, table: &str, unique: bool, columns: &[IndexColumnDraft]) -> String {
    let names: Vec<&str> = columns.iter().map(|col| col.name.as_str()).collect();
    naming.name(table, unique, &names, index_name_limit(engine))
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

/// “在查询中打开”的取样语句。引擎识别不出来时不猜方言，返回 None（菜单不提供这一项）。
fn select_sample_sql(target: &GridTarget) -> Option<String> {
    let engine = DdlEngine::parse(&target.engine).ok()?;
    let qualified = crate::adapters::qualified_object(engine, &target.database, &target.schema, &target.table);
    Some(crate::adapters::first_rows_select(engine, &qualified, 100))
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
    /// 字段注释列（只在新建表且方言支持字段注释、宽度够时显示；否则为 0）。
    comment: f32,
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
            comment: 0.,
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
            comment: 0.,
        }
    }

    #[cfg(test)]
    fn compact() -> Self {
        Self::compact_for(true)
    }

    fn min_total(&self, create: bool) -> f32 {
        let mut total = self.name_min + self.kind + self.nullable + self.default + self.drop;
        let mut columns = 4;
        if self.comment > 0. {
            total += self.comment;
            columns += 1;
        }
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

    /// 有余量时加上字段注释列（从默认值列加宽的部分里让出），不够宽就不显示。
    fn with_comment(mut self, content_w: f32, enabled: bool) -> Self {
        const COMMENT_W: f32 = 160.;
        if !enabled {
            return self;
        }
        let base = Self { default: Self::wide().default.min(self.default), ..self };
        if content_w - base.min_total(true) >= COMMENT_W + Self::GAP {
            let spare = self.default - base.default;
            self.default = base.default + (spare - COMMENT_W - Self::GAP).max(0.);
            self.comment = COMMENT_W;
            if self.min_total(true) > content_w {
                self.default = base.default;
            }
        }
        self
    }
}

const FIELDS_TABLE: [ColumnSpec; 4] = [
    ColumnSpec { key: "structure.field.column", weight: 2., min_w: 96. },
    ColumnSpec { key: "structure.field.type", weight: 1.4, min_w: 76. },
    ColumnSpec { key: "ui.structure.nullable", weight: 0.6, min_w: 52. },
    ColumnSpec { key: "ui.structure.default", weight: 1.4, min_w: 80. },
];

/// 索引页：名称 | 类型 | 字段 | 注释 | 可见性（行尾是编辑/删除）。
const INDEXES_TABLE: [ColumnSpec; 5] = [
    ColumnSpec { key: "structure.field.index", weight: 1.4, min_w: 96. },
    ColumnSpec { key: "structure.index.kind", weight: 0.8, min_w: 72. },
    ColumnSpec { key: "structure.section.columns", weight: 2.2, min_w: 120. },
    ColumnSpec { key: "structure.field.comment", weight: 1.2, min_w: 72. },
    ColumnSpec { key: "structure.index.visible", weight: 0.6, min_w: 56. },
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

/// 分区方案是不是这个引擎的结构能力，由方言的库级段落声明（见 DdlDialect::admin_sections），
/// 界面不自己判断引擎名字。
fn supports_partitions(engine: &str) -> bool {
    crate::adapters::ddl::ddl_named(engine).is_some_and(|dialect| dialect.admin_sections().partitions)
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
    /// 分区方案（RANGE/LIST/HASH/KEY 的建立、修改与移除）。只有 MySQL 与 MariaDB 有这一段。
    Partitions,
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
    /// i18n 句型与填入的参数，显示时按当前语言填写。
    Filled(&'static str, Vec<(&'static str, String)>),
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
            Note::Filled(key, params) => {
                let pairs: Vec<(&str, &str)> = params.iter().map(|(name, value)| (*name, value.as_str())).collect();
                i18n::fill(&i18n::text(locale, key), &pairs)
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        match self {
            Note::Empty => true,
            Note::Key(_) => false,
            Note::Raw(parts) => parts.iter().all(|part| part.is_empty()),
            Note::Filled(..) => false,
        }
    }
}

/// 执行回执里的事务说明：应用、最终结果与隐式提交分开说（R4、R9）。
fn execution_note(executed: &crate::application::ddl::Executed) -> Note {
    if executed.implicit_commit {
        Note::Key("structure.execute.implicitCommit")
    } else if executed.applied_not_committed {
        Note::Key("structure.execute.appliedNotCommitted")
    } else if executed.committed {
        Note::Key("structure.execute.committed")
    } else if let Some(note) = executed.transaction_note.clone().filter(|note| !note.trim().is_empty()) {
        Note::Raw(vec![note])
    } else {
        Note::Key("structure.preview.done")
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
    /// 新建表的表级选项（引擎、字符集、排序规则、表注释等，按方言显示）。
    pub table_options: TableOptions,
    pub index_name: String,
    pub index_column: String,
    pub index_unique: bool,
    pub index_columns: Vec<IndexColumnDraft>,
    /// 索引类型。只有唯一开关的引擎由 `index_unique` 决定（见 `IndexKind::resolve`）。
    pub index_kind: IndexKind,
    /// 索引注释（方言支持时才显示输入框）。
    pub index_comment: String,
    /// 正在修改的已有索引名；None 表示新建。
    pub index_editing: Option<String>,
    pub fk_name: String,
    pub fk_column: String,
    pub fk_ref_schema: String,
    pub fk_ref_table: String,
    pub fk_ref_column: String,
    pub fk_on_delete: String,
    pub fk_on_update: String,
    pub view_sql: String,
    /// 分区方案草稿。方式、操作、表达式、数量与分区定义都在这里，语句由方言生成。
    pub partition: PartitionDraft,
    /// KEY 分区的列清单原文（逗号分隔）。原样保留输入，动作前才拆成列名。
    pub partition_columns: String,
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
    /// 最近一次执行的逐步回执。回执留着，成功收起预览后也能回看每一步做了什么。
    receipt: Option<crate::ddl::Receipt>,
    /// 外壳按连接当前标签与标签表色调算出的生产判断；没有下发时按环境 id 判断。
    pub prod: Option<bool>,
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
            table_options: TableOptions::default(),
            index_name: String::new(),
            index_column: String::new(),
            index_unique: false,
            index_columns: Vec::new(),
            index_kind: IndexKind::Normal,
            index_comment: String::new(),
            index_editing: None,
            fk_name: String::new(),
            fk_column: String::new(),
            fk_ref_schema: String::new(),
            fk_ref_table: String::new(),
            fk_ref_column: String::new(),
            fk_on_delete: "NO ACTION".to_owned(),
            fk_on_update: "NO ACTION".to_owned(),
            view_sql: String::new(),
            partition: PartitionDraft::default(),
            partition_columns: String::new(),
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
            receipt: None,
            prod: None,
        }
    }

    pub fn note(&self) -> String {
        self.note.text(self.locale)
    }

    /// 最近一次执行的逐步回执（被拦下、部分成功、全部成功都会留着）。
    pub fn receipt(&self) -> Option<&crate::ddl::Receipt> {
        self.receipt.as_ref()
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
        let mut sections = vec![Section::Columns, Section::Indexes, Section::ForeignKeys];
        // 分区方案只有 MySQL 家族的表有：别的引擎不给入口，也就不存在“看起来能用”。
        if supports_partitions(&self.target.engine) {
            sections.push(Section::Partitions);
        }
        sections.push(Section::Ddl);
        sections.push(Section::Danger);
        sections
    }

    pub fn production(&self) -> bool {
        self.prod.unwrap_or(matches!(self.target.environment.as_str(), "prod" | "production"))
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
                default: DefaultSpec::None,
                comment: String::new(),
                primary: false,
                auto: false,
                dropped: false,
                ..ColumnDraft::default()
            });
        }
        if columns.is_empty() {
            columns.push(ColumnDraft {
                original: None,
                name: self.column_name.clone(),
                type_name: self.column_type.clone(),
                nullable: true,
                default: DefaultSpec::None,
                comment: String::new(),
                primary: false,
                auto: false,
                dropped: false,
                ..ColumnDraft::default()
            });
        }
        columns
    }

    /// 分区草稿换成动作参数：KEY 的列清单来自输入原文（逗号分隔），其余方式不用列清单。
    /// 输入里的外层反引号与双写反引号按读回表达式同一规则还原成裸列名，用户手写 `id` 也不会被再包一层。
    fn partition_action(&self) -> PartitionDraft {
        let mut draft = self.partition.clone();
        if draft.method == PartitionMethod::Key {
            draft.columns = crate::adapters::mysql::table_read::key_columns_from_expression(&self.partition_columns);
        } else {
            draft.columns.clear();
        }
        draft
    }

    fn index_drafts(&self) -> Vec<IndexColumnDraft> {
        if !self.index_columns.is_empty() {
            return self.index_columns.clone();
        }
        if self.index_column.trim().is_empty() {
            return Vec::new();
        }
        vec![IndexColumnDraft { name: self.index_column.clone(), descending: false, prefix: None }]
    }

    /// 当前索引草稿（新建与修改共用）。
    pub fn index_draft(&self) -> IndexDraft {
        IndexDraft {
            name: self.index_name.clone(),
            kind: IndexKind::resolve(self.index_unique, self.index_kind),
            columns: self.index_drafts(),
            comment: self.index_comment.clone(),
        }
    }

    /// 选索引类型：唯一开关跟着类型走，两者不会互相矛盾。
    pub fn set_index_kind(&mut self, kind: IndexKind) {
        self.index_kind = kind;
        self.index_unique = kind == IndexKind::Unique;
        if kind.plain_key_parts() {
            for column in &mut self.index_columns {
                column.descending = false;
                column.prefix = None;
            }
        }
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
                Action::CreateTable { name: self.table_name.clone(), columns: self.column_drafts_for_action(), options: self.table_options.clone() }
            }
            Section::Columns | Section::Ddl => Action::AlterColumns { table, columns: self.column_drafts_for_action() },
            Section::Indexes => match &self.intent {
                EditIntent::DropIndex { name } => Action::DropIndex { table, name: name.clone() },
                _ => match &self.index_editing {
                    Some(old) => Action::AlterIndex { table, old: old.clone(), new: self.index_draft() },
                    None => Action::CreateIndex {
                        table,
                        name: self.index_name.clone(),
                        unique: self.index_unique,
                        columns: self.index_drafts(),
                        index_kind: self.index_kind,
                        comment: self.index_comment.clone(),
                    },
                },
            },
            Section::ForeignKeys => match &self.intent {
                EditIntent::DropForeignKey { name } => Action::DropForeignKey { table, name: name.clone() },
                _ => Action::CreateForeignKey { table, foreign_key: self.foreign_key_draft() },
            },
            Section::Partitions => Action::AlterPartitions { table, partition: self.partition_action() },
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
        // 新的一次预览：上一次的逐步回执不再对应当前计划。
        self.receipt = None;
    }

    pub fn arm_fixture(&mut self, snapshot: &Snapshot) {
        let plan = preview_plan(snapshot, &self.action());
        self.note = Note::Key("admin.preview.unconnected");
        self.preview = Some(plan);
        self.live = false;
        self.open = true;
        self.armed = false;
        self.receipt = None;
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

    /// 落一次执行回执：算结局、留着回执、写提示；只有全部成功才收起预览。
    ///
    /// 回执的目标必须还是这份预览的目标：目标换了（用户切了表或重新预览）就只提示过期，
    /// 不覆盖当前目标上的回执与提示。返回 None 表示已按过期处理。
    pub(crate) fn apply_receipt(&mut self, receipt: crate::ddl::Receipt) -> Option<receipt::ReceiptOutcome> {
        self.armed = false;
        if self.preview_target() != receipt.target {
            self.note = Note::Key("structure.execute.stale");
            return None;
        }
        let outcome = receipt::ReceiptOutcome::of(&receipt);
        let problems = receipt::problems(&receipt);
        self.receipt = Some(receipt);
        if outcome.is_success() {
            self.open = false;
        }
        self.note = match outcome.note_key() {
            // 部分成功也有一条说明，后面紧跟逐步回执里的原始错误。
            Some(key) => Note::Key(key),
            None => Note::Raw(problems),
        };
        Some(outcome)
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
type TemplatesSink = Arc<dyn Fn(TemplateStore, &mut App)>;

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
    /// 下一帧聚焦时同时全选（新增字段的默认名可直接改写）。
    pending_focus_select: bool,
    connect_request: Option<String>,
    content_width: f32,
    data_sink: Option<DataSink>,
    query_sink: Option<QuerySink>,
    structure_nav_sink: Option<StructureNavSink>,
    /// 常用字段模板（外壳在启动时下发；改动后经 `templates_sink` 交回外壳保存）。
    templates: TemplateStore,
    templates_sink: Option<TemplatesSink>,
    /// 常用字段管理面板（打开时为 Some）。
    template_manager: Option<create::TemplateManager>,
    /// 新建表的表级选项折叠区是否展开。
    options_open: bool,
    /// 索引命名模板（设置页下发）。
    index_naming: IndexNaming,
    /// 新建索引表单里的多选列选择器与它的订阅。
    index_picker: Option<(Entity<Picker>, Subscription)>,
    /// 字段页（已有表）正在编辑的那一行（`kept_columns` 下标）。其余行以文本浏览。
    editing_row: Option<usize>,
    /// 字段页（已有表）键盘选中的行（`kept_columns` 下标），用于 Up/Down/Enter/F2/Delete。
    focused_row: Option<usize>,
    /// 下一帧把字段列表滚到这一行（`kept_columns` 下标）。
    scroll_to_column: Option<usize>,
    /// 窄窗口里被按优先级隐藏的列（注释、默认值）强制显示。
    show_all_columns: bool,
    columns_scroll: UniformListScrollHandle,
    /// DDL 预览弹窗的焦点句柄与焦点陷阱载体。
    preview_focus: Option<FocusHandle>,
    /// 预览弹窗打开后是否已执行过首次自动聚焦（避免每帧抢夺输入框焦点）。
    preview_focused: bool,
    /// 下一帧将焦点还给 StructurePane 根节点（如退出编辑或关闭弹窗后）。
    pending_focus_pane: bool,
    /// 由外壳 Workbench 托管全窗口遮罩与浮层（独立渲染测试时保持 false）。
    shell_overlay_host: bool,
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
            pending_focus_select: false,
            connect_request: None,
            content_width: 0.,
            data_sink: None,
            query_sink: None,
            structure_nav_sink: None,
            templates: TemplateStore::default(),
            templates_sink: None,
            template_manager: None,
            options_open: false,
            index_naming: IndexNaming::default(),
            index_picker: None,
            editing_row: None,
            focused_row: None,
            scroll_to_column: None,
            show_all_columns: false,
            columns_scroll: UniformListScrollHandle::new(),
            preview_focus: None,
            preview_focused: false,
            pending_focus_pane: false,
            shell_overlay_host: false,
        }
    }

    pub fn index_naming(&self) -> &IndexNaming {
        &self.index_naming
    }

    /// 结构页根焦点句柄，供外壳在关闭浮层后把键盘焦点还原回表结构视图。
    pub fn focus_handle(&mut self, cx: &mut Context<Self>) -> FocusHandle {
        self.focus.get_or_insert_with(|| cx.focus_handle()).clone()
    }

    /// 预览弹窗焦点句柄（焦点陷阱与非生产回车确认载体）。
    pub fn preview_focus_handle(&mut self, cx: &mut Context<Self>) -> FocusHandle {
        self.preview_focus.get_or_insert_with(|| cx.focus_handle()).clone()
    }

    /// 声明外壳将负责渲染全窗口遮罩层（覆盖顶栏/侧栏/标签栏并同步 `ShellOverlayOpen`）。
    pub fn set_shell_overlay_host(&mut self, hosted: bool) {
        self.shell_overlay_host = hosted;
    }

    /// 预览弹窗或模板管理弹窗是否处于打开状态。
    pub fn has_modal_overlay(&self) -> bool {
        self.session.preview_open() || self.template_manager.is_some()
    }

    /// DDL 预览弹窗是否打开。
    pub fn preview_open(&self) -> bool {
        self.session.preview_open()
    }

    /// 供外壳在根容器顶层渲染全窗口遮罩与结构模态（预览弹窗或模板管理弹窗）。
    pub fn render_shell_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let p = Palette::of(cx);
        let metrics = kit::Metrics::of(cx);
        let section = self.session.section;
        let entity = cx.entity();
        if let Some(preview) = self.render_preview_dialog(&entity, &p, metrics, section, window, cx) {
            return Some(preview);
        }
        self.render_template_manager(&entity, &p, metrics, window, cx)
    }

    /// 设置页改了索引命名模板：自动命名中的索引名立即按新模板重算，手动名称不动。
    pub fn set_index_naming(&mut self, naming: IndexNaming) {
        if self.index_naming == naming {
            return;
        }
        self.index_naming = naming;
        if self.index_name_auto && !self.session.index_columns.is_empty() {
            self.session.index_name = self.suggested_index_name();
        }
    }

    fn suggested_index_name(&self) -> String {
        let unique = IndexKind::resolve(self.session.index_unique, self.session.index_kind).is_unique();
        suggested_index_name(&self.index_naming, &self.session.target.engine, &self.session.target.table, unique, &self.session.index_columns)
    }

    /// 新建索引的名字与当前表已有索引重名（大小写不敏感）。提交前就地提示，预览也会拦下。
    pub fn index_name_conflict(&self) -> bool {
        let existing: Vec<String> = self.snapshot.as_ref().map(|snap| snap.indexes.iter().map(|index| index.name.clone()).collect()).unwrap_or_default();
        crate::ddl::naming::collides(&existing, &self.session.index_name)
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

    pub fn set_templates_sink(&mut self, sink: TemplatesSink) {
        self.templates_sink = Some(sink);
    }

    /// 外壳下发保存的常用字段模板（启动恢复）。
    pub fn set_templates(&mut self, templates: TemplateStore) {
        self.templates = templates;
    }

    pub fn templates(&self) -> &TemplateStore {
        &self.templates
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
        self.session.table_options = TableOptions::default();
        self.session.kept_columns = vec![default_id(&self.session.target.engine)];
        self.drafts_ready = true;
        self.pending_focus_draft = Some(4);
    }

    pub fn bind(&mut self, target: GridTarget) {
        self.bind_item(target, "");
    }

    /// 外壳下发的生产判断（按标签表色调现算）；标签改色后再次下发即生效。
    pub fn set_prod_target(&mut self, prod: bool) {
        self.session.prod = Some(prod);
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
        self.index_picker = None;
        self.editing_row = None;
        self.focused_row = None;
        self.scroll_to_column = None;
        self.show_all_columns = false;
        self.preview_focused = false;
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
                let result = crate::application::ddl::execute_request_tracked(&services.connections, &connection, &request, &sql, true).await;
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
                            match result {
                                Ok(executed) => {
                                    let note = execution_note(&executed);
                                    let transaction_note = executed.applied_not_committed
                                        || executed.committed
                                        || executed.implicit_commit
                                        || executed.transaction_note.as_ref().is_some_and(|note| !note.trim().is_empty());
                                    match this.session.apply_receipt(executed.receipt) {
                                        Some(outcome) if outcome.is_success() => {
                                            // 只有每一条都 Ok 才收起预览：部分成功留着看逐步状态。
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
                                        // 部分成功：服务器上可能只改了一半，读回真实结构；草稿和已填输入不动。
                                        Some(outcome) if outcome.rereads_target() => this.reread_after_partial(),
                                        _ => {}
                                    }
                                    if transaction_note {
                                        this.session.note = note;
                                    }
                                }
                                Err(message) => {
                                    // 执行链直接报错：逐步回执没有，草稿和已填输入一份都不丢。
                                    this.session.armed = false;
                                    this.session.note = Note::raw(message);
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
            self.editing_row = None;
            self.focused_row = None;
            if self.session.view_sql.is_empty() {
                self.session.view_sql = snapshot.table_sql.clone();
            }
            // 分区草稿按读回的实际方案起稿：界面先显示服务器上的现状，用户改的才是改动。
            self.seed_partition_draft(snapshot);
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

    /// 分区草稿的起稿值：方式与表达式取读回的分区方案，命名分区按定义顺序填入。
    /// 读不到分区方案（或读失败）时保留“建立分区方案”的空草稿，不猜现状。
    fn seed_partition_draft(&mut self, snapshot: &Snapshot) {
        use crate::ddl::model::{PartitionDef, PartitionDraft};
        self.session.partition = match snapshot.partitions.as_ref() {
            Some(current) => PartitionDraft {
                op: crate::ddl::model::PartitionOp::Replace,
                method: current.method,
                expression: if current.method == crate::ddl::model::PartitionMethod::Key {
                    String::new()
                } else {
                    current.expression.clone()
                },
                columns: Vec::new(),
                count: if current.method.named() { String::new() } else { current.definitions.len().to_string() },
                definitions: current
                    .definitions
                    .iter()
                    .map(|def| PartitionDef { name: def.name.clone(), description: def.description.clone() })
                    .collect(),
                targets: Vec::new(),
            },
            None => PartitionDraft::default(),
        };
        // KEY 分区的读回表达式是反引号包住的列清单，起稿就还原成裸列名：
        // 加引号是 table_ddl::key_columns 按方言做的事，界面带着引号会把整段再包一层（KEY(```id```)）。
        // RANGE/LIST/HASH 的表达式不是列清单，原样保留。
        self.session.partition_columns = match snapshot.partitions.as_ref() {
            Some(current) if current.method == crate::ddl::model::PartitionMethod::Key => {
                crate::adapters::mysql::table_read::key_columns_from_expression(&current.expression).join(", ")
            }
            _ => String::new(),
        };
    }

    fn reset_composer_drafts(&mut self) {
        self.session.index_name.clear();
        self.session.index_column.clear();
        self.session.index_unique = false;
        self.session.index_columns.clear();
        self.session.index_kind = IndexKind::Normal;
        self.session.index_comment.clear();
        self.session.index_editing = None;
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
                let auto = self.suggested_index_name();
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
                7 => replace_text(&mut self.session.table_options.comment, value),
                8 => {
                    self.write_template_name(value);
                    false
                }
                _ => false,
            },
            1 => self.edit_column(index, |column| replace_text(&mut column.name, value)),
            2 => self.edit_column(index, |column| replace_text(&mut column.type_name, value)),
            3 => self.edit_column(index, |column| column.default.set_text(value)),
            4 => self.edit_column(index, |column| replace_text(&mut column.comment, value)),
            5..=8 => {
                self.write_template_cell(group, index, value);
                false
            }
            // ENUM/SET 候选值：key 的低 16 位拆成“字段行 × 值序号”，每个候选值单独一个输入框。
            9 => {
                let column = index / Self::ENUM_VALUE_STRIDE;
                let position = index % Self::ENUM_VALUE_STRIDE;
                self.edit_column(column, |draft| match draft.enum_set.as_mut().and_then(|state| state.values.get_mut(position)) {
                    Some(slot) => replace_text(slot, value),
                    None => false,
                })
            }
            // 生成列表达式：每个字段行一个输入框。
            10 => self.edit_column(index, |draft| match draft.generated.as_mut() {
                Some(generated) => replace_text(&mut generated.expression, value),
                None => false,
            }),
            // 分区方案：0/1/2 是表达式、数量、KEY 列清单；10 起是分区定义的名字与边界。
            11 => self.write_partition_cell(index, value),
            // 索引草稿：0 是索引注释，1 起是已选列的前缀长度（按列序号）。
            12 => match index {
                0 => replace_text(&mut self.session.index_comment, value),
                position => self.set_index_prefix(position - 1, &value),
            },
            _ => false,
        };
        if changed && key != 6 {
            self.session.cancel();
        }
    }

    /// 一个字段行最多容纳的候选值个数（ENUM/SET 的输入框编号按它分段）。
    const ENUM_VALUE_STRIDE: usize = 64;

    fn write_partition_cell(&mut self, slot: usize, value: String) -> bool {
        match slot {
            0 => replace_text(&mut self.session.partition.expression, value),
            1 => replace_text(&mut self.session.partition.count, value),
            2 => replace_text(&mut self.session.partition_columns, value),
            slot if slot >= 10 => {
                let position = (slot - 10) / 2;
                let which = (slot - 10) % 2;
                let Some(def) = self.session.partition.definitions.get_mut(position) else {
                    return false;
                };
                if which == 0 {
                    replace_text(&mut def.name, value)
                } else {
                    replace_text(&mut def.description, value)
                }
            }
            _ => false,
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
        let Some((before, before_engine)) = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.columns.iter().find(|col| col.name == original_name).map(|col| (col.clone(), snapshot.engine)))
        else {
            return;
        };
        let enum_set = self.snapshot.as_ref().and_then(|snapshot| snapshot.enum_sets.get(&original_name)).map(|state| EnumSetDraft {
            kind: state.kind,
            values: state.values.clone(),
        });
        let generated = self.snapshot.as_ref().and_then(|snapshot| snapshot.generation.get(&original_name)).map(|state| GeneratedColumnDraft {
            expression: state.expression.clone(),
            storage: state.storage,
        });
        if let Some(draft) = self.session.kept_columns.get_mut(index) {
            draft.name = before.name;
            draft.type_name = before.type_name;
            draft.nullable = before.nullable;
            draft.default = existing_default(before_engine, before.default_sql.as_deref());
            draft.primary = before.primary;
            draft.auto = before.auto;
            draft.dropped = false;
            // 主键位置回到原来的序号；其余主键列随后重新排号。
            draft.pk_order = self
                .snapshot
                .as_ref()
                .filter(|_| before.primary)
                .and_then(|snapshot| crate::ddl::plan::primary_key_names(snapshot).iter().position(|name| *name == original_name))
                .and_then(|position| u16::try_from(position).ok());
            // 还原也要连同专有形态一起还原：否则“还原”之后候选值或表达式还留着上一次的输入。
            draft.enum_set = enum_set;
            draft.generated = generated;
            primary_key::normalize_pk_order(&mut self.session.kept_columns);
            self.session.cancel();
        }
    }

    fn toggle_column(&mut self, index: usize, which: &str) {
        let create = self.session.mode == StructureMode::Create;
        if which == "drop" {
            let is_unsaved_new = self.session.kept_columns.get(index).is_some_and(|col| col.original.is_none());
            if is_unsaved_new && (!create || self.session.kept_columns.len() > 1) {
                self.session.kept_columns.remove(index);
                self.forget_row(index);
                self.session.cancel();
                return;
            }
        }
        if which == "primary" {
            // 新建表与已有表都可以勾选主键：顺序记在 pk_order 里，勾上时自动设为 NOT NULL。
            let Some(on) = self.session.kept_columns.get(index).map(|column| !column.primary) else {
                return;
            };
            if self.session.kept_columns.get(index).is_some_and(|column| column.dropped || column.generated.is_some()) {
                return;
            }
            self.set_primary(index, on);
            return;
        }
        let changed = self.edit_column(index, |column| match which {
            "null" => {
                column.nullable = !column.nullable;
                true
            }
            "auto" if create => {
                column.auto = !column.auto;
                if column.auto {
                    column.nullable = false;
                    column.default = DefaultSpec::None;
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
            default: DefaultSpec::None,
            comment: String::new(),
            primary: false,
            auto: false,
            dropped: false,
            ..ColumnDraft::default()
        });
        self.session.column_name.clear();
        self.only_changed_columns = false;
        if self.session.mode != StructureMode::Create {
            // 已有表：新增的这一行直接进入编辑，并滚到可见处。
            self.editing_row = Some(new_index);
            self.focused_row = Some(new_index);
            self.scroll_to_column = Some(new_index);
        }
        self.pending_focus_draft = Some(10_000 + new_index as u32);
        self.pending_focus_select = true;
        self.session.cancel();
    }

    /// 新建表与修改表共用的常用字段菜单：未隐藏的内置模板与适用于当前引擎的用户模板。
    pub fn available_templates(&self) -> Vec<ColumnTemplate> {
        self.templates.applicable(&self.session.target.engine)
    }

    /// 套用一个常用字段模板：重名字段不重复添加，并就近提示跳过了哪些。
    pub fn apply_template(&mut self, id: &str) -> (Vec<String>, Vec<String>) {
        let Some(template) = self.available_templates().into_iter().find(|template| template.id == id) else {
            return (Vec::new(), Vec::new());
        };
        let (added, skipped) = crate::ddl::templates::apply_template(&mut self.session.kept_columns, &template);
        if !added.is_empty() {
            self.session.cancel();
            self.only_changed_columns = false;
        }
        self.session.note = if skipped.is_empty() {
            Note::Empty
        } else {
            Note::Filled("structure.templates.skipped", vec![("names", skipped.join(", "))])
        };
        (added, skipped)
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
            self.session.table_options = TableOptions::default();
            self.session.kept_columns = vec![default_id(&self.session.target.engine)];
            return;
        }
        if let Some(snapshot) = &self.snapshot {
            self.session.kept_columns = column_drafts(snapshot);
            self.session.view_sql = snapshot.table_sql.clone();
        }
        self.editing_row = None;
        self.focused_row = None;
    }

    fn sync_auto_index_name(&mut self) {
        if self.index_name_auto || self.session.index_name.trim().is_empty() {
            self.session.index_name = self.suggested_index_name();
            self.index_name_auto = true;
        }
    }

    pub fn apply_auto_index_name(&mut self) {
        self.index_name_auto = true;
        self.session.index_name = self.suggested_index_name();
        self.session.cancel();
    }

    fn toggle_index_column(&mut self, name: &str) {
        if let Some(index) = self.session.index_columns.iter().position(|column| column.name == name) {
            self.session.index_columns.remove(index);
        } else {
            self.session.index_columns.push(IndexColumnDraft { name: name.to_owned(), descending: false, prefix: None });
        }
        self.session.index_column = self.session.index_columns.first().map(|column| column.name.clone()).unwrap_or_default();
        self.sync_auto_index_name();
        self.session.intent = EditIntent::Apply;
        self.session.cancel();
    }

    /// 拖放：把已选的第 `from` 列移到第 `to` 个位置。
    pub fn move_index_column_to(&mut self, from: usize, to: usize) {
        let len = self.session.index_columns.len();
        if from == to || from >= len || to >= len {
            return;
        }
        let column = self.session.index_columns.remove(from);
        self.session.index_columns.insert(to, column);
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

    /// 打开某个已有索引的编辑：把它的类型、列（含前缀与升降序）与注释填进草稿，预览时生成修改语句。
    pub fn begin_edit_index(&mut self, name: &str) -> bool {
        let Some(index) = self.snapshot.as_ref().and_then(|snapshot| snapshot.indexes.iter().find(|index| index.name == name)).cloned() else {
            return false;
        };
        self.reset_composer_drafts();
        self.session.index_editing = Some(index.name.clone());
        self.session.index_name = index.name.clone();
        let kind = display_kind(&index);
        self.session.index_kind = kind;
        self.session.index_unique = kind == IndexKind::Unique;
        self.session.index_columns = index
            .columns
            .iter()
            .map(|column| IndexColumnDraft { name: column.name.clone(), descending: column.descending, prefix: column.prefix })
            .collect();
        self.session.index_column = self.session.index_columns.first().map(|column| column.name.clone()).unwrap_or_default();
        self.session.index_comment = index.comment.clone().unwrap_or_default();
        self.index_name_auto = false;
        self.index_picker = None;
        self.composer_open = true;
        self.session.section = Section::Indexes;
        self.session.intent = EditIntent::Apply;
        self.session.cancel();
        true
    }

    /// 选索引类型（界面上的分段选择）。
    pub fn choose_index_kind(&mut self, kind: IndexKind) {
        if IndexKind::resolve(self.session.index_unique, self.session.index_kind) == kind {
            return;
        }
        self.session.set_index_kind(kind);
        self.sync_auto_index_name();
        self.session.intent = EditIntent::Apply;
        self.session.cancel();
    }

    /// 已选第 `position` 列的前缀长度。空或不是正整数时不写前缀。
    pub fn set_index_prefix(&mut self, position: usize, text: &str) -> bool {
        let Some(column) = self.session.index_columns.get_mut(position) else {
            return false;
        };
        let prefix = text.trim().parse::<u32>().ok().filter(|length| *length > 0);
        if column.prefix == prefix {
            return false;
        }
        column.prefix = prefix;
        self.session.intent = EditIntent::Apply;
        true
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
            .scroll_area_y()
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
            Section::Indexes => index_display_order(&snapshot.indexes)
                .into_iter()
                .map(|index| {
                    let row = &snapshot.indexes[index];
                    let reason = localize(&row.reason, locale);
                    let editable = self.index_editable(row);
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
                        id: format!("edit-index-{index}"),
                        label: text("structure.index.edit"),
                        action: RowAction::EditIndex(row.name.clone()),
                        disabled: editable.is_some(),
                        reason: editable,
                    });
                    menu.push(MenuEntry {
                        id: format!("drop-index-{index}"),
                        label: text("structure.index.delete"),
                        action: RowAction::DropIndex(row.name.clone()),
                        disabled: row.protected,
                        reason: (row.protected && !reason.is_empty()).then_some(reason.clone()),
                    });
                    RowSpec {
                        id: format!("indexes-{index}"),
                        dim: row.visible == Some(false),
                        cells: vec![
                            index_name_cell(index, row, &reason, p),
                            index_kind_cell(index, row, locale, p),
                            index_columns_cell(row, mono, p),
                            index_comment_cell(index, row, p),
                            index_visible_cell(row, locale, p),
                        ],
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
                        .min_w_0()
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

    /// 方言的索引能力（新建/修改索引表单按它隐藏不支持的项）。
    fn index_features(&self) -> crate::adapters::ddl::IndexFeatures {
        crate::adapters::ddl::ddl_named(&self.session.target.engine).map(|dialect| dialect.index_features()).unwrap_or_default()
    }

    /// 这个索引能不能在索引页修改；不能时给出原因的 i18n 键。
    /// 不能在页面上修改时给出原因（已按界面语言）。
    fn index_editable(&self, row: &crate::ddl::Index) -> Option<String> {
        let locale = self.session.locale;
        if row.partial || row.expression {
            return Some(i18n::text(locale, "structure.index.editExpr"));
        }
        // 能在一条语句里删了再加的方言（MySQL 家族）连主键与外键依赖的索引也能改；其余引擎要先删，受约束维护的不行。
        let single_statement = self.index_features().primary;
        if row.protected && !single_statement {
            return Some(i18n::text(locale, "structure.index.editLocked"));
        }
        // 先删后建只能按普通定义重建；带访问方法、INCLUDE、聚集等属性的索引会被悄悄改掉，不让改。
        if !single_statement && !row.rebuild_loses.is_empty() {
            return Some(i18n::text(locale, "structure.index.editLoses").replace("{props}", &row.rebuild_loses.join(", ")));
        }
        None
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
                    .min_w_0()
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

    /// 部分成功：只重新读回服务器现状，不重置草稿的起稿状态——用户填的草稿、
    /// 分区方案和视图定义都还在，能对照逐步回执改了再试。
    fn reread_after_partial(&mut self) {
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

    /// 同连接事务结束后重读结构快照：提交后的真实结构与回滚后的原结构都按数据库重新读取（R4、R5）。
    pub fn transaction_settled(&mut self, connection_id: &str) {
        if self.session.target.connection_id == connection_id {
            self.reload_after_execute();
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

    /// 底部“预览 SQL”按钮：已有表的字段页没有改动时不显示（只读浏览），只留一句说明。
    pub fn preview_button_visible(&self) -> bool {
        self.preview_action_visible() && !(self.session.section == Section::Columns && self.session.mode != StructureMode::Create && self.pending_changes() == 0)
    }

    fn apply_row_action(&mut self, action: RowAction, cx: &mut Context<Self>) {
        match action {
            RowAction::Copy(text) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
            RowAction::ToggleColumnDrop(index) => self.toggle_column(index, "drop"),
            RowAction::TogglePrimary(index) => self.toggle_column(index, "primary"),
            RowAction::RevertColumn(index) => self.revert_column(index),
            RowAction::AddColumn => self.add_column(),
            RowAction::EditIndex(name) => {
                self.begin_edit_index(&name);
            }
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
        self.draft_issues().into_iter().next().map(|issue| issue.text)
    }

    /// 全部草稿问题，带所在行与字段，界面就近显示在对应行下方（R2）。新建表有问题时预览与执行不可提交。
    pub fn draft_issues(&self) -> Vec<DraftIssue> {
        let locale = self.session.locale;
        let mut issues = Vec::new();
        if self.session.section != Section::Columns {
            return issues;
        }
        let create = self.session.mode == StructureMode::Create;
        if create {
            let name = self.session.table_name.trim();
            if name.is_empty() {
                issues.push(DraftIssue { row: None, field: IssueField::TableName, text: i18n::text(locale, "structure.diag.emptyTable") });
            } else if !valid_name(name) {
                issues.push(DraftIssue { row: None, field: IssueField::TableName, text: i18n::text(locale, "structure.diag.invalidTable") });
            }
        }
        let engine = DdlEngine::parse(&self.session.target.engine).ok();
        let mut seen = HashSet::new();
        for (row_idx, col) in self.session.kept_columns.iter().enumerate().filter(|(_, col)| !col.dropped) {
            let row = Some(row_idx);
            let trimmed = col.name.trim();
            if trimmed.is_empty() {
                let text = i18n::fill(&i18n::text(locale, "structure.diag.emptyCol"), &[("index", &(row_idx + 1).to_string())]);
                issues.push(DraftIssue { row, field: IssueField::Name, text });
                continue;
            }
            if !seen.insert(trimmed.to_lowercase()) {
                let text = i18n::fill(&i18n::text(locale, "structure.diag.duplicateCol"), &[("name", trimmed)]);
                issues.push(DraftIssue { row, field: IssueField::Name, text });
                continue;
            }
            let before = col
                .original
                .as_deref()
                .and_then(|orig| self.snapshot.as_ref().and_then(|snap| snap.columns.iter().find(|item| item.name == orig)));
            if let Some(eng) = engine {
                // 已有字段如果没动过类型，按原样放行（例如数据库读出的带显示宽度或自定义类型）。
                let untouched_type = before.is_some_and(|before| before.type_name.eq_ignore_ascii_case(col.type_name.trim()));
                if !untouched_type && !valid_type(eng, &col.type_name) {
                    let text = i18n::fill(&i18n::text(locale, "structure.diag.invalidType"), &[("name", trimmed), ("kind", col.type_name.trim())]);
                    issues.push(DraftIssue { row, field: IssueField::Type, text });
                    continue;
                }
                let untouched_default = before.is_some_and(|before| existing_default(eng, before.default_sql.as_deref()) == col.default);
                if !untouched_default && !(create && col.auto) {
                    if let Some(problem) = default_problem(eng, col) {
                        issues.push(DraftIssue { row, field: IssueField::Default, text: localize(&problem, locale) });
                        continue;
                    }
                }
            }
            if col.auto && !col.primary && !self.snapshot.as_ref().is_some_and(|snap| crate::ddl::plan::auto_keyed_elsewhere(snap, col, create)) {
                let text = i18n::fill(&i18n::text(locale, "structure.diag.autoNotPk"), &[("name", trimmed)]);
                issues.push(DraftIssue { row, field: IssueField::Auto, text });
                continue;
            }
            if self.session.mode == StructureMode::Existing
                && col.original.is_none()
                && !col.nullable
                && !col.default.has_value()
                && (self.snapshot.as_ref().and_then(|snap| snap.row_count).unwrap_or(0) > 0
                    || engine.and_then(crate::adapters::ddl::ddl_of).is_some_and(|dialect| dialect.add_not_null_without_default(trimmed).is_some()))
            {
                let text = i18n::fill(&i18n::text(locale, "structure.diag.notNullNeedsDefault"), &[("name", trimmed)]);
                issues.push(DraftIssue { row, field: IssueField::Default, text });
            }
        }
        issues
    }

    /// 新建表时草稿有问题：预览与执行都不可提交。
    pub fn create_blocked(&self) -> bool {
        self.session.mode == StructureMode::Create && self.session.section == Section::Columns && !self.draft_issues().is_empty()
    }

    /// 新建表或已有表字段草稿存在阻塞性校验错误时，禁用“预览 SQL”。
    pub fn preview_blocked(&self) -> bool {
        self.session.section == Section::Columns && !self.draft_issues().is_empty()
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
        if !self.preview_button_visible() {
            return Some(
                div()
                    .id("structure-actions")
                    .test_support()
                    .flex_none()
                    .flex()
                    .items_center()
                    .px(px(metrics.page_pad))
                    .py(px(kit::Space::MD))
                    .border_t_1()
                    .border_color(p.line_soft)
                    .bg(p.surface)
                    .child(div().id("structure-browse-hint").test_support().flex_1().min_w_0().child(kit::muted(t("structure.columns.browseHint"), p)))
                    .into_any_element(),
            );
        }
        let button = if section == Section::Danger {
            kit::danger_button("structure-preview", Some(IconName::Eye), t("structure.action.preview"), click)
                .loading(self.previewing)
                .tooltip(tip)
                .into_any_element()
        } else {
            kit::primary_button("structure-preview", Some(IconName::Eye), t("structure.action.preview"), click)
                .loading(self.previewing)
                .disabled(self.preview_blocked())
                .tooltip(tip)
                .into_any_element()
        };
        let status_node: AnyElement = if let Some(diag) = diagnostic {
            div()
                .id("structure-draft-diagnostic")
                .test_support()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(kit::icon(IconName::TriangleAlert, 13., p.warn))
                .child(div().min_w_0().truncate().text_size(px(TypeScale::SM)).text_color(p.warn).font_weight(FontWeight::MEDIUM).child(SharedString::from(diag)))
                .into_any_element()
        } else if pending > 0 {
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(kit::icon(IconName::CircleDot, 13., p.warn))
                .child(div().truncate().text_size(px(TypeScale::SM)).font_weight(FontWeight::MEDIUM).child(SharedString::from(breakdown)))
                .into_any_element()
        } else if let Some((no_pk, text)) = create_summary {
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(6.))
                .when(no_pk, |this| this.child(kit::icon(IconName::TriangleAlert, 13., p.warn)))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(px(TypeScale::SM))
                        .text_color(if no_pk { p.warn } else { p.text_muted })
                        .child(SharedString::from(text)),
                )
                .into_any_element()
        } else {
            div().flex_1().min_w_0().child(kit::muted(t("structure.action.hint"), p)).into_any_element()
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
                .border_color(p.line_soft)
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

/// 草稿问题所在的输入。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IssueField {
    TableName,
    Name,
    Type,
    Default,
    Auto,
}

/// 一条草稿问题：`row` 为 None 时属于表名。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftIssue {
    pub row: Option<usize>,
    pub field: IssueField,
    pub text: String,
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
        .min_w_0()
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
    TogglePrimary(usize),
    RevertColumn(usize),
    AddColumn,
    EditIndex(String),
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
        .border_color(p.line_soft)
        .text_size(px(TypeScale::XS))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(p.text_faint);
    for column in columns {
        head = head.child(table_cell(*column, div().truncate().child(SharedString::from(i18n::text(locale, column.key)))));
    }
    // 行尾快捷按钮：编辑（索引）与删除。各行按钮个数取最多的一行，表头留同样宽度，列才对得齐。
    let quick_of = |entries: &[MenuEntry]| {
        entries
            .iter()
            .filter(|entry| matches!(entry.action, RowAction::EditIndex(_) | RowAction::DropIndex(_) | RowAction::DropForeignKey(_)))
            .cloned()
            .collect::<Vec<_>>()
    };
    let quick_count = rows.iter().map(|row| quick_of(&row.menu).len()).max().unwrap_or(0);
    let quick_w = QUICK_W * quick_count as f32;
    if quick_count > 0 {
        head = head.child(div().w(px(quick_w)).flex_none());
    }
    let mut body = div().flex().flex_col();
    for (row_index, row) in rows.into_iter().enumerate() {
        let owner = entity.clone();
        let quick = quick_of(&row.menu);
        let entries = row.menu;
        let mut cells = div()
            .id(SharedString::from(format!("structure-row-cells-{}", row.id)))
            .flex()
            .flex_1()
            .min_w_0()
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
            .when(row_index % 2 == 1, |this| this.bg(p.zebra))
            .when(row.dim, |this| this.opacity(0.5))
            .hover(|style| style.bg(p.hover))
            .child(cells)
            .when(quick_count > 0, |this| {
                let mut slot = div().w(px(quick_w)).flex_none().flex().items_center().justify_end();
                for entry in quick {
                    let owner = entity.clone();
                    let action = entry.action.clone();
                    let icon = if matches!(entry.action, RowAction::EditIndex(_)) { IconName::SquarePen } else { IconName::Trash };
                    let button = kit::icon_button(
                        SharedString::from(format!("structure-quick-{}", entry.id)),
                        icon,
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
                    slot = slot.child(div().w(px(QUICK_W)).flex_none().flex().justify_center().child(button));
                }
                this.child(slot)
            });
        body = body.child(line);
    }
    div().flex().flex_col().child(head).child(body).into_any_element()
}

/// 行尾每个快捷按钮占的宽度。
const QUICK_W: f32 = 28.;

/// 界面上显示的索引类型。读取层没填类型（旧快照、测试夹具）时按主键原因与唯一标记推出。
fn display_kind(row: &crate::ddl::Index) -> IndexKind {
    if row.kind == IndexKind::Primary || (row.protected && row.reason == "由主键约束维护") {
        IndexKind::Primary
    } else {
        IndexKind::resolve(row.unique, row.kind)
    }
}

/// 索引的显示顺序：主键置顶，其余保持读取顺序。返回 `indexes` 的下标。
fn index_display_order(indexes: &[crate::ddl::Index]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..indexes.len()).collect();
    order.sort_by_key(|position| display_kind(&indexes[*position]) != IndexKind::Primary);
    order
}

/// 索引类型的显示键与配色。
fn index_kind_key(kind: IndexKind) -> &'static str {
    match kind {
        IndexKind::Primary => "structure.index.primaryBadge",
        IndexKind::Unique => "structure.index.uniqueBadge",
        IndexKind::Normal => "structure.index.normalBadge",
        IndexKind::Fulltext => "structure.index.fulltextBadge",
        IndexKind::Spatial => "structure.index.spatialBadge",
    }
}

/// 一个索引列在字段 chips 里的文字：列名、前缀长度与降序标记。
fn index_column_label(column: &crate::ddl::IndexColumn) -> String {
    let mut label = column.name.clone();
    if let Some(length) = column.prefix {
        label.push_str(&format!("({length})"));
    }
    if column.descending {
        label.push_str(" DESC");
    }
    label
}

fn index_name_cell(index: usize, row: &crate::ddl::Index, reason: &str, p: &Palette) -> AnyElement {
    let is_pk = display_kind(row) == IndexKind::Primary;
    let mut tip = row.name.clone();
    if row.protected && !reason.is_empty() {
        tip = format!("{tip}\n{reason}");
    }
    div()
        .id(SharedString::from(format!("structure-index-name-{index}")))
        .test_support()
        .flex()
        .items_center()
        .gap(px(kit::Space::SM))
        .min_w_0()
        .overflow_hidden()
        .tooltip_text(SharedString::from(tip))
        .when(row.protected, |this| {
            let icon = if is_pk { IconName::Key } else { IconName::Lock };
            this.child(
                div()
                    .id(SharedString::from(format!("structure-index-lock-{index}")))
                    .test_support()
                    .flex_none()
                    .child(kit::icon(icon, kit::IconSize::INLINE, if is_pk { p.warn } else { p.text_faint })),
            )
        })
        .child(div().flex_1().min_w_0().truncate().font_weight(FontWeight::MEDIUM).child(SharedString::from(row.name.clone())))
        .into_any_element()
}

fn index_kind_cell(index: usize, row: &crate::ddl::Index, locale: Locale, p: &Palette) -> AnyElement {
    let text = |key: &str| i18n::text(locale, key);
    let kind = display_kind(row);
    let (fg, bg) = match kind {
        IndexKind::Primary => (p.warn, p.warn_soft),
        IndexKind::Unique => (p.accent_text, p.accent_soft),
        IndexKind::Fulltext | IndexKind::Spatial => (p.info, p.accent_soft),
        IndexKind::Normal => (p.text_muted, p.hover),
    };
    let mut badges = div()
        .id(SharedString::from(format!("structure-index-type-{index}")))
        .test_support()
        .flex()
        .items_center()
        .gap(px(kit::Space::XS))
        .min_w_0()
        .overflow_hidden()
        .child(kit::pill(text(index_kind_key(kind)), fg, bg));
    if row.partial {
        badges = badges.child(kit::pill(text("structure.index.partialBadge"), p.info, p.accent_soft));
    } else if row.expression {
        badges = badges.child(kit::pill(text("structure.index.exprBadge"), p.info, p.accent_soft));
    }
    match row.method.clone().filter(|method| !method.trim().is_empty()) {
        Some(method) => badges.tooltip_text(SharedString::from(method)).into_any_element(),
        None => badges.into_any_element(),
    }
}

fn index_columns_cell(row: &crate::ddl::Index, mono: &SharedString, p: &Palette) -> AnyElement {
    if row.columns.is_empty() {
        return cell_mono(extract_index_target_expr(row.sql.as_deref()), mono, p);
    }
    let full = row.columns.iter().map(index_column_label).collect::<Vec<_>>().join(", ");
    let mut chips = div().flex().items_center().gap(px(kit::Space::XS)).min_w_0().overflow_hidden();
    for (position, column) in row.columns.iter().enumerate() {
        chips = chips.child(
            div()
                .flex()
                .flex_shrink(1.)
                .min_w_0()
                .items_center()
                .gap(px(kit::Space::XXS))
                .px(px(kit::Space::XS))
                .rounded(px(Radius::TAG))
                .bg(p.hover)
                .text_size(px(TypeScale::XS))
                .child(div().flex_none().text_color(p.text_faint).child(SharedString::from(format!("{}", position + 1))))
                .child(div().min_w_0().truncate().font_family(mono.clone()).text_color(p.text_muted).child(SharedString::from(index_column_label(column)))),
        );
    }
    div()
        .id(SharedString::from(format!("structure-index-cols-{}", row.name)))
        .min_w_0()
        .overflow_hidden()
        .tooltip_text(SharedString::from(full))
        .child(chips)
        .into_any_element()
}

fn index_comment_cell(index: usize, row: &crate::ddl::Index, p: &Palette) -> AnyElement {
    let comment = row.comment.clone().unwrap_or_default();
    if comment.trim().is_empty() {
        return cell_text("—".into(), p);
    }
    div()
        .id(SharedString::from(format!("structure-index-comment-{index}")))
        .min_w_0()
        .truncate()
        .text_color(p.text_muted)
        .tooltip_text(SharedString::from(comment.clone()))
        .child(SharedString::from(comment))
        .into_any_element()
}

fn index_visible_cell(row: &crate::ddl::Index, locale: Locale, p: &Palette) -> AnyElement {
    match row.visible {
        Some(true) => cell_text(i18n::text(locale, "structure.index.visibleYes"), p),
        Some(false) => kit::pill(i18n::text(locale, "structure.index.visibleNo"), p.warn, p.warn_soft),
        None => cell_text("—".into(), p),
    }
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
        .min_w_0()
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
        if self.template_manager.is_some() {
            self.template_manager = None;
            return true;
        }
        if self.session.preview_open() && !self.executing {
            self.session.cancel();
            return true;
        }
        if self.composer_open {
            self.composer_open = false;
            return true;
        }
        self.end_edit_row()
    }

    pub fn preview_enter(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.session.preview_open() || self.executing {
            return false;
        }
        // 生产环境禁止回车执行，避免误触；吞掉按键不向底层透传。
        if self.session.production() {
            return true;
        }
        if self.session.confirm_blocked() || self.session.preview_sql().join("\n").trim().is_empty() {
            return true;
        }
        self.confirm(cx);
        true
    }

    pub fn trigger_preview_shortcut(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.session.preview_open() && self.preview_button_visible() && !self.previewing {
            self.preview_current(cx);
            return true;
        }
        false
    }

    fn preview_current(&mut self, cx: &mut Context<Self>) {
        self.session.intent = EditIntent::Apply;
        if self.preview_blocked() {
            // 新建表或已有表字段草稿有就近标出的问题：不发起预览，也就不能执行。
            self.session.note = Note::Key("structure.diag.fixFirst");
            cx.notify();
            return;
        }
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
        let Some((before, engine)) =
            self.snapshot.as_ref().and_then(|snapshot| snapshot.columns.iter().find(|item| item.name == original).map(|item| (item, snapshot.engine)))
        else {
            return ColumnChange::Clean;
        };
        // ENUM/SET 候选值与生成列定义的变化用计划里的同一份判据，界面与 SQL 不会各说一套。
        let shape = self
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| crate::ddl::plan::shape_changed(snapshot, before, column));
        let changed = before.name != column.name
            || !before.type_name.eq_ignore_ascii_case(&column.type_name)
            || before.nullable != column.nullable
            || before.primary != column.primary
            || existing_default(engine, before.default_sql.as_deref()) != column.default
            || shape;
        if changed { ColumnChange::Modified } else { ColumnChange::Clean }
    }

    pub fn pending_changes(&self) -> usize {
        if self.session.mode == StructureMode::Create {
            return 0;
        }
        let changed = self.session.kept_columns.iter().filter(|column| self.column_change(column) != ColumnChange::Clean).count();
        // 只调了主键顺序时没有哪一列算“已改”，但仍是一处待应用的变更。
        if changed == 0 && self.primary_key_changed() { 1 } else { changed }
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
            || column.default.label().to_lowercase().contains(&needle)
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
                // 建表语句与随后的注释语句一起显示，和预览确认层里的步骤一致。
                (!plan.steps.is_empty()).then(|| plan.steps.iter().map(|step| step.sql.clone()).collect::<Vec<_>>().join("\n"))
            }
            (StructureMode::Existing, Section::Indexes) => {
                let snap = self.snapshot.as_ref()?;
                let plan = preview_plan(snap, &self.session.action());
                // 修改索引在不能一条语句完成的引擎上是删除 + 新建两条：两条都显示。
                (!plan.steps.is_empty()).then(|| plan.steps.iter().map(|step| step.sql.clone()).collect::<Vec<_>>().join("\n"))
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
        let widths = FieldWidths::of(content_w, create).with_comment(content_w, create && self.shows_column_comments());
        let mut editor = div().flex().flex_col().gap(px(metrics.section_gap()));
        let browse_cols = browse::BrowseColumns::of(content_w, self.show_all_columns);
        let hides_columns = !create && (self.show_all_columns || browse::BrowseColumns::hides_any(content_w));

        let mut toolbar_has_presets = false;
        let issues = self.draft_issues();
        if create {
            let name_placeholder = i18n::text(locale, "structure.create.tableNamePlaceholder");
            let name = self.draft_input_enter(4, &self.session.table_name.clone(), &name_placeholder, window, cx);
            let add_top_entity = entity.clone();
            let presets_btn = self.templates_button(&entity);
            let name_issue = create::table_issue(&issues)
                .filter(|_| !self.session.table_name.trim().is_empty() || self.session.kept_columns.len() > 1)
                .map(|issue| create::issue_line("structure-table-name-issue", &issue.text, &p));
            let mut create_card = kit::card(&p)
                .p(px(metrics.card_pad()))
                .flex()
                .flex_col()
                .gap(px(kit::Space::MD))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_end()
                        .gap(px(kit::Space::LG))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(200.))
                                .max_w(px(380.))
                                .flex()
                                .flex_col()
                                .child(kit::form_row(t("structure.action.createTable"), name, &p))
                                .children(name_issue),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(kit::Space::SM))
                                .child(presets_btn)
                                .child(kit::secondary_button("structure-add-column-top", Some(IconName::Plus), t("structure.field.addColumn"), move |_, _, cx| {
                                    add_top_entity.update(cx, |pane, cx| {
                                        pane.add_column();
                                        cx.notify();
                                    });
                                })),
                        ),
                );
            if let Some(options) = self.table_options_panel(&entity, &p, window, cx) {
                create_card = create_card.child(options);
            }
            editor = editor.child(create_card);
            editor = editor.child(kit::muted(t("structure.create.keyboardHint"), &p));
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
            let show_toolbar = total_cols >= 5 || !filter_val.is_empty() || self.only_changed_columns || pending > 0 || hides_columns;
            toolbar_has_presets = show_toolbar && content_w >= 560.;
            if show_toolbar {
                let clear_entity = entity.clone();
                let changed_entity = entity.clone();
                let add_top_entity = entity.clone();
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
                let presets_btn = self.templates_button(&entity);
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
                                .border_color(if !filter_val.is_empty() { p.accent } else { p.line_soft })
                                .bg(p.surface)
                                .flex()
                                .items_center()
                                .child(kit::input_prefix(IconName::Search, 13., p.text_faint))
                                .child(div().flex_1().min_w_0().child(kit::borderless_input("structure-column-filter", &filter_state, filter_placeholder)))
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
                        .when(hides_columns, |this| {
                            let owner = entity.clone();
                            this.child(
                                kit::tool_button("structure-show-all-columns", Some(IconName::Columns3), t("structure.columns.showAll"), move |_, _, cx| {
                                    owner.update(cx, |pane, cx| {
                                        pane.show_all_columns = !pane.show_all_columns;
                                        cx.notify();
                                    });
                                })
                                .selected(self.show_all_columns),
                            )
                        })
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

        // 主键条：PRIMARY KEY (a, b)，可拖动调整主键顺序；新建表与已有表都显示。
        editor = editor.child(self.primary_key_strip(&entity, &p, cx));

        if !create {
            // 已有表：先读后改。虚拟列表只渲染可见行，点哪一行就只编辑哪一行。
            let visible: Vec<usize> = self
                .session
                .kept_columns
                .iter()
                .enumerate()
                .filter(|(_, column)| self.column_matches_filter(column, self.column_change(column)))
                .map(|(index, _)| index)
                .collect();
            if visible.is_empty() && (!self.column_filter.trim().is_empty() || self.only_changed_columns) {
                let clear_owner = entity.clone();
                let clear_btn = kit::secondary_button("structure-clear-col-filter", None, t("structure.columns.filterClear"), move |_, _, cx| {
                    clear_owner.update(cx, |pane, cx| {
                        pane.clear_column_filter();
                        cx.notify();
                    });
                });
                return editor
                    .child(kit::card(&p).overflow_hidden().child(kit::no_match_state_with_action(
                        IconName::Search,
                        t("structure.columns.noMatch"),
                        t("structure.columns.noMatchHint"),
                        clear_btn,
                        &p,
                    )))
                    .into_any_element();
            }
            // 正在编辑的那一行下面挂 ENUM/SET 候选值与生成列定义：这两个形态需要多行输入，
            // 不塞进等高的一行里，免得挤掉整张字段表。
            let detail = self
                .editing_row
                .and_then(|index| self.session.kept_columns.get(index).cloned().map(|column| (index, column)))
                .and_then(|(index, column)| self.column_special_editor(index, &column, &engine, &p, window, cx));
            let browser = self.column_browser(visible, browse_cols, !toolbar_has_presets, window, cx);
            return match detail {
                Some(panel) => editor
                    .flex_1()
                    .min_h_0()
                    .child(div().flex().flex_col().flex_1().min_h_0().child(browser).child(panel))
                    .into_any_element(),
                None => editor.flex_1().min_h_0().child(browser).into_any_element(),
            };
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
            .border_color(p.line_soft)
            .text_size(px(TypeScale::XS))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(p.text_faint)
            .child(div().flex_1().min_w_0().overflow_hidden().pl(px(browse::NAME_INSET)).truncate().child(t("structure.field.column")))
            .child(div().w(px(widths.kind)).flex_none().child(t("structure.field.type")))
            .child(div().w(px(widths.nullable)).flex_none().flex().justify_center().child(t("ui.structure.nullable")))
            .child(div().w(px(widths.default)).flex_none().child(t("ui.structure.default")))
            .when(widths.comment > 0., |this| this.child(div().w(px(widths.comment)).flex_none().child(t("structure.field.comment"))))
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
                self.column_row(*index, column, *change, create, &engine, row_h, widths, None, &p, &entity, window, cx)
            }));
            if let Some(issue) = self.row_issue(&issues, *index, &p) {
                rows = rows.child(issue);
            }
            if let Some(panel) = self.column_special_editor(*index, column, &engine, &p, window, cx) {
                rows = rows.child(panel);
            }
        }
        let add_entity = entity.clone();
        let add = div()
            .id("structure-add-column-row")
            .flex()
            .items_center()
            .h(px(row_h + kit::Space::XS))
            .px(px(kit::Space::SM))
            .border_t_1()
            .border_color(p.line_soft)
            .bg(p.chrome)
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
                            .scroll_area_y()
                            .font_family(mono)
                            .text_size(px(TypeScale::SM))
                            .text_color(if live_sql.is_some() { p.text } else { p.text_faint })
                            .child(SharedString::from(live_sql.unwrap_or_else(|| {
                                let key = if self.session.table_name.trim().is_empty() { "structure.create.nameHint" } else { "structure.create.fixHint" };
                                i18n::text(locale, key)
                            }))),
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
        issue: Option<&str>,
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

        let name = self.draft_input_enter(10_000 + index as u32, &column.name, "name", window, cx);
        // ENUM/SET 的类型写法由候选值生成，不再让人在类型框里手写：这里只显示形态与候选值个数，
        // 候选值本身在行下面的面板里编辑。
        let kind: AnyElement = match column.enum_set.as_ref() {
            Some(state) => div()
                .id(SharedString::from(format!("structure-col-enum-{index}")))
                .flex()
                .items_center()
                .min_w_0()
                .truncate()
                .text_color(p.text_muted)
                .tooltip_text(t("structure.enumTypePreview"))
                .child(SharedString::from(format!("{} · {}", state.kind.keyword(), state.values.len())))
                .into_any_element(),
            None => self.draft_input_enter(20_000 + index as u32, &column.type_name, default_column_type(engine), window, cx),
        };
        // 已有字段的注释本轮只读（修改注释不在需求内）：编辑行里仍以文本显示，悬停看全文。
        let comment_input = (widths.comment > 0.).then(|| {
            if create {
                self.draft_input(40_000 + index as u32, &column.comment, "", window, cx)
            } else {
                let text = col_comment.clone().unwrap_or_else(|| column.comment.clone());
                div()
                    .id(SharedString::from(format!("structure-col-comment-{index}")))
                    .min_w_0()
                    .truncate()
                    .text_size(px(TypeScale::SM))
                    .text_color(p.text_faint)
                    .when(!text.is_empty(), |this| this.tooltip_text(SharedString::from(text.clone())))
                    .child(SharedString::from(text))
                    .into_any_element()
            }
        });

        let type_entity = entity.clone();
        let type_locked = orig_primary || orig_generated;
        let generated_flag = column.generated.is_some();
        let menu_engine = engine.to_owned();
        let current_type = column.type_name.clone();
        let type_menu = kit::inline_menu_button(SharedString::from(format!("structure-types-{index}")))
            .icon(IconName::ChevronDown)
            .disabled(type_locked)
            .tooltip(if orig_primary {
                t("structure.field.pkTypeLocked")
            } else if orig_generated {
                t("structure.field.genLocked")
            } else {
                t("structure.field.pickType")
            })
            .dropdown_menu(move |menu, _, _| {
                let entity = type_entity.clone();
                let pick_entity = entity.clone();
                let mut menu = create::type_menu(menu, &menu_engine, &current_type, locale, move |choice, cx| {
                    pick_entity.update(cx, |pane, cx| {
                        pane.write_draft(20_000 + index as u32, choice);
                        pane.pending_focus_draft = Some(20_000 + index as u32);
                        cx.notify();
                    });
                });
                // ENUM/SET 与生成列不在“常用类型”里：选中后这一行下面出现候选值或表达式面板。
                menu = menu.separator().label(i18n::text(locale, "structure.type.proprietary"));
                for (key, kind) in [("structure.type.enum", EnumSetKind::Enum), ("structure.type.set", EnumSetKind::Set)] {
                    let owner = entity.clone();
                    menu = menu.item(PopupMenuItem::new(i18n::text(locale, key)).on_click(move |_, _, cx| {
                        owner.update(cx, |pane, cx| {
                            pane.set_enum_set_kind(index, kind);
                            cx.notify();
                        });
                    }));
                }
                let owner = entity.clone();
                let label = if generated_flag { "structure.type.generatedOff" } else { "structure.type.generated" };
                menu.item(PopupMenuItem::new(i18n::text(locale, label)).on_click(move |_, _, cx| {
                    owner.update(cx, |pane, cx| {
                        pane.toggle_generated(index);
                        cx.notify();
                    });
                }))
            });

        let default_enabled = !orig_generated && !(create && column.auto) && change != ColumnChange::Dropped;
        let default_cell = self.default_cell(index, &column.default, column.nullable, default_enabled, engine, entity, p, window, cx);

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

        let row_menu_entries = self.column_menu_entries(index, column, change, create);

        let slot_icon: Option<(IconName, gpui_kit::Hsla, String)> = if let Some(issue) = issue {
            Some((IconName::TriangleAlert, p.danger, issue.to_owned()))
        } else if column.primary {
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
        let slot_icon_missing = slot_icon.is_none();
        // 联合主键时在名称旁标出这一列在主键里的序号。
        let pk_badge = self.primary_key_position(index).filter(|_| self.primary_key_order().len() > 1);
        let menu_owner = entity.clone();

        let mut row = div()
            .id(SharedString::from(format!("structure-row-columns-{index}")))
            .test_support()
            .w_full()
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
            .when(create, |this| {
                let accent = p.accent_soft;
                this.drag_over::<create::ColumnDrag>(move |style, _, _, _| style.bg(accent)).on_drop(cx.listener(move |pane, drag: &create::ColumnDrag, _, cx| {
                    pane.move_column_to(drag.index, index);
                    cx.notify();
                }))
            })
            .context_menu(move |menu, _, _| row_menu(&menu_owner, &row_menu_entries, menu))
            .when_some(bar, |this, color| {
                let indicator = div().absolute().left_0().top_0().bottom_0().w(px(3.)).bg(color);
                this.child(indicator)
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
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
                            .when_some(slot_icon.filter(|_| !create), |this, (ic, color, tip)| {
                                // 已有表编辑行：图标槽就是主键开关，点一下加入或移出主键。
                                let pk_owner = entity.clone();
                                let can_toggle = !dropped && !orig_generated;
                                let (ic, color) = if column.primary { (IconName::Key, p.warn) } else if issue.is_some() { (ic, color) } else { (IconName::Key, p.text_faint) };
                                let pk_tip = if column.primary { i18n::text(locale, "structure.pk.remove") } else { i18n::text(locale, "structure.pk.makePrimary") };
                                this.child(
                                    div()
                                        .id(SharedString::from(format!("structure-col-mark-{index}")))
                                        .test_support()
                                        .role(gpui_kit::Role::CheckBox)
                                        .aria_selected(column.primary)
                                        .aria_label(SharedString::from(pk_tip.clone()))
                                        .when(can_toggle, |this| this.cursor_pointer())
                                        .tooltip_text(SharedString::from(format!("{pk_tip}\n{tip}")))
                                        .when(can_toggle, |this| {
                                            this.on_click(move |_, _, cx| {
                                                cx.stop_propagation();
                                                pk_owner.update(cx, |pane, cx| {
                                                    pane.toggle_column(index, "primary");
                                                    cx.notify();
                                                });
                                            })
                                        })
                                        .child(kit::icon(ic, 12., color)),
                                )
                            })
                            .when(!create && slot_icon_missing, |this| {
                                let pk_owner = entity.clone();
                                let can_toggle = !dropped && !orig_generated;
                                this.child(
                                    div()
                                        .id(SharedString::from(format!("structure-col-mark-{index}")))
                                        .test_support()
                                        .role(gpui_kit::Role::CheckBox)
                                        .aria_selected(false)
                                        .aria_label(t("structure.pk.makePrimary"))
                                        .when(can_toggle, |this| this.cursor_pointer())
                                        .tooltip_text(t("structure.pk.makePrimary"))
                                        .when(can_toggle, |this| {
                                            this.on_click(move |_, _, cx| {
                                                cx.stop_propagation();
                                                pk_owner.update(cx, |pane, cx| {
                                                    pane.toggle_column(index, "primary");
                                                    cx.notify();
                                                });
                                            })
                                        })
                                        .child(kit::icon(IconName::Key, 12., p.text_faint)),
                                )
                            })
                            .when(create, |this| {
                                let drag = create::ColumnDrag { index, label: SharedString::from(column.name.clone()) };
                                let (icon, color) = if column.primary { (IconName::Key, p.warn) } else { (IconName::GripVertical, p.text_faint) };
                                this.child(
                                    div()
                                        .id(SharedString::from(format!("structure-col-grip-{index}")))
                                        .test_support()
                                        .cursor_grab()
                                        .tooltip_text(t("structure.field.dragHint"))
                                        .on_drag(drag, |drag, _, _, cx| cx.new(|_| create::ColumnDragGhost { label: drag.label.clone() }))
                                        .child(kit::icon(icon, 12., color)),
                                )
                            }),
                    )
                    .child(div().flex_1().min_w_0().when(dropped, |this| this.opacity(0.5).line_through()).child(name))
                    .when_some(pk_badge, |this, position| this.child(kit::pill(SharedString::from(format!("PK{position}")), p.warn, p.warn_soft)))
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
                    .child(div().flex_1().min_w_0().child(kind))
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
                        // 不可修改时提示框直接说明原因。
                        if column.primary {
                            t("structure.field.pkNullLocked")
                        } else if orig_generated {
                            t("structure.field.genLocked")
                        } else {
                            t("ui.structure.nullable")
                        },
                        !dropped && !column.primary && !orig_generated,
                    )),
            )
            .when(widths.default > 0., |this| {
                this.child(div().w(px(widths.default)).flex_none().when(dropped, |this| this.opacity(0.5)).child(default_cell))
            })
            .when_some(comment_input, |this, input| this.child(div().w(px(widths.comment)).flex_none().child(input)));

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
                        .child(div().flex_1().min_w_0().child(kit::muted(t("structure.view.readonlyColumns"), &p)))
                        .child(kit::secondary_button("structure-switch-view-def", Some(IconName::SquarePen), t("structure.view.editDef"), move |_, _, cx| {
                            edit_entity.update(cx, |pane, cx| {
                                pane.session.section = Section::View;
                                pane.session.cancel();
                                cx.notify();
                            });
                        })),
                )
            }
            Section::Columns if self.session.mode != StructureMode::Create => form.flex_1().min_h_0().child(self.column_editor(window, cx)),
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
                let features = self.index_features();
                let kind = IndexKind::resolve(unique, self.session.index_kind);
                // 主键在 MySQL 家族里固定叫 PRIMARY：选主键时不显示名称输入。
                let name = (kind != IndexKind::Primary).then(|| self.field("index_name", "structure.field.index", "idx_table_column", window, cx));
                let unique_entity = entity.clone();
                let auto_entity = entity.clone();
                let many_kinds = features.fulltext || features.spatial || features.primary;
                let kind_choice = many_kinds.then(|| {
                    let mut kinds = Vec::new();
                    if features.primary {
                        kinds.push(IndexKind::Primary);
                    }
                    kinds.extend([IndexKind::Unique, IndexKind::Normal]);
                    if features.fulltext {
                        kinds.push(IndexKind::Fulltext);
                    }
                    if features.spatial {
                        kinds.push(IndexKind::Spatial);
                    }
                    let items = kinds
                        .into_iter()
                        .map(|choice| {
                            let owner = entity.clone();
                            (
                                SharedString::from(format!("structure-index-kind-{choice:?}").to_lowercase()),
                                t(index_kind_key(choice)),
                                kind == choice,
                                kit::click(move |_, _, cx| {
                                    owner.update(cx, |pane, cx| {
                                        pane.choose_index_kind(choice);
                                        cx.notify();
                                    });
                                }),
                            )
                        })
                        .collect();
                    kit::form_row(t("structure.index.kind"), kit::segmented_sized(items, &p, metrics.control_h), &p)
                });
                form = form.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_end()
                        .gap(px(kit::Space::LG))
                        .when_some(name, |this, name| this.child(div().flex_1().min_w(px(ControlSize::FORM_MIN_W)).max_w(px(ControlSize::FORM_MAX_W)).child(name)))
                        .when(!self.index_name_auto && !selected.is_empty() && kind != IndexKind::Primary, |this| {
                            this.child(kit::tool_button("structure-index-autoname", Some(IconName::Sparkles), t("structure.index.autoName"), move |_, _, cx| {
                                auto_entity.update(cx, |pane, cx| {
                                    pane.apply_auto_index_name();
                                    cx.notify();
                                });
                            }))
                        })
                        .when_some(kind_choice, |this, choice| this.child(choice))
                        .when(!many_kinds, |this| this.child(
                            div()
                                .id("structure-index-unique")
                                .test_support()
                                .h(px(metrics.control_h))
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .cursor_pointer()
                                .child(check_box(SharedString::from("structure-index-unique-box"), unique, true, t("structure.index.unique"), &p))
                                .child(div().text_size(px(TypeScale::SM)).child(t("structure.index.unique")))
                                .on_click(move |_, _, cx| {
                                    unique_entity.update(cx, |pane, cx| {
                                        let next = if pane.session.index_unique { IndexKind::Normal } else { IndexKind::Unique };
                                        pane.choose_index_kind(next);
                                        cx.notify();
                                    });
                                }),
                        )),
                );
                let picker = self.ensure_index_picker(&names, window, cx);
                let ordered = self.index_order_list(&entity, &selected, kind, features.prefix, &p, window, cx);
                // 左右两栏：左边是可搜索的可选字段，右边是已选字段（可拖动排序，设方向与前缀长度）。
                // 两栏都在表单里原地展开，不弹浮层；底部的预览操作栏固定在滚动区之外。
                let selected_label = i18n::fill(&i18n::text(locale, "structure.index.selected"), &[("count", &selected.len().to_string())]);
                form = form.child(
                    div()
                        .id("structure-index-columns")
                        .test_support()
                        .w_full()
                        .flex()
                        .items_start()
                        .gap(px(kit::Space::LG))
                        .child(
                            div()
                                .id("structure-index-available")
                                .test_support()
                                .flex_none()
                                .w(px(ControlSize::PICKER_MIN_W))
                                .flex()
                                .flex_col()
                                .gap(px(kit::Space::XS))
                                .child(kit::eyebrow(t("structure.index.available"), &p))
                                .child(picker),
                        )
                        .child(
                            div()
                                .id("structure-index-selected")
                                .test_support()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap(px(kit::Space::XS))
                                .child(kit::eyebrow(SharedString::from(selected_label), &p))
                                .child(ordered),
                        ),
                );
                if features.comment {
                    let comment_value = self.session.index_comment.clone();
                    let placeholder = i18n::text(locale, "structure.index.commentHint");
                    let comment = self.draft_input(120_000, &comment_value, &placeholder, window, cx);
                    form = form.child(kit::form_row(t("structure.field.comment"), div().max_w(px(ControlSize::FORM_MAX_W)).child(comment), &p));
                }
                if self.index_name_conflict() && self.session.index_editing.is_none() {
                    form = form.child(create::issue_line(
                        "structure-index-name-conflict",
                        &i18n::fill(&i18n::text(locale, "structure.index.nameTaken"), &[("name", self.session.index_name.trim())]),
                        &p,
                    ));
                }
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
                    kit::menu_button("structure-fk-local")
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
                    kit::menu_button("structure-fk-tables")
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
                    kit::menu_button("structure-fk-ref-cols")
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
                                .child(div().flex_1().min_w_0().child(column))
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
                            .child(div().flex_1().min_w_0().child(schema))
                            .child(
                                kit::menu_button("structure-fk-schemas")
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
                                .child(div().flex_1().min_w_0().child(table))
                                .child(tables_button),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(160.))
                                .flex()
                                .items_end()
                                .gap(px(4.))
                                .child(div().flex_1().min_w_0().child(target))
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
                    kit::field_menu_button("structure-fk-on-delete")
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
                    kit::field_menu_button("structure-fk-on-update")
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
                            .border_color(if view_dirty { p.warn } else { p.line_soft })
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
                    .child(div().h(px(height)).rounded(px(Radius::CONTROL)).border_1().border_color(p.line_soft).overflow_hidden().child(viewer));
                if !missing.is_empty() {
                    let sep = match locale {
                        Locale::ZhCn => "、",
                        Locale::En => ", ",
                    };
                    let localized_items = missing.iter().map(|item| localize_missing_item(item, locale)).collect::<Vec<_>>().join(sep);
                    let notice = i18n::fill(&i18n::text(locale, "structure.meta.rebuiltHint"), &[("items", &localized_items)]);
                    form = form.child(
                        kit::sunken(&p)
                            .flex()
                            .items_start()
                            .gap(px(kit::Space::MD))
                            .px(px(kit::Space::LG))
                            .py(px(kit::Space::MD))
                            .text_size(px(TypeScale::XS))
                            .text_color(p.text_muted)
                            .child(kit::icon(IconName::Info, kit::IconSize::INLINE, p.text_muted))
                            .child(div().flex_1().min_w_0().child(SharedString::from(notice))),
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
                                                .scroll_area_y()
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
            Section::Partitions => div().child(self.partition_form(window, cx)),
            Section::Danger => stack_frame(|| {
                let mut form = form;
                let view = self.session.object_kind == "view";
                let back_owner = entity.clone();
                let mut mode_bar = div().flex().flex_wrap().items_center().gap(px(kit::Space::SM));
                if !view {
                    let danger_items = [
                        (DangerKind::Rename, "catalog.tree.renameTable"),
                        (DangerKind::Truncate, "catalog.tree.truncateTable"),
                        (DangerKind::Drop, "catalog.tree.dropTable"),
                    ]
                    .into_iter()
                    .map(|(kind, key)| {
                        let owner = entity.clone();
                        let active = self.session.danger == kind;
                        (
                            SharedString::from(format!("structure-danger-kind-{kind:?}")),
                            t(key),
                            active,
                            kit::click(move |_, _, cx| {
                                owner.update(cx, |pane, cx| {
                                    pane.session.danger = kind;
                                    pane.session.cancel();
                                    cx.notify();
                                });
                            }),
                        )
                    })
                    .collect();
                    mode_bar = mode_bar.child(kit::segmented_sized(danger_items, &p, metrics.control_h));
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
                                .min_w_0()
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
                if std::mem::take(&mut self.pending_focus_select) {
                    self.draft_fields.focus_select_all(key, window, cx);
                } else {
                    self.draft_fields.focus(key, window, cx);
                }
            }
        }
        if std::mem::take(&mut self.pending_focus_pane) {
            let handle = self.focus_handle(cx);
            window.focus(&handle, cx);
        }
        let p = Palette::of(cx);
        let metrics = kit::Metrics::of(cx);
        let section = self.session.section;
        let entity = cx.entity();
        let preview = if self.shell_overlay_host {
            None
        } else {
            self.render_preview_dialog(&entity, &p, metrics, section, window, cx)
        };
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
        let mut tabs_track = div()
            .flex()
            .flex_none()
            .items_center()
            .p(px(kit::Space::XXS))
            .gap(px(kit::Space::XXS))
            .rounded(px(Radius::CONTROL))
            .bg(p.sunken);
        for value in self.session.visible_sections() {
            if value == Section::Danger && section != Section::Danger {
                continue;
            }
            let key = match value {
                Section::Columns => "structure.section.columns",
                Section::Indexes => "structure.section.indexes",
                Section::ForeignKeys => "structure.section.foreign",
                Section::Partitions => "structure.section.partitions",
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
            tabs_track = tabs_track.child(
                kit::interactive_states(div().id(SharedString::from(key)), &p, false)
                    .test_support()
                    .role(gpui_kit::Role::Tab)
                    .aria_label(t(key))
                    .aria_selected(selected)
                    .flex_none()
                    .whitespace_nowrap()
                    .h(px(metrics.control_h))
                    .px(px(kit::Space::MD))
                    .flex()
                    .items_center()
                    .gap(px(kit::Space::SM))
                    .rounded(px(Radius::CONTROL))
                    .text_size(px(TypeScale::SM))
                    .when(selected, |this| {
                        this.bg(if danger {
                            p.danger_soft
                        } else if p.dark {
                            p.raised
                        } else {
                            p.surface
                        })
                        .text_color(if danger { p.danger } else { p.text })
                        .font_weight(FontWeight::MEDIUM)
                        .shadow_sm()
                    })
                    .when(!selected, |this| {
                        this.text_color(p.text_muted).hover(|style| style.text_color(p.text))
                    })
                    .child(t(key))
                    .when_some(count, |this, count| {
                        this.child(
                            div()
                                .px(px(kit::Space::XS))
                                .rounded(px(Radius::TAG))
                                .bg(if dirty_dot {
                                    p.warn_soft
                                } else if selected {
                                    p.accent_soft
                                } else {
                                    p.hover
                                })
                                .text_size(px(TypeScale::XS))
                                .text_color(if dirty_dot {
                                    p.warn
                                } else if selected {
                                    p.accent_text
                                } else {
                                    p.text_muted
                                })
                                .font_features(kit::tabular_figures())
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
        let tabs = div()
            .id("structure-tabs")
            .test_support()
            .role(gpui_kit::Role::TabList)
            .w_full()
            // 滚动条例外：页签栏单行横向滚动，不占高度绘制滚动条。
            .overflow_x_scroll()
            .flex()
            .items_center()
            .child(tabs_track);

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

        let effective_w = if self.content_width > 0. { self.content_width } else { f32::from(window.viewport_size().width) };
        let compact_header = effective_w > 0. && effective_w < 680.;

        let more = (self.session.mode != StructureMode::Create && self.session.visible_sections().contains(&Section::Danger)).then(|| {
            let owner = entity.clone();
            let view = self.session.object_kind == "view";
            let query_sink = self.query_sink.clone();
            let target_for_menu = self.session.target.clone();
            let qualified_for_menu = qualified.clone();
            let ddl_for_menu = self.snapshot.as_ref().map(|snap| definition_text(snap).0).unwrap_or_default();
            let btn = kit::menu_button("structure-more")
                .icon(IconName::Ellipsis);
            let btn = if compact_header {
                btn.tooltip(t("structure.action.more"))
            } else {
                btn.label(t("structure.action.more"))
            };
            btn.dropdown_menu(move |mut menu, _, cx| {
                    let pal = Palette::of(cx);
                    menu = menu.min_w(px(200.));
                    if let (Some(sink), Some(sql)) = (query_sink.clone(), select_sample_sql(&target_for_menu)) {
                        let tgt = target_for_menu.clone();
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
                    if let Some(dialect) = crate::adapters::ddl::ddl_of(snap.engine) {
                        for label in dialect.table_option_labels(snap) {
                            pills.push(kit::pill(label, p.text_muted, p.hover));
                        }
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
                .min_w_0()
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
                .min_w_0()
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
                            .min_w_0()
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
            .on_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, window, cx| {
                let key = event.keystroke.key.as_str();
                if key == "escape" && this.handle_escape() {
                    let handle = this.focus_handle(cx);
                    window.focus(&handle, cx);
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                if key == "enter"
                    && this.session.preview_open()
                    && !event.keystroke.modifiers.control
                    && !event.keystroke.modifiers.platform
                    && !event.keystroke.modifiers.alt
                    && !event.keystroke.modifiers.shift
                {
                    if this.preview_enter(cx) {
                        cx.stop_propagation();
                        cx.notify();
                    }
                    return;
                }
                if this.handle_create_key(event, window, cx) {
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                if this.handle_browse_key(event, window, cx) {
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
                    .pb(px(kit::Space::SM))
                    .flex()
                    .flex_col()
                    .gap(px(kit::Space::MD))
                    .bg(p.surface)
                    .border_b_1()
                    .border_color(p.line_soft)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(kit::engine_badge(&self.session.target.engine, 28.))
                            .child(div().flex_1().min_w_0().flex().flex_col().child(title_row).child(banner_row))
                            .children(refresh_btn)
                            .when_some(
                                self.data_sink
                                    .clone()
                                    .filter(|_| self.session.mode != StructureMode::Create && !self.session.target.table.is_empty()),
                                |this, sink| {
                                    if compact_header {
                                        this.child(
                                            kit::icon_button("structure-open-data", IconName::Table, t("ui.table.switchData"), move |_, _, cx| {
                                                sink(cx);
                                            }),
                                        )
                                    } else {
                                        this.child(kit::secondary_button("structure-open-data", Some(IconName::Table), t("ui.table.switchData"), move |_, _, cx| {
                                            sink(cx);
                                        }))
                                    }
                                },
                            )
                            .children(more),
                    )
                    .child(tabs),
            );

        let mut body = div().flex().flex_col().gap(px(metrics.section_gap()));
        let readonly_view = self.session.object_kind == "view" && self.session.mode != StructureMode::Create;
        let editing_columns = section == Section::Columns && self.form_visible() && !readonly_view;
        // 已有表的字段页用虚拟列表，自己滚动：外层不再滚动，列表占满剩余高度。
        let virtual_columns = editing_columns && self.session.mode != StructureMode::Create;
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
                                // 关掉“编辑索引”后再打开是新建：不把被编辑索引的草稿带过去。
                                if !pane.composer_open && pane.session.index_editing.is_some() {
                                    pane.reset_composer_drafts();
                                    pane.index_picker = None;
                                }
                                pane.session.cancel();
                                cx.notify();
                            });
                        },
                    )),
                );
            }
            let ddl_direct = section == Section::Ddl && form.is_some() && self.has_definition();
            let danger_direct = section == Section::Danger && form.is_some() && !self.loading && self.load_error.is_empty();
            // 分区方案是一张表单，不需要先铺一张定义卡：直接给表单，读回来的现状写在表单顶部。
            let partitions_direct = section == Section::Partitions && form.is_some() && !self.loading && self.load_error.is_empty();
            let mut form = form;
            if ddl_direct || danger_direct || partitions_direct {
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
                    Section::Indexes => match &self.session.index_editing {
                        Some(name) => SharedString::from(i18n::fill(&i18n::text(locale, "structure.index.editing"), &[("name", name)])),
                        None => t("structure.index.new"),
                    },
                    Section::ForeignKeys => t("structure.fk.new"),
                    Section::Danger => t("structure.section.danger"),
                    Section::Partitions => t("structure.section.partitions"),
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
                kit::sunken(&p)
                    .px(px(kit::Space::LG))
                    .py(px(kit::Space::MD))
                    .text_size(px(TypeScale::SM))
                    .text_color(p.text_muted)
                    .child(SharedString::from(self.session.note())),
            );
        }
        // 逐步回执：成功、部分成功、被拦下都逐条列出 label、SQL、状态与原始 message。
        if let Some(receipt) = self.session.receipt() {
            body = body.child(receipt::panel("structure", receipt, &p, self.session.locale));
        }
        let body = if virtual_columns {
            div()
                .id("structure-body")
                .test_support()
                .flex_1()
                .min_h_0()
                .p(px(metrics.page_pad))
                .flex()
                .flex_col()
                .child(body.flex_1().min_h_0())
                .into_any_element()
        } else {
            div()
                .id("structure-body")
                .test_support()
                .flex_1()
                .min_h_0()
                .scroll_area_y()
                .p(px(metrics.page_pad))
                .flex()
                .flex_col()
                .child(body)
                .into_any_element()
        };
        column = column.child(body);
        if let Some(actions) = self.preview_footer(&entity, &p, locale, metrics) {
            column = column.child(actions);
        }
        if let Some(preview) = preview {
            column = column.child(preview);
        }
        if !self.shell_overlay_host {
            if let Some(manager) = self.render_template_manager(&entity, &p, metrics, window, cx) {
                column = column.child(manager);
            }
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
            self.preview_focused = false;
            return None;
        }
        let preview_focus = self.preview_focus_handle(cx);
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
        let prod_danger_input = section == Section::Danger && self.session.production();
        let danger_field = if prod_danger_input {
            Some(kit::form_row(
                t("structure.danger.typeName"),
                self.draft_input(6, &self.session.danger_name.clone(), &self.session.target.table.clone(), window, cx),
                p,
            ))
        } else {
            None
        };
        if !self.preview_focused {
            self.preview_focused = true;
            if prod_danger_input && self.draft_fields.contains(6) {
                self.draft_fields.focus(6, window, cx);
            } else {
                window.focus(&preview_focus, cx);
            }
        }
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
                        pane.pending_focus_pane = true;
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
                        pane.pending_focus_pane = true;
                        cx.notify();
                    }
                })
            }
        };
        let scrim_entity = entity.clone();
        let key_entity = entity.clone();
        let copy_entity = entity.clone();
        let copy_sql_text = sql_raw.clone();
        let copied = self.preview_copied;
        let steps_heading = SharedString::from(i18n::fill(
            &i18n::text(locale, "structure.preview.steps"),
            &[("count", &steps.len().to_string())],
        ));

        let dialog = div()
            .id("structure-preview-scrim")
            .test_support()
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
                        pane.pending_focus_pane = true;
                        cx.notify();
                    }
                });
            })
            .child(
                kit::modal_surface("structure-preview-dialog", preview_title.clone(), kit::ModalWidth::PREVIEW, p)
                    .test_support()
                    .track_focus(&preview_focus)
                    .focus_trap("structure-preview-dialog", &preview_focus)
                    .on_key_down(move |event: &gpui_kit::KeyDownEvent, window, cx| {
                        let key = event.keystroke.key.as_str();
                        if key == "escape" {
                            let closed = key_entity.update(cx, |pane, cx| {
                                if pane.handle_escape() {
                                    let handle = pane.focus_handle(cx);
                                    window.focus(&handle, cx);
                                    cx.notify();
                                    true
                                } else {
                                    false
                                }
                            });
                            if closed {
                                cx.stop_propagation();
                            }
                            return;
                        }
                        if key == "enter"
                            && !event.keystroke.modifiers.control
                            && !event.keystroke.modifiers.platform
                            && !event.keystroke.modifiers.alt
                            && !event.keystroke.modifiers.shift
                        {
                            let handled = key_entity.update(cx, |pane, cx| {
                                let ok = pane.preview_enter(cx);
                                if ok {
                                    cx.notify();
                                }
                                ok
                            });
                            if handled {
                                cx.stop_propagation();
                            }
                        }
                    })
                    .overflow_hidden()
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
                            .child(div().flex_1().min_w_0())
                            .child(
                                div()
                                    .id("structure-preview-object")
                                    .test_support()
                                    .min_w_0()
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
                            .min_h_0()
                            .scroll_area_y()
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
                                            kit::sunken(p)
                                                .id("structure-preview-sql")
                                                .max_h(px(240.))
                                                .scroll_area_y()
                                                .p(px(kit::Space::LG))
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
                                        (p.text_muted, p.sunken, IconName::Info)
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
                                            .child(div().flex_1().min_w_0().child(SharedString::from(text))),
                                    );
                                }
                                this.child(list)
                            })
                            .when(!rule.is_empty() && !nothing_to_run, |this| this.child(kit::muted(SharedString::from(rule), p)))
                            .when(!blocked_text.is_empty(), |this| {
                                let (fg, bg) = if only_unchanged { (p.text_muted, p.sunken) } else { (p.danger, p.danger_soft) };
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
                                            .child(div().flex_1().min_w_0().child(SharedString::from(note_text))),
                                    )
                                } else {
                                    this.child(kit::muted(SharedString::from(note_text), p))
                                }
                            })
                            // 部分成功时预览不收起：逐步回执就显示在这张卡片里，一眼看清哪一步没过。
                            .when_some(self.session.receipt(), |this, receipt| {
                                this.child(receipt::panel("structure-dialog", receipt, p, locale))
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

mod browse;
mod create;
mod primary_key;
pub(crate) mod receipt;
mod special;

#[cfg(test)]
mod tests;

// ---------------------------------------------------------------- 结构事务回执（ADR-5）

#[cfg(test)]
mod transaction_note_tests {
    use super::*;

    fn executed(applied_not_committed: bool, committed: bool, implicit_commit: bool) -> crate::application::ddl::Executed {
        crate::application::ddl::Executed {
            receipt: crate::ddl::exec::Receipt {
                blocked: Vec::new(),
                rule: String::new(),
                steps: Vec::new(),
                target: String::new(),
                rebuild: false,
            },
            effect: crate::transaction::TransactionEffect::StillActive,
            transaction: None,
            implicit_commit,
            applied_not_committed,
            committed,
            transaction_note: None,
        }
    }

    #[test]
    fn the_structure_receipt_states_apply_commit_and_implicit_commit() {
        assert_eq!(execution_note(&executed(true, false, false)), Note::Key("structure.execute.appliedNotCommitted"));
        assert_eq!(execution_note(&executed(false, true, false)), Note::Key("structure.execute.committed"));
        assert_eq!(execution_note(&executed(false, false, true)), Note::Key("structure.execute.implicitCommit"));
        assert_eq!(execution_note(&executed(false, false, false)), Note::Key("structure.preview.done"));
    }
}
