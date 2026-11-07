//! GPUI 查询工作台。编辑器复用 SqlEditor，结果复用 DataTable。
//! 方言规则在 query::assist / query::desk；这里只提交任务和显示。

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use gpui_kit::component::table::{Column, DataTable, TableDelegate, TableState};
use gpui_kit::{
    App, AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, Pixels, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};

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
}

pub struct QueryPane {
    desk: Desk,
    services: Arc<AppServices>,
    editors: HashMap<String, Entity<SqlEditor>>,
    links: HashMap<String, DraftLink>,
    grid: Entity<TableState<ResultDelegate>>,
    locale: Locale,
    note: String,
    statement: usize,
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
}

impl QueryPane {
    pub fn new(services: Arc<AppServices>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let grid = cx.new(|cx| {
            TableState::new(ResultDelegate::default(), window, cx)
                .cell_selectable(true)
                .row_selectable(true)
                .col_resizable(true)
                .row_header(true)
        });
        Self {
            desk: Desk::new(),
            services,
            editors: HashMap::new(),
            links: HashMap::new(),
            grid,
            locale: Locale::ZhCn,
            note: String::new(),
            statement: 0,
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
        }
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

    pub fn note(&self) -> &str {
        &self.note
    }

    pub fn draft(&self) -> String {
        self.desk.draft()
    }

    pub fn cell(&self, row: usize, col: usize, cx: &App) -> String {
        self.grid.read(cx).delegate().cell(row, col)
    }

    pub fn command(&mut self, id: &str, cx: &mut Context<Self>) {
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
                self.queue_object(&object);
            }
            cx.notify();
            return;
        }
        if let Some(index) = id.strip_prefix("statement:") {
            if let Ok(index) = index.parse::<usize>() {
                self.statement = index;
                self.refresh_grid(cx);
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
        self.grid.update(cx, |state, cx| {
            state.delegate_mut().columns = columns;
            state.delegate_mut().rows = rows;
            state.refresh(cx);
        });
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
                self.note = i18n::text(self.locale, "query.variables.title");
                cx.notify();
            }
            BeginRun::Rejected(reason) => {
                self.note = reason;
                cx.notify();
            }
        }
    }

    fn spawn_run(&mut self, prepared: PreparedRun, cx: &mut Context<Self>) {
        let ticket = Ticket { tab_id: prepared.tab_id.clone(), token: prepared.token, version: prepared.version };
        let services = Arc::clone(&self.services);
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
                    timeout_ms: None,
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
            for _ in 0..400 {
                match crate::query::commands::poll_run(&services.query, &started.id, false).await {
                    Ok(snap) if !matches!(snap.status.as_str(), "running" | "cancel-requested") => {
                        let _ = done_tx.send(Ok(snap));
                        return;
                    }
                    Ok(_) => tokio::time::sleep(std::time::Duration::from_millis(25)).await,
                    Err(error) => {
                        let _ = done_tx.send(Err(error));
                        return;
                    }
                }
            }
            let _ = done_tx.send(Err("执行没有结束".to_owned()));
        });
        let tab_id = ticket.tab_id.clone();
        let view = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            if let Ok(run_id) = id_rx.await {
                let _ = cx.update(|cx| {
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
                let _ = cx.update(|cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |pane, cx| {
                            match result {
                                Ok(run) => {
                                    if !pane.desk.accept_run(&pane.services.tasks, &ticket, run) && pane.note.is_empty() {
                                        pane.note = i18n::text(pane.locale, "query.run.cancel");
                                    }
                                }
                                Err(error) => pane.note = error,
                            }
                            pane.refresh_grid(cx);
                            cx.notify();
                        });
                    }
                });
            }
        })
        .detach();
        self.note = i18n::text(self.locale, "query.status.running");
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
        self.note = i18n::text(self.locale, "query.run.cancel");
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
            self.note.clear();
        } else if let Some(key) = formatted.reason_key {
            self.note = i18n::text(self.locale, key);
        }
        cx.notify();
    }

    fn expand(&mut self, cx: &mut Context<Self>) {
        let expanded = self.desk.expand_active();
        if expanded.ok {
            self.pending_text = Some(expanded.sql);
        } else if let Some(key) = expanded.reason_key {
            self.note = i18n::text(self.locale, key);
        }
        cx.notify();
    }

    fn explain(&mut self, cx: &mut Context<Self>) {
        match self.desk.explain_active() {
            Ok(sql) => self.start(RunScope::Script, Some(sql), cx),
            Err(reason) => {
                self.note = reason;
                cx.notify();
            }
        }
    }

    fn complete(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.desk.active_id().map(str::to_owned) else {
            return;
        };
        let items = self.desk.completion_for("");
        let labels: Vec<String> = items.iter().map(|item| item.label.clone()).collect();
        if let Some(ticket) = self.desk.issue(&self.services.tasks) {
            let _ = self.desk.accept_completion(&self.services.tasks, &ticket, items);
        }
        if let Some(editor) = self.editors.get(&id) {
            editor.update(cx, |editor, _| editor.set_extra_suggestions(labels));
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
                            Ok((objects, databases, columns)) => {
                                pane.desk.set_objects(objects, databases);
                                for (key, cols) in columns {
                                    let mut parts = key.split('\u{1}');
                                    let connection = parts.next().unwrap_or("");
                                    let database = parts.next().unwrap_or("");
                                    let table = parts.next().unwrap_or("");
                                    pane.desk.remember_columns(connection, database, table, cols);
                                }
                                pane.note.clear();
                            }
                            Err(error) => pane.note = error,
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
                    let loaded = assist::read_sql_file(&path);
                    let _ = cx.update(|cx| {
                        let _ = this.update(cx, |pane, cx| pane.finish_open(loaded, cx));
                    });
                }
            }
        })
        .detach();
    }

    fn finish_open(&mut self, loaded: Result<assist::FileLoad, String>, cx: &mut Context<Self>) {
        match loaded {
            Ok(file) => {
                if file.execute {
                    self.note = i18n::text(self.locale, "history.page.loadOnlySql");
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
                self.note = i18n::text(self.locale, "history.page.loadOnlySql");
            }
            Err(error) => self.note = error,
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
                let wrote = assist::write_sql_file(&path, &sql);
                let _ = cx.update(|cx| {
                    this.update(cx, |pane, cx| {
                        pane.note = match wrote {
                            Ok(()) => i18n::text(pane.locale, "query.fileIo.saved"),
                            Err(error) => error,
                        };
                        cx.notify();
                    })
                });
            }
        })
        .detach();
    }

    fn refresh_grid(&mut self, cx: &mut App) {
        let results = self.desk.results();
        self.statement = self.statement.min(results.len().saturating_sub(1));
        let Some(sheet) = results.get(self.statement) else {
            self.grid.update(cx, |state, cx| {
                state.delegate_mut().columns.clear();
                state.delegate_mut().rows.clear();
                state.refresh(cx);
            });
            return;
        };
        let columns = if sheet.columns.is_empty() { vec!["status".to_owned()] } else { sheet.columns.clone() };
        let rows = if sheet.columns.is_empty() {
            vec![vec![sheet.message.clone()]]
        } else {
            sheet
                .rows
                .iter()
                .map(|row| row.iter().map(|cell| cell.clone().unwrap_or_else(|| "NULL".to_owned())).collect())
                .collect()
        };
        self.grid.update(cx, |state, cx| {
            state.delegate_mut().columns = columns;
            state.delegate_mut().rows = rows;
            state.refresh(cx);
        });
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
                if let Some(editor) = self.editors.get(id).cloned() {
                    let ranges = assist::fold_line_ranges(&sql, self.desk.target().map(|target| target.engine).as_deref().unwrap_or("sqlite"));
                    editor.update(cx, |editor, cx| {
                        let _ = editor.apply_folds(&ranges, cx);
                    });
                }
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
}

