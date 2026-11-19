//! R4 工作台：后台扫描事件、模板视图与批量预览、分隔符分组、生产确认、集群跨槽提示、诊断页与页签。
//! 只注入模型状态，不发网络请求；渲染走 GPUI 无头窗口。

use super::*;
use crate::adapters::redis::cluster::ClusterNode;
use gpui_kit::test::TestWindowExt;

fn services(name: &str) -> (Arc<AppServices>, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("rillsql-redis-wb-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
    (services, dir)
}

/// 等后台任务（tokio 上的读写）把结果交回界面。
fn settle(cx: &mut gpui_kit::TestAppContext, view: &Entity<RedisPane>, mut ready: impl FnMut(&RedisPane) -> bool) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        cx.run_until_parked();
        if cx.update(|app| ready(view.read(app))) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

fn key(name: &str, key_type: &str) -> RedisKeyInfo {
    RedisKeyInfo { name: name.into(), key_type: key_type.into(), ttl_ms: Some(-1) }
}

fn pane(name: &str) -> (RedisPane, std::path::PathBuf) {
    let (services, dir) = services(name);
    let mut pane = RedisPane::new(services);
    pane.connection_id = "cache".into();
    pane.connection_name = "订单缓存".into();
    (pane, dir)
}

fn open(cx: &mut gpui_kit::TestAppContext, width: f32, setup: impl FnOnce(&mut RedisPane) + 'static) -> (gpui_kit::AnyWindowHandle, Entity<RedisPane>, std::path::PathBuf) {
    let (services, dir) = services("render");
    cx.update(gpui_kit::init);
    let (window, view) = cx.update(|cx| {
        let bounds = gpui_kit::Bounds { origin: gpui_kit::Point::default(), size: gpui_kit::size(px(width), px(900.)) };
        gpui_kit::open_window(
            gpui_kit::WindowOptions { window_bounds: Some(gpui_kit::WindowBounds::Windowed(bounds)), ..Default::default() },
            cx,
            |_window, cx| {
                cx.new(|_| {
                    let mut pane = RedisPane::new(services);
                    pane.connection_id = "cache".into();
                    pane.connection_name = "订单缓存".into();
                    setup(&mut pane);
                    pane
                })
            },
        )
        .expect("window")
    });
    (window, view, dir)
}

/// 后台扫描：事件按批并入索引（同名键原位更新），进度总数、结束状态与上限都记下；旧一轮扫描的事件被丢弃。
#[gpui_kit::test]
fn scan_events_fill_the_index_report_progress_and_ignore_stale_runs(cx: &mut gpui_kit::TestAppContext) {
    let (pane, dir) = pane("scan");
    let view = cx.update(|cx| cx.new(|_| pane));
    cx.update(|cx| {
        view.update(cx, |pane, cx| {
            pane.scan.epoch = 7;
            pane.scan.running = true;
            pane.apply_scan_events(
                vec![
                    (7, ScanEvent::Total(Some(2500))),
                    (7, ScanEvent::Batch(vec![key("a:b:1", "string"), key("a:b:2", "hash")])),
                    (6, ScanEvent::Batch(vec![key("stale", "string")])),
                    (7, ScanEvent::Batch(vec![key("a:b:1", "list")])),
                ],
                cx,
            );
            assert_eq!(pane.keys().len(), 2, "旧一轮的批次丢弃，同名键不重复");
            assert_eq!(pane.keys()[0].key_type, "list", "同名键原位更新");
            assert_eq!(pane.scan.total, Some(2500));
            assert!(pane.scan_running());
            pane.apply_scan_events(vec![(7, ScanEvent::Finished { resume: ScanResume { cursors: vec![Some(42)] }, cancelled: false, capped: true })], cx);
            assert!(!pane.scan_running());
            assert!(pane.scan_capped(), "达到上限要停下并提示模式搜索");
            assert!(!pane.scan_done(), "还有游标没读完");
            assert_eq!(pane.scan.resume.cursors, vec![Some(42)]);
            pane.apply_scan_events(vec![(7, ScanEvent::Finished { resume: ScanResume { cursors: vec![None] }, cancelled: false, capped: false })], cx);
            assert!(pane.scan_done());
            // 取消：已读结果保留，可以继续。
            pane.scan.running = true;
            pane.command("redis-scan-cancel", cx);
            assert!(!pane.scan_running());
            assert!(pane.scan.cancelled);
            assert!(pane.scan.cancel.load(Ordering::SeqCst), "后台任务收到取消标志");
            assert_eq!(pane.keys().len(), 2);
        });
    });
    let _ = std::fs::remove_dir_all(dir);
}

/// 分隔符按连接配置：a:b:1、a:b:2 在 a→b 分组且计数 2；换成 | 后按新分隔符分组。
#[test]
fn tree_rows_follow_connection_separators() {
    let (mut pane, dir) = pane("tree");
    pane.set_keys(vec![key("a:b:1", "string"), key("a:b:2", "string"), key("x|y", "hash")]);
    pane.view_mode = ViewMode::Tree;
    pane.expanded = ["a".to_owned(), "a:b".to_owned()].into_iter().collect();
    let rows = pane.build_key_rows();
    assert!(rows.iter().any(|row| matches!(row, KeyRow::Folder { prefix, count: 2, .. } if prefix == "a:b")), "{rows:?}");
    assert!(rows.iter().any(|row| matches!(row, KeyRow::Key { display, depth: 0, .. } if display == "x|y")));
    pane.prefs.set_separators_text(": |");
    pane.invalidate_rows();
    let rows = pane.build_key_rows();
    assert!(rows.iter().any(|row| matches!(row, KeyRow::Folder { prefix, count: 1, .. } if prefix == "x")), "| 也成为分隔符：{rows:?}");
    let _ = std::fs::remove_dir_all(dir);
}

/// 模板视图：order:1001、order:1002、order:<uuid> 归并为 order:* 计数 3；展开列出成员；
/// 批量删除先进确认框（数量 + 样例），生产库还要输入连接名。
#[gpui_kit::test]
fn template_view_groups_keys_and_batch_delete_previews_before_confirming(cx: &mut gpui_kit::TestAppContext) {
    cx.executor().allow_parking();
    let (pane, dir) = pane("template");
    let services = Arc::clone(&pane.services);
    let view = cx.update(|cx| cx.new(|_| pane));
    cx.update(|cx| {
        view.update(cx, |pane, cx| {
            pane.set_keys(vec![
                key("order:1001", "hash"),
                key("order:1002", "hash"),
                key("order:3f2504e0-4f89-11d3-9a0c-0305e82c3301", "hash"),
                key("config", "string"),
            ]);
            pane.command("redis-view-template", cx);
            let rows = pane.key_rows();
            let groups = pane.templates();
            let order = groups.iter().position(|group| group.template == "order:*").expect("order:*");
            assert_eq!(groups[order].count, 3);
            assert!(rows.contains(&KeyRow::Template { group: order, open: false }));
            pane.command("redis-tpl:order:*", cx);
            let rows = pane.key_rows();
            assert_eq!(rows.iter().filter(|row| matches!(row, KeyRow::Key { depth: 1, .. })).count(), 3, "展开后列出成员：{rows:?}");

            pane.set_prod_target(true);
            pane.command("redis-tpl-delete:order:*", cx);
            assert!(pane.confirm_open());
            let preview = pane.confirm_preview();
            assert!(preview.starts_with("UNLINK … (3)"), "{preview}");
            assert!(preview.contains("order:1001"), "预览带样例：{preview}");
            assert!(pane.confirm_destructive());
            // 生产库：没输入或输错连接名都不执行，确认框留着。
            pane.command("redis-confirm", cx);
            assert!(pane.confirm_open(), "生产库没输入连接名不能执行");
            assert!(pane.confirm_typed_error);
            pane.confirm_typed = "订单".into();
            pane.command("redis-confirm", cx);
            assert!(pane.confirm_open());
            assert!(pane.batch.is_none(), "没有开始批量操作");
            pane.confirm_typed = " 订单缓存 ".into();
            pane.command("redis-confirm", cx);
            assert!(!pane.confirm_open(), "输入一致后才执行");
            let batch = pane.batch.as_ref().expect("批量操作开始");
            assert_eq!(batch.progress.total, 3);
            assert_eq!(batch.template, "order:*");
            pane.command("redis-batch-cancel", cx);
            assert!(pane.batch.as_ref().unwrap().cancel.load(Ordering::SeqCst), "批量操作可取消");
        });
    });
    // 连接没打开：批量操作就地失败并结束，不会留下进行中的状态。
    assert!(settle(cx, &view, |pane| pane.batch.as_ref().is_some_and(|batch| batch.progress.finished)), "批量操作没有结束");
    cx.update(|cx| {
        view.update(cx, |pane, cx| {
            assert!(pane.error().contains("没有打开"), "{}", pane.error());
            // 批量 TTL：输入非法秒数不进确认；合法秒数进确认。
            pane.batch = None;
            pane.set_prod_target(false);
            pane.command("redis-tpl-ttl:order:*", cx);
            pane.batch_ttl_input = "abc".into();
            pane.command("redis-batch-ttl-apply", cx);
            assert!(!pane.confirm_open());
            pane.batch_ttl_input = "600".into();
            pane.command("redis-batch-ttl-apply", cx);
            assert!(pane.confirm_open());
            assert!(pane.confirm_preview().starts_with("EXPIRE … 600 (3)"), "{}", pane.confirm_preview());
            assert!(!pane.confirm_destructive(), "设 TTL 不是删除");
            pane.command("redis-cancel", cx);

            // 标记变量段为固定：order:* 拆回各自的键。
            pane.command("redis-tpl-mark:1:0:order:*", cx);
            assert_eq!(pane.prefs.rules.len(), 1);
            let groups = pane.templates();
            assert!(groups.iter().all(|group| group.template != "order:*"), "标记固定后不再归并");
            assert!(groups.iter().any(|group| group.template == "order:1001"));
        });
    });
    // 规则按连接保存：重读偏好仍在。
    let saved = || {
        let services = Arc::clone(&services);
        let (tx, rx) = std::sync::mpsc::channel();
        crate::app::views::grid::runtime_for_views().spawn(async move {
            let _ = tx.send(crate::application::redis::load_prefs(services.connections.as_ref(), "cache").await);
        });
        rx.recv_timeout(std::time::Duration::from_secs(10)).expect("prefs")
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while saved().rules.is_empty() && std::time::Instant::now() < deadline {
        cx.run_until_parked();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(saved().rules.len(), 1, "模板规则按连接保存");
    cx.run_until_parked();
    let _ = std::fs::remove_dir_all(dir);
}

/// 生产标签接入所有写入确认：单键删除、命令行写命令都要输入连接名；非生产库照常一次确认。
#[gpui_kit::test]
fn production_target_requires_typing_the_connection_name(cx: &mut gpui_kit::TestAppContext) {
    cx.executor().allow_parking();
    assert!(confirm_gate(false, "", "x"));
    assert!(!confirm_gate(true, "", "x"));
    assert!(!confirm_gate(true, "y", "x"));
    assert!(confirm_gate(true, " x ", "x"));
    assert!(!confirm_gate(true, "", ""), "没有可核对的名字时不放行");
    let (pane, dir) = pane("prod");
    let view = cx.update(|cx| cx.new(|_| pane));
    cx.update(|cx| {
        view.update(cx, |pane, cx| {
            pane.plant_key("user:1");
            pane.connection_name = "订单缓存".into();
            pane.set_prod_target(true);
            pane.set_command("SET user:1 v");
            pane.command("redis-exec", cx);
            assert!(pane.confirm_open());
            pane.command("redis-confirm", cx);
            assert!(pane.confirm_open(), "生产库写命令没输入连接名不执行");
            pane.command("redis-cancel", cx);
            assert!(pane.confirm_typed.is_empty(), "取消后清掉已输入的名字");
            pane.command("redis-delete", cx);
            pane.confirm_typed = "订单缓存".into();
            pane.command("redis-confirm", cx);
            assert!(!pane.confirm_open(), "输入一致后执行");
        });
    });
    assert!(settle(cx, &view, |pane| !pane.running()));
    cx.update(|cx| {
        view.update(cx, |pane, cx| {
            // 只读命令不需要确认。
            pane.set_command("GET user:1");
            pane.command("redis-exec", cx);
            assert!(!pane.confirm_open());
        });
    });
    assert!(settle(cx, &view, |pane| !pane.running()));
    let _ = std::fs::remove_dir_all(dir);
}

fn cluster_topology() -> Topology {
    let node = |port: u16, slots: Vec<(u16, u16)>| ClusterNode {
        id: format!("n{port}"),
        host: "127.0.0.1".into(),
        port,
        master: true,
        master_id: String::new(),
        flags: vec!["master".into()],
        link: "connected".into(),
        slots,
        myself: false,
    };
    Topology { cluster: true, nodes: vec![node(7001, vec![(0, 5460)]), node(7002, vec![(5461, 10922)]), node(7003, vec![(10923, 16383)])], addr: "127.0.0.1:7001".into(), fallback: None }
}

/// 集群：多键命令跨槽在发送前就给出 CROSSSLOT 说明；同 hash tag 照常；键详情显示槽、节点与 hash tag。
#[gpui_kit::test]
fn cluster_crossslot_is_explained_and_key_placement_is_shown(cx: &mut gpui_kit::TestAppContext) {
    let (window, view, dir) = open(cx, 1280., |pane| {
        pane.topology = cluster_topology();
        pane.plant_key("{user:1}:profile");
        pane.placement = Some(cluster::placement(&pane.topology.nodes, "{user:1}:profile"));
    });
    cx.update(|cx| {
        view.update(cx, |pane, cx| {
            pane.set_command("MGET foo bar");
            pane.command("redis-exec", cx);
            assert!(pane.error().contains("CROSSSLOT"), "{}", pane.error());
            assert!(!pane.confirm_open());
            pane.set_command("DEL foo bar");
            pane.command("redis-exec", cx);
            assert!(!pane.confirm_open(), "跨槽的写命令也不进确认");
            assert!(pane.error().contains("hash tag"));
            pane.set_command("DEL {u}:a {u}:b");
            pane.command("redis-exec", cx);
            assert!(pane.confirm_open(), "同槽的写命令照常确认");
            pane.command("redis-cancel", cx);
        });
    });
    let (chip, nodes_tab, placement) = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            (
                window.try_find("redis-cluster-chip").is_some(),
                window.try_find("redis-page:nodes").is_some(),
                window.try_find("redis-key-placement").is_some(),
            )
        })
        .unwrap();
    assert!(chip, "集群顶栏显示集群标识（不再是库选择）");
    assert!(nodes_tab, "集群才有节点页签");
    assert!(placement, "键详情显示槽位");
    let place = view.read_with(cx, |pane, _| pane.placement.clone()).unwrap();
    assert_eq!(place.hash_tag.as_deref(), Some("user:1"));
    assert_eq!(place.slot, cluster::key_slot("user:1"));
    assert!(place.node.is_some());
    let _ = std::fs::remove_dir_all(dir);
}

/// 键页虚拟列表没有 500 行上限；模板视图与扫描进度（进行中可取消、达到上限提示）都画出来。
#[gpui_kit::test]
fn key_page_renders_virtual_rows_templates_and_scan_progress(cx: &mut gpui_kit::TestAppContext) {
    let (window, view, dir) = open(cx, 1280., |pane| {
        let keys = (0..2500).map(|index| key(&format!("order:{index}"), "hash")).collect();
        pane.set_keys(keys);
        pane.scan.running = true;
        pane.scan.total = Some(2500);
    });
    let (rows, cancel, more, label) = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            (
                view.update(cx, |pane, _| pane.key_rows().len()),
                window.try_find("redis-scan-cancel").is_some(),
                window.try_find("redis-more").is_some(),
                window.find("redis-scan-progress").label().map(str::to_owned),
            )
        })
        .unwrap();
    assert_eq!(rows, 2500, "没有 500 行上限");
    assert!(cancel, "扫描中可取消");
    assert!(!more);
    let label = label.unwrap_or_default();
    assert!(label.contains("2500") && label.contains("后台"), "{label}");
    cx.update(|cx| {
        view.update(cx, |pane, cx| {
            pane.scan.running = false;
            pane.scan.capped = true;
            pane.command("redis-view-template", cx);
        })
    });
    let (template_row, capped) = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            (window.try_find("redis-tpl:order:*").map(|row| row.label().map(str::to_owned)), window.find("redis-scan-progress").label().map(str::to_owned))
        })
        .unwrap();
    assert_eq!(template_row.flatten().as_deref(), Some("order:* 2500"));
    assert!(capped.unwrap_or_default().contains("上限"), "达到上限提示模式搜索");
    let _ = std::fs::remove_dir_all(dir);
}

