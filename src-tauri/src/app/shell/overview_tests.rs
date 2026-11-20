//! 对象概览（R3）：预览标签的打开/就地替换/固定、按需连接、读取回填、双击与右键，以及真实 SQLite 会话上的整条路径。

use std::sync::Arc;

use gpui_kit::test::TestWindowExt;
use gpui_kit::AppContext as _;

use crate::app::views::connections::{sample_draft, LinkStatus};
use crate::app::views::overview::{OverviewColumn, OverviewLoad, OverviewTarget};
use crate::app::Shell;
use crate::application::AppServices;
use crate::catalog::hierarchy::{connection_node_id, database_node_id, object_node_id, schema_node_id};
use crate::catalog::model::{ConnStatus, DatabaseNode, ObjectKind, ObjectRow, SchemaNode};
use crate::catalog::{NavConnection, StatRow, StatsBody, StatsLevel};
use crate::connections::model::Engine;
use crate::connections::{ConnectionRecord, Service};

use super::model::{PendingJob, WorkItemKind, WorkTool, WorkbenchModel};

fn record(id: &str, name: &str, engine: Engine) -> ConnectionRecord {
    let mut draft = sample_draft(engine);
    draft.name = name.into();
    ConnectionRecord { saved: crate::connections::model::normalize(&draft, id.into()).unwrap(), ssh_host_key: None }
}

fn database(name: &str, schema: &str, objects: Vec<ObjectRow>) -> DatabaseNode {
    let total = objects.len();
    DatabaseNode {
        name: name.to_owned(),
        schemas: vec![SchemaNode { name: schema.to_owned(), objects, total, error: None, kind_reports: Vec::new() }],
        error: None,
    }
}

/// 已连接的 MySQL（shop、crm 两个库）、PostgreSQL（shop.public）与 Redis。
fn model() -> WorkbenchModel {
    let mut model = WorkbenchModel::new(Shell::default());
    model.connections.rows = vec![record("my", "本地 MySQL", Engine::Mysql), record("pg", "Atlas", Engine::Postgres), record("rd", "缓存", Engine::Redis)];
    for id in ["my", "pg", "rd"] {
        model.connections.status.insert(id.to_owned(), LinkStatus::Connected);
    }
    let mut mysql = NavConnection::new("my", "本地 MySQL", Engine::Mysql, ConnStatus::Connected);
    mysql.databases = vec![database("shop", "", vec![ObjectRow::table("orders", Some(3))]), database("crm", "", Vec::new())];
    let mut pg = NavConnection::new("pg", "Atlas", Engine::Postgres, ConnStatus::Connected);
    pg.databases = vec![database("shop", "public", vec![ObjectRow::table("orders", Some(3))])];
    assert!(model.apply_navigation(&[mysql, pg], false));
    model
}

fn body(level: StatsLevel, rows: Vec<StatRow>, missing: Option<&str>) -> StatsBody {
    StatsBody { level, rows, missing: missing.map(str::to_owned) }
}

