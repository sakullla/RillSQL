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
        assert_eq!(window.find("redis-more").label(), Some("再读取一批（SCAN）"));
        assert_eq!(window.find("redis-ttl").label(), Some("设置 TTL"));
        assert_eq!(window.find("redis-rename").label(), Some("重命名"));
        assert_eq!(window.find("redis-delete").label(), Some("删除"));
        assert_eq!(window.find("redis-exec").label(), Some("执行"));
        assert_eq!(window.find("redis-copy").label(), Some("值"));
        window.click("redis-pattern", cx);
        window.input("ab", cx);
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
        assert!(pane.error().contains("KEYS") || pane.error().contains("禁用"), "{}", pane.error());
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
    let _cleanup = RedisCleanup { services: Arc::clone(&services), id: "live-redis".into(), keys: vec![key.clone(), renamed.clone()] };
    if let Err(err) = block(open_redis(Arc::clone(&services), "live-redis")) {
        panic!("Redis 夹具没有连上：{err}");
    }
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
        window.click("redis-filter", cx);
    })
    .unwrap();
    assert!(pump_more(cx, window, &view, &key), "确认写入后 SCAN 没有看到临时键");
    read_key(cx, window, &view, &key, "hello-t8");
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |pane, _| pane.set_ttl("90"));
        window.render_frame(cx);
        window.click("redis-ttl", cx);
    })
    .unwrap();
    assert!(pump(cx, window, &view, |pane| !pane.running() && pane.detail_ttl().is_some_and(|ttl| ttl > 0 && ttl <= 90_000)), "TTL 没有写成 90 秒");
    assert_quiet(cx, &view);
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |pane, _| pane.set_ttl(""));
        window.render_frame(cx);
        window.click("redis-ttl", cx);
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
    assert!(pump(cx, window, &view, |pane| !pane.running() && !pane.error().is_empty()), "断线后的 PING 没有失败");
    cx.update(|app| {
        let pane = view.read(app);
        assert!(pane.error().contains("还没有打开") || !pane.error().is_empty(), "{}", pane.error());
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
    let _cleanup = RedisCleanup {
        services: Arc::clone(&services),
        id: "live-redis-snap".into(),
        keys: vec![key_a.clone(), key_b.clone(), key_c.clone(), key_d.clone(), key_e.clone()],
    };
    if let Err(err) = block(open_redis(Arc::clone(&services), "live-redis-snap")) {
        panic!("Redis 夹具没有连上：{err}");
    }
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
        (window.into(), view)
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
        (window.into(), view)
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
