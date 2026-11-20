//! 对象概览（ADR-2 / R3）的外壳接线：预览标签的打开/替换/固定、概览命令与绘制。
//!
//! 纯状态（过滤、排序、格式化）在 `app/views/overview`；这里把它挂到工作项与对象树上：
//! 单击连接/库/模式 → 打开或就地替换“预览标签”；双击、排序、过滤、打开对象等操作把它转为固定标签。

use gpui_kit::assets::IconName;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, px, relative, AnyElement, ClickEvent, Context, ElementId, FontWeight, InteractiveElement as _, IntoElement as _, MouseButton, MouseDownEvent,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window,
};

use crate::app::views::connections::LinkStatus;
use crate::app::views::overview::{columns, format_size, OverviewColumn, OverviewLoad, OverviewState, OverviewTarget, MISSING};
use crate::catalog::hierarchy::NavKind;
use crate::catalog::{StatRow, StatsLevel};
use crate::ui::kit::{self, Palette, TypeScale};

use super::conn_modal::{ConnectAction, ConnectIntent};
use super::model::{next_epoch, DraftField, WorkItemKind, WorkbenchModel};
use super::chrome::act;
use super::view::Workbench;

impl WorkbenchModel {
    /// 连接行的概览目标；Redis/MongoDB 等非关系型连接没有概览。
    pub fn overview_target_for_connection(&self, connection_id: &str) -> Option<OverviewTarget> {
        let (name, engine, env) = match self.connections.rows.iter().find(|row| row.saved.id == connection_id) {
            Some(row) => (row.saved.name.clone(), row.saved.engine, row.saved.env.clone()),
            None => {
                let root = self.nav.node(&crate::catalog::hierarchy::connection_node_id(connection_id))?;
                (root.connection_name.clone(), root.engine, String::new())
            }
        };
        engine.is_relational().then(|| OverviewTarget::new(connection_id, name, "", "", engine, env))
    }

    /// 树节点的概览目标：连接、库与模式节点有概览，其余（分组、对象、列）没有。
    pub fn overview_target_for_node(&self, node_id: &str) -> Option<OverviewTarget> {
        let node = self.nav.node(node_id).filter(|node| !node.stale && !node.connection_id.is_empty())?;
        if !node.engine.is_relational() {
            return None;
        }
        let environment = self.nav_environment(&node.connection_id);
        let (database, schema) = match node.kind {
            NavKind::Connection => return self.overview_target_for_connection(&node.connection_id),
            NavKind::Database => (node.database.clone(), String::new()),
            NavKind::Schema => (node.database.clone(), node.name.clone()),
            _ => return None,
        };
        Some(OverviewTarget::new(node.connection_id.clone(), node.connection_name.clone(), database, schema, node.engine, environment))
    }

