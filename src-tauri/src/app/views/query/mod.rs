//! GPUI 查询工作台。编辑器复用 SqlEditor，结果复用 DataTable。
//! 方言规则在 query::assist / query::desk；这里只提交任务和显示。

mod log;
mod side;

use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
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
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div, px,
};

use crate::ui::kit::{self, Palette, Tip as _, Radius, TypeScale};

use crate::app::views::grid::{GridPane, GridTarget, QuerySheet};
use crate::app::model::ObjectTarget;
use crate::app::Density;
use crate::application::AppServices;
use crate::i18n::{self, Locale};
use crate::query::assist::{self, LintFinding, RunScope, TreeObject};
use crate::query::commands::RunDto;
use crate::query::desk::{self, BeginRun, Desk, DraftLink, DraftSync, ErrorMark, PreparedRun, ResultView, Target, Ticket};

pub use side::{SideMode, SideState};
use crate::ui::{ObjectHandler, ObjectLink, SqlEditor};

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
    /// 每个查询标签自己的右侧面板状态（开关、大纲/对象）。
    side: HashMap<String, SideState>,
    /// 外壳密度；消息页和右侧面板的行高与控件高度跟它走。
    density: Density,
    /// 最近一次绘制时视口是否窄；窄窗口里右侧面板默认收起、打开时是浮层。
    narrow: bool,
    /// 测试用：消息页/右侧面板最近一次绘制时用的底色 (surface, chrome)。
    #[cfg(test)]
    drawn_colors: Option<(gpui_kit::Hsla, gpui_kit::Hsla)>,
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
    /// 最近一次 capture 时活动编辑器的选区：(标签, 起, 止)，字节偏移。外壳插入模板时用。
    captured: Option<(String, usize, usize)>,
    /// 编辑器里 Ctrl/⌘+点击对象后交给外壳：树定位、结构页都在外壳里。
    definition_sink: Option<ObjectHandler>,
    /// 每个标签最近一次画错误标记时的结果代次；同一批结果只画一次，编辑清掉后不再重画。
    marks_seen: HashMap<String, u64>,
}

/// 失败语句在草稿里要标的范围。偏移标到那个词（至少一个字符，不跨行）；行号标整行；
/// 驱动没给位置时标整条语句。范围对不上草稿的结果不标。
/// 偏移落在输入末尾或换行上（如 PG “at end of input”）时没有可标的字符，空范围会被编辑器丢掉：
/// 改标它前面最近的非空白字符；前面全是空白时标偏移所在的整行。
pub fn error_ranges(draft: &str, results: &[desk::StatementView]) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    for view in results {
        let Some(mark) = view.error_mark else { continue };
        let range = match mark {
            ErrorMark::Statement => view.range.map(|(start, end)| start..end),
            ErrorMark::Offset(offset) => {
                let offset = offset.min(draft.len());
                draft.is_char_boundary(offset).then(|| {
                    let rest = &draft[offset..];
                    let word = rest.chars().take_while(|ch| ch.is_alphanumeric() || *ch == '_').map(char::len_utf8).sum::<usize>();
                    let single = rest.chars().next().filter(|ch| *ch != '\n' && *ch != '\r').map(char::len_utf8).unwrap_or(0);
                    let len = word.max(single);
                    if len > 0 {
                        return offset..offset + len;
                    }
                    let before = &draft[..offset];
                    match before.char_indices().rev().find(|(_, ch)| !ch.is_whitespace()) {
                        Some((at, ch)) => at..at + ch.len_utf8(),
                        None => line_at(draft, offset),
                    }
                })
            }
            ErrorMark::Line(line) => {
                let mut start = 0;
                let mut found = None;
                for (index, text) in draft.split('\n').enumerate() {
                    if index == line {
                        found = Some(start..start + text.trim_end_matches('\r').len());
                        break;
                    }
                    start += text.len() + 1;
                }
                found
            }
        };
        if let Some(range) = range.filter(|range| range.end <= draft.len() && range.start < range.end) {
            ranges.push(range);
        }
    }
    ranges
}

/// 偏移所在的整行（不含换行和行尾 \r）。
fn line_at(draft: &str, offset: usize) -> Range<usize> {
    let start = draft[..offset].rfind('\n').map_or(0, |at| at + 1);
    let end = draft[offset..].find('\n').map_or(draft.len(), |at| offset + at);
    let end = if draft[start..end].ends_with('\r') { end - 1 } else { end };
    start..end.max(start)
}

/// 结果标题里的执行状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResultTiming {
    None,
    Running,
    Cancelled,
    Elapsed(u64),
}

/// 结果标题里的耗时：一秒以内按毫秒，再长按秒。
/// 语句的说明文字。执行层给“没执行到”的语句写的是固定的中文说明，界面按当前语言换成同一个状态词。
pub fn statement_message(locale: Locale, status: &str, message: &str) -> String {
    if status == "skipped" {
        i18n::text(locale, "query.log.skipped")
    } else {
        message.to_owned()
    }
}

