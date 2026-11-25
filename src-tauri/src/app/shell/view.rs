//! GPUI 外壳。菜单、工具栏、活动栏、侧栏、标签、状态栏、命令面板和六类菜单走同一个 dispatch。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use gpui_kit::{
    App, AppContext as _, Context, Entity, FocusHandle, Focusable, IntoElement, ParentElement as _, PathPromptOptions, Render, SharedString, Styled as _, Window,
    WindowAppearance, div,
};

use crate::app::views::admin::AdminPane;
use crate::app::views::compare::{CompareContext, ComparePane};
use crate::app::views::connections::blank_draft;
use crate::app::views::diagram::DiagramPane;
use crate::app::views::export::{ExchangePane, ExchangeSource, ExchangeSourceKind};
use crate::app::views::grid::{GridPane, GridTarget};
use crate::app::views::mongo::MongoPane;
use crate::app::views::object_editor::{EditorRequest, ObjectEditorPane};
use crate::app::views::query::{requires_active_query, QueryPane, SyncEffect, TabSnap};
use crate::app::views::redis::RedisPane;
use crate::app::views::settings::SaveState;
use crate::app::views::structure::StructurePane;
use crate::app::model::ObjectTarget;
use crate::app::views::diagram::DiagramOpen;
use crate::app::{AppPage, Effect};
use crate::connections::model::Engine;
use crate::state::Platform;
use crate::application::AppServices;

use super::jobs::{self, apply_done, job_request};
use super::menus::NavKey;
use super::model::{DraftField, PendingJob, WorkItemKind, WorkTool, WorkbenchModel};
use super::transaction::{GuardCause, GuardFact, GuardFollowup, GuardOutcome, TransactionRequest};
use crate::app::Shell;
use crate::transaction::{TransactionEffect, TransactionState};

#[derive(PartialEq, Eq)]
enum ExchangeStamp {
    Query(Option<String>, bool, (Option<String>, u64, usize, bool, String)),
    Table(Option<String>, bool, (GridTarget, (u64, usize, u64, bool), bool)),
}

pub struct Workbench {
    pub model: WorkbenchModel,
    pub(super) query: Entity<QueryPane>,
    pub(super) table: Entity<GridPane>,
    pub(super) structure: Entity<StructurePane>,
    pub(super) admin: Entity<AdminPane>,
    /// 事件 / 函数 / 存储过程 / 触发器的统一编辑器（覆盖在所有页面之上，按需打开）。
    pub(super) object_editor: Entity<ObjectEditorPane>,
    pub(super) exchange: Entity<ExchangePane>,
    pub(super) compare: Entity<ComparePane>,
    pub(super) diagram: Entity<DiagramPane>,
    pub(super) redis: Entity<RedisPane>,
    pub(super) mongo: Entity<MongoPane>,
    applied_tool: Option<String>,
    applied_locale: Option<crate::i18n::Locale>,
    applied_catalog_revision: Option<u64>,
    /// 已按哪一版标签表下发过生产判断；标签改名改色或连接移出标签后重新下发。
    applied_tags_revision: u64,
    /// 共享表格网格当前展示的表规格；暂存必须归属到这张表对应的工作项。
    table_view_spec: Option<String>,
    /// 交换面板显式选择的来源；切换工作项后按新工作项重建。
    exchange_kind: ExchangeSourceKind,
    exchange_item: Option<String>,
    exchange_stamp: Option<ExchangeStamp>,
    applied_diagram: Option<String>,
    applied_redis: Option<String>,
    applied_mongo: Option<String>,
    pub(super) services: Arc<AppServices>,
    pub(super) focus: FocusHandle,
    /// 对象树自己的焦点；点击树行后方向键归树，离开时归还编辑器。
    pub(super) nav_focus: FocusHandle,
    /// 库概览列表：单击行后获得焦点，方向键移动选中，回车执行主操作（与双击相同）。
    pub(super) overview_focus: FocusHandle,
    /// 就地重命名的输入框已拿到焦点（只在打开时聚焦一次）。
    pub(super) tab_rename_focused: bool,
    /// 通过快捷键请求聚焦对象树搜索框；渲染对象树时消费一次。
    pub(super) focus_explorer_filter: bool,
    /// 重命名输入框失焦即确认（点到编辑器或别的区域时不让输入框一直挂在标签栏上）。
    pub(super) tab_rename_blur: Option<gpui_kit::Subscription>,
    /// 启动台"新建连接"按钮：模态关闭后焦点回到触发它的这个控件。
    pub(super) conn_launcher_focus: FocusHandle,
    /// 对象树空态"新建连接"按钮：同上。
    pub(super) conn_sidebar_focus: FocusHandle,
    /// 连接模态面板自己的焦点：GPUI focus trap 的容器句柄。
    pub(super) modal_focus: FocusHandle,
    /// 模态里每个可交互控件的焦点句柄，按渲染 id 存取。
    pub(super) modal_controls: std::collections::HashMap<String, FocusHandle>,
    /// 这一帧登记过的控件动作：鼠标点击与回车激活读同一份。
    pub(super) modal_actions: std::collections::HashMap<String, super::conn_modal::ModalAction>,
    close_hooked: bool,
    booted: bool,
    health_task: Option<gpui_kit::Task<()>>,
    /// 最近一次设置写盘的回执监听：持有句柄让它随视图一起在界面线程上释放，
    /// 否则脱离的本地任务会在测试拆卸时被别的线程丢弃并 panic。
    settings_task: Option<gpui_kit::Task<()>>,
    /// 接收后台作业的本地任务随工作台释放；下次派发时回收已完成句柄。
    job_tasks: Vec<gpui_kit::Task<()>>,
    /// 应用内更新的后台任务（检查、下载、安装）与启动后的延迟检查：持有句柄，随视图在界面线程上释放。
    update_task: Option<gpui_kit::Task<()>>,
    update_timer: Option<gpui_kit::Task<()>>,
    /// 正在执行的一条事务命令；同一时刻只排一条，执行期间控件显示忙碌。
    transaction_task: Option<gpui_kit::Task<()>>,
    /// 在途事务命令针对的连接：切到别的连接时不把忙碌状态挂到新连接的控件上。
    transaction_task_connection: Option<String>,
    /// 手动提交默认方式下已为哪个连接的哪一代事务尝试过自动开启，避免失败后反复重试。
    auto_begin_attempt: Option<(String, u64)>,
    /// 轮询兜底：每条连接上一次看到的共享事务快照修订；变化时通知各业务页重绘同一状态。
    transaction_revisions: std::collections::BTreeMap<String, u64>,
    /// 状态栏提示的时效：同一句提示显示一段时间后自动退场，不让“已连接”“清空历史？”一直挂着。
    pub(super) notice_seen: (Option<&'static str>, String),
    pub(super) notice_since: std::time::Instant,
    pub(super) notice_timer: Option<gpui_kit::Task<()>>,
    /// 分组名输入面板出现后是否已经把焦点交给它（只交一次，不抢用户后来的点击）。
    pub(super) group_prompt_focused: bool,
    pub(super) tag_label_focused: bool,
    boot: bool,
    sized: bool,
    last_state: String,
    revision: Arc<AtomicU64>,
    persist_gate: Arc<tokio::sync::Mutex<()>>,
    /// R6：手动排序的落库队列。界面线程按操作顺序入队，后台持闸一次取完、合并后依次写入，
    /// 快速连续移动不会乱序提交，同一容器只写最新的顺序。
    reorder_queue: Arc<std::sync::Mutex<std::collections::VecDeque<super::model::ReorderOp>>>,
    reorder_gate: Arc<tokio::sync::Mutex<()>>,
    /// R6：拖动排序时指针停在哪一行的下半部（"conn:id" / "group:id"）：插入线画在下沿，放手插到它后面。
    pub(super) drop_after: Option<String>,
    pub(super) fields: crate::ui::kit::Fields<DraftField>,
    pub(super) menu_at: gpui_kit::Point<gpui_kit::Pixels>,
    /// 启动台筛选浮层的锚点：来自按钮上这一次真实点击。
    pub(super) conn_filter_at: gpui_kit::Point<gpui_kit::Pixels>,
    pub(super) palette_focused: bool,
    /// 快捷键面板打开后把焦点放进搜索框一次。
    pub(super) help_focused: bool,
    pub(super) pending_browser: Option<String>,
    connected_seen: std::collections::BTreeSet<String>,
    nav_refresh_active: std::collections::BTreeSet<Option<String>>,
    nav_refresh_again: std::collections::BTreeSet<Option<String>>,
    nav_refresh_tasks: std::collections::HashMap<Option<String>, futures::future::AbortHandle>,
    nav_revision: std::collections::HashMap<String, u64>,
    nav_reads: std::collections::HashMap<String, (String, futures::future::AbortHandle)>,
    nav_search_abort: Option<futures::future::AbortHandle>,
    nav_gate: Arc<crate::catalog::nav_read::MetadataGate>,
    /// 定义查看器的只读编辑器（SQL 高亮），按读取编号填入文本。
    pub(super) ddl_editor: Option<(u64, Entity<gpui_kit::component::input::EditorState>)>,
    /// 对象树列表的滚动；跳转定义后把选中行滚进可见区。
    pub(super) explorer_scroll: gpui_kit::ScrollHandle,
    /// 命令面板列表滚动：上下/Home/End 导航与循环越界时把选中行滚进可视区。
    pub(super) palette_scroll: gpui_kit::ScrollHandle,
    pub(super) palette_virtual_scroll: gpui_kit::UniformListScrollHandle,
    pub(super) palette_scroll_seen: Option<(usize, String, super::model::PaletteScope)>,
    /// 历史/保存查询列表滚动：方向键移动时跟随选中卡片。
    pub(super) history_scroll: gpui_kit::ScrollHandle,
    /// 连接启动台“全部连接”列表滚动：方向键移动时跟随选中行。
    pub(super) launcher_scroll: gpui_kit::ScrollHandle,
    pub(super) danger_focused_epoch: Option<u64>,
    pub(super) password_focused_epoch: Option<u64>,
    pub(super) history_rename_focused: bool,
    /// 工作项页签条的横向滚动。跨帧保存偏移，不能每帧新建句柄；
    /// Ctrl+Tab、新建或关闭之后要把当前页签滚进可视区。
    pub(super) tab_scroll: gpui_kit::ScrollHandle,
    /// 上一次已经请求过滚动的（当前页签 id，页签数量）。两者都没变就不再新增请求。
    pub(super) tab_scroll_seen: Option<(String, usize)>,
    /// 还没确认进入视口的当前页签：几何要等下一帧才量得出来，所以请求会挂着，
    /// 直到它在列表视口里（或超过帧数上限——零宽视口确认不了，必须自己停下，
    /// 否则每帧都要一帧重绘）。确认之后不再动滚动位置，用户手动横向滚开不会被拉回。
    pub(super) tab_scroll_pending: Option<crate::ui::tabs::Reveal>,
    /// 切到次级页面后，下一帧把键盘焦点交回工作区根。
    pub(super) restore_pending: bool,
    pub(super) seen_page: AppPage,
    admin_link: Option<(String, bool)>,
    /// 历史预览的只读 SQL 编辑器，按条目 id 与原文重建。
    pub(super) history_editor: Option<(String, Entity<gpui_kit::component::input::EditorState>)>,
    /// 页签、资源树行、状态栏等位置的转圈：每个位置一个隔离实体，动画帧不通知工作台。
    pub(super) spinners: crate::ui::loading::SpinnerPool,
    /// 页签条的缓存视图：编辑器光标闪烁、转圈动画只把工作台标脏，不通知它；
    /// 页签条沿用上一帧的布局和绘制，几百个标签时空闲重绘不再逐个重排页签。
    pub(super) tab_strip_host: Entity<TabStripHost>,
    /// 页签条“标签列表”弹层（按标题、连接、库搜索打开的标签）。
    pub(super) tab_picker: Option<(Entity<crate::ui::picker::Picker>, gpui_kit::Subscription)>,
    /// 标签列表按钮上一帧的位置：弹层贴着它弹出。
    pub(super) tab_picker_anchor: std::rc::Rc<std::cell::Cell<Option<gpui_kit::Bounds<gpui_kit::Pixels>>>>,
    /// 这一波事件里已经连续整理了几遍；画完一帧或隔了一会儿归零。
    settle_passes: u32,
    settle_burst: std::time::Instant,
    settling: bool,
    /// 这一帧里已经因为整理上限补排过一次延后整理；画完一帧归零。
    settle_deferred: bool,
    /// 测试用：render 与 settle 各执行了多少次。
    #[cfg(test)]
    pub(super) render_count: std::cell::Cell<usize>,
    #[cfg(test)]
    pub(super) settle_count: std::cell::Cell<usize>,
    #[cfg(test)]
    pub(super) deferred_settle_count: std::cell::Cell<usize>,
    #[cfg(test)]
    pub(super) strip_count: std::cell::Cell<usize>,
    _child_subscriptions: Vec<gpui_kit::Subscription>,
}

impl Drop for Workbench {
    fn drop(&mut self) {
        for (_, handle) in self.nav_reads.values() { handle.abort(); }
        for handle in self.nav_refresh_tasks.values() { handle.abort(); }
    }
}

/// R6：指针纵坐标落在行的下半部时，拖放插到这一行之后。
pub(super) fn drop_lower_half(top: f32, height: f32, y: f32) -> bool {
    y > top + height / 2.0
}

/// 读一个节点所属库的会话。会话绑定到单个库的引擎（PostgreSQL）读别的库时另开一个连到
/// 那个库的临时会话（和比较页同一条路，读完随返回值一起断开），当前库仍用原会话。
/// 能跨库读目录的引擎（SQL Server 三段名）同一个会话就够，不发 USE。
async fn nav_read_session(
    services: &AppServices,
    engine: Engine,
    connection_id: &str,
    database: &str,
    current: Option<&str>,
) -> Result<(crate::connections::CatalogSession, Option<crate::connections::EphemeralRead>), String> {
    if crate::adapters::adapter(engine).capabilities().database_scoped_session && !database.is_empty() && current != Some(database) {
        let read = services.connections.open_ephemeral_postgres(connection_id, database).await?;
        let session = read.catalog();
        return Ok((session, Some(read)));
    }
    let session = services
        .connections
        .catalog_session(connection_id)
        .ok_or_else(|| "这条连接还没有打开".to_owned())?;
    Ok((session, None))
}

impl Workbench {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let services = Arc::new(AppServices::from_state(&crate::connections::commands::ConnectionsState::new()));
        Self::with_services(window, cx, services, true)
    }

