//! GPUI 查询工作台。编辑器复用 SqlEditor，结果复用 DataTable。
//! 方言规则在 query::assist / query::desk；这里只提交任务和显示。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{resizable_panel, v_resizable, ActiveTheme as _, Disableable as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};

use crate::ui::kit::{self, Palette, Tip as _};

use crate::app::views::grid::{GridPane, GridTarget};
use crate::app::model::ObjectTarget;
use crate::application::AppServices;
use crate::i18n::{self, Locale};
use crate::query::assist::{self, LintFinding, RunScope, TreeObject};
use crate::query::desk::{self, BeginRun, Desk, DraftLink, DraftSync, PreparedRun, Target, Ticket};
use crate::ui::SqlEditor;

#[derive(Clone)]
pub struct TabSnap {
    pub id: String,
    pub draft: String,
    pub connection_id: Option<String>,
    pub connection_name: Option<String>,
    pub database: Option<String>,
    pub engine: Option<String>,
}

pub struct OpenRequest {
    pub draft: String,
    pub connection_id: Option<String>,
    pub connection_name: Option<String>,
    pub database: Option<String>,
    pub engine: Option<String>,
}

pub struct TargetRequest {
    pub id: String,
    pub connection_id: Option<String>,
    pub connection_name: Option<String>,
    pub database: Option<String>,
    pub engine: Option<String>,
}

pub struct SyncEffect {
    pub drafts: Vec<(String, String)>,
    pub running: Vec<String>,
    /// 每个标签最近一次真正执行过的 SQL。
    pub executed: Vec<(String, String)>,
    pub opens: Vec<OpenRequest>,
    pub targets: Vec<TargetRequest>,
    pub selected_object: Option<String>,
}

pub struct QueryPane {
    desk: Desk,
    services: Arc<AppServices>,
    editors: HashMap<String, Entity<SqlEditor>>,
    links: HashMap<String, DraftLink>,
    grid: Entity<GridPane>,
    locale: Locale,
    pending_text: Option<String>,
    opens: Vec<OpenRequest>,
    targets: Vec<TargetRequest>,
    variable_values: HashMap<String, String>,
    outline_open: bool,
    pending_completion: bool,
    pending_lint: Option<Vec<LintFinding>>,
    lint_seen: Option<(String, u64)>,
    timeout_ms: Option<u64>,
    polls: Arc<AtomicU32>,
    #[cfg(test)]
    last_timeout: Option<u64>,
    /// 外壳当前活动工作项是否是查询；为 false 时执行类命令只能拒绝。
    context_active: bool,
    focus_editor: bool,
    seen_active: Option<String>,
    fold_key: Option<(String, String)>,
    fold_wait: Option<(String, String)>,
    painted: Option<(Option<String>, u64, usize)>,
    selected_object: Option<String>,
    variable_inputs: HashMap<String, (Entity<InputState>, String, gpui_kit::Subscription)>,
    grid_menu_requested: bool,
    /// 公开设置：编辑器字号/配色、查询读取行数、预览上限、对象上限、空值文本。
    editor_font: i64,
    editor_theme: String,
    page_size: u64,
    preview_kb: u64,
    object_page: usize,
    null_copy: String,
}

/// 只有一条成功、只读且被读取上限截断的语句才允许“加载更多”。
/// 重新执行会再跑一次语句，写语句不能因为翻页被偷偷重放。
fn is_pageable(sheet: &desk::StatementView) -> bool {
    sheet.status == "ok"
        && !sheet.columns.is_empty()
        && matches!(
            crate::query::first_keyword(&sheet.sql).as_str(),
            "SELECT" | "WITH" | "VALUES" | "TABLE"
        )
}

fn load_more_plan(results: &[desk::StatementView], page_size: u64) -> Option<(String, u64)> {
    if results.len() != 1 {
        return None;
    }
    let sheet = &results[0];
    if !sheet.truncated || !is_pageable(sheet) {
        return None;
    }
    Some((sheet.sql.clone(), (sheet.rows.len() as u64).saturating_add(page_size.max(1))))
}

impl QueryPane {
    pub fn new(services: Arc<AppServices>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let grid = cx.new(|cx| GridPane::new(Arc::clone(&services), window, cx));
        Self {
            desk: Desk::new(),
            services,
            editors: HashMap::new(),
            links: HashMap::new(),
            grid,
            locale: Locale::ZhCn,
            pending_text: None,
            opens: Vec::new(),
            targets: Vec::new(),
            variable_values: HashMap::new(),
            outline_open: true,
            pending_completion: false,
            pending_lint: None,
            lint_seen: None,
            timeout_ms: None,
            polls: Arc::new(AtomicU32::new(0)),
            #[cfg(test)]
            last_timeout: None,
            context_active: false,
            focus_editor: false,
            seen_active: None,
            fold_key: None,
            fold_wait: None,
            painted: None,
            selected_object: None,
            variable_inputs: HashMap::new(),
            grid_menu_requested: false,
            editor_font: 13,
            editor_theme: "auto".to_owned(),
            page_size: 100,
            preview_kb: 1,
            object_page: 100,
            null_copy: "NULL".to_owned(),
        }
    }

    pub fn tree_objects(&self) -> &[TreeObject] {
        self.desk.objects()
    }

    pub fn tree_error(&self) -> &str {
        self.desk.tree_error()
    }

    pub fn remember_environments(&mut self, pairs: Vec<(String, String)>) {
        self.desk.remember_environments(pairs);
    }

    pub fn set_query_timeout_sec(&mut self, seconds: i64) {
        self.timeout_ms = if seconds > 0 { Some(seconds as u64 * 1000) } else { None };
    }

    /// 把公开设置下发给编辑器和结果网格；无需重启，下一次绘制就生效。
    pub fn apply_public_settings(&mut self, settings: &crate::state::ShellSettings, cx: &mut Context<Self>) {
        let form = crate::app::views::settings::SettingsForm::from_settings(settings);
        self.editor_font = form.editor_font;
        self.editor_theme = form.editor_theme;
        self.page_size = u64::try_from(form.page_size.max(1)).unwrap_or(1);
        self.preview_kb = u64::try_from(form.preview_kb.max(1)).unwrap_or(1);
        self.object_page = usize::try_from(form.object_page.max(1)).unwrap_or(1);
        // 保留原始设置；GridPane::set_null_copy 负责转成显示文本，避免二次归一。
        self.null_copy = form.null_copy.clone();
        self.desk.set_page_size(self.page_size);
        let font = self.editor_font;
        let theme = self.editor_theme.clone();
        for editor in self.editors.values() {
            // 每帧都会下发一次；值没变时不要 notify，否则编辑器会持续重绘。
            let unchanged = {
                let current = editor.read(cx);
                current.font_size() == font && current.theme() == theme
            };
            if unchanged {
                continue;
            }
            editor.update(cx, |editor, cx| {
                editor.set_font_size(font);
                editor.set_theme(&theme);
                cx.notify();
            });
        }
        let preview_kb = self.preview_kb;
        let null_copy = self.null_copy.clone();
        self.grid.update(cx, |grid, _| {
            grid.set_preview_kb(preview_kb);
            grid.set_null_copy(&null_copy);
        });
    }

    pub fn editor_font(&self) -> i64 {
        self.editor_font
    }

    pub fn editor_theme(&self) -> &str {
        &self.editor_theme
    }

    pub fn page_size(&self) -> u64 {
        self.page_size
    }

    /// 下一次查询执行真正会用的读取行数上限（Desk 的当前值）。
    pub fn run_limit(&self) -> u64 {
        self.desk.page_size()
    }

    pub fn preview_kb(&self) -> u64 {
        self.preview_kb
    }

    pub fn object_page(&self) -> usize {
        self.object_page
    }

    pub fn null_copy(&self) -> &str {
        &self.null_copy
    }

    pub fn request_focus(&mut self) {
        self.focus_editor = true;
    }

    #[cfg(test)]
    pub fn has_object(&self, name: &str) -> bool {
        self.desk.objects().iter().any(|object| object.name == name)
    }

    #[cfg(test)]
    pub fn run_count(&self) -> u32 {
        self.desk.executions()
    }

    pub fn executed(&self) -> bool {
        self.desk.executed()
    }

    pub fn note(&self) -> String {
        self.desk.active_note()
    }

    /// 当前查询是否仍在执行；交换面板据此显示“结果尚未就绪”。
    pub fn busy(&self) -> bool {
        self.desk.active_id().is_some_and(|id| self.desk.running_ids().iter().any(|running| running == id))
    }

    /// 当前查询项的当前语句结果（列、已读取行、是否有表头）。
    /// 直接读 Desk 的结果，避免交换面板取到上一帧的网格快照。
    #[allow(clippy::type_complexity)]
    pub fn exchange_result(&self) -> (Vec<String>, Vec<Vec<Option<String>>>, bool) {
        let results = self.desk.results();
        let Some(sheet) = results.get(self.desk.statement().min(results.len().saturating_sub(1))) else {
            return (Vec::new(), Vec::new(), false);
        };
        (sheet.columns.clone(), sheet.rows.clone(), !sheet.columns.is_empty())
    }

    /// 当前查询目标的连接 / 库，用于交换面板标明来源。
    pub fn exchange_context(&self) -> String {
        self.desk.target().map(|target| {
            let name = if target.connection_name.is_empty() { target.connection_id } else { target.connection_name };
            if target.database.is_empty() {
                name
            } else {
                format!("{name} / {}", target.database)
            }
        }).unwrap_or_default()
    }

    #[cfg(test)]
    pub fn editor_text(&self, cx: &App) -> String {
        self.desk.active_id().and_then(|id| self.editors.get(id)).map(|editor| editor.read(cx).value(cx)).unwrap_or_default()
    }

