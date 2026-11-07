//! GPUI 外壳。菜单、工具栏、活动栏、侧栏、标签、状态栏、命令面板和六类菜单走同一个 dispatch。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use gpui_kit::component::{ActiveTheme as _, Theme, ThemeMode};
use gpui_kit::{
    App, AppContext as _, Context, Entity, FocusHandle, Focusable, IntoElement, MouseButton,
    ParentElement as _, PathPromptOptions, Render, SharedString, Styled as _, Window, WindowAppearance, div, prelude::*,
    px,
};

use crate::app::views::admin::AdminPane;
use crate::app::views::compare::ComparePane;
use crate::app::views::connections::{blank_draft, engines};
use crate::app::views::diagram::DiagramPane;
use crate::app::views::export::ExchangePane;
use crate::app::views::grid::{GridPane, GridTarget};
use crate::app::views::query::{QueryPane, SyncEffect, TabSnap};
use crate::app::views::structure::StructurePane;
use crate::app::{AppPage, Effect, FocusTarget};
use crate::connections::model::Engine;
use crate::state::Platform;
use crate::application::AppServices;
use crate::ui::Foundation;

use super::jobs::{self, apply_done, job_request};
use super::menus::NavKey;
use super::model::{DraftField, PendingJob, WorkTool, WorkbenchModel};
use crate::app::Shell;

