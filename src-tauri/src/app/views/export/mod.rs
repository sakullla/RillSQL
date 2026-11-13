//! 导入、导出、全库导出、跨库迁移和任务中心。确认前不写库，取消后不再显示运行中。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{Disableable as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, PathPromptOptions, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    TestSupportExt as _, Window, div, px,
};

use crate::ui::kit::{self, Palette, Radius, TypeScale};

use crate::application::AppServices;
use crate::export::{
    commit_import_guarded, export_full, open_import, prefer_cancel, watch_dump, watch_migrate, CancelPoint, ImportEntry, ImportPreview, LoadedExport, Phase, TaskLog,
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
    /// 来源分段与文字清单里的名称（i18n 键）。
    pub fn label_key(self) -> &'static str {
        match self {
            Self::Table => "ui.exchange.kind.table",
            Self::Query => "ui.exchange.kind.query",
        }
    }

    pub fn is_query(self) -> bool {
        matches!(self, Self::Query)
    }
}

/// 来源的四态（需求 R3 / 规范 S8.5、S12.4）：运行中优先于空态，没有表与没有结果是
/// 两条原因和下一步都不同的空态，就绪时才谈导出能力。四者互斥，界面只认这一份判定。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceState {
    /// 来源还在读：表数据读取中，或查询执行中。
    Running,
    /// 当前工作项没有表。
    NoTable,
    /// 有目标但还没有可用的列（查询没有结果、表的列信息未到）。
    NoResult,
    /// 可以导出已读取的行。
    Ready,
}

/// 交换面板里会被挡住的行动作。原因与下一步分开给出，界面、文字清单与测试共用这一份判定。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockedAction {
    /// 导出已读取的行。
    ExportLoaded,
    /// 导出全部结果（只有当前表数据支持）。
    ExportFull,
    /// 全库导出。
    Dump,
    /// 跨库迁移。
    Migrate,
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
    loaded: Option<Arc<crate::export::LoadedExport>>,
    loaded_control: Option<(String, Arc<crate::export::ExportControl>)>,
    dump_request: Option<DumpRequest>,
    migrate_request: Option<MigrateRequest>,
    pub notice: String,
    /// 来源切换由外壳决定工作项与来源，面板只把请求交回 Workbench。
    owner: Option<gpui_kit::WeakEntity<crate::app::shell::Workbench>>,
}

