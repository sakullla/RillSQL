//! 导入、导出、全库导出、跨库迁移和任务中心。确认前不写库，取消后不再显示运行中。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, PathPromptOptions, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};

use crate::application::AppServices;
use crate::export::{
    commit_import, export_full, export_loaded, open_import, watch_dump, watch_migrate, CancelPoint, ImportEntry, ImportPreview, Phase, TaskLog, TaskView,
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
    confirmed: bool,
    table: Option<TableRef>,
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
            confirmed: false,
            table: None,
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

    pub fn command(&mut self, id: &str, cx: &mut Context<Self>) {
        match id {
            "exchange-cancel-import" => {
                self.confirmed = false;
                self.notice = i18n::text(self.locale, "export.import.unconfirmed");
            }
            "exchange-confirm" => self.confirm(cx),
            "exchange-pick" => self.pick(cx),
            "exchange-export-loaded" => self.export_loaded_now(),
            "exchange-export-full" => self.export_full_now(false, cx),
            "exchange-dump" => self.dump_now(CancelPoint::No, cx),
            "exchange-migrate" => self.migrate_now(CancelPoint::No, cx),
            "exchange-task-cancel" => self.cancel_current(),
            "exchange-clear" => self.log.clear_finished(),
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
                let _ = cx.update(|cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |this, cx| {
                            match result {
                                Ok(preview) => {
                                    this.notice = preview.notice.clone();
                                    this.mapping.clear();
                                    this.confirmed = false;
                                    this.preview = Some(preview);
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
            let _ = cx.update(|cx| {
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

    fn confirm(&mut self, cx: &mut Context<Self>) {
        let (Some(preview), Some(table)) = (self.preview.clone(), self.table.as_ref().map(copy_table)) else {
            self.notice = i18n::text(self.locale, "export.import.unconfirmed");
            return;
        };
        self.confirmed = true;
        let mapping = self.mapping.clone();
        let services = Arc::clone(&self.services);
        let view = cx.entity().downgrade();
        self.log.push(TaskView::running("import", "import", i18n::text(self.locale, "export.import.confirm")));
        cx.spawn(async move |_, cx| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::app::views::grid::runtime_for_views().spawn(async move {
                let fields = match crate::application::grid::describe(&services.connections, &table).await {
                    Ok(schema) => schema
                        .columns
                        .iter()
                        .map(|column| crate::export::TableField { name: column.name.clone(), nullable: column.nullable, auto: column.auto })
                        .collect::<Vec<_>>(),
                    Err(message) => {
                        let _ = tx.send(Err(message));
                        return;
                    }
                };
                let mapping = if mapping.is_empty() { crate::export::suggest_mapping(&fields, &preview.table.columns) } else { mapping };
                let result = commit_import(&services.connections, &table, &preview.table, &mapping, true).await;
                let _ = tx.send(result);
            });
            if let Ok(result) = rx.await {
                let _ = cx.update(|cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |this, cx| {
                            let task = match result {
                                Ok(report) if report.success => TaskView::finish("import", "import", Phase::Done, report.applied as u64, report.message, None),
                                Ok(report) if report.applied == 0 && !report.wrote => {
                                    TaskView::finish("import", "import", Phase::Blocked, 0, report.message, None)
                                }
                                Ok(report) => TaskView::finish("import", "import", Phase::Partial, report.applied as u64, report.message, None),
                                Err(message) => TaskView::finish("import", "import", Phase::Failed, 0, message, None),
                            };
                            this.notice = task.detail.clone();
                            this.log.push(task);
                            cx.notify();
                        });
                    }
                });
            }
        })
        .detach();
    }

    fn export_loaded_now(&mut self) {
        let Some(spec) = self.loaded.clone() else {
            self.notice = i18n::text(self.locale, "export.export.loaded");
            return;
        };
        let task = export_loaded(&spec);
        self.notice = task.detail.clone();
        self.log.push(task);
    }

    fn export_full_now(&mut self, cancel_late: bool, cx: &mut Context<Self>) {
        let Some(request) = self.export_request.take() else {
            self.notice = i18n::text(self.locale, "export.export.full");
            return;
        };
        let services = Arc::clone(&self.services);
        let view = cx.entity().downgrade();
        self.log.push(TaskView::running(request.id.clone(), "export", i18n::text(self.locale, "export.task.running")));
        cx.spawn(async move |_, cx| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::app::views::grid::runtime_for_views().spawn(async move {
                let result = export_full(&services.connections, &services.tasks, &request, cancel_late).await;
                let _ = tx.send(result);
            });
            if let Ok(result) = rx.await {
                let _ = cx.update(|cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |this, cx| {
                            match result {
                                Ok(task) => {
                                    this.notice = task.detail.clone();
                                    this.log.push(task);
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

    fn dump_now(&mut self, stop: CancelPoint, cx: &mut Context<Self>) {
        let Some(request) = self.dump_request.take() else {
            self.notice = i18n::text(self.locale, "export.dump.start");
            return;
        };
        self.spawn_dump(request, stop, cx);
    }

    fn migrate_now(&mut self, stop: CancelPoint, cx: &mut Context<Self>) {
        let Some(request) = self.migrate_request.take() else {
            self.notice = i18n::text(self.locale, "export.migrate.start");
            return;
        };
        self.spawn_migrate(request, stop, cx);
    }

    fn spawn_dump(&mut self, request: DumpRequest, stop: CancelPoint, cx: &mut Context<Self>) {
        let services = Arc::clone(&self.services);
        let view = cx.entity().downgrade();
        self.log.push(TaskView::running(request.id.clone(), "dump", i18n::text(self.locale, "export.task.running")));
        cx.spawn(async move |_, cx| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::app::views::grid::runtime_for_views().spawn(async move {
                let result = watch_dump(&services.connections, &services.tasks, &request, stop).await;
                let _ = tx.send(result);
            });
            if let Ok(result) = rx.await {
                let _ = cx.update(|cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |this, cx| this.store_task(result, cx));
                    }
                });
            }
        })
        .detach();
    }

    fn spawn_migrate(&mut self, request: MigrateRequest, stop: CancelPoint, cx: &mut Context<Self>) {
        let services = Arc::clone(&self.services);
        let view = cx.entity().downgrade();
        self.log.push(TaskView::running(request.id.clone(), "migrate", i18n::text(self.locale, "export.task.running")));
        cx.spawn(async move |_, cx| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::app::views::grid::runtime_for_views().spawn(async move {
                let result = watch_migrate(&services.connections, &services.tasks, &request, stop).await;
                let _ = tx.send(result);
            });
            if let Ok(result) = rx.await {
                let _ = cx.update(|cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |this, cx| this.store_task(result, cx));
                    }
                });
            }
        })
        .detach();
    }

    fn store_task(&mut self, result: Result<TaskView, String>, cx: &mut Context<Self>) {
        match result {
            Ok(task) => {
                self.notice = task.detail.clone();
                self.log.push(task);
            }
            Err(message) => self.notice = message,
        }
        cx.notify();
    }

    fn cancel_current(&mut self) {
        if let Some(task) = self.log.tasks.iter_mut().rev().find(|task| task.running_now()) {
            let id = task.id.clone();
            let kind = task.kind;
            task.phase = Phase::Cancelled;
            task.success = false;
            task.detail = format!("{} {}", i18n::text(self.locale, "export.task.cancelled"), i18n::text(self.locale, "export.task.range"));
            match kind {
                "dump" => crate::application::cancel_dump(&self.services.tasks, "dump", &id),
                "migrate" => crate::application::cancel_migrate(&self.services.tasks, "migrate", &id),
                "export" => crate::grid::request_grid_export_cancel(&id),
                _ => self.services.tasks.cancel_owner_tasks(kind),
            }
            self.notice = task.detail.clone();
        }
    }

    pub fn lines(&self) -> Vec<String> {
        let mut lines = vec![i18n::text(self.locale, "export.task.title"), self.notice.clone()];
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