    pub fn with_services(window: &mut Window, cx: &mut Context<Self>, services: Arc<AppServices>, boot: bool) -> Self {
        // 构造一结束就整理一次：第一帧画出来时状态已经就绪。
        cx.defer_in(window, |this, window, cx| this.attach(window, cx));
        let mut this = Self::build(window, cx, services, boot);
        // 测试里窗口常在同一个闭包里直接绘制，延迟回调还没轮到；先把窗口量好。
        this.measure(window);
        this
    }

    fn build(window: &mut Window, cx: &mut Context<Self>, services: Arc<AppServices>, boot: bool) -> Self {
        let metadata_gate = Arc::clone(&services.metadata_gate);
        let query = cx.new(|cx| QueryPane::new(Arc::clone(&services), window, cx));
        let table = cx.new(|cx| GridPane::new(Arc::clone(&services), window, cx));
        // 外键跳转的出口：网格只算出目标和定位查询，真正打开工作项由外壳负责。
        // 这条接线就是"跳转"与"只复制 SQL"的区别，所以必须在这里接上，而不是留给网格自己。
        let nav_owner = cx.entity().downgrade();
        let struct_owner = cx.entity().downgrade();
        table.update(cx, |pane, _| {
            pane.set_navigation_sink(Arc::new(move |navigation: crate::app::views::grid::GridNavigation, cx: &mut App| {
                if let Some(owner) = nav_owner.upgrade() {
                    owner.update(cx, |view, cx| view.open_reference(navigation, cx));
                }
            }));
            pane.set_structure_sink(Arc::new(move |cx: &mut App| {
                if let Some(owner) = struct_owner.upgrade() {
                    owner.update(cx, |view, cx| view.perform("switch-to-structure", cx));
                }
            }));
        });
        // 单元格右键：表格组件截住了右键，外层的 table-host / editor-host 收不到；
        // 网格把指针位置交过来，这里弹出同一个网格菜单（表数据页与查询结果共用）。
        let menu_owner = cx.entity().downgrade();
        let grid_menu: crate::app::views::grid::MenuSink = Arc::new(move |position, cx: &mut App| {
            if let Some(owner) = menu_owner.upgrade() {
                owner.update(cx, |view, cx| {
                    view.menu_at = position;
                    view.perform("menu-grid", cx);
                });
            }
        });
        table.update(cx, |pane, _| pane.set_menu_sink(Arc::clone(&grid_menu)));
        // 网格里的“提交事务”与顶部事务条同一条命令：状态只由外壳推进。
        let transaction_owner = cx.entity().downgrade();
        let transaction_sink: crate::app::views::grid::CommandSink = Arc::new(move |id: &str, cx: &mut App| {
            if let Some(owner) = transaction_owner.upgrade() {
                owner.update(cx, |view, cx| view.perform(id, cx));
            }
        });
        table.update(cx, |pane, _| pane.set_transaction_sink(Arc::clone(&transaction_sink)));
        query.update(cx, |pane, cx| pane.set_grid_menu_sink(grid_menu, cx));
        // 编辑器里 Ctrl/⌘+点击对象的出口：查询页只解析出对象，树定位和结构页由外壳负责。
        let definition_owner = cx.entity().downgrade();
        query.update(cx, |pane, _| {
            pane.set_definition_sink(std::rc::Rc::new(move |link: crate::ui::ObjectLink, cx: &mut App| {
                if let Some(owner) = definition_owner.upgrade() {
                    owner.update(cx, |view, cx| view.go_to_definition(link, cx));
                }
            }));
        });
        let data_owner = cx.entity().downgrade();
        let struct_query_owner = cx.entity().downgrade();
        let struct_nav_owner = cx.entity().downgrade();
        let structure = cx.new(|_| {
            let mut pane = StructurePane::new(Arc::clone(&services));
            pane.set_shell_overlay_host(true);
            pane.set_data_sink(Arc::new(move |cx: &mut App| {
                if let Some(owner) = data_owner.upgrade() {
                    owner.update(cx, |view, cx| view.perform("switch-to-data", cx));
                }
            }));
            pane.set_query_sink(Arc::new(move |target: GridTarget, sql: String, cx: &mut App| {
                if let Some(owner) = struct_query_owner.upgrade() {
                    owner.update(cx, |view, cx| {
                        view.model.open_bound_query(
                            sql,
                            (!target.connection_id.is_empty()).then_some(target.connection_id),
                            (!target.connection_name.is_empty()).then_some(target.connection_name),
                            (!target.database.is_empty()).then_some(target.database),
                            (!target.engine.is_empty()).then_some(target.engine),
                        );
                        view.model.shell.focus = crate::app::FocusTarget::Editor;
                        view.adopt_shell(Effect::None, cx);
                        view.query.update(cx, |q, _| q.request_focus());
                        view.persist_if_needed(cx);
                        cx.notify();
                    });
                }
            }));
            let templates_owner = struct_nav_owner.clone();
            pane.set_templates_sink(Arc::new(move |templates: crate::ddl::templates::TemplateStore, cx: &mut App| {
                if let Some(owner) = templates_owner.upgrade() {
                    owner.update(cx, |view, cx| {
                        view.model.column_templates = templates;
                        view.persist_if_needed(cx);
                    });
                }
            }));
            pane.set_structure_nav_sink(Arc::new(move |target: GridTarget, cx: &mut App| {
                if let Some(owner) = struct_nav_owner.upgrade() {
                    owner.update(cx, |view, cx| {
                        let spec = ObjectTarget::new(
                            target.connection_id,
                            target.connection_name,
                            target.database,
                            target.schema,
                            target.table,
                            target.engine,
                            target.environment,
                        )
                        .spec();
                        view.perform(&format!("open-structure-for:{spec}"), cx);
                    });
                }
            }));
            pane
        });
        let admin = cx.new(|_| AdminPane::new(Arc::clone(&services)));
        let object_editor = cx.new(|_| ObjectEditorPane::new(Arc::clone(&services)));
        let exchange = cx.new(|_| ExchangePane::new(Arc::clone(&services)));
        let compare = cx.new(|_| ComparePane::new(Arc::clone(&services)));
        let diagram = cx.new(|_| DiagramPane::new(Arc::clone(&services)));
        let redis = cx.new(|_| RedisPane::new(Arc::clone(&services)));
        let mongo = cx.new(|_| MongoPane::new(Arc::clone(&services)));
        super::chrome::bind_palette_keys(cx);
        let palette_owner = cx.entity().downgrade();
        let child_subscriptions = vec![
            cx.observe(&query, |_, _, cx| cx.notify()),
            cx.observe(&table, |_, _, cx| cx.notify()),
            cx.observe(&admin, |_, _, cx| cx.notify()),
            cx.observe(&object_editor, |_, _, cx| cx.notify()),
            cx.observe(&exchange, |_, _, cx| cx.notify()),
            cx.observe(&structure, |_, _, cx| cx.notify()),
            cx.observe(&compare, |_, _, cx| cx.notify()),
            cx.observe(&diagram, |_, _, cx| cx.notify()),
            // 键浏览器里双击、回车或菜单「在新标签中固定打开」留下的请求：在这里取走并新建固定的 Redis 标签。
            cx.observe(&redis, |this, redis, cx| {
                this.drain_redis_open_requests(&redis, cx);
                cx.notify();
            }),
            cx.observe(&mongo, |_, _, cx| cx.notify()),
            // 命令面板打开且搜索框为空时，Input 的 Home/End 动作会先于 capture_key_down 抢走按键；
            // 在拦截阶段转成首项/末项跳转，有输入文本时则保留原生光标跳转。
            cx.intercept_keystrokes(move |event, _window, cx| {
                let key = event.keystroke.key.as_str();
                if !matches!(key, "home" | "end") || event.keystroke.modifiers.modified() {
                    return;
                }
                let Some(owner) = palette_owner.upgrade() else {
                    return;
                };
                let should_handle = {
                    let this = owner.read(cx);
                    this.model.shell.palette_open && this.model.palette_query.is_empty()
                };
                if !should_handle {
                    return;
                }
                cx.stop_propagation();
                owner.update(cx, |this, cx| {
                    let nav = if key == "home" { super::menus::NavKey::Home } else { super::menus::NavKey::End };
                    let _ = this.model.key(nav);
                    cx.notify();
                });
            }),
            // 跟随系统主题：系统外观变化由 GPUI 窗口事件推送，不轮询。
            // 句柄随工作台释放，回调只同步主题，不触碰草稿、标签、连接或后台任务。
            cx.observe_window_appearance(window, |this, window, cx| {
                this.sync_window_appearance(window.appearance(), cx);
            }),
            // 窗口失焦时关掉右键菜单：回来时不会还挂着一个指向旧现场的菜单。
            cx.observe_window_activation(window, |this, window, cx| {
                if !window.is_window_active() && this.model.close_menu() {
                    cx.notify();
                }
            }),
            // 事件驱动的整理：本视图被通知（命令、后台回执、子视图变化转来的通知）后、下一帧之前整理一次。
            // Spinner 动画帧只通知它自己的小视图，不会走到这里。
            cx.observe_self(|this, cx| {
                this.settle_from_event(cx);
                // 工作台真的被通知（状态可能变了）时，页签条才重画；只是被标脏时沿用缓存。
                this.tab_strip_host.update(cx, |_, cx| cx.notify());
            }),
            // 窗口尺寸变化：重新量宽高（窄窗口收起侧栏、按宽度排版）。
            cx.observe_window_bounds(window, |this, window, cx| {
                this.measure(window);
                cx.notify();
            }),
        ];
        let weak_self = cx.entity().downgrade();
        let mut model = WorkbenchModel::new(Shell::default());
        let _ = model.shell.set_prefers_dark(matches!(window.appearance(), WindowAppearance::Dark | WindowAppearance::VibrantDark));
        crate::ui::kit::apply_theme(matches!(model.shell.resolved_theme(), crate::app::ResolvedTheme::Dark), cx);
        Self {
            model,
            query,
            table,
            structure,
            admin,
            object_editor,
            exchange,
            compare,
            diagram,
            redis,
            mongo,
            applied_tool: None,
            applied_locale: None,
            applied_catalog_revision: None,
            applied_tags_revision: 0,
            table_view_spec: None,
            exchange_kind: ExchangeSourceKind::Table,
            exchange_item: None,
            exchange_stamp: None,
            applied_diagram: None,
            applied_redis: None,
            applied_mongo: None,
            services,
            transaction_task: None,
            transaction_task_connection: None,
            auto_begin_attempt: None,
            transaction_revisions: std::collections::BTreeMap::new(),
            focus: cx.focus_handle().tab_stop(true),
            nav_focus: cx.focus_handle().tab_stop(true),
            overview_focus: cx.focus_handle(),
            tab_rename_focused: false,
            focus_explorer_filter: false,
            tab_rename_blur: None,
            conn_launcher_focus: cx.focus_handle().tab_stop(true),
            conn_sidebar_focus: cx.focus_handle().tab_stop(true),
            modal_focus: cx.focus_handle(),
            modal_controls: std::collections::HashMap::new(),
            modal_actions: std::collections::HashMap::new(),
            close_hooked: false,
            booted: false,
            health_task: None,
            settings_task: None,
            job_tasks: Vec::new(),
            update_task: None,
            update_timer: None,
            notice_seen: (None, String::new()),
            notice_since: std::time::Instant::now(),
            notice_timer: None,
            group_prompt_focused: false,
            tag_label_focused: false,
            boot,
            sized: false,
            last_state: String::new(),
            revision: Arc::new(AtomicU64::new(0)),
            persist_gate: Arc::new(tokio::sync::Mutex::new(())),
            reorder_queue: Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new())),
            reorder_gate: Arc::new(tokio::sync::Mutex::new(())),
            drop_after: None,
            fields: crate::ui::kit::Fields::default(),
            menu_at: gpui_kit::Point::default(),
            conn_filter_at: gpui_kit::Point::default(),
            palette_focused: false,
            help_focused: false,
            pending_browser: None,
            connected_seen: std::collections::BTreeSet::new(),
            nav_refresh_active: std::collections::BTreeSet::new(),
            nav_refresh_again: std::collections::BTreeSet::new(),
            nav_refresh_tasks: std::collections::HashMap::new(),
            nav_revision: std::collections::HashMap::new(),
            nav_reads: std::collections::HashMap::new(),
            nav_search_abort: None,
            nav_gate: metadata_gate,
            ddl_editor: None,
            explorer_scroll: gpui_kit::ScrollHandle::new(),
            palette_scroll: gpui_kit::ScrollHandle::new(),
            palette_virtual_scroll: gpui_kit::UniformListScrollHandle::new(),
            palette_scroll_seen: None,
            history_scroll: gpui_kit::ScrollHandle::new(),
            launcher_scroll: gpui_kit::ScrollHandle::new(),
            danger_focused_epoch: None,
            password_focused_epoch: None,
            history_rename_focused: false,
            tab_scroll: gpui_kit::ScrollHandle::new(),
            tab_scroll_seen: None,
            tab_scroll_pending: None,
            restore_pending: false,
            seen_page: AppPage::Workspace,
            admin_link: None,
            history_editor: None,
            spinners: crate::ui::loading::SpinnerPool::new(),
            tab_strip_host: cx.new(|_| TabStripHost { workbench: weak_self }),
            tab_picker: None,
            tab_picker_anchor: std::rc::Rc::new(std::cell::Cell::new(None)),
            settle_passes: 0,
            settle_burst: std::time::Instant::now(),
            settling: false,
            settle_deferred: false,
            #[cfg(test)]
            render_count: std::cell::Cell::new(0),
            #[cfg(test)]
            settle_count: std::cell::Cell::new(0),
            #[cfg(test)]
            deferred_settle_count: std::cell::Cell::new(0),
            #[cfg(test)]
            strip_count: std::cell::Cell::new(0),
            _child_subscriptions: child_subscriptions,
        }
    }

    /// 窗口里的第一次整理：量窗口、接上关窗钩子、启动状态读取。之后只由事件驱动。
    /// 构造完成后由 `new` / `with_services` 的调用方触发（见 `attach`）。
    fn attach(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.hook_close(window, cx);
        self.measure(window);
        self.settle(cx);
    }

    pub fn shell(&self) -> &Shell {
        &self.model.shell
    }

    /// 命令的唯一入口。命令改完状态后立即整理一次（补拉作业、同步标签与工具页），
    /// 这样同一个事件里紧接着画的那一帧看到的已经是整理后的状态。
    pub fn perform(&mut self, id: &str, cx: &mut Context<Self>) {
        self.perform_command(id, cx);
        self.settle(cx);
    }

    /// 对象编辑器要编辑的那个对象：右键的那一行优先，其次对象树焦点行。
    ///
    /// 只认事件、函数、存储过程与触发器；表与视图仍走结构页和定义页。
    pub(super) fn object_editor_request(&mut self) -> Option<EditorRequest> {
        let node_id = self.model.nav_menu_node.clone().or_else(|| self.model.nav.focus().map(str::to_owned));
        self.model.nav_menu_node = None;
        let node_id = node_id?;
        let object = self.model.nav.node(&node_id)?.object()?;
        if !crate::app::views::object_editor::editable(object.kind) {
            return None;
        }
        Some(EditorRequest {
            connection_id: object.connection_id,
            database: object.database,
            schema: object.schema,
            engine: object.engine.as_str().to_owned(),
            kind: object.kind,
            name: object.name,
        })
    }

    /// 编辑器里改成功（或删掉）一个对象后刷新对象树：树里的条目要跟服务器一致。
    fn consume_object_editor(&mut self, cx: &mut Context<Self>) {
        // 管理页的事件行要改整份定义：先取走它的请求，再由同一个编辑器打开。
        if let Some(request) = self.admin.update(cx, |pane, _| pane.take_object_editor_request()) {
            self.object_editor.update(cx, |pane, cx| pane.open_request(request, cx));
        }
        let Some((connection, database, _kind, name)) = self.object_editor.update(cx, |pane, _| pane.take_changed()) else { return };
        self.model.request_nav_refresh(Some(connection));
        self.model.notice_key = None;
        self.model.notice_detail = String::new();
        let _ = (database, name);
    }

    /// 对象编辑器盖在当前标签上：打开着并指向已有对象时，侧栏定位指向这个例程/事件/触发器。
    fn sync_object_editor_target(&mut self, cx: &mut Context<Self>) {
        let target = {
            let pane = self.object_editor.read(cx);
            let session = &pane.session;
            (pane.is_open() && !session.name.trim().is_empty() && !session.connection_id.is_empty()).then(|| super::model::RevealTarget::Object {
                connection_id: session.connection_id.clone(),
                database: session.database.clone(),
                schema: session.schema.clone(),
                name: session.name.clone(),
                kinds: vec![session.kind],
            })
        };
        if self.model.object_editor_target != target {
            self.model.object_editor_target = target;
        }
    }

    fn perform_command(&mut self, id: &str, cx: &mut Context<Self>) {
        if id == "explorer-search" {
            self.focus_explorer_filter = true;
        }
        if id == "reveal-active-tab" {
            // 定位高亮到时自动淡出：到点重绘一次。
            let view = cx.entity().downgrade();
            cx.spawn(async move |_, cx| {
                cx.background_executor().timer(super::model::REVEAL_FLASH + std::time::Duration::from_millis(50)).await;
                cx.update(|cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |_, cx| cx.notify());
                    }
                });
            })
            .detach();
        }
        if id == "menu-grid" {
            // 网格菜单的条目取决于此刻的格子：表数据页或查询结果里那张网格现报事实。
            let table = self.model.shell.page == crate::app::AppPage::Workspace && self.model.tool == WorkTool::Table;
            self.model.grid_menu = if table { self.table.read(cx).menu_facts() } else { self.query.read(cx).grid_menu_facts(cx) };
        }
        if id == "menu-results" {
            self.model.results_menu = self.query.read(cx).results_menu_facts();
        }
        // 事务保护：断开只处理这一条连接；安装更新前处理所有未结束事务。
        if id == "conn-disconnect" {
            let selected = self.model.connections.selected.clone();
            if self.begin_transaction_guard(GuardCause::Disconnect, selected.as_deref(), cx) {
                return;
            }
        }
        if id == "update-install" && self.begin_transaction_guard(GuardCause::Update, None, cx) {
            return;
        }
        // 统一按需连接：树展开、连接行双击、打开数据/结构、新建查询到此对象都先到这里。
        // 已连接时返回 false，命令照常执行；否则连接在后台建立，动作排队等一次续接。
        if self.model.gate_on_demand(id) {
            self.persist_if_needed(cx);
            cx.notify();
            return;
        }
        // 对象编辑器：对象树右键、管理页按钮与命令面板都到这一个入口，不另开执行路径。
        if id == "open-object-editor" {
            match self.object_editor_request() {
                Some(request) => {
                    self.object_editor.update(cx, |pane, cx| pane.open_request(request, cx));
                }
                None => {
                    self.model.notice_key = Some("catalog.tree.noStructure");
                    self.model.notice_detail = self.model.object_target().map(|target| target.name).unwrap_or_default();
                }
            }
            self.persist_if_needed(cx);
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
            self.persist_if_needed(cx);
            cx.notify();
            return;
        }
        if id == "update-open-page" {
            let url = self.model.update.offer.as_ref().map(|offer| offer.page_url.clone()).unwrap_or_else(|| crate::release::check::RELEASES_PAGE.to_owned());
            cx.open_url(&url);
            return;
        }
        if id == "update-restart" {
            self.restart_for_update(cx);
            cx.notify();
            return;
        }
        if id == "quit-window" {
            if self.allow_close(cx) {
                cx.quit();
            }
            cx.notify();
            return;
        }
        if matches!(id, "conn-import-dbeaver" | "conn-import-navicat") {
            if self.model.close_prompt.is_none() && matches!(self.model.profile_import, super::conn_import::ImportState::Idle) {
                use super::conn_import::ProfileFormat;
                self.prompt_profile_import(if id == "conn-import-navicat" { ProfileFormat::Navicat } else { ProfileFormat::DBeaver }, cx);
            }
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
            self.persist_if_needed(cx);
            cx.notify();
            return;
        }
        // 设置页的“重试保存”：不重新派发任何设置命令，只再写一次当前快照。
        if id == "settings-retry-save" {
            self.persist_settings(cx);
            return;
        }
        // 历史页的 Delete 与条目右键菜单一致：删除当前选中的记录。
        // 表格的标记删除只在工作台的网格上有意义，不能在次级页里空转。
        if id == "mark-delete" && self.model.shell.page == AppPage::History {
            // 确认层开着时 Delete 归确认层：不能在这里再开一层或直接执行删除。
            if self.model.history.confirm_open() {
                cx.notify();
                return;
            }
            if let Some(command) = self.model.history.delete_command() {
                self.perform(command, cx);
            }
            return;
        }
        // 启动页“再次执行”：先像“在新查询中打开”一样载入，只读语句再按整段脚本执行
        // （重放历史里记录的全部语句，不受光标位置影响；同一条按需连接与执行路径）；
        // 写入语句只载入，等用户确认后自己执行。
        if id == "welcome-run-again" {
            let _ = self.model.dispatch(id);
            self.adopt_shell(Effect::None, cx);
            if std::mem::take(&mut self.model.rerun_pending) {
                self.perform("run-script", cx);
            }
            self.persist_if_needed(cx);
            cx.notify();
            return;
        }
        // 唯一的“打开文件”入口（⌘O / Ctrl+O、顶栏、欢迎页、连接页、菜单、Dock 都到这里）。
        if id == "open-file" {
            let Some(effect) = self.model.dispatch(id) else {
                return;
            };
            if matches!(effect, Effect::PromptOpen) {
                self.adopt_shell(Effect::None, cx);
                self.prompt_open_files(cx);
            } else {
                self.apply_effect(effect, cx);
            }
            self.persist_if_needed(cx);
            cx.notify();
            return;
        }
        // 认不出类型的文件：用户选的打开方式。
        if matches!(id, "open-as-sql" | "open-as-database" | "open-choice-cancel") {
            if let Some(path) = self.model.open_choices.pop_front() {
                match id {
                    "open-as-sql" => self.open_sql_paths(vec![path], cx),
                    "open-as-database" => self.apply_database_path(path, crate::adapters::registry::LOCAL_FILE),
                    _ => {}
                }
            }
            self.persist_if_needed(cx);
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
            self.persist_if_needed(cx);
            cx.notify();
            return;
        }
        if id == "compare" || id.starts_with("compare-") {
            if self.model.close_prompt.is_some() {
                let _ = self.model.dispatch(id);
                cx.notify();
                return;
            }
            // 表对表对比：“对比表…”取右键的这张表预填基准侧；“对比表”取多选的两张表。
            if id == "compare-table" || id == "compare-tables" {
                let (left, right) = if id == "compare-tables" {
                    let mut picked = self.model.selected_table_targets().into_iter();
                    match (picked.next(), picked.next(), picked.next()) {
                        (Some(left), Some(right), None) => (Some(left), Some(right)),
                        _ => (None, None),
                    }
                } else {
                    (self.model.object_target(), None)
                };
                self.model.nav_menu_node = None;
                let Some(left) = left.filter(|target| target.is_relational()) else {
                    self.model.notice_key = Some(if id == "compare-tables" { "catalog.tree.compareNeedsTwo" } else { "catalog.tree.relationalOnly" });
                    self.model.notice_detail.clear();
                    self.persist_if_needed(cx);
                    cx.notify();
                    return;
                };
                let left = CompareContext::of(&left);
                let right = right.map(|target| CompareContext::of(&target));
                let _ = self.model.dispatch(id);
                let locale = self.model.locale();
                self.compare.update(cx, |pane, cx| {
                    pane.set_locale(locale);
                    pane.open_table_pair(left, right, cx);
                    pane.command(id, cx);
                });
                self.refresh_compare(cx);
                self.persist_if_needed(cx);
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
                // 从对象树右键库/模式/连接进入时，以被点的那一行为准：object_spec 只在
                // 对象节点上同步，之前点过的表会残留，不能顶掉这次右键的库。
                "compare-object" => {
                    let from_tree_menu = self.model.nav_menu_node.is_some();
                    let picked = if from_tree_menu {
                        self.model.nav_scope_target().or_else(|| self.model.object_target())
                    } else {
                        self.model.object_target().or_else(|| self.model.nav_scope_target())
                    };
                    let target = picked.or_else(|| self.model.compare_target());
                    self.model.nav_menu_node = None;
                    if target.as_ref().is_some_and(|target| !target.is_relational()) {
                        self.model.notice_key = Some("catalog.tree.relationalOnly");
                        self.model.notice_detail = target.as_ref().map(|target| target.name.clone()).unwrap_or_default();
                        self.persist_if_needed(cx);
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
            self.persist_if_needed(cx);
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
            self.persist_if_needed(cx);
            cx.notify();
            return;
        }
        if id == "commit"
            && self.model.shell.page == AppPage::Workspace
            && self.model.tool == WorkTool::Query
            && !self.query.read(cx).grid_uncommitted(cx)
        {
            if self.model.close_prompt.is_some() {
                let _ = self.model.dispatch(id);
                cx.notify();
                return;
            }
            self.forward_query("save-sql-file", cx);
            self.persist_if_needed(cx);
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
            self.persist_if_needed(cx);
            cx.notify();
            return;
        }
        // Redis 工作台可以在同一标签中切库，新查询沿用面板里实际显示的库。
        let redis_database = (id == "new-query"
            && self.model.tool == WorkTool::Redis
            && self.applied_redis.is_some()
            && self.applied_redis == self.model.redis_spec)
            .then(|| self.redis.read(cx).database().to_owned());
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
                self.persist_if_needed(cx);
                cx.notify();
            }
            return;
        };
        if let Some(database) = redis_database {
            if let Some(tab) = self.model.active_mut().filter(|tab| tab.kind.is_query() && tab.engine.as_deref().and_then(crate::adapters::query::workbench_named) == Some(crate::adapters::Workbench::Redis)) {
                tab.database = Some(database);
            }
        }
        // 新建查询或从定义查看器转入查询后光标直接落在编辑器里：点完就能编辑。
        if matches!(effect, Effect::FocusEditor)
            || (matches!(id, "new-query" | "ddl-open-query") && self.model.tool == super::model::WorkTool::Query)
        {
            if matches!(id, "new-query" | "ddl-open-query") {
                self.model.shell.focus = crate::app::FocusTarget::Editor;
            }
            self.adopt_shell(Effect::None, cx);
            self.query.update(cx, |pane, _| pane.request_focus());
        } else if query_command(id) {
            self.adopt_shell(Effect::None, cx);
            self.forward_query(id, cx);
        } else {
            self.apply_effect(effect, cx);
        }
        self.flush_clipboard(cx);
        // 设置类命令走带反馈的写盘；其余命令沿用静默写盘。
        if crate::app::views::settings::is_settings_command(id) {
            self.persist_settings(cx);
        } else {
            self.persist_if_needed(cx);
        }
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
        self.persist_if_needed(cx);
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

    /// 状态栏的目标在查询页上就是“换执行目标”：直接弹出查询页的目标选择器。
    pub(super) fn open_query_target_picker(&mut self, cx: &mut Context<Self>) {
        let on_query = self.model.shell.page == crate::app::AppPage::Workspace
            && self.model.active_tab().is_some_and(|tab| tab.kind.is_query());
        if on_query {
            self.query.update(cx, |pane, cx| pane.open_target_picker(cx));
        }
    }

    fn forward_query(&mut self, id: &str, cx: &mut Context<Self>) {
        // 先把编辑器里还没收回的输入对齐进标签：不再每帧同步，这一步不能丢掉回执，
        // 否则下面按旧快照对齐会把外壳的旧草稿拉回编辑器。
        self.sync_query(cx);
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
            // 外壳已经过按需连接这道门；这里直接执行，避免面板再回一次外壳。
            pane.forwarded_command(id, cx);
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
        // 对象提示是全局的：所有 SQL 编辑器的补全与悬停提供者下一次请求时读取。
        crate::ui::SqlEditor::publish_object_hints(crate::app::views::settings::ObjectHints::parse(&settings.object_hints), cx);
        self.query.update(cx, |pane, cx| pane.apply_public_settings(&settings, cx));
        if self.structure.read(cx).index_naming() != &settings.index_naming {
            let naming = settings.index_naming.clone();
            self.structure.update(cx, |pane, cx| {
                pane.set_index_naming(naming);
                cx.notify();
            });
        }
        if self.model.connections.loaded {
            let saved: Vec<&str> = self.model.connections.rows.iter().map(|row| row.saved.id.as_str()).collect();
            self.query.update(cx, |pane, cx| pane.retain_saved_targets(&saved, cx));
        }
        // 对比的每次读取都按查询超时设置收口，避免一直停在“正在对比…”。
        let compare_timeout = u64::try_from(self.model.settings.query_timeout_sec.max(0)).unwrap_or(0);
        self.compare.update(cx, |pane, _| pane.set_query_timeout_sec(compare_timeout));
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
            pane.set_empty_hint(settings.empty_display != "blank", cx);
        });
    }

    /// 事件触发：把标签快照交给查询页对齐。编辑器不在这里建，查询页画当前标签时才懒建。
    fn sync_query(&mut self, cx: &mut Context<Self>) {
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
            effect
        });
        self.apply_query_effect(effect);
    }

    /// 对象索引只在渲染时从查询页同步一次；资源管理器、对象筛选和快速打开都读这份快照。
    fn sync_catalog(&mut self, cx: &mut Context<Self>) {
        let pane = self.query.read(cx);
        let revision = pane.catalog_revision();
        if self.applied_catalog_revision == Some(revision) { return; }
        self.applied_catalog_revision = Some(revision);
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
        for (id, sql) in effect.saved {
            if let Some(tab) = self.model.tabs.iter_mut().find(|tab| tab.id == id) {
                tab.saved_sql = Some(sql);
            }
        }
    }

    pub(crate) fn mark_query_saved(&mut self, tab_id: &str, sql: String, cx: &mut Context<Self>) {
        if let Some(tab) = self.model.tabs.iter_mut().find(|tab| tab.id == tab_id) {
            tab.saved_sql = Some(sql);
            cx.notify();
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
        let structure_dirty = self.structure.read(cx).pending_changes() > 0;
        let structure_target = self.structure.read(cx).session.target.clone();
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
                WorkItemKind::Structure => {
                    let matches_active = active.as_deref() == Some(tab.id.as_str()) && self.model.tool == WorkTool::Structure;
                    let matches_spec = !structure_target.connection_id.is_empty()
                        && tab.spec.as_deref().and_then(parse_target).is_some_and(|t| {
                            t.connection_id == structure_target.connection_id
                                && t.database == structure_target.database
                                && t.schema == structure_target.schema
                                && t.table == structure_target.table
                        });
                    if matches_active || matches_spec {
                        tab.structure_dirty = structure_dirty;
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
                    let prod = self.prod_for(&target);
                    self.table.update(cx, |pane, cx| {
                        pane.set_locale(locale);
                        pane.set_prod_target(prod);
                        pane.open_table(target, page_size, cx);
                    });
                }
            }
            WorkTool::Structure => {
                let prod = self.prod_for(&target);
                self.structure.update(cx, |pane, _| {
                    pane.set_locale(locale);
                    pane.bind_item(target, &item);
                    pane.set_prod_target(prod);
                });
            }
            WorkTool::Admin => {
                // 重新绑定会清掉探测闩；允许本帧按当前连接状态再探一次。
                self.admin_link = None;
                self.admin.update(cx, |pane, _| {
                    pane.set_locale(locale);
                    pane.bind_item(target, &item);
                });
            }
            WorkTool::Query | WorkTool::Redis | WorkTool::Mongo | WorkTool::Overview => {}
        }
    }

    fn ensure_redis(&mut self, cx: &mut Context<Self>) {
        if self.model.tool != WorkTool::Redis {
            return;
        }
        let locale = self.model.locale();
        let spec = self.model.redis_spec.clone().unwrap_or_default();
        // 生产标签按连接当前的标签现算：写入确认要输入连接名。
        let prod = self.redis_prod(&spec);
        self.redis.update(cx, |pane, _| {
            pane.set_locale(locale);
            pane.set_prod_target(prod);
        });
        if self.applied_redis.as_deref() == Some(spec.as_str()) {
            return;
        }
        self.applied_redis = Some(spec.clone());
        self.redis.update(cx, |pane, cx| pane.open_spec(&spec, cx));
    }

    /// 取走 Redis 面板的「在新标签中固定打开」请求（`连接|名称|库|键`），逐个新建固定标签。
    /// 新标签激活后由 `ensure_redis` 把带键的规格交给面板，面板直接选中并读取这个键。
    pub(super) fn drain_redis_open_requests(&mut self, redis: &Entity<RedisPane>, cx: &mut Context<Self>) {
        let requests = redis.update(cx, |pane, _| pane.take_open_requests());
        if requests.is_empty() {
            return;
        }
        for spec in requests {
            let _ = self.model.open_redis_key(&spec);
        }
        self.persist_if_needed(cx);
    }

    /// 键浏览目标（`id|name|db`）对应的连接此刻是否按生产处理。
    fn redis_prod(&self, spec: &str) -> bool {
        let id = spec.split('|').next().unwrap_or("").trim();
        !id.is_empty() && self.model.connection_is_prod(id)
    }

    /// 活动概览标签：已连接且待读时在后台读取统计；未连接的先按需连接（连上后下一帧再读）。
    fn ensure_overview(&mut self, cx: &mut Context<Self>) {
        self.model.sync_overviews();
        if self.model.tool != WorkTool::Overview {
            return;
        }
        let pending_before = self.model.pending.len();
        let Some((target, generation)) = self.model.overview_read_request() else {
            if self.model.pending.len() != pending_before {
                cx.notify();
            }
            return;
        };
        let spec = target.spec();
        let services = Arc::clone(&self.services);
        let gate = Arc::clone(&self.nav_gate);
        let current = self.model.nav_connection_database(&target.connection_id);
        let view = cx.entity().downgrade();
        let (tx, rx) = tokio::sync::oneshot::channel();
        jobs::runtime().spawn(async move {
            // 统计与名称退回都在同一个超时里：慢库不会让概览一直转圈。
            let result = match nav_read_session(&services, target.engine, &target.connection_id, &target.database, current.as_deref()).await {
                Ok((session, held)) => {
                    let body = crate::catalog::nav_read::limited_metadata(
                        gate,
                        &target.connection_id,
                        crate::catalog::read_object_stats(target.engine, &session, &target.database, &target.schema),
                    )
                    .await
                    .unwrap_or_else(Err);
                    drop(held);
                    body
                }
                Err(message) => Err(message),
            };
            let _ = tx.send(result);
        });
        cx.spawn(async move |_, cx| {
            let Ok(result) = rx.await else { return };
            cx.update(|cx| {
                let Some(view) = view.upgrade() else { return };
                view.update(cx, |this, cx| {
                    if this.model.finish_overview_read(&spec, generation, result) {
                        cx.notify();
                    }
                });
            });
        })
        .detach();
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
    pub fn commit_table_for_test(&mut self, cx: &mut Context<Self>) {
        self.table.update(cx, |pane, cx| pane.commit_all_for_test(cx));
    }

    #[cfg(test)]
    pub fn mark_table_dirty(&mut self, cx: &mut Context<Self>) {
        self.table.update(cx, |pane, cx| {
            pane.stage_sample_edit();
            cx.notify();
        });
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

    /// 测试用：查询面板里还挂着的行注释请求属于哪个标签。
    #[cfg(test)]
    pub fn query_pending_comment(&self, cx: &App) -> Option<String> {
        self.query.read(cx).pending_comment_request().map(str::to_owned)
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
        match effect {
            Effect::ApplyTheme => {
                let dark = matches!(self.model.shell.resolved_theme(), crate::app::ResolvedTheme::Dark);
                crate::ui::kit::apply_theme(dark, cx);
            }
            Effect::RefreshMenus => crate::app::install_menus(cx, &self.model.shell),
            _ => {}
        }
    }

    pub(crate) fn apply_effect(&mut self, effect: Effect, cx: &mut Context<Self>) {
        self.adopt_shell(effect, cx);
        let dark = matches!(self.model.shell.resolved_theme(), crate::app::ResolvedTheme::Dark);
        crate::ui::kit::apply_theme(dark, cx);
    }

    /// 窗口外观事件的落点：记录最新系统外观，只在解析出的主题真的变化时走 ApplyTheme。
    /// 显式浅色/深色下只记下外观，之后切回“跟随系统”时直接用这份最新值；
    /// 重复的同一外观（含 Vibrant 变体与普通变体之间）什么也不做。
    pub(crate) fn sync_window_appearance(&mut self, appearance: WindowAppearance, cx: &mut Context<Self>) {
        let prefers_dark = matches!(appearance, WindowAppearance::Dark | WindowAppearance::VibrantDark);
        if self.model.shell.prefers_dark == prefers_dark {
            return;
        }
        let before = self.model.shell.resolved_theme();
        let effect = self.model.shell.set_prefers_dark(prefers_dark);
        if self.model.shell.resolved_theme() == before {
            // 显式偏好不受系统外观影响；子视图持有的外壳副本在下次 adopt 时同步。
            return;
        }
        self.apply_effect(effect, cx);
        cx.notify();
    }

    /// 连上或断开连接后刷新对象树，资源管理器才看得到表。
    /// 目录树读取只在视图里发起：模型排队，结果回填到同一份 NavTree。
    fn pump_nav(&mut self, cx: &mut Context<Self>) {
        while let Some(request) = self.model.take_nav_request() {
            match request {
                super::nav::NavRequest::Refresh { connection } => self.spawn_nav_refresh(connection, cx),
                super::nav::NavRequest::Database(id) => self.spawn_nav_database(id, cx),
                super::nav::NavRequest::Details(id) => self.spawn_nav_details(id, cx),
                super::nav::NavRequest::More(id) => self.spawn_nav_more_with_limit(id, None, cx),
                super::nav::NavRequest::All(id) => self.spawn_nav_more_with_limit(id, Some(5_000), cx),
                super::nav::NavRequest::Search(query) => self.spawn_nav_search(query, cx),
                super::nav::NavRequest::CancelSearch => {
                    if let Some(previous) = self.nav_search_abort.take() {
                        previous.abort();
                    }
                }
                super::nav::NavRequest::Group(id) => self.spawn_nav_group(id, cx),
                super::nav::NavRequest::Reload(id) => self.spawn_nav_reload(id, cx),
                super::nav::NavRequest::Definition(id) => self.spawn_definition(id, cx),
            }
        }
        while let Some(request) = self.model.take_search_more_request() {
            self.spawn_search_more(request, cx);
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
        let current = self.model.nav_connection_database(&node.connection_id);
        let view = cx.entity().downgrade();
        let (tx, rx) = tokio::sync::oneshot::channel();
        jobs::runtime().spawn(futures::future::Abortable::new(async move {
            let body = match nav_read_session(&services, node.engine, &node.connection_id, &node.database, current.as_deref()).await {
                Ok((session, held)) => {
                    let body = crate::catalog::nav_read::limited_metadata(gate, &node.connection_id, crate::catalog::read_object_page(&session, node.engine, &node.database, &node.schema, 0, page)).await
                        .unwrap_or_else(|error| crate::catalog::read::ObjectPageBody { objects: Vec::new(), total: 0, error: Some(error), kind_reports: Vec::new() });
                    drop(held);
                    body
                }
                Err(message) => crate::catalog::read::ObjectPageBody { objects: Vec::new(), total: 0, error: Some(message), kind_reports: Vec::new() },
            };
            // 按种类的失败一并带回：失败的种类不能当成“整类都被删了”。
            let _ = tx.send((body.objects, body.total, body.error, body.kind_reports));
        }, abort));
        cx.spawn(async move |_, cx| {
            let result = rx.await;
            cx.update(|cx| {
                let Some(view) = view.upgrade() else { return };
                view.update(cx, |this, cx| {
                    if this.nav_revision.get(&connection_id).copied() != revision || cancelled.is_aborted() {
                        if this.drop_nav_read(&node_id, &cancelled) {
                            cx.notify();
                        }
                        return;
                    }
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

    /// 作废的读取回来了（被取消、换了更新的读取、整条连接刷新过）：结果丢掉；这个节点上
    /// 没有别的读取在途时收起加载态，否则转圈会一直转下去（空闲时也一直请求动画帧）。
    /// 返回加载态是否真的变了（调用方据此重绘）。
    fn drop_nav_read(&mut self, node_id: &str, ours: &futures::future::AbortHandle) -> bool {
        let other_live = self.nav_reads.get(node_id).is_some_and(|(_, handle)| !handle.is_aborted());
        if !nav_read_drop_clears(ours.is_aborted(), other_live) {
            return false;
        }
        self.nav_reads.remove(node_id);
        self.model.nav.end_loading(node_id)
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
                                for id in expanded { this.model.request_lazy_read(&id); }
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
        // 定位在等这次目录读取：读回后再定位一次（可能继续按需读取下一层）。
        if self.model.pending_reveal.is_some() && self.model.retry_pending_reveal() {
            self.pump_nav(cx);
        }
    }

    /// 库节点展开：读这个库的 schema 清单。加载态已由模型标在这个库上。
    fn spawn_nav_database(&mut self, node_id: String, cx: &mut Context<Self>) {
        let Some(node) = self.model.nav_node(&node_id).cloned() else { return };
        let (abort, cancelled) = self.track_nav_read(&node);
        let revision = self.nav_revision.get(&node.connection_id).copied();
        let connection_id = node.connection_id.clone();
        let gate = Arc::clone(&self.nav_gate);
        let services = Arc::clone(&self.services);
        let current = self.model.nav_connection_database(&node.connection_id);
        let view = cx.entity().downgrade();
        let (tx, rx) = tokio::sync::oneshot::channel();
        jobs::runtime().spawn(futures::future::Abortable::new(async move {
            let result = match nav_read_session(&services, node.engine, &node.connection_id, &node.database, current.as_deref()).await {
                Ok((session, held)) => {
                    let schemas = crate::catalog::nav_read::limited_metadata(gate, &node.connection_id, crate::catalog::read_database_outline(node.engine, &session, &node.database)).await;
                    drop(held);
                    schemas.unwrap_or_else(Err)
                }
                Err(message) => Err(message),
            };
            let _ = tx.send(result);
        }, abort));
        cx.spawn(async move |_, cx| {
            let result = rx.await;
            cx.update(|cx| {
                let Some(view) = view.upgrade() else { return };
                view.update(cx, |this, cx| {
                    if this.nav_revision.get(&connection_id).copied() != revision || cancelled.is_aborted() {
                        if this.drop_nav_read(&node_id, &cancelled) {
                            cx.notify();
                        }
                        return;
                    }
                    this.nav_reads.remove(&node_id);
                    match result {
                        Ok(schemas) => { this.model.nav.set_database(&node_id, schemas); }
                        Err(_) => { this.model.nav.set_database(&node_id, Err("读取库失败".to_owned())); }
                    }
                    this.sync_navigation_index(cx);
                    cx.notify();
                });
            });
        })
        .detach();
    }

    fn spawn_nav_details(&mut self, node_id: String, cx: &mut Context<Self>) {
        let Some(node) = self.model.nav_node(&node_id).cloned() else { return };
        let (abort, cancelled) = self.track_nav_read(&node);
        let revision = self.nav_revision.get(&node.connection_id).copied();
        let connection_id = node.connection_id.clone();
        let gate = Arc::clone(&self.nav_gate);
        let services = Arc::clone(&self.services);
        let current = self.model.nav_connection_database(&node.connection_id);
        let view = cx.entity().downgrade();
        let (tx, rx) = tokio::sync::oneshot::channel();
        jobs::runtime().spawn(futures::future::Abortable::new(async move {
            let result = match nav_read_session(&services, node.engine, &node.connection_id, &node.database, current.as_deref()).await {
                Ok((session, held)) => {
                    let pair = crate::catalog::nav_read::limited_metadata(gate, &node.connection_id, crate::catalog::read_object_details(&session, node.engine, &node.database, &node.schema, &node.name)).await
                        .unwrap_or_else(|error| (Err(error.clone()), Err(error)));
                    drop(held);
                    pair
                }
                Err(message) => (Err(message.clone()), Err(message)),
            };
            let _ = tx.send(result);
        }, abort));
        cx.spawn(async move |_, cx| {
            let result = rx.await;
            cx.update(|cx| {
                let Some(view) = view.upgrade() else { return };
                view.update(cx, |this, cx| {
                    if this.nav_revision.get(&connection_id).copied() != revision || cancelled.is_aborted() {
                        if this.drop_nav_read(&node_id, &cancelled) {
                            cx.notify();
                        }
                        return;
                    }
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

    fn spawn_nav_more_with_limit(&mut self, node_id: String, override_limit: Option<usize>, cx: &mut Context<Self>) {
        let Some(node) = self.model.nav_node(&node_id).cloned() else { return };
        if node.paged && node.kind == crate::catalog::hierarchy::NavKind::Group {
            self.spawn_nav_kind_with_limit(node_id, if node.unloaded { 0 } else { node.loaded }, override_limit, cx);
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
        let page = override_limit.unwrap_or_else(|| usize::try_from(self.model.settings.object_page.max(1)).unwrap_or(500));
        let offset = node.loaded;
        let current = self.model.nav_connection_database(&node.connection_id);
        let view = cx.entity().downgrade();
        let (tx, rx) = tokio::sync::oneshot::channel();
        jobs::runtime().spawn(futures::future::Abortable::new(async move {
            let body = match nav_read_session(&services, node.engine, &node.connection_id, &node.database, current.as_deref()).await {
                Ok((session, held)) => {
                    let body = crate::catalog::nav_read::limited_metadata(gate, &node.connection_id, crate::catalog::read_object_page(&session, node.engine, &node.database, &node.schema, offset, page)).await
                        .unwrap_or_else(|error| crate::catalog::read::ObjectPageBody { objects: Vec::new(), total: 0, error: Some(error), kind_reports: Vec::new() });
                    drop(held);
                    body
                }
                Err(message) => crate::catalog::read::ObjectPageBody { objects: Vec::new(), total: 0, error: Some(message), kind_reports: Vec::new() },
            };
            let _ = tx.send((body.objects, body.total, body.error));
        }, abort));
        cx.spawn(async move |_, cx| {
            let result = rx.await;
            cx.update(|cx| {
                let Some(view) = view.upgrade() else { return };
                view.update(cx, |this, cx| {
                    if this.nav_revision.get(&connection_id).copied() != revision || cancelled.is_aborted() {
                        if this.drop_nav_read(&node_id, &cancelled) {
                            cx.notify();
                        }
                        return;
                    }
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
        self.spawn_nav_kind_with_limit(group_id, 0, None, cx);
    }

    fn spawn_nav_kind_with_limit(&mut self, group_id: String, offset: usize, override_limit: Option<usize>, cx: &mut Context<Self>) {
        let Some(node) = self.model.nav_node(&group_id).cloned() else { return };
        let Some(kind) = node.object_kind else { return };
        self.model.nav.begin_group(&group_id);
        let (abort, cancelled) = self.track_nav_read(&node);
        let revision = self.nav_revision.get(&node.connection_id).copied();
        let connection_id = node.connection_id.clone();
        let services = Arc::clone(&self.services);
        let gate = Arc::clone(&self.nav_gate);
        let page = override_limit.unwrap_or_else(|| usize::try_from(self.model.settings.object_page.max(1)).unwrap_or(500));
        let current = self.model.nav_connection_database(&node.connection_id);
        let view = cx.entity().downgrade();
        let (tx, rx) = tokio::sync::oneshot::channel();
        jobs::runtime().spawn(futures::future::Abortable::new(async move {
            let result = crate::catalog::nav_read::limited_metadata(gate, &node.connection_id, async {
                let (session, _held) = nav_read_session(&services, node.engine, &node.connection_id, &node.database, current.as_deref()).await?;
                Ok(crate::catalog::read::read_navigation_kind_page(&session, node.engine, &node.database, &node.schema, kind, offset, page).await)
            }).await.and_then(|result| result);
            let _ = tx.send(result);
        }, abort));
        cx.spawn(async move |_, cx| {
            let result = rx.await;
            cx.update(|cx| {
                let Some(view) = view.upgrade() else { return };
                view.update(cx, |this, cx| {
                    if this.nav_revision.get(&connection_id).copied() != revision || cancelled.is_aborted() {
                        if this.drop_nav_read(&group_id, &cancelled) {
                            cx.notify();
                        }
                        return;
                    }
                    this.nav_reads.remove(&group_id);
                    match result {
                        Ok(Ok((body, total_known))) => {
                            if body.error.is_none() { this.model.nav.set_kind_total_known(&group_id, total_known); }
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

    fn spawn_nav_search(&mut self, query: String, cx: &mut Context<Self>) {
        if let Some(previous) = self.nav_search_abort.take() {
            previous.abort();
        }
        let trimmed = query.trim().to_owned();
        if trimmed.is_empty() {
            return;
        }
        let generation = self.model.nav_search_generation;
        let targets: Vec<(String, String, crate::catalog::Engine)> = self
            .model
            .connections
            .rows
            .iter()
            .filter(|row| {
                self.model.connections.row_status(&row.saved.id) == crate::app::views::connections::LinkStatus::Connected
                    && row.saved.engine.is_relational()
            })
            .map(|row| (row.saved.id.clone(), row.saved.name.clone(), row.saved.engine))
            .collect();
        self.model.begin_nav_search(generation, &targets);
        if targets.is_empty() {
            return;
        }
        let (handle, abort) = futures::future::AbortHandle::new_pair();
        self.nav_search_abort = Some(handle.clone());
        let services = Arc::clone(&self.services);
        let gate = Arc::clone(&self.nav_gate);
        let view = cx.entity().downgrade();
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let needle = trimmed.clone();
        // 连接之间并发（最多 4 条），每条完成就送回界面合并；失败、超时、会话丢失都按连接报告。
        jobs::runtime().spawn(futures::future::Abortable::new(async move {
            use futures::StreamExt;
            let reads = futures::stream::iter(targets).map(|(conn_id, conn_name, engine)| {
                let services = Arc::clone(&services);
                let gate = Arc::clone(&gate);
                let needle = needle.clone();
                async move {
                    let result = match services.connections.catalog_session(&conn_id) {
                        None => crate::catalog::read::CatalogSearch::failed("", crate::catalog::read::read_failure("连接已断开，重新连接后再搜索")),
                        Some(session) => {
                            let read = async move { crate::catalog::read::search_catalog_objects(&session, engine, &needle).await };
                            crate::catalog::nav_read::limited_metadata(gate, &conn_id, read)
                                .await
                                .unwrap_or_else(|error| crate::catalog::read::CatalogSearch::failed("", error))
                        }
                    };
                    (conn_id, conn_name, engine, result)
                }
            });
            let reads = reads.buffer_unordered(4);
            futures::pin_mut!(reads);
            while let Some(item) = reads.next().await {
                if tx.send(item).await.is_err() {
                    return;
                }
            }
        }, abort));
        cx.spawn(async move |_, cx| {
            while let Some((conn_id, conn_name, engine, result)) = rx.recv().await {
                let mut alive = false;
                cx.update(|cx| {
                    let Some(view) = view.upgrade() else { return };
                    alive = true;
                    view.update(cx, |this, cx| {
                        if handle.is_aborted() {
                            return;
                        }
                        if this.model.apply_search_result(generation, &trimmed, (conn_id, conn_name, engine), result) {
                            this.sync_navigation_index(cx);
                            cx.notify();
                        }
                    });
                });
                if !alive {
                    break;
                }
            }
        })
        .detach();
    }

    /// “还有更多，展开”：对一个截断的库按库再取一页命中。
    fn spawn_search_more(&mut self, request: super::model::SearchMoreRequest, cx: &mut Context<Self>) {
        let services = Arc::clone(&self.services);
        let gate = Arc::clone(&self.nav_gate);
        let view = cx.entity().downgrade();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let ask = request.clone();
        jobs::runtime().spawn(async move {
            let result = match services.connections.catalog_session(&ask.connection_id) {
                None => crate::catalog::read::CatalogSearch::failed(&ask.database, crate::catalog::read::read_failure("连接已断开，重新连接后再搜索")),
                Some(session) => {
                    let read = async { crate::catalog::read::search_catalog_database(&session, ask.engine, &ask.query, &ask.database, ask.offset).await };
                    crate::catalog::nav_read::limited_metadata(gate, &ask.connection_id, read)
                        .await
                        .unwrap_or_else(|error| crate::catalog::read::CatalogSearch::failed(&ask.database, error))
                }
            };
            let _ = tx.send(result);
        });
        cx.spawn(async move |_, cx| {
            let Ok(result) = rx.await else { return };
            cx.update(|cx| {
                let Some(view) = view.upgrade() else { return };
                view.update(cx, |this, cx| {
                    if this.model.apply_search_more(&request, result) {
                        this.sync_navigation_index(cx);
                        cx.notify();
                    }
                });
            });
        })
        .detach();
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
                        .any(|row| row.saved.id == intent.connection_id && !row.saved.engine.is_relational())
                    {
                        self.pending_browser = Some(intent.connection_id.clone());
                    }
                    // 连接页没有资源管理器：在那里“保存并连接”成功后进入工作区，
                    // 刚展开的连接树就在眼前，而不是停在列表上只换一个状态点。
                    if self.model.shell.page == AppPage::Connections {
                        self.perform("activity-workspace", cx);
                    }
                    cx.notify();
                }
                _ => {
                    let command = intent.command().map(str::to_owned);
                    if let Some(command) = command {
                        self.perform(&command, cx);
                    }
                }
            }
        }
        self.model.fail_pending_intents();
    }

    /// 启动后延迟检查一次更新：不和启动时的连接、目录读取抢网络；开关关闭时不检查。
    fn schedule_auto_update(&mut self, cx: &mut Context<Self>) {
        if self.model.update.auto_scheduled {
            return;
        }
        self.model.update.auto_scheduled = true;
        if cfg!(test) {
            // 测试不等真实时间：直接走同一个入口（开关、记账与排任务），由 pump_update 跳过网络。
            self.model.begin_update_check();
            return;
        }
        let view = cx.entity().downgrade();
        self.update_timer = Some(cx.spawn(async move |_, cx| {
            cx.background_executor().timer(AUTO_UPDATE_DELAY).await;
            cx.update(|cx| {
                let Some(view) = view.upgrade() else { return };
                view.update(cx, |this, cx| {
                    if this.model.begin_update_check() {
                        cx.notify();
                    }
                });
            });
        }));
    }

    /// 目标连接此刻是否按生产处理：连接还在时按它当前的标签现算，否则按打开时记下的环境。
    fn prod_for(&self, target: &GridTarget) -> bool {
        if self.model.connections.rows.iter().any(|row| row.saved.id == target.connection_id) {
            self.model.connection_is_prod(&target.connection_id)
        } else {
            super::danger::env_is_prod(&target.environment, Some(&self.model.env_tags))
        }
    }

    /// 标签表换了（改色、改名、删除或连接移出标签）：已打开的表数据页与结构页按新标签重算生产提示。
    fn sync_prod_targets(&mut self, cx: &mut Context<Self>) {
        if self.applied_tags_revision == self.model.env_tags_revision {
            return;
        }
        self.applied_tags_revision = self.model.env_tags_revision;
        if let Some(target) = self.table_view_spec.as_deref().and_then(parse_target) {
            let prod = self.prod_for(&target);
            self.table.update(cx, |pane, cx| {
                pane.set_prod_target(prod);
                cx.notify();
            });
        }
        if let Some(spec) = self.model.redis_spec.clone() {
            let prod = self.redis_prod(&spec);
            self.redis.update(cx, |pane, cx| {
                pane.set_prod_target(prod);
                cx.notify();
            });
        }
        let structure = self.structure.read(cx).session.target.clone();
        if !structure.connection_id.is_empty() {
            let prod = self.prod_for(&structure);
            self.structure.update(cx, |pane, cx| {
                pane.set_prod_target(prod);
                cx.notify();
            });
        }
    }

    /// 标签管理的读写：保存或删除后重读标签列表，结果交回模型。
    fn pump_tag_job(&mut self, cx: &mut Context<Self>) {
        let Some(job) = self.model.tag_job.take() else { return };
        let done = job.clone();
        let services = Arc::clone(&self.services);
        let view = cx.entity().downgrade();
        let (tx, rx) = tokio::sync::oneshot::channel();
        jobs::runtime().spawn(async move {
            let outcome = match job {
                super::model::TagJob::Save { id, label, tone } => services.connections.save_tag(id, label, tone).await.map(|_| ()).map_err(|err| err.message()),
                super::model::TagJob::Delete(id) => services.connections.delete_tag(&id).await,
                super::model::TagJob::Untag { connection } => services.connections.untag_connection(&connection).await,
            };
            let result = match outcome {
                Ok(()) => services.connections.list_tags().await,
                Err(message) => Err(message),
            };
            let _ = tx.send(result);
        });
        self.job_tasks.push(cx.spawn(async move |_, cx| {
            let result = rx.await.unwrap_or_else(|_| Err("tag job stopped".to_owned()));
            cx.update(|cx| {
                let Some(view) = view.upgrade() else { return };
                view.update(cx, |this, cx| {
                    this.model.finish_tag_job(&done, result);
                    cx.notify();
                });
            });
        }));
    }

    /// 取走模型排的更新任务并在后台线程执行。测试里不发真实网络请求，状态由测试直接驱动。
    fn pump_update(&mut self, cx: &mut Context<Self>) {
        let Some(job) = self.model.update_job.take() else { return };
        if cfg!(test) {
            return;
        }
        match job {
            super::update::UpdateJob::Check => self.spawn_update_check(cx),
            super::update::UpdateJob::Download => self.spawn_update_download(cx),
        }
    }

    fn spawn_update_check(&mut self, cx: &mut Context<Self>) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        // 阻塞的 HTTP 读取放在独立线程：不占 tokio 运行时，也不卡界面。
        let spawned = std::thread::Builder::new().name("rillsql-update-check".into()).spawn(move || {
            let _ = tx.send(crate::release::check::check_now(crate::release::PRODUCT_VERSION));
        });
        if let Err(err) = spawned {
            self.model.update.finish_check(Err(err.to_string()), String::new());
            return;
        }
        let view = cx.entity().downgrade();
        self.update_task = Some(cx.spawn(async move |_, cx| {
            let result = rx.await.unwrap_or_else(|_| Err("update check stopped".to_owned()));
            cx.update(|cx| {
                let Some(view) = view.upgrade() else { return };
                view.update(cx, |this, cx| {
                    let result = result.map_err(|raw| this.update_error_text(raw));
                    this.model.update.finish_check(result, chrono::Local::now().format("%H:%M").to_string());
                    cx.notify();
                });
            });
        }));
    }

    fn spawn_update_download(&mut self, cx: &mut Context<Self>) {
        let platform = self.model.update.offer.as_ref().and_then(|offer| offer.platform.clone());
        let route = self.model.update_route.clone();
        let (Some(platform), crate::release::check::InstallRoute::InPlace(extract)) = (platform, route) else {
            self.model.update.fail(self.model.text("update.unavailable"));
            return;
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<UpdateMessage>();
        let spawned = std::thread::Builder::new().name("rillsql-update-download".into()).spawn(move || {
            let reported = std::sync::atomic::AtomicU64::new(0);
            let progress = tx.clone();
            let result = crate::release::check::download_verified(&platform, &extract, |done, total| {
                // 进度按 256 KB 一档回报，不为每个读取块唤醒一次界面。
                let last = reported.load(Ordering::Relaxed);
                if done.saturating_sub(last) >= 256 * 1024 || total == Some(done) {
                    reported.store(done, Ordering::Relaxed);
                    let _ = progress.send(UpdateMessage::Progress(done, total));
                }
            });
            let _ = tx.send(UpdateMessage::Downloaded(Box::new(result)));
        });
        if let Err(err) = spawned {
            self.model.update.fail(err.to_string());
            return;
        }
        let view = cx.entity().downgrade();
        self.update_task = Some(cx.spawn(async move |_, cx| {
            while let Some(message) = rx.recv().await {
                let finished = matches!(message, UpdateMessage::Downloaded(_));
                cx.update(|cx| {
                    let Some(view) = view.upgrade() else { return };
                    view.update(cx, |this, cx| {
                        match message {
                            UpdateMessage::Progress(done, total) => this.model.update.progress(done, total),
                            UpdateMessage::Downloaded(result) => this.install_downloaded(*result, cx),
                        }
                        cx.notify();
                    });
                });
                if finished {
                    break;
                }
            }
        }));
    }

    /// 下载并验签完成：安装前再过一次门禁（下载期间可能开始了新任务或新编辑），
    /// 再把当前会话写盘——Windows 安装器启动后本进程会立即退出。
    fn install_downloaded(&mut self, result: Result<crate::release::check::VerifiedPackage, String>, cx: &mut Context<Self>) {
        let package = match result {
            Ok(package) => package,
            Err(message) => {
                let text = self.update_error_text(message);
                self.model.update.fail(text);
                return;
            }
        };
        if !self.transaction_facts(None).is_empty() {
            // 下载期间开始了新事务：安装会终止会话，必须先把事务处理完再重试安装。
            let text = self.model.text("transaction.guard.blocksUpdate");
            self.model.update.fail(text);
            self.model.notice_key = Some("transaction.guard.blocksUpdate");
            self.model.notice_detail.clear();
            cx.notify();
            return;
        }
        if self.model.request_update_install().is_err() {
            return;
        }
        self.model.update.stage = super::update::UpdateStage::Installing;
        let state = self.model.snapshot();
        self.last_state = serde_json::to_string(&state).unwrap_or_default();
        let generation = self.revision.fetch_add(1, Ordering::SeqCst) + 1;
        let revision = Arc::clone(&self.revision);
        let gate = Arc::clone(&self.persist_gate);
        let services = Arc::clone(&self.services);
        let (tx, rx) = tokio::sync::oneshot::channel();
        jobs::runtime().spawn(async move {
            if let Err(err) = jobs::persist_revision(&services, &state, &revision, generation, &gate).await {
                let _ = tx.send(Err(err));
                return;
            }
            let installed = tokio::task::spawn_blocking(move || crate::release::launch_verified_install(&package.update, package.bytes))
                .await
                .unwrap_or_else(|err| Err(err.to_string()));
            let _ = tx.send(installed);
        });
        let view = cx.entity().downgrade();
        self.update_task = Some(cx.spawn(async move |_, cx| {
            let result = rx.await.unwrap_or_else(|_| Err("install stopped".to_owned()));
            cx.update(|cx| {
                let Some(view) = view.upgrade() else { return };
                view.update(cx, |this, cx| {
                    match result {
                        Ok(()) => this.model.update.stage = super::update::UpdateStage::Ready,
                        Err(message) => {
                            let text = this.update_error_text(message);
                            this.model.update.fail(text);
                        }
                    }
                    cx.notify();
                });
            });
        }));
    }

    /// 原始错误换成当前语言的一句话；归不了类的保留原文，排查时还看得到细节。
    fn update_error_text(&self, raw: String) -> String {
        match super::update::error_key(&raw) {
            Some(key) => self.model.text(key),
            None => raw,
        }
    }

    /// 新版本已替换到位（macOS / AppImage）：按正常关闭流程退出，再打开新版本。
    /// 有未保存内容时走关闭守卫，不强行退出。
    fn restart_for_update(&mut self, cx: &mut Context<Self>) {
        if self.model.update.stage != super::update::UpdateStage::Ready {
            return;
        }
        let Some(mut command) = crate::release::check::relaunch_command(&self.model.update_route) else {
            return;
        };
        if !self.allow_close(cx) {
            return;
        }
        self.persist_if_needed(cx);
        match command.spawn() {
            Ok(_) => cx.quit(),
            Err(err) => self.model.update.fail(err.to_string()),
        }
    }

    // ---------------------------------------------------------- 共享事务（ADR-5）

    /// 已打开连接里还有未结束事务或结果未知的那些，供关闭保护逐条询问。
    /// 事实从连接服务的共享快照读，不从标签或页面本地推断。
    fn transaction_facts(&self, only: Option<&str>) -> Vec<GuardFact> {
        let mut known: Vec<(String, String, String, String)> = self
            .model
            .connections
            .rows
            .iter()
            .map(|row| {
                let saved = &row.saved;
                (saved.id.clone(), saved.name.clone(), saved.engine.as_str().to_owned(), saved.database.clone().unwrap_or_default())
            })
            .collect();
        for tab in &self.model.tabs {
            let (Some(id), Some(engine)) = (&tab.connection_id, &tab.engine) else { continue };
            if known.iter().any(|(known_id, ..)| known_id == id) {
                continue;
            }
            known.push((id.clone(), tab.connection_name.clone().unwrap_or_default(), engine.clone(), tab.database.clone().unwrap_or_default()));
        }
        known
            .into_iter()
            .filter(|(connection_id, ..)| only.is_none_or(|wanted| wanted == connection_id))
            .filter_map(|(connection_id, connection_name, engine, database)| {
                if !crate::adapters::is_relational_named(&engine) {
                    return None;
                }
                let snapshot = self.services.connections.transaction_snapshot(&connection_id)?;
                let unresolved = snapshot.unresolved.is_some();
                if snapshot.state == TransactionState::Idle && !unresolved {
                    return None;
                }
                Some(GuardFact { connection_id, connection_name, engine, database, state: snapshot.state, unresolved })
            })
            .collect()
    }

    /// 关闭/断开/更新前的事务保护。返回 true 表示流程已被拦下（对话框已打开或未知阻止关闭）。
    fn begin_transaction_guard(&mut self, cause: GuardCause, only: Option<&str>, cx: &mut Context<Self>) -> bool {
        // 对话框已经打开：重复的关闭/断开请求不再重置当前进度。
        if self.model.guard_open() {
            return true;
        }
        let facts = self.transaction_facts(only);
        let blocked = self.model.open_transaction_guard(cause, facts);
        if blocked {
            self.persist_if_needed(cx);
            cx.notify();
        }
        blocked
    }

    /// 共享事务控件是否正在执行一条命令（只对命令所属连接显示忙碌）。
    pub(super) fn transaction_busy(&self) -> bool {
        let active = self.model.transaction_target().map(|target| target.connection_id);
        let in_flight = self.transaction_task_connection.is_some() && self.transaction_task_connection.as_deref() == active.as_deref();
        self.model.transactions.busy || in_flight
    }

    /// 窗口关闭与“退出”动作：先处理未结束事务，再走原有草稿/暂存守卫。
    pub(super) fn allow_close(&mut self, cx: &mut Context<Self>) -> bool {
        if self.begin_transaction_guard(GuardCause::Quit, None, cx) {
            return false;
        }
        self.model.allow_system_close()
    }

    /// 默认方式为手动提交时：连接空闲就自动开启事务。同一连接同一代事务只尝试一次。
    fn auto_begin_transaction(&mut self) {
        if !self.model.settings.transaction_manual
            || self.model.transaction_request.is_some()
            || self.model.transaction_guard.is_some()
        {
            return;
        }
        let Some(target) = self.model.transaction_target() else { return };
        let Some(snapshot) = self.services.connections.transaction_snapshot(&target.connection_id) else { return };
        let explicit = self
            .services
            .connections
            .transaction_coordinator(&target.connection_id)
            .is_some_and(|coordinator| coordinator.capabilities().explicit_control);
        if !explicit || snapshot.state != crate::transaction::TransactionState::Idle || snapshot.unresolved.is_some() {
            return;
        }
        let key = (target.connection_id.clone(), snapshot.generation);
        if self.auto_begin_attempt.as_ref() == Some(&key) {
            return;
        }
        self.auto_begin_attempt = Some(key);
        self.model.request_transaction(super::transaction::TransactionCommand::Begin);
    }

    /// 取走并执行一条事务命令；结果写回共享控件，关闭保护会按结果决定是否继续。
    fn pump_transactions(&mut self, cx: &mut Context<Self>) {
        if self.transaction_task.is_some() {
            return;
        }
        self.auto_begin_transaction();
        let Some(request) = self.model.transaction_request.take() else { return };
        self.transaction_task_connection = Some(request.target.connection_id.clone());
        let services = Arc::clone(&self.services);
        let view = cx.entity().downgrade();
        self.transaction_task = Some(cx.spawn(async move |_, cx| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            let target = request.target.clone();
            let command = request.command.clone();
            jobs::runtime().spawn(async move {
                let result = super::transaction::execute(&services.connections, &target, &command).await;
                let _ = tx.send(result);
            });
            let result = rx.await.unwrap_or_else(|_| Err("事务命令没有完成".to_owned()));
            cx.update(|cx| {
                let Some(view) = view.upgrade() else { return };
                view.update(cx, |this, cx| {
                    this.transaction_task = None;
                    this.transaction_task_connection = None;
                    this.finish_transaction(request, result, cx);
                });
            });
        }));
    }

    /// 一条事务命令的结果：先写控件回执，再由关闭保护或业务页消费。
    fn finish_transaction(&mut self, request: TransactionRequest, result: Result<TransactionEffect, String>, cx: &mut Context<Self>) {
        self.model.apply_transaction_result(&request.target, &request.command, &result);
        // 事务结束后业务结果与目录代次都要更新；活动事务里的中间结果不需要重读。
        if let Ok(effect) = result {
            self.refresh_after_transaction(&request, effect, cx);
        }
        if request.guard.is_some() {
            match self.model.guard_finished(result) {
                GuardOutcome::Continue(followup) => self.after_guard(followup, cx),
                GuardOutcome::Pending => {}
                // 失败或未知：关闭已中止，保护层与共享快照一起保留在界面上。
                GuardOutcome::Stopped => {
                    self.persist_if_needed(cx);
                }
            }
        }
        cx.notify();
        self.settle(cx);
    }

    /// 所有连接都确认结束后继续原来的流程。只有确认成功才会走到这里。
    fn after_guard(&mut self, followup: GuardFollowup, cx: &mut Context<Self>) {
        match followup {
            GuardFollowup::Quit => {
                if self.model.allow_system_close() {
                    cx.quit();
                }
            }
            GuardFollowup::Update => self.perform("update-install", cx),
            GuardFollowup::Disconnect(id) => {
                if self.model.connections.selected.as_deref() == Some(id.as_str()) {
                    self.model.enqueue(PendingJob::Disconnect);
                }
            }
        }
        cx.notify();
    }

    /// 提交/回滚/隐式提交之后：同连接的表数据与结构页重读，目录刷新；未应用草稿保持原样。
    fn refresh_after_transaction(&mut self, request: &TransactionRequest, effect: TransactionEffect, cx: &mut Context<Self>) {
        if !effect.ends_transaction() {
            return;
        }
        let connection = request.target.connection_id.clone();
        self.query.update(cx, |pane, cx| pane.transaction_settled(&connection, cx));
        self.table.update(cx, |pane, cx| pane.transaction_settled(&connection, cx));
        self.structure.update(cx, |pane, cx| {
            pane.transaction_settled(&connection);
            cx.notify();
        });
        self.model.request_nav_refresh(Some(connection.clone()));
        self.pump_nav(cx);
    }

    /// 轮询与命令之后的兜底：共享快照修订一变，三个业务页都按同一份状态重绘。
    fn sync_transaction_views(&mut self, cx: &mut Context<Self>) -> bool {
        let mut ids: std::collections::BTreeSet<String> = self.model.connections.rows.iter().map(|row| row.saved.id.clone()).collect();
        ids.extend(self.model.tabs.iter().filter_map(|tab| tab.connection_id.clone()));
        let mut changed = false;
        for id in ids {
            match self.services.connections.transaction_snapshot(&id) {
                Some(snapshot) => {
                    if self.transaction_revisions.get(&id) != Some(&snapshot.revision) {
                        self.transaction_revisions.insert(id, snapshot.revision);
                        changed = true;
                    }
                }
                None => changed |= self.transaction_revisions.remove(&id).is_some(),
            }
        }
        if changed {
            self.query.update(cx, |_, cx| cx.notify());
            self.table.update(cx, |_, cx| cx.notify());
            self.structure.update(cx, |_, cx| cx.notify());
        }
        changed
    }

    fn refresh_tree_on_connect(&mut self, cx: &mut Context<Self>) {        let connected: std::collections::BTreeSet<String> = self
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
                // 恢复出来的表标签可能在连接打开前读过一次并失败：连上后自动重读。
                let id = id.clone();
                self.table.update(cx, |pane, cx| {
                    pane.retry_after_connect(&id, cx);
                });
                self.structure.update(cx, |pane, cx| pane.retry_after_connect(&id, cx));
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
                                match next {
                                    LinkStatus::Connected => {
                                        view.model.connections.failures.remove(&status.id);
                                    }
                                    LinkStatus::Failed => {
                                        if let Some(reason) = status.reason.clone().filter(|reason| !reason.trim().is_empty()) {
                                            view.model.connections.failures.insert(status.id.clone(), reason);
                                        }
                                    }
                                    _ => {}
                                }
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
                    // 关窗判断前先按最新的子页面状态整理一次（导出、对比、网格暂存都在里面）。
                    this.settle(cx);
                    this.refresh_exchange(cx);
                    slot.set(this.allow_close(cx));
                    cx.notify();                });
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
                            let mut shell_settings = startup.state.shell;
                            if shell_settings.object_page == 100 {
                                // 旧版本默认值为 100，升级到 500，避免大库每 100 张表就截断一次。
                                shell_settings.object_page = 500;
                            }
                            this.model.apply_settings(&shell_settings);
                            // 保存的主题（含深色）要在启动时就写进组件库主题，否则重启后仍是浅色色板。
                            this.apply_effect(Effect::ApplyTheme, cx);
                            // 启动时的自动更新检查只走这个开关；关闭时不发起检查。
                            this.schedule_auto_update(cx);
                            this.model.restore_session(&startup.state.session);
                            this.model.column_templates = startup.state.column_templates.clone();
                            let templates = startup.state.column_templates.clone();
                            this.structure.update(cx, |pane, _| pane.set_templates(templates));
                            if let Some(key) = startup.warning_key {
                                this.model.notice_key = Some(key);
                                this.model.startup_warning = Some(startup.warning_detail);
                            }
                            if this.model.viewport_width <= 960 {
                                this.model.shell.sidebar_open = false;
                            }
                            let _ = this.model.dispatch("conn-refresh");
                            // 分组与折叠状态跟着配置库走：重启后回到离开时的分组结构。
                            let _ = this.model.dispatch("group-refresh");
                            let _ = this.model.dispatch("history-refresh");
                            let _ = this.model.dispatch("nav-refresh");
                            this.load_env_tags(cx);
                            this.persist_if_needed(cx);
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
            // 分组用自己的任务板：分组的读写不能顶掉正在读取的连接列表。
            PendingJob::RefreshGroups | PendingJob::GroupOp => "groups",
            PendingJob::RetryVaultMigration | PendingJob::MigrateAllLegacy | PendingJob::DismissLegacy => "vault",
            PendingJob::ReadProfiles(..) | PendingJob::SaveProfiles(_) => "profile-import",
            _ => "connections",
        };
        let token = if owner == "connections" {
            self.model.prepare_list_token(&self.services.tasks)
        } else {
            self.model.begin_owner(&self.services.tasks, owner)
        };
        let request = job_request(&self.model, job.clone(), token);
        if matches!(job, PendingJob::SaveNamed) {
            self.model.preview_save = None;
        }
        let services = Arc::clone(&self.services);
        let rx = jobs::spawn_job(services, request);
        self.receive_job(owner, rx, cx);
    }

    pub(super) fn receive_job(&mut self, owner: &'static str, rx: tokio::sync::oneshot::Receiver<jobs::JobDone>, cx: &mut Context<Self>) {
        let view = cx.entity().downgrade();
        self.job_tasks.retain(|task| !task.is_ready());
        self.job_tasks.push(cx.spawn(async move |_, cx| {
            let done = rx.await;
            cx.update(|cx| {
                if let Some(view) = view.upgrade() {
                    view.update(cx, |this, cx| {
                        if let Ok(done) = done {
                            apply_done(&mut this.model, &this.services, owner, done);
                            if owner == "profile-import" {
                                this.pump_jobs(cx);
                            }
                        }
                        this.model.finish_job();
                        this.persist_if_needed(cx);
                        cx.notify();
                    })
                    ;
                }
            });
        }));
    }

    pub(super) fn persist_if_needed(&mut self, cx: &mut Context<Self>) {
        let state = self.model.snapshot();
        let blob = serde_json::to_string(&state).unwrap_or_default();
        if blob == self.last_state {
            return;
        }
        // 无关命令的新快照包含尚未保存的设置。接管写盘时也必须接管回执，
        // 不能让旧代次被跳过的 Ok(()) 提前把界面标为“已保存”。
        if self.model.settings_save == SaveState::Saving {
            self.persist_settings(cx);
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

    /// 设置改动的写盘路径：与 persist_if_needed 同一条去重、代次与闸门，
    /// 区别是把结果写回模型——设置页和状态栏都要能把“没保存成功”说清楚。
    /// last_state 仅用于合并已排队的快照；显式设置保存/重试必须取得真正的写盘回执。
    pub(super) fn persist_settings(&mut self, cx: &mut Context<Self>) {
        let state = self.model.snapshot();
        let blob = serde_json::to_string(&state).unwrap_or_default();
        self.last_state = blob.clone();
        self.model.settings_save = SaveState::Saving;
        let generation = self.revision.fetch_add(1, Ordering::SeqCst) + 1;
        let revision = Arc::clone(&self.revision);
        let gate = Arc::clone(&self.persist_gate);
        let services = Arc::clone(&self.services);
        let view = cx.entity().downgrade();
        // 写盘走后台 tokio 运行时（存储层用 spawn_blocking），结果再回到界面线程。
        let (tx, rx) = tokio::sync::oneshot::channel();
        jobs::runtime().spawn(async move {
            let result = jobs::persist_revision(&services, &state, &revision, generation, &gate).await;
            let _ = tx.send(result);
        });
        self.settings_task = Some(cx.spawn(async move |_, cx| {
            let result = rx.await;
            cx.update(|cx| {
                let Some(view) = view.upgrade() else { return };
                view.update(cx, |this, cx| {
                    this.apply_persist_settings(result, generation, blob);
                    cx.notify();
                });
            });
        }));
        cx.notify();
    }

    /// 一次设置写盘回到界面：只有仍是最新一次写入的结果才算设置页的答案。
    /// 被更晚的写入接管时（那份快照含同样的设置改动，成功与否由它自己报），
    /// 旧的失败不能盖掉新的结果——否则一次已经成功的写盘还会显示"保存失败"。
    pub(super) fn apply_persist_settings(
        &mut self,
        result: Result<Result<(), String>, tokio::sync::oneshot::error::RecvError>,
        generation: u64,
        blob: String,
    ) {
        let newest = self.revision.load(Ordering::SeqCst) == generation;
        if !newest {
            return;
        }
        match result {
            Ok(Ok(())) => {
                // 当前代次拿到 Ok 才说明写盘完成；被接管的请求由新回执负责。
                self.model.settings_save = SaveState::Saved;
                self.last_state = blob;
            }
            Ok(Err(detail)) => {
                self.model.settings_save = SaveState::Failed { detail };
            }
            // 通道被丢弃说明这次写入已经作废：保持原状态，不假装成功。
            Err(_) => {}
        }
    }

    /// 当前写盘代次：设置页的用例要把"最新"和"被接管"两种结果分开验。
    #[cfg(test)]
    pub(super) fn publish_generation(&self) -> u64 {
        self.revision.load(Ordering::SeqCst)
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

    fn prompt_profile_import(&mut self, format: super::conn_import::ProfileFormat, cx: &mut Context<Self>) {
        use super::conn_import::ImportState;
        self.model.profile_import = ImportState::Picking;
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(self.t(format.entry())),
        });
        cx.spawn(async move |this, cx| {
            let result = receiver.await;
            cx.update(|cx| {
                let _ = this.update(cx, |view, cx| {
                    match result {
                        Ok(Ok(Some(paths))) => {
                            if let Some(path) = paths.into_iter().next() {
                                view.model.queue_profile_preview(format, path);
                                view.pump_jobs(cx);
                            } else {
                                view.model.profile_import = ImportState::Idle;
                            }
                        }
                        Ok(Err(error)) => view.model.profile_import = ImportState::Error(error.to_string()),
                        _ => view.model.profile_import = ImportState::Idle,
                    }
                    cx.notify();
                });
            });
        })
        .detach();
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

    fn prompt_open_files(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some(SharedString::from(self.model.text("shell.command.openFile"))),
        });
        cx.spawn(async move |this, cx| {
            let message = receiver.await;
            cx.update(|cx| {
                let _ = this.update(cx, |view, cx| view.finish_open_prompt(message, cx));
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
                let raw = error.to_string();
                self.model.notice_key = Some("connections.page.importRillSql");
                self.model.notice_detail = humanize_dialog_error(&raw, self.model.locale());
            }
            Err(error) => {
                self.model.notice_key = Some("connections.page.importRillSql");
                self.model.notice_detail = error.to_string();
            }
        }
        self.persist_if_needed(cx);
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
                let raw = error.to_string();
                self.model.notice_key = Some("connections.page.exportButton");
                self.model.notice_detail = humanize_dialog_error(&raw, self.model.locale());
            }
            Err(error) => {
                self.model.notice_key = Some("connections.page.exportButton");
                self.model.notice_detail = error.to_string();
            }
        }
        self.persist_if_needed(cx);
        cx.notify();
    }

    pub(super) fn finish_open_prompt<E: std::fmt::Display>(
        &mut self,
        message: Result<Result<Option<Vec<std::path::PathBuf>>, E>, futures::channel::oneshot::Canceled>,
        cx: &mut Context<Self>,
    ) {
        match message {
            Ok(Ok(Some(paths))) => {
                let mut accepted = Vec::with_capacity(paths.len());
                for path in paths {
                    match crate::app::accept_open_path(&path) {
                        Ok(path) => accepted.push(path),
                        Err(error) => {
                            self.model.notice_key = Some("connections.validate.chooseFile");
                            self.model.notice_detail = error;
                        }
                    }
                }
                if !accepted.is_empty() {
                    self.open_dropped_or_picked_paths(&accepted, false, cx);
                }
            }
            Ok(Ok(None)) => {}
            Ok(Err(error)) => {
                let raw = error.to_string();
                if is_portal_dialog_error(&raw) {
                    // 无桌面 Portal（容器、远程 X11/Wayland）时原生对话框不可用：
                    // 直接打开 SQLite 连接表单并把光标放到文件路径框，让用户能继续。
                    let origin = self.model.shell.focus;
                    self.model.open_connection_modal(
                        Some(crate::adapters::registry::LOCAL_FILE),
                        origin,
                        None,
                        "open-file",
                    );
                    self.model.connections.issues.push(crate::app::views::connections::FieldIssue::new(
                        "file_path",
                        "ui.sqlite.portalFallback",
                    ));
                    self.model.conn_modal.focus_first_pending = true;
                    self.model.notice_key = Some("ui.sqlite.portalFallback");
                    self.model.notice_detail.clear();
                } else {
                    self.model.notice_key = Some("connections.validate.chooseFile");
                    self.model.notice_detail = raw;
                }
            }
            Err(error) => {
                self.model.notice_key = Some("connections.validate.chooseFile");
                self.model.notice_detail = error.to_string();
            }
        }
        self.persist_if_needed(cx);
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

    pub(super) fn active_query_context(&self) -> bool {
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
                    // 查询结果没有可重读的表：网格不会借 SQL 筛选重跑语句，导出口径就是这些已读取行。
                    condition: Default::default(),
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
                        condition: Default::default(),
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
                    // 全表导出带的就是这一份已应用条件，导出与分页、计数同口径。
                    condition: self.table.read(cx).exchange_condition(),
                }
            }
        }
    }

    pub(super) fn accept_exchange_drop(&mut self, paths: &gpui_kit::ExternalPaths, cx: &mut Context<Self>) {
        self.open_dropped_or_picked_paths(paths.paths(), true, cx);
    }

    pub(super) fn open_dropped_or_picked_paths(
        &mut self,
        raw_paths: &[std::path::PathBuf],
        allow_exchange_import: bool,
        cx: &mut Context<Self>,
    ) {
        let mut sql_paths = Vec::new();
        let mut exchange_opened = false;
        for raw in raw_paths {
            let Ok(path) = crate::app::accept_drop_path(raw) else { continue };
            // 文件头优先于扩展名：认出数据库文件就建连接，.sql 进编辑器。
            let header = crate::app::read_file_header(&path);
            match crate::app::route_open_path(&path, &header) {
                crate::app::OpenRoute::SqlText => sql_paths.push(path),
                crate::app::OpenRoute::Database(engine) => self.apply_database_path(path, engine),
                // 拖进来的表格文件（CSV、JSON、Excel）走导入预览；其余认不出的文件与“打开文件”一样问用户怎么打开。
                crate::app::OpenRoute::Ask if allow_exchange_import && crate::app::is_drop_import_path(&path) => {
                    exchange_opened = true;
                    self.exchange.update(cx, |pane, cx| {
                        pane.open = true;
                        pane.preview_file(path, crate::export::ImportEntry::Drop, cx);
                    });
                }
                crate::app::OpenRoute::Ask => self.model.open_choices.push_back(path),
            }
        }
        if !sql_paths.is_empty() {
            self.open_sql_paths(sql_paths, cx);
        }
        if exchange_opened {
            self.model.exchange_open = true;
            self.refresh_exchange(cx);
        }
        self.persist_if_needed(cx);
        cx.notify();
    }

    fn open_sql_paths(&mut self, paths: Vec<std::path::PathBuf>, cx: &mut Context<Self>) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        jobs::runtime().spawn(async move {
            let mut results = Vec::with_capacity(paths.len());
            for path in paths {
                let p = path.clone();
                let loaded = crate::application::run_blocking(move || crate::query::assist::read_sql_file(&p)).await;
                results.push((path, loaded));
            }
            let _ = tx.send(results);
        });
        cx.spawn(async move |this, cx| {
            let Ok(results) = rx.await else { return };
            cx.update(|cx| {
                let _ = this.update(cx, |view, cx| {
                    for (path, loaded) in results {
                        match loaded {
                            Ok(file) => {
                                view.model.shell.note_opened(path.clone());
                                view.model.open_sql_file(&path, file.sql);
                                if let Some(tab_id) = view.model.active_tab.clone() {
                                    view.query.update(cx, |pane, _| pane.set_tab_file_path(&tab_id, path.clone()));
                                }
                                view.query.update(cx, |pane, _| pane.request_focus());
                            }
                            Err(error) => {
                                view.model.notice_key = Some("query.fileIo.open");
                                view.model.notice_detail = error;
                            }
                        }
                    }
                    view.persist_if_needed(cx);
                    cx.notify();
                });
            });
        })
        .detach();
    }

    /// R6：拖动经过一行时记下指针在上半还是下半行；只在变化时重绘。
    pub(super) fn track_drop_half(
        &mut self,
        key: &str,
        bounds: gpui_kit::Bounds<gpui_kit::Pixels>,
        position: gpui_kit::Point<gpui_kit::Pixels>,
        cx: &mut Context<Self>,
    ) {
        if !bounds.contains(&position) {
            return;
        }
        let after = drop_lower_half(f32::from(bounds.top()), f32::from(bounds.size.height), f32::from(position.y));
        let next = after.then(|| key.to_owned());
        if self.drop_after != next {
            self.drop_after = next;
            cx.notify();
        }
    }

    /// 放手时取出“插到这一行之后”的判断，并清掉悬停状态。
    pub(super) fn take_drop_after(&mut self, key: &str) -> bool {
        self.drop_after.take().as_deref() == Some(key)
    }

    /// R6：把模型里已经生效的手动顺序写进配置库。失败时报告原因并从配置库重读，恢复原顺序。
    fn pump_reorder(&mut self, cx: &mut Context<Self>) {
        let Some(op) = self.model.take_pending_reorder() else { return };
        // 入队在界面线程上完成，顺序就是用户操作的顺序；后台任务的启动顺序不影响提交顺序。
        if let Ok(mut queue) = self.reorder_queue.lock() {
            queue.push_back(op);
        }
        let services = Arc::clone(&self.services);
        let queue = Arc::clone(&self.reorder_queue);
        let gate = Arc::clone(&self.reorder_gate);
        let view = cx.entity().downgrade();
        let (tx, rx) = tokio::sync::oneshot::channel();
        jobs::runtime().spawn(async move {
            let _guard = gate.lock().await;
            let batch: Vec<_> = queue.lock().map(|mut queue| queue.drain(..).collect()).unwrap_or_default();
            let mut failure = None;
            for op in super::model::coalesce_reorders(batch) {
                let result = match op {
                    super::model::ReorderOp::Connections { group, ids } => services.connections.reorder_connections(group.as_deref(), &ids).await,
                    super::model::ReorderOp::Groups { ids } => services.connections.reorder_groups(&ids).await,
                };
                if let Err(detail) = result {
                    failure.get_or_insert(detail);
                }
            }
            let _ = tx.send(failure);
        });
        cx.spawn(async move |_, cx| {
            if let Ok(Some(detail)) = rx.await {
                cx.update(|cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |this, cx| {
                            this.model.notice_key = Some("ui.group.reorderFailed");
                            this.model.notice_detail = detail;
                            let _ = this.model.dispatch("group-refresh");
                            let _ = this.model.dispatch("conn-refresh");
                            this.settle(cx);
                            cx.notify();
                        });
                    }
                });
            }
        })
        .detach();
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
                            this.model.apply_env_tags(tags);
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
        let engine = row.map(|row| row.saved.engine.as_str().to_owned()).unwrap_or_else(|| crate::adapters::registry::DEFAULT_ENGINE.as_str().to_owned());
        let env = row.map(|row| row.saved.env.clone()).unwrap_or_default();
        let spec = ObjectTarget::new(open.connection_id, name, open.database, open.schema, open.table, engine, env).spec();
        if open.structure {
            self.model.open_item(WorkItemKind::Structure, spec);
        } else {
            self.model.open_item(WorkItemKind::Table, spec);
        }
    }

    fn note_admin_link(&mut self, cx: &mut Context<Self>) {
        // 管理页空态（以及表头）的「立即连接」只表达意图：这里取走请求，接到树/连接行同一个
        // 按需连接入口（queue_connect → PendingJob::OpenConnection）；连上后回到下面的
        // note_connection，管理页自己重读清单。请求只消费一次，不在这里另开连接路径。
        if let Some(id) = self.admin.update(cx, |pane, _| pane.take_connect_request()) {
            self.model.queue_connect(super::conn_modal::ConnectIntent::new(id, super::conn_modal::ConnectAction::Expand, None));
        }
        // 表数据页的“重试”在连接没打开时同样走按需连接；连上后 refresh_tree_on_connect 触发重读。
        // 查询页目标选择器里点了“连接”：同一条按需连接入口。
        if let Some(id) = self.query.update(cx, |pane, _| pane.take_connect_request()) {
            self.model.queue_connect(super::conn_modal::ConnectIntent::new(id, super::conn_modal::ConnectAction::ConnectOnly, None));
        }
        if let Some(id) = self.structure.update(cx, |pane, _| pane.take_connect_request()) {
            self.model.queue_connect(super::conn_modal::ConnectIntent::new(id, super::conn_modal::ConnectAction::Expand, None));
        }
        if let Some(id) = self.table.update(cx, |pane, _| pane.take_connect_request()) {
            self.model.queue_connect(super::conn_modal::ConnectIntent::new(id, super::conn_modal::ConnectAction::Expand, None));
        }
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

    fn apply_database_path(&mut self, path: std::path::PathBuf, engine: crate::catalog::Engine) {
        self.model.shell.note_opened(path.clone());
        let mut draft = blank_draft(engine);
        draft.file_path = path.display().to_string();
        draft.name = path.file_stem().and_then(|name| name.to_str()).unwrap_or(engine.as_str()).to_owned();
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
    /// 只构建元素。同步、补拉、按需读取都在 `settle` 里，由事件（自身通知、子视图通知、
    /// 窗口尺寸变化、后台回执）触发；Spinner 动画帧让本视图重绘时不再重复这些工作。
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        self.render_count.set(self.render_count.get() + 1);
        // 一帧画完，下一批事件重新计数；见 `settle_from_event`。
        self.settle_passes = 0;
        self.settle_deferred = false;
        // 唯一的例外：对比页的交接（放入查询页、返回、运行标记）在这里也收一次。只有两次 take 和一次
        // 布尔读取，不遍历、不分配；对比页的按钮直接改它自己的实体，嵌套更新里观察回调还没轮到时，
        // 下一帧仍能拿到交接，关闭窗口的判断也不会读到旧的运行标记。
        self.refresh_compare(cx);
        let root = self.render_root(window, cx);
        // 对象编辑器覆盖在当前页面上：它自己是一层模态，不替换下面的工作项。
        if self.object_editor.read(cx).is_open() {
            let editor = self.object_editor.clone();
            return div().relative().size_full().child(root).child(editor).into_any_element();
        }
        root
    }
}

/// 一次事件波及里最多连续整理几遍：整理本身改了状态会再通知自己，通常两遍内收敛；
/// 超过就停，避免某个同步函数每次都通知自己时空转。下一帧或稍后的事件会重新计数。
pub(super) const SETTLE_PASSES: u32 = 4;

impl Workbench {
    /// 自身被通知（含子视图通知转来的）时整理一次状态。连续整理有上限，见 `SETTLE_PASSES`。
    pub(super) fn settle_from_event(&mut self, cx: &mut Context<Self>) {
        let now = std::time::Instant::now();
        if now.duration_since(self.settle_burst) > std::time::Duration::from_millis(50) {
            self.settle_passes = 0;
            self.settle_deferred = false;
        }
        if self.settle_passes == 0 {
            self.settle_burst = now;
        }
        if self.settle_passes >= SETTLE_PASSES {
            // 到了上限不直接丢：后面的通知可能带着新回执或新请求。补排一次延后整理，
            // 每帧最多一次，避免某个同步函数每次都通知自己时空转。
            if !self.settle_deferred {
                self.settle_deferred = true;
                let this = cx.entity().downgrade();
                cx.defer(move |cx| {
                    if let Some(this) = this.upgrade() {
                        this.update(cx, |this, cx| {
                            #[cfg(test)]
                            this.deferred_settle_count.set(this.deferred_settle_count.get() + 1);
                            // 延后整理可能不产生新帧（没改东西或窗口被遮挡），在这里就放开，不等 render。
                            this.settle_deferred = false;
                            this.settle(cx);
                        });
                    }
                });
            }
            return;
        }
        self.settle_passes += 1;
        self.settle(cx);
    }

    /// 原来每帧在 render 里做的整理：补拉后台回执、下发设置、同步查询标签与目录、按需打开工具页。
    /// 现在只在事件到来时执行一次；render 只读这些结果构建元素。
    pub(crate) fn settle(&mut self, cx: &mut Context<Self>) {
        // 整理过程中派发的命令（例如连接建好后续接的动作）不再嵌套整理，外层这一遍做完即可。
        if self.settling {
            return;
        }
        self.settling = true;
        self.settle_body(cx);
        self.settling = false;
    }

    fn settle_body(&mut self, cx: &mut Context<Self>) {
        #[cfg(test)]
        self.settle_count.set(self.settle_count.get() + 1);
        self.boot_state(cx);
        self.monitor_connections(cx);
        self.pump_jobs(cx);
        self.pump_reorder(cx);
        self.pump_transactions(cx);
        self.sync_transaction_views(cx);
        self.pump_nav(cx);
        self.pump_danger(cx);
        self.pump_update(cx);
        self.pump_tag_job(cx);
        self.sync_prod_targets(cx);
        self.flush_clipboard(cx);
        // 结构页删除、改名或修改结构成功后，对象树、工作项与网格列结构缓存同步更新。
        if let Some((target, is_view)) = self.structure.update(cx, |pane, _| pane.take_dropped_object()) {
            let kind = if is_view { crate::catalog::model::ObjectKind::View } else { crate::catalog::model::ObjectKind::Table };
            self.model.object_dropped(&target.connection_id, &target.database, &target.schema, &target.table, kind);
            self.table.update(cx, |pane, _| pane.invalidate_schema_cache(&target));
            self.applied_tool = None;
            self.pump_nav(cx);
        }
        if let Some((old_target, new_name)) = self.structure.update(cx, |pane, _| pane.take_renamed()) {
            self.model.table_renamed(&old_target.connection_id, &old_target.database, &old_target.schema, &old_target.table, &new_name);
            self.table.update(cx, |pane, _| pane.invalidate_schema_cache(&old_target));
            if !self.table.read(cx).uncommitted() {
                self.table_view_spec = None;
            }
            self.applied_tool = None;
            self.pump_nav(cx);
        }
        if let Some(target) = self.structure.update(cx, |pane, _| pane.take_created()) {
            self.adopt_created_table(target);
            self.pump_nav(cx);
        }
        if let Some((target, is_view)) = self.structure.update(cx, |pane, _| pane.take_modified()) {
            self.model.table_structure_changed(&target.connection_id, &target.database, &target.schema, &target.table, is_view);
            self.table.update(cx, |pane, _| pane.invalidate_schema_cache(&target));
            if !self.table.read(cx).uncommitted() {
                self.table_view_spec = None;
            }
            self.pump_nav(cx);
        }
        self.consume_object_editor(cx);
        self.sync_object_editor_target(cx);
        self.sync_view_settings(cx);
        self.sync_density(cx);
        self.sync_query(cx);
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
            self.object_editor.update(cx, |pane, cx| {
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
        self.exchange.update(cx, |pane, _| pane.attach_owner(owner.clone()));
        // 查询工具条的「运行」同样只能走 Workbench::perform 这份门。
        self.query.update(cx, |pane, _| pane.attach_owner(owner));
        self.ensure_tool(cx);
        self.ensure_redis(cx);
        self.ensure_mongo(cx);
        self.ensure_overview(cx);
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
    }
}

/// 页签条的缓存视图。内容仍由工作台构建（状态和命令都在工作台），这里只提供一个独立的实体边界：
/// 用 `.cached()` 挂进工作台后，只有它自己被通知（工作台被通知时转告、悬停变化）才重建。
/// 缓存视图在 prepaint 阶段才画，工作台的 render 已经结束，可以在这里借用工作台。
pub(super) struct TabStripHost {
    workbench: gpui_kit::WeakEntity<Workbench>,
}

impl Render for TabStripHost {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        match self.workbench.upgrade() {
            Some(workbench) => workbench.update(cx, |workbench, cx| workbench.tab_strip(window, cx)),
            None => gpui_kit::div().into_any_element(),
        }
    }
}

/// 作废的目录读取回来时要不要收起节点的加载态。
/// `ours_aborted`：这次读取被取消了；`other_live`：节点上登记着一个仍在途的读取。
/// 没被取消（只是连接版本变了）时节点登记的就是这次读取自己，收起；
/// 被同一节点更新的读取顶掉时，新读取还在途，加载态留给它收尾。
pub(super) fn nav_read_drop_clears(ours_aborted: bool, other_live: bool) -> bool {
    !ours_aborted || !other_live
}

/// 启动后多久做第一次自动更新检查。
const AUTO_UPDATE_DELAY: std::time::Duration = std::time::Duration::from_secs(20);

/// 下载线程发回界面的消息。
enum UpdateMessage {
    Progress(u64, Option<u64>),
    Downloaded(Box<Result<crate::release::check::VerifiedPackage, String>>),
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
    // 网格自己的命令统一以 grid- 开头（含右键菜单里的 grid-column:*）：全部交给活动网格。
    id.starts_with("grid-") || matches!(
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
            | "toggle-comment"
            | "toggle-block-comment"
            | "format"
            | "autocomplete"
            | "explain"
            | "expand-star"
            | "toggle-outline"
            | "toggle-side"
            | "close-side"
            | "switch-pane-focus"
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
        // 结果区的全屏、收起、关闭与复制（结果区菜单与页签头按钮）。
        || id.starts_with("result-")
        // 查询页把自己的运行类命令（含大纲里的单条执行）交回外壳统一按需连接；
        // 外壳必须认得同一份清单再转发回去，否则这类命令会在外壳这一层被静默丢掉。
        || crate::app::views::query::runs_query(id)
        || id.starts_with("outline:")
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

/// 识别无桌面 Portal / D-Bus 时的原生文件框失败，避免把底层 zbus 报错原样抛给用户。
pub(super) fn is_portal_dialog_error(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("portal request failed")
        || lower.contains("org.freedesktop.portal")
        || lower.contains("org.freedesktop.dbus.error.serviceunknown")
        || lower.contains("zbus")
}

pub(super) fn humanize_dialog_error(error: &str, locale: crate::i18n::Locale) -> String {
    if is_portal_dialog_error(error) {
        crate::i18n::text(locale, "ui.filePicker.portalUnavailable")
    } else {
        error.to_owned()
    }
}
