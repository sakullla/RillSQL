//! 共享虚拟网格。DataTable 只画可见窗口；选择和已读取/全量范围留在 session。

mod session;

use std::sync::{Arc, OnceLock};

pub use session::{operation_banner, prove_single_table, GridMode, GridTarget, QuerySheet, SharedGrid};

use gpui_kit::assets::IconName;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::table::{Column, DataTable, TableDelegate, TableEvent, TableState};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _};
use gpui_kit::{
    App, AppContext as _, ClipboardItem, Context, Entity, FocusHandle, Focusable as _, InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px,
    relative,
};

use crate::ui::kit::{self, Palette};
use gpui_kit::prelude::FluentBuilder as _;

use crate::application::AppServices;
use crate::grid::commands::{CountDto, CountRequest, FilterArg, PageDto, PageRequest, SchemaDto, TableRef};
use crate::i18n::{self, Locale};

use session::{columns_from_schema, rows_from_page, schema_from_dto, Continuation, FindScope, Gate};

pub struct GridPane {
    session: SharedGrid,
    services: Arc<AppServices>,
    table: Entity<TableState<SheetDelegate>>,
    locale: Locale,
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
    /// 左键按下的格子；拖拽期间用它作为起点，松开或下一次按下时重置。
    press: Option<(usize, usize)>,
    /// 首屏或替换读取尚未返回；交换面板据此禁止把旧快照当成当前表。
    fetching: bool,
    /// 正在请求的翻页偏移。偏移在结果读回来之前不改，失败重试仍是同一页。
    pending_page: Option<u64>,
    /// 筛选条每一行的取值输入（值、上界/集合）；结构变化时整体重建。
    filter_inputs: Vec<FilterRowInputs>,
    filter_subscriptions: Vec<gpui_kit::Subscription>,
    search: Option<Entity<InputState>>,
    /// 换表后清掉查找框里的旧文字，避免输入框和实际筛选条件不一致。
    clear_search: bool,
    search_subscription: Option<gpui_kit::Subscription>,
    _subscription: gpui_kit::Subscription,
    _edit_subscription: gpui_kit::Subscription,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct EditTarget {
    row_id: String,
    column: usize,
    original: Option<Option<String>>,
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

impl GridPane {
    pub fn new(services: Arc<AppServices>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let table = cx.new(|cx| {
            TableState::new(SheetDelegate::default(), window, cx)
                .cell_selectable(true)
                .row_selectable(true)
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
            scheduled: 0,
            search_text: String::new(),
            edit_input,
            edit_target: None,
            suppress_enter: false,
            pending_edit: None,
            notice: String::new(),
            grid_focus: cx.focus_handle().tab_stop(true),
            refocus: false,
            loading_more: false,
            press: None,
            fetching: false,
            pending_page: None,
            filter_inputs: Vec::new(),
            filter_subscriptions: Vec::new(),
            search: None,
            clear_search: false,
            search_subscription: None,
            _subscription: subscription,
            _edit_subscription: edit_subscription,
        }
    }

    pub fn set_locale(&mut self, locale: Locale) {
        self.locale = locale;
        self.session.locale = locale;
    }

    /// 大字段预览上限来自设置 preview_kb；下一次读取和完整值查看都用它。
    pub fn set_preview_kb(&mut self, kb: u64) {
        self.session.set_preview_bytes(usize::try_from(kb.max(1).saturating_mul(1024)).unwrap_or(usize::MAX));
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
        self.pending_page = None;
        self.session = SharedGrid::new();
        self.session.locale = locale;
        self.session.set_preview_bytes(preview_bytes);
        self.session.set_null_copy(&null_copy);
        self.scheduled = 0;
        self.edit_target = None;
        self.pending_edit = None;
        self.press = None;
        self.filter_input_reset();
        self.notice.clear();
        self.sync_table(cx);
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

    #[allow(clippy::type_complexity)]
    pub fn exchange_sheet(&self) -> (Option<TableRef>, Vec<String>, Vec<Vec<Option<String>>>, bool) {
        self.session.exchange_sheet()
    }

    /// 查询结果进网格。截断与完整行数由查询页给出：网格据此区分"已到末尾"与
    /// "这个结果不能由网格继续"，不会把被截断的结果说成已读完。
    pub fn show_query(&mut self, target: GridTarget, sheet: QuerySheet, cx: &mut Context<Self>) {
        self.session.target = target;
        self.session.locale = self.locale;
        // 新的结果集意味着旧的编辑目标不再存在，先结束编辑。
        self.edit_target = None;
        self.pending_edit = None;
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
        self.press = None;
        self.pending_page = None;
        self.search_text.clear();
        self.clear_search = true;
        self.fetch_current(true, cx);
    }

    pub fn command(&mut self, id: &str, cx: &mut Context<Self>) -> Option<String> {
        let copied = match id {
            "edit-cell" => {
                self.select_first_if_needed();
                self.request_edit(false);
                None
            }
            "cancel-edit" => {
                self.cancel_edit();
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
                Some(self.session.tsv())
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
        copied
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

    /// 条件、排序或查找范围改变后：表数据从第一页重读，查询结果只在本地重排。
    fn reload_after_condition_change(&mut self, cx: &mut Context<Self>) {
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
    /// 查询结果和没有目标表时不能切到整表筛选：给出原因，而不是让关键字静默失效。
    pub fn set_find_scope(&mut self, scope: FindScope, cx: &mut Context<Self>) -> bool {
        if self.session.find_scope() == scope {
            return true;
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
            return;
        }
        self.filter_input_reset();
        let locale = self.locale;
        let mut inputs = Vec::new();
        let mut subscriptions = Vec::new();
        for (index, draft) in drafts.iter().enumerate() {
            let placeholder = SharedString::from(i18n::text(locale, "grid.filter.valuePlaceholder"));
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

    /// 左键按下格子。普通按下把选择收拢到这一格；Ctrl/Cmd 或 Shift 按下时保留既有锚点，
    /// 于是单击落在矩形另一角时得到“已有选择 + 该格”的矩形，而不是替换选择。
    pub(crate) fn cell_press(&mut self, row: usize, column: usize, extend: bool, cx: &mut Context<Self>) {
        self.press = Some((row, column));
        if extend && self.session.focus_cell() == Some((row, column)) {
            return;
        }
        self.session.select(row, column, extend);
        // 上一次禁写/校验提示属于旧格子，移动目标后不再显示。
        self.notice.clear();
        self.sync_table(cx);
        cx.notify();
    }

    /// 按住左键拖过另一格：从按下时的选择锚点扩展到当前格。没有按下时不改变选择，
    /// 松开后再移动指针也不会扩展。
    pub(crate) fn cell_drag(&mut self, row: usize, column: usize, cx: &mut Context<Self>) {
        if self.press.is_none() || self.session.focus_cell() == Some((row, column)) {
            return;
        }
        self.session.select(row, column, true);
        self.notice.clear();
        self.sync_table(cx);
        cx.notify();
    }

    /// 松开左键：结束拖拽。下一个鼠标移动不会继续改选择，直到重新按下。
    pub(crate) fn cell_release(&mut self) {
        self.press = None;
    }

    /// 开始编辑：可写的格子走正式文本输入，只读的格子改为查看值并说明原因。
    fn request_edit(&mut self, select_all: bool) {
        let Some((row, column)) = self.session.focus_cell() else {
            return;
        };
        let reason = self.session.write_reason(row, column);
        if !reason.is_empty() {
            self.notice = reason;
            self.session.open_value(row, column);
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
        let Some(row_id) = self.session.row_id(pending.row).map(str::to_owned) else {
            return;
        };
        // 切换到另一格前先把正在输入的内容暂存，避免双击别处时悄悄丢掉已输入文本。
        if let Some(previous) = self.edit_target.take() {
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
        self.edit_target = Some(EditTarget { row_id, column: pending.column, original });
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
            "column-copy" => {
                let row = self.session.focus_cell().map(|(row, _)| row).unwrap_or(0);
                self.session.select(row, column, false);
                Some(self.session.copy_column())
            }
            _ => None,
        };
        if let Some(text) = copied {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
        self.sync_table(cx);
        cx.notify();
    }

    fn apply_table_event(&mut self, event: &TableEvent) {
        match event {
            TableEvent::ColumnWidthsChanged(widths) => {
                for (index, width) in widths.iter().enumerate() {
                    if index == 0 {
                        continue;
                    }
                    self.session.set_column_width(index - 1, f32::from(*width));
                }
            }
            TableEvent::SelectCell(row, col) | TableEvent::RightClickedCell(row, col) => {
                let column = col.saturating_sub(1);
                // sync_table 会把会话焦点写回表格，表格随后又发回同一条事件；已经一致时不再改写选择。
                if self.session.focus_cell() != Some((*row, column)) {
                    self.session.select(*row, column, false);
                    // 上一次禁写/校验提示属于旧格子，移动目标后不再显示。
                    self.notice.clear();
                }
            }
            TableEvent::DoubleClickedCell(row, col) => {
                // 双击与 F2 都进入同一条编辑路径；只读时退化为查看值并说明原因。
                let column = col.saturating_sub(1);
                self.session.select(*row, column, false);
                self.request_edit(true);
            }
            TableEvent::SelectRow(row) | TableEvent::DoubleClickedRow(row) => {
                if self.session.focus_cell() != Some((*row, 0)) {
                    self.session.select(*row, 0, false);
                }
                if matches!(event, TableEvent::DoubleClickedRow(_)) {
                    self.session.open_value(*row, 0);
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
        // 分页追加不阻塞交换来源：已读取的窗口仍然完整；替换读取开始后不能被追加清掉。
        if replace {
            self.fetching = true;
        }
        self.session.token = self.session.token.saturating_add(1);
        let token = self.session.token;
        self.scheduled = token;
        let services = Arc::clone(&self.services);
        let view = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            runtime().spawn(async move {
                let described = crate::application::grid::describe(&services.connections, &request.table).await;
                let counted = crate::application::grid::count(&services.connections, &count_of(&request)).await;
                let paged = crate::application::grid::page(&services.connections, &request).await;
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
                            let read = described.and_then(|schema| paged.map(|page| (schema, page, counted)));
                            this.apply_page_result(offset, replace, landed_page, read, cx);
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
    ) {
        let stale = self.session.stale_target();
        let (schema_dto, page, counted) = match read {
            Ok(read) => read,
            Err(failure) => {
                // 读取失败：旧页、条件、选择、滚动、暂存和偏移都不动。
                self.keep_sheet_after_failure(failure, stale);
                self.sync_table(cx);
                return;
            }
        };
        let Ok(schema) = schema_from_dto(&schema_dto) else {
            self.keep_sheet_after_failure(i18n::text(self.locale, "grid.state.schemaUnknown"), stale);
            self.sync_table(cx);
            return;
        };
        let columns = columns_from_schema(&schema, true);
        let full = counted.ok().map(|item| item.matched);
        let truncated = page.truncated;
        let rows = rows_from_page(&columns, &page);
        if replace {
            self.session.replace_fetched(columns, rows, offset, full, truncated, Some(schema));
            // 新的一页真的到了才换窗口；失败路径不动选择、滚动和偏移。
            if landed_page {
                self.session.reset_page_view();
            }
        } else {
            self.session.append_page(rows, full, truncated);
        }
        self.notice.clear();
        self.loading_more = false;
        self.fetching = false;
        self.sync_table(cx);
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
        let offset = self.session.page_offset() + self.session.source_len() as u64;
        self.fetch_at(offset, false, cx);
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
            include_foreign_keys: false,
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
        let rows = self.session.sheet_labels();
        let loading = self.loading_more || self.fetching;
        let has_more = self.session.continuation(loading) == Continuation::More;
        let locale = self.locale;
        let writable = self.session.writable();
        let dirty = self.session.uncommitted();
        // 排序列用数据列下标传给表头；列顺序变化后重新按列名定位。
        let sort = self.session.sort_index();
        // 表格自己的高亮也要跟着稳定身份走：重排后按会话焦点重新指到同一行。
        // 事件是异步送达的，靠 apply_table_event 的相等判断避免回声改写选择。
        let focus = self.session.focus_cell().map(|(row, column)| (row, column + 1));
        self.table.update(cx, |state, cx| {
            state.delegate_mut().locale = locale;
            state.delegate_mut().writable = writable;
            state.delegate_mut().dirty = dirty;
            state.delegate_mut().columns = std::iter::once(("#".to_owned(), 56.0, true))
                .chain(names.into_iter().zip(widths).map(|(name, width)| (name, width, false)))
                .collect();
            state.delegate_mut().rows = rows;
            state.delegate_mut().sort = sort;
            state.delegate_mut().has_more = has_more;
            state.delegate_mut().loading = loading;
            state.refresh(cx);
            match focus {
                Some((row, column)) if state.selected_cell() != Some((row, column)) => state.set_selected_cell(row, column, cx),
                None if state.selected_cell().is_some() => state.clear_selection(cx),
                _ => {}
            }
        });
    }
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
        if let Some(pending) = self.pending_edit.take() {
            self.begin_edit(pending, window, cx);
        } else if self.edit_target.is_none() && std::mem::take(&mut self.refocus) {
            window.focus(&self.grid_focus, cx);
        }
        let start = self.table.read(cx).delegate().visible_start;
        if self.table.read(cx).delegate().visible_seen {
            self.session.scroll_to(start);
        }
        let p = Palette::of(cx);
        let locale = self.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let entity = cx.entity();
        let search = self.search_input(window, cx);
        if std::mem::take(&mut self.clear_search) {
            search.update(cx, |state, cx| state.set_value("", window, cx));
        }
        let writable = self.session.writable();
        let dirty = self.session.uncommitted();
        let table_mode = self.session.mode == GridMode::Table;
        let has_columns = !self.session.column_names().is_empty();
        let range = SharedString::from(self.session.range_text());
        let banner = SharedString::from(self.session.banner());
        let table = self.table.clone();
        let loading = self.loading_more || self.fetching;
        // 无匹配、表为空、加载中、还有更多、已到末尾各自有说法，不用同一个空列表。
        let state_text = SharedString::from(self.session.state_text(loading));
        let find_scope = self.session.find_scope();
        // 排序状态不依赖焦点：表头有方向标记，状态栏也写出列名和方向。
        let sort_text = self.session.sort_text();
        let unapplied = self.session.unapplied_filters();
        // 查询结果和没有目标表时，整表筛选的开关不该看起来可以用。
        let scope_block = self.session.find_scope_block();
        let sort_dir = self.focused_column_name().and_then(|name| self.session.sort_for(&name));
        let sort_icon = if sort_dir == Some(true) { IconName::ArrowDownWideNarrow } else { IconName::ArrowUpNarrowWide };
        let sort_label = match sort_dir {
            Some(true) => t("grid.action.sortDesc"),
            Some(false) => t("grid.action.sortAsc"),
            None => t("grid.action.sort"),
        };

        let mut tools = div()
            .id("grid-tools")
            .flex_none()
            .h(px(40.))
            .px(px(10.))
            .flex()
            .items_center()
            .gap(px(4.))
            .border_b_1()
            .border_color(p.line)
            .child(div().w(px(200.)).child(kit::text_input("grid-search", &search, t("grid.action.search")).small().prefix(kit::icon(IconName::Search, 13., p.text_faint)).cleanable(true)))
            // 查找范围是一等状态：已读取行（本地）与整表（服务器筛选）不能混为一谈。
            .child({
                let button = kit::tool_button(
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
                );
                // 不可用时按钮禁用，原因写在旁边；不能让开关看起来生效却什么都不发。
                button.disabled(scope_block.is_some())
            })
            .when_some(scope_block, |this, reason| {
                this.child(
                    div()
                        .id("grid-find-scope-reason")
                        .px(px(4.))
                        .max_w(px(300.))
                        .truncate()
                        .text_size(px(12.))
                        .text_color(p.text_muted)
                        .child(t(reason)),
                )
            })
            .child(kit::tool_button("grid-filter-add", Some(IconName::Plus), t("grid.filter.add"), {
                let entity = entity.clone();
                move |_, _, cx| {
                    entity.update(cx, |pane, cx| {
                        let _ = pane.command("grid-filter", cx);
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
        if table_mode {
            tools = tools
                .child(kit::vline(&p))
                .child(grid_icon("grid-page-prev", IconName::ChevronLeft, t("grid.action.pagePrev"), entity.clone()))
                .child(div().px(px(4.)).text_size(px(12.)).text_color(p.text_muted).child(range.clone()))
                .child(grid_icon("grid-page-next", IconName::ChevronRight, t("grid.action.pageNext"), entity.clone()))
                .child(div().px(px(6.)).max_w(px(260.)).truncate().text_size(px(12.)).text_color(p.text_muted).child(state_text.clone()));
        } else {
            tools = tools
                .child(div().px(px(6.)).text_size(px(12.)).text_color(p.text_muted).child(range.clone()))
                .child(div().px(px(6.)).max_w(px(260.)).truncate().text_size(px(12.)).text_color(p.text_muted).child(state_text.clone()));
        }
        // 当前排序列和方向写进状态栏：焦点移到别处后仍然看得出结果是按哪一列排的。
        if let Some(sorted) = sort_text {
            tools = tools.child(kit::pill(SharedString::from(sorted), p.accent_text, p.accent_soft));
        }
        tools = tools.child(div().flex_1()).child(kit::pill(
            if writable { t("grid.status.writable") } else { t("grid.status.readonly") },
            if writable { p.ok } else { p.text_muted },
            if writable { p.ok_soft } else { p.hover },
        ));
        // 只读不是一句状态词：把具体原因放在旁边，用户知道为什么写不进去。
        let readonly_reason = self.session.readonly_reason().to_owned();
        if !writable && !readonly_reason.is_empty() && readonly_reason != "grid.status.readonly" {
            tools = tools.child(
                div().px(px(2.)).max_w(px(360.)).truncate().text_size(px(12.)).text_color(p.text_muted).child(t(&readonly_reason)),
            );
        }
        if !self.notice.is_empty() {
            tools = tools.child(
                div().px(px(6.)).max_w(px(360.)).truncate().text_size(px(12.)).text_color(p.text_muted).child(SharedString::from(self.notice.clone())),
            );
        }
        tools = tools.child(grid_icon("copy-region", IconName::Copy, t("grid.value.copyValue"), entity.clone()));
        if writable {
            tools = tools
                .child(grid_icon("grid-insert", IconName::Plus, t("grid.action.insert"), entity.clone()))
                .child(grid_icon("mark-delete", IconName::Trash, t("grid.menu.markDelete"), entity.clone()));
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
            tools = tools.child(if dirty {
                kit::primary_button("grid-preview", Some(IconName::Eye), t("grid.action.preview"), preview).into_any_element()
            } else {
                kit::tool_button("grid-preview", Some(IconName::Eye), t("grid.action.preview"), preview).into_any_element()
            });
        }

        // 筛选条：每条条件可辨认、可换条件、可改值、可单删；非法原因和全清也在这里。
        let drafts = self.session.filters().to_vec();
        let filter_error = self.session.filter_error().map(str::to_owned);
        let mut filters_strip: Option<gpui_kit::AnyElement> = None;
        if !drafts.is_empty() || filter_error.is_some() {
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
                .child(kit::eyebrow(t("grid.filter.title"), &p));
            for (index, draft) in drafts.iter().enumerate() {
                let op = SharedString::from(self.session.op_label(&draft.op));
                let cycle = {
                    let entity = entity.clone();
                    move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                        entity.update(cx, |pane, cx| {
                            pane.cycle_filter_op(index, cx);
                        });
                    }
                };
                let remove = {
                    let entity = entity.clone();
                    move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                        entity.update(cx, |pane, cx| pane.remove_filter(index, cx));
                    }
                };
                let mut row = div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .px(px(6.))
                    .py(px(2.))
                    .rounded(px(6.))
                    .border_1()
                    // 正在编辑、尚未应用的条件用强调边框标出来，避免把旧结果当成新条件的结果。
                    .border_color(if draft.dirty { p.accent } else { p.line })
                    .bg(p.raised)
                    .child(div().text_size(px(12.)).text_color(p.text_muted).child(SharedString::from(draft.column.clone())))
                    .child(kit::tool_button(SharedString::from(format!("grid-filter-op-{index}")), None, op, cycle));
                if draft.dirty {
                    row = row.child(
                        div()
                            .debug_selector(|| "grid-filter-unapplied".to_owned())
                            .child(kit::pill(t("grid.filter.unapplied"), p.accent_text, p.accent_soft)),
                    );
                }
                let needs_value = !matches!(draft.op.as_str(), "null" | "notnull" | "empty");
                if let Some(inputs) = self.filter_inputs.get(index) {
                    if needs_value {
                        if let Some(state) = inputs.value.as_ref() {
                            row = row.child(
                                div()
                                    .w(px(120.))
                                    .child(kit::text_input(SharedString::from(format!("grid-filter-value-{index}")), state, t("grid.filter.value")).small()),
                            );
                        }
                    }
                    if draft.op == "between" {
                        if let Some(state) = inputs.second.as_ref() {
                            row = row.child(
                                div()
                                    .w(px(120.))
                                    .child(kit::text_input(SharedString::from(format!("grid-filter-upper-{index}")), state, t("grid.filter.upper")).small()),
                            );
                        }
                    }
                }
                row = row.child(kit::icon_button(SharedString::from(format!("grid-filter-remove-{index}")), IconName::X, t("grid.filter.remove"), remove));
                strip = strip.child(row);
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
                        let _ = pane.command("grid-filter", cx);
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
                strip = strip.child(div().max_w(px(360.)).truncate().text_size(px(12.)).text_color(p.danger).child(SharedString::from(error)));
            }
            filters_strip = Some(strip.into_any_element());
        }

        let mut column = div()
            .id("shared-grid")
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(p.surface)
            .track_focus(&self.grid_focus)
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
                if key == "escape" {
                    if this.session.preview_open() {
                        this.cancel_preview(cx);
                    } else if this.session.viewer().is_some() {
                        this.session.close_value();
                        cx.notify();
                    } else {
                        this.session.clear_selection();
                        this.sync_table(cx);
                        this.table.update(cx, |state, cx| state.clear_selection(cx));
                    }
                    return;
                }
                let table_focused = this.table.read(cx).focus_handle(cx).is_focused(window);
                if matches!(key, "up" | "down" | "left" | "right" | "home" | "end") {
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
                    if let Some((row, column)) = this.session.focus_cell() {
                        this.session.open_value(row, column);
                    }
                    cx.notify();
                } else if let Some(id) = grid_shortcut(key, event.keystroke.modifiers.secondary(cx), event.keystroke.modifiers.shift) {
                    cx.stop_propagation();
                    let _ = this.command(id, cx);
                    cx.notify();
                }
            }))
            .child(tools)
            .children(filters_strip)
            .child(div().id("grid-range").hidden().child(range))
            .child(div().id("grid-banner").hidden().child(banner));
        if has_columns {
            column = column.child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .on_mouse_down(gpui_kit::MouseButton::Left, cx.listener(|this, _, window, cx| {
                        // 点中格子时把键盘交给网格本身，之后的方向键、Enter 和 F2 才有效。
                        window.focus(&this.grid_focus, cx);
                    }))
                    .child(DataTable::new(&table).small().stripe(true).accessibility_label("shared-grid")),
            );
        } else if loading {
            // 首屏、换表与替换读取还没回来：不画"没有数据"的空表插图，加载优先于空。
            column = column.child(
                div()
                    .id("grid-loading")
                    .debug_selector(|| "grid-loading".to_owned())
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(12.))
                    .text_color(p.text_muted)
                    .child(t("grid.state.loading")),
            );
        } else {
            // 查询结果没有行是"结果集为空"，不是"表里没有行"。
            let heading = match self.session.mode {
                GridMode::Query => t("ui.grid.emptyResult"),
                GridMode::Table => t("ui.grid.empty"),
            };
            column = column.child(
                div()
                    .debug_selector(|| "grid-empty".to_owned())
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(kit::empty_state(IconName::Table, heading, SharedString::from(""), &p)),
            );
        }
        if self.edit_target.is_some() {
            let input = self.edit_input.clone();
            let set_null = {
                let entity = entity.clone();
                move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                    entity.update(cx, |pane, cx| {
                        let _ = pane.command("grid-set-null", cx);
                    });
                }
            };
            column = column.child(
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
                    .child(div().flex_1().child(kit::text_input("grid-edit-input", &input, t("shell.shortcuts.editCell")).small()))
                    .child(kit::tool_button("grid-set-null", None, t("grid.value.null"), set_null))
                    .child(kit::kbd("Enter", &p))
                    .child(kit::kbd("Esc", &p)),
            );
        }
        if let Some(shown) = self.session.viewer() {
            let body = if shown.body.is_empty() { shown.label } else { shown.body };
            column = column.child(
                div()
                    .id("grid-value")
                    .role(gpui_kit::Role::Dialog)
                    .aria_label(t("grid.value.copyValue"))
                    .flex_none()
                    .max_h(px(180.))
                    .overflow_y_scroll()
                    .px(px(14.))
                    .py(px(10.))
                    .border_t_1()
                    .border_color(p.line)
                    .bg(p.chrome)
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(kit::eyebrow(SharedString::from(format!("{:?}", shown.kind)), &p))
                    .child(div().font_family(cx.theme().mono_font_family.clone()).text_size(px(12.5)).child(SharedString::from(body))),
            );
            let reason = self.session.viewer_reason();
            if !reason.is_empty() {
                column = column.child(
                    div()
                        .id("grid-value-reason")
                        .flex_none()
                        .px(px(14.))
                        .pb(px(8.))
                        .bg(p.chrome)
                        .text_size(px(12.))
                        .text_color(p.text_muted)
                        .child(SharedString::from(reason)),
                );
            }
        }
        if self.session.preview_open() {
            let sql = self.session.preview_sql().join("\n");
            column = column.child(
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
                            .rounded(px(12.))
                            .shadow_lg()
                            .child(kit::title(t("grid.preview.title"), 15., &p))
                            .child(kit::muted(SharedString::from(self.session.banner()), &p))
                            .child(
                                div()
                                    .id("grid-preview-sql")
                                    .max_h(px(280.))
                                    .overflow_y_scroll()
                                    .p(px(12.))
                                    .rounded(px(8.))
                                    .bg(p.chrome)
                                    .border_1()
                                    .border_color(p.line)
                                    .font_family(cx.theme().mono_font_family.clone())
                                    .text_size(px(12.5))
                                    .child(SharedString::from(sql)),
                            )
                            .child(
                                div()
                                    .flex()
                                    .justify_end()
                                    .gap(px(8.))
                                    .child(grid_button("grid-preview-cancel", false, t("grid.preview.cancel"), entity.clone()))
                                    .child(grid_button("grid-preview-confirm", true, t("grid.preview.confirm"), entity)),
                            ),
                    ),
            );
        }
        column
    }
}

impl GridPane {
    fn search_input(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        if let Some(state) = &self.search {
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
    rows: Vec<Vec<String>>,
    /// 当前排序列（数据列下标）与方向。表头据此标出 ↑/↓，与焦点无关。
    sort: Option<(usize, bool)>,
    has_more: bool,
    loading: bool,
    visible_start: usize,
    visible_seen: bool,
    owner: Option<gpui_kit::WeakEntity<GridPane>>,
    locale: Locale,
    writable: bool,
    dirty: bool,
}

/// 表格获得焦点时的快捷键。与快捷键帮助里登记的组合一致。
fn grid_shortcut(key: &str, secondary: bool, shift: bool) -> Option<&'static str> {
    match (key, secondary, shift) {
        ("c", true, true) => Some("copy-sql-value"),
        ("c", true, false) => Some("copy-region"),
        ("a", true, false) => Some("select-all-cells"),
        ("z", true, false) => Some("grid-undo"),
        ("delete", false, _) => Some("mark-delete"),
        ("f2", false, _) => Some("edit-cell"),
        _ => None,
    }
}

/// 单元格右键菜单：(命令, 文案, 需要可写, 危险)。
const CELL_MENU: &[(&str, &str, bool, bool)] = &[
    ("copy-region", "grid.value.copyValue", false, false),
    ("copy-sql-value", "shell.shortcuts.copySqlValue", false, false),
    ("grid-copy-row", "grid.menu.copyRow", false, false),
    ("grid-copy-column", "grid.menu.copyColumn", false, false),
    ("", "", false, false),
    ("grid-filter-value", "grid.menu.filterByValue", false, false),
    ("select-all-cells", "grid.menu.selectAll", false, false),
    ("", "", false, false),
    ("edit-cell", "shell.shortcuts.editCell", true, false),
    ("grid-set-null", "grid.value.null", true, false),
    ("grid-insert", "grid.action.insert", true, false),
    ("mark-delete", "grid.menu.markDelete", true, true),
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
    ("column-copy-name", "grid.menu.copyColumnName"),
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

pub(crate) fn cell_menu(menu: PopupMenu, owner: Option<gpui_kit::WeakEntity<GridPane>>, locale: Locale, writable: bool, dirty: bool) -> PopupMenu {
    let mut menu = menu;
    for (id, key, needs_write, _danger) in CELL_MENU.iter().copied() {
        if id.is_empty() {
            menu = menu.separator();
            continue;
        }
        let owner = owner.clone();
        menu = menu.item(
            PopupMenuItem::new(menu_label(locale, key, id)).disabled(needs_write && !writable).on_click(move |_, _, cx| {
                if let Some(owner) = owner.as_ref().and_then(|owner| owner.upgrade()) {
                    owner.update(cx, |pane, cx| {
                        let _ = pane.command(id, cx);
                        cx.notify();
                    });
                }
            }),
        );
    }
    if dirty {
        let owner = owner.clone();
        menu = menu.separator().item(PopupMenuItem::new(menu_label(locale, "grid.menu.undo", "grid-undo")).on_click(move |_, _, cx| {
            if let Some(owner) = owner.as_ref().and_then(|owner| owner.upgrade()) {
                owner.update(cx, |pane, cx| {
                    let _ = pane.command("grid-undo", cx);
                    cx.notify();
                });
            }
        }));
    }
    menu
}

fn column_menu(menu: PopupMenu, owner: Option<gpui_kit::WeakEntity<GridPane>>, locale: Locale, column: usize, sorted: Option<bool>) -> PopupMenu {
    let mut menu = menu;
    for (id, key) in COLUMN_MENU.iter().copied() {
        if id.is_empty() {
            menu = menu.separator();
            continue;
        }
        let owner = owner.clone();
        // 当前排序方向在菜单里也标出来：菜单和表头不能给出两种说法。
        let current = matches!((id, sorted), ("column-sort-asc", Some(false)) | ("column-sort-desc", Some(true)));
        let label = if current { SharedString::from(format!("✓ {}", i18n::text(locale, key))) } else { menu_label(locale, key, id) };
        menu = menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
            if let Some(owner) = owner.as_ref().and_then(|owner| owner.upgrade()) {
                owner.update(cx, |pane, cx| pane.column_command(column, id, cx));
            }
        }));
    }
    menu
}


impl TableDelegate for SheetDelegate {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len().max(1)
    }

    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        let (name, width, fixed) = self.columns.get(col_ix).cloned().unwrap_or_else(|| (" ".to_owned(), 120.0, false));
        let mut column = Column::new(format!("c{col_ix}"), name).width(px(width)).resizable(!fixed);
        if fixed {
            column = column.fixed_left();
        }
        column
    }

    fn render_td(&mut self, row_ix: usize, col_ix: usize, _: &mut Window, cx: &mut Context<TableState<Self>>) -> impl IntoElement {
        let text = self.rows.get(row_ix).and_then(|line| line.get(col_ix)).cloned().unwrap_or_default();
        let null = text == "NULL" || text == i18n::text(Locale::ZhCn, "grid.value.null") || text == i18n::text(Locale::En, "grid.value.null");
        let p = Palette::of(cx);
        // 表格的 SelectCell 不带修饰键和拖拽阶段，直接在格子上取鼠标事件：该格里按下、
        // 拖到另一格、松开都带着真实行列，不需要按像素反推行号。
        let owner = self.owner.clone();
        let column = col_ix.saturating_sub(1);
        div()
            .id(gpui_kit::ElementId::named_usize(format!("grid-cell-{row_ix}"), col_ix))
            // 测试按住真实坐标点击/拖拽时用它找到格子；只在测试/debug+test-support 记录。
            .debug_selector(move || format!("grid-cell-{row_ix}-{col_ix}"))
            .size_full()
            .truncate()
            .when(null, |this| this.text_color(p.text_faint).italic())
            .child(SharedString::from(text))
            .when_some(owner, move |this, owner| {
                let drag = owner.clone();
                let release = owner.clone();
                this.on_mouse_down(MouseButton::Left, move |event: &MouseDownEvent, _, cx| {
                    // Ctrl/Cmd 单击加选；Shift 单击同样按扩展处理，与键盘 Shift 一致。
                    let extend = event.modifiers.secondary(cx) || event.modifiers.shift;
                    if let Some(pane) = owner.upgrade() {
                        pane.update(cx, |pane, cx| pane.cell_press(row_ix, column, extend, cx));
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
            })
    }

    fn render_th(&mut self, col_ix: usize, _: &mut Window, _: &mut Context<TableState<Self>>) -> impl IntoElement {
        let name = self.columns.get(col_ix).map(|column| column.0.clone()).unwrap_or_default();
        let header = div().id(("grid-th", col_ix)).size_full().flex().items_center().truncate().child(SharedString::from(name));
        if col_ix == 0 {
            return header.into_any_element();
        }
        // 表头自己带方向标记：焦点移到工具栏或别的格子后，仍然看得出按哪一列、哪个方向排的。
        let sorted = self.sort.filter(|(index, _)| *index + 1 == col_ix).map(|(_, descending)| descending);
        let header = match sorted {
            Some(descending) => header
                .child(div().flex_none().px(px(4.)).text_size(px(11.)).child(SharedString::from(if descending { "↓" } else { "↑" }))),
            None => header,
        };
        let owner = self.owner.clone();
        let locale = self.locale;
        header
            .context_menu(move |menu, _, _| column_menu(menu, owner.clone(), locale, col_ix - 1, sorted))
            .into_any_element()
    }

    fn context_menu(&mut self, _row_ix: usize, menu: PopupMenu, _: &mut Window, _: &mut Context<TableState<Self>>) -> PopupMenu {
        cell_menu(menu, self.owner.clone(), self.locale, self.writable, self.dirty)
    }

    fn cell_label(&self, row_ix: usize, col_ix: usize, _: &App) -> Option<SharedString> {
        self.rows.get(row_ix).and_then(|line| line.get(col_ix)).map(|text| SharedString::from(text.clone()))
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
    }
}


#[cfg(test)]
mod pane_tests {
    use super::session::CellKind;
    use super::*;
    use crate::grid::commands::{ColumnDto, RowDto};
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{AnyWindowHandle, Bounds, Modifiers, Point, TestAppContext, VisualTestContext, WindowBounds, WindowOptions, size};

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

    #[gpui_kit::test]
    fn modifier_click_extends_the_existing_selection_into_a_rectangle(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant(&pane, cx);
            window.render_frame(cx);
        })
        .unwrap();
        // 第一次普通单击建立锚点；表格补发的同格 SelectCell 在真实事件循环里随后送达。
        cx.update(|cx| pane.update(cx, |pane, cx| pane.cell_press(0, 0, false, cx)));
        settle(cx);
        // 两次 Ctrl/Cmd 单击分别向下、向右扩展，锚点始终留在 (0, 0)。
        cx.update(|cx| pane.update(cx, |pane, cx| pane.cell_press(1, 0, true, cx)));
        settle(cx);
        cx.update(|cx| pane.update(cx, |pane, cx| pane.cell_press(1, 1, true, cx)));
        settle(cx);
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert_eq!(pane.session.focus_cell(), Some((1, 1)), "加选只移动焦点，不动锚点");
            assert_eq!(pane.session.selection_count(), 4, "两次加选应形成 2x2 矩形");
            assert_eq!(pane.session.tsv(), "1\t甲\n2\t乙");
        });
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
        cx.update(|cx| pane.update(cx, |pane, cx| pane.cell_press(0, 0, false, cx)));
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
            assert_eq!(pane.read(cx).session.selection_count(), 4, "Ctrl/Cmd 单击应扩展为矩形");
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
        cx.update(|cx| pane.update(cx, |pane, cx| pane.cell_press(0, 0, false, cx)));
        settle(cx);
        cx.update(|cx| pane.update(cx, |pane, cx| pane.cell_drag(1, 1, cx)));
        settle(cx);
        // 普通单击另一格替换整个矩形，而不是继续扩展。
        cx.update(|cx| pane.update(cx, |pane, cx| pane.cell_press(1, 1, false, cx)));
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
                pane.set_preview_kb(4);
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
    fn find_scope_switch_is_visible_and_separate_from_local_find(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            plant(&pane, cx);
            window.render_frame(cx);
        })
        .unwrap();
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                assert_eq!(pane.session.find_scope(), FindScope::Loaded);
                assert!(pane.session.find_scope_block().is_none(), "有目标表的网格可以整表筛选");
                pane.session.set_search("乙");
                assert_eq!(pane.loaded_len(), 1, "已读取行查找在本地生效");
                // 切到整表筛选后不在本地重复筛，关键字随请求下发。
                assert!(pane.set_find_scope(FindScope::Server, cx));
                assert_eq!(pane.session.find_scope(), FindScope::Server);
                assert_eq!(pane.session.server_search().as_deref(), Some("乙"));
                assert_eq!(pane.loaded_len(), 2);
                assert!(pane.toggle_find_scope(cx));
                assert_eq!(pane.session.find_scope(), FindScope::Loaded);
                assert_eq!(pane.session.server_search(), None);
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
                assert_eq!(pane.session.state_text(false), i18n::text(Locale::ZhCn, "grid.state.complete"));
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
                assert_eq!(pane.session.state_text(false), i18n::text(Locale::ZhCn, "grid.state.noContinuation"));
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