impl Drop for ExchangePane {
    fn drop(&mut self) {
        if let Some((_, control)) = &self.loaded_control { control.cancel(); }
    }
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
            loaded_control: None,
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
        self.loaded = Some(Arc::new(loaded));
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
        let limit = self.row_limit.unwrap_or(self.source.rows.len()).min(self.source.rows.len());
        (self.source.rows[..limit].to_vec(), limit < self.source.rows.len())
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
            self.notice = i18n::text(self.locale, "ui.exchange.importBusy");
            return;
        }
        let (Some(preview), Some(table)) = (self.preview.clone(), self.source.table.as_ref().map(copy_table)) else {
            self.notice = i18n::text(self.locale, "export.import.unconfirmed");
            return;
        };
        if let Some(key) = block_unmapped(self.mapping_edited, &self.mapping) {
            let message = i18n::text(self.locale, key);
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
                            Err(_) => TaskView::finish("import", "import", Phase::Failed, 0, i18n::text(this.locale, "ui.exchange.noTaskResult"), None),
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
                    self.block("full", "export", i18n::text(self.locale, "ui.exchange.fullQueryOnly"));
                    return;
                }
                SaveKind::Dump => {
                    self.block("dump", "dump", i18n::text(self.locale, "ui.exchange.dumpQueryOnly"));
                    return;
                }
                SaveKind::Loaded => {}
            }
        }
        if self.source.running {
            let (id, owner) = save_slot(kind);
            self.block(id, owner, i18n::text(self.locale, "ui.exchange.sourceRunning"));
            return;
        }
        if matches!(kind, SaveKind::Loaded) {
            if self.loaded_control.is_some() { self.notice = i18n::text(self.locale, "export.task.running"); return; }
            if let Some(loaded) = self.loaded.take() {
                self.launch_loaded(loaded, cx);
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
            SaveKind::Loaded => { if let Some(spec) = self.prepare_loaded(path) { self.launch_loaded(Arc::new(spec), cx); } },
            SaveKind::Full => self.start_full(path, cx),
            SaveKind::Dump => self.start_dump(path, cx),
        }
        cx.notify();
    }

    fn prepare_loaded(&mut self, path: PathBuf) -> Option<LoadedExport> {
        if self.loaded_control.is_some() { self.notice = i18n::text(self.locale, "export.task.running"); return None; }
        if self.source.running {
            self.block("loaded", "export", i18n::text(self.locale, "ui.exchange.queryRunningExport"));
            return None;
        }
        if !self.source.has_columns() {
            self.block("loaded", "export", self.empty_reason());
            return None;
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
            return None;
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
        Some(spec)
    }

    fn finish_loaded(&mut self, task: TaskView) {
        self.notice = task.detail.clone();
        self.log.push(task);
    }

    #[cfg(test)]
    fn start_loaded(&mut self, path: PathBuf) {
        if let Some(spec) = self.prepare_loaded(path) { self.finish_loaded(crate::export::export_loaded(&spec)); }
    }

    fn launch_loaded(&mut self, spec: Arc<LoadedExport>, cx: &mut Context<Self>) {
        if self.loaded_control.is_some() {
            self.notice = i18n::text(self.locale, "export.task.running");
            return;
        }
        let control = Arc::new(crate::export::ExportControl::default());
        self.loaded_control = Some((spec.id.clone(), Arc::clone(&control)));
        self.log.push(TaskView::running(&spec.id, "export", i18n::text(self.locale, "export.task.running")));
        let id = spec.id.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        crate::app::views::grid::runtime_for_views().spawn(async move {
            let result = tokio::task::spawn_blocking(move || spec.write_controlled(&control)).await;
            let task = result.unwrap_or_else(|error| TaskView::finish(id, "export", Phase::Failed, 0, error.to_string(), None));
            let _ = tx.send(task);
        });
        let owner = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let Ok(task) = rx.await else { return };
            cx.update(|cx| {
                let Some(owner) = owner.upgrade() else { return };
                owner.update(cx, |pane, cx| {
                    let cancelled = pane.log.tasks.iter().any(|old| old.id == task.id && old.phase == Phase::Cancelled);
                    if pane.loaded_control.as_ref().is_some_and(|(id, _)| id == &task.id) { pane.loaded_control = None; }
                    pane.finish_loaded(prefer_cancel(task, cancelled));
                    cx.notify();
                });
            });
        }).detach();
        cx.notify();
    }

    fn start_full(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.source.kind.is_query() {
            self.block("full", "export", i18n::text(self.locale, "ui.exchange.fullQueryOnly"));
            return;
        }
        if self.source.running {
            self.block("full", "export", i18n::text(self.locale, "ui.exchange.fullSourceRunning"));
            return;
        }
        let Some(table) = self.source.table.as_ref().map(copy_table) else {
            self.fail_open("full", "export", i18n::text(self.locale, "ui.exchange.fullNoTable"));
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
                            Err(_) => this.fail_open(&id, "export", i18n::text(this.locale, "ui.exchange.noTaskResult")),
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
            self.block("dump", "dump", i18n::text(self.locale, "ui.exchange.dumpQueryOnly"));
            return;
        }
        let Some(table) = self.source.table.as_ref().map(copy_table) else {
            self.fail_open("dump", "dump", i18n::text(self.locale, "ui.exchange.dumpNoConnection"));
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
            self.block("migrate", "migrate", i18n::text(self.locale, "ui.exchange.migrateQueryOnly"));
            return;
        }
        if self.source.running {
            self.block("migrate", "migrate", i18n::text(self.locale, "ui.exchange.migrateSourceRunning"));
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
            self.fail_open("migrate", "migrate", i18n::text(self.locale, "ui.exchange.migrateNoTable"));
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
        let locale = self.locale;
        cx.spawn(async move |_, cx| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::app::views::grid::runtime_for_views().spawn(async move {
                let result = async {
                    let target_id = open_sqlite_target(&services.connections, &path, locale).await?;
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
                            Err(_) => this.fail_open(&fail_id, "migrate", i18n::text(this.locale, "ui.exchange.noTaskResult")),
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
                            Err(_) => this.fail_open(&id, "dump", i18n::text(this.locale, "ui.exchange.noTaskResult")),
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
                            Err(_) => this.fail_open(&id, "migrate", i18n::text(this.locale, "ui.exchange.noTaskResult")),
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

    fn empty_reason(&self) -> String {
        let key = match self.source.kind {
            ExchangeSourceKind::Query if self.source.running => "ui.exchange.queryRunning",
            ExchangeSourceKind::Query => "ui.exchange.queryEmpty",
            ExchangeSourceKind::Table if self.source.running => "ui.exchange.tableLoading",
            ExchangeSourceKind::Table => "ui.exchange.noTableData",
        };
        i18n::text(self.locale, key)
    }

    /// 来源的四态。运行中优先于空态（加载优先于空），没有表与没有结果是两条不同的空态。
    fn source_state(&self) -> SourceState {
        if self.source.running {
            return SourceState::Running;
        }
        if self.source.kind == ExchangeSourceKind::Table && self.source.table.is_none() {
            return SourceState::NoTable;
        }
        if !self.source.has_columns() {
            return SourceState::NoResult;
        }
        SourceState::Ready
    }

    /// 运行中那一行的说明：表数据在读取，还是查询在执行。
    fn running_key(&self) -> &'static str {
        match self.source.kind {
            ExchangeSourceKind::Query => "ui.exchange.queryRunning",
            ExchangeSourceKind::Table => "ui.exchange.tableLoading",
        }
    }

    /// 没有可用结果时的说明：查询还没执行，还是表的列信息还没读到。
    fn no_result_key(&self) -> &'static str {
        match self.source.kind {
            ExchangeSourceKind::Query => "ui.exchange.noResult",
            ExchangeSourceKind::Table => "ui.exchange.noTableColumns",
        }
    }

    /// 全量导出为什么不能用：来源是查询结果、来源还在执行、没有当前表。
    fn full_block_key(&self) -> &'static str {
        if self.source.kind.is_query() {
            "ui.exchange.fullQueryOnly"
        } else if self.source.running {
            "ui.exchange.fullSourceRunning"
        } else {
            "ui.exchange.fullNoTable"
        }
    }

    /// 被挡住的行动作的原因（为什么现在不能做）。与文字清单同源，不各写一份。
    fn blocked_reason(&self, action: BlockedAction) -> &'static str {
        match action {
            BlockedAction::ExportLoaded => match (self.source.running, self.source.kind) {
                (true, _) => self.running_key(),
                (false, ExchangeSourceKind::Query) => "ui.exchange.queryEmpty",
                (false, ExchangeSourceKind::Table) => "ui.exchange.noTableData",
            },
            BlockedAction::ExportFull => self.full_block_key(),
            BlockedAction::Dump => {
                if self.source.kind.is_query() {
                    "ui.exchange.dumpQueryOnly"
                } else {
                    "ui.exchange.dumpNoConnection"
                }
            }
            BlockedAction::Migrate => match (self.source.kind.is_query(), self.source.running) {
                (true, _) => "ui.exchange.migrateQueryOnly",
                (false, true) => "ui.exchange.migrateSourceRunning",
                (false, false) => "ui.exchange.migrateNoTable",
            },
        }
    }

    /// 同一个行动作的下一步（然后能做什么）。原因与下一步在界面上分开呈现（S8.5）。
    fn blocked_next(&self, _action: BlockedAction) -> &'static str {
        match (self.source_state(), self.source.kind) {
            (SourceState::Running, _) => "ui.exchange.next.waitRead",
            (SourceState::NoTable, _) => "ui.exchange.next.openTable",
            (SourceState::NoResult, ExchangeSourceKind::Query) => "ui.exchange.next.runQuery",
            (SourceState::NoResult, ExchangeSourceKind::Table) => "ui.exchange.next.refresh",
            // 已经就绪却仍被挡住，只剩来源类型不允许（查询结果不能全量导出/全库导出/跨库迁移）。
            (SourceState::Ready, _) => "ui.exchange.next.switchToTable",
        }
    }

    fn mark_cancelled(&mut self, kind: &str) {
        if let Some(task) = self.log.tasks.iter_mut().rev().find(|task| task.running_now() && task.kind == kind) {
            let done = task.done_rows;
            task.phase = Phase::Cancelled;
            task.success = false;
            task.detail = i18n::fill(&i18n::text(self.locale, "ui.exchange.cancelledRange"), &[("done", &done.to_string())]);
            self.notice = task.detail.clone();
        }
    }

    fn cancel_current(&mut self) {
        if let Some(task) = self.log.tasks.iter_mut().rev().find(|task| task.running_now()) {
            let id = task.id.clone();
            if let Some((running, control)) = &self.loaded_control {
                if running == &id { control.cancel(); }
            }
            let kind = task.kind;
            let done = task.done_rows;
            task.phase = Phase::Cancelled;
            task.success = false;
            task.detail = i18n::fill(&i18n::text(self.locale, "ui.exchange.cancelledRange"), &[("done", &done.to_string())]);
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
        lines.push(i18n::fill(&i18n::text(self.locale, "ui.exchange.line.source"), &[("kind", &i18n::text(self.locale, self.source.kind.label_key()))]));
        match (&self.source.kind, &self.source.table) {
            (ExchangeSourceKind::Table, Some(table)) => {
                lines.push(i18n::fill(&i18n::text(self.locale, "ui.exchange.line.target"), &[("target", &table.table)]));
                if !self.source.context.is_empty() {
                    lines.push(i18n::fill(&i18n::text(self.locale, "ui.exchange.line.connection"), &[("context", &self.source.context)]));
                }
            }
            (ExchangeSourceKind::Table, None) => lines.push(i18n::text(self.locale, "ui.exchange.line.noTarget")),
            (ExchangeSourceKind::Query, _) => {
                lines.push(i18n::text(self.locale, "ui.exchange.line.queryTarget"));
                if !self.source.context.is_empty() {
                    lines.push(i18n::fill(&i18n::text(self.locale, "ui.exchange.line.connection"), &[("context", &self.source.context)]));
                }
            }
        }
        lines.push(i18n::fill(&i18n::text(self.locale, "ui.exchange.loadedRows"), &[("count", &self.source.rows.len().to_string()), ("partial", "")]));
        if self.source.truncated {
            lines.push(i18n::text(self.locale, "ui.exchange.line.partial"));
        }
        if self.source.kind.is_query() && !self.source.full_available() {
            lines.push(i18n::text(self.locale, "ui.exchange.line.noFull"));
        }
        if self.source.running {
            lines.push(i18n::text(self.locale, "ui.exchange.queryRunning"));
        } else if !self.source.has_columns() {
            lines.push(self.empty_reason());
        }
        lines.push(i18n::fill(&i18n::text(self.locale, "ui.exchange.line.format"), &[("format", &self.format_name)]));
        lines.push(match self.row_limit {
            Some(limit) => i18n::fill(&i18n::text(self.locale, "ui.exchange.line.rangeFirst"), &[("limit", &limit.to_string())]),
            None => i18n::text(self.locale, "ui.exchange.line.rangeAll"),
        });
        if self.mapping.is_empty() {
            if self.preview.is_some() && self.source.table.is_some() {
                lines.push(i18n::text(self.locale, "export.map.empty"));
            }
        } else {
            lines.push(i18n::text(self.locale, "export.map.title"));
            for (field, source) in &self.mapping {
                lines.push(i18n::fill(&i18n::text(self.locale, "ui.exchange.line.mapping"), &[("field", field), ("source", source)]));
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some((id, control)) = &self.loaded_control {
            if let Some(task) = self.log.tasks.iter_mut().find(|task| &task.id == id) { task.done_rows = control.done_rows(); }
        }
        if !self.open {
            return div().id("exchange-idle");
        }
        let p = Palette::of(cx);
        // 抽屉最窄时也要留住内容：页边距按视口钳到紧凑档，宽度由容器分配，面板自己不写死像素。
        let metrics = kit::Metrics::of(cx).for_viewport(f32::from(window.viewport_size().width));
        let state = self.source_state();
        let locale = self.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let entity = cx.entity();
        let act = |id: &'static str| {
            let entity = entity.clone();
            move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut gpui_kit::App| entity.update(cx, |pane, cx| pane.command(id, cx))
        };
        let section = |title: SharedString, body: gpui_kit::AnyElement| div().flex().flex_col().gap(px(8.)).child(kit::eyebrow(title, &p)).child(body);
        // 空态的主操作：重新读取来源。它和分段控件一样走外壳，面板不自己猜来源。
        let refresh = {
            let entity = entity.clone();
            let label = t("ui.exchange.state.refresh");
            move |id: &'static str| {
                let entity = entity.clone();
                kit::secondary_button(id, Some(IconName::RefreshCw), label.clone(), move |_, _, cx: &mut gpui_kit::App| {
                    let entity = entity.clone();
                    entity.update(cx, |pane, cx| {
                        let id = if pane.source.kind.is_query() { "exchange-source:query" } else { "exchange-source:table" };
                        pane.request_source(id, cx);
                    });
                })
            }
        };

        let mut panel = div()
            .id("exchange")
            .role(gpui_kit::Role::Dialog)
            .aria_label(t("export.task.title"))
            .flex()
            .flex_col()
            .gap(px(metrics.section_gap()))
            .p(px(metrics.page_pad));
        if !self.notice.is_empty() {
            panel = panel.child(div().px(px(12.)).py(px(8.)).rounded(px(Radius::PANEL)).bg(p.hover).text_size(px(TypeScale::SM)).child(SharedString::from(self.notice.clone())));
        }

        // 来源选择：当前表数据或当前查询结果，两种来源的列、行与能力互不混用。
        let mut kinds = Vec::new();
        for (kind, id, label) in [
            (ExchangeSourceKind::Table, "exchange-source:table", t(ExchangeSourceKind::Table.label_key())),
            (ExchangeSourceKind::Query, "exchange-source:query", t(ExchangeSourceKind::Query.label_key())),
        ] {
            let entity = entity.clone();
            kinds.push((
                SharedString::from(format!("exchange-{id}")),
                label,
                self.source.kind == kind,
                kit::click(move |_, _, cx| {
                    let id = id.to_owned();
                    entity.update(cx, |pane, cx| pane.request_source(&id, cx));
                }),
            ));
        }
        let mut source_body = div().flex().flex_col().gap(px(6.));
        // 标题行只说明"现在在交换什么"；没有表时由下面的统一空态说明原因与下一步，不再自绘灰字。
        if let (ExchangeSourceKind::Table, Some(table)) = (self.source.kind, &self.source.table) {
            source_body = source_body.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(kit::engine_badge(&table.engine, 18.))
                    .child(div().font_weight(gpui_kit::FontWeight::MEDIUM).child(SharedString::from(table.table.clone())))
                    .child(kit::muted(SharedString::from(table.database.clone()), &p)),
            );
        } else if self.source.kind == ExchangeSourceKind::Query {
            source_body = source_body.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(kit::icon(IconName::SquareTerminal, 15., p.accent))
                    .child(div().font_weight(gpui_kit::FontWeight::MEDIUM).child(t("ui.exchange.queryResult")))
                    .child(kit::muted(SharedString::from(self.source.context.clone()), &p)),
            );
        }
        // 来源的四态在这里统一呈现：运行中是局部加载（不整块换掉面板），没有表与没有结果是
        // 两种文案不同的空态，各自带原因和下一步；就绪时才补已读取行数。
        match state {
            SourceState::Running => {
                source_body = source_body
                    .child(kit::inline_loading(t(self.running_key()), &metrics, &p))
                    .child(div().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(t("ui.exchange.next.waitRead")));
            }
            SourceState::NoTable => {
                source_body = source_body.child(
                    kit::empty_state_with_action(
                        IconName::Table,
                        t("ui.exchange.state.noTableTitle"),
                        t("ui.exchange.noTableData"),
                        refresh("exchange-source-refresh-no-table"),
                        &p,
                    )
                    .into_any_element(),
                );
            }
            SourceState::NoResult => {
                source_body = source_body.child(
                    kit::empty_state_with_action(
                        IconName::ListOrdered,
                        t("ui.exchange.state.noResultTitle"),
                        t(self.no_result_key()),
                        refresh("exchange-source-refresh-no-result"),
                        &p,
                    )
                    .into_any_element(),
                );
            }
            SourceState::Ready => {
                let partial = if self.source.truncated { i18n::text(locale, "ui.exchange.partialMark") } else { String::new() };
                source_body = source_body.child(
                    div()
                        .text_size(px(TypeScale::SM))
                        .text_color(p.text_muted)
                        .child(SharedString::from(i18n::fill(&i18n::text(locale, "ui.exchange.loadedRows"), &[("count", &self.source.rows.len().to_string()), ("partial", &partial)]))),
                );
                if self.source.kind.is_query() {
                    source_body = source_body.child(
                        div()
                            .text_size(px(TypeScale::XS))
                            .text_color(p.text_faint)
                            .child(t("ui.exchange.loadedOnly")),
                    );
                }
            }
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
        for (id, label, limit) in [("all", t("ui.exchange.range.all"), None), ("100", t("ui.exchange.range.first100"), Some(100)), ("1000", t("ui.exchange.range.first1000"), Some(1000))] {
            let entity = entity.clone();
            let command = format!("exchange-range:{id}");
            ranges.push((
                SharedString::from(format!("exchange-exchange-range:{id}")),
                label,
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
                .id("exchange-source-card")
                .test_support()
                .p(px(14.))
                .flex()
                .flex_col()
                // 抽屉最窄时内容按面板宽收窄，不撑出横向滚动。
                .min_w(px(0.))
                .max_w_full()
                .gap(px(metrics.section_gap()))
                .child(kit::segmented_sized(kinds, &p, metrics.control_h))
                .child(source_body)
                .child(kit::form_row(t("ui.exchange.format"), kit::segmented_sized(formats, &p, metrics.control_h), &p))
                .when(range_enabled, |this| {
                    this.child(kit::form_row(
                        t("ui.exchange.range"),
                        kit::segmented_sized(ranges, &p, metrics.control_h),
                        &p,
                    ))
                })
                .into_any_element(),
        ));

        // 导出：不可用的能力说明原因与下一步，而不是留一个点了没反应的入口。
        let loaded_ready = state == SourceState::Ready;
        let mut export = div().id("exchange-export-card").test_support().flex().flex_col().gap(px(6.)).min_w(px(0.)).max_w_full();
        if loaded_ready {
            export = export.child(action_row(
                "exchange-exchange-export-loaded",
                IconName::FileDown,
                t("export.export.loaded"),
                SharedString::from(i18n::fill(&i18n::text(locale, "ui.exchange.loadedOnlyCount"), &[("count", &self.source.rows.len().to_string())])),
                &p,
                act("exchange-export-loaded"),
            ));
        } else {
            export = export.child(blocked_row(
                "exchange-exchange-export-loaded",
                IconName::FileDown,
                t("export.export.loaded"),
                &i18n::text(locale, self.blocked_reason(BlockedAction::ExportLoaded)),
                &i18n::text(locale, self.blocked_next(BlockedAction::ExportLoaded)),
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
                &i18n::text(locale, self.blocked_reason(BlockedAction::ExportFull)),
                &i18n::text(locale, self.blocked_next(BlockedAction::ExportFull)),
                &p,
            ));
        }
        // 全库导出与跨库迁移都要有当前表；来源还在执行时迁移也要等。
        if self.source.kind.is_query() || self.source.table.is_none() {
            export = export
                .child(blocked_row(
                    "exchange-exchange-dump",
                    IconName::DatabaseBackup,
                    t("export.dump.start"),
                    &i18n::text(locale, self.blocked_reason(BlockedAction::Dump)),
                    &i18n::text(locale, self.blocked_next(BlockedAction::Dump)),
                    &p,
                ))
                .child(blocked_row(
                    "exchange-exchange-migrate",
                    IconName::ArrowRightLeft,
                    t("export.migrate.start"),
                    &i18n::text(locale, self.blocked_reason(BlockedAction::Migrate)),
                    &i18n::text(locale, self.blocked_next(BlockedAction::Migrate)),
                    &p,
                ));
        } else if self.source.running {
            export = export
                .child(action_row("exchange-exchange-dump", IconName::DatabaseBackup, t("export.dump.start"), t("ui.exchange.dumpHint"), &p, act("exchange-dump")))
                .child(blocked_row(
                    "exchange-exchange-migrate",
                    IconName::ArrowRightLeft,
                    t("export.migrate.start"),
                    &i18n::text(locale, self.blocked_reason(BlockedAction::Migrate)),
                    &i18n::text(locale, self.blocked_next(BlockedAction::Migrate)),
                    &p,
                ));
        } else {
            export = export
                .child(action_row("exchange-exchange-dump", IconName::DatabaseBackup, t("export.dump.start"), t("ui.exchange.dumpHint"), &p, act("exchange-dump")))
                .child(action_row("exchange-exchange-migrate", IconName::ArrowRightLeft, t("export.migrate.start"), t("ui.exchange.migrateHint"), &p, act("exchange-migrate")));
        }
        panel = panel.child(section(t("ui.exchange.export"), export.into_any_element()));

        // 导入：写入目标必须来自当前表来源。查询结果不能写回，所以这里给的是
        // 一条空态（原因 + 下一步），主操作直接把来源切到当前表数据。
        let mut import = kit::card(&p)
            .id("exchange-import-card")
            .test_support()
            .p(px(14.))
            .flex()
            .flex_col()
            .min_w(px(0.))
            .max_w_full()
            .gap(px(metrics.section_gap()));
        if self.source.kind.is_query() {
            let action = {
                let entity = entity.clone();
                kit::secondary_button("exchange-import-use-table", Some(IconName::Table), t("ui.exchange.useTable"), move |_, _, cx: &mut gpui_kit::App| {
                    let entity = entity.clone();
                    entity.update(cx, |pane, cx| pane.request_source("exchange-source:table", cx));
                })
            };
            import = import.child(
                kit::empty_state_with_action(
                    IconName::FileUp,
                    t("ui.exchange.importNeedsTable"),
                    t("ui.exchange.importNeedsTableHint"),
                    action,
                    &p,
                )
                .into_any_element(),
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
                        .rounded(px(Radius::PANEL))
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
                            .rounded(px(Radius::CONTROL))
                            .bg(p.chrome)
                            .text_size(px(TypeScale::SM))
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
                            .flex_wrap()
                            .justify_end()
                            // 窄面板里三个动作换行而不是挤出面板（R4：窄宽主操作仍可达）。
                            .min_w(px(0.))
                            .gap(px(8.))
                            .child(kit::tool_button("exchange-exchange-cancel-import", None, t("export.import.cancel"), act("exchange-cancel-import")))
                            .child(kit::secondary_button("exchange-exchange-pick", Some(IconName::FolderOpen), t("export.import.choose"), act("exchange-pick")))
                            .child(kit::primary_button("exchange-exchange-confirm", Some(IconName::Check), t("export.import.confirm"), act("exchange-confirm"))),
                    );
            }
        }
        }
        panel = panel.child(section(t("ui.exchange.import"), import.into_any_element()));

        // 任务：没有任务时给空态（说明 + 下一步），不再是一行灰字。
        let mut tasks = kit::card(&p).id("exchange-tasks").test_support().flex().flex_col().min_w(px(0.)).max_w_full();
        if self.log.tasks.is_empty() {
            let empty = if loaded_ready {
                kit::empty_state_with_action(
                    IconName::ListOrdered,
                    t("ui.exchange.state.tasksTitle"),
                    t("ui.exchange.state.tasksDetail"),
                    kit::secondary_button("exchange-tasks-export", Some(IconName::FileDown), t("export.export.loaded"), act("exchange-export-loaded")),
                    &p,
                )
                .into_any_element()
            } else {
                kit::empty_state(IconName::ListOrdered, t("ui.exchange.state.tasksTitle"), t("ui.exchange.state.tasksDetail"), &p).into_any_element()
            };
            tasks = tasks.child(empty);
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
                            .child(div().truncate().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(SharedString::from(if task.detail.is_empty() { task.path.clone().unwrap_or_default() } else { task.detail.clone() }))),
                    )
                    .child(div().text_size(px(TypeScale::SM)).text_color(p.text_muted).child(SharedString::from(format!("{} {}", task.done_rows, i18n::text(locale, "ui.grid.rows")))))
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
                        .flex_wrap()
                        .justify_end()
                        .min_w(px(0.))
                        .gap(px(6.))
                        // 没有可取消/可清除的任务时按钮就是禁用态：不可聚焦、无 hover、点了没反应要有原因。
                        .child(
                            kit::tool_button("exchange-exchange-task-cancel", Some(IconName::CircleStop), t("export.task.cancel"), act("exchange-task-cancel"))
                                .disabled(!self.log.tasks.iter().any(|task| task.running_now())),
                        )
                        .child(
                            kit::tool_button("exchange-exchange-clear", Some(IconName::BrushCleaning), t("export.task.clear"), act("exchange-clear"))
                                .disabled(!self.log.tasks.iter().any(|task| !task.running_now())),
                        ),
                )
                .into_any_element(),
        ));
        panel
    }
}

