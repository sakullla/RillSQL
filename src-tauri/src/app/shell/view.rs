//! GPUI 外壳。菜单、工具栏、活动栏、侧栏、标签、状态栏、命令面板和六类菜单走同一个 dispatch。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use gpui_kit::{
    App, AppContext as _, Context, Entity, FocusHandle, Focusable, IntoElement, PathPromptOptions, Render, SharedString, Window,
    WindowAppearance,
};

use crate::app::views::admin::AdminPane;
use crate::app::views::compare::ComparePane;
use crate::app::views::connections::blank_draft;
use crate::app::views::diagram::DiagramPane;
use crate::app::views::export::ExchangePane;
use crate::app::views::grid::{GridPane, GridTarget};
use crate::app::views::mongo::MongoPane;
use crate::app::views::query::{QueryPane, SyncEffect, TabSnap};
use crate::app::views::redis::RedisPane;
use crate::app::views::structure::StructurePane;
use crate::app::{AppPage, Effect};
use crate::connections::model::Engine;
use crate::state::Platform;
use crate::application::AppServices;
use crate::ui::Foundation;

use super::jobs::{self, apply_done, job_request};
use super::menus::NavKey;
use super::model::{DraftField, PendingJob, WorkItemKind, WorkTool, WorkbenchModel};
use crate::app::Shell;

pub struct Workbench {
    pub model: WorkbenchModel,
    foundation: Entity<Foundation>,
    pub(super) query: Entity<QueryPane>,
    pub(super) table: Entity<GridPane>,
    pub(super) structure: Entity<StructurePane>,
    pub(super) admin: Entity<AdminPane>,
    pub(super) exchange: Entity<ExchangePane>,
    pub(super) compare: Entity<ComparePane>,
    pub(super) diagram: Entity<DiagramPane>,
    pub(super) redis: Entity<RedisPane>,
    pub(super) mongo: Entity<MongoPane>,
    applied_tool: Option<String>,
    /// 共享表格网格当前展示的表规格；暂存必须归属到这张表对应的工作项。
    table_view_spec: Option<String>,
    applied_diagram: Option<String>,
    applied_redis: Option<String>,
    applied_mongo: Option<String>,
    services: Arc<AppServices>,
    focus: FocusHandle,
    close_hooked: bool,
    booted: bool,
    boot: bool,
    sized: bool,
    last_state: String,
    revision: Arc<AtomicU64>,
    persist_gate: Arc<tokio::sync::Mutex<()>>,
    pub(super) fields: crate::ui::kit::Fields<DraftField>,
    pub(super) expanded: std::collections::BTreeSet<String>,
    pub(super) menu_at: gpui_kit::Point<gpui_kit::Pixels>,
    pub(super) palette_focused: bool,
    pub(super) pending_browser: Option<String>,
    connected_seen: std::collections::BTreeSet<String>,
}