    #[cfg(test)]
    pub fn editor_focused(&self, window: &Window, cx: &App) -> bool {
        let Some(id) = self.desk.active_id() else {
            return false;
        };
        self.editors.get(id).is_some_and(|editor| editor.read(cx).is_focused(window, cx))
    }

    #[cfg(test)]
    pub fn polls(&self) -> u32 {
        self.polls.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub fn last_timeout(&self) -> Option<u64> {
        self.last_timeout
    }

    #[cfg(test)]
    pub fn running(&self) -> bool {
        self.desk.active_id().is_some_and(|id| self.desk.running_ids().iter().any(|running| running == id))
    }

    pub fn draft(&self) -> String {
        self.desk.draft()
    }

    pub fn cell(&self, row: usize, col: usize, cx: &App) -> String {
        self.grid.read(cx).cell(row, col)
    }

    pub fn grid_uncommitted(&self, cx: &App) -> bool {
        self.grid.read(cx).uncommitted()
    }

    pub fn grid_writable(&self, cx: &App) -> bool {
        self.grid.read(cx).writable()
    }

    pub fn grid_command(&mut self, id: &str, cx: &mut Context<Self>) -> Option<String> {
        self.grid.update(cx, |grid, cx| grid.command(id, cx))
    }

    pub fn grid_banner(&self, cx: &App) -> String {
        self.grid.read(cx).banner()
    }

    #[cfg(test)]
    pub fn plant_customers(&mut self) {
        use crate::query::desk::OpenLink;
        self.desk.set_links(vec![OpenLink {
            id: "notes".to_owned(),
            name: "笔记".to_owned(),
            engine: "sqlite".to_owned(),
            databases: vec!["main".to_owned()],
            environment: "dev".to_owned(),
        }]);
        self.desk.set_objects(
            vec![TreeObject {
                connection_id: "notes".to_owned(),
                connection_name: "笔记".to_owned(),
                engine: "sqlite".to_owned(),
                database: "main".to_owned(),
                schema: String::new(),
                name: "customers".to_owned(),
                kind: "table".to_owned(),
            }],
            vec!["main".to_owned()],
        );
    }

    /// 两个连接里有同名表：对象动作必须按完整目标区分，不能只比对象名。
    #[cfg(test)]
    pub fn plant_same_name_objects(&mut self) {
        use crate::query::desk::OpenLink;
        self.desk.set_links(vec![
            OpenLink {
                id: "pg".to_owned(),
                name: "销售库".to_owned(),
                engine: "postgres".to_owned(),
                databases: vec!["shop".to_owned()],
                environment: "dev".to_owned(),
            },
            OpenLink {
                id: "pg2".to_owned(),
                name: "仓库库".to_owned(),
                engine: "postgres".to_owned(),
                databases: vec!["shop".to_owned()],
                environment: "prod".to_owned(),
            },
        ]);
        self.desk.set_objects(
            vec![
                TreeObject {
                    connection_id: "pg".to_owned(),
                    connection_name: "销售库".to_owned(),
                    engine: "postgres".to_owned(),
                    database: "shop".to_owned(),
                    schema: "public".to_owned(),
                    name: "orders".to_owned(),
                    kind: "table".to_owned(),
                },
                TreeObject {
                    connection_id: "pg2".to_owned(),
                    connection_name: "仓库库".to_owned(),
                    engine: "postgres".to_owned(),
                    database: "shop".to_owned(),
                    schema: "public".to_owned(),
                    name: "orders".to_owned(),
                    kind: "table".to_owned(),
                },
            ],
            vec!["shop".to_owned()],
        );
    }

    pub fn command(&mut self, id: &str, cx: &mut Context<Self>) {
        // 表/结构等非查询工作项激活时，补全、格式化、执行、计划、变量、取消和语句选择
        // 都不能落在隐藏的查询标签上。
        if requires_active_query(id) && !self.context_active {
            self.desk.set_active_note(NOT_ACTIVE_QUERY);
            cx.notify();
            return;
        }
        if let Some(connection) = id.strip_prefix("bind-connection:") {
            self.queue_connection(connection);
            cx.notify();
            return;
        }
        if let Some(database) = id.strip_prefix("database:") {
            self.queue_database(database);
            cx.notify();
            return;
        }
        if let Some(rest) = id.strip_prefix("object:") {
            // 用完整目标定位对象：同名对象不会因为只比 连接:对象名 而串到一起。
            if let Some(target) = ObjectTarget::parse(rest) {
                if let Some(object) = self
                    .desk
                    .objects()
                    .iter()
                    .find(|object| {
                        target.matches_parts(
                            &object.connection_id,
                            &object.database,
                            &object.schema,
                            &object.name,
                        )
                    })
                    .cloned()
                {
                    let spec = object_spec(&object, &self.desk.environment_of(&object.connection_id));
                    self.selected_object = Some(spec);
                    self.queue_object(&object);
                } else {
                    // 目标已不在目录里：保留原选择并说明原因，不静默改用同名对象。
                    self.desk.set_active_note(&i18n::text(self.locale, "catalog.tree.staleObject"));
                }
            }
            cx.notify();
            return;
        }
        if let Some(index) = id.strip_prefix("statement:") {
            if let Ok(index) = index.parse::<usize>() {
                self.desk.set_statement(index);
                self.rebind_grid(cx);
            }
            cx.notify();
            return;
        }
        self.capture(cx);
        match id {
            "execute-query" | "run-selection" => self.start(RunScope::Auto, None, cx),
            "run-current" => self.start(RunScope::Current, None, cx),
            "run-script" => self.start(RunScope::Script, None, cx),
            "request-cancel" => self.cancel(cx),
            "query-load-more" => self.load_more(cx),
            "format" => self.format(cx),
            "autocomplete" => self.complete(cx),
            "explain" => self.explain(cx),
            "expand-star" => self.expand(cx),
            "toggle-outline" => {
                self.outline_open = !self.outline_open;
                cx.notify();
            }
            "refresh-tree" => self.refresh_tree(cx),
            "confirm-variables" => self.confirm_variables(cx),
            "cancel-variables" => {
                self.desk.cancel_variables();
                self.variable_values.clear();
                cx.notify();
            }
            "open-sql-file" => self.prompt_open(cx),
            "save-sql-file" => self.prompt_save(cx),
            _ => {}
        }
    }

    pub fn sync(&mut self, tabs: &[TabSnap], active: Option<&str>, locale: Locale, window: &mut Window, cx: &mut Context<Self>) -> SyncEffect {
        let context = active.is_some_and(|id| tabs.iter().any(|tab| tab.id == id));
        let effect = self.prepare(tabs, active, locale, context, cx);
        self.ensure_editors(window, cx);
        effect
    }

    /// 把外壳标签和编辑器草稿对齐。不创建编辑器，所以命令层没有 Window 也能调用。
    /// context 由外壳给出：只有查询工作项处于活动状态且在工作区时才是 true。
    pub fn prepare(&mut self, tabs: &[TabSnap], active: Option<&str>, locale: Locale, context: bool, cx: &App) -> SyncEffect {
        self.locale = locale;
        let next = active.map(str::to_owned);
        if next != self.seen_active {
            self.seen_active = next;
            if self.seen_active.is_some() {
                self.focus_editor = true;
            }
        }
        let ids: Vec<String> = tabs.iter().map(|tab| tab.id.clone()).collect();
        for tab in tabs {
            self.desk.import_tab(&tab.id, &tab.draft, target_of(tab));
            if !self.links.contains_key(&tab.id) {
                self.links.insert(
                    tab.id.clone(),
                    DraftLink { applied: tab.draft.clone(), editor: tab.draft.clone(), dirty: false },
                );
            }
            if self.pending_text.is_none() {
                if let Some(editor) = self.editors.get(&tab.id) {
                    let text = editor.read(cx).value(cx);
                    if let Some(link) = self.links.get_mut(&tab.id) {
                        if link.editor != text {
                            link.editor = text;
                            link.dirty = true;
                        }
                    }
                }
            }
            let action = self.links.get_mut(&tab.id).map(|link| desk::sync_draft(link, &tab.draft));
            match action {
                Some(DraftSync::Pull) => {
                    self.desk.replace_draft(&tab.id, &tab.draft, &self.services.tasks);
                    self.pending_text = Some(tab.draft.clone());
                }
                Some(DraftSync::Push(text)) => self.desk.set_tab_draft(&tab.id, &text),
                Some(DraftSync::Same) | None => {}
            }
        }
        self.desk.focus_tab(active);
        self.context_active = context && active.is_some_and(|id| self.desk.has_tab(id));
        self.desk.retain_tabs(&ids, &self.services.tasks);
        self.editors.retain(|id, _| ids.iter().any(|keep| keep == id));
        self.links.retain(|id, _| ids.iter().any(|keep| keep == id));
        self.desk.refresh_outline();
        self.note_lint();
        let mut drafts = Vec::new();
        for tab in tabs {
            let Some(text) = self.desk.tab_draft(&tab.id) else { continue };
            if text == tab.draft {
                continue;
            }
            if let Some(link) = self.links.get_mut(&tab.id) {
                link.applied = text.clone();
                if self.pending_text.is_none() {
                    link.editor = text.clone();
                }
                link.dirty = false;
            }
            drafts.push((tab.id.clone(), text));
        }
        let executed = tabs.iter().filter_map(|tab| self.desk.executed_sql(&tab.id).map(|sql| (tab.id.clone(), sql))).collect();
        SyncEffect {
            drafts,
            running: self.desk.running_ids(),
            executed,
            opens: std::mem::take(&mut self.opens),
            targets: std::mem::take(&mut self.targets),
            selected_object: self.selected_object.take(),
        }
    }

    pub fn ensure_editors(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for id in self.desk.tab_ids() {
            if self.editors.contains_key(&id) {
                continue;
            }
            let sql = self.desk.tab_draft(&id).unwrap_or_default();
            let font = self.editor_font;
            let theme = self.editor_theme.clone();
            let editor = cx.new(|cx| SqlEditor::new(window, cx));
            editor.update(cx, |editor, cx| {
                editor.set_font_size(font);
                editor.set_theme(&theme);
                editor.set_sql(&sql, window, cx);
            });
            self.editors.insert(id.clone(), editor);
            self.links.insert(id, DraftLink { applied: sql.clone(), editor: sql, dirty: false });
        }
    }

    pub fn show_sheet(&mut self, columns: Vec<String>, rows: Vec<Vec<String>>, cx: &mut App) {
        let rows = rows.into_iter().map(|row| row.into_iter().map(Some).collect()).collect();
        self.desk.set_active_sheet(columns, rows);
        self.rebind_grid(cx);
    }

    fn capture(&mut self, cx: &App) {
        let Some(id) = self.desk.active_id().map(str::to_owned) else {
            return;
        };
        let Some(editor) = self.editors.get(&id) else {
            return;
        };
        let text = editor.read(cx).value(cx);
        let range = editor.read(cx).selected_range(cx);
        self.desk.focus_tab(Some(&id));
        self.desk.set_tab_draft(&id, &text);
        self.desk.set_selection(range.start, range.end, range.end);
        if let Some(link) = self.links.get_mut(&id) {
            link.editor = text;
            link.dirty = false;
        }
    }

    fn note_lint(&mut self) {
        if !self.context_active {
            return;
        }
        let Some(id) = self.desk.active_id().map(str::to_owned) else {
            return;
        };
        if self.desk.running_ids().iter().any(|running| running == &id) {
            return;
        }
        let version = self.desk.version();
        if self.lint_seen.as_ref().is_some_and(|(seen, seen_version)| seen == &id && *seen_version == version) {
            return;
        }
        let engine = self.desk.target().map(|target| target.engine).unwrap_or_default();
        let sql = self.desk.draft();
        let Some(ticket) = self.desk.issue(&self.services.tasks) else {
            return;
        };
        let findings = assist::lint_sql(&sql, &engine);
        if self.desk.accept_lint(&self.services.tasks, &ticket, findings.clone()) {
            self.lint_seen = Some((id, version));
            self.pending_lint = Some(findings);
        }
    }

    fn start(&mut self, scope: RunScope, override_sql: Option<String>, cx: &mut Context<Self>) {
        match self.desk.begin_run(&self.services.tasks, scope, override_sql) {
            BeginRun::Ready(prepared) => self.spawn_run(prepared, cx),
            BeginRun::Variables(fields) => {
                self.variable_values.clear();
                for field in fields {
                    self.variable_values.insert(field.key, String::new());
                }
                self.desk.set_active_note(&i18n::text(self.locale, "query.variables.title"));
                cx.notify();
            }
            BeginRun::Rejected(reason) => {
                self.desk.set_active_note(&reason);
                cx.notify();
            }
        }
    }

    fn spawn_run(&mut self, prepared: PreparedRun, cx: &mut Context<Self>) {
        let ticket = Ticket { tab_id: prepared.tab_id.clone(), token: prepared.token, version: prepared.version };
        let services = Arc::clone(&self.services);
        let timeout_ms = self.timeout_ms;
        #[cfg(test)]
        {
            self.last_timeout = timeout_ms;
        }
        self.polls.store(0, Ordering::Relaxed);
        let polls = Arc::clone(&self.polls);
        let (id_tx, id_rx) = tokio::sync::oneshot::channel();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        runtime().spawn(async move {
            let started = match crate::query::commands::start_run(
                &services.connections,
                &services.query,
                crate::query::commands::StartRequest {
                    connection_id: prepared.connection_id.clone(),
                    connection_name: prepared.connection_name.clone(),
                    database: prepared.database.clone(),
                    engine: prepared.engine.clone(),
                    sql: prepared.sql.clone(),
                    limit: prepared.limit,
                    timeout_ms,
                },
            )
            .await
            {
                Ok(started) => started,
                Err(error) => {
                    let _ = done_tx.send(Err(error));
                    return;
                }
            };
            let _ = id_tx.send(started.id.clone());
            loop {
                match crate::query::commands::poll_run(&services.query, &started.id, false).await {
                    Ok(snap) if !matches!(snap.status.as_str(), "running" | "cancel-requested") => {
                        polls.fetch_add(1, Ordering::Relaxed);
                        let _ = done_tx.send(Ok(snap));
                        return;
                    }
                    Ok(_) => {
                        polls.fetch_add(1, Ordering::Relaxed);
                        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                    }
                    Err(error) => {
                        polls.fetch_add(1, Ordering::Relaxed);
                        let _ = done_tx.send(Err(error));
                        return;
                    }
                }
            }
        });
        let tab_id = ticket.tab_id.clone();
        let view = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            if let Ok(run_id) = id_rx.await {
                cx.update(|cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |pane, cx| pane.note_started(&tab_id, &run_id, cx));
                    }
                });
            }
        })
        .detach();
        let view = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            if let Ok(result) = done_rx.await {
                cx.update(|cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |pane, cx| {
                            let cancel_text = i18n::text(pane.locale, "query.run.cancel");
                            pane.desk.finish_polled(&pane.services.tasks, &ticket, result, &cancel_text);
                            pane.rebind_grid(cx);
                            cx.notify();
                        });
                    }
                });
            }
        })
        .detach();
        // 运行态由结果标题的 spinner 表达；note 留给错误或提示，避免“正在执行”重复两遍。
        self.desk.set_active_note("");
        cx.notify();
    }

    /// 结果被读取上限截断时，再执行一次同一条只读语句并放大窗口。
    /// 写语句不会因为翻页被自动重放。
    fn load_more(&mut self, cx: &mut Context<Self>) {
        if !self.context_active {
            self.desk.set_active_note(NOT_ACTIVE_QUERY);
            cx.notify();
            return;
        }
        let Some((sql, limit)) = load_more_plan(&self.desk.results(), self.page_size) else {
            return;
        };
        match self.desk.begin_run_limited(&self.services.tasks, RunScope::Current, Some(sql), limit) {
            BeginRun::Ready(prepared) => self.spawn_run(prepared, cx),
            BeginRun::Variables(_) => {}
            BeginRun::Rejected(reason) => {
                self.desk.set_active_note(&reason);
                cx.notify();
            }
        }
    }

    fn note_started(&mut self, tab_id: &str, run_id: &str, cx: &mut Context<Self>) {
        self.desk.note_run(tab_id, run_id);
        if self.desk.cancelled(tab_id) {
            self.spawn_cancel(run_id.to_owned(), cx);
        }
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        let run_id = self.desk.cancel(&self.services.tasks);
        if let Some(run_id) = run_id {
            self.spawn_cancel(run_id, cx);
        }
        self.desk.set_active_note(&i18n::text(self.locale, "query.run.cancel"));
        cx.notify();
    }

    fn spawn_cancel(&self, run_id: String, cx: &mut Context<Self>) {
        let services = Arc::clone(&self.services);
        runtime().spawn(async move {
            let _ = crate::query::commands::request_cancel(&services.connections, &services.query, &run_id).await;
        });
        cx.notify();
    }

    fn format(&mut self, cx: &mut Context<Self>) {
        let formatted = self.desk.format_active();
        if formatted.ok {
            self.pending_text = Some(formatted.sql);
            self.desk.set_active_note("");
        } else if let Some(key) = formatted.reason_key {
            self.desk.set_active_note(&i18n::text(self.locale, key));
        }
        cx.notify();
    }

    fn expand(&mut self, cx: &mut Context<Self>) {
        let expanded = self.desk.expand_active();
        if expanded.ok {
            self.pending_text = Some(expanded.sql);
        } else if let Some(key) = expanded.reason_key {
            self.desk.set_active_note(&i18n::text(self.locale, key));
        }
        cx.notify();
    }

    fn explain(&mut self, cx: &mut Context<Self>) {
        match self.desk.explain_active() {
            Ok(sql) => self.start(RunScope::Script, Some(sql), cx),
            Err(reason) => {
                self.desk.set_active_note(&reason);
                cx.notify();
            }
        }
    }

    fn complete(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.desk.active_id().map(str::to_owned) else {
            return;
        };
        let prefix = self.editors.get(&id).map(|editor| editor.read(cx).completion_prefix(cx)).unwrap_or_default();
        let items = self.desk.completion_for(&prefix);
        let pairs: Vec<(String, String)> = items.iter().map(|item| (item.label.clone(), item.insert.clone())).collect();
        if let Some(ticket) = self.desk.issue(&self.services.tasks) {
            let _ = self.desk.accept_completion(&self.services.tasks, &ticket, items);
        }
        if let Some(editor) = self.editors.get(&id) {
            editor.update(cx, |editor, _| editor.set_extra_suggestions(pairs));
        }
        self.pending_completion = true;
        cx.notify();
    }

    fn confirm_variables(&mut self, cx: &mut Context<Self>) {
        let Some(sql) = self.desk.confirm_variables(&self.variable_values) else {
            return;
        };
        self.variable_values.clear();
        self.start(RunScope::Script, Some(sql), cx);
    }

    fn refresh_tree(&mut self, cx: &mut Context<Self>) {
        let services = Arc::clone(&self.services);
        let page = self.object_page;
        let (tx, rx) = tokio::sync::oneshot::channel();
        runtime().spawn(async move {
            let _ = tx.send(desk::load_tree(&services.connections, page).await);
        });
        cx.spawn(async move |this, cx| {
            if let Ok(loaded) = rx.await {
                let _ = cx.update(|cx| {
                    this.update(cx, |pane, cx| {
                        match loaded {
                            Ok(loaded) => {
                                pane.desk.set_objects(loaded.objects, loaded.databases);
                                pane.desk.set_links(loaded.links);
                                let failure = loaded.failures.join("\n");
                                pane.desk.set_tree_error(&failure);
                                for (key, cols) in loaded.columns {
                                    let mut parts = key.split('\u{1}');
                                    let connection = parts.next().unwrap_or("");
                                    let database = parts.next().unwrap_or("");
                                    let table = parts.next().unwrap_or("");
                                    pane.desk.remember_columns(connection, database, table, cols);
                                }
                                if failure.is_empty() {
                                    pane.desk.set_active_note("");
                                } else {
                                    pane.desk.set_active_note(&failure);
                                }
                            }
                            Err(error) => {
                                // 整次读取失败也要进入对象区错误态，而不是显示成“尚未读取对象”。
                                pane.desk.set_tree_error(&error);
                                pane.desk.set_active_note(&error);
                            }
                        }
                        cx.notify();
                    })
                });
            }
        })
        .detach();
    }

    fn prompt_open(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(SharedString::from(i18n::text(self.locale, "query.fileIo.open"))),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await {
                if let Some(path) = paths.into_iter().next() {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    runtime().spawn(async move {
                        let loaded = crate::application::run_blocking(move || assist::read_sql_file(&path)).await;
                        let _ = tx.send(loaded);
                    });
                    if let Ok(loaded) = rx.await {
                        cx.update(|cx| {
                            let _ = this.update(cx, |pane, cx| pane.finish_open(loaded, cx));
                        });
                    }
                }
            }
        })
        .detach();
    }

    fn finish_open(&mut self, loaded: Result<assist::FileLoad, String>, cx: &mut Context<Self>) {
        match loaded {
            Ok(file) => {
                if file.execute {
                    self.desk.set_active_note(&i18n::text(self.locale, "history.page.loadOnlySql"));
                    return;
                }
                let Some(id) = self.desk.active_id().map(str::to_owned) else {
                    self.opens.push(OpenRequest {
                        draft: file.sql,
                        connection_id: None,
                        connection_name: None,
                        database: None,
                        engine: None,
                    });
                    cx.notify();
                    return;
                };
                self.desk.replace_draft(&id, &file.sql, &self.services.tasks);
                self.pending_text = Some(file.sql.clone());
                if let Some(link) = self.links.get_mut(&id) {
                    link.editor = file.sql;
                    link.applied = link.editor.clone();
                    link.dirty = true;
                }
                self.desk.set_active_note(&i18n::text(self.locale, "history.page.loadOnlySql"));
            }
            Err(error) => self.desk.set_active_note(&error),
        }
        cx.notify();
    }

    fn prompt_save(&mut self, cx: &mut Context<Self>) {
        self.capture(cx);
        let sql = self.desk.draft();
        let directory = std::env::temp_dir();
        let name = assist::sql_save_name();
        let receiver = cx.prompt_for_new_path(&directory, Some(&name));
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(path))) = receiver.await {
                let (tx, rx) = tokio::sync::oneshot::channel();
                runtime().spawn(async move {
                    let wrote = crate::application::run_blocking(move || assist::write_sql_file(&path, &sql)).await;
                    let _ = tx.send(wrote);
                });
                if let Ok(wrote) = rx.await {
                    let _ = cx.update(|cx| {
                        this.update(cx, |pane, cx| {
                            pane.desk.set_active_note(&match wrote {
                                Ok(()) => i18n::text(pane.locale, "query.fileIo.saved"),
                                Err(error) => error,
                            });
                            cx.notify();
                        })
                    });
                }
            }
        })
        .detach();
    }

    fn rebind_grid(&mut self, cx: &mut App) {
        self.refresh_grid(cx);
        self.painted = Some((self.desk.active_id().map(str::to_owned), self.desk.view_gen(), self.desk.statement()));
    }

    fn sync_grid(&mut self, cx: &mut App) {
        let key = (self.desk.active_id().map(str::to_owned), self.desk.view_gen(), self.desk.statement());
        if self.painted.as_ref() == Some(&key) {
            return;
        }
        self.rebind_grid(cx);
    }

    fn refresh_grid(&mut self, cx: &mut App) {
        let results = self.desk.results();
        let index = if results.is_empty() { 0 } else { self.desk.statement().min(results.len() - 1) };
        let Some(sheet) = results.get(index) else {
            self.grid.update(cx, |grid, cx| grid.clear_sheet(cx));
            return;
        };
        let columns = if sheet.columns.is_empty() { vec!["status".to_owned()] } else { sheet.columns.clone() };
        let rows = if sheet.columns.is_empty() {
            vec![vec![Some(sheet.message.clone())]]
        } else {
            sheet.rows.clone()
        };
        let target = self.grid_target();
        let sql = sheet.sql.clone();
        let engine = target.engine.clone();
        self.grid.update(cx, |grid, cx| {
            let mut target = target;
            target.engine = engine;
            grid.show_query(target, &sql, columns, rows, cx);
        });
    }

    fn grid_target(&self) -> GridTarget {
        let Some(target) = self.desk.target() else {
            return GridTarget::default();
        };
        let environment = if target.environment.is_empty() { self.desk.environment_of(&target.connection_id) } else { target.environment };
        GridTarget {
            connection_id: target.connection_id,
            connection_name: target.connection_name,
            database: target.database,
            schema: target.schema,
            table: String::new(),
            engine: target.engine,
            environment,
        }
    }

    fn apply_pending(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.desk.active_id().map(str::to_owned);
        if let Some(editor) = id.as_deref().and_then(|id| self.editors.get(id)) {
            let scope = self.desk.completion_scope();
            editor.read(cx).set_completion_scope(scope);
        }
        if let Some(sql) = self.pending_text.take() {
            if let Some(id) = id.as_deref() {
                if let Some(editor) = self.editors.get(id).cloned() {
                    editor.update(cx, |editor, cx| editor.set_sql(&sql, window, cx));
                }
                if let Some(link) = self.links.get_mut(id) {
                    link.editor = sql.clone();
                    link.applied = sql.clone();
                    link.dirty = false;
                }
                self.desk.refresh_outline();
            }
        }
        if self.pending_completion {
            self.pending_completion = false;
            if let Some(id) = id.as_deref() {
                if let Some(editor) = self.editors.get(id).cloned() {
                    editor.update(cx, |editor, cx| editor.show_completions(window, cx));
                }
            }
        }
        if let Some(findings) = self.pending_lint.take() {
            if let Some(id) = self.desk.active_id() {
                if let Some(editor) = self.editors.get(id).cloned() {
                    let version = editor.read(cx).version();
                    let locale = self.locale;
                    editor.update(cx, |editor, cx| {
                        if let Some(finding) = findings.first() {
                            let message = i18n::text(locale, finding.message_key);
                            let _ = editor.apply_lint(version, &message, finding.start..finding.end, cx);
                        } else {
                            let _ = editor.clear_lint(version, cx);
                        }
                    });
                }
            }
        }
    }

    /// 高亮会在消化文本改动的那一帧覆盖折叠候选，所以等到下一帧再安装。
    fn refresh_folds(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.desk.active_id().map(str::to_owned) else {
            return;
        };
        let Some(editor) = self.editors.get(&id).cloned() else {
            return;
        };
        let sql = editor.read(cx).value(cx);
        let key = (id, sql.clone());
        if self.fold_key.as_ref() == Some(&key) {
            return;
        }
        if self.fold_wait.as_ref() != Some(&key) {
            self.fold_wait = Some(key);
            return;
        }
        self.fold_key = Some(key);
        self.fold_wait = None;
        let engine = self.desk.target().map(|target| target.engine).filter(|engine| !engine.is_empty()).unwrap_or_else(|| "sqlite".to_owned());
        let ranges = assist::fold_line_ranges(&sql, &engine);
        editor.update(cx, |editor, cx| {
            let _ = editor.apply_folds(&ranges, cx);
        });
    }

    fn take_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus_editor {
            return;
        }
        let Some(editor) = self.desk.active_id().and_then(|id| self.editors.get(id)).cloned() else {
            return;
        };
        self.focus_editor = false;
        editor.update(cx, |editor, cx| editor.focus(window, cx));
    }

    fn queue_object(&mut self, object: &TreeObject) {
        self.opens.push(OpenRequest {
            draft: assist::browse_sql(&object.engine, &object.schema, &object.name),
            connection_id: Some(object.connection_id.clone()),
            connection_name: Some(object.connection_name.clone()),
            database: Some(object.database.clone()),
            engine: Some(object.engine.clone()),
        });
    }

    fn queue_database(&mut self, database: &str) {
        let Some(id) = self.desk.active_id().map(str::to_owned) else {
            return;
        };
        if self.desk.select_database(database) {
            if let Some(target) = self.desk.target() {
                self.targets.push(TargetRequest {
                    id,
                    connection_id: Some(target.connection_id),
                    connection_name: Some(target.connection_name),
                    database: Some(target.database),
                    engine: Some(target.engine),
                });
            }
        }
    }

    fn queue_connection(&mut self, id: &str) {
        let Some(tab_id) = self.desk.active_id().map(str::to_owned) else {
            return;
        };
        if self.desk.select_connection(id) {
            if let Some(target) = self.desk.target() {
                self.targets.push(TargetRequest {
                    id: tab_id,
                    connection_id: Some(target.connection_id),
                    connection_name: Some(target.connection_name),
                    database: Some(target.database),
                    engine: Some(target.engine),
                });
            }
        }
    }
}

