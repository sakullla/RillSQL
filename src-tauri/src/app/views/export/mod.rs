//! 导入、导出、全库导出、跨库迁移和任务中心。确认前不写库，取消后不再显示运行中。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, PathPromptOptions, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};

use crate::application::AppServices;
use crate::export::{
    commit_import_guarded, export_full, export_loaded, open_import, prefer_cancel, watch_dump, watch_migrate, CancelPoint, ImportEntry, ImportPreview, LoadedExport, Phase, TaskLog,
    TaskView,
};
use crate::grid::commands::{ExportRequest, TableRef};
use crate::i18n::{self, Locale};
use crate::dump::dump::DumpRequest;
use crate::migrate::migrate::{ColumnPair, MigrateRequest, MigrateTableRef};

pub struct ExchangePane {
    services: Arc<AppServices>,
    locale: Locale,
    pub open: bool,
    log: TaskLog,
    preview: Option<ImportPreview>,
    mapping: BTreeMap<String, String>,
    mapping_edited: bool,
    mapping_attempted: bool,
    confirmed: bool,
    table: Option<TableRef>,
    loaded_columns: Vec<String>,
    loaded_rows: Vec<Vec<Option<String>>>,
    loaded_truncated: bool,
    format_name: String,
    export_request: Option<ExportRequest>,
    loaded: Option<crate::export::LoadedExport>,
    dump_request: Option<DumpRequest>,
    migrate_request: Option<MigrateRequest>,
    pub notice: String,
}

impl ExchangePane {
    pub fn new(services: Arc<AppServices>) -> Self {
        Self {
            services,
            locale: Locale::ZhCn,
            open: false,
            log: TaskLog::default(),
            preview: None,
            mapping: BTreeMap::new(),
            mapping_edited: false,
            mapping_attempted: false,
            confirmed: false,
            table: None,
            loaded_columns: Vec::new(),
            loaded_rows: Vec::new(),
            loaded_truncated: false,
            format_name: "csv".to_owned(),
            export_request: None,
            loaded: None,
            dump_request: None,
            migrate_request: None,
            notice: String::new(),
        }
    }

    pub fn set_locale(&mut self, locale: Locale) {
        self.locale = locale;
    }

    pub fn running(&self) -> bool {
        self.log.running()
    }

    pub fn tasks(&self) -> &[TaskView] {
        &self.log.tasks
    }

    pub fn stage_table(&mut self, table: TableRef) {
        self.table = Some(table);
    }

    pub fn stage_export(&mut self, request: ExportRequest) {
        self.export_request = Some(request);
    }

    pub fn stage_loaded(&mut self, loaded: crate::export::LoadedExport) {
        self.loaded = Some(loaded);
    }

    pub fn stage_dump(&mut self, request: DumpRequest) {
        self.dump_request = Some(request);
    }

    pub fn stage_migrate(&mut self, request: MigrateRequest) {
        self.migrate_request = Some(request);
    }

    /// 当前网格或已连接目标。每次工作台命令都按它重绑，空目标会清掉旧表。
    pub fn bind_source(&mut self, table: Option<TableRef>, columns: Vec<String>, rows: Vec<Vec<Option<String>>>, truncated: bool) {
        let changed = match (&self.table, &table) {
            (Some(old), Some(new)) => {
                old.connection_id != new.connection_id || old.database != new.database || old.schema != new.schema || old.table != new.table
            }
            (None, None) => false,
            _ => true,
        };
        if changed && !self.mapping_edited {
            self.mapping.clear();
            self.mapping_attempted = false;
        }
        self.table = table;
        self.loaded_columns = columns;
        self.loaded_rows = rows;
        self.loaded_truncated = truncated;
    }

    pub fn ensure_mapping(&mut self, cx: &mut Context<Self>) {
        if self.preview.is_some() && self.table.is_some() && self.mapping.is_empty() && !self.mapping_edited && !self.mapping_attempted {
            self.mapping_attempted = true;
            self.queue_mapping(cx);
        }
    }

