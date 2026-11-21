//! 真实 Redis 夹具上的键浏览。连接走 AppServices，临时键测完删除，不动种子。

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::{Point, TestAppContext, WindowBounds, WindowOptions, px, size};
use gpui_kit::test::TestWindowExt;

use super::*;
use crate::application::AppServices;
use crate::connections::model::{ConnectionDraft, Engine, TlsMode};

#[gpui_kit::test]
fn rendered_controls_refuse_keys_and_keep_unconfirmed_input(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-redis-pane-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
    let (window, view) = open_pane(cx, services);
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |pane, _| pane.plant_key("rillsql:string:welcome"));
        window.render_frame(cx);
        assert_eq!(window.find("redis-filter").label(), Some("过滤"));
        assert_eq!(window.find("redis-refresh").label(), Some("刷新"));
        assert_eq!(window.find("redis-more").label(), Some("继续扫描"));
        assert_eq!(window.find("redis-ttl").label(), Some("设置 TTL"));
        assert_eq!(window.find("redis-delete").label(), Some("删除"));
        // 重命名、复制值是次要操作，收在「更多」里。
        assert_eq!(window.find("redis-more-actions").label(), Some("更多操作"));
        assert!(window.try_find("redis-rename").is_none() && window.try_find("redis-copy").is_none());
        window.click("redis-pattern", cx);
        window.input("ab", cx);
        // 命令输入在控制台页（顶部页签），不再常驻键页底部。
        window.click("redis-page:console", cx);
        window.render_frame(cx);
        assert_eq!(window.find("redis-exec").label(), Some("执行"));
        window.click("redis-command", cx);
        window.input("KEYS", cx);
    })
    .unwrap();
    cx.update(|app| {
        let pane = view.read(app);
        assert_eq!(pane.pattern(), "ab");
        assert_eq!(pane.command_input(), "KEYS");
    });
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |pane, _| pane.set_command("KEYS *"));
        window.render_frame(cx);
        window.click("redis-exec", cx);
    })
    .unwrap();
    cx.update(|app| {
        let pane = view.read(app);
        let refused = pane.console_error().unwrap_or_default();
        assert!(refused.contains("KEYS") || refused.contains("禁用"), "拒绝写在控制台记录里：{refused}");
        assert!(pane.note().is_empty(), "{}", pane.note());
        assert!(!pane.confirm_open());
        assert_eq!(pane.command_input(), "KEYS *");
    });
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |pane, _| pane.set_command("FLUSHALL"));
        window.render_frame(cx);
        window.click("redis-exec", cx);
    })
    .unwrap();
    cx.update(|app| {
        let pane = view.read(app);
        assert!(pane.confirm_open(), "{}", pane.status_line());
        assert!(pane.note().is_empty(), "{}", pane.note());
        assert!(pane.error().is_empty(), "{}", pane.error());
        assert!(pane.status_line().contains("FLUSHALL"), "{}", pane.status_line());
    });
    cx.update_window(window, |_, window, cx| window.click("redis-cancel", cx)).unwrap();
    cx.update(|app| {
        let pane = view.read(app);
        assert!(!pane.confirm_open());
        assert_eq!(pane.command_input(), "FLUSHALL");
        assert!(pane.note().is_empty());
    });
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |pane, _| pane.set_ttl("nope"));
        window.render_frame(cx);
        window.click("redis-page:keys", cx);
        window.render_frame(cx);
        window.click("redis-ttl", cx);
    })
    .unwrap();
    cx.update(|app| {
        let pane = view.read(app);
        assert!(pane.error().contains("TTL"), "{}", pane.error());
        assert_eq!(pane.ttl_input(), "nope");
        assert!(pane.note().is_empty());
        assert!(!pane.confirm_open());
    });
    cx.update_window(window, |_, window, cx| {
        window.click("redis-delete", cx);
        window.render_frame(cx);
        window.click("redis-cancel", cx);
    })
    .unwrap();
    cx.update(|app| {
        let pane = view.read(app);
        assert_eq!(pane.ttl_input(), "nope");
        assert_eq!(pane.selected(), "rillsql:string:welcome");
        assert!(pane.note().is_empty(), "{}", pane.note());
        assert!(!pane.confirm_open());
    });
}