pub fn elapsed_label(ms: u64) -> String {
    if ms >= 1000 {
        format!("{:.2} s", ms as f64 / 1000.)
    } else {
        format!("{ms} ms")
    }
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

/// 加载更多会按"结果来自的目标"重跑同一条 SQL；标签当前目标已经换了（换库、换连接）
/// 就不能在新目标上续读旧结果，入口隐藏。没有结果来源记录时视为同一目标。
fn load_more_targets_match(results_target: Option<&desk::Target>, current: Option<&desk::Target>) -> bool {
    match (results_target, current) {
        (Some(from), Some(now)) => from.connection_id == now.connection_id && from.database == now.database && from.schema == now.schema,
        (Some(_), None) => false,
        (None, _) => true,
    }
}

impl QueryPane {
    /// 当前结果能否在标签此刻的目标上续读。
    fn load_more_allowed(&self) -> bool {
        load_more_targets_match(self.desk.results_target().as_ref(), self.desk.target().as_ref())
    }

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
            side: HashMap::new(),
            density: Density::Compact,
            narrow: false,
            #[cfg(test)]
            drawn_colors: None,
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
            captured: None,
            definition_sink: None,
            marks_seen: HashMap::new(),
        }
    }

    /// 跳转定义的出口。未设置时 Ctrl/⌘+点击什么也不做。
    pub fn set_definition_sink(&mut self, sink: ObjectHandler) {
        self.definition_sink = Some(sink);
    }

    /// 跳转目标在查询页目录里的完整规格；对象树还没读到它时，外壳仍用它打开结构页。
    pub fn definition_spec(&self, link: &ObjectLink) -> Option<String> {
        let object = self.desk.objects().iter().find(|object| {
            object.connection_id == link.connection_id
                && object.database == link.database
                && object.schema.eq_ignore_ascii_case(&link.schema)
                && object.name.eq_ignore_ascii_case(&link.name)
        })?;
        Some(object_spec(object, &self.desk.environment_of(&object.connection_id)))
    }

    /// 最近一次 capture 记下的活动标签选区（字节偏移）。
    pub fn captured_selection(&self) -> Option<(String, usize, usize)> {
        self.captured.clone()
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

    /// 外壳密度下发到查询页；变了返回 true，调用方据此重绘。
    pub fn set_density(&mut self, density: Density) -> bool {
        let changed = self.density != density;
        self.density = density;
        changed
    }

    /// 活动查询标签的右侧面板状态。
    pub fn side_state(&self) -> SideState {
        self.desk.active_id().and_then(|id| self.side.get(id)).copied().unwrap_or_default()
    }

    /// 活动查询标签的右侧面板此刻是否打开（未动过时按最近一次绘制的宽窄取默认值）。
    pub fn side_open(&self) -> bool {
        self.side_state().is_open(self.narrow)
    }

    fn update_side(&mut self, change: impl FnOnce(&mut SideState, bool)) {
        let Some(id) = self.desk.active_id().map(str::to_owned) else {
            return;
        };
        let narrow = self.narrow;
        change(self.side.entry(id).or_default(), narrow);
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
        self.grid.update(cx, |grid, cx| {
            // 预览上限变了：结果网格按新上限重读表数据，或说明查询结果需要重新执行。
            grid.set_preview_kb(preview_kb, cx);
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
        if let Some(index) = id.strip_prefix("outline:") {
            if let Ok(index) = index.parse::<usize>() {
                let _ = self.jump_to_statement(index, cx);
            }
            cx.notify();
            return;
        }
        if let Some(index) = id.strip_prefix("statement:") {
            if let Ok(index) = index.parse::<usize>() {
                self.desk.set_statement(index);
                self.desk.set_result_view(ResultView::Current);
                self.rebind_grid(cx);
            }
            cx.notify();
            return;
        }
        if let Some(view) = id.strip_prefix("query-view:") {
            let view = match view {
                "current" => Some(ResultView::Current),
                "messages" => Some(ResultView::Messages),
                other => other.strip_prefix("pinned:").and_then(|pin| pin.parse::<u64>().ok()).map(ResultView::Pinned),
            };
            // 表格是否需要重新绑定由结果代次决定（sync_grid）：消息页与当前结果之间切换不重绑。
            if let Some(view) = view {
                let _ = self.desk.set_result_view(view);
            }
            cx.notify();
            return;
        }
        if let Some(pin) = id.strip_prefix("unpin:") {
            if let Ok(pin) = pin.parse::<u64>() {
                if self.desk.unpin(pin) {
                    self.rebind_grid(cx);
                }
            }
            cx.notify();
            return;
        }
        if let Some(index) = id.strip_prefix("log:") {
            if let Ok(index) = index.parse::<usize>() {
                self.open_log_entry(index, cx);
            }
            cx.notify();
            return;
        }
        if let Some(mode) = id.strip_prefix("side-mode:") {
            let mode = match mode {
                "object" => SideMode::Object,
                _ => SideMode::Outline,
            };
            self.update_side(|state, _| {
                state.mode = mode;
                state.open = Some(true);
            });
            cx.notify();
            return;
        }
        if id == "pin-result" {
            if self.desk.pin_current().is_some() {
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
            "toggle-side" => {
                self.update_side(|state, narrow| state.open = Some(!state.is_open(narrow)));
                cx.notify();
            }
            // 旧入口：“语句大纲”开关。打开时固定在大纲页。
            "toggle-outline" => {
                self.update_side(|state, narrow| {
                    let open = !(state.is_open(narrow) && state.mode == SideMode::Outline);
                    state.open = Some(open);
                    if open {
                        state.mode = SideMode::Outline;
                    }
                });
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
        self.apply_grid_locale(locale, cx);
        self.ensure_editors(window, cx);
        effect
    }

    /// 网格是独立实体，父页面换语言时必须主动下发并重绘，否则工具栏保留旧语言。
    pub fn apply_grid_locale(&mut self, locale: Locale, cx: &mut Context<Self>) {
        let changed = self.grid.update(cx, |grid, _| grid.set_locale(locale));
        if changed {
            self.grid.update(cx, |_, cx| cx.notify());
        }
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
        self.side.retain(|id, _| ids.iter().any(|keep| keep == id));
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
            let pane = cx.weak_entity();
            editor.update(cx, |editor, cx| {
                editor.set_font_size(font);
                editor.set_theme(&theme);
                editor.set_sql(&sql, window, cx);
                // 链接在编辑器更新里触发；推迟到这一轮结束再交给外壳，外壳会回头读编辑器。
                editor.set_object_sink(Rc::new(move |link: ObjectLink, cx: &mut App| {
                    let pane = pane.clone();
                    cx.defer(move |cx| {
                        let sink = pane.upgrade().and_then(|pane| pane.read(cx).definition_sink.clone());
                        if let Some(sink) = sink {
                            sink(link, cx);
                        }
                    });
                }));
            });
            self.editors.insert(id.clone(), editor);
            self.links.insert(id, DraftLink { applied: sql.clone(), editor: sql, dirty: false });
        }
    }

    /// 测试用：把一批结果当作一次完成的执行写到活动标签（含消息记录）。
    #[cfg(test)]
    pub fn plant_run(&mut self, results: Vec<desk::StatementView>, cx: &mut App) {
        self.desk.plant_run(results);
        self.rebind_grid(cx);
    }

    /// 测试用：消息页/右侧面板最近一次绘制时用的底色 (surface, chrome)。
    #[cfg(test)]
    pub fn drawn_colors(&self) -> Option<(gpui_kit::Hsla, gpui_kit::Hsla)> {
        self.drawn_colors
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
        self.captured = Some((id.clone(), range.start, range.end));
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

    /// 结果标题的耗时状态：执行中、已取消，或当前语句自己的耗时。
    pub fn result_timing(&self) -> ResultTiming {
        let Some(id) = self.desk.active_id() else {
            return ResultTiming::None;
        };
        if self.desk.running_ids().iter().any(|running| running == id) {
            return ResultTiming::Running;
        }
        if let ResultView::Pinned(_) = self.desk.result_view() {
            // 固定结果显示它自己那次的耗时，不跟当前执行的状态。
            return self.desk.shown_result().and_then(|result| result.elapsed_ms).map_or(ResultTiming::None, ResultTiming::Elapsed);
        }
        let results = self.desk.results();
        let active = results.get(self.desk.statement()).or_else(|| results.first());
        if self.desk.cancelled(id) || active.is_some_and(|result| result.status == "cancelled") {
            return ResultTiming::Cancelled;
        }
        active.and_then(|result| result.elapsed_ms).map_or(ResultTiming::None, ResultTiming::Elapsed)
    }

    /// 大纲点击：在编辑器里选中第 index 条语句（从 1 开始）并滚到它。大纲按编辑器当前文本重算，
    /// 所以刚改过的草稿也不会跳到旧位置。找不到这条语句时什么也不做。
    fn jump_to_statement(&mut self, index: usize, cx: &mut Context<Self>) -> bool {
        self.capture(cx);
        self.desk.refresh_outline();
        let Some(item) = self.desk.outline().into_iter().find(|item| item.index == index) else {
            return false;
        };
        let Some(editor) = self.desk.active_id().and_then(|id| self.editors.get(id)).cloned() else {
            return false;
        };
        editor.update(cx, |editor, cx| editor.select_range(item.start..item.end, cx));
        self.focus_editor = true;
        true
    }

    /// 消息页里点一条记录：成功的切到它的结果；失败的跳到编辑器里的错误位置。
    /// 整次执行的失败/取消记录没有对应的结果，什么也不做。
    fn open_log_entry(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(entry) = self.desk.log().get(index).cloned() else {
            return;
        };
        let Some(statement) = entry.statement else {
            return;
        };
        if statement >= self.desk.results().len() {
            return;
        }
        self.desk.set_statement(statement);
        if entry.failed() {
            if !self.jump_to_error(statement, cx) {
                self.desk.set_active_note(&i18n::text(self.locale, "query.log.noPosition"));
            }
        } else {
            self.desk.set_result_view(ResultView::Current);
        }
        self.rebind_grid(cx);
    }

    /// 在编辑器里选中第 statement 条结果的错误位置（与错误标记同一份范围）。
    /// 草稿在执行后改过时位置不再可信，不跳。
    fn jump_to_error(&mut self, statement: usize, cx: &mut Context<Self>) -> bool {
        let Some(id) = self.desk.active_id().map(str::to_owned) else {
            return false;
        };
        let Some(editor) = self.editors.get(&id).cloned() else {
            return false;
        };
        let results = self.desk.results();
        let Some(view) = results.get(statement) else {
            return false;
        };
        let text = editor.read(cx).value(cx);
        if self.desk.executed_sql(&id).as_deref() != Some(text.as_str()) {
            return false;
        }
        let Some(range) = error_ranges(&text, std::slice::from_ref(view)).into_iter().next() else {
            return false;
        };
        editor.update(cx, |editor, cx| editor.select_range(range, cx));
        self.focus_editor = true;
        true
    }

    /// 把活动标签最近一次执行的失败位置画到编辑器上：驱动给了位置就标那一处/那一行，
    /// 没给就整条语句标错。草稿在执行后改过（与执行时的文本不同）时不画。
    fn sync_error_marks(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.desk.active_id().map(str::to_owned) else {
            return;
        };
        if self.desk.running_ids().iter().any(|running| running == &id) {
            return;
        }
        let generation = self.desk.view_gen();
        if self.marks_seen.get(&id) == Some(&generation) {
            return;
        }
        let Some(editor) = self.editors.get(&id).cloned() else {
            return;
        };
        self.marks_seen.insert(id.clone(), generation);
        let text = editor.read(cx).value(cx);
        if self.desk.executed_sql(&id).as_deref() != Some(text.as_str()) {
            return;
        }
        let ranges = error_ranges(&text, &self.desk.results());
        if !ranges.is_empty() {
            editor.update(cx, |editor, cx| editor.mark_errors(ranges, cx));
        }
    }

    fn start(&mut self, scope: RunScope, override_sql: Option<String>, cx: &mut Context<Self>) {
        // 重新执行：上一次的错误标记不再成立。
        if let Some(editor) = self.desk.active_id().and_then(|id| self.editors.get(id)).cloned() {
            editor.update(cx, |editor, cx| editor.clear_errors(cx));
        }
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
        // 一条执行只开一条有序快照通道：中间批次和终态按产生顺序送达界面。
        // 只等终态时，分批读取停在“加载更多”的等待上，界面永远停在“正在执行”且没有行。
        let (snapshots, mut received) = tokio::sync::mpsc::unbounded_channel::<Result<RunDto, String>>();
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
                    let _ = snapshots.send(Err(error));
                    return;
                }
            };
            let run_id = started.id.clone();
            let mut pending = desk::run_pending(&started.status);
            if snapshots.send(Ok(started)).is_err() {
                return;
            }
            // 每次带 continue_read 的轮询放行一批行并等到下一批，不忙轮询。
            while pending {
                match crate::query::commands::poll_run(&services.query, &run_id, true).await {
                    Ok(snap) => {
                        polls.fetch_add(1, Ordering::Relaxed);
                        pending = desk::run_pending(&snap.status);
                        if snapshots.send(Ok(snap)).is_err() {
                            return;
                        }
                    }
                    Err(error) => {
                        polls.fetch_add(1, Ordering::Relaxed);
                        let _ = snapshots.send(Err(error));
                        return;
                    }
                }
            }
        });
        let view = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            while let Some(item) = received.recv().await {
                let mut keep = false;
                cx.update(|cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |pane, cx| {
                            keep = pane.apply_run_snapshot(&ticket, item, cx);
                        });
                    }
                });
                if !keep {
                    break;
                }
            }
        })
        .detach();
        // 运行态由结果标题的 spinner 表达；note 留给错误或提示，避免“正在执行”重复两遍。
        self.desk.set_active_note("");
        cx.notify();
    }

    /// 一条执行快照到达界面：中间批次也要立刻落到结果和网格上，
    /// 否则分批读取的行要等到终态才显示。返回 false 表示这次执行已经结束。
    fn apply_run_snapshot(&mut self, ticket: &Ticket, result: Result<RunDto, String>, cx: &mut Context<Self>) -> bool {
        let pending = match &result {
            Ok(run) => {
                if !self.desk.run_noted(&ticket.tab_id, &run.id) {
                    // 运行 id 第一次出现：取消要能定位到它，之后才算“已记录”。
                    self.note_started(&ticket.tab_id, &run.id, cx);
                }
                desk::run_pending(&run.status)
            }
            Err(_) => false,
        };
        let cancel_text = i18n::text(self.locale, "query.run.cancel");
        self.desk.finish_polled(&self.services.tasks, ticket, result, &cancel_text);
        self.rebind_grid(cx);
        cx.notify();
        pending
    }

    /// 结果被读取上限截断时，再执行一次同一条只读语句并放大窗口。
    /// 写语句不会因为翻页被自动重放。
    fn load_more(&mut self, cx: &mut Context<Self>) {
        if !self.context_active {
            self.desk.set_active_note(NOT_ACTIVE_QUERY);
            cx.notify();
            return;
        }
        if !self.load_more_allowed() {
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
        // 整段语句和光标一起交给补全：子句、别名和 ON 两侧都从当前语句得出。
        let (text, cursor) = match self.editors.get(&id) {
            Some(editor) => {
                let editor = editor.read(cx);
                let text = editor.value(cx);
                let cursor = editor.selected_range(cx).end.min(text.len());
                (text, cursor)
            }
            None => (String::new(), 0),
        };
        let locale = self.locale;
        let items = self.desk.completion_for(&text, cursor, locale);
        let pairs: Vec<(String, String)> = items.iter().map(|item| (item.label.clone(), item.insert.clone())).collect();
        if let Some(ticket) = self.desk.issue(&self.services.tasks) {
            let _ = self.desk.accept_completion(&self.services.tasks, &ticket, items);
        }
        if let Some(editor) = self.editors.get(&id) {
            editor.update(cx, |editor, _| {
                editor.set_locale(locale);
                editor.set_extra_suggestions(pairs);
            });
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
                                    let schema = parts.next().unwrap_or("");
                                    let table = parts.next().unwrap_or("");
                                    pane.desk.remember_table_columns(connection, database, schema, table, cols);
                                }
                                pane.desk.set_object_comments(loaded.comments);
                                pane.desk.set_foreign_keys(loaded.foreign_keys);
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
        // 看固定结果时表格显示它自己的快照；消息页不动表格，仍绑定当前结果。
        let Some(shown) = self.desk.shown_result() else {
            self.grid.update(cx, |grid, cx| grid.clear_sheet(cx));
            return;
        };
        let sheet = &shown;
        let columns = if sheet.columns.is_empty() { vec!["status".to_owned()] } else { sheet.columns.clone() };
        let rows = if sheet.columns.is_empty() {
            vec![vec![Some(statement_message(self.locale, &sheet.status, &sheet.message))]]
        } else {
            sheet.rows.clone()
        };
        // 固定结果按产生它的那次执行的目标绑定：标签之后切了库，也不能拿旧库的行对新库暂存写入。
        let target = match self.desk.shown_pinned_target() {
            Some(pinned) => self.pinned_grid_target(pinned),
            None => self.grid_target(),
        };
        let sql = sheet.sql.clone();
        let engine = target.engine.clone();
        // 截断与完整行数原样交给网格：被截断的结果不能被说成"已到末尾"。
        let truncated = sheet.truncated;
        let full_count = if truncated { None } else { Some(rows.len() as i64) };
        let sheet = QuerySheet { sql, columns, rows, truncated, full_count };
        self.grid.update(cx, |grid, cx| {
            let mut target = target;
            target.engine = engine;
            grid.show_query(target, sheet, cx);
        });
    }

    /// 固定结果的表格目标。来源无法证明时不给连接：表格不会去定位，结果保持未定位只读。
    /// 来源连接已经关闭时由表格自己的会话检查拒绝定位，同样是只读。
    fn pinned_grid_target(&self, pinned: Option<Target>) -> GridTarget {
        let Some(target) = pinned else {
            let engine = self.desk.target().map(|target| target.engine).unwrap_or_default();
            return GridTarget { engine, ..GridTarget::default() };
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

    /// 测试用：结果表格当前绑定的目标与它是否可写。
    #[cfg(test)]
    pub fn grid_binding(&self, cx: &mut App) -> (GridTarget, bool) {
        self.grid.update(cx, |grid, _| grid.with_session(|session| (session.target.clone(), session.writable())))
    }

    /// 当前结果的表格目标：优先用产生这批结果的那次执行的目标（执行后切库不会把旧结果绑到新库），
    /// 没有执行来源时按标签当前目标。
    fn grid_target(&self) -> GridTarget {
        let Some(target) = self.desk.results_target().or_else(|| self.desk.target()) else {
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
            let locale = self.locale;
            let link_target = self.desk.target().map(|target| (target.connection_id, target.database));
            editor.update(cx, |editor, _| {
                editor.set_locale(locale);
                editor.set_completion_scope(scope);
                editor.set_link_target(link_target);
            });
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
                    let locale = self.locale;
                    let note = editor.update(cx, |editor, cx| {
                        editor.set_locale(locale);
                        editor.show_completions(window, cx);
                        editor.completion_note()
                    });
                    // 元数据失败、部分失败或没有列时说明原因，而不是只留一个空列表。
                    if let Some(note) = note.filter(|note| !note.trim().is_empty()) {
                        self.desk.set_active_note(&note);
                    }
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
        self.sync_error_marks(cx);
        self.refresh_folds(cx);
        self.sync_grid(cx);
        self.take_focus(window, cx);
        self.narrow = f32::from(window.viewport_size().width) < side::NARROW_WIDTH;
        let p = Palette::of(cx);
        let metrics = self.density.metrics();
        let mono = cx.theme().mono_font_family.clone();
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
            .h(px(metrics.toolbar_h()))
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
            .child(database_picker)
            .child(kit::vline(&p))
            .child(
                Button::new("query-side-toggle")
                    .ghost()
                    .small()
                    .icon(IconName::PanelRight)
                    .tooltip(t("query.side.toggle"))
                    .accessibility_label(t("query.side.toggle"))
                    .on_click(pane_act("toggle-side", &entity)),
            );

        // ---- 编辑器 + 右侧面板（大纲 / 对象）
        let side_state = self.side_state();
        let side_open = side_state.is_open(self.narrow);
        let mut editor_row = div().id("query-editor-row").relative().flex().size_full().min_h(px(0.));
        let mut cursor_text: Option<(String, usize)> = None;
        if let Some(editor) = editor {
            if side_open && side_state.mode == SideMode::Object {
                let state = editor.read(cx);
                let text = state.value(cx);
                let cursor = state.selected_range(cx).end.min(text.len());
                cursor_text = Some((text, cursor));
            }
            editor_row = editor_row.child(div().id("query-editor").test_support().flex_1().min_w(px(0.)).h_full().child(editor));
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
        if side_open && self.desk.active_id().is_some() {
            let info = cursor_text.and_then(|(text, cursor)| self.desk.object_info_at(&text, cursor));
            let body = match side_state.mode {
                SideMode::Outline => side::SideBody::Outline(&outline),
                SideMode::Object => side::SideBody::Object(info.as_ref()),
            };
            let panel = side::SidePanel { mode: side_state.mode, body, locale, metrics, overlay: self.narrow, mono: mono.clone() };
            editor_row = editor_row.child(panel.render(&p, &entity));
            #[cfg(test)]
            {
                self.drawn_colors = Some((p.surface, p.chrome));
            }
        }

        // ---- 结果：页签从左到右是 固定结果… | 当前结果（语句 N） | 消息
        let result_view = self.desk.result_view();
        let pinned = self.desk.pinned();
        let log = self.desk.log();
        let mut header = div()
            .id("query-results-head")
            .flex_none()
            .h(px(metrics.bar_h()))
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(2.))
            .bg(p.chrome)
            .border_b_1()
            .border_color(p.line);
        let tab = |id: SharedString, label: SharedString, selected: bool| {
            kit::interactive(div().id(id), &p, false)
                .test_support()
                .role(gpui_kit::Role::Tab)
                .aria_label(label)
                .aria_selected(selected)
                .flex_none()
                .h(px(metrics.control_h))
                .px(px(10.))
                .flex()
                .items_center()
                .gap(px(6.))
                .rounded(px(Radius::CONTROL))
                .text_size(px(TypeScale::SM))
                .when(selected, |this| this.bg(p.surface).text_color(p.text).shadow_sm())
                .when(!selected, |this| this.text_color(p.text_muted))
        };
        let mut strip = div()
            .id("query-result-tabs")
            .role(gpui_kit::Role::TabList)
            .flex_initial()
            .min_w(px(0.))
            .overflow_x_scroll()
            .flex()
            .items_center()
            .gap(px(2.));
        for pin in &pinned {
            let selected = result_view == ResultView::Pinned(pin.id);
            let label = i18n::fill(
                &i18n::text(locale, "query.results.pinned"),
                &[("n", &pin.id.to_string()), ("statement", &(pin.statement + 1).to_string())],
            );
            let failed = pin.view.status == "error" || pin.view.status == "failed";
            let open = entity.clone();
            let close = entity.clone();
            let open_id = format!("query-view:pinned:{}", pin.id);
            let close_id = format!("unpin:{}", pin.id);
            strip = strip.child(
                tab(SharedString::from(format!("query-pinned-{}", pin.id)), SharedString::from(label.clone()), selected)
                    .pr(px(4.))
                    .child(kit::icon(IconName::Bookmark, 12., if failed { p.danger } else { p.accent }))
                    .child(SharedString::from(label))
                    .child(
                        div()
                            .id(SharedString::from(format!("query-pinned-close-{}", pin.id)))
                            .test_support()
                            .role(gpui_kit::Role::Button)
                            .aria_label(t("query.results.unpin"))
                            .size(px(18.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(Radius::CONTROL))
                            .hover(|style| style.bg(p.active))
                            .child(kit::icon(IconName::X, 12., p.text_faint))
                            .on_click(move |_, _, cx| {
                                cx.stop_propagation();
                                let id = close_id.clone();
                                close.update(cx, |pane, cx| pane.command(&id, cx));
                            }),
                    )
                    .on_click(move |_, _, cx| {
                        let id = open_id.clone();
                        open.update(cx, |pane, cx| pane.command(&id, cx));
                    }),
            );
        }
        // 固定结果与当前结果之间用一道竖线分开；没有固定结果时不加。
        if !pinned.is_empty() {
            strip = strip.child(kit::vline(&p));
        }
        if results.len() > 1 {
            let current = self.desk.statement();
            for (index, result) in results.iter().enumerate() {
                let selected = result_view == ResultView::Current && index == current;
                let failed = result.status == "error" || result.status == "failed";
                let id = format!("statement:{index}");
                let entity = entity.clone();
                let label = SharedString::from(format!("{} {}", i18n::text(locale, "query.results.statement"), index + 1));
                strip = strip.child(
                    tab(SharedString::from(format!("query-act-statement-{index}")), label.clone(), selected)
                        .child(kit::status_dot(if failed { p.danger } else { p.ok }))
                        .child(label)
                        .on_click(move |_, _, cx| {
                            let id = id.clone();
                            entity.update(cx, |pane, cx| pane.command(&id, cx));
                        }),
                );
            }
        } else {
            let entity = entity.clone();
            strip = strip.child(
                tab(SharedString::from("query-results-tab"), t("ui.query.results"), result_view == ResultView::Current)
                    .child(t("ui.query.results"))
                    .on_click(move |_, _, cx| entity.update(cx, |pane, cx| pane.command("query-view:current", cx))),
            );
        }
        header = header.child(strip);
        // 消息页签在页签条之外、紧随其后：页签再多也不会被挤出视口。
        let log_failed = log.iter().any(|entry| entry.failed());
        let log_label = if log.is_empty() {
            i18n::text(locale, "query.log.tab")
        } else {
            format!("{} {}", i18n::text(locale, "query.log.tab"), log.len())
        };
        let log_entity = entity.clone();
        header = header.child(
            tab(SharedString::from("query-log-tab"), SharedString::from(log_label.clone()), result_view == ResultView::Messages)
                .when(log_failed, |this| this.child(kit::status_dot(p.danger)))
                .child(SharedString::from(log_label))
                .on_click(move |_, _, cx| log_entity.update(cx, |pane, cx| pane.command("query-view:messages", cx))),
        );
        let can_pin = self.desk.can_pin();
        let pin_entity = entity.clone();
        header = header.child(
            Button::new("query-act-pin")
                .ghost()
                .xsmall()
                .icon(IconName::Bookmark)
                .disabled(!can_pin)
                .tooltip(t("query.results.pin"))
                .accessibility_label(t("query.results.pin"))
                .on_click(move |_, _, cx| pin_entity.update(cx, |pane, cx| pane.command("pin-result", cx))),
        );
        let active_result = self.desk.shown_result();
        let summary = active_result.as_ref().map(|result| {
            if let Some(affected) = result.affected.filter(|_| result.columns.is_empty()) {
                format!("{affected} ✓")
            } else {
                format!("{} {}", result.rows.len(), i18n::text(locale, "ui.grid.rows"))
            }
        });
        let failed_note = active_result.as_ref().is_some_and(|result| result.status == "error" || result.status == "failed");
        // 耗时：执行中显示运行态，取消后显示已取消，完成后显示当前语句自己的耗时。
        let timing = self.result_timing();
        let cancelled = timing == ResultTiming::Cancelled;
        let elapsed = match timing {
            ResultTiming::Elapsed(ms) => Some(ms),
            _ => None,
        };
        header = header.child(div().flex_1()).when(running, |this| {
            this.child(div().id("query-elapsed-running").flex().items_center().gap(px(6.)).text_size(px(TypeScale::SM)).text_color(p.text_muted).child(Spinner::new().xsmall().color(p.accent)).child(t("query.status.running")))
        });
        if cancelled {
            header = header.child(div().id("query-elapsed-cancelled").px(px(6.)).text_size(px(TypeScale::SM)).text_color(p.warn).child(t("query.status.cancelled")));
        }
        if let Some(ms) = elapsed {
            header = header.child(
                div()
                    .id("query-elapsed")
                    .px(px(6.))
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .text_size(px(TypeScale::SM))
                    .text_color(p.text_faint)
                    .child(kit::icon(IconName::Clock, 12., p.text_faint))
                    .child(SharedString::from(elapsed_label(ms))),
            );
        }
        if !note.is_empty() {
            header = header.child(
                div()
                    .id("query-note")
                    .min_w(px(0.))
                    .max_w(px(520.))
                    .truncate()
                    .px(px(6.))
                    .text_size(px(TypeScale::SM))
                    .text_color(if failed_note { p.danger } else { p.text_muted })
                    .tooltip_text(SharedString::from(note.clone()))
                    .child(SharedString::from(note.clone())),
            );
        }
        if let Some(summary) = summary {
            header = header.child(div().px(px(6.)).text_size(px(TypeScale::SM)).text_color(p.text_faint).child(SharedString::from(summary)));
        }
        if !running && result_view == ResultView::Current && results.len() == 1 && is_pageable(&results[0]) && self.load_more_allowed() {
            match load_more_plan(&results, self.page_size) {
                Some((_, limit)) => {
                    let loaded = results[0].rows.len() as u64;
                    let more = entity.clone();
                    header = header.child(
                        kit::interactive(div().id("query-load-more"), &p, false)
                            .role(gpui_kit::Role::Button)
                            .aria_label(SharedString::from(i18n::text(locale, "query.results.loadMore")))
                            .h(px(metrics.control_h))
                            .px(px(8.))
                            .rounded(px(Radius::CONTROL))
                            .text_size(px(TypeScale::SM))
                            .text_color(p.accent)
                            .bg(p.accent_soft)
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
                            .h(px(metrics.control_h))
                            .px(px(8.))
                            .flex()
                            .items_center()
                            .text_size(px(TypeScale::SM))
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
                .child(div().flex().items_center().gap(px(6.)).h(px(metrics.bar_h() - 4.)).child(kit::icon(IconName::Braces, 14., p.accent_text)).child(div().font_weight(gpui_kit::FontWeight::MEDIUM).child(t("query.variables.title"))));
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
        if result_view == ResultView::Messages {
            let panel = log::LogPanel { entries: &log, batch: self.desk.log_batch(), locale, metrics, mono: mono.clone() };
            results_panel = results_panel.child(panel.render(&p, &entity));
            #[cfg(test)]
            {
                self.drawn_colors = Some((p.surface, p.chrome));
            }
        } else if active_result.is_none() {
            results_panel = results_panel.child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(kit::empty_state(IconName::Table, t("ui.query.results"), t("ui.query.noResults"), &p).child(div().flex().gap(px(6.)).child(kit::kbd(kit::shortcut("Ctrl+Enter"), &p)))),
            );
        } else if let Some(result) = active_result.clone().filter(|result| result.columns.is_empty()) {
            let ok = !(result.status == "error" || result.status == "failed");
            let message = if result.message.is_empty() { note.clone() } else { statement_message(locale, &result.status, &result.message) };
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
                    .child(div().font_family(mono.clone()).text_size(px(TypeScale::SM)).text_color(p.text_muted).child(SharedString::from(result.sql.clone()))),
            );
        } else {
            results_panel = results_panel.child(
                div()
                    .id("query-grid")
                    .test_support()
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
            | "pin-result"
            | "toggle-side"
    ) || id.starts_with("statement:")
        || id.starts_with("outline:")
        || id.starts_with("query-view:")
        || id.starts_with("unpin:")
        || id.starts_with("log:")
        || id.starts_with("side-mode:")
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

    /// 没执行到的语句：执行层写的是中文说明，界面按当前语言只显示状态词；其它说明（数据库错误）原样保留。
    #[test]
    fn skipped_statements_read_in_the_ui_language() {
        assert_eq!(statement_message(Locale::En, "skipped", "未执行"), "Skipped");
        assert_eq!(statement_message(Locale::ZhCn, "skipped", "未执行"), "未执行");
        assert_eq!(statement_message(Locale::En, "error", "no such table: x"), "no such table: x");
        let entry = crate::query::desk::LogEntry {
            statement: Some(2),
            sql: "select 3".to_owned(),
            status: "skipped".to_owned(),
            rows: None,
            affected: None,
            elapsed_ms: None,
            message: "未执行".to_owned(),
        };
        let en = log::entry_text(Locale::En, &entry);
        assert!(!en.chars().any(|ch| ('\u{4e00}'..='\u{9fff}').contains(&ch)), "{en}");
        assert!(en.starts_with("Skipped"), "{en}");
    }

    fn sheet(sql: &str, truncated: bool, status: &str) -> desk::StatementView {
        desk::StatementView {
            sql: sql.to_owned(),
            status: status.to_owned(),
            message: String::new(),
            columns: vec!["id".to_owned()],
            rows: vec![vec![Some("1".to_owned())]],
            truncated,
            affected: None,
            elapsed_ms: None,
            error_at: None,
            range: None,
            error_mark: None,
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

    #[test]
    fn load_more_is_only_offered_on_the_target_the_results_came_from() {
        let target = |connection: &str, database: &str| desk::Target {
            connection_id: connection.to_owned(),
            connection_name: connection.to_owned(),
            database: database.to_owned(),
            engine: "sqlite".to_owned(),
            schema: String::new(),
            environment: String::new(),
        };
        assert!(load_more_targets_match(Some(&target("a", "main")), Some(&target("a", "main"))));
        assert!(!load_more_targets_match(Some(&target("a", "main")), Some(&target("a", "other"))), "换库后不在新库续读旧结果");
        assert!(!load_more_targets_match(Some(&target("a", "main")), Some(&target("b", "main"))), "换连接后不续读");
        assert!(!load_more_targets_match(Some(&target("a", "main")), None), "标签已解绑不续读");
        assert!(load_more_targets_match(None, Some(&target("a", "main"))));
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
    fn completion_follows_the_current_statement_and_shows_the_metadata_state(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        let snap = bound("q", "SELECT * FROM orders o WHERE o.");
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
                pane.desk.remember_columns("notes", "main", "orders", vec!["id".to_owned(), "status".to_owned()]);
            });
            window.render_frame(cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| {
            pane.update(cx, |pane, cx| {
                let editor = pane.editors.get("q").unwrap().clone();
                let end = editor.read(cx).value(cx).len();
                editor.update(cx, |editor, cx| editor.select_range(end..end, cx));
                pane.command("autocomplete", cx);
            });
            window.render_frame(cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| {
            let (labels, note) = pane.update(cx, |pane, cx| {
                let editor = pane.editors.get("q").unwrap().clone();
                (editor.read(cx).completion_labels(cx), pane.note())
            });
            assert_eq!(labels, vec!["id".to_owned(), "status".to_owned()], "别名后只给该表的列：{labels:?}");
            assert!(note.is_empty(), "正常元数据下不该有状态说明：{note}");

            pane.update(cx, |pane, cx| {
                pane.desk.set_tree_error("读取库表失败：连接中断");
                let editor = pane.editors.get("q").unwrap().clone();
                editor.update(cx, |editor, cx| {
                    editor.set_sql("SELECT * FROM ", window, cx);
                    editor.select_range(14..14, cx);
                });
                pane.command("autocomplete", cx);
            });
            window.render_frame(cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(window, |_, _, cx| {
            let note = pane.read(cx).note();
            assert!(note.contains("连接中断"), "元数据失败要说明原因而不是空列表：{note}");
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
        // 快照通道按“下一批已就绪”推进，不再靠 25ms 忙轮询，所以这里只确认执行过程
        // 经过了轮询并且终态落地，不再用轮询次数当时间刻度的替代。
        assert!(polls >= 1, "这次执行没有经过轮询：{polls}");
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

    fn streamed_run(id: &str, status: &str, rows: &[&str]) -> RunDto {
        RunDto {
            id: id.to_owned(),
            connection_id: "notes".to_owned(),
            connection_name: "笔记".to_owned(),
            database: "main".to_owned(),
            engine: "sqlite".to_owned(),
            status: status.to_owned(),
            cancel: "none".to_owned(),
            cancel_message: String::new(),
            via_ssh: false,
            fresh_channel: false,
            statements: vec![crate::query::commands::StatementDto {
                sql: "SELECT * FROM thread_items".to_owned(),
                status: if desk::run_pending(status) { "running" } else { "ok" }.to_owned(),
                message: String::new(),
                columns: vec!["id".to_owned()],
                column_types: vec!["integer".to_owned()],
                rows: rows.iter().map(|value| vec![Some((*value).to_owned())]).collect(),
                truncated: false,
                affected: None,
                elapsed_ms: None,
                error_at: None,
            }],
            started_at: String::new(),
            elapsed_ms: 0,
        }
    }

    #[gpui_kit::test]
    fn intermediate_snapshots_reach_the_grid_before_the_run_ends(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            pane.update(cx, |pane, cx| {
                pane.desk.open_database("notes", "笔记", "sqlite", "main");
                pane.desk.set_draft("SELECT * FROM thread_items");
                let prepared = match pane.desk.begin_run(&pane.services.tasks, RunScope::Script, None) {
                    BeginRun::Ready(prepared) => prepared,
                    BeginRun::Rejected(reason) => panic!("{reason}"),
                    BeginRun::Variables(_) => panic!("variables"),
                };
                let ticket = Ticket { tab_id: prepared.tab_id.clone(), token: prepared.token, version: prepared.version };
                // 中间快照：行立刻显示，执行还没结束，也不能再起第二次。
                assert!(
                    pane.apply_run_snapshot(&ticket, Ok(streamed_run("run-1", "running", &["1", "2"])), cx),
                    "中间快照之后还要继续等下一批"
                );
                assert!(pane.running(), "中间快照清掉了运行态");
                assert_eq!(pane.cell(1, 0, cx), "2", "分批到达的行没有显示");
                assert!(!pane.executed());
                // 终态：补上剩余行并结束这次执行。
                assert!(
                    !pane.apply_run_snapshot(&ticket, Ok(streamed_run("run-1", "ok", &["1", "2", "3"])), cx),
                    "终态之后不该再等快照"
                );
                assert!(!pane.running());
                assert!(pane.executed());
                assert_eq!(pane.cell(2, 0, cx), "3");
            });
            window.render_frame(cx);
        })
        .unwrap();
    }

    /// 用户报的缺陷回归：行数超过一批时，第一次发布之后执行会停下等下一批。
    /// 只等终态的实现会一直停在“正在执行”，这里必须自己走到 200 行。
    #[gpui_kit::test]
    fn a_chunked_result_finishes_instead_of_staying_running(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let dir = std::env::temp_dir().join(format!("rillsql-chunked-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let database = dir.join("thread.sqlite");
        let config_dir = dir.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        runtime().spawn(async move {
            let setup = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).create_if_missing(true))
                .await
                .unwrap();
            sqlx::query("CREATE TABLE thread_items (id INTEGER PRIMARY KEY)").execute(&setup).await.unwrap();
            for id in 1..=200i64 {
                sqlx::query("INSERT INTO thread_items (id) VALUES (?)").bind(id).execute(&setup).await.unwrap();
            }
            setup.close().await;
            let services = Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(config_dir.join("connections.sqlite")))));
            services.connections.open_sqlite_for_test("notes", "笔记", &database).await.unwrap();
            let _ = tx.send(services);
        });
        let services = rx.recv_timeout(std::time::Duration::from_secs(30)).expect("sqlite setup");
        cx.update(gpui_kit::init);
        let (window, pane) = cx.update(|cx| {
            let bounds = gpui_kit::Bounds { origin: Point::default(), size: size(px(1100.), px(720.)) };
            gpui_kit::open_window(
                WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
                cx,
                |window, cx| cx.new(|cx| QueryPane::new(services, window, cx)),
            )
            .expect("query pane")
        });
        cx.update_window(window, |_, window, cx| {
            pane.update(cx, |pane, cx| {
                // 读取上限 250、结果 200 行：每批 125 行，第二批必须有人放行。
                pane.desk.set_page_size(250);
                let snap = bound("q", "SELECT id FROM thread_items ORDER BY id");
                let _ = pane.sync(std::slice::from_ref(&snap), Some("q"), Locale::ZhCn, window, cx);
                pane.command("execute-query", cx);
                assert!(pane.running(), "执行没有标成运行中：{}", pane.note());
            });
            window.render_frame(cx);
        })
        .unwrap();
        let landed = wait_for_tokio(cx, window, std::time::Duration::from_secs(30), |cx| {
            cx.update(|cx| {
                let pane = pane.read(cx);
                !pane.running() && pane.cell(199, 0, cx) == "200"
            })
        });
        let note = cx.update(|cx| pane.read(cx).note());
        assert!(landed, "分批读取没有走完：{note}");
        assert!(cx.update(|cx| pane.read(cx).executed()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn error_ranges_follow_the_driver_position_or_the_whole_statement() {
        let draft = "select 1;\nselect *\nfrom missing;\nselect 3;";
        let second = draft.find("select *").unwrap();
        let end = draft.find("missing;").unwrap() + "missing;".len();
        let failed = |mark| desk::StatementView { range: Some((second, end)), error_mark: Some(mark), ..sheet("x", false, "error") };
        // 驱动给了行：标整行（从 0 数的第 2 行）。
        let line = error_ranges(draft, &[failed(ErrorMark::Line(2))]);
        assert_eq!(line.iter().map(|range| &draft[range.clone()]).collect::<Vec<_>>(), vec!["from missing;"]);
        // 驱动给了偏移：标那个词。
        let at = draft.find("missing").unwrap();
        let offset = error_ranges(draft, &[failed(ErrorMark::Offset(at))]);
        assert_eq!(offset.iter().map(|range| &draft[range.clone()]).collect::<Vec<_>>(), vec!["missing"]);
        // 没有位置（SQLite）：整条失败语句。
        let whole = error_ranges(draft, &[failed(ErrorMark::Statement)]);
        assert_eq!(whole, vec![second..end]);
        // 成功的语句没有标记。
        assert!(error_ranges(draft, &[sheet("select 1", false, "ok")]).is_empty());
        // “at end of input”：偏移在语句末尾（全文末尾或换行上）不能得到空范围，改标前一个非空白字符。
        let tail = "select 1;\nselect * from";
        let at_end = |draft: &str, offset| {
            let view = desk::StatementView { range: Some((0, draft.len())), error_mark: Some(ErrorMark::Offset(offset)), ..sheet("x", false, "error") };
            error_ranges(draft, &[view])
        };
        let end = at_end(tail, tail.len());
        assert_eq!(end.len(), 1, "末尾位置必须有非空标记：{end:?}");
        assert!(!end[0].is_empty());
        assert_eq!(&tail[end[0].clone()], "m", "标到输入最后一个非空白字符");
        let crlf = "select * from\r\n";
        let end = at_end(crlf, "select * from".len());
        assert_eq!(end.iter().map(|range| &crlf[range.clone()]).collect::<Vec<_>>(), vec!["m"], "落在行尾 \\r\\n 上也标前一个字符");
        let trailing = "select * from  \n";
        let end = at_end(trailing, trailing.len());
        assert_eq!(end.iter().map(|range| &trailing[range.clone()]).collect::<Vec<_>>(), vec!["m"], "跳过末尾空白");
        let blank = "   ";
        assert_eq!(at_end(blank, blank.len()), vec![0..3], "前面全是空白时标整行");
        assert!(at_end("", 0).is_empty(), "空草稿没有可标的位置");
        assert_eq!(elapsed_label(12), "12 ms");
        assert_eq!(elapsed_label(1500), "1.50 s");
    }

    #[gpui_kit::test]
    fn outline_click_selects_and_reveals_the_statement(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        let draft = "select 1;\n\nselect 2;\n\nselect 3 from item;";
        let snap = bound("q", draft);
        cx.update_window(window, |_, window, cx| {
            pane.update(cx, |pane, cx| {
                let _ = pane.sync(std::slice::from_ref(&snap), Some("q"), Locale::ZhCn, window, cx);
            });
            window.render_frame(cx);
            pane.update(cx, |pane, cx| pane.command("outline:3", cx));
            window.render_frame(cx);
            let editor = pane.read(cx).editors.get("q").cloned().unwrap();
            assert_eq!(editor.read(cx).selected_text(cx).trim_end_matches(';'), "select 3 from item");
            pane.update(cx, |pane, cx| pane.command("outline:9", cx));
            assert_eq!(editor.read(cx).selected_text(cx).trim_end_matches(';'), "select 3 from item", "不存在的大纲项什么也不做");
            // 大纲行本身可以点击。
            window.click("query-outline-2", cx);
            window.render_frame(cx);
            assert_eq!(editor.read(cx).selected_text(cx).trim_end_matches(';'), "select 2");
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn a_failed_statement_is_marked_and_the_header_shows_timing(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let dir = std::env::temp_dir().join(format!("rillsql-marks-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let database = dir.join("marks.sqlite");
        let config_dir = dir.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        runtime().spawn(async move {
            let setup = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).create_if_missing(true))
                .await
                .unwrap();
            sqlx::query("CREATE TABLE item (id INTEGER PRIMARY KEY)").execute(&setup).await.unwrap();
            sqlx::query("INSERT INTO item (id) VALUES (1)").execute(&setup).await.unwrap();
            setup.close().await;
            let services = Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(config_dir.join("connections.sqlite")))));
            services.connections.open_sqlite_for_test("notes", "笔记", &database).await.unwrap();
            let _ = tx.send(services);
        });
        let services = rx.recv_timeout(std::time::Duration::from_secs(30)).expect("sqlite setup");
        cx.update(gpui_kit::init);
        let (window, pane) = cx.update(|cx| {
            let bounds = gpui_kit::Bounds { origin: Point::default(), size: size(px(1100.), px(720.)) };
            gpui_kit::open_window(
                WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
                cx,
                |window, cx| cx.new(|cx| QueryPane::new(services, window, cx)),
            )
            .expect("query pane")
        });
        let draft = "SELECT id FROM item;\nSELECT *\nFROM missing_table;";
        let run = |cx: &mut TestAppContext| {
            cx.update_window(window, |_, window, cx| {
                pane.update(cx, |pane, cx| {
                    pane.command("run-script", cx);
                    assert!(pane.running(), "执行没有标成运行中：{}", pane.note());
                    assert_eq!(pane.result_timing(), ResultTiming::Running, "执行中显示运行态");
                });
                let editor = pane.read(cx).editors.get("q").cloned().unwrap();
                assert!(editor.read(cx).error_ranges(cx).is_empty(), "重新执行时清掉上一次的错误标记");
                window.render_frame(cx);
            })
            .unwrap();
            let done = wait_for_tokio(cx, window, std::time::Duration::from_secs(30), |cx| {
                cx.update(|cx| {
                    let pane = pane.read(cx);
                    !pane.running() && pane.desk.results().len() == 2
                })
            });
            assert!(done, "执行没有结束：{}", cx.update(|cx| pane.read(cx).note()));
        };
        cx.update_window(window, |_, window, cx| {
            pane.update(cx, |pane, cx| {
                let snap = bound("q", draft);
                let _ = pane.sync(std::slice::from_ref(&snap), Some("q"), Locale::ZhCn, window, cx);
            });
            window.render_frame(cx);
        })
        .unwrap();
        run(cx);
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let editor = pane.read(cx).editors.get("q").cloned().unwrap();
            let marks = editor.read(cx).error_ranges(cx);
            assert_eq!(marks.len(), 1, "{:?} {:?}", marks, pane.read(cx).desk.results());
            let marked = &draft[marks[0].clone()];
            assert!(marked.contains("missing_table") && !marked.contains("FROM item"), "SQLite 没有位置时整条失败语句标错：{marked}");
            // 第一条成功：结果标题显示它自己的非零耗时。
            pane.update(cx, |pane, cx| pane.command("statement:0", cx));
            match pane.read(cx).result_timing() {
                ResultTiming::Elapsed(ms) => assert!(ms > 0),
                other => panic!("完成后应显示耗时：{other:?}"),
            }
            window.render_frame(cx);
            // 下一次编辑清掉标记。
            editor.update(cx, |editor, cx| editor.select_range(0..0, cx));
            window.press("x", cx);
        })
        .unwrap();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let editor = pane.read(cx).editors.get("q").cloned().unwrap();
            assert!(editor.read(cx).value(cx).starts_with('x'));
            assert!(editor.read(cx).error_ranges(cx).is_empty(), "编辑后错误标记清除");
            window.press("backspace", cx);
        })
        .unwrap();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let editor = pane.read(cx).editors.get("q").cloned().unwrap();
            assert_eq!(editor.read(cx).value(cx), draft);
            assert!(editor.read(cx).error_ranges(cx).is_empty(), "改回原文也不会把旧标记画回来");
        })
        .unwrap();
        // 再执行一次：开始时清掉，结束后重新标上。
        run(cx);
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let editor = pane.read(cx).editors.get("q").cloned().unwrap();
            assert_eq!(editor.read(cx).error_ranges(cx).len(), 1);
            // 取消：结果标题显示已取消。
            pane.update(cx, |pane, cx| {
                pane.command("execute-query", cx);
                pane.command("request-cancel", cx);
                assert_eq!(pane.result_timing(), ResultTiming::Cancelled);
            });
            window.render_frame(cx);
        })
        .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[gpui_kit::test]
    fn ctrl_click_hands_the_object_to_the_shell(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        type Seen = Rc<std::cell::RefCell<Vec<(ObjectLink, Option<String>)>>>;
        let seen: Seen = Rc::new(std::cell::RefCell::new(Vec::new()));
        let draft = "select customer from customers where nosuch = 1";
        cx.update_window(window, |_, window, cx| {
            let sink_seen = seen.clone();
            let owner = pane.downgrade();
            pane.update(cx, |pane, cx| {
                pane.plant_customers();
                pane.set_definition_sink(Rc::new(move |link: ObjectLink, cx: &mut App| {
                    // 外壳会回头读查询页（结构页规格）：这里同样读一次，确认不在查询页更新中。
                    let spec = owner.upgrade().and_then(|pane| pane.read(cx).definition_spec(&link));
                    sink_seen.borrow_mut().push((link, spec));
                }));
                let _ = pane.sync(std::slice::from_ref(&bound("q", draft)), Some("q"), Locale::ZhCn, window, cx);
            });
            window.render_frame(cx);
            let editor = pane.read(cx).editors.get("q").cloned().unwrap();
            let at = draft.find("customers").unwrap() + 2;
            assert!(editor.update(cx, |editor, cx| editor.follow_definition_at(at, cx)), "表名可以跳转");
            let unknown = draft.find("nosuch").unwrap() + 2;
            assert!(!editor.update(cx, |editor, cx| editor.follow_definition_at(unknown, cx)), "认不出的名字什么也不做");
        })
        .unwrap();
        cx.run_until_parked();
        let seen = seen.borrow();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0.connection_id, "notes");
        assert_eq!(seen[0].0.database, "main");
        assert_eq!(seen[0].0.name, "customers");
        assert!(seen[0].1.as_deref().is_some_and(|spec| spec.contains("customers")), "{:?}", seen[0].1);
    }

    fn result(sql: &str, status: &str, columns: &[&str], rows: &[&str], affected: Option<u64>, message: &str) -> desk::StatementView {
        desk::StatementView {
            sql: sql.to_owned(),
            status: status.to_owned(),
            message: message.to_owned(),
            columns: columns.iter().map(|name| (*name).to_owned()).collect(),
            rows: rows.iter().map(|value| vec![Some((*value).to_owned())]).collect(),
            truncated: false,
            affected,
            elapsed_ms: Some(5),
            error_at: None,
            range: None,
            error_mark: None,
        }
    }

    #[gpui_kit::test]
    fn messages_tab_lists_each_statement_and_follows_clicks(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let dir = std::env::temp_dir().join(format!("rillsql-log-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let database = dir.join("log.sqlite");
        let config_dir = dir.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        runtime().spawn(async move {
            let setup = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).create_if_missing(true))
                .await
                .unwrap();
            sqlx::query("CREATE TABLE item (id INTEGER PRIMARY KEY, label TEXT)").execute(&setup).await.unwrap();
            sqlx::query("INSERT INTO item (id, label) VALUES (1, 'a'), (2, 'b')").execute(&setup).await.unwrap();
            setup.close().await;
            let services = Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(config_dir.join("connections.sqlite")))));
            services.connections.open_sqlite_for_test("notes", "笔记", &database).await.unwrap();
            let _ = tx.send(services);
        });
        let services = rx.recv_timeout(std::time::Duration::from_secs(30)).expect("sqlite setup");
        cx.update(gpui_kit::init);
        let (window, pane) = cx.update(|cx| {
            let bounds = gpui_kit::Bounds { origin: Point::default(), size: size(px(1100.), px(720.)) };
            gpui_kit::open_window(
                WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
                cx,
                |window, cx| cx.new(|cx| QueryPane::new(services, window, cx)),
            )
            .expect("query pane")
        });
        let draft = "SELECT id FROM item;\nUPDATE item SET label = label;\nSELECT *\nFROM missing_table;";
        cx.update_window(window, |_, window, cx| {
            pane.update(cx, |pane, cx| {
                let _ = pane.sync(std::slice::from_ref(&bound("q", draft)), Some("q"), Locale::En, window, cx);
            });
            window.render_frame(cx);
            pane.update(cx, |pane, cx| pane.command("run-script", cx));
            window.render_frame(cx);
        })
        .unwrap();
        let done = wait_for_tokio(cx, window, std::time::Duration::from_secs(30), |cx| {
            cx.update(|cx| {
                let pane = pane.read(cx);
                !pane.running() && pane.desk.log().len() == 3
            })
        });
        assert!(done, "执行没有结束：{} {:?}", cx.update(|cx| pane.read(cx).note()), cx.update(|cx| pane.read(cx).desk.results()));
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let tab = window.find("query-log-tab");
            assert!(tab.visible());
            assert!(tab.label().is_some_and(|label| label.contains("Messages") && label.contains('3')), "{:?}", tab.label());
            window.click("query-log-tab", cx);
            window.render_frame(cx);
            assert_eq!(pane.read(cx).desk.result_view(), ResultView::Messages);
            let labels: Vec<String> = (0..3).map(|index| window.find(SharedString::from(format!("query-log-{index}"))).label().unwrap_or_default().to_owned()).collect();
            assert!(labels[0].starts_with("Succeeded") && labels[0].contains("Result 1") && labels[0].contains("2 rows") && labels[0].contains(" ms"), "{labels:?}");
            assert!(labels[1].starts_with("Succeeded") && labels[1].contains("Result 2") && labels[1].contains("2 affected"), "{labels:?}");
            assert!(labels[2].starts_with("Failed") && labels[2].contains("Result 3") && labels[2].contains("missing_table"), "失败一条要带错误：{labels:?}");
            for entry in pane.read(cx).desk.log() {
                assert!(entry.elapsed_ms.is_some(), "每条都有耗时：{entry:?}");
            }

            // 点成功的一条：切到它的结果。
            window.click("query-log-0", cx);
            window.render_frame(cx);
            assert_eq!(pane.read(cx).desk.result_view(), ResultView::Current);
            assert_eq!(pane.read(cx).desk.statement(), 0);
            assert_eq!(pane.read(cx).cell(0, 0, cx), "1", "切到第一条的结果表");

            // 点失败的一条：编辑器选中错误位置（SQLite 没有位置，整条失败语句）。
            window.click("query-log-tab", cx);
            window.render_frame(cx);
            window.click("query-log-2", cx);
            window.render_frame(cx);
            let editor = pane.read(cx).editors.get("q").cloned().unwrap();
            assert_eq!(editor.read(cx).selected_text(cx).trim_end_matches(';'), "SELECT *\nFROM missing_table", "跳到失败语句");
            assert_eq!(pane.read(cx).desk.result_view(), ResultView::Messages, "跳错误位置时留在消息页");
        })
        .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[gpui_kit::test]
    fn a_pinned_result_survives_the_next_run_and_closes_by_hand(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            pane.update(cx, |pane, cx| {
                let _ = pane.sync(std::slice::from_ref(&bound("q", "select 'a'")), Some("q"), Locale::ZhCn, window, cx);
                pane.desk.plant_run(vec![result("select 'a'", "ok", &["v"], &["a"], None, "")]);
            });
            window.render_frame(cx);
            assert_eq!(pane.read(cx).cell(0, 0, cx), "a");
            assert!(window.try_find("query-pinned-1").is_none());
            window.click("query-act-pin", cx);
            window.render_frame(cx);
            assert!(window.find("query-pinned-1").visible(), "固定后多出一个固定页签");
            assert_eq!(window.find("query-pinned-1").selected(), Some(true));
            assert_eq!(pane.read(cx).cell(0, 0, cx), "a");

            // 再执行一次：固定页签还在、内容不变，新结果落在当前结果页。
            pane.update(cx, |pane, _| pane.desk.plant_run(vec![result("select 'b'", "ok", &["v"], &["b"], None, "")]));
            window.render_frame(cx);
            assert!(window.find("query-pinned-1").visible(), "固定结果被下一次执行替换掉了");
            assert_eq!(window.find("query-results-tab").selected(), Some(true), "新结果打开在当前结果页");
            assert_eq!(pane.read(cx).cell(0, 0, cx), "b");
            window.click("query-pinned-1", cx);
            window.render_frame(cx);
            assert_eq!(pane.read(cx).cell(0, 0, cx), "a", "固定结果内容不变");

            // 未固定的结果照旧被替换。
            pane.update(cx, |pane, _| pane.desk.plant_run(vec![result("select 'c'", "ok", &["v"], &["c"], None, "")]));
            window.render_frame(cx);
            assert_eq!(pane.read(cx).cell(0, 0, cx), "c");
            assert_eq!(pane.read(cx).desk.pinned().len(), 1);

            // 手动关闭固定结果。
            window.click("query-pinned-1", cx);
            window.render_frame(cx);
            window.click("query-pinned-close-1", cx);
            window.render_frame(cx);
            assert!(window.try_find("query-pinned-1").is_none(), "固定页签可以手动关闭");
            assert!(pane.read(cx).desk.pinned().is_empty());
            assert_eq!(pane.read(cx).cell(0, 0, cx), "c", "关掉正在看的固定结果后回到当前结果");
        })
        .unwrap();
    }

    fn run_in(pane: &mut QueryPane) -> Ticket {
        let prepared = match pane.desk.begin_run(&pane.services.tasks, RunScope::Script, None) {
            BeginRun::Ready(prepared) => prepared,
            BeginRun::Rejected(reason) => panic!("{reason}"),
            BeginRun::Variables(_) => panic!("variables"),
        };
        Ticket { tab_id: prepared.tab_id.clone(), token: prepared.token, version: prepared.version }
    }

    #[gpui_kit::test]
    fn a_pinned_result_stays_bound_to_the_database_it_came_from(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            pane.update(cx, |pane, cx| {
                let _ = pane.sync(std::slice::from_ref(&bound("q", "SELECT * FROM thread_items")), Some("q"), Locale::ZhCn, window, cx);
                // 在库 A（main）上执行并固定。
                let ticket = run_in(pane);
                assert!(!pane.apply_run_snapshot(&ticket, Ok(streamed_run("run-1", "ok", &["1"])), cx));
                pane.command("pin-result", cx);
                assert_eq!(pane.desk.pinned().len(), 1);
                assert_eq!(pane.desk.pinned()[0].target.as_ref().map(|target| target.database.as_str()), Some("main"));
                // 标签切到库 B。
                pane.command("database:other", cx);
                assert_eq!(pane.desk.target().map(|target| target.database), Some("other".to_owned()));
                // 先回当前结果，再点固定页签：表格重新绑定。
                pane.command("query-view:current", cx);
            });
            window.render_frame(cx);
            let (target, _) = pane.update(cx, |pane, cx| pane.grid_binding(cx));
            assert_eq!(target.database, "main", "当前结果也仍是产生它的库");
            pane.update(cx, |pane, cx| pane.command("query-view:pinned:1", cx));
            window.render_frame(cx);
            assert_eq!(pane.read(cx).desk.result_view(), ResultView::Pinned(1));
            let (target, writable) = pane.update(cx, |pane, cx| pane.grid_binding(cx));
            assert_eq!(target.connection_id, "notes");
            assert_eq!(target.database, "main", "固定结果不能绑到切换后的库 B");
            assert!(!writable);
            assert_eq!(pane.read(cx).cell(0, 0, cx), "1");

            // 来源无法证明的结果（外部写入）固定后不带连接：表格不会定位，只读。
            pane.update(cx, |pane, cx| {
                let tabs = [bound("q", "SELECT * FROM thread_items"), bound("u", "select 'x'")];
                let _ = pane.sync(&tabs, Some("u"), Locale::ZhCn, window, cx);
                pane.show_sheet(vec!["label".to_owned()], vec![vec!["x".to_owned()]], cx);
                pane.command("pin-result", cx);
            });
            window.render_frame(cx);
            assert_eq!(pane.read(cx).desk.result_view(), ResultView::Pinned(1));
            assert!(pane.read(cx).desk.pinned()[0].target.is_none());
            let (target, writable) = pane.update(cx, |pane, cx| pane.grid_binding(cx));
            assert!(target.connection_id.is_empty() && target.database.is_empty(), "{target:?}");
            assert!(!writable);
            assert_eq!(pane.read(cx).cell(0, 0, cx), "x");
        })
        .unwrap();
    }

    fn two_statement_run(status: &str, second: &str) -> RunDto {
        let mut run = streamed_run("run-1", status, &["1"]);
        let mut first = run.statements[0].clone();
        first.status = "ok".to_owned();
        first.elapsed_ms = Some(3);
        let mut next = first.clone();
        next.sql = "SELECT 2".to_owned();
        next.status = second.to_owned();
        next.elapsed_ms = None;
        run.statements = vec![first, next];
        run
    }

    #[gpui_kit::test]
    fn a_cancelled_or_failed_run_still_logs_the_statements_that_finished(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        cx.update_window(window, |_, window, cx| {
            pane.update(cx, |pane, cx| {
                let _ = pane.sync(std::slice::from_ref(&bound("q", "SELECT * FROM thread_items; SELECT 2")), Some("q"), Locale::ZhCn, window, cx);
                // 取消：第一条已完成，第二条还在执行。
                let ticket = run_in(pane);
                assert!(pane.apply_run_snapshot(&ticket, Ok(two_statement_run("running", "running")), cx));
                pane.command("request-cancel", cx);
                let log = pane.desk.log();
                assert_eq!(log.len(), 2, "{log:?}");
                assert_eq!(log[0].statement, Some(0));
                assert_eq!(log[0].status, "ok");
                assert_eq!(log[0].elapsed_ms, Some(3));
                assert_eq!(log[1].statement, None);
                assert_eq!(log[1].status, "cancelled");

                // 整次失败（连接中断）：同样先记已完成的语句。
                let ticket = run_in(pane);
                assert!(pane.desk.log().is_empty(), "新一次执行清空消息");
                assert!(pane.desk.stop_run("q", ticket.token, "连接中断"), "没有快照时也要结束");
                let log = pane.desk.log();
                assert_eq!(log.len(), 1, "本次没有快照时不把上一次的结果记进来：{log:?}");
                assert_eq!(log[0].statement, None);

                let ticket = run_in(pane);
                assert!(pane.apply_run_snapshot(&ticket, Ok(two_statement_run("running", "pending")), cx));
                assert!(pane.desk.stop_run("q", ticket.token, "连接中断"));
                let log = pane.desk.log();
                assert_eq!(log.len(), 2, "{log:?}");
                assert_eq!((log[0].statement, log[0].status.as_str()), (Some(0), "ok"));
                assert_eq!((log[1].statement, log[1].status.as_str(), log[1].message.as_str()), (None, "error", "连接中断"));
            });
        })
        .unwrap();
    }

    #[test]
    fn pane_commands_only_act_on_the_active_query() {
        for id in ["pin-result", "toggle-side", "query-view:current", "query-view:pinned:1", "unpin:1", "log:0", "side-mode:object"] {
            assert!(requires_active_query(id), "{id}");
        }
    }

    #[gpui_kit::test]
    fn pane_commands_refuse_a_hidden_query(cx: &mut TestAppContext) {
        let (window, pane) = open(cx);
        let snap = bound("q", "select 1");
        cx.update_window(window, |_, window, cx| {
            pane.update(cx, |pane, cx| {
                let _ = pane.sync(std::slice::from_ref(&snap), Some("q"), Locale::ZhCn, window, cx);
                pane.desk.plant_run(vec![result("select 1", "ok", &["v"], &["1"], None, "")]);
                let _ = pane.sync(std::slice::from_ref(&snap), Some("table-1"), Locale::ZhCn, window, cx);
                assert!(!pane.context_active);
                pane.command("pin-result", cx);
                assert!(pane.desk.pinned().is_empty(), "不得固定隐藏查询的结果");
                assert!(pane.note().contains("不是查询"), "{}", pane.note());
                pane.command("query-view:messages", cx);
                assert_eq!(pane.desk.result_view(), ResultView::Current);
                let before = pane.side.get("q").copied();
                pane.command("toggle-side", cx);
                pane.command("side-mode:object", cx);
                assert_eq!(pane.side.get("q").copied(), before, "不得改隐藏查询的右侧面板");
            });
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn the_side_panel_switches_views_and_keeps_its_state_per_tab(cx: &mut TestAppContext) {
        use crate::query::assist::ColumnMeta;
        let (window, pane) = open(cx);
        let draft = "select status from customers";
        let a = bound("a", draft);
        let b = bound("b", "select 1;\nselect 2;");
        cx.update_window(window, |_, window, cx| {
            pane.update(cx, |pane, cx| {
                pane.plant_customers();
                pane.desk.remember_table_columns(
                    "notes",
                    "main",
                    "",
                    "customers",
                    vec![
                        ColumnMeta { name: "id".to_owned(), type_name: "INTEGER".to_owned(), nullable: Some(false), comment: String::new() },
                        ColumnMeta { name: "status".to_owned(), type_name: "TEXT".to_owned(), nullable: Some(true), comment: "订单状态".to_owned() },
                    ],
                );
                let _ = pane.sync(&[a.clone(), b.clone()], Some("a"), Locale::ZhCn, window, cx);
            });
            window.render_frame(cx);
            // 宽窗口里默认在编辑器旁边打开，先是大纲。
            let panel = window.find("query-side-panel").bounds();
            let editor = window.find("query-editor").bounds();
            assert!(f32::from(editor.origin.x + editor.size.width) <= f32::from(panel.origin.x) + 0.5, "右侧面板在编辑器旁边，不盖住编辑器");
            assert!(window.try_find("query-outline").is_some());
            assert_eq!(window.find("query-side-mode-outline").selected(), Some(true));

            // 切到对象：光标在 customers 上时列出列、类型、可空和注释。
            window.click("query-side-mode-object", cx);
            let editor = pane.read(cx).editors.get("a").cloned().unwrap();
            let at = draft.find("customers").unwrap() + 3;
            editor.update(cx, |editor, cx| editor.select_range(at..at, cx));
            window.render_frame(cx);
            assert!(window.try_find("query-outline").is_none());
            assert!(window.find("query-side-object-name").label().is_some_and(|label| label.contains("customers")));
            let columns: Vec<String> = (0..2).map(|index| window.find(SharedString::from(format!("query-side-column-{index}"))).label().unwrap_or_default().to_owned()).collect();
            assert!(columns[0].contains("id") && columns[0].contains("INTEGER") && columns[0].contains("非空"), "{columns:?}");
            assert!(columns[1].contains("status") && columns[1].contains("TEXT") && columns[1].contains("订单状态"), "{columns:?}");
            // 光标落在列上：信息仍是它所在的表，这一列被标出来。
            let at = draft.find("status").unwrap() + 2;
            editor.update(cx, |editor, cx| editor.select_range(at..at, cx));
            window.render_frame(cx);
            assert_eq!(window.find("query-side-column-1").selected(), Some(true));
            assert_eq!(window.find("query-side-column-0").selected(), Some(false));
            // 认不出的位置说明没有对象。
            editor.update(cx, |editor, cx| editor.select_range(0..0, cx));
            window.render_frame(cx);
            assert!(window.try_find("query-side-object-none").is_some());

            // 另一个标签有自己的状态：在 b 里收起，回到 a 仍是打开的对象页。
            pane.update(cx, |pane, cx| {
                let _ = pane.sync(&[a.clone(), b.clone()], Some("b"), Locale::ZhCn, window, cx);
            });
            window.render_frame(cx);
            assert!(window.try_find("query-outline").is_some(), "b 还是默认的大纲页");
            window.click("query-side-close", cx);
            window.render_frame(cx);
            assert!(window.try_find("query-side-panel").is_none(), "收起后不再画右侧面板");
            pane.update(cx, |pane, cx| {
                let _ = pane.sync(&[a.clone(), b.clone()], Some("a"), Locale::ZhCn, window, cx);
            });
            window.render_frame(cx);
            assert!(window.try_find("query-side-panel").is_some(), "a 的面板状态不受 b 影响");
            assert_eq!(window.find("query-side-mode-object").selected(), Some(true));
            pane.update(cx, |pane, cx| {
                let _ = pane.sync(&[a.clone(), b.clone()], Some("b"), Locale::ZhCn, window, cx);
            });
            window.render_frame(cx);
            assert!(window.try_find("query-side-panel").is_none(), "b 保持收起");
            window.click("query-side-toggle", cx);
            window.render_frame(cx);
            assert!(window.try_find("query-side-panel").is_some(), "工具条按钮重新打开");
        })
        .unwrap();
    }
}
