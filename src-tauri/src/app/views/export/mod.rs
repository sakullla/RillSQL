//! 导入、导出、全库导出、跨库迁移和任务中心。确认前不写库，取消后不再显示运行中。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::Sizable as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, PathPromptOptions, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};

use crate::ui::kit::{self, Palette};

use crate::application::AppServices;
use crate::export::{
    commit_import_guarded, export_full, export_loaded, open_import, prefer_cancel, watch_dump, watch_migrate, CancelPoint, ImportEntry, ImportPreview, LoadedExport, Phase, TaskLog,
    TaskView,
};
use crate::grid::commands::{ExportRequest, TableRef};
use crate::i18n::{self, Locale};
use crate::dump::dump::DumpRequest;
use crate::migrate::migrate::{ColumnPair, MigrateRequest, MigrateTableRef};

/// 交换面板的两种数据来源。表数据可以重新读取全量，查询结果只有本次已读取的行，
/// 两者的目标、列和能力不能混用。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExchangeSourceKind {
    #[default]
    Table,
    Query,
}

impl ExchangeSourceKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Table => "当前表数据",
            Self::Query => "当前查询结果",
        }
    }

    pub fn is_query(self) -> bool {
        matches!(self, Self::Query)
    }
}

/// 面板当前展示的来源快照。列、已读取行、目标与全量能力全部随它切换，
/// 不再从 shell 的 table_spec 兜底，避免查询页拿到上一张表的目标。
#[derive(Debug, Default)]
pub struct ExchangeSource {
    pub kind: ExchangeSourceKind,
    /// 来源工作项 id；切换工作项时用它判断是否要重建映射。
    pub item: Option<String>,
    pub table: Option<TableRef>,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    pub truncated: bool,
    /// 来源仍在该工作项上执行（例如查询尚未结束）。
    pub running: bool,
    pub label: String,
    pub context: String,
    /// 这些行读出来时的大字段预览上限。导出前的信封检查按它比较；
    /// None 表示这份行没经过预览截断（例如查询结果），协议形状的普通文本仍是原值。
    pub preview_bytes: Option<usize>,
}

impl ExchangeSource {
    pub fn full_available(&self) -> bool {
        self.kind == ExchangeSourceKind::Table && self.table.is_some() && !self.running
    }

    pub fn has_columns(&self) -> bool {
        !self.columns.is_empty()
    }

    fn table_key(table: &Option<TableRef>) -> Option<(&str, &str, &str, &str)> {
        table
            .as_ref()
            .map(|table| (table.connection_id.as_str(), table.database.as_str(), table.schema.as_str(), table.table.as_str()))
    }
}

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
    source: ExchangeSource,
    /// 来源版本；迟到映射结果只在版本一致时写回。
    source_generation: u64,
    format_name: String,
    /// 已读取行的导出上限；None 表示整个已读取范围。
    row_limit: Option<usize>,
    export_request: Option<ExportRequest>,
    loaded: Option<crate::export::LoadedExport>,
    dump_request: Option<DumpRequest>,
    migrate_request: Option<MigrateRequest>,
    pub notice: String,
    /// 来源切换由外壳决定工作项与来源，面板只把请求交回 Workbench。
    owner: Option<gpui_kit::WeakEntity<crate::app::shell::Workbench>>,
}

impl ExchangePane {
    pub(crate) fn attach_owner(&mut self, owner: gpui_kit::WeakEntity<crate::app::shell::Workbench>) {
        self.owner = Some(owner);
    }

    /// 面板按钮的唯一样本来回外壳：切换来源必须走 Workbench::perform。
    pub(crate) fn request_source(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(owner) = self.owner.clone() else { return };
        let id = id.to_owned();
        // 面板正在更新时不能同步回头更新外壳（会重入面板）；defer 到本轮结束再切换来源。
        cx.defer(move |cx| {
            if let Some(owner) = owner.upgrade() {
                owner.update(cx, |view, cx| view.perform(&id, cx));
            }
        });
    }

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
            source: ExchangeSource::default(),
            source_generation: 0,
            format_name: "csv".to_owned(),
            row_limit: None,
            export_request: None,
            loaded: None,
            dump_request: None,
            migrate_request: None,
            notice: String::new(),
            owner: None,
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
        self.source.table = Some(table);
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