#[gpui_kit::test]
fn sidebar_redis_button_opens_the_browser(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-redis-side-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
    let (window, view) = open_workbench(cx, Arc::clone(&services));
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |workbench, cx| {
            workbench.model.connections.rows.push(crate::connections::ConnectionRecord {
                saved: crate::connections::model::SavedConnection {
                    id: "cache".into(),
                    name: "缓存".into(),
                    engine: Engine::Redis,
                    env: "local".into(),
                    host: Some("127.0.0.1".into()),
                    port: Some(6379),
                    database: Some("0".into()),
                    username: None,
                    file_path: None,
                    replica_set: None,
                    tls: None,
                    ssh: None,
                    password_saved: false,
                    ssh_password_saved: false,
                },
                ssh_host_key: None,
            });
            cx.notify();
        });
        window.render_frame(cx);
        assert_eq!(window.find("side-open-redis-cache").label(), Some("键空间 缓存"));
        window.click("side-open-redis-cache", cx);
        window.render_frame(cx);
        assert_eq!(window.find("redis-filter").label(), Some("过滤"));
    })
    .unwrap();
    assert!(pump_status(cx, window, |text| text.contains("还没有打开")), "侧栏打开后，未连接不应显示成功");
    let status = status_label(cx, window);
    assert!(!status.contains("note=已执行"), "{status}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[gpui_kit::test]
fn live_redis_scans_filters_reads_ttl_rename_delete_and_confirms(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-redis-live-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let key = format!("rillsql:t8:{suffix}");
    let renamed = format!("rillsql:t8:renamed:{suffix}");
    if let Err(err) = block(open_redis(Arc::clone(&services), "live-redis")) {
        if fixture_unreachable(&err) {
            let _ = std::fs::remove_dir_all(&dir);
            eprintln!("跳过：Redis 夹具没有连上：{err}");
            return;
        }
        panic!("Redis 夹具没有连上：{err}");
    }
    let _cleanup = RedisCleanup { services: Arc::clone(&services), id: "live-redis".into(), keys: vec![key.clone(), renamed.clone()] };
    if let Err(err) = block(restore_token(Arc::clone(&services))) {
        panic!("带 TTL 的种子键没有恢复：{err}");
    }
    let (window, view) = open_pane(cx, Arc::clone(&services));
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |pane, cx| pane.open_spec("live-redis|夹具 Redis|0", cx));
        window.render_frame(cx);
    })
    .unwrap();
    assert!(pump(cx, window, &view, |pane| !pane.running() && (pane.done || !pane.keys().is_empty() || !pane.error().is_empty())), "SCAN 没有返回");
    assert_quiet(cx, &view);
    assert!(pump_more(cx, window, &view, "rillsql:string:welcome"), "SCAN 没有看到种子键 rillsql:string:welcome");
    for (name, needle) in [
        ("rillsql:string:welcome", "澄库"),
        ("rillsql:hash:order:1001", "林夏"),
        ("rillsql:list:queue", "任务A"),
        ("rillsql:set:tags", "紧急"),
        ("rillsql:zset:rank", "张伟"),
        ("rillsql:stream:events", "order.created"),
    ] {
        read_key(cx, window, &view, name, needle);
    }
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |pane, _| pane.set_pattern("rillsql:string:*"));
        window.render_frame(cx);
        window.click("redis-page:keys", cx);
        window.render_frame(cx);
        window.click("redis-filter", cx);
    })
    .unwrap();
    assert!(pump(cx, window, &view, |pane| !pane.running() && (pane.done || !pane.keys().is_empty() || !pane.error().is_empty())), "过滤 SCAN 没有返回");
    assert_quiet(cx, &view);
    assert!(pump_until_done(cx, window, &view), "过滤 SCAN 没有读完");
    cx.update(|app| {
        let pane = view.read(app);
        assert!(pane.keys().iter().all(|key| key.name.starts_with("rillsql:string:")), "{}", pane.status_line());
        assert!(pane.keys().iter().any(|key| key.name == "rillsql:string:temp-token" && key.ttl_ms.unwrap_or(-1) > 0), "{}", pane.status_line());
    });
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |pane, _| pane.set_command("PING"));
        window.render_frame(cx);
        window.click("redis-exec", cx);
    })
    .unwrap();
    assert!(pump(cx, window, &view, |pane| !pane.running() && (!pane.note().is_empty() || !pane.error().is_empty())), "PING 没有返回");
    cx.update(|app| {
        let pane = view.read(app);
        assert!(pane.error().is_empty(), "{}", pane.error());
        assert!(pane.reply().eq_ignore_ascii_case("pong") || pane.reply() == "OK", "{}", pane.reply());
        assert!(!pane.confirm_open());
    });
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |pane, _| pane.set_command(&format!("SET {key} hello-t8")));
        window.render_frame(cx);
        window.click("redis-exec", cx);
    })
    .unwrap();
    cx.update(|app| {
        let pane = view.read(app);
        assert!(pane.confirm_open(), "{}", pane.status_line());
        assert!(pane.note().is_empty(), "{}", pane.note());
        assert_eq!(pane.command_input(), format!("SET {key} hello-t8"));
    });
    cx.update_window(window, |_, window, cx| window.click("redis-cancel", cx)).unwrap();
    let missed = block({
        let services = Arc::clone(&services);
        let key = key.clone();
        async move { crate::application::redis::exec(services.connections.as_ref(), "live-redis", "GET".into(), vec![key], false).await }
    });
    let missed = missed.expect("GET");
    assert_eq!(missed.reply.kind, "nil", "未确认的 SET 写进了 Redis");
    cx.update_window(window, |_, window, cx| {
        window.click("redis-exec", cx);
        window.render_frame(cx);
        window.click("redis-confirm", cx);
    })
    .unwrap();
    assert!(pump(cx, window, &view, |pane| !pane.running() && (!pane.note().is_empty() || !pane.error().is_empty())), "确认后的 SET 没有返回");
    assert_quiet(cx, &view);
    let written = block({
        let services = Arc::clone(&services);
        let key = key.clone();
        async move { crate::application::redis::exec(services.connections.as_ref(), "live-redis", "GET".into(), vec![key], false).await }
    })
    .expect("GET after SET");
    assert_eq!(written.reply.text.as_deref(), Some("hello-t8"));
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |pane, _| pane.set_pattern(&key));
        window.render_frame(cx);
        window.click("redis-page:keys", cx);
        window.render_frame(cx);
        window.click("redis-filter", cx);
    })
    .unwrap();
    assert!(pump_more(cx, window, &view, &key), "确认写入后 SCAN 没有看到临时键");
    read_key(cx, window, &view, &key, "hello-t8");
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |pane, _| pane.set_ttl("90"));
        window.render_frame(cx);
        window.click("redis-page:keys", cx);
        window.render_frame(cx);
        window.click("redis-ttl", cx);
        window.render_frame(cx);
        window.click("redis-confirm", cx);
    })
    .unwrap();
    assert!(pump(cx, window, &view, |pane| !pane.running() && pane.detail_ttl().is_some_and(|ttl| ttl > 0 && ttl <= 90_000)), "TTL 没有写成 90 秒");
    assert_quiet(cx, &view);
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |pane, _| pane.set_ttl(""));
        window.render_frame(cx);
        window.click("redis-page:keys", cx);
        window.render_frame(cx);
        window.click("redis-ttl", cx);
        window.render_frame(cx);
        window.click("redis-confirm", cx);
    })
    .unwrap();
    assert!(pump(cx, window, &view, |pane| !pane.running() && pane.detail_ttl() == Some(-1) && pane.note().contains("移除")), "PERSIST 没有去掉过期");
    assert_quiet(cx, &view);
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |pane, _| pane.set_rename(&renamed));
        window.render_frame(cx);
        window.click("redis-rename", cx);
        window.render_frame(cx);
        window.click("redis-confirm", cx);
    })
    .unwrap();
    assert!(pump(cx, window, &view, |pane| !pane.running() && (pane.selected() == renamed || !pane.error().is_empty())), "重命名没有返回");
    assert_quiet(cx, &view);
    cx.update(|app| assert_eq!(view.read(app).selected(), renamed));
    let old = block({
        let services = Arc::clone(&services);
        let key = key.clone();
        async move { crate::application::redis::exec(services.connections.as_ref(), "live-redis", "GET".into(), vec![key], false).await }
    })
    .expect("GET old");
    assert_eq!(old.reply.kind, "nil");
    cx.update_window(window, |_, window, cx| {
        window.click("redis-delete", cx);
        window.render_frame(cx);
        window.click("redis-confirm", cx);
    })
    .unwrap();
    assert!(pump(cx, window, &view, |pane| !pane.running() && (!pane.note().is_empty() || !pane.error().is_empty())), "删除没有返回");
    assert_quiet(cx, &view);
    let gone = block({
        let services = Arc::clone(&services);
        let renamed = renamed.clone();
        async move { crate::application::redis::exec(services.connections.as_ref(), "live-redis", "GET".into(), vec![renamed], false).await }
    })
    .expect("GET renamed");
    assert_eq!(gone.reply.kind, "nil");
    services.disconnect("live-redis");
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |pane, _| pane.set_command("PING"));
        window.render_frame(cx);
        window.click("redis-exec", cx);
    })
    .unwrap();
    assert!(pump(cx, window, &view, |pane| !pane.running() && pane.console_error().is_some()), "断线后的 PING 没有失败");
    cx.update(|app| {
        let pane = view.read(app);
        assert!(pane.console_error().is_some_and(|error| !error.is_empty()), "失败写在控制台记录里");
        assert!(pane.note().is_empty(), "{}", pane.note());
        assert_eq!(pane.command_input(), "PING");
    });
    let _ = std::fs::remove_dir_all(&dir);
}