#[test]
fn clicking_a_connection_opens_a_preview_that_later_clicks_replace_in_place_and_double_click_pins() {
    let mut model = model();
    assert!(model.show_welcome());
    assert!(model.overview_click_connection("my", false));
    assert!(!model.show_welcome(), "单击连接后主区是概览，不再是启动页");
    let tab = model.active_tab().cloned().expect("概览标签");
    assert_eq!(tab.kind, WorkItemKind::Overview);
    assert_eq!(model.tool, WorkTool::Overview);
    assert_eq!(tab.title, "本地 MySQL");
    assert!(model.is_preview_tab(&tab.id));
    assert_eq!(model.active_overview().map(|state| state.level()), Some(StatsLevel::Databases));

    // 单击库：就地替换同一个预览标签，不新开。
    let shop = database_node_id("my", "shop");
    assert!(model.overview_click_node(&shop, false));
    assert_eq!(model.tabs.len(), 1);
    let replaced = model.active_tab().cloned().unwrap();
    assert_eq!(replaced.id, tab.id);
    assert_eq!(replaced.title, "shop");
    assert_eq!(replaced.database.as_deref(), Some("shop"));
    assert_ne!(replaced.epoch, tab.epoch, "换了目标就是新实例");
    assert_eq!(model.active_overview().map(|state| state.level()), Some(StatsLevel::Objects));
    assert_eq!(model.overviews.len(), 1, "被替换的范围不再保留读取结果");

    // 双击固定；之后单击别的库新开一个预览标签。
    assert!(model.overview_click_node(&shop, true));
    assert!(model.preview_tab.is_none());
    assert!(model.overview_click_node(&database_node_id("my", "crm"), false));
    assert_eq!(model.tabs.len(), 2);
    let crm = model.active_tab().cloned().unwrap();
    assert!(model.is_preview_tab(&crm.id));
    // 已有固定标签的范围直接切过去，预览标签不动。
    assert!(model.overview_click_node(&shop, false));
    assert_eq!(model.active_tab.as_deref(), Some(tab.id.as_str()));
    assert_eq!(model.tabs.len(), 2);
    assert!(model.is_preview_tab(&crm.id));

    // 关掉预览标签后，下一次单击新开。
    model.request_close_tab(&crm.id);
    model.sync_overviews();
    assert!(model.preview_tab.is_none());
    assert!(!model.overviews.keys().any(|spec| spec.contains("|crm|")));
}

#[test]
fn sorting_filtering_or_opening_an_object_pins_the_preview() {
    let mut model = model();
    model.overview_click_node(&database_node_id("my", "shop"), false);
    model.dispatch("overview-sort:size");
    assert!(model.preview_tab.is_none(), "排序是在概览里操作，转为固定");
    assert_eq!(model.active_overview().map(|state| (state.sort, state.descending)), Some((OverviewColumn::Size, true)));

    model.overview_click_node(&database_node_id("my", "crm"), false);
    assert!(model.preview_tab.is_some());
    model.set_field(super::model::DraftField::OverviewFilter, "ord".to_owned());
    assert!(model.preview_tab.is_none(), "过滤同样转为固定");
    assert_eq!(model.field_value(super::model::DraftField::OverviewFilter), "ord");
}

#[test]
fn connected_targets_read_once_and_results_fill_the_right_tab_with_missing_stats_marked() {
    let mut model = model();
    model.overview_click_connection("my", false);
    let (target, generation) = model.overview_read_request().expect("已连接就读");
    assert_eq!(target.level(), StatsLevel::Databases);
    assert!(model.overview_read_request().is_none(), "读取中不重复发起");
    assert_eq!(model.active_overview().map(|state| state.load.clone()), Some(OverviewLoad::Loading));
    let rows = vec![
        StatRow { tables: Some(12), size: Some(1 << 20), ..StatRow::named("shop", None) },
        StatRow::named("crm", None),
    ];
    assert!(model.finish_overview_read(&target.spec(), generation, Ok(body(StatsLevel::Databases, rows, None))));
    let state = model.active_overview().unwrap();
    assert_eq!(state.load, OverviewLoad::Ready);
    assert_eq!(state.visible().iter().map(|row| row.name.as_str()).collect::<Vec<_>>(), ["crm", "shop"]);

    // 双击库行：进入下一层（新的预览标签），当前连接概览固定下来。
    let connection_tab = model.active_tab.clone().unwrap();
    model.dispatch("overview-activate:shop");
    assert!(!model.is_preview_tab(&connection_tab));
    assert_eq!(model.active_overview().map(|state| state.target.database.clone()), Some("shop".to_owned()));
    let (objects, generation) = model.overview_read_request().unwrap();
    // 统计读不到：仍显示名称，并带上缺失原因。
    let rows = vec![StatRow::named("orders", Some(ObjectKind::Table)), StatRow::named("paid", Some(ObjectKind::View))];
    assert!(model.finish_overview_read(&objects.spec(), generation, Ok(body(StatsLevel::Objects, rows, Some("denied")))));
    let state = model.active_overview().unwrap();
    assert_eq!(state.missing.as_deref(), Some("denied"));
    assert_eq!(state.rows.len(), 2);

    // 双击表行打开表数据，规格指向同一个库与表。
    model.dispatch("overview-activate:orders");
    let tab = model.active_tab().cloned().unwrap();
    assert_eq!(tab.kind, WorkItemKind::Table);
    let target = crate::app::model::ObjectTarget::parse(tab.spec.as_deref().unwrap()).unwrap();
    assert_eq!((target.connection_id.as_str(), target.database.as_str(), target.name.as_str()), ("my", "shop", "orders"));

    // 迟到的旧结果（刷新之后）不覆盖新状态。
    let spec = objects.spec();
    let _ = model.activate_item(&model.tabs.iter().find(|tab| tab.spec.as_deref() == Some(spec.as_str())).unwrap().id.clone());
    model.dispatch("overview-refresh");
    assert!(!model.finish_overview_read(&spec, generation, Ok(body(StatsLevel::Objects, Vec::new(), None))));
    assert_eq!(model.active_overview().map(|state| state.load.clone()), Some(OverviewLoad::Pending));
}

