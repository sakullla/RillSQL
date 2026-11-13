//! GPUI 外壳。菜单、工具栏、活动栏、侧栏、标签、状态栏、命令面板和六类菜单走同一个 dispatch。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use gpui_kit::{
    App, AppContext as _, Context, Entity, FocusHandle, Focusable, IntoElement, PathPromptOptions, Render, SharedString, Window,
    WindowAppearance,
};

use crate::app::views::admin::AdminPane;
use crate::app::views::compare::{CompareContext, ComparePane};
use crate::app::views::connections::blank_draft;
use crate::app::views::diagram::DiagramPane;
use crate::app::views::export::{ExchangePane, ExchangeSource, ExchangeSourceKind};
use crate::app::views::grid::{GridPane, GridTarget};
use crate::app::views::mongo::MongoPane;
use crate::app::views::query::{requires_active_query, QueryPane, SyncEffect, TabSnap};
use crate::app::views::redis::RedisPane;
use crate::app::views::structure::StructurePane;
use crate::app::model::ObjectTarget;
use crate::app::views::diagram::DiagramOpen;
use crate::app::{AppPage, Effect};
use crate::connections::model::Engine;
use crate::state::Platform;
use crate::application::AppServices;
use crate::ui::Foundation;

use super::jobs::{self, apply_done, job_request};
use super::menus::NavKey;
use super::model::{DraftField, PendingJob, WorkItemKind, WorkTool, WorkbenchModel};
use crate::app::Shell;

#[derive(PartialEq, Eq)]
enum ExchangeStamp {
    Query(Option<String>, bool, (Option<String>, u64, usize, bool, String)),
    Table(Option<String>, bool, (GridTarget, (u64, usize, u64, bool), bool)),
}

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
    applied_locale: Option<crate::i18n::Locale>,
    /// 共享表格网格当前展示的表规格；暂存必须归属到这张表对应的工作项。
    table_view_spec: Option<String>,
    /// 交换面板显式选择的来源；切换工作项后按新工作项重建。
    exchange_kind: ExchangeSourceKind,
    exchange_item: Option<String>,
    exchange_stamp: Option<ExchangeStamp>,
    applied_diagram: Option<String>,
    applied_redis: Option<String>,
    applied_mongo: Option<String>,
    services: Arc<AppServices>,
    pub(super) focus: FocusHandle,
    /// 对象树自己的焦点；点击树行后方向键归树，离开时归还编辑器。
    pub(super) nav_focus: FocusHandle,
    /// 启动台"新建连接"按钮：模态关闭后焦点回到触发它的这个控件。
    pub(super) conn_launcher_focus: FocusHandle,
    /// 对象树空态"新建连接"按钮：同上。
    pub(super) conn_sidebar_focus: FocusHandle,
    /// 连接模态页脚按钮的焦点句柄：Tab 序列在模态内闭合，不逃到下面页面。
    pub(super) modal_close_focus: FocusHandle,
    pub(super) modal_test_focus: FocusHandle,
    pub(super) modal_save_focus: FocusHandle,
    pub(super) modal_save_connect_focus: FocusHandle,
    close_hooked: bool,
    booted: bool,
    health_task: Option<gpui_kit::Task<()>>,
    boot: bool,
    sized: bool,
    last_state: String,
    revision: Arc<AtomicU64>,
    persist_gate: Arc<tokio::sync::Mutex<()>>,
    pub(super) fields: crate::ui::kit::Fields<DraftField>,
    pub(super) menu_at: gpui_kit::Point<gpui_kit::Pixels>,
    /// 启动台筛选浮层的锚点：来自按钮上这一次真实点击。
    pub(super) conn_filter_at: gpui_kit::Point<gpui_kit::Pixels>,
    pub(super) palette_focused: bool,
    pub(super) pending_browser: Option<String>,
    connected_seen: std::collections::BTreeSet<String>,
    nav_refresh_active: std::collections::BTreeSet<Option<String>>,
    nav_refresh_again: std::collections::BTreeSet<Option<String>>,
    nav_refresh_tasks: std::collections::HashMap<Option<String>, futures::future::AbortHandle>,
    nav_revision: std::collections::HashMap<String, u64>,
    nav_reads: std::collections::HashMap<String, (String, futures::future::AbortHandle)>,
    nav_gate: Arc<crate::catalog::nav_read::MetadataGate>,
    /// 定义查看器的只读编辑器（SQL 高亮），按读取编号填入文本。
    pub(super) ddl_editor: Option<(u64, Entity<gpui_kit::component::input::EditorState>)>,
    /// 对象树列表的滚动；跳转定义后把选中行滚进可见区。
    pub(super) explorer_scroll: gpui_kit::ScrollHandle,
    /// 切到次级页面后，下一帧把键盘焦点交回工作区根。
    pub(super) restore_pending: bool,
    pub(super) seen_page: AppPage,
    admin_link: Option<(String, bool)>,
    /// 历史预览的只读 SQL 编辑器，按条目 id 与原文重建。
    pub(super) history_editor: Option<(String, Entity<gpui_kit::component::input::EditorState>)>,
    _child_subscriptions: Vec<gpui_kit::Subscription>,
}

impl Drop for Workbench {
    fn drop(&mut self) {
        for (_, handle) in self.nav_reads.values() { handle.abort(); }
        for handle in self.nav_refresh_tasks.values() { handle.abort(); }
    }
}