/// 现在不能做的行动作：保留标题，分开给出原因（为什么不能做）与下一步（然后做什么），
/// 并走统一禁用语义（不可聚焦、无 hover/active、not-allowed 光标）而不是只调低不透明度。
fn blocked_row(id: &'static str, icon: IconName, title: SharedString, reason: &str, next: &str, p: &Palette) -> gpui_kit::AnyElement {
    // 可访问名称带上原因与下一步：读屏用户听到的和屏幕上看到的是同一句话。
    let label = SharedString::from(format!("{title}：{reason} {next}"));
    kit::interactive_states(div().id(id), p, true)
        .role(gpui_kit::Role::Button)
        .aria_label(label)
        .test_support()
        .p(px(10.))
        .flex()
        .items_center()
        .gap(px(12.))
        .rounded(px(Radius::PANEL))
        .border_1()
        .border_color(p.line)
        .min_w(px(0.))
        .child(div().flex_none().size(px(30.)).rounded(px(Radius::PANEL)).bg(p.hover).flex().items_center().justify_center().child(kit::icon(icon, 15., p.text_faint)))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(div().text_color(p.text_muted).child(title))
                .child(div().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(SharedString::from(reason.to_owned())))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .min_w(px(0.))
                        .child(kit::icon(IconName::ChevronRight, 12., p.text_faint))
                        .child(div().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(SharedString::from(next.to_owned()))),
                ),
        )
        .into_any_element()
}