#[test]
fn a_disconnected_connection_connects_on_demand_before_reading() {
    let mut model = model();
    model.connections.status.remove("my");
    model.overview_click_connection("my", false);
    assert!(model.overview_read_request().is_none(), "没连上不读");
    assert_eq!(model.active_overview().map(|state| state.load.clone()), Some(OverviewLoad::Connecting));
    assert_eq!(model.connections.status.get("my"), Some(&LinkStatus::Connecting));
    assert!(model.pending.iter().any(|job| matches!(job, PendingJob::OpenConnection)), "排队按需连接");
    assert!(model.connect_intents.iter().any(|intent| intent.connection_id == "my"));
    assert!(model.overview_read_request().is_none(), "连接中不重复排队");
    assert_eq!(model.connect_intents.iter().filter(|intent| intent.connection_id == "my").count(), 1);

    // 连上之后下一帧读取。
    model.connections.status.insert("my".to_owned(), LinkStatus::Connected);
    assert!(model.overview_read_request().is_some());

    // 连接失败：停在失败说明上，重试重新排队连接。
    let mut model = self::model();
    model.connections.status.remove("my");
    model.overview_click_connection("my", false);
    let _ = model.overview_read_request();
    model.connections.status.insert("my".to_owned(), LinkStatus::Failed);
    model.connections.failures.insert("my".to_owned(), "密码错误".to_owned());
    assert!(model.overview_read_request().is_none());
    assert_eq!(model.active_overview().map(|state| state.load.clone()), Some(OverviewLoad::Failed("密码错误".to_owned())));
    model.dispatch("overview-connect");
    assert_eq!(model.active_overview().map(|state| state.load.clone()), Some(OverviewLoad::Connecting));

    // 按需连接因缺密码失败，随后在密码框里连上：概览不停在失败说明上，下一帧直接读取。
    let mut model = self::model();
    model.connections.status.remove("my");
    model.overview_click_connection("my", false);
    let _ = model.overview_read_request();
    model.connections.status.insert("my".to_owned(), LinkStatus::Failed);
    model.connections.failures.insert("my".to_owned(), "请填写数据库密码".to_owned());
    assert!(model.overview_read_request().is_none());
    assert!(matches!(model.active_overview().map(|state| state.load.clone()), Some(OverviewLoad::Failed(_))));
    model.connections.status.insert("my".to_owned(), LinkStatus::Connected);
    assert!(model.overview_read_request().is_some(), "连上后自动重读");
    assert_eq!(model.active_overview().map(|state| state.load.clone()), Some(OverviewLoad::Loading));
}

