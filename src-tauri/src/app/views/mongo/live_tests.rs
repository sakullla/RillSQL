//! 真实 MongoDB 夹具上的集合文档。连接走 AppServices，只删除带 t8mark 的临时文档。

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::{Point, TestAppContext, WindowBounds, WindowOptions, px, size};
use gpui_kit::test::TestWindowExt;

use super::*;
use crate::application::AppServices;
use crate::connections::model::{ConnectionDraft, Engine, TlsMode};

#[gpui_kit::test]
fn rendered_controls_keep_invalid_json_and_unconfirmed_delete(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-mongo-pane-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
    let (window, view) = open_pane(cx, services);
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |pane, _| {
            pane.plant_documents(
                "rillsql",
                "customers",
                vec![MongoDoc { id_json: r#""seed""#.into(), json: r#"{"_id":"seed","姓名":"林夏","等级":"gold"}"#.into() }],
            );
        });
        window.render_frame(cx);
        assert_eq!(window.find("mongo-filter").label(), Some("过滤"));
        assert_eq!(window.find("mongo-refresh").label(), Some("刷新"));
        assert_eq!(window.find("mongo-prev").label(), Some("上一页"));
        assert_eq!(window.find("mongo-next").label(), Some("下一页"));
        assert_eq!(window.find("mongo-insert").label(), Some("新增文档"));
        assert_eq!(window.find("mongo-view:0").label(), Some("查看 0"));
        assert_eq!(window.find("mongo-copy:0").label(), Some("复制 JSON 0"));
        assert_eq!(window.find("mongo-edit:0").label(), Some("编辑 0"));
        assert_eq!(window.find("mongo-delete:0").label(), Some("删除 0"));
        assert!(window.find("mongo-status").label().unwrap_or("").contains("姓名"), "动态字段没有画出来");
        window.click("mongo-view:0", cx);
        window.click("mongo-copy:0", cx);
        window.click("mongo-insert", cx);
        view.update(cx, |pane, _| pane.set_editor("{"));
        window.render_frame(cx);
        window.click("mongo-save", cx);
    })
    .unwrap();
    cx.update(|app| {
        let pane = view.read(app);
        assert!(pane.error().contains("语法"), "{}", pane.error());
        assert!(pane.note().is_empty(), "{}", pane.note());
        assert_eq!(pane.editor_text(), "{");
        assert!(pane.editor_open());
        assert!(pane.view_json().contains("林夏"), "{}", pane.view_json());
        assert!(pane.copied().contains("林夏"), "{}", pane.copied());
        assert!(pane.columns().iter().any(|column| column == "姓名"));
    });
    cx.update_window(window, |_, window, cx| {
        window.click("mongo-delete:0", cx);
        window.render_frame(cx);
        window.click("mongo-cancel", cx);
    })
    .unwrap();
    cx.update(|app| {
        let pane = view.read(app);
        assert_eq!(pane.editor_text(), "{");
        assert!(pane.editor_open());
        assert!(!pane.confirm_open());
        assert_eq!(pane.documents().len(), 1);
        assert!(pane.note().is_empty(), "{}", pane.note());
    });
    let _ = std::fs::remove_dir_all(&dir);
}