impl Workbench {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let services = Arc::new(AppServices::from_state(&crate::connections::commands::ConnectionsState::new()));
        Self::with_services(window, cx, services, true)
    }

    pub fn with_services(window: &mut Window, cx: &mut Context<Self>, services: Arc<AppServices>, boot: bool) -> Self {
        let foundation = cx.new(|cx| Foundation::new(window, cx));
        let query = cx.new(|cx| QueryPane::new(Arc::clone(&services), window, cx));
        let table = cx.new(|cx| GridPane::new(Arc::clone(&services), window, cx));
        let structure = cx.new(|_| StructurePane::new(Arc::clone(&services)));
        let admin = cx.new(|_| AdminPane::new(Arc::clone(&services)));
        let exchange = cx.new(|_| ExchangePane::new(Arc::clone(&services)));
        let compare = cx.new(|_| ComparePane::new(Arc::clone(&services)));
        let diagram = cx.new(|_| DiagramPane::new(Arc::clone(&services)));
        let redis = cx.new(|_| RedisPane::new(Arc::clone(&services)));
        let mongo = cx.new(|_| MongoPane::new(Arc::clone(&services)));
        let mut model = WorkbenchModel::new(Shell::default());
        let _ = model.shell.set_prefers_dark(matches!(window.appearance(), WindowAppearance::Dark | WindowAppearance::VibrantDark));
        let shell = model.shell.clone();
        foundation.update(cx, |view, cx| {
            view.show_chrome = false;
            view.adopt(shell, Effect::ApplyTheme, cx);
        });
        Self {
            model,
            foundation,
            query,
            table,
            structure,
            admin,
            exchange,
            compare,
            diagram,
            redis,
            mongo,
            applied_tool: None,
            table_view_spec: None,
            applied_diagram: None,
            applied_redis: None,
            applied_mongo: None,
            services,
            focus: cx.focus_handle(),
            close_hooked: false,
            booted: false,
            boot,
            sized: false,
            last_state: String::new(),
            revision: Arc::new(AtomicU64::new(0)),
            persist_gate: Arc::new(tokio::sync::Mutex::new(())),
            fields: crate::ui::kit::Fields::default(),
            expanded: std::collections::BTreeSet::new(),
            menu_at: gpui_kit::Point::default(),
            palette_focused: false,
            pending_browser: None,
            connected_seen: std::collections::BTreeSet::new(),
        }
    }

    pub fn shell(&self) -> &Shell {
        &self.model.shell
    }

    pub fn perform(&mut self, id: &str, cx: &mut Context<Self>) {
        self.refresh_exchange(cx);
        if id == "quit-window" {
            if self.model.allow_system_close() {
                cx.quit();
            }
            cx.notify();
            return;
        }
        if id == "conn-import" || id == "conn-export" {
            if self.model.close_prompt.is_some() {
                let _ = self.model.dispatch(id);
                cx.notify();
                return;
            }
            if id == "conn-import" {
                self.prompt_import(cx);
            } else {
                self.prompt_export(cx);
            }
            return;
        }
        if id == "save-query" {
            if self.model.close_prompt.is_some() {
                let _ = self.model.dispatch(id);
                cx.notify();
                return;
            }
            let _ = self.model.dispatch(id);
            self.adopt_shell(Effect::None, cx);
            self.forward_query("save-sql-file", cx);
            self.persist_if_needed();
            cx.notify();
            return;
        }
        if id == "open-sqlite" {
            let Some(effect) = self.model.dispatch(id) else {
                return;
            };
            if matches!(effect, Effect::PromptOpen) {
                self.adopt_shell(Effect::None, cx);
                self.prompt_sqlite(cx);
            } else {
                self.apply_effect(effect, cx);
            }
            self.persist_if_needed();
            cx.notify();
            return;
        }
        if id == "open-exchange" || id.starts_with("exchange-") {
            if self.model.close_prompt.is_some() {
                let _ = self.model.dispatch(id);
                cx.notify();
                return;
            }
            let _ = self.model.dispatch(id);
            let locale = self.model.locale();
            let open = self.model.exchange_open;
            let source = self.exchange_source(cx);
            self.exchange.update(cx, |pane, cx| {
                pane.set_locale(locale);
                pane.open = open;
                pane.bind_source(source.0, source.1, source.2, source.3);
                pane.ensure_mapping(cx);
                if id != "open-exchange" {
                    pane.command(id, cx);
                } else {
                    cx.notify();
                }
            });
            self.refresh_exchange(cx);
            self.persist_if_needed();
            cx.notify();
            return;
        }
        if id == "compare" || id.starts_with("compare-") {
            if self.model.close_prompt.is_some() {
                let _ = self.model.dispatch(id);
                cx.notify();
                return;
            }
            let _ = self.model.dispatch(id);
            let locale = self.model.locale();
            self.compare.update(cx, |pane, cx| {
                pane.set_locale(locale);
                pane.command(id, cx);
            });
            self.refresh_compare(cx);
            self.persist_if_needed();
            cx.notify();
            return;
        }
        if id == "open-diagram" || id.starts_with("open-diagram:") || id.starts_with("diagram-") {
            if self.model.close_prompt.is_some() {
                let _ = self.model.dispatch(id);
                cx.notify();
                return;
            }
            let _ = self.model.dispatch(id);
            let locale = self.model.locale();
            if id.starts_with("diagram-") {
                self.diagram.update(cx, |pane, cx| {
                    pane.set_locale(locale);
                    pane.command(id, cx);
                });
            } else {
                self.applied_diagram = None;
                self.diagram.update(cx, |pane, _| pane.set_locale(locale));
                self.ensure_diagram(cx);
            }
            self.persist_if_needed();
            cx.notify();
            return;
        }
        if grid_surface_command(id) {
            if self.model.close_prompt.is_some() {
                let _ = self.model.dispatch(id);
                cx.notify();
                return;
            }
            let _ = self.model.dispatch(id);
            if let Some(text) = self.route_shared_grid(id, cx) {
                self.model.copied = text;
            }
            self.persist_if_needed();
            cx.notify();
            return;
        }
        let Some(effect) = self.model.dispatch(id) else {
            if id.starts_with("redis-") {
                self.redis.update(cx, |pane, cx| pane.command(id, cx));
                self.model.redis_running = self.redis.read(cx).running();
                cx.notify();
                return;
            }
            if id.starts_with("mongo-") {
                self.mongo.update(cx, |pane, cx| pane.command(id, cx));
                self.model.mongo_running = self.mongo.read(cx).running();
                cx.notify();
                return;
            }
            if query_command(id) {
                self.forward_query(id, cx);
                self.persist_if_needed();
                cx.notify();
            }
            return;
        };
        if matches!(effect, Effect::FocusEditor) {
            self.adopt_shell(Effect::None, cx);
            self.query.update(cx, |pane, _| pane.request_focus());
        } else if query_command(id) {
            self.adopt_shell(Effect::None, cx);
            self.forward_query(id, cx);
        } else {
            self.apply_effect(effect, cx);
        }
        self.persist_if_needed();
        cx.notify();
    }

    fn route_shared_grid(&mut self, id: &str, cx: &mut Context<Self>) -> Option<String> {
        let table = self.model.shell.page == crate::app::AppPage::Workspace && self.model.tool == WorkTool::Table;
        if table {
            self.table.update(cx, |pane, cx| pane.command(id, cx))
        } else if self.model.shell.page == crate::app::AppPage::Workspace {
            self.query.update(cx, |pane, cx| pane.grid_command(id, cx))
        } else {
            None
        }
    }

    #[cfg(test)]
    pub fn plant_object_tree(&mut self, cx: &mut Context<Self>) {
        self.query.update(cx, |pane, _| pane.plant_customers());
    }

    #[cfg(test)]
    pub fn table_preview(&self, cx: &App) -> (bool, String) {
        let pane = self.table.read(cx);
        (pane.preview_open(), pane.preview_text())
    }

    #[cfg(test)]
    pub fn table_cell(&self, row: usize, column: usize, cx: &App) -> String {
        self.table.read(cx).cell(row, column)
    }

    #[cfg(test)]
    pub fn table_uncommitted(&self, cx: &App) -> bool {
        self.table.read(cx).uncommitted()
    }

    fn query_snaps(&self) -> Vec<TabSnap> {
        self.model
            .tabs
            .iter()
            .filter(|tab| tab.kind.is_query())
            .map(|tab| TabSnap {
                id: tab.id.clone(),
                draft: tab.draft.clone(),
                connection_id: tab.connection_id.clone(),
                connection_name: tab.connection_name.clone(),
                database: tab.database.clone(),
                engine: tab.engine.clone(),
            })
            .collect()
    }

    fn forward_query(&mut self, id: &str, cx: &mut Context<Self>) {
        let snaps = self.query_snaps();
        let active = self.model.active_tab.clone();
        let locale = self.model.locale();
        let timeout = self.model.settings.query_timeout_sec;
        let effect = self.query.update(cx, |pane, cx| {
            pane.set_query_timeout_sec(timeout);
            let _ = pane.prepare(&snaps, active.as_deref(), locale, cx);
            pane.command(id, cx);
            pane.prepare(&snaps, active.as_deref(), locale, cx)
        });
        self.apply_query_effect(effect);
    }

    fn sync_query(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let snaps = self.query_snaps();
        let active = self.model.active_tab.clone();
        let locale = self.model.locale();
        let timeout = self.model.settings.query_timeout_sec;
        let environments = self
            .model
            .connections
            .rows
            .iter()
            .map(|row| (row.saved.id.clone(), row.saved.env.clone()))
            .collect::<Vec<_>>();
        let effect = self.query.update(cx, |pane, cx| {
            pane.remember_environments(environments);
            pane.set_query_timeout_sec(timeout);
            let effect = pane.prepare(&snaps, active.as_deref(), locale, cx);
            pane.ensure_editors(window, cx);
            effect
        });
        self.apply_query_effect(effect);
    }

    fn apply_query_effect(&mut self, effect: SyncEffect) {
        for (id, draft) in effect.drafts {
            if let Some(tab) = self.model.tabs.iter_mut().find(|tab| tab.id == id) {
                tab.draft = draft;
            }
        }
        for open in effect.opens {
            self.model.open_bound_query(open.draft, open.connection_id, open.connection_name, open.database, open.engine);
        }
        for target in effect.targets {
            self.model.set_query_target(&target.id, target.connection_id, target.connection_name, target.database, target.engine);
        }
        if let Some(spec) = effect.selected_object {
            self.model.object_spec = Some(spec);
        }
        for tab in &mut self.model.tabs {
            if tab.kind.is_query() {
                tab.running = effect.running.iter().any(|id| id == &tab.id);
            }
        }
        for (id, sql) in effect.executed {
            if let Some(tab) = self.model.tabs.iter_mut().find(|tab| tab.id == id) {
                tab.last_executed_sql = Some(sql);
            }
        }
    }

    fn sync_grid_flags(&mut self, cx: &mut Context<Self>) {
        // 表格项全部关闭后，共享网格里的暂存与旧数据都不再有归属；清掉它，
        // 避免窗口关闭被幽灵暂存阻塞，也避免重开同一张表时沿用陈旧快照。
        let has_table_item = self.model.tabs.iter().any(|tab| tab.kind == WorkItemKind::Table);
        if !has_table_item && self.table_view_spec.is_some() {
            self.table.update(cx, |pane, cx| pane.clear_sheet(cx));
            self.applied_tool = None;
            self.table_view_spec = None;
        }
        let table_dirty = self.table.read(cx).uncommitted();
        let query_dirty = self.query.read(cx).grid_uncommitted(cx);
        self.model.grid_dirty = table_dirty || query_dirty;
        self.model.grid_writable = self.table.read(cx).writable() || self.query.read(cx).grid_writable(cx);
        let active = self.model.active_tab.clone();
        let table_spec = self.table_view_spec.clone();
        for tab in &mut self.model.tabs {
            match tab.kind {
                WorkItemKind::Table => {
                    tab.uncommitted_cells = table_dirty
                        && table_spec
                            .as_deref()
                            .is_some_and(|spec| tab.spec.as_deref() == Some(spec));
                }
                WorkItemKind::Query => {
                    if query_dirty {
                        if active.as_deref() == Some(tab.id.as_str()) {
                            tab.uncommitted_cells = true;
                        }
                    } else {
                        tab.uncommitted_cells = false;
                    }
                }
                _ => {}
            }
        }
    }

    fn ensure_tool(&mut self, cx: &mut Context<Self>) {
        let spec = self.model.table_spec.clone().unwrap_or_default();
        let key = format!("{:?}:{spec}", self.model.tool);
        if self.applied_tool.as_deref() == Some(key.as_str()) {
            return;
        }
        self.applied_tool = Some(key);
        let Some(target) = parse_target(&spec) else {
            return;
        };
        let locale = self.model.locale();
        match self.model.tool {
            WorkTool::Table => {
                // 同一张表在结构/查询间来回切换时不要重新拉取：重开会清空尚未提交的暂存。
                if self.table_view_spec.as_deref() != Some(spec.as_str()) {
                    self.table_view_spec = Some(spec.clone());
                    self.table.update(cx, |pane, cx| {
                        pane.set_locale(locale);
                        pane.open_table(target, 100, cx);
                    });
                }
            }
            WorkTool::Structure => self.structure.update(cx, |pane, _| {
                pane.set_locale(locale);
                pane.bind(target);
            }),
            WorkTool::Admin => self.admin.update(cx, |pane, _| {
                pane.set_locale(locale);
                pane.bind(target);
            }),
            WorkTool::Query | WorkTool::Redis | WorkTool::Mongo => {}
        }
    }

    fn ensure_redis(&mut self, cx: &mut Context<Self>) {
        if self.model.tool != WorkTool::Redis {
            return;
        }
        let locale = self.model.locale();
        self.redis.update(cx, |pane, _| pane.set_locale(locale));
        let spec = self.model.redis_spec.clone().unwrap_or_default();
        if self.applied_redis.as_deref() == Some(spec.as_str()) {
            return;
        }
        self.applied_redis = Some(spec.clone());
        self.redis.update(cx, |pane, cx| pane.open_spec(&spec, cx));
    }

    fn ensure_mongo(&mut self, cx: &mut Context<Self>) {
        if self.model.tool != WorkTool::Mongo {
            return;
        }
        let locale = self.model.locale();
        self.mongo.update(cx, |pane, _| pane.set_locale(locale));
        let spec = self.model.mongo_spec.clone().unwrap_or_default();
        if self.applied_mongo.as_deref() == Some(spec.as_str()) {
            return;
        }
        self.applied_mongo = Some(spec.clone());
        self.mongo.update(cx, |pane, cx| pane.open_spec(&spec, cx));
    }

    #[cfg(test)]
    pub fn structure_banner(&self, cx: &App) -> String {
        self.structure.read(cx).session.banner()
    }

    #[cfg(test)]
    pub fn plant_exchange_sheet(&mut self, spec: &str, columns: Vec<String>, rows: Vec<Vec<Option<String>>>, cx: &mut Context<Self>) {
        let target = parse_target(spec).expect("exchange table");
        self.table.update(cx, |pane, cx| pane.plant_loaded(target, columns, rows, cx));
    }

    #[cfg(test)]
    pub fn exchange_text(&self, cx: &App) -> String {
        self.exchange.read(cx).lines().join("\n")
    }

    #[cfg(test)]
    pub fn exchange_running_now(&self, cx: &App) -> bool {
        self.exchange.read(cx).running()
    }

    #[cfg(test)]
    pub fn exchange_tasks(&self, cx: &App) -> Vec<crate::export::TaskView> {
        self.exchange.read(cx).tasks().to_vec()
    }

    #[cfg(test)]
    pub fn sync_exchange_running(&mut self, cx: &mut Context<Self>) {
        self.refresh_exchange(cx);
    }

    #[cfg(test)]
    pub fn compare_text(&self, cx: &App) -> String {
        self.compare.read(cx).lines().join("\n")
    }

    #[cfg(test)]
    pub fn compare_running_now(&self, cx: &App) -> bool {
        self.compare.read(cx).running()
    }

    #[cfg(test)]
    pub fn compare_structure_ready(&self, cx: &App) -> bool {
        self.compare.read(cx).structure_ready()
    }

    #[cfg(test)]
    pub fn compare_data_ready(&self, cx: &App) -> bool {
        self.compare.read(cx).data_ready()
    }

    #[cfg(test)]
    pub fn arm_compare_pause(&mut self, cx: &mut Context<Self>) {
        self.compare.update(cx, |pane, _| pane.arm_pause());
    }

    #[cfg(test)]
    pub fn release_compare_pause(&mut self, cx: &mut Context<Self>) {
        self.compare.update(cx, |pane, _| pane.release_pause());
    }

    #[cfg(test)]
    pub fn diagram_text(&self, cx: &App) -> String {
        self.diagram.read(cx).lines().join("\n")
    }

    #[cfg(test)]
    pub fn diagram_column(&self, name: &str, cx: &App) -> Option<usize> {
        self.diagram.read(cx).column_of(name)
    }

    #[cfg(test)]
    pub fn admin_banner(&self, cx: &App) -> String {
        self.admin.read(cx).session.banner()
    }

    #[cfg(test)]
    pub fn table_banner(&self, cx: &App) -> String {
        self.table.read(cx).banner()
    }

    #[cfg(test)]
    pub fn mark_table_dirty(&mut self, cx: &mut Context<Self>) {
        self.table.update(cx, |pane, _| pane.stage_sample_edit());
    }

    #[cfg(test)]
    pub fn query_cell(&self, row: usize, col: usize, cx: &App) -> String {
        self.query.read(cx).cell(row, col, cx)
    }

    #[cfg(test)]
    pub fn query_note(&self, cx: &App) -> String {
        self.query.read(cx).note()
    }

    #[cfg(test)]
    pub fn query_editor_text(&self, cx: &App) -> String {
        self.query.read(cx).editor_text(cx)
    }

    #[cfg(test)]
    pub fn query_editor_focused(&self, window: &Window, cx: &App) -> bool {
        self.query.read(cx).editor_focused(window, cx)
    }

    #[cfg(test)]
    pub fn query_executed(&self, cx: &App) -> bool {
        self.query.read(cx).executed()
    }

    #[cfg(test)]
    pub fn query_has_object(&self, name: &str, cx: &App) -> bool {
        self.query.read(cx).has_object(name)
    }

    #[cfg(test)]
    pub fn query_runs(&self, cx: &App) -> u32 {
        self.query.read(cx).run_count()
    }

    #[cfg(test)]
    pub fn query_draft(&self, cx: &App) -> String {
        self.query.read(cx).draft()
    }

    fn adopt_shell(&mut self, effect: Effect, cx: &mut Context<Self>) {
        let shell = self.model.shell.clone();
        self.foundation.update(cx, |view, cx| view.adopt(shell, effect, cx));
    }

    pub(crate) fn apply_effect(&mut self, effect: Effect, cx: &mut Context<Self>) {
        self.adopt_shell(effect, cx);
        let dark = matches!(self.model.shell.resolved_theme(), crate::app::ResolvedTheme::Dark);
        crate::ui::kit::apply_theme(dark, cx);
    }

    /// 连上或断开连接后刷新对象树，资源管理器才看得到表。
    fn refresh_tree_on_connect(&mut self, cx: &mut Context<Self>) {
        let connected: std::collections::BTreeSet<String> = self
            .model
            .connections
            .status
            .iter()
            .filter(|(_, status)| **status == crate::app::views::connections::LinkStatus::Connected)
            .map(|(id, _)| id.clone())
            .collect();
        if connected != self.connected_seen {
            for id in connected.difference(&self.connected_seen) {
                self.expanded.insert(id.clone());
            }
            let browse = self.pending_browser.take_if(|id| connected.contains(id));
            self.connected_seen = connected;
            self.forward_query("refresh-tree", cx);
            if let Some(id) = browse {
                if let Some((command, _, _)) = self.model.connections.rows.iter().find(|row| row.saved.id == id).and_then(super::chrome::browser_command) {
                    self.perform(&command, cx);
                }
            }
        }
    }

    fn hook_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.close_hooked {
            return;
        }
        self.close_hooked = true;
        let view = cx.entity().downgrade();
        window.on_window_should_close(cx, move |_window, cx| {
            let allow = std::rc::Rc::new(std::cell::Cell::new(true));
            let slot = allow.clone();
            if let Some(view) = view.upgrade() {
                view.update(cx, |this, cx| {
                    this.refresh_exchange(cx);
                    slot.set(this.model.allow_system_close());
                    cx.notify();
                });
            }
            allow.get()
        });
    }

    fn measure(&mut self, window: &Window) {
        let width = f32::from(window.viewport_size().width) as i32;
        if width <= 0 {
            return;
        }
        if !self.sized {
            self.sized = true;
            self.model.set_width(width);
            if width <= 960 {
                self.model.shell.sidebar_open = false;
            }
            return;
        }
        if width != self.model.viewport_width {
            self.model.set_width(width);
        }
    }

    fn boot_state(&mut self, cx: &mut Context<Self>) {
        if self.booted || !self.boot {
            return;
        }
        self.booted = true;
        let services = Arc::clone(&self.services);
        let legacy = legacy_webview_root();
        let platform = host_platform();
        let view = cx.entity().downgrade();
        let (tx, rx) = tokio::sync::oneshot::channel();
        jobs::runtime().spawn(async move {
            let startup = super::model::load_startup(&services, legacy.as_deref(), platform).await;
            let _ = tx.send(startup);
        });
        cx.spawn(async move |_, cx| {
            if let Ok(startup) = rx.await {
                cx.update(|cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |this, cx| {
                            this.model.apply_settings(&startup.state.shell);
                            this.model.restore_session(&startup.state.session);
                            if let Some(key) = startup.warning_key {
                                this.model.notice_key = Some(key);
                                this.model.startup_warning = Some(startup.warning_detail);
                            }
                            if this.model.viewport_width <= 960 {
                                this.model.shell.sidebar_open = false;
                            }
                            let _ = this.model.dispatch("conn-refresh");
                            let _ = this.model.dispatch("history-refresh");
                            this.persist_if_needed();
                            cx.notify();
                        })
                        ;
                    }
                });
            }
        })
        .detach();
    }

    fn pump_jobs(&mut self, cx: &mut Context<Self>) {
        let jobs = self.model.drain_pending();
        for job in jobs {
            self.spawn_job(job, cx);
        }
    }

    fn spawn_job(&mut self, job: PendingJob, cx: &mut Context<Self>) {
        let owner: &'static str = match job {
            PendingJob::RefreshHistory | PendingJob::SaveNamed | PendingJob::ClearHistory | PendingJob::DeleteRun | PendingJob::DeleteSaved => "history",
            _ => "connections",
        };
        let token = if owner == "history" {
            self.model.begin_owner(&self.services.tasks, owner)
        } else {
            self.model.prepare_list_token(&self.services.tasks)
        };
        let request = job_request(&self.model, job, token);
        let services = Arc::clone(&self.services);
        let rx = jobs::spawn_job(services, request);
        let view = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let done = rx.await;
            cx.update(|cx| {
                if let Some(view) = view.upgrade() {
                    view.update(cx, |this, cx| {
                        if let Ok(done) = done {
                            apply_done(&mut this.model, &this.services, owner, done);
                        }
                        this.model.finish_job();
                        this.persist_if_needed();
                        cx.notify();
                    })
                    ;
                }
            });
        })
        .detach();
    }

    pub(super) fn persist_if_needed(&mut self) {
        let state = self.model.snapshot();
        let blob = serde_json::to_string(&state).unwrap_or_default();
        if blob == self.last_state {
            return;
        }
        self.last_state = blob;
        let generation = self.revision.fetch_add(1, Ordering::SeqCst) + 1;
        let revision = Arc::clone(&self.revision);
        let gate = Arc::clone(&self.persist_gate);
        let services = Arc::clone(&self.services);
        jobs::runtime().spawn(async move {
            let _ = jobs::persist_revision(&services, &state, &revision, generation, &gate).await;
        });
    }

    fn prompt_dir(&self) -> std::path::PathBuf {
        self.services
            .state
            .path()
            .parent()
            .map(|path| path.to_path_buf())
            .filter(|path| path.is_dir())
            .unwrap_or_else(std::env::temp_dir)
    }

    fn prompt_import(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(SharedString::from(self.model.text("connections.page.importRillSql"))),
        });
        cx.spawn(async move |this, cx| {
            let message = receiver.await;
            cx.update(|cx| {
                let _ = this.update(cx, |view, cx| view.finish_import_prompt(message, cx));
            });
        })
        .detach();
    }

    fn prompt_export(&mut self, cx: &mut Context<Self>) {
        let suggested = if self.model.bundle_passphrase.trim().is_empty() {
            "rillsql-connections.json".to_owned()
        } else {
            "rillsql-connections.rillsqlcx".to_owned()
        };
        let directory = self.prompt_dir();
        let receiver = cx.prompt_for_new_path(&directory, Some(&suggested));
        cx.spawn(async move |this, cx| {
            let message = receiver.await;
            cx.update(|cx| {
                let _ = this.update(cx, |view, cx| view.finish_export_prompt(message, cx));
            });
        })
        .detach();
    }

    fn prompt_sqlite(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(SharedString::from(self.model.text("shell.command.openSqlite"))),
        });
        cx.spawn(async move |this, cx| {
            let message = receiver.await;
            cx.update(|cx| {
                let _ = this.update(cx, |view, cx| view.finish_sqlite_prompt(message, cx));
            });
        })
        .detach();
    }

    fn finish_import_prompt<E: std::fmt::Display>(
        &mut self,
        message: Result<Result<Option<Vec<std::path::PathBuf>>, E>, futures::channel::oneshot::Canceled>,
        cx: &mut Context<Self>,
    ) {
        match message {
            Ok(Ok(Some(paths))) => {
                if let Some(path) = paths.into_iter().next() {
                    match crate::app::accept_open_path(&path) {
                        Ok(path) => {
                            self.model.import_path = Some(path.display().to_string());
                            let _ = self.model.dispatch("conn-import");
                        }
                        Err(error) => {
                            self.model.notice_key = Some("connections.page.importRillSql");
                            self.model.notice_detail = error;
                        }
                    }
                }
            }
            Ok(Ok(None)) => {}
            Ok(Err(error)) => {
                self.model.notice_key = Some("connections.page.importRillSql");
                self.model.notice_detail = error.to_string();
            }
            Err(error) => {
                self.model.notice_key = Some("connections.page.importRillSql");
                self.model.notice_detail = error.to_string();
            }
        }
        self.persist_if_needed();
        cx.notify();
    }

    fn finish_export_prompt<E: std::fmt::Display>(
        &mut self,
        message: Result<Result<Option<std::path::PathBuf>, E>, futures::channel::oneshot::Canceled>,
        cx: &mut Context<Self>,
    ) {
        match message {
            Ok(Ok(Some(path))) => match crate::app::accept_save_path(&path) {
                Ok(path) => {
                    self.model.export_path = Some(path.display().to_string());
                    let _ = self.model.dispatch("conn-export");
                }
                Err(error) => {
                    self.model.notice_key = Some("connections.page.exportButton");
                    self.model.notice_detail = error;
                }
            },
            Ok(Ok(None)) => {}
            Ok(Err(error)) => {
                self.model.notice_key = Some("connections.page.exportButton");
                self.model.notice_detail = error.to_string();
            }
            Err(error) => {
                self.model.notice_key = Some("connections.page.exportButton");
                self.model.notice_detail = error.to_string();
            }
        }
        self.persist_if_needed();
        cx.notify();
    }

    fn finish_sqlite_prompt<E: std::fmt::Display>(
        &mut self,
        message: Result<Result<Option<Vec<std::path::PathBuf>>, E>, futures::channel::oneshot::Canceled>,
        cx: &mut Context<Self>,
    ) {
        match message {
            Ok(Ok(Some(paths))) => {
                if let Some(path) = paths.into_iter().next() {
                    match crate::app::accept_open_path(&path) {
                        Ok(path) => self.apply_sqlite_path(path),
                        Err(error) => {
                            self.model.notice_key = Some("connections.validate.chooseFile");
                            self.model.notice_detail = error;
                        }
                    }
                }
            }
            Ok(Ok(None)) => {}
            Ok(Err(error)) => {
                self.model.notice_key = Some("connections.validate.chooseFile");
                self.model.notice_detail = error.to_string();
            }
            Err(error) => {
                self.model.notice_key = Some("connections.validate.chooseFile");
                self.model.notice_detail = error.to_string();
            }
        }
        self.persist_if_needed();
        cx.notify();
    }

    fn refresh_compare(&mut self, cx: &mut Context<Self>) {
        let place = self.compare.update(cx, |pane, _| pane.take_place());
        self.model.compare_running = self.compare.read(cx).running();
        if let Some(place) = place {
            self.model.open_bound_query(place.sql, place.connection_id, place.connection_name, place.database, place.engine);
        }
    }

    fn ensure_diagram(&mut self, cx: &mut Context<Self>) {
        let spec = self.model.diagram_spec.clone().unwrap_or_default();
        if self.applied_diagram.as_deref() == Some(spec.as_str()) {
            return;
        }
        self.applied_diagram = Some(spec.clone());
        let locale = self.model.locale();
        self.diagram.update(cx, |pane, cx| {
            pane.set_locale(locale);
            pane.open_spec(&spec, cx);
        });
    }

    fn refresh_exchange(&mut self, cx: &mut Context<Self>) {
        let source = self.exchange_source(cx);
        self.exchange.update(cx, |pane, cx| {
            pane.bind_source(source.0, source.1, source.2, source.3);
            pane.ensure_mapping(cx);
        });
        self.model.exchange_running = self.exchange.read(cx).running();
    }

    #[allow(clippy::type_complexity)]
    fn exchange_source(&self, cx: &App) -> (Option<crate::grid::commands::TableRef>, Vec<String>, Vec<Vec<Option<String>>>, bool) {
        let (mut table, columns, rows, truncated) = self.table.read(cx).exchange_sheet();
        if table.is_none() {
            if let Some(target) = self.model.table_spec.as_deref().and_then(parse_target) {
                if !target.connection_id.is_empty() && !target.table.is_empty() {
                    table = Some(crate::grid::commands::TableRef {
                        connection_id: target.connection_id,
                        database: target.database,
                        schema: target.schema,
                        table: target.table,
                        engine: target.engine,
                        include_foreign_keys: false,
                    });
                }
            }
        }
        (table, columns, rows, truncated)
    }

    pub(super) fn accept_exchange_drop(&mut self, paths: &gpui_kit::ExternalPaths, cx: &mut Context<Self>) {
        let mut opened = false;
        for path in paths.paths() {
            if let Ok(path) = crate::app::accept_drop_path(path) {
                opened = true;
                self.exchange.update(cx, |pane, cx| {
                    pane.open = true;
                    pane.preview_file(path, crate::export::ImportEntry::Drop, cx);
                });
            }
        }
        if opened {
            self.model.exchange_open = true;
        }
        self.refresh_exchange(cx);
        cx.notify();
    }

    fn apply_sqlite_path(&mut self, path: std::path::PathBuf) {
        self.model.shell.note_opened(path.clone());
        let mut draft = blank_draft(Engine::Sqlite);
        draft.file_path = path.display().to_string();
        draft.name = path.file_stem().and_then(|name| name.to_str()).unwrap_or("sqlite").to_owned();
        self.model.connections.editor = Some(draft);
        self.model.connections.issues.clear();
        self.model.shell.page = AppPage::Connections;
        self.model.focused_field = Some(DraftField::File);
        let _ = self.model.dispatch("conn-save");
    }

}