#[test]
fn postgres_database_lists_schemas_and_schema_lists_tables_while_redis_has_no_overview() {
    let mut model = model();
    assert!(model.overview_click_node(&database_node_id("pg", "shop"), false));
    let state = model.active_overview().unwrap();
    assert_eq!(state.level(), StatsLevel::Schemas);
    // 双击模式行进入模式的表列表。
    let spec = state.target.spec();
    let (_, generation) = model.overview_read_request().unwrap();
    model.finish_overview_read(&spec, generation, Ok(body(StatsLevel::Schemas, vec![StatRow::named("public", None)], None)));
    model.dispatch("overview-activate:public");
    let state = model.active_overview().unwrap();
    assert_eq!((state.target.database.as_str(), state.target.schema.as_str()), ("shop", "public"));
    assert_eq!(state.level(), StatsLevel::Objects);
    // 树上的模式节点打开同一范围。
    let schema = schema_node_id("pg", "shop", "public");
    let tabs = model.tabs.len();
    assert!(model.overview_click_node(&schema, false));
    assert_eq!(model.tabs.len(), tabs, "同一范围复用已有标签");

    // Redis 与对象/分组节点没有概览：不开标签，原有行为不变。
    let before = model.tabs.len();
    assert!(!model.overview_click_connection("rd", false));
    let orders = object_node_id("my", "shop", "", &ObjectRow::table("orders", Some(3)));
    assert!(!model.overview_click_node(&orders, false));
    assert_eq!(model.tabs.len(), before);
}

#[test]
fn row_menu_reuses_the_object_tree_menu_or_falls_back_and_new_query_binds_the_database() {
    let mut model = model();
    model.overview_click_node(&database_node_id("my", "shop"), false);
    let (target, generation) = model.overview_read_request().unwrap();
    let rows = vec![StatRow::named("orders", Some(ObjectKind::Table)), StatRow::named("ghost", Some(ObjectKind::Table))];
    model.finish_overview_read(&target.spec(), generation, Ok(body(StatsLevel::Objects, rows, None)));

    // 对象树里有这张表：右键是对象树同一册菜单（含结构、DDL 与危险操作）。
    model.dispatch("overview-menu:orders");
    let node = object_node_id("my", "shop", "", &ObjectRow::table("orders", Some(3)));
    assert_eq!(model.nav_menu_node.as_deref(), Some(node.as_str()));
    let items: Vec<&str> = model.menu.as_ref().unwrap().items.iter().map(|item| item.id).collect();
    assert!(items.contains(&"tree-open-data") && items.contains(&"open-structure"), "{items:?}");
    assert!(model.preview_tab.is_none(), "右键也算操作");
    model.menu = None;

    // 树上还没有的表：概览自己的菜单，动作绑定这一行。
    model.dispatch("overview-menu:ghost");
    let items: Vec<String> = model.menu.as_ref().unwrap().items.iter().map(|item| item.command()).collect();
    assert!(items.contains(&"overview-open-data:ghost".to_owned()), "{items:?}");
    assert!(items.contains(&"overview-open-structure:ghost".to_owned()));
    assert!(items.contains(&"overview-new-query:ghost".to_owned()));
    model.menu = None;
    model.dispatch("overview-open-structure:ghost");
    assert_eq!(model.active_tab().map(|tab| tab.kind), Some(WorkItemKind::Structure));

    let _ = model.activate_item(&model.tabs.iter().find(|tab| tab.kind == WorkItemKind::Overview).unwrap().id.clone());
    model.dispatch("overview-new-query:ghost");
    let query = model.active_tab().cloned().unwrap();
    assert!(query.kind.is_query());
    assert_eq!((query.connection_id.as_deref(), query.database.as_deref()), (Some("my"), Some("shop")));
    assert!(query.draft.is_empty() && model.executions == 0, "只新建查询，不执行");

    let _ = model.activate_item(&model.tabs.iter().find(|tab| tab.kind == WorkItemKind::Overview).unwrap().id.clone());
    model.dispatch("overview-copy-name:ghost");
    assert!(model.copied.contains("ghost"));
}

