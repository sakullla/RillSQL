//! GPUI 查询工作台。编辑器复用 SqlEditor，结果复用 DataTable。
//! 方言规则在 query::assist / query::desk；这里只提交任务和显示。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};

use gpui_kit::{
    App, AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};

use crate::app::views::grid::{GridPane, GridTarget};
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
    variable_focus: Option<String>,
    focus: FocusHandle,
    outline_open: bool,
    pending_completion: bool,
    pending_lint: Option<Vec<LintFinding>>,
    lint_seen: Option<(String, u64)>,
    timeout_ms: Option<u64>,
    polls: Arc<AtomicU32>,
    #[cfg(test)]
    last_timeout: Option<u64>,
    focus_editor: bool,
    seen_active: Option<String>,
    fold_key: Option<(String, String)>,
    fold_wait: Option<(String, String)>,
    painted: Option<(Option<String>, u64, usize)>,
    selected_object: Option<String>,
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
            variable_focus: None,
            focus: cx.focus_handle(),
            outline_open: true,
            pending_completion: false,
            pending_lint: None,
            lint_seen: None,
            timeout_ms: None,
            polls: Arc::new(AtomicU32::new(0)),
            #[cfg(test)]
            last_timeout: None,
            focus_editor: false,
            seen_active: None,
            fold_key: None,
            fold_wait: None,
            painted: None,
            selected_object: None,
        }
    }

    pub fn remember_environments(&mut self, pairs: Vec<(String, String)>) {
        self.desk.remember_environments(pairs);
    }

    pub fn set_query_timeout_sec(&mut self, seconds: i64) {
        self.timeout_ms = if seconds > 0 { Some(seconds as u64 * 1000) } else { None };
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

    pub fn command(&mut self, id: &str, cx: &mut Context<Self>) {
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
            if let Some(object) = self
                .desk
                .objects()
                .iter()
                .find(|object| format!("{}:{}", object.connection_id, object.name) == rest)
                .cloned()
            {
                let spec = object_spec(&object, &self.desk.environment_of(&object.connection_id));
                self.selected_object = Some(spec);
                self.queue_object(&object);
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
        let effect = self.prepare(tabs, active, locale, cx);
        self.ensure_editors(window, cx);
        effect
    }

    /// 把外壳标签和编辑器草稿对齐。不创建编辑器，所以命令层没有 Window 也能调用。
    pub fn prepare(&mut self, tabs: &[TabSnap], active: Option<&str>, locale: Locale, cx: &App) -> SyncEffect {
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
        SyncEffect {
            drafts,
            running: self.desk.running_ids(),
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
            let editor = cx.new(|cx| SqlEditor::new(window, cx));
            editor.update(cx, |editor, cx| editor.set_sql(&sql, window, cx));
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

    fn type_variable(&mut self, key: &str, text: &str) {
        let slot = self.variable_values.entry(key.to_owned()).or_default();
        if text == "backspace" {
            slot.pop();
        } else {
            slot.push_str(text);
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
        self.desk.set_active_note(&i18n::text(self.locale, "query.status.running"));
        cx.notify();
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
        let (tx, rx) = tokio::sync::oneshot::channel();
        runtime().spawn(async move {
            let _ = tx.send(desk::load_tree(&services.connections).await);
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
                            Err(error) => pane.desk.set_active_note(&error),
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
        let locale = self.locale;
        let label = |key| SharedString::from(i18n::text(locale, key));
        let objects = self.desk.objects().to_vec();
        let links = self.desk.links().to_vec();
        let databases = self.desk.databases().to_vec();
        let outline = self.desk.outline();
        let results = self.desk.results();
        let variables = self.desk.variables();
        let note = self.desk.active_note();
        let editor = self.desk.active_id().and_then(|id| self.editors.get(id)).cloned();
        let grid = self.grid.clone();
        let mut column = div().id("query-pane").flex().flex_col().size_full().gap(px(4.));
        column = column.child(
            div()
                .id("query-toolbar")
                .flex()
                .gap(px(4.))
                .child(action_button("run-script", label("query.run.runScript"), cx))
                .child(action_button("request-cancel", label("shell.shortcuts.requestCancel"), cx))
                .child(action_button("format", label("query.editor.menu.format"), cx))
                .child(action_button("autocomplete", label("shell.shortcuts.autocomplete"), cx))
                .child(action_button("explain", label("query.editor.menu.plan"), cx))
                .child(action_button("expand-star", label("query.complete.expandStar"), cx))
                .child(action_button("toggle-outline", label("query.outline.heading"), cx))
                .child(action_button("open-sql-file", label("query.fileIo.open"), cx))
                .child(action_button("save-sql-file", label("query.fileIo.save"), cx))
                .child(action_button("refresh-tree", label("query.tree.refresh"), cx)),
        );
        if !links.is_empty() {
            let mut row = div().id("query-connections").flex().gap(px(4.));
            for link in links {
                let id = link.id.clone();
                let name = if link.name.is_empty() { id.clone() } else { link.name.clone() };
                row = row.child(action_button(&format!("bind-connection:{id}"), SharedString::from(name), cx));
            }
            column = column.child(row);
        }
        if !databases.is_empty() {
            let mut row = div().id("query-databases").flex().gap(px(4.));
            for database in databases {
                let name = database.clone();
                row = row.child(action_button(&format!("database:{name}"), SharedString::from(name), cx));
            }
            column = column.child(row);
        }
        let mut body = div().id("query-body").flex().flex_1().gap(px(4.));
        let mut tree = div().id("query-tree").flex().flex_col().w(px(180.));
        let tree_error = self.desk.tree_error().to_owned();
        if !tree_error.is_empty() {
            tree = tree.child(SharedString::from(tree_error));
        } else if objects.is_empty() {
            tree = tree.child(label("query.status.noObjects"));
        }
        for object in &objects {
            let key = format!("object:{}:{}", object.connection_id, object.name);
            tree = tree.child(action_button(&key, SharedString::from(object.name.clone()), cx));
        }
        body = body.child(tree);
        if let Some(editor) = editor {
            body = body.child(div().id("query-editor").flex_1().child(editor));
        } else {
            body = body.child(div().id("query-editor-empty").flex_1().child(label("shell.statusbar.noTarget")));
        }
        if self.outline_open {
            let mut list = div().id("query-outline").flex().flex_col().w(px(220.)).child(label("query.outline.heading"));
            for item in outline {
                list = list.child(SharedString::from(format!("{}. {}", item.index, item.preview)));
            }
            body = body.child(list);
        }
        column = column.child(body);
        if !variables.is_empty() {
            let mut row = div().id("query-variables").flex().gap(px(4.)).child(label("query.variables.title"));
            for field in &variables {
                let value = self.variable_values.get(&field.key).cloned().unwrap_or_default();
                row = row.child(variable_box(field.key.clone(), field.label.clone(), value, self.focus.clone(), cx));
            }
            row = row.child(action_button("confirm-variables", label("query.variables.proceed"), cx));
            row = row.child(action_button("cancel-variables", label("query.variables.cancel"), cx));
            column = column.child(row);
        }
        if results.len() > 1 {
            let mut row = div().id("query-statements").flex().gap(px(4.));
            for index in 0..results.len() {
                let caption = SharedString::from(format!("{} {}", i18n::text(locale, "query.results.statement"), index + 1));
                row = row.child(action_button(&format!("statement:{index}"), caption, cx));
            }
            column = column.child(row);
        }
        if !note.is_empty() {
            column = column.child(SharedString::from(note));
        }
        if results.is_empty() {
            column = column.child(label("query.status.empty"));
        } else {
            column = column.child(div().id("query-results").h(px(220.)).child(grid));
        }
        column
    }
}

fn variable_box(key: String, name: String, value: String, focus: FocusHandle, cx: &mut Context<QueryPane>) -> impl IntoElement {
    let label = SharedString::from(format!("{name} {value}"));
    let entity = cx.entity();
    let typed = key.clone();
    div()
        .id(SharedString::from(format!("query-var-{key}")))
        .track_focus(&focus)
        .role(gpui_kit::Role::TextInput)
        .aria_label(label.clone())
        .child(label)
        .on_mouse_down(MouseButton::Left, {
            let entity = entity.clone();
            let focus = focus.clone();
            let key = key.clone();
            move |_, window, cx| {
                window.focus(&focus, cx);
                entity.update(cx, |pane, cx| {
                    pane.variable_focus = Some(key.clone());
                    cx.notify();
                });
            }
        })
        .on_key_down(move |event: &gpui_kit::KeyDownEvent, _, cx| {
            let Some(text) = typed_text(event) else { return };
            let typed = typed.clone();
            entity.update(cx, |pane, cx| {
                if pane.variable_focus.as_deref() == Some(typed.as_str()) {
                    pane.type_variable(&typed, &text);
                    cx.notify();
                }
            });
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

fn action_button(id: &str, label: SharedString, cx: &mut Context<QueryPane>) -> impl IntoElement {
    let command = id.to_owned();
    let entity = cx.entity();
    div()
        .id(SharedString::from(format!("query-act-{}", id.replace(':', "-"))))
        .role(gpui_kit::Role::Button)
        .aria_label(label.clone())
        .child(label)
        .on_click(move |_, _, cx| {
            let command = command.clone();
            entity.update(cx, |pane, cx| {
                if let Some(database) = command.strip_prefix("database:") {
                    pane.queue_database(database);
                    cx.notify();
                    return;
                }
                if let Some(rest) = command.strip_prefix("object:") {
                    if let Some(object) = pane.desk.objects().iter().find(|object| format!("{}:{}", object.connection_id, object.name) == rest).cloned() {
                        let spec = object_spec(&object, &pane.desk.environment_of(&object.connection_id));
                        pane.selected_object = Some(spec);
                        pane.queue_object(&object);
                        cx.notify();
                    }
                    return;
                }
                pane.command(&command, cx);
            });
        })
}

fn object_spec(object: &TreeObject, environment: &str) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}|{}",
        object.connection_id, object.connection_name, object.database, object.schema, object.name, object.engine, environment
    )
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
            view.update(cx, |view, cx| view.perform("object:notes:item", cx));
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
}