impl Focusable for Workbench {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Workbench {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.hook_close(window, cx);
        self.measure(window);
        self.boot_state(cx);
        self.pump_jobs(cx);
        self.sync_query(window, cx);
        self.ensure_tool(cx);
        self.ensure_redis(cx);
        self.ensure_mongo(cx);
        self.model.redis_running = self.redis.read(cx).running();
        self.model.mongo_running = self.mongo.read(cx).running();
        self.ensure_diagram(cx);
        self.sync_grid_flags(cx);
        self.refresh_exchange(cx);
        self.refresh_compare(cx);
        self.refresh_tree_on_connect(cx);
        self.render_root(window, cx)
    }
}

fn parse_target(spec: &str) -> Option<GridTarget> {
    let parts: Vec<&str> = spec.split('|').collect();
    if parts.len() < 7 || parts[0].is_empty() || parts[4].is_empty() {
        return None;
    }
    Some(GridTarget {
        connection_id: parts[0].to_owned(),
        connection_name: parts[1].to_owned(),
        database: parts[2].to_owned(),
        schema: parts[3].to_owned(),
        table: parts[4].to_owned(),
        engine: parts[5].to_owned(),
        environment: parts[6].to_owned(),
    })
}

fn grid_surface_command(id: &str) -> bool {
    matches!(
        id,
        "edit-cell"
            | "cancel-edit"
            | "mark-delete"
            | "commit"
            | "copy-region"
            | "copy-sql-value"
            | "grid-copy-row"
            | "grid-copy-column"
            | "grid-filter-value"
            | "select-all-cells"
            | "grid-preview"
            | "grid-insert"
            | "grid-search"
            | "grid-sort"
            | "grid-filter"
            | "grid-page-next"
            | "grid-page-prev"
    )
}