#[gpui_kit::test]
fn confirm_keeps_the_target_opened_with(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-redis-snap-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
    let prefix = format!("rillsql:t8:snap-{}", uuid::Uuid::new_v4().simple());
    let key_a = format!("{prefix}-a");
    let key_b = format!("{prefix}-b");
    let key_c = format!("{prefix}-c");
    let key_d = format!("{prefix}-d");
    let key_e = format!("{prefix}-e");
    if let Err(err) = block(open_redis(Arc::clone(&services), "live-redis-snap")) {
        if fixture_unreachable(&err) {
            let _ = std::fs::remove_dir_all(&dir);
            eprintln!("跳过：Redis 夹具没有连上：{err}");
            return;
        }
        panic!("Redis 夹具没有连上：{err}");
    }
    let _cleanup = RedisCleanup {
        services: Arc::clone(&services),
        id: "live-redis-snap".into(),
        keys: vec![key_a.clone(), key_b.clone(), key_c.clone(), key_d.clone(), key_e.clone()],
    };
    if let Err(err) = block({
        let services = Arc::clone(&services);
        let pairs = vec![(key_a.clone(), "va".into()), (key_b.clone(), "vb".into()), (key_c.clone(), "vc".into()), (key_e.clone(), "ve".into())];
        async move {
            for (key, value) in pairs {
                crate::application::redis::exec(services.connections.as_ref(), "live-redis-snap", "SET".into(), vec![key, value], true).await?;
            }
            Ok::<(), String>(())
        }
    }) {
        panic!("临时键没有写入：{err}");
    }
    let (window, view) = open_pane(cx, Arc::clone(&services));
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |pane, cx| pane.open_spec("live-redis-snap|快照|0", cx));
        window.render_frame(cx);
    })
    .unwrap();
    assert!(pump(cx, window, &view, |pane| !pane.running() && (pane.scan_done() || !pane.keys().is_empty() || !pane.error().is_empty())), "SCAN 没有返回");
    assert_quiet(cx, &view);
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |pane, _| pane.set_pattern(&format!("{prefix}*")));
        window.render_frame(cx);
        window.click("redis-page:keys", cx);
        window.render_frame(cx);
        window.click("redis-filter", cx);
    })
    .unwrap();
    assert!(pump_until_done(cx, window, &view), "临时键 SCAN 没有读完");
    assert_quiet(cx, &view);

    read_key(cx, window, &view, &key_c, "vc");
    cx.update_window(window, |_, window, cx| {
        window.click("redis-delete", cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|app| {
        let pane = view.read(app);
        assert!(pane.confirm_open());
        assert!(pane.confirm_prompt().contains(&key_c), "{}", pane.confirm_prompt());
    });
    cx.update_window(window, |_, window, cx| window.click("redis-cancel", cx)).unwrap();
    cx.update(|app| assert!(!view.read(app).confirm_open()));
    assert_eq!(redis_get(&services, &key_c).0, "bulk");

    read_key(cx, window, &view, &key_a, "va");
    cx.update_window(window, |_, window, cx| {
        window.click("redis-delete", cx);
        window.render_frame(cx);
    })
    .unwrap();
    let delete_prompt = cx.update(|app| view.read(app).confirm_prompt().to_owned());
    assert!(delete_prompt.contains(&key_a), "{delete_prompt}");
    click_listed(cx, window, &view, &key_b);
    cx.update(|app| {
        let pane = view.read(app);
        assert_eq!(pane.selected(), key_a);
        assert_eq!(pane.confirm_prompt(), delete_prompt);
        assert!(pane.confirm_open());
    });
    cx.update_window(window, |_, window, cx| window.click("redis-confirm", cx)).unwrap();
    assert!(pump(cx, window, &view, |pane| !pane.running() && (pane.note().contains("删除") || !pane.error().is_empty())), "删除确认没有返回");
    assert_quiet(cx, &view);
    assert_eq!(redis_get(&services, &key_a).0, "nil", "确认删成了别的键");
    assert_eq!(redis_get(&services, &key_b).1.as_deref(), Some("vb"), "改选的键被删了");

    let cancel_command = format!("DEL {key_e}");
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |pane, _| pane.set_command(&cancel_command));
        window.render_frame(cx);
        window.click("redis-exec", cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|app| {
        let pane = view.read(app);
        assert!(pane.confirm_open(), "{}", pane.status_line());
        assert!(pane.confirm_prompt().contains("DEL"), "{}", pane.confirm_prompt());
        assert!(pane.note().is_empty(), "{}", pane.note());
    });
    cx.update_window(window, |_, window, cx| window.click("redis-cancel", cx)).unwrap();
    cx.update(|app| {
        let pane = view.read(app);
        assert!(!pane.confirm_open());
        assert_eq!(pane.command_input(), cancel_command);
    });
    assert_eq!(redis_get(&services, &key_e).1.as_deref(), Some("ve"), "取消的 DEL 写进了 Redis");

    let original = format!("SET {key_d} snapped");
    let rewritten = format!("DEL {key_e}");
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |pane, _| pane.set_command(&original));
        window.render_frame(cx);
        window.click("redis-exec", cx);
        window.render_frame(cx);
    })
    .unwrap();
    let exec_prompt = cx.update(|app| {
        let pane = view.read(app);
        assert!(pane.confirm_open(), "{}", pane.status_line());
        assert!(pane.confirm_prompt().contains("SET"), "{}", pane.confirm_prompt());
        pane.confirm_prompt().to_owned()
    });
    cx.update_window(window, |_, window, cx| {
        window.click("redis-command", cx);
        for _ in 0..original.chars().count() {
            window.press("backspace", cx);
        }
        window.input(&rewritten, cx);
    })
    .unwrap();
    cx.update(|app| {
        let pane = view.read(app);
        assert_eq!(pane.command_input(), original, "确认期间命令被改成了另一条");
        assert_eq!(pane.confirm_prompt(), exec_prompt);
        assert!(pane.confirm_open());
    });
    cx.update_window(window, |_, window, cx| window.click("redis-confirm", cx)).unwrap();
    assert!(pump(cx, window, &view, |pane| !pane.running() && (pane.note().contains("已执行") || !pane.error().is_empty())), "命令确认没有返回");
    assert_quiet(cx, &view);
    assert_eq!(redis_get(&services, &key_d).1.as_deref(), Some("snapped"), "确认没有执行打开时的 SET");
    assert_eq!(redis_get(&services, &key_e).1.as_deref(), Some("ve"), "确认执行了后改的 DEL");
    let _ = std::fs::remove_dir_all(&dir);
}