impl Render for QueryPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.apply_pending(window, cx);
        self.refresh_folds(cx);
        self.sync_grid(cx);
        self.take_focus(window, cx);
        let p = Palette::of(cx);
        let locale = self.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let links = self.desk.links().to_vec();
        let databases = self.desk.databases().to_vec();
        let outline = self.desk.outline();
        let results = self.desk.results();
        let variables = self.desk.variables();
        let note = self.desk.active_note();
        let target = self.desk.target();
        let running = self.desk.active_id().is_some_and(|id| self.desk.running_ids().iter().any(|running| running == id));
        let editor = self.desk.active_id().and_then(|id| self.editors.get(id)).cloned();
        let grid = self.grid.clone();
        let entity = cx.entity();

        // ---- 工具条
        let run: AnyElement = if running {
            kit::danger_button("query-act-request-cancel", Some(IconName::Square), t("ui.query.stop"), pane_act("request-cancel", &entity)).into_any_element()
        } else {
            kit::primary_button("query-act-execute-query", Some(IconName::Play), t("ui.query.run"), pane_act("execute-query", &entity))
                .tooltip(SharedString::from(format!("{} · {}", i18n::text(locale, "shell.shortcuts.runSelection"), kit::shortcut("Ctrl+Enter"))))
                .into_any_element()
        };
        let more_entity = entity.clone();
        let more = Button::new("query-more")
            .ghost()
            .small()
            .icon(IconName::Ellipsis)
            .tooltip(t("ui.query.more"))
            .accessibility_label(t("ui.query.more"))
            .dropdown_menu(move |menu, _, _| {
                let item = |id: &'static str, key: &str| {
                    let entity = more_entity.clone();
                    PopupMenuItem::new(i18n::text(locale, key)).on_click(move |_, _, cx| {
                        entity.update(cx, |pane, cx| pane.command(id, cx));
                    })
                };
                menu.item(item("run-current", "query.editor.menu.runCurrent"))
                    .item(item("run-script", "query.run.runScript"))
                    .separator()
                    .item(item("autocomplete", "shell.shortcuts.autocomplete"))
                    .item(item("expand-star", "query.complete.expandStar"))
                    .item(item("toggle-outline", "query.outline.heading"))
                    .separator()
                    .item(item("open-sql-file", "query.fileIo.open"))
                    .item(item("save-sql-file", "query.fileIo.save"))
            });

        let connection_label = target
            .as_ref()
            .map(|target| if target.connection_name.is_empty() { target.connection_id.clone() } else { target.connection_name.clone() })
            .unwrap_or_else(|| i18n::text(locale, "ui.query.chooseConnection"));
        let engine = target.as_ref().map(|target| target.engine.clone()).unwrap_or_default();
        let conn_entity = entity.clone();
        let conn_links = links.clone();
        let mut conn_button = Button::new("query-connection")
            .outline()
            .small()
            .label(SharedString::from(connection_label))
            .dropdown_caret(true)
            .accessibility_label(t("ui.query.chooseConnection"));
        if target.is_some() {
            conn_button = conn_button.icon(IconName::Database);
        }
        let connection_picker = conn_button.dropdown_menu(move |mut menu, _, _| {
            if conn_links.is_empty() {
                menu = menu.item(PopupMenuItem::new(i18n::text(locale, "ui.query.noConnection")).disabled(true));
            }
            for link in &conn_links {
                let id = link.id.clone();
                let entity = conn_entity.clone();
                let name = if link.name.is_empty() { link.id.clone() } else { link.name.clone() };
                menu = menu.item(PopupMenuItem::new(format!("{name}  ·  {}", kit::engine_label(&link.engine))).on_click(move |_, _, cx| {
                    let id = id.clone();
                    entity.update(cx, |pane, cx| pane.command(&format!("bind-connection:{id}"), cx));
                }));
            }
            menu
        });
        let database_label = target.as_ref().map(|target| target.database.clone()).filter(|name| !name.is_empty()).unwrap_or_else(|| i18n::text(locale, "ui.query.chooseDatabase"));
        let db_entity = entity.clone();
        let db_list = databases.clone();
        let database_picker = Button::new("query-database")
            .outline()
            .small()
            .label(SharedString::from(database_label))
            .dropdown_caret(true)
            .disabled(databases.is_empty())
            .accessibility_label(t("query.database.label"))
            .dropdown_menu(move |mut menu, _, _| {
                for database in &db_list {
                    let name = database.clone();
                    let entity = db_entity.clone();
                    menu = menu.item(PopupMenuItem::new(name.clone()).on_click(move |_, _, cx| {
                        let name = name.clone();
                        entity.update(cx, |pane, cx| pane.command(&format!("database:{name}"), cx));
                    }));
                }
                menu
            });

        let toolbar = div()
            .id("query-toolbar")
            .flex_none()
            .h(px(42.))
            .px(px(10.))
            .flex()
            .items_center()
            .gap(px(4.))
            .border_b_1()
            .border_color(p.line)
            .child(run)
            .child(kit::vline(&p))
            .child(kit::tool_button("query-act-format", Some(IconName::WandSparkles), t("query.editor.menu.format"), pane_act("format", &entity)))
            .child(kit::tool_button("query-act-explain", Some(IconName::ChartNetwork), t("query.editor.menu.plan"), pane_act("explain", &entity)))
            .child(more)
            .child(div().flex_1())
            .when(!engine.is_empty(), |this| this.child(kit::engine_badge(&engine, 18.)))
            .child(connection_picker)
            .child(kit::icon(IconName::ChevronRight, 13., p.text_faint))
            .child(database_picker);

        // ---- 编辑器 + 大纲
        let mut editor_row = div().flex().size_full().min_h(px(0.));
        if let Some(editor) = editor {
            editor_row = editor_row.child(div().id("query-editor").flex_1().min_w(px(0.)).h_full().child(editor));
        } else {
            editor_row = editor_row.child(
                div()
                    .id("query-editor-empty")
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(kit::empty_state(IconName::SquareTerminal, t("shell.command.newQuery"), t("shell.statusbar.noTarget"), &p)),
            );
        }
        if self.outline_open && outline.len() > 1 {
            let mut list = div()
                .id("query-outline")
                .flex_none()
                .w(px(220.))
                .h_full()
                .overflow_y_scroll()
                .border_l_1()
                .border_color(p.line)
                .bg(p.chrome)
                .p(px(8.))
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(div().px(px(6.)).pb(px(6.)).child(kit::eyebrow(t("query.outline.heading"), &p)));
            for item in outline {
                let failed = item.status == "error" || item.status == "failed";
                list = list.child(
                    div()
                        .px(px(6.))
                        .py(px(4.))
                        .rounded(px(6.))
                        .flex()
                        .gap(px(6.))
                        .text_size(px(12.))
                        .hover(|style| style.bg(p.hover))
                        .child(div().flex_none().text_color(p.text_faint).child(SharedString::from(format!("{}", item.index))))
                        .child(div().flex_1().min_w(px(0.)).truncate().font_family(cx.theme().mono_font_family.clone()).when(failed, |this| this.text_color(p.danger)).child(SharedString::from(item.preview))),
                );
            }
            editor_row = editor_row.child(list);
        }

        // ---- 结果
        let mut header = div()
            .id("query-results-head")
            .flex_none()
            .h(px(34.))
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(2.))
            .bg(p.chrome)
            .border_b_1()
            .border_color(p.line);
        if results.len() > 1 {
            let current = self.desk.statement();
            for (index, result) in results.iter().enumerate() {
                let selected = index == current;
                let failed = result.status == "error" || result.status == "failed";
                let id = format!("statement:{index}");
                let entity = entity.clone();
                header = header.child(
                    div()
                        .id(SharedString::from(format!("query-act-statement-{index}")))
                        .role(gpui_kit::Role::Tab)
                        .aria_label(SharedString::from(format!("{} {}", i18n::text(locale, "query.results.statement"), index + 1)))
                        .aria_selected(selected)
                        .h(px(26.))
                        .px(px(10.))
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .rounded(px(6.))
                        .text_size(px(12.))
                        .cursor_pointer()
                        .when(selected, |this| this.bg(p.surface).text_color(p.text).shadow_sm())
                        .when(!selected, |this| this.text_color(p.text_muted).hover(|style| style.bg(p.hover)))
                        .child(kit::status_dot(if failed { p.danger } else { p.ok }))
                        .child(SharedString::from(format!("{} {}", i18n::text(locale, "query.results.statement"), index + 1)))
                        .on_click(move |_, _, cx| {
                            let id = id.clone();
                            entity.update(cx, |pane, cx| pane.command(&id, cx));
                        }),
                );
            }
        } else {
            header = header.child(div().px(px(6.)).child(kit::eyebrow(t("ui.query.results"), &p)));
        }
        let active_result = results.get(self.desk.statement()).or_else(|| results.first()).cloned();
        let summary = active_result.as_ref().map(|result| {
            if let Some(affected) = result.affected.filter(|_| result.columns.is_empty()) {
                format!("{affected} ✓")
            } else {
                format!("{} {}", result.rows.len(), i18n::text(locale, "ui.grid.rows"))
            }
        });
        let failed_note = active_result.as_ref().is_some_and(|result| result.status == "error" || result.status == "failed");
        header = header.child(div().flex_1()).when(running, |this| {
            this.child(div().flex().items_center().gap(px(6.)).text_size(px(12.)).text_color(p.text_muted).child(Spinner::new().xsmall().color(p.accent)).child(t("query.status.running")))
        });
        if !note.is_empty() {
            header = header.child(
                div()
                    .id("query-note")
                    .max_w(px(520.))
                    .truncate()
                    .px(px(6.))
                    .text_size(px(12.))
                    .text_color(if failed_note { p.danger } else { p.text_muted })
                    .tooltip_text(SharedString::from(note.clone()))
                    .child(SharedString::from(note.clone())),
            );
        }
        if let Some(summary) = summary {
            header = header.child(div().px(px(6.)).text_size(px(12.)).text_color(p.text_faint).child(SharedString::from(summary)));
        }
        if !running && results.len() == 1 && is_pageable(&results[0]) {
            match load_more_plan(&results, self.page_size) {
                Some((_, limit)) => {
                    let loaded = results[0].rows.len() as u64;
                    let more = entity.clone();
                    header = header.child(
                        div()
                            .id("query-load-more")
                            .role(gpui_kit::Role::Button)
                            .aria_label(SharedString::from(i18n::text(locale, "query.results.loadMore")))
                            .h(px(24.))
                            .px(px(8.))
                            .rounded(px(6.))
                            .text_size(px(12.))
                            .text_color(p.accent)
                            .bg(p.accent_soft)
                            .cursor_pointer()
                            .hover(|style| style.bg(p.hover))
                            .child(SharedString::from(format!(
                                "{} +{}",
                                i18n::text(locale, "query.results.loadMore"),
                                limit.saturating_sub(loaded)
                            )))
                            .on_click(move |_, _, cx| {
                                more.update(cx, |pane, cx| pane.load_more(cx));
                            }),
                    );
                }
                None => {
                    // 结果没有被截断：明确告诉用户已经读完，而不是看起来没有分页入口。
                    header = header.child(
                        div()
                            .id("query-load-more-done")
                            .h(px(24.))
                            .px(px(8.))
                            .flex()
                            .items_center()
                            .text_size(px(12.))
                            .text_color(p.text_muted)
                            .child(SharedString::from(i18n::text(locale, "query.results.allLoaded"))),
                    );
                }
            }
        }

        let mut results_panel = div().id("query-results").flex().flex_col().size_full().bg(p.surface).child(header);
        if !variables.is_empty() {
            let mut row = div()
                .id("query-variables")
                .flex_none()
                .p(px(10.))
                .flex()
                .flex_wrap()
                .items_end()
                .gap(px(10.))
                .bg(p.accent_soft)
                .border_b_1()
                .border_color(p.line)
                .child(div().flex().items_center().gap(px(6.)).h(px(30.)).child(kit::icon(IconName::Braces, 14., p.accent_text)).child(div().font_weight(gpui_kit::FontWeight::MEDIUM).child(t("query.variables.title"))));
            for field in &variables {
                let value = self.variable_values.get(&field.key).cloned().unwrap_or_default();
                let input = self.variable_input(&field.key, &value, window, cx);
                row = row.child(div().w(px(180.)).child(kit::form_row(SharedString::from(field.label.clone()), kit::text_input(SharedString::from(format!("query-var-{}", field.key)), &input, SharedString::from(field.label.clone())).small(), &p)));
            }
            row = row
                .child(kit::primary_button("query-act-confirm-variables", Some(IconName::Play), t("query.variables.proceed"), pane_act("confirm-variables", &entity)))
                .child(kit::secondary_button("query-act-cancel-variables", None, t("query.variables.cancel"), pane_act("cancel-variables", &entity)));
            results_panel = results_panel.child(row);
        }
        let grid_entity = entity.clone();
        if results.is_empty() {
            results_panel = results_panel.child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(kit::empty_state(IconName::Table, t("ui.query.results"), t("ui.query.noResults"), &p).child(div().flex().gap(px(6.)).child(kit::kbd(kit::shortcut("Ctrl+Enter"), &p)))),
            );
        } else if active_result.as_ref().is_some_and(|result| result.columns.is_empty()) {
            let result = active_result.clone().unwrap_or_else(|| results[0].clone());
            let ok = !(result.status == "error" || result.status == "failed");
            let message = if result.message.is_empty() { note.clone() } else { result.message.clone() };
            results_panel = results_panel.child(
                div()
                    .flex_1()
                    .p(px(16.))
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .text_color(if ok { p.ok } else { p.danger })
                            .child(kit::icon(if ok { IconName::CircleCheck } else { IconName::CircleAlert }, 16., if ok { p.ok } else { p.danger }))
                            .child(SharedString::from(if message.is_empty() { result.status.clone() } else { message })),
                    )
                    .child(div().font_family(cx.theme().mono_font_family.clone()).text_size(px(12.)).text_color(p.text_muted).child(SharedString::from(result.sql.clone()))),
            );
        } else {
            results_panel = results_panel.child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .on_mouse_down(MouseButton::Right, move |_, _, cx| {
                        grid_entity.update(cx, |pane, _| pane.grid_menu_requested = true);
                    })
                    .child(grid),
            );
        }

        div()
            .id("query-pane")
            .flex()
            .flex_col()
            .size_full()
            .child(toolbar)
            .child(
                div().flex_1().min_h(px(0.)).child(
                    v_resizable("query-split")
                        .child(resizable_panel().size_range(px(120.)..px(4000.)).child(editor_row))
                        .child(resizable_panel().size(px(300.)).size_range(px(90.)..px(4000.)).child(results_panel)),
                ),
            )
    }
}