#[test]
fn overview_tabs_survive_a_session_round_trip() {
    let mut model = model();
    model.overview_click_node(&database_node_id("pg", "shop"), true);
    let snapshot = model.snapshot();
    let mut restored = WorkbenchModel::new(Shell::default());
    restored.restore_session(&snapshot.session);
    let tab = restored.active_tab().cloned().expect("恢复的概览");
    assert_eq!(tab.kind, WorkItemKind::Overview);
    assert_eq!(restored.tool, WorkTool::Overview);
    restored.sync_overviews();
    let target = OverviewTarget::parse(tab.spec.as_deref().unwrap()).unwrap();
    assert_eq!((target.engine, target.database.as_str()), (Engine::Postgres, "shop"));
    assert_eq!(restored.active_overview().map(|state| state.load.clone()), Some(OverviewLoad::Pending), "恢复后按需重读");
    let _ = connection_node_id("pg");
}

/// 真实 SQLite 会话：单击连接行 → 主区出现概览预览并读到表；双击表行打开数据。
#[gpui_kit::test]
fn clicking_a_real_sqlite_connection_renders_its_overview_and_double_click_opens_the_table(cx: &mut gpui_kit::TestAppContext) {
    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-overview-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let database = dir.join("items.sqlite");
    let config = dir.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("runtime");
        runtime.block_on(async move {
            let pool = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).create_if_missing(true))
                .await
                .unwrap();
            for sql in [
                "CREATE TABLE item (id INTEGER PRIMARY KEY, label TEXT NOT NULL)",
                "INSERT INTO item (label) VALUES ('甲'), ('乙')",
                "CREATE VIEW item_labels AS SELECT label FROM item",
            ] {
                sqlx::query(sql).execute(&pool).await.unwrap();
            }
            pool.close().await;
            let services = Arc::new(AppServices::new(Arc::new(Service::new_for_test(config.join("connections.sqlite")))));
            services.connections.open_sqlite_for_test("notes", "笔记", &database).await.unwrap();
            let _ = tx.send(services);
        });
    });
    let services = rx.recv_timeout(std::time::Duration::from_secs(120)).expect("sqlite setup");
    cx.update(gpui_kit::init);
    let (window, view) = cx.update(|cx| {
        let bounds = gpui_kit::Bounds { origin: gpui_kit::Point::default(), size: gpui_kit::size(gpui_kit::px(1360.), gpui_kit::px(800.)) };
        let (window, view) = gpui_kit::open_window(
            gpui_kit::WindowOptions { window_bounds: Some(gpui_kit::WindowBounds::Windowed(bounds)), ..Default::default() },
            cx,
            |window, cx| cx.new(|cx| super::view::Workbench::with_services(window, cx, services, true)),
        )
        .expect("workbench");
        let shell = view.read(cx).shell().clone();
        crate::app::install_workbench_actions(cx, &view);
        crate::app::install_menus(cx, &shell);
        (window, view)
    });
    let wait = |cx: &mut gpui_kit::TestAppContext, done: &dyn Fn(&super::view::Workbench) -> bool| {
        let started = std::time::Instant::now();
        while started.elapsed() < std::time::Duration::from_secs(30) {
            cx.run_until_parked();
            let _ = cx.update_window(window, |_, window, cx| window.render_frame(cx));
            if cx.update(|cx| done(view.read(cx))) {
                return true;
            }
            std::thread::park_timeout(std::time::Duration::from_millis(25));
        }
        false
    };
    assert!(
        wait(cx, &|view| view.model.connections.rows.iter().any(|row| row.saved.id == "notes")
            && view.model.connections.row_status("notes") == LinkStatus::Connected),
        "外壳没有读到已打开的连接"
    );
    cx.update_window(window, |_, window, cx| window.click("side-notes", cx)).unwrap();
    assert!(
        wait(cx, &|view| view.model.active_overview().is_some_and(|state| state.load == OverviewLoad::Ready)),
        "单击连接后概览没有读完"
    );
    let found = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            (window.try_find("overview-page").is_some(), window.try_find("overview-row-0").is_some(), window.try_find("overview-row-1").is_some())
        })
        .unwrap();
    // 虚拟化的对象列表同样常驻可见滚动条（ADR-8 / R15），叠层铺满列表区域。
    let bar = cx.update_window(window, |_, window, _| window.try_find("overview-list-bar").map(|found| found.bounds())).unwrap();
    let bar = bar.expect("概览列表缺可见滚动条叠层");
    assert!(bar.size.height > gpui_kit::px(0.) && bar.size.width > gpui_kit::px(0.), "滚动条叠层应铺在列表上：{bar:?}");
    assert_eq!(found, (true, true, true), "概览页与两行（表、视图）都画出来");
    // 行铺满列表宽度，各列与列头对齐（原生截图里行曾按内容宽度挤在左边）。
    let row = cx.update_window(window, |_, window, _| window.find("overview-row-0").bounds()).unwrap();
    assert!(row.size.width >= bar.size.width - gpui_kit::px(1.), "行应铺满列表：{row:?} vs {bar:?}");
    cx.update(|cx| {
        let view = view.read(cx);
        let tab = view.model.active_tab().cloned().unwrap();
        assert_eq!(tab.kind, WorkItemKind::Overview);
        assert!(view.model.is_preview_tab(&tab.id), "单击打开的是预览标签");
        let state = view.model.active_overview().unwrap();
        let names: Vec<_> = state.visible().iter().map(|row| (row.name.clone(), row.kind)).collect();
        assert_eq!(names, vec![("item".to_owned(), Some(ObjectKind::Table)), ("item_labels".to_owned(), Some(ObjectKind::View))]);
        // 带 dbstat 的构建读得到表大小；读不到时必须标出统计缺失，而不是悄悄显示空白。
        assert!(state.rows[0].size.is_some() || state.missing.is_some(), "{:?}", state);
    });
    cx.update_window(window, |_, window, cx| window.double_click("overview-row-0", cx)).unwrap();
    assert!(wait(cx, &|view| view.model.active_tab().is_some_and(|tab| tab.kind == WorkItemKind::Table)), "双击表行打开数据");
    cx.update(|cx| {
        let view = view.read(cx);
        assert!(view.model.preview_tab.is_none(), "在概览里双击打开对象后预览转为固定");
        let spec = view.model.active_tab().and_then(|tab| tab.spec.clone()).unwrap();
        assert!(spec.starts_with("notes|笔记|main||item|"), "{spec}");
    });
    // R14：回车与双击相同。回到概览，单击第一行获得焦点，↓ 移到视图行，回车打开它的数据。
    let overview_tab = cx.update(|cx| view.read(cx).model.tabs.iter().find(|tab| tab.kind == WorkItemKind::Overview).map(|tab| tab.id.clone()).unwrap());
    cx.update(|cx| view.update(cx, |view, cx| {
        let _ = view.model.activate_item(&overview_tab);
        cx.notify();
    }));
    assert!(wait(cx, &|view| view.model.active_tab().is_some_and(|tab| tab.kind == WorkItemKind::Overview)), "回到概览");
    cx.update_window(window, |_, window, cx| window.click("overview-row-0", cx)).unwrap();
    cx.update_window(window, |_, window, cx| window.press("down", cx)).unwrap();
    cx.update(|cx| assert_eq!(view.read(cx).model.active_overview().and_then(|state| state.selected.clone()).as_deref(), Some("item_labels"), "↓ 移动选中"));
    // R13：⌘C 复制选中行（制表符分隔）。
    cx.update_window(window, |_, window, cx| window.press(if cfg!(target_os = "macos") { "cmd-c" } else { "ctrl-c" }, cx)).unwrap();
    cx.update(|cx| {
        let copied = view.read(cx).model.copied.clone();
        assert!(copied.starts_with("item_labels\t"), "⌘C 复制选中行：{copied:?}");
    });
    cx.update_window(window, |_, window, cx| window.press("enter", cx)).unwrap();
    assert!(
        wait(cx, &|view| view.model.active_tab().and_then(|tab| tab.spec.clone()).is_some_and(|spec| spec.starts_with("notes|笔记|main||item_labels|"))),
        "回车打开选中的视图"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