    pub fn command(&mut self, id: &str, cx: &mut Context<Self>) {
        match id {
            "exchange-cancel-import" => self.cancel_import(),
            "exchange-confirm" => self.confirm(cx),
            "exchange-pick" => self.pick(cx),
            "exchange-export-loaded" => self.prompt_save(SaveKind::Loaded, cx),
            "exchange-export-full" => self.prompt_save(SaveKind::Full, cx),
            "exchange-dump" => self.prompt_save(SaveKind::Dump, cx),
            "exchange-migrate" => self.prompt_migrate(cx),
            "exchange-task-cancel" => self.cancel_current(),
            "exchange-clear" => self.log.clear_finished(),
            other if other.starts_with("exchange-format:") => {
                self.format_name = other.trim_start_matches("exchange-format:").to_owned();
            }
            other if other.starts_with("exchange-map:") => self.edit_mapping(other),
            _ => {}
        }
        cx.notify();
    }

    pub fn preview_file(&mut self, path: PathBuf, entry: ImportEntry, cx: &mut Context<Self>) {
        let services_locale = self.locale;
        let view = cx.entity().downgrade();
        let path = path.display().to_string();
        cx.spawn(async move |_, cx| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::app::views::grid::runtime_for_views().spawn(async move {
                let result = open_import(&path, entry).await;
                let _ = tx.send(result);
            });
            if let Ok(result) = rx.await {
                cx.update(|cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |this, cx| {
                            match result {
                                Ok(preview) => {
                                    this.notice = preview.notice.clone();
                                    this.mapping.clear();
                                    this.mapping_edited = false;
                                    this.mapping_attempted = false;
                                    this.confirmed = false;
                                    this.preview = Some(preview);
                                    this.ensure_mapping(cx);
                                }
                                Err(message) => {
                                    this.preview = None;
                                    this.confirmed = false;
                                    this.notice = message;
                                }
                            }
                            let _ = services_locale;
                            cx.notify();
                        });
                    }
                });
            }
        })
        .detach();
    }

    fn pick(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(SharedString::from(i18n::text(self.locale, "export.import.choose"))),
        });
        cx.spawn(async move |this, cx| {
            let message = receiver.await;
            cx.update(|cx| {
                let _ = this.update(cx, |pane, cx| pane.finish_pick(message, cx));
            });
        })
        .detach();
    }

    fn finish_pick<E: std::fmt::Display>(
        &mut self,
        message: Result<Result<Option<Vec<PathBuf>>, E>, futures::channel::oneshot::Canceled>,
        cx: &mut Context<Self>,
    ) {
        match message {
            Ok(Ok(Some(paths))) => {
                if let Some(path) = paths.into_iter().next() {
                    match crate::app::accept_open_path(&path) {
                        Ok(path) => {
                            self.open = true;
                            self.preview_file(path, ImportEntry::Dialog, cx);
                        }
                        Err(error) => self.notice = error,
                    }
                }
            }
            Ok(Ok(None)) => {}
            Ok(Err(error)) => self.notice = error.to_string(),
            Err(error) => self.notice = error.to_string(),
        }
        cx.notify();
    }

    fn edit_mapping(&mut self, id: &str) {
        let Some(body) = id.strip_prefix("exchange-map:") else { return };
        let Some((field, source)) = body.split_once('=') else { return };
        self.mapping_edited = true;
        if source.is_empty() {
            self.mapping.remove(field);
        } else {
            self.mapping.insert(field.to_owned(), source.to_owned());
        }
    }

    fn cancel_import(&mut self) {
        self.confirmed = false;
        self.services.tasks.cancel_owner_tasks("import");
        self.notice = i18n::text(self.locale, "export.import.unconfirmed");
        self.mark_cancelled("import");
    }

    fn queue_mapping(&mut self, cx: &mut Context<Self>) {
        let (Some(preview), Some(table)) = (self.preview.clone(), self.table.as_ref().map(copy_table)) else { return };
        let services = Arc::clone(&self.services);
        let view = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::app::views::grid::runtime_for_views().spawn(async move {
                let result = crate::application::grid::describe(&services.connections, &table).await;
                let _ = tx.send(result);
            });
            if let Ok(result) = rx.await {
                cx.update(|cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |this, cx| {
                            match result {
                                Ok(schema) => {
                                    let fields = schema
                                        .columns
                                        .iter()
                                        .map(|column| crate::export::TableField { name: column.name.clone(), nullable: column.nullable, auto: column.auto })
                                        .collect::<Vec<_>>();
                                    if !this.mapping_edited {
                                        this.mapping = crate::export::suggest_mapping(&fields, &preview.table.columns);
                                    }
                                }
                                Err(message) => this.notice = message,
                            }
                            cx.notify();
                        });
                    }
                });
            }
        })
        .detach();
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        if self.log.tasks.iter().any(|task| task.kind == "import" && task.running_now()) {
            self.notice = "导入还在进行，不能再次确认。".to_owned();
            return;
        }
        let (Some(preview), Some(table)) = (self.preview.clone(), self.table.as_ref().map(copy_table)) else {
            self.notice = i18n::text(self.locale, "export.import.unconfirmed");
            return;
        };
        if let Some(message) = block_unmapped(self.mapping_edited, &self.mapping) {
            self.confirmed = false;
            self.notice = message.clone();
            self.log.push(TaskView::finish("import", "import", Phase::Blocked, 0, message, None));
            return;
        }
        self.confirmed = true;
        let token = self.services.tasks.begin("import");
        let mapping = self.mapping.clone();
        let services = Arc::clone(&self.services);
        let view = cx.entity().downgrade();
        self.log.push(TaskView::running("import", "import", i18n::text(self.locale, "export.task.running")));
        cx.spawn(async move |_, cx| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::app::views::grid::runtime_for_views().spawn(async move {
                let result = commit_import_guarded(&services.connections, &services.tasks, token, &table, &preview.table, &mapping, true).await;
                let _ = tx.send(result);
            });
            let settled = rx.await;
            cx.update(|cx| {
                if let Some(view) = view.upgrade() {
                    view.update(cx, |this, cx| {
                        let task = match settled {
                            Ok(Ok(report)) if report.cancelled => {
                                TaskView::finish("import", "import", Phase::Cancelled, report.applied as u64, report.message, None)
                            }
                            Ok(Ok(report)) if report.success => TaskView::finish("import", "import", Phase::Done, report.applied as u64, report.message, None),
                            Ok(Ok(report)) if report.applied == 0 && !report.wrote => {
                                TaskView::finish("import", "import", Phase::Blocked, 0, report.message, None)
                            }
                            Ok(Ok(report)) => TaskView::finish("import", "import", Phase::Partial, report.applied as u64, report.message, None),
                            Ok(Err(message)) => TaskView::finish("import", "import", Phase::Failed, 0, message, None),
                            Err(_) => TaskView::finish("import", "import", Phase::Failed, 0, "后台任务没有返回结果。", None),
                        };
                        let cancelled = this.log.tasks.iter().any(|item| item.id == "import" && item.phase == Phase::Cancelled);
                        let task = prefer_cancel(task, cancelled);
                        this.notice = task.detail.clone();
                        this.log.push(task);
                        cx.notify();
                    });
                }
            });
        })
        .detach();
    }

    fn prompt_save(&mut self, kind: SaveKind, cx: &mut Context<Self>) {
        if let Some(loaded) = self.loaded.clone() {
            if matches!(kind, SaveKind::Loaded) {
                let task = export_loaded(&loaded);
                self.notice = task.detail.clone();
                self.log.push(task);
                return;
            }
        }
        if let Some(request) = self.export_request.take() {
            if matches!(kind, SaveKind::Full) {
                self.launch_full(request, cx);
                return;
            }
        }
        if let Some(request) = self.dump_request.take() {
            if matches!(kind, SaveKind::Dump) {
                self.spawn_dump(request, CancelPoint::No, cx);
                return;
            }
        }
        let suggested = match kind {
            SaveKind::Loaded => format!("loaded.{}", extension_of(&self.format_name)),
            SaveKind::Full => format!("full.{}", extension_of(&self.format_name)),
            SaveKind::Dump => "dump.sql".to_owned(),
        };
        let directory = std::env::temp_dir();
        let receiver = cx.prompt_for_new_path(&directory, Some(&suggested));
        cx.spawn(async move |this, cx| {
            let message = receiver.await;
            cx.update(|cx| {
                let _ = this.update(cx, |pane, cx| pane.finish_save(kind, message, cx));
            });
        })
        .detach();
    }

    fn finish_save<E: std::fmt::Display>(
        &mut self,
        kind: SaveKind,
        message: Result<Result<Option<PathBuf>, E>, futures::channel::oneshot::Canceled>,
        cx: &mut Context<Self>,
    ) {
        let path = match message {
            Ok(Ok(Some(path))) => match crate::app::accept_save_path(&path) {
                Ok(path) => path,
                Err(error) => {
                    let (id, kind) = save_slot(kind);
                    self.fail_open(id, kind, error);
                    cx.notify();
                    return;
                }
            },
            Ok(Ok(None)) => return,
            Ok(Err(error)) => {
                let (id, kind) = save_slot(kind);
                self.fail_open(id, kind, error.to_string());
                cx.notify();
                return;
            }
            Err(error) => {
                let (id, kind) = save_slot(kind);
                self.fail_open(id, kind, error.to_string());
                cx.notify();
                return;
            }
        };
        match kind {
            SaveKind::Loaded => self.start_loaded(path),
            SaveKind::Full => self.start_full(path, cx),
            SaveKind::Dump => self.start_dump(path, cx),
        }
        cx.notify();
    }

    fn start_loaded(&mut self, path: PathBuf) {
        if self.loaded_columns.is_empty() {
            self.fail_open("loaded", "export", "没有已读取的行，不能导出。".to_owned());
            return;
        }
        let table_name = self.table.as_ref().map(|table| table.table.clone()).unwrap_or_default();
        let schema = self.table.as_ref().map(|table| table.schema.clone()).unwrap_or_default();
        let spec = LoadedExport {
            id: format!("loaded-{}", uuid::Uuid::new_v4()),
            format: self.format_name.clone(),
            path,
            columns: self.loaded_columns.clone(),
            rows: self.loaded_rows.clone(),
            truncated: self.loaded_truncated,
            delimiter: ',',
            null_token: "NULL".to_owned(),
            bom: self.format_name != "json",
            table: table_name,
            schema,
            cancel_after_rows: None,
            fail_after_rows: None,
        };
        let task = export_loaded(&spec);
        self.notice = task.detail.clone();
        self.log.push(task);
    }

    fn start_full(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(table) = self.table.as_ref().map(copy_table) else {
            self.fail_open("full", "export", "没有当前表，不能全量导出。".to_owned());
            return;
        };
        let request = ExportRequest {
            id: format!("full-{}", uuid::Uuid::new_v4()),
            mode: "table-full".to_owned(),
            table,
            filters: Vec::new(),
            sort: None,
            search: None,
            limit: 0,
            truncated: false,
            statement: String::new(),
            acknowledged: true,
            path: path.display().to_string(),
            delimiter: ",".to_owned(),
            null_token: "NULL".to_owned(),
            bom: false,
            header: true,
            format: self.format_name.clone(),
        };
        self.launch_full(request, cx);
    }

    fn launch_full(&mut self, request: ExportRequest, cx: &mut Context<Self>) {
        let services = Arc::clone(&self.services);
        let view = cx.entity().downgrade();
        let id = request.id.clone();
        self.log.push(TaskView::running(id.clone(), "export", i18n::text(self.locale, "export.task.running")));
        cx.spawn(async move |_, cx| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::app::views::grid::runtime_for_views().spawn(async move {
                let result = export_full(&services.connections, &services.tasks, &request, false).await;
                let _ = tx.send(result);
            });
            let settled = rx.await;
            cx.update(|cx| {
                if let Some(view) = view.upgrade() {
                    view.update(cx, |this, cx| {
                        match settled {
                            Ok(result) => this.store_task(&id, "export", result, cx),
                            Err(_) => this.fail_open(&id, "export", "后台任务没有返回结果。".to_owned()),
                        }
                        cx.notify();
                    });
                }
            });
        })
        .detach();
    }

    fn start_dump(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(table) = self.table.as_ref().map(copy_table) else {
            self.fail_open("dump", "dump", "没有当前连接，不能全库导出。".to_owned());
            return;
        };
        let request = DumpRequest {
            id: format!("dump-{}", uuid::Uuid::new_v4()),
            connection_id: table.connection_id,
            engine: table.engine,
            database: if table.database.is_empty() { "main".to_owned() } else { table.database },
            schemas: Vec::new(),
            tables: Vec::new(),
            include_data: true,
            path: path.display().to_string(),
        };
        self.spawn_dump(request, CancelPoint::No, cx);
    }

    fn prompt_migrate(&mut self, cx: &mut Context<Self>) {
        if let Some(request) = self.migrate_request.take() {
            self.spawn_migrate(request, CancelPoint::No, cx);
            return;
        }
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(SharedString::from(i18n::text(self.locale, "export.migrate.start"))),
        });
        cx.spawn(async move |this, cx| {
            let message = receiver.await;
            cx.update(|cx| {
                let _ = this.update(cx, |pane, cx| pane.finish_migrate_pick(message, cx));
            });
        })
        .detach();
    }

    fn finish_migrate_pick<E: std::fmt::Display>(
        &mut self,
        message: Result<Result<Option<Vec<PathBuf>>, E>, futures::channel::oneshot::Canceled>,
        cx: &mut Context<Self>,
    ) {
        let path = match message {
            Ok(Ok(Some(paths))) => paths.into_iter().next(),
            Ok(Ok(None)) => None,
            Ok(Err(error)) => {
                self.fail_open("migrate", "migrate", error.to_string());
                cx.notify();
                return;
            }
            Err(error) => {
                self.fail_open("migrate", "migrate", error.to_string());
                cx.notify();
                return;
            }
        };
        let Some(path) = path else {
            cx.notify();
            return;
        };
        let path = match crate::app::accept_open_path(&path) {
            Ok(path) => path,
            Err(error) => {
                self.fail_open("migrate", "migrate", error);
                cx.notify();
                return;
            }
        };
        let Some(source) = self.table.as_ref().map(copy_table) else {
            self.fail_open("migrate", "migrate", "没有当前表，不能跨库迁移。".to_owned());
            cx.notify();
            return;
        };
        self.launch_migrate(source, path, cx);
    }

    fn launch_migrate(&mut self, source: TableRef, path: PathBuf, cx: &mut Context<Self>) {
        let services = Arc::clone(&self.services);
        let view = cx.entity().downgrade();
        let id = format!("migrate-{}", uuid::Uuid::new_v4());
        self.log.push(TaskView::running(id.clone(), "migrate", i18n::text(self.locale, "export.task.running")));
        let fail_id = id.clone();
        cx.spawn(async move |_, cx| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::app::views::grid::runtime_for_views().spawn(async move {
                let result = async {
                    let target_id = open_sqlite_target(&services.connections, &path).await?;
                    let request = MigrateRequest {
                        id: id.clone(),
                        source: migrate_ref(&source.connection_id, &source.table),
                        target: MigrateTableRef {
                            connection_id: target_id,
                            engine: "sqlite".to_owned(),
                            database: "main".to_owned(),
                            schema: source.schema.clone(),
                            table: source.table.clone(),
                        },
                        mapping: Vec::new(),
                        options: None,
                    };
                    // 源引擎以当前表为准，避免 sqlite 辅助函数盖住已连接的引擎名。
                    let mut request = request;
                    request.source.engine = source.engine.clone();
                    request.source.database = if source.database.is_empty() { "main".to_owned() } else { source.database.clone() };
                    request.source.schema = source.schema.clone();
                    watch_migrate(&services.connections, &services.tasks, &request, CancelPoint::No).await
                }
                .await;
                let _ = tx.send(result);
            });
            let settled = rx.await;
            cx.update(|cx| {
                if let Some(view) = view.upgrade() {
                    view.update(cx, |this, cx| {
                        match settled {
                            Ok(result) => this.store_task(&fail_id, "migrate", result, cx),
                            Err(_) => this.fail_open(&fail_id, "migrate", "后台任务没有返回结果。".to_owned()),
                        }
                        cx.notify();
                    });
                }
            });
        })
        .detach();
    }

    fn spawn_dump(&mut self, request: DumpRequest, stop: CancelPoint, cx: &mut Context<Self>) {
        let services = Arc::clone(&self.services);
        let view = cx.entity().downgrade();
        let id = request.id.clone();
        self.log.push(TaskView::running(id.clone(), "dump", i18n::text(self.locale, "export.task.running")));
        cx.spawn(async move |_, cx| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::app::views::grid::runtime_for_views().spawn(async move {
                let result = watch_dump(&services.connections, &services.tasks, &request, stop).await;
                let _ = tx.send(result);
            });
            let settled = rx.await;
            cx.update(|cx| {
                if let Some(view) = view.upgrade() {
                    view.update(cx, |this, cx| {
                        match settled {
                            Ok(result) => this.store_task(&id, "dump", result, cx),
                            Err(_) => this.fail_open(&id, "dump", "后台任务没有返回结果。".to_owned()),
                        }
                        cx.notify();
                    });
                }
            });
        })
        .detach();
    }

    fn spawn_migrate(&mut self, request: MigrateRequest, stop: CancelPoint, cx: &mut Context<Self>) {
        let services = Arc::clone(&self.services);
        let view = cx.entity().downgrade();
        let id = request.id.clone();
        self.log.push(TaskView::running(id.clone(), "migrate", i18n::text(self.locale, "export.task.running")));
        cx.spawn(async move |_, cx| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::app::views::grid::runtime_for_views().spawn(async move {
                let result = watch_migrate(&services.connections, &services.tasks, &request, stop).await;
                let _ = tx.send(result);
            });
            let settled = rx.await;
            cx.update(|cx| {
                if let Some(view) = view.upgrade() {
                    view.update(cx, |this, cx| {
                        match settled {
                            Ok(result) => this.store_task(&id, "migrate", result, cx),
                            Err(_) => this.fail_open(&id, "migrate", "后台任务没有返回结果。".to_owned()),
                        }
                        cx.notify();
                    });
                }
            });
        })
        .detach();
    }

    fn store_task(&mut self, id: &str, kind: &'static str, result: Result<TaskView, String>, cx: &mut Context<Self>) {
        match result {
            Ok(task) => {
                let cancelled = self.log.tasks.iter().any(|item| item.id == task.id && item.phase == Phase::Cancelled);
                let task = prefer_cancel(task, cancelled);
                self.notice = task.detail.clone();
                self.log.push(task);
            }
            Err(message) => self.fail_open(id, kind, message),
        }
        cx.notify();
    }

    fn fail_open(&mut self, id: &str, kind: &'static str, message: String) {
        if let Some(task) = self.log.tasks.iter_mut().find(|task| task.id == id) {
            if task.phase != Phase::Cancelled {
                let done = task.done_rows;
                let task_kind = task.kind;
                *task = TaskView::finish(id.to_owned(), task_kind, Phase::Failed, done, message.clone(), None);
            }
        } else {
            self.log.push(TaskView::finish(id.to_owned(), kind, Phase::Failed, 0, message.clone(), None));
        }
        self.notice = message;
    }

    fn mark_cancelled(&mut self, kind: &str) {
        if let Some(task) = self.log.tasks.iter_mut().rev().find(|task| task.running_now() && task.kind == kind) {
            let done = task.done_rows;
            task.phase = Phase::Cancelled;
            task.success = false;
            task.detail = format!("已取消。实际完成范围 {done} 行。");
            self.notice = task.detail.clone();
        }
    }

    fn cancel_current(&mut self) {
        if let Some(task) = self.log.tasks.iter_mut().rev().find(|task| task.running_now()) {
            let id = task.id.clone();
            let kind = task.kind;
            let done = task.done_rows;
            task.phase = Phase::Cancelled;
            task.success = false;
            task.detail = format!("已取消。实际完成范围 {done} 行。");
            match kind {
                "dump" => crate::application::cancel_dump(&self.services.tasks, "dump", &id),
                "migrate" => crate::application::cancel_migrate(&self.services.tasks, "migrate", &id),
                "export" => {
                    crate::grid::request_grid_export_cancel(&id);
                    self.services.tasks.cancel_owner_tasks("export");
                }
                "import" => self.services.tasks.cancel_owner_tasks("import"),
                _ => self.services.tasks.cancel_owner_tasks(kind),
            }
            self.notice = task.detail.clone();
        }
    }

    pub fn lines(&self) -> Vec<String> {
        let mut lines = vec![i18n::text(self.locale, "export.task.title"), self.notice.clone()];
        if let Some(table) = &self.table {
            lines.push(format!("目标 {}", table.table));
        }
        lines.push(format!("格式 {}", self.format_name));
        if self.mapping.is_empty() {
            if self.preview.is_some() && self.table.is_some() {
                lines.push(i18n::text(self.locale, "export.map.empty"));
            }
        } else {
            lines.push(i18n::text(self.locale, "export.map.title"));
            for (field, source) in &self.mapping {
                lines.push(format!("映射 {field}={source}"));
            }
        }
        if let Some(preview) = &self.preview {
            lines.push(i18n::text(self.locale, "export.preview.title"));
            lines.push(format!("{} {}", preview.format, preview.table.rows.len()));
            if preview.truncated {
                lines.push(i18n::text(self.locale, "export.preview.truncated"));
            } else {
                lines.push(i18n::text(self.locale, "export.preview.noWrite"));
            }
        }
        if self.log.tasks.is_empty() {
            lines.push(i18n::text(self.locale, "export.task.empty"));
        }
        for task in &self.log.tasks {
            lines.push(format!("{} {} {}", i18n::text(self.locale, task.label_key()), task.done_rows, task.detail));
        }
        lines
    }
}