fn click_listed(cx: &mut TestAppContext, window: gpui_kit::AnyWindowHandle, view: &gpui_kit::Entity<RedisPane>, name: &str) {
    let index = cx.update(|app| view.read(app).keys().iter().position(|key| key.name == name).unwrap_or(usize::MAX));
    assert!(index < usize::MAX, "列表里没有 {name}");
    let id = format!("redis-select:{index}");
    cx.update_window(window, |_, window, cx| window.click(id, cx)).unwrap();
}

fn redis_get(services: &Arc<AppServices>, key: &str) -> (String, Option<String>) {
    let services = Arc::clone(services);
    let key = key.to_owned();
    let outcome = block(async move { crate::application::redis::exec(services.connections.as_ref(), "live-redis-snap", "GET".into(), vec![key], false).await }).expect("GET");
    (outcome.reply.kind.to_owned(), outcome.reply.text)
}

fn read_key(cx: &mut TestAppContext, window: gpui_kit::AnyWindowHandle, view: &gpui_kit::Entity<RedisPane>, name: &str, needle: &str) {
    let index = cx.update(|app| view.read(app).keys().iter().position(|key| key.name == name).unwrap_or(usize::MAX));
    assert!(index < usize::MAX, "列表里没有 {name}");
    let id = format!("redis-select:{index}");
    cx.update_window(window, |_, window, cx| window.click(id, cx)).unwrap();
    assert!(pump(cx, window, view, |pane| !pane.running() && (pane.detail_text().contains(needle) || !pane.error().is_empty())), "{name} 没有读到");
    let detail = cx.update(|app| view.read(app).detail_text());
    let error = cx.update(|app| view.read(app).error().to_owned());
    assert!(error.is_empty(), "{name} {error}");
    assert!(detail.contains(needle), "{name} 值是 {detail}");
}

fn pump_until_done(cx: &mut TestAppContext, window: gpui_kit::AnyWindowHandle, view: &gpui_kit::Entity<RedisPane>) -> bool {
    for _ in 0..30 {
        let (done, running, error) = cx.update(|app| {
            let pane = view.read(app);
            (pane.scan_done(), pane.running(), pane.error().to_owned())
        });
        if !error.is_empty() {
            return false;
        }
        if done && !running {
            return true;
        }
        if running {
            if !pump(cx, window, view, |pane| !pane.running()) {
                return false;
            }
            continue;
        }
        cx.update_window(window, |_, window, cx| window.click("redis-more", cx)).unwrap();
        if !pump(cx, window, view, |pane| !pane.running()) {
            return false;
        }
    }
    false
}

fn pump_more(cx: &mut TestAppContext, window: gpui_kit::AnyWindowHandle, view: &gpui_kit::Entity<RedisPane>, name: &str) -> bool {
    for _ in 0..8 {
        let (found, done, running, error) = cx.update(|app| {
            let pane = view.read(app);
            (pane.keys().iter().any(|key| key.name == name), pane.scan_done(), pane.running(), pane.error().to_owned())
        });
        if !error.is_empty() {
            return false;
        }
        if found {
            return true;
        }
        if running {
            if !pump(cx, window, view, |pane| !pane.running()) {
                return false;
            }
            continue;
        }
        if done {
            return false;
        }
        cx.update_window(window, |_, window, cx| window.click("redis-more", cx)).unwrap();
        if !pump(cx, window, view, |pane| !pane.running()) {
            return false;
        }
    }
    false
}