#[gpui_kit::test]
fn sidebar_mongo_button_opens_the_browser(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-mongo-side-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
    let (window, view) = open_workbench(cx, services);
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |workbench, cx| {
            workbench.model.connections.rows.push(crate::connections::ConnectionRecord {
                saved: crate::connections::model::SavedConnection {
                    id: "docs".into(),
                    name: "文档".into(),
                    engine: Engine::Mongodb,
                    env: "local".into(),
                    host: Some("127.0.0.1".into()),
                    port: Some(27017),
                    database: Some("admin".into()),
                    username: Some("root".into()),
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
        assert_eq!(window.find("side-open-mongo-docs").label(), Some("集合文档 文档"));
        window.click("side-open-mongo-docs", cx);
        window.render_frame(cx);
        assert_eq!(window.find("mongo-insert").label(), Some("新增文档"));
    })
    .unwrap();
    assert!(pump_status(cx, window, |text| text.contains("还没有打开")), "侧栏打开后，未连接不应显示成功");
    let status = status_label(cx, window);
    assert!(!status.contains("note=已新增"), "{status}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[gpui_kit::test]
fn live_mongo_pages_filters_inserts_replaces_and_deletes(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-mongo-live-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
    let mark = format!("t8-{}", uuid::Uuid::new_v4().simple());
    if let Err(err) = block(open_mongo(Arc::clone(&services), "live-mongo")) {
        if fixture_unreachable(&err) {
            let _ = std::fs::remove_dir_all(&dir);
            eprintln!("跳过：MongoDB 夹具没有连上：{err}");
            return;
        }
        panic!("MongoDB 夹具没有连上：{err}");
    }
    let _cleanup = MongoCleanup { services: Arc::clone(&services), id: "live-mongo".into(), mark: mark.clone() };
    let (window, view) = open_pane(cx, Arc::clone(&services));
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |pane, cx| pane.open_spec("live-mongo|夹具 MongoDB", cx));
        window.render_frame(cx);
    })
    .unwrap();
    assert!(pump(cx, window, &view, |pane| !pane.running() && (!pane.databases().is_empty() || !pane.error().is_empty())), "库清单没有返回");
    assert_quiet(cx, &view);
    click_named(cx, window, &view, true, "rillsql");
    assert!(pump(cx, window, &view, |pane| !pane.running() && (!pane.collections().is_empty() || !pane.error().is_empty())), "集合清单没有返回");
    assert_quiet(cx, &view);
    cx.update(|app| {
        let names = view.read(app).collections().iter().map(|collection| collection.name.clone()).collect::<Vec<_>>().join(",");
        assert!(names.contains("customers") && names.contains("orders"), "{names}");
    });
    click_named(cx, window, &view, false, "customers");
    assert!(pump(cx, window, &view, |pane| !pane.running() && (!pane.documents().is_empty() || !pane.error().is_empty())), "customers 没有返回");
    assert_quiet(cx, &view);
    cx.update(|app| {
        let pane = view.read(app);
        let blob = pane.status_line();
        assert!(blob.contains("林夏") && blob.contains("周宁") && blob.contains("张伟"), "{blob}");
        assert!(pane.columns().iter().any(|column| column == "姓名"), "{blob}");
    });
    cx.update_window(window, |_, window, cx| {
        window.click("mongo-view:0", cx);
        window.click("mongo-copy:0", cx);
    })
    .unwrap();
    cx.update(|app| {
        let pane = view.read(app);
        assert!(!pane.view_json().is_empty(), "{}", pane.status_line());
        assert_eq!(pane.copied(), pane.view_json());
        assert!(pane.note().contains("复制"), "{}", pane.note());
    });
    click_named(cx, window, &view, false, "orders");
    assert!(pump(cx, window, &view, |pane| !pane.running() && (pane.total() >= 25 || !pane.error().is_empty())), "orders 没有返回");
    assert_quiet(cx, &view);
    cx.update(|app| {
        let pane = view.read(app);
        assert_eq!(pane.total(), 25, "{}", pane.status_line());
        assert_eq!(pane.documents().len(), 20, "{}", pane.status_line());
        assert_eq!(pane.skip(), 0);
    });
    cx.update_window(window, |_, window, cx| window.click("mongo-next", cx)).unwrap();
    assert!(pump(cx, window, &view, |pane| !pane.running() && (pane.skip() == 20 || !pane.error().is_empty())), "下一页没有返回");
    assert_quiet(cx, &view);
    cx.update(|app| {
        let pane = view.read(app);
        assert_eq!(pane.skip(), 20);
        assert_eq!(pane.documents().len(), 5, "{}", pane.status_line());
    });
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |pane, _| pane.set_filter(r#"{"status":"active"}"#));
        window.render_frame(cx);
        window.click("mongo-filter", cx);
    })
    .unwrap();
    assert!(pump(cx, window, &view, |pane| !pane.running() && (pane.applied_filter() == r#"{"status":"active"}"# || !pane.error().is_empty())), "服务端过滤没有返回");
    assert_quiet(cx, &view);
    cx.update(|app| {
        let pane = view.read(app);
        assert_eq!(pane.total(), 14, "{}", pane.status_line());
        assert!(pane.documents().iter().all(|doc| doc.json.contains("\"status\":\"active\"")), "{}", pane.status_line());
        assert!(pane.columns().iter().any(|column| column == "status"));
    });
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |pane, _| pane.set_filter("{"));
        window.render_frame(cx);
        window.click("mongo-filter", cx);
    })
    .unwrap();
    assert!(pump(cx, window, &view, |pane| !pane.running() && !pane.error().is_empty()), "非法过滤没有失败");
    cx.update(|app| {
        let pane = view.read(app);
        // 一次读失败只有一处呈现：数据区错误态里那句错误，标题仍是"读取文档失败"。
        assert!(pane.error().contains("语法"), "{}", pane.status_line());
        assert!(pane.read_failed(), "非法过滤要走读文档的失败路径：{}", pane.status_line());
        assert_eq!(pane.error_title(), "mongo.tab.loadFailed", "{}", pane.status_line());
        assert!(pane.note().is_empty(), "{}", pane.note());
        assert_eq!(pane.filter_input(), "{");
        assert_eq!(pane.applied_filter(), r#"{"status":"active"}"#);
        assert_eq!(pane.total(), 14);
    });
    click_named(cx, window, &view, false, "customers");
    assert!(pump(cx, window, &view, |pane| !pane.running() && pane.collection() == "customers" && (!pane.documents().is_empty() || !pane.error().is_empty())), "回到 customers 失败");
    assert_quiet(cx, &view);
    let insert = format!(r#"{{"t8mark":"{mark}","姓名":"临时"}}"#);
    cx.update_window(window, |_, window, cx| {
        window.click("mongo-insert", cx);
        view.update(cx, |pane, _| pane.set_editor("{"));
        window.render_frame(cx);
        window.click("mongo-save", cx);
    })
    .unwrap();
    cx.update(|app| {
        let pane = view.read(app);
        assert!(pane.error().contains("语法"), "{}", pane.error());
        assert_eq!(pane.editor_text(), "{");
        assert!(pane.note().is_empty());
    });
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |pane, _| pane.set_editor(&insert));
        window.render_frame(cx);
        window.click("mongo-save", cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|app| {
        let pane = view.read(app);
        assert!(pane.confirm_open(), "新增文档必须先预览确认：{}", pane.status_line());
        assert!(pane.confirm_prompt().contains("新增"), "{}", pane.confirm_prompt());
    });
    cx.update_window(window, |_, window, cx| window.click("mongo-confirm", cx)).unwrap();
    assert!(pump(cx, window, &view, |pane| !pane.running() && (pane.note().contains("新增") || !pane.error().is_empty())), "新增没有返回");
    assert_quiet(cx, &view);
    let filter = format!(r#"{{"t8mark":"{mark}"}}"#);
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |pane, _| pane.set_filter(&filter));
        window.render_frame(cx);
        window.click("mongo-filter", cx);
    })
    .unwrap();
    assert!(pump(cx, window, &view, |pane| !pane.running() && (pane.applied_filter() == filter || !pane.error().is_empty())), "临时文档过滤没有返回");
    assert_quiet(cx, &view);
    cx.update(|app| {
        let pane = view.read(app);
        assert_eq!(pane.documents().len(), 1, "{}", pane.status_line());
        assert!(pane.status_line().contains("临时"), "{}", pane.status_line());
    });
    let replaced = format!(r#"{{"t8mark":"{mark}","姓名":"临时已改","t8flag":true}}"#);
    cx.update_window(window, |_, window, cx| {
        window.click("mongo-edit:0", cx);
        view.update(cx, |pane, _| pane.set_editor(&replaced));
        window.render_frame(cx);
        window.click("mongo-save", cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|app| {
        let pane = view.read(app);
        assert!(pane.confirm_open(), "覆盖文档必须先预览确认：{}", pane.status_line());
        assert!(pane.confirm_prompt().contains("覆盖"), "{}", pane.confirm_prompt());
    });
    cx.update_window(window, |_, window, cx| window.click("mongo-confirm", cx)).unwrap();
    assert!(pump(cx, window, &view, |pane| !pane.running() && (pane.note().contains("覆盖") || !pane.error().is_empty())), "替换没有返回");
    assert_quiet(cx, &view);
    cx.update_window(window, |_, window, cx| window.click("mongo-refresh", cx)).unwrap();
    assert!(pump(cx, window, &view, |pane| !pane.running() && (pane.status_line().contains("临时已改") || !pane.error().is_empty())), "刷新后没有看到替换");
    assert_quiet(cx, &view);
    cx.update(|app| {
        let pane = view.read(app);
        assert!(pane.columns().iter().any(|column| column == "t8flag"), "{}", pane.status_line());
        assert!(pane.status_line().contains("临时已改"), "{}", pane.status_line());
    });
    cx.update_window(window, |_, window, cx| {
        window.click("mongo-delete:0", cx);
        window.render_frame(cx);
        window.click("mongo-cancel", cx);
    })
    .unwrap();
    cx.update(|app| {
        let pane = view.read(app);
        assert!(!pane.confirm_open());
        assert!(pane.note().is_empty() || pane.note().contains("覆盖"), "{}", pane.note());
        assert_eq!(pane.documents().len(), 1);
    });
    cx.update_window(window, |_, window, cx| {
        window.click("mongo-delete:0", cx);
        window.render_frame(cx);
        window.click("mongo-confirm", cx);
    })
    .unwrap();
    assert!(pump(cx, window, &view, |pane| !pane.running() && (pane.note().contains("删除") || !pane.error().is_empty())), "删除没有返回");
    assert_quiet(cx, &view);
    cx.update_window(window, |_, window, cx| window.click("mongo-refresh", cx)).unwrap();
    assert!(pump(cx, window, &view, |pane| !pane.running()), "删除后的刷新没有返回");
    assert_quiet(cx, &view);
    cx.update(|app| assert!(view.read(app).documents().is_empty(), "{}", view.read(app).status_line()));
    cx.update_window(window, |_, window, cx| window.click("mongo-clear-filter", cx)).unwrap();
    assert!(pump(cx, window, &view, |pane| !pane.running() && pane.applied_filter().is_empty()), "清除过滤没有返回");
    assert_quiet(cx, &view);
    cx.update(|app| {
        let pane = view.read(app);
        let docs = pane.documents().iter().map(|doc| doc.json.clone()).collect::<Vec<_>>().join("\n");
        assert!(docs.contains("林夏") && docs.contains("周宁") && docs.contains("张伟"), "{}", pane.status_line());
        assert!(!docs.contains(&mark), "{}", pane.status_line());
        assert_eq!(pane.total(), 3, "{}", pane.status_line());
    });
    services.disconnect("live-mongo");
    let failed = format!(r#"{{"t8mark":"{mark}","姓名":"不该写入"}}"#);
    cx.update_window(window, |_, window, cx| {
        window.click("mongo-insert", cx);
        view.update(cx, |pane, _| pane.set_editor(&failed));
        window.render_frame(cx);
        window.click("mongo-save", cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.update_window(window, |_, window, cx| window.click("mongo-confirm", cx)).unwrap();
    assert!(pump(cx, window, &view, |pane| !pane.running() && !pane.error().is_empty()), "断线写入没有失败");
    cx.update(|app| {
        let pane = view.read(app);
        assert!(pane.error().contains("还没有打开") || !pane.error().is_empty(), "{}", pane.error());
        assert!(pane.note().is_empty(), "{}", pane.note());
        assert_eq!(pane.editor_text(), failed);
        assert!(pane.editor_open());
    });
    let _ = std::fs::remove_dir_all(&dir);
}

fn click_named(cx: &mut TestAppContext, window: gpui_kit::AnyWindowHandle, view: &gpui_kit::Entity<MongoPane>, database: bool, name: &str) {
    let index = cx.update(|app| {
        let pane = view.read(app);
        if database { pane.databases().iter().position(|item| item.name == name) } else { pane.collections().iter().position(|item| item.name == name) }
    });
    let index = index.unwrap_or_else(|| panic!("没有找到 {name}"));
    let id = if database { format!("mongo-database:{index}") } else { format!("mongo-collection:{index}") };
    cx.update_window(window, |_, window, cx| window.click(id, cx)).unwrap();
}

fn assert_quiet(cx: &mut TestAppContext, view: &gpui_kit::Entity<MongoPane>) {
    let (error, note) = cx.update(|app| {
        let pane = view.read(app);
        (pane.error().to_owned(), pane.note().to_owned())
    });
    assert!(error.is_empty(), "{error} / {note}");
}

fn pump(cx: &mut TestAppContext, window: gpui_kit::AnyWindowHandle, view: &gpui_kit::Entity<MongoPane>, mut ready: impl FnMut(&MongoPane) -> bool) -> bool {
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
        window.find("mongo-status").label().unwrap_or("").to_owned()
    })
    .unwrap_or_default()
}

fn open_pane(cx: &mut TestAppContext, services: Arc<AppServices>) -> (gpui_kit::AnyWindowHandle, gpui_kit::Entity<MongoPane>) {
    cx.update(gpui_kit::init);
    cx.update(|cx| {
        let bounds = gpui_kit::Bounds { origin: Point::default(), size: size(px(1400.), px(12000.)) };
        let (window, view) = gpui_kit::open_window(
            WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
            cx,
            |_, cx| cx.new(|_| MongoPane::new(services)),
        )
        .expect("mongo pane");
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

async fn open_mongo(services: Arc<AppServices>, id: &str) -> Result<(), String> {
    let mut draft = ConnectionDraft::blank(Engine::Mongodb);
    draft.id = Some(id.to_owned());
    draft.name = "夹具 MongoDB".into();
    draft.host = "127.0.0.1".into();
    draft.port = "27017".into();
    draft.database = "admin".into();
    draft.username = "root".into();
    draft.tls_mode = TlsMode::Off;
    draft.save_password = true;
    services.save_connection(draft, Some("rillsql-mongo".into()), None, None).await.map_err(|err| err.message())?;
    let opened = services.open_connection(id, Some("rillsql-mongo".into()), None, None).await?;
    if !opened.connected || services.connections.mongo_session(id).is_none() {
        return Err(opened.report.summary.unwrap_or_else(|| "MongoDB 会话没有建立".into()));
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
    rx.recv_timeout(Duration::from_secs(30)).expect("mongo task")
}

struct MongoCleanup {
    services: Arc<AppServices>,
    id: String,
    mark: String,
}

impl Drop for MongoCleanup {
    fn drop(&mut self) {
        let services = Arc::clone(&self.services);
        let id = self.id.clone();
        let mark = self.mark.clone();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            block(async move {
                let filter = format!(r#"{{"t8mark":"{mark}"}}"#);
                if let Ok(page) = crate::application::mongo::find_documents(services.connections.as_ref(), &id, "rillsql", "customers", Some(0), Some(50), Some(filter)).await {
                    for doc in page.documents {
                        let _ = crate::application::mongo::delete_document(services.connections.as_ref(), &id, "rillsql", "customers", &doc.id_json, true).await;
                    }
                }
            });
        }));
    }
}