impl Render for ExchangePane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.open {
            return div().id("exchange-idle");
        }
        let entity = cx.entity();
        let mut panel = div()
            .id("exchange")
            .role(gpui_kit::Role::Dialog)
            .aria_label(SharedString::from(i18n::text(self.locale, "export.task.title")))
            .flex()
            .flex_col()
            .gap(px(6.))
            .p(px(8.));
        for line in self.lines() {
            panel = panel.child(SharedString::from(line));
        }
        for (id, key) in [
            ("exchange-confirm", "export.import.confirm"),
            ("exchange-pick", "export.import.choose"),
            ("exchange-cancel-import", "export.import.cancel"),
            ("exchange-export-loaded", "export.export.loaded"),
            ("exchange-export-full", "export.export.full"),
            ("exchange-format:xlsx", "export.export.xlsx"),
            ("exchange-dump", "export.dump.start"),
            ("exchange-migrate", "export.migrate.start"),
            ("exchange-task-cancel", "export.task.cancel"),
            ("exchange-clear", "export.task.clear"),
        ] {
            let label = SharedString::from(i18n::text(self.locale, key));
            let command = id.to_owned();
            let entity_click = entity.clone();
            panel = panel.child(
                div()
                    .id(SharedString::from(format!("exchange-{id}")))
                    .role(gpui_kit::Role::Button)
                    .aria_label(label.clone())
                    .child(label)
                    .on_click(move |_, _, cx| {
                        let command = command.clone();
                        entity_click.update(cx, |this, cx| this.command(&command, cx));
                    }),
            );
        }
        panel
    }
}