/// 诊断页：页签切换、慢日志按耗时/时间排序、清空需确认；客户端 KILL 需确认；信息页指标卡片。
#[gpui_kit::test]
fn diagnostic_pages_sort_slowlog_and_confirm_resets_and_kills(cx: &mut gpui_kit::TestAppContext) {
    let (window, view, dir) = open(cx, 1280., |pane| {
        pane.page = Page::Slowlog;
        pane.diag.slow_loaded = true;
        pane.diag.slowlog = vec![
            SlowEntry { id: 1, at: 100, micros: 500, command: "GET a".into(), client: "c".into(), client_name: String::new(), node: String::new() },
            SlowEntry { id: 2, at: 50, micros: 90_000, command: "KEYS *".into(), client: "c".into(), client_name: String::new(), node: String::new() },
        ];
        pane.sort_slowlog();
        pane.diag.clients_loaded = true;
        pane.diag.clients = vec![ClientInfo {
            id: "7".into(),
            addr: "10.0.0.1:5000".into(),
            name: "app".into(),
            age: "3".into(),
            idle: "0".into(),
            db: "0".into(),
            cmd: "get".into(),
            flags: "N".into(),
            node: String::new(),
        }];
        pane.diag.info_loaded = true;
        pane.diag.info = crate::adapters::redis::diag::parse_info("# Memory\r\nused_memory_human:1.50M\r\n");
    });
    let first = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("redis-diag-slowlog").is_some());
            assert!(window.try_find("redis-keys").is_none(), "诊断页不画键列表");
            view.read(cx).diag.slowlog[0].id
        })
        .unwrap();
    assert_eq!(first, 2, "默认按耗时降序");
    cx.update_window(window, |_, window, cx| window.click("redis-slow-sort:time", cx)).unwrap();
    assert_eq!(view.read_with(cx, |pane, _| pane.diag.slowlog[0].id), 1, "按时间降序");
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("redis-slow-sort:time", cx);
    })
    .unwrap();
    assert_eq!(view.read_with(cx, |pane, _| pane.diag.slowlog[0].id), 2, "再点切换升序");
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("redis-slow-reset", cx);
    })
    .unwrap();
    assert!(view.read_with(cx, |pane, _| pane.confirm_open() && pane.confirm_preview().contains("SLOWLOG RESET")), "清空慢日志需确认");
    cx.update(|cx| {
        view.update(cx, |pane, cx| {
            pane.command("redis-cancel", cx);
            pane.command("redis-page:clients", cx);
        })
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("redis-client-kill:0", cx);
    })
    .unwrap();
    assert!(view.read_with(cx, |pane, _| pane.confirm_open() && pane.confirm_preview().contains("CLIENT KILL ID 7")), "KILL 需确认");
    cx.update(|cx| {
        view.update(cx, |pane, cx| {
            pane.command("redis-cancel", cx);
            pane.command("redis-page:info", cx);
            pane.command("redis-page:nodes", cx);
            assert_eq!(pane.page(), Page::Info, "单机没有节点页");
        })
    });
    let info = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.try_find("redis-info-scroll").is_some()
        })
        .unwrap();
    assert!(info);
    let _ = std::fs::remove_dir_all(dir);
}