    /// 把面板绑定到当前来源。来源类型、工作项、目标表或列发生变化时重建映射，
    /// 旧来源的映射绝不沿用到新来源。
    pub fn bind_source(&mut self, source: ExchangeSource) {
        let changed = self.source.kind != source.kind
            || self.source.item != source.item
            || ExchangeSource::table_key(&self.source.table) != ExchangeSource::table_key(&source.table)
            || self.source.columns != source.columns;
        if changed {
            self.source_generation = self.source_generation.wrapping_add(1);
            self.mapping.clear();
            self.mapping_edited = false;
            self.mapping_attempted = false;
            self.confirmed = false;
            self.row_limit = None;
        }
        self.source = source;
    }

    pub fn source(&self) -> &ExchangeSource {
        &self.source
    }

    pub fn source_generation(&self) -> u64 {
        self.source_generation
    }

    pub fn ensure_mapping(&mut self, cx: &mut Context<Self>) {
        if self.preview.is_some()
            && self.source.kind == ExchangeSourceKind::Table
            && self.source.table.is_some()
            && self.mapping.is_empty()
            && !self.mapping_edited
            && !self.mapping_attempted
        {
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
            other if other.starts_with("exchange-range:") => {
                self.apply_range(other.trim_start_matches("exchange-range:"));
            }
            other if other.starts_with("exchange-map:") => self.edit_mapping(other),
            _ => {}
        }
        cx.notify();
    }

    fn apply_range(&mut self, text: &str) {
        self.row_limit = match text {
            "all" => None,
            value => value.parse::<usize>().ok().filter(|limit| *limit > 0),
        };
    }