fn assert_quiet(cx: &mut TestAppContext, view: &gpui_kit::Entity<RedisPane>) {
    let (error, note) = cx.update(|app| {
        let pane = view.read(app);
        (pane.error().to_owned(), pane.note().to_owned())
    });
    assert!(error.is_empty(), "{error} / {note}");
}

fn pump(cx: &mut TestAppContext, window: gpui_kit::AnyWindowHandle, view: &gpui_kit::Entity<RedisPane>, mut ready: impl FnMut(&RedisPane) -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(40);
    loop {
        let done = cx.update(|app| ready(view.read(app)));
        if done {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(40));
    }
}

fn pump_status(cx: &mut TestAppContext, window: gpui_kit::AnyWindowHandle, mut ready: impl FnMut(&str) -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let label = status_label(cx, window);
        if ready(&label) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(40));
    }
}

fn status_label(cx: &mut TestAppContext, window: gpui_kit::AnyWindowHandle) -> String {
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.find("redis-status").label().unwrap_or("").to_owned()
    })
    .unwrap_or_default()
}

fn open_pane(cx: &mut TestAppContext, services: Arc<AppServices>) -> (gpui_kit::AnyWindowHandle, gpui_kit::Entity<RedisPane>) {
    cx.update(gpui_kit::init);
    cx.update(|cx| {
        let bounds = gpui_kit::Bounds { origin: Point::default(), size: size(px(1200.), px(2400.)) };
        let (window, view) = gpui_kit::open_window(
            WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
            cx,
            |_, cx| cx.new(|_| RedisPane::new(services)),
        )
        .expect("redis pane");
        (window, view)
    })
}

fn open_workbench(cx: &mut TestAppContext, services: Arc<AppServices>) -> (gpui_kit::AnyWindowHandle, gpui_kit::Entity<crate::app::shell::Workbench>) {
    cx.update(gpui_kit::init);
    cx.update(|cx| {
        let bounds = gpui_kit::Bounds { origin: Point::default(), size: size(px(1360.), px(800.)) };
        let (window, view) = gpui_kit::open_window(
            WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
            cx,
            |window, cx| cx.new(|cx| crate::app::shell::Workbench::with_services(window, cx, services, false)),
        )
        .expect("workbench");
        (window, view)
    })
}

async fn restore_token(services: Arc<AppServices>) -> Result<(), String> {
    let current = crate::application::redis::exec(services.connections.as_ref(), "live-redis", "GET".into(), vec!["rillsql:string:temp-token".into()], false).await?;
    if current.reply.kind == "nil" {
        crate::application::redis::exec(
            services.connections.as_ref(),
            "live-redis",
            "SET".into(),
            vec!["rillsql:string:temp-token".into(), "短时令牌".into()],
            true,
        )
        .await?;
    }
    crate::application::redis::exec(services.connections.as_ref(), "live-redis", "EXPIRE".into(), vec!["rillsql:string:temp-token".into(), "3600".into()], true).await?;
    Ok(())
}

async fn open_redis(services: Arc<AppServices>, id: &str) -> Result<(), String> {
    let mut draft = ConnectionDraft::blank(Engine::Redis);
    draft.id = Some(id.to_owned());
    draft.name = "夹具 Redis".into();
    draft.host = "127.0.0.1".into();
    draft.port = "6379".into();
    draft.database = "0".into();
    draft.tls_mode = TlsMode::Off;
    draft.save_password = true;
    services.save_connection(draft, Some("rillsql-redis".into()), None, None).await.map_err(|err| err.message())?;
    let opened = services.open_connection(id, Some("rillsql-redis".into()), None, None).await?;
    if !opened.connected || services.connections.redis_session(id).is_none() {
        return Err(opened.report.summary.unwrap_or_else(|| "Redis 会话没有建立".into()));
    }
    Ok(())
}

fn fixture_unreachable(err: &str) -> bool {
    let text = err.to_ascii_lowercase();
    err.contains("没有打开会话")
        || err.contains("会话没有建立")
        || text.contains("connection refused")
        || text.contains("error connecting")
        || text.contains("os error 61")
        || text.contains("os error 10061")
}

fn block<T: Send + 'static>(work: impl std::future::Future<Output = T> + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    crate::app::views::grid::runtime_for_views().spawn(async move {
        let _ = tx.send(work.await);
    });
    rx.recv_timeout(Duration::from_secs(30)).expect("redis task")
}

struct RedisCleanup {
    services: Arc<AppServices>,
    id: String,
    keys: Vec<String>,
}

impl Drop for RedisCleanup {
    fn drop(&mut self) {
        let services = Arc::clone(&self.services);
        let id = self.id.clone();
        let keys = self.keys.clone();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            block(async move {
                let _ = crate::application::redis::delete_keys(services.connections.as_ref(), &id, keys, true).await;
            });
        }));
    }
}

// ------------------------------------------------------------------ R4：后台全量扫描、模板批量、集群（可选运行）

async fn open_redis_at(services: Arc<AppServices>, id: &str, port: u16, db: &str) -> Result<(), String> {
    let mut draft = ConnectionDraft::blank(Engine::Redis);
    draft.id = Some(id.to_owned());
    draft.name = format!("夹具 {port}");
    draft.host = "127.0.0.1".into();
    draft.port = port.to_string();
    draft.database = db.into();
    draft.tls_mode = TlsMode::Off;
    draft.save_password = true;
    services.save_connection(draft, Some("rillsql-redis".into()), None, None).await.map_err(|err| err.message())?;
    let opened = services.open_connection(id, Some("rillsql-redis".into()), None, None).await?;
    if !opened.connected || services.connections.redis_handle(id).is_none() {
        return Err(opened.report.summary.unwrap_or_else(|| "Redis 会话没有建立".into()));
    }
    Ok(())
}

