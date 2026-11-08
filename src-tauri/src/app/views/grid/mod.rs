//! 共享虚拟网格。DataTable 只画可见窗口；选择和已读取/全量范围留在 session。

mod session;

use std::sync::{Arc, OnceLock};

pub use session::{operation_banner, prove_single_table, GridMode, GridTarget, SharedGrid};

use gpui_kit::assets::IconName;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::table::{Column, DataTable, TableDelegate, TableEvent, TableState};
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::{
    App, AppContext as _, ClipboardItem, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px, relative,
};

use crate::ui::kit::{self, Palette};
use gpui_kit::prelude::FluentBuilder as _;

use crate::application::AppServices;
use crate::grid::commands::{CountRequest, FilterArg, PageRequest, TableRef};
use crate::i18n::{self, Locale};

use session::{columns_from_schema, rows_from_page, schema_from_dto, Gate};

pub struct GridPane {
    session: SharedGrid,
    services: Arc<AppServices>,
    table: Entity<TableState<SheetDelegate>>,
    locale: Locale,
    scheduled: u64,
    search_text: String,
    editing: bool,
    edit_buffer: String,
    loading_more: bool,
    sort_desc: bool,
    search: Option<Entity<InputState>>,
    search_subscription: Option<gpui_kit::Subscription>,
    _subscription: gpui_kit::Subscription,
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
        let subscription = cx.subscribe(&table, |this, _, event: &TableEvent, cx| {
            this.apply_table_event(event);
            cx.notify();
        });
        let owner = cx.entity().downgrade();
        table.update(cx, |state, _| state.delegate_mut().owner = Some(owner));
        Self {
            session: SharedGrid::new(),
            services,
            table,
            locale: Locale::ZhCn,
            scheduled: 0,
            search_text: String::new(),
            editing: false,
            edit_buffer: String::new(),
            loading_more: false,
            sort_desc: false,
            search: None,
            search_subscription: None,
            _subscription: subscription,
        }
    }

    pub fn set_locale(&mut self, locale: Locale) {
        self.locale = locale;
        self.session.locale = locale;
    }

    pub fn clear_sheet(&mut self, cx: &mut Context<Self>) {
        let locale = self.locale;
        self.session = SharedGrid::new();
        self.session.locale = locale;
        self.scheduled = 0;
        self.sync_table(cx);
    }

    pub fn uncommitted(&self) -> bool {
        self.session.uncommitted()
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

    pub fn show_query(&mut self, target: GridTarget, sql: &str, columns: Vec<String>, rows: Vec<Vec<Option<String>>>, cx: &mut Context<Self>) {
        self.session.target = target;
        self.session.locale = self.locale;
        let token = self.session.show_query_sheet(sql, columns, rows);
        self.sync_table(cx);
        self.schedule_locate(token, cx);
    }

    pub fn open_table(&mut self, target: GridTarget, limit: u64, cx: &mut Context<Self>) {
        self.session.mode = GridMode::Table;
        self.session.target = target;
        self.session.locale = self.locale;
        self.session.set_page(0, limit);
        self.fetch_current(true, cx);
    }

    pub fn command(&mut self, id: &str, cx: &mut Context<Self>) -> Option<String> {
        let copied = match id {
            "edit-cell" => {
                self.editing = true;
                self.edit_buffer = self.session.focus_cell().map(|(row, column)| self.session.cell_label(row, column)).unwrap_or_default();
                if !self.session.focus_cell().is_some() {
                    self.session.select(0, 0, false);
                    self.edit_buffer = self.session.focus_cell().map(|(row, column)| self.session.cell_label(row, column)).unwrap_or_default();
                }
                None
            }
            "cancel-edit" => {
                self.editing = false;
                self.edit_buffer.clear();
                None
            }
            "commit" => {
                if self.editing {
                    let value = if self.edit_buffer == "NULL" { None } else { Some(self.edit_buffer.clone()) };
                    let _ = self.edit_focus(value, cx);
                    self.editing = false;
                }
                // 工作台 commit 只打开预览。写库只来自预览对话框的 confirm。
                let _ = self.preview(cx);
                None
            }
            "mark-delete" => {
                if self.session.focus_cell().is_none() {
                    self.session.select(0, 0, false);
                }
                if let Some((row, _)) = self.session.focus_cell() {
                    let _ = self.session.stage_delete(row);
                }
                None
            }
            "copy-region" | "copy-sql-value" => {
                if !self.session.focus_cell().is_some() {
                    self.session.select_all_loaded();
                }
                Some(self.session.tsv())
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
            "grid-search" => {
                self.session.set_search(&self.search_text);
                None
            }
            "grid-sort" => {
                self.sort_desc = !self.sort_desc;
                if let Some(name) = self.session.column_names().first().cloned() {
                    self.session.set_sort(&name, self.sort_desc);
                }
                None
            }
            "grid-filter" => {
                if let Some(name) = self.session.column_names().get(1).cloned().or_else(|| self.session.column_names().first().cloned()) {
                    if self.search_text.is_empty() {
                        self.session.clear_filters();
                    } else {
                        self.session.set_filter(&name, "contains", &self.search_text);
                    }
                }
                None
            }
            "grid-page-next" => {
                let limit = self.session.page_limit().max(1);
                let offset = self.session.page_offset().saturating_add(limit);
                self.session.set_page(offset, limit);
                self.fetch_current(true, cx);
                None
            }
            "grid-page-prev" => {
                let limit = self.session.page_limit().max(1);
                let offset = self.session.page_offset().saturating_sub(limit);
                self.session.set_page(offset, limit);
                self.fetch_current(true, cx);
                None
            }
            "grid-insert" => {
                let _ = self.session.stage_insert();
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
        copied
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
                self.session.select(*row, col.saturating_sub(1), false);
            }
            TableEvent::DoubleClickedCell(row, col) => {
                let column = col.saturating_sub(1);
                self.session.select(*row, column, false);
                self.session.open_value(*row, column);
                self.editing = true;
                self.edit_buffer = self.session.cell_label(*row, column);
            }
            TableEvent::SelectRow(row) | TableEvent::DoubleClickedRow(row) => {
                self.session.select(*row, 0, false);
                if matches!(event, TableEvent::DoubleClickedRow(_)) {
                    self.session.open_value(*row, 0);
                }
            }
            TableEvent::ClearSelection => self.session.clear_selection(),
            _ => {}
        }
    }

    pub fn edit_focus(&mut self, value: Option<String>, cx: &mut Context<Self>) -> Gate {
        let gate = if let Some((row, column)) = self.session.focus_cell() {
            self.session.stage_text(row, column, value)
        } else {
            Gate::Readonly
        };
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
        self.fetch_at(self.session.page_offset(), replace, cx);
    }

    fn fetch_at(&mut self, offset: u64, replace: bool, cx: &mut Context<Self>) {
        self.session.token = self.session.token.saturating_add(1);
        let token = self.session.token;
        self.scheduled = token;
        let services = Arc::clone(&self.services);
        let mut request = self.session.page_request();
        request.offset = offset;
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
                                this.loading_more = false;
                                return;
                            }
                            if let (Ok(schema_dto), Ok(page)) = (described, paged) {
                                if let Ok(schema) = schema_from_dto(&schema_dto) {
                                    let columns = columns_from_schema(&schema, true);
                                    let full = counted.ok().map(|item| item.matched);
                                    let truncated = page.truncated;
                                    let rows = rows_from_page(&columns, &page);
                                    if replace {
                                        this.session.replace_fetched(columns, rows, offset, full, truncated, Some(schema));
                                    } else {
                                        this.session.append_page(rows, full, truncated);
                                    }
                                    this.loading_more = false;
                                    this.sync_table(cx);
                                }
                            } else {
                                this.loading_more = false;
                                this.sync_table(cx);
                            }
                        });
                    }
                });
            }
        })
        .detach();
    }

    fn load_next_page(&mut self, cx: &mut Context<Self>) {
        if self.loading_more || !self.session.has_more_rows() || self.session.mode != GridMode::Table || self.session.target.connection_id.is_empty() {
            return;
        }
        self.loading_more = true;
        let offset = self.session.page_offset() + self.session.source_len() as u64;
        self.fetch_at(offset, false, cx);
    }

    fn schedule_locate(&mut self, token: u64, cx: &mut Context<Self>) {
        if self.scheduled == token || self.session.target.connection_id.is_empty() {
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
        let has_more = self.session.has_more_rows();
        self.table.update(cx, |state, cx| {
            state.delegate_mut().columns = std::iter::once(("#".to_owned(), 56.0, true))
                .chain(names.into_iter().zip(widths).map(|(name, width)| (name, width, false)))
                .collect();
            state.delegate_mut().rows = rows;
            state.delegate_mut().has_more = has_more;
            state.delegate_mut().loading = false;
            state.refresh(cx);
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
        let start = self.table.read(cx).delegate().visible_start;
        if self.table.read(cx).delegate().visible_seen {
            self.session.scroll_to(start);
        }
        let p = Palette::of(cx);
        let locale = self.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let entity = cx.entity();
        let search = self.search_input(window, cx);
        let writable = self.session.writable();
        let dirty = self.session.uncommitted();
        let table_mode = self.session.mode == GridMode::Table;
        let has_columns = !self.session.column_names().is_empty();
        let range = SharedString::from(self.session.range_text());
        let banner = SharedString::from(self.session.banner());
        let table = self.table.clone();

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
            .child(div().w(px(220.)).child(kit::text_input("grid-search", &search, t("grid.action.search")).small().prefix(kit::icon(IconName::Search, 13., p.text_faint)).cleanable(true)))
            .child(
                kit::icon_button("grid-sort", if self.sort_desc { IconName::ArrowDownWideNarrow } else { IconName::ArrowUpNarrowWide }, t("grid.action.sort"), {
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
                .child(grid_icon("grid-page-next", IconName::ChevronRight, t("grid.action.pageNext"), entity.clone()));
        } else {
            tools = tools.child(div().px(px(6.)).text_size(px(12.)).text_color(p.text_muted).child(range.clone()));
        }
        tools = tools.child(div().flex_1()).child(kit::pill(
            if writable { t("grid.status.writable") } else { t("grid.status.readonly") },
            if writable { p.ok } else { p.text_muted },
            if writable { p.ok_soft } else { p.hover },
        ));
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

        let mut column = div()
            .id("shared-grid")
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(p.surface)
            .on_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, _, cx| {
                if this.editing {
                    this.type_edit(event, cx);
                    return;
                }
                let key = event.keystroke.key.as_str();
                let key = match key {
                    "arrowup" => "up",
                    "arrowdown" => "down",
                    "arrowleft" => "left",
                    "arrowright" => "right",
                    other => other,
                };
                if matches!(key, "up" | "down" | "left" | "right" | "home" | "end") {
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
                }
            }))
            .child(tools)
            .child(div().id("grid-range").hidden().child(range))
            .child(div().id("grid-banner").hidden().child(banner));
        if has_columns {
            column = column.child(div().flex_1().min_h(px(0.)).child(DataTable::new(&table).small().stripe(true).accessibility_label("shared-grid")));
        } else {
            column = column.child(div().flex_1().flex().items_center().justify_center().child(kit::empty_state(IconName::Table, t("ui.grid.empty"), SharedString::from(""), &p)));
        }
        if self.editing {
            let buffer = SharedString::from(if self.edit_buffer.is_empty() { " ".to_owned() } else { self.edit_buffer.clone() });
            column = column.child(
                div()
                    .id("grid-edit")
                    .role(gpui_kit::Role::TextInput)
                    .aria_label(t("shell.shortcuts.editCell"))
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
                    .child(
                        div()
                            .flex_1()
                            .px(px(8.))
                            .py(px(4.))
                            .rounded(px(6.))
                            .bg(p.surface)
                            .border_1()
                            .border_color(p.accent)
                            .font_family(cx.theme().mono_font_family.clone())
                            .child(buffer),
                    )
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

impl GridPane {
    fn type_edit(&mut self, event: &gpui_kit::KeyDownEvent, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "backspace" => {
                self.edit_buffer.pop();
            }
            "escape" => {
                self.editing = false;
                self.edit_buffer.clear();
            }
            "enter" => {
                let value = if self.edit_buffer == "NULL" { None } else { Some(self.edit_buffer.clone()) };
                if let Some((row, column)) = self.session.focus_cell() {
                    let _ = self.session.stage_text(row, column, value);
                }
                self.editing = false;
            }
            _ => {
                if let Some(text) = event.keystroke.key_char.clone().filter(|text| !text.is_empty()) {
                    self.edit_buffer.push_str(&text);
                }
            }
        }
        self.sync_table(cx);
    }
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
    has_more: bool,
    loading: bool,
    visible_start: usize,
    visible_seen: bool,
    owner: Option<gpui_kit::WeakEntity<GridPane>>,
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
        div()
            .truncate()
            .when(null, |this| this.text_color(p.text_faint).italic())
            .child(SharedString::from(text))
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
        let _ = self.session.stage_text(0, 1, Some("乙".into()));
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