    /// 已读取行的导出范围。上限小于当前行数时标记为截断，任务和文件都只写这段。
    fn rows_for_export(&self) -> (Vec<Vec<Option<String>>>, bool) {
        let mut rows = self.source.rows.clone();
        let limited = self.row_limit.is_some_and(|limit| limit < rows.len());
        if let Some(limit) = self.row_limit {
            rows.truncate(limit);
        }
        (rows, limited)
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
        let (Some(preview), Some(table)) = (self.preview.clone(), self.source.table.as_ref().map(copy_table)) else { return };
        let generation = self.source_generation;
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
                            // 来源已经切换：旧目标的映射不能写回新来源。
                            if generation != this.source_generation {
                                return;
                            }
                            match result {
                                Ok(schema) => {
                                    let fields = schema
                                        .columns
                                        .iter()
                                        .map(|column| crate::export::TableField { name: column.name.clone(), nullable: column.nullable, auto: column.auto })
                                        .collect::<Vec<_>>();
                                    let mapped = crate::export::suggest_mapping(&fields, &preview.table.columns);
                                    let _ = this.apply_mapping(generation, mapped);
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

    /// 迟到的映射结果只有在来源版本仍一致时才应用。
    fn apply_mapping(&mut self, generation: u64, mapping: BTreeMap<String, String>) -> bool {
        if generation != self.source_generation || self.mapping_edited {
            return false;
        }
        self.mapping = mapping;
        true
    }

    #[cfg(test)]
    pub fn apply_mapping_for(&mut self, generation: u64, mapping: BTreeMap<String, String>) -> bool {
        self.apply_mapping(generation, mapping)
    }

    #[cfg(test)]
    pub fn mapping(&self) -> &BTreeMap<String, String> {
        &self.mapping
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        if self.log.tasks.iter().any(|task| task.kind == "import" && task.running_now()) {
            self.notice = "导入还在进行，不能再次确认。".to_owned();
            return;
        }
        let (Some(preview), Some(table)) = (self.preview.clone(), self.source.table.as_ref().map(copy_table)) else {
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
        // 先挡住来源本身不能导出的情况，避免先弹出保存对话框再报失败。
        if self.source.kind.is_query() {
            match kind {
                SaveKind::Full => {
                    self.block("full", "export", "当前来源是查询结果，不能全量导出；只能导出已读取的行。".to_owned());
                    return;
                }
                SaveKind::Dump => {
                    self.block("dump", "dump", "当前来源是查询结果，不能全库导出；先切换到当前表数据。".to_owned());
                    return;
                }
                SaveKind::Loaded => {}
            }
        }
        if self.source.running {
            let (id, owner) = save_slot(kind);
            self.block(id, owner, "来源正在执行，结果尚未就绪，不能导出。".to_owned());
            return;
        }
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
        if self.source.running {
            self.block("loaded", "export", "查询正在执行，结果尚未就绪，不能导出。".to_owned());
            return;
        }
        if !self.source.has_columns() {
            self.block("loaded", "export", self.empty_reason().to_owned());
            return;
        }
        // 预览信封不是原值：先在网格里读取完整值，导出写的是同一次定位读到的原值。
        if self
            .source
            .rows
            .iter()
            .flatten()
            .flatten()
            .any(|value| crate::grid::looks_like_preview(value, self.source.preview_bytes))
        {
            self.block("loaded", "export", i18n::text(self.locale, "grid.export.needFull").to_owned());
            return;
        }
        let table_name = self.source.table.as_ref().map(|table| table.table.clone()).unwrap_or_else(|| "查询结果".to_owned());
        let schema = self.source.table.as_ref().map(|table| table.schema.clone()).unwrap_or_default();
        let (rows, limited) = self.rows_for_export();
        let spec = LoadedExport {
            id: format!("loaded-{}", uuid::Uuid::new_v4()),
            format: self.format_name.clone(),
            path,
            columns: self.source.columns.clone(),
            rows,
            truncated: self.source.truncated || limited,
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
        if self.source.kind.is_query() {
            self.block("full", "export", "当前来源是查询结果，不能全量导出；只能导出已读取的行。".to_owned());
            return;
        }
        if self.source.running {
            self.block("full", "export", "来源正在执行，结果尚未就绪，不能全量导出。".to_owned());
            return;
        }
        let Some(table) = self.source.table.as_ref().map(copy_table) else {
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
        if self.source.kind.is_query() {
            self.block("dump", "dump", "当前来源是查询结果，不能全库导出；先切换到当前表数据。".to_owned());
            return;
        }
        let Some(table) = self.source.table.as_ref().map(copy_table) else {
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
        if self.source.kind.is_query() {
            self.block("migrate", "migrate", "当前来源是查询结果，不能跨库迁移；先切换到当前表数据。".to_owned());
            return;
        }
        if self.source.running {
            self.block("migrate", "migrate", "来源正在执行，结果尚未就绪，不能迁移。".to_owned());
            return;
        }
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
        let Some(source) = self.source.table.as_ref().map(copy_table) else {
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

    /// 来源本身不满足导出条件时记一条“未开始”的结果，不冒充失败或成功。
    fn block(&mut self, id: &str, kind: &'static str, message: String) {
        if let Some(task) = self.log.tasks.iter_mut().find(|task| task.id == id && task.running_now()) {
            let done = task.done_rows;
            let task_kind = task.kind;
            *task = TaskView::finish(id.to_owned(), task_kind, Phase::Blocked, done, message.clone(), None);
        } else {
            self.log.push(TaskView::finish(id.to_owned(), kind, Phase::Blocked, 0, message.clone(), None));
        }
        self.notice = message;
    }

    fn empty_reason(&self) -> &'static str {
        match self.source.kind {
            ExchangeSourceKind::Query if self.source.running => "查询正在执行，结果尚未就绪。",
            ExchangeSourceKind::Query => "当前查询还没有结果；先执行一次查询，再导出或交换。",
            ExchangeSourceKind::Table if self.source.running => "表数据正在读取，当前行数尚未完整。",
            ExchangeSourceKind::Table => "当前没有表数据；先在对象树里打开一张表。",
        }
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
        lines.push(format!("来源 {}", self.source.kind.label()));
        match (&self.source.kind, &self.source.table) {
            (ExchangeSourceKind::Table, Some(table)) => {
                lines.push(format!("目标 {}", table.table));
                if !self.source.context.is_empty() {
                    lines.push(format!("连接 {}", self.source.context));
                }
            }
            (ExchangeSourceKind::Table, None) => lines.push("目标 未选择表".to_owned()),
            (ExchangeSourceKind::Query, _) => {
                lines.push("目标 查询结果".to_owned());
                if !self.source.context.is_empty() {
                    lines.push(format!("连接 {}", self.source.context));
                }
            }
        }
        lines.push(format!("已读取 {} 行", self.source.rows.len()));
        if self.source.truncated {
            lines.push("仅已读取部分".to_owned());
        }
        if self.source.kind.is_query() && !self.source.full_available() {
            lines.push("全量导出不可用：查询结果只有已读取的行".to_owned());
        }
        if self.source.running {
            lines.push("查询正在执行，结果尚未就绪。".to_owned());
        } else if !self.source.has_columns() {
            lines.push(self.empty_reason().to_owned());
        }
        lines.push(format!("格式 {}", self.format_name));
        lines.push(match self.row_limit {
            Some(limit) => format!("范围 前 {limit} 行"),
            None => "范围 全部已读取行".to_owned(),
        });
        if self.mapping.is_empty() {
            if self.preview.is_some() && self.source.table.is_some() {
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
        let p = Palette::of(cx);
        let locale = self.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let entity = cx.entity();
        let act = |id: &'static str| {
            let entity = entity.clone();
            move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut gpui_kit::App| entity.update(cx, |pane, cx| pane.command(id, cx))
        };
        let section = |title: SharedString, body: gpui_kit::Div| div().flex().flex_col().gap(px(8.)).child(kit::eyebrow(title, &p)).child(body);

        let mut panel = div()
            .id("exchange")
            .role(gpui_kit::Role::Dialog)
            .aria_label(t("export.task.title"))
            .flex()
            .flex_col()
            .gap(px(18.))
            .p(px(16.));
        if !self.notice.is_empty() {
            panel = panel.child(div().px(px(12.)).py(px(8.)).rounded(px(8.)).bg(p.hover).text_size(px(12.5)).child(SharedString::from(self.notice.clone())));
        }

        // 来源选择：当前表数据或当前查询结果，两种来源的列、行与能力互不混用。
        let mut kinds = Vec::new();
        for (kind, id, label) in [
            (ExchangeSourceKind::Table, "exchange-source:table", "当前表数据"),
            (ExchangeSourceKind::Query, "exchange-source:query", "当前查询结果"),
        ] {
            let entity = entity.clone();
            kinds.push((
                SharedString::from(format!("exchange-{id}")),
                SharedString::from(label),
                self.source.kind == kind,
                kit::click(move |_, _, cx| {
                    let id = id.to_owned();
                    entity.update(cx, |pane, cx| pane.request_source(&id, cx));
                }),
            ));
        }
        let mut source_body = div().flex().flex_col().gap(px(6.));
        match self.source.kind {
            ExchangeSourceKind::Table => match &self.source.table {
                Some(table) => {
                    source_body = source_body.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(kit::engine_badge(&table.engine, 18.))
                            .child(div().font_weight(gpui_kit::FontWeight::MEDIUM).child(SharedString::from(table.table.clone())))
                            .child(kit::muted(SharedString::from(table.database.clone()), &p)),
                    );
                }
                None => {
                    source_body = source_body.child(
                        div()
                            .text_size(px(12.5))
                            .text_color(p.text_faint)
                            .child(SharedString::from("当前没有表数据；先在对象树里打开一张表。")),
                    );
                }
            },
            ExchangeSourceKind::Query => {
                source_body = source_body.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(kit::icon(IconName::SquareTerminal, 15., p.accent))
                        .child(div().font_weight(gpui_kit::FontWeight::MEDIUM).child(SharedString::from("查询结果")))
                        .child(kit::muted(SharedString::from(self.source.context.clone()), &p)),
                );
                if self.source.running {
                    source_body = source_body.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .text_size(px(12.))
                            .text_color(p.text_muted)
                            .child(Spinner::new().xsmall().color(p.accent))
                            .child(SharedString::from(if self.source.kind.is_query() {
                                "查询正在执行，结果尚未就绪。"
                            } else {
                                "表数据正在读取，当前行数尚未完整。"
                            })),
                    );
                } else if !self.source.has_columns() {
                    source_body = source_body.child(
                        div()
                            .text_size(px(12.5))
                            .text_color(p.text_faint)
                            .child(SharedString::from("当前查询还没有结果；先执行一次查询。")),
                    );
                }
            }
        }
        if self.source.running && !self.source.kind.is_query() {
            source_body = source_body.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .text_size(px(12.))
                    .text_color(p.text_muted)
                    .child(Spinner::new().xsmall().color(p.accent))
                    .child(SharedString::from("表数据正在读取，当前行数尚未完整。")),
            );
        }
        if self.source.has_columns() && !self.source.running {
            let partial = if self.source.truncated { "（仅已读取部分）" } else { "" };
            source_body = source_body.child(
                div()
                    .text_size(px(12.))
                    .text_color(p.text_muted)
                    .child(SharedString::from(format!("已读取 {} 行{partial}", self.source.rows.len()))),
            );
        }
        if self.source.kind.is_query() {
            source_body = source_body.child(
                div()
                    .text_size(px(11.5))
                    .text_color(p.text_faint)
                    .child(SharedString::from("查询结果只有已读取的行；全量导出不可用。")),
            );
        }
        let mut formats = Vec::new();
        for (format, label) in [("csv", "CSV"), ("xlsx", "Excel"), ("json", "JSON"), ("insert", "SQL"), ("markdown", "Markdown")] {
            let entity = entity.clone();
            let id = format!("exchange-format:{format}");
            formats.push((
                SharedString::from(format!("exchange-exchange-format:{format}")),
                SharedString::from(label),
                self.format_name == format,
                kit::click(move |_, _, cx| {
                    let id = id.clone();
                    entity.update(cx, |pane, cx| pane.command(&id, cx));
                }),
            ));
        }
        let mut ranges = Vec::new();
        for (id, label, limit) in [("all", "全部已读取", None), ("100", "前 100 行", Some(100)), ("1000", "前 1000 行", Some(1000))] {
            let entity = entity.clone();
            let command = format!("exchange-range:{id}");
            ranges.push((
                SharedString::from(format!("exchange-exchange-range:{id}")),
                SharedString::from(label),
                self.row_limit == limit,
                kit::click(move |_, _, cx| {
                    let command = command.clone();
                    entity.update(cx, |pane, cx| pane.command(&command, cx));
                }),
            ));
        }
        let range_enabled = self.source.kind == ExchangeSourceKind::Query || self.source.rows.len() > 100;
        panel = panel.child(section(
            t("ui.exchange.source"),
            kit::card(&p)
                .p(px(14.))
                .flex()
                .flex_col()
                .gap(px(12.))
                .child(kit::segmented(kinds, &p))
                .child(source_body)
                .child(kit::form_row(t("ui.exchange.format"), kit::segmented(formats, &p), &p))
                .when(range_enabled, |this| {
                    this.child(kit::form_row(
                        SharedString::from(if locale == Locale::En { "Export range" } else { "导出范围" }),
                        kit::segmented(ranges, &p),
                        &p,
                    ))
                }),
        ));

        // 导出：不可用的能力直接说明原因，而不是留一个点了没反应的入口。
        let loaded_ready = self.source.has_columns() && !self.source.running;
        let mut export = div().flex().flex_col().gap(px(6.));
        if loaded_ready {
            export = export.child(action_row(
                "exchange-exchange-export-loaded",
                IconName::FileDown,
                t("export.export.loaded"),
                SharedString::from(format!("只导出当前来源已读取的 {} 行", self.source.rows.len())),
                &p,
                act("exchange-export-loaded"),
            ));
        } else {
            export = export.child(blocked_row(
                "exchange-exchange-export-loaded",
                IconName::FileDown,
                t("export.export.loaded"),
                self.empty_reason(),
                &p,
            ));
        }
        if self.source.full_available() {
            export = export.child(action_row(
                "exchange-exchange-export-full",
                IconName::Download,
                t("export.export.full"),
                t("ui.exchange.fullHint"),
                &p,
                act("exchange-export-full"),
            ));
        } else {
            export = export.child(blocked_row(
                "exchange-exchange-export-full",
                IconName::Download,
                t("export.export.full"),
                "当前来源不支持全量导出；请切换到当前表数据。",
                &p,
            ));
        }
        if self.source.kind.is_query() {
            export = export
                .child(blocked_row("exchange-exchange-dump", IconName::DatabaseBackup, t("export.dump.start"), "查询结果不能全库导出。", &p))
                .child(blocked_row("exchange-exchange-migrate", IconName::ArrowRightLeft, t("export.migrate.start"), "查询结果不能跨库迁移。", &p));
        } else {
            export = export
                .child(action_row("exchange-exchange-dump", IconName::DatabaseBackup, t("export.dump.start"), t("ui.exchange.dumpHint"), &p, act("exchange-dump")))
                .child(action_row("exchange-exchange-migrate", IconName::ArrowRightLeft, t("export.migrate.start"), t("ui.exchange.migrateHint"), &p, act("exchange-migrate")));
        }
        panel = panel.child(section(t("ui.exchange.export"), export));

        // 导入：写入目标必须来自当前表来源。
        let mut import = kit::card(&p).p(px(14.)).flex().flex_col().gap(px(12.));
        if self.source.kind.is_query() {
            import = import.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .py(px(10.))
                    .child(div().font_weight(gpui_kit::FontWeight::MEDIUM).child(SharedString::from("导入需要当前表数据")))
                    .child(kit::muted(SharedString::from("切换到“当前表数据”并打开一张表后，才能选择文件并确认写入。"), &p)),
            );
        } else {
        match &self.preview {
            None => {
                import = import.child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(8.))
                        .py(px(14.))
                        .rounded(px(8.))
                        .border_1()
                        .border_dashed()
                        .border_color(p.line_strong)
                        .child(kit::icon(IconName::FileUp, 22., p.text_faint))
                        .child(kit::muted(t("ui.exchange.dropHint"), &p))
                        .child(kit::secondary_button("exchange-exchange-pick", Some(IconName::FolderOpen), t("export.import.choose"), act("exchange-pick"))),
                );
            }
            Some(preview) => {
                import = import
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(kit::icon(IconName::FileText, 16., p.accent))
                            .child(div().flex_1().min_w(px(0.)).truncate().font_weight(gpui_kit::FontWeight::MEDIUM).child(SharedString::from(preview.path.clone())))
                            .child(kit::pill(SharedString::from(format!("{} · {} {}", preview.format, preview.table.rows.len(), i18n::text(locale, "ui.grid.rows"))), p.accent_text, p.accent_soft)),
                    )
                    .child(kit::muted(t(if preview.truncated { "export.preview.truncated" } else { "export.preview.noWrite" }), &p));
                let mut mapping = div().flex().flex_col().gap(px(2.));
                if self.mapping.is_empty() {
                    mapping = mapping.child(kit::muted(t("export.map.empty"), &p));
                }
                for (field, source) in &self.mapping {
                    mapping = mapping.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .px(px(8.))
                            .py(px(4.))
                            .rounded(px(6.))
                            .bg(p.chrome)
                            .text_size(px(12.5))
                            .child(div().flex_1().min_w(px(0.)).truncate().child(SharedString::from(source.clone())))
                            .child(kit::icon(IconName::ArrowRight, 12., p.text_faint))
                            .child(div().flex_1().min_w(px(0.)).truncate().font_weight(gpui_kit::FontWeight::MEDIUM).child(SharedString::from(field.clone()))),
                    );
                }
                import = import
                    .child(div().flex().flex_col().gap(px(6.)).child(kit::eyebrow(t("export.map.title"), &p)).child(mapping))
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap(px(8.))
                            .child(kit::tool_button("exchange-exchange-cancel-import", None, t("export.import.cancel"), act("exchange-cancel-import")))
                            .child(kit::secondary_button("exchange-exchange-pick", Some(IconName::FolderOpen), t("export.import.choose"), act("exchange-pick")))
                            .child(kit::primary_button("exchange-exchange-confirm", Some(IconName::Check), t("export.import.confirm"), act("exchange-confirm"))),
                    );
            }
        }
        }
        panel = panel.child(section(t("ui.exchange.import"), import));