fn pane_act(id: &'static str, entity: &Entity<QueryPane>) -> impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut App) + 'static {
    let entity = entity.clone();
    move |_, _, cx| {
        entity.update(cx, |pane, cx| pane.command(id, cx));
    }
}

impl QueryPane {
    fn variable_input(&mut self, key: &str, value: &str, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        if let Some((state, last, _)) = self.variable_inputs.get_mut(key) {
            let state = state.clone();
            kit::sync_text(&state, last, value, window, cx);
            return state;
        }
        let initial = SharedString::from(value.to_owned());
        let state = cx.new(|cx| InputState::new(window, cx).default_value(initial));
        let owned = key.to_owned();
        let subscription = cx.subscribe(&state, move |this, state, event: &InputEvent, cx| {
            match event {
                InputEvent::Change => {
                    let text = state.read(cx).value().to_string();
                    this.variable_values.insert(owned.clone(), text);
                }
                InputEvent::PressEnter { .. } => this.command("confirm-variables", cx),
                _ => {}
            }
        });
        self.variable_inputs.insert(key.to_owned(), (state.clone(), value.to_owned(), subscription));
        state
    }

    /// 结果表格上的右键：外壳据此打开表格菜单而不是编辑器菜单。
    pub fn take_grid_menu_request(&mut self) -> bool {
        std::mem::take(&mut self.grid_menu_requested)
    }
}

/// 非查询工作项激活时的统一说明。
pub const NOT_ACTIVE_QUERY: &str = "当前工作项不是查询标签；切回查询标签后再执行、格式化或取消。";

/// 补全、格式化、执行范围、计划、变量、取消和多语句结果都只作用于当前查询工作项。
/// 外壳和查询页共用这份清单，避免只挡住其中一层。
pub fn requires_active_query(id: &str) -> bool {
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
            | "confirm-variables"
            | "cancel-variables"
            | "open-sql-file"
            | "save-sql-file"
            | "save-query"
    ) || id.starts_with("statement:")
        || id.starts_with("database:")
        || id.starts_with("bind-connection:")
}