#[derive(Clone, Copy)]
enum SaveKind {
    Loaded,
    Full,
    Dump,
}

fn block_unmapped(edited: bool, mapping: &BTreeMap<String, String>) -> Option<String> {
    if mapping.values().any(|source| !source.is_empty()) {
        return None;
    }
    if edited {
        Some("必填字段没有对应列，不能开始写入。".to_owned())
    } else {
        Some("还没有列映射，不能开始写入。".to_owned())
    }
}

fn save_slot(kind: SaveKind) -> (&'static str, &'static str) {
    match kind {
        SaveKind::Loaded => ("loaded", "export"),
        SaveKind::Full => ("full", "export"),
        SaveKind::Dump => ("dump", "dump"),
    }
}

fn extension_of(format: &str) -> &'static str {
    match format {
        "json" => "json",
        "xlsx" => "xlsx",
        "markdown" => "md",
        "insert" => "sql",
        _ => "csv",
    }
}

async fn open_sqlite_target(service: &crate::connections::Service, path: &std::path::Path) -> Result<String, String> {
    let mut draft = crate::app::views::connections::blank_draft(crate::connections::model::Engine::Sqlite);
    draft.id = Some("exchange-migrate-target".to_owned());
    draft.name = "迁移目标".to_owned();
    draft.file_path = path.display().to_string();
    let saved = service.save(draft, crate::connections::model::SecretInput::default()).await.map_err(|error| error.message())?;
    let outcome = service.open(&saved.connection.id, crate::connections::model::SecretInput::default()).await?;
    if !outcome.connected {
        return Err(outcome.report.summary.unwrap_or_else(|| "目标库没有打开".to_owned()));
    }
    Ok(saved.connection.id)
}