        // 任务
        let mut tasks = kit::card(&p).flex().flex_col();
        if self.log.tasks.is_empty() {
            tasks = tasks.child(div().p(px(14.)).text_size(px(12.5)).text_color(p.text_faint).child(t("export.task.empty")));
        }
        let count = self.log.tasks.len();
        for (index, task) in self.log.tasks.iter().enumerate() {
            let (fg, bg) = match task.phase {
                Phase::Running => (p.accent_text, p.accent_soft),
                Phase::Done => (p.ok, p.ok_soft),
                Phase::Failed => (p.danger, p.danger_soft),
                Phase::Partial | Phase::Blocked => (p.warn, p.warn_soft),
                Phase::Cancelled => (p.text_muted, p.hover),
            };
            tasks = tasks.child(
                div()
                    .px(px(14.))
                    .py(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .when(index + 1 < count, |this| this.border_b_1().border_color(p.line_soft))
                    .child(if task.running_now() { Spinner::new().xsmall().color(p.accent).into_any_element() } else { kit::status_dot(fg) })
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .child(div().truncate().font_weight(gpui_kit::FontWeight::MEDIUM).child(SharedString::from(task.kind.to_owned())))
                            .child(div().truncate().text_size(px(11.5)).text_color(p.text_faint).child(SharedString::from(if task.detail.is_empty() { task.path.clone().unwrap_or_default() } else { task.detail.clone() }))),
                    )
                    .child(div().text_size(px(12.)).text_color(p.text_muted).child(SharedString::from(format!("{} {}", task.done_rows, i18n::text(locale, "ui.grid.rows")))))
                    .child(kit::pill(t(task.label_key()), fg, bg)),
            );
        }
        panel = panel.child(section(
            t("export.task.title"),
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(tasks)
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap(px(6.))
                        .child(kit::tool_button("exchange-exchange-task-cancel", Some(IconName::CircleStop), t("export.task.cancel"), act("exchange-task-cancel")))
                        .child(kit::tool_button("exchange-exchange-clear", Some(IconName::BrushCleaning), t("export.task.clear"), act("exchange-clear"))),
                ),
        ));
        panel
    }
}