fn query_command(id: &str) -> bool {
    matches!(
        id,
        "execute-query"
            | "run-selection"
            | "run-current"
            | "run-script"
            | "request-cancel"
            | "format"
            | "autocomplete"
            | "explain"
            | "expand-star"
            | "toggle-outline"
            | "refresh-tree"
            | "confirm-variables"
            | "cancel-variables"
            | "open-sql-file"
            | "save-sql-file"
    ) || id.starts_with("object:")
        || id.starts_with("database:")
        || id.starts_with("bind-connection:")
        || id.starts_with("statement:")
}

pub(super) fn nav_key(key: &str) -> Option<NavKey> {
    match key {
        "escape" => Some(NavKey::Escape),
        "up" => Some(NavKey::ArrowUp),
        "down" => Some(NavKey::ArrowDown),
        "enter" => Some(NavKey::Enter),
        "home" => Some(NavKey::Home),
        "end" => Some(NavKey::End),
        _ => None,
    }
}

pub fn host_platform() -> Platform {
    if cfg!(target_os = "macos") {
        Platform::Macos
    } else if cfg!(target_os = "linux") {
        Platform::Linux
    } else {
        Platform::Windows
    }
}

#[derive(Clone, Debug)]
pub struct LegacyRoots {
    pub platform: Platform,
    pub home: std::path::PathBuf,
    pub local_app_data: Option<std::path::PathBuf>,
    pub roaming_app_data: Option<std::path::PathBuf>,
    pub xdg_data_home: Option<std::path::PathBuf>,
}