impl Workbench {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let services = Arc::new(AppServices::from_state(&crate::connections::commands::ConnectionsState::new()));
        Self::with_services(window, cx, services, true)
    }

    pub fn with_services(window: &mut Window, cx: &mut Context<Self>, services: Arc<AppServices>, boot: bool) -> Self {
        let metadata_gate = Arc::clone(&services.metadata_gate);
        let foundation = cx.new(|cx| Foundation::new(window, cx));
        let query = cx.new(|cx| QueryPane::new(Arc::clone(&services), window, cx));
        let table = cx.new(|cx| GridPane::new(Arc::clone(&services), window, cx));
        // 外键跳转的出口：网格只算出目标和定位查询，真正打开工作项由外壳负责。
        // 这条接线就是"跳转"与"只复制 SQL"的区别，所以必须在这里接上，而不是留给网格自己。
        let nav_owner = cx.entity().downgrade();
        table.update(cx, |pane, _| {
            pane.set_navigation_sink(Arc::new(move |navigation: crate::app::views::grid::GridNavigation, cx: &mut App| {
                if let Some(owner) = nav_owner.upgrade() {
                    owner.update(cx, |view, cx| view.open_reference(navigation, cx));
                }
            }));
        });
        // 编辑器里 Ctrl/⌘+点击对象的出口：查询页只解析出对象，树定位和结构页由外壳负责。
        let definition_owner = cx.entity().downgrade();
        query.update(cx, |pane, _| {
            pane.set_definition_sink(std::rc::Rc::new(move |link: crate::ui::ObjectLink, cx: &mut App| {
                if let Some(owner) = definition_owner.upgrade() {
                    owner.update(cx, |view, cx| view.go_to_definition(link, cx));
                }
            }));
        });
        let structure = cx.new(|_| StructurePane::new(Arc::clone(&services)));
        let admin = cx.new(|_| AdminPane::new(Arc::clone(&services)));
        let exchange = cx.new(|_| ExchangePane::new(Arc::clone(&services)));
        let compare = cx.new(|_| ComparePane::new(Arc::clone(&services)));
        let diagram = cx.new(|_| DiagramPane::new(Arc::clone(&services)));
        let redis = cx.new(|_| RedisPane::new(Arc::clone(&services)));
        let mongo = cx.new(|_| MongoPane::new(Arc::clone(&services)));
        let child_subscriptions = vec![
            cx.observe(&query, |_, _, cx| cx.notify()),
            cx.observe(&table, |_, _, cx| cx.notify()),
            cx.observe(&exchange, |_, _, cx| cx.notify()),
            cx.observe(&structure, |_, _, cx| cx.notify()),
            cx.observe(&compare, |_, _, cx| cx.notify()),
            cx.observe(&diagram, |_, _, cx| cx.notify()),
            cx.observe(&redis, |_, _, cx| cx.notify()),
            cx.observe(&mongo, |_, _, cx| cx.notify()),
        ];
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
            applied_locale: None,
            table_view_spec: None,
            exchange_kind: ExchangeSourceKind::Table,
            exchange_item: None,
            exchange_stamp: None,
            applied_diagram: None,
            applied_redis: None,
            applied_mongo: None,
            services,
            focus: cx.focus_handle().tab_stop(true),
            nav_focus: cx.focus_handle().tab_stop(true),
            conn_launcher_focus: cx.focus_handle().tab_stop(true),
            conn_sidebar_focus: cx.focus_handle().tab_stop(true),
            modal_close_focus: cx.focus_handle().tab_stop(true),
            modal_test_focus: cx.focus_handle().tab_stop(true),
            modal_save_focus: cx.focus_handle().tab_stop(true),
            modal_save_connect_focus: cx.focus_handle().tab_stop(true),
            close_hooked: false,
            booted: false,
            health_task: None,
            boot,
            sized: false,
            last_state: String::new(),
            revision: Arc::new(AtomicU64::new(0)),
            persist_gate: Arc::new(tokio::sync::Mutex::new(())),
            fields: crate::ui::kit::Fields::default(),
            menu_at: gpui_kit::Point::default(),
            conn_filter_at: gpui_kit::Point::default(),
            palette_focused: false,
            pending_browser: None,
            connected_seen: std::collections::BTreeSet::new(),
            nav_refresh_active: std::collections::BTreeSet::new(),
            nav_refresh_again: std::collections::BTreeSet::new(),
            nav_refresh_tasks: std::collections::HashMap::new(),
            nav_revision: std::collections::HashMap::new(),
            nav_reads: std::collections::HashMap::new(),
            nav_gate: metadata_gate,
            ddl_editor: None,
            explorer_scroll: gpui_kit::ScrollHandle::new(),
            restore_pending: false,
            seen_page: AppPage::Workspace,
            admin_link: None,
            history_editor: None,
            _child_subscriptions: child_subscriptions,
        }
    }

    pub fn shell(&self) -> &Shell {
        &self.model.shell
    }

    pub fn perform(&mut self, id: &str, cx: &mut Context<Self>) {
        // 统一按需连接：树展开、连接行双击、打开数据/结构、新建查询到此对象都先到这里。
        // 已连接时返回 false，命令照常执行；否则连接在后台建立，动作排队等一次续接。
        if self.model.gate_on_demand(id) {
            self.persist_if_needed();
            cx.notify();
            return;
        }
        self.refresh_exchange_data(cx, self.model.exchange_open || id == "exchange-export-loaded");
        if id == "refresh-tree" {
            // 目录树与查询页的对象索引各自刷新；树保留展开、焦点与已读子节点。
            self.model.request_nav_refresh(None);
            cx.notify();
            return;
        }
        if id.starts_with("nav-insert-")
            || matches!(
                id,
                "tree-insert-object" | "tree-insert-select" | "tree-insert-column" | "tree-template-insert" | "tree-template-update" | "tree-template-delete"
            )
        {
            // 插入前先把编辑器里的最新文本收回草稿，追加时才不会丢掉刚输入的内容。
            // 对象树菜单和导航命令走同一条捕获路径。
            self.capture_query(cx);
        }
        // 查询命令只在查询工作项处于活动状态时生效；表格/结构等页面不能操作隐藏查询。
        if requires_active_query(id) && !self.active_query_context() {
            self.model.notice_key = Some("query.errors.needTarget");
            self.model.notice_detail = crate::app::views::query::NOT_ACTIVE_QUERY.to_owned();
            self.persist_if_needed();
            cx.notify();
            return;
        }
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
            // 显式切换来源时固定到当前工作项，后续渲染不再按默认来源覆盖它。
            if let Some(kind) = id.strip_prefix("exchange-source:") {
                self.exchange_kind = match kind {
                    "query" => ExchangeSourceKind::Query,
                    _ => ExchangeSourceKind::Table,
                };
                self.exchange_item = self.model.active_tab.clone();
            }
            let source = self.exchange_source(cx, true);
            self.exchange.update(cx, |pane, cx| {
                pane.set_locale(locale);
                pane.open = open;
                pane.bind_source(source);
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
            // 对比入口按触发面取目标：网格菜单只认正在显示的表格/结果，
            // 对象树菜单认树里的选择，命令面板与快捷键用当前工作项或对象树选择。
            // 目标在切换页面前取，避免“打开对比”自己改掉页面的判定。
            let context = match id {
                // 网格菜单来自表格/结果右键：目标必须与屏幕上显示的数据一致，
                // 不能拿对象树里的另一行当当前对象。
                "compare-grid" => self.grid_compare_target(),
                // 对象菜单以右键/当前对象为准；没有选中对象时退回当前工作项的目标。
                // 文档集合没有关系型比较：说明原因，不换成别的对象。
                "compare-object" => {
                    let target = self.model.object_target().or_else(|| self.model.compare_target());
                    if target.as_ref().is_some_and(|target| !target.is_relational()) {
                        self.model.notice_key = Some("catalog.tree.relationalOnly");
                        self.model.notice_detail = target.as_ref().map(|target| target.name.clone()).unwrap_or_default();
                        self.persist_if_needed();
                        cx.notify();
                        return;
                    }
                    target
                }
                "compare" => self.model.compare_target(),
                _ => None,
            }
            .map(|target| CompareContext::of(&target));
            let _ = self.model.dispatch(id);
            let locale = self.model.locale();
            self.compare.update(cx, |pane, cx| {
                pane.set_locale(locale);
                if id == "compare" || id == "compare-object" || id == "compare-grid" {
                    pane.open_context(context, cx);
                }
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

    /// 网格上下文菜单的当前目标：表格页用正在显示的表，结果页用该工作的连接/库。
    /// 这里刻意不回退对象树选择，否则右键“对比”会带走树里另一行。
    fn grid_compare_target(&self) -> Option<crate::app::model::ObjectTarget> {
        if self.model.shell.page == crate::app::AppPage::Workspace && self.model.tool == WorkTool::Table {
            if let Some(target) = self.table_view_spec.as_deref().and_then(crate::app::model::ObjectTarget::parse) {
                return Some(target);
            }
        }
        let tab = self.model.active_tab()?;
        tab.spec
            .as_deref()
            .and_then(crate::app::model::ObjectTarget::parse)
            .or_else(|| self.model.active_scope())
    }

    /// 外键跳转：真的打开引用表的工作项，并带上复合/单键的定位查询。
    /// 打开的是新的查询工作项，所以原来那张表的选择、滚动、暂存和查看目标都还在原处。
    pub(super) fn open_reference(&mut self, navigation: crate::app::views::grid::GridNavigation, cx: &mut Context<Self>) {
        self.model.open_bound_query(
            navigation.sql.clone(),
            Some(navigation.connection_id.clone()),
            Some(navigation.connection_name.clone()),
            Some(navigation.database.clone()),
            Some(navigation.engine.clone()),
        );
        let live = self.services.connections.catalog_session(&navigation.connection_id).is_some();
        if live {
            // 只读定位 SELECT：直接执行，界面落到引用行；执行结果走查询页自己的失败说明。
            self.forward_query("execute-query", cx);
        } else {
            // 连接没打开时不假装跳到行：语句留在编辑器里，原因写清楚。
            self.model.notice_detail = crate::i18n::text(self.model.locale(), "grid.fk.prepared");
        }
        cx.notify();
    }

    /// 跳转定义：在对象树里定位并选中对象，表和视图再打开结构页。
    pub(super) fn go_to_definition(&mut self, link: crate::ui::ObjectLink, cx: &mut Context<Self>) {
        let fallback = self.query.read(cx).definition_spec(&link);
        let _ = self.model.go_to_definition(&link.connection_id, &link.database, &link.schema, &link.name, &link.kind, fallback);
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
    pub fn plant_same_name_objects(&mut self, cx: &mut Context<Self>) {
        self.query.update(cx, |pane, _| pane.plant_same_name_objects());
    }

    /// 对比页的两侧、带入说明与“返回工作项”在测试里都要能读到。
    #[cfg(test)]
    pub fn compare_state(&self, cx: &App) -> (Option<String>, String, String, String) {
        let pane = self.compare.read(cx);
        (
            pane.context().map(|context| context.path.clone()),
            pane.context_note().to_owned(),
            pane.sides().0,
            pane.sides().1,
        )
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

    /// 把编辑器里的最新文本收回草稿，并把光标/选区交给模型（SQL 模板按它插入）。
    fn capture_query(&mut self, cx: &mut Context<Self>) {
        self.forward_query("capture", cx);
        if let Some((tab_id, from, to)) = self.query.read(cx).captured_selection() {
            self.model.set_draft_cursor(&tab_id, from, to);
        }
    }

    fn forward_query(&mut self, id: &str, cx: &mut Context<Self>) {
        let snaps = self.query_snaps();
        let active = self.model.active_tab.clone();
        let locale = self.model.locale();
        let timeout = self.model.settings.query_timeout_sec;
        let context = self.active_query_context();
        let density = self.model.shell.density;
        let effect = self.query.update(cx, |pane, cx| {
            pane.set_query_timeout_sec(timeout);
            if pane.set_density(density) {
                cx.notify();
            }
            let _ = pane.prepare(&snaps, active.as_deref(), locale, context, cx);
            pane.apply_grid_locale(locale, cx);
            pane.command(id, cx);
            let effect = pane.prepare(&snaps, active.as_deref(), locale, context, cx);
            pane.apply_grid_locale(locale, cx);
            effect
        });
        self.apply_query_effect(effect);
    }

    /// 密度发布成全局尺寸：网格、记录视图、结构/Mongo/Redis/对比等子视图都从 `Metrics::of` 取行高与控件高。
    /// 变了就通知全部子视图重绘，切换密度立即生效。
    fn sync_density(&mut self, cx: &mut Context<Self>) {
        if !self.model.shell.density.metrics().publish(cx) {
            return;
        }
        self.query.update(cx, |_, cx| cx.notify());
        self.table.update(cx, |_, cx| cx.notify());
        self.structure.update(cx, |_, cx| cx.notify());
        self.admin.update(cx, |_, cx| cx.notify());
        self.exchange.update(cx, |_, cx| cx.notify());
        self.compare.update(cx, |_, cx| cx.notify());
        self.diagram.update(cx, |_, cx| cx.notify());
        self.redis.update(cx, |_, cx| cx.notify());
        self.mongo.update(cx, |_, cx| cx.notify());
    }

    /// 公开设置只在这里下发；每个工作项打开或新建时沿用当前值，不需要重启。
    fn sync_view_settings(&mut self, cx: &mut Context<Self>) {
        let settings = self.model.settings.to_settings();
        self.query.update(cx, |pane, cx| pane.apply_public_settings(&settings, cx));
        let page_size = u64::try_from(settings.page_size.max(1)).unwrap_or(1);
        let preview_kb = u64::try_from(settings.preview_kb.max(1)).unwrap_or(1);
        let null_copy = settings.null_copy.clone();
        let locale = self.model.locale();
        let locale_changed = self.table.update(cx, |pane, _| pane.set_locale(locale));
        self.table.update(cx, |pane, cx| {
            if locale_changed {
                cx.notify();
            }
            pane.set_page_limit(page_size, cx);
            // 上限变了就重读表数据并关掉查看面板，旧行列不再按新上限解释。
            pane.set_preview_kb(preview_kb, cx);
            pane.set_null_copy(&null_copy);
        });
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
        let context = self.active_query_context();
        // 密度和语言一样每帧下发：消息页、右侧面板的行高与控件高度立即跟着变。
        let density = self.model.shell.density;
        let effect = self.query.update(cx, |pane, cx| {
            pane.remember_environments(environments);
            pane.set_query_timeout_sec(timeout);
            if pane.set_density(density) {
                cx.notify();
            }
            let effect = pane.prepare(&snaps, active.as_deref(), locale, context, cx);
            pane.apply_grid_locale(locale, cx);
            pane.ensure_editors(window, cx);
            effect
        });
        self.apply_query_effect(effect);
    }

    /// 对象索引只在渲染时从查询页同步一次；资源管理器、对象筛选和快速打开都读这份快照。
    fn sync_catalog(&mut self, cx: &mut Context<Self>) {
        let pane = self.query.read(cx);
        self.model.set_catalog(pane.tree_objects(), pane.tree_error());
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
        // 工作项关闭后，专用页面缓存的目标也不能继续复用；下次打开同一目标时
        // 必须重新 bind/open_spec，清掉上一个已关闭工作项的草稿和结果。
        let has_structure_item = self.model.tabs.iter().any(|tab| matches!(tab.kind, WorkItemKind::Structure | WorkItemKind::Admin));
        if !has_structure_item && self.applied_tool.as_deref().is_some_and(|key| key.starts_with("Structure:") || key.starts_with("Admin:")) {
            self.applied_tool = None;
        }
        if !self.model.tabs.iter().any(|tab| tab.kind == WorkItemKind::Redis) {
            self.applied_redis = None;
        }
        if !self.model.tabs.iter().any(|tab| tab.kind == WorkItemKind::Mongo) {
            self.applied_mongo = None;
        }
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
        let item = self
            .model
            .active_tab()
            .map(|tab| format!("{}#{}", tab.id, tab.epoch))
            .unwrap_or_default();
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
                    let page_size = u64::try_from(self.model.settings.page_size.max(1)).unwrap_or(1);
                    self.table.update(cx, |pane, cx| {
                        pane.set_locale(locale);
                        pane.open_table(target, page_size, cx);
                    });
                }
            }
            WorkTool::Structure => self.structure.update(cx, |pane, _| {
                pane.set_locale(locale);
                pane.bind_item(target, &item);
            }),
            WorkTool::Admin => {
                // 重新绑定会清掉探测闩；允许本帧按当前连接状态再探一次。
                self.admin_link = None;
                self.admin.update(cx, |pane, _| {
                    pane.set_locale(locale);
                    pane.bind_item(target, &item);
                });
            }
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
    pub fn plant_query_result(&mut self, columns: Vec<String>, rows: Vec<Vec<String>>, cx: &mut Context<Self>) {
        self.query.update(cx, |pane, cx| pane.show_sheet(columns, rows, cx));
    }

    #[cfg(test)]
    pub fn exchange_kind(&self, cx: &App) -> ExchangeSourceKind {
        self.exchange.read(cx).source().kind
    }

    #[cfg(test)]
    pub fn exchange_columns(&self, cx: &App) -> Vec<String> {
        self.exchange.read(cx).source().columns.clone()
    }

    #[cfg(test)]
    pub fn exchange_generation(&self, cx: &App) -> u64 {
        self.exchange.read(cx).source_generation()
    }

    #[cfg(test)]
    pub fn apply_exchange_mapping(&mut self, generation: u64, mapping: std::collections::BTreeMap<String, String>, cx: &mut Context<Self>) -> bool {
        self.exchange.update(cx, |pane, _| pane.apply_mapping_for(generation, mapping))
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
    /// 目录树读取只在视图里发起：模型排队，结果回填到同一份 NavTree。
    fn pump_nav(&mut self, cx: &mut Context<Self>) {
        while let Some(request) = self.model.take_nav_request() {
            match request {
                super::nav::NavRequest::Refresh { connection } => self.spawn_nav_refresh(connection, cx),
                super::nav::NavRequest::Details(id) => self.spawn_nav_details(id, cx),
                super::nav::NavRequest::More(id) => self.spawn_nav_more(id, cx),
                super::nav::NavRequest::Group(id) => self.spawn_nav_group(id, cx),
                super::nav::NavRequest::Reload(id) => self.spawn_nav_reload(id, cx),
                super::nav::NavRequest::Definition(id) => self.spawn_definition(id, cx),
            }
        }
    }

    /// 库/模式“刷新此节点”：从第一页重读这个范围。加载态已由模型标好。
    fn spawn_nav_reload(&mut self, node_id: String, cx: &mut Context<Self>) {
        let Some(node) = self.model.nav_node(&node_id).cloned() else { return };
        let (abort, cancelled) = self.track_nav_read(&node);
        let revision = self.nav_revision.get(&node.connection_id).copied();
        let connection_id = node.connection_id.clone();
        let gate = Arc::clone(&self.nav_gate);
        let services = Arc::clone(&self.services);
        let page = usize::try_from(self.model.settings.object_page.max(1)).unwrap_or(100);
        let view = cx.entity().downgrade();
        let (tx, rx) = tokio::sync::oneshot::channel();
        jobs::runtime().spawn(futures::future::Abortable::new(async move {
            let Some(session) = services.connections.catalog_session(&node.connection_id) else {
                let _ = tx.send((Vec::new(), 0, Some("这条连接还没有打开".to_owned()), Vec::new()));
                return;
            };
            let body = crate::catalog::nav_read::limited_metadata(gate, &node.connection_id, crate::catalog::read_object_page(&session, node.engine, &node.database, &node.schema, 0, page)).await
                .unwrap_or_else(|error| crate::catalog::read::ObjectPageBody { objects: Vec::new(), total: 0, error: Some(error), kind_reports: Vec::new() });
            // 按种类的失败一并带回：失败的种类不能当成“整类都被删了”。
            let _ = tx.send((body.objects, body.total, body.error, body.kind_reports));
        }, abort));
        cx.spawn(async move |_, cx| {
            let result = rx.await;
            cx.update(|cx| {
                let Some(view) = view.upgrade() else { return };
                view.update(cx, |this, cx| {
                    if this.nav_revision.get(&connection_id).copied() != revision || cancelled.is_aborted() { return; }
                    this.nav_reads.remove(&node_id);
                    match result {
                        Ok((objects, total, error, kind_reports)) => {
                            this.model.nav.reload_scope(&node_id, &objects, total, error, &kind_reports);
                        }
                        Err(_) => {
                            this.model.nav.reload_scope(&node_id, &[], 0, Some("刷新对象失败".to_owned()), &[]);
                        }
                    }
                    this.sync_navigation_index(cx);
                    cx.notify();
                });
            });
        })
        .detach();
    }

    /// 读一个对象的定义（查看/复制 DDL）。只在已打开的会话上读，不另开连接。
    fn spawn_definition(&mut self, node_id: String, cx: &mut Context<Self>) {
        let Some(ddl) = self.model.ddl_view.clone().filter(|view| view.node_id == node_id) else { return };
        let services = Arc::clone(&self.services);
        let view = cx.entity().downgrade();
        let epoch = ddl.epoch;
        let (tx, rx) = tokio::sync::oneshot::channel();
        jobs::runtime().spawn(async move {
            let key = crate::catalog::definition::DdlKey {
                database: ddl.object.database.clone(),
                schema: ddl.object.schema.clone(),
                name: ddl.object.name.clone(),
                kind: ddl.object.kind,
                signature: ddl.signature.clone(),
                relation: ddl.relation.clone(),
            };
            let result = crate::application::ddl::object_definition(&services.connections, &ddl.object.connection_id, ddl.object.engine, &key).await;
            let _ = tx.send(result);
        });
        cx.spawn(async move |_, cx| {
            let result = rx.await;
            cx.update(|cx| {
                let Some(view) = view.upgrade() else { return };
                view.update(cx, |this, cx| {
                    let result = result.unwrap_or_else(|_| Err("读取定义失败".to_owned()));
                    this.model.definition_loaded(epoch, result);
                    this.flush_clipboard(cx);
                    cx.notify();
                });
            });
        })
        .detach();
    }

    /// 确认层排队的预览或执行。都只在已打开的会话上走 application::ddl。
    fn pump_danger(&mut self, cx: &mut Context<Self>) {
        while let Some((step, prompt)) = self.model.take_danger_request() {
            let services = Arc::clone(&self.services);
            let view = cx.entity().downgrade();
            let epoch = prompt.epoch;
            let request = crate::application::ddl::request(
                prompt.object.database.clone(),
                prompt.object.schema.clone(),
                prompt.engine().as_str(),
                prompt.action(),
            );
            let connection = prompt.object.connection_id.clone();
            match step {
                super::danger::DangerStep::Preview => {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    jobs::runtime().spawn(async move {
                        let tags = services.connections.list_tags().await.ok();
                        let plan = match request {
                            Ok(request) => crate::application::ddl::preview_request(&services.connections, &connection, &request).await,
                            Err(message) => Err(message),
                        };
                        let _ = tx.send((tags, plan));
                    });
                    cx.spawn(async move |_, cx| {
                        let result = rx.await;
                        cx.update(|cx| {
                            let Some(view) = view.upgrade() else { return };
                            view.update(cx, |this, cx| {
                                let (tags, plan) = result.unwrap_or_else(|_| (None, Err("生成计划失败".to_owned())));
                                this.model.danger_planned(epoch, tags, plan);
                                cx.notify();
                            });
                        });
                    })
                    .detach();
                }
                super::danger::DangerStep::Execute => {
                    let sql = prompt.confirmed_sql();
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    jobs::runtime().spawn(async move {
                        let receipt = match request {
                            Ok(request) => crate::application::ddl::execute_request(&services.connections, &connection, &request, &sql, true).await,
                            Err(message) => Err(message),
                        };
                        let _ = tx.send(receipt);
                    });
                    cx.spawn(async move |_, cx| {
                        let result = rx.await;
                        cx.update(|cx| {
                            let Some(view) = view.upgrade() else { return };
                            view.update(cx, |this, cx| {
                                let receipt = result.unwrap_or_else(|_| Err("执行失败".to_owned()));
                                this.model.danger_finished(epoch, receipt);
                                cx.notify();
                            });
                        });
                    })
                    .detach();
                }
            }
        }
    }

    /// 把模型交出的文本写进系统剪贴板。
    pub(super) fn flush_clipboard(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = self.model.take_clipboard() {
            cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(text));
        }
    }

    fn track_nav_read(&mut self, node: &crate::catalog::hierarchy::NavNode) -> (futures::future::AbortRegistration, futures::future::AbortHandle) {
        let (handle, registration) = futures::future::AbortHandle::new_pair();
        let cancelled = handle.clone();
        if let Some((_, old)) = self.nav_reads.insert(node.id.clone(), (node.connection_id.clone(), handle)) { old.abort(); }
        (registration, cancelled)
    }

    fn finish_nav_refresh(&mut self, scope: &Option<String>) {
        self.nav_refresh_active.remove(scope);
        self.nav_refresh_tasks.remove(scope);
        if self.nav_refresh_active.contains(&None) { return; }
        if self.nav_refresh_again.contains(&None) {
            self.nav_refresh_again.clear();
            self.model.request_nav_refresh(None);
        } else {
            let queued: Vec<_> = self.nav_refresh_again.iter().filter(|scope| !self.nav_refresh_active.contains(*scope)).cloned().collect();
            for next in queued {
                self.nav_refresh_again.remove(&next);
                self.model.request_nav_refresh(next);
            }
        }
    }

    fn spawn_nav_refresh(&mut self, connection: Option<String>, cx: &mut Context<Self>) {
        if self.nav_refresh_active.contains(&None) || !self.nav_refresh_active.insert(connection.clone()) {
            self.nav_refresh_again.insert(connection);
            self.model.nav_loading = self.model.nav_loading.saturating_sub(1);
            return;
        }
        self.nav_reads.retain(|_, (id, handle)| {
            if connection.as_ref().is_none_or(|scope| scope == id) { handle.abort(); false } else { true }
        });
        let ids: std::collections::BTreeSet<String> = match &connection {
            Some(id) => std::iter::once(id.clone()).collect(),
            None => self.model.connections.rows.iter().map(|row| row.saved.id.clone())
                .chain(self.model.nav_source.iter().map(|connection| connection.id.clone()))
                .chain(self.nav_revision.keys().cloned()).collect(),
        };
        for id in ids { *self.nav_revision.entry(id).or_default() += 1; }
        self.query.update(cx, |pane, _| pane.invalidate_metadata());
        let revisions = self.nav_revision.clone();
        let services = Arc::clone(&self.services);
        let page = usize::try_from(self.model.settings.object_page.max(1)).unwrap_or(100);
        let view = cx.entity().downgrade();
        let partial = connection.is_some();
        let scope = connection.clone();
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let (handle, abort) = futures::future::AbortHandle::new_pair();
        self.nav_refresh_tasks.insert(scope.clone(), handle);
        jobs::runtime().spawn(futures::future::Abortable::new(async move {
            crate::catalog::nav_read::stream_navigation(&services.connections, page, connection.as_deref(), Arc::clone(&services.metadata_gate), tx).await;
        }, abort));
        cx.spawn(async move |_, cx| {
            let mut finished = false;
            while let Some(event) = rx.recv().await {
                let mut alive = false;
                cx.update(|cx| {
                    let Some(view) = view.upgrade() else { return };
                    alive = true;
                    view.update(cx, |this, cx| {
                        use crate::catalog::nav_read::NavigationEvent;
                        let pending = this.model.nav_loading;
                        match event {
                            NavigationEvent::Started(mut roots) => {
                                roots.retain(|root| this.nav_revision.get(&root.id) == revisions.get(&root.id));
                                for root in &mut roots {
                                    if let Some(old) = this.model.nav_source.iter().find(|old| old.id == root.id) {
                                        root.databases = old.databases.clone();
                                    }
                                }
                                this.model.apply_navigation(&roots, partial);
                                this.model.nav_loading = pending;
                            }
                            NavigationEvent::Connection(mut connection) => {
                                if this.nav_revision.get(&connection.id) != revisions.get(&connection.id) { return; }
                                if connection.error.is_some() && connection.databases.is_empty() {
                                    if let Some(old) = this.model.nav_source.iter().find(|old| old.id == connection.id) {
                                        connection.databases = old.databases.clone();
                                    }
                                }
                                this.model.apply_navigation(&[connection], true);
                                this.model.nav_loading = pending;
                                let expanded: Vec<_> = this.model.nav_rows().into_iter()
                                    .filter(|row| row.expanded).map(|row| row.id).collect();
                                for id in expanded { this.model.request_lazy_group(&id); }
                                this.sync_navigation_index(cx);
                            }
                            NavigationEvent::Finished(_ids) => {
                                this.model.nav_loading = pending.saturating_sub(1);
                                this.finish_nav_refresh(&scope);
                                finished = true;
                                this.sync_navigation_index(cx);
                            }
                            NavigationEvent::Failed(message) => {
                                this.model.nav_failed(message);
                                this.finish_nav_refresh(&scope);
                                finished = true;
                            }
                        }
                        cx.notify();
                    });
                });
                if !alive { break; }
            }
            if !finished {
                cx.update(|cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |this, cx| {
                            this.finish_nav_refresh(&scope);
                            this.model.nav_failed("读取对象目录失败");
                            cx.notify();
                        });
                    }
                });
            }
        }).detach();
    }

    fn sync_navigation_index(&mut self, cx: &mut Context<Self>) {
        let objects = self.model.nav.nodes().filter(|node| node.kind == crate::catalog::hierarchy::NavKind::Object && !node.stale)
            .map(|node| crate::query::assist::TreeObject {
                connection_id: node.connection_id.clone(), connection_name: node.connection_name.clone(),
                engine: node.engine.as_str().to_owned(), database: node.database.clone(), schema: node.schema.clone(),
                name: node.name.clone(), kind: crate::catalog::model::kind_name(node.object_kind.unwrap_or(crate::catalog::model::ObjectKind::Table)).to_owned(),
            }).collect();
        self.query.update(cx, |pane, cx| pane.adopt_navigation(&self.model.nav_source, objects, cx));
    }

    fn spawn_nav_details(&mut self, node_id: String, cx: &mut Context<Self>) {
        let Some(node) = self.model.nav_node(&node_id).cloned() else { return };
        let (abort, cancelled) = self.track_nav_read(&node);
        let revision = self.nav_revision.get(&node.connection_id).copied();
        let connection_id = node.connection_id.clone();
        let gate = Arc::clone(&self.nav_gate);
        let services = Arc::clone(&self.services);
        let view = cx.entity().downgrade();
        let (tx, rx) = tokio::sync::oneshot::channel();
        jobs::runtime().spawn(futures::future::Abortable::new(async move {
            let Some(session) = services.connections.catalog_session(&node.connection_id) else {
                let _ = tx.send((Err("这条连接还没有打开".to_owned()), Err("这条连接还没有打开".to_owned())));
                return;
            };
            let result = crate::catalog::nav_read::limited_metadata(gate, &node.connection_id, crate::catalog::read_object_details(&session, node.engine, &node.database, &node.schema, &node.name)).await
                .unwrap_or_else(|error| (Err(error.clone()), Err(error)));
            let _ = tx.send(result);
        }, abort));
        cx.spawn(async move |_, cx| {
            let result = rx.await;
            cx.update(|cx| {
                let Some(view) = view.upgrade() else { return };
                view.update(cx, |this, cx| {
                    if this.nav_revision.get(&connection_id).copied() != revision || cancelled.is_aborted() { return; }
                    this.nav_reads.remove(&node_id);
                    let (columns, indexes) = match result {
                        Ok(pair) => pair,
                        Err(_) => (Err("读取列表失败".to_owned()), Err("读取列表失败".to_owned())),
                    };
                    if this.model.template_waits_for(&node_id) {
                        // 模板要并入草稿：先把编辑器里的最新文本收回来，追加时才不会丢。
                        this.capture_query(cx);
                    }
                    this.model.nav_details_loaded(&node_id, columns, indexes);
                    cx.notify();
                });
            });
        })
        .detach();
    }

    fn spawn_nav_more(&mut self, node_id: String, cx: &mut Context<Self>) {
        let Some(node) = self.model.nav_node(&node_id).cloned() else { return };
        if node.paged && node.kind == crate::catalog::hierarchy::NavKind::Group {
            self.spawn_nav_kind(node_id, if node.unloaded { 0 } else { node.loaded }, cx);
            return;
        }
        // 排队的 More 请求已经由模型标好加载态（并且在那里去重）；这里再按 loading
        // 提前返回会让“读取更多”永远不发请求、加载态也永远不落。
        self.model.nav.begin_scope(&node_id);
        let (abort, cancelled) = self.track_nav_read(&node);
        let revision = self.nav_revision.get(&node.connection_id).copied();
        let connection_id = node.connection_id.clone();
        let gate = Arc::clone(&self.nav_gate);
        let services = Arc::clone(&self.services);
        let page = usize::try_from(self.model.settings.object_page.max(1)).unwrap_or(100);
        let offset = node.loaded;
        let view = cx.entity().downgrade();
        let (tx, rx) = tokio::sync::oneshot::channel();
        jobs::runtime().spawn(futures::future::Abortable::new(async move {
            let Some(session) = services.connections.catalog_session(&node.connection_id) else {
                let _ = tx.send((Vec::new(), 0, Some("这条连接还没有打开".to_owned())));
                return;
            };
            let body = crate::catalog::nav_read::limited_metadata(gate, &node.connection_id, crate::catalog::read_object_page(&session, node.engine, &node.database, &node.schema, offset, page)).await
                .unwrap_or_else(|error| crate::catalog::read::ObjectPageBody { objects: Vec::new(), total: 0, error: Some(error), kind_reports: Vec::new() });
            let _ = tx.send((body.objects, body.total, body.error));
        }, abort));
        cx.spawn(async move |_, cx| {
            let result = rx.await;
            cx.update(|cx| {
                let Some(view) = view.upgrade() else { return };
                view.update(cx, |this, cx| {
                    if this.nav_revision.get(&connection_id).copied() != revision || cancelled.is_aborted() { return; }
                    this.nav_reads.remove(&node_id);
                    match result {
                        Ok((objects, total, error)) => {
                            this.model.nav.append_objects(&node_id, &objects, total, error);
                        }
                        Err(_) => {
                            this.model.nav.set_error(&node_id, "读取更多对象失败");
                        }
                    }
                    this.sync_navigation_index(cx);
                    cx.notify();
                });
            });
        })
        .detach();
    }

    /// 只重读一个对象种类分组：失败的那一类单独重试，兄弟分组不动。
    /// 排队的 Group 请求已经由模型标好加载态，这里不能再按 loading 提前返回，
    /// 否则排队的重试永远不发读取请求、加载态也永远不落。
    fn spawn_nav_group(&mut self, group_id: String, cx: &mut Context<Self>) {
        self.spawn_nav_kind(group_id, 0, cx);
    }

    fn spawn_nav_kind(&mut self, group_id: String, offset: usize, cx: &mut Context<Self>) {
        let Some(node) = self.model.nav_node(&group_id).cloned() else { return };
        let Some(kind) = node.object_kind else { return };
        self.model.nav.begin_group(&group_id);
        let (abort, cancelled) = self.track_nav_read(&node);
        let revision = self.nav_revision.get(&node.connection_id).copied();
        let connection_id = node.connection_id.clone();
        let services = Arc::clone(&self.services);
        let gate = Arc::clone(&self.nav_gate);
        let page = usize::try_from(self.model.settings.object_page.max(1)).unwrap_or(100);
        let view = cx.entity().downgrade();
        let (tx, rx) = tokio::sync::oneshot::channel();
        jobs::runtime().spawn(futures::future::Abortable::new(async move {
            let result = crate::catalog::nav_read::limited_metadata(gate, &node.connection_id, async {
                match services.connections.catalog_session(&node.connection_id) {
                    Some(session) => crate::catalog::read_object_kind_page(&session, node.engine, &node.database, &node.schema, kind, offset, page).await,
                    None => crate::catalog::read::ObjectPageBody { objects: Vec::new(), total: 0, error: Some("这条连接还没有打开".to_owned()), kind_reports: Vec::new() },
                }
            }).await;
            let _ = tx.send(result);
        }, abort));
        cx.spawn(async move |_, cx| {
            let result = rx.await;
            cx.update(|cx| {
                let Some(view) = view.upgrade() else { return };
                view.update(cx, |this, cx| {
                    if this.nav_revision.get(&connection_id).copied() != revision || cancelled.is_aborted() { return; }
                    this.nav_reads.remove(&group_id);
                    match result {
                        Ok(Ok(body)) => {
                            if node.paged { this.model.nav.set_kind_slice(&group_id, &body.objects, body.total, offset, body.error); }
                            else { this.model.nav.set_kind_page(&group_id, &body.objects, body.total, body.error); }
                        }
                        Ok(Err(error)) => { this.model.nav.set_error(&group_id, error); }
                        Err(_) => { this.model.nav.set_error(&group_id, "读取这一类对象失败"); }
                    }
                    this.sync_navigation_index(cx);
                    cx.notify();
                });
            });
        }).detach();
    }

    /// 连接成功的续接动作执行一次；连接失败的留在重试入口里，不丢用户想打开的对象。
    fn run_connect_intents(&mut self, cx: &mut Context<Self>) {
        for intent in self.model.take_ready_intents() {
            if let Some(spec) = intent.spec.clone() {
                // 按排队时的对象重放，不重新读当前焦点。
                self.model.object_spec = Some(spec);
            }
            self.model.connections.selected = Some(intent.connection_id.clone());
            match intent.action {
                super::conn_modal::ConnectAction::Expand => {
                    self.model.expand_nav_connection(&intent.connection_id);
                    if self
                        .model
                        .connections
                        .rows
                        .iter()
                        .any(|row| row.saved.id == intent.connection_id && matches!(row.saved.engine, Engine::Redis | Engine::Mongodb))
                    {
                        self.pending_browser = Some(intent.connection_id.clone());
                    }
                    cx.notify();
                }
                action => {
                    let command = super::conn_modal::ConnectIntent { action, ..intent }.command();
                    if let Some(command) = command {
                        self.perform(command, cx);
                    }
                }
            }
        }
        self.model.fail_pending_intents();
    }

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
                // 展开状态存在树上，连接成功后资源管理器就能看到下一层。
                self.model.expand_nav_connection(id);
            }
            let browse = self.pending_browser.take_if(|id| connected.contains(id));
            self.connected_seen = connected;
            self.model.request_nav_refresh(None);
            self.pump_nav(cx);
            if let Some(id) = browse {
                if let Some((command, _, _)) = self.model.connections.rows.iter().find(|row| row.saved.id == id).and_then(super::chrome::browser_command) {
                    self.perform(&command, cx);
                }
            }
        }
    }

    fn monitor_connections(&mut self, cx: &mut Context<Self>) {
        if !self.boot || self.health_task.is_some() { return; }
        let services = Arc::clone(&self.services);
        let owner = cx.entity().downgrade();
        self.health_task = Some(cx.spawn(async move |_, cx| {
            let mut first = true;
            loop {
                let service = Arc::clone(&services.connections);
                let (tx, rx) = tokio::sync::oneshot::channel();
                jobs::runtime().spawn(async move {
                    if !first { tokio::time::sleep(std::time::Duration::from_secs(5)).await; }
                    if tx.is_closed() { return; }
                    let _ = tx.send(service.session_statuses().await);
                });
                first = false;
                let Ok(statuses) = rx.await else { break };
                let mut alive = false;
                cx.update(|cx| {
                    let Some(owner) = owner.upgrade() else { return };
                    alive = true;
                    owner.update(cx, |view, cx| {
                        use crate::app::views::connections::LinkStatus;
                        let mut changed = false;
                        for status in statuses {
                            let Some(current) = view.services.connections.session_condition(&status.id) else { continue };
                            let next = match current {
                                crate::connections::SessionCondition::Connected => LinkStatus::Connected,
                                crate::connections::SessionCondition::Lost { .. } if status.status == "reconnecting" => LinkStatus::Connecting,
                                crate::connections::SessionCondition::Lost { .. } => LinkStatus::Failed,
                            };
                            if next == LinkStatus::Failed && view.model.running_jobs > 0 && view.model.connections.status.get(&status.id) == Some(&LinkStatus::Connecting) { continue; }
                            if view.model.connections.status.get(&status.id) != Some(&next) {
                                view.model.connections.status.insert(status.id.clone(), next);
                                changed = true;
                                if next == LinkStatus::Failed && view.model.connections.selected.as_deref() == Some(status.id.as_str()) {
                                    view.model.notice_key = Some("connections.status.error");
                                    view.model.notice_detail = status.reason.unwrap_or_default();
                                }
                            }
                        }
                        if changed { cx.notify(); }
                    });
                });
                if !alive { break; }
            }
        }));
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
        let height = f32::from(window.viewport_size().height) as i32;
        self.model.viewport_height = height.max(0);
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
                            // 保存的主题（含深色）要在启动时就写进组件库主题，否则重启后仍是浅色色板。
                            this.apply_effect(Effect::ApplyTheme, cx);
                            // 启动时的自动更新检查只走这个开关；关闭时不发起检查。
                            this.model.begin_update_check();
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
                            let _ = this.model.dispatch("nav-refresh");
                            this.load_env_tags(cx);
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
        let owner: &'static str = match &job {
            PendingJob::RefreshHistory
            | PendingJob::SaveNamed
            | PendingJob::RenameSaved
            | PendingJob::ClearHistory
            | PendingJob::DeleteRun
            | PendingJob::DeleteSaved => "history",
            _ => "connections",
        };
        let token = if owner == "history" {
            self.model.begin_owner(&self.services.tasks, owner)
        } else {
            self.model.prepare_list_token(&self.services.tasks)
        };
        let request = job_request(&self.model, job.clone(), token);
        if matches!(job, PendingJob::SaveNamed) {
            self.model.preview_save = None;
        }
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
        let back = self.compare.update(cx, |pane, _| pane.take_back());
        self.model.compare_running = self.compare.read(cx).running();
        if let Some(place) = place {
            self.model.open_bound_query(place.sql, place.connection_id, place.connection_name, place.database, place.engine);
        }
        if back {
            // 返回原工作项：对比自己的两侧与结果留在对比页里，不重建成新工作项。
            self.model.return_to_work();
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
        self.refresh_exchange_data(cx, self.model.exchange_open);
    }

    fn refresh_exchange_data(&mut self, cx: &mut Context<Self>, include_rows: bool) {
        self.sync_exchange_selection();
        let stamp = match self.exchange_kind {
            ExchangeSourceKind::Query => ExchangeStamp::Query(self.exchange_item.clone(), include_rows, self.query.read(cx).exchange_stamp()),
            ExchangeSourceKind::Table => ExchangeStamp::Table(self.exchange_item.clone(), include_rows, self.table.read(cx).exchange_stamp()),
        };
        if self.exchange_stamp.as_ref() != Some(&stamp) {
            let source = self.exchange_source(cx, include_rows);
            self.exchange.update(cx, |pane, _| pane.bind_source(source));
            self.exchange_stamp = Some(stamp);
        }
        if self.model.exchange_open { self.exchange.update(cx, |pane, cx| pane.ensure_mapping(cx)); }
        self.model.exchange_running = self.exchange.read(cx).running();
    }

    /// 工作项变化时，交换来源回到该项的默认来源（查询项→查询结果，其余→表数据）。
    /// 同一工作项内的显式选择保持不变。
    fn sync_exchange_selection(&mut self) {
        let active = self.model.active_tab.clone();
        if active == self.exchange_item {
            return;
        }
        let kind = self.model.active_tab().map(|tab| tab.kind);
        self.exchange_item = active;
        self.exchange_kind = match kind {
            Some(WorkItemKind::Query) => ExchangeSourceKind::Query,
            Some(WorkItemKind::Table) => ExchangeSourceKind::Table,
            Some(_) if self.model.table_spec.is_some() => ExchangeSourceKind::Table,
            Some(_) => self.exchange_kind,
            None => ExchangeSourceKind::Table,
        };
    }

    fn active_query_context(&self) -> bool {
        self.model.shell.page == AppPage::Workspace && self.model.active_tab().is_some_and(|tab| tab.kind == WorkItemKind::Query)
    }

    /// 来源只取所选工作项的数据：查询来自 QueryPane 的当前语句结果，表来自共享网格。
    /// 不再用 shell 的 table_spec 兜底，查询页不会拿到上一张表的目标。
    fn exchange_source(&self, cx: &App, include_rows: bool) -> ExchangeSource {
        match self.exchange_kind {
            ExchangeSourceKind::Query => {
                let pane = self.query.read(cx);
                let (columns, rows) = if include_rows { let (columns, rows, _) = pane.exchange_result(); (columns, rows) } else { (pane.exchange_columns(), Vec::new()) };
                ExchangeSource {
                    kind: ExchangeSourceKind::Query,
                    item: self.exchange_item.clone(),
                    table: None,
                    columns,
                    rows,
                    truncated: pane.exchange_truncated(),
                    running: pane.busy(),
                    label: "查询结果".to_owned(),
                    context: pane.exchange_context(),
                    // 查询结果是完整值，没有经过预览截断：协议形状的普通文本照原值导出。
                    preview_bytes: None,
                }
            }
            ExchangeSourceKind::Table => {
                let (table, columns, truncated) = self.table.read(cx).exchange_metadata();
                let running = self.table.read(cx).fetching();
                let preview_bytes = self.table.read(cx).sheet_preview_bytes();
                // 只有当前工作项的目标与网格实际表一致时才能把网格当来源；
                // 结构页、刚切换的表和隐藏的旧表都不能混用列与行。
                let consistent = match self.model.active_tab() {
                    // 没有工作项的旧表格界面（测试/手动工具）仍以当前网格为准。
                    None => true,
                    Some(item) if item.kind == WorkItemKind::Query => false,
                    Some(item) => match (item.spec.as_deref(), table.as_ref()) {
                        (Some(spec), Some(table)) => spec_matches_table(spec, table),
                        _ => false,
                    },
                };
                if !consistent {
                    return ExchangeSource {
                        kind: ExchangeSourceKind::Table,
                        item: self.exchange_item.clone(),
                        table: None,
                        columns: Vec::new(),
                        rows: Vec::new(),
                        truncated: false,
                        running,
                        label: String::new(),
                        context: String::new(),
                        preview_bytes: None,
                    };
                }
                let rows = if include_rows { self.table.read(cx).exchange_sheet().2 } else { Vec::new() };
                let label = table.as_ref().map(|table| table.table.clone()).unwrap_or_default();
                let context = table
                    .as_ref()
                    .map(|table| {
                        let name = if table.connection_id.is_empty() { table.engine.clone() } else { table.connection_id.clone() };
                        if table.database.is_empty() {
                            name
                        } else {
                            format!("{name} / {}", table.database)
                        }
                    })
                    .unwrap_or_default();
                ExchangeSource {
                    kind: ExchangeSourceKind::Table,
                    item: self.exchange_item.clone(),
                    table,
                    columns,
                    rows,
                    truncated,
                    running,
                    label,
                    context,
                    preview_bytes: Some(preview_bytes),
                }
            }
        }
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

    fn load_env_tags(&mut self, cx: &mut Context<Self>) {
        let services = Arc::clone(&self.services);
        let view = cx.entity().downgrade();
        let (tx, rx) = tokio::sync::oneshot::channel();
        jobs::runtime().spawn(async move {
            let tags = services.connections.list_tags().await.unwrap_or_default();
            let _ = tx.send(tags);
        });
        cx.spawn(async move |_, cx| {
            if let Ok(tags) = rx.await {
                cx.update(|cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |this, cx| {
                            this.model.env_tags = tags;
                            cx.notify();
                        });
                    }
                });
            }
        })
        .detach();
    }

    fn consume_pending_create(&mut self, cx: &mut Context<Self>) {
        let Some(spec) = self.model.pending_create.take() else { return };
        let Some(target) = parse_target(&spec) else { return };
        self.structure.update(cx, |pane, _| pane.begin_create(target));
    }

    fn adopt_created_table(&mut self, target: GridTarget) {
        let spec = ObjectTarget::new(
            target.connection_id.clone(),
            target.connection_name.clone(),
            target.database.clone(),
            target.schema.clone(),
            target.table.clone(),
            target.engine.clone(),
            target.environment.clone(),
        )
        .spec();
        self.model.adopt_created_table(spec);
        self.applied_tool = None;
    }

    /// 关系图页头的“返回”：与对比页走同一条路径——回到原工作项，
    /// 图自己的范围、筛选与滚动都留在图上，不重建成新工作项。
    fn consume_diagram_back(&mut self, cx: &mut Context<Self>) {
        if self.diagram.update(cx, |pane, _| pane.take_back()) {
            self.model.return_to_work();
        }
    }

    fn consume_diagram_open(&mut self, cx: &mut Context<Self>) {
        let Some(open) = self.diagram.update(cx, |pane, _| pane.take_open()) else { return };
        self.open_diagram_target(open);
    }

    fn open_diagram_target(&mut self, open: DiagramOpen) {
        let row = self.model.connections.rows.iter().find(|row| row.saved.id == open.connection_id);
        let name = row.map(|row| row.saved.name.clone()).unwrap_or_else(|| open.connection_id.clone());
        let engine = row.map(|row| row.saved.engine.as_str().to_owned()).unwrap_or_else(|| "postgres".into());
        let env = row.map(|row| row.saved.env.clone()).unwrap_or_default();
        let spec = ObjectTarget::new(open.connection_id, name, open.database, open.schema, open.table, engine, env).spec();
        if open.structure {
            self.model.open_item(WorkItemKind::Structure, spec);
        } else {
            self.model.open_item(WorkItemKind::Table, spec);
        }
    }

    fn note_admin_link(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.model.active_tab().and_then(|tab| tab.connection_id.clone()).filter(|id| !id.is_empty()) else {
            return;
        };
        if self.model.tool != WorkTool::Admin && self.model.shell.page != AppPage::Structure {
            return;
        }
        let connected = self.model.connections.status.get(&id).copied() == Some(crate::app::views::connections::LinkStatus::Connected);
        if self.admin_link.as_ref().is_some_and(|(seen, state)| seen == &id && *state == connected) {
            return;
        }
        self.admin_link = Some((id, connected));
        self.admin.update(cx, |pane, cx| pane.note_connection(connected, cx));
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
        self.monitor_connections(cx);
        self.pump_jobs(cx);
        self.pump_nav(cx);
        self.pump_danger(cx);
        self.flush_clipboard(cx);
        // 结构页删除表成功后，对象树也要跟着变。
        if let Some(target) = self.structure.update(cx, |pane, _| pane.take_dropped()) {
            self.model.table_dropped(&target.connection_id, &target.database, &target.schema, &target.table);
            self.pump_nav(cx);
        }
        if let Some(target) = self.structure.update(cx, |pane, _| pane.take_created()) {
            self.adopt_created_table(target);
            self.pump_nav(cx);
        }
        self.sync_view_settings(cx);
        self.sync_density(cx);
        self.sync_query(window, cx);
        self.sync_catalog(cx);
        let locale = self.model.locale();
        if self.applied_locale != Some(locale) {
            self.applied_locale = Some(locale);
            self.structure.update(cx, |pane, cx| {
                pane.set_locale(locale);
                cx.notify();
            });
            self.admin.update(cx, |pane, cx| {
                pane.set_locale(locale);
                cx.notify();
            });
            self.compare.update(cx, |pane, cx| {
                pane.set_locale(locale);
                cx.notify();
            });
            self.exchange.update(cx, |pane, cx| {
                pane.set_locale(locale);
                cx.notify();
            });
            self.diagram.update(cx, |pane, cx| {
                pane.set_locale(locale);
                cx.notify();
            });
        }
        let owner = cx.entity().downgrade();
        self.exchange.update(cx, |pane, _| pane.attach_owner(owner));
        self.ensure_tool(cx);
        self.ensure_redis(cx);
        self.ensure_mongo(cx);
        self.model.redis_running = self.redis.read(cx).running();
        self.model.mongo_running = self.mongo.read(cx).running();
        self.ensure_diagram(cx);
        self.consume_pending_create(cx);
        self.consume_diagram_open(cx);
        self.consume_diagram_back(cx);
        self.sync_grid_flags(cx);
        self.refresh_exchange(cx);
        self.refresh_compare(cx);
        self.refresh_tree_on_connect(cx);
        self.run_connect_intents(cx);
        self.note_admin_link(cx);
        self.render_root(window, cx)
    }
}