    /// 单击打开概览预览：同一范围已有标签就切过去；否则复用（就地替换）当前预览标签，没有才新开一个。
    pub fn open_overview_preview(&mut self, target: OverviewTarget) {
        let spec = target.spec();
        self.overviews.entry(spec.clone()).or_insert_with(|| OverviewState::new(target.clone()));
        if let Some(id) = self.tabs.iter().find(|tab| tab.kind == WorkItemKind::Overview && tab.spec.as_deref() == Some(spec.as_str())).map(|tab| tab.id.clone()) {
            let _ = self.activate_item(&id);
            return;
        }
        let preview = self
            .preview_tab
            .clone()
            .filter(|id| self.tabs.iter().any(|tab| &tab.id == id && tab.kind == WorkItemKind::Overview));
        match preview {
            Some(id) => {
                let title = target.title();
                let mut replaced = None;
                if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
                    replaced = tab.spec.replace(spec.clone());
                    tab.title = title;
                    tab.epoch = next_epoch();
                    tab.connection_id = Some(target.connection_id.clone());
                    tab.connection_name = Some(target.connection_name.clone()).filter(|name| !name.is_empty());
                    tab.database = Some(target.database.clone()).filter(|database| !database.is_empty());
                    tab.engine = Some(target.engine.as_str().to_owned());
                }
                if let Some(old) = replaced.filter(|old| old != &spec) {
                    self.overviews.remove(&old);
                }
                let _ = self.activate_item(&id);
            }
            None => {
                self.open_item(WorkItemKind::Overview, spec);
                self.preview_tab = self.active_tab.clone().filter(|id| self.tabs.iter().any(|tab| &tab.id == id && tab.kind == WorkItemKind::Overview));
            }
        }
    }

    /// 把预览标签转为固定标签（双击、排序、过滤、打开对象等操作之后）。
    pub fn pin_overview(&mut self) {
        if self.preview_tab.is_some() && self.preview_tab == self.active_tab {
            self.preview_tab = None;
        }
    }

    /// 这个标签是不是预览标签（标题斜体）。
    pub fn is_preview_tab(&self, id: &str) -> bool {
        self.preview_tab.as_deref() == Some(id)
    }

    /// 对象树连接行的单击/双击：已连接与未连接都打开概览（未连接的按需连接后再读）；双击固定。
    pub fn overview_click_connection(&mut self, connection_id: &str, double: bool) -> bool {
        let Some(target) = self.overview_target_for_connection(connection_id) else { return false };
        self.open_overview_preview(target);
        if double {
            self.pin_overview();
        }
        true
    }

    /// 对象树库/模式行的单击/双击。
    pub fn overview_click_node(&mut self, node_id: &str, double: bool) -> bool {
        if !self.nav.node(node_id).is_some_and(|node| matches!(node.kind, NavKind::Connection | NavKind::Database | NavKind::Schema)) {
            return false;
        }
        let Some(target) = self.overview_target_for_node(node_id) else { return false };
        self.open_overview_preview(target);
        if double {
            self.pin_overview();
        }
        true
    }

    pub fn active_overview(&self) -> Option<&OverviewState> {
        let spec = self.active_overview_spec()?;
        self.overviews.get(&spec)
    }

    pub fn active_overview_mut(&mut self) -> Option<&mut OverviewState> {
        let spec = self.active_overview_spec()?;
        let target = OverviewTarget::parse(&spec)?;
        Some(self.overviews.entry(spec).or_insert_with(|| OverviewState::new(target)))
    }

    fn active_overview_spec(&self) -> Option<String> {
        self.active_tab().filter(|tab| tab.kind == WorkItemKind::Overview).and_then(|tab| tab.spec.clone())
    }

    pub(super) fn set_overview_filter(&mut self, value: String) {
        let Some(state) = self.active_overview_mut() else { return };
        if state.filter == value {
            return;
        }
        state.filter = value;
        self.pin_overview();
    }

    /// 关闭的概览标签不再保留读取结果；预览标签被关掉后，下一次单击新开一个。
    pub fn sync_overviews(&mut self) {
        let open: std::collections::BTreeSet<String> = self
            .tabs
            .iter()
            .filter(|tab| tab.kind == WorkItemKind::Overview)
            .filter_map(|tab| tab.spec.clone())
            .collect();
        self.overviews.retain(|spec, _| open.contains(spec));
        for spec in &open {
            if !self.overviews.contains_key(spec) {
                if let Some(target) = OverviewTarget::parse(spec) {
                    self.overviews.insert(spec.clone(), OverviewState::new(target));
                }
            }
        }
        if self.preview_tab.as_ref().is_some_and(|id| !self.tabs.iter().any(|tab| &tab.id == id && tab.kind == WorkItemKind::Overview)) {
            self.preview_tab = None;
        }
    }

    /// 活动概览这一帧要不要发起读取：已连接且待读时返回目标与代数；未连接时按需排队连接。
    pub fn overview_read_request(&mut self) -> Option<(OverviewTarget, u64)> {
        let spec = self.active_overview_spec()?;
        let connection_id = self.active_overview()?.target.connection_id.clone();
        let connected = self.connections.row_status(&connection_id) == LinkStatus::Connected;
        let state = self.active_overview_mut()?;
        if connected && state.connect_failed {
            // 按需连接失败后，用户经密码框或对象树连上了：不必再点“重试”，直接重读。
            state.reload();
        }
        if !matches!(state.load, OverviewLoad::Pending | OverviewLoad::Connecting) {
            return None;
        }
        let target = state.target.clone();
        let status = self.connections.row_status(&target.connection_id);
        let known = self.connections.rows.iter().any(|row| row.saved.id == target.connection_id);
        match status {
            LinkStatus::Connected => {
                let state = self.overviews.get_mut(&spec)?;
                let generation = state.begin_load();
                Some((target, generation))
            }
            LinkStatus::Connecting => {
                if let Some(state) = self.overviews.get_mut(&spec) {
                    state.load = OverviewLoad::Connecting;
                }
                None
            }
            LinkStatus::Disconnected | LinkStatus::Failed => {
                if !known {
                    return None;
                }
                let waiting = self.overviews.get(&spec).is_some_and(|state| state.load == OverviewLoad::Connecting);
                if status == LinkStatus::Failed && waiting {
                    // 连接失败：停在失败说明上，等用户重试，不反复自动重连。
                    let reason = self.connections.failures.get(&target.connection_id).cloned().unwrap_or_default();
                    if let Some(state) = self.overviews.get_mut(&spec) {
                        state.load = OverviewLoad::Failed(if reason.is_empty() { "catalog.overview.connectFailed".to_owned() } else { reason });
                        state.connect_failed = true;
                    }
                    return None;
                }
                if !waiting {
                    // 未连接的连接先按需连接，连上后再读；与对象树、查询页共用同一条连接流程。
                    self.queue_connect(ConnectIntent::new(target.connection_id.clone(), ConnectAction::ConnectOnly, None));
                    if let Some(state) = self.overviews.get_mut(&spec) {
                        state.load = OverviewLoad::Connecting;
                    }
                }
                None
            }
        }
    }

    /// 读取结果回填到对应范围（标签已关闭或期间刷新过的迟到结果丢弃）。
    pub fn finish_overview_read(&mut self, spec: &str, generation: u64, result: Result<crate::catalog::StatsBody, String>) -> bool {
        self.overviews.get_mut(spec).is_some_and(|state| state.finish_load(generation, result))
    }

    fn overview_row(&self, name: &str) -> Option<(OverviewTarget, StatRow)> {
        let state = self.active_overview()?;
        Some((state.target.clone(), state.row(name)?.clone()))
    }

    /// 概览行对应的树节点（已读进对象树的表/视图）：右键复用对象树的同一册菜单。
    fn overview_nav_node(&self, target: &OverviewTarget, row: &StatRow) -> Option<String> {
        let kind = row.kind?;
        let database = target.object_database();
        self.nav
            .object_by_key(&target.connection_id, &database, &target.schema, &row.name, kind)
            .filter(|node| !node.stale)
            .map(|node| node.id.clone())
    }

    pub(super) fn overview_command(&mut self, id: &str) -> bool {
        if let Some(node) = id.strip_prefix("overview-open-node:") {
            self.overview_click_node(node, false);
            return true;
        }
        if let Some(connection) = id.strip_prefix("overview-open-connection:") {
            self.overview_click_connection(connection, false);
            return true;
        }
        if id == "overview-pin" {
            self.pin_overview();
            return true;
        }
        if id == "overview-refresh" {
            if let Some(state) = self.active_overview_mut() {
                state.reload();
            }
            self.pin_overview();
            return true;
        }
        if id == "overview-connect" {
            if let Some(state) = self.active_overview_mut() {
                state.reload();
                let connection = state.target.connection_id.clone();
                self.queue_connect(ConnectIntent::new(connection, ConnectAction::ConnectOnly, None));
                if let Some(state) = self.active_overview_mut() {
                    state.load = OverviewLoad::Connecting;
                }
            }
            return true;
        }
        if let Some(column) = id.strip_prefix("overview-sort:") {
            let Some(column) = OverviewColumn::parse(column) else { return true };
            if let Some(state) = self.active_overview_mut() {
                state.toggle_sort(column);
            }
            self.pin_overview();
            return true;
        }
        if id == "overview-next" || id == "overview-prev" {
            if let Some(state) = self.active_overview_mut() {
                state.step_selection(if id == "overview-next" { 1 } else { -1 });
            }
            return true;
        }
        if let Some(name) = id.strip_prefix("overview-select:") {
            if let Some(state) = self.active_overview_mut() {
                if state.row(name).is_some() {
                    state.selected = Some(name.to_owned());
                }
            }
            return true;
        }
        if let Some(name) = id.strip_prefix("overview-activate:") {
            // 双击（或回车）：库/模式行进入下一层，表/视图打开数据。
            let Some((target, row)) = self.overview_row(name) else { return true };
            self.pin_overview();
            match target.child(&row) {
                Some(child) => self.open_overview_preview(child),
                None if OverviewState::opens_data(&row) => self.open_item(WorkItemKind::Table, target.object_spec(&row.name)),
                None => {}
            }
            return true;
        }
        if id == "overview-activate-selected" {
            let Some(name) = self.active_overview().and_then(|state| state.selected.clone()) else { return true };
            return self.overview_command(&format!("overview-activate:{name}"));
        }
        if let Some(name) = id.strip_prefix("overview-menu:") {
            let Some((target, row)) = self.overview_row(name) else { return true };
            if let Some(state) = self.active_overview_mut() {
                state.selected = Some(name.to_owned());
            }
            self.pin_overview();
            // 对象树里已经有这一行：用对象树同一册菜单（结构、DDL、模板、危险操作都在）。
            if let Some(node) = self.overview_nav_node(&target, &row) {
                return self.open_nav_menu(&node);
            }
            let items = super::menus::overview_menu_items(name, row.kind.is_some(), target.engine.is_relational());
            self.open_menu(super::menus::MenuPlace::ObjectTree, super::menus::MenuTarget::Object);
            self.menu = Some(super::menus::OpenMenu::with_items(super::menus::MenuPlace::ObjectTree, items));
            return true;
        }
        if let Some(name) = id.strip_prefix("overview-open-data:") {
            let Some((target, row)) = self.overview_row(name) else { return true };
            self.pin_overview();
            if OverviewState::opens_data(&row) {
                self.open_item(WorkItemKind::Table, target.object_spec(&row.name));
            }
            return true;
        }
        if let Some(name) = id.strip_prefix("overview-open-structure:") {
            let Some((target, row)) = self.overview_row(name) else { return true };
            self.pin_overview();
            if OverviewState::opens_data(&row) {
                self.open_item(WorkItemKind::Structure, target.object_spec(&row.name));
            }
            return true;
        }
        if let Some(name) = id.strip_prefix("overview-open-child:") {
            let Some((target, row)) = self.overview_row(name) else { return true };
            self.pin_overview();
            if let Some(child) = target.child(&row) {
                self.open_overview_preview(child);
            }
            return true;
        }
        if let Some(name) = id.strip_prefix("overview-new-query:") {
            let Some((target, row)) = self.overview_row(name) else { return true };
            self.pin_overview();
            let database = match target.level() {
                StatsLevel::Databases => row.name.clone(),
                StatsLevel::Schemas | StatsLevel::Objects => target.object_database(),
            };
            self.open_bound_query(
                String::new(),
                Some(target.connection_id.clone()),
                Some(target.connection_name.clone()),
                Some(database).filter(|database| !database.is_empty()),
                Some(target.engine.as_str().to_owned()),
            );
            return true;
        }
        if let Some(name) = id.strip_prefix("overview-copy-name:") {
            let Some((target, row)) = self.overview_row(name) else { return true };
            let text = if row.kind.is_some() {
                crate::adapters::qualified_object(target.engine, &target.object_database(), &target.schema, &row.name)
            } else {
                row.name.clone()
            };
            self.copied = text.clone();
            self.clipboard_out = Some(text);
            return true;
        }
        false
    }
}