pub struct Workbench {
    pub model: WorkbenchModel,
    foundation: Entity<Foundation>,
    query: Entity<QueryPane>,
    table: Entity<GridPane>,
    structure: Entity<StructurePane>,
    admin: Entity<AdminPane>,
    exchange: Entity<ExchangePane>,
    compare: Entity<ComparePane>,
    diagram: Entity<DiagramPane>,
    applied_tool: Option<String>,
    applied_diagram: Option<String>,
    services: Arc<AppServices>,
    focus: FocusHandle,
    field_focus: FocusHandle,
    close_hooked: bool,
    booted: bool,
    boot: bool,
    sized: bool,
    last_state: String,
    revision: Arc<AtomicU64>,
    persist_gate: Arc<tokio::sync::Mutex<()>>,
    bar: Option<&'static str>,
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
            applied_tool: None,
            applied_diagram: None,
            services,
            focus: cx.focus_handle(),
            field_focus: cx.focus_handle(),
            close_hooked: false,
            booted: false,
            boot,
            sized: false,
            last_state: String::new(),
            revision: Arc::new(AtomicU64::new(0)),
            persist_gate: Arc::new(tokio::sync::Mutex::new(())),
            bar: None,
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
            tab.running = effect.running.iter().any(|id| id == &tab.id);
        }
    }

    fn sync_grid_flags(&mut self, cx: &mut Context<Self>) {
        let dirty = self.table.read(cx).uncommitted() || self.query.read(cx).grid_uncommitted(cx);
        let writable = self.table.read(cx).writable() || self.query.read(cx).grid_writable(cx);
        self.model.grid_dirty = dirty;
        self.model.grid_writable = writable;
        if dirty {
            if let Some(id) = self.model.active_tab.clone() {
                if let Some(tab) = self.model.tabs.iter_mut().find(|tab| tab.id == id) {
                    tab.uncommitted_cells = true;
                }
            }
        } else {
            for tab in &mut self.model.tabs {
                tab.uncommitted_cells = false;
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
            WorkTool::Table => self.table.update(cx, |pane, cx| {
                pane.set_locale(locale);
                pane.open_table(target, 100, cx);
            }),
            WorkTool::Structure => self.structure.update(cx, |pane, _| {
                pane.set_locale(locale);
                pane.bind(target);
            }),
            WorkTool::Admin => self.admin.update(cx, |pane, _| {
                pane.set_locale(locale);
                pane.bind(target);
            }),
            WorkTool::Query => {}
        }
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
        if matches!(self.model.shell.resolved_theme(), crate::app::ResolvedTheme::Dark) {
            Theme::change(ThemeMode::Dark, None, cx);
        } else if self.model.shell.theme != crate::app::ThemePreference::System {
            Theme::change(ThemeMode::Light, None, cx);
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
                let _ = cx.update(|cx| {
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
            let _ = cx.update(|cx| {
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

    fn persist_if_needed(&mut self) {
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

    fn accept_exchange_drop(&mut self, paths: &gpui_kit::ExternalPaths, cx: &mut Context<Self>) {
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
        self.ensure_diagram(cx);
        self.sync_grid_flags(cx);
        self.refresh_exchange(cx);
        self.refresh_compare(cx);
        let frame = self.model.painted();
        let pad = px(frame.pad as f32);
        let entity = cx.entity();
        let focus = self.field_focus.clone();
        let page = self.model.shell.page;
        let table_view = page == AppPage::Workspace && self.model.tool == WorkTool::Table;
        let welcome = self.model.show_welcome() && !table_view;
        let palette = self.model.shell.palette_open;
        let help = self.model.shell.help_open;
        let menu_open = self.model.menu.is_some();
        let prompt = self.model.close_prompt.is_some();
        let confirm_delete = self.model.connections.confirm_delete;
        let bar = self.bar;
        let dark = matches!(self.model.shell.resolved_theme(), crate::app::ResolvedTheme::Dark);
        div()
            .id("workbench")
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_drop({
                let entity = entity.clone();
                move |paths: &gpui_kit::ExternalPaths, _, cx| {
                    entity.update(cx, |this, cx| this.accept_exchange_drop(paths, cx));
                }
            })
            .on_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" && this.model.connections.confirm_delete && this.model.menu.is_none() && this.model.close_prompt.is_none() {
                    this.perform("conn-delete-cancel", cx);
                    return;
                }
                if let Some(nav) = nav_key(&event.keystroke.key) {
                    if this.model.menu.is_some() || this.model.close_prompt.is_some() || this.model.shell.palette_open {
                        let _ = this.model.key(nav);
                        cx.notify();
                    }
                }
            }))
            .child(menu_bar(entity.clone(), bar, &self.model))
            .child(toolbar(entity.clone(), &self.model))
            .child(
                div()
                    .id("workbench-body")
                    .relative()
                    .flex()
                    .flex_1()
                    .w(px(frame.body_width as f32))
                    .overflow_hidden()
                    .child(activity(entity.clone(), &self.model))
                    .when(frame.sidebar_in_flex, |this| this.child(sidebar_pane(entity.clone(), &self.model)))
                    .child(
                        div()
                            .id("workbench-content")
                            .flex()
                            .flex_col()
                            .flex_1()
                            .w(px(frame.content_width as f32))
                            .overflow_hidden()
                            .child(tab_strip(entity.clone(), &self.model))
                            .when(page == AppPage::Connections, |this| this.child(connections_page(entity.clone(), focus.clone(), &self.model)))
                            .when(page == AppPage::History, |this| this.child(history_page(entity.clone(), focus.clone(), &self.model)))
                            .when(page == AppPage::Settings, |this| this.child(settings_page(entity.clone(), &self.model)))
                            .when(welcome, |this| this.child(welcome_page(entity.clone(), &self.model)))
                            .when(page == AppPage::Structure && self.model.tool == WorkTool::Admin, |this| this.child(self.admin.clone()))
                            .when(page == AppPage::Structure && self.model.tool != WorkTool::Admin, |this| this.child(self.structure.clone()))
                            .when(page == AppPage::Compare, |this| this.child(self.compare.clone()))
                            .when(page == AppPage::Diagram, |this| this.child(self.diagram.clone()))
                            .when(table_view, |this| {
                                this.child(
                                    div()
                                        .id("table-host")
                                        .flex()
                                        .flex_col()
                                        .flex_1()
                                        .child(self.table.clone()),
                                )
                            })
                            .when(page == AppPage::Workspace && !welcome && !table_view, |this| {
                                this.child(
                                    div()
                                        .id("editor-host")
                                        .flex()
                                        .flex_col()
                                        .flex_1()
                                        .on_mouse_down(MouseButton::Right, {
                                            let entity = entity.clone();
                                            move |_, _, cx| {
                                                entity.update(cx, |this, cx| this.perform("menu-editor", cx));
                                            }
                                        })
                                        .child(command_button("menu-editor", "query.editor.area", entity.clone(), &self.model))
                                        .child(command_button("menu-grid", "grid.menu.copy", entity.clone(), &self.model))
                                        .child(self.query.clone()),
                                )
                            }),
                    )
                    .when(frame.sidebar_overlay, |this| {
                        this.child(
                            div()
                                .id("sidebar-overlay")
                                .absolute()
                                .top_0()
                                .bottom_0()
                                .left(px(frame.activity_width as f32))
                                .w(px(frame.sidebar_width as f32))
                                .child(sidebar_pane(entity.clone(), &self.model)),
                        )
                    }),
            )
            .child(status_bar(&self.model, dark))
            .when(palette || help, |this| this.child(palette_layer(entity.clone(), focus.clone(), &self.model)))
            .when(menu_open, |this| this.child(context_layer(entity.clone(), &self.model)))
            .when(prompt, |this| this.child(close_layer(entity.clone(), &self.model)))
            .when(confirm_delete, |this| this.child(delete_layer(entity.clone(), &self.model)))
            .child(self.exchange.clone())
            .p(pad)
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

fn nav_key(key: &str) -> Option<NavKey> {
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

fn role_for(name: &str) -> gpui_kit::Role {
    match name {
        "MenuBar" => gpui_kit::Role::MenuBar,
        "Menu" => gpui_kit::Role::Menu,
        "MenuItem" => gpui_kit::Role::MenuItem,
        "Toolbar" => gpui_kit::Role::Toolbar,
        "Navigation" => gpui_kit::Role::Navigation,
        "TabList" => gpui_kit::Role::TabList,
        "Status" => gpui_kit::Role::Status,
        "Dialog" => gpui_kit::Role::Dialog,
        "Button" => gpui_kit::Role::Button,
        "MultilineTextInput" => gpui_kit::Role::MultilineTextInput,
        "Grid" => gpui_kit::Role::Grid,
        "TextInput" | "PasswordInput" => gpui_kit::Role::TextInput,
        _ => gpui_kit::Role::Group,
    }
}

fn command_button(id: &str, label_key: &str, entity: Entity<Workbench>, model: &WorkbenchModel) -> impl IntoElement {
    let command = id.to_owned();
    let label = SharedString::from(model.text(label_key));
    static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let element_id = SharedString::from(format!("cmd-{}-{id}", NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
    div()
        .id(element_id)
        .role(gpui_kit::Role::Button)
        .aria_label(label.clone())
        .aria_description(SharedString::from(format!("enabled {id}")))
        .child(label)
        .on_click(move |_, _, cx| {
            let command = command.clone();
            entity.update(cx, |this, cx| this.perform(&command, cx));
        })
}

fn menu_bar(entity: Entity<Workbench>, open: Option<&str>, model: &WorkbenchModel) -> impl IntoElement {
    let sections = [
        ("shell.menu.file", &["new-query", "open-sqlite", "save-query", "shell.command.quit"][..]),
        ("shell.menu.edit", &["format", "autocomplete"][..]),
        ("shell.menu.view", &["sidebar", "theme-light", "theme-dark", "theme-system", "settings", "help"][..]),
        ("shell.menu.query", &["execute-query", "run-current", "history", "saved-queries"][..]),
        ("shell.menu.tools", &["connections", "new-connection", "compare", "open-structure", "open-exchange"][..]),
        ("shell.menu.window", &["sidebar", "close-tab", "next-tab"][..]),
        ("shell.menu.help", &["help", "settings"][..]),
    ];
    let mut row = div()
        .id("menu-bar")
        .role(gpui_kit::Role::MenuBar)
        .aria_label(SharedString::from(model.text("shell.menu.file")))
        .aria_expanded(open.is_some())
        .flex()
        .gap(px(8.));
    for (key, commands) in sections {
        let label = SharedString::from(model.text(key));
        let key_owned = key.to_owned();
        let entity_click = entity.clone();
        row = row.child(
            div()
                .id(SharedString::from(format!("bar-{key}")))
                .role(gpui_kit::Role::MenuItem)
                .aria_label(label.clone())
                .aria_expanded(open == Some(key))
                .child(label)
                .on_click(move |_, _, cx| {
                    let key_owned = key_owned.clone();
                    entity_click
                        .update(cx, |this, cx| {
                            this.bar = if this.bar == Some(key) { None } else { Some(key) };
                            let _ = key_owned;
                            cx.notify();
                        })
                        ;
                }),
        );
        if open == Some(key) {
            for command in commands {
                let id = command.strip_prefix("shell.command.").unwrap_or(command);
                let action = if *command == "shell.command.quit" { "update-install" } else { id };
                // 退出仍走关闭守卫：Quit 动作在动作层里调用 allow_system_close。
                let action = if *command == "shell.command.quit" { "quit-window" } else { action };
                row = row.child(command_button(action, label_key_for(id), entity.clone(), model));
            }
        }
    }
    row
}

fn label_key_for(id: &str) -> &'static str {
    crate::app::Shell::command(id).map(|command| command.label_key).unwrap_or(match id {
        "format" => "shell.shortcuts.format",
        "autocomplete" => "shell.shortcuts.autocomplete",
        "run-current" => "shell.shortcuts.runCurrent",
        "close-tab" => "common.close",
        "next-tab" => "shell.shortcuts.nextTab",
        "quit" => "shell.command.quit",
        "open-exchange" => "export.open",
        _ => "shell.menu.file",
    })
}

fn toolbar(entity: Entity<Workbench>, model: &WorkbenchModel) -> impl IntoElement {
    let frame = model.painted();
    div()
        .id("toolbar")
        .role(gpui_kit::Role::Toolbar)
        .aria_label(SharedString::from(model.text("shell.command.newQuery")))
        .flex()
        .w(px(frame.body_width as f32))
        .when(frame.toolbar_wrap, |this| this.flex_wrap())
        .gap(px(8.))
        .child(command_button("new-query", "shell.command.newQuery", entity.clone(), model))
        .child(command_button("execute-query", "shell.command.execute", entity.clone(), model))
        .child(command_button("open-sqlite", "shell.command.openSqlite", entity.clone(), model))
        .child(command_button("save-query", "shell.command.saveQuery", entity.clone(), model))
        .child(command_button("history", "shell.command.history", entity.clone(), model))
        .child(command_button("command-palette", "shell.palette.title", entity.clone(), model))
        .child(command_button("sidebar", "shell.command.toggleSidebar", entity.clone(), model))
        .child(command_button("settings", "shell.command.settings", entity, model))
}

fn activity(entity: Entity<Workbench>, model: &WorkbenchModel) -> impl IntoElement {
    div()
        .id("activity")
        .role(gpui_kit::Role::Navigation)
        .aria_label(SharedString::from(model.text("shell.nav.main")))
        .w(px(model.layout().activity_width as f32))
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(command_button("activity-workspace", "shell.nav.workspace", entity.clone(), model))
        .child(command_button("activity-connections", "shell.nav.connections", entity.clone(), model))
        .child(command_button("activity-history", "shell.nav.history", entity.clone(), model))
        .child(command_button("activity-settings", "shell.nav.settings", entity.clone(), model))
        .child(command_button("open-exchange", "export.open", entity, model))
}

fn sidebar_pane(entity: Entity<Workbench>, model: &WorkbenchModel) -> impl IntoElement {
    let mut column = div()
        .id("sidebar")
        .role(gpui_kit::Role::Navigation)
        .aria_label(SharedString::from(model.text("shell.sidebar.explorer")))
        .aria_expanded(model.shell.sidebar_open)
        .w(px(model.layout().sidebar_width as f32))
        .flex()
        .flex_col()
        .child(command_button("menu-object", "catalog.tree.menuLabel", entity.clone(), model));
    for row in model.connections.rows.iter().take(12) {
        let id = format!("connection:{}", row.saved.id);
        let name = row.saved.name.clone();
        let entity_click = entity.clone();
        column = column.child(
            div()
                .id(SharedString::from(format!("side-{}", row.saved.id)))
                .role(gpui_kit::Role::TreeItem)
                .aria_label(SharedString::from(name.clone()))
                .aria_selected(model.connections.selected.as_deref() == Some(row.saved.id.as_str()))
                .child(SharedString::from(name))
                .on_mouse_down(MouseButton::Right, {
                    let entity_click = entity_click.clone();
                    let id = id.clone();
                    move |_, _, cx| {
                        let id = id.clone();
                        entity_click
                            .update(cx, |this, cx| {
                                let _ = this.model.dispatch(&id);
                                this.perform("menu-connection", cx);
                            })
                            ;
                    }
                })
                .on_click({
                    let id = id.clone();
                    move |_, _, cx| {
                        let id = id.clone();
                        entity_click.update(cx, |this, cx| this.perform(&id, cx));
                    }
                }),
        );
    }
    column
}

fn tab_strip(entity: Entity<Workbench>, model: &WorkbenchModel) -> impl IntoElement {
    let mut row = div()
        .id("tabs")
        .role(gpui_kit::Role::TabList)
        .aria_label(SharedString::from(model.text("shell.tabs.tablist")))
        .h(px(model.layout().tab_height as f32))
        .flex()
        .child(command_button("menu-tab", "shell.tabs.tablist", entity.clone(), model));
    for tab in &model.tabs {
        let selected = model.active_tab.as_deref() == Some(tab.id.as_str());
        let title = SharedString::from(tab.title.clone());
        let id = tab.id.clone();
        let entity_click = entity.clone();
        row = row.child(
            div()
                .id(SharedString::from(format!("tab-{id}")))
                .role(gpui_kit::Role::Tab)
                .aria_label(title.clone())
                .aria_selected(selected)
                .child(title)
                .on_mouse_down(MouseButton::Right, {
                    let entity_click = entity_click.clone();
                    move |_, _, cx| {
                        entity_click.update(cx, |this, cx| this.perform("menu-tab", cx));
                    }
                })
                .on_click(move |_, _, cx| {
                    let id = id.clone();
                    entity_click
                        .update(cx, |this, cx| {
                            this.model.active_tab = Some(id);
                            cx.notify();
                        })
                        ;
                }),
        );
    }
    row
}

fn status_bar(model: &WorkbenchModel, dark: bool) -> impl IntoElement {
    let notice = match model.notice_key {
        Some(key) if !model.notice_detail.is_empty() => format!("{} {}", model.text(key), model.notice_detail),
        Some(key) => model.text(key),
        None => model.text("shell.statusbar.noTarget"),
    };
    let summary = format!("{} {} {}", model.shell.locale.code(), if dark { "dark" } else { "light" }, notice);
    div()
        .id("status")
        .role(gpui_kit::Role::Status)
        .aria_label(SharedString::from(model.text("shell.statusbar.noTarget")))
        .h(px(model.layout().status_height as f32))
        .child(SharedString::from(summary))
}

fn palette_layer(entity: Entity<Workbench>, focus: FocusHandle, model: &WorkbenchModel) -> impl IntoElement {
    let mut list = div()
        .id("palette")
        .role(gpui_kit::Role::Dialog)
        .aria_label(SharedString::from(model.text("shell.palette.title")))
        .aria_expanded(model.shell.palette_open)
        .key_context("Palette")
        .flex()
        .flex_col()
        .child(field_box(entity.clone(), focus, DraftField::PaletteQuery, model.text("shell.palette.title"), model.palette_query.clone(), false));
    for (index, entry) in model.palette_entries().into_iter().take(20).enumerate() {
        let id = entry.id.clone();
        let label = SharedString::from(entry.label);
        let entity_click = entity.clone();
        list = list.child(
            div()
                .id(SharedString::from(format!("pal-{index}")))
                .role(gpui_kit::Role::MenuItem)
                .aria_label(label.clone())
                .aria_selected(index == model.palette_index)
                .child(label)
                .on_click(move |_, _, cx| {
                    let id = id.clone();
                    entity_click
                        .update(cx, |this, cx| {
                            this.model.shell.close_overlay();
                            this.perform(&id, cx);
                        })
                        ;
                }),
        );
    }
    list
}

fn context_layer(entity: Entity<Workbench>, model: &WorkbenchModel) -> impl IntoElement {
    let Some(menu) = &model.menu else {
        return div().id("context-menu-empty");
    };
    let mut list = div()
        .id("context-menu")
        .role(gpui_kit::Role::Menu)
        .aria_label(SharedString::from(model.text(menu.place.label_key())))
        .flex()
        .flex_col();
    for (index, item) in menu.items.iter().enumerate() {
        let id = item.id.to_owned();
        let label = SharedString::from(model.text(item.label_key));
        let state = if item.enabled { "enabled" } else { "disabled" };
        let entity_click = entity.clone();
        let enabled = item.enabled;
        list = list.child(
            div()
                .id(SharedString::from(format!("ctx-{id}")))
                .role(gpui_kit::Role::MenuItem)
                .aria_label(label.clone())
                .aria_selected(index == menu.index)
                .aria_description(SharedString::from(format!("{state} {}", item.reason_key.unwrap_or(item.id))))
                .child(label)
                .on_click(move |_, _, cx| {
                    if !enabled {
                        return;
                    }
                    let id = id.clone();
                    entity_click
                        .update(cx, |this, cx| {
                            this.model.menu = None;
                            this.model.shell.focus = this.model.menu_return;
                            this.perform(&id, cx);
                        })
                        ;
                }),
        );
    }
    list
}

fn close_layer(entity: Entity<Workbench>, model: &WorkbenchModel) -> impl IntoElement {
    let key = model.notice_key.unwrap_or("shell.closeGuard.closeWindow");
    let title = if model.close_prompt.as_ref().is_some_and(|prompt| matches!(prompt.scope, super::close::CloseScope::Tab)) {
        model.text("shell.closeGuard.closeTab")
    } else {
        model.text("shell.closeGuard.closeWindow")
    };
    div()
        .id("close-guard")
        .role(gpui_kit::Role::Dialog)
        .aria_label(SharedString::from(title.clone()))
        .flex()
        .flex_col()
        .child(SharedString::from(title))
        .child(SharedString::from(model.text(key)))
        .child(command_button("close-stay", "shell.closeGuard.stay", entity.clone(), model))
        .child(leave_button(entity, model))
}

fn delete_layer(entity: Entity<Workbench>, model: &WorkbenchModel) -> impl IntoElement {
    div()
        .id("delete-connection")
        .role(gpui_kit::Role::Dialog)
        .aria_label(SharedString::from(model.text("connections.deleteDialog.title")))
        .flex()
        .flex_col()
        .child(SharedString::from(model.text("connections.deleteDialog.title")))
        .child(SharedString::from(model.text("connections.deleteDialog.willKeepDb")))
        .child(command_button("conn-delete-cancel", "common.cancel", entity.clone(), model))
        .child(command_button("conn-delete-confirm", "connections.deleteDialog.confirm", entity, model))
}

fn leave_button(entity: Entity<Workbench>, model: &WorkbenchModel) -> impl IntoElement {
    let label = SharedString::from(model.text("shell.closeGuard.leave"));
    div()
        .id("cmd-close-leave")
        .role(gpui_kit::Role::Button)
        .aria_label(label.clone())
        .child(label)
        .on_click(move |_, _, cx| {
            entity
                .update(cx, |this, cx| {
                    let window_scope = this.model.close_prompt.as_ref().is_some_and(|prompt| matches!(prompt.scope, super::close::CloseScope::Window));
                    this.perform("close-leave", cx);
                    if window_scope && this.model.allow_system_close() {
                        cx.quit();
                    }
                })
                ;
        })
}

fn connections_page(entity: Entity<Workbench>, focus: FocusHandle, model: &WorkbenchModel) -> impl IntoElement {
    let mut page = div()
        .id("connections-page")
        .role(gpui_kit::Role::Group)
        .aria_label(SharedString::from(model.text("connections.page.title")))
        .flex()
        .flex_col()
        .child(SharedString::from(model.text("connections.page.title")))
        .child(SharedString::from(model.text("connections.page.note")))
        .child(SharedString::from(model.text("connections.page.emptyFirstHint")))
        .child(field_box(entity.clone(), focus.clone(), DraftField::Filter, model.text("connections.page.title"), model.connections.filter_text.clone(), false))
        .child(command_button("conn-filter-engine", "connections.page.title", entity.clone(), model))
        .child(command_button("conn-refresh", "connections.page.connect", entity.clone(), model))
        .child(field_box(entity.clone(), focus.clone(), DraftField::Url, model.text("connections.editor.newTitle"), model.url_buffer.clone(), false))
        .child(command_button("conn-apply-url", "connections.editor.newTitle", entity.clone(), model))
        .child(command_button("conn-import", "connections.page.importRillSql", entity.clone(), model))
        .child(command_button("conn-export", "connections.page.exportButton", entity.clone(), model))
        .child(field_box(entity.clone(), focus.clone(), DraftField::BundlePassphrase, model.text("connections.exportDialog.passphrase"), model.bundle_passphrase.clone(), true));
    for note in &model.import_notes {
        page = page.child(SharedString::from(format!("{} {}", model.text("connections.importReport.skippedTitle"), note)));
    }
    for engine in engines() {
        let id = format!("conn-engine:{}", engine.as_str());
        page = page.child(command_button(&id, "connections.editor.newTitle", entity.clone(), model));
    }
    for row in model.connections.filtered() {
        let id = format!("connection:{}", row.saved.id);
        let label = format!("{} {}", row.saved.name, row.saved.engine.as_str());
        let entity_click = entity.clone();
        let select = id.clone();
        page = page.child(
            div()
                .id(SharedString::from(format!("conn-row-{}", row.saved.id)))
                .role(gpui_kit::Role::ListItem)
                .aria_label(SharedString::from(label.clone()))
                .aria_selected(model.connections.selected.as_deref() == Some(row.saved.id.as_str()))
                .child(SharedString::from(label))
                .on_mouse_down(MouseButton::Right, {
                    let entity_click = entity_click.clone();
                    let select = select.clone();
                    move |_, _, cx| {
                        let select = select.clone();
                        entity_click
                            .update(cx, |this, cx| {
                                let _ = this.model.dispatch(&select);
                                this.perform("menu-connection", cx);
                            })
                            ;
                    }
                })
                .on_click(move |event, _, cx| {
                    let select = select.clone();
                    entity_click
                        .update(cx, |this, cx| {
                            this.perform(&select, cx);
                            if event.click_count() >= 2 {
                                this.perform("conn-connect", cx);
                            }
                        })
                        ;
                }),
        );
    }
    if let Some(draft) = &model.connections.editor {
        page = page
            .child(SharedString::from(model.text("connections.editor.newTitle")))
            .child(field_box(entity.clone(), focus.clone(), DraftField::Name, model.text("connections.editor.newTitle"), draft.name.clone(), false))
            .child(field_box(entity.clone(), focus.clone(), DraftField::Host, model.text("connections.validate.hostRequired"), draft.host.clone(), false))
            .child(field_box(entity.clone(), focus.clone(), DraftField::Port, model.text("connections.validate.portRange"), draft.port.clone(), false))
            .child(field_box(entity.clone(), focus.clone(), DraftField::Database, model.text("connections.page.title"), draft.database.clone(), false))
            .child(field_box(entity.clone(), focus.clone(), DraftField::Username, model.text("connections.validate.userRequired"), draft.username.clone(), false))
            .child(field_box(entity.clone(), focus.clone(), DraftField::File, model.text("connections.validate.chooseFile"), draft.file_path.clone(), false))
            .child(field_box(entity.clone(), focus.clone(), DraftField::Password, model.text("connections.page.note"), String::new(), true))
            .child(command_button("conn-tls-cycle", "connections.editor.tls.title", entity.clone(), model))
            .child(command_button("conn-ssh-toggle", "connections.editor.ssh.title", entity.clone(), model))
            .child(command_button("conn-ca-mode", "connections.editor.tls.caLabel", entity.clone(), model))
            .child(SharedString::from(model.text(if draft.ca_mode == crate::connections::model::CaMode::File {
                "connections.editor.tls.caFile"
            } else {
                "connections.editor.tls.caSystem"
            })))
            .child(field_box(entity.clone(), focus.clone(), DraftField::CaFile, model.text("connections.validate.caFileRequired"), draft.ca_file.clone(), false))
            .child(field_box(entity.clone(), focus.clone(), DraftField::ClientCert, model.text("connections.editor.tls.clientCert"), draft.client_cert.clone(), false))
            .child(field_box(entity.clone(), focus.clone(), DraftField::ClientKey, model.text("connections.editor.tls.clientKey"), draft.client_key.clone(), false))
            .child(field_box(entity.clone(), focus.clone(), DraftField::Replica, model.text("connections.editor.replicaSet"), draft.replica_set.clone(), false))
            .child(command_button("conn-ssh-auth", "connections.editor.ssh.authLabel", entity.clone(), model))
            .child(SharedString::from(model.text(if draft.ssh_auth == crate::connections::model::SshAuthKind::Password {
                "connections.editor.ssh.authPassword"
            } else {
                "connections.editor.ssh.authKey"
            })))
            .child(field_box(entity.clone(), focus.clone(), DraftField::SshHost, model.text("connections.validate.sshHostRequired"), draft.ssh_host.clone(), false))
            .child(field_box(entity.clone(), focus.clone(), DraftField::SshPort, model.text("connections.editor.ssh.port"), draft.ssh_port.clone(), false))
            .child(field_box(entity.clone(), focus.clone(), DraftField::SshUser, model.text("connections.validate.sshUserRequired"), draft.ssh_user.clone(), false))
            .child(field_box(entity.clone(), focus.clone(), DraftField::SshKey, model.text("connections.editor.ssh.keyFile"), draft.ssh_key.clone(), false))
            .child(field_box(entity.clone(), focus.clone(), DraftField::SshPassword, model.text("connections.editor.ssh.authPassword"), model.connections.secrets.ssh_password.clone(), true))
            .child(field_box(entity.clone(), focus, DraftField::Passphrase, model.text("connections.editor.ssh.title"), model.connections.secrets.ssh_passphrase.clone(), true))
            .child(command_button("conn-env-cycle", "connections.page.note", entity.clone(), model));
        for issue in &model.connections.issues {
            page = page.child(SharedString::from(model.text(issue.key)));
        }
        page = page
            .child(command_button("conn-save", "common.save", entity.clone(), model))
            .child(command_button("conn-test", "connections.editor.test.button", entity.clone(), model))
            .child(command_button("conn-connect", "connections.page.connect", entity.clone(), model))
            .child(command_button("conn-disconnect", "connections.page.menuDisconnect", entity.clone(), model))
            .child(command_button("conn-duplicate", "common.copy", entity.clone(), model))
            .child(command_button("conn-delete", "connections.page.menuDelete", entity, model));
    }
    page
}

fn history_page(entity: Entity<Workbench>, focus: FocusHandle, model: &WorkbenchModel) -> impl IntoElement {
    let mut page = div()
        .id("history-page")
        .role(gpui_kit::Role::Group)
        .aria_label(SharedString::from(model.text("history.page.titleRuns")))
        .flex()
        .flex_col()
        .child(SharedString::from(model.text("history.page.titleRuns")))
        .child(SharedString::from(model.text("history.page.loadOnlySql")))
        .child(SharedString::from(model.text("history.savedPanel.openNote")))
        .child(command_button("history", "history.page.runs", entity.clone(), model))
        .child(command_button("saved-queries", "history.page.saved", entity.clone(), model))
        .child(field_box(entity.clone(), focus.clone(), DraftField::HistorySearch, model.text("history.page.titleRuns"), model.history.filter_text.clone(), false))
        .child(field_box(entity.clone(), focus, DraftField::SaveName, model.text("history.savedPanel.save"), model.history.save_name.clone(), false))
        .child(command_button("history-save-named", "history.savedPanel.save", entity.clone(), model))
        .child(command_button("history-load", "history.page.loadToEditor", entity.clone(), model))
        .child(command_button("history-clear", "history.page.clearConfirm", entity.clone(), model))
        .child(command_button("menu-history", "history.page.tabsLabel", entity.clone(), model));
    for entry in model.history.filtered_runs().into_iter().take(20) {
        let id = entry.id.clone();
        let label = SharedString::from(entry.sql.clone());
        let entity_click = entity.clone();
        page = page.child(
            div()
                .id(SharedString::from(format!("run-{id}")))
                .role(gpui_kit::Role::ListItem)
                .aria_label(label.clone())
                .aria_selected(model.history.selected_run.as_deref() == Some(id.as_str()))
                .child(label)
                .on_mouse_down(MouseButton::Right, {
                    let entity_click = entity_click.clone();
                    let id = id.clone();
                    move |_, _, cx| {
                        let id = id.clone();
                        entity_click
                            .update(cx, |this, cx| {
                                this.model.history.selected_run = Some(id);
                                this.perform("menu-history", cx);
                            })
                            ;
                    }
                })
                .on_click(move |_, _, cx| {
                    let id = id.clone();
                    entity_click
                        .update(cx, |this, cx| {
                            this.model.history.selected_run = Some(id);
                            this.perform("history-load", cx);
                        })
                        ;
                }),
        );
    }
    for item in model.history.filtered_saved().into_iter().take(20) {
        let id = item.id.clone();
        let label = SharedString::from(item.name.clone());
        let entity_click = entity.clone();
        page = page.child(
            div()
                .id(SharedString::from(format!("saved-{id}")))
                .role(gpui_kit::Role::ListItem)
                .aria_label(label.clone())
                .child(label)
                .on_click(move |_, _, cx| {
                    let id = id.clone();
                    entity_click
                        .update(cx, |this, cx| {
                            this.model.history.section = crate::app::views::history::HistorySection::Saved;
                            this.model.history.selected_saved = Some(id);
                            this.perform("history-load", cx);
                        })
                        ;
                }),
        );
    }
    page
}

fn settings_page(entity: Entity<Workbench>, model: &WorkbenchModel) -> impl IntoElement {
    div()
        .id("settings-page")
        .role(gpui_kit::Role::Group)
        .aria_label(SharedString::from(model.text("shell.settings.title")))
        .flex()
        .flex_col()
        .child(SharedString::from(model.text("shell.settings.title")))
        .child(SharedString::from(model.text("shell.settings.density")))
        .child(command_button("density-compact", "shell.settings.densityCompact", entity.clone(), model))
        .child(command_button("density-standard", "shell.settings.densityStandard", entity.clone(), model))
        .child(command_button("density-relaxed", "shell.settings.densityRelaxed", entity.clone(), model))
        .child(command_button("theme-light", "shell.theme.light", entity.clone(), model))
        .child(command_button("theme-dark", "shell.theme.dark", entity.clone(), model))
        .child(command_button("theme-system", "shell.theme.system", entity.clone(), model))
        .child(command_button("locale-zh", "common.localeZh", entity.clone(), model))
        .child(command_button("locale-en", "common.localeEn", entity.clone(), model))
        .child(command_button("font-up", "shell.settings.editorFont", entity.clone(), model))
        .child(command_button("font-down", "shell.settings.editorFont", entity.clone(), model))
        .child(command_button("editor-theme-next", "shell.settings.editorTheme", entity.clone(), model))
        .child(SharedString::from(model.text(crate::app::views::settings::editor_theme_key(&model.settings.editor_theme))))
        .child(command_button("page-size-up", "shell.settings.rowsPerPage", entity.clone(), model))
        .child(command_button("preview-up", "shell.settings.previewLimit", entity.clone(), model))
        .child(command_button("null-next", "shell.settings.nullCopy", entity.clone(), model))
        .child(command_button("timeout-cycle", "shell.settings.timeout", entity.clone(), model))
        .child(command_button("toggle-updates", "shell.update.autoCheck", entity, model))
        .child(SharedString::from(model.text("shell.settings.editorThemeAuto")))
        .child(SharedString::from(model.text("shell.settings.editorThemeWarm")))
}

fn welcome_page(entity: Entity<Workbench>, model: &WorkbenchModel) -> impl IntoElement {
    let mut page = div()
        .id("welcome-page")
        .role(gpui_kit::Role::Group)
        .aria_label(SharedString::from(model.text("shell.welcome.title")))
        .flex()
        .flex_col()
        .child(SharedString::from(model.text("shell.welcome.title")))
        .child(SharedString::from(model.text("shell.welcome.factEngines")))
        .child(SharedString::from(model.text("shell.welcome.resumeTitle")))
        .child(SharedString::from(model.text("shell.welcome.recent")))
        .child(SharedString::from(model.text("shell.welcome.saved")))
        .child(command_button("welcome-connections", "common.newConnection", entity.clone(), model))
        .child(command_button("welcome-sqlite", "common.openSqlite", entity.clone(), model))
        .child(command_button("new-query", "shell.command.newQuery", entity.clone(), model));
    for entry in model.welcome.recent(&model.history.runs) {
        let id = entry.id.clone();
        let label = SharedString::from(entry.sql.clone());
        let entity_click = entity.clone();
        page = page.child(
            div()
                .id(SharedString::from(format!("welcome-run-{id}")))
                .role(gpui_kit::Role::Button)
                .aria_label(label.clone())
                .child(label)
                .on_click(move |_, _, cx| {
                    let id = id.clone();
                    entity_click
                        .update(cx, |this, cx| {
                            this.model.history.selected_run = Some(id);
                            this.model.history.section = crate::app::views::history::HistorySection::Runs;
                            this.perform("history-load", cx);
                        })
                        ;
                }),
        );
    }
    for item in model.welcome.saved(&model.history.saved) {
        let saved = item.clone();
        let label = SharedString::from(item.name.clone());
        let entity_click = entity.clone();
        page = page.child(
            div()
                .id(SharedString::from(format!("welcome-saved-{}", saved.id)))
                .role(gpui_kit::Role::Button)
                .aria_label(label.clone())
                .child(label)
                .on_click(move |_, _, cx| {
                    let saved = saved.clone();
                    entity_click
                        .update(cx, |this, cx| {
                            let known = this.model.known_ids();
                            let plan = this.model.welcome.open_saved(&saved, &known);
                            this.model.history.section = crate::app::views::history::HistorySection::Saved;
                            this.model.history.selected_saved = Some(saved.id.clone());
                            this.model.apply_load(plan);
                            cx.notify();
                        })
                        ;
                }),
        );
    }
    page
}

fn field_box(entity: Entity<Workbench>, focus: FocusHandle, field: DraftField, label: String, value: String, secret: bool) -> impl IntoElement {
    let shown = if secret {
        if value.is_empty() { String::new() } else { "••••".into() }
    } else {
        value
    };
    let label = SharedString::from(label);
    let id = SharedString::from(format!("field-{field:?}"));
    div()
        .id(id)
        .track_focus(&focus)
        .role(if secret { gpui_kit::Role::PasswordInput } else { gpui_kit::Role::TextInput })
        .aria_label(label.clone())
        .aria_description(SharedString::from("editable"))
        .child(label)
        .child(SharedString::from(shown))
        .on_mouse_down(MouseButton::Left, {
            let entity = entity.clone();
            let focus = focus.clone();
            move |_, window, cx| {
                window.focus(&focus, cx);
                entity
                    .update(cx, |this, cx| {
                        this.model.focused_field = Some(field);
                        this.model.shell.focus = FocusTarget::Settings;
                        cx.notify();
                    })
                    ;
            }
        })
        .on_key_down(move |event: &gpui_kit::KeyDownEvent, _, cx| {
            let text = typed_text(event);
            if let Some(text) = text {
                entity
                    .update(cx, |this, cx| {
                        this.model.focused_field = Some(field);
                        this.model.type_into(&text);
                        cx.notify();
                    })
                    ;
            }
        })
}

fn typed_text(event: &gpui_kit::KeyDownEvent) -> Option<String> {
    if event.keystroke.modifiers.control || event.keystroke.modifiers.platform || event.keystroke.modifiers.alt {
        return None;
    }
    match event.keystroke.key.as_str() {
        "backspace" => Some("backspace".into()),
        "escape" | "enter" | "tab" | "up" | "down" | "left" | "right" => None,
        _ => event.keystroke.key_char.clone().filter(|text| !text.is_empty()),
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

#[allow(dead_code)]
fn _role_name(role: gpui_kit::Role) -> &'static str {
    let _ = role_for("Button");
    let _ = role;
    "Button"
}