/// 来源不支持的操作：保留说明，不提供可点击但无效的入口。
fn blocked_row(id: &'static str, icon: IconName, title: SharedString, detail: &str, p: &Palette) -> gpui_kit::AnyElement {
    div()
        .id(id)
        .role(gpui_kit::Role::Button)
        .aria_label(title.clone())
        .p(px(10.))
        .flex()
        .items_center()
        .gap(px(12.))
        .rounded(px(9.))
        .border_1()
        .border_color(p.line)
        .opacity(0.55)
        .child(div().flex_none().size(px(30.)).rounded(px(8.)).bg(p.hover).flex().items_center().justify_center().child(kit::icon(icon, 15., p.text_faint)))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .child(div().text_color(p.text_muted).child(title))
                .child(div().text_size(px(11.5)).text_color(p.text_faint).child(SharedString::from(detail.to_owned()))),
        )
        .into_any_element()
}

fn action_row(id: &'static str, icon: IconName, title: SharedString, detail: SharedString, p: &Palette, on_click: impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut gpui_kit::App) + 'static) -> gpui_kit::AnyElement {
    div()
        .id(id)
        .role(gpui_kit::Role::Button)
        .aria_label(title.clone())
        .p(px(10.))
        .flex()
        .items_center()
        .gap(px(12.))
        .rounded(px(9.))
        .border_1()
        .border_color(p.line)
        .cursor_pointer()
        .hover(|style| style.border_color(p.accent).bg(p.accent_soft))
        .child(div().flex_none().size(px(30.)).rounded(px(8.)).bg(p.accent_soft).flex().items_center().justify_center().child(kit::icon(icon, 15., p.accent_text)))
        .child(div().flex_1().min_w(px(0.)).flex().flex_col().child(div().font_weight(gpui_kit::FontWeight::MEDIUM).child(title)).child(div().text_size(px(11.5)).text_color(p.text_faint).child(detail)))
        .child(kit::icon(IconName::ChevronRight, 14., p.text_faint))
        .on_click(on_click)
        .into_any_element()
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

    fn exchange_pane() -> ExchangePane {
        ExchangePane::new(Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(std::env::temp_dir().join(format!(
            "rillsql-exchange-source-{}",
            uuid::Uuid::new_v4()
        )))))))
    }

    fn table_source(table: &str, columns: &[&str]) -> ExchangeSource {
        ExchangeSource {
            kind: ExchangeSourceKind::Table,
            item: Some("table-1".to_owned()),
            table: Some(TableRef {
                connection_id: "notes".to_owned(),
                database: "main".to_owned(),
                schema: String::new(),
                table: table.to_owned(),
                engine: "sqlite".to_owned(),
                include_foreign_keys: false,
            }),
            columns: columns.iter().map(|name| (*name).to_owned()).collect(),
            rows: vec![vec![Some("甲".to_owned())]],
            truncated: false,
            running: false,
            label: table.to_owned(),
            context: "笔记 / main".to_owned(),
            preview_bytes: Some(crate::grid::DEFAULT_PREVIEW_BYTES),
        }
    }

    #[test]
    fn switching_sources_clears_the_mapping_and_drops_late_results() {
        let mut pane = exchange_pane();
        pane.bind_source(table_source("customers", &["id", "name"]));
        let old = pane.source_generation();
        pane.bind_source(table_source("orders", &["id", "total"]));
        let current = pane.source_generation();
        assert!(current > old, "切换目标必须推进来源版本");
        assert!(pane.mapping().is_empty(), "旧来源映射不能保留");

        let stale: BTreeMap<String, String> = [("id".to_owned(), "id".to_owned())].into_iter().collect();
        assert!(!pane.apply_mapping_for(old, stale.clone()), "迟到的旧来源映射不能写回");
        assert!(pane.mapping().is_empty());

        let fresh: BTreeMap<String, String> = [("total".to_owned(), "total".to_owned())].into_iter().collect();
        assert!(pane.apply_mapping_for(current, fresh.clone()));
        assert_eq!(pane.mapping(), &fresh);
    }

    fn query_source(running: bool) -> ExchangeSource {
        ExchangeSource {
            kind: ExchangeSourceKind::Query,
            item: Some("query-1".to_owned()),
            table: None,
            columns: vec!["label".to_owned()],
            rows: vec![vec![Some("真实".to_owned())]],
            truncated: false,
            running,
            label: "查询结果".to_owned(),
            context: "笔记 / main".to_owned(),
            preview_bytes: None,
        }
    }

    #[test]
    fn query_source_has_no_full_capability_and_reports_running_not_success() {
        let mut pane = exchange_pane();
        pane.bind_source(query_source(true));
        assert!(!pane.source().full_available(), "查询结果没有全量导出能力");
        let text = pane.lines().join("\n");
        assert!(text.contains("来源 当前查询结果"), "{text}");
        assert!(text.contains("目标 查询结果"), "{text}");
        assert!(text.contains("已读取 1 行"), "{text}");
        assert!(text.contains("查询正在执行"), "{text}");
        assert!(!text.contains("已完成"), "{text}");

        pane.bind_source(query_source(false));
        let text = pane.lines().join("\n");
        assert!(text.contains("全量导出不可用"), "{text}");
        assert!(!text.contains("查询正在执行"), "{text}");
    }

    #[test]
    fn export_range_limits_loaded_rows_and_resets_with_the_source() {
        let mut pane = exchange_pane();
        let mut source = table_source("customers", &["id", "name"]);
        source.rows = (0..150).map(|index| vec![Some(index.to_string()), Some("甲".to_owned())]).collect();
        pane.bind_source(source);
        pane.apply_range("100");
        let (rows, limited) = pane.rows_for_export();
        assert_eq!(rows.len(), 100);
        assert!(limited, "超过上限的已读取行必须标记截断");
        let text = pane.lines().join("\n");
        assert!(text.contains("范围 前 100 行"), "{text}");
        pane.apply_range("all");
        let (rows, limited) = pane.rows_for_export();
        assert_eq!(rows.len(), 150);
        assert!(!limited);
        pane.apply_range("1000");
        assert_eq!(pane.row_limit, Some(1000));
        pane.bind_source(table_source("orders", &["id"]));
        assert_eq!(pane.row_limit, None, "换来源后范围不能沿用");
    }

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

    /// 已读取行里还有预览值时导出会被挡住；读到原值后写的是原值，信封不会落进文件。
    #[test]
    fn loaded_export_refuses_previews_and_writes_the_original_after_the_shared_read() {
        let mut pane = exchange_pane();
        let mut source = table_source("docs", &["id", "body"]);
        source.rows = vec![vec![Some("1".to_owned()), Some("\u{1}t:4096:预览".to_owned())]];
        pane.bind_source(source);
        let path = std::env::temp_dir().join(format!("rillsql-loaded-{}.csv", uuid::Uuid::new_v4()));
        pane.start_loaded(path.clone());
        let text = pane.lines().join("\n");
        assert!(text.contains(&i18n::text(Locale::ZhCn, "grid.export.needFull")), "{text}");
        assert!(!path.exists(), "预览值不该写出文件");

        // 网格按定位读到原值后再导出：写的是同一个原值。
        let mut source = table_source("docs", &["id", "body"]);
        source.rows = vec![vec![Some("1".to_owned()), Some("完整正文".to_owned())]];
        pane.bind_source(source);
        pane.start_loaded(path.clone());
        let written = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(written.contains("完整正文"), "{written}");
        assert!(!written.contains('\u{1}'), "文件里不能出现预览信封");
        let _ = std::fs::remove_file(&path);
    }

    /// 协议形状的普通文本：是不是信封取决于读出时的上限。1 KB 上限下 3000 字节被截断，
    /// 4 KB/64 KB 上限下它就是原值，导出必须原样写出；查询结果没有经过截断，同样不挡。
    #[test]
    fn loaded_export_compares_protocol_shaped_text_against_the_sheet_limit() {
        let shaped = format!("\u{1}t:3000:{}", "a".repeat(32));
        let mut pane = exchange_pane();
        let mut source = table_source("docs", &["id", "body"]);
        source.rows = vec![vec![Some("1".to_owned()), Some(shaped.clone())]];
        source.preview_bytes = Some(1024);
        pane.bind_source(source);
        let path = std::env::temp_dir().join(format!("rillsql-loaded-limit-{}.csv", uuid::Uuid::new_v4()));
        pane.start_loaded(path.clone());
        let text = pane.lines().join("\n");
        assert!(text.contains(&i18n::text(Locale::ZhCn, "grid.export.needFull")), "1 KB 上限下是信封：{text}");
        assert!(!path.exists(), "信封不该写出文件");

        for limit in [4 * 1024, 64 * 1024] {
            let mut source = table_source("docs", &["id", "body"]);
            source.rows = vec![vec![Some("1".to_owned()), Some(shaped.clone())]];
            source.preview_bytes = Some(limit);
            pane.bind_source(source);
            pane.start_loaded(path.clone());
            let written = std::fs::read_to_string(&path).unwrap_or_default();
            assert!(written.contains(&shaped), "{limit} 上限下这段是原值：{written}");
            let _ = std::fs::remove_file(&path);
        }

        // 查询结果没有预览截断：协议形状的文本直接导出。
        let mut source = query_source(false);
        source.rows = vec![vec![Some(shaped.clone())]];
        source.columns = vec!["label".to_owned()];
        pane.bind_source(source);
        pane.start_loaded(path.clone());
        let written = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(written.contains(&shaped), "查询结果里的协议形状文本是原值：{written}");
        let _ = std::fs::remove_file(&path);
    }
}