/// 值分页：已读条数 / 总数，下一页接在后面；JSON 文档按 JSON.SET 写回。
#[test]
fn paged_values_show_total_and_json_writes_use_json_set() {
    let (mut pane, dir) = pane("paged");
    pane.selected = "h".into();
    let detail = RedisKeyDetail {
        key_type: "hash".into(),
        ttl_ms: Some(-1),
        value: RedisValue::Hash { entries: vec![keys::KeyValuePair { field: "a".into(), value: "1".into() }] },
        total: Some(5000),
        next: Some("17".into()),
    };
    let label = pane.count_label(&detail);
    assert!(label.contains("5000"), "{label}");
    let mut value = detail.value.clone();
    keys::append_page(&mut value, RedisValue::Hash { entries: vec![keys::KeyValuePair { field: "b".into(), value: String::new() }] });
    assert!(matches!(&value, RedisValue::Hash { entries } if entries.len() == 2));
    pane.detail = Some(Arc::new(RedisKeyDetail { key_type: keys::JSON_TYPE.into(), ttl_ms: Some(-1), value: RedisValue::Json { text: "{}".into() }, total: None, next: None }));
    pane.stage_if_key(RedisMutation::JsonSet { key: "h".into(), value: "{\"a\":1}".into() });
    assert!(pane.confirm_open());
    assert_eq!(pane.confirm_preview(), "JSON.SET h $ {\"a\":1}");
    let _ = std::fs::remove_dir_all(dir);
}