/// 跑一轮扫描，收集事件。`stop_after` 为 Some(n) 时收到第 n 批后取消。
fn scan_once(
    services: &Arc<AppServices>,
    id: &str,
    resume: crate::adapters::redis::scan::ScanResume,
    pattern: &str,
    budget: usize,
    stop_after: Option<usize>,
) -> (Vec<String>, Option<u64>, crate::adapters::redis::scan::ScanResume, bool, bool) {
    use crate::adapters::redis::scan::{ScanEvent, ScanOptions};
    let services = Arc::clone(services);
    let id = id.to_owned();
    let pattern = pattern.to_owned();
    block(async move {
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let events = std::sync::Mutex::new(Vec::new());
        let batches = std::sync::atomic::AtomicUsize::new(0);
        let options = ScanOptions { pattern: Some(pattern), count: 200, budget };
        let flag = Arc::clone(&cancel);
        crate::application::redis::scan_keyspace(services.connections.as_ref(), &id, resume, options, cancel, |event| {
            if matches!(event, ScanEvent::Batch(_)) && stop_after.is_some_and(|n| batches.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1 >= n) {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            events.lock().unwrap().push(event);
            true
        })
        .await;
        let mut names = Vec::new();
        let mut total = None;
        let mut end = (Default::default(), false, false);
        for event in events.into_inner().unwrap() {
            match event {
                ScanEvent::Total(value) => total = value,
                ScanEvent::Batch(keys) => names.extend(keys.into_iter().map(|key| key.name)),
                ScanEvent::Finished { resume, cancelled, capped } => end = (resume, cancelled, capped),
                ScanEvent::Failed(message) => panic!("扫描失败：{message}"),
            }
        }
        (names, total, end.0, end.1, end.2)
    })
}

/// 2500 键逻辑库：后台连续 SCAN 读全（进度总数来自 DBSIZE），可取消且保留已读、可续扫，超过上限停下；
/// 模板把 2500 个订单键归并为一个模板，批量 UNLINK 分批删完。不使用 KEYS。
#[test]
fn live_scan_reads_2500_keys_cancels_resumes_caps_and_batch_deletes() {
    let dir = std::env::temp_dir().join(format!("rillsql-redis-scan-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
    if let Err(err) = block(open_redis_at(Arc::clone(&services), "live-scan", 6379, "9")) {
        let _ = std::fs::remove_dir_all(&dir);
        if fixture_unreachable(&err) {
            eprintln!("跳过：Redis 夹具没有连上：{err}");
            return;
        }
        panic!("Redis 夹具没有连上：{err}");
    }
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let prefix = format!("rillsql:t9:{suffix}");
    let names: Vec<String> = (0..2500).map(|index| format!("{prefix}:order:{index}")).collect();
    {
        let services = Arc::clone(&services);
        let names = names.clone();
        block(async move {
            let (handle, _) = services.connections.redis_handle("live-scan").expect("handle");
            let mut conn = handle.conn();
            for chunk in names.chunks(500) {
                let mut cmd = redis::cmd("MSET");
                for name in chunk {
                    cmd.arg(name).arg("v");
                }
                let _: () = cmd.query_async(&mut conn).await.expect("MSET");
            }
        });
    }
    let pattern = format!("{prefix}:*");
    let fresh = crate::adapters::redis::scan::ScanResume::default();
    let (all, total, resume, cancelled, capped) = scan_once(&services, "live-scan", fresh.clone(), &pattern, 200_000, None);
    let unique: std::collections::BTreeSet<_> = all.iter().cloned().collect();
    assert_eq!(unique.len(), 2500, "全部读到，没有 500 上限");
    assert!(total.is_some_and(|total| total >= 2500), "进度总数来自 DBSIZE：{total:?}");
    assert!(resume.finished() && !cancelled && !capped);

    let (part, _, resume, cancelled, _) = scan_once(&services, "live-scan", fresh.clone(), &pattern, 200_000, Some(1));
    assert!(cancelled, "取消生效");
    assert!(!part.is_empty() && part.len() < 2500, "取消后保留已读的部分：{}", part.len());
    let (rest, _, end, _, _) = scan_once(&services, "live-scan", resume, &pattern, 200_000, None);
    let resumed: std::collections::BTreeSet<_> = part.into_iter().chain(rest).collect();
    assert_eq!(resumed.len(), 2500, "从续扫位置继续能读全");
    assert!(end.finished());

    let (capped_keys, _, _, _, capped) = scan_once(&services, "live-scan", fresh, &pattern, 1000, None);
    assert!(capped, "超过上限停止");
    assert!(capped_keys.len() >= 1000 && capped_keys.len() < 2500, "{}", capped_keys.len());

    let indexed: Vec<(usize, &str, &str)> = names.iter().enumerate().map(|(index, name)| (index, name.as_str(), "string")).collect();
    let groups = crate::adapters::redis::keyspace::group_templates(&indexed, &[":".to_owned()], &[]);
    assert_eq!(groups.len(), 1, "{:?}", groups.iter().map(|group| &group.template).collect::<Vec<_>>());
    assert_eq!(groups[0].count, 2500);
    assert_eq!(groups[0].template, "rillsql:t9:*:order:*");

    let services_batch = Arc::clone(&services);
    let progress = block(async move {
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let steps = std::sync::atomic::AtomicUsize::new(0);
        let outcome = crate::application::redis::batch(services_batch.connections.as_ref(), "live-scan", crate::application::redis::BatchRequest { db: 9, keys: names, op: crate::adapters::redis::batch::BatchOp::Unlink }, true, cancel, |_| {
            steps.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            true
        })
        .await
        .expect("batch");
        (outcome, steps.into_inner())
    });
    assert_eq!(progress.0.done, 2500);
    assert_eq!(progress.0.failed, 0);
    assert!(progress.1 >= 5, "分批执行并报告进度：{}", progress.1);
    let (left, _, _, _, _) = scan_once(&services, "live-scan", Default::default(), &pattern, 200_000, None);
    assert!(left.is_empty(), "批量删除后没有剩余");
    services.connections.disconnect("live-scan");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Redis Cluster 夹具（docker compose 的 redis-cluster，7001-7003）：自动识别集群、逐主节点扫描合并、
/// 键详情的槽与节点、跨槽 MGET 的 CROSSSLOT 提示、诊断按节点汇总、按槽分组批量删除。
#[test]
fn live_cluster_scans_every_primary_explains_crossslot_and_batches_by_slot() {
    let dir = std::env::temp_dir().join(format!("rillsql-redis-cluster-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
    if let Err(err) = block(open_redis_at(Arc::clone(&services), "live-cluster", 7001, "0")) {
        let _ = std::fs::remove_dir_all(&dir);
        if fixture_unreachable(&err) {
            eprintln!("跳过：Redis Cluster 夹具没有连上：{err}");
            return;
        }
        panic!("Redis Cluster 夹具没有连上：{err}");
    }
    let (handle, _) = services.connections.redis_handle("live-cluster").expect("handle");
    assert!(handle.is_cluster(), "INFO cluster 自动识别为集群");
    let topology = handle.topology();
    assert_eq!(topology.masters().len(), 3, "{topology:?}");
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let names: Vec<String> = (0..300).map(|index| format!("rillsql:t9c:{suffix}:{index}")).collect();
    {
        let names = names.clone();
        let handle = handle.clone();
        block(async move {
            let mut conn = handle.conn();
            for name in &names {
                let _: () = redis::cmd("SET").arg(name).arg("v").query_async(&mut conn).await.expect("SET 自动路由");
            }
        });
    }
    let pattern = format!("rillsql:t9c:{suffix}:*");
    let (all, total, resume, _, _) = scan_once(&services, "live-cluster", Default::default(), &pattern, 200_000, None);
    let unique: std::collections::BTreeSet<_> = all.into_iter().collect();
    assert_eq!(unique.len(), 300, "键列表合并所有主节点");
    assert_eq!(resume.cursors.len(), 3, "逐主节点扫描");
    assert!(total.is_some_and(|total| total >= 300));
    let slots: std::collections::BTreeSet<_> = names.iter().map(|name| crate::adapters::redis::cluster::key_slot(name)).collect();
    assert!(slots.len() > 3, "样本键分布在多个槽");

    let services_read = Arc::clone(&services);
    let first = names[0].clone();
    let detail = block(async move { crate::application::redis::read_key(services_read.connections.as_ref(), "live-cluster", first).await }).expect("read");
    assert_eq!(detail.key_type, "string");
    let place = crate::adapters::redis::cluster::placement(&handle.topology().nodes, &names[0]);
    assert!(place.node.is_some(), "槽能对应到主节点");

    let services_exec = Arc::clone(&services);
    let (a, b) = (names[0].clone(), names.iter().find(|name| crate::adapters::redis::cluster::key_slot(name) != crate::adapters::redis::cluster::key_slot(&names[0])).cloned().unwrap());
    let refused = block(async move { crate::application::redis::exec(services_exec.connections.as_ref(), "live-cluster", "MGET".into(), vec![a, b], false).await });
    assert!(refused.as_ref().err().is_some_and(|message| message.contains("CROSSSLOT")), "{:?}", refused.map(|outcome| outcome.command));

    let services_diag = Arc::clone(&services);
    let (info, clients) = block(async move {
        let info = crate::application::redis::info(services_diag.connections.as_ref(), "live-cluster", String::new()).await;
        let clients = crate::application::redis::clients(services_diag.connections.as_ref(), "live-cluster", String::new()).await;
        (info, clients)
    });
    assert!(info.expect("info").iter().any(|section| section.name == "Cluster"));
    let clients = clients.expect("clients");
    let nodes: std::collections::BTreeSet<_> = clients.iter().map(|client| client.node.clone()).collect();
    assert_eq!(nodes.len(), 3, "客户端列表按节点汇总");

    let services_batch = Arc::clone(&services);
    let outcome = block(async move {
        crate::application::redis::batch(services_batch.connections.as_ref(), "live-cluster", crate::application::redis::BatchRequest { db: 0, keys: names, op: crate::adapters::redis::batch::BatchOp::Unlink }, true, Arc::new(std::sync::atomic::AtomicBool::new(false)), |_| true).await
    })
    .expect("batch");
    assert_eq!((outcome.done, outcome.failed), (300, 0), "按槽分组的 UNLINK 不触发 CROSSSLOT");
    services.connections.disconnect("live-cluster");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 批量操作绑定发起时的逻辑库：主连接（多路复用，同一条 socket）上随后发生的 SELECT 不会让剩余批次落到别的库。
/// 回归：以前批量操作共用主连接，切库后同名键在新库被删。
#[test]
fn live_batch_stays_on_its_db_after_the_shared_connection_selects_another() {
    let dir = std::env::temp_dir().join(format!("rillsql-redis-batch-db-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
    if let Err(err) = block(open_redis_at(Arc::clone(&services), "live-batch-db", 6379, "10")) {
        let _ = std::fs::remove_dir_all(&dir);
        if fixture_unreachable(&err) {
            eprintln!("跳过：Redis 夹具没有连上：{err}");
            return;
        }
        panic!("Redis 夹具没有连上：{err}");
    }
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let names: Vec<String> = (0..1200).map(|index| format!("rillsql:t9db:{suffix}:{index}")).collect();
    let (handle, _) = services.connections.redis_handle("live-batch-db").expect("handle");
    let count_in = |db: i64, names: Vec<String>| {
        let handle = handle.clone();
        block(async move {
            let mut conn = handle.pinned(db).await.expect("pinned");
            let mut cmd = redis::cmd("EXISTS");
            for name in &names {
                cmd.arg(name);
            }
            cmd.query_async::<i64>(&mut conn).await.expect("EXISTS")
        })
    };
    {
        let names = names.clone();
        let handle = handle.clone();
        block(async move {
            for db in [10, 11] {
                let mut conn = handle.pinned(db).await.expect("pinned");
                for chunk in names.chunks(500) {
                    let mut cmd = redis::cmd("MSET");
                    for name in chunk {
                        cmd.arg(name).arg("v");
                    }
                    let _: () = cmd.query_async(&mut conn).await.expect("MSET");
                }
            }
            // 共用的主连接切到 11 号库（相当于界面切库或控制台 SELECT）。
            let mut shared = handle.conn();
            let _: () = redis::cmd("SELECT").arg(11).query_async(&mut shared).await.expect("SELECT");
        });
    }
    assert_eq!(count_in(10, names.clone()), 1200);
    assert_eq!(count_in(11, names.clone()), 1200);

    let services_batch = Arc::clone(&services);
    let batch_names = names.clone();
    let outcome = block(async move {
        crate::application::redis::batch(
            services_batch.connections.as_ref(),
            "live-batch-db",
            crate::application::redis::BatchRequest { db: 10, keys: batch_names, op: crate::adapters::redis::batch::BatchOp::Unlink },
            true,
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            |_| true,
        )
        .await
    })
    .expect("batch");
    assert_eq!((outcome.done, outcome.failed), (1200, 0));
    assert_eq!(count_in(10, names.clone()), 0, "发起时的 10 号库被删完");
    assert_eq!(count_in(11, names.clone()), 1200, "主连接所在的 11 号库同名键不受影响");

    let cleanup = names.clone();
    let handle_cleanup = handle.clone();
    block(async move {
        let mut conn = handle_cleanup.pinned(11).await.expect("pinned");
        for chunk in cleanup.chunks(500) {
            let mut cmd = redis::cmd("UNLINK");
            for name in chunk {
                cmd.arg(name);
            }
            let _: i64 = cmd.query_async(&mut conn).await.expect("UNLINK");
        }
    });
    services.connections.disconnect("live-batch-db");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 集群节点公告的地址不可达（模拟 NAT / 容器内地址）：连接照常打开，退回种子节点上的单节点会话并说明原因。
#[test]
fn live_cluster_with_unreachable_announced_nodes_falls_back_to_the_seed() {
    let dir = std::env::temp_dir().join(format!("rillsql-redis-cluster-fallback-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
    let id = format!("live-cluster-fallback-{}", uuid::Uuid::new_v4().simple());
    crate::adapters::redis::connect::UNREACHABLE_ANNOUNCE.lock().unwrap().push(id.clone());
    let id_open = id.clone();
    let services_open = Arc::clone(&services);
    let opened = block(async move { open_redis_at(services_open, &id_open, 7001, "0").await });
    crate::adapters::redis::connect::UNREACHABLE_ANNOUNCE.lock().unwrap().retain(|item| item != &id);
    if let Err(err) = opened {
        let _ = std::fs::remove_dir_all(&dir);
        if fixture_unreachable(&err) {
            eprintln!("跳过：Redis Cluster 夹具没有连上：{err}");
            return;
        }
        panic!("集群建连失败时应退回单节点会话，而不是打开失败：{err}");
    }
    let (handle, _) = services.connections.redis_handle(&id).expect("handle");
    assert!(!handle.is_cluster(), "退回单节点会话");
    let topology = handle.topology();
    assert!(
        matches!(&topology.fallback, Some(crate::adapters::redis::conn::ClusterFallback::Unreachable(reason)) if !reason.is_empty()),
        "{topology:?}"
    );
    let services_read = Arc::clone(&services);
    let id_read = id.clone();
    let pong = block(async move {
        crate::application::redis::exec(services_read.connections.as_ref(), &id_read, "PING".into(), Vec::new(), false).await
    })
    .expect("单节点会话可用");
    assert_eq!(pong.command, "PING");
    services.connections.disconnect(&id);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 用户反馈：控制台连续执行。回车执行后输入框清空，下一条命令照样回车执行，结果按顺序追加；
/// 出错的命令留在输入框里方便改。
#[gpui_kit::test]
fn console_runs_commands_back_to_back(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-redis-console-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
    if let Err(err) = block(open_redis(Arc::clone(&services), "live-console")) {
        let _ = std::fs::remove_dir_all(&dir);
        if fixture_unreachable(&err) {
            eprintln!("跳过：Redis 夹具没有连上：{err}");
            return;
        }
        panic!("Redis 夹具没有连上：{err}");
    }
    let (window, view) = open_pane(cx, Arc::clone(&services));
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |pane, cx| {
            pane.open_spec("live-console|夹具 Redis|0", cx);
            pane.command("redis-page:console", cx);
        });
        window.render_frame(cx);
    })
    .unwrap();
    for (round, (command, ok)) in [("PING", true), ("TIME", true), ("NOSUCHCOMMAND x", false), ("DBSIZE", true)].into_iter().enumerate() {
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("redis-command", cx);
            if !ok || round == 3 {
                // 上一条失败的命令还在输入框里：先清掉。
                view.update(cx, |pane, _| pane.set_command(""));
                window.render_frame(cx);
            }
            window.input(command, cx);
            window.press("enter", cx);
            window.render_frame(cx);
        })
        .unwrap();
        assert!(pump(cx, window, &view, |pane| !pane.running() && pane.console_log().len() > round), "第 {} 条命令回车后没有执行", round + 1);
        cx.update(|cx| {
            let pane = view.read(cx);
            let entry = pane.console_log().back().unwrap();
            assert_eq!(entry.line, command);
            assert_eq!(entry.ok, ok, "{}", entry.output);
            if ok {
                assert_eq!(pane.command_input(), "", "执行成功后输入框清空");
            } else {
                assert_eq!(pane.command_input(), command, "失败的命令留着方便改");
            }
        });
    }
    services.disconnect("live-console");
    let _ = std::fs::remove_dir_all(&dir);
}