pub fn legacy_webview_root_from(roots: &LegacyRoots) -> Option<std::path::PathBuf> {
    let id = "app.rillsql.desktop";
    match roots.platform {
        Platform::Windows => {
            let mut bases = Vec::new();
            if let Some(local) = &roots.local_app_data {
                bases.push(local.clone());
            }
            if let Some(roaming) = &roots.roaming_app_data {
                bases.push(roaming.clone());
            }
            bases.into_iter().map(|base| base.join(id)).find(|path| path.is_dir())
        }
        Platform::Macos => {
            let root = roots.home.join("Library").join("WebKit").join(id);
            root.is_dir().then_some(root)
        }
        Platform::Linux => {
            let base = roots.xdg_data_home.clone().unwrap_or_else(|| roots.home.join(".local").join("share"));
            let webkit = base.join("webkitgtk").join(id);
            let app = base.join(id);
            [webkit, app].into_iter().find(|path| path.is_dir())
        }
    }
}

pub fn legacy_webview_root() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    legacy_webview_root_from(&LegacyRoots {
        platform: host_platform(),
        home,
        local_app_data: std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from),
        roaming_app_data: std::env::var_os("APPDATA").map(std::path::PathBuf::from),
        xdg_data_home: std::env::var_os("XDG_DATA_HOME").map(std::path::PathBuf::from),
    })
}
