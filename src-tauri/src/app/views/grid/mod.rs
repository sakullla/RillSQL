//! 共享虚拟网格。DataTable 只画可见窗口；选择和已读取/全量范围留在 session。

mod session;

use std::sync::{Arc, OnceLock};

pub use session::{operation_banner, prove_single_table, GridMode, GridTarget, SharedGrid};

use gpui_kit::component::table::{Column, DataTable, TableDelegate, TableEvent, TableState};
use gpui_kit::{
    App, AppContext as _, ClipboardItem, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};

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
                if self.session.preview_open() {
                    self.confirm(cx);
                } else {
                    let _ = self.preview(cx);
                }
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
                let _ = cx.update(|cx| {
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
                let _ = cx.update(|cx| {
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
        let Ok(proof) = prove_single_table(&self.session.target.engine, &self.session.sql_text()) else {
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
                let _ = cx.update(|cx| {
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
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let start = self.table.read(cx).delegate().visible_start;
        if self.table.read(cx).delegate().visible_seen {
            self.session.scroll_to(start);
        }
        let locale = self.locale;
        let range = SharedString::from(self.session.range_text());
        let banner = SharedString::from(self.session.banner());
        let status = SharedString::from(if self.session.writable() {
            i18n::text(locale, "grid.status.writable")
        } else {
            i18n::text(locale, "grid.status.readonly")
        });
        let table = self.table.clone();
        let preview = self.session.preview_open();
        let sql = self.session.preview_sql().join("\n");
        let entity = cx.entity();
        let search = SharedString::from(if self.search_text.is_empty() { i18n::text(locale, "grid.action.search") } else { self.search_text.clone() });
        let mut column = div()
            .id("shared-grid")
            .flex()
            .flex_col()
            .size_full()
            .gap(px(4.))
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
            .child(div().id("grid-range").child(range).child(" ").child(status))
            .child(div().id("grid-banner").child(banner))
            .child(
                div()
                    .id("grid-tools")
                    .flex()
                    .gap(px(4.))
                    .child(
                        div()
                            .id("grid-search")
                            .role(gpui_kit::Role::TextInput)
                            .aria_label(search.clone())
                            .child(search)
                            .on_key_down({
                                let entity = entity.clone();
                                move |event: &gpui_kit::KeyDownEvent, _, cx| {
                                    let Some(text) = grid_typed(event) else { return };
                                    entity.update(cx, |pane, cx| {
                                        pane.type_search(&text);
                                        cx.notify();
                                    });
                                }
                            }),
                    )
                    .child(tool_button("grid-search", i18n::text(locale, "grid.action.search"), entity.clone()))
                    .child(tool_button("grid-sort", i18n::text(locale, "grid.action.sort"), entity.clone()))
                    .child(tool_button("grid-filter", i18n::text(locale, "grid.action.filter"), entity.clone()))
                    .child(tool_button("grid-page-prev", i18n::text(locale, "grid.action.pagePrev"), entity.clone()))
                    .child(tool_button("grid-page-next", i18n::text(locale, "grid.action.pageNext"), entity.clone()))
                    .child(tool_button("copy-region", i18n::text(locale, "grid.value.copyValue"), entity.clone()))
                    .child(tool_button("grid-insert", i18n::text(locale, "grid.action.insert"), entity.clone()))
                    .child(tool_button("mark-delete", i18n::text(locale, "grid.menu.markDelete"), entity.clone()))
                    .child(tool_button("grid-preview", i18n::text(locale, "grid.action.preview"), entity.clone())),
            )
            .child(DataTable::new(&table).accessibility_label("shared-grid"));
        if self.editing {
            let buffer = SharedString::from(self.edit_buffer.clone());
            column = column.child(
                div()
                    .id("grid-edit")
                    .role(gpui_kit::Role::TextInput)
                    .aria_label(SharedString::from(i18n::text(locale, "shell.shortcuts.editCell")))
                    .child(buffer),
            );
        }
        if let Some(shown) = self.session.viewer() {
            column = column.child(
                div()
                    .id("grid-value")
                    .role(gpui_kit::Role::Dialog)
                    .aria_label(SharedString::from(i18n::text(locale, "grid.value.copyValue")))
                    .child(SharedString::from(format!("{:?} {}", shown.kind, if shown.body.is_empty() { shown.label } else { shown.body }))),
            );
        }
        if preview {
            let entity = cx.entity();
            column = column.child(
                div()
                    .id("grid-preview")
                    .role(gpui_kit::Role::Dialog)
                    .aria_label(SharedString::from(i18n::text(locale, "grid.preview.title")))
                    .child(SharedString::from(self.session.banner()))
                    .child(SharedString::from(sql))
                    .child(grid_button("grid-preview-confirm", i18n::text(locale, "grid.preview.confirm"), entity.clone()))
                    .child(grid_button("grid-preview-cancel", i18n::text(locale, "grid.preview.cancel"), entity)),
            );
        }
        column
    }
}

fn tool_button(id: &'static str, label: String, entity: Entity<GridPane>) -> impl IntoElement {
    let text = SharedString::from(label);
    div()
        .id(id)
        .role(gpui_kit::Role::Button)
        .aria_label(text.clone())
        .child(text)
        .on_click(move |_, _, cx| {
            entity.update(cx, |pane, cx| {
                let _ = pane.command(id, cx);
            });
        })
}

fn grid_typed(event: &gpui_kit::KeyDownEvent) -> Option<String> {
    if event.keystroke.modifiers.control || event.keystroke.modifiers.platform || event.keystroke.modifiers.alt {
        return None;
    }
    match event.keystroke.key.as_str() {
        "backspace" => Some("backspace".into()),
        "escape" | "enter" | "tab" | "up" | "down" | "left" | "right" => None,
        _ => event.keystroke.key_char.clone().filter(|text| !text.is_empty()),
    }
}

impl GridPane {
    fn type_search(&mut self, text: &str) {
        if text == "backspace" {
            self.search_text.pop();
        } else {
            self.search_text.push_str(text);
        }
    }

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

fn grid_button(id: &'static str, label: String, entity: Entity<GridPane>) -> impl IntoElement {
    let text = SharedString::from(label);
    div()
        .id(id)
        .role(gpui_kit::Role::Button)
        .aria_label(text.clone())
        .child(text)
        .on_click(move |_, _, cx| {
            entity.update(cx, |pane, cx| {
                if id.ends_with("confirm") {
                    pane.confirm(cx);
                } else {
                    pane.cancel_preview(cx);
                }
            });
        })
}

struct SheetDelegate {
    columns: Vec<(String, f32, bool)>,
    rows: Vec<Vec<String>>,
    has_more: bool,
    loading: bool,
    visible_start: usize,
    visible_seen: bool,
    owner: Option<gpui_kit::WeakEntity<GridPane>>,
}

impl Default for SheetDelegate {
    fn default() -> Self {
        Self { columns: Vec::new(), rows: Vec::new(), has_more: false, loading: false, visible_start: 0, visible_seen: false, owner: None }
    }
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

    fn render_td(&mut self, row_ix: usize, col_ix: usize, _: &mut Window, _: &mut Context<TableState<Self>>) -> impl IntoElement {
        let text = self.rows.get(row_ix).and_then(|line| line.get(col_ix)).cloned().unwrap_or_default();
        div().child(SharedString::from(text))
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
            let _ = cx.update(|cx| {
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
}

fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("grid runtime"))
}