fn copy_table(table: &TableRef) -> TableRef {
    TableRef {
        connection_id: table.connection_id.clone(),
        database: table.database.clone(),
        schema: table.schema.clone(),
        table: table.table.clone(),
        engine: table.engine.clone(),
        include_foreign_keys: table.include_foreign_keys,
    }
}

pub fn migrate_ref(connection_id: &str, table: &str) -> MigrateTableRef {
    MigrateTableRef {
        connection_id: connection_id.to_owned(),
        engine: "sqlite".to_owned(),
        database: "main".to_owned(),
        schema: String::new(),
        table: table.to_owned(),
    }
}

pub fn migrate_request(id: &str, source: MigrateTableRef, target: MigrateTableRef, mapping: &[(&str, &str)]) -> MigrateRequest {
    MigrateRequest {
        id: id.to_owned(),
        source,
        target,
        mapping: mapping.iter().map(|(source, target)| ColumnPair { source: (*source).to_owned(), target: (*target).to_owned() }).collect(),
        options: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelled_task_copy_is_not_the_running_label() {
        let mut pane = ExchangePane::new(Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(std::env::temp_dir().join(format!(
            "rillsql-exchange-copy-{}",
            uuid::Uuid::new_v4()
        )))))));
        pane.notice = "已取消。实际完成范围 2 行。".to_owned();
        pane.log.push(TaskView::finish("dump-1", "dump", Phase::Cancelled, 2, pane.notice.clone(), None));
        let zh = pane.lines().join("\n");
        assert!(zh.contains("已取消"), "{zh}");
        assert!(zh.contains("2"), "{zh}");
        assert!(!zh.contains("进行中"), "{zh}");
        pane.set_locale(Locale::En);
        let en = pane.lines().join("\n");
        assert!(en.contains("Cancelled"), "{en}");
        assert!(!en.contains("Running"), "{en}");
    }
}
