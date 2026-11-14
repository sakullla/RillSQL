//! 共享虚拟网格。DataTable 只画可见窗口；选择和已读取/全量范围留在 session。

mod record;
mod session;
pub use session::group_digits;

use std::ops::DerefMut;
use std::sync::{Arc, OnceLock};

pub use session::{operation_banner, prove_single_table, GridMode, GridTarget, QuerySheet, SharedGrid};

use gpui_kit::assets::IconName;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::table::{Column, DataTable, TableDelegate, TableEvent, TableSelection, TableState};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _};
use gpui_kit::{
    App, AppContext as _, ClipboardItem, Context, Entity, FocusHandle, Focusable as _, InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, ParentElement as _, Pixels, Render, SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div, px,
    relative, DragMoveEvent,
};

use crate::ui::kit::{self, Palette, Tip as _, Radius, TypeScale};
use crate::ui::{EditMode, FormatViewer, FormatViewerEvent, TruncationState, ViewerSpec};
use gpui_kit::prelude::FluentBuilder as _;

use crate::application::AppServices;
use crate::grid::commands::{CountDto, CountRequest, FilterArg, PageDto, PageRequest, SchemaDto, TableRef};
use crate::i18n::{self, Locale};

use session::{
    cell_of, columns_from_schema, rows_from_page, schema_from_dto, BatchReject, CellPhase, Continuation, CopyBlock, CopyFormat, CopyScope, FindScope,
    ForeignKeyInfo, FullRead, Gate, GridAction, Reference, SheetEmptiness,
};

// Tab / Shift+Tab 在网格里是「下一格 / 上一格」，不是焦点遍历：绑定在网格自己的 key context 上，
// 才比外壳的 Tab（焦点遍历）更靠近焦点、先拿到这个键；表格自己有同名绑定，优先级更高。
gpui_kit::actions!(rillsql_grid, [GridNextCell, GridPrevCell]);

/// 外键跳转要打开的引用表工作项：目标身份和定位查询一起交给外壳，由它真正打开并定位。
/// 网格只算出目标，不自己开工作项，也不把这段 SQL 当作"已经跳过去"。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GridNavigation {
    pub connection_id: String,
    pub connection_name: String,
    pub database: String,
    pub schema: String,
    pub table: String,
    pub engine: String,
    pub sql: String,
}

/// 外键跳转的出口签名：外壳注入，网格只调用。
pub type NavigationSink = Arc<dyn Fn(GridNavigation, &mut App)>;

/// 查看阶段里的 Preview 不等于截断。完整原值保持可编辑；真预览在读完原值前锁定。
fn truncation_for_open_cell(phase: CellPhase, truncated: bool, total_bytes: u64, can_load_full: bool, error: &str) -> TruncationState {
    match phase {
        CellPhase::Full => TruncationState::Full,
        CellPhase::Loading => TruncationState::Loading,
        CellPhase::Error => TruncationState::Error(error.to_owned()),
        CellPhase::Preview if truncated => TruncationState::Preview { total_bytes, can_load_full },
        CellPhase::Preview => TruncationState::Full,
    }
}

/// 来源状态快照：(选择锚点/焦点, 可见首行, 是否有暂存, 查看目标的 (行 id, 列名))。
#[cfg(test)]
pub type GridStateSnapshot = (Option<(String, String)>, usize, bool, Option<(String, String)>);

const RECORD_SIDE_DEFAULT: f32 = 340.;
const RECORD_SIDE_MIN: f32 = 260.;
const RECORD_SIDE_MAX: f32 = 640.;

impl GridNavigation {
    /// 工作项里的对象规格：连接|连接名|库|模式||表|引擎|环境。
    pub fn spec(&self, environment: &str) -> String {
        format!(
            "{}|{}|{}|{}||{}|{}|{}",
            self.connection_id, self.connection_name, self.database, self.schema, self.table, self.engine, environment
        )
    }
}

/// 左键按下时的意图。拖拽沿用按下时的意图：Shift 拉大主矩形，Ctrl/Cmd 长出新区域。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PressMode {
    Plain,
    Extend,
    Toggle,
}

pub struct GridPane {
    session: SharedGrid,
    services: Arc<AppServices>,
    table: Entity<TableState<SheetDelegate>>,
    locale: Locale,
    search_hint_locale: Locale,
    filter_hint_locale: Locale,
    scheduled: u64,
    search_text: String,
    /// 编辑栏使用正式文本输入：光标、选区、粘贴、撤销和输入法都由它处理。
    edit_input: Entity<InputState>,
    /// 正在编辑的格子用稳定行 id 记住，排序或翻页后仍写回同一行。
    edit_target: Option<EditTarget>,
    /// 编辑框处理完 Enter 后，同一次按键还会冒泡到网格；这里消费掉那一次回声。
    suppress_enter: bool,
    /// 编辑栏要等下一次绘制才能拿到 Window，先把请求记下来。
    pending_edit: Option<PendingEdit>,
    notice: String,
    grid_focus: FocusHandle,
    /// 编辑栏关闭后把键盘焦点还回网格，Esc 之后方向键和 F2 仍然可用。
    refocus: bool,
    loading_more: bool,
    /// 左键按下的格子；拖拽期间用它作为起点，松开、失焦、调整列宽或滚动时结束。
    press: Option<(usize, usize)>,
    /// 按下时的意图：普通替换、Shift 扩展、Ctrl/Cmd 增减非连续区域。拖拽按它分派。
    press_mode: PressMode,
    /// 我们写回表格焦点时发出的 SelectCell 回声（显示列号含行号列）。
    /// 事件是异步送达的，一次选择更新可能连发几条；只有认得出的回声才能被忽略。
    echo_cells: Vec<(usize, usize)>,
    /// 上一次绘制的可见首行；滚动换行后不能继续把指针下的格子当成同一段拖选。
    visible_start: usize,
    /// 表格为行号列进入了行/列模式：事件回调里不能重入表格，下一帧绘制前把选择拉回会话。
    table_resync: bool,
    /// 上一帧键盘焦点是否在网格里；从"在"变成"不在"才算失焦，程序化拖选不受影响。
    focus_inside: bool,
    /// 首屏或替换读取尚未返回；交换面板据此禁止把旧快照当成当前表。
    fetching: bool,
    schema_cache: Option<(GridTarget, Option<Arc<crate::connections::SessionHealth>>, SchemaDto)>,
    next_offset: u64,
    counting: Option<u64>,
    count_sequence: u64,
    count_scope: Option<CountRequest>,
    /// 正在请求的翻页偏移。偏移在结果读回来之前不改，失败重试仍是同一页。
    pending_page: Option<u64>,
    /// 筛选条每一行的取值输入（值、上界/集合）；结构变化时整体重建。
    filter_inputs: Vec<FilterRowInputs>,
    filter_subscriptions: Vec<gpui_kit::Subscription>,
    search: Option<Entity<InputState>>,
    /// 换表后清掉查找框里的旧文字，避免输入框和实际筛选条件不一致。
    clear_search: bool,
    /// 已经发起、等着回填的完整值读取；成功回来后重跑同一条命令，让各入口共用同一个原值。
    /// 除了命令 id 还记住当时的快照代次：目标或行列换了就不再重跑。
    pending_full: Option<PendingFull>,
    /// 外键跳转的出口：由外壳（直接消费者）注入，网格只负责生成目标和定位查询。
    navigation: Option<NavigationSink>,
    /// 行头双击等表格事件里不能重入，绘制前再打开完整值查看。
    pending_viewer: bool,
    /// 右侧纵向记录视图是否打开；它始终显示网格的焦点行。
    record_open: bool,
    /// 记录面板宽度。默认 340，拖拽限制在 260..=640，双击复位。
    side_width: f32,
    record_dragging: bool,
    /// 记录面板里正在就地暂存的列；非模态，值直接变成字段行里的输入框。
    record_edit: Option<usize>,
    /// 记录面板的文本输入：Enter 暂存，Esc 取消。
    record_input: Entity<InputState>,
    /// 记录面板的就地暂存要等下一次绘制才能拿到 Window。
    pending_record: Option<(usize, usize)>,
    /// 点到还是预览信封的记录字段：记下这一列，按定位读到原值后再回到同一条暂存入口。
    record_edit_deferred: Option<usize>,
    _record_subscription: gpui_kit::Subscription,
    /// 单元格查看和记录面板共用的格式工作台。预览态不能提交。
    format_viewer: Option<Entity<FormatViewer>>,
    format_subscription: Option<gpui_kit::Subscription>,
    /// 行 id、列名、阶段、语言。阶段没变就不重建，避免清掉编辑草稿。
    format_stamp: Option<(String, String, CellPhase, Locale)>,
    /// 表头菜单"自适应列宽"要用窗口量字宽，绘制前再执行（逻辑列）。
    pending_fit: Option<usize>,
    /// "选区设值"要拿到 Window 才能聚焦输入，绘制时再打开编辑栏。
    pending_batch: bool,
    search_subscription: Option<gpui_kit::Subscription>,
    _subscription: gpui_kit::Subscription,
    _edit_subscription: gpui_kit::Subscription,
}

/// 等回填时要重跑的命令，以及发起时的快照代次。
#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingFull {
    id: String,
    token: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct EditTarget {
    row_id: String,
    column: usize,
    original: Option<Option<String>>,
    /// 非空时是"选区设值"：输入的值作为一批暂存到这些格子（行 id、逻辑列）。
    cells: Vec<(String, usize)>,
}

/// 一条筛选条件的两个取值输入；null/notnull/empty 不用它们。
#[derive(Clone, Default)]
struct FilterRowInputs {
    value: Option<Entity<InputState>>,
    second: Option<Entity<InputState>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingEdit {
    row: usize,
    column: usize,
    select_all: bool,
}

/// 哪些命令需要原值：它们的每一格都会先走同一条共享读取。
fn full_action(id: &str) -> Option<GridAction> {
    Some(match id {
        "grid-value-copy" => GridAction::CopyCell,
        "copy-region" => GridAction::CopyRegion,
        "copy-sql-value" => GridAction::CopySqlValues,
        "grid-copy-row" => GridAction::CopyRow,
        "grid-copy-column" => GridAction::CopyColumn,
        "grid-copy-where" => GridAction::WhereCell,
        "grid-copy-row-where" => GridAction::WhereRow,
        "grid-copy-column-where" => GridAction::WhereColumn,
        "edit-cell" => GridAction::Edit,
        "grid-filter-value" => GridAction::FilterValue,
        "grid-fk-jump" | "grid-fk-select" => GridAction::Reference,
        "grid-full-values" => GridAction::ExportValues,
        // 生成的 SELECT/UPDATE 用整行 WHERE，预览信封先读成原值。
        "grid-sql-select" | "grid-sql-update" => GridAction::WhereRow,
        // 复制为新行要拿到焦点行每一格的原值。
        "grid-clone-row" => GridAction::CopyRow,
        other if CopyFormat::from_command(other).is_some() => GridAction::CopyRegion,
        _ => return None,
    })
}

impl GridPane {
    pub fn new(services: Arc<AppServices>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let table = cx.new(|cx| {
            TableState::new(SheetDelegate::default(), window, cx)
                .cell_selectable(true)
                .row_selectable(true)
                // 列头也能选：整列同样是一个 anchor/focus 矩形，高亮与复制、统计同源。
                .col_selectable(true)
                .col_resizable(true)
                .row_header(true)
        });
        // 表格事件与我们的会话状态互相回声，apply_table_event 只接受真正改变了目标的事件。
        let subscription = cx.subscribe(&table, |this, _, event: &TableEvent, cx| {
            this.apply_table_event(event);
            cx.notify();
        });
        let owner = cx.entity().downgrade();
        table.update(cx, |state, _| state.delegate_mut().owner = Some(owner));
        let edit_input = cx.new(|cx| InputState::new(window, cx));
        table.update(cx, |state, _| state.delegate_mut().edit_input = Some(edit_input.clone()));
        // 记录面板复用同一套"Enter 提交、Esc 取消"：值是暂存到行上的字段，不经过弹窗。
        let record_input = cx.new(|cx| InputState::new(window, cx));
        let record_subscription = cx.subscribe(&record_input, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.suppress_enter = true;
                this.finish_record_edit(cx);
            }
        });
        let grid_focus = cx.focus_handle().tab_stop(true);
        // 只在网格自己的 key context 里把 Tab 解释成「下一格」；走到首格/末格时再交还外壳。
        // 同一条绑定重复注册不会改变行为，多个网格共存也只有一种解释。
        cx.bind_keys([
            gpui_kit::KeyBinding::new("tab", GridNextCell, Some("Grid")),
            gpui_kit::KeyBinding::new("shift-tab", GridPrevCell, Some("Grid")),
        ]);
        // 只有 Enter 结束编辑。失焦可能来自我们自己的重绘或切换焦点，贸然取消会丢掉正在输入的值。
        let edit_subscription = cx.subscribe(&edit_input, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.suppress_enter = true;
                this.finish_edit(cx);
            }
        });
        Self {
            session: SharedGrid::new(),
            services,
            table,
            locale: Locale::ZhCn,
            search_hint_locale: Locale::ZhCn,
            filter_hint_locale: Locale::ZhCn,
            scheduled: 0,
            search_text: String::new(),
            edit_input,
            edit_target: None,
            suppress_enter: false,
            pending_edit: None,
            notice: String::new(),
            grid_focus,
            refocus: false,
            loading_more: false,
            press: None,
            press_mode: PressMode::Plain,
            echo_cells: Vec::new(),
            visible_start: 0,
            table_resync: false,
            focus_inside: false,
            fetching: false,
            schema_cache: None,
            next_offset: 0,
            counting: None,
            count_sequence: 0,
            count_scope: None,
            pending_page: None,
            filter_inputs: Vec::new(),
            filter_subscriptions: Vec::new(),
            search: None,
            clear_search: false,
            pending_full: None,
            navigation: None,
            pending_viewer: false,
            record_open: false,
            side_width: RECORD_SIDE_DEFAULT,
            record_dragging: false,
            record_edit: None,
            record_input,
            pending_record: None,
            record_edit_deferred: None,
            _record_subscription: record_subscription,
            format_viewer: None,
            format_subscription: None,
            format_stamp: None,
            pending_fit: None,
            pending_batch: false,
            search_subscription: None,
            _subscription: subscription,
            _edit_subscription: edit_subscription,
        }
    }

    pub fn set_locale(&mut self, locale: Locale) -> bool {
        let changed = self.locale != locale;
        self.locale = locale;
        self.session.locale = locale;
        changed
    }

    /// 外键跳转的出口由外壳注入：网格不知道工作项，只把目标和定位查询交出去。
    pub fn set_navigation_sink(&mut self, sink: NavigationSink) {
        self.navigation = Some(sink);
    }

    /// 大字段预览上限来自设置 preview_kb。上限一变，旧行列是按别的上限截断出来的，
    /// 查看面板和已读原值都不能继续用：表数据重读第一页，查询结果只能重新执行后生效。
    pub fn set_preview_kb(&mut self, kb: u64, cx: &mut Context<Self>) {
        let bytes = usize::try_from(kb.max(1).saturating_mul(1024)).unwrap_or(usize::MAX);
        if !self.session.set_preview_bytes(bytes) {
            return;
        }
        // 正在编辑的格子按旧上限显示：结束编辑并回到网格，读完的原值缓存也已经作废。
        self.abandon_edit();
        self.pending_full = None;
        if self.session.mode == GridMode::Table && !self.session.target.connection_id.is_empty() {
            // 表数据可以重读：从第一页重新读取，行列与预览都按新上限解释。
            self.session.set_page_keep_identity(0, self.session.page_limit().max(1));
            self.fetch_current(true, cx);
        } else if self.session.loaded_len() > 0 {
            // 查询结果不能由网格重跑：保留行，但说明新上限要重新执行查询才生效。
            self.notice = i18n::text(self.locale, "grid.preview.reloadQuery");
        }
        self.sync_table(cx);
        cx.notify();
    }

    /// TSV 里数据库 NULL 的显示文本来自设置；SQL 字面量不受影响。
    pub fn set_null_copy(&mut self, value: &str) {
        self.session.set_null_copy(crate::app::views::settings::null_display(value));
    }

    /// 页大小来自设置；改变后从第一页重读，翻页和查询结果沿用同一上限。
    pub fn set_page_limit(&mut self, limit: u64, cx: &mut Context<Self>) {
        let limit = limit.max(1);
        if self.session.page_limit() == limit {
            return;
        }
        self.session.set_page(0, limit);
        self.pending_page = None;
        if self.session.mode == GridMode::Table && !self.session.target.connection_id.is_empty() {
            self.fetch_current(true, cx);
        }
    }

    pub fn clear_sheet(&mut self, cx: &mut Context<Self>) {
        let locale = self.locale;
        let preview_bytes = self.session.preview_bytes();
        let null_copy = self.session.null_copy().to_owned();
        self.fetching = false;
        self.schema_cache = None;
        self.next_offset = 0;
        self.counting = None;
        self.count_scope = None;
        self.pending_page = None;
        self.session = SharedGrid::new();
        self.session.locale = locale;
        self.session.set_preview_bytes(preview_bytes);
        self.session.set_null_copy(&null_copy);
        self.scheduled = 0;
        self.edit_target = None;
        self.pending_edit = None;
        self.discard_record_edit();
        self.press = None;
        self.filter_input_reset();
        self.notice.clear();
        self.sync_table(cx);
    }

    /// 菜单/查看面板关闭后把键盘交回网格；下一帧绘制时真正聚焦。
    pub fn request_focus(&mut self) {
        self.refocus = true;
    }

    #[cfg(test)]
    pub fn focus_handle(&self) -> FocusHandle {
        self.grid_focus.clone()
    }

    #[cfg(test)]
    pub fn editing(&self) -> bool {
        self.edit_target.is_some()
    }

    #[cfg(test)]
    pub fn edit_value(&self, cx: &App) -> String {
        self.edit_input.read(cx).value().to_string()
    }

    pub fn uncommitted(&self) -> bool {
        self.session.uncommitted()
    }

    /// 首屏或替换读取仍在进行；旧快照不能当成本表的数据源。
    pub fn fetching(&self) -> bool {
        self.fetching
    }

    pub fn writable(&self) -> bool {
        self.session.writable()
    }

    pub fn cell(&self, row: usize, column: usize) -> String {
        self.session.cell_label(row, column)
    }

    pub fn rendered_len(&self) -> usize {
        self.session.rendered_len()
    }

    pub fn loaded_len(&self) -> usize {
        self.session.loaded_len()
    }

    pub fn full_count(&self) -> Option<i64> {
        self.session.full_count()
    }

    pub fn banner(&self) -> String {
        self.session.banner()
    }

    pub fn preview_open(&self) -> bool {
        self.session.preview_open()
    }

    pub fn preview_text(&self) -> String {
        self.session.preview_sql().join("\n")
    }

    pub fn with_session<R>(&mut self, body: impl FnOnce(&mut SharedGrid) -> R) -> R {
        body(&mut self.session)
    }

    pub fn exchange_metadata(&self) -> (Option<TableRef>, Vec<String>, bool) { self.session.exchange_metadata() }

    pub fn exchange_stamp(&self) -> (GridTarget, (u64, usize, u64, bool), bool) {
        (self.session.target.clone(), self.session.exchange_stamp(), self.fetching)
    }

    #[allow(clippy::type_complexity)]
    pub fn exchange_sheet(&self) -> (Option<TableRef>, Vec<String>, Vec<Vec<Option<String>>>, bool) {
        self.session.exchange_sheet()
    }

    /// 交换/导出看到的这些行是按哪个预览上限读出来的；导出前的信封检查用它，不用设置里的新值。
    pub fn sheet_preview_bytes(&self) -> usize {
        self.session.sheet_preview_bytes()
    }

    /// 查询结果进网格。截断与完整行数由查询页给出：网格据此区分"已到末尾"与
    /// "这个结果不能由网格继续"，不会把被截断的结果说成已读完。
    pub fn show_query(&mut self, target: GridTarget, sheet: QuerySheet, cx: &mut Context<Self>) {
        self.counting = None;
        self.count_scope = None;
        self.session.target = target;
        self.session.locale = self.locale;
        // 新的结果集意味着旧的编辑目标不再存在，先结束编辑。
        self.edit_target = None;
        self.pending_edit = None;
        // 记录面板的列号属于上一份列结构：留在这里，下一份结果的同名列会继承上一张表的文字。
        self.discard_record_edit();
        self.press = None;
        self.pending_page = None;
        let QuerySheet { sql, columns, rows, truncated, full_count } = sheet;
        let token = self.session.show_query_sheet(&sql, columns, rows, truncated, full_count);
        self.sync_table(cx);
        self.schedule_locate(token, cx);
    }

    pub fn open_table(&mut self, target: GridTarget, limit: u64, cx: &mut Context<Self>) {
        self.session.mode = GridMode::Table;
        self.session.switch_target(target);
        self.session.locale = self.locale;
        self.session.set_page(0, limit);
        self.edit_target = None;
        self.pending_edit = None;
        self.discard_record_edit();
        self.press = None;
        self.pending_page = None;
        self.search_text.clear();
        self.clear_search = true;
        self.fetch_current(true, cx);
    }

    pub fn command(&mut self, id: &str, cx: &mut Context<Self>) -> Option<String> {
        // 先对齐连接状态：连接在整表筛选期间丢失时，这个命令里的查找/翻页都按已读取行处理。
        self.sync_find_scope();
        // 需要原值的动作先读同一定位结果；没读到就不执行，绝不用预览信封顶替。
        if let Some(action) = full_action(id) {
            if !self.prepare_full(action, id, cx) {
                cx.notify();
                return None;
            }
        }
        // 外键跳转的目标先记下来，网格自己的状态更新完再交给外壳打开工作项。
        let mut navigation: Option<GridNavigation> = None;
        let copied = match id {
            "grid-count" => { self.count_rows(cx); None }
            // 读取失败后按当前条件重读同一窗口；查询结果不重新执行语句，只把原因继续留在屏幕上。
            "grid-retry" => {
                if self.session.mode == GridMode::Table && !self.fetching {
                    self.fetch_current(true, cx);
                }
                None
            }
            "edit-cell" => {
                self.select_first_if_needed();
                self.request_edit(false);
                None
            }
            "cancel-edit" => {
                self.cancel_edit();
                None
            }
            // 选区设值的编辑栏里点 NULL：整批设为数据库 NULL，而不是只改焦点格。
            "grid-set-null" if self.edit_target.as_ref().is_some_and(|target| !target.cells.is_empty()) => {
                let cells = self.edit_target.take().map(|target| target.cells).unwrap_or_default();
                let result = self.session.stage_batch(cells.into_iter().map(|(row_id, column)| (row_id, column, None)).collect());
                self.report_batch(result);
                self.abandon_edit();
                None
            }
            "grid-set-null" => {
                self.select_first_if_needed();
                if let Some((row, column)) = self.session.focus_cell() {
                    // 显式 NULL：不经过任何特殊文本，直接写数据库 NULL。
                    let gate = self.session.stage_text(row, column, None);
                    self.notice = self.session.gate_text(gate);
                    self.abandon_edit();
                }
                None
            }
            "commit" => {
                if self.edit_target.is_some() {
                    self.finish_edit(cx);
                }
                // 工作台 commit 只打开预览。写库只来自预览对话框的 confirm。
                let _ = self.preview(cx);
                None
            }
            "mark-delete" => {
                self.select_first_if_needed();
                if let Some((row, _)) = self.session.focus_cell() {
                    let gate = self.session.stage_delete(row);
                    self.notice = self.session.gate_text(gate);
                }
                None
            }
            "copy-region" => {
                if self.session.focus_cell().is_none() {
                    self.session.select_all_loaded();
                }
                // 单格复制值本身；多格复制含表头的 TSV，粘贴回表格时列名对得上。
                Some(self.session.copy_text())
            }
            "copy-sql-value" => {
                if self.session.focus_cell().is_none() {
                    self.session.select_all_loaded();
                }
                Some(self.session.sql_values())
            }
            "grid-copy-row" => {
                if self.session.focus_cell().is_none() {
                    self.session.select(0, 0, false);
                }
                Some(self.session.copy_row())
            }
            "grid-copy-column" => {
                if self.session.focus_cell().is_none() {
                    self.session.select(0, 0, false);
                }
                Some(self.session.copy_column())
            }
            "grid-copy-where" | "grid-copy-row-where" | "grid-copy-column-where" => {
                self.select_first_if_needed();
                let scope = match id {
                    "grid-copy-row-where" => CopyScope::Row,
                    "grid-copy-column-where" => CopyScope::Column,
                    _ => CopyScope::Cell,
                };
                // 没有可复制的条件时保留剪贴板原内容，不写一个空字符串覆盖它。
                let text = self.session.copy_where(scope);
                (!text.is_empty()).then_some(text)
            }
            "grid-filter-value" => {
                self.session.filter_focused_value();
                None
            }
            "select-all-cells" => {
                self.session.select_all_loaded();
                None
            }
            "grid-undo" => {
                let _ = self.session.undo_last_stage();
                self.notice.clear();
                None
            }
            "grid-search" => {
                // 查找默认只筛已读取的行；服务器范围才把关键字下发并重读。
                let text = self.search_text.clone();
                self.session.set_search(&text);
                if self.session.mode == GridMode::Table && self.session.find_scope() == FindScope::Server {
                    self.session.set_page_keep_identity(0, self.session.page_limit().max(1));
                    self.fetch_current(true, cx);
                }
                None
            }
            "grid-find-scope" => {
                self.toggle_find_scope(cx);
                None
            }
            "grid-sort" => {
                // 任意列都能排序：工具栏作用于当前列，升序 → 降序 → 取消。
                let _ = self.cycle_sort_here(cx);
                None
            }
            "grid-filter" => {
                let _ = self.add_filter_here(cx);
                None
            }
            "grid-filter-new" => {
                self.new_filter(cx);
                None
            }
            "grid-filter-apply" => {
                let _ = self.apply_filters(cx);
                None
            }
            "grid-filter-clear" => {
                self.clear_filter_panel(cx);
                None
            }
            "grid-page-next" | "grid-page-prev" => {
                // 查询结果没有"下一页"：网格不重新执行语句，多语句和写语句更不能。
                let forward = id == "grid-page-next";
                if self.session.mode != GridMode::Table {
                    self.notice = self.session.state_text(false);
                } else if !self.session.can_page(forward) {
                    // 第一页已经没有上一页，读到末尾就没有下一页；这里不假装成功，也不改偏移。
                    self.notice = i18n::text(self.locale, if forward { "grid.state.noMorePages" } else { "grid.state.firstPage" });
                } else if self.fetching || self.loading_more {
                    // 上一次翻页还在路上：重复点击不能叠加请求，也不能提前换页。
                    self.notice = i18n::text(self.locale, "grid.state.loading");
                } else {
                    // 编辑目标所在的行要离开窗口了，先结束编辑，避免 Enter 落到不存在的位置。
                    self.abandon_edit();
                    let limit = self.session.page_limit().max(1);
                    let offset = if forward {
                        self.session.page_offset().saturating_add(limit)
                    } else {
                        self.session.page_offset().saturating_sub(limit)
                    };
                    // 偏移留到结果读回来才改：失败时旧页、选择、滚动和暂存都还在，重试还是这一页。
                    self.pending_page = Some(offset);
                    self.session.clear_read_failure();
                    self.fetch_at(offset, true, cx);
                }
                None
            }
            "grid-insert" => {
                let gate = self.session.stage_insert();
                self.notice = self.session.gate_text(gate);
                None
            }
            "grid-preview" => {
                let _ = self.preview(cx);
                None
            }
            // 查看完整值：先显示预览，再按定位读原值；普通选择与键盘移动不会打开它。
            "grid-value-open" => {
                self.select_first_if_needed();
                if let Some((row, column)) = self.session.focus_cell() {
                    self.session.open_value(row, column);
                    let reads = self.session.begin_full_cells(&[(row, column)]);
                    self.start_full_reads(reads, None, cx);
                    self.notice.clear();
                    self.refocus = true;
                }
                None
            }
            "grid-value-close" => {
                self.session.close_value();
                self.refocus = true;
                None
            }
            "grid-value-retry" => {
                if let Some((row, column)) = self.session.viewer_cell() {
                    self.session.open_value(row, column);
                    let reads = self.session.begin_full_cells(&[(row, column)]);
                    self.start_full_reads(reads, None, cx);
                }
                None
            }
            // 查看面板里的复制：走同一条按格复制，和网格里的复制值一致。
            "grid-value-copy" => Some(self.session.copy_cell()),
            // 导出前把当前窗口里还是预览的格子一次读成原值，导出与 WHERE 随后共用。
            "grid-full-values" => {
                self.notice = i18n::text(self.locale, "grid.status.fullValues");
                None
            }
            // 外键：两件事分开。"跳转到引用行"真的打开引用表的工作项并带上定位查询；
            // "生成外键查询"只把这段 SELECT 放到剪贴板。条件不完整时两者都说明原因。
            "grid-fk-jump" | "grid-fk-select" => {
                let target = self.session.focus_cell();
                let reference = match target {
                    Some((row, column)) => self.session.reference(row, column),
                    None => Err("grid.fk.none"),
                };
                match reference {
                    Ok(reference) => {
                        let path = if reference.schema.is_empty() {
                            format!("{}.{}", reference.database, reference.table)
                        } else {
                            format!("{}.{}.{}", reference.database, reference.schema, reference.table)
                        };
                        self.notice = i18n::fill(&i18n::text(self.locale, "grid.fk.target"), &[("path", &path)]);
                        if id == "grid-fk-select" {
                            Some(reference.sql)
                        } else {
                            navigation = Some(self.reference_navigation(reference));
                            None
                        }
                    }
                    Err(key) => {
                        self.notice = i18n::text(self.locale, key);
                        None
                    }
                }
            }
            // 多格式复制：原值已在 prepare_full 里读好；读不到或没有选区时剪贴板保持原样。
            other if CopyFormat::from_command(other).is_some() => {
                let format = CopyFormat::from_command(other).unwrap_or(CopyFormat::TsvHeader);
                match self.session.copy_as(format) {
                    Ok(copied) => {
                        // 占位表名要说出来：这段 INSERT 不能原样执行。
                        self.notice = if copied.placeholder { i18n::text(self.locale, "grid.copyAs.placeholder") } else { String::new() };
                        Some(copied.text)
                    }
                    Err(CopyBlock::Empty) => {
                        self.notice = i18n::text(self.locale, "grid.copyAs.empty");
                        None
                    }
                    Err(CopyBlock::Missing) => {
                        let cells = self.session.action_cells(GridAction::CopyRegion);
                        self.notice = self.session.cell_block(&cells).unwrap_or_else(|| i18n::text(self.locale, "grid.value.noOriginal"));
                        None
                    }
                }
            }
            "grid-clone-row" => {
                self.select_first_if_needed();
                self.notice = match self.session.stage_clone() {
                    Ok(()) => i18n::text(self.locale, "grid.write.cloned"),
                    Err(reason) => reason,
                };
                None
            }
            "grid-paste" => {
                // 只认剪贴板里的文本（TSV）；为空或不是文本时整次拒绝。
                let text = cx.read_from_clipboard().and_then(|item| item.text());
                let result = self.session.paste_tsv(text.as_deref());
                self.report_batch(result);
                None
            }
            "grid-selection-null" => {
                let result = self.session.stage_selection(None);
                self.report_batch(result);
                None
            }
            "grid-selection-set" => {
                if !self.session.writable() {
                    self.notice = BatchReject::Readonly(self.session.gate_text(Gate::Readonly)).message(self.locale);
                } else if self.session.selection_targets().is_empty() {
                    self.notice = BatchReject::NoFocus.message(self.locale);
                } else {
                    // 编辑栏要在绘制时拿到 Window 才能聚焦。
                    self.pending_batch = true;
                }
                None
            }
            "grid-record-toggle" => {
                self.record_open = !self.record_open;
                // 关上面板就等于离开这次就地暂存：输入框跟着面板走，不能留到下次打开。
                if !self.record_open {
                    self.discard_record_edit();
                    self.refocus = true;
                }
                None
            }
            "grid-record-close" => {
                self.record_open = false;
                // 关上之前丢掉这次就地暂存：焦点和输入框都跟着面板一起走，留着会在下次打开时冒出来。
                self.discard_record_edit();
                self.refocus = true;
                None
            }
            "grid-record-next" | "grid-record-prev" => {
                // 换行前先丢掉这次就地暂存：否则下一行会显示上一行的文字，Enter 还会写进错的那一行。
                // 已经把值暂存到行上的修改不受影响。
                self.discard_record_edit();
                let _ = record::record_step(&mut self.session, id == "grid-record-next");
                self.refocus = true;
                self.sync_table(cx);
                cx.notify();
                None
            }
            // 预览信封记录字段的原值读回来了：回到同一条暂存入口，这次才是真的原值。
            "record-edit" => {
                if let Some(column) = self.record_edit_deferred.take() {
                    self.record_field_press(column, cx);
                }
                None
            }
            "grid-undo-cell" => {
                let target = self.session.focus_cell();
                if let Some((row, column)) = target {
                    let row_id = SharedGrid::row_id(&self.session, row).map(str::to_owned);
                    if let Some(row_id) = row_id {
                        let _ = self.session.revert_cell(&row_id, column);
                    }
                }
                None
            }
            "grid-undo-row" => {
                let target = self.session.focus_cell();
                if let Some((row, _)) = target {
                    let row_id = SharedGrid::row_id(&self.session, row).map(str::to_owned);
                    if let Some(row_id) = row_id {
                        let _ = self.session.revert_row(&row_id);
                    }
                }
                None
            }
            "grid-discard" => {
                self.session.discard_all_staged();
                None
            }
            "grid-jump-staged" => {
                if let Some((row, column)) = self.session.next_staged_cell() {
                    self.session.select(row, column, false);
                }
                None
            }
            "grid-sql-select" => {
                let sql = self.session.select_statement();
                if sql.is_empty() { None } else { Some(sql) }
            }
            "grid-sql-update" => {
                let sql = self.session.update_statement();
                if sql.is_empty() { None } else { Some(sql) }
            }
            _ => None,
        };
        if let Some(text) = copied.clone() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
        if !matches!(id, "grid-page-next" | "grid-page-prev" | "commit") {
            self.sync_table(cx);
        }
        if let Some((row, _)) = self.session.focus_cell() {
            self.table.update(cx, |state, cx| state.scroll_to_row(row, cx));
        }
        cx.notify();
        if let Some(navigation) = navigation {
            self.open_navigation(navigation, cx);
        }
        copied
    }

    /// 批量暂存的结果写进提示：成功说暂存了几格，拒绝说具体原因（已有暂存不变）。
    fn report_batch(&mut self, result: Result<usize, BatchReject>) {
        self.notice = match result {
            Ok(count) => i18n::fill(&i18n::text(self.locale, "grid.write.staged"), &[("count", &count.to_string())]),
            Err(reject) => reject.message(self.locale),
        };
    }

    /// 选区规模：一到几格给出准确数，大选区只报"多格"，不把整张表展开成格子。
    /// 记录面板底部每帧都要问，所以先看包围盒面积。
    fn selection_size(&self) -> usize {
        const LIMIT: usize = 256;
        match self.session.selection_bounds() {
            Some((row_start, column_start, row_end, column_end)) => {
                let area = (row_end - row_start + 1).saturating_mul(column_end - column_start + 1);
                if area <= LIMIT {
                    self.session.selection_count()
                } else {
                    usize::MAX
                }
            }
            None => 0,
        }
    }

    /// 记录面板标题旁的排序/筛选状态：当前结果是按哪一列排的、有多少条已应用的条件。
    fn record_scope_text(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(sorted) = self.session.sort_text() {
            parts.push(sorted);
        }
        let applied = self.session.filters().iter().filter(|draft| !draft.dirty).count();
        if applied > 0 {
            parts.push(i18n::fill(&i18n::text(self.locale, "grid.record.filtered"), &[("count", &applied.to_string())]));
        }
        (!parts.is_empty()).then(|| parts.join(" · "))
    }

    /// 记录面板里点一列的值：可写就地暂存，只读说明原因并打开完整值。
    /// 两条路都先选中那一格，记录面板和网格高亮因此始终指同一行同一列。
    pub(crate) fn record_field_press(&mut self, column: usize, cx: &mut Context<Self>) {
        let Some(row) = record::record_row(&self.session) else {
            self.record_edit_deferred = None;
            return;
        };
        self.session.select(row, column, false);
        let reason = self.session.write_reason(row, column);
        if !reason.is_empty() {
            self.notice = reason;
            self.session.open_value(row, column);
            self.pending_viewer = true;
            self.discard_record_edit();
            self.refocus = true;
            cx.notify();
            return;
        }
        // 预览信封不是原值：和网格原位编辑（begin_edit）同一条门控，先按定位读一次再进暂存。
        // 否则长文本/二进制格会打开一个空输入框，Enter 接着把空串暂存成一次静默改写。
        if self.session.needs_full(row, column) && self.session.full_text_at(row, column).is_none() {
            let cell = (row, column);
            let reads = self.session.begin_full_cells(&[cell]);
            self.discard_record_edit();
            if reads.is_empty() {
                self.notice = self.session.cell_block(&[cell]).unwrap_or_else(|| i18n::text(self.locale, "grid.value.noOriginal"));
                self.refocus = true;
                cx.notify();
                return;
            }
            let pending = PendingFull { id: "record-edit".to_owned(), token: self.session.sheet_serial() };
            self.record_edit_deferred = Some(column);
            self.pending_full = Some(pending.clone());
            self.start_full_reads(reads, Some(pending), cx);
            cx.notify();
            return;
        }
        self.notice.clear();
        self.record_edit_deferred = None;
        self.record_edit = Some(column);
        self.pending_record = Some((row, column));
        self.sync_table(cx);
        cx.notify();
    }

    /// 丢掉记录面板里这次就地暂存：换行、关闭面板、重读都先走这里。
    /// 暂存状态跨行没有意义——留着它，下一行会显示上一行的文字，Enter 还会写进错的那一行。
    fn discard_record_edit(&mut self) {
        self.record_edit = None;
        self.pending_record = None;
        self.record_edit_deferred = None;
    }

    /// 绘制阶段才能拿到 Window：把这一格的原值写进面板输入框并聚焦。
    fn begin_record_edit(&mut self, row: usize, column: usize, window: &mut Window, cx: &mut Context<Self>) {
        // 值还是预览信封（按下之后刚好又重读过这一行）：不进暂存，说明原因，
        // 绝不把一个空输入框摆出来让 Enter 暂存成空串。正常路径已在 record_field_press 里先读过原值。
        if self.session.needs_full(row, column) && self.session.full_text_at(row, column).is_none() {
            self.discard_record_edit();
            self.notice = self.session.cell_block(&[(row, column)]).unwrap_or_else(|| i18n::text(self.locale, "grid.value.noOriginal"));
            self.refocus = true;
            cx.notify();
            return;
        }
        let text = self.session.edit_text(row, column);
        self.record_input.update(cx, |state, cx| state.set_value(text, window, cx));
        let handle = self.record_input.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
    }

    /// Enter：这一列的值原样暂存到行上。只读、未定位和校验失败都给出同一套原因，
    /// 已有的暂存保持不动（和网格原位编辑同一个门控）。
    pub(crate) fn finish_record_edit(&mut self, cx: &mut Context<Self>) {
        let Some(column) = self.record_edit.take() else {
            return;
        };
        let Some(row) = record::record_row(&self.session) else {
            return;
        };
        let text = self.record_input.read(cx).value().to_string();
        // 原来就是 NULL 且没有输入任何文字时保持 NULL；输入空字符串才是空字符串。
        let original = self.session.raw_value(row, column);
        let value = match (&original, text.is_empty()) {
            (Some(None), true) => None,
            _ => Some(text),
        };
        let gate = self.session.stage_text(row, column, value);
        self.notice = self.session.gate_text(gate);
        self.refocus = true;
        self.sync_table(cx);
        cx.notify();
    }

    /// Esc：放弃这次输入。已经暂存的其它格子不受影响。
    fn cancel_record_edit(&mut self, cx: &mut Context<Self>) {
        self.record_edit = None;
        self.refocus = true;
        cx.notify();
    }

    /// 记录视图里点开某一列的完整值：焦点移到那一格，再走网格同一条查看路径。
    pub fn open_record_value(&mut self, column: usize, cx: &mut Context<Self>) {
        let Some(row) = record::record_row(&self.session) else {
            return;
        };
        self.session.select(row, column, false);
        let _ = self.command("grid-value-open", cx);
    }

    /// 打开"选区设值"的编辑栏：输入一个值，Enter 后作为一批暂存到选区里的每个可见格子。
    fn begin_batch_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let cells = self.session.selection_targets();
        let Some((row_id, column)) = cells.first().cloned() else {
            self.notice = BatchReject::NoFocus.message(self.locale);
            return;
        };
        self.edit_target = Some(EditTarget { row_id, column, original: None, cells });
        self.edit_input.update(cx, |state, cx| state.set_value("", window, cx));
        let handle = self.edit_input.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        self.notice.clear();
        cx.notify();
    }

    /// 把外键跳转交给外壳：打开引用表的工作项，选择、滚动、暂存和查看都留在原来那一项上。
    /// 没有接线时如实说明，而不是只把 SQL 放到剪贴板冒充已跳转。
    fn reference_navigation(&self, reference: Reference) -> GridNavigation {
        GridNavigation {
            connection_id: reference.connection_id,
            connection_name: self.session.target.connection_name.clone(),
            database: reference.database,
            schema: reference.schema,
            table: reference.table,
            engine: self.session.target.engine.clone(),
            sql: reference.sql,
        }
    }

    fn open_navigation(&mut self, navigation: GridNavigation, cx: &mut Context<Self>) {
        let Some(sink) = self.navigation.clone() else {
            self.notice = i18n::text(self.locale, "grid.fk.noShell");
            cx.notify();
            return;
        };
        sink(navigation, cx);
    }

    /// 需要原值的动作先读同一定位结果。全部读完才返回 true，调用方随后才执行动作。
    fn prepare_full(&mut self, action: GridAction, id: &str, cx: &mut Context<Self>) -> bool {
        // 导出读的是 exchange_sheet 的整份来源行：被本地查找/筛选隐藏的预览行也在读取范围内，
        // 否则导出被挡而网格上没有能读到它们的入口。
        let exported = action == GridAction::ExportValues;
        let cells = self.session.action_cells(action);
        if cells.is_empty() && !exported {
            return true;
        }
        let reads = if exported {
            self.session.begin_unresolved_full()
        } else {
            self.session.begin_full_cells(&cells)
        };
        if !reads.is_empty() {
            // 一次读取批次成功后重跑同一个命令，查看、复制、编辑、WHERE 因此共用同一个原值。
            // 待重跑的命令带上快照代次：目标或行列一变，回填结果不再触发旧动作。
            let pending = PendingFull { id: id.to_owned(), token: self.session.sheet_serial() };
            self.pending_full = Some(pending.clone());
            self.start_full_reads(reads, Some(pending), cx);
            return false;
        }
        // 没有新请求时还要确认这几格真的拿到了原值：加载中或上次失败都停在这里。
        let block = if exported { self.session.unresolved_block() } else { self.session.cell_block(&cells) };
        match block {
            None => true,
            Some(reason) => {
                self.notice = reason;
                self.sync_table(cx);
                cx.notify();
                false
            }
        }
    }

    /// 一次按定位读取完整值：读到就缓存，失败保留原因并可重试；两处都不写预览。
    fn start_full_reads(&mut self, reads: Vec<FullRead>, pending: Option<PendingFull>, cx: &mut Context<Self>) {
        if reads.is_empty() {
            return;
        }
        let schema = self.session.schema().cloned();
        let session = self.services.connections.catalog_session(&self.session.target.connection_id);
        let Some(connection) = session else {
            let reason = i18n::text(self.locale, "grid.value.noConnection");
            for read in &reads {
                self.session.fail_full(read, &reason);
            }
            self.notice = reason;
            self.pending_full = None;
            self.sync_table(cx);
            cx.notify();
            return;
        };
        let view = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            runtime().spawn(async move {
                let mut results = Vec::new();
                for read in reads {
                    let locator = read.locator.iter().map(|(name, value)| (name.clone(), cell_of(value.clone()))).collect::<Vec<_>>();
                    let result = match &schema {
                        Some(schema) => crate::grid::read_full_cell(&connection, schema, &locator, &read.column_name).await,
                        None => Err("没有读到表结构，无法按定位读取完整值".to_owned()),
                    };
                    results.push((read, result));
                }
                let _ = tx.send(results);
            });
            if let Ok(results) = rx.await {
                cx.update(|cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |this, cx| this.finish_full(results, pending, cx));
                    }
                });
            }
        })
        .detach();
    }

    /// 读取回来后把原值放回缓存；全部成功才重跑同一个命令，各自入口因此共用同一个原值。
    fn finish_full(&mut self, results: Vec<(FullRead, Result<crate::grid::Cell, String>)>, pending: Option<PendingFull>, cx: &mut Context<Self>) {
        let mut failure = None;
        let mut applied = 0usize;
        for (read, result) in results {
            match result {
                Ok(cell) => {
                    let value = match cell {
                        crate::grid::Cell::Null => None,
                        crate::grid::Cell::Text(text) => Some(text),
                    };
                    // 目标或行列已经换了的迟到结果被丢弃，不写到同一 id 的另一格上。
                    if self.session.apply_full(&read, value) {
                        applied += 1;
                    }
                }
                Err(message) => {
                    if self.session.fail_full(&read, &message) {
                        failure = Some(message);
                    }
                }
            }
        }
        self.sync_table(cx);
        if self.pending_full.as_ref() == pending.as_ref() {
            self.pending_full = None;
        }
        match failure {
            Some(message) => {
                // 读取失败：预览仍然可见，但已经标成错误，不重跑动作也不退回预览。
                self.notice = message;
                cx.notify();
            }
            None => {
                // 全部过期时没有新原值可重跑，动作停在原处等用户重试。
                if let Some(pending) = pending.filter(|_| applied > 0) {
                    if pending.token == self.session.sheet_serial() {
                        let _ = self.command(&pending.id, cx);
                    }
                }
                cx.notify();
            }
        }
    }

    /// 当前格所在列名；没有选择时退回第一列。
    fn focused_column_name(&self) -> Option<String> {
        let column = self.session.focus_cell().map(|(_, column)| column).unwrap_or(0);
        self.session.column_names().get(column).cloned()
    }

    /// 目标连接是否真的已经打开。没有打开就不另开连接、也不发读取。
    fn live_target(&self) -> bool {
        !self.session.target.connection_id.is_empty() && self.services.connections.catalog_session(&self.session.target.connection_id).is_some()
    }

    /// 连接状态在界面外也会变（关闭、失联、重连）。每次操作和每帧都同步一次：
    /// 整表筛选失去可重新读取的表时退回已读取行，并留下具体原因。
    fn sync_find_scope(&mut self) -> Option<&'static str> {
        self.session.set_target_live(self.live_target());
        if self.session.demote_server_find_scope() {
            self.notice = i18n::text(self.locale, "grid.find.scopeOffline");
        }
        self.session.find_scope_block()
    }

    /// 条件、排序或查找范围改变后：表数据从第一页重读，查询结果只在本地重排。
    fn reload_after_condition_change(&mut self, cx: &mut Context<Self>) {
        self.sync_find_scope();
        if self.session.mode == GridMode::Table && self.live_target() {
            let limit = self.session.page_limit().max(1);
            // 条件变了，旧偏移指向另一组行；选择仍按行 id 重新挂回。
            self.session.set_page_keep_identity(0, limit);
            self.abandon_edit();
            self.fetch_current(true, cx);
        } else {
            self.sync_table(cx);
        }
        cx.notify();
    }

    /// 任意列升序 → 降序 → 取消；工具栏和表头菜单共用这条路径。
    pub fn cycle_sort(&mut self, column: &str, cx: &mut Context<Self>) -> Option<bool> {
        let next = self.session.toggle_sort(column);
        self.reload_after_condition_change(cx);
        next
    }

    pub fn cycle_sort_here(&mut self, cx: &mut Context<Self>) -> Option<bool> {
        let name = self.focused_column_name()?;
        self.cycle_sort(&name, cx)
    }

    /// 在当前格所在列上加一条条件：NULL、空串或等于当前值。
    pub fn add_filter_here(&mut self, cx: &mut Context<Self>) -> Option<usize> {
        let name = self.focused_column_name()?;
        let row = self.session.focus_cell();
        let index = self.session.add_filter(&name);
        match row.and_then(|(row, column)| self.session.raw_value(row, column)) {
            Some(None) => {
                self.session.set_filter_op(index, "null");
            }
            Some(Some(text)) if text.is_empty() => {
                self.session.set_filter_op(index, "empty");
            }
            Some(Some(text)) => {
                self.session.set_filter_op(index, "eq");
                self.session.update_filter_text(index, true, &text);
            }
            None => {}
        }
        self.filter_input_reset();
        self.apply_filters(cx).ok()?;
        Some(index)
    }

    /// 应用筛选条里的全部条件；非法输入只提示原因，不发查询也不清旧结果。
    pub fn apply_filters(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        match self.session.apply_filters() {
            Ok(()) => {
                self.notice.clear();
                self.reload_after_condition_change(cx);
                Ok(())
            }
            Err(error) => {
                // 条件输入和上一次有效结果都保留，只把原因显示出来。
                self.notice = error.clone();
                self.sync_table(cx);
                cx.notify();
                Err(error)
            }
        }
    }

    /// 新建一条空条件：默认用焦点列（没有焦点时用第一个可见列），先不应用，等用户填值。
    pub fn new_filter(&mut self, cx: &mut Context<Self>) -> Option<usize> {
        let name = self.focused_column_name().or_else(|| {
            let names = self.session.column_names();
            self.session.visible_columns().first().and_then(|index| names.get(*index).cloned())
        })?;
        let index = self.session.add_filter(&name);
        self.filter_input_reset();
        cx.notify();
        Some(index)
    }

    /// 从下拉里换条件的列：运算符不适用新列时退回该列的第一个运算符。
    pub fn pick_filter_column(&mut self, index: usize, column: &str, cx: &mut Context<Self>) {
        if self.session.set_filter_column(index, column) {
            self.filter_input_reset();
            cx.notify();
        }
    }

    /// 从下拉里换运算符：不需要值的运算符（为空、非空、空字符串）立即生效。
    pub fn pick_filter_op(&mut self, index: usize, op: &str, cx: &mut Context<Self>) {
        if !self.session.set_filter_op(index, op) {
            return;
        }
        self.filter_input_reset();
        if matches!(op, "null" | "notnull" | "empty") {
            self.apply_filters(cx).ok();
        }
        cx.notify();
    }

    pub fn cycle_filter_op(&mut self, index: usize, cx: &mut Context<Self>) -> Option<&'static str> {
        let op = self.session.cycle_filter_op(index)?;
        self.filter_input_reset();
        self.apply_filters(cx).ok();
        Some(op)
    }

    pub fn update_filter_text(&mut self, index: usize, first: bool, text: &str) {
        self.session.update_filter_text(index, first, text);
    }

    pub fn remove_filter(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.session.remove_filter(index) {
            self.filter_input_reset();
            self.apply_filters(cx).ok();
        }
    }

    pub fn clear_filter_panel(&mut self, cx: &mut Context<Self>) {
        self.session.clear_filters();
        self.search_text.clear();
        self.clear_search = true;
        self.filter_input_reset();
        self.apply_filters(cx).ok();
    }

    /// 查找范围：已读取行（本地）或整表（服务器筛选，需要重读）。
    /// 查询结果、没有目标表、连接已关闭或失联时不能切到整表筛选：给出原因，
    /// 而不是让关键字静默失效；关键字继续作用于已读取行。
    pub fn set_find_scope(&mut self, scope: FindScope, cx: &mut Context<Self>) -> bool {
        if self.session.find_scope() == scope {
            return true;
        }
        if scope == FindScope::Server {
            if let Some(reason) = self.sync_find_scope() {
                self.notice = i18n::text(self.locale, reason);
                self.sync_table(cx);
                cx.notify();
                return false;
            }
        }
        match self.session.set_find_scope(scope) {
            Ok(()) => {
                self.notice.clear();
                self.reload_after_condition_change(cx);
                true
            }
            Err(reason) => {
                self.notice = i18n::text(self.locale, reason);
                self.sync_table(cx);
                cx.notify();
                false
            }
        }
    }

    pub fn toggle_find_scope(&mut self, cx: &mut Context<Self>) -> bool {
        let next = if self.session.find_scope() == FindScope::Loaded { FindScope::Server } else { FindScope::Loaded };
        self.set_find_scope(next, cx)
    }

    fn filter_input_reset(&mut self) {
        self.filter_inputs.clear();
        self.filter_subscriptions.clear();
    }

    /// 筛选条输入与条件行数对齐；结构变化（增删、换条件）后整体重建，避免下标错位。
    fn ensure_filter_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let drafts = self.session.filters().to_vec();
        if self.filter_inputs.len() == drafts.len() {
            if self.filter_hint_locale != self.locale {
                let locale = self.locale;
                let upper_hint = SharedString::from(i18n::text(locale, "grid.filter.upperPlaceholder"));
                for (row, draft) in self.filter_inputs.iter().zip(drafts.iter()) {
                    let value_hint = SharedString::from(i18n::text(locale, if draft.op == "in" { "grid.filter.valuePlaceholder" } else { "grid.filter.value" }));
                    if let Some(state) = &row.value {
                        state.update(cx, |state, cx| state.set_placeholder(value_hint.clone(), window, cx));
                    }
                    if let Some(state) = &row.second {
                        state.update(cx, |state, cx| state.set_placeholder(upper_hint.clone(), window, cx));
                    }
                }
                self.filter_hint_locale = locale;
            }
            return;
        }
        self.filter_input_reset();
        let locale = self.locale;
        let mut inputs = Vec::new();
        let mut subscriptions = Vec::new();
        for (index, draft) in drafts.iter().enumerate() {
            // 只有“属于集合”才提示逗号分隔；其它运算符只要一个值。
            let placeholder = SharedString::from(i18n::text(locale, if draft.op == "in" { "grid.filter.valuePlaceholder" } else { "grid.filter.value" }));
            let upper = SharedString::from(i18n::text(locale, "grid.filter.upperPlaceholder"));
            let mut row = FilterRowInputs::default();
            for (slot, placeholder) in [(0usize, placeholder), (1usize, upper)] {
                let state = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder.clone()));
                let text = if slot == 0 { draft.value.clone() } else { draft.second.clone() };
                state.update(cx, |state, cx| state.set_value(text, window, cx));
                let subscription = cx.subscribe(&state, move |this, state, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        let text = state.read(cx).value().to_string();
                        this.update_filter_text(index, slot == 0, &text);
                        cx.notify();
                    } else if matches!(event, InputEvent::PressEnter { .. }) {
                        // 只有显式应用才会发查询；非法输入停在这里。
                        let _ = this.apply_filters(cx);
                    }
                });
                subscriptions.push(subscription);
                if slot == 0 {
                    row.value = Some(state);
                } else {
                    row.second = Some(state);
                }
            }
            inputs.push(row);
        }
        self.filter_inputs = inputs;
        self.filter_subscriptions = subscriptions;
    }

    fn select_first_if_needed(&mut self) {
        if self.session.focus_cell().is_none() {
            self.session.select(0, 0, false);
        }
    }

    /// 左键按下格子。普通按下把选择收拢到这一格；Shift 按下保留锚点扩成矩形；
    /// Ctrl/Cmd 按下把这一格加进选区，或者把已经选中的这一格减掉（非连续选择）。
    pub(crate) fn cell_press(&mut self, row: usize, column: usize, mode: PressMode, cx: &mut Context<Self>) {
        self.press = Some((row, column));
        self.press_mode = mode;
        // Shift 落在焦点格上没有新信息；Ctrl/Cmd 落在同一格上要真的加/减，不能吞掉。
        if mode == PressMode::Extend && self.session.focus_cell() == Some((row, column)) {
            return;
        }
        match mode {
            PressMode::Plain => self.session.select(row, column, false),
            PressMode::Extend => self.session.select(row, column, true),
            PressMode::Toggle => self.session.toggle_press(row, column),
        }
        // 上一次禁写/校验提示属于旧格子，移动目标后不再显示。
        self.notice.clear();
        self.sync_table(cx);
        cx.notify();
    }

    /// 按住左键拖过另一格：普通/Shift 按下时把主矩形扩展到当前格；Ctrl/Cmd 按下时
    /// 从按下格长出一块新区域（或者把已选区域挖掉一块）。没有按下时不改变选择。
    pub(crate) fn cell_drag(&mut self, row: usize, column: usize, cx: &mut Context<Self>) {
        if self.press.is_none() || self.session.focus_cell() == Some((row, column)) {
            return;
        }
        match self.press_mode {
            PressMode::Toggle => self.session.toggle_drag(row, column),
            _ => self.session.select(row, column, true),
        }
        self.notice.clear();
        self.sync_table(cx);
        cx.notify();
    }

    /// 松开左键：结束拖拽并把 Ctrl/Cmd 区域落定。下一个鼠标移动不会继续改选择。
    pub(crate) fn cell_release(&mut self) {
        self.press = None;
        self.session.finish_region();
    }

    /// 结束拖拽但不改选择：指针在表外松开、窗口失焦、调整列宽或滚动都走这里。
    /// 返回刚才是否真的在拖选，界面据此只重绘真正变了的状态。
    pub(crate) fn cancel_drag(&mut self) -> bool {
        self.session.finish_region();
        self.press.take().is_some()
    }

    #[cfg(test)]
    pub(crate) fn dragging(&self) -> bool {
        self.press.is_some()
    }

    /// 开始编辑：可写的格子走正式文本输入，只读的格子改为查看值并说明原因。
    fn request_edit(&mut self, select_all: bool) {
        let Some((row, column)) = self.session.focus_cell() else {
            return;
        };
        let reason = self.session.write_reason(row, column);
        if !reason.is_empty() {
            self.notice = reason;
            // 只读时退化为查看完整值：统一走查看路径，预览值会先按定位读取原值。
            self.session.open_value(row, column);
            self.pending_viewer = true;
            self.edit_target = None;
            self.pending_edit = None;
            self.refocus = true;
            return;
        }
        self.notice.clear();
        self.pending_edit = Some(PendingEdit { row, column, select_all });
    }

    /// 绘制阶段才能拿到 Window：在这里填充并聚焦编辑输入，选区/光标由输入本身维护。
    fn begin_edit(&mut self, pending: PendingEdit, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row_id) = SharedGrid::row_id(&self.session, pending.row).map(str::to_owned) else {
            return;
        };
        // 预览信封不是原值：双击和 F2 也要先按定位读一次，绝不把信封（或空串）放进编辑框。
        if self.session.needs_full(pending.row, pending.column) && self.session.full_text_at(pending.row, pending.column).is_none() {
            let cell = (pending.row, pending.column);
            let reads = self.session.begin_full_cells(&[cell]);
            self.pending_edit = None;
            if reads.is_empty() {
                self.notice = self.session.cell_block(&[cell]).unwrap_or_else(|| i18n::text(self.locale, "grid.value.noOriginal"));
                cx.notify();
                return;
            }
            let command = PendingFull { id: "edit-cell".to_owned(), token: self.session.sheet_serial() };
            self.pending_full = Some(command.clone());
            self.start_full_reads(reads, Some(command), cx);
            return;
        }
        // 切换到另一格前先把正在输入的内容暂存，避免双击别处时悄悄丢掉已输入文本。
        // 选区设值没按 Enter 就不算数，不能把整批值写到单格上。
        if let Some(previous) = self.edit_target.take().filter(|target| target.cells.is_empty()) {
            let text = self.edit_input.read(cx).value().to_string();
            let value = match (&previous.original, text.is_empty()) {
                (Some(None), true) => None,
                _ => Some(text),
            };
            let gate = self.session.stage_text_by_id(&previous.row_id, previous.column, value);
            self.notice = self.session.gate_text(gate);
        }
        let original = self.session.raw_value(pending.row, pending.column);
        let text = self.session.edit_text(pending.row, pending.column);
        self.edit_target = Some(EditTarget { row_id, column: pending.column, original, cells: Vec::new() });
        self.edit_input.update(cx, |state, cx| {
            state.set_value(text, window, cx);
            if pending.select_all {
                state.select_all(window, cx);
            }
        });
        let handle = self.edit_input.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        self.notice.clear();
        cx.notify();
    }

    /// 提交编辑栏：把输入原样暂存，不把 "NULL" 之类的文本当成数据库 NULL。
    pub fn finish_edit(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.edit_target.take() else {
            return;
        };
        let text = self.edit_input.read(cx).value().to_string();
        if !target.cells.is_empty() {
            // 选区设值：输入原样作为文本，整批预检，任何一格不行就整批不生效。
            let result = self.session.stage_batch(target.cells.into_iter().map(|(row_id, column)| (row_id, column, Some(text.clone()))).collect());
            self.report_batch(result);
            self.pending_edit = None;
            self.refocus = true;
            self.sync_table(cx);
            cx.notify();
            return;
        }
        // 原来就是 NULL 且没有输入任何文字时保持 NULL；输入空字符串才是空字符串。
        let value = match (&target.original, text.is_empty()) {
            (Some(None), true) => None,
            _ => Some(text),
        };
        let gate = self.session.stage_text_by_id(&target.row_id, target.column, value);
        self.notice = self.session.gate_text(gate);
        self.pending_edit = None;
        self.refocus = true;
        self.sync_table(cx);
        cx.notify();
    }

    fn cancel_edit(&mut self) {
        self.edit_target = None;
        self.pending_edit = None;
        self.notice.clear();
        self.refocus = true;
    }

    fn abandon_edit(&mut self) {
        self.edit_target = None;
        self.pending_edit = None;
        self.refocus = true;
    }

    /// 表头右键菜单的命令。`column` 是数据列序号（不含行号列）。
    pub fn column_command(&mut self, column: usize, id: &str, cx: &mut Context<Self>) {
        let Some(name) = self.session.column_names().get(column).cloned() else {
            return;
        };
        let copied = match id {
            "column-sort-asc" => {
                self.session.set_sort(&name, false);
                self.reload_after_condition_change(cx);
                None
            }
            "column-sort-desc" => {
                self.session.set_sort(&name, true);
                self.reload_after_condition_change(cx);
                None
            }
            "column-sort-cycle" => {
                let _ = self.cycle_sort(&name, cx);
                None
            }
            "column-clear-sort" => {
                self.session.clear_sort();
                self.reload_after_condition_change(cx);
                None
            }
            "column-filter" => {
                self.session.add_filter(&name);
                self.filter_input_reset();
                None
            }
            "column-filter-null" => {
                self.session.set_filter(&name, "null", "");
                self.filter_input_reset();
                None
            }
            "column-clear-filters" => {
                self.search_text.clear();
                self.session.clear_filters();
                self.filter_input_reset();
                None
            }
            "column-copy-name" => Some(name),
            // 隐藏和冻结只改显示；复制、导出、筛选和排序仍按全部逻辑列。
            "column-hide" => {
                if !self.session.hide_column(column) {
                    self.notice = i18n::text(self.locale, "grid.column.lastVisible");
                }
                None
            }
            "column-show" => {
                self.session.show_column(column);
                None
            }
            "column-show-all" => {
                self.session.show_all_columns();
                None
            }
            "column-freeze" => {
                self.session.freeze_to(column);
                None
            }
            "column-unfreeze" => {
                self.session.unfreeze();
                None
            }
            "column-fit" => {
                // 量字宽要用窗口的文字系统，留到下一次绘制。
                self.pending_fit = Some(column);
                None
            }
            // 列头的整列复制与整列 WHERE 走单元格菜单同名动作的同一条原值读取：
            // 有预览就先按定位读取，读不到时说明原因，绝不复制空串或静默无动作。
            "column-copy" | "column-copy-where" => {
                let row = self.session.focus_cell().map(|(row, _)| row).unwrap_or(0);
                self.session.select(row, column, false);
                let command = if id == "column-copy" { "grid-copy-column" } else { "grid-copy-column-where" };
                self.command(command, cx);
                return;
            }
            _ => None,
        };
        if let Some(text) = copied {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
        self.sync_table(cx);
        cx.notify();
    }

    /// 行号列（表格的第 0 列）不是数据列。表格自己的点击处理会为它进入单元格/行/列模式，
    /// 底色、键盘模式和会话选区因此不一致；下一帧绘制前统一按会话焦点复位。
    fn schedule_table_resync(&mut self) {
        self.table_resync = true;
    }

    /// 把表格自己的选择状态拉回会话：有焦点就是那一格，没有焦点就彻底清空。
    /// 表格为行号列留下的列模式、行模式都会消失，键盘回到单元格模式。
    fn resync_table_selection(&mut self, cx: &mut App) {
        let focus = self.table_focus();
        self.table.update(cx, |state, cx| match focus {
            Some((row, column)) if state.selection() != TableSelection::Cell(row, column) => {
                state.set_selected_cell(row, column, cx);
            }
            None if state.selection() != TableSelection::None => state.clear_selection(cx),
            _ => {}
        });
    }

    /// 会话焦点在表格里的位置（含行号列）；焦点列被隐藏时没有位置。
    fn table_focus(&self) -> Option<(usize, usize)> {
        let (row, column) = self.session.focus_cell()?;
        self.session.logical_to_display(column).map(|display| (row, display + 1))
    }

    /// 表格列号（含行号列）→ 逻辑数据列。行号列或越界时为空。
    fn logical_of(&self, col: usize) -> Option<usize> {
        col.checked_sub(1).and_then(|display| self.session.display_to_logical(display))
    }

    fn apply_table_event(&mut self, event: &TableEvent) {
        match event {
            TableEvent::ColumnWidthsChanged(widths) => {
                // 拉宽列会让拖动经过的格子换位置；结束拖拽，别把新格子接进旧矩形。
                self.press = None;
                for (index, width) in widths.iter().enumerate() {
                    // 表格只有可见列：按显示位置找回逻辑列再记宽度。
                    if let Some(logical) = self.logical_of(index) {
                        self.session.set_column_width(logical, f32::from(*width));
                    }
                }
            }
            TableEvent::SelectCell(row, col) | TableEvent::RightClickedCell(row, col) => {
                // 行号列不是数据列：点它不能落到第一列数据上，只把表格的选中状态复位。
                let Some(column) = self.logical_of(*col) else {
                    self.schedule_table_resync();
                    return;
                };
                // sync_table 会把会话焦点写回表格，表格随后又发回同一条事件：认得出的回声直接忽略，
                // 否则一次选择更新连发的旧回声会把刚加进来的非连续区域冲掉。
                if let Some(index) = self.echo_cells.iter().position(|item| *item == (*row, *col)) {
                    self.echo_cells.remove(index);
                    return;
                }
                // 已经一致时不再改写选择。
                if self.session.focus_cell() != Some((*row, column)) {
                    self.session.select(*row, column, false);
                    // 上一次禁写/校验提示属于旧格子，移动目标后不再显示。
                    self.notice.clear();
                }
            }
            TableEvent::DoubleClickedCell(row, col) => {
                // 双击行号列不会打开第一列数据的编辑器；它与单击一样只复位表格状态。
                let Some(column) = self.logical_of(*col) else {
                    self.schedule_table_resync();
                    return;
                };
                // 双击与 F2 都进入同一条编辑路径；只读时退化为查看值并说明原因。
                self.session.select(*row, column, false);
                self.request_edit(true);
            }
            TableEvent::SelectColumn(col) => {
                // 列头索引含行号列：点 # 列头不是选择数据列。忽略它，否则会整列选中
                // 第一列数据，高亮和复制都落到用户没点的列上。
                // 表格在此之前已经把自己切成列模式（selected_col=0）并给 # 列刷激活底色，
                // 所以这里必须把表格选择拉回会话焦点：否则 # 整列还亮着，复制和统计却是空的，
                // 方向键也停在列模式。
                let Some(column) = self.logical_of(*col) else {
                    self.schedule_table_resync();
                    return;
                };
                // 列头与行头对称：整列就是从首行到末行的矩形，
                // 高亮、TSV、SQL 值和统计读的是同一段行列。
                let last = self.session.loaded_len().saturating_sub(1);
                if self.session.selection_bounds() != Some((0, column, last, column)) {
                    self.session.select(0, column, false);
                    self.session.select(last, column, true);
                }
            }
            TableEvent::SelectRow(row) | TableEvent::DoubleClickedRow(row) => {
                // 行头选择走同一组 anchor/focus：整行就是一个从首列到末列的矩形，
                // 高亮、TSV、SQL 值和统计因而和拖选整行得到的结果一致。
                let last = self.session.column_names().len().saturating_sub(1);
                let bounds = self.session.selection_bounds();
                if bounds != Some((*row, 0, *row, last)) {
                    self.session.select(*row, 0, false);
                    self.session.select(*row, last, true);
                }
                if matches!(event, TableEvent::DoubleClickedRow(_)) {
                    // 事件回调里不能重入，绘制前再把完整值查看打开。
                    self.pending_viewer = true;
                }
            }
            // 表格清空高亮常常只是 sync_table 的回声（事件异步送达）；选择身份由会话自己维护，
            // 真正的取消在按键与命令路径里处理，这里不能清掉用户刚选中的行。
            TableEvent::ClearSelection => {}
            _ => {}
        }
    }

    pub fn edit_focus(&mut self, value: Option<String>, cx: &mut Context<Self>) -> Gate {
        let gate = if let Some((row, column)) = self.session.focus_cell() {
            self.session.stage_text(row, column, value)
        } else {
            Gate::Readonly
        };
        self.notice = self.session.gate_text(gate);
        self.sync_table(cx);
        gate
    }

    pub fn preview(&mut self, cx: &mut Context<Self>) -> Result<Vec<String>, String> {
        let sql = self.session.open_preview();
        cx.notify();
        sql
    }

    pub fn cancel_preview(&mut self, cx: &mut Context<Self>) {
        self.session.cancel_preview();
        cx.notify();
    }

    pub fn confirm(&mut self, cx: &mut Context<Self>) {
        let Ok(request) = self.session.confirm_request() else {
            cx.notify();
            return;
        };
        let services = Arc::clone(&self.services);
        let view = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            runtime().spawn(async move {
                let result = crate::application::grid::commit(&services.connections, &request, true).await;
                let _ = tx.send(result);
            });
            if let Ok(result) = rx.await {
                cx.update(|cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |this, cx| {
                            match result {
                                Ok(report) => this.session.apply_commit(&report),
                                Err(message) => this.session.keep_failure(&message),
                            }
                            this.sync_table(cx);
                        });
                    }
                });
            }
        })
        .detach();
    }

    fn fetch_current(&mut self, replace: bool, cx: &mut Context<Self>) {
        // 只有翻页命令自己挂着 pending_page；其余替换读取不能继承那一页的落地语义。
        self.pending_page = None;
        self.fetch_at(self.session.page_offset(), replace, cx);
    }

    fn fetch_at(&mut self, offset: u64, replace: bool, cx: &mut Context<Self>) {
        // 条件非法时不发查询：保留上一次有效结果和条件输入，只说明原因。
        let mut request = match self.session.page_request() {
            Ok(request) => request,
            Err(error) => {
                self.loading_more = false;
                self.fetching = false;
                self.pending_page = None;
                self.notice = error;
                self.sync_table(cx);
                return;
            }
        };
        request.offset = offset;
        // 表结构连带真实外键一起读：外键导航用的是连接/库/模式同一次定位的结果。
        request.table.include_foreign_keys = true;
        // 分页追加不阻塞交换来源：已读取的窗口仍然完整；替换读取开始后不能被追加清掉。
        if replace {
            self.fetching = true;
        }
        let health = self.services.connections.cancel_transport(&request.table.connection_id).and_then(|transport| transport.health().cloned());
        let paging = !replace || self.pending_page.is_some();
        let cached_total = if paging { self.session.full_count() } else { None };
        let cached = self.schema_cache.as_ref().filter(|(target, old_health, _)| {
            paging && target == &self.session.target && matches!((old_health, &health), (Some(old), Some(current)) if Arc::ptr_eq(old, current))
        }).map(|(_, _, schema)| schema.clone());
        if !paging || self.count_scope.as_ref().is_some_and(|scope| scope != &count_of(&request)) {
            self.counting = None;
            self.count_scope = None;
        }
        self.session.token = self.session.token.saturating_add(1);
        let token = self.session.token;
        self.scheduled = token;
        let services = Arc::clone(&self.services);
        let view = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            runtime().spawn(async move {
                let described = match cached {
                    Some(schema) => Ok(schema),
                    None => crate::application::grid::describe(&services.connections, &request.table).await,
                };
                let paged = match described.as_ref().map_err(Clone::clone).and_then(schema_from_dto) {
                    Ok(schema) => crate::application::grid::page_described(&services.connections, &request, &schema).await,
                    Err(error) => Err(error),
                };
                // 大表首屏和翻页不执行 COUNT(*)；用户可以按需统计。
                let counted = cached_total.map(|matched| CountDto { matched }).ok_or_else(|| "count not requested".to_owned());
                let _ = tx.send((described, paged, counted));
            });
            if let Ok((described, paged, counted)) = rx.await {
                cx.update(|cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |this, cx| {
                            if this.session.token != token {
                                // 更新的请求持有 fetching，旧结果不能替它清加载态。
                                this.loading_more = false;
                                return;
                            }
                            // 这次读取无论成败都已经结束：翻页请求不再挂着，失败时也不落新偏移。
                            let landed_page = this.pending_page.take().is_some();
                            // 结构或分页任一失败都算这次读取失败；计数失败只让完整行数未知。
                            let counted = if paging {
                                this.session.full_count().map(|matched| Ok(CountDto { matched })).unwrap_or(counted)
                            } else { counted };
                            let read = described.and_then(|schema| paged.map(|page| (schema, page, counted)));
                            if this.apply_page_result(offset, replace, landed_page, read, cx) {
                                if let Some((_, cached_health, _)) = this.schema_cache.as_mut() { *cached_health = health; }
                            }
                        });
                    }
                });
            }
        })
        .detach();
    }

    /// 一次读取的结果落到会话上。成功时替换/追加已读取窗口；翻页成功才换窗口，
    /// 失败时旧页、条件、选择、滚动和暂存全部保留，偏移也留在上次成功的页上。
    fn apply_page_result(
        &mut self,
        offset: u64,
        replace: bool,
        landed_page: bool,
        read: Result<(SchemaDto, PageDto, Result<CountDto, String>), String>,
        cx: &mut Context<Self>,
    ) -> bool {
        let stale = self.session.stale_target();
        let (schema_dto, page, counted) = match read {
            Ok(read) => read,
            Err(failure) => {
                // 读取失败：旧页、条件、选择、滚动、暂存和偏移都不动。
                self.keep_sheet_after_failure(failure, stale);
                self.sync_table(cx);
                return false;
            }
        };
        let Ok(schema) = schema_from_dto(&schema_dto) else {
            self.keep_sheet_after_failure(i18n::text(self.locale, "grid.state.schemaUnknown"), stale);
            self.sync_table(cx);
            return false;
        };
        let columns = columns_from_schema(&schema, true);
        // 外键来自这次读取的真实结构；没有读到就清空，不沿用上一张表的键。
        self.session.set_foreign_keys(foreign_keys_of(&schema_dto));
        let truncated = page.truncated;
        let end = offset.saturating_add(page.rows.len() as u64);
        let full = if !truncated && (!page.rows.is_empty() || offset == 0) {
            i64::try_from(end).ok()
        } else {
            counted.ok().map(|item| item.matched).filter(|count| u64::try_from(*count).is_ok_and(|count| {
                if truncated { count > end } else { count <= end }
            }))
        };
        self.next_offset = end;
        self.schema_cache = Some((self.session.target.clone(), None, schema_dto.clone()));
        let rows = rows_from_page(&columns, &page);
        if replace {
            self.session.replace_fetched(columns, rows, offset, full, truncated, Some(schema));
            // 新的一页真的到了才换窗口；失败路径不动选择、滚动和偏移。
            if landed_page {
                self.session.reset_page_view();
            }
        } else {
            self.session.set_full_count(full);
            self.session.append_page(rows, full, truncated);
        }
        self.notice.clear();
        self.loading_more = false;
        self.fetching = false;
        self.sync_table(cx);
        true
    }

    /// 读取失败后决定保留还是丢弃：同一目标的刷新/追加/排序/筛选/翻页失败保留上次有效内容；
    /// 换了表又读失败，旧表快照不能留下来当新表数据。
    fn keep_sheet_after_failure(&mut self, message: String, stale: bool) {
        self.loading_more = false;
        self.fetching = false;
        self.pending_page = None;
        self.notice = message.clone();
        if stale {
            self.session.drop_stale_sheet();
        } else {
            self.session.keep_after_failure(&message);
        }
    }

    fn count_rows(&mut self, cx: &mut Context<Self>) {
        if self.fetching || self.counting.is_some() || self.session.mode != GridMode::Table { return; }
        let request = match self.session.page_request() { Ok(request) => count_of(&request), Err(error) => { self.notice = error; return; } };
        self.count_sequence = self.count_sequence.wrapping_add(1);
        let token = self.count_sequence;
        let scope = request.clone();
        self.counting = Some(token);
        self.count_scope = Some(scope.clone());
        self.notice = i18n::text(self.locale, "grid.count.running");
        let services = Arc::clone(&self.services);
        let (tx, rx) = tokio::sync::oneshot::channel();
        runtime().spawn(async move { let _ = tx.send(crate::application::grid::count(&services.connections, &request).await); });
        let owner = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let result = rx.await;
            cx.update(|cx| {
                let Some(owner) = owner.upgrade() else { return };
                owner.update(cx, |pane, cx| {
                    let current = pane.session.page_request().ok().map(|request| count_of(&request));
                    if pane.counting != Some(token) || current.as_ref() != Some(&scope) { return; }
                    pane.counting = None;
                    match result {
                        Ok(Ok(count)) => { pane.session.set_full_count(Some(count.matched)); pane.notice.clear(); }
                        Ok(Err(error)) => pane.notice = error,
                        Err(_) => pane.notice = i18n::text(pane.locale, "grid.count.failed"),
                    }
                    pane.sync_table(cx);
                    cx.notify();
                });
            });
        }).detach();
    }

    fn load_next_page(&mut self, cx: &mut Context<Self>) {
        if self.loading_more || self.fetching || self.session.mode != GridMode::Table || self.session.target.connection_id.is_empty() {
            return;
        }
        // 查询结果和没有更多行都不在这里继续读；网格从不重新执行 SQL。
        if self.session.continuation(false) != Continuation::More {
            return;
        }
        self.loading_more = true;
        self.pending_page = None;
        self.fetch_at(self.next_offset, false, cx);
    }

    fn schedule_locate(&mut self, token: u64, cx: &mut Context<Self>) {
        if self.scheduled == token || !self.live_target() {
            return;
        }
        let Ok(proof) = prove_single_table(&self.session.target.engine, self.session.sql_text()) else {
            return;
        };
        self.scheduled = token;
        let services = Arc::clone(&self.services);
        let request = TableRef {
            connection_id: self.session.target.connection_id.clone(),
            database: self.session.target.database.clone(),
            schema: proof.schema.clone().unwrap_or_else(|| self.session.target.schema.clone()),
            table: proof.table.clone(),
            engine: self.session.target.engine.clone(),
            include_foreign_keys: true,
        };
        let view = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            runtime().spawn(async move {
                let result = crate::application::grid::describe(&services.connections, &request).await;
                let _ = tx.send(result);
            });
            if let Ok(Ok(dto)) = rx.await {
                cx.update(|cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |this, cx| {
                            this.session.set_foreign_keys(foreign_keys_of(&dto));
                            if let Ok(schema) = schema_from_dto(&dto) {
                                if this.session.finish_locate(token, schema) {
                                    this.sync_table(cx);
                                }
                            }
                        });
                    }
                });
            }
        })
        .detach();
    }

    fn sync_table(&mut self, cx: &mut App) {
        let names = self.session.column_names();
        let widths = self.session.widths();
        let row_count = self.session.loaded_len();
        let loading = self.loading_more || self.fetching;
        let has_more = self.session.continuation(loading) == Continuation::More;
        let locale = self.locale;
        let writable = self.session.writable();
        let dirty = self.session.uncommitted();
        // 排序列用数据列下标传给表头；列顺序变化后重新按列名定位。
        let sort = self.session.sort_index();
        // 表格自己的高亮也要跟着稳定身份走：重排后按会话焦点重新指到同一行。
        // 事件是异步送达的，靠 apply_table_event 的相等判断避免回声改写选择。
        let focus = self.table_focus();
        // 高亮的唯一来源是会话选区：拖选、Shift/Ctrl、键盘扩展和行头选择的每一格都由它画底色。
        // 非连续选择是多块区域加减选的洞，格子逐格问自己算不算选中。
        let selected = SelectionPaint { regions: self.session.selection_regions(), holes: self.session.selection_holes() };
        // 外键入口只在适用行出现；读取完整值的入口只在还有预览值时出现。
        let fk = self.session.has_reference();
        let unresolved = self.session.unresolved_previews();
        // 隐藏列不交给表格画；冻结的前几列和行号列一起固定在左侧。
        let visible = self.session.visible_columns();
        let frozen = self.session.frozen_count();
        let hidden = self.session.hidden_columns();
        let has_selection = self.session.has_selection();
        let logical_count = names.len();
        let headers = (0..names.len()).filter_map(|column| self.session.column_header(column)).collect::<Vec<_>>();
        let (cell_dirty, row_dirty) = self
            .session
            .focus_cell()
            .map(|(row, column)| (self.session.cell_dirty_state(row, column) != session::CellDirty::Clean, row_is_dirty(&self.session, row)))
            .unwrap_or((false, false));
        let write_reason = if writable {
            String::new()
        } else {
            let key = self.session.readonly_reason();
            if key.is_empty() { i18n::text(locale, "grid.status.readonly") } else { i18n::text(locale, key) }
        };
        // 格子内边距只由当前密度决定；换密度时同步刷新，列宽测量读的也是这一份。
        let cell_pad = cell_padding(kit::Metrics::of(cx));
        // 写回表格焦点的格子记下来：表格随后发回的 SelectCell 是回声，不是选择变更。
        let mut echoed: Option<(usize, usize)> = None;
        self.table.update(cx, |state, cx| {
            state.delegate_mut().locale = locale;
            state.delegate_mut().cell_pad = cell_pad;
            state.delegate_mut().selected = selected;
            state.delegate_mut().writable = writable;
            state.delegate_mut().dirty = dirty;
            state.delegate_mut().columns = std::iter::once(("#".to_owned(), 56.0, true))
                .chain(visible.iter().enumerate().map(|(display, logical)| (names[*logical].clone(), widths[*logical], display < frozen)))
                .collect();
            state.delegate_mut().visible = visible;
            state.delegate_mut().logical_count = logical_count;
            state.delegate_mut().hidden = hidden;
            state.delegate_mut().has_selection = has_selection;
            state.delegate_mut().headers = headers;
            state.delegate_mut().cell_dirty = cell_dirty;
            state.delegate_mut().row_dirty = row_dirty;
            state.delegate_mut().write_reason = write_reason;
            state.delegate_mut().row_count = row_count;
            state.delegate_mut().sort = sort;
            state.delegate_mut().fk = fk;
            state.delegate_mut().unresolved = unresolved;
            state.delegate_mut().has_more = has_more;
            state.delegate_mut().loading = loading;
            state.refresh(cx);
            // 比较整份选择（行/列/单元格）而不只是 selected_cell：表格为行号列留下的列模式
            // 即使焦点没变也要复位，否则 # 整列会继续画激活底色。
            match focus {
                Some((row, column)) if state.selection() != TableSelection::Cell(row, column) => {
                    state.set_selected_cell(row, column, cx);
                    echoed = Some((row, column));
                }
                None if state.selection() != TableSelection::None => state.clear_selection(cx),
                _ => {}
            }
        });
        self.echo_cells.extend(echoed);
        // 回声表只用来认自己发出去的事件，留最近几条就够。
        if self.echo_cells.len() > 8 {
            let drop = self.echo_cells.len() - 8;
            self.echo_cells.drain(..drop);
        }
    }
}

/// 网格行高与表头高：都取当前密度的行高（规范 S7.2 紧凑 26 / 标准 32 / 宽松 40）。
/// 正文行、表头行和骨架占位行共用这一份值，换密度时三处一起变。
fn row_height(m: kit::Metrics) -> Pixels {
    px(m.row_h)
}

/// 单元格左右内边距：按密度取分栏内边距的一半（紧凑 5 / 标准 6 / 宽松 8），
/// 上下由行高与垂直居中决定，不再叠一层裸间距。列宽测量与自适应也读它。
fn cell_padding(m: kit::Metrics) -> gpui_kit::Edges<Pixels> {
    let horizontal = px(m.pane_pad() / 2.);
    gpui_kit::Edges { top: px(0.), bottom: px(0.), left: horizontal, right: horizontal }
}

/// 单元格的悬停提示：长文本截断后仍能看到整段值；大值/二进制指向查看面板；
/// NULL 与空字符串不给提示（它们本身已经有自己的画法）。
fn cell_tooltip(locale: Locale, kind: session::CellKind, text: &str) -> Option<SharedString> {
    match kind {
        session::CellKind::Text if !text.is_empty() => Some(SharedString::from(text.to_owned())),
        session::CellKind::Large | session::CellKind::Binary => Some(SharedString::from(i18n::text(locale, "grid.value.openFullHint"))),
        _ => None,
    }
}

fn dirty_mark(state: session::CellDirty) -> u8 {
    match state {
        session::CellDirty::Clean => 0,
        session::CellDirty::Inserted => 1,
        session::CellDirty::Updated => 2,
        session::CellDirty::Deleted => 3,
    }
}

fn row_is_dirty(grid: &SharedGrid, row: usize) -> bool {
    (0..grid.column_names().len()).any(|column| grid.cell_dirty_state(row, column) != session::CellDirty::Clean)
}

/// 结构里带回来的真实外键；没有读到时给空表，界面因此不会显示不适用的入口。
fn foreign_keys_of(dto: &SchemaDto) -> Vec<ForeignKeyInfo> {
    dto.foreign_keys
        .as_ref()
        .map(|keys| {
            keys.iter()
                .map(|key| ForeignKeyInfo {
                    name: key.name.clone(),
                    columns: key.columns.clone(),
                    ref_schema: key.ref_schema.clone(),
                    ref_table: key.ref_table.clone(),
                    ref_columns: key.ref_columns.clone(),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn count_of(request: &PageRequest) -> CountRequest {
    CountRequest {
        table: TableRef {
            connection_id: request.table.connection_id.clone(),
            database: request.table.database.clone(),
            schema: request.table.schema.clone(),
            table: request.table.table.clone(),
            engine: request.table.engine.clone(),
            include_foreign_keys: false,
        },
        filters: request
            .filters
            .iter()
            .map(|filter| FilterArg {
                column: filter.column.clone(),
                op: filter.op.clone(),
                value: filter.value.clone(),
                second: filter.second.clone(),
                values: filter.values.clone(),
            })
            .collect(),
        search: request.search.clone(),
    }
}

impl Render for GridPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 编辑请求在绘制阶段落地，才能创建输入状态、写入文本并把焦点交给它。
        // 记录面板的就地暂存和网格原位编辑一样，要等绘制阶段才拿得到 Window。
        if let Some((row, column)) = self.pending_record.take() {
            self.begin_record_edit(row, column, window, cx);
        }
        if let Some(pending) = self.pending_edit.take() {
            self.begin_edit(pending, window, cx);
        } else if std::mem::take(&mut self.pending_batch) {
            self.begin_batch_edit(window, cx);
        } else if std::mem::take(&mut self.pending_viewer) {
            // 行头双击等表格事件只记下意图，绘制阶段再打开查看并读取原值。
            let _ = self.command("grid-value-open", cx);
        } else if self.edit_target.is_none() && std::mem::take(&mut self.refocus) {
            window.focus(&self.grid_focus, cx);
        }
        // 行头/列头选择和表格回声只改会话，不一定走 sync_table；绘制前把矩形推给格子画法，
        // 否则高亮会落后复制和统计一步。只写字段、不 notify，本帧的表格紧接着就按新值画。
        let selected = SelectionPaint { regions: self.session.selection_regions(), holes: self.session.selection_holes() };
        if self.table.read(cx).delegate().selected != selected {
            self.table.update(cx, |state, _| state.delegate_mut().selected = selected);
        }
        // 单格编辑原位进行：输入框画在被编辑的格子里；选区设值仍用底部编辑栏。
        let editing = self
            .edit_target
            .as_ref()
            .filter(|target| target.cells.is_empty())
            .and_then(|target| self.session.row_index(&target.row_id).map(|row| (row, target.column)));
        if self.table.read(cx).delegate().editing != editing {
            self.table.update(cx, |state, cx| {
                state.delegate_mut().editing = editing;
                cx.notify();
            });
        }
        // 自适应列宽要用窗口量字宽：表格量完发出列宽事件，会话按逻辑列记下新宽度。
        if let Some(logical) = self.pending_fit.take() {
            if let Some(display) = self.session.logical_to_display(logical) {
                // 此时 GridPane 正在 render，delegate 不能再读 owner；先准备有界候选。
                let texts = self.session.fit_column_labels(logical);
                self.table.update(cx, |state, cx| {
                    state.delegate_mut().fit_labels = Some((logical, texts));
                    let _ = state.fit_col(display + 1, window, cx);
                    state.delegate_mut().fit_labels = None;
                });
            }
        }
        // 行号列的事件只把表格切进列/行模式就返回了；事件回调不能重入表格，这里在绘制前
        // 按会话焦点复位，本帧的 # 列、行头和键盘模式都回到单元格选择。
        if std::mem::take(&mut self.table_resync) {
            self.resync_table_selection(cx);
        }
        // 切窗口、点别的面板：焦点从网格内移到网格外时结束拖选，已选矩形保留。
        // 表格自己拿焦点也算在网格内，正常点击和方向键不会被误判成失焦。
        let focus_inside = self.grid_focus.is_focused(window) || self.table.read(cx).focus_handle(cx).is_focused(window);
        if self.press.is_some() && self.focus_inside && !focus_inside {
            self.press = None;
        }
        self.focus_inside = focus_inside;
        // 键盘焦点环：焦点在网格里、且最近一次输入来自键盘才画（与 kit 的 focus-visible 同一条判据），
        // 鼠标点击选格不画环，方向键/Tab/翻页之后画。环画在当前焦点格上，不是整个表格外面一圈。
        let ring = if focus_inside && window.last_input_was_keyboard() { self.session.focus_cell() } else { None };
        if self.table.read(cx).delegate().focus_ring != ring {
            self.table.update(cx, |state, cx| {
                state.delegate_mut().focus_ring = ring;
                cx.notify();
            });
        }
        let start = self.table.read(cx).delegate().visible_start;
        if self.table.read(cx).delegate().visible_seen {
            // 先把屏幕上真实放得下的行数交给会话：翻页步长、可见窗口和滚动钳制都用这一份，
            // 不再让会话里那份默认行数和实际窗口对不上。
            let visible_len = self.table.read(cx).delegate().visible_len;
            if visible_len > 0 && visible_len != self.session.viewport_rows() {
                self.session.set_viewport(visible_len);
            }
            // 滚动换行后指针下的格子已经不是拖选起点的邻居：结束拖拽，保留已选矩形。
            if start != self.visible_start {
                self.visible_start = start;
                self.press = None;
            }
            self.session.scroll_to(start);
        }
        let p = Palette::of(cx);
        // 行高、表头与工具条高度跟随外壳密度（规范 S7.2：网格行 紧凑 26 / 标准 32）。
        let m = kit::Metrics::of(cx);
        let entity = cx.entity();
        let range = SharedString::from(self.session.range_text());
        let banner = SharedString::from(self.session.banner());
        let loading = self.loading_more || self.fetching;
        // 各区域在各自不内联的函数里搭建：未优化构建不复用栈槽，整块写在一个函数里时单帧约 700 KB，
        // 网格又画在查询页/表数据页的布局递归深处，叠上记录视图会撑爆 Windows 主线程默认 1 MB 栈。
        self.sync_format_viewer(window, cx);
        let tools = self.render_tools(loading, &p, m, &entity, window, cx);
        let filters_strip = self.render_filters(&p, &entity, window, cx);

        let mut column = div()
            .id("shared-grid")
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(p.surface)
            .key_context("Grid")
            .track_focus(&self.grid_focus)
            // Tab / Shift+Tab：网格里逐格移动选择；走到边界就把这个键还给外壳的焦点遍历。
            .on_action(cx.listener(|this, _: &GridNextCell, window, cx| {
                this.step_cell(true, window, cx);
            }))
            .on_action(cx.listener(|this, _: &GridPrevCell, window, cx| {
                this.step_cell(false, window, cx);
            }))
            .on_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, window, cx| {
                let key = event.keystroke.key.as_str();
                let key = match key {
                    "arrowup" => "up",
                    "arrowdown" => "down",
                    "arrowleft" => "left",
                    "arrowright" => "right",
                    other => other,
                };
                if this.suppress_enter {
                    this.suppress_enter = false;
                    if key == "enter" {
                        cx.stop_propagation();
                        return;
                    }
                }
                // 文本输入接管字符、光标、选区、粘贴和撤销，这里只处理取消编辑。
                if this.edit_target.is_some() {
                    if key == "escape" {
                        cx.stop_propagation();
                        this.cancel_edit();
                        cx.notify();
                    }
                    return;
                }
                // 记录面板的就地暂存：Esc 取消，其它键交给字段输入框自己处理。
                if this.record_edit.is_some() {
                    if key == "escape" {
                        cx.stop_propagation();
                        this.cancel_record_edit(cx);
                    }
                    return;
                }
                if key == "escape" {
                    if this.session.preview_open() {
                        this.cancel_preview(cx);
                        this.refocus = true;
                    } else if this.session.viewer().is_some() {
                        // 关闭查看面板后键盘回到网格，方向和 Enter 仍然可用。
                        this.session.close_value();
                        this.refocus = true;
                        cx.notify();
                    } else {
                        this.session.clear_selection();
                        this.sync_table(cx);
                        this.table.update(cx, |state, cx| state.clear_selection(cx));
                    }
                    return;
                }
                let table_focused = this.table.read(cx).focus_handle(cx).is_focused(window);
                // 方向键、Home/End 和 PageUp/PageDown 都走同一条会话移动路径；滚动跟在焦点后面。
                if matches!(key, "up" | "down" | "left" | "right" | "home" | "end" | "pageup" | "pagedown") {
                    // 表格自身已移动过一次时不要再移动；Shift 没有绑定，由这里扩展选区。
                    if table_focused && !event.keystroke.modifiers.shift {
                        return;
                    }
                    this.session.move_key(key, event.keystroke.modifiers.shift);
                    this.sync_table(cx);
                    if let Some((row, _)) = this.session.focus_cell() {
                        this.table.update(cx, |state, cx| state.scroll_to_row(row, cx));
                    }
                } else if key == "enter" {
                    // Enter 是写入口（和快捷键帮助里的 "Enter / F2" 一致）：可写格直接进编辑，
                    // 只读格退化为查看完整值并说明原因。Ctrl/Cmd+Enter 才是显式的查看完整值。
                    cx.stop_propagation();
                    if event.keystroke.modifiers.secondary(cx) {
                        let _ = this.command("grid-value-open", cx);
                    } else {
                        let _ = this.command("edit-cell", cx);
                    }
                    cx.notify();
                } else if let Some(id) = grid_shortcut(key, event.keystroke.modifiers.secondary(cx), event.keystroke.modifiers.shift) {
                    cx.stop_propagation();
                    let _ = this.command(id, cx);
                    cx.notify();
                }
            }))
            .child(tools)
            .children(self.render_read_error(&p, m, &entity))
            .children(filters_strip)
            .child(div().id("grid-range").hidden().child(range))
            .child(div().id("grid-banner").hidden().child(banner));
        column = column.child(self.render_grid_body(loading, &p, m, &entity, cx));
        if let Some(bar) = self.render_edit_bar(&p, &entity) {
            column = column.child(bar);
        }
        column = column.children(self.render_value_viewer(&p, &entity, cx));
        if let Some(preview) = self.render_preview(&p, &entity, cx) {
            column = column.child(preview);
        }
        column
    }
}

impl GridPane {
    /// 把当前查看格交给 FormatViewer。同一格的草稿保留；完整值到达后才替换。
    #[inline(never)]
    fn sync_format_viewer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.session.viewer_state().cloned() else {
            self.format_viewer = None;
            self.format_subscription = None;
            self.format_stamp = None;
            return;
        };
        let Some(shown) = self.session.viewer() else {
            self.format_viewer = None;
            self.format_subscription = None;
            self.format_stamp = None;
            return;
        };
        let stamp = (state.row_id.clone(), state.column_name.clone(), state.phase, self.locale);
        if let (Some(viewer), Some(previous)) = (&self.format_viewer, self.format_stamp.clone()) {
            if previous.0 == stamp.0 && previous.1 == stamp.1 && previous.2 == stamp.2 && previous.3 == stamp.3 {
                return;
            }
            if previous.0 == stamp.0 && previous.1 == stamp.1 && previous.3 == stamp.3 && stamp.2 == CellPhase::Full && previous.2 != CellPhase::Full {
                let text = if shown.body.is_empty() { shown.label.clone() } else { shown.body };
                viewer.update(cx, |view, cx| view.replace_full(text, None, window, cx));
                self.format_stamp = Some(stamp);
                return;
            }
            if previous.0 == stamp.0 && previous.1 == stamp.1 && previous.2 == stamp.2 && previous.3 != stamp.3 {
                viewer.update(cx, |view, cx| view.set_locale(self.locale, window, cx));
                self.format_stamp = Some(stamp);
                return;
            }
        }
        let spec = self.viewer_spec(&state, &shown);
        let viewer = cx.new(|cx| FormatViewer::new(spec, window, cx));
        self.format_subscription = Some(cx.subscribe(&viewer, |this, _, event: &FormatViewerEvent, cx| match event {
            FormatViewerEvent::LoadFull => {
                let _ = this.command("grid-value-retry", cx);
            }
            FormatViewerEvent::Commit(text) => {
                let Some((row, column)) = this.session.viewer_cell() else {
                    return;
                };
                // 截断预览和两种信封都不能写回；非法 JSON 由暂存校验拦住，草稿留在查看器里。
                if this.session.needs_full(row, column) || text.contains('\u{1}') {
                    return;
                }
                let Some(row_id) = SharedGrid::row_id(&this.session, row).map(str::to_owned) else {
                    return;
                };
                let gate = this.session.stage_text_by_id(&row_id, column, Some(text.clone()));
                this.notice = this.session.gate_text(gate);
                this.sync_table(cx);
                cx.notify();
            }
        }));
        self.format_viewer = Some(viewer);
        self.format_stamp = Some(stamp);
    }

    fn viewer_spec(&self, state: &session::ViewerState, shown: &session::ShownCell) -> ViewerSpec {
        let (row, column) = self.session.viewer_cell().unwrap_or((0, 0));
        // open_value 在没有完整值缓存时一律标成 Preview。那只是“还没另读”，
        // 不是截断。只有真信封（\u{1}t / \u{1}b）才锁住编辑。
        let truncated = shown.preview || self.session.needs_full(row, column);
        let truncation = truncation_for_open_cell(
            state.phase,
            truncated,
            self.session.preview_total(row, column).unwrap_or(0),
            self.session.can_reread(row),
            &state.message,
        );
        let text = if shown.kind == session::CellKind::Null {
            String::new()
        } else if !shown.body.is_empty() {
            shown.body.clone()
        } else if state.phase == CellPhase::Full {
            shown.label.clone()
        } else {
            shown.body.clone()
        };
        ViewerSpec {
            text,
            column_hint: Some(state.column_name.clone()),
            type_hint: self.session.column_type(column).map(str::to_owned),
            bytes: None,
            truncation,
            mode: if self.session.write_reason(row, column).is_empty() { EditMode::Editable } else { EditMode::ReadOnly },
            locale: self.locale,
        }
    }

    /// 工具条：查找、查找范围、筛选、排序、翻页与范围、状态组和动作组。
    #[inline(never)]
    fn render_tools(&mut self, loading: bool, p: &Palette, m: kit::Metrics, entity: &Entity<GridPane>, window: &mut Window, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let locale = self.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let search = self.search_input(window, cx);
        if std::mem::take(&mut self.clear_search) {
            search.update(cx, |state, cx| state.set_value("", window, cx));
        }
        let table_mode = self.session.mode == GridMode::Table;
        let range = SharedString::from(self.session.range_text());
        // 无匹配、表为空、加载中、还有更多、已到末尾各自有说法，不用同一个空列表。
        // 读取失败已经由工具条下方的错误条说一次，状态栏不再重复同一段原因。
        let state_text = if self.session.read_failure().is_some() {
            SharedString::default()
        } else {
            SharedString::from(self.session.state_text(loading))
        };
        // 查询结果、没有目标表、连接已关闭或失联时，整表筛选的开关不该看起来可以用。
        // 这里同时把失去连接的整表筛选退回已读取行，原因随后一起画出来。
        let scope_block = self.sync_find_scope();
        let find_scope = self.session.find_scope();
        // 排序状态不依赖焦点：表头有方向标记，状态栏也写出列名和方向。
        let sort_text = self.session.sort_text();
        let sort_dir = self.focused_column_name().and_then(|name| self.session.sort_for(&name));
        let sort_icon = if sort_dir == Some(true) { IconName::ArrowDownWideNarrow } else { IconName::ArrowUpNarrowWide };
        let sort_label = match sort_dir {
            Some(true) => t("grid.action.sortDesc"),
            Some(false) => t("grid.action.sortAsc"),
            None => t("grid.action.sort"),
        };
        // 窄窗口（640）一行放不下时换行，而不是把右端的“预览变更”等按钮裁掉：
        // 最小高度仍是 tools_h，右侧动作组整组换到下一行并靠右。
        let mut tools = div()
            .id("grid-tools")
            .flex_none()
            .min_h(px(m.tools_h()))
            .py(px(3.))
            .px(px(10.))
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(4.))
            .border_b_1()
            .border_color(p.line)
            .child(div().w(px(200.)).child(kit::text_input("grid-search", &search, t("grid.action.search")).small().prefix(kit::icon(IconName::Search, 13., p.text_faint)).cleanable(true)))
            // 查找范围是一等状态：已读取行（本地）与整表（服务器筛选）不能混为一谈。
            // 不可用时按钮禁用，原因只放在悬停提示里，不常驻占一行。
            // 查找范围只在能切到整表筛选时出现；查询结果、没有目标表或断开时没有可选的另一档，
            // 不再常驻一个禁用按钮。
            .when(scope_block.is_none(), |this| {
                this.child(
                    div().id("grid-find-scope-hint").debug_selector(|| "grid-find-scope-hint".to_owned()).child(
                        kit::tool_button(
                            "grid-find-scope",
                            Some(IconName::Funnel),
                            if find_scope == FindScope::Server { t("grid.find.server") } else { t("grid.find.loaded") },
                            {
                                let entity = entity.clone();
                                move |_, _, cx| {
                                    entity.update(cx, |pane, cx| {
                                        let _ = pane.command("grid-find-scope", cx);
                                    });
                                }
                            },
                        ),
                    ),
                )
            })
            .child(kit::tool_button("grid-filter-add", Some(IconName::ListFilter), t("grid.action.filter"), {
                let entity = entity.clone();
                move |_, _, cx| {
                    entity.update(cx, |pane, cx| {
                        let _ = pane.command("grid-filter-new", cx);
                    });
                }
            }))
            .child(
                kit::icon_button("grid-sort", sort_icon, sort_label, {
                    let entity = entity.clone();
                    move |_, _, cx| {
                        entity.update(cx, |pane, cx| {
                            let _ = pane.command("grid-sort", cx);
                        });
                    }
                }),
            );
        // 表数据：翻页与“起–止 / 总数”放在一起；查询结果的行数由结果页签头负责，这里不重复。
        if table_mode {
            tools = tools
                .child(kit::vline(p))
                .child(grid_icon("grid-page-prev", IconName::ChevronLeft, t("grid.action.pagePrev"), entity.clone()))
                .child(div().id("grid-range-label").test_support().role(gpui_kit::Role::Button).cursor_pointer()
                    .tooltip_text(t("grid.count.hint")).px(px(4.)).text_size(px(TypeScale::SM)).text_color(p.text_muted).child(range.clone())
                    .on_click({ let entity = entity.clone(); move |_, _, cx| { entity.update(cx, |pane, cx| { let _ = pane.command("grid-count", cx); }); } }))
                .child(grid_icon("grid-page-next", IconName::ChevronRight, t("grid.action.pageNext"), entity.clone()));
        }
        if !state_text.is_empty() {
            tools = tools.child(div().px(px(6.)).max_w(px(260.)).truncate().text_size(px(TypeScale::SM)).text_color(p.text_muted).child(state_text.clone()));
        }
        // 当前排序列和方向写进状态栏：焦点移到别处后仍然看得出结果是按哪一列排的。
        if let Some(sorted) = sort_text {
            tools = tools.child(kit::pill(SharedString::from(sorted), p.accent_text, p.accent_soft));
        }
        tools.child(self.render_tools_status(p)).child(self.render_tools_actions(p, entity)).into_any_element()
    }

    /// 状态组（可写/只读、只读原因、操作反馈）占住中间的剩余空间并靠右，空间不够时文字截断。
    #[inline(never)]
    fn render_tools_status(&self, p: &Palette) -> gpui_kit::AnyElement {
        let locale = self.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let writable = self.session.writable();
        let mut status = div()
            .id("grid-tools-status")
            .flex_1()
            .min_w(px(120.))
            .flex()
            .items_center()
            .justify_end()
            .gap(px(4.));
        // 可编辑是默认状态，不再挂一个“可编辑”标签；只读时给一个带锁的小标签，
        // 具体原因放在悬停提示里，不常驻占一整段文字。
        let readonly_reason = self.session.readonly_reason().to_owned();
        if !writable {
            let reason = if readonly_reason.is_empty() { t("grid.status.readonly") } else { t(&readonly_reason) };
            status = status.child(
                div()
                    .id("grid-readonly-reason")
                    .debug_selector(|| "grid-readonly-reason".to_owned())
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .px(px(6.))
                    .h(px(20.))
                    .rounded(px(Radius::TAG))
                    .bg(p.hover)
                    .text_size(px(TypeScale::XS))
                    .text_color(p.text_muted)
                    .tooltip_text(reason)
                    .child(kit::icon(IconName::Lock, 11., p.text_muted))
                    .child(t("grid.status.readonly")),
            );
        }
        // 读取失败的原因已经由工具条下方的错误条说一次；状态区不再重复同一段话。
        if !self.notice.is_empty() && self.session.read_failure().is_none() {
            status = status.child(
                div().px(px(6.)).min_w(px(0.)).max_w(px(360.)).truncate().text_size(px(TypeScale::SM)).text_color(p.text_muted).child(SharedString::from(self.notice.clone())),
            );
        }
        status.into_any_element()
    }

    /// 动作组不收缩：一行放不下时整组换行，始终完整可点。
    #[inline(never)]
    fn render_tools_actions(&self, p: &Palette, entity: &Entity<GridPane>) -> gpui_kit::AnyElement {
        let locale = self.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let writable = self.session.writable();
        let dirty = self.session.uncommitted();
        let mut actions = div()
            .id("grid-tools-actions")
            .flex_none()
            .ml_auto()
            .flex()
            .items_center()
            .gap(px(4.))
            .child(grid_icon("copy-region", IconName::Copy, t("grid.menu.copySelection"), entity.clone()))
            .child(grid_icon("grid-record-toggle", IconName::PanelRight, t("grid.record.open"), entity.clone()));
        if writable {
            actions = actions
                .child(kit::vline(p))
                .child(grid_icon("grid-insert", IconName::Plus, t("grid.action.insert"), entity.clone()))
                .child(grid_icon("grid-clone-row", IconName::Copy, t("grid.write.clone"), entity.clone()))
                .child(grid_icon("grid-selection-null", IconName::X, t("grid.write.setNull"), entity.clone()))
                .child(grid_icon("grid-selection-set", IconName::Pencil, t("grid.write.setValue"), entity.clone()))
                .child(grid_icon("mark-delete", IconName::Trash, t("grid.menu.markDelete"), entity.clone()));
            let linked = self.session.has_reference();
            let jump = div().id("grid-fk-jump-hint").debug_selector(|| "grid-fk-jump-hint".to_owned()).child(
                grid_icon("grid-fk-jump", IconName::Link, t("grid.fk.jump"), entity.clone()).disabled(!linked),
            );
            actions = actions.child(if linked { jump } else { jump.tooltip_text(t("grid.fk.none")) });
        }
        let summary = self.session.staged_summary();
        if summary.inserts + summary.updates + summary.deletes > 0 {
            actions = actions.child(self.render_staged_tools(p, entity));
        }
        if writable || dirty {
            let preview = {
                let entity = entity.clone();
                move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                    entity.update(cx, |pane, cx| {
                        let _ = pane.command("grid-preview", cx);
                    });
                }
            };
            actions = actions.child(if dirty {
                kit::primary_button("grid-preview", Some(IconName::Eye), t("grid.action.preview"), preview).into_any_element()
            } else {
                kit::tool_button("grid-preview", Some(IconName::Eye), t("grid.action.preview"), preview).into_any_element()
            });
        }
        actions.into_any_element()
    }

    /// 暂存计数和单格/单行撤销。有未提交变更时才出现。
    #[inline(never)]
    fn render_staged_tools(&self, p: &Palette, entity: &Entity<GridPane>) -> gpui_kit::AnyElement {
        let locale = self.locale;
        let summary = self.session.staged_summary();
        let count = |key: &str, value: usize| SharedString::from(i18n::fill(&i18n::text(locale, key), &[("count", &value.to_string())]));
        div()
            .id("grid-staged-summary")
            .debug_selector(|| "grid-staged-summary".to_owned())
            .flex_none()
            .flex()
            .items_center()
            .gap(px(4.))
            // 只显示非零的计数；撤销、定位、丢弃是图标按钮，文字放在悬停提示里。
            .when(summary.inserts > 0, |this| this.child(kit::pill(count("grid.write.stagedInserts", summary.inserts), p.ok, p.ok_soft)))
            .when(summary.updates > 0, |this| this.child(kit::pill(count("grid.write.stagedUpdates", summary.updates), p.warn, p.warn_soft)))
            .when(summary.deletes > 0, |this| this.child(kit::pill(count("grid.write.stagedDeletes", summary.deletes), p.danger, p.danger_soft)))
            .child(kit::icon_button("grid-jump-staged", IconName::LocateFixed, SharedString::from(i18n::text(locale, "grid.write.jumpStaged")), pane_click(entity, "grid-jump-staged")))
            .child(kit::icon_button("grid-undo-cell", IconName::Undo2, SharedString::from(i18n::text(locale, "grid.write.undoCell")), pane_click(entity, "grid-undo-cell")))
            .child(kit::icon_button("grid-undo-row", IconName::RotateCcw, SharedString::from(i18n::text(locale, "grid.write.undoRow")), pane_click(entity, "grid-undo-row")))
            .child(kit::icon_button("grid-discard", IconName::ListRestart, SharedString::from(i18n::text(locale, "grid.write.discardAll")), pane_click(entity, "grid-discard")))
            .child(kit::vline(p))
            .into_any_element()
    }

    /// 筛选条：每条条件可辨认、可换条件、可改值、可单删；非法原因和全清也在这里。
    #[inline(never)]
    fn render_filters(&mut self, p: &Palette, entity: &Entity<GridPane>, window: &mut Window, cx: &mut Context<Self>) -> Option<gpui_kit::AnyElement> {
        let locale = self.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let unapplied = self.session.unapplied_filters();
        let drafts = self.session.filters().to_vec();
        let filter_error = self.session.filter_error().map(str::to_owned);
        if drafts.is_empty() && filter_error.is_none() {
            return None;
        }
        self.ensure_filter_inputs(window, cx);
        let mut strip = div()
            .id("grid-filters")
            .flex_none()
            .px(px(10.))
            .py(px(3.))
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(6.))
            .border_b_1()
            .border_color(p.line)
            .bg(p.chrome)
            .child(div().id("grid-filter-title").tooltip_text(t("grid.filter.title")).child(kit::icon(IconName::Funnel, 13., p.text_muted)));
        for (index, draft) in drafts.iter().enumerate() {
            // 条件之间是“且”：写出来，避免误以为是“或”。
            if index > 0 {
                strip = strip.child(div().text_size(px(TypeScale::XS)).font_weight(gpui_kit::FontWeight::MEDIUM).text_color(p.text_faint).child(t("grid.filter.and")));
            }
            strip = strip.child(self.render_filter_row(index, draft, p, entity));
        }
        let apply = {
            let entity = entity.clone();
            move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                entity.update(cx, |pane, cx| {
                    let _ = pane.command("grid-filter-apply", cx);
                });
            }
        };
        let clear = {
            let entity = entity.clone();
            move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                entity.update(cx, |pane, cx| {
                    let _ = pane.command("grid-filter-clear", cx);
                });
            }
        };
        let add = {
            let entity = entity.clone();
            move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                entity.update(cx, |pane, cx| {
                    let _ = pane.command("grid-filter-new", cx);
                });
            }
        };
        strip = strip
            .child(kit::icon_button("grid-filter-add-strip", IconName::Plus, t("grid.filter.add"), add))
            // 有未应用的条件时"应用"变成主按钮：这里才是让条件真正生效的一步。
            .child(if unapplied {
                kit::primary_button("grid-filter-apply", Some(IconName::Check), t("grid.filter.apply"), apply.clone()).into_any_element()
            } else {
                kit::icon_button("grid-filter-apply", IconName::Check, t("grid.filter.apply"), apply).into_any_element()
            })
            .child(kit::icon_button("grid-filter-clear", IconName::X, t("grid.filter.clearAll"), clear));
        if let Some(error) = filter_error {
            strip = strip.child(div().max_w(px(360.)).truncate().text_size(px(TypeScale::SM)).text_color(p.danger).child(SharedString::from(error)));
        }
        Some(strip.into_any_element())
    }

    #[inline(never)]
    fn render_filter_row(&self, index: usize, draft: &session::FilterDraft, p: &Palette, entity: &Entity<GridPane>) -> gpui_kit::AnyElement {
        let locale = self.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let remove = {
            let entity = entity.clone();
            move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                entity.update(cx, |pane, cx| pane.remove_filter(index, cx));
            }
        };
        // 列：下拉列出全部列，换列不丢掉已经输入的值。
        let names = self.session.column_names();
        let column_entity = entity.clone();
        let column_picker = Button::new(SharedString::from(format!("grid-filter-column-{index}")))
            .ghost()
            .xsmall()
            .label(SharedString::from(draft.column.clone()))
            .dropdown_caret(true)
            .dropdown_menu(move |mut menu, _, _| {
                for name in names.clone() {
                    let entity = column_entity.clone();
                    let picked = name.clone();
                    menu = menu.item(PopupMenuItem::new(name).on_click(move |_, _, cx| {
                        entity.update(cx, |pane, cx| pane.pick_filter_column(index, &picked, cx));
                    }));
                }
                menu
            });
        // 运算符：只列出这一列类型适用的运算符，当前项打勾。
        let ops = self.session.filter_ops_for(&draft.column);
        let current = draft.op.clone();
        let op_entity = entity.clone();
        let op_picker = Button::new(SharedString::from(format!("grid-filter-op-{index}")))
            .ghost()
            .xsmall()
            .label(SharedString::from(self.session.op_label(&draft.op)))
            .dropdown_caret(true)
            .dropdown_menu(move |mut menu, _, _| {
                for op in ops.clone() {
                    let entity = op_entity.clone();
                    let label = i18n::text(locale, &format!("grid.op.{op}"));
                    menu = menu.item(PopupMenuItem::new(label).checked(op == current).on_click(move |_, _, cx| {
                        entity.update(cx, |pane, cx| pane.pick_filter_op(index, op, cx));
                    }));
                }
                menu
            });
        let mut row = div()
            .id(SharedString::from(format!("grid-filter-chip-{index}")))
            .flex()
            .items_center()
            .gap(px(2.))
            .pl(px(2.))
            .pr(px(2.))
            .h(px(26.))
            .rounded(px(Radius::CONTROL))
            .border_1()
            // 正在编辑、尚未应用的条件用强调边框标出来，避免把旧结果当成新条件的结果。
            .border_color(if draft.dirty { p.accent } else { p.line })
            .bg(p.raised)
            .child(column_picker)
            .child(op_picker);
        if draft.dirty {
            row = row.child(
                div()
                    .id(SharedString::from(format!("grid-filter-unapplied-{index}")))
                    .debug_selector(|| "grid-filter-unapplied".to_owned())
                    .mx(px(2.))
                    .size(px(6.))
                    .rounded_full()
                    .bg(p.accent)
                    .tooltip_text(t("grid.filter.unapplied")),
            );
        }
        let needs_value = !matches!(draft.op.as_str(), "null" | "notnull" | "empty");
        if let Some(inputs) = self.filter_inputs.get(index) {
            if needs_value {
                if let Some(state) = inputs.value.as_ref() {
                    let placeholder = if draft.op == "in" { t("grid.filter.valuePlaceholder") } else { t("grid.filter.value") };
                    row = row.child(
                        div()
                            .w(px(if draft.op == "in" { 180. } else { 120. }))
                            .child(kit::text_input(SharedString::from(format!("grid-filter-value-{index}")), state, placeholder).appearance(false).xsmall()),
                    );
                }
            }
            if draft.op == "between" {
                if let Some(state) = inputs.second.as_ref() {
                    row = row
                        .child(div().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(t("grid.filter.and")))
                        .child(
                            div()
                                .w(px(120.))
                                .child(kit::text_input(SharedString::from(format!("grid-filter-upper-{index}")), state, t("grid.filter.upper")).appearance(false).xsmall()),
                        );
                }
            }
        }
        row.child(kit::icon_button(SharedString::from(format!("grid-filter-remove-{index}")), IconName::X, t("grid.filter.remove"), remove).xsmall())
            .into_any_element()
    }

    /// 网格主体：表格（可带右侧记录视图）、加载中或空结果。
    #[inline(never)]
    fn render_grid_body(&self, loading: bool, p: &Palette, m: kit::Metrics, entity: &Entity<GridPane>, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let locale = self.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        // 有已读取的行才画表格；空表格不保留表头骨架，交给下面的三态（需求 R4）。
        if !self.session.column_names().is_empty() && self.session.loaded_len() > 0 {
            let table = self.table.clone();
            // 行高、表头高与格子内边距只由当前密度决定，网格不再自带一套尺寸常量。
            let data_table = DataTable::new(&table)
                .with_size(gpui_kit::component::Size::Size(row_height(m)))
                .header_row_height(row_height(m))
                .stripe(true)
                .accessibility_label("shared-grid");
            let content = if self.record_open {
                let width = self.side_width.clamp(RECORD_SIDE_MIN, RECORD_SIDE_MAX);
                let dragging = self.record_dragging;
                let reset = entity.clone();
                // 纵向记录视图靠右，和网格共用同一个焦点行。左缘拖拽调宽，双击回到 340。
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .on_drag_move(cx.listener(|this, event: &DragMoveEvent<kit::ResizeDrag>, _, cx| {
                        if event.drag(cx).key.as_ref() != "grid-record-resize" {
                            return;
                        }
                        let width = f32::from(event.bounds.right() - event.event.position.x);
                        this.side_width = width.clamp(RECORD_SIDE_MIN, RECORD_SIDE_MAX);
                        this.record_dragging = true;
                        cx.notify();
                    }))
                    .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| {
                        if this.record_dragging {
                            this.record_dragging = false;
                            cx.notify();
                        }
                    }))
                    .child(div().flex_1().min_w(px(0.)).h_full().child(data_table))
                    .child(kit::resize_handle("grid-record-resize", t("grid.record.open"), dragging, p, move |_, _, cx| {
                        reset.update(cx, |pane, cx| {
                            pane.side_width = RECORD_SIDE_DEFAULT;
                            cx.notify();
                        });
                    }))
                    .child(record::render_record(
                        record::record_view(&self.session),
                        locale,
                        p,
                        m,
                        cx.theme().mono_font_family.clone(),
                        entity,
                        width,
                        record::RecordEdit {
                            column: self.record_edit,
                            input: Some(self.record_input.clone()),
                            writable: self.session.writable(),
                            selected: self.selection_size(),
                            scope: self.record_scope_text(),
                        },
                        self.format_viewer.clone(),
                    ))
            } else {
                div().flex_1().min_h(px(0.)).child(data_table)
            };
            // 横向滚动只发生在网格正文里：容器收窄到可用宽度并裁掉溢出，
            // 列再宽也只是表格自己的横向滚动条，不会把查询页/表数据页撑宽。
            return div()
                .id("grid-body")
                .debug_selector(|| "grid-body".to_owned())
                .test_support()
                .relative()
                .flex()
                .flex_col()
                .flex_1()
                .min_h(px(0.))
                .min_w(px(0.))
                .overflow_hidden()
                .on_mouse_down(gpui_kit::MouseButton::Left, cx.listener(|this, _, window, cx| {
                    // 点中格子时把键盘交给网格本身，之后的方向键、Enter 和 F2 才有效。
                    window.focus(&this.grid_focus, cx);
                }))
                // 在表格外松开（拖到表格下方空白）也结束拖选，不持续跟手。
                .on_mouse_up_out(gpui_kit::MouseButton::Left, cx.listener(|this, _, _, cx| {
                    if this.cancel_drag() {
                        cx.notify();
                    }
                }))
                .child(content)
                .into_any_element();
        }
        if loading {
            // 首屏、换表与替换读取还没回来：按当前密度排骨架行（规范 S8.5 的加载优先于空），
            // 行数取自可视行数，密度一换骨架行高也跟着换，不再用整块转圈盖住内容区。
            let rows = self.session.viewport_rows().clamp(4, 10);
            return div()
                .id("grid-loading")
                .debug_selector(|| "grid-loading".to_owned())
                .flex_1()
                .min_h(px(0.))
                .min_w(px(0.))
                .flex()
                .flex_col()
                .child(kit::skeleton_state(rows, &m, p))
                .into_any_element();
        }
        // 一次都没读到、也没有可保留的结果：用统一错误态替换内容区，并给出能立刻做的纠正动作。
        // 已经读到行时的失败不走这里：正文保留上次结果，只在工具栏下方挂一条错误条。
        if self.session.read_failure().is_some() {
            let message = SharedString::from(self.session.read_failure().unwrap_or_default().to_owned());
            let retry = {
                let entity = entity.clone();
                move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                    entity.update(cx, |pane, cx| {
                        let _ = pane.command("grid-retry", cx);
                    });
                }
            };
            let body = if self.session.mode == GridMode::Table {
                kit::error_state_with_action(
                    t("grid.state.readErrorTitle"),
                    message,
                    kit::secondary_button("grid-retry", Some(IconName::RotateCcw), t("grid.action.retry"), retry).into_any_element(),
                    p,
                )
                .into_any_element()
            } else {
                kit::error_state(t("grid.state.readErrorTitle"), message, p).into_any_element()
            };
            return div()
                .id("grid-error")
                .debug_selector(|| "grid-error".to_owned())
                .test_support()
                .flex_1()
                .min_h(px(0.))
                .min_w(px(0.))
                .flex()
                .flex_col()
                .child(body)
                .into_any_element();
        }
        // 空网格按语义分三类：无命中（调条件）、表里没有行（插入）、查询没有返回行（改语句）。
        // 三类文案不同、下一步不同，都不保留表头骨架。
        let emptiness = self.session.emptiness();
        let body = match emptiness {
            SheetEmptiness::NoMatch => {
                let clear = {
                    let entity = entity.clone();
                    move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                        entity.update(cx, |pane, cx| {
                            let _ = pane.command("grid-filter-clear", cx);
                        });
                    }
                };
                kit::no_match_state_with_action(
                    IconName::Search,
                    t("grid.state.noMatch"),
                    t("grid.state.noMatchDetail"),
                    kit::tool_button("grid-condition-clear", Some(IconName::ListRestart), t("grid.state.clearConditions"), clear).into_any_element(),
                    p,
                )
                .into_any_element()
            }
            SheetEmptiness::TableEmpty | SheetEmptiness::Rows => {
                let detail = t("grid.state.tableEmptyDetail");
                if self.session.writable() {
                    let insert = {
                        let entity = entity.clone();
                        move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                            entity.update(cx, |pane, cx| {
                                let _ = pane.command("grid-insert", cx);
                            });
                        }
                    };
                    kit::empty_state_with_action(
                        IconName::Table,
                        t("ui.grid.empty"),
                        detail,
                        kit::tool_button("grid-empty-insert", Some(IconName::Plus), t("grid.action.insert"), insert).into_any_element(),
                        p,
                    )
                    .into_any_element()
                } else {
                    kit::empty_state(IconName::Table, t("ui.grid.empty"), detail, p).into_any_element()
                }
            }
            SheetEmptiness::ResultEmpty => {
                kit::empty_state(IconName::Table, t("ui.grid.emptyResult"), t("grid.state.resultEmptyDetail"), p).into_any_element()
            }
        };
        div()
            .debug_selector(|| "grid-empty".to_owned())
            .flex_1()
            .min_h(px(0.))
            .min_w(px(0.))
            .flex()
            .flex_col()
            .child(body)
            .into_any_element()
    }

    /// 网格自己的 Tab / Shift+Tab：逐格移动选择并让滚动跟过去；
    /// 走到首格或末格时不再拦截，把 Tab 交还给外壳的焦点遍历，键盘用户不会被关在网格里。
    fn step_cell(&mut self, forward: bool, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.session.move_tab(forward) {
            cx.propagate();
            return;
        }
        self.sync_table(cx);
        if let Some((row, _)) = self.session.focus_cell() {
            self.table.update(cx, |state, cx| state.scroll_to_row(row, cx));
        }
        cx.notify();
    }

    /// 读取失败但上次结果还在时的一条错误条：原因只说一次、只出现在一处，并带一个重试动作。
    /// 失败不清空旧页，所以这条挂在工具条下面，不替换正文；一次都没读到时用整块错误态。
    #[inline(never)]
    fn render_read_error(&self, p: &Palette, m: kit::Metrics, entity: &Entity<GridPane>) -> Option<gpui_kit::AnyElement> {
        if self.session.column_names().is_empty() {
            return None;
        }
        let message = self.session.read_failure()?.to_owned();
        let locale = self.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let retry = {
            let entity = entity.clone();
            move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                entity.update(cx, |pane, cx| {
                    let _ = pane.command("grid-retry", cx);
                });
            }
        };
        let mut strip = div()
            .id("grid-read-error")
            .debug_selector(|| "grid-read-error".to_owned())
            .test_support()
            .flex_none()
            .min_h(px(m.strip_h()))
            .px(px(m.pane_pad()))
            .py(px(3.))
            .flex()
            .items_center()
            .gap(px(8.))
            // 底色与描边取错误色，和 kit 的错误态同一个语义，只是压成一条不占内容区的横条。
            .bg(p.danger_soft)
            .border_b_1()
            .border_color(p.danger)
            .child(kit::icon(IconName::CircleAlert, 14., p.danger))
            .child(div().flex_none().text_size(px(TypeScale::SM)).text_color(p.danger).child(t("grid.state.readErrorTitle")))
            .child(
                div()
                    .id("grid-read-error-detail")
                    .debug_selector(|| "grid-read-error-detail".to_owned())
                    .test_support()
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .text_size(px(TypeScale::SM))
                    .text_color(p.text_muted)
                    .tooltip_text(SharedString::from(message.clone()))
                    .child(SharedString::from(message)),
            );
        if self.session.mode == GridMode::Table {
            strip = strip.child(kit::tool_button("grid-retry", Some(IconName::RotateCcw), t("grid.action.retry"), retry));
        }
        Some(strip.into_any_element())
    }

    /// 单元格编辑栏；选区设值时说清会写到几格。
    #[inline(never)]
    fn render_edit_bar(&self, p: &Palette, entity: &Entity<GridPane>) -> Option<gpui_kit::AnyElement> {
        let target = self.edit_target.as_ref().filter(|target| !target.cells.is_empty())?;
        let locale = self.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let hint = if target.cells.is_empty() {
            t("shell.shortcuts.editCell")
        } else {
            SharedString::from(i18n::fill(&i18n::text(locale, "grid.write.setValueHint"), &[("count", &target.cells.len().to_string())]))
        };
        let input = self.edit_input.clone();
        let set_null = {
            let entity = entity.clone();
            move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                entity.update(cx, |pane, cx| {
                    let _ = pane.command("grid-set-null", cx);
                });
            }
        };
        Some(
            div()
                .id("grid-edit")
                .flex_none()
                .px(px(12.))
                .py(px(8.))
                .flex()
                .items_center()
                .gap(px(10.))
                .bg(p.accent_soft)
                .border_t_1()
                .border_color(p.accent)
                .child(kit::icon(IconName::Pencil, 14., p.accent_text))
                .child(div().flex_1().child(kit::text_input("grid-edit-input", &input, hint).small()))
                .child(kit::tool_button("grid-set-null", None, t("grid.value.null"), set_null))
                .child(kit::kbd("Enter", p))
                .child(kit::kbd("Esc", p))
                .into_any_element(),
        )
    }

    /// 单元格值查看面板与其说明行；没有打开查看时为空。
    #[inline(never)]
    fn render_value_viewer(&self, p: &Palette, entity: &Entity<GridPane>, cx: &mut Context<Self>) -> Vec<gpui_kit::AnyElement> {
        let Some(shown) = self.session.viewer() else {
            return Vec::new();
        };
        let locale = self.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let state = self.session.viewer_state().cloned();
        let phase = state.as_ref().map(|state| state.phase).unwrap_or(CellPhase::Preview);
        // 预览、加载、完整和错误各有自己的标记：预览绝不被读成原值。
        let tag = match phase {
            CellPhase::Preview => t("grid.value.previewTag"),
            CellPhase::Loading => t("grid.value.loadingTag"),
            CellPhase::Full => t("grid.value.fullTag"),
            CellPhase::Error => t("grid.value.errorTag"),
        };
        let body = match phase {
            CellPhase::Loading => t("grid.value.loadingFull"),
            CellPhase::Error => SharedString::from(state.as_ref().map(|state| state.message.clone()).unwrap_or_default()),
            _ => SharedString::from(if shown.body.is_empty() { shown.label.clone() } else { shown.body.clone() }),
        };
        let title = state
            .as_ref()
            .map(|state| SharedString::from(format!("{}.{}", self.session.banner(), state.column_name)))
            .unwrap_or_else(|| t("grid.value.copyValue"));
        let copy = {
            let entity = entity.clone();
            move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                entity.update(cx, |pane, cx| {
                    let _ = pane.command("grid-value-copy", cx);
                });
            }
        };
        let close = {
            let entity = entity.clone();
            move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                entity.update(cx, |pane, cx| {
                    let _ = pane.command("grid-value-close", cx);
                });
            }
        };
        let embed = self.format_viewer.clone().filter(|_| !self.record_open);
        let mut panel = div()
            .id("grid-value")
            .debug_selector(|| format!("grid-value-{phase:?}").to_lowercase())
            .role(gpui_kit::Role::Dialog)
            .aria_label(t("grid.value.copyValue"))
            .flex_none()
            .max_h(px(if embed.is_some() { 460. } else { 180. }))
            .overflow_y_scroll()
            .px(px(14.))
            .py(px(10.))
            .border_t_1()
            .border_color(p.line)
            .bg(p.chrome)
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(kit::eyebrow(tag, p))
                    .child(div().flex_1().truncate().text_size(px(TypeScale::XS)).text_color(p.text_muted).child(title))
                    .child(kit::icon_button("grid-value-copy", IconName::Copy, t("grid.value.copyValue"), copy))
                    .child(kit::icon_button("grid-value-close", IconName::X, t("grid.value.close"), close)),
            );
        if let Some(viewer) = embed {
            panel = panel.child(div().h(px(280.)).min_h(px(180.)).child(viewer));
        } else {
            panel = panel.child(div().font_family(cx.theme().mono_font_family.clone()).text_size(px(TypeScale::SM)).child(body));
        }
        if phase == CellPhase::Error {
            let retry = {
                let entity = entity.clone();
                move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                    entity.update(cx, |pane, cx| {
                        let _ = pane.command("grid-value-retry", cx);
                    });
                }
            };
            panel = panel.child(kit::tool_button("grid-value-retry", None, t("grid.value.retry"), retry));
        }
        let mut items = vec![panel.into_any_element()];
        let reason = self.session.viewer_reason();
        if !reason.is_empty() {
            items.push(
                div()
                    .id("grid-value-reason")
                    .flex_none()
                    .px(px(14.))
                    .pb(px(8.))
                    .bg(p.chrome)
                    .text_size(px(TypeScale::SM))
                    .text_color(p.text_muted)
                    .child(SharedString::from(reason))
                    .into_any_element(),
            );
        }
        items
    }

    /// 变更预览浮层：写入前列出将执行的 SQL，确认或取消。
    #[inline(never)]
    fn render_preview(&self, p: &Palette, entity: &Entity<GridPane>, cx: &mut Context<Self>) -> Option<gpui_kit::AnyElement> {
        if !self.session.preview_open() {
            return None;
        }
        let locale = self.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let sql = self.session.preview_sql().join("\n");
        Some(
            div()
                .absolute()
                .inset_0()
                .bg(gpui_kit::hsla(0., 0., 0., 0.25))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .id("grid-preview")
                        .role(gpui_kit::Role::Dialog)
                        .aria_label(t("grid.preview.title"))
                        .occlude()
                        .w(px(620.))
                        .max_w(relative(0.92))
                        .p(px(18.))
                        .flex()
                        .flex_col()
                        .gap(px(12.))
                        .bg(p.raised)
                        .border_1()
                        .border_color(p.line)
                        .rounded(px(Radius::PANEL))
                        .shadow_lg()
                        .child(kit::title(t("grid.preview.title"), TypeScale::LG, p))
                        .child(kit::muted(SharedString::from(self.session.banner()), p))
                        .child(
                            div()
                                .id("grid-preview-sql")
                                .max_h(px(280.))
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
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap(px(8.))
                                .child(grid_button("grid-preview-cancel", false, t("grid.preview.cancel"), entity.clone()))
                                .child(grid_button("grid-preview-confirm", true, t("grid.preview.confirm"), entity.clone())),
                        ),
                )
                .into_any_element(),
        )
    }

    fn search_input(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        if let Some(state) = &self.search {
            if self.search_hint_locale != self.locale {
                let hint = SharedString::from(i18n::text(self.locale, "ui.grid.searchPlaceholder"));
                state.update(cx, |state, cx| state.set_placeholder(hint, window, cx));
                self.search_hint_locale = self.locale;
            }
            return state.clone();
        }
        let placeholder = SharedString::from(i18n::text(self.locale, "ui.grid.searchPlaceholder"));
        let state = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let subscription = cx.subscribe(&state, |this, state, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change | InputEvent::PressEnter { .. }) {
                this.search_text = state.read(cx).value().to_string();
                let _ = this.command("grid-search", cx);
                cx.notify();
            }
        });
        self.search_subscription = Some(subscription);
        self.search = Some(state.clone());
        state
    }
}

fn pane_click(entity: &Entity<GridPane>, id: &'static str) -> impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut App) + 'static {
    let entity = entity.clone();
    move |_, _, cx| {
        entity.update(cx, |pane, cx| {
            let _ = pane.command(id, cx);
        });
    }
}

fn grid_icon(id: &'static str, icon: IconName, label: SharedString, entity: Entity<GridPane>) -> gpui_kit::component::button::Button {
    kit::icon_button(id, icon, label, move |_, _, cx| {
        entity.update(cx, |pane, cx| {
            let _ = pane.command(id, cx);
        });
    })
}

fn grid_button(id: &'static str, primary: bool, label: SharedString, entity: Entity<GridPane>) -> impl IntoElement {
    let click = move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
        entity.update(cx, |pane, cx| {
            if id.ends_with("confirm") {
                pane.confirm(cx);
            } else {
                pane.cancel_preview(cx);
            }
        });
    };
    if primary {
        kit::primary_button(id, Some(IconName::Check), label, click)
    } else {
        kit::secondary_button(id, None, label, click)
    }
}

#[derive(Default)]
struct SheetDelegate {
    columns: Vec<(String, f32, bool)>,
    row_count: usize,
    /// 从 GridPane 内发起自适应时使用，避免读取正在更新的父实体；调用后立即释放。
    fit_labels: Option<(usize, Vec<String>)>,
    /// 当前排序列（数据列下标）与方向。表头据此标出 ↑/↓，与焦点无关。
    sort: Option<(usize, bool)>,
    has_more: bool,
    loading: bool,
    /// 会话选区的区域与减选的洞（行起、列起、行止、列止，数据列下标）。
    /// 格子据此逐格画高亮，和复制、统计读的是同一份；行号列属于整行。
    selected: SelectionPaint,
    visible_start: usize,
    visible_seen: bool,
    owner: Option<gpui_kit::WeakEntity<GridPane>>,
    locale: Locale,
    writable: bool,
    dirty: bool,
    /// 焦点格适用外键导航（列有真实外键且行已定位）；菜单项只在适用行出现。
    fk: bool,
    /// 当前窗口里还没读到原值的预览格子数：大于零才给"读取完整值"入口。
    unresolved: usize,
    /// 界面第 n 个数据列对应的逻辑列；隐藏列不在里面。
    visible: Vec<usize>,
    /// 逻辑列总数：选区矩形和"整行点亮行号"都按逻辑列算。
    logical_count: usize,
    /// 被隐藏的列（逻辑列号、列名），表头菜单用它给出恢复入口。
    hidden: Vec<(usize, String)>,
    /// 有没有选区：空选区时"复制为"禁用。
    has_selection: bool,
    /// 与可见数据列对齐的表头：类型缩写、主键、外键。
    headers: Vec<session::ColumnHeader>,
    cell_dirty: bool,
    row_dirty: bool,
    /// 只读时写操作禁用的原因，悬停时显示。
    write_reason: String,
    /// 正在原位编辑的格子（已读取行下标、逻辑列）；输入框直接画在这一格里。
    editing: Option<(usize, usize)>,
    edit_input: Option<Entity<InputState>>,
    /// 单元格左右内边距：按当前密度发布，正文格与表头格共用同一个值。
    cell_pad: gpui_kit::Edges<Pixels>,
    /// 键盘焦点所在的格子（已读取行下标、逻辑列）：只有键盘刚动过、焦点还在网格里时才画焦点环。
    focus_ring: Option<(usize, usize)>,
    /// 最近一帧屏幕上真实放得下的行数；会话据此更新翻页步长和可见窗口。
    visible_len: usize,
}

impl SheetDelegate {
    /// 表格列号（含行号列）→ 逻辑数据列。
    fn logical(&self, col_ix: usize) -> Option<usize> {
        col_ix.checked_sub(1).and_then(|display| self.visible.get(display).copied())
    }

    /// DataTable 只请求可见格；文字与暂存标记直接来自会话，不保留全量显示副本。
    fn cell_at(&self, row_ix: usize, col_ix: usize, cx: &App) -> Option<(String, u8)> {
        if row_ix >= self.row_count {
            return None;
        }
        let owner = self.owner.as_ref()?.upgrade()?;
        let grid = &owner.read(cx).session;
        if row_ix >= grid.loaded_len() {
            return None;
        }
        if col_ix == 0 {
            let state = grid.cell_dirty_state(row_ix, 0);
            let mark = if matches!(state, session::CellDirty::Inserted | session::CellDirty::Deleted) { dirty_mark(state) } else { 0 };
            return Some((grid.row_number(row_ix), mark));
        }
        let column = self.logical(col_ix)?;
        grid.column_type(column)?;
        Some((grid.cell_label(row_ix, column), dirty_mark(grid.cell_dirty_state(row_ix, column))))
    }

    /// 这一格的语义（NULL / 空字符串 / 大值预览）：与 `cell_label` 同一套判据，只是不拼显示文本。
    /// 界面据此决定是否弱化斜体、给什么悬停提示，不再拿显示文本和 "NULL" 字面量比较。
    fn cell_kind_at(&self, row_ix: usize, col_ix: usize, cx: &App) -> session::CellKind {
        let Some(column) = self.logical(col_ix) else {
            return session::CellKind::Unset;
        };
        let Some(owner) = self.owner.as_ref().and_then(|owner| owner.upgrade()) else {
            return session::CellKind::Unset;
        };
        let owner = owner.read(cx);
        if row_ix >= owner.session.loaded_len() {
            return session::CellKind::Unset;
        }
        owner.session.cell_kind_at(row_ix, column)
    }
}

/// 一个格子是否落在会话选择的矩形里。列 0 是行号列，它只表示"整行被选中"：
/// 只有矩形从首列一直盖到末列（行头选择、横跨整行拖选、全选）才点亮行号，
/// 单格选择或只覆盖部分列的矩形不会把行号格当成第一个数据列的颜色。
/// `column` 是逻辑数据列；None 表示行号列。隐藏列不改变矩形的逻辑范围。
fn cell_selected(
    paint: &SelectionPaint,
    row_ix: usize,
    column: Option<usize>,
    data_columns: usize,
) -> bool {
    let covered = |row: usize, column: usize| {
        paint.regions.iter().any(|(row_start, column_start, row_end, column_end)| {
            row >= *row_start && row <= *row_end && column >= *column_start && column <= *column_end
        })
    };
    let holed = |row: usize, column: usize| {
        paint.holes.iter().any(|(row_start, column_start, row_end, column_end)| {
            row >= *row_start && row <= *row_end && column >= *column_start && column <= *column_end
        })
    };
    match column {
        None => {
            // 整行才亮行号：某一区域从首列盖到末列，并且这一行没被减选的洞挖掉。
            data_columns > 0
                && paint.regions.iter().any(|(row_start, column_start, row_end, column_end)| {
                    row_ix >= *row_start && row_ix <= *row_end && *column_start == 0 && column_end + 1 >= data_columns
                })
                && !(0..data_columns).all(|column| holed(row_ix, column))
        }
        Some(column) => covered(row_ix, column) && !holed(row_ix, column),
    }
}

/// 画高亮用的选区快照：区域加减选的洞，和会话里的 selection_regions/selection_holes 同源。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct SelectionPaint {
    regions: Vec<(usize, usize, usize, usize)>,
    holes: Vec<(usize, usize, usize, usize)>,
}

/// 表格获得焦点时的快捷键。与快捷键帮助里登记的组合一致。
fn grid_shortcut(key: &str, secondary: bool, shift: bool) -> Option<&'static str> {
    match (key, secondary, shift) {
        ("c", true, true) => Some("copy-sql-value"),
        ("c", true, false) => Some("copy-region"),
        ("a", true, false) => Some("select-all-cells"),
        ("z", true, false) => Some("grid-undo"),
        // 粘贴只在网格拿到键盘焦点时生效；编辑栏里的 Ctrl/⌘+V 仍由文本输入自己处理。
        ("v", true, false) => Some("grid-paste"),
        ("delete", false, _) => Some("mark-delete"),
        ("f2", false, _) => Some("edit-cell"),
        _ => None,
    }
}

/// 单元格右键菜单：(命令, 文案, 需要可写, 危险)。
const CELL_MENU: &[(&str, &str, bool, bool)] = &[
    ("grid-value-open", "grid.value.openFull", false, false),
    ("grid-record-toggle", "grid.record.open", false, false),
    ("copy-region", "grid.menu.copySelection", false, false),
    ("copy-sql-value", "shell.shortcuts.copySqlValue", false, false),
    ("grid-copy-row", "grid.menu.copyRow", false, false),
    ("grid-copy-column", "grid.menu.copyColumn", false, false),
    ("grid-copy-where", "grid.menu.copyWhere", false, false),
    ("grid-copy-row-where", "grid.menu.copyRowWhere", false, false),
    ("grid-copy-column-where", "grid.menu.copyColumnWhere", false, false),
    ("", "", false, false),
    // 复制为各种格式：一组平铺的条目，空选区时禁用。
    ("grid-copy-as-tsv", "grid.copyAs.tsv", false, false),
    ("grid-copy-as-csv", "grid.copyAs.csv", false, false),
    ("grid-copy-as-json", "grid.copyAs.json", false, false),
    ("grid-copy-as-markdown", "grid.copyAs.markdown", false, false),
    ("grid-copy-as-insert", "grid.copyAs.insert", false, false),
    ("", "", false, false),
    ("grid-filter-value", "grid.menu.filterByValue", false, false),
    ("select-all-cells", "grid.menu.selectAll", false, false),
    ("", "", false, false),
    ("edit-cell", "shell.shortcuts.editCell", true, false),
    ("grid-set-null", "grid.value.null", true, false),
    ("grid-paste", "grid.write.paste", true, false),
    ("grid-selection-set", "grid.write.setValue", true, false),
    ("grid-selection-null", "grid.write.setNull", true, false),
    ("grid-clone-row", "grid.write.clone", true, false),
    ("grid-insert", "grid.action.insert", true, false),
    ("mark-delete", "grid.menu.markDelete", true, true),
    ("grid-undo-cell", "grid.write.undoCell", false, false),
    ("grid-undo-row", "grid.write.undoRow", false, false),
    ("grid-discard", "grid.write.discardAll", false, false),
    ("grid-jump-staged", "grid.write.jumpStaged", false, false),
    ("grid-sql-select", "grid.sql.select", false, false),
    ("grid-sql-update", "grid.sql.update", false, false),
    ("grid-set-null", "grid.value.null", true, false),
];

/// 表头右键菜单。
const COLUMN_MENU: &[(&str, &str)] = &[
    ("column-sort-asc", "grid.menu.sortAsc"),
    ("column-sort-desc", "grid.menu.sortDesc"),
    ("column-sort-cycle", "grid.menu.sortCycle"),
    ("column-clear-sort", "grid.menu.clearSort"),
    ("", ""),
    ("column-filter", "grid.menu.filterColumn"),
    ("column-filter-null", "grid.menu.filterNull"),
    ("", ""),
    ("column-copy", "grid.menu.copyColumn"),
    ("column-copy-where", "grid.menu.copyColumnWhere"),
    ("column-copy-name", "grid.menu.copyColumnName"),
    ("", ""),
    ("column-hide", "grid.column.hide"),
    ("column-show-all", "grid.column.showAll"),
    ("column-freeze", "grid.column.freeze"),
    ("column-unfreeze", "grid.column.unfreeze"),
    ("column-fit", "grid.column.fit"),
    ("", ""),
    ("column-clear-filters", "grid.menu.clearFilters"),
];

fn shortcut_hint(id: &str) -> Option<&'static str> {
    let mac = cfg!(target_os = "macos");
    Some(match id {
        "copy-region" => if mac { "⌘C" } else { "Ctrl+C" },
        "copy-sql-value" => if mac { "⇧⌘C" } else { "Ctrl+Shift+C" },
        "select-all-cells" => if mac { "⌘A" } else { "Ctrl+A" },
        "grid-undo" => if mac { "⌘Z" } else { "Ctrl+Z" },
        "grid-paste" => if mac { "⌘V" } else { "Ctrl+V" },
        "edit-cell" => "F2",
        "mark-delete" => "Delete",
        _ => return None,
    })
}

fn menu_label(locale: Locale, key: &str, id: &str) -> SharedString {
    let text = i18n::text(locale, key);
    match shortcut_hint(id) {
        Some(hint) => SharedString::from(format!("{text}    {hint}")),
        None => SharedString::from(text),
    }
}

/// 单元格菜单需要的网格状态。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CellMenuState {
    pub writable: bool,
    pub dirty: bool,
    pub fk: bool,
    pub unresolved: usize,
    pub has_selection: bool,
}

/// 单元格菜单条目是否禁用：写入类在只读网格上禁用（原因沿用工具栏的只读说明），
/// "复制为"在空选区时禁用。
fn cell_item_disabled(id: &str, needs_write: bool, state: CellMenuState) -> bool {
    (needs_write && !state.writable) || (CopyFormat::from_command(id).is_some() && !state.has_selection)
}

/// 表头菜单需要的状态：当前排序方向、能不能再藏一列、有没有冻结、哪些列被藏了。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ColumnMenuState {
    sorted: Option<bool>,
    can_hide: bool,
    frozen: bool,
    hidden: Vec<(usize, String)>,
}

/// 表头菜单条目是否禁用：只剩一列可见时不能再藏；没有冻结时不能取消；没有隐藏列时不用"显示全部"。
fn column_item_disabled(id: &str, state: &ColumnMenuState) -> bool {
    match id {
        "column-hide" => !state.can_hide,
        "column-unfreeze" => !state.frozen,
        "column-show-all" => state.hidden.is_empty(),
        _ => false,
    }
}

fn grid_click(owner: Option<gpui_kit::WeakEntity<GridPane>>, id: &'static str) -> impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut App) + 'static {
    move |_, _, cx| {
        if let Some(owner) = owner.as_ref().and_then(|owner| owner.upgrade()) {
            owner.update(cx, |pane, cx| {
                let _ = pane.command(id, cx);
                cx.notify();
            });
        }
    }
}

/// 禁用的写入项仍留在菜单里，悬停说明原因。
fn grid_item(locale: Locale, key: &'static str, id: &'static str, disabled: bool, reason: Option<String>, owner: Option<gpui_kit::WeakEntity<GridPane>>) -> PopupMenuItem {
    let label = menu_label(locale, key, id);
    if disabled {
        let reason = reason.unwrap_or_default();
        PopupMenuItem::element(move |_, cx| {
            let p = Palette::of(cx);
            div()
                .id(SharedString::from(format!("grid-menu-{id}")))
                .test_support()
                .w_full()
                .px(px(8.))
                .py(px(4.))
                .text_size(px(TypeScale::SM))
                .text_color(p.text_faint)
                .child(label.clone())
                .tooltip_text(SharedString::from(reason.clone()))
        })
        .disabled(true)
    } else {
        PopupMenuItem::new(label).on_click(grid_click(owner, id))
    }
}

fn write_reason_for(needs_write: bool, state: CellMenuState, write_reason: &str, locale: Locale, id: &str) -> Option<String> {
    if cell_item_disabled(id, needs_write, state) && needs_write && !state.writable {
        let reason = if write_reason.is_empty() { i18n::text(locale, "grid.status.readonly") } else { write_reason.to_owned() };
        Some(reason)
    } else if CopyFormat::from_command(id).is_some() && !state.has_selection {
        Some(i18n::text(locale, "grid.copyAs.empty"))
    } else {
        None
    }
}

#[inline(never)]
pub(crate) fn cell_menu(
    menu: PopupMenu,
    owner: Option<gpui_kit::WeakEntity<GridPane>>,
    locale: Locale,
    state: CellMenuState,
    write_reason: String,
    cell_dirty: bool,
    row_dirty: bool,
    window: &mut Window,
    cx: &mut Context<TableState<SheetDelegate>>,
) -> PopupMenu {
    let reason_text = write_reason.clone();
    let reason = move |needs_write: bool, id: &str| write_reason_for(needs_write, state, &reason_text, locale, id);
    let item = |menu: PopupMenu, key: &'static str, id: &'static str, needs_write: bool| {
        let disabled = cell_item_disabled(id, needs_write, state);
        menu.item(grid_item(locale, key, id, disabled, reason(needs_write, id), owner.clone()))
    };
    // 常用在上：复制 → 编辑 → 筛选/导航 → 子菜单；每组之间用分隔线。
    let mut menu = item(menu, "grid.menu.copySelection", "copy-region", false);
    menu = menu.separator();
    menu = item(menu, "grid.menu.editCell", "edit-cell", true);
    menu = item(menu, "grid.value.null", "grid-set-null", true);
    menu = item(menu, "grid.value.openFull", "grid-value-open", false);
    if cell_dirty {
        menu = item(menu, "grid.write.undoCell", "grid-undo-cell", false);
    }
    menu = menu.separator();
    menu = item(menu, "grid.menu.filterByValue", "grid-filter-value", false);
    if state.fk {
        menu = item(menu, "grid.fk.jump", "grid-fk-jump", false);
        menu = item(menu, "grid.fk.select", "grid-fk-select", false);
    }
    menu = menu.separator();
    let row_menu = PopupMenu::build(window, cx.deref_mut(), {
        let owner = owner.clone();
        let write_reason = write_reason.clone();
        move |menu, _, _| {
            let mut menu = menu;
            for (key, id, needs_write) in [
                ("grid.action.insert", "grid-insert", true),
                ("grid.write.clone", "grid-clone-row", true),
                ("grid.write.setValue", "grid-selection-set", true),
                ("grid.write.undoRow", "grid-undo-row", false),
                ("grid.menu.markDelete", "mark-delete", true),
            ] {
                let disabled = cell_item_disabled(id, needs_write, state) || (id == "grid-undo-row" && !row_dirty);
                let why = if id == "grid-undo-row" && !row_dirty {
                    None
                } else {
                    write_reason_for(needs_write, state, &write_reason, locale, id)
                };
                menu = menu.item(grid_item(locale, key, id, disabled, why, owner.clone()));
            }
            if state.dirty {
                menu = menu.separator().item(grid_item(locale, "grid.menu.undo", "grid-undo", false, None, owner.clone()));
            }
            menu
        }
    });
    menu = menu.item(PopupMenuItem::submenu(SharedString::from(i18n::text(locale, "grid.menu.rowOps")), row_menu));
    let copy_menu = PopupMenu::build(window, cx.deref_mut(), {
        let owner = owner.clone();
        let write_reason = write_reason.clone();
        move |menu, _, _| {
            let mut menu = menu;
            for (key, id, needs_write) in [
                ("grid.copyAs.value", "copy-region", false),
                ("grid.copyAs.tsv", "grid-copy-as-tsv", false),
                ("grid.copyAs.csv", "grid-copy-as-csv", false),
                ("grid.copyAs.json", "grid-copy-as-json", false),
                ("grid.copyAs.markdown", "grid-copy-as-markdown", false),
                ("grid.copyAs.sqlLiteral", "copy-sql-value", false),
                ("grid.copyAs.insert", "grid-copy-as-insert", false),
            ] {
                let disabled = cell_item_disabled(id, needs_write, state);
                menu = menu.item(grid_item(locale, key, id, disabled, write_reason_for(needs_write, state, &write_reason, locale, id), owner.clone()));
            }
            menu
        }
    });
    menu = menu.item(PopupMenuItem::submenu(SharedString::from(i18n::text(locale, "grid.menu.copyAs")), copy_menu));
    let sql_menu = PopupMenu::build(window, cx.deref_mut(), {
        let owner = owner.clone();
        move |menu, _, _| {
            let mut menu = menu;
            for (key, id) in [
                ("grid.sql.whereCell", "grid-copy-where"),
                ("grid.sql.whereRow", "grid-copy-row-where"),
                ("grid.sql.whereColumn", "grid-copy-column-where"),
                ("grid.sql.select", "grid-sql-select"),
                ("grid.sql.update", "grid-sql-update"),
            ] {
                menu = menu.item(grid_item(locale, key, id, false, None, owner.clone()));
            }
            menu
        }
    });
    menu = menu.item(PopupMenuItem::submenu(SharedString::from(i18n::text(locale, "grid.menu.generateSql")), sql_menu));
    let tools = PopupMenu::build(window, cx.deref_mut(), {
        let owner = owner.clone();
        move |menu, _, _| {
            let mut menu = menu.item(grid_item(locale, "grid.record.open", "grid-record-toggle", false, None, owner.clone()));
            if state.unresolved > 0 {
                menu = menu.item(grid_item(locale, "grid.action.readFull", "grid-full-values", false, None, owner.clone()));
            }
            menu
        }
    });
    menu.item(PopupMenuItem::submenu(SharedString::from(i18n::text(locale, "grid.menu.moreTools")), tools))
}

#[inline(never)]
fn column_menu(
    menu: PopupMenu,
    owner: Option<gpui_kit::WeakEntity<GridPane>>,
    locale: Locale,
    column: usize,
    state: ColumnMenuState,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let mut menu = menu;
    for (id, key) in [
        ("column-sort-asc", "grid.menu.sortAsc"),
        ("column-sort-desc", "grid.menu.sortDesc"),
        ("column-clear-sort", "grid.menu.clearSort"),
        ("column-filter", "grid.menu.filterColumn"),
        ("column-fit", "grid.column.fit"),
    ] {
        let item_owner = owner.clone();
        let current = matches!((id, state.sorted), ("column-sort-asc", Some(false)) | ("column-sort-desc", Some(true)));
        let label = if current { SharedString::from(format!("✓ {}", i18n::text(locale, key))) } else { menu_label(locale, key, id) };
        menu = menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
            if let Some(owner) = item_owner.as_ref().and_then(|owner| owner.upgrade()) {
                owner.update(cx, |pane, cx| pane.column_command(column, id, cx));
            }
        }));
    }
    let hidden = state.hidden.clone();
    let can_hide = state.can_hide;
    let frozen = state.frozen;
    menu.submenu(SharedString::from(i18n::text(locale, "grid.column.menu")), window, cx, move |menu, _, _| {
        let mut menu = menu;
        let owner_freeze = owner.clone();
        menu = menu.item(PopupMenuItem::new(menu_label(locale, "grid.column.freeze", "column-freeze")).on_click(move |_, _, cx| {
            if let Some(owner) = owner_freeze.as_ref().and_then(|owner| owner.upgrade()) {
                owner.update(cx, |pane, cx| pane.column_command(column, "column-freeze", cx));
            }
        }));
        let owner_unfreeze = owner.clone();
        menu = menu.item(column_disabled_item(locale, "grid.column.unfreeze", "column-unfreeze", !frozen, None, owner_unfreeze, column));
        let owner_hide = owner.clone();
        let hide_reason = (!can_hide).then(|| i18n::text(locale, "grid.column.lastVisible"));
        menu = menu.item(column_disabled_item(locale, "grid.column.hide", "column-hide", !can_hide, hide_reason, owner_hide, column));
        for (hidden_index, name) in hidden.iter().cloned() {
            let owner_show = owner.clone();
            let label = SharedString::from(i18n::fill(&i18n::text(locale, "grid.column.show"), &[("name", &name)]));
            menu = menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                if let Some(owner) = owner_show.as_ref().and_then(|owner| owner.upgrade()) {
                    owner.update(cx, |pane, cx| pane.column_command(hidden_index, "column-show", cx));
                }
            }));
        }
        let owner_all = owner.clone();
        menu.item(column_disabled_item(locale, "grid.column.showAll", "column-show-all", hidden.is_empty(), None, owner_all, column))
    })
}

fn column_disabled_item(
    locale: Locale,
    key: &'static str,
    id: &'static str,
    disabled: bool,
    reason: Option<String>,
    owner: Option<gpui_kit::WeakEntity<GridPane>>,
    column: usize,
) -> PopupMenuItem {
    let label = menu_label(locale, key, id);
    if disabled {
        let reason = reason.unwrap_or_default();
        PopupMenuItem::element(move |_, cx| {
            let p = Palette::of(cx);
            div()
                .id(SharedString::from(format!("grid-col-menu-{id}")))
                .test_support()
                .w_full()
                .px(px(8.))
                .py(px(4.))
                .text_size(px(TypeScale::SM))
                .text_color(p.text_faint)
                .child(label.clone())
                .tooltip_text(SharedString::from(reason.clone()))
        })
        .disabled(true)
    } else {
        PopupMenuItem::new(label).on_click(move |_, _, cx| {
            if let Some(owner) = owner.as_ref().and_then(|owner| owner.upgrade()) {
                owner.update(cx, |pane, cx| pane.column_command(column, id, cx));
            }
        })
    }
}


impl TableDelegate for SheetDelegate {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len().max(1)
    }

    fn rows_count(&self, _: &App) -> usize {
        self.row_count
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        let (name, width, fixed) = self.columns.get(col_ix).cloned().unwrap_or_else(|| (" ".to_owned(), 120.0, false));
        // 行号列不能拉宽；冻结的数据列仍然能拉。宽度上限和会话里记宽度的上限一致。
        // 列宽来自会话记下的宽度；内边距来自当前密度，表格不再用自己的默认 8/4 一组常量。
        let mut column = Column::new(format!("c{col_ix}"), name)
            .width(px(width))
            .paddings(self.cell_pad)
            .resizable(col_ix != 0)
            .max_width(px(640.));
        // 第 0 列是行号列，不是数据列：整列选择在数据列之外没有意义。声明成不可选，
        // 表头点击就不会把表格切进列模式，也不会给 # 列和它的格子刷激活底色。
        if col_ix == 0 {
            column = column.selectable(false);
        }
        if fixed {
            column = column.fixed_left();
        }
        column
    }

    fn render_td(&mut self, row_ix: usize, col_ix: usize, _: &mut Window, cx: &mut Context<TableState<Self>>) -> impl IntoElement {
        let (text, mark) = self.cell_at(row_ix, col_ix, cx).unwrap_or_default();
        // 语义跟着值从会话回来：不再拿显示文本和 "NULL" 字面量比较，
        // 真正存着 "NULL" 的字符串是普通文本，NULL 与空字符串各自可辨认。
        let kind = self.cell_kind_at(row_ix, col_ix, cx);
        let null = kind == session::CellKind::Null;
        let tooltip = cell_tooltip(self.locale, kind, &text);
        let p = Palette::of(cx);
        // 表格的 SelectCell 不带修饰键和拖拽阶段，直接在格子上取鼠标事件：该格里按下、
        // 拖到另一格、松开都带着真实行列，不需要按像素反推行号。
        // 行号格不是数据格：按下、拖动都不改选择，也不会把第一列数据顺手选中。
        let logical = self.logical(col_ix);
        let owner = self.owner.clone().filter(|_| logical.is_some());
        let column = logical.unwrap_or(0);
        // 选中的每一格自己画底色，不是只让焦点那格亮：矩形里没有画的格子会让高亮和
        // 复制/统计说的范围对不上。行号列（col 0）只在整行被选中时亮，单格选择不点亮它。
        let selected = cell_selected(&self.selected, row_ix, logical, self.logical_count);
        let (tone_bg, tone_line) = match mark {
            1 => (Some(p.ok_soft), Some(p.ok)),
            2 => (Some(p.warn_soft), Some(p.warn)),
            3 => (Some(p.danger_soft), Some(p.danger)),
            _ => (None, None),
        };
        if let (Some(edit), Some(input)) = (self.editing, self.edit_input.as_ref()) {
            if logical.is_some() && edit == (row_ix, column) {
                // 原位编辑：输入框占满这一格，强调色描边；Enter 写入暂存，Esc 取消。
                return div()
                    .id(gpui_kit::ElementId::named_usize(format!("grid-cell-{row_ix}"), col_ix))
                    .debug_selector(move || format!("grid-cell-{row_ix}-{col_ix}"))
                    .size_full()
                    .flex()
                    .items_center()
                    .bg(p.surface)
                    .border_1()
                    .border_color(p.accent)
                    // 输入框在表格里面：Esc 绑定的是表格的 Cancel 动作，动作先于按键监听分发。
                    // 在格子这一层接住它，取消编辑而不是让表格当成清除选择。
                    .when_some(self.owner.clone(), |this, owner| {
                        this.on_action(move |_: &gpui_kit::base::actions::Cancel, _, cx| {
                            if let Some(pane) = owner.upgrade() {
                                pane.update(cx, |pane, cx| {
                                    pane.cancel_edit();
                                    pane.sync_table(cx);
                                    cx.notify();
                                });
                            }
                        })
                    })
                    .child(kit::text_input("grid-edit-input", input, SharedString::from(i18n::text(self.locale, "shell.shortcuts.editCell"))).appearance(false).small())
                    .test_support()
                    .into_any_element();
            }
        }
        div()
            .id(gpui_kit::ElementId::named_usize(format!("grid-cell-{row_ix}"), col_ix))
            // 测试按住真实坐标点击/拖拽时用它找到格子；只在测试/debug+test-support 记录。
            .debug_selector(move || format!("grid-cell-{row_ix}-{col_ix}"))
            .size_full()
            .truncate()
            .when(null, |this| this.text_color(p.text_faint).italic())
            .when(mark == 3, |this| this.line_through())
            .when_some(tone_line, |this, color| this.border_l_1().border_color(color))
            .when_some(tooltip, |this, tip| this.tooltip_text(tip))
            // 键盘焦点环：只有键盘刚动过、焦点还在网格里时才画（和 kit 的 focus-visible 同一条判据），
            // 宽度取设计 token 的 2px，颜色取强调色，深浅两套主题都够清楚。
            .when(self.focus_ring == Some((row_ix, column)) && logical.is_some(), |this| {
                this.border_2().border_color(p.accent)
            })
            // 选中时保留选择色；未选中的暂存格用新增/修改/删除底色。
            .when(!selected, |this| this.when_some(tone_bg, |this, color| this.bg(color)))
            .when(selected, |this| this.bg(p.selection))
            .child(SharedString::from(text))
            .when_some(owner, move |this, owner| {
                let drag = owner.clone();
                let release = owner.clone();
                let release_out = owner.clone();
                this.on_mouse_down(MouseButton::Left, move |event: &MouseDownEvent, _, cx| {
                    // Ctrl/Cmd 单击增减这一格；Shift 单击扩展主矩形，与键盘 Shift 一致。
                    let mode = if event.modifiers.secondary(cx) {
                        PressMode::Toggle
                    } else if event.modifiers.shift {
                        PressMode::Extend
                    } else {
                        PressMode::Plain
                    };
                    if let Some(pane) = owner.upgrade() {
                        pane.update(cx, |pane, cx| pane.cell_press(row_ix, column, mode, cx));
                    }
                })
                .on_mouse_move(move |event: &MouseMoveEvent, _, cx| {
                    // 只在左键仍按着时扩展；松开后单纯悬停不改选择。
                    if event.pressed_button != Some(MouseButton::Left) {
                        return;
                    }
                    if let Some(pane) = drag.upgrade() {
                        pane.update(cx, |pane, cx| pane.cell_drag(row_ix, column, cx));
                    }
                })
                .on_mouse_up(MouseButton::Left, move |_, _, cx| {
                    if let Some(pane) = release.upgrade() {
                        pane.update(cx, |pane, _| pane.cell_release());
                    }
                })
                // 从这一格按下、在别的格子或表外松开：同样结束拖选，不会一直跟着指针。
                .on_mouse_up_out(MouseButton::Left, move |_, _, cx| {
                    if let Some(pane) = release_out.upgrade() {
                        pane.update(cx, |pane, _| pane.cell_release());
                    }
                })
            })
            // 选中状态同时写进这一格的语义，帧级测试按同样的值核对"画出来的高亮"。
            .aria_selected(selected)
            .test_support()
            .into_any_element()
    }

    fn render_th(&mut self, col_ix: usize, _: &mut Window, cx: &mut Context<TableState<Self>>) -> impl IntoElement {
        let p = Palette::of(cx);
        let name = self.columns.get(col_ix).map(|column| column.0.clone()).unwrap_or_default();
        // 表头也留稳定选择器：测试点击真实表头坐标，区分 # 列头和第一列数据。
        let header = div()
            .id(("grid-th", col_ix))
            .debug_selector(move || format!("grid-th-{col_ix}"))
            .size_full()
            .flex()
            .items_center()
            .truncate()
            .child(SharedString::from(name))
            .test_support();
        let Some(logical) = self.logical(col_ix) else {
            return header.into_any_element();
        };
        // 表头自己带方向标记：焦点移到工具栏或别的格子后，仍然看得出按哪一列、哪个方向排的。
        let sorted = self.sort.filter(|(index, _)| *index == logical).map(|(_, descending)| descending);
        let meta = self.headers.get(logical);
        let primary = meta.is_some_and(|item| item.primary);
        let foreign = meta.is_some_and(|item| item.foreign);
        let abbr = meta.map(|item| item.abbr.clone()).unwrap_or_default();
        let frozen = self.columns.get(col_ix).is_some_and(|(_, _, fixed)| *fixed);
        let mut header = header;
        if primary {
            header = header.child(div().id(("grid-th-pk", col_ix)).debug_selector(move || format!("grid-th-pk-{col_ix}")).flex_none().child(kit::icon(IconName::Key, 12., p.accent)));
        }
        if foreign {
            header = header.child(div().id(("grid-th-fk", col_ix)).debug_selector(move || format!("grid-th-fk-{col_ix}")).flex_none().child(kit::icon(IconName::Link, 12., p.accent)));
        }
        if !abbr.is_empty() {
            header = header.child(
                div()
                    .id(("grid-th-type", col_ix))
                    .debug_selector(move || format!("grid-th-type-{col_ix}"))
                    .flex_none()
                    .px(px(4.))
                    .text_size(px(TypeScale::XS))
                    .text_color(p.text_faint)
                    .child(SharedString::from(abbr)),
            );
        }
        if let Some(descending) = sorted {
            header = header.child(div().flex_none().px(px(4.)).text_size(px(TypeScale::XS)).child(SharedString::from(if descending { "↓" } else { "↑" })));
        }
        if frozen {
            header = header.child(
                div()
                    .id(("grid-th-frozen", col_ix))
                    .debug_selector(move || format!("grid-th-frozen-{col_ix}"))
                    .flex_none()
                    .child(kit::icon(IconName::Bookmark, 12., p.text_faint)),
            );
        }
        let owner = self.owner.clone();
        let locale = self.locale;
        let state = ColumnMenuState {
            sorted,
            can_hide: self.visible.len() > 1,
            frozen: self.columns.iter().skip(1).any(|(_, _, fixed)| *fixed),
            hidden: self.hidden.clone(),
        };
        header
            .context_menu(move |menu, window, cx| column_menu(menu, owner.clone(), locale, logical, state.clone(), window, cx))
            .into_any_element()
    }

    fn context_menu(&mut self, _row_ix: usize, menu: PopupMenu, window: &mut Window, cx: &mut Context<TableState<Self>>) -> PopupMenu {
        let state = CellMenuState { writable: self.writable, dirty: self.dirty, fk: self.fk, unresolved: self.unresolved, has_selection: self.has_selection };
        cell_menu(menu, self.owner.clone(), self.locale, state, self.write_reason.clone(), self.cell_dirty, self.row_dirty, window, cx)
    }

    fn cell_label(&self, row_ix: usize, col_ix: usize, cx: &App) -> Option<SharedString> {
        self.cell_at(row_ix, col_ix, cx).map(|(text, _)| SharedString::from(text))
    }

    /// 自适应列宽：用表格正文的字体量已加载行里这一列的文字和列名，取最宽的。
    /// 表格再加上格子内边距，并夹在列的最大宽度里。
    fn fit_column(&mut self, col_ix: usize, window: &mut Window, cx: &mut Context<TableState<Self>>) -> Option<Pixels> {
        let logical = self.logical(col_ix)?;
        let font = gpui_kit::Font { family: cx.theme().font_family.clone(), ..Default::default() };
        let size = window.rem_size() * 0.875;
        let color = cx.theme().foreground;
        let texts = match self.fit_labels.take().filter(|(column, _)| *column == logical) {
            Some((_, texts)) => texts,
            // 表头双击从 TableState 发起，此时父实体没有处于更新中。
            None => self.owner.as_ref()?.upgrade()?.read(cx).session.fit_column_labels(logical),
        };
        let header = self.columns.get(col_ix).map(|column| column.0.clone()).unwrap_or_default();
        let widest = std::iter::once(header.as_str())
            .chain(texts.iter().map(String::as_str))
            .map(|text| {
                let line = text.lines().next().unwrap_or("").to_owned();
                let run = gpui_kit::TextRun { len: line.len(), font: font.clone(), color, background_color: None, underline: None, strikethrough: None };
                window.text_system().shape_line(SharedString::from(line), size, &[run], None).width
            })
            .fold(px(0.), Pixels::max);
        // 表头还要放排序箭头。
        Some(widest + px(16.))
    }

    fn has_more(&self, _: &App) -> bool {
        self.has_more && !self.loading
    }

    fn load_more(&mut self, _: &mut Window, cx: &mut Context<TableState<Self>>) {
        if self.loading || !self.has_more {
            return;
        }
        self.loading = true;
        let Some(owner) = self.owner.clone() else {
            self.loading = false;
            return;
        };
        cx.spawn(async move |_, cx| {
            cx.update(|cx| {
                if let Some(owner) = owner.upgrade() {
                    owner.update(cx, |pane, cx| pane.load_next_page(cx));
                }
            });
        })
        .detach();
    }

    fn visible_rows_changed(&mut self, visible_range: std::ops::Range<usize>, _: &mut Window, _: &mut Context<TableState<Self>>) {
        self.visible_start = visible_range.start;
        // 屏幕上真实放得下多少行：翻页步长和会话的可见窗口都用它，
        // 否则会话里那份默认行数会和实际可见窗口对不上。
        self.visible_len = visible_range.len();
        self.visible_seen = true;
    }
}

pub(crate) fn runtime_for_views() -> &'static tokio::runtime::Runtime {
    runtime()
}

#[cfg(test)]
impl GridPane {
    pub fn stage_sample_edit(&mut self) {
        use session::{column_of, schema_from_parts, GridColumn, GridRow};
        self.session.target = GridTarget {
            connection_id: "notes".to_owned(),
            connection_name: "笔记".to_owned(),
            database: "main".to_owned(),
            schema: String::new(),
            table: "customers".to_owned(),
            engine: "sqlite".to_owned(),
            environment: "dev".to_owned(),
        };
        let schema = schema_from_parts(
            "sqlite",
            "",
            "customers",
            "table",
            vec![column_of("id", "integer", true), column_of("name", "text", false)],
            vec!["id".to_owned()],
        )
        .expect("schema");
        let columns = vec![
            GridColumn {
                name: "id".into(),
                type_name: "integer".into(),
                nullable: false,
                primary: true,
                unique: true,
                auto: true,
                width: 80.0,
                editable: true,
            },
            GridColumn {
                name: "name".into(),
                type_name: "text".into(),
                nullable: true,
                primary: false,
                unique: false,
                auto: false,
                width: 140.0,
                editable: true,
            },
        ];
        self.session.load_local(
            columns,
            vec![GridRow {
                id: "1".into(),
                values: vec![Some("1".into()), Some("甲".into())],
                located: true,
                locator: vec![("id".into(), Some("1".into()))],
                inserted: false,
            }],
            Some(1),
            Some(schema),
        );
        // 指向 name 列，后续的显式 NULL、复制和预览都作用在这一格。
        self.session.select(0, 1, false);
        let _ = self.session.stage_text(0, 1, Some("乙".into()));
    }

    /// 装一张真正可写的表：用于键盘编辑、显式 NULL 与预览的行为测试。
    #[cfg(test)]
    pub fn plant_writable(&mut self, target: GridTarget, columns: Vec<(&str, &str)>, rows: Vec<Vec<Option<&str>>>, key: &str, cx: &mut Context<Self>) {
        use session::{schema_from_parts, GridRow};
        let schema: crate::grid::TableSchema = schema_from_parts(
            &target.engine,
            &target.schema,
            &target.table,
            "table",
            columns.iter().map(|(name, type_name)| session::column_of(name, type_name, *name == key)).collect(),
            vec![key.to_owned()],
        )
        .expect("schema");
        let grid_columns = columns_from_schema(&schema, true);
        let key_index = columns.iter().position(|(name, _)| *name == key).unwrap_or(0);
        let grid_rows = rows
            .into_iter()
            .enumerate()
            .map(|(index, values)| {
                let locator = vec![(key.to_owned(), values.get(key_index).cloned().flatten().map(str::to_owned))];
                GridRow {
                    id: (index + 1).to_string(),
                    locator,
                    values: values.into_iter().map(|value| value.map(str::to_owned)).collect(),
                    located: true,
                    inserted: false,
                }
            })
            .collect::<Vec<_>>();
        let count = grid_rows.len() as i64;
        self.session.mode = GridMode::Table;
        self.session.switch_target(target);
        self.session.locale = self.locale;
        self.session.load_local(grid_columns, grid_rows, Some(count), Some(schema));
        self.sync_table(cx);
    }

    /// 测试用：焦点移到某一格，不改锚点以外的状态。
    pub fn choose_cell(&mut self, row: usize, column: usize) {
        self.session.select(row, column, false);
    }

    /// 测试用：在某一格暂存一个修改。
    pub fn stage_cell(&mut self, row: usize, column: usize, value: &str) -> bool {
        self.session.stage_text(row, column, Some(value.to_owned())) == Gate::Writable
    }

    /// 测试用：当前表确实读到了外键，跳转才有目标。
    pub fn has_reference(&self) -> bool {
        self.session.has_reference()
    }

    /// 测试用：一次跳转前后应当完全一致的来源状态（选择、滚动、暂存、查看目标）。
    pub fn state_snapshot(&self) -> GridStateSnapshot {
        (
            self.session.selection_ids().map(|(anchor, focus)| (anchor.to_owned(), focus.to_owned())),
            self.session.visible_bounds().0,
            self.session.uncommitted(),
            self.session.viewer_state().map(|state| (state.row_id.clone(), state.column_name.clone())),
        )
    }

    pub fn plant_loaded(&mut self, target: GridTarget, columns: Vec<String>, rows: Vec<Vec<Option<String>>>, cx: &mut Context<Self>) {
        use session::{GridColumn, GridRow};
        self.session.mode = GridMode::Table;
        self.session.target = target;
        self.session.locale = self.locale;
        let grid_columns = columns
            .iter()
            .map(|name| GridColumn {
                name: name.clone(),
                type_name: "TEXT".to_owned(),
                nullable: true,
                primary: false,
                unique: false,
                auto: false,
                width: 120.0,
                editable: true,
            })
            .collect();
        let grid_rows = rows
            .into_iter()
            .enumerate()
            .map(|(index, values)| GridRow {
                id: index.to_string(),
                values,
                located: true,
                locator: Vec::new(),
                inserted: false,
            })
            .collect::<Vec<_>>();
        let count = grid_rows.len() as i64;
        self.session.load_local(grid_columns, grid_rows, Some(count), None);
        self.sync_table(cx);
    }
}

fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("grid runtime"))
}

#[cfg(test)]
mod shortcut_tests {
    use super::*;

    #[test]
    fn grid_shortcuts_follow_the_help_table() {
        assert_eq!(grid_shortcut("c", true, false), Some("copy-region"));
        assert_eq!(grid_shortcut("c", true, true), Some("copy-sql-value"));
        assert_eq!(grid_shortcut("a", true, false), Some("select-all-cells"));
        assert_eq!(grid_shortcut("z", true, false), Some("grid-undo"));
        assert_eq!(grid_shortcut("delete", false, false), Some("mark-delete"));
        assert_eq!(grid_shortcut("f2", false, false), Some("edit-cell"));
        assert_eq!(grid_shortcut("c", false, false), None);
    }

    /// 网格状态、动作和提示用到的文案键必须在字典里真的有这一行：
    /// 只断言 text() 会用键名回退，漏提交的键就看不出来。
    #[test]
    fn every_grid_state_key_exists_in_the_catalog() {
        let keys = crate::i18n::catalog_keys();
        for key in [
            "grid.state.readErrorTitle",
            "grid.state.clearConditions",
            "grid.state.noMatchDetail",
            "grid.state.tableEmptyDetail",
            "grid.state.resultEmptyDetail",
            "grid.action.retry",
            "grid.value.openFullHint",
            "grid.menu.copySelection",
            "grid.record.editValue",
            "grid.record.editHint",
            "grid.record.selection",
            "grid.record.selectionMany",
            "grid.record.copySelection",
            "grid.record.filtered",
            "grid.record.preview",
        ] {
            assert!(keys.contains(&key), "{key} 必须在字典里");
            for locale in [Locale::ZhCn, Locale::En] {
                assert_ne!(i18n::text(locale, key), key, "{key}");
            }
        }
    }
    #[test]
    fn every_menu_label_is_translated() {
        for (id, key, _, _) in CELL_MENU.iter().chain([("grid-undo", "grid.menu.undo", false, false)].iter()) {
            if id.is_empty() {
                continue;
            }
            for locale in [Locale::ZhCn, Locale::En] {
                assert_ne!(i18n::text(locale, key), *key, "{key}");
            }
        }
        for (id, key) in COLUMN_MENU {
            if id.is_empty() {
                continue;
            }
            for locale in [Locale::ZhCn, Locale::En] {
                assert_ne!(i18n::text(locale, key), *key, "{key}");
            }
        }
        for key in ["grid.menu.rowOps", "grid.menu.copyAs", "grid.menu.generateSql", "grid.menu.moreTools", "grid.column.menu", "grid.copyAs.value", "grid.copyAs.sqlLiteral"] {
            for locale in [Locale::ZhCn, Locale::En] {
                assert_ne!(i18n::text(locale, key), key, "{key}");
            }
        }
    }

    #[test]
    fn complete_cell_preview_phase_stays_editable_until_a_real_envelope() {
        let complete = super::truncation_for_open_cell(CellPhase::Preview, false, 0, true, "");
        assert_eq!(complete, TruncationState::Full, "没有信封的格子已经是完整值");
        assert!(!crate::ui::editing_locked(&complete));
        let pretty = crate::ui::commit_draft(crate::ui::DataFormat::Json, EditMode::Editable, &complete, "{\n  \"a\": 1\n}");
        assert!(matches!(pretty, crate::ui::CommitResult::Applied(_)), "美化后的完整 JSON 可以写回");

        let envelope = super::truncation_for_open_cell(CellPhase::Preview, true, 4096, true, "");
        assert!(matches!(envelope, TruncationState::Preview { total_bytes: 4096, can_load_full: true }));
        assert!(crate::ui::editing_locked(&envelope));
        let blocked = crate::ui::commit_draft(crate::ui::DataFormat::Json, EditMode::Editable, &envelope, "{\"a\":1}");
        assert!(matches!(blocked, crate::ui::CommitResult::Blocked { block: crate::ui::ApplyBlock::Preview, .. }));
    }
}


#[cfg(test)]
mod pane_tests {
    use super::session::CellKind;
    use super::*;
    use crate::grid::commands::{ColumnDto, RowDto};
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{AnyWindowHandle, Bounds, Modifiers, Pixels, Point, TestAppContext, VisualTestContext, WindowBounds, WindowOptions, size};

    /// 选区里的格子数（非连续选择按并集算）。
    fn selected_cells(session: &SharedGrid) -> usize {
        session.selection_count()
    }

    fn target() -> GridTarget {
        GridTarget {
            connection_id: "notes".to_owned(),
            connection_name: "笔记".to_owned(),
            database: "main".to_owned(),
            schema: String::new(),
            table: "customers".to_owned(),
            engine: "sqlite".to_owned(),
            environment: "dev".to_owned(),
        }
    }

    fn open(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<GridPane>) {
        let dir = std::env::temp_dir().join(format!("rillsql-grid-pane-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let services = Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            let bounds = Bounds { origin: Point::default(), size: size(px(1100.), px(720.)) };
            gpui_kit::open_window(
                WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
                cx,
                |window, cx| cx.new(|cx| GridPane::new(services, window, cx)),
            )
            .expect("grid pane")
        })
    }

    fn plant(pane: &Entity<GridPane>, cx: &mut App) {
        pane.update(cx, |pane, cx| {
            pane.plant_writable(
                target(),
                vec![("id", "integer"), ("name", "text")],
                vec![vec![Some("1"), Some("甲")], vec![Some("2"), Some("乙")]],
                "id",
                cx,
            );
        });
    }

    #[gpui_kit::test]
    fn lazy_cells_follow_the_session_and_hidden_column_mapping(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant(&pane, cx);
            window.render_frame(cx);
            let table = pane.read(cx).table.clone();
            assert_eq!(table.read(cx).delegate().cell_at(0, 2, cx), Some(("甲".to_owned(), 0)));
            assert_eq!(table.read(cx).delegate().cell_label(0, 2, cx).as_deref(), Some("甲"));
            pane.update(cx, |pane, _| {
                assert_eq!(pane.session.stage_text(0, 1, Some("新值".to_owned())), Gate::Writable);
            });
            // 不必重建显示快照：文字和暂存标记在下一次读取时就来自同一会话。
            assert_eq!(table.read(cx).delegate().cell_at(0, 2, cx), Some(("新值".to_owned(), 2)));
            pane.update(cx, |pane, cx| {
                assert!(pane.session.hide_column(0));
                pane.sync_table(cx);
            });
            assert_eq!(table.read(cx).delegate().cell_at(0, 1, cx), Some(("新值".to_owned(), 2)));
            assert!(table.read(cx).delegate().cell_at(0, 2, cx).is_none());
            assert!(table.read(cx).delegate().cell_at(2, 1, cx).is_none());
            pane.update(cx, |pane, cx| {
                assert_eq!(pane.session.stage_delete(1), Gate::Writable);
                pane.sync_table(cx);
            });
            assert_eq!(table.read(cx).delegate().cell_at(1, 0, cx), Some(("2".to_owned(), 3)));
            assert_eq!(table.read(cx).delegate().cell_at(1, 1, cx), Some(("乙".to_owned(), 3)));
            window.render_frame(cx);
            assert_eq!(table.read(cx).delegate().row_count, 2);
            assert!(table.read(cx).delegate().fit_labels.is_none());
            let detached = SheetDelegate { row_count: 1, ..Default::default() };
            assert!(detached.cell_at(0, 0, cx).is_none());
        }).unwrap();
    }

    #[gpui_kit::test]
    fn lazy_delegate_survives_owner_release_and_direct_header_fit(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        let orphan = cx.update_window(window, |_, window, cx| {
            plant(&pane, cx);
            let table = pane.read(cx).table.clone();
            // 与表头双击相同的 TableState 入口；这里没有父实体的更新借用。
            assert!(table.update(cx, |state, cx| state.fit_col(2, window, cx)));
            assert!(table.read(cx).delegate().fit_labels.is_none());
            let services = Arc::clone(&pane.read(cx).services);
            let temporary = cx.new(|cx| GridPane::new(services, window, cx));
            plant(&temporary, cx);
            let orphan = temporary.read(cx).table.clone();
            assert!(orphan.read(cx).delegate().cell_at(0, 1, cx).is_some());
            orphan
        }).unwrap();
        // GPUI 在更新边界之后处理待发送事件和实体释放。
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| {
            assert!(orphan.read(cx).delegate().cell_at(0, 1, cx).is_none());
            assert!(!orphan.update(cx, |state, cx| state.fit_col(2, window, cx)));
            window.render_frame(cx);
        }).unwrap();
    }

    /// 640 宽的窗口里网格只有约 560 宽：工具条换行，“预览变更”等右端动作完整可见、可点，不被裁掉。
    #[gpui_kit::test]
    fn narrow_toolbar_wraps_instead_of_clipping_the_preview_button(cx: &mut TestAppContext) {
        use gpui_kit::test::TestWindowExt;
        let dir = std::env::temp_dir().join(format!("rillsql-grid-narrow-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let services = Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
        cx.update(gpui_kit::init);
        let width = 560.;
        let (window, pane) = cx.update(|cx| {
            let bounds = Bounds { origin: Point::default(), size: size(px(width), px(600.)) };
            gpui_kit::open_window(WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() }, cx, |window, cx| {
                cx.new(|cx| GridPane::new(services, window, cx))
            })
            .expect("grid pane")
        });
        cx.update_window(window, |_, window, cx| {
            plant(&pane, cx);
            window.render_frame(cx);
            for id in ["grid-preview", "grid-record-toggle", "copy-region", "grid-insert", "mark-delete"] {
                let el = window.find(id);
                assert!(el.visible(), "{id} 应可见");
                let b = el.bounds();
                let right = f32::from(b.origin.x + b.size.width);
                assert!(f32::from(b.origin.x) >= -0.5 && right <= width + 0.5, "{id} 被裁切：{b:?}");
            }
            // 换行后动作组在搜索框下方：说明是换行而不是被挤出窗口。
            let search = window.find("grid-search").bounds();
            let preview = window.find("grid-preview").bounds();
            assert!(preview.origin.y > search.origin.y, "窄宽度下动作组换到下一行：{search:?} / {preview:?}");
            window.click("grid-preview", cx);
        })
        .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 编辑器事件由应用队列异步送达，断言前先清空队列。
    fn settle(cx: &mut TestAppContext) {
        cx.run_until_parked();
    }

    /// 真实读取在 tokio 线程上完成后再排回测试调度器；轮询到读取结束为止。
    fn wait_for_fetch(cx: &mut TestAppContext, window: AnyWindowHandle, pane: &Entity<GridPane>) -> bool {
        let started = std::time::Instant::now();
        loop {
            cx.run_until_parked();
            cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
            let done = cx.update(|cx| !pane.read(cx).fetching() && pane.read(cx).pending_page.is_none());
            if done {
                return true;
            }
            if started.elapsed() >= std::time::Duration::from_secs(5) {
                return false;
            }
            std::thread::park_timeout(std::time::Duration::from_millis(20));
        }
    }


    fn table_target(table: &str) -> GridTarget {
        GridTarget { table: table.to_owned(), ..target() }
    }

    fn open_with(cx: &mut TestAppContext, services: Arc<AppServices>) -> (AnyWindowHandle, Entity<GridPane>) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            let bounds = Bounds { origin: Point::default(), size: size(px(1100.), px(720.)) };
            gpui_kit::open_window(
                WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
                cx,
                |window, cx| cx.new(|cx| GridPane::new(services, window, cx)),
            )
            .expect("grid pane")
        })
    }

    /// 真实 sqlite 夹具：预览上限的切换必须能在真实读取上复验，而不是只改一个字段。
    fn sqlite_services(name: &str, statements: &[String]) -> (std::path::PathBuf, Arc<AppServices>) {
        let dir = std::env::temp_dir().join(format!("rillsql-grid-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let database = dir.join("notes.sqlite");
        let config = dir.clone();
        let statements = statements.to_vec();
        let (tx, rx) = std::sync::mpsc::channel();
        runtime_for_views().spawn(async move {
            let pool = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).create_if_missing(true))
                .await
                .unwrap();
            for sql in statements {
                // 语句由测试自己给出，这里只是执行建表/插入，不接收用户输入。
                sqlx::query(sqlx::AssertSqlSafe(sql.clone())).execute(&pool).await.unwrap();
            }
            pool.close().await;
            let services = Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(config.join("connections.sqlite")))));
            services.connections.open_sqlite_for_test("notes", "笔记", &database).await.unwrap();
            let _ = tx.send(services);
        });
        let services = rx.recv_timeout(std::time::Duration::from_secs(30)).expect("sqlite 夹具");
        (dir, services)
    }

    #[gpui_kit::test]
    fn large_view_loads_without_counting_and_counts_only_on_request(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (dir, services) = sqlite_services("on-demand-count", &["CREATE VIEW numbers AS WITH RECURSIVE seq(n) AS (VALUES(1) UNION ALL SELECT n + 1 FROM seq WHERE n < 1000000) SELECT n FROM seq".to_owned()]);
        let (window, pane) = open_with(cx, services);
        cx.update_window(window, |_, _, cx| pane.update(cx, |pane, cx| {
            let mut target = target();
            target.table = "numbers".into();
            pane.open_table(target, 100, cx);
        })).unwrap();
        assert!(wait_for_fetch(cx, window, &pane));
        cx.update_window(window, |_, window, cx| {
            assert_eq!(pane.read(cx).loaded_len(), 100);
            assert_eq!(pane.read(cx).full_count(), None, "首屏不能隐式扫描百万行计数");
            window.render_frame(cx);
            window.click("grid-range-label", cx);
            assert!(pane.read(cx).counting.is_some(), "点击行数应启动统计：{}", pane.read(cx).notice);
            pane.update(cx, |pane, cx| pane.load_next_page(cx));
        }).unwrap();
        let start = std::time::Instant::now();
        loop {
            cx.run_until_parked();
            if cx.update(|cx| pane.read(cx).counting.is_none() && !pane.read(cx).loading_more) { break; }
            assert!(start.elapsed() < std::time::Duration::from_secs(30));
            std::thread::park_timeout(std::time::Duration::from_millis(5));
        }
        cx.update(|cx| {
            assert_eq!(pane.read(cx).full_count(), Some(1000000), "{}", pane.read(cx).notice);
            assert_eq!(pane.read(cx).loaded_len(), 200, "统计与翻页并行，但不把全表行加载到内存");
        });
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 只读原因必须画在界面上：联接、聚合、多语句、括号和多表各显示各自那句，
    /// 可写表不显示这一栏。以前最笼统的那条会被藏起来，用户只看到一个"只读"。
    #[gpui_kit::test]
    fn readonly_reason_is_painted_for_every_specific_denial(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant(&pane, cx);
            window.render_frame(cx);
        })
        .unwrap();
        let mut visual = VisualTestContext::from_window(window, cx);
        assert!(visual.debug_bounds("grid-readonly-reason").is_none(), "可写时不显示只读原因");
        for (sql, key) in [
            ("select * from customers join people on 1 = 1", "grid.readonly.join"),
            ("select count(*) from customers", "grid.readonly.aggregate"),
            ("select * from customers; select * from people", "grid.readonly.multiStatement"),
            ("select * from (select * from customers) as inner_customers", "grid.readonly.parentheses"),
            ("select * from customers, people", "grid.readonly.multiTable"),
        ] {
            let sheet = query_sheet(sql, &["name"], vec![vec![Some("乙")]], false);
            visual.update(|_, cx| {
                pane.update(cx, |pane, cx| {
                    pane.show_query(target(), sheet, cx);
                });
            });
            visual.update(|window, cx| window.render_frame(cx));
            visual.update(|_, cx| {
                let pane = pane.read(cx);
                assert_eq!(pane.session.readonly_reason(), key, "{sql}");
            });
            assert!(visual.debug_bounds("grid-readonly-reason").is_some(), "{key} 的只读原因必须画出来");
        }
    }

    /// 预览上限 1 KB → 4 KB → 64 KB：旧行列按旧上限解释，改上限会关闭查看面板、清空原值缓存，
    /// 表数据随即按新上限重读，同一段文本从"预览"变成"原值"。
    #[gpui_kit::test]
    fn preview_limit_change_reloads_rows_and_closes_the_viewer(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let long = "a".repeat(3000);
        let (dir, services) = sqlite_services(
            "preview-reload",
            &["CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT)".to_owned(), format!("INSERT INTO docs (id, body) VALUES (1, '{long}')")],
        );
        let (window, pane) = open_with(cx, services);
        cx.update_window(window, |_, window, cx| {
            pane.update(cx, |pane, cx| pane.open_table(table_target("docs"), 100, cx));
            window.render_frame(cx);
        })
        .unwrap();
        assert!(wait_for_fetch(cx, window, &pane), "首屏读取完成");
        cx.update(|cx| {
            pane.update(cx, |pane, _| {
                assert_eq!(pane.session.sheet_preview_bytes(), 1024, "快照记住读出时的上限");
                assert!(pane.session.needs_full(0, 1), "3000 字节超过 1 KB 上限");
                pane.session.open_value(0, 1);
                assert!(pane.session.viewer().is_some(), "查看面板已打开");
            });
        });

        cx.update_window(window, |_, window, cx| {
            pane.update(cx, |pane, cx| pane.set_preview_kb(4, cx));
            window.render_frame(cx);
        })
        .unwrap();
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert!(pane.session.viewer().is_none(), "改上限后查看面板关闭");
            assert!(pane.session.full_text_at(0, 1).is_none(), "旧原值缓存不再可用");
        });
        assert!(wait_for_fetch(cx, window, &pane), "按新上限重读");
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert_eq!(pane.session.sheet_preview_bytes(), 4096);
            assert!(!pane.session.needs_full(0, 1), "4 KB 上限下 3000 字节是原值");
            assert_eq!(pane.cell(0, 1), long, "重读后显示整段原值");
        });

        cx.update_window(window, |_, window, cx| {
            pane.update(cx, |pane, cx| pane.set_preview_kb(64, cx));
            window.render_frame(cx);
        })
        .unwrap();
        assert!(wait_for_fetch(cx, window, &pane), "64 KB 上限重读");
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert_eq!(pane.session.sheet_preview_bytes(), 64 * 1024);
            assert!(!pane.session.needs_full(0, 1));
            assert_eq!(pane.cell(0, 1), long);
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 预览信封不是原值：F2、双击和命令三条路径都不能把信封（或空串）放进编辑框。
    #[gpui_kit::test]
    fn preview_cell_never_puts_the_envelope_into_the_editor(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            pane.update(cx, |pane, cx| {
                pane.plant_writable(
                    target(),
                    vec![("id", "integer"), ("body", "text")],
                    vec![vec![Some("1"), Some("\u{1}t:3000:片段")]],
                    "id",
                    cx,
                );
                pane.session.select(0, 1, false);
            });
            window.render_frame(cx);
        })
        .unwrap();
        cx.update(|cx| assert!(pane.read(cx).session.needs_full(0, 1), "长文本只读到预览"));
        // 命令路径：读不到原值（连接没打开），编辑不打开，输入框里没有信封。
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                let _ = pane.command("edit-cell", cx);
            });
        });
        cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert!(!pane.editing(), "预览值不能直接进编辑");
            assert!(!pane.edit_value(cx).contains('\u{1}'), "编辑框里不能出现信封");
            assert!(!pane.notice.is_empty(), "应说明为什么没有打开编辑");
        });
        // 双击路径：同样先按定位读取，绝不把信封或空串填进编辑器。
        cx.update(|cx| {
            pane.update(cx, |pane, _| {
                pane.pending_edit = Some(PendingEdit { row: 0, column: 1, select_all: true });
            });
        });
        cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert!(!pane.editing(), "双击预览格不打开编辑器");
            assert!(!pane.edit_value(cx).contains('\u{1}'), "双击也不能写入信封");
            assert!(pane.edit_value(cx).is_empty(), "编辑框没有被塞进空串以外的内容");
        });
    }

    fn query_sheet(sql: &str, columns: &[&str], rows: Vec<Vec<Option<&str>>>, truncated: bool) -> QuerySheet {
        let rows: Vec<Vec<Option<String>>> = rows.into_iter().map(|row| row.into_iter().map(|value| value.map(str::to_owned)).collect()).collect();
        // 未截断时行数就是完整行数；被截断时完整行数未知，与查询页给网格的信息一致。
        let full_count = if truncated { None } else { Some(rows.len() as i64) };
        QuerySheet { sql: sql.to_owned(), columns: columns.iter().map(|name| (*name).to_owned()).collect(), rows, truncated, full_count }
    }

    fn schema_dto() -> SchemaDto {
        SchemaDto {
            engine: "sqlite".to_owned(),
            database: "main".to_owned(),
            schema: String::new(),
            name: "customers".to_owned(),
            kind: "table".to_owned(),
            columns: vec![
                ColumnDto { name: "id".to_owned(), type_name: "integer".to_owned(), nullable: false, primary: true, unique: true, auto: true },
                ColumnDto { name: "name".to_owned(), type_name: "text".to_owned(), nullable: true, primary: false, unique: false, auto: false },
            ],
            key: vec!["id".to_owned()],
            foreign_keys: None,
        }
    }

    fn row_dto(id: &str, name: &str) -> RowDto {
        let mut cells = std::collections::BTreeMap::new();
        cells.insert("id".to_owned(), Some(id.to_owned()));
        cells.insert("name".to_owned(), Some(name.to_owned()));
        RowDto { id: id.to_owned(), cells, located: true, locator_columns: vec!["id".to_owned()], locator_values: vec![Some(id.to_owned())] }
    }


    #[gpui_kit::test]
    fn keyboard_editing_uses_a_real_text_input_and_commits_or_cancels(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant(&pane, cx);
            let handle = pane.read(cx).focus_handle();
            window.focus(&handle, cx);
            window.press("right", cx);
        })
        .unwrap();
        settle(cx);
        cx.update(|cx| assert_eq!(pane.read(cx).session.focus_cell(), Some((0, 1)), "方向键应选中 name 列"));

        // F2 打开编辑栏，原值直接进入正式文本输入。
        cx.update_window(window, |_, window, cx| window.press("f2", cx)).unwrap();
        settle(cx);
        cx.update(|cx| {
            assert!(pane.read(cx).editing(), "F2 应打开编辑栏");
            assert_eq!(pane.read(cx).edit_value(cx), "甲", "编辑栏应带出原值");
        });

        // 选区 + 输入替换；撤销只回退输入文本，不产生暂存。
        cx.update_window(window, |_, window, cx| {
            window.press("secondary-a", cx);
            window.input("乙", cx);
        })
        .unwrap();
        settle(cx);
        cx.update(|cx| assert_eq!(pane.read(cx).edit_value(cx), "乙", "全选后输入应替换原值"));
        cx.update_window(window, |_, window, cx| window.press("secondary-z", cx)).unwrap();
        settle(cx);
        cx.update(|cx| {
            assert_eq!(pane.read(cx).edit_value(cx), "甲", "编辑栏撤销应恢复原文本");
            assert!(!pane.read(cx).session.uncommitted(), "输入阶段不应进入暂存");
        });

        // Enter 提交到暂存（不写库）。
        cx.update_window(window, |_, window, cx| {
            window.press("secondary-a", cx);
            window.input("乙", cx);
            window.press("enter", cx);
        })
        .unwrap();
        settle(cx);
        cx.update(|cx| {
            assert!(!pane.read(cx).editing(), "Enter 应结束编辑");
            assert!(pane.read(cx).session.viewer().is_none(), "Enter 暂存后不应顺带打开完整值面板");
            assert_eq!(pane.read(cx).cell(0, 1), "乙");
            assert!(pane.read(cx).session.uncommitted(), "Enter 只暂存");
        });

        // Esc 取消输入，不改动已有暂存。
        cx.update_window(window, |_, window, cx| window.press("f2", cx)).unwrap();
        settle(cx);
        cx.update_window(window, |_, window, cx| {
            window.input("丙", cx);
            window.press("escape", cx);
        })
        .unwrap();
        settle(cx);
        cx.update(|cx| {
            assert!(!pane.read(cx).editing(), "Esc 应关闭编辑栏");
            assert_eq!(pane.read(cx).cell(0, 1), "乙", "Esc 不应写入输入内容");
        });

        // 粘贴由文本输入自身处理。
        cx.update(|cx| cx.write_to_clipboard(ClipboardItem::new_string("粘贴".to_owned())));
        cx.update_window(window, |_, window, cx| window.press("f2", cx)).unwrap();
        settle(cx);
        cx.update_window(window, |_, window, cx| {
            window.press("secondary-a", cx);
            window.press("secondary-v", cx);
        })
        .unwrap();
        settle(cx);
        cx.update(|cx| assert_eq!(pane.read(cx).edit_value(cx), "粘贴", "粘贴应替换选中的文本"));
        cx.update_window(window, |_, window, cx| window.press("enter", cx)).unwrap();
        settle(cx);
        cx.update(|cx| assert_eq!(pane.read(cx).cell(0, 1), "粘贴"));
    }

    #[gpui_kit::test]
    fn double_click_starts_the_same_edit_as_f2(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant(&pane, cx);
            // DataTable 事件带行号列偏移；双击等同于选中后 F2，而不是同时打开查看值。
            pane.update(cx, |pane, _| pane.apply_table_event(&TableEvent::DoubleClickedCell(0, 2)));
            window.render_frame(cx);
        })
        .unwrap();
        cx.update(|cx| {
            assert!(pane.read(cx).editing(), "双击应进入编辑");
            assert_eq!(pane.read(cx).edit_value(cx), "甲");
            assert_eq!(pane.read(cx).session.focus_cell(), Some((0, 1)));
        });
    }

    /// Ctrl/Cmd 单击是加减一格，不是把选区扩成矩形：多块区域各自独立，
    /// 统计、复制和批量动作都按并集算。
    #[gpui_kit::test]
    fn modifier_click_adds_and_removes_single_cells(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant(&pane, cx);
            window.render_frame(cx);
        })
        .unwrap();
        // 第一次普通单击建立锚点；表格补发的同格 SelectCell 在真实事件循环里随后送达。
        cx.update(|cx| pane.update(cx, |pane, cx| pane.cell_press(0, 0, PressMode::Plain, cx)));
        settle(cx);
        // 两次 Ctrl/Cmd 单击各加一格，中间没点到的格子不会被补进选区。
        cx.update(|cx| pane.update(cx, |pane, cx| pane.cell_press(1, 0, PressMode::Toggle, cx)));
        settle(cx);
        cx.update(|cx| pane.update(cx, |pane, cx| pane.cell_press(1, 1, PressMode::Toggle, cx)));
        settle(cx);
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert_eq!(pane.session.focus_cell(), Some((1, 1)), "加选把焦点移到刚点的格子");
            assert!(pane.session.has_extra_regions(), "加选留下多块区域");
            assert_eq!(pane.session.selection_count(), 3, "三格各自独立，没有把中间补成矩形");
            assert_eq!(pane.session.tsv(), "1\t\n2\t乙", "块里没选中的格子留空，列仍然对齐");
            assert_eq!(pane.session.copy_text(), "id\tname\n1\t\n2\t乙", "多格复制带表头");
        });
        // 再 Ctrl/Cmd 点一次同一格：从选择里减掉它。
        cx.update(|cx| pane.update(cx, |pane, cx| pane.cell_press(1, 1, PressMode::Toggle, cx)));
        settle(cx);
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert_eq!(pane.session.selection_count(), 2, "再点一次减掉这一格");
            assert_eq!(pane.session.tsv(), "1\n2");
            assert!(!pane.session.selection_holes().is_empty(), "减选记在洞里");
        });
    }

    /// Ctrl/Cmd 拖过一片格子就加一块矩形区域；再拖过已选区域把它挖掉，主矩形不动。
    #[gpui_kit::test]
    fn modifier_drag_adds_a_region_and_toggles_it_back_out(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant(&pane, cx);
            window.render_frame(cx);
        })
        .unwrap();
        cx.update(|cx| pane.update(cx, |pane, cx| pane.cell_press(0, 0, PressMode::Plain, cx)));
        settle(cx);
        cx.update(|cx| pane.update(cx, |pane, cx| pane.cell_press(1, 0, PressMode::Toggle, cx)));
        settle(cx);
        cx.update(|cx| pane.update(cx, |pane, cx| pane.cell_drag(1, 1, cx)));
        settle(cx);
        cx.update(|cx| pane.update(cx, |pane, _| pane.cell_release()));
        settle(cx);
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert_eq!(pane.session.selection_count(), 3, "一格加一块 1x2 区域");
            assert_eq!(pane.session.selection_regions().len(), 2);
            assert_eq!(pane.session.tsv(), "1\t\n2\t乙");
        });
        // 从已选区域里 Ctrl/Cmd 起拖：整块被挖掉，只剩原来的那一格。
        cx.update(|cx| pane.update(cx, |pane, cx| pane.cell_press(1, 0, PressMode::Toggle, cx)));
        settle(cx);
        cx.update(|cx| pane.update(cx, |pane, cx| pane.cell_drag(1, 1, cx)));
        settle(cx);
        cx.update(|cx| pane.update(cx, |pane, _| pane.cell_release()));
        settle(cx);
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert_eq!(pane.session.selection_count(), 1, "拖过的区域被减掉");
            assert_eq!(pane.session.tsv(), "1");
            assert_eq!(pane.session.selection_regions().len(), 1, "被整块挖掉的区域在松开时收掉");
            assert!(pane.session.selection_holes().is_empty(), "洞已经没有覆盖对象，一起收掉");
        });
    }

    /// Ctrl+C：单格复制值本身，多格复制含表头的 TSV；多格式复制各自保持原样。
    #[gpui_kit::test]
    fn copy_region_copies_the_value_for_one_cell_and_headers_for_many(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant(&pane, cx);
            window.render_frame(cx);
        })
        .unwrap();
        let copied = |cx: &mut TestAppContext, id: &str| -> Option<String> {
            let _ = cx.update(|cx| pane.update(cx, |pane, cx| pane.command(id, cx)));
            cx.update(|cx| cx.read_from_clipboard().and_then(|item| item.text()))
        };
        // 单格：值本身，没有表头。
        cx.update(|cx| pane.update(cx, |pane, _| pane.session.select(0, 1, false)));
        assert_eq!(copied(cx, "copy-region").as_deref(), Some("甲"));
        // 多格：列名一行 + 每行值。
        cx.update(|cx| pane.update(cx, |pane, _| {
            pane.session.select(0, 0, false);
            pane.session.select(1, 1, true);
        }));
        assert_eq!(copied(cx, "copy-region").as_deref(), Some("id\tname\n1\t甲\n2\t乙"));
        // 复制为 TSV 是同一份含表头文本；CSV 仍按 CSV 写。
        assert_eq!(copied(cx, "grid-copy-as-tsv").as_deref(), Some("id\tname\n1\t甲\n2\t乙"));
        assert_eq!(copied(cx, "grid-copy-as-csv").as_deref(), Some("\"id\",\"name\"\r\n\"1\",\"甲\"\r\n\"2\",\"乙\""));
        // SQL 字面量复制不受影响。
        assert_eq!(copied(cx, "copy-sql-value").as_deref(), Some("1, '甲'\n2, '乙'"));
    }

    /// Enter 是写入口（快捷键帮助里的 "Enter / F2"）：可写格进编辑，Esc 取消；
    /// Ctrl/Cmd+Enter 才是显式的查看完整值。
    #[gpui_kit::test]
    fn enter_edits_the_cell_and_ctrl_enter_opens_the_full_value(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant(&pane, cx);
            let handle = pane.read(cx).focus_handle();
            window.focus(&handle, cx);
            window.render_frame(cx);
        })
        .unwrap();
        cx.update_window(window, |_, window, cx| window.press("enter", cx)).unwrap();
        settle(cx);
        cx.update(|cx| {
            assert!(pane.read(cx).editing(), "Enter 应进入编辑");
            assert_eq!(pane.read(cx).edit_value(cx), "1", "编辑框带出原值");
        });
        cx.update_window(window, |_, window, cx| window.press("escape", cx)).unwrap();
        settle(cx);
        cx.update(|cx| assert!(!pane.read(cx).editing(), "Esc 取消编辑"));
        // Ctrl/Cmd+Enter 打开完整值查看，不再进编辑。
        cx.update_window(window, |_, window, cx| window.press("secondary-enter", cx)).unwrap();
        settle(cx);
        cx.update(|cx| {
            assert!(pane.read(cx).session.viewer().is_some(), "Ctrl/Cmd+Enter 查看完整值");
            assert!(!pane.read(cx).editing());
        });
    }

    /// 双击列分隔柄走表格自己的 fit_col：量完发 ColumnWidthsChanged，会话按逻辑列记下新宽度。
    #[gpui_kit::test]
    fn double_clicking_a_column_divider_fits_that_column(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        let long = "很长的一段备注文本，需要把列拉宽才能完整显示".repeat(2);
        cx.update_window(window, |_, window, cx| {
            pane.update(cx, |pane, cx| {
                pane.plant_writable(
                    target(),
                    vec![("id", "integer"), ("note", "text")],
                    vec![vec![Some("1"), Some(long.as_str())]],
                    "id",
                    cx,
                );
            });
            window.render_frame(cx);
        })
        .unwrap();
        let before = cx.update(|cx| pane.read(cx).session.column_width(1));
        let table = cx.update(|cx| pane.read(cx).table.clone());
        // 表格的分隔柄双击直接调 fit_col；和真实双击同一条入口。
        let fitted = cx.update_window(window, |_, window, cx| table.update(cx, |state, cx| state.fit_col(2, window, cx)));
        assert!(fitted.unwrap(), "双击分隔柄应量出一份宽度");
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
        cx.run_until_parked();
        let after = cx.update(|cx| pane.read(cx).session.column_width(1));
        assert!(after > before + 100.0, "自适应后列宽应跟着变：{before} -> {after}");
        // 行号列没有可量的内容，双击它不做任何事。
        let rownum = cx.update_window(window, |_, window, cx| table.update(cx, |state, cx| state.fit_col(0, window, cx)));
        assert!(!rownum.unwrap(), "行号列不参与自适应");
    }
    #[gpui_kit::test]
    fn dragging_across_cells_selects_the_rectangle_and_release_ends_it(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant(&pane, cx);
            window.render_frame(cx);
        })
        .unwrap();
        // 按下第一格，拖到右下角；每一步之间让表格补发的 SelectCell 回声送达。
        cx.update(|cx| pane.update(cx, |pane, cx| pane.cell_press(0, 0, PressMode::Plain, cx)));
        settle(cx);
        cx.update(|cx| pane.update(cx, |pane, cx| pane.cell_drag(1, 1, cx)));
        settle(cx);
        cx.update(|cx| pane.update(cx, |pane, _| pane.cell_release()));
        // 松开后继续移动不应再扩展。
        cx.update(|cx| pane.update(cx, |pane, cx| pane.cell_drag(0, 1, cx)));
        settle(cx);
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert_eq!(pane.session.selection_count(), 4, "拖拽应选中按下格到当前格的矩形");
            assert_eq!(pane.session.tsv(), "1\t甲\n2\t乙");
            assert_eq!(pane.session.focus_cell(), Some((1, 1)), "松开后焦点停在拖到的格子");
        });
    }

    #[gpui_kit::test]
    fn real_mouse_events_reach_the_cell_gesture_handlers(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant(&pane, cx);
            window.render_frame(cx);
        })
        .unwrap();
        let mut visual = VisualTestContext::from_window(window, cx);
        let first = visual.debug_bounds("grid-cell-0-1").expect("首行 id 格");
        let second = visual.debug_bounds("grid-cell-1-2").expect("次行 name 格");
        let secondary = visual.update(|_, cx| Modifiers::secondary_key(cx));

        // 普通单击第一格，再 Ctrl/Cmd 单击第二格：真实鼠标事件经格子监听器进入加选。
        visual.simulate_mouse_down(first.center(), MouseButton::Left, Modifiers::default());
        visual.simulate_mouse_up(first.center(), MouseButton::Left, Modifiers::default());
        visual.simulate_mouse_down(second.center(), MouseButton::Left, secondary);
        visual.simulate_mouse_up(second.center(), MouseButton::Left, secondary);
        visual.update(|_, cx| {
            let session = &pane.read(cx).session;
            assert_eq!(session.selection_count(), 2, "Ctrl/Cmd 单击只加这一格，不把中间补成矩形");
            assert!(session.has_extra_regions());
            assert_eq!(session.tsv(), "1\t\n\t乙");
        });

        // 普通单击替换矩形，再按住从第一格拖到第二格。
        visual.simulate_mouse_down(first.center(), MouseButton::Left, Modifiers::default());
        visual.simulate_mouse_up(first.center(), MouseButton::Left, Modifiers::default());
        visual.update(|_, cx| assert_eq!(pane.read(cx).session.selection_count(), 1, "普通单击应替换选择"));
        visual.simulate_mouse_down(first.center(), MouseButton::Left, Modifiers::default());
        visual.simulate_mouse_move(second.center(), MouseButton::Left, Modifiers::default());
        visual.simulate_mouse_up(second.center(), MouseButton::Left, Modifiers::default());
        visual.update(|_, cx| {
            let pane = pane.read(cx);
            assert_eq!(pane.session.selection_count(), 4, "拖拽应选中按下格到当前格的矩形");
            assert_eq!(pane.session.tsv(), "1\t甲\n2\t乙");
            assert_eq!(pane.session.focus_cell(), Some((1, 1)));
        });
    }

    #[gpui_kit::test]
    fn plain_click_replaces_an_existing_rectangle(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant(&pane, cx);
            window.render_frame(cx);
        })
        .unwrap();
        cx.update(|cx| pane.update(cx, |pane, cx| pane.cell_press(0, 0, PressMode::Plain, cx)));
        settle(cx);
        cx.update(|cx| pane.update(cx, |pane, cx| pane.cell_drag(1, 1, cx)));
        settle(cx);
        // 普通单击另一格替换整个矩形，而不是继续扩展。
        cx.update(|cx| pane.update(cx, |pane, cx| pane.cell_press(1, 1, PressMode::Plain, cx)));
        settle(cx);
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert_eq!(pane.session.selection_count(), 1, "普通单击只保留一格");
            assert_eq!(pane.session.tsv(), "乙");
            assert_eq!(pane.session.focus_cell(), Some((1, 1)));
        });
    }

    #[gpui_kit::test]
    fn explicit_null_is_a_separate_action_from_typing_the_text_null(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant(&pane, cx);
            pane.update(cx, |pane, cx| {
                pane.session.select(0, 1, false);
                let _ = pane.command("grid-set-null", cx);
                pane.session.select(1, 1, false);
                let _ = pane.command("edit-cell", cx);
            });
            window.render_frame(cx);
        })
        .unwrap();
        cx.update_window(window, |_, window, cx| {
            window.press("secondary-a", cx);
            window.input("NULL", cx);
            window.press("enter", cx);
        })
        .unwrap();
        settle(cx);
        cx.update(|cx| {
            assert_eq!(pane.read(cx).session.show_cell(0, 1, false).kind, CellKind::Null);
            assert_eq!(pane.read(cx).session.show_cell(1, 1, false).kind, CellKind::Text);
            let sql = pane.update(cx, |pane, cx| pane.preview(cx)).unwrap();
            assert!(sql.iter().any(|item| item.contains("= NULL") && !item.contains("'NULL'")), "{sql:?}");
            assert!(sql.iter().any(|item| item.contains("= 'NULL'")), "{sql:?}");
        });
    }

    #[gpui_kit::test]
    fn readonly_results_keep_copy_and_explain_the_reason(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            pane.update(cx, |pane, cx| {
                pane.show_query(target(), query_sheet("select * from customers join people", &["name"], vec![vec![Some("乙")]], false), cx);
            });
            window.render_frame(cx);
        })
        .unwrap();
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                assert!(!pane.writable());
                pane.session.select(0, 0, false);
                assert_eq!(pane.command("copy-region", cx).as_deref(), Some("乙"), "只读也要能复制");
                let _ = pane.command("edit-cell", cx);
                assert!(!pane.editing(), "只读格子不应进入编辑");
                assert!(!pane.notice.is_empty(), "应说明禁写原因");
                assert!(!pane.session.uncommitted());
                let _ = pane.command("grid-set-null", cx);
                assert!(!pane.session.uncommitted(), "只读的显式 NULL 也不能假装暂存");
                assert!(pane.session.viewer().is_some(), "只读仍可查看完整值");
                assert_eq!(pane.command("commit", cx), None);
                assert!(!pane.session.preview_open(), "只读不能打开写入预览");
            });
        });
    }

    #[gpui_kit::test]
    fn staged_edit_previews_keep_the_row_after_sorting(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant(&pane, cx);
            pane.update(cx, |pane, cx| {
                pane.session.select(1, 1, false);
                let _ = pane.command("edit-cell", cx);
            });
            window.render_frame(cx);
        })
        .unwrap();
        cx.update_window(window, |_, window, cx| {
            window.press("secondary-a", cx);
            window.input("改", cx);
            window.press("enter", cx);
        })
        .unwrap();
        settle(cx);
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                pane.column_command(0, "column-sort-desc", cx);
                assert_eq!(pane.cell(0, 1), "改", "排序后暂存值跟着行 identity 移动");
                let sql = pane.preview(cx).unwrap();
                assert!(sql[0].contains("\"id\" = 2"), "{sql:?}");
                assert!(!sql[0].contains("\"id\" = 1"), "{sql:?}");
            });
        });
    }

    #[gpui_kit::test]
    fn public_settings_change_page_request_preview_and_null_copy(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            pane.update(cx, |pane, cx| {
                pane.set_page_limit(250, cx);
                pane.set_preview_kb(4, cx);
                plant_writable(pane, cx);
                pane.session.select(0, 1, false);
            });
            window.render_frame(cx);
        })
        .unwrap();
        cx.update(|cx| {
            let pane = pane.read(cx);
            let request = pane.session.page_request().expect("条件合法");
            assert_eq!(request.limit, 250, "page_size 应改变读取行数");
            assert_eq!(request.preview_bytes, 4096, "preview_kb 应改变预览截断上限");
        });

        // 数据库 NULL 的 TSV 文本随设置变化，SQL 字面量仍是 NULL。
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                pane.plant_writable(
                    target(),
                    vec![("id", "integer"), ("name", "text")],
                    vec![vec![Some("1"), None]],
                    "id",
                    cx,
                );
                pane.session.select(0, 1, false);
                pane.set_null_copy("N");
            });
        });
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert_eq!(pane.session.tsv(), "\\N");
            assert_eq!(pane.session.sql_values(), "NULL");
        });
        cx.update(|cx| pane.update(cx, |pane, _| pane.set_null_copy("NULL")));
        cx.update(|cx| assert_eq!(pane.read(cx).session.tsv(), "NULL"));
    }

    #[gpui_kit::test]
    fn any_column_cycles_sort_and_returns_to_the_original_order(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant(&pane, cx);
            window.render_frame(cx);
        })
        .unwrap();
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                // 焦点落在第二列，工具栏排序作用于当前列，而不是固定第一列。
                pane.session.select(1, 1, false);
                assert_eq!(pane.cycle_sort_here(cx), Some(false));
                assert_eq!(pane.session.sort_for("name"), Some(false));
                // 升序按文本比较，"乙" 排在 "甲" 之前。
                assert_eq!(pane.cell(0, 1), "乙");
                assert_eq!(pane.cycle_sort_here(cx), Some(true));
                assert_eq!(pane.cell(0, 1), "甲");
                assert_eq!(pane.cycle_sort_here(cx), None);
                assert!(pane.session.sort_state().is_none(), "第三次点击取消排序");
                assert_eq!(pane.cell(0, 1), "甲", "取消排序回到原始顺序");
                assert_eq!(pane.cell(1, 1), "乙");
                // 表头菜单的直接排序与取消也走同一状态。
                pane.column_command(0, "column-sort-desc", cx);
                assert_eq!(pane.session.sort_for("id"), Some(true));
                pane.column_command(0, "column-clear-sort", cx);
                assert!(pane.session.sort_state().is_none());
            });
        });
    }

    #[gpui_kit::test]
    fn find_scope_switch_hides_when_unavailable_and_stays_separate_from_local_find(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant(&pane, cx);
            window.render_frame(cx);
        })
        .unwrap();
        {
            // 没有可切换的另一档（连接未打开）时不再常驻一个禁用的开关。
            let mut visual = VisualTestContext::from_window(window, cx);
            assert!(visual.debug_bounds("grid-find-scope-hint").is_none(), "不可用时不画范围开关");
        }
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                assert_eq!(pane.session.find_scope(), FindScope::Loaded);
                // 摆上的快照没有打开的连接：重新读取不可能成功，开关如实说明而不是假装可用。
                assert_eq!(pane.session.find_scope_block(), Some("grid.find.scopeOffline"));
                pane.session.set_search("乙");
                assert_eq!(pane.loaded_len(), 1, "已读取行查找在本地生效");
                // 连接没打开时切整表筛选没有意义：不发请求，原因写出来，关键字仍在本地生效。
                assert!(!pane.set_find_scope(FindScope::Server, cx));
                assert_eq!(pane.session.find_scope(), FindScope::Loaded);
                assert_eq!(pane.session.server_search(), None);
                assert_eq!(pane.notice, i18n::text(Locale::ZhCn, "grid.find.scopeOffline"));
                assert_eq!(pane.loaded_len(), 1, "关键字没有被静默停用");
                assert!(!pane.toggle_find_scope(cx), "同一个不可用原因也挡住开关");
            });
        });
        // 没有目标表时整表筛选不可用：给出原因，关键字仍只在已读取行里生效。
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                pane.clear_sheet(cx);
                pane.session.set_search("乙");
                assert_eq!(pane.session.find_scope_block(), Some("grid.find.scopeNoTable"));
                assert!(!pane.set_find_scope(FindScope::Server, cx), "不可用时不能切成整表筛选");
                assert_eq!(pane.session.find_scope(), FindScope::Loaded);
                assert_eq!(pane.notice, i18n::text(Locale::ZhCn, "grid.find.scopeNoTable"), "原因要显示出来");
                assert_eq!(pane.session.server_search(), None);
            });
        });
    }

    /// 真实表 + 真实连接：整表筛选只在连接可用时成立。连接关掉后不能切进服务器范围，
    /// 已经在整表筛选里的关键字退回已读取行继续生效，并写出具体原因。
    #[gpui_kit::test]
    fn whole_table_filter_needs_a_live_connection_and_keeps_the_keyword(cx: &mut TestAppContext) {
        // 真实读取在 tokio 线程上完成，等待它必须允许调度器挂起线程。
        cx.executor().allow_parking();
        let dir = std::env::temp_dir().join(format!("rillsql-grid-find-scope-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let database = dir.join("notes.sqlite");
        let config = dir.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        runtime_for_views().spawn(async move {
            let pool = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).create_if_missing(true))
                .await
                .unwrap();
            sqlx::query("CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT NOT NULL)").execute(&pool).await.unwrap();
            sqlx::query("INSERT INTO customers (id, name) VALUES (1, '甲'), (2, '乙')").execute(&pool).await.unwrap();
            pool.close().await;
            let services = Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(config.join("connections.sqlite")))));
            services.connections.open_sqlite_for_test("notes", "笔记", &database).await.unwrap();
            let _ = tx.send(services);
        });
        let services = rx.recv_timeout(std::time::Duration::from_secs(30)).expect("sqlite 夹具");
        cx.update(gpui_kit::init);
        let (window, pane) = cx.update(|cx| {
            let bounds = Bounds { origin: Point::default(), size: size(px(1100.), px(720.)) };
            gpui_kit::open_window(
                WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
                cx,
                |window, cx| cx.new(|cx| GridPane::new(Arc::clone(&services), window, cx)),
            )
            .expect("grid pane")
        });
        // 打开并读到真实表的两行。
        cx.update(|cx| pane.update(cx, |pane, cx| pane.open_table(target(), 50, cx)));
        assert!(wait_for_fetch(cx, window, &pane), "真实读取应在超时前结束");
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert_eq!(pane.loaded_len(), 2, "真实表读回两行");
            assert!(pane.session.find_scope_block().is_none(), "连接可用时整表筛选成立");
        });
        // 关键字先在已读取行里筛。
        cx.update(|cx| pane.update(cx, |pane, _| pane.session.set_search("乙")));
        cx.update(|cx| assert_eq!(pane.read(cx).loaded_len(), 1, "已读取行查找在本地生效"));
        // 连接还开着：切到整表筛选真的发请求，只剩服务器筛过的一行。
        cx.update(|cx| assert!(pane.update(cx, |pane, cx| pane.set_find_scope(FindScope::Server, cx)), "连接可用时应能切整表筛选"));
        assert!(wait_for_fetch(cx, window, &pane), "整表筛选应真的发请求");
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert_eq!(pane.session.find_scope(), FindScope::Server);
            assert_eq!(pane.session.server_search().as_deref(), Some("乙"));
            assert_eq!(pane.loaded_len(), 1);
            assert!(pane.session.find_scope_block().is_none(), "连接仍可用");
        });
        // 连接被关掉：整表筛选退回已读取行，关键字继续本地生效，并写出原因。
        services.connections.disconnect("notes");
        cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert_eq!(pane.session.find_scope_block(), Some("grid.find.scopeOffline"), "开关此时应禁用并给原因");
            assert_eq!(pane.session.find_scope(), FindScope::Loaded, "不能停在一个发不出请求的范围");
            assert_eq!(pane.notice, i18n::text(Locale::ZhCn, "grid.find.scopeOffline"));
            assert_eq!(pane.session.server_search(), None);
            assert_eq!(pane.loaded_len(), 1, "关键字仍在已读取行里生效");
        });
        // 再点整表筛选：不发请求、原因照旧，关键字仍能筛本地行。
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                pane.session.set_search("甲");
                assert_eq!(pane.loaded_len(), 0, "关键字仍在本地筛");
                pane.session.set_search("乙");
                assert_eq!(pane.loaded_len(), 1);
                assert!(!pane.set_find_scope(FindScope::Server, cx), "连接关闭后不能切到整表筛选");
                assert_eq!(pane.session.find_scope(), FindScope::Loaded);
                assert_eq!(pane.session.server_search(), None);
                assert_eq!(pane.notice, i18n::text(Locale::ZhCn, "grid.find.scopeOffline"), "原因要显示出来");
                assert_eq!(pane.loaded_len(), 1);
                assert!(!pane.fetching(), "没有发新请求");
            });
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[gpui_kit::test]
    fn filter_panel_adds_rejects_and_clears_conditions(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant(&pane, cx);
            pane.update(cx, |pane, _| pane.session.select(0, 1, false));
            window.render_frame(cx);
        })
        .unwrap();
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                // 把当前格的"甲"加成条件，立即生效。
                assert_eq!(pane.add_filter_here(cx), Some(0));
                assert_eq!(pane.session.filters().len(), 1);
                assert_eq!(pane.session.filters()[0].op, "eq");
                assert_eq!(pane.session.filters()[0].value, "甲");
                assert_eq!(pane.loaded_len(), 1);
                // 非法输入（空值）不发查询，保留上一次有效结果并说明原因。
                pane.session.update_filter_text(0, true, "");
                let error = pane.apply_filters(cx).unwrap_err();
                assert!(!error.is_empty());
                assert_eq!(pane.notice, error, "非法原因要显示出来");
                assert_eq!(pane.loaded_len(), 1, "非法条件不能清掉旧结果");
                // 改好后再应用成功。
                pane.session.update_filter_text(0, true, "乙");
                assert!(pane.apply_filters(cx).is_ok());
                assert!(pane.notice.is_empty());
                assert_eq!(pane.cell(0, 1), "乙");
                // 全清恢复全部已读取行。
                pane.clear_filter_panel(cx);
                assert!(pane.session.filters().is_empty());
                assert_eq!(pane.loaded_len(), 2);
                assert!(pane.session.filter_error().is_none());
            });
        });
    }

    #[gpui_kit::test]
    fn failed_read_keeps_the_sheet_unless_the_target_changed(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant(&pane, cx);
            pane.update(cx, |pane, _| pane.session.select(1, 1, false));
            window.render_frame(cx);
        })
        .unwrap();
        cx.update(|cx| {
            pane.update(cx, |pane, _| {
                // 同一目标的刷新失败：行、列、条件、选择和暂存都保留。
                pane.keep_sheet_after_failure("连接已断开".to_owned(), false);
                assert_eq!(pane.loaded_len(), 2);
                assert_eq!(pane.session.focused_row_id(), Some("2"));
                assert!(pane.session.read_failure().is_some());
                assert_eq!(pane.notice, "连接已断开");
                // 换了目标又失败：旧表快照不能留下来。
                pane.keep_sheet_after_failure("表不存在".to_owned(), true);
                assert_eq!(pane.loaded_len(), 0);
                assert!(pane.session.read_failure().is_none(), "旧快照不带着失败原因一起留下");
            });
        });
    }

    #[gpui_kit::test]
    fn query_results_report_completion_and_never_page_by_replay(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            pane.update(cx, |pane, cx| {
                pane.show_query(target(), query_sheet("select * from customers", &["name"], vec![vec![Some("乙")]], false), cx);
            });
            window.render_frame(cx);
        })
        .unwrap();
        settle(cx);
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                assert_eq!(pane.session.continuation(false), Continuation::Complete);
                assert_eq!(pane.session.state_text(false), "", "读完不再常驻一句状态说明");
                // 查询结果没有"下一页"：这里只改窗口状态，从不重发 SQL。
                let before = pane.session.page_offset();
                let _ = pane.command("grid-page-next", cx);
                assert_eq!(pane.session.page_offset(), before);
                assert!(!pane.fetching(), "查询结果不会发新的读取请求");
                assert!(!pane.session.can_page(true), "查询结果不能翻页");
                // 查询结果不能切到整表筛选：给出原因，关键字仍在已读取行里生效。
                assert_eq!(pane.session.find_scope_block(), Some("grid.find.scopeQueryOnly"));
                assert!(!pane.set_find_scope(FindScope::Server, cx));
                assert_eq!(pane.session.find_scope(), FindScope::Loaded);
                assert_eq!(pane.notice, i18n::text(Locale::ZhCn, "grid.find.scopeQueryOnly"));
                pane.session.set_search("乙");
                assert_eq!(pane.loaded_len(), 1, "查找仍然作用于已读取行，没有被静默忽略");
                pane.session.set_search("");
            });
        });
        // 被读取上限截断的结果：不能说成"已读到末尾"，也不能由网格延续。
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                pane.show_query(target(), query_sheet("select * from customers", &["name"], vec![vec![Some("乙")]], true), cx);
            });
        });
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                assert!(pane.session.full_count().is_none(), "截断的结果没有完整行数");
                assert_eq!(pane.session.continuation(false), Continuation::Unavailable);
                assert_eq!(pane.session.state_text(false), "", "不能续读不再常驻一句解释");
                let _ = pane.command("grid-page-next", cx);
                assert_eq!(pane.session.page_offset(), 0);
                assert!(!pane.fetching());
            });
        });
        // 空结果集用结果集措辞，而不是"表里没有行"。
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                pane.show_query(target(), query_sheet("select * from customers where 0", &["name"], Vec::new(), false), cx);
            });
        });
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert_eq!(pane.session.emptiness(), super::session::SheetEmptiness::ResultEmpty);
            assert_eq!(pane.session.state_text(false), i18n::text(Locale::ZhCn, "grid.state.resultEmpty"));
        });
    }

    #[gpui_kit::test]
    fn failed_page_keeps_the_old_page_and_retries_the_same_offset(cx: &mut TestAppContext) {
        // 真实读取在 tokio 线程上完成，等待它必须允许调度器挂起线程。
        cx.executor().allow_parking();
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant(&pane, cx);
            pane.update(cx, |pane, _| {
                // 只读到第一页两行（页大小也是 2），服务器上还有两行没读。
                pane.with_session(|session| {
                    session.set_page_keep_identity(0, 2);
                    session.set_total_rows_for_test(4);
                });
                pane.session.select(1, 1, false);
                pane.session.scroll_to(1);
                let _ = pane.session.stage_text(1, 1, Some("改".to_owned()));
            });
            window.render_frame(cx);
        })
        .unwrap();
        settle(cx);
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                // 目标连接没有打开，真实读取会失败；这时偏移还不能改。
                let _ = pane.command("grid-page-next", cx);
                assert_eq!(pane.pending_page, Some(2), "第二页的请求偏移");
                assert_eq!(pane.session.page_offset(), 0, "结果回来之前不改偏移");
                assert_eq!(pane.session.visible_bounds().0, 1, "请求期间不动滚动");
            });
        });
        assert!(wait_for_fetch(cx, window, &pane), "翻页读取应在超时前结束");
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert_eq!(pane.session.page_offset(), 0, "失败不落到新偏移");
            assert_eq!(pane.loaded_len(), 2, "旧页的行仍然在");
            assert_eq!(pane.session.focused_row_id(), Some("2"), "选择按稳定行 id 保留");
            assert_eq!(pane.cell(1, 1), "改", "暂存仍然挂在原来的行上");
            assert!(pane.session.uncommitted());
            assert!(pane.session.read_failure().is_some(), "失败原因要能显示");
            assert!(pane.pending_page.is_none(), "失败后不再挂着翻页请求");
        });
        // 与异步回调同一入口的失败：偏移、行、选择、滚动和条件都不动。
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                pane.session.scroll_to(1);
                // 读取失败沿用同一条落地路径：只记原因，旧窗口原样保留。
                pane.apply_page_result(2, true, true, Err("连接已断开".to_owned()), cx);
                assert_eq!(pane.session.page_offset(), 0, "失败不落新偏移");
                assert_eq!(pane.loaded_len(), 2);
                assert_eq!(pane.session.focused_row_id(), Some("2"));
                assert_eq!(pane.session.visible_bounds().0, 1, "滚动位置保留");
                assert_eq!(pane.notice, "连接已断开");
                assert!(pane.session.uncommitted());
            });
        });
        // 再点一次下一页：重试的还是同一页，不会跳过失败页。
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                let _ = pane.command("grid-page-next", cx);
                assert_eq!(pane.pending_page, Some(2), "重试仍是同一页，没有跳过");
            });
        });
        assert!(wait_for_fetch(cx, window, &pane));
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert_eq!(pane.session.page_offset(), 0);
            assert_eq!(pane.loaded_len(), 2);
        });
    }

    #[gpui_kit::test]
    fn a_landed_page_swaps_the_window_and_drops_the_old_selection(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant(&pane, cx);
            pane.update(cx, |pane, _| {
                pane.with_session(|session| {
                    session.set_page_keep_identity(0, 2);
                    session.set_total_rows_for_test(4);
                });
                pane.session.select(1, 1, false);
                let _ = pane.session.stage_text(1, 1, Some("改".to_owned()));
            });
            window.render_frame(cx);
        })
        .unwrap();
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                // 与异步读取回调同一个入口：第二页真的读回来了。
                pane.pending_page = Some(2);
                let page = PageDto { rows: vec![row_dto("3", "丙"), row_dto("4", "丁")], truncated: false, sql: "select".to_owned() };
                pane.apply_page_result(2, true, true, Ok((schema_dto(), page, Ok(CountDto { matched: 4 }))), cx);
                assert_eq!(pane.session.page_offset(), 2, "成功才换偏移");
                assert_eq!(pane.loaded_len(), 2);
                assert_eq!(pane.cell(0, 1), "丙");
                assert_eq!(pane.session.focused_row_id(), None, "旧页的选择不属于新页");
                assert_eq!(pane.session.visible_bounds().0, 0, "新页从顶部开始");
                assert!(pane.session.read_failure().is_none());
                assert_eq!(pane.session.continuation(false), Continuation::Complete);
                // 暂存的那一行已经不在窗口里，但暂存本身仍然保留并可预览。
                assert!(pane.session.uncommitted());
                assert!(pane.preview(cx).unwrap().iter().any(|sql| sql.contains("\"id\" = 2")));
            });
        });
    }

    #[gpui_kit::test]
    fn loading_outranks_emptiness_on_first_open(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            pane.update(cx, |pane, cx| {
                pane.clear_sheet(cx);
                // 首屏读取还在路上：没有列也没有行，但状态是加载中而不是空表。
                pane.fetching = true;
            });
            window.render_frame(cx);
        })
        .unwrap();
        settle(cx);
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert_eq!(pane.session.state_text(true), i18n::text(Locale::ZhCn, "grid.state.loading"));
            assert_eq!(pane.session.emptiness(), super::session::SheetEmptiness::TableEmpty);
        });
        let mut visual = VisualTestContext::from_window(window, cx);
        assert!(visual.debug_bounds("grid-loading").is_some(), "加载中要显示读取提示");
        assert!(visual.debug_bounds("grid-empty").is_none(), "首次读取时不能画空表插图");
        visual.update(|_, cx| {
            pane.update(cx, |pane, _| pane.fetching = false);
        });
        visual.update(|window, cx| window.render_frame(cx));
        assert!(visual.debug_bounds("grid-loading").is_none());
        assert!(visual.debug_bounds("grid-empty").is_some(), "读取结束后才说明没有数据");
    }

    #[gpui_kit::test]
    fn a_dirty_filter_shows_unapplied_and_keeps_the_applied_result(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant(&pane, cx);
            pane.update(cx, |pane, _| pane.session.select(0, 1, false));
            window.render_frame(cx);
        })
        .unwrap();
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                // 按当前值加条件并立即应用。
                assert_eq!(pane.add_filter_here(cx), Some(0));
                assert_eq!(pane.loaded_len(), 1);
                assert!(!pane.session.unapplied_filters());
                // 改动输入：草稿未应用，可见行仍是上次应用的条件结果。
                pane.session.update_filter_text(0, true, "不存在");
                assert!(pane.session.unapplied_filters(), "编辑中的条件要标成未应用");
                assert_eq!(pane.loaded_len(), 1, "未应用的条件不改变可见行");
                assert_eq!(pane.cell(0, 1), "甲", "仍显示上次应用的结果");
            });
        });
        cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
        let mut visual = VisualTestContext::from_window(window, cx);
        assert!(visual.debug_bounds("grid-filter-unapplied").is_some(), "筛选条要标出未应用的条件");
        visual.update(|_, cx| {
            pane.update(cx, |pane, cx| {
                pane.apply_filters(cx).expect("条件合法");
                assert!(!pane.session.unapplied_filters(), "应用后不再是未应用");
                assert_eq!(pane.loaded_len(), 0, "应用后按新条件筛出空结果");
                assert_eq!(pane.session.state_text(false), i18n::text(Locale::ZhCn, "grid.state.noMatch"));
            });
        });
    }

    #[gpui_kit::test]
    fn the_sorted_column_and_direction_stay_visible_without_focus(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant(&pane, cx);
            pane.update(cx, |pane, cx| {
                pane.session.select(0, 1, false);
                assert_eq!(pane.cycle_sort_here(cx), Some(false));
                assert_eq!(pane.cycle_sort_here(cx), Some(true));
                // 焦点移到第一列，排序仍然按 name 降序。
                pane.session.select(0, 0, false);
                assert_eq!(pane.session.sort_state(), Some(("name", true)));
                assert_eq!(pane.table.read(cx).delegate().sort, Some((1, true)), "表头自己带方向标记");
                let sorted = pane.session.sort_text().expect("状态栏要有排序说明");
                assert!(sorted.contains("name"), "{sorted}");
                assert!(sorted.contains('↓'), "{sorted}");
            });
            window.render_frame(cx);
        })
        .unwrap();
        // 第三次点击取消排序：表头标记和状态栏文字一起消失。
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                pane.session.select(0, 1, false);
                assert_eq!(pane.cycle_sort_here(cx), None);
                assert!(pane.session.sort_text().is_none());
                assert_eq!(pane.table.read(cx).delegate().sort, None);
            });
        });
    }

    /// 鼠标（普通按下 + Shift 点）和键盘（Shift 扩展）必须落到同一个矩形。
    #[gpui_kit::test]
    fn mouse_and_keyboard_produce_the_same_rectangle_and_copies(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant(&pane, cx);
            window.render_frame(cx);
        })
        .unwrap();
        // 两次鼠标动作之间让表格补发的 SelectCell 回声送达，和真实点击顺序一致。
        cx.update(|cx| {
            pane.update(cx, |pane, cx| pane.cell_press(0, 0, PressMode::Plain, cx));
        });
        settle(cx);
        cx.update(|cx| {
            pane.update(cx, |pane, cx| pane.cell_press(1, 1, PressMode::Extend, cx));
        });
        settle(cx);
        let mouse = cx.update(|cx| {
            let session = &pane.read(cx).session;
            (session.selection_bounds(), session.focused_row_id().map(str::to_owned), session.tsv(), session.sql_values())
        });

        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                pane.cell_release();
                pane.session.select(0, 0, false);
                pane.session.move_key("down", true);
                pane.session.move_key("right", true);
                pane.sync_table(cx);
            });
        });
        settle(cx);
        cx.update(|cx| {
            let session = &pane.read(cx).session;
            assert_eq!(session.selection_bounds(), mouse.0, "鼠标与键盘应得到同一矩形");
            assert_eq!(session.focused_row_id().map(str::to_owned), mouse.1);
            assert_eq!(session.tsv(), mouse.2);
            assert_eq!(session.sql_values(), mouse.3);
        });
    }

    #[gpui_kit::test]
    fn pointer_cancel_column_resize_and_scroll_end_the_drag(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant(&pane, cx);
            window.render_frame(cx);
        })
        .unwrap();
        // 取消拖拽：已选矩形保留，但下一次移动不再扩展。
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                pane.cell_press(0, 0, PressMode::Plain, cx);
                assert!(pane.dragging());
                assert!(pane.cancel_drag());
                pane.cell_drag(1, 1, cx);
            });
        });
        settle(cx);
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert!(!pane.dragging());
            assert_eq!(pane.session.selection_count(), 1, "取消后指针移动不应继续扩展");
        });

        // 调整列宽结束拖拽：拖到的新格子不会被接进旧矩形。
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                pane.cell_press(0, 0, PressMode::Plain, cx);
                pane.apply_table_event(&TableEvent::ColumnWidthsChanged(vec![px(56.), px(80.), px(140.)]));
                pane.cell_drag(1, 1, cx);
            });
        });
        settle(cx);
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert!(!pane.dragging(), "调整列宽后应结束拖拽");
            assert_eq!(pane.session.selection_count(), 1);
        });

        // 滚动换行结束拖拽，已选矩形仍然保留。
        cx.update_window(window, |_, window, cx| {
            pane.update(cx, |pane, cx| {
                pane.session.select(0, 0, false);
                pane.cell_press(0, 0, PressMode::Plain, cx);
            });
            pane.update(cx, |pane, cx| {
                pane.table.update(cx, |state, _| {
                    state.delegate_mut().visible_start = 1;
                    state.delegate_mut().visible_seen = true;
                });
            });
            window.render_frame(cx);
        })
        .unwrap();
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert!(!pane.dragging(), "滚动后应结束拖拽");
            assert_eq!(pane.session.selection_count(), 1);
        });
    }

    #[gpui_kit::test]
    fn losing_focus_ends_the_drag_without_moving_the_selection(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant(&pane, cx);
            let handle = pane.read(cx).focus_handle();
            window.focus(&handle, cx);
            window.render_frame(cx);
        })
        .unwrap();
        settle(cx);
        cx.update(|cx| {
            pane.update(cx, |pane, cx| pane.cell_press(0, 0, PressMode::Plain, cx));
        });
        cx.update(|cx| assert!(pane.read(cx).dragging()));
        cx.update_window(window, |_, window, cx| {
            // 焦点交给网格外的另一个控件：这是真实会发生的焦点转移。
            let other = cx.focus_handle();
            window.focus(&other, cx);
            window.render_frame(cx);
        })
        .unwrap();
        settle(cx);
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert!(!pane.dragging(), "网格失焦后应结束拖拽");
            assert_eq!(pane.session.focus_cell(), Some((0, 0)), "失焦不移动选择");
        });
    }

    #[gpui_kit::test]
    fn where_copy_commands_write_stable_conditions_to_the_clipboard(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant(&pane, cx);
            window.render_frame(cx);
        })
        .unwrap();
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                pane.session.select(1, 1, false);
                assert_eq!(pane.command("grid-copy-where", cx).as_deref(), Some("WHERE \"customers\".\"name\" = '乙'"));
                assert_eq!(pane.command("grid-copy-row-where", cx).as_deref(), Some("WHERE \"customers\".\"id\" = 2"));
                assert_eq!(pane.command("grid-copy-column-where", cx).as_deref(), Some("WHERE \"customers\".\"name\" IN ('乙')"));
            });
        });
        settle(cx);
        cx.update(|cx| {
            assert_eq!(
                cx.read_from_clipboard().and_then(|item| item.text()).as_deref(),
                Some("WHERE \"customers\".\"name\" IN ('乙')"),
                "最后一个 WHERE 复制应进入剪贴板"
            );
        });
    }

    /// 行头选中的是整行矩形，和按住拖过这一行得到的高亮、TSV、SQL 值一致。
    #[gpui_kit::test]
    fn row_header_selection_matches_dragging_across_the_row(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant(&pane, cx);
            window.render_frame(cx);
        })
        .unwrap();
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                pane.apply_table_event(&TableEvent::SelectRow(1));
                assert_eq!(pane.session.selection_bounds(), Some((1, 0, 1, 1)), "行头选择应覆盖整行");
                assert_eq!(pane.session.copy_row(), "2\t乙");
                assert_eq!(pane.session.sql_values(), "2, '乙'");
                assert_eq!(selected_cells(&pane.session), 2);
                // 同一格的回声不收起整行；点到别的格子才换成单格选择。
                pane.apply_table_event(&TableEvent::SelectCell(1, 2));
                assert_eq!(pane.session.selection_bounds(), Some((1, 0, 1, 1)));
                pane.apply_table_event(&TableEvent::SelectCell(0, 1));
                assert_eq!(pane.session.selection_bounds(), Some((0, 0, 0, 0)));
                pane.cell_press(0, 0, PressMode::Plain, cx);
                pane.cell_drag(1, 1, cx);
                assert_eq!(pane.session.selection_bounds(), Some((0, 0, 1, 1)), "拖选和行头用同一个矩形模型");
            });
        });
    }

    /// 上一帧里每一格画出来的选中状态；格子按与点击相同的稳定 ElementId 定位。
    fn painted_cells(
        visual: &mut VisualTestContext,
        rows: usize,
        table_columns: usize,
    ) -> std::collections::HashMap<(usize, usize), (bool, bool, Bounds<Pixels>)> {
        let snapshots = visual.update(|window, _| gpui_kit::base::test_support::snapshots(window));
        let mut painted = std::collections::HashMap::new();
        for row in 0..rows {
            for col_ix in 0..table_columns {
                let id = gpui_kit::ElementId::named_usize(format!("grid-cell-{row}"), col_ix);
                if let Some(snapshot) = snapshots.iter().find(|item| item.path().last() == Some(&id)) {
                    painted.insert((row, col_ix), (snapshot.selected() == Some(true), snapshot.visible(), snapshot.bounds()));
                }
            }
        }
        painted
    }

    /// 帧里每一格的高亮必须与 session 的选区矩形逐格一致：矩形内每格都亮（行号列随整行），
    /// 矩形外都不亮，都真的画在可见区域里，而且和复制、统计框住的格子数相同。
    fn assert_paint_matches_selection(cx: &mut TestAppContext, window: AnyWindowHandle, pane: &Entity<GridPane>) {
        cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
        let (bounds, rows, columns, expected_cells) = cx.update(|cx| {
            let pane = pane.read(cx);
            let session = &pane.session;
            (session.selection_bounds(), session.loaded_len(), session.column_names().len(), session.selection_count())
        });
        let mut visual = VisualTestContext::from_window(window, cx);
        // 表头那一列之外，行号列 + 每个数据列都由格子自己画出选中状态。
        let painted = painted_cells(&mut visual, rows, columns + 1);
        assert_eq!(painted.len(), rows * (columns + 1), "可见窗口里的每一格都要真的画出来 {:?}", painted.keys());
        let mut selected = 0;
        for row in 0..rows {
            for col_ix in 0..=columns {
                let expected = bounds.is_some_and(|(row_start, column_start, row_end, column_end)| {
                    if row < row_start || row > row_end {
                        return false;
                    }
                    // 列 0 是行号列，只有整行（首列到末列）被选中时才亮，与 cell_selected 同一套规则。
                    if col_ix == 0 {
                        return column_start == 0 && columns > 0 && column_end + 1 >= columns;
                    }
                    (column_start..=column_end).contains(&(col_ix - 1))
                });
                let (is_selected, visible, _) = painted[&(row, col_ix)];
                assert_eq!(is_selected, expected, "({row}, {col_ix}) 格的高亮必须与选区矩形 {bounds:?} 一致");
                assert!(visible, "({row}, {col_ix}) 格应画在可见区域里");
                if col_ix > 0 && is_selected {
                    selected += 1;
                }
            }
        }
        assert_eq!(selected, expected_cells, "画出来的格子数必须等于复制和统计的范围");
    }

    /// 拖选、Ctrl/Cmd 加选、Shift 键盘扩展、行头与列头选择都从同一个 anchor/focus 矩形取高亮：
    /// 每一格都画出选中底色，行头选择画整行而不是最后一格。
    #[gpui_kit::test]
    fn every_cell_in_the_selection_rectangle_is_painted_selected(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant(&pane, cx);
            window.render_frame(cx);
        })
        .unwrap();

        // 鼠标拖选 2x2：矩形里的四格都要亮，行号列随整行走。
        cx.update(|cx| pane.update(cx, |pane, cx| pane.cell_press(0, 0, PressMode::Plain, cx)));
        settle(cx);
        cx.update(|cx| pane.update(cx, |pane, cx| pane.cell_drag(1, 1, cx)));
        settle(cx);
        cx.update(|cx| pane.update(cx, |pane, _| pane.cell_release()));
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert_eq!(pane.session.tsv(), "1\t甲\n2\t乙", "拖选范围就是高亮范围");
            assert_eq!(selected_cells(&pane.session), 4);
        });
        assert_paint_matches_selection(cx, window, &pane);
        // 高亮画在真实的格子元素上：观测到的边界就是点击用的那个格的边界。
        let mut visual = VisualTestContext::from_window(window, cx);
        let painted = painted_cells(&mut visual, 2, 3);
        assert_eq!(
            painted[&(1, 2)].2,
            visual.debug_bounds("grid-cell-1-2").expect("name 格"),
            "高亮与格子是同一个元素"
        );
        // 选中格子不再常驻统计条：选择本身只靠高亮表达，不额外占位。
        assert!(visual.debug_bounds("grid-stats").is_none(), "选择时不显示自动统计");

        // Ctrl/Cmd 单击换角：同一个矩形，同样每格都亮。
        cx.update(|cx| pane.update(cx, |pane, cx| pane.cell_press(0, 0, PressMode::Plain, cx)));
        settle(cx);
        cx.update(|cx| pane.update(cx, |pane, cx| pane.cell_press(1, 1, PressMode::Extend, cx)));
        settle(cx);
        assert_paint_matches_selection(cx, window, &pane);

        // 键盘 Shift 扩展：和鼠标落在同一组行列上。
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                pane.session.select(0, 0, false);
                pane.session.move_key("down", true);
                pane.session.move_key("right", true);
                pane.sync_table(cx);
            })
        });
        settle(cx);
        assert_paint_matches_selection(cx, window, &pane);

        // 单格选择第一列数据：行号格不跟着亮。行号只表示整行选中，
        // 否则选一个数据格会顺手点亮一个没被选中的行号格。
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                pane.session.select(0, 0, false);
                pane.sync_table(cx);
            })
        });
        settle(cx);
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert_eq!(pane.session.selection_bounds(), Some((0, 0, 0, 0)));
            assert_eq!(selected_cells(&pane.session), 1);
        });
        assert_paint_matches_selection(cx, window, &pane);
        let mut visual = VisualTestContext::from_window(window, cx);
        let painted = painted_cells(&mut visual, 2, 3);
        assert!(!painted[&(0, 0)].0, "单格选择第一列数据时行号格不该亮");
        assert!(painted[&(0, 1)].0, "被选中的数据格自己要亮");

        // 行头选择：整行都亮，不是只有最后一格；别的行不亮。
        cx.update(|cx| pane.update(cx, |pane, _| pane.apply_table_event(&TableEvent::SelectRow(1))));
        settle(cx);
        cx.update(|cx| assert_eq!(pane.read(cx).session.selection_bounds(), Some((1, 0, 1, 1))));
        assert_paint_matches_selection(cx, window, &pane);

        // 列头选择：整列都亮，与整列复制和统计的范围一致。
        cx.update(|cx| pane.update(cx, |pane, _| pane.apply_table_event(&TableEvent::SelectColumn(2))));
        settle(cx);
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert_eq!(pane.session.selection_bounds(), Some((0, 1, 1, 1)), "列头选择应覆盖整列");
            assert_eq!(selected_cells(&pane.session), 2);
        });
        assert_paint_matches_selection(cx, window, &pane);

        // 取消选择后不再有格子亮着，也没有统计条。
        cx.update(|cx| pane.update(cx, |pane, cx| {
            pane.session.clear_selection();
            pane.sync_table(cx);
        }));
        settle(cx);
        assert_paint_matches_selection(cx, window, &pane);
        let mut visual = VisualTestContext::from_window(window, cx);
        assert!(visual.debug_bounds("grid-stats").is_none());
    }

    /// 真实点击表头坐标：# 列头不是数据列，不能把高亮、复制和统计挪到第一列数据上；
    /// 点第一列数据的列头才是整列选择，并且画出来的高亮格子数与复制、统计一致。
    #[gpui_kit::test]
    fn real_header_click_keeps_the_row_number_column_out_of_the_selection(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant(&pane, cx);
            window.render_frame(cx);
        })
        .unwrap();
        let mut visual = VisualTestContext::from_window(window, cx);
        let number_head = visual.debug_bounds("grid-th-0").expect("# 列头");
        let first_data_head = visual.debug_bounds("grid-th-1").expect("第一列数据的列头");
        assert!(
            first_data_head.origin.x >= number_head.origin.x + number_head.size.width,
            "第一列数据的列头应在行号列头右侧：{number_head:?} / {first_data_head:?}"
        );

        // 点 # 列头：这不是一次数据列选择，高亮、复制和统计都必须是空的。
        // 表格自己会先切进列模式（selected_col=0）并给 # 列刷激活底色，处理器早退之后
        // 必须在同一帧里把表格选择拉回会话，否则会出现"# 整列亮着、复制统计为空"。
        visual.simulate_click(number_head.center(), Modifiers::default());
        // 固定列头可能被滚动表头的留白盖住，合成点击不一定会打到它；无论点击落在哪里，
        // 表格为 # 列切列模式这一步都必须被覆盖，所以再走一次表格自己的列选择入口。
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                pane.table.update(cx, |state, cx| state.set_selected_col(0, cx));
            })
        });
        cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert_eq!(pane.session.selection_bounds(), None, "# 列头点下去不产生整列选择");
            assert_eq!(pane.session.tsv(), "", "点 # 列头不该带出第一列数据的复制内容");
            assert_eq!(selected_cells(&pane.session), 0, "统计范围也不能落到第一列数据上");
            let table = pane.table.read(cx);
            assert_eq!(table.selected_col(), None, "# 列头点下去表格不能停在列模式");
            assert!(!matches!(table.selection(), TableSelection::Column(_)), "# 列头不该留下整列选择：{:?}", table.selection());
        });
        // 帧级：没有任何格子（含行号格）画成选中。
        let mut visual = VisualTestContext::from_window(window, cx);
        let painted = painted_cells(&mut visual, 2, 3);
        for row in 0..2 {
            for col_ix in 0..3 {
                assert!(!painted[&(row, col_ix)].0, "# 列头点下去 ({row}, {col_ix}) 不该有选中底色");
            }
        }
        assert_paint_matches_selection(cx, window, &pane);

        // 键盘不能停在列模式：焦点回到表格后按方向键，选择落在行/单元格上。
        {
            let mut visual = VisualTestContext::from_window(window, cx);
            visual.update(|window, cx| {
                let handle = pane.read(cx).table.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
            });
            visual.simulate_keystrokes("down");
        }
        cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert!(
                !matches!(pane.table.read(cx).selection(), TableSelection::Column(_)),
                "点过 # 列头之后方向键不能还在列模式：{:?}",
                pane.table.read(cx).selection()
            );
            assert_eq!(pane.session.selection_bounds(), Some((0, 0, 0, 1)), "方向键应选中第一整行");
            assert_eq!(pane.session.tsv(), "1\t甲");
        });

        // 已经有单元格选择时点 # 列头：会话选择原样保留，表格回到会话焦点那一格，
        // # 列和行号格都没有激活底色。
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                pane.session.select(1, 1, false);
                pane.sync_table(cx);
            })
        });
        cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
        {
            let mut visual = VisualTestContext::from_window(window, cx);
            visual.simulate_click(number_head.center(), Modifiers::default());
        }
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                pane.table.update(cx, |state, cx| state.set_selected_col(0, cx));
            })
        });
        cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert_eq!(pane.session.selection_bounds(), Some((1, 1, 1, 1)), "# 列头不该改会话选择");
            assert_eq!(pane.session.tsv(), "乙");
            assert_eq!(pane.table.read(cx).selection(), TableSelection::Cell(1, 2), "表格要回到会话焦点那一格");
        });
        assert_paint_matches_selection(cx, window, &pane);

        // 点第一列数据的列头：整列矩形，高亮、复制与统计看的是同一段行列。
        visual.simulate_click(first_data_head.center(), Modifiers::default());
        visual.update(|_, cx| {
            let pane = pane.read(cx);
            assert_eq!(pane.session.selection_bounds(), Some((0, 0, 1, 0)), "第一列数据的列头应覆盖整列");
            assert_eq!(pane.session.tsv(), "1\n2", "整列 TSV 只含这一列数据");
            assert_eq!(selected_cells(&pane.session), 2);
        });
        assert_paint_matches_selection(cx, window, &pane);
    }

    /// 行号格（表格第 0 列）不是数据格：点它、右击它都只复位表格状态，不会把第一列数据选中。
    /// 行选择仍可从 12px 行头和行号列的整行高亮到达，所以这里按"忽略"处理。
    #[gpui_kit::test]
    fn row_number_cell_clicks_never_select_the_first_data_column(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant(&pane, cx);
            window.render_frame(cx);
        })
        .unwrap();
        let mut visual = VisualTestContext::from_window(window, cx);
        let number_cell = visual.debug_bounds("grid-cell-0-0").expect("行号格");
        let first_data_cell = visual.debug_bounds("grid-cell-0-1").expect("第一列数据格");
        assert!(
            first_data_cell.origin.x >= number_cell.origin.x + number_cell.size.width,
            "第一列数据应在行号列右侧：{number_cell:?} / {first_data_cell:?}"
        );

        // 点行号格：不加选择，也不落到第一列数据上。
        visual.simulate_click(number_cell.center(), Modifiers::default());
        cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert_eq!(pane.session.selection_bounds(), None, "点行号格不该选中第一列数据");
            assert_eq!(pane.session.tsv(), "");
            assert_ne!(pane.table.read(cx).selection(), TableSelection::Cell(0, 1), "表格不能把行号格当成第一列数据");
        });
        assert_paint_matches_selection(cx, window, &pane);

        // 右击行号格走同一条表格事件：同样不该把第一列数据选中。
        cx.update(|cx| {
            pane.update(cx, |pane, _| pane.apply_table_event(&TableEvent::RightClickedCell(0, 0)));
        });
        cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert_eq!(pane.session.selection_bounds(), None, "右击行号格不该选中第一列数据");
            assert_ne!(pane.table.read(cx).selection(), TableSelection::Cell(0, 1));
        });

        // 数据格本身照常可选，忽略行号格不会削弱正常点击。
        {
            let mut visual = VisualTestContext::from_window(window, cx);
            visual.simulate_click(first_data_cell.center(), Modifiers::default());
        }
        cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert_eq!(pane.session.selection_bounds(), Some((0, 0, 0, 0)), "点第一列数据格应选中这一格");
            assert_eq!(pane.session.tsv(), "1");
            assert_eq!(pane.table.read(cx).selection(), TableSelection::Cell(0, 1));
        });
        assert_paint_matches_selection(cx, window, &pane);
    }

    /// 超过默认 1 KB 预览上限的文本：页面读取只会给它截断预览。
    fn envelope_body() -> String {
        format!("\u{1}t:4096:{}", "预".repeat(12))
    }

    fn plant_long(pane: &Entity<GridPane>, cx: &mut App) {
        let body = envelope_body();
        pane.update(cx, |pane, cx| {
            pane.plant_writable(
                target(),
                vec![("id", "integer"), ("body", "text")],
                vec![vec![Some("1"), Some(body.as_str())]],
                "id",
                cx,
            );
        });
    }

    /// 两行都只有长文本预览：覆盖整列复制与整列 WHERE 的原值读取。
    fn plant_long_column(pane: &Entity<GridPane>, cx: &mut App) {
        let body = envelope_body();
        pane.update(cx, |pane, cx| {
            pane.plant_writable(
                target(),
                vec![("id", "integer"), ("body", "text")],
                vec![vec![Some("1"), Some(body.as_str())], vec![Some("2"), Some(body.as_str())]],
                "id",
                cx,
            );
        });
    }

    fn long_full(index: usize) -> String {
        format!("长文{}", "甲".repeat(600 + index))
    }

    fn selection_state(pane: &GridPane) -> (Option<(String, String)>, usize) {
        (pane.session.selection_ids().map(|(anchor, focus)| (anchor.to_owned(), focus.to_owned())), pane.session.visible_bounds().0)
    }

    #[gpui_kit::test]
    fn viewer_shares_one_original_with_copy_edit_where_and_export(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant_long(&pane, cx);
            window.render_frame(cx);
        })
        .unwrap();
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                assert!(pane.session.needs_full(0, 1), "长文本只读到了预览");
                pane.session.select(0, 1, false);
                // 普通选择不打开查看面板，也不触发读取。
                assert!(pane.session.viewer().is_none());
                assert_eq!(pane.command("copy-region", cx), None, "原值没到手前不复制预览");
                assert!(!pane.notice.is_empty(), "应说明为什么还没有复制");
                assert!(pane.session.viewer().is_none(), "复制不会顺带打开查看面板");
            });
        });
        let full = format!("正文{}", "甲".repeat(600));
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                // 定位读取的结果回到会话后，五个入口用的是同一个原值。
                let read = pane.session.pending_read(0, 1).expect("长文本需要按定位读取");
                assert!(pane.session.apply_full(&read, Some(full.clone())));
                assert!(pane.session.begin_full(0, 1).is_none(), "读到原值后不再重复读取");
                assert_eq!(pane.command("copy-region", cx).as_deref(), Some(full.as_str()));
                let _ = pane.command("grid-value-open", cx);
                assert_eq!(pane.session.viewer_state().map(|state| state.phase), Some(CellPhase::Full));
                assert_eq!(pane.session.viewer().map(|shown| shown.body), Some(full.clone()));
                let _ = pane.command("copy-region", cx);
                assert!(pane.session.begin_full(0, 1).is_none(), "复用同一次读取，不发起第二次");
                let where_sql = pane.command("grid-copy-where", cx).expect("WHERE 用原值");
                assert!(where_sql.contains(&full), "{where_sql}");
                let (_, _, rows, _) = pane.exchange_sheet();
                assert_eq!(rows[0][1].as_deref(), Some(full.as_str()), "导出拿到同一个原值");
            });
        });
        // 编辑栏里放的是原值，不是预览信封。
        cx.update_window(window, |_, window, cx| {
            pane.update(cx, |pane, cx| {
                let _ = pane.command("edit-cell", cx);
            });
            window.render_frame(cx);
        })
        .unwrap();
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert!(pane.editing());
            assert_eq!(pane.edit_value(cx), full);
        });
    }

    #[gpui_kit::test]
    fn failed_full_value_marks_the_viewer_error_and_retries_without_the_preview(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant_long(&pane, cx);
            window.render_frame(cx);
        })
        .unwrap();
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                pane.session.select(0, 1, false);
                // 这条连接没打开：读取失败，查看面板标成错误并保留预览。
                let _ = pane.command("grid-value-open", cx);
                assert_eq!(pane.session.viewer_state().map(|state| state.phase), Some(CellPhase::Error));
                assert!(pane.session.viewer_state().is_some_and(|state| !state.message.is_empty()));
                assert!(pane.session.viewer().unwrap().preview);
                assert_eq!(pane.command("copy-region", cx), None, "失败后仍不复制预览");
                // 重试仍走同一条定位读取；连接还是没打开，于是继续是错误而不是原值。
                let _ = pane.command("grid-value-retry", cx);
                assert_eq!(pane.session.viewer_state().map(|state| state.phase), Some(CellPhase::Error));
                assert_eq!(pane.command("copy-region", cx), None, "失败后仍拿不到原值");
            });
        });
    }

    /// 列头菜单的整列复制和整列 WHERE 与单元格菜单同名动作走同一条原值读取：
    /// 有预览时不复制空串也不静默无动作，读不到说明原因；读到原值后复制的是原值。
    #[gpui_kit::test]
    fn column_menu_copy_and_where_use_the_shared_full_read(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant_long_column(&pane, cx);
            window.render_frame(cx);
        })
        .unwrap();
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                assert!(pane.session.needs_full(0, 1) && pane.session.needs_full(1, 1), "两行都只读到预览");
                // 读不到原值时剪贴板保持原内容，绝不写一个空串覆盖它。
                cx.write_to_clipboard(ClipboardItem::new_string("剪贴板原内容".to_owned()));
                pane.column_command(1, "column-copy", cx);
                assert!(!pane.notice.is_empty(), "整列复制读不到原值要说明原因，不能静默无动作");
                pane.column_command(1, "column-copy-where", cx);
                assert!(!pane.notice.is_empty(), "整列 WHERE 同样要说明原因");
                assert_eq!(pane.session.copy_column(), "", "预览不参与复制");
            });
        });
        cx.update(|cx| {
            assert_eq!(
                cx.read_from_clipboard().and_then(|item| item.text()).as_deref(),
                Some("剪贴板原内容"),
                "没有原值时不能把空串写进剪贴板"
            );
        });

        // 按定位读到的原值回到会话后，列头复制与整列 WHERE 都用这两个原值。
        let full: Vec<String> = (0..2).map(long_full).collect();
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                for (row, value) in full.iter().enumerate() {
                    let read = pane.session.pending_read(row, 1).expect("预览格需要按定位读取");
                    assert!(pane.session.apply_full(&read, Some(value.clone())));
                }
                pane.column_command(1, "column-copy", cx);
            });
        });
        cx.update(|cx| {
            let copied = cx.read_from_clipboard().and_then(|item| item.text()).unwrap_or_default();
            assert_eq!(copied, format!("{}\n{}", full[0], full[1]), "整列复制用原值");
            assert!(!copied.contains('\u{1}'), "剪贴板里不能出现预览信封");
        });
        cx.update(|cx| {
            pane.update(cx, |pane, cx| pane.column_command(1, "column-copy-where", cx));
        });
        cx.update(|cx| {
            let copied = cx.read_from_clipboard().and_then(|item| item.text()).unwrap_or_default();
            assert!(copied.starts_with("WHERE "), "{copied}");
            assert!(copied.contains(&full[0]), "整列 WHERE 用原值：{copied}");
            assert!(!copied.contains('\u{1}'), "条件里不能出现预览信封：{copied}");
        });
    }

    #[gpui_kit::test]
    fn foreign_key_entries_only_apply_to_the_foreign_key_column(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant(&pane, cx);
            window.render_frame(cx);
        })
        .unwrap();
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                pane.session.set_foreign_keys(vec![ForeignKeyInfo {
                    name: "fk_people".to_owned(),
                    columns: vec!["name".to_owned()],
                    ref_schema: "main".to_owned(),
                    ref_table: "people".to_owned(),
                    ref_columns: vec!["name".to_owned()],
                }]);
                pane.session.select(0, 1, false);
                pane.sync_table(cx);
                assert!(pane.table.read(cx).delegate().fk, "外键列上给出导航入口");
                assert_eq!(pane.table.read(cx).delegate().unresolved, 0, "没有预览就不给读取完整值入口");
                pane.session.select(0, 0, false);
                pane.sync_table(cx);
                assert!(!pane.table.read(cx).delegate().fk, "非外键列不给导航入口");
            });
        });
        // 有预览值时"读取完整值"入口出现。
        cx.update_window(window, |_, window, cx| {
            plant_long(&pane, cx);
            window.render_frame(cx);
        })
        .unwrap();
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert_eq!(pane.table.read(cx).delegate().unresolved, 1);
        });
    }

    #[gpui_kit::test]
    fn cancelled_edit_and_closed_viewer_keep_selection_scroll_staging_and_viewer(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant(&pane, cx);
            window.render_frame(cx);
        })
        .unwrap();
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                pane.session.select(1, 1, false);
                let before = selection_state(pane);
                assert_eq!(pane.edit_focus(Some("改".into()), cx), Gate::Writable);
                let _ = pane.command("grid-value-open", cx);
                let viewer = pane.session.viewer_state().cloned();
                assert!(viewer.is_some());
                // 取消编辑不改选择、滚动、暂存和查看目标。
                let _ = pane.command("cancel-edit", cx);
                assert_eq!(selection_state(pane), before);
                assert!(pane.session.uncommitted(), "取消编辑不影响已暂存的值");
                assert_eq!(pane.session.viewer_state().cloned(), viewer);
                // 关闭查看面板后选择仍在同一格。
                let _ = pane.command("grid-value-close", cx);
                assert!(pane.session.viewer().is_none());
                assert_eq!(selection_state(pane), before);
            });
        });
        // Esc 关闭查看面板后键盘回到网格，不会困在已经消失的弹层里。
        cx.update_window(window, |_, window, cx| {
            let handle = pane.read(cx).focus_handle();
            window.focus(&handle, cx);
            pane.update(cx, |pane, cx| {
                pane.session.select(1, 1, false);
                let _ = pane.command("grid-value-open", cx);
            });
            window.render_frame(cx);
        })
        .unwrap();
        cx.simulate_keystrokes(window, "escape");
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(pane.read(cx).session.viewer().is_none(), "Esc 应关闭查看面板");
            let handle = pane.read(cx).focus_handle();
            assert!(handle.is_focused(window), "关闭后焦点要回到网格");
        })
        .unwrap();
    }

    fn plant_writable(pane: &mut GridPane, cx: &mut Context<GridPane>) {
        pane.plant_writable(
            target(),
            vec![("id", "integer"), ("name", "text")],
            vec![vec![Some("1"), Some("甲")], vec![Some("2"), Some("乙")]],
            "id",
            cx,
        );
    }
}

/// 结果复制、列显示、记录视图和批量写入在真实窗口里的行为（R2/R3/R4/R13）。
#[cfg(test)]
mod workbench_tests {
    use super::*;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{AnyWindowHandle, Bounds, Point, TestAppContext, VisualTestContext, WindowBounds, WindowOptions, size};

    fn target() -> GridTarget {
        GridTarget {
            connection_id: "notes".to_owned(),
            connection_name: "笔记".to_owned(),
            database: "main".to_owned(),
            schema: String::new(),
            table: "people".to_owned(),
            engine: "sqlite".to_owned(),
            environment: "dev".to_owned(),
        }
    }

    fn open(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<GridPane>) {
        let dir = std::env::temp_dir().join(format!("rillsql-grid-workbench-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let services = Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            let bounds = Bounds { origin: Point::default(), size: size(px(1100.), px(720.)) };
            gpui_kit::open_window(
                WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
                cx,
                |window, cx| cx.new(|cx| GridPane::new(services, window, cx)),
            )
            .expect("grid pane")
        })
    }

    /// 指定窗口宽度的网格窗口：1360/900/640 三档证据共用同一条布局路径。
    fn open_sized(cx: &mut TestAppContext, width: f32) -> (AnyWindowHandle, Entity<GridPane>) {
        let dir = std::env::temp_dir().join(format!("rillsql-grid-width-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let services = Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            let bounds = Bounds { origin: Point::default(), size: size(px(width), px(720.)) };
            gpui_kit::open_window(
                WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
                cx,
                |window, cx| cx.new(|cx| GridPane::new(services, window, cx)),
            )
            .expect("grid pane")
        })
    }

    /// people(id 主键, name, note)：两行，含一个 NULL 和一个带逗号与引号的文本。
    fn plant(pane: &Entity<GridPane>, cx: &mut App) {
        pane.update(cx, |pane, cx| {
            pane.plant_writable(
                target(),
                vec![("id", "integer"), ("name", "text"), ("note", "text")],
                vec![vec![Some("1"), Some("a,\"b\""), None], vec![Some("2"), Some("乙"), Some("x")], vec![Some("3"), Some("丙"), Some("y")]],
                "id",
                cx,
            );
        });
    }

    fn clipboard(cx: &mut TestAppContext) -> Option<String> {
        cx.update(|cx| cx.read_from_clipboard().and_then(|item| item.text()))
    }

    fn header_names(pane: &Entity<GridPane>, cx: &mut TestAppContext) -> Vec<String> {
        cx.update(|cx| pane.read(cx).table.read(cx).delegate().columns.iter().map(|(name, _, _)| name.clone()).collect())
    }

    #[gpui_kit::test]
    fn copy_as_writes_each_format_or_keeps_the_clipboard_with_a_reason(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant(&pane, cx);
            window.render_frame(cx);
        })
        .unwrap();
        cx.update(|cx| cx.write_to_clipboard(ClipboardItem::new_string("原内容".to_owned())));
        // 空选区：剪贴板不动，给出原因；菜单里这些条目是禁用的。
        let notice = cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                pane.session.clear_selection();
                assert_eq!(pane.command("grid-copy-as-csv", cx), None);
                pane.notice.clone()
            })
        });
        assert_eq!(notice, i18n::text(Locale::ZhCn, "grid.copyAs.empty"));
        assert_eq!(clipboard(cx).as_deref(), Some("原内容"));
        let empty = CellMenuState { writable: true, has_selection: false, ..CellMenuState::default() };
        for format in CopyFormat::ALL {
            assert!(cell_item_disabled(format.command(), false, empty), "{format:?}");
            assert!(!cell_item_disabled(format.command(), false, CellMenuState { has_selection: true, ..empty }), "{format:?}");
        }
        // 2 行 × 3 列：逐格式核对剪贴板文本。
        let expected = [
            ("grid-copy-as-tsv", "id\tname\tnote\n1\t\"a,\"\"b\"\"\"\tNULL\n2\t乙\tx"),
            ("grid-copy-as-csv", "\"id\",\"name\",\"note\"\r\n\"1\",\"a,\"\"b\"\"\",NULL\r\n\"2\",\"乙\",\"x\""),
            ("grid-copy-as-json", "[{\"id\":\"1\",\"name\":\"a,\\\"b\\\"\",\"note\":null},{\"id\":\"2\",\"name\":\"乙\",\"note\":\"x\"}]"),
            ("grid-copy-as-markdown", "| id | name | note |\r\n| --- | --- | --- |\r\n| 1 | a,\"b\" | NULL |\r\n| 2 | 乙 | x |"),
            ("grid-copy-as-insert", "INSERT INTO \"people\" (\"id\", \"name\", \"note\") VALUES (1, 'a,\"b\"', NULL);\r\nINSERT INTO \"people\" (\"id\", \"name\", \"note\") VALUES (2, '乙', 'x');"),
        ];
        for (id, text) in expected {
            cx.update(|cx| {
                pane.update(cx, |pane, cx| {
                    pane.session.select(0, 0, false);
                    pane.session.select(1, 2, true);
                    assert_eq!(pane.command(id, cx).as_deref(), Some(text), "{id}");
                })
            });
            assert_eq!(clipboard(cx).as_deref(), Some(text), "{id}");
        }
        // 未定位的查询结果：INSERT 用占位表名，并告诉用户。
        let notice = cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                let sheet = QuerySheet { sql: "select name from a join b on a.id = b.id".into(), columns: vec!["name".into()], rows: vec![vec![Some("甲".into())]], truncated: false, full_count: Some(1) };
                pane.show_query(target(), sheet, cx);
                pane.session.select(0, 0, false);
                assert_eq!(pane.command("grid-copy-as-insert", cx).as_deref(), Some("INSERT INTO \"table_name\" (\"name\") VALUES ('甲');"));
                pane.notice.clone()
            })
        });
        assert_eq!(notice, i18n::text(Locale::ZhCn, "grid.copyAs.placeholder"));
    }

    #[gpui_kit::test]
    fn hidden_columns_are_not_drawn_but_stay_in_copies(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant(&pane, cx);
            window.render_frame(cx);
        })
        .unwrap();
        assert_eq!(header_names(&pane, cx), vec!["#", "id", "name", "note"]);
        {
            let mut visual = VisualTestContext::from_window(window, cx);
            assert!(visual.debug_bounds("grid-th-3").is_some());
        }
        cx.update(|cx| pane.update(cx, |pane, cx| pane.column_command(1, "column-hide", cx)));
        cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
        assert_eq!(header_names(&pane, cx), vec!["#", "id", "note"], "隐藏的列不再画出来");
        {
            let mut visual = VisualTestContext::from_window(window, cx);
            assert!(visual.debug_bounds("grid-th-3").is_none());
            // 第 2 个数据列现在画的是 note 列的值。
            assert!(visual.debug_bounds("grid-cell-0-2").is_some());
        }
        let row = cx.update(|cx| pane.update(cx, |pane, cx| {
            pane.session.select(1, 0, false);
            pane.command("grid-copy-row", cx)
        }));
        assert_eq!(row.as_deref(), Some("2\t乙\tx"), "复制整行仍包含隐藏列");
        // 表格上的点击按显示位置换回逻辑列：点第 2 个数据列选中的是 note。
        cx.update(|cx| pane.update(cx, |pane, _| pane.apply_table_event(&TableEvent::SelectCell(0, 2))));
        assert_eq!(cx.update(|cx| pane.read(cx).session.focus_cell()), Some((0, 2)));
        // 只剩一列可见时"隐藏列"禁用。
        cx.update(|cx| pane.update(cx, |pane, cx| pane.column_command(2, "column-hide", cx)));
        let state = cx.update(|cx| {
            let delegate = pane.read(cx).table.read(cx).delegate();
            ColumnMenuState { sorted: None, can_hide: delegate.visible.len() > 1, frozen: false, hidden: delegate.hidden.clone() }
        });
        assert!(column_item_disabled("column-hide", &state));
        assert!(!column_item_disabled("column-show-all", &state));
        assert_eq!(state.hidden.len(), 2);
        cx.update(|cx| pane.update(cx, |pane, cx| pane.column_command(0, "column-show-all", cx)));
        assert_eq!(header_names(&pane, cx), vec!["#", "id", "name", "note"]);
    }

    #[gpui_kit::test]
    fn frozen_columns_stay_visible_while_scrolling_right(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        let names: Vec<String> = (0..12).map(|index| format!("c{index}")).collect();
        cx.update_window(window, |_, window, cx| {
            pane.update(cx, |pane, cx| {
                let columns: Vec<(&str, &str)> = names.iter().map(|name| (name.as_str(), "text")).collect();
                let values: Vec<Vec<Option<&str>>> = (0..3).map(|_| names.iter().map(|name| Some(name.as_str())).collect()).collect();
                pane.plant_writable(target(), columns, values, "c0", cx);
                pane.column_command(1, "column-freeze", cx);
            });
            window.render_frame(cx);
        })
        .unwrap();
        let fixed = cx.update(|cx| pane.read(cx).table.read(cx).delegate().columns.iter().map(|(_, _, fixed)| *fixed).collect::<Vec<_>>());
        assert_eq!(&fixed[..4], &[true, true, true, false], "行号列和前两个数据列固定在左侧");
        let (first, second) = {
            let mut visual = VisualTestContext::from_window(window, cx);
            (visual.debug_bounds("grid-th-1").expect("第 1 列"), visual.debug_bounds("grid-th-2").expect("第 2 列"))
        };
        cx.update(|cx| pane.update(cx, |pane, cx| pane.table.update(cx, |state, cx| state.scroll_to_col(12, cx))));
        cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
        cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
        let mut visual = VisualTestContext::from_window(window, cx);
        let last = visual.debug_bounds("grid-th-12").expect("横向滚动到最后一列");
        assert!(last.origin.x < px(1100.), "{last:?}");
        assert_eq!(visual.debug_bounds("grid-th-1"), Some(first), "冻结的第 1 列不随横向滚动移动");
        assert_eq!(visual.debug_bounds("grid-th-2"), Some(second), "冻结的第 2 列不随横向滚动移动");
        let third = visual.debug_bounds("grid-th-3");
        assert!(third.is_none_or(|bounds| bounds.origin.x != second.origin.x + second.size.width), "未冻结的列滚走了：{third:?}");
    }

    #[gpui_kit::test]
    fn auto_fit_widens_a_long_text_column_within_the_limit(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        let long = "很长的一段备注文本，需要把列拉宽才能完整显示".repeat(2);
        let longest = "超".repeat(200);
        cx.update_window(window, |_, window, cx| {
            pane.update(cx, |pane, cx| {
                pane.plant_writable(
                    target(),
                    vec![("id", "integer"), ("note", "text"), ("huge", "text")],
                    vec![vec![Some("1"), Some(long.as_str()), Some(longest.as_str())], vec![Some("2"), Some("短"), Some("短")]],
                    "id",
                    cx,
                );
            });
            window.render_frame(cx);
        })
        .unwrap();
        let before = cx.update(|cx| pane.read(cx).session.column_width(1));
        cx.update(|cx| pane.update(cx, |pane, cx| pane.column_command(1, "column-fit", cx)));
        cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
        cx.run_until_parked();
        let after = cx.update(|cx| pane.read(cx).session.column_width(1));
        assert!(after > before + 100.0, "长文本列应变宽：{before} -> {after}");
        assert!(after <= 640.0);
        let measured = cx.update_window(window, |_, window, cx| {
            let font = gpui_kit::Font { family: cx.theme().font_family.clone(), ..Default::default() };
            let run = gpui_kit::TextRun { len: long.len(), font, color: cx.theme().foreground, background_color: None, underline: None, strikethrough: None };
            f32::from(window.text_system().shape_line(SharedString::from(long.clone()), window.rem_size() * 0.875, &[run], None).width)
        });
        assert!(after >= measured.unwrap(), "列宽要放得下已加载行里最长的值");
        // 超过上限的值只拉到上限。
        cx.update(|cx| pane.update(cx, |pane, cx| pane.column_command(2, "column-fit", cx)));
        cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
        cx.run_until_parked();
        assert_eq!(cx.update(|cx| pane.read(cx).session.column_width(2)), 640.0);
    }

    #[gpui_kit::test]
    fn record_view_shows_the_focused_row_and_disables_the_ends(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant(&pane, cx);
            pane.update(cx, |pane, cx| {
                pane.session.select(1, 1, false);
                let _ = pane.command("grid-record-toggle", cx);
            });
            window.render_frame(cx);
        })
        .unwrap();
        {
            let mut visual = VisualTestContext::from_window(window, cx);
            assert!(visual.debug_bounds("grid-record").is_some(), "记录视图画在网格右侧");
            for selector in ["grid-record-field-0", "grid-record-field-1", "grid-record-field-2"] {
                assert!(visual.debug_bounds(selector).is_some(), "列出每一列：{selector}");
            }
        }
        let view = cx.update(|cx| record::record_view(&pane.read(cx).session).unwrap());
        assert_eq!(view.fields[1].label, "乙");
        cx.update(|cx| pane.update(cx, |pane, cx| {
            let _ = pane.command("grid-record-next", cx);
        }));
        let view = cx.update(|cx| record::record_view(&pane.read(cx).session).unwrap());
        assert_eq!(view.fields[1].label, "丙");
        assert!(!view.can_next, "最后一行时下一行禁用");
        // 网格的焦点跟着走：高亮和记录视图是同一行。
        assert_eq!(cx.update(|cx| pane.read(cx).session.focus_cell()), Some((2, 1)));
        cx.update(|cx| pane.update(cx, |pane, cx| {
            let _ = pane.command("grid-record-close", cx);
        }));
        cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
        let mut visual = VisualTestContext::from_window(window, cx);
        assert!(visual.debug_bounds("grid-record").is_none());
    }

    /// 记录面板的非模态整行暂存：点字段就地改，Enter 暂存、Esc 取消。
    /// 只读格给原因并打开完整值；选区多于一格时面板底部出现批量动作。
    #[gpui_kit::test]
    fn record_panel_stages_a_field_in_place_and_offers_batch_actions(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant(&pane, cx);
            pane.update(cx, |pane, cx| {
                let _ = pane.command("grid-record-toggle", cx);
                pane.record_field_press(1, cx);
            });
            window.render_frame(cx);
        })
        .unwrap();
        cx.update(|cx| assert_eq!(pane.read(cx).record_edit, Some(1), "字段就地进入暂存"));
        cx.update_window(window, |_, window, cx| {
            window.press("secondary-a", cx);
            window.input("新值", cx);
        })
        .unwrap();
        cx.update_window(window, |_, window, cx| window.press("enter", cx)).unwrap();
        cx.run_until_parked();
        cx.update(|cx| {
            assert_eq!(pane.read(cx).cell(0, 1), "新值", "Enter 把值暂存到行上");
            assert!(pane.read(cx).session.uncommitted());
            assert_eq!(pane.read(cx).record_edit, None, "Enter 结束这次暂存");
        });
        // Esc 取消这次输入：已暂存的值保持不动。
        cx.update_window(window, |_, window, cx| {
            pane.update(cx, |pane, cx| pane.record_field_press(1, cx));
            window.render_frame(cx);
        })
        .unwrap();
        cx.update_window(window, |_, window, cx| {
            window.press("secondary-a", cx);
            window.input("丢弃", cx);
            window.press("escape", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update(|cx| {
            assert_eq!(pane.read(cx).record_edit, None, "Esc 结束暂存");
            assert_eq!(pane.read(cx).cell(0, 1), "新值", "Esc 不写值");
            assert!(pane.read(cx).session.uncommitted(), "已有暂存保留");
        });
        // 多格选区：面板底部出现这一选区适用的批量动作。
        cx.update_window(window, |_, window, cx| {
            pane.update(cx, |pane, _| {
                pane.session.select(0, 0, false);
                pane.session.select(1, 1, true);
            });
            window.render_frame(cx);
        })
        .unwrap();
        let mut visual = VisualTestContext::from_window(window, cx);
        let batch = visual.debug_bounds("grid-record-batch");
        assert!(batch.is_some(), "多格选区出现批量动作条");
    }

    /// 只读结果的记录面板不假装能写：点字段给出原因，并走同一条完整值查看路径。
    #[gpui_kit::test]
    fn record_panel_explains_readonly_fields_instead_of_staging(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            pane.update(cx, |pane, cx| {
                let sheet = QuerySheet {
                    sql: "select * from customers join people".to_owned(),
                    columns: vec!["name".to_owned()],
                    rows: vec![vec![Some("乙".to_owned())]],
                    truncated: false,
                    full_count: Some(1),
                };
                pane.show_query(target(), sheet, cx);
                let _ = pane.command("grid-record-toggle", cx);
                pane.record_field_press(0, cx);
            });
            window.render_frame(cx);
        })
        .unwrap();
        cx.update(|cx| {
            assert_eq!(pane.read(cx).record_edit, None, "只读格不进入暂存");
            assert!(!pane.read(cx).session.writable(), "查询结果不可写");
            assert!(!pane.read(cx).session.readonly_reason().is_empty(), "只读原因来自会话");
            assert!(pane.read(cx).session.viewer().is_some(), "只读时退化为查看完整值");
        });
    }
    /// 超过默认 1 KB 预览上限的文本：页面读取只会给它截断预览。
    fn long_body() -> String {
        format!("\u{1}t:4096:{}", "预".repeat(12))
    }

    fn plant_long_record(pane: &Entity<GridPane>, cx: &mut App) {
        let body = long_body();
        pane.update(cx, |pane, cx| {
            pane.plant_writable(
                target(),
                vec![("id", "integer"), ("body", "text")],
                vec![vec![Some("1"), Some(body.as_str())], vec![Some("2"), Some(body.as_str())]],
                "id",
                cx,
            );
        });
    }

    /// 记录面板里点长文本（预览信封）格：先按定位读原值，读到才进就地暂存。
    /// 绝不打开一个空输入框，也绝不会把空串暂存成一次静默改写。
    #[gpui_kit::test]
    fn record_panel_reads_the_original_before_staging_a_long_text_field(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant_long_record(&pane, cx);
            window.render_frame(cx);
        })
        .unwrap();
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                let _ = pane.command("grid-record-toggle", cx);
                assert!(pane.session.needs_full(0, 1), "长文本只读到了预览");
                pane.record_field_press(1, cx);
            });
        });
        cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert_eq!(pane.record_edit, None, "原值没到手前不打开就地暂存");
            assert_eq!(pane.record_edit_deferred, Some(1), "记下这一列，读完原值再回来");
            assert!(!pane.notice.is_empty(), "读不到原值要说明原因，不能静默无反应");
            assert!(!pane.session.uncommitted(), "没有暂存任何东西");
            assert!(!pane.cell(0, 1).contains('\u{1}'), "预览值原样保留，界面上不出现信封");
        });
        // 原值回来后回到同一条入口：输入框里是原值，Enter 暂存原值而不是空串。
        let full = format!("正文{}", "甲".repeat(600));
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                let read = pane.session.pending_read(0, 1).expect("预览格需要按定位读取");
                assert!(pane.session.apply_full(&read, Some(full.clone())));
                let _ = pane.command("record-edit", cx);
            });
        });
        cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert_eq!(pane.record_edit, Some(1));
            assert_eq!(pane.record_input.read(cx).value().to_string(), full, "输入框里是原值");
        });
        cx.update_window(window, |_, window, cx| window.press("enter", cx)).unwrap();
        cx.run_until_parked();
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert_eq!(pane.record_edit, None);
            assert!(pane.session.uncommitted(), "暂存的是这一格");
            // 暂存的实际值从生成的 UPDATE 里看：空串会变成 = ''，和原值完全不同。
            let sql = pane.session.update_statement();
            assert!(sql.contains(&full), "暂存的是原值而不是空串：{sql}");
        });
    }

    /// 就算暂存状态被别的路径带进绘制阶段，长文本格也不会摆出空输入框：
    /// 值还是预览信封时只解释原因，不进入暂存。
    #[gpui_kit::test]
    fn record_panel_never_opens_a_blank_editor_for_a_preview_cell(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant_long_record(&pane, cx);
            pane.update(cx, |pane, cx| {
                let _ = pane.command("grid-record-toggle", cx);
                // 绕过 record_field_press 直接摆出暂存状态，模拟按下之后这一行又被重读过。
                pane.record_edit = Some(1);
                pane.pending_record = Some((0, 1));
            });
            window.render_frame(cx);
        })
        .unwrap();
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert_eq!(pane.record_edit, None, "预览值不进暂存");
            assert!(pane.pending_record.is_none());
            assert!(!pane.notice.is_empty(), "要说明原值还没读到");
            assert_eq!(pane.record_input.read(cx).value().to_string(), "", "输入框没有被填上空串再提交");
        });
        // 这时按 Enter 不会把空串暂存上去。
        cx.update_window(window, |_, window, cx| window.press("enter", cx)).unwrap();
        cx.run_until_parked();
        cx.update(|cx| assert!(!pane.read(cx).session.uncommitted(), "Enter 没有暂存空串"));
    }

    /// 就地暂存开着时切换行/关面板：这次输入整个丢掉，绝不带到下一行去。
    #[gpui_kit::test]
    fn record_panel_switching_rows_discards_the_in_place_edit(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant(&pane, cx);
            pane.update(cx, |pane, cx| {
                let _ = pane.command("grid-record-toggle", cx);
                pane.record_field_press(1, cx);
            });
            window.render_frame(cx);
        })
        .unwrap();
        cx.update(|cx| assert_eq!(pane.read(cx).record_edit, Some(1), "字段就地进入暂存"));
        // 输入一半就切到下一行。
        cx.update_window(window, |_, window, cx| {
            window.press("secondary-a", cx);
            window.input("半截", cx);
        })
        .unwrap();
        cx.update(|cx| pane.update(cx, |pane, cx| {
            let _ = pane.command("grid-record-next", cx);
        }));
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert_eq!(pane.record_edit, None, "换行丢掉这次就地暂存");
            assert!(pane.pending_record.is_none());
            assert!(!pane.session.uncommitted(), "半截的输入没有进暂存");
            assert_eq!(pane.cell(1, 1), "乙", "新的一行原样");
        });
        cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
        // 记录视图现在画第二行，不带上一行的文字；Enter 也不会写进错的行。
        cx.update_window(window, |_, window, cx| window.press("enter", cx)).unwrap();
        cx.run_until_parked();
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert!(!pane.session.uncommitted(), "Enter 没有把上一行的文字写进新的一行");
            assert_eq!(pane.cell(1, 1), "乙");
        });
        // 关面板同样丢掉这次输入：重新打开时不会冒出上一行的文字。
        cx.update_window(window, |_, window, cx| {
            pane.update(cx, |pane, cx| pane.record_field_press(1, cx));
            window.render_frame(cx);
        })
        .unwrap();
        cx.update_window(window, |_, window, cx| {
            window.press("secondary-a", cx);
            window.input("丢掉", cx);
        })
        .unwrap();
        cx.update(|cx| pane.update(cx, |pane, cx| {
            let _ = pane.command("grid-record-close", cx);
        }));
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert_eq!(pane.record_edit, None, "关面板丢掉这次暂存");
            assert!(pane.pending_record.is_none());
            assert!(!pane.session.uncommitted(), "关面板不写值");
        });
        cx.update(|cx| pane.update(cx, |pane, cx| {
            let _ = pane.command("grid-record-toggle", cx);
        }));
        cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
        cx.update(|cx| assert_eq!(pane.read(cx).record_edit, None, "重新打开也不是上次那次输入"));
    }

    /// 记录面板里只是预览片段的字段要标出来：用户不会把截断显示当成完整值。
    #[gpui_kit::test]
    fn record_panel_marks_preview_fields(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant_long_record(&pane, cx);
            pane.update(cx, |pane, cx| {
                let _ = pane.command("grid-record-toggle", cx);
            });
            window.render_frame(cx);
        })
        .unwrap();
        let mut visual = VisualTestContext::from_window(window, cx);
        assert!(visual.debug_bounds("grid-record-preview-1").is_some(), "预览字段标出预览");
        assert!(visual.debug_bounds("grid-record-preview-0").is_none(), "普通值不标");
    }

    /// 工具条上的记录视图按钮点一下就打开，再点一下关上（走真实点击，不是直接调命令）。
    #[gpui_kit::test]
    fn record_toggle_button_opens_and_closes_the_record_view(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant(&pane, cx);
            pane.update(cx, |pane, _| pane.session.select(1, 1, false));
            window.render_frame(cx);
            window.click("grid-record-toggle", cx);
            window.render_frame(cx);
        })
        .unwrap();
        assert!(cx.update(|cx| pane.read(cx).record_open), "点按钮打开记录视图");
        {
            let mut visual = VisualTestContext::from_window(window, cx);
            assert!(visual.debug_bounds("grid-record").is_some());
        }
        cx.update_window(window, |_, window, cx| {
            window.click("grid-record-toggle", cx);
            window.render_frame(cx);
        })
        .unwrap();
        assert!(!cx.update(|cx| pane.read(cx).record_open), "再点一下关上");
    }

    #[gpui_kit::test]
    fn ctrl_v_pastes_tsv_only_from_the_grid_focus_and_respects_readonly(cx: &mut TestAppContext) {
        assert_eq!(grid_shortcut("v", true, false), Some("grid-paste"));
        assert_eq!(grid_shortcut("v", false, false), None);
        assert_eq!(grid_shortcut("v", true, true), None, "Ctrl+Shift+V 不占用");
        let (window, pane) = open(cx);
        cx.update(|cx| cx.write_to_clipboard(ClipboardItem::new_string("甲\t一\n乙\t二".to_owned())));
        cx.update_window(window, |_, window, cx| {
            plant(&pane, cx);
            pane.update(cx, |pane, _| pane.session.select(0, 1, false));
            let handle = pane.read(cx).focus_handle();
            window.focus(&handle, cx);
            window.press("secondary-v", cx);
        })
        .unwrap();
        cx.run_until_parked();
        let (staged, notice) = cx.update(|cx| (pane.read(cx).session.staged_count(), pane.read(cx).notice.clone()));
        assert_eq!(staged, 4, "{notice}");
        assert!(!pane_preview_open(&pane, cx), "粘贴只暂存，不打开预览、不写库");
        // 一次 Ctrl+Z 撤销整批。
        cx.update_window(window, |_, window, cx| window.press("secondary-z", cx)).unwrap();
        cx.run_until_parked();
        assert_eq!(cx.update(|cx| pane.read(cx).session.staged_count()), 0);
        // 焦点在最后一列：整次拒绝并说明越界，暂存数量不变。
        cx.update_window(window, |_, window, cx| {
            pane.update(cx, |pane, _| pane.session.select(0, 2, false));
            window.press("secondary-v", cx);
        })
        .unwrap();
        cx.run_until_parked();
        let (staged, notice) = cx.update(|cx| (pane.read(cx).session.staged_count(), pane.read(cx).notice.clone()));
        assert_eq!(staged, 0);
        assert_eq!(notice, i18n::text(Locale::ZhCn, "grid.batch.outOfRange"));
        // 只读结果：沿用只读原因，什么都不暂存；菜单里三项写入都禁用。
        cx.update_window(window, |_, window, cx| {
            pane.update(cx, |pane, cx| {
                let sheet = QuerySheet { sql: "select count(*) from people".into(), columns: vec!["n".into()], rows: vec![vec![Some("3".into())]], truncated: false, full_count: Some(1) };
                pane.show_query(target(), sheet, cx);
                pane.session.select(0, 0, false);
            });
            window.press("secondary-v", cx);
        })
        .unwrap();
        cx.run_until_parked();
        let (dirty, notice) = cx.update(|cx| (pane.read(cx).session.uncommitted(), pane.read(cx).notice.clone()));
        assert!(!dirty);
        assert!(notice.contains(&i18n::text(Locale::ZhCn, "grid.readonly.aggregate")), "{notice}");
        let readonly = CellMenuState { writable: false, has_selection: true, ..CellMenuState::default() };
        for id in ["grid-paste", "grid-clone-row", "grid-selection-set", "grid-selection-null"] {
            let needs_write = CELL_MENU.iter().find(|item| item.0 == id).map(|item| item.2).unwrap();
            assert!(cell_item_disabled(id, needs_write, readonly), "{id}");
        }
    }

    /// 编辑栏里的 Ctrl/⌘+V 仍由文本输入自己粘贴，网格不接管，也不产生批量暂存。
    #[gpui_kit::test]
    fn ctrl_v_inside_the_edit_bar_pastes_into_the_input_not_the_grid(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update(|cx| cx.write_to_clipboard(ClipboardItem::new_string("粘贴值".to_owned())));
        cx.update_window(window, |_, window, cx| {
            plant(&pane, cx);
            pane.update(cx, |pane, _| pane.session.select(0, 1, false));
            let handle = pane.read(cx).focus_handle();
            window.focus(&handle, cx);
            window.press("f2", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
        assert!(cx.update(|cx| pane.read(cx).editing()), "F2 打开编辑栏");
        cx.update_window(window, |_, window, cx| {
            window.press("secondary-a", cx);
            window.press("secondary-v", cx);
        })
        .unwrap();
        cx.run_until_parked();
        let (value, staged, dirty) = cx.update(|cx| (pane.read(cx).edit_value(cx), pane.read(cx).session.staged_count(), pane.read(cx).session.uncommitted()));
        assert_eq!(value, "粘贴值");
        assert_eq!(staged, 0);
        assert!(!dirty, "输入阶段不进入暂存");
    }

    fn pane_preview_open(pane: &Entity<GridPane>, cx: &mut TestAppContext) -> bool {
        cx.update(|cx| pane.read(cx).preview_open())
    }

    #[gpui_kit::test]
    fn set_value_on_selection_stages_one_batch_from_the_edit_bar(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant(&pane, cx);
            pane.update(cx, |pane, cx| {
                pane.session.select(0, 1, false);
                pane.session.select(2, 2, true);
                let _ = pane.command("grid-selection-set", cx);
            });
            window.render_frame(cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert!(cx.update(|cx| pane.read(cx).editing()), "选区设值打开编辑栏");
        cx.update_window(window, |_, window, cx| {
            window.input("统一", cx);
            window.press("enter", cx);
        })
        .unwrap();
        cx.run_until_parked();
        let (staged, editing) = cx.update(|cx| (pane.read(cx).session.staged_count(), pane.read(cx).editing()));
        assert_eq!(staged, 6);
        assert!(!editing);
        assert_eq!(cx.update(|cx| pane.read(cx).cell(1, 2)), "统一");
        // 复制为新行：主键留空的一条 INSERT。
        cx.update(|cx| pane.update(cx, |pane, cx| {
            let _ = pane.command("grid-undo", cx);
            pane.session.select(1, 1, false);
            let _ = pane.command("grid-clone-row", cx);
            let sql = pane.preview(cx).unwrap();
            assert_eq!(sql, vec!["INSERT INTO \"people\" (\"name\", \"note\") VALUES ('乙', 'x')".to_owned()]);
            pane.cancel_preview(cx);
        }));
        assert_eq!(cx.update(|cx| pane.read(cx).notice.clone()), i18n::text(Locale::ZhCn, "grid.write.cloned"));
    }

    /// 空网格的三类语义各用统一三态：无命中给“清除条件”，表里没有行给“插入行”，
    /// 查询没有返回行不给写入口；空态替换内容区，不保留表头骨架。
    #[gpui_kit::test]
    fn empty_grids_use_the_kit_states_and_their_own_next_step(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        // 表里没有行：无数据空态 + 添加入口。
        cx.update_window(window, |_, window, cx| {
            pane.update(cx, |pane, cx| {
                pane.plant_writable(target(), vec![("id", "integer"), ("name", "text")], Vec::<Vec<Option<&str>>>::new(), "id", cx);
            });
            window.render_frame(cx);
            assert!(window.try_find("empty-state").is_some(), "表里没有行用无数据空态");
            assert!(window.try_find("no-match-state").is_none());
            assert!(window.try_find("grid-body").is_none(), "空态替换内容区，不保留表头骨架");
            assert_eq!(window.find("grid-empty-insert").label(), Some("插入行"), "空态给添加入口");
        })
        .unwrap();
        // 条件没命中：无命中空态 + 清除条件，和“表里没有行”不是同一段文案。
        cx.update_window(window, |_, window, cx| {
            plant(&pane, cx);
            pane.update(cx, |pane, _| pane.session.set_search("不存在"));
            window.render_frame(cx);
            assert!(window.try_find("no-match-state").is_some(), "条件没命中用无命中空态");
            assert!(window.try_find("empty-state").is_none());
            assert_eq!(window.find("grid-condition-clear").label(), Some("清除条件"));
        })
        .unwrap();
        // 查询没有返回行：同样替换内容区，但查询结果不是可写来源，不给插入入口。
        cx.update_window(window, |_, window, cx| {
            // 查询结果自带一套空态：上一张表的查找条件不跟着带过去。
            let sheet = QuerySheet { sql: "select 1".to_owned(), columns: vec!["x".to_owned()], rows: Vec::new(), truncated: false, full_count: Some(0) };
            pane.update(cx, |pane, cx| {
                pane.session.set_search("");
                pane.show_query(target(), sheet, cx);
            });
            window.render_frame(cx);
            assert!(window.try_find("empty-state").is_some(), "查询没有返回行也要有空态");
            assert!(window.try_find("no-match-state").is_none());
            assert!(window.try_find("grid-empty-insert").is_none(), "查询结果不给插入入口");
        })
        .unwrap();
    }

    /// 一次都没读到时的失败：整块错误态替换内容区，并给一个重试动作。
    #[gpui_kit::test]
    fn a_first_read_failure_uses_the_kit_error_state_with_retry(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            pane.update(cx, |pane, _| {
                pane.session.keep_after_failure("连接已断开");
            });
            window.render_frame(cx);
            assert!(window.try_find("error-state").is_some(), "一次都没读到用整块错误态");
            assert!(window.try_find("empty-state").is_none(), "失败不是空态");
            assert_eq!(window.find("grid-retry").label(), Some("重试"));
        })
        .unwrap();
        // 读取成功才收掉失败态：清掉原因后错误态消失，回到空态。
        cx.update(|cx| pane.update(cx, |pane, _| pane.session.clear_read_failure()));
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("error-state").is_none(), "重试成功后错误态收掉");
            assert!(window.try_find("grid-retry").is_none());
        })
        .unwrap();
    }

    /// 读取失败但上次结果还在：正文保留，只有一条错误条，带重试；原因不在状态区重复。
    #[gpui_kit::test]
    fn a_failed_read_keeps_the_sheet_and_shows_one_error_entry(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant(&pane, cx);
            window.render_frame(cx);
        })
        .unwrap();
        let before = cx.update(|cx| {
            let pane = pane.read(cx);
            (pane.cell(0, 0), pane.cell(0, 1))
        });
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                pane.apply_page_result(0, true, true, Err("连接已断开".to_owned()), cx);
                // 失败只改会话和表格，绘制前要让面板自己也重画一次。
                cx.notify();
            });
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let pane = pane.read(cx);
            assert!(pane.session.loaded_len() > 0, "失败不丢弃上次结果");
            assert_eq!(pane.cell(0, 0), before.0, "第 0 行的值原样保留");
            assert_eq!(pane.cell(0, 1), before.1, "第 1 列的值原样保留");
            assert_eq!(window.find_all("grid-read-error").len(), 1, "错误条只出现一次");
            assert!(window.try_find("grid-read-error-detail").is_some(), "错误条写出失败原因");
            assert_eq!(window.find("grid-retry").label(), Some("重试"));
            assert!(window.try_find("error-state").is_none(), "有旧结果时不用整块错误态");
            assert!(window.try_find("grid-body").is_some(), "旧结果继续画在内容区");
            assert!(window.try_find("notice").is_none());
        })
        .unwrap();
        // 重试成功后错误条收掉：读取成功会清掉失败原因。
        cx.update(|cx| {
            pane.update(cx, |pane, _| pane.session.clear_read_failure());
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find_all("grid-read-error").is_empty(), "成功读取后错误条收掉");
            assert!(window.try_find("grid-retry").is_none());
        })
        .unwrap();
    }

    /// 键盘翻页与 Tab：选择与滚动一起走，并且键盘操作才画焦点环。
    #[gpui_kit::test]
    fn page_and_tab_keys_move_the_selection_and_paint_the_focus_ring(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            pane.update(cx, |pane, cx| {
                let rows = (0..40).map(|_| vec![Some("1"), Some("值")]).collect::<Vec<_>>();
                pane.plant_writable(target(), vec![("id", "integer"), ("name", "text")], rows, "id", cx);
                pane.session.select(0, 0, false);
            });
            let handle = pane.read(cx).focus_handle();
            window.focus(&handle, cx);
            window.render_frame(cx);
        })
        .unwrap();
        // 翻页步长就是屏幕上真实放得下的行数：表格量完可见范围后，下一帧把行数发布给会话。
        cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
        let step = cx.update(|cx| pane.read(cx).session.viewport_rows());
        assert!(step > 1 && step < 40, "可见行数应在 1..40 之间：{step}");
        cx.simulate_keystrokes(window, "pagedown");
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let pane = pane.read(cx);
            let (start, end) = pane.session.visible_bounds();
            let focus = pane.session.focus_cell().expect("有焦点");
            assert_eq!(focus, (step, 0), "PageDown 走一屏");
            assert!(focus.0 >= start && focus.0 < end, "选中的格子始终在可见窗口里：{start}..{end}");
            assert_eq!(pane.table.read(cx).delegate().focus_ring, Some((step, 0)), "键盘移动后画焦点环");
            assert!(window.last_input_was_keyboard(), "键盘刚动过才画环");
        })
        .unwrap();
        cx.simulate_keystrokes(window, "tab");
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let view = pane.read(cx);
            let grid_focused = view.focus_handle().is_focused(window);
            let table_focused = view.table.read(cx).focus_handle(cx).is_focused(window);
            assert_eq!(view.session.focus_cell(), Some((step, 1)), "Tab 走下一格（网格焦点 {grid_focused}，表格焦点 {table_focused}）");
            assert_eq!(view.table.read(cx).delegate().focus_ring, Some((step, 1)), "焦点环跟到新格子");
        })
        .unwrap();
        cx.simulate_keystrokes(window, "pageup");
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(pane.read(cx).session.focus_cell(), Some((0, 1)), "PageUp 回到第一屏");
        })
        .unwrap();
        // 末格处 Tab 不再拦：选择不动，焦点可以离开网格。
        cx.update(|cx| pane.update(cx, |pane, _| pane.session.select(39, 1, false)));
        cx.simulate_keystrokes(window, "tab");
        cx.update(|cx| assert_eq!(pane.read(cx).session.focus_cell(), Some((39, 1)), "末格处选择不再移动"));
    }

    /// 行高、表头高与格子内边距只从密度 token 来；选中底色从色板来，两套主题各一份。
    /// 三档宽度（1360/900/640）下即使列宽总和远大于窗口，网格正文也不会撑出窗口。
    #[gpui_kit::test]
    fn grid_geometry_comes_from_tokens_and_stays_inside_three_widths(cx: &mut TestAppContext) {
        // token：紧凑 26 / 标准 32 / 宽松 40（规范 S7.2 / S9），内边距是分栏内边距的一半。
        for (metrics, expected) in [(kit::Metrics::compact(), 26.), (kit::Metrics::standard(), 32.), (kit::Metrics::relaxed(), 40.)] {
            assert_eq!(row_height(metrics), px(expected), "行高跟密度走");
            let pad = cell_padding(metrics);
            assert_eq!((pad.top, pad.bottom), (px(0.), px(0.)), "上下由行高与居中决定");
            assert_eq!(pad.left, pad.right, "左右内边距对称");
            assert_eq!(pad.left, px(metrics.pane_pad() / 2.), "内边距按密度算，不是裸数字");
        }
        assert_ne!(kit::Palette::light().selection, kit::Palette::dark().selection, "两套主题的选中底色不同");
        assert_ne!(kit::Palette::light().chrome, kit::Palette::dark().chrome, "表头底色也随主题变");
        // 三档宽度：六列各 260px 远超窗口，正文仍然等于窗口宽度（横向滚动留在正文里）。
        for width in [640., 900., 1360.] {
            let (window, pane) = open_sized(cx, width);
            let mut visual = VisualTestContext::from_window(window, cx);
            visual.update(|window, cx| {
                pane.update(cx, |pane, cx| {
                    let columns = (0..6).map(|index| (format!("c{index}"), "text")).collect::<Vec<_>>();
                    let named = columns.iter().map(|(name, kind)| (name.as_str(), *kind)).collect::<Vec<_>>();
                    let rows = (0..8).map(|_| vec![Some("值"), Some("值"), Some("值"), Some("值"), Some("值"), Some("值")]).collect::<Vec<_>>();
                    pane.plant_writable(target(), named, rows, "c0", cx);
                    for column in 0..6 {
                        pane.session.set_column_width(column, 260.);
                    }
                });
                window.render_frame(cx);
            });
            let body = visual.debug_bounds("grid-body").expect("网格正文");
            assert!(
                body.size.width <= px(width) + px(1.),
                "{width} 宽：正文 {} 不能超过窗口",
                body.size.width
            );
        }
    }

    /// 换密度立刻反映到网格：格子内边距跟着 Metrics 发布，选中底色跟着当前色板。
    #[gpui_kit::test]
    fn density_and_theme_reach_the_rendered_grid(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        for (metrics, expected) in [(kit::Metrics::compact(), 5.), (kit::Metrics::standard(), 6.), (kit::Metrics::relaxed(), 8.)] {
            cx.update(|cx| {
                metrics.publish(cx);
            });
            cx.update_window(window, |_, window, cx| {
                plant(&pane, cx);
                window.render_frame(cx);
                let view = pane.read(cx);
                let cell_pad = view.table.read(cx).delegate().cell_pad;
                assert_eq!(cell_pad.left, px(expected), "格子内边距随密度发布");
                assert_eq!(cell_pad, cell_padding(metrics), "和 token 算出的一致");
            })
            .unwrap();
        }
        // 主题：暗色下网格用的是同一份色板，选中色来自 Palette 而不是写死的颜色。
        cx.update(|cx| kit::apply_theme(true, cx));
        cx.update(|cx| {
            let p = Palette::of(cx);
            assert_eq!(p.selection, kit::Palette::dark().selection, "暗色主题的选中底色");
            assert_eq!(p.chrome, kit::Palette::dark().chrome, "暗色主题的表头底色");
        });
        cx.update(|cx| kit::apply_theme(false, cx));
        cx.update(|cx| assert_eq!(Palette::of(cx).selection, kit::Palette::light().selection, "浅色主题的选中底色"));
    }

    /// 表格操作在三档宽度 × 两套主题 × 两档密度下都走得通：单击、Ctrl/Cmd 加选、
    /// 复制含表头、记录面板就地暂存、双击分隔柄自适应。这是 headless 布局级证据，
    /// 不是原生像素截图（本任务没有原生窗口与截屏通道的授权）。
    #[gpui_kit::test]
    fn grid_flow_runs_at_three_widths_in_both_themes(cx: &mut TestAppContext) {
        for width in [1360., 900., 640.] {
            for dark in [false, true] {
                for metrics in [kit::Metrics::compact(), kit::Metrics::standard()] {
                    let (window, pane) = open_sized(cx, width);
                    let mut visual = VisualTestContext::from_window(window, cx);
                    cx.update(|cx| {
                        metrics.publish(cx);
                        kit::apply_theme(dark, cx);
                    });
                    visual.update(|window, cx| {
                        plant(&pane, cx);
                        pane.update(cx, |pane, cx| {
                            pane.cell_press(0, 0, PressMode::Plain, cx);
                            pane.cell_press(1, 2, PressMode::Toggle, cx);
                            let _ = pane.command("grid-record-toggle", cx);
                        });
                        window.render_frame(cx);
                    });
                    visual.update(|window, cx| {
                        let view = pane.read(cx);
                        assert_eq!(view.session.selection_count(), 2, "{width}/{dark}/{metrics:?}");
                        assert!(view.session.has_extra_regions(), "{width}/{dark}/{metrics:?}");
                        assert!(view.session.copy_text().starts_with("id\t"), "多格复制带表头：{width}/{dark}");
                        assert!(!view.session.tsv().is_empty());
                        // 单元格内边距跟着当前密度走，主题只换颜色不动几何。
                        assert_eq!(view.table.read(cx).delegate().cell_pad, cell_padding(metrics));
                        window.render_frame(cx);
                    });
                    assert!(visual.debug_bounds("grid-body").is_some(), "{width} 宽时正文在");
                    assert!(visual.debug_bounds("grid-record").is_some(), "{width} 宽时记录面板在");
                    assert!(visual.debug_bounds("grid-record-value-1").is_some(), "记录字段在");
                    // 双击列分隔柄的自适应在三档宽度里都能量出宽度并写回会话。
                    let table = cx.update(|cx| pane.read(cx).table.clone());
                    let fitted = cx.update_window(window, |_, window, cx| table.update(cx, |state, cx| state.fit_col(2, window, cx)));
                    assert!(fitted.unwrap(), "{width} 宽时双击分隔柄应量出一份宽度");
                    cx.run_until_parked();
                }
            }
        }
    }
    /// 长文本按列宽截断，但完整值仍在：悬停拿到整段原文，大值/二进制指向查看面板。
    /// NULL、空字符串和真的存着 "NULL" 的文本各画各的；暂存修改的格子带自己的标记。
    #[gpui_kit::test]
    fn long_values_truncate_with_the_value_reachable_and_null_differs_from_empty(cx: &mut TestAppContext) {
        let long = "长".repeat(200);
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            pane.update(cx, |pane, cx| {
                pane.plant_writable(
                    target(),
                    vec![("id", "integer"), ("name", "text")],
                    vec![
                        vec![Some("1"), None],
                        vec![Some("2"), Some("")],
                        vec![Some("3"), Some("NULL")],
                        vec![Some("4"), Some("")],
                    ],
                    "id",
                    cx,
                );
                // 第四行放一段长文本（用暂存写入，避免再走一次读取）。
                let _ = pane.session.stage_text(3, 1, Some(long.clone()));
            });
            window.render_frame(cx);
            let view = pane.read(cx);
            let table = view.table.read(cx);
            let delegate = table.delegate();
            // 表格第 0 列是行号列，数据列从 1 起：第 1 列数据 = 表格列 2。
            assert_eq!(delegate.cell_kind_at(0, 2, cx), session::CellKind::Null, "第 0 行是 NULL");
            assert_eq!(delegate.cell_kind_at(1, 2, cx), session::CellKind::Empty, "第 1 行是空字符串");
            assert_eq!(delegate.cell_kind_at(2, 2, cx), session::CellKind::Text, "第 2 行是文本 NULL");
            assert_eq!(delegate.cell_label(2, 2, cx).as_deref(), Some("NULL"), "文本 NULL 原样显示");
            assert_ne!(delegate.cell_label(0, 2, cx), delegate.cell_label(1, 2, cx), "NULL 与空字符串显示不同");
            assert_eq!(delegate.cell_at(0, 2, cx).map(|(_, mark)| mark), Some(0), "没有暂存的格子不带标记");
            assert_eq!(delegate.cell_at(3, 2, cx).map(|(_, mark)| mark), Some(2), "暂存修改的格子带自己的标记");
            // 长文本整段仍在会话里：悬停提示给整段，格子本身只是视觉截断。
            let shown = delegate.cell_label(3, 2, cx).expect("长文本");
            assert_eq!(shown.len(), long.len());
            assert_eq!(
                cell_tooltip(Locale::ZhCn, session::CellKind::Text, &shown).as_deref(),
                Some(long.as_str()),
                "悬停提示拿得到完整值"
            );
            assert_eq!(
                cell_tooltip(Locale::ZhCn, session::CellKind::Large, "大值 2048").as_deref(),
                Some(i18n::text(Locale::ZhCn, "grid.value.openFullHint").as_str()),
                "大值指向查看面板"
            );
            assert!(cell_tooltip(Locale::ZhCn, session::CellKind::Null, "NULL").is_none(), "NULL 不再额外提示");
        })
        .unwrap();
    }
}