/// 批量操作绑定发起时的逻辑库：进行中不能切库、控制台 SELECT 被拒；确认框开着时库变了，确认后不在新库上执行。
#[gpui_kit::test]
fn running_batch_pins_its_db_and_blocks_db_switches(cx: &mut gpui_kit::TestAppContext) {
    let (pane, dir) = pane("batch-db");
    let view = cx.update(|cx| cx.new(|_| pane));
    cx.update(|cx| {
        view.update(cx, |pane, cx| {
            pane.database = "3".into();
            pane.set_keys(vec![key("order:1001", "hash"), key("order:1002", "hash")]);
            pane.batch = Some(BatchRun {
                template: "order:*".into(),
                op: BatchOp::Unlink,
                progress: BatchProgress { total: 2, ..BatchProgress::default() },
                cancel: Arc::new(AtomicBool::new(false)),
            });
            pane.command("redis-db-pick:4", cx);
            assert_eq!(pane.database, "3", "批量操作进行中不切库");
            assert!(pane.error().contains("批量操作进行中"), "{}", pane.error());
            assert_eq!(pane.keys().len(), 2, "索引保留");

            pane.error.clear();
            pane.exec_command("SELECT".into(), vec!["5".into()], true, "SELECT 5".into(), cx);
            assert_eq!(pane.database, "3", "控制台 SELECT 被拒");
            assert!(pane.error().contains("批量操作进行中"), "{}", pane.error());

            // 批量操作结束后，确认框记下的是发起时的库；库变了就不执行。
            pane.batch.as_mut().unwrap().progress.finished = true;
            pane.command("redis-tpl-delete:order:*", cx);
            assert!(matches!(pane.confirm, Some(Confirm::Batch { db: 3, .. })), "确认框记下发起时的库");
            pane.database = "6".into();
            pane.error.clear();
            pane.command("redis-confirm", cx);
            assert!(pane.batch.as_ref().is_some_and(|batch| batch.progress.finished), "没有开始新的批量操作");
            assert!(pane.error().contains("逻辑库已切换"), "{}", pane.error());
        });
    });
    let _ = std::fs::remove_dir_all(dir);
}

/// 集群退回单节点：顶栏显示单节点浏览标记，说明里带原因。
#[gpui_kit::test]
fn cluster_fallback_is_explained(cx: &mut gpui_kit::TestAppContext) {
    let (pane, dir) = pane("fallback");
    let view = cx.update(|cx| cx.new(|_| pane));
    cx.update(|cx| {
        view.update(cx, |pane, _| {
            assert!(pane.fallback_text().is_none());
            pane.topology = Topology { cluster: false, nodes: Vec::new(), addr: "10.0.0.5:7001".into(), fallback: Some(ClusterFallback::Unreachable("connection refused".into())) };
            let text = pane.fallback_text().expect("说明");
            assert!(text.contains("connection refused") && text.contains("单节点"), "{text}");
            pane.topology.fallback = Some(ClusterFallback::Tunnel);
            assert!(pane.fallback_text().unwrap().contains("SSH"));
        });
    });
    let _ = std::fs::remove_dir_all(dir);
}