/// 网格里的实际表是否就是这条规格指向的对象。MySQL 的对象没有 schema，
/// describe 会回填库名，所以空的 schema 段按通配处理；解析只有一份，避免各入口各写一套。
pub(crate) fn spec_matches_table(spec: &str, table: &crate::grid::commands::TableRef) -> bool {
    crate::app::model::ObjectTarget::parse(spec).is_some_and(|target| {
        target.matches_parts(
            &table.connection_id,
            &table.database,
            &table.schema,
            &table.table,
        )
    })
}

fn parse_target(spec: &str) -> Option<GridTarget> {
    let target = crate::app::model::ObjectTarget::parse(spec)?;
    Some(GridTarget {
        connection_id: target.connection_id,
        connection_name: target.connection_name,
        database: target.database,
        schema: target.schema,
        table: target.name,
        engine: target.engine,
        environment: target.environment,
    })
}

fn grid_surface_command(id: &str) -> bool {
    matches!(
        id,
        "edit-cell"
            | "cancel-edit"
            | "grid-set-null"
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
            | "toggle-side"
            | "pin-result"
            | "refresh-tree"
            | "confirm-variables"
            | "cancel-variables"
            | "open-sql-file"
            | "save-sql-file"
    ) || id.starts_with("object:")
        || id.starts_with("database:")
        || id.starts_with("bind-connection:")
        || id.starts_with("statement:")
        || id.starts_with("query-view:")
        || id.starts_with("unpin:")
        || id.starts_with("log:")
        || id.starts_with("side-mode:")
}

pub(super) fn nav_key(key: &str) -> Option<NavKey> {
    match key {
        "escape" => Some(NavKey::Escape),
        "up" => Some(NavKey::ArrowUp),
        "down" => Some(NavKey::ArrowDown),
        "left" => Some(NavKey::ArrowLeft),
        "right" => Some(NavKey::ArrowRight),
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