/// 目录里一个对象的完整目标；对象动作、工作项和覆盖层都从这里取连接/库/模式/对象。
pub fn object_target(object: &TreeObject, environment: &str) -> ObjectTarget {
    ObjectTarget::new(
        object.connection_id.clone(),
        object.connection_name.clone(),
        object.database.clone(),
        object.schema.clone(),
        object.name.clone(),
        object.engine.clone(),
        environment,
    )
}

pub fn object_spec(object: &TreeObject, environment: &str) -> String {
    object_target(object, environment).spec()
}

fn target_of(tab: &TabSnap) -> Option<Target> {
    let connection_id = tab.connection_id.clone()?;
    Some(Target {
        connection_id,
        connection_name: tab.connection_name.clone().unwrap_or_default(),
        database: tab.database.clone().unwrap_or_default(),
        engine: tab.engine.clone().unwrap_or_default(),
        schema: String::new(),
        environment: String::new(),
    })
}

fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("query runtime"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::test::TestWindowExt;

    fn sheet(sql: &str, truncated: bool, status: &str) -> desk::StatementView {
        desk::StatementView {
            sql: sql.to_owned(),
            status: status.to_owned(),
            message: String::new(),
            columns: vec!["id".to_owned()],
            rows: vec![vec![Some("1".to_owned())]],
            truncated,
            affected: None,
        }
    }

    #[test]
    fn load_more_only_offered_for_a_truncated_single_readonly_statement() {
        assert_eq!(
            load_more_plan(&[sheet("SELECT * FROM item", true, "ok")], 100),
            Some(("SELECT * FROM item".to_owned(), 101)),
            "被截断的 SELECT 可以继续读"
        );
        assert_eq!(load_more_plan(&[sheet("SELECT * FROM item", false, "ok")], 100), None, "没有截断就没有下一页");
        assert_eq!(load_more_plan(&[sheet("DELETE FROM item", true, "ok")], 100), None, "写语句不能因为加载更多被重放");
        assert_eq!(
            load_more_plan(&[sheet("SELECT 1", true, "ok"), sheet("SELECT 2", true, "ok")], 100),
            None,
            "多语句结果不提供整批重放"
        );
        assert_eq!(load_more_plan(&[sheet("SELECT * FROM item", true, "error")], 100), None, "失败结果不提供加载更多");
    }
    use gpui_kit::{Point, TestAppContext, WindowBounds, WindowOptions, px, size};

    fn open(cx: &mut TestAppContext) -> (gpui_kit::AnyWindowHandle, Entity<QueryPane>) {
        let dir = std::env::temp_dir().join(format!("rillsql-pane-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let services = Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            let bounds = gpui_kit::Bounds {
                origin: Point::default(),
                size: size(px(1100.), px(720.)),
            };
            let (window, pane) = gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                cx,
                |window, cx| cx.new(|cx| QueryPane::new(services, window, cx)),
            )
            .expect("query pane");
            (window, pane)
        })
    }

    #[gpui_kit::test]
    fn editor_and_grid_keep_drafts_and_show_results(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            let snap = TabSnap {
                id: "query-1".to_owned(),
                draft: String::new(),
                connection_id: Some("notes".to_owned()),
                connection_name: Some("笔记".to_owned()),
                database: Some("main".to_owned()),
                engine: Some("sqlite".to_owned()),
            };
            pane.update(cx, |pane, cx| {
                let _ = pane.sync(std::slice::from_ref(&snap), Some("query-1"), Locale::ZhCn, window, cx);
            });
            window.render_frame(cx);
            let loaded = TabSnap { draft: "SELECT loaded".to_owned(), ..snap.clone() };
            pane.update(cx, |pane, cx| {
                let effect = pane.sync(std::slice::from_ref(&loaded), Some("query-1"), Locale::En, window, cx);
                assert!(effect.running.is_empty());
                assert!(!pane.executed());
            });
            window.render_frame(cx);
            let text = pane.update(cx, |pane, cx| pane.editors.get("query-1").unwrap().read(cx).value(cx));
            assert!(text.contains("SELECT loaded"), "{text}");
            assert!(!pane.read(cx).executed());
            pane.update(cx, |pane, _| pane.pending_text = Some("select   1".to_owned()));
            window.render_frame(cx);
            pane.update(cx, |pane, cx| pane.command("format", cx));
            window.render_frame(cx);
            let formatted = pane.update(cx, |pane, cx| pane.editors.get("query-1").unwrap().read(cx).value(cx));
            assert!(formatted.starts_with("SELECT"), "{formatted}");
            pane.update(cx, |pane, _| pane.pending_text = Some("select 'abc".to_owned()));
            window.render_frame(cx);
            pane.update(cx, |pane, cx| pane.command("format", cx));
            window.render_frame(cx);
            let kept = pane.update(cx, |pane, cx| pane.editors.get("query-1").unwrap().read(cx).value(cx));
            assert_eq!(kept, "select 'abc");
            pane.update(cx, |pane, cx| {
                pane.show_sheet(vec!["label".to_owned()], vec![vec!["真实".to_owned()]], cx);
            });
        })
        .unwrap();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(pane.read(cx).cell(0, 0, cx), "真实");
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn query_result_keeps_the_connection_environment(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            let snap = TabSnap {
                id: "q".to_owned(),
                draft: "select 1".to_owned(),
                connection_id: Some("notes".to_owned()),
                connection_name: Some("笔记".to_owned()),
                database: Some("main".to_owned()),
                engine: Some("sqlite".to_owned()),
            };
            pane.update(cx, |pane, cx| {
                let _ = pane.sync(std::slice::from_ref(&snap), Some("q"), Locale::ZhCn, window, cx);
                pane.remember_environments(vec![("notes".to_owned(), "dev".to_owned())]);
                pane.show_sheet(vec!["label".to_owned()], vec![vec!["真实".to_owned()]], cx);
            });
            window.render_frame(cx);
            let banner = pane.read(cx).grid_banner(cx);
            assert!(banner.contains("dev"), "{banner}");
            assert!(banner.contains("笔记"), "{banner}");
            assert_eq!(pane.read(cx).cell(0, 0, cx), "真实");
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn workbench_opens_a_table_and_runs_a_real_select(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let dir = std::env::temp_dir().join(format!("rillsql-query-ui-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let database = dir.join("items.sqlite");
        let config_dir = dir.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        runtime().spawn(async move {
            let setup = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).create_if_missing(true))
                .await
                .unwrap();
            sqlx::query("CREATE TABLE item (id INTEGER PRIMARY KEY, label TEXT)").execute(&setup).await.unwrap();
            sqlx::query("INSERT INTO item (id, label) VALUES (1, '真实')").execute(&setup).await.unwrap();
            setup.close().await;
            let services = Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(config_dir.join("connections.sqlite")))));
            services.connections.open_sqlite_for_test("notes", "笔记", &database).await.unwrap();
            let _ = tx.send(services);
        });
        let services = rx.recv_timeout(std::time::Duration::from_secs(30)).expect("sqlite setup");
        cx.update(gpui_kit::init);
        let (window, view) = cx.update(|cx| {
            let bounds = gpui_kit::Bounds { origin: Point::default(), size: size(px(1100.), px(720.)) };
            let (window, view) = gpui_kit::open_window(
                WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
                cx,
                |window, cx| cx.new(|cx| crate::app::shell::Workbench::with_services(window, cx, services, false)),
            )
            .expect("workbench");
            (window, view)
        });
        cx.update_window(window, |_, _, cx| {
            view.update(cx, |view, cx| view.perform("refresh-tree", cx));
        })
        .unwrap();
        let saw_item = wait_for_tokio(cx, window, std::time::Duration::from_secs(30), |cx| {
            cx.update(|cx| view.read(cx).query_has_object("item", cx))
        });
        assert!(saw_item, "对象树没有列出 item：{}", cx.update(|cx| view.read(cx).query_note(cx)));
        cx.update_window(window, |_, window, cx| {
            view.update(cx, |view, cx| view.perform("object:notes|笔记|main||item|sqlite|", cx));
            window.render_frame(cx);
            view.update(cx, |view, cx| {
                let tab = view.model.active_tab().expect("bound query").clone();
                assert_eq!(tab.connection_id.as_deref(), Some("notes"));
                assert_eq!(tab.database.as_deref(), Some("main"));
                assert!(tab.draft.contains("item"), "{}", tab.draft);
                assert!(!tab.running);
                assert!(!view.query_executed(cx));
                view.perform("execute-query", cx);
                assert!(view.model.active_tab().unwrap().running, "执行没有标成运行中");
            });
        })
        .unwrap();
        let landed = wait_for_tokio(cx, window, std::time::Duration::from_secs(30), |cx| {
            cx.update(|cx| view.read(cx).query_cell(0, 1, cx) == "真实")
        });
        let note = cx.update(|cx| view.read(cx).query_note(cx));
        assert!(landed, "真实 SELECT 没有回到结果表：{note}");
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            view.update(cx, |view, _| {
                let tab = view.model.active_tab().expect("bound query").clone();
                assert_eq!(tab.last_executed_sql.as_deref(), Some(tab.draft.as_str()), "执行过的 SQL 没有记到标签上");
                assert!(view.model.allow_system_close(), "执行过的查询仍被当成未执行草稿，关闭按钮会被拦下");
            });
        })
        .unwrap();
        let runs = cx.update(|cx| view.read(cx).query_runs(cx));
        cx.update_window(window, |_, window, cx| {
            view.update(cx, |view, _| view.model.tabs[0].draft = "SELECT 'only-draft'".into());
            window.render_frame(cx);
            view.update(cx, |view, cx| {
                assert!(!view.query_executed(cx));
                assert_eq!(view.query_runs(cx), runs);
                assert_eq!(view.query_draft(cx), "SELECT 'only-draft'");
            });
        })
        .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 目录和查询结果在 tokio 上完成，再从工作线程排回这个测试调度器。
    /// 固定次数的 `run_until_parked` 会在队列仍空时返回。
    fn wait_for_tokio(
        cx: &mut TestAppContext,
        window: gpui_kit::AnyWindowHandle,
        timeout: std::time::Duration,
        mut ready: impl FnMut(&mut TestAppContext) -> bool,
    ) -> bool {
        let started = std::time::Instant::now();
        loop {
            cx.run_until_parked();
            cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
            if ready(cx) {
                return true;
            }
            if started.elapsed() >= timeout {
                return false;
            }
            std::thread::park_timeout(std::time::Duration::from_millis(25));
        }
    }

    fn bound(id: &str, draft: &str) -> TabSnap {
        TabSnap {
            id: id.to_owned(),
            draft: draft.to_owned(),
            connection_id: Some("notes".to_owned()),
            connection_name: Some("笔记".to_owned()),
            database: Some("main".to_owned()),
            engine: Some("sqlite".to_owned()),
        }
    }

    #[gpui_kit::test]
    fn switching_tabs_keeps_each_result(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        let a = bound("a", "select a");
        let b = bound("b", "select b");
        cx.update_window(window, |_, window, cx| {
            pane.update(cx, |pane, cx| {
                let _ = pane.sync(std::slice::from_ref(&a), Some("a"), Locale::ZhCn, window, cx);
                pane.show_sheet(vec!["label".to_owned()], vec![vec!["真实".to_owned()]], cx);
            });
            window.render_frame(cx);
            assert_eq!(pane.read(cx).cell(0, 0, cx), "真实");
            pane.update(cx, |pane, cx| {
                let _ = pane.sync(&[a.clone(), b.clone()], Some("b"), Locale::ZhCn, window, cx);
            });
            window.render_frame(cx);
            assert_ne!(pane.read(cx).cell(0, 0, cx), "真实");
            pane.update(cx, |pane, _| pane.desk.set_note("a", "迟到失败"));
            window.render_frame(cx);
            assert_ne!(pane.read(cx).note(), "迟到失败");
            pane.update(cx, |pane, cx| {
                let _ = pane.sync(&[a.clone(), b.clone()], Some("a"), Locale::ZhCn, window, cx);
            });
            window.render_frame(cx);
            assert_eq!(pane.read(cx).cell(0, 0, cx), "真实");
            assert_eq!(pane.read(cx).note(), "迟到失败");
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn execution_and_format_refuse_when_another_work_item_is_active(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        let snap = bound("q", "select   1");
        cx.update_window(window, |_, window, cx| {
            pane.update(cx, |pane, cx| {
                let _ = pane.sync(std::slice::from_ref(&snap), Some("table-1"), Locale::ZhCn, window, cx);
                assert!(!pane.context_active, "表工作项激活时查询上下文必须关闭");
                pane.command("execute-query", cx);
                assert_eq!(pane.run_count(), 0, "不得在隐藏查询上执行");
                assert!(!pane.running());
                assert!(pane.note().contains("不是查询"), "{}", pane.note());
                pane.command("format", cx);
                assert_eq!(pane.desk.draft(), "select   1", "不得格式化隐藏查询");
                pane.command("run-current", cx);
                assert_eq!(pane.run_count(), 0);
                pane.command("request-cancel", cx);
                assert_eq!(pane.desk.status(), "");
            });
            window.render_frame(cx);
            assert_eq!(pane.read(cx).editor_text(cx), "select   1");
        })
        .unwrap();

        cx.update_window(window, |_, window, cx| {
            pane.update(cx, |pane, cx| {
                let _ = pane.sync(std::slice::from_ref(&snap), Some("q"), Locale::ZhCn, window, cx);
                assert!(pane.context_active);
                pane.command("format", cx);
                assert!(pane.desk.draft().starts_with("SELECT"), "{}", pane.desk.draft());
            });
            window.render_frame(cx);
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn draft_installs_fold_candidates(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        let snap = bound("fold", "select 1\nfrom item;\n");
        cx.update_window(window, |_, window, cx| {
            pane.update(cx, |pane, cx| {
                let _ = pane.sync(std::slice::from_ref(&snap), Some("fold"), Locale::ZhCn, window, cx);
                assert!(pane.pending_text.is_none());
            });
            window.render_frame(cx);
            window.render_frame(cx);
            let folded = pane.update(cx, |pane, cx| pane.editors.get("fold").unwrap().read(cx).fold_candidate(0, cx));
            assert!(folded, "草稿应用到编辑器后没有折叠候选");
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn completion_inserts_snippet_text_and_lists_object_names(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        let snap = bound("q", "ord");
        cx.update_window(window, |_, window, cx| {
            pane.update(cx, |pane, cx| {
                let _ = pane.sync(std::slice::from_ref(&snap), Some("q"), Locale::ZhCn, window, cx);
                pane.desk.set_objects(
                    vec![TreeObject {
                        connection_id: "notes".to_owned(),
                        connection_name: "笔记".to_owned(),
                        engine: "sqlite".to_owned(),
                        database: "main".to_owned(),
                        schema: String::new(),
                        name: "orders".to_owned(),
                        kind: "table".to_owned(),
                    }],
                    vec!["main".to_owned()],
                );
                let editor = pane.editors.get("q").unwrap().clone();
                editor.update(cx, |editor, cx| editor.select_range(3..3, cx));
                pane.command("autocomplete", cx);
            });
            window.render_frame(cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| {
            let (labels, insert) = pane.update(cx, |pane, cx| {
                let editor = pane.editors.get("q").unwrap().clone();
                (editor.read(cx).completion_labels(cx), editor.read(cx).completion_insert("orders", cx))
            });
            assert!(labels.iter().any(|label| label == "orders"), "{labels:?}");
            assert_eq!(insert.as_deref(), Some("orders"));
            pane.update(cx, |pane, cx| {
                let editor = pane.editors.get("q").unwrap().clone();
                editor.update(cx, |editor, cx| {
                    editor.set_sql("select", window, cx);
                    editor.select_range(6..6, cx);
                });
                pane.command("autocomplete", cx);
            });
            window.render_frame(cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| {
            pane.update(cx, |pane, cx| {
                let editor = pane.editors.get("q").unwrap().clone();
                assert_eq!(editor.read(cx).completion_insert("select-from", cx).as_deref(), Some("SELECT  FROM "));
                let version = editor.read(cx).version();
                editor.update(cx, |editor, cx| {
                    assert!(editor.apply_completion_insert(version, "select-from", "SELECT  FROM ", window, cx));
                });
                let text = editor.read(cx).value(cx);
                assert!(text.contains("SELECT  FROM "), "{text}");
                assert!(!text.contains("select-from"), "{text}");
            });
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn opening_or_activating_a_query_focuses_its_editor(cx: &mut TestAppContext) {
        let dir = std::env::temp_dir().join(format!("rillsql-focus-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let services = Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
        cx.update(gpui_kit::init);
        let (window, view) = cx.update(|cx| {
            let bounds = gpui_kit::Bounds { origin: Point::default(), size: size(px(1100.), px(720.)) };
            let (window, view) = gpui_kit::open_window(
                WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
                cx,
                |window, cx| cx.new(|cx| crate::app::shell::Workbench::with_services(window, cx, services, false)),
            )
            .expect("workbench");
            (window, view)
        });
        cx.update_window(window, |_, window, cx| {
            view.update(cx, |view, cx| view.perform("new-query", cx));
            window.render_frame(cx);
            view.update(cx, |view, cx| assert!(view.query_editor_focused(window, cx), "新建查询没有把焦点交给编辑器"));
            let first = view.read(cx).model.active_tab.clone().unwrap();
            view.update(cx, |view, cx| view.perform("new-query", cx));
            window.render_frame(cx);
            view.update(cx, |view, cx| {
                assert_ne!(view.model.active_tab.as_deref(), Some(first.as_str()));
                assert!(view.query_editor_focused(window, cx), "新标签的编辑器没有获得焦点");
                view.model.active_tab = Some(first);
            });
            window.render_frame(cx);
            view.update(cx, |view, cx| assert!(view.query_editor_focused(window, cx), "切回的查询编辑器没有获得焦点"));
        })
        .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[gpui_kit::test]
    fn long_select_keeps_polling_past_ten_seconds(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let dir = std::env::temp_dir().join(format!("rillsql-slow-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let database = dir.join("slow.sqlite");
        let config_dir = dir.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        runtime().spawn(async move {
            let setup = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).create_if_missing(true))
                .await
                .unwrap();
            sqlx::query("CREATE TABLE item (id INTEGER PRIMARY KEY)").execute(&setup).await.unwrap();
            setup.close().await;
            let services = Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(config_dir.join("connections.sqlite")))));
            services.connections.open_sqlite_for_test("notes", "笔记", &database).await.unwrap();
            let _ = tx.send(services);
        });
        let services = rx.recv_timeout(std::time::Duration::from_secs(30)).expect("sqlite setup");
        cx.update(gpui_kit::init);
        let (window, pane) = cx.update(|cx| {
            let bounds = gpui_kit::Bounds { origin: Point::default(), size: size(px(1100.), px(720.)) };
            let (window, pane) = gpui_kit::open_window(
                WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
                cx,
                |window, cx| cx.new(|cx| QueryPane::new(services, window, cx)),
            )
            .expect("query pane");
            (window, pane)
        });
        let sql = "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c WHERE x < 36000) SELECT count(*) FROM c a, c b";
        let snap = bound("slow", sql);
        cx.update_window(window, |_, window, cx| {
            pane.update(cx, |pane, cx| {
                pane.set_query_timeout_sec(15);
                let _ = pane.sync(std::slice::from_ref(&snap), Some("slow"), Locale::ZhCn, window, cx);
                pane.command("execute-query", cx);
                assert_eq!(pane.last_timeout(), Some(15_000));
                assert!(pane.running());
            });
        })
        .unwrap();
        let started = std::time::Instant::now();
        let mut landed = false;
        let mut note = String::new();
        let mut polls = 0u32;
        while started.elapsed() < std::time::Duration::from_secs(60) {
            cx.run_until_parked();
            let _ = cx.update_window(window, |_, window, cx| window.render_frame(cx));
            landed = cx.update(|cx| pane.read(cx).cell(0, 0, cx) == "1296000000");
            note = cx.update(|cx| pane.read(cx).note());
            polls = cx.update(|cx| pane.read(cx).polls());
            if landed {
                break;
            }
            if note == "执行没有结束" {
                break;
            }
        }
        assert_ne!(note, "执行没有结束");
        assert!(landed, "超过 10 秒的 SELECT 没有回到结果：{note} polls={polls}");
        assert!(polls > 400, "轮询次数 {polls} 没有越过原来的 400 次上限");
        assert!(!cx.update(|cx| pane.read(cx).running()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[gpui_kit::test]
    fn public_settings_reach_the_editor_results_and_object_tree(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        let snap = TabSnap {
            id: "query-1".to_owned(),
            draft: "select 1".to_owned(),
            connection_id: Some("notes".to_owned()),
            connection_name: Some("笔记".to_owned()),
            database: Some("main".to_owned()),
            engine: Some("sqlite".to_owned()),
        };
        cx.update_window(window, |_, window, cx| {
            pane.update(cx, |pane, cx| {
                let _ = pane.sync(std::slice::from_ref(&snap), Some("query-1"), Locale::ZhCn, window, cx);
            });
            window.render_frame(cx);
        })
        .unwrap();

        let settings = crate::state::ShellSettings {
            editor_font: 18,
            editor_theme: "warm".to_owned(),
            page_size: 250,
            preview_kb: 4,
            object_page: 50,
            null_copy: "N".to_owned(),
            ..Default::default()
        };
        cx.update(|cx| {
            pane.update(cx, |pane, cx| pane.apply_public_settings(&settings, cx));
        });
        cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert_eq!(pane.editor_font(), 18);
            assert_eq!(pane.editor_theme(), "warm");
            assert_eq!(pane.page_size(), 250);
            assert_eq!(pane.desk.page_size(), 250, "查询读取行数必须来自 page_size");
            assert_eq!(pane.preview_kb(), 4);
            assert_eq!(pane.object_page(), 50);
            assert_eq!(pane.null_copy(), "N");
            let editor = pane.editors.get("query-1").expect("已建编辑器").read(cx);
            assert_eq!(editor.font_size(), 18, "已有编辑器立即改字号");
            assert_eq!(editor.theme(), "warm", "已有编辑器立即改配色");
        });
    }
}