impl Workbench {
    /// 概览页：标题与汇总、过滤框、可排序的列头和虚拟列表。
    pub(super) fn overview_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let m = self.metrics();
        let Some(state) = self.model.active_overview().cloned() else {
            return div().size_full().into_any_element();
        };
        let level = state.level();
        let target = state.target.clone();
        let visible: Vec<StatRow> = state.visible().into_iter().cloned().collect();
        let cols = columns(level);
        let total_width: f32 = cols.iter().map(|column| column.width()).sum();
        let heading_key = match level {
            StatsLevel::Databases => "catalog.overview.databases",
            StatsLevel::Schemas => "catalog.overview.schemas",
            StatsLevel::Objects => "catalog.overview.objects",
        };
        let path = [target.connection_name.as_str(), target.database.as_str(), target.schema.as_str()]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" / ");
        let count_text = if state.filter.trim().is_empty() {
            crate::i18n::fill(&self.model.text("catalog.overview.count"), &[("count", &state.rows.len().to_string())])
        } else {
            crate::i18n::fill(
                &self.model.text("catalog.overview.filtered"),
                &[("shown", &visible.len().to_string()), ("count", &state.rows.len().to_string())],
            )
        };
        let size_text = state.total_size().map(|size| format!(" · {}", format_size(Some(size)))).unwrap_or_default();
        // 统计值来自引擎缓存（如 MySQL 8 的 information_schema_stats_expiry、PG 的 ANALYZE 估算），如实提示可能滞后。
        let cached_text = if level == StatsLevel::Objects && state.missing.is_none() {
            format!(" · {}", self.model.text("catalog.overview.statsCached"))
        } else {
            String::new()
        };
        let filter = self.input_for(DraftField::OverviewFilter, &self.model.text("catalog.overview.filter"), false, window, cx);
        let header = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(10.))
            .px(px(m.pane_pad()))
            .py(px(8.))
            .border_b_1()
            .border_color(p.line_soft)
            .child(kit::engine_badge(target.engine.as_str(), 18.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w(px(0.))
                    .flex_1()
                    .child(
                        div()
                            .id("overview-title")
                            .test_support()
                            .text_size(px(TypeScale::MD))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.text)
                            .truncate()
                            .child(SharedString::from(path)),
                    )
                    .child(
                        div()
                            .id("overview-summary")
                            .test_support()
                            .text_size(px(TypeScale::XS))
                            .text_color(p.text_muted)
                            .child(SharedString::from(format!("{} · {count_text}{size_text}{cached_text}", self.model.text(heading_key)))),
                    ),
            )
            .child(div().w(px(220.)).flex_none().child(filter))
            .child(
                kit::icon_button("overview-refresh", IconName::RefreshCw, self.t("catalog.overview.refresh"), act("overview-refresh", cx))
                    .xsmall()
                    .tooltip(self.t("catalog.overview.refresh")),
            );
        let missing_banner = state.missing.as_ref().map(|reason| {
            let text = format!("{}：{}", self.model.text("catalog.overview.statsMissing"), self.model.text(reason));
            div()
                .id("overview-stats-missing")
                .test_support()
                .px(px(m.pane_pad()))
                .py(px(6.))
                .child(kit::banner(kit::BannerTone::Warn, IconName::TriangleAlert, SharedString::from(text), None, &p))
                .into_any_element()
        });
        let column_header = {
            let mut row = div().flex_none().flex().items_center().h(px(m.row_h + 2.)).px(px(m.pane_pad())).border_b_1().border_color(p.line_soft).bg(p.chrome);
            for column in cols {
                let column = *column;
                let active = state.sort == column;
                let arrow = if !active {
                    None
                } else if state.descending {
                    Some(IconName::ArrowDown)
                } else {
                    Some(IconName::ArrowUp)
                };
                let cell = kit::interactive_states(div().id(SharedString::from(format!("overview-sort-{}", column.id()))), &p, false)
                    .test_support()
                    .role(gpui_kit::Role::Button)
                    .aria_label(self.t(column.label_key()))
                    .flex_none()
                    .w(relative(column.width() / total_width))
                    .min_w(px(0.))
                    .h_full()
                    .px(px(6.))
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .when(column.numeric(), |this| this.justify_end())
                    .text_size(px(TypeScale::XS))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(if active { p.text } else { p.text_muted })
                    .child(div().truncate().child(self.t(column.label_key())))
                    .when_some(arrow, |this, arrow| this.child(kit::icon(arrow, 11., p.text_muted)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.perform(&format!("overview-sort:{}", column.id()), cx);
                    }));
                row = row.child(cell);
            }
            row
        };
        let body: AnyElement = match &state.load {
            OverviewLoad::Pending | OverviewLoad::Loading if state.rows.is_empty() => {
                div().flex_1().child(kit::loading_state(self.t("catalog.overview.loading"), &p)).into_any_element()
            }
            OverviewLoad::Connecting => div().flex_1().child(kit::loading_state(self.t("catalog.overview.connecting"), &p)).into_any_element(),
            OverviewLoad::Failed(reason) => {
                let retry = kit::secondary_button("overview-retry", Some(IconName::RefreshCw), self.t("common.retry"), act("overview-connect", cx));
                div()
                    .id("overview-error")
                    .test_support()
                    .flex_1()
                    .child(kit::error_state_with_action(self.t("catalog.overview.failed"), SharedString::from(self.model.text(reason)), retry, &p))
                    .into_any_element()
            }
            _ if state.rows.is_empty() => div().flex_1().child(kit::empty_state(IconName::Database, self.t("catalog.overview.empty"), "", &p)).into_any_element(),
            _ if visible.is_empty() => div()
                .flex_1()
                .child(kit::no_match_state(IconName::Search, self.t("catalog.overview.noMatch"), SharedString::from(state.filter.clone()), &p))
                .into_any_element(),
            _ => {
                let row_h = m.row_h + 4.;
                let owner = cx.entity().downgrade();
                let selected = state.selected.clone();
                let rows = std::rc::Rc::new(visible);
                let list_rows = std::rc::Rc::clone(&rows);
                // 每个概览目标各记一份滚动位置；句柄同时喂给可见滚动条（ADR-8）。
                let scroll_key = SharedString::from(format!("overview-list-scroll:{}:{}:{}", target.connection_id, target.database, target.schema));
                let scroll = window.use_keyed_state(ElementId::Name(scroll_key), cx, |_, _| gpui_kit::UniformListScrollHandle::new()).read(cx).clone();
                let list = gpui_kit::uniform_list("overview-list", rows.len(), move |range, _, cx| {
                    owner
                        .update(cx, |this, cx| {
                            range
                                .map(|index| this.overview_row_element(&list_rows[index], index, cols, total_width, row_h, selected.as_deref() == Some(list_rows[index].name.as_str()), cx))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default()
                })
                .track_scroll(&scroll)
                .size_full();
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .relative()
                    .child(list)
                    .child(kit::scrollbar_overlay("overview-list-bar", &scroll, kit::ScrollbarAxis::Vertical))
                    .into_any_element()
            }
        };
        div()
            .id("overview-page")
            .test_support()
            .size_full()
            .flex()
            .flex_col()
            .bg(p.surface)
            .child(header)
            .children(missing_banner)
            .child(column_header)
            .child(div().flex_1().min_h(px(0.)).flex().flex_col().child(body))
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn overview_row_element(&self, row: &StatRow, index: usize, cols: &'static [OverviewColumn], total_width: f32, row_h: f32, selected: bool, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let m = self.metrics();
        let name = row.name.clone();
        let click_name = name.clone();
        let menu_name = name.clone();
        let icon = match row.kind {
            Some(crate::catalog::model::ObjectKind::View) => IconName::Eye,
            Some(_) => IconName::Table,
            None => IconName::Database,
        };
        let mut element = kit::row_states(div().id(SharedString::from(format!("overview-row-{index}"))), &p)
            .test_support()
            .role(gpui_kit::Role::Row)
            .aria_label(SharedString::from(name.clone()))
            .aria_selected(selected)
            // 虚拟列表的行按内容宽度布局：不写满宽时各列按百分比算不出宽度，整行挤在左边、与列头对不齐。
            .w_full()
            .h(px(row_h))
            .px(px(m.pane_pad()))
            .flex()
            .items_center()
            .border_b_1()
            .border_color(p.line_soft)
            .when(selected, |this| this.bg(p.selection))
            .text_size(px(TypeScale::SM));
        for column in cols {
            let text = column.text(row);
            let faint = text == MISSING || (*column == OverviewColumn::Comment && text.is_empty());
            let mut cell = div()
                .flex_none()
                .w(relative(column.width() / total_width))
                .min_w(px(0.))
                .px(px(6.))
                .flex()
                .items_center()
                .gap(px(6.))
                .text_color(if faint { p.text_faint } else if *column == OverviewColumn::Name { p.text } else { p.text_muted })
                .when(column.numeric(), |this| this.justify_end().font_features(kit::tabular_figures()));
            if *column == OverviewColumn::Name {
                cell = cell.child(kit::icon(icon, 13., p.text_muted));
            }
            cell = cell.child(div().min_w(px(0.)).truncate().child(SharedString::from(text)));
            element = element.child(cell);
        }
        element
            .on_mouse_down(MouseButton::Right, cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                this.menu_at = event.position;
                this.perform(&format!("overview-menu:{menu_name}"), cx);
            }))
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                this.perform(&format!("overview-select:{click_name}"), cx);
                if event.click_count() >= 2 {
                    this.perform(&format!("overview-activate:{click_name}"), cx);
                }
                cx.notify();
            }))
            .into_any_element()
    }
}