impl Render for QueryPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.apply_pending(window, cx);
        let locale = self.locale;
        let label = |key| SharedString::from(i18n::text(locale, key));
        let objects = self.desk.objects().to_vec();
        let databases = self.desk.databases().to_vec();
        let outline = self.desk.outline();
        let results = self.desk.results();
        let variables = self.desk.variables();
        let note = self.note.clone();
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
        if objects.is_empty() {
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
            column = column.child(
                div()
                    .id("query-results")
                    .h(px(220.))
                    .child(DataTable::new(&grid).accessibility_label("query-results")),
            );
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
                        pane.queue_object(&object);
                        cx.notify();
                    }
                    return;
                }
                pane.command(&command, cx);
            });
        })
}

fn target_of(tab: &TabSnap) -> Option<Target> {
    let connection_id = tab.connection_id.clone()?;
    Some(Target {
        connection_id,
        connection_name: tab.connection_name.clone().unwrap_or_default(),
        database: tab.database.clone().unwrap_or_default(),
        engine: tab.engine.clone().unwrap_or_default(),
        schema: String::new(),
    })
}

fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("query runtime"))
}

struct ResultDelegate {
    columns: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl Default for ResultDelegate {
    fn default() -> Self {
        Self { columns: Vec::new(), rows: Vec::new() }
    }
}

impl ResultDelegate {
    fn cell(&self, row: usize, col: usize) -> String {
        self.rows.get(row).and_then(|line| line.get(col)).cloned().unwrap_or_default()
    }
}

impl TableDelegate for ResultDelegate {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len().max(1)
    }

    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        let name = self.columns.get(col_ix).cloned().unwrap_or_else(|| " ".to_owned());
        Column::new(format!("c{col_ix}"), name).width(px(120.)).resizable(true)
    }

    fn render_td(&mut self, row_ix: usize, col_ix: usize, _: &mut Window, _: &mut Context<TableState<Self>>) -> impl IntoElement {
        div().child(SharedString::from(self.cell(row_ix, col_ix)))
    }

    fn cell_text(&self, row_ix: usize, col_ix: usize, _: &App) -> String {
        self.cell(row_ix, col_ix)
    }

    fn cell_label(&self, row_ix: usize, col_ix: usize, _: &App) -> Option<SharedString> {
        Some(SharedString::from(self.cell(row_ix, col_ix)))
    }

    fn fit_column(&mut self, _: usize, _: &mut Window, _: &mut Context<TableState<Self>>) -> Option<Pixels> {
        Some(px(160.))
    }
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
            (window.into(), pane)
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
            (window.into(), view)
        });
        cx.update_window(window, |_, _, cx| {
            view.update(cx, |view, cx| view.perform("refresh-tree", cx));
        })
        .unwrap();
        let mut saw_item = false;
        for _ in 0..40 {
            cx.run_until_parked();
            saw_item = cx.update(|cx| view.read(cx).query_has_object("item", cx));
            if saw_item {
                break;
            }
        }
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
        let mut landed = false;
        let mut note = String::new();
        for _ in 0..50 {
            cx.run_until_parked();
            cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
            landed = cx.update(|cx| view.read(cx).query_cell(0, 1, cx) == "真实");
            note = cx.update(|cx| view.read(cx).query_note(cx));
            if landed {
                break;
            }
        }
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
}