fn action_row(id: &'static str, icon: IconName, title: SharedString, detail: SharedString, p: &Palette, on_click: impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut gpui_kit::App) + 'static) -> gpui_kit::AnyElement {
    kit::interactive_states(div().id(id), p, false)
        .role(gpui_kit::Role::Button)
        .aria_label(title.clone())
        .p(px(10.))
        .flex()
        .items_center()
        .gap(px(12.))
        .rounded(px(Radius::PANEL))
        .border_1()
        .border_color(p.line)
        .hover(|style| style.border_color(p.accent).bg(p.accent_soft))
        .child(div().flex_none().size(px(30.)).rounded(px(Radius::PANEL)).bg(p.accent_soft).flex().items_center().justify_center().child(kit::icon(icon, 15., p.accent_text)))
        .child(div().flex_1().min_w(px(0.)).flex().flex_col().child(div().font_weight(gpui_kit::FontWeight::MEDIUM).child(title)).child(div().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(detail)))
        .child(kit::icon(IconName::ChevronRight, 14., p.text_faint))
        .on_click(on_click)
        .test_support()
        .into_any_element()
}

#[derive(Clone, Copy)]
enum SaveKind {
    Loaded,
    Full,
    Dump,
}

fn block_unmapped(edited: bool, mapping: &BTreeMap<String, String>) -> Option<&'static str> {
    if mapping.values().any(|source| !source.is_empty()) {
        return None;
    }
    if edited {
        Some("export.import.gaps")
    } else {
        Some("ui.exchange.noMapping")
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

async fn open_sqlite_target(service: &crate::connections::Service, path: &std::path::Path, locale: Locale) -> Result<String, String> {
    let mut draft = crate::app::views::connections::blank_draft(crate::connections::model::Engine::Sqlite);
    draft.id = Some("exchange-migrate-target".to_owned());
    draft.name = i18n::text(locale, "ui.exchange.migrateTarget");
    draft.file_path = path.display().to_string();
    let saved = service.save(draft, crate::connections::model::SecretInput::default()).await.map_err(|error| error.message())?;
    let outcome = service.open(&saved.connection.id, crate::connections::model::SecretInput::default()).await?;
    if !outcome.connected {
        return Err(outcome.report.summary.unwrap_or_else(|| i18n::text(locale, "ui.exchange.migrateTargetClosed")));
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

    /// 英文界面里导入导出面板的文字（来源分段、文字清单、不可用原因、取消说明）不含中文。
    #[test]
    fn english_exchange_copy_has_no_chinese() {
        let cjk = |text: &str| text.chars().any(|ch| ('\u{4e00}'..='\u{9fff}').contains(&ch) || ('\u{3000}'..='\u{303f}').contains(&ch) || ('\u{ff00}'..='\u{ffef}').contains(&ch));
        let mut pane = exchange_pane();
        pane.set_locale(Locale::En);
        let ascii = |mut source: ExchangeSource| {
            source.context = "notes / main".to_owned();
            source.rows = vec![vec![Some("a".to_owned())]];
            source.label = "result".to_owned();
            source
        };
        let mut empty_table = ascii(table_source("orders", &[]));
        empty_table.columns.clear();
        let mut loading = ascii(table_source("orders", &["id"]));
        loading.running = true;
        loading.truncated = true;
        let mut no_table = ascii(table_source("orders", &[]));
        no_table.table = None;
        no_table.columns.clear();
        let mut empty_query = ascii(query_source(false));
        empty_query.columns.clear();
        for source in [ascii(table_source("orders", &["id"])), empty_table, loading, no_table, ascii(query_source(true)), ascii(query_source(false)), empty_query] {
            pane.bind_source(source);
            for range in ["all", "100"] {
                pane.apply_range(range);
                let text = pane.lines().join("\n");
                assert!(!cjk(&text), "英文清单残留中文：{text}");
                assert!(!cjk(&pane.empty_reason()), "{}", pane.empty_reason());
            }
        }
        for kind in [ExchangeSourceKind::Table, ExchangeSourceKind::Query] {
            let label = i18n::text(Locale::En, kind.label_key());
            assert!(!cjk(&label) && i18n::text(Locale::ZhCn, kind.label_key()).starts_with("当前"), "{label}");
        }
        pane.log.push(TaskView::running("dump-1", "dump", "running"));
        pane.cancel_current();
        assert!(!cjk(&pane.notice) && pane.notice.starts_with("Cancelled"), "{}", pane.notice);
        for key in [Some("export.import.gaps"), Some("ui.exchange.noMapping")] {
            assert!(!cjk(&i18n::text(Locale::En, key.unwrap())));
        }
        assert_eq!(block_unmapped(false, &BTreeMap::new()), Some("ui.exchange.noMapping"));
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


#[cfg(test)]
mod background_export_tests {
    use super::*;
    use gpui_kit::AppContext as _;

    #[gpui_kit::test]
    fn loaded_file_write_runs_in_background_and_releases_its_snapshot(cx: &mut gpui_kit::TestAppContext) {
        cx.executor().allow_parking();
        let dir = std::env::temp_dir().join(format!("rillsql-background-export-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("large.csv");
        let services = Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
        let pane = cx.update(|cx| cx.new(|_| ExchangePane::new(services)));
        let spec = Arc::new(crate::export::LoadedExport {
            id: "background".into(), format: "csv".into(), path: path.clone(), columns: vec!["body".into()],
            rows: (0..10_000).map(|_| vec![Some("x".repeat(1024))]).collect(), truncated: false,
            delimiter: ',', null_token: "NULL".into(), bom: false, table: "items".into(), schema: String::new(), cancel_after_rows: None, fail_after_rows: None,
        });
        let snapshot = Arc::downgrade(&spec);
        cx.update(|cx| pane.update(cx, |pane, cx| {
            pane.launch_loaded(spec, cx);
            assert!(pane.running());
            pane.command("exchange-format:json", cx);
            assert_eq!(pane.format_name, "json", "写文件期间仍能操作界面状态");
        }));
        let start = std::time::Instant::now();
        loop {
            cx.run_until_parked();
            if cx.update(|cx| !pane.read(cx).running()) { break; }
            assert!(start.elapsed() < std::time::Duration::from_secs(30));
            std::thread::park_timeout(std::time::Duration::from_millis(5));
        }
        cx.update(|cx| {
            assert_eq!(pane.read(cx).tasks()[0].phase, Phase::Done);
            assert_eq!(pane.read(cx).tasks()[0].done_rows, 10_000);
        });
        assert!(std::fs::metadata(&path).unwrap().len() > 10_000_000);
        assert!(snapshot.upgrade().is_none(), "完成后不得继续持有导出数据快照");
        let _ = std::fs::remove_dir_all(dir);
    }
}
#[cfg(test)]
mod unified_state_tests {
    use super::*;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{AppContext as _, Point, TestAppContext, WindowBounds, WindowOptions, px, size};

    fn services() -> Arc<AppServices> {
        let dir = std::env::temp_dir().join(format!("rillsql-exchange-state-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))))
    }

    fn pane() -> ExchangePane {
        ExchangePane::new(services())
    }

    fn rows(columns: &[&str]) -> Vec<Vec<Option<String>>> {
        if columns.is_empty() {
            Vec::new()
        } else {
            vec![columns.iter().map(|name| Some((*name).to_owned())).collect()]
        }
    }

    /// 表来源：table 为 None 就是“当前没有表”，列清空就是“没有可用结果”。
    fn table_source(table: Option<&str>, columns: &[&str], running: bool) -> ExchangeSource {
        ExchangeSource {
            kind: ExchangeSourceKind::Table,
            item: Some("table-1".to_owned()),
            table: table.map(|name| TableRef {
                connection_id: "notes".to_owned(),
                database: "main".to_owned(),
                schema: String::new(),
                table: name.to_owned(),
                engine: "sqlite".to_owned(),
                include_foreign_keys: false,
            }),
            columns: columns.iter().map(|name| (*name).to_owned()).collect(),
            rows: rows(columns),
            truncated: false,
            running,
            label: "orders".to_owned(),
            context: "笔记 / main".to_owned(),
            preview_bytes: None,
        }
    }

    fn query_source(columns: &[&str], running: bool) -> ExchangeSource {
        ExchangeSource {
            kind: ExchangeSourceKind::Query,
            item: Some("query-1".to_owned()),
            table: None,
            columns: columns.iter().map(|name| (*name).to_owned()).collect(),
            rows: rows(columns),
            truncated: false,
            running,
            label: "结果".to_owned(),
            context: "笔记 / main".to_owned(),
            preview_bytes: None,
        }
    }

    /// 四态互斥且顺序固定：运行中优先于空态，没有表与没有结果各自成立，就绪才谈导出。
    #[test]
    fn source_state_separates_running_no_table_no_result_and_ready() {
        let mut pane = pane();
        for (source, expected) in [
            (table_source(Some("orders"), &["id"], false), SourceState::Ready),
            (table_source(Some("orders"), &["id"], true), SourceState::Running),
            (table_source(None, &[], false), SourceState::NoTable),
            (table_source(Some("orders"), &[], false), SourceState::NoResult),
            (query_source(&["label"], false), SourceState::Ready),
            (query_source(&[], false), SourceState::NoResult),
            (query_source(&["label"], true), SourceState::Running),
        ] {
            pane.bind_source(source);
            assert_eq!(pane.source_state(), expected);
        }
        // 运行中压过其它判定：即使查询一项的列已经清空，也先报“读取中”。
        pane.bind_source(query_source(&[], true));
        assert_eq!(pane.source_state(), SourceState::Running);
    }

    /// 每条被挡住的行动作都给出自己的原因，并补一个与原因不同的下一步。
    #[test]
    fn blocked_actions_name_the_reason_and_the_next_step() {
        let mut pane = pane();

        pane.bind_source(query_source(&["label"], true));
        assert_eq!(pane.blocked_reason(BlockedAction::ExportLoaded), "ui.exchange.queryRunning");
        assert_eq!(pane.blocked_next(BlockedAction::ExportLoaded), "ui.exchange.next.waitRead");
        assert_eq!(pane.blocked_reason(BlockedAction::ExportFull), "ui.exchange.fullQueryOnly");
        assert_eq!(pane.blocked_reason(BlockedAction::Dump), "ui.exchange.dumpQueryOnly");
        assert_eq!(pane.blocked_reason(BlockedAction::Migrate), "ui.exchange.migrateQueryOnly");

        pane.bind_source(table_source(None, &[], false));
        assert_eq!(pane.blocked_reason(BlockedAction::ExportLoaded), "ui.exchange.noTableData");
        assert_eq!(pane.blocked_next(BlockedAction::ExportLoaded), "ui.exchange.next.openTable");
        assert_eq!(pane.blocked_reason(BlockedAction::ExportFull), "ui.exchange.fullNoTable");
        assert_eq!(pane.blocked_reason(BlockedAction::Dump), "ui.exchange.dumpNoConnection");
        assert_eq!(pane.blocked_reason(BlockedAction::Migrate), "ui.exchange.migrateNoTable");

        pane.bind_source(query_source(&[], false));
        assert_eq!(pane.blocked_reason(BlockedAction::ExportLoaded), "ui.exchange.queryEmpty");
        assert_eq!(pane.blocked_next(BlockedAction::ExportLoaded), "ui.exchange.next.runQuery");

        pane.bind_source(table_source(Some("orders"), &["id"], true));
        assert_eq!(pane.blocked_reason(BlockedAction::Migrate), "ui.exchange.migrateSourceRunning");
        assert_eq!(pane.blocked_next(BlockedAction::Migrate), "ui.exchange.next.waitRead");

        // 就绪却仍被挡住：只剩来源类型不允许，下一步是切到当前表数据。
        pane.bind_source(query_source(&["label"], false));
        assert_eq!(pane.blocked_next(BlockedAction::ExportFull), "ui.exchange.next.switchToTable");

        // 原因与下一步是两句不同的话，不能把状态说明当下一步用。
        for source in [table_source(None, &[], false), query_source(&[], false), query_source(&["label"], false)] {
            pane.bind_source(source);
            for action in [BlockedAction::ExportLoaded, BlockedAction::ExportFull, BlockedAction::Dump, BlockedAction::Migrate] {
                let reason = i18n::text(Locale::ZhCn, pane.blocked_reason(action));
                let next = i18n::text(Locale::ZhCn, pane.blocked_next(action));
                assert!(!reason.is_empty(), "{action:?}");
                assert!(next.starts_with("下一步"), "{next}");
                assert_ne!(reason, next);
            }
        }
    }

    /// 无表、无结果、任务空态的文案各不相同；英文里不夹中文。
    #[test]
    fn state_copy_differs_per_case_and_english_has_no_chinese() {
        let keys = [
            "ui.exchange.state.noTableTitle",
            "ui.exchange.noTableData",
            "ui.exchange.state.noResultTitle",
            "ui.exchange.noResult",
            "ui.exchange.noTableColumns",
            "ui.exchange.state.tasksTitle",
            "ui.exchange.state.tasksDetail",
            "ui.exchange.state.refresh",
            "ui.exchange.useTable",
            "ui.exchange.next.waitRead",
            "ui.exchange.next.openTable",
            "ui.exchange.next.runQuery",
            "ui.exchange.next.refresh",
            "ui.exchange.next.switchToTable",
        ];
        let cjk = |text: &str| text.chars().any(|ch| ('\u{4e00}'..='\u{9fff}').contains(&ch) || ('\u{3000}'..='\u{303f}').contains(&ch) || ('\u{ff00}'..='\u{ffef}').contains(&ch));
        let mut zh: Vec<String> = Vec::new();
        for key in keys {
            let zh_text = i18n::text(Locale::ZhCn, key);
            let en_text = i18n::text(Locale::En, key);
            assert!(!zh_text.is_empty() && !en_text.is_empty(), "{key}");
            assert!(!cjk(&en_text), "{key} 英文里夹了中文：{en_text}");
            zh.push(zh_text);
        }
        // “没有表”与“没有结果”必须是两句不同的话（R9：无数据与无命中不共用文案）。
        assert_ne!(i18n::text(Locale::ZhCn, "ui.exchange.noTableData"), i18n::text(Locale::ZhCn, "ui.exchange.noResult"));
        assert_ne!(i18n::text(Locale::ZhCn, "ui.exchange.state.noTableTitle"), i18n::text(Locale::ZhCn, "ui.exchange.state.noResultTitle"));
        for (index, text) in zh.iter().enumerate() {
            for other in zh.iter().skip(index + 1) {
                assert_ne!(text, other, "两条状态文案重复：{text}");
            }
        }
    }

    struct PaneHost {
        pane: gpui_kit::Entity<ExchangePane>,
    }

    impl Render for PaneHost {
        fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            // 与外壳的交换层同一套几何：宽 520，窗口更窄时右端留 60px。
            let drawer = 520.0_f32.min(f32::from(window.viewport_size().width) - 60.);
            div()
                .id("pane-host")
                .size_full()
                .flex()
                .justify_end()
                .child(
                    div()
                        .id("exchange-drawer-frame")
                        .test_support()
                        .w(px(drawer))
                        .h_full()
                        .overflow_y_scroll()
                        .child(self.pane.clone()),
                )
        }
    }

    fn open_pane(cx: &mut TestAppContext, width: f32) -> (gpui_kit::AnyWindowHandle, gpui_kit::Entity<ExchangePane>) {
        let services = services();
        cx.update(|cx| {
            let pane = cx.new(|_| {
                let mut pane = ExchangePane::new(services);
                pane.open = true;
                pane
            });
            let host = cx.new(|_| PaneHost { pane: pane.clone() });
            let bounds = gpui_kit::Bounds { origin: Point::default(), size: size(px(width), px(900.)) };
            let (window, _view) = gpui_kit::open_window(
                WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
                cx,
                |_, _cx| host,
            )
            .expect("交换面板窗口");
            (window, pane)
        })
    }

    fn bind(cx: &mut TestAppContext, pane: &gpui_kit::Entity<ExchangePane>, source: ExchangeSource) {
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                pane.bind_source(source);
                cx.notify();
            })
        });
    }

    fn frame(cx: &mut TestAppContext, window: gpui_kit::AnyWindowHandle) {
        cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
    }

    /// 抽屉宽度按外壳的钳制取值，面板里的动作不越过它的右边缘（R4：窄宽主操作可达）。
    fn assert_inside_drawer(cx: &mut TestAppContext, window: gpui_kit::AnyWindowHandle, ids: &[&'static str], tag: &str) {
        cx.update_window(window, |_, window, _cx| {
            let drawer = window.try_find("exchange-drawer-frame").expect("抽屉框").bounds();
            let right = drawer.origin.x + drawer.size.width;
            let expected = 520.0_f32.min(f32::from(window.viewport_size().width) - 60.);
            assert!((drawer.size.width.as_f32() - expected).abs() < 0.5, "{tag}：抽屉宽度必须钳到 min(520, 视口-60)");
            for id in ids {
                if let Some(snapshot) = window.try_find(*id) {
                    let bounds = snapshot.bounds();
                    assert!(bounds.origin.x >= drawer.origin.x - px(0.5), "{tag}：{id} 越过左边缘");
                    assert!(bounds.origin.x + bounds.size.width <= right + px(0.5), "{tag}：{id} 越过右边缘");
                }
            }
        })
        .unwrap();
    }

    fn pane_states_hold_at(cx: &mut TestAppContext, width: f32) {
        cx.update(gpui_kit::init);
        let (window, pane) = open_pane(cx, width);
        for (dark, compact) in [(false, true), (false, false), (true, true), (true, false)] {
            cx.update(|cx| {
                kit::apply_theme(dark, cx);
                let metrics = if compact { kit::Metrics::compact() } else { kit::Metrics::standard() };
                metrics.publish(cx);
            });
            let tag = format!("{width}/{}/{}", if dark { "dark" } else { "light" }, if compact { "compact" } else { "standard" });
            let action_ids = [
                "exchange-exchange-export-loaded",
                "exchange-exchange-export-full",
                "exchange-exchange-dump",
                "exchange-exchange-migrate",
                "exchange-exchange-task-cancel",
                "exchange-exchange-clear",
            ];

            // 就绪：来源卡没有空态，第一行是可点的导出动作，任务空态给出“导出已读取的行”。
            bind(cx, &pane, table_source(Some("orders"), &["id", "total"], false));
            frame(cx, window);
            cx.update_window(window, |_, window, _cx| {
                {
                    let source = window.within("exchange-source-card");
                    assert!(source.try_find("empty-state").is_none(), "{tag}：就绪时不该有空态");
                    assert!(source.try_find("inline-loading").is_none(), "{tag}：就绪时不该有加载态");
                }
                {
                    let export = window.within("exchange-export-card");
                    let loaded = export.try_find("exchange-exchange-export-loaded").expect("导出已读取的行");
                    assert_eq!(loaded.role(), Some(gpui_kit::Role::Button), "{tag}");
                    assert!(export.try_find("exchange-exchange-migrate").is_some(), "{tag}：有表时可迁移");
                }
                {
                    let tasks = window.within("exchange-tasks");
                    assert!(tasks.try_find("empty-state").is_some(), "{tag}：没有任务时是空态");
                    assert!(tasks.try_find("exchange-tasks-export").is_some(), "{tag}：任务空态给出导出这一步");
                }
                // 任务区底部两个工具按钮在空态卡片之外，按 id 单独查。
                let cancel = window.try_find("exchange-exchange-task-cancel").expect("取消任务按钮");
                assert_eq!(cancel.disabled(), Some(true), "{tag}：没有运行中的任务时取消不可用");
                let clear = window.try_find("exchange-exchange-clear").expect("清除按钮");
                assert_eq!(clear.disabled(), Some(true), "{tag}：没有已结束的任务时清除不可用");
            })
            .unwrap();
            assert_inside_drawer(cx, window, &action_ids, &tag);

            // 运行中：来源卡是局部加载，不是空态；被挡住的导出动作分开说原因与下一步。
            bind(cx, &pane, table_source(Some("orders"), &["id"], true));
            frame(cx, window);
            cx.update_window(window, |_, window, _cx| {
                assert!(window.within("exchange-source-card").try_find("inline-loading").is_some(), "{tag}：运行中是局部加载");
                assert!(window.within("exchange-source-card").try_find("empty-state").is_none(), "{tag}：运行中不画空态");
                let blocked = window.within("exchange-export-card").try_find("exchange-exchange-export-loaded").expect("被挡住的导出");
                let label = blocked.label().unwrap_or_default().to_owned();
                assert!(label.contains("读取"), "{tag}：原因要说在读取：{label}");
                assert!(label.contains("下一步"), "{tag}：要给下一步：{label}");
            })
            .unwrap();
            assert_inside_drawer(cx, window, &action_ids, &tag);

            // 没有表：空态 + 重新读取来源，导出、全库导出与迁移都说明原因。
            bind(cx, &pane, table_source(None, &[], false));
            frame(cx, window);
            cx.update_window(window, |_, window, _cx| {
                {
                    let source = window.within("exchange-source-card");
                    assert!(source.try_find("empty-state").is_some(), "{tag}：没有表要有空态");
                    assert!(source.try_find("state-action").is_some(), "{tag}：空态要有下一步");
                    assert!(source.try_find("exchange-source-refresh-no-table").is_some(), "{tag}：空态的主操作是重新读取来源");
                }
                let dump = window.within("exchange-export-card").try_find("exchange-exchange-dump").expect("被挡住的全库导出");
                let label = dump.label().unwrap_or_default().to_owned();
                assert!(label.contains("没有当前连接"), "{tag}：全库导出要说明没有连接目标：{label}");
                assert!(label.contains("下一步"), "{tag}：全库导出要给下一步：{label}");
                assert!(window.within("exchange-import-card").try_find("empty-state").is_none(), "{tag}：表数据来源不挂查询提示");
            })
            .unwrap();
            assert_inside_drawer(cx, window, &action_ids, &tag);

            // 没有结果：空态说明与下一步都不是“没有表”那一句；查询来源的导入区给出切换动作。
            bind(cx, &pane, query_source(&[], false));
            frame(cx, window);
            cx.update_window(window, |_, window, _cx| {
                {
                    let source = window.within("exchange-source-card");
                    assert!(source.try_find("empty-state").is_some(), "{tag}：没有结果要有空态");
                    assert!(source.try_find("exchange-source-refresh-no-result").is_some(), "{tag}：没有结果的下一步");
                }
                {
                    let import = window.within("exchange-import-card");
                    assert!(import.try_find("empty-state").is_some(), "{tag}：查询来源的导入要说明原因");
                    assert!(import.try_find("exchange-import-use-table").is_some(), "{tag}：查询来源的导入要给出切换动作");
                }
                let blocked = window.within("exchange-export-card").try_find("exchange-exchange-export-loaded").expect("被挡住的导出");
                let label = blocked.label().unwrap_or_default().to_owned();
                assert!(label.contains("没有结果"), "{tag}：原因要是没有结果：{label}");
            })
            .unwrap();
            assert_inside_drawer(cx, window, &action_ids, &tag);
        }
    }

    #[gpui_kit::test]
    fn exchange_states_hold_at_1360(cx: &mut TestAppContext) {
        pane_states_hold_at(cx, 1360.);
    }

    #[gpui_kit::test]
    fn exchange_states_hold_at_900(cx: &mut TestAppContext) {
        pane_states_hold_at(cx, 900.);
    }

    #[gpui_kit::test]
    fn exchange_states_hold_at_640(cx: &mut TestAppContext) {
        pane_states_hold_at(cx, 640.);
    }

    /// 来源没就绪时不摆一个点了没用的导出动作：任务空态回到只说明。
    #[gpui_kit::test]
    fn tasks_empty_state_drops_the_export_action_when_the_source_is_not_ready(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (window, pane) = open_pane(cx, 900.);
        bind(cx, &pane, table_source(None, &[], false));
        frame(cx, window);
        cx.update_window(window, |_, window, _cx| {
            let tasks = window.within("exchange-tasks");
            assert!(tasks.try_find("empty-state").is_some());
            assert!(tasks.try_find("exchange-tasks-export").is_none(), "来源没就绪时任务空态不提供导出动作");
        })
        .unwrap();
    }
}
