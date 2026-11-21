//! 外壳、连接、历史、设置和关闭守卫。不连接远程数据库。

use std::sync::Arc;

use crate::app::shell::jobs::{self, apply_done, job_request};
use crate::app::shell::menus::{menu_items, MenuPlace, MenuTarget, NavKey};
use crate::app::views::connections::{sample_draft, with_tls_ssh, Secrets};
use crate::app::views::history::{HistorySection, ListPhase};
use crate::app::{dictionary_keys, translate, AppPage, Density, Locale, ThemePreference};
use crate::application::AppServices;
use crate::connections::model::{CaMode, Engine, SavedConnection, SshAuthKind};
use crate::connections::{ConnectionRecord, Service};
use crate::i18n;
use crate::query::{HistoryEntry, SaveRequest};
use crate::state::{AppState, SessionState, ShellSettings};

use super::conn_modal::ModalControl;
use super::model::{DraftField, PendingJob, WorkItemKind, WorkTool, WorkbenchModel};
use crate::app::views::connections::LinkStatus;
use crate::app::Shell;
use gpui_kit::AppContext as _;
use gpui_kit::test::TestWindowExt;

fn services_at(dir: &std::path::Path) -> Arc<AppServices> {
    Arc::new(AppServices::new(Arc::new(Service::new_for_test(dir.join("connections.sqlite")))))
}

fn request_of(model: &mut WorkbenchModel, token: crate::application::tasks::TaskToken) -> jobs::JobRequest {
    let job = model.take_pending().expect("pending");
    job_request(model, job, token)
}

#[tokio::test]
async fn dbeaver_import_jobs_preview_cancel_select_and_report() {
    use super::conn_import::{ImportResult, ImportState, ProfileFormat};

    let dir = std::env::temp_dir().join(format!("rillsql-import-preview-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let mut model = WorkbenchModel::new(Shell::default());
    let file = dir.join("data-sources.json");
    let a = dir.join("a.sqlite");
    let b = dir.join("b.sqlite");
    let c = dir.join("c.sqlite");
    let fixture = serde_json::json!({
        "connections": {
            "1": {"provider":"sqlite","driver":"sqlite_jdbc","name":"First",
                "configuration":{"database":a}},
            "2": {"provider":"sqlite","driver":"sqlite_jdbc","name":"Second",
                "configuration":{"database":b}},
            "3": {"provider":"oracle","driver":"oracle_thin","name":"Unsupported",
                "configuration":{}},
            "4": {"provider":"sqlite","driver":"sqlite_jdbc","name":"Retry",
                "configuration":{"database":c}}
        }
    });
    std::fs::write(&file, serde_json::to_vec(&fixture).unwrap()).unwrap();

    model.queue_profile_preview(ProfileFormat::DBeaver, file.clone());
    finish_import_job(&mut model, &services).await;
    assert!(matches!(model.profile_import, ImportState::Preview(_)));
    let _ = model.dispatch("profile-import-close");
    assert!(matches!(model.profile_import, ImportState::Idle));
    assert!(services.list_connections().await.unwrap().is_empty());
    assert!(model.take_pending().is_none());

    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(dir.join("connections.sqlite")))
        .await
        .unwrap();
    sqlx::query("CREATE TRIGGER fail_row BEFORE INSERT ON connections WHEN NEW.name = 'Retry' BEGIN SELECT RAISE(ABORT, 'fixture'); END")
        .execute(&pool)
        .await
        .unwrap();
    model.queue_profile_preview(ProfileFormat::DBeaver, file.clone());
    finish_import_job(&mut model, &services).await;
    let _ = model.dispatch("profile-import-toggle:1");
    let _ = model.dispatch("profile-import-confirm");
    let _ = model.dispatch("profile-import-confirm");
    assert_eq!(model.pending_len(), 1);
    finish_import_job(&mut model, &services).await;
    let ImportState::Report(report) = &model.profile_import else {
        panic!("missing per-row import report");
    };
    assert_eq!(report.counts(), (1, 2, 1));
    assert!(matches!(report.rows[1].result, ImportResult::Unchecked));
    assert!(matches!(report.rows[2].result, ImportResult::Invalid(_)));
    assert!(matches!(report.rows[3].result, ImportResult::Failed));
    assert_eq!(model.take_pending(), Some(PendingJob::RefreshConnections));
    let saved = services.list_connections().await.unwrap();
    assert_eq!(saved[0].saved.file_path.as_deref(), a.to_str());
    assert!(!a.exists() && !b.exists() && !c.exists(), "import opened a target SQLite database");

    sqlx::query("DROP TRIGGER fail_row").execute(&pool).await.unwrap();
    pool.close().await;
    let _ = model.dispatch("profile-import-close");
    model.queue_profile_preview(ProfileFormat::DBeaver, file);
    finish_import_job(&mut model, &services).await;
    let ImportState::Preview(preview) = &model.profile_import else {
        panic!("missing repeat import preview");
    };
    assert!(preview.rows[0].conflict.is_some());
    assert!(!preview.rows[0].checked);
    let _ = model.dispatch("profile-import-toggle:1");
    let _ = model.dispatch("profile-import-confirm");
    finish_import_job(&mut model, &services).await;
    let ImportState::Report(report) = &model.profile_import else {
        panic!("missing import report on retry");
    };
    assert_eq!(report.counts(), (1, 3, 0));
    assert!(matches!(report.rows[0].result, ImportResult::Conflict(_)));
    assert_eq!(services.list_connections().await.unwrap().len(), 2);
    drop(services);
    let _ = std::fs::remove_dir_all(dir);
}

async fn finish_import_job(model: &mut WorkbenchModel, services: &AppServices) {
    let token = model.begin_owner(&services.tasks, "profile-import");
    let request = request_of(model, token);
    let done = jobs::execute_job(services, request).await;
    apply_done(model, services, "profile-import", done);
    model.finish_job();
}

#[tokio::test]
async fn navicat_jobs_preserve_the_selected_subset_and_skip_existing_profiles() {
    use super::conn_import::{ImportState, ProfileFormat};

    let dir = std::env::temp_dir().join(format!("rillsql-ncx-preview-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let mut model = WorkbenchModel::new(Shell::default());
    let path = dir.join("connections.ncx");
    std::fs::write(&path, r#"<Connections Ver="1.5">
        <Connection ConnectionName="PG" ConnType="POSTGRESQL" Host="db.example.invalid" Port="5432" Database="sales" UserName="a" SSL="false" SSH="false" HTTP="false"/>
        <Connection ConnectionName="Another" ConnType="MYSQL" Host="other.example.invalid" Port="3306" UserName="b" SSL="false" SSH="false" HTTP="false"/>
        <Connection ConnectionName="Unavailable" ConnType="ORACLE"/>
    </Connections>"#).unwrap();
    for (pass, expected) in [(0, (1, 2, 0)), (1, (0, 3, 0))] {
        model.queue_profile_preview(ProfileFormat::Navicat, path.clone());
        finish_import_job(&mut model, &services).await;
        let ImportState::Preview(preview) = &model.profile_import else {
            panic!("missing NCX preview: {:?}", model.profile_import);
        };
        assert_eq!(preview.rows[0].conflict.is_some(), pass == 1);
        let _ = model.dispatch("profile-import-toggle:1");
        let _ = model.dispatch("profile-import-confirm");
        finish_import_job(&mut model, &services).await;
        let ImportState::Report(report) = &model.profile_import else {
            panic!("missing NCX report: {:?}", model.profile_import);
        };
        assert_eq!(report.counts(), expected);
        assert_eq!(model.take_pending(), Some(PendingJob::RefreshConnections));
        let _ = model.dispatch("profile-import-close");
    }
    let saved = services.list_connections().await.unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].saved.database.as_deref(), Some("sales"));
    drop(services);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn encrypted_bundle_import_prompts_and_retries_from_the_connections_entry() {
    let dir = std::env::temp_dir().join(format!("rillsql-bundle-unlock-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let mut model = WorkbenchModel::new(Shell::default());
    let path = dir.join("locked.rillsqlcx");
    let row = crate::connections::model::normalize(&sample_draft(Engine::Sqlite), "source".into()).unwrap();
    let bytes = crate::connections::export::encode_export(&[row], Some("fixture-unlock")).unwrap();
    std::fs::write(&path, bytes).unwrap();
    model.import_path = Some(path.display().to_string());

    let _ = model.dispatch("conn-import");
    finish_bundle_import(&mut model, &services).await;
    assert_eq!(model.bundle_unlock, Some("connections.profile.bundlePrompt"));
    assert!(services.list_connections().await.unwrap().is_empty());

    model.set_field(DraftField::BundlePassphrase, "wrong-fixture-unlock".into());
    let _ = model.dispatch("conn-unlock-import");
    finish_bundle_import(&mut model, &services).await;
    assert_eq!(model.bundle_unlock, Some("connections.profile.bundleBad"));
    assert!(model.bundle_passphrase.is_empty());
    assert!(services.list_connections().await.unwrap().is_empty());

    model.set_field(DraftField::BundlePassphrase, "fixture-unlock".into());
    let _ = model.dispatch("conn-unlock-import");
    finish_bundle_import(&mut model, &services).await;
    assert!(model.bundle_unlock.is_none() && model.bundle_passphrase.is_empty());
    assert_eq!(services.list_connections().await.unwrap().len(), 1);
    drop(services);
    let _ = std::fs::remove_dir_all(dir);
}

async fn finish_bundle_import(model: &mut WorkbenchModel, services: &AppServices) {
    let token = model.prepare_list_token(&services.tasks);
    let request = request_of(model, token);
    let done = jobs::execute_job(services, request).await;
    apply_done(model, services, "connections", done);
    model.finish_job();
}

/// 表 / 结构 / 专用页面也会占用工作项，查询项不一定在索引 0。
fn query_item(model: &mut WorkbenchModel) -> &mut super::model::WorkItem {
    model.tabs.iter_mut().find(|tab| tab.kind.is_query()).expect("query item")
}

#[test]
fn dictionaries_match_the_existing_shell_keys() {
    for key in dictionary_keys() {
        assert_eq!(i18n::text(i18n::Locale::ZhCn, key), translate(Locale::ZhCn, key), "{key}");
        assert_eq!(i18n::text(i18n::Locale::En, key), translate(Locale::En, key), "{key}");
    }
}

#[test]
fn copy_fits_every_width_density_theme_and_locale() {
    let themes = [ThemePreference::Light, ThemePreference::Dark];
    let locales = [i18n::Locale::ZhCn, i18n::Locale::En];
    let pages = [AppPage::Workspace, AppPage::Connections, AppPage::History, AppPage::Settings];
    let densities = [Density::Compact, Density::Standard, Density::Relaxed];
    for width in [1360, 1024, 900, 760, 640] {
        for sidebar in [true, false] {
            for theme in themes {
                for locale in locales {
                    for density in densities {
                        for page in pages {
                            let mut model = WorkbenchModel::new(Shell::default());
                            model.set_width(width);
                            model.shell.sidebar_open = sidebar;
                            model.shell.theme = theme;
                            model.shell.page = page;
                            model.shell.density = density;
                            if locale == i18n::Locale::En {
                                model.shell.locale = Locale::En;
                            }
                            if page == AppPage::Workspace {
                                model.tabs.clear();
                            }
                            assert!(model.missing_copy(locale).is_empty(), "{:?} {:?}", page, model.missing_copy(locale));
                            assert_eq!(model.copy_overflow(locale), 0, "{width} {page:?} {locale:?} {density:?}");
                            assert!(model.layout().content_width >= 160 || width < 160);
                            // 密度不仅是设置里的文案：同一宽度下根 padding 必须跟着变。
                            assert_eq!(model.painted().pad, density.metrics().pad as i32);
                            for node in model.access_nodes() {
                                assert!(!node.id.is_empty());
                                assert!(!node.role.is_empty());
                                assert_ne!(model.text(node.label_key), node.label_key);
                                assert!(!node.state.is_empty());
                                assert!(!node.action.is_empty());
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn english_copy_has_no_chinese_and_query_titles_follow_locale() {
    for key in i18n::catalog_keys() {
        // 语言选择器里“中文（简体）”本来就用中文书写，其余英文界面不得残留中文。
        if *key == "common.localeZh" {
            continue;
        }
        let en = i18n::text(i18n::Locale::En, key);
        assert!(!en.chars().any(|ch| ('\u{4e00}'..='\u{9fff}').contains(&ch)), "{key}: {en}");
    }
    let mut model = WorkbenchModel::new(Shell::default());
    assert!(model.dispatch("new-query").is_some());
    assert!(model.dispatch("new-query").is_some());
    let query_titles = |model: &WorkbenchModel| {
        model.tabs.iter().filter(|tab| tab.kind.is_query()).map(|tab| tab.title.clone()).collect::<Vec<_>>()
    };
    assert_eq!(query_titles(&model), ["查询 1", "查询 2"]);
    assert!(model.dispatch("locale-en").is_some());
    assert_eq!(query_titles(&model), ["Query 1", "Query 2"]);
    assert!(model.dispatch("locale-zh").is_some());
    assert_eq!(query_titles(&model), ["查询 1", "查询 2"]);
    let template = i18n::text(i18n::Locale::En, "ui.query.tabTitle");
    assert_eq!(i18n::fill(&template, &[("n", "7")]), "Query 7");
}

#[gpui_kit::test]
fn density_theme_and_language_reach_the_rendered_chrome(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::component::ActiveTheme as _;

    // 页面切换会在真实 Tokio 线程读取连接/历史，设置也异步写盘。
    // 允许这些线程唤醒测试调度器，并在结束前等待回执，不能只验证绘制后就拆掉调度器。
    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-shell-scale-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let (window, view) = open_workbench_sized(cx, Arc::clone(&services), false, 640., 900.);
    cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
    cx.update(|cx| {
        view.update(cx, |view, cx| {
            view.perform("settings", cx);
            view.perform("density-compact", cx);
            view.perform("theme-dark", cx);
            view.perform("locale-en", cx);
        });
    });
    let (compact, english, dark) = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let height = f32::from(window.find("density-compact").bounds().size.height);
            let label = window.find("settings-page").label().map(str::to_owned);
            (height, label, cx.theme().is_dark())
        })
        .unwrap();
    assert!(dark, "theme-dark 必须落到实际绘制主题");
    assert_eq!(english.as_deref(), Some("Settings"));
    cx.update(|cx| view.update(cx, |view, cx| view.perform("density-relaxed", cx)));
    let (relaxed, relaxed_selected) = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let height = f32::from(window.find("density-relaxed").bounds().size.height);
            (height, window.find("density-relaxed").selected())
        })
        .unwrap();
    assert!(relaxed > compact, "密度切换必须改变实际控件高度：{compact} -> {relaxed}");
    assert_eq!(relaxed_selected, Some(true));
    eprintln!("compact density control height={compact}, relaxed={relaxed}, locale={english:?}, dark={dark}");

    // 紧凑窗口：主要区域仍在视口内，状态栏、设置控件和语言入口不被裁掉。
    assert_eq!(cx.update(|cx| view.read(cx).model.viewport_width), 640);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let width = f32::from(window.find("workbench").bounds().size.width);
        assert!((width - 640.).abs() < 1., "workbench {width}");
        for id in ["toolbar", "status", "settings-page", "density-compact", "density-standard", "density-relaxed", "locale-en", "locale-zh"] {
            let bounds = window.find(id).bounds();
            assert!(window.find(id).visible(), "{id} 应可见");
            assert!(f32::from(bounds.origin.x) >= 0., "{id} 左侧越界");
            assert!(f32::from(bounds.origin.x + bounds.size.width) <= width + 0.5, "{id} 右侧越界");
            assert!(f32::from(bounds.origin.y + bounds.size.height) <= 901., "{id} 下方越界");
        }
        assert!(f32::from(window.find("settings-page").bounds().size.width) >= 160.);
    })
    .unwrap();

    // 紧凑宽度下每个主要页面的首要操作都要真正可见、可点。
    for (command, action) in [("connections", "conn-new"), ("history", "history-empty-query")] {
        cx.update(|cx| view.update(cx, |view, cx| view.perform(command, cx)));
        // 进历史页会重读本机历史：这里直接落一份空清单，量的是读完后的空态按钮。
        cx.update(|cx| view.update(cx, |view, _| view.model.history.apply_list(Vec::new(), Vec::new())));
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let width = f32::from(window.find("workbench").bounds().size.width);
            let button = window.find(action);
            assert!(button.visible(), "{action} 应可见");
            assert!(f32::from(button.bounds().origin.x + button.bounds().size.width) <= width + 0.5);
            assert!(f32::from(button.bounds().origin.y + button.bounds().size.height) <= 901.);
        })
        .unwrap();
    }

    // 连接模态在 640 宽度下也要能看到并点到测试/保存/保存并连接。
    cx.update(|cx| view.update(cx, |view, cx| view.perform("new-connection", cx)));
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let width = f32::from(window.find("workbench").bounds().size.width);
        for id in ["conn-test", "conn-save", "conn-save-connect", "conn-cancel"] {
            let button = window.find(id);
            assert!(button.visible(), "{id} 应可见");
            assert!(f32::from(button.bounds().origin.x + button.bounds().size.width) <= width + 0.5);
            assert!(f32::from(button.bounds().origin.y + button.bounds().size.height) <= 901.);
        }
    })
    .unwrap();
    // 模态是覆盖层：关上之后才轮到命令面板。
    cx.update(|cx| view.update(cx, |view, cx| view.perform("conn-cancel", cx)));
    cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();

    cx.update(|cx| {
        view.update(cx, |view, cx| {
            view.perform("settings", cx);
            view.perform("locale-zh", cx);
        });
    });
    let chinese = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.find("settings-page").label().map(str::to_owned)
        })
        .unwrap();
    assert_eq!(chinese.as_deref(), Some("设置"));
    cx.update(|cx| view.update(cx, |view, cx| view.perform("theme-light", cx)));
    let light = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            cx.theme().is_dark()
        })
        .unwrap();
    assert!(!light, "theme-light 必须切回浅色绘制");

    // 命令面板在紧凑宽度下完整落在视口内。
    cx.update_window(window, |_, window, cx| {
        window.click("top-search", cx);
        window.render_frame(cx);
        let width = f32::from(window.find("workbench").bounds().size.width);
        let palette = window.find("palette").bounds();
        assert!(f32::from(palette.origin.x) >= 0.);
        assert!(f32::from(palette.origin.x + palette.size.width) <= width + 0.5);
        assert!(f32::from(palette.size.width) <= 620.);
    })
    .unwrap();
    assert!(
        pump_until(cx, window, &view, |view, _| {
            view.model.running_jobs == 0
                && view.model.pending_len() == 0
                && view.model.settings_save == crate::app::views::settings::SaveState::Saved
        }),
        "连接/历史读取与设置写盘必须在测试调度器退出前回到界面线程"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn six_menus_keyboard_and_close_guard_stay_on_one_command_layer() {
    let mut model = WorkbenchModel::new(Shell::default());
    for place in MenuPlace::all() {
        let target = match place {
            MenuPlace::ObjectTree => MenuTarget::Object,
            MenuPlace::Grid => MenuTarget::ReadonlyGrid,
            MenuPlace::TabBar => MenuTarget::QueryTab,
            MenuPlace::Editor => MenuTarget::QueryTab,
            MenuPlace::ConnectionRow => MenuTarget::Connection,
            MenuPlace::HistoryEntry => MenuTarget::Run,
            // 分组菜单由模型用 with_items 现算，不在这份通用表里。
            MenuPlace::ExplorerGroup => MenuTarget::Connection,
            MenuPlace::Results => MenuTarget::QueryTab,
        };
        let items = menu_items(place, target);
        assert!(!items.is_empty(), "{place:?}");
        model.open_menu(place, target);
        let _ = model.key(NavKey::ArrowDown);
        let _ = model.key(NavKey::Enter);
    }
    assert!(model.commands.iter().any(|id| !id.is_empty()));
    let _ = model.dispatch("new-query");
    model.tabs[0].draft = "select 1".into();
    assert!(!model.allow_system_close());
    assert!(model.close_prompt.is_some());
    // 关窗口要为未执行草稿确认，但更新不丢草稿（安装前写一次会话、重启后恢复），不挡更新。
    assert!(model.request_update_install().is_ok());
    assert!(!model.release_close);
    model.close_prompt = None;
    model.tabs[0].draft.clear();
    model.tabs[0].uncommitted_cells = true;
    assert!(model.request_update_install().unwrap_err().contains("pending"));
    model.tabs[0].uncommitted_cells = false;
    model.grid_dirty = true;
    assert!(!model.allow_system_close());
    let saved = serde_json::to_string(&model.snapshot()).unwrap();
    assert!(!saved.contains("uncommittedChanges"), "{saved}");
    model.close_prompt = None;
    model.grid_dirty = false;
    assert!(model.dispatch("open-table:notes|笔记|main||customers|sqlite|dev").is_some());
    assert_eq!(model.tool, WorkTool::Table);
    assert_eq!(model.shell.page, AppPage::Workspace);
    assert!(model.dispatch("open-structure-for:notes|笔记|main||customers|sqlite|dev").is_some());
    assert_eq!(model.tool, WorkTool::Structure);
    assert_eq!(model.shell.page, AppPage::Structure);
    assert!(model.dispatch("open-admin").is_some());
    assert_eq!(model.tool, WorkTool::Admin);
    model.tabs[0].running = true;
    assert_eq!(model.request_update_install().unwrap_err(), "shell.update.busy");
    assert!(!model.allow_system_close());
    assert!(model.close_prompt.is_none());
}

#[test]
fn exchange_running_blocks_before_uncommitted_grid_or_query_draft() {
    use crate::app::shell::close::{evaluate, CloseKind};
    let mut model = WorkbenchModel::new(Shell::default());
    assert!(model.dispatch("open-exchange").is_some());
    assert!(model.exchange_open);
    model.exchange_running = true;
    model.grid_dirty = true;
    assert_eq!(evaluate(model.facts()), CloseKind::BlockedRunning);
    assert!(!model.allow_system_close());
    assert!(model.close_prompt.is_none());
    model.exchange_running = false;
    model.grid_dirty = true;
    model.close_prompt = None;
    assert_eq!(evaluate(model.facts()), CloseKind::Confirm { uncommitted: true, draft: false });
    model.grid_dirty = false;
    model.close_prompt = None;
    assert!(model.dispatch("new-query").is_some());
    model.tabs[0].draft = "select 1".into();
    model.tabs[0].last_executed_sql = None;
    model.tabs[0].uncommitted_cells = false;
    assert_eq!(evaluate(model.facts()), CloseKind::Confirm { uncommitted: false, draft: true });
}

#[test]
fn redis_and_mongo_running_block_before_uncommitted_grid_or_query_draft() {
    use crate::app::shell::close::{evaluate, CloseKind};
    let mut model = WorkbenchModel::new(Shell::default());
    assert!(model.dispatch("open-redis:cache|缓存|0").is_some());
    assert_eq!(model.tool, WorkTool::Redis);
    assert_eq!(model.shell.page, AppPage::Workspace);
    assert_eq!(model.redis_spec.as_deref(), Some("cache|缓存|0"));
    model.redis_running = true;
    model.grid_dirty = true;
    assert!(model.dispatch("new-query").is_some());
    let query = query_item(&mut model);
    query.draft = "select 1".into();
    query.last_executed_sql = None;
    query.uncommitted_cells = false;
    assert_eq!(evaluate(model.facts()), CloseKind::BlockedRunning);
    assert!(!model.allow_system_close());
    assert!(model.close_prompt.is_none());
    model.redis_running = false;
    model.mongo_running = true;
    assert!(model.dispatch("open-mongo:docs|文档").is_some());
    assert_eq!(model.tool, WorkTool::Mongo);
    assert_eq!(model.mongo_spec.as_deref(), Some("docs|文档"));
    model.close_prompt = None;
    assert_eq!(evaluate(model.facts()), CloseKind::BlockedRunning);
    model.mongo_running = false;
    model.close_prompt = None;
    assert_eq!(evaluate(model.facts()), CloseKind::Confirm { uncommitted: true, draft: true });
    model.grid_dirty = false;
    query_item(&mut model).uncommitted_cells = false;
    model.close_prompt = None;
    assert_eq!(evaluate(model.facts()), CloseKind::Confirm { uncommitted: false, draft: true });
    query_item(&mut model).draft.clear();
    model.close_prompt = None;
    assert_eq!(evaluate(model.facts()), CloseKind::Allow);
}

#[test]
fn compare_running_blocks_before_uncommitted_grid_or_query_draft() {
    use crate::app::shell::close::{evaluate, CloseKind};
    let mut model = WorkbenchModel::new(Shell::default());
    assert!(model.dispatch("compare").is_some());
    assert_eq!(model.shell.page, AppPage::Compare);
    model.compare_running = true;
    model.grid_dirty = true;
    assert!(model.dispatch("new-query").is_some());
    let query = query_item(&mut model);
    query.draft = "select 1".into();
    query.last_executed_sql = None;
    assert_eq!(evaluate(model.facts()), CloseKind::BlockedRunning);
    assert!(!model.allow_system_close());
    assert!(model.close_prompt.is_none());
    model.compare_running = false;
    model.close_prompt = None;
    assert_eq!(evaluate(model.facts()), CloseKind::Confirm { uncommitted: true, draft: true });
    model.grid_dirty = false;
    query_item(&mut model).uncommitted_cells = false;
    model.close_prompt = None;
    assert_eq!(evaluate(model.facts()), CloseKind::Confirm { uncommitted: false, draft: true });
    query_item(&mut model).draft.clear();
    model.close_prompt = None;
    assert_eq!(evaluate(model.facts()), CloseKind::Allow);
}

#[test]
fn opening_the_same_target_reuses_one_work_item() {
    let mut model = WorkbenchModel::new(Shell::default());
    let customers = "notes|笔记|main||customers|sqlite|dev";
    assert!(model.dispatch(&format!("open-table:{customers}")).is_some());
    let first = model.active_tab.clone().expect("table item");
    assert!(model.dispatch(&format!("open-table:{customers}")).is_some());
    assert_eq!(model.tabs.len(), 1, "同一张表重复打开不应复制工作项");
    assert_eq!(model.active_tab.as_deref(), Some(first.as_str()));
    assert_eq!(model.tabs[0].kind, WorkItemKind::Table);
    assert_eq!(model.tabs[0].spec.as_deref(), Some(customers));

    assert!(model.dispatch("open-table:notes|笔记|main||orders|sqlite|dev").is_some());
    assert_eq!(model.tabs.len(), 2, "不同目标应各自保留一个工作项");
    assert_ne!(model.active_tab.as_deref(), Some(first.as_str()));
}

#[test]
fn switching_between_table_items_keeps_each_spec_and_tool() {
    let mut model = WorkbenchModel::new(Shell::default());
    let customers = "notes|笔记|main||customers|sqlite|dev";
    let orders = "notes|笔记|main||orders|sqlite|dev";
    assert!(model.dispatch(&format!("open-table:{customers}")).is_some());
    let customers_id = model.active_tab.clone().expect("customers");
    assert!(model.dispatch(&format!("open-table:{orders}")).is_some());
    let orders_id = model.active_tab.clone().expect("orders");
    assert_eq!(model.table_spec.as_deref(), Some(orders));

    model.activate_item(&customers_id);
    assert_eq!(model.tool, WorkTool::Table);
    assert_eq!(model.shell.page, AppPage::Workspace);
    assert_eq!(model.table_spec.as_deref(), Some(customers));
    assert_eq!(model.active_tab.as_deref(), Some(customers_id.as_str()));
    assert_eq!(model.tabs.iter().find(|tab| tab.id == orders_id).unwrap().spec.as_deref(), Some(orders));

    model.activate_item(&orders_id);
    assert_eq!(model.table_spec.as_deref(), Some(orders));
    assert_eq!(model.tabs.iter().find(|tab| tab.id == customers_id).unwrap().spec.as_deref(), Some(customers));
}

#[gpui_kit::test]
fn switching_work_items_drops_structure_admin_redis_and_mongo_state(cx: &mut gpui_kit::TestAppContext) {
    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-shell-tool-state-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench(cx, services_at(&dir), false);
    let customers = "notes|笔记|main||customers|sqlite|dev";
    let orders = "notes|笔记|main||orders|sqlite|dev";
    let snapshot = crate::ddl::Snapshot::bare(crate::ddl::DdlEngine::Sqlite, "main", "", "customers");

    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.perform(&format!("open-structure-for:{customers}"), cx));
        window.render_frame(cx);
        view.update(cx, |view, cx| {
            view.structure.update(cx, |pane, _| {
                pane.type_into("column_name", "leak");
                pane.session.section = crate::app::views::structure::Section::Indexes;
                pane.session.arm_fixture(&snapshot);
            });
            view.perform(&format!("open-structure-for:{orders}"), cx);
        });
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        let pane = view.read(cx).structure.read(cx);
        assert_eq!(pane.session.target.table, "orders");
        assert!(pane.session.column_name.is_empty(), "换表不能带走上一张表的列名");
        assert_eq!(pane.session.section, crate::app::views::structure::Section::Columns);
        assert!(!pane.session.preview_open(), "换表不能带走预览");
    });

    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.perform(&format!("open-table:{customers}"), cx);
            view.perform("open-admin", cx);
        });
        window.render_frame(cx);
        view.update(cx, |view, cx| {
            view.admin.update(cx, |pane, _| {
                pane.type_into("reindex", "customers");
                pane.choose("reindex");
                pane.session.arm_fixture(&snapshot);
            });
            view.perform(&format!("open-table:{orders}"), cx);
            view.perform("open-admin", cx);
        });
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        let pane = view.read(cx).admin.read(cx);
        assert_eq!(pane.session.target.table, "orders");
        assert!(matches!(pane.session.operation, crate::app::views::admin::AdminOp::Vacuum), "换表不能沿用上一个索引操作");
        assert!(!pane.session.preview_open(), "换表不能沿用管理预览");
    });

    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.perform("open-redis:cache|缓存|0", cx);
        });
        window.render_frame(cx);
    })
    .unwrap();
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.redis.update(cx, |pane, _| {
                pane.plant_key("leak-key");
                pane.set_command("DEL leak-key");
            });
            view.perform("open-redis:cache|缓存|1", cx);
        });
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        let pane = view.read(cx).redis.read(cx);
        assert!(!pane.confirm_open(), "换 Redis 目标不能保留旧确认");
        assert!(pane.selected().is_empty());
        assert!(pane.command_input().is_empty());
    });

    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.perform("open-mongo:docs|文档|app|users", cx);
        });
        window.render_frame(cx);
    })
    .unwrap();
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.mongo.update(cx, |pane, _| {
                pane.set_editor(r#"{"name":"临时"}"#);
            });
            view.perform("open-mongo:docs|文档|app|orders", cx);
        });
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        let pane = view.read(cx).mongo.read(cx);
        assert!(!pane.confirm_open(), "换集合不能保留旧写入预览");
        assert!(!pane.editor_open());
        assert!(pane.editor_text().is_empty());
    });

    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.perform("open-diagram:notes|main||图A", cx));
        window.render_frame(cx);
    })
    .unwrap();
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.perform("open-diagram:notes|main||图B", cx));
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        let text = view.read(cx).diagram_text(cx);
        assert!(text.contains("图B"), "{text}");
        assert!(!text.contains("图A"), "关系图目标切换后不能还显示旧范围：{text}");
    });
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn reopening_the_same_item_gets_a_new_epoch_even_when_the_id_is_reused() {
    let mut model = WorkbenchModel::new(Shell::default());
    let spec = "notes|笔记|main||customers|sqlite|dev";
    assert!(model.dispatch(&format!("open-structure-for:{spec}")).is_some());
    let first = model.active_tab().expect("structure").clone();
    model.request_close_tab(&first.id);
    assert!(model.tabs.is_empty());
    assert!(model.dispatch(&format!("open-structure-for:{spec}")).is_some());
    let second = model.active_tab().expect("structure").clone();
    assert_eq!(first.id, second.id, "标签 id 可能被回收");
    assert_ne!(first.epoch, second.epoch, "重开必须得到新的工作项实例");
}

#[test]
fn staged_cells_block_switching_to_another_table() {
    let mut model = WorkbenchModel::new(Shell::default());
    let customers = "notes|笔记|main||customers|sqlite|dev";
    let orders = "notes|笔记|main||orders|sqlite|dev";
    assert!(model.dispatch(&format!("open-table:{customers}")).is_some());
    let customers_id = model.active_tab.clone().expect("customers");
    model.tabs[0].uncommitted_cells = true;

    assert!(model.dispatch(&format!("open-table:{orders}")).is_some());
    assert_eq!(model.tabs.len(), 1, "有暂存修改时不得打开另一张表并清空网格");
    assert_eq!(model.active_tab.as_deref(), Some(customers_id.as_str()));
    assert_eq!(model.notice_key, Some("shell.tabs.pendingCells"));

    // 同一张表的结构页共享同一网格，可以切换。
    assert!(model.dispatch(&format!("open-structure-for:{customers}")).is_some());
    assert_eq!(model.tool, WorkTool::Structure);
    assert_eq!(model.shell.page, AppPage::Structure);

    // 别的表的结构页不碰共享网格，也不需要先提交暂存。
    assert!(model.dispatch(&format!("open-structure-for:{orders}")).is_some());
    assert_eq!(model.tabs.len(), 3);
    assert_eq!(model.table_spec.as_deref(), Some(orders));

    // 提交或撤销暂存后恢复切换。
    model.tabs.iter_mut().find(|tab| tab.id == customers_id).unwrap().uncommitted_cells = false;
    assert!(model.dispatch(&format!("open-table:{orders}")).is_some());
    assert_eq!(model.tabs.len(), 4);
    assert_eq!(model.table_spec.as_deref(), Some(orders));
}

#[test]
fn targetless_structure_command_explains_instead_of_showing_stale_page() {
    let mut model = WorkbenchModel::new(Shell::default());
    assert!(model.dispatch("open-structure").is_some());
    assert_eq!(model.notice_key, Some("shell.tabs.needObject"));
    assert_eq!(model.shell.page, AppPage::Workspace);
    assert!(model.tabs.is_empty());
}

#[test]
fn tab_context_menu_targets_right_clicked_item_without_activating_it() {
    let mut model = WorkbenchModel::new(Shell::default());
    let customers = "notes|笔记|main||customers|sqlite|dev";
    assert!(model.dispatch(&format!("open-table:{customers}")).is_some());
    let table_id = model.active_tab.clone().expect("table");
    assert!(model.dispatch(&format!("open-structure-for:{customers}")).is_some());
    let structure_id = model.active_tab.clone().expect("structure");
    assert_eq!(model.tool, WorkTool::Structure);

    // 右键非激活标签：不切换当前激活标签与页面，菜单以右键标签为目标。
    assert!(model.tab_menu_for(&table_id));
    assert_eq!(model.active_tab.as_deref(), Some(structure_id.as_str()));
    assert_eq!(model.tool, WorkTool::Structure);
    assert_eq!(model.shell.page, AppPage::Structure);
    assert_eq!(model.tab_menu_target.as_deref(), Some(table_id.as_str()));
    assert!(model.menu.is_some(), "右键应打开以被点中标签为基准的标签菜单");

    // 执行“固定标签”作用在右键标签上，当前激活页仍保持为结构页。
    assert!(model.dispatch("tab-pin").is_some());
    assert!(model.is_pinned(&table_id), "右键目标标签应被固定");
    assert!(!model.is_pinned(&structure_id), "原激活标签不应被误固定");
    assert_eq!(model.active_tab.as_deref(), Some(structure_id.as_str()));
}

#[test]
fn keyboard_tab_cycling_reaches_every_work_item() {
    let mut model = WorkbenchModel::new(Shell::default());
    assert!(model.dispatch("new-query").is_some());
    let first = model.active_tab.clone().expect("query");
    assert!(model.dispatch("open-table:notes|笔记|main||customers|sqlite|dev").is_some());
    let second = model.active_tab.clone().expect("table");
    assert!(model.dispatch("open-structure-for:notes|笔记|main||customers|sqlite|dev").is_some());
    let third = model.active_tab.clone().expect("structure");

    assert!(model.dispatch("next-tab").is_some());
    assert_eq!(model.active_tab.as_deref(), Some(first.as_str()));
    assert_eq!(model.tool, WorkTool::Query);
    assert!(model.dispatch("prev-tab").is_some());
    assert_eq!(model.active_tab.as_deref(), Some(third.as_str()));
    assert_eq!(model.tool, WorkTool::Structure);
    assert!(model.dispatch("prev-tab").is_some());
    assert_eq!(model.active_tab.as_deref(), Some(second.as_str()));
    assert_eq!(model.table_spec.as_deref(), Some("notes|笔记|main||customers|sqlite|dev"));
}

#[test]
fn closing_the_active_item_reactivates_a_neighbour_and_follows_its_tool() {
    let mut model = WorkbenchModel::new(Shell::default());
    let customers = "notes|笔记|main||customers|sqlite|dev";
    assert!(model.dispatch(&format!("open-table:{customers}")).is_some());
    let table_id = model.active_tab.clone().expect("table");
    assert!(model.dispatch(&format!("open-structure-for:{customers}")).is_some());
    let structure_id = model.active_tab.clone().expect("structure");
    assert_eq!(model.tool, WorkTool::Structure);
    assert_eq!(model.shell.page, AppPage::Structure);

    model.request_close_tab(&structure_id);
    assert!(model.close_prompt.is_none(), "没有未提交内容时应直接关闭");
    assert_eq!(model.tabs.len(), 1);
    assert_eq!(model.active_tab.as_deref(), Some(table_id.as_str()));
    assert_eq!(model.tool, WorkTool::Table);
    assert_eq!(model.shell.page, AppPage::Workspace);
    assert_eq!(model.table_spec.as_deref(), Some(customers));
}

#[test]
fn batch_close_stops_at_an_uncommitted_item_then_resumes_or_cancels() {
    use crate::app::shell::close::{CloseKind, CloseScope};
    let mut model = WorkbenchModel::new(Shell::default());
    let customers = "notes|笔记|main||customers|sqlite|dev";
    let orders = "notes|笔记|main||orders|sqlite|dev";
    assert!(model.dispatch("new-query").is_some());
    let keep = model.active_tab.clone().expect("query");
    assert!(model.dispatch(&format!("open-table:{customers}")).is_some());
    let uncommitted = model.active_tab.clone().expect("customers");
    assert!(model.dispatch(&format!("open-table:{orders}")).is_some());
    model.tabs.iter_mut().find(|tab| tab.id == uncommitted).unwrap().uncommitted_cells = true;
    model.activate_item(&keep);

    assert!(model.dispatch("tab-close-others").is_some());
    assert_eq!(model.tabs.len(), 3, "确认前不得关闭任何工作项");
    let prompt = model.close_prompt.as_ref().expect("未提交项需要确认");
    assert_eq!(prompt.scope, CloseScope::Tab);
    assert!(matches!(prompt.kind, CloseKind::Confirm { uncommitted: true, draft: false }));
    assert_eq!(prompt.tab_id.as_deref(), Some(uncommitted.as_str()));
    assert_eq!(model.close_batch.len(), 2, "确认后还要继续关闭剩余项");

    assert!(model.dispatch("close-stay").is_some());
    assert!(model.close_prompt.is_none());
    assert!(model.close_batch.is_empty(), "取消后不得残留待关闭队列");
    assert_eq!(model.tabs.len(), 3, "取消后其余工作项必须保留");

    assert!(model.dispatch("tab-close-others").is_some());
    assert!(model.dispatch("close-leave").is_some());
    assert!(model.close_prompt.is_none());
    assert_eq!(model.tabs.len(), 1);
    assert_eq!(model.active_tab.as_deref(), Some(keep.as_str()));
    assert_eq!(model.tool, WorkTool::Query);
}

#[test]
fn middle_click_close_goes_through_the_close_guard() {
    use crate::app::shell::close::CloseKind;
    let mut model = WorkbenchModel::new(Shell::default());
    assert!(model.dispatch("open-table:notes|笔记|main||customers|sqlite|dev").is_some());
    let id = model.active_tab.clone().expect("table");
    model.tabs[0].uncommitted_cells = true;
    model.request_close_tab(&id);
    assert_eq!(model.tabs.len(), 1, "有未提交修改时不得直接关闭");
    let prompt = model.close_prompt.as_ref().expect("需要确认");
    assert_eq!(prompt.tab_id.as_deref(), Some(id.as_str()));
    assert!(matches!(prompt.kind, CloseKind::Confirm { uncommitted: true, draft: false }));
    assert!(model.dispatch("close-leave").is_some());
    assert!(model.tabs.is_empty());
    assert!(model.close_prompt.is_none());
    assert_eq!(model.tool, WorkTool::Query);
}

#[test]
fn snapshot_restores_table_and_structure_items_without_running_or_uncommitted_state() {
    let mut model = WorkbenchModel::new(Shell::default());
    let customers = "notes|笔记|main||customers|sqlite|dev";
    assert!(model.dispatch(&format!("open-table:{customers}")).is_some());
    assert!(model.dispatch(&format!("open-structure-for:{customers}")).is_some());
    let structure_id = model.active_tab.clone().expect("structure");
    for tab in &mut model.tabs {
        tab.running = true;
        tab.uncommitted_cells = true;
    }
    let state = model.snapshot();
    let saved = serde_json::to_string(&state).unwrap();
    assert!(saved.contains(&format!("\"spec\":\"{customers}\"")), "{saved}");
    assert!(state.session.tabs.iter().any(|tab| tab.kind == "table" && tab.spec.as_deref() == Some(customers)));
    assert!(state.session.tabs.iter().any(|tab| tab.kind == "structure" && tab.spec.as_deref() == Some(customers)));

    let mut restored = WorkbenchModel::new(Shell::default());
    restored.restore_session(&state.session);
    assert_eq!(restored.tabs.len(), 2);
    assert!(restored.tabs.iter().all(|tab| !tab.running && !tab.uncommitted_cells));
    let table = restored.tabs.iter().find(|tab| tab.kind == WorkItemKind::Table).expect("table");
    let structure = restored.tabs.iter().find(|tab| tab.kind == WorkItemKind::Structure).expect("structure");
    assert_eq!(table.spec.as_deref(), Some(customers));
    assert_eq!(structure.spec.as_deref(), Some(customers));
    assert_eq!(restored.active_tab.as_deref(), Some(structure_id.as_str()));
    assert_eq!(restored.tool, WorkTool::Structure);
    assert_eq!(restored.shell.page, AppPage::Structure);
    assert_eq!(restored.table_spec.as_deref(), Some(customers));
}

#[test]
fn legacy_structure_tabs_without_a_target_are_not_restored_as_empty_items() {
    let session: SessionState = serde_json::from_str(
        r#"{"activeId":"definition-1","tabs":[
            {"id":"query-1","title":"查询","kind":"query","draft":"select 1"},
            {"id":"definition-1","title":"结构","kind":"definition","draft":""}
        ]}"#,
    )
    .unwrap();
    let mut model = WorkbenchModel::new(Shell::default());
    model.restore_session(&session);
    assert_eq!(model.tabs.len(), 1, "没有目标规格的结构标签不应恢复成空壳工作项");
    assert_eq!(model.tabs[0].kind, WorkItemKind::Query);
    assert_eq!(model.active_tab.as_deref(), Some("query-1"));
}

#[inline(never)]
async fn seven_engines_save(dir: &std::path::Path, services: &Arc<AppServices>, model: &mut WorkbenchModel) -> String {
    for engine in [Engine::Postgres, Engine::Mysql, Engine::Mariadb, Engine::Sqlserver, Engine::Sqlite, Engine::Redis, Engine::Mongodb] {
        let mut draft = if engine == Engine::Postgres { with_tls_ssh(engine) } else { sample_draft(engine) };
        if engine == Engine::Sqlite {
            let file = dir.join("notes.sqlite");
            let pool = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&file).create_if_missing(true))
                .await
                .unwrap();
            pool.close().await;
            draft.file_path = file.display().to_string();
        }
        model.connections.editor = Some(draft);
        model.connections.secrets = Secrets::default();
        let _ = model.dispatch("conn-save");
        let token = model.prepare_list_token(&services.tasks);
        let request = request_of(model, token);
        let done = jobs::execute_job(services, request).await;
        apply_done(model, services, "connections", done);
    }
    assert_eq!(model.connections.rows.len(), 7, "saved {:?}", model.connections.rows.iter().map(|row| row.saved.engine).collect::<Vec<_>>());
    let postgres_id = model.connections.rows.iter().find(|row| row.saved.engine == Engine::Postgres).unwrap().saved.id.clone();
    model.connections.selected = Some(postgres_id);
    model.connections.edit_selected();
    let edited = model.connections.editor.clone().unwrap();
    assert_eq!(edited.ssh_auth, SshAuthKind::Key);
    assert_eq!(edited.ssh_key, "id_ed25519");
    assert_eq!(edited.ca_mode, CaMode::File);
    assert_eq!(edited.ca_file, "ca.pem");
    assert_eq!(edited.client_cert, "client.pem");
    assert_eq!(edited.client_key, "client.key");
    model.set_field(DraftField::SshPassword, "jump-secret".into());
    assert_eq!(model.connections.editor.as_ref().unwrap().ssh_auth, SshAuthKind::Password);
    assert_eq!(model.connections.editor.as_ref().unwrap().ssh_key, "id_ed25519");
    assert!(!serde_json::to_string(&model.snapshot()).unwrap().contains("jump-secret"));
    model.connections.filter_engine = Some(Engine::Redis);
    assert_eq!(model.connections.filtered().len(), 1);
    model.connections.filter_engine = None;
    model.connections.filter_text = "sample-mysql".into();
    assert_eq!(model.connections.filtered().len(), 1);
    model.connections.filter_text.clear();
    let sqlite_id = model.connections.rows.iter().find(|row| row.saved.engine == Engine::Sqlite).unwrap().saved.id.clone();
    model.connections.selected = Some(sqlite_id.clone());
    model.connections.duplicate_selected();
    let _ = model.dispatch("conn-save");
    let token = model.prepare_list_token(&services.tasks);
    let done = jobs::execute_job(services, request_of(model, token)).await;
    apply_done(model, services, "connections", done);
    assert_eq!(model.connections.rows.len(), 8);
    sqlite_id
}

#[inline(never)]
async fn seven_engines_transfer(dir: &std::path::Path, services: &Arc<AppServices>, model: &mut WorkbenchModel, sqlite_id: &str) {
    model.connections.selected = Some(sqlite_id.to_owned());
    let _ = model.dispatch("conn-connect");
    let token = model.prepare_list_token(&services.tasks);
    let done = jobs::execute_job(services, request_of(model, token)).await;
    apply_done(model, services, "connections", done);
    assert!(matches!(model.connections.status.get(sqlite_id), Some(crate::app::views::connections::LinkStatus::Connected)), "{:?}", model.connections.message_detail);

    let _ = model.dispatch("conn-disconnect");
    let token = model.prepare_list_token(&services.tasks);
    let done = jobs::execute_job(services, request_of(model, token)).await;
    apply_done(model, services, "connections", done);
    assert!(matches!(model.connections.status.get(sqlite_id), Some(crate::app::views::connections::LinkStatus::Disconnected)));
    let _ = model.dispatch("conn-test");
    let token = model.prepare_list_token(&services.tasks);
    let done = jobs::execute_job(services, request_of(model, token)).await;
    apply_done(model, services, "connections", done);
    assert_eq!(model.connections.message_key, Some("connections.editor.test.ok"), "{}", model.connections.message_detail);

    let export = dir.join("connections.json");
    model.export_path = Some(export.display().to_string());
    let _ = model.dispatch("conn-export");
    let token = model.prepare_list_token(&services.tasks);
    let done = jobs::execute_job(services, request_of(model, token)).await;
    apply_done(model, services, "connections", done);
    assert!(export.is_file());
    let blob = std::fs::read_to_string(&export).unwrap();
    assert!(!blob.contains("sshPassphrase"));
    assert!(!blob.to_ascii_lowercase().contains("\"password\""));
    seven_engines_delete(dir, services, model).await;
    seven_engines_sealed(dir, services, model, &export).await;
}

#[inline(never)]
async fn seven_engines_delete(dir: &std::path::Path, services: &Arc<AppServices>, model: &mut WorkbenchModel) {
    let before_delete = model.connections.rows.len();
    model.connections.selected = Some(model.connections.rows[0].saved.id.clone());
    let _ = model.dispatch("conn-delete");
    assert!(model.connections.confirm_delete);
    assert_eq!(model.notice_key, Some("connections.deleteDialog.title"));
    assert_eq!(model.pending_len(), 0);
    let _ = model.dispatch("conn-delete");
    assert_eq!(model.pending_len(), 0, "second click must not delete");
    let _ = model.dispatch("conn-delete-cancel");
    assert!(!model.connections.confirm_delete);
    assert_eq!(services.list_connections().await.unwrap().len(), before_delete);
    let _ = model.dispatch("conn-delete");
    let _ = model.dispatch("conn-delete-confirm");
    let token = model.prepare_list_token(&services.tasks);
    let done = jobs::execute_job(services, request_of(model, token)).await;
    apply_done(model, services, "connections", done);
    assert_eq!(model.connections.rows.len(), before_delete - 1);

    let broken = dir.join("broken.json");
    std::fs::write(&broken, b"{").unwrap();
    let kept = model.connections.rows.len();
    model.import_path = Some(broken.display().to_string());
    model.bundle_passphrase.clear();
    let _ = model.dispatch("conn-import");
    let token = model.prepare_list_token(&services.tasks);
    let done = jobs::execute_job(services, request_of(model, token)).await;
    apply_done(model, services, "connections", done);
    assert_eq!(model.connections.rows.len(), kept);
    assert_eq!(services.list_connections().await.unwrap().len(), kept);
    let skip_file = dir.join("skip.json");
    std::fs::write(&skip_file, r#"{"format":"rillsql-connections","version":1,"connections":[{"name":"跳过我","engine":"oracle"}]}"#).unwrap();
    model.import_path = Some(skip_file.display().to_string());
    let _ = model.dispatch("conn-import");
    let token = model.prepare_list_token(&services.tasks);
    let done = jobs::execute_job(services, request_of(model, token)).await;
    apply_done(model, services, "connections", done);
    assert_eq!(model.connections.rows.len(), kept);
    assert!(!model.import_notes.is_empty(), "{:?}", model.import_notes);
    assert_eq!(model.notice_key, Some("connections.importReport.skippedTitle"));
}

#[inline(never)]
async fn seven_engines_sealed(dir: &std::path::Path, services: &Arc<AppServices>, model: &mut WorkbenchModel, export: &std::path::Path) {
    model.bundle_passphrase = "correct-horse".into();
    let secret_export = dir.join("connections.rillsqlcx");
    model.export_path = Some(secret_export.display().to_string());
    let _ = model.dispatch("conn-export");
    let token = model.prepare_list_token(&services.tasks);
    let done = jobs::execute_job(services, request_of(model, token)).await;
    apply_done(model, services, "connections", done);
    let sealed = std::fs::read(&secret_export).unwrap();
    assert!(sealed.starts_with(b"RILLSQLCX1"));
    assert!(!serde_json::to_string(&model.snapshot()).unwrap().contains("correct-horse"));
    let wrong_dir = dir.join("import-wrong");
    std::fs::create_dir_all(&wrong_dir).unwrap();
    let wrong = services_at(&wrong_dir);
    let mut wrong_model = WorkbenchModel::new(Shell::default());
    wrong_model.connections.editor = Some(sample_draft(Engine::Redis));
    let _ = wrong_model.dispatch("conn-save");
    let token = wrong_model.prepare_list_token(&wrong.tasks);
    let done = jobs::execute_job(&wrong, request_of(&mut wrong_model, token)).await;
    apply_done(&mut wrong_model, &wrong, "connections", done);
    assert_eq!(wrong_model.connections.rows.len(), 1);
    wrong_model.import_path = Some(secret_export.display().to_string());
    wrong_model.bundle_passphrase = "not-the-passphrase".into();
    let _ = wrong_model.dispatch("conn-import");
    let token = wrong_model.prepare_list_token(&wrong.tasks);
    let done = jobs::execute_job(&wrong, request_of(&mut wrong_model, token)).await;
    apply_done(&mut wrong_model, &wrong, "connections", done);
    assert_eq!(wrong_model.connections.rows.len(), 1);
    assert_eq!(std::fs::read(&secret_export).unwrap(), sealed);
    let good_dir = dir.join("import-sealed");
    std::fs::create_dir_all(&good_dir).unwrap();
    let good = services_at(&good_dir);
    let mut good_model = WorkbenchModel::new(Shell::default());
    good_model.import_path = Some(secret_export.display().to_string());
    good_model.bundle_passphrase = "correct-horse".into();
    let _ = good_model.dispatch("conn-import");
    let token = good_model.prepare_list_token(&good.tasks);
    let done = jobs::execute_job(&good, request_of(&mut good_model, token)).await;
    apply_done(&mut good_model, &good, "connections", done);
    assert!(good_model.connections.rows.len() >= 7, "{}", good_model.connections.rows.len());
    model.bundle_passphrase.clear();

    let fresh = services_at(&dir.join("import"));
    std::fs::create_dir_all(dir.join("import")).unwrap();
    let mut imported = WorkbenchModel::new(Shell::default());
    imported.import_path = Some(export.display().to_string());
    let _ = imported.dispatch("conn-import");
    let token = imported.prepare_list_token(&fresh.tasks);
    let done = jobs::execute_job(&fresh, request_of(&mut imported, token)).await;
    apply_done(&mut imported, &fresh, "connections", done);
    assert!(imported.connections.rows.len() >= 7, "{}", imported.connections.rows.len());
}

#[inline(never)]
async fn seven_engines_history(services: &Arc<AppServices>, model: &mut WorkbenchModel) {
    let _ = model.dispatch("new-query");
    model.tabs[0].draft = "select 1".into();
    model.history.save_name = "计数".into();
    model.history.section = HistorySection::Saved;
    let before = model.executions;
    let _ = model.dispatch("history-save-named");
    let token = model.begin_owner(&services.tasks, "history");
    let done = jobs::execute_job(services, request_of(model, token)).await;
    apply_done(model, services, "history", done);
    assert_eq!(model.executions, before);
    assert_eq!(model.history.runs.len(), 0);
    assert_eq!(model.history.saved.len(), 1);
    model.history.selected_saved = Some(model.history.saved[0].id.clone());
    let loads = model.loads;
    let _ = model.dispatch("history-load");
    assert_eq!(model.loads, loads + 1);
    assert_eq!(model.executions, before);
    assert_eq!(model.active_tab().unwrap().draft, "select 1");
    assert!(model.active_tab().unwrap().last_executed_sql.is_none());
    assert_eq!(model.welcome.saved(&model.history.saved).len(), 1);
    let welcome_plan = model.welcome.open_saved(&model.history.saved[0], &model.known_ids());
    assert!(!welcome_plan.execute);
    model.apply_load(welcome_plan);
    assert_eq!(model.executions, before);
    assert!(model.active_tab().unwrap().last_executed_sql.is_none());

    model.settings.density = "relaxed".into();
    model.settings.theme = "dark".into();
    model.settings.locale = "en".into();
    model.settings.editor_font = 18;
    model.settings.editor_theme = "warm".into();
    model.settings.apply_shell(&mut model.shell);
    let state = model.snapshot();
    assert!(!serde_json::to_string(&state).unwrap().contains("\"password\""));
    super::model::persist_state(services, &state).await.unwrap();
    let loaded = services.load_state().await.unwrap();
    let mut restored = WorkbenchModel::new(Shell::default());
    restored.apply_settings(&loaded.shell);
    restored.restore_session(&loaded.session);
    assert_eq!(restored.shell.density, crate::app::Density::Relaxed);
    assert_eq!(restored.shell.theme, ThemePreference::Dark);
    assert_eq!(restored.shell.locale, Locale::En);
    assert_eq!(restored.settings.editor_font, 18);
    assert_eq!(restored.settings.editor_theme, "warm");
    assert!(restored.tabs.iter().all(|tab| !tab.uncommitted_cells));

    let stale = model.connections.begin(&services.tasks);
    let current = model.connections.begin(&services.tasks);
    assert!(!model.connections.accept_list(&services.tasks, stale, Vec::new()));
    assert!(model.connections.accept_list(&services.tasks, current, model.connections.rows.clone()));

    let _ = SaveRequest {
        name: "计数".into(),
        sql: "select 1".into(),
        connection_id: String::new(),
        connection_name: String::new(),
        database_name: String::new(),
        engine: String::new(),
    };
    let _ = AppState::new(ShellSettings::default(), SessionState::default());
}

#[tokio::test]
async fn seven_engines_round_trip_without_remote_sockets_and_history_does_not_run() {
    let dir = std::env::temp_dir().join(format!("rillsql-shell-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let mut model = WorkbenchModel::new(Shell::default());
    let sqlite_id = seven_engines_save(&dir, &services, &mut model).await;
    seven_engines_transfer(&dir, &services, &mut model, &sqlite_id).await;
    seven_engines_history(&services, &mut model).await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[gpui_kit::test]
fn long_copy_is_truncated_but_fully_available_and_never_holds_neighbours(cx: &mut gpui_kit::TestAppContext) {
    let dir = std::env::temp_dir().join(format!("rillsql-shell-long-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let (window, view) = open_workbench_sized(cx, Arc::clone(&services), false, 640., 900.);
    cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();

    let long_name = "客户订单归档与结算连接的完整名称-".repeat(4);
    let long_host = "very-long-internal-hostname-for-the-archive-cluster.example.test".to_owned();
    let long_title = "一个非常长的查询标签标题，用来确认截断之后仍然能读到完整名称".repeat(3);

    cx.update(|cx| {
        view.update(cx, |view, cx| {
            view.model.connections.rows.push(ConnectionRecord {
                saved: SavedConnection {
                    id: "long".into(),
                    name: long_name.clone(),
                    engine: Engine::Postgres,
                    env: "dev".into(),
                    host: Some(long_host),
                    port: Some(5432),
                    database: Some("archive_and_settlement_reporting".into()),
                    username: Some("reporting_operator".into()),
                    file_path: None,
                    replica_set: None,
                    tls: None,
                    ssh: None,
                    password_saved: false,
                    ssh_password_saved: false,
                },
                ssh_host_key: None,
            });
            view.perform("connections", cx);
        });
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let width = f32::from(window.find("workbench").bounds().size.width);
        let row = window.find("conn-row-long");
        assert!(row.visible());
        assert!(f32::from(row.bounds().origin.x + row.bounds().size.width) <= width + 0.5);
        let label = row.label().expect("连接行的无障碍名称").to_owned();
        assert!(label.contains(&long_name), "完整名称仍要可读：{label}");
    })
    .unwrap();

    cx.update(|cx| {
        view.update(cx, |view, cx| {
            view.perform("activity-workspace", cx);
            view.perform("new-query", cx);
            let tab = view.model.tabs.last_mut().expect("query tab");
            tab.title = long_title.clone();
            tab.connection_id = Some("long".into());
            tab.connection_name = Some(long_name.clone());
            tab.database = Some("archive_and_settlement_reporting".into());
            view.model.shell.sidebar_open = true;
        });
    });
    let tab_id = cx.update(|cx| format!("tab-{}", view.read(cx).model.tabs.last().unwrap().id));
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let width = f32::from(window.find("workbench").bounds().size.width);
        let tab = window.find(gpui_kit::SharedString::from(tab_id));
        assert!(tab.visible());
        assert!(f32::from(tab.bounds().size.width) <= 220.5, "标签宽度 {}", f32::from(tab.bounds().size.width));
        eprintln!(
            "long copy at 640: tab width={}, status target width={}, strip={}",
            f32::from(tab.bounds().size.width),
            f32::from(window.find("status-target").bounds().size.width),
            f32::from(window.find("tabs").bounds().size.width)
        );
        assert_eq!(tab.label(), Some(long_title.as_str()), "截断后仍要能读完整标题");
        let target = window.find("status-target").bounds();
        assert!(f32::from(target.origin.x + target.size.width) <= width + 0.5);
        assert!(f32::from(target.size.width) <= width * 0.45 + 1.);
        let sidebar = window.find("sidebar").bounds();
        assert!(f32::from(sidebar.origin.x + sidebar.size.width) <= width + 0.5);
        assert!(f32::from(sidebar.size.width) <= 234.);
    })
    .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[gpui_kit::test]
fn headless_workbench_renders_on_windows_widths(cx: &mut gpui_kit::TestAppContext) {
    let dir = std::env::temp_dir().join(format!("rillsql-shell-ui-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    cx.update(gpui_kit::init);
    let (window, view) = cx.update(|cx| {
        let bounds = gpui_kit::Bounds {
            origin: gpui_kit::Point::default(),
            size: gpui_kit::size(gpui_kit::px(1360.), gpui_kit::px(800.)),
        };
        let (window, view) = gpui_kit::open_window(
            gpui_kit::WindowOptions {
                window_bounds: Some(gpui_kit::WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            cx,
            |window, cx| cx.new(|cx| super::view::Workbench::with_services(window, cx, Arc::clone(&services), false)),
        )
        .expect("workbench");
        let shell = view.read(cx).shell().clone();
        crate::app::install_workbench_actions(cx, &view);
        crate::app::install_menus(cx, &shell);
        (window, view)
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        view.update(cx, |view, cx| {
            view.perform("connections", cx);
            view.perform("menu-connection", cx);
            view.perform("settings", cx);
            view.perform("density-relaxed", cx);
            assert_eq!(view.model.shell.density, crate::app::Density::Relaxed);
            view.model.set_width(900);
            view.model.set_width(640);
            view.model.shell.sidebar_open = true;
            let frame = view.model.painted();
            assert!(frame.sidebar_overlay);
            assert!(!frame.sidebar_in_flex);
            assert!(!frame.toolbar_wrap, "顶栏单行，不占两行高度");
            assert_eq!(frame.occupied_width(), 640);
            assert!(super::layout::flex_sidebar_row_width(640, true, frame.pad) > 640);
            assert_eq!(view.model.copy_overflow(i18n::Locale::ZhCn), 0);
            assert_eq!(view.model.copy_overflow(i18n::Locale::En), 0);
        });
    });
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn save_names_the_failing_field_and_ssh_password_selects_password_auth() {
    let mut model = WorkbenchModel::new(Shell::default());
    model.connections.start_new(Engine::Mysql);
    let _ = model.dispatch("conn-save");
    assert_eq!(model.notice_key, Some("connections.validate.hostRequired"));
    model.connections.editor.as_mut().unwrap().host = "db.example.test".into();
    model.connections.editor.as_mut().unwrap().port = "0".into();
    let _ = model.dispatch("conn-save");
    assert_eq!(model.notice_key, Some("connections.validate.portRange"));
    model.set_field(DraftField::CaFile, "ca.pem".into());
    assert_eq!(model.connections.editor.as_ref().unwrap().ca_mode, CaMode::File);
    model.connections.toggle_ssh();
    model.set_field(DraftField::SshPassword, "secret".into());
    assert_eq!(model.connections.editor.as_ref().unwrap().ssh_auth, SshAuthKind::Password);
    assert!(model.connections.editor.as_ref().unwrap().ssh_key.is_empty());
    let _ = model.dispatch("conn-ssh-auth");
    assert_eq!(model.connections.editor.as_ref().unwrap().ssh_auth, SshAuthKind::Key);
    let _ = model.dispatch("conn-ca-mode");
    assert_eq!(model.connections.editor.as_ref().unwrap().ca_mode, CaMode::System);
}

#[tokio::test]
async fn injected_macos_and_linux_legacy_roots_import_without_rewriting_sources() {
    let root = std::env::temp_dir().join(format!("rillsql-legacy-roots-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let session = r#"{"activeId":"t1","tabs":[{"id":"t1","title":"查询","draft":"select 1","kind":"query"}]}"#;
    let shell = r#"{"theme":"dark"}"#;
    let cases = [
        (crate::state::Platform::Macos, root.join("mac-home"), root.join("mac-data")),
        (crate::state::Platform::Linux, root.join("linux-home"), root.join("linux-data")),
        (crate::state::Platform::Windows, root.join("win-home"), root.join("win-data")),
    ];
    for (platform, home, data) in cases {
        std::fs::create_dir_all(&data).unwrap();
        let (legacy, marker) = match platform {
            crate::state::Platform::Macos => {
                let marker = home.join("Library").join("WebKit").join("app.rillsql.desktop").join("WebsiteData").join("LocalStorage").join("https_tauri.localhost_0.localstorage");
                write_legacy_store(&marker, session, shell).await;
                (home.join("Library").join("WebKit").join("app.rillsql.desktop"), marker)
            }
            crate::state::Platform::Linux => {
                let marker = home.join("xdg").join("webkitgtk").join("app.rillsql.desktop").join("localstorage").join("localstorage.sqlite3");
                write_legacy_store(&marker, session, shell).await;
                (home.join("xdg").join("webkitgtk").join("app.rillsql.desktop"), marker)
            }
            crate::state::Platform::Windows => {
                let legacy = home.join("local").join("app.rillsql.desktop");
                std::fs::create_dir_all(&legacy).unwrap();
                let marker = legacy.join("marker.txt");
                std::fs::write(&marker, b"leave-me").unwrap();
                (legacy, marker)
            }
        };
        let found = super::view::legacy_webview_root_from(&super::view::LegacyRoots {
            platform,
            home: home.clone(),
            local_app_data: Some(home.join("local")),
            roaming_app_data: None,
            xdg_data_home: Some(home.join("xdg")),
        });
        assert_eq!(found.as_ref(), Some(&legacy), "{platform:?}");
        let before = std::fs::read(&marker).unwrap();
        let services = services_at(&data);
        let report = services.import_legacy(&legacy, platform).await;
        assert_eq!(std::fs::read(&marker).unwrap(), before, "{platform:?} 旧文件被改写");
        if platform == crate::state::Platform::Windows {
            assert!(!report.wrote_state);
        } else {
            assert!(report.imported, "{platform:?} {}", report.warning.unwrap_or_default());
        }
    }
    let _ = std::fs::remove_dir_all(&root);
}

async fn write_legacy_store(path: &std::path::Path, session: &str, shell: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let options = sqlx::sqlite::SqliteConnectOptions::new().filename(path).create_if_missing(true);
    let pool = sqlx::sqlite::SqlitePoolOptions::new().max_connections(1).connect_with(options).await.unwrap();
    sqlx::query("CREATE TABLE ItemTable (key TEXT, value BLOB)").execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO ItemTable (key, value) VALUES ('rillsql.session', ?)").bind(session.as_bytes()).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO ItemTable (key, value) VALUES ('rillsql.shell', ?)").bind(shell.as_bytes()).execute(&pool).await.unwrap();
    pool.close().await;
}

fn open_workbench(cx: &mut gpui_kit::TestAppContext, services: Arc<AppServices>, boot: bool) -> (gpui_kit::AnyWindowHandle, gpui_kit::Entity<super::view::Workbench>) {
    open_workbench_sized(cx, services, boot, 1360., 800.)
}

/// 同一套工作台，按指定窗口尺寸打开；紧凑窗口要测真实绘制几何，不能只改模型宽度。
fn open_workbench_sized(
    cx: &mut gpui_kit::TestAppContext,
    services: Arc<AppServices>,
    boot: bool,
    width: f32,
    height: f32,
) -> (gpui_kit::AnyWindowHandle, gpui_kit::Entity<super::view::Workbench>) {
    // 使用真实 AppServices/Tokio/SQLite 的集成夹具允许真实 I/O 唤醒。
    // 纯模型测试不使用此夹具，仍保留确定性调度检查。
    cx.executor().allow_parking();
    cx.update(gpui_kit::init);
    let (window, view) = cx.update(|cx| {
        let bounds = gpui_kit::Bounds {
            origin: gpui_kit::Point::default(),
            size: gpui_kit::size(gpui_kit::px(width), gpui_kit::px(height)),
        };
        let (window, view) = gpui_kit::open_window(
            gpui_kit::WindowOptions {
                window_bounds: Some(gpui_kit::WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            cx,
            |window, cx| cx.new(|cx| super::view::Workbench::with_services(window, cx, services, boot)),
        )
        .expect("workbench");
        let shell = view.read(cx).shell().clone();
        crate::app::install_workbench_actions(cx, &view);
        crate::app::install_menus(cx, &shell);
        (window, view)
    });
    (window, view)
}

#[gpui_kit::test]
fn closing_workbench_cancels_pending_job_receivers_on_the_ui_thread(cx: &mut gpui_kit::TestAppContext) {
    let dir = std::env::temp_dir().join(format!("rillsql-job-drop-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench(cx, services_at(&dir), false);
    let (tx, rx) = tokio::sync::oneshot::channel();
    cx.update(|cx| view.update(cx, |view, cx| view.receive_job("history", rx, cx)));
    cx.run_until_parked();
    assert!(!tx.is_closed(), "作业仍在等待后台回执");
    cx.update_window(window, |_, window, _| window.remove_window()).unwrap();
    cx.update(|_| drop(view));
    cx.run_until_parked();
    assert!(tx.is_closed(), "关闭工作台必须在界面线程取消接收任务，后台发送者不能继续持有其唤醒器");
    let _ = std::fs::remove_dir_all(&dir);
}

#[gpui_kit::test]
fn boot_refreshes_finish_and_close_is_not_blocked(cx: &mut gpui_kit::TestAppContext) {
    // 启动刷新在 tokio 上完成，工作线程会唤醒本调度器。不放行这次停车会被记成非确定性。
    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-shell-boot-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let (tx, rx) = std::sync::mpsc::channel();
    let seeded = Arc::clone(&services);
    let file = dir.join("boot.sqlite");
    jobs::runtime().spawn(async move {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&file).create_if_missing(true))
            .await
            .unwrap();
        pool.close().await;
        let mut draft = sample_draft(Engine::Sqlite);
        draft.file_path = file.display().to_string();
        crate::app::views::connections::save_draft(&seeded, draft, &Secrets::default()).await.unwrap();
        crate::app::views::history::save_named(
            &seeded,
            &SaveRequest {
                name: "欢迎".into(),
                sql: "select 1".into(),
                connection_id: String::new(),
                connection_name: String::new(),
                database_name: String::new(),
                engine: "sqlite".into(),
            },
        )
        .await
        .unwrap();
        let state = AppState::new(ShellSettings::default(), SessionState::default());
        std::fs::write(seeded.state.path(), crate::state::to_bytes(&state).unwrap()).unwrap();
        let _ = tx.send(());
    });
    rx.recv_timeout(std::time::Duration::from_secs(30)).unwrap();
    let (window, view) = open_workbench(cx, Arc::clone(&services), true);
    let started = std::time::Instant::now();
    let mut ready = false;
    while started.elapsed() < std::time::Duration::from_secs(30) {
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
        })
        .unwrap();
        cx.run_until_parked();
        ready = cx.update(|cx| {
            let model = &view.read(cx).model;
            model.running_jobs == 0 && model.pending_len() == 0 && !model.connections.rows.is_empty() && !model.history.saved.is_empty()
        });
        if ready {
            break;
        }
        std::thread::park_timeout(std::time::Duration::from_millis(25));
    }
    assert!(ready, "boot refreshes did not both finish");
    cx.update(|cx| {
        view.update(cx, |view, _| {
            assert_eq!(view.model.running_jobs, 0);
            assert!(view.model.allow_system_close());
            assert!(view.model.close_prompt.is_none());
        });
    });
    let _ = std::fs::remove_dir_all(&dir);
}

#[gpui_kit::test]
fn file_commands_call_gpui_dialogs(cx: &mut gpui_kit::TestAppContext) {
    // Export runs on the tokio runtime. A worker wake while this scheduler is
    // running is rejected as non-deterministic, so allow that parking and do
    // not wait here. The sealed round trip is execute_job in seven_engines.
    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-shell-dialogs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let (window, view) = open_workbench(cx, services, false);
    let export = dir.join("out.rillsqlcx");
    let token = crate::application::tasks::TaskToken { id: 0, generation: 0 };
    cx.update_window(window, |_, _, cx| {
        view.update(cx, |view, cx| {
            view.model.bundle_passphrase = "correct-horse".into();
            view.perform("conn-export", cx);
        });
    })
    .unwrap();
    assert!(cx.did_prompt_for_new_path());
    cx.simulate_new_path_selection(|directory| {
        assert_eq!(directory, dir.as_path());
        Some(export.clone())
    });
    assert!(!cx.did_prompt_for_new_path());
    cx.run_until_parked();
    cx.update(|cx| {
        view.update(cx, |view, _| {
            let path = view.model.export_path.clone().expect("export path");
            assert!(path.contains("out.rillsqlcx"), "{path}");
            assert_eq!(view.model.bundle_passphrase, "correct-horse");
            let request = job_request(&view.model, PendingJob::ExportConnections, token);
            assert_eq!(request.bundle_passphrase.as_deref(), Some("correct-horse"));
            assert_eq!(request.export_path.as_deref(), Some(path.as_str()));
        });
    });

    let import_file = dir.join("in.json");
    std::fs::write(&import_file, b"{}").unwrap();
    let opened = import_file.canonicalize().unwrap();
    cx.update_window(window, |_, _, cx| {
        view.update(cx, |view, cx| {
            view.model.bundle_passphrase.clear();
            view.perform("conn-import", cx);
        });
    })
    .unwrap();
    assert!(cx.did_prompt_for_paths());
    cx.simulate_path_prompt_response(|options| {
        assert!(options.files && !options.directories && !options.multiple);
        Some(vec![opened.clone()])
    });
    assert!(!cx.did_prompt_for_paths());
    cx.run_until_parked();
    cx.update(|cx| {
        view.update(cx, |view, _| {
            let path = view.model.import_path.clone().expect("import path");
            assert_eq!(path, opened.display().to_string());
            let request = job_request(&view.model, PendingJob::ImportConnections, token);
            assert!(request.bundle_passphrase.is_none());
            assert_eq!(request.import_path.as_deref(), Some(path.as_str()));
        });
    });

    let sqlite = dir.join("open.sqlite");
    std::fs::write(&sqlite, b"sqlite").unwrap();
    let sqlite_opened = sqlite.canonicalize().unwrap();
    cx.update_window(window, |_, _, cx| {
        view.update(cx, |view, cx| view.perform("open-file", cx));
    })
    .unwrap();
    assert!(cx.did_prompt_for_paths());
    cx.simulate_path_prompt_response(|_| Some(vec![sqlite_opened.clone()]));
    assert!(!cx.did_prompt_for_paths());
    cx.run_until_parked();
    cx.update(|cx| {
        view.update(cx, |view, _| {
            let draft = view.model.connections.editor.as_ref().expect("sqlite draft");
            assert_eq!(draft.file_path, sqlite_opened.display().to_string());
            assert_eq!(draft.engine, Engine::Sqlite);
            assert_eq!(view.model.shell.page, AppPage::Connections);
            assert_eq!(view.model.shell.opened_path.as_deref(), Some(sqlite_opened.as_path()));
        });
    });
    let _ = std::fs::remove_dir_all(&dir);
}

#[gpui_kit::test]
fn grid_edits_block_close_and_structure_admin_bind_the_target(cx: &mut gpui_kit::TestAppContext) {
    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-shell-grid-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let (window, view) = open_workbench(cx, services, false);
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.mark_table_dirty(cx));
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        view.update(cx, |view, _cx| {
            assert!(view.model.grid_dirty);
            assert!(!view.model.allow_system_close());
            let saved = serde_json::to_string(&view.model.snapshot()).unwrap();
            assert!(!saved.contains("uncommittedChanges"), "{saved}");
            assert!(!saved.contains('乙'), "{saved}");
            view.model.close_prompt = None;
            view.model.grid_dirty = false;
        });
    });
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.perform("open-structure-for:notes|笔记|main||customers|sqlite|dev", cx);
        });
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        let banner = view.read(cx).structure_banner(cx);
        assert!(banner.contains("笔记") && banner.contains("dev") && banner.contains("customers") && banner.contains("sqlite"), "{banner}");
    });
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.perform("open-admin", cx));
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        let banner = view.read(cx).admin_banner(cx);
        assert!(banner.contains("笔记") && banner.contains("dev") && banner.contains("customers") && banner.contains("sqlite"), "{banner}");
    });
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.perform("open-table:notes|笔记|main||customers|sqlite|dev", cx));
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        let banner = view.read(cx).table_banner(cx);
        assert!(banner.contains("笔记") && banner.contains("dev") && banner.contains("customers") && banner.contains("sqlite"), "{banner}");
        assert_eq!(view.read(cx).model.tool, WorkTool::Table);
    });
    let _ = std::fs::remove_dir_all(&dir);
}

/// 结构页字段表按内容区真实宽度排版：完整工作台里 900 / 640、开着侧栏、双主题、
/// 紧凑/标准密度、中英文下，行尾每个动作都在行框内、可见、可点。
/// 面板曾经拿窗口视口宽当内容宽，900 默认侧栏和 640 下右侧动作被 overflow_hidden 裁掉。
#[gpui_kit::test]
fn structure_row_actions_stay_inside_the_real_content_column(cx: &mut gpui_kit::TestAppContext) {
    use crate::app::views::structure::StructureMode;

    let dir = std::env::temp_dir().join(format!("rillsql-structure-width-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    // 目标不带连接 id：结构页不会发起 describe，快照由测试直接装进面板。
    let target = crate::app::views::grid::GridTarget {
        connection_id: String::new(),
        connection_name: String::new(),
        database: "app".into(),
        schema: "public".into(),
        table: "orders".into(),
        engine: "postgres".into(),
        environment: "dev".into(),
    };
    for width in [900., 640.] {
        let (window, view) = open_workbench_sized(cx, Arc::clone(&services), false, width, 800.);
        for (theme, dark) in [("theme-light", "浅色"), ("theme-dark", "深色")] {
            for density in ["density-compact", "density-standard"] {
                for (locale, language) in [("locale-zh", "中文"), ("locale-en", "英文")] {
                    let note = format!("{width} {dark} {density} {language}");
                    cx.update(|cx| {
                        view.update(cx, |view, cx| {
                            view.perform(theme, cx);
                            view.perform(density, cx);
                            view.perform(locale, cx);
                            // 侧栏开着才是被裁掉的场景：内容列少掉活动栏与停靠侧栏。
                            view.model.shell.sidebar_open = true;
                            view.model.shell.page = AppPage::Structure;
                            view.model.tool = WorkTool::Structure;
                            view.structure.update(cx, |pane, cx| {
                                pane.bind(target.clone());
                                pane.install_snapshot_for_test(4);
                                pane.session.mode = StructureMode::Create;
                                pane.session.table_name = "notes".into();
                                cx.notify();
                            });
                            cx.notify();
                        });
                    });
                    for mode in [StructureMode::Create, StructureMode::Existing] {
                        let mode_note = if mode == StructureMode::Create { "新建" } else { "已有表" };
                        let note = format!("{note} {mode_note}");
                        cx.update(|cx| {
                            view.update(cx, |view, cx| {
                                view.structure.update(cx, |pane, cx| {
                                    pane.session.mode = mode;
                                    cx.notify();
                                });
                            });
                        });
                        let layout_width = cx.update(|cx| view.read(cx).model.layout().content_width as f32);
                        let (content_width, viewport, workbench_w, body, row, actions) = cx
                            .update_window(window, |_, window, cx| {
                                window.render_frame(cx);
                                let content_width = view.read(cx).structure.read(cx).content_width();
                                let body = window.find("structure-body").bounds();
                                let row = window.find("structure-row-columns-0").bounds();
                                // 新建表才有多出的上移/下移；已有表每行只有标记删除。
                                let ids: &[&str] = if mode == StructureMode::Create {
                                    &["structure-up-1", "structure-down-1", "structure-drop-col-1"]
                                } else {
                                    &["structure-drop-col-1"]
                                };
                                let actions: Vec<_> = ids
                                    .iter()
                                    .map(|id| {
                                        let element = window.find(*id);
                                        (*id, element.bounds(), element.visible(), element.role())
                                    })
                                    .collect();
                                let workbench_w = f32::from(window.find("workbench").bounds().size.width);
                                (content_width, f32::from(window.viewport_size().width), workbench_w, body, row, actions)
                            })
                            .unwrap();
                        assert!((content_width - layout_width).abs() < 0.5, "{note}: 面板内容宽 {content_width} 应来自布局 {layout_width}");
                        assert!(content_width < viewport - 1., "{note}: 内容宽不能是窗口视口宽 {viewport}");
                        let row_right = f32::from(row.origin.x + row.size.width);
                        let body_right = f32::from(body.origin.x + body.size.width);
                        assert!(row_right <= body_right + 0.5, "{note}: 整行越出滚动内容（{row_right} > {body_right}）");
                        for (id, bounds, visible, role) in actions {
                            assert!(visible, "{note}: {id} 应可见");
                            assert_eq!(role, Some(gpui_kit::Role::Button), "{note}: {id} 的角色");
                            let left = f32::from(bounds.origin.x);
                            let right = f32::from(bounds.origin.x + bounds.size.width);
                            assert!(left >= f32::from(row.origin.x) - 0.5, "{note}: {id} 越出整行左侧");
                            assert!(right <= row_right + 0.5, "{note}: {id} 被挤出整行（{right} > {row_right}）");
                            assert!(right <= workbench_w + 0.5, "{note}: {id} 越出工作台");
                        }
                        // 动作不只是画得下，也要真的改草稿。
                        // 640 的侧栏是覆盖式，遮罩会吞掉内容点击（侧栏浮在内容之上的既有行为）；
                        // 收起后内容列宽度不变（覆盖与收起都是 588），在这里验证动作可点。
                        if width < 700. {
                            cx.update(|cx| view.update(cx, |view, _| view.model.shell.sidebar_open = false));
                            cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
                        }
                        let before: Vec<String> = cx.update(|cx| view.read(cx).structure.read(cx).session.kept_columns.iter().map(|column| column.name.clone()).collect());
                        cx.update_window(window, |_, window, cx| window.click("structure-drop-col-1", cx)).unwrap();
                        assert!(cx.update(|cx| view.read(cx).structure.read(cx).session.kept_columns[1].dropped), "{note}: 标记删除可点");
                        cx.update_window(window, |_, window, cx| window.click("structure-drop-col-1", cx)).unwrap();
                        assert!(!cx.update(|cx| view.read(cx).structure.read(cx).session.kept_columns[1].dropped), "{note}: 再点一次取消标记");
                        if mode == StructureMode::Create {
                            cx.update_window(window, |_, window, cx| window.click("structure-down-0", cx)).unwrap();
                            let after: Vec<String> = cx.update(|cx| view.read(cx).structure.read(cx).session.kept_columns.iter().map(|column| column.name.clone()).collect());
                            assert_eq!(after[0], before[1], "{note}: 下移可点且换到草稿顺序里");
                            assert_eq!(after[1], before[0]);
                        }
                        if width < 700. {
                            cx.update(|cx| view.update(cx, |view, _| view.model.shell.sidebar_open = true));
                        }
                    }
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// 管理页空态（和表头）的「立即连接」不是死路：外壳取走请求，走树/连接行同一条按需连接入口，
/// 连上后由 note_connection 让管理页自己重读清单。请求只消费一次，不重复排队。
#[gpui_kit::test]
fn admin_connect_action_enters_the_shared_on_demand_flow(cx: &mut gpui_kit::TestAppContext) {
    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-admin-connect-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let (window, view) = open_workbench(cx, services, false);
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            // 未连接的 SQLite 连接：与对象树的「数据库管理」是同一个目标。
            view.model.connections.rows.push(record_for("notes", "笔记", Engine::Sqlite));
            view.model.connections.status.remove("notes");
            view.perform("open-structure-for:notes|笔记|main||customers|sqlite|dev", cx);
            view.perform("open-admin", cx);
        });
        window.render_frame(cx);
    })
    .unwrap();
    // 外壳同步连接状态会触发一次重读；读到"清单读完、但没连上"的状态后空态才出现。
    // 读取跑在 tokio 上，按本文件既有做法放行等待，不靠猜时间。
    let started = std::time::Instant::now();
    while started.elapsed() < std::time::Duration::from_secs(10) {
        cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
        cx.run_until_parked();
        if cx.update(|cx| view.read(cx).admin.read(cx).session.inventory.probed) {
            break;
        }
        std::thread::park_timeout(std::time::Duration::from_millis(25));
    }
    let empty = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.try_find("admin-offline").is_some()
        })
        .unwrap();
    assert!(empty, "未连接的管理页要先给出带「立即连接」的空态");
    // 点空态主操作：页面只写意图并重绘。
    cx.update_window(window, |_, window, cx| window.click("admin-body-connect", cx)).unwrap();
    // 下一帧外壳取走请求，接到同一条按需连接入口。
    cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(
            view.model.connections.status.get("notes"),
            Some(&LinkStatus::Connecting),
            "空态主操作要真的发起连接，不能只写进 AdminPane::connect_request"
        );
        let queued: Vec<_> = view.model.connect_intents.iter().filter(|intent| intent.connection_id == "notes").collect();
        assert_eq!(queued.len(), 1, "请求只排一次");
        assert_eq!(queued[0].action, super::conn_modal::ConnectAction::Expand, "接的是树/连接行同一条入口：连上后停在管理页并刷新清单");
        assert!(view.admin.read(cx).connect_request.is_none(), "请求已被外壳消费");
    });
    // 再渲染一帧不重复排队：消费一次就够。
    cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.model.connect_intents.iter().filter(|intent| intent.connection_id == "notes").count(), 1, "不重复排队");
    });
    let _ = std::fs::remove_dir_all(&dir);
}

#[gpui_kit::test]
fn object_tree_opens_the_table_and_structure_and_commits_through_the_shared_grid(cx: &mut gpui_kit::TestAppContext) {
    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-shell-tree-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let (window, view) = open_workbench(cx, services, false);
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.plant_object_tree(cx);
            view.perform("object:notes|笔记|main||customers|sqlite|dev", cx);
            view.perform("tree-open-data", cx);
        });
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        let banner = view.read(cx).table_banner(cx);
        assert!(banner.contains("笔记") && banner.contains("dev") && banner.contains("customers") && banner.contains("sqlite"), "{banner}");
        assert_eq!(view.read(cx).model.tool, WorkTool::Table);
        assert_eq!(view.read(cx).model.shell.page, AppPage::Workspace);
    });
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.perform("open-structure", cx));
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        let banner = view.read(cx).structure_banner(cx);
        assert!(banner.contains("笔记") && banner.contains("dev") && banner.contains("customers") && banner.contains("sqlite"), "{banner}");
        assert_eq!(view.read(cx).model.shell.page, AppPage::Structure);
        assert_eq!(view.read(cx).model.tool, WorkTool::Structure);
    });
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.perform("tree-open-data", cx);
            view.mark_table_dirty(cx);
            view.perform("commit", cx);
            view.perform("grid-copy-row", cx);
        });
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        let (open, sql) = view.read(cx).table_preview(cx);
        assert!(open, "确认前应先打开共享网格预览");
        assert!(sql.contains("乙"), "{sql}");
        assert_ne!(view.read(cx).model.copied, "grid-copy-row");
        assert!(view.read(cx).model.copied.contains('乙') || view.read(cx).model.copied.contains('甲'), "{}", view.read(cx).model.copied);
    });
    let _ = std::fs::remove_dir_all(&dir);
}

#[gpui_kit::test]
fn explicit_null_and_copy_go_through_the_shared_shell_grid(cx: &mut gpui_kit::TestAppContext) {
    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-shell-null-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench(cx, services_at(&dir), false);
    cx.update_window(window, |_, window, cx| {
        // 先让表项完成一次绑定，随后的 sample 暂存才不会被重新打开表时清掉。
        view.update(cx, |view, cx| view.perform("open-table:notes|笔记|main||customers|sqlite|dev", cx));
        window.render_frame(cx);
    })
    .unwrap();
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.mark_table_dirty(cx));
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        view.update(cx, |view, cx| {
            assert_eq!(view.model.tool, WorkTool::Table);
            assert_eq!(view.table_cell(0, 1, cx), "乙");
            view.perform("copy-region", cx);
            assert_eq!(view.model.copied, "乙");
            // 显式 NULL 不走特殊文本：命令直接把这一格 stage 成数据库 NULL。
            view.perform("grid-set-null", cx);
            assert_eq!(view.table_cell(0, 1, cx), "NULL");
            assert!(view.table_uncommitted(cx));
            view.perform("copy-region", cx);
            assert_eq!(view.model.copied, "NULL");
            view.perform("commit", cx);
            let (open, sql) = view.table_preview(cx);
            assert!(open, "显式 NULL 也要经过预览");
            assert!(sql.contains("= NULL"), "{sql}");
            assert!(!sql.contains("'NULL'"), "字符串 NULL 才能写成 'NULL'：{sql}");
        });
    });
    let _ = std::fs::remove_dir_all(&dir);
}

#[gpui_kit::test]
fn staged_table_survives_switching_to_structure_and_back(cx: &mut gpui_kit::TestAppContext) {
    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-shell-stage-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench(cx, services_at(&dir), false);
    let spec = "notes|笔记|main||customers|sqlite|dev";
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.perform(&format!("open-table:{spec}"), cx));
        window.render_frame(cx);
    })
    .unwrap();
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.mark_table_dirty(cx));
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        assert!(view.read(cx).table_uncommitted(cx), "先有暂存");
        let table = view.read(cx).model.tabs.iter().find(|tab| tab.kind == WorkItemKind::Table).cloned().expect("table item");
        assert!(table.uncommitted_cells, "暂存应归到当前表项");
    });

    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.perform(&format!("open-structure-for:{spec}"), cx);
        });
        window.render_frame(cx);
    })
    .unwrap();
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.perform(&format!("open-table:{spec}"), cx));
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        assert!(view.read(cx).table_uncommitted(cx), "同表切到结构再回来，暂存必须仍在");
        let table = view.read(cx).model.tabs.iter().find(|tab| tab.kind == WorkItemKind::Table).cloned().expect("table item");
        assert!(table.uncommitted_cells, "切回后标签仍应标记未提交");
    });

    // 用户明确关闭带暂存的表项后，网格暂存随项清理，窗口关闭不再被它阻塞。
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            let id = view.model.tabs.iter().find(|tab| tab.kind == WorkItemKind::Table).map(|tab| tab.id.clone()).expect("table item");
            view.model.request_close_tab(&id);
            view.perform("close-leave", cx);
        });
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        let view = view.read(cx);
        assert!(view.model.tabs.iter().all(|tab| tab.kind != WorkItemKind::Table));
        assert!(!view.table_uncommitted(cx), "关闭表项后不应留下孤儿暂存");
        assert!(!view.model.grid_dirty);
        use crate::app::shell::close::{evaluate, CloseKind};
        assert_eq!(evaluate(view.model.facts()), CloseKind::Allow, "孤儿暂存不得永久阻塞关闭");
    });
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn batch_close_skips_running_items_without_closing_them() {
    let mut model = WorkbenchModel::new(Shell::default());
    assert!(model.dispatch("new-query").is_some());
    let keep = model.active_tab.clone().expect("query");
    assert!(model.dispatch("open-table:notes|笔记|main||customers|sqlite|dev").is_some());
    let running = model.active_tab.clone().expect("table");
    assert!(model.dispatch("open-table:notes|笔记|main||orders|sqlite|dev").is_some());
    model.tabs.iter_mut().find(|tab| tab.id == running).unwrap().running = true;
    model.activate_item(&keep);

    assert!(model.dispatch("tab-close-others").is_some());
    assert!(model.tabs.iter().any(|tab| tab.id == running), "运行中的项不得被批量关闭");
    assert_eq!(model.notice_key, Some("shell.update.busy"));
    assert_eq!(model.tabs.len(), 2, "另一项应继续关闭，只留下查询与运行中的表");
    assert_eq!(model.active_tab.as_deref(), Some(keep.as_str()));
}

/// 外键跳转与"生成外键查询"是两条不同的路：跳转真的打开引用表的工作项并带上定位查询，
/// 原来那张表的选择、滚动、暂存和查看目标都留在原处；生成查询只复制 SQL，不开工作项。
#[gpui_kit::test]
fn foreign_key_jump_opens_the_referenced_row_and_keeps_the_source_state(cx: &mut gpui_kit::TestAppContext) {
    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-shell-fk-jump-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let database = dir.join("notes.sqlite");
    let services = std::thread::scope(|scope| {
        let database = database.clone();
        let dir = dir.clone();
        scope
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
                runtime.block_on(async move {
                    let pool = sqlx::sqlite::SqlitePoolOptions::new()
                        .max_connections(1)
                        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).create_if_missing(true))
                        .await
                        .unwrap();
                    for sql in [
                        "CREATE TABLE authors (id INTEGER PRIMARY KEY, name TEXT)",
                        "CREATE TABLE books (id INTEGER PRIMARY KEY, author_id INTEGER, title TEXT, FOREIGN KEY (author_id) REFERENCES authors(id))",
                        "INSERT INTO authors VALUES (1, '甲'), (2, '乙')",
                        "INSERT INTO books VALUES (1, 1, '书'), (2, 2, '册')",
                    ] {
                        sqlx::query(sql).execute(&pool).await.unwrap();
                    }
                    pool.close().await;
                    let services = Arc::new(AppServices::new(Arc::new(Service::new_for_test(dir.join("connections.sqlite")))));
                    services.connections.open_sqlite_for_test("notes", "笔记", &database).await.unwrap();
                    services
                })
            })
            .join()
            .unwrap()
    });
    let (window, view) = open_workbench(cx, services, false);
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.perform("open-table:notes|笔记|main||books|sqlite|dev", cx));
        window.render_frame(cx);
    })
    .unwrap();
    let table = cx.update(|cx| view.read(cx).table.clone());
    let books = cx.update(|cx| {
        view.read(cx)
            .model
            .tabs
            .iter()
            .find(|tab| tab.kind == WorkItemKind::Table)
            .map(|tab| tab.id.clone())
            .expect("表工作项")
    });
    // 等真实分页读取落地：外键和定位都来自这次读取。
    let started = std::time::Instant::now();
    loop {
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
        if cx.update(|cx| !table.read(cx).fetching() && table.read(cx).loaded_len() > 0) {
            break;
        }
        assert!(started.elapsed() < std::time::Duration::from_secs(30), "分页读取没有落地");
        std::thread::park_timeout(std::time::Duration::from_millis(20));
    }
    cx.update(|cx| {
        table.update(cx, |pane, cx| {
            pane.choose_cell(0, 1);
            assert!(pane.has_reference(), "author_id 是真实外键列，行也定位到了");
            // 暂存一个修改并打开查看面板：跳转后它们必须还在。
            assert!(pane.stage_cell(0, 2, "改名"), "标题列可写");
            let _ = pane.command("grid-value-open", cx);
            assert!(pane.state_snapshot().3.is_some(), "查看目标已打开");
        });
    });
    let before = cx.update(|cx| table.read(cx).state_snapshot());
    assert!(before.2, "跳转前有暂存");
    let runs_before = cx.update(|cx| view.read(cx).query_runs(cx));
    let tabs_before = cx.update(|cx| view.read(cx).model.tabs.len());

    // 真正的跳转：走网格命令，经外壳打开引用表的工作项。
    let copied = cx.update(|cx| table.update(cx, |pane, cx| pane.command("grid-fk-jump", cx)));
    assert!(copied.is_none(), "跳转不是把 SQL 塞进剪贴板");
    assert_eq!(cx.update(|cx| view.read(cx).model.tabs.len()), tabs_before + 1, "跳转打开了一个工作项");

    let (item, sql, connection, database_name, engine, active_is_query) = cx.update(|cx| {
        let view = view.read(cx);
        let item = view.model.tabs.iter().find(|tab| tab.kind.is_query()).cloned().expect("引用表工作项");
        (
            item.id.clone(),
            item.draft.clone(),
            item.connection_id.clone(),
            item.database.clone(),
            item.engine.clone(),
            view.model.active_tab.as_deref() == Some(item.id.as_str()),
        )
    });
    assert!(active_is_query, "跳转后当前工作项就是引用表的查询");
    assert_eq!(sql, "SELECT id FROM main.authors WHERE id = 1", "单键定位查询");
    assert_eq!(connection.as_deref(), Some("notes"), "目标仍是被引用的那条连接");
    assert_eq!(database_name.as_deref(), Some("main"));
    assert_eq!(engine.as_deref(), Some("sqlite"));
    // 连接已打开：定位查询真的执行了，不是只摆在编辑器里。
    assert_eq!(cx.update(|cx| view.read(cx).query_runs(cx)), runs_before + 1, "定位 SELECT 被执行");
    // 来源状态一点没动。
    assert_eq!(cx.update(|cx| table.read(cx).state_snapshot()), before, "选择、滚动、暂存和查看目标都保留");

    // 返回原来的表工作项：还是同一份状态。
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, _| {
            assert!(view.model.activate_item(&books), "回到表工作项");
        });
        window.render_frame(cx);
    })
    .unwrap();
    assert_eq!(cx.update(|cx| table.read(cx).state_snapshot()), before, "返回后状态不变");
    assert!(cx.update(|cx| table.read(cx).uncommitted()), "暂存仍在");

    // 生成外键查询：只复制 SQL，不开工作项，也不动状态。
    let tabs_now = cx.update(|cx| view.read(cx).model.tabs.len());
    let copied = cx.update(|cx| table.update(cx, |pane, cx| pane.command("grid-fk-select", cx)));
    assert_eq!(
        copied.as_deref(),
        Some("SELECT id FROM main.authors WHERE id = 1"),
        "生成查询复制同一段 SQL"
    );
    assert_eq!(cx.update(|cx| view.read(cx).model.tabs.len()), tabs_now, "复制不打开工作项");
    assert_eq!(cx.update(|cx| table.read(cx).state_snapshot()), before, "复制也不动来源状态");
    let _ = item;
    let _ = std::fs::remove_dir_all(&dir);
}

#[gpui_kit::test]
fn second_workbench_commit_stays_on_preview_without_writing(cx: &mut gpui_kit::TestAppContext) {
    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-shell-commit-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let database = dir.join("notes.sqlite");
    let services = std::thread::scope(|scope| {
        let database = database.clone();
        let dir = dir.clone();
        scope
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
                runtime.block_on(async move {
                    let setup = sqlx::sqlite::SqlitePoolOptions::new()
                        .max_connections(1)
                        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).create_if_missing(true))
                        .await
                        .unwrap();
                    sqlx::query("CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT)").execute(&setup).await.unwrap();
                    sqlx::query("INSERT INTO customers (id, name) VALUES (1, '甲')").execute(&setup).await.unwrap();
                    setup.close().await;
                    let services = Arc::new(AppServices::new(Arc::new(Service::new_for_test(dir.join("connections.sqlite")))));
                    services.connections.open_sqlite_for_test("notes", "笔记", &database).await.unwrap();
                    services
                })
            })
            .join()
            .unwrap()
    });
    let (window, view) = open_workbench(cx, services, false);
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.model.shell.page = AppPage::Workspace;
            view.model.tool = WorkTool::Table;
            view.mark_table_dirty(cx);
            view.perform("commit", cx);
        });
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        let (open, sql) = view.read(cx).table_preview(cx);
        assert!(open, "第一次 commit 只打开预览");
        assert!(sql.contains("乙"), "{sql}");
        assert_eq!(view.read(cx).table_cell(0, 1, cx), "乙");
        assert!(view.read(cx).table_uncommitted(cx));
    });
    assert_eq!(customer_names(&database), vec![(1, "甲".to_owned())]);

    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.perform("commit", cx));
        window.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        let (open, sql) = view.read(cx).table_preview(cx);
        assert!(open, "预览已打开时再发工作台 commit 仍只停留在预览");
        assert!(sql.contains("乙"), "{sql}");
        assert_eq!(view.read(cx).table_cell(0, 1, cx), "乙");
        assert!(view.read(cx).table_uncommitted(cx));
    });
    assert_eq!(customer_names(&database), vec![(1, "甲".to_owned())]);
    let _ = std::fs::remove_dir_all(&dir);
}

fn customer_names(path: &std::path::Path) -> Vec<(i64, String)> {
    let path = path.to_path_buf();
    std::thread::scope(|scope| {
        scope
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
                runtime.block_on(async move {
                    let pool = sqlx::sqlite::SqlitePoolOptions::new()
                        .max_connections(1)
                        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(path).read_only(true))
                        .await
                        .unwrap();
                    let rows: Vec<(i64, String)> = sqlx::query_as("SELECT id, name FROM customers ORDER BY id").fetch_all(&pool).await.unwrap();
                    pool.close().await;
                    rows
                })
            })
            .join()
            .unwrap()
    })
}

fn people_count(path: &std::path::Path) -> i64 {
    let path = path.to_path_buf();
    std::thread::scope(|scope| {
        scope
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
                runtime.block_on(async move {
                    let pool = sqlx::sqlite::SqlitePoolOptions::new()
                        .max_connections(1)
                        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&path).read_only(true))
                        .await
                        .unwrap();
                    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM people").fetch_one(&pool).await.unwrap();
                    pool.close().await;
                    count
                })
            })
            .join()
            .unwrap()
    })
}

fn pump_exchange(
    cx: &mut gpui_kit::TestAppContext,
    window: gpui_kit::AnyWindowHandle,
    view: &gpui_kit::Entity<super::view::Workbench>,
    ready: impl FnMut(&super::view::Workbench, &gpui_kit::App) -> bool,
) {
    if pump_for(cx, window, view, std::time::Duration::from_secs(30), ready) {
        return;
    }
    let text = cx.update(|app| view.read(app).exchange_text(app));
    panic!("导入导出没有在 30 秒内结束：\n{text}");
}

#[gpui_kit::test]
fn exchange_commands_prompt_map_cancel_and_fail_from_the_workbench(cx: &mut gpui_kit::TestAppContext) {
    use crate::app::shell::close::{evaluate, CloseKind};
    use crate::export::Phase;

    cx.executor().allow_parking();
    let _workbook = crate::export::workbook_test_lock();
    let dir = std::env::temp_dir().join(format!("rillsql-exchange-product-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("source.sqlite");
    let target = dir.join("target.sqlite");
    let csv = dir.join("people.csv");
    std::fs::write(&csv, "name\n乙\n丙\n").unwrap();
    let services = std::thread::scope(|scope| {
        let source = source.clone();
        let target = target.clone();
        let dir = dir.clone();
        scope
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
                runtime.block_on(async move {
                    for (path, seed) in [(&source, true), (&target, false)] {
                        let pool = sqlx::sqlite::SqlitePoolOptions::new()
                            .max_connections(1)
                            .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(path).create_if_missing(true))
                            .await
                            .unwrap();
                        sqlx::query("CREATE TABLE people (name TEXT NOT NULL)").execute(&pool).await.unwrap();
                        if seed {
                            sqlx::query("INSERT INTO people (name) VALUES ('甲')").execute(&pool).await.unwrap();
                        }
                        pool.close().await;
                    }
                    let services = Arc::new(AppServices::new(Arc::new(Service::new_for_test(dir.join("connections.sqlite")))));
                    services.connections.open_sqlite_for_test("notes", "笔记", &source).await.unwrap();
                    services
                })
            })
            .join()
            .unwrap()
    });
    let (window, view) = open_workbench(cx, services, false);
    let csv_opened = csv.canonicalize().unwrap();
    let target_opened = target.canonicalize().unwrap();

    cx.update_window(window, |_, _, cx| {
        view.update(cx, |view, cx| view.perform("open-exchange", cx));
    })
    .unwrap();
    cx.update_window(window, |_, _, cx| {
        view.update(cx, |view, cx| view.perform("exchange-dump", cx));
    })
    .unwrap();
    assert!(cx.did_prompt_for_new_path(), "全库导出应打开保存对话框");
    let dump_path = dir.join("missing.sql");
    cx.simulate_new_path_selection(|_| Some(dump_path.clone()));
    pump_exchange(cx, window, &view, |view, app| {
        let text = view.exchange_text(app);
        !view.exchange_running_now(app) && text.contains("没有当前连接")
    });
    cx.update(|app| {
        let text = view.read(app).exchange_text(app);
        assert!(text.contains("没有当前连接"), "{text}");
        assert!(!text.contains("开始全库导出"), "{text}");
        assert!(!text.contains("进行中"), "{text}");
        assert!(!view.read(app).exchange_running_now(app));
        let tasks = view.read(app).exchange_tasks(app);
        assert!(tasks.iter().any(|task| task.phase == Phase::Failed && !task.success), "{tasks:?}");
        assert!(tasks.iter().all(|task| task.phase != Phase::Running), "{tasks:?}");
    });
    assert!(!dump_path.exists());
    assert_eq!(people_count(&source), 1);

    cx.update_window(window, |_, _, cx| {
        view.update(cx, |view, cx| {
            view.plant_exchange_sheet("notes|笔记|main||people|sqlite|local", vec!["name".to_owned()], vec![vec![Some("甲".to_owned())]], cx);
        });
    })
    .unwrap();

    cx.update_window(window, |_, _, cx| {
        view.update(cx, |view, cx| view.perform("exchange-export-loaded", cx));
    })
    .unwrap();
    assert!(cx.did_prompt_for_new_path(), "已读取导出应打开保存对话框");
    let loaded_path = dir.join("loaded.csv");
    cx.simulate_new_path_selection(|_| Some(loaded_path.clone()));
    pump_exchange(cx, window, &view, |view, app| loaded_path.exists() && !view.exchange_running_now(app));
    let loaded = String::from_utf8_lossy(&std::fs::read(&loaded_path).unwrap()).into_owned();
    assert!(loaded_path.is_absolute());
    assert!(loaded.contains('甲'), "{loaded}");
    assert!(!view_has_partial(&dir), "已读取导出不应留下 .partial");
    cx.update(|app| {
        assert!(!view.read(app).exchange_running_now(app));
        let text = view.read(app).exchange_text(app);
        assert!(text.contains("目标 people"), "{text}");
        assert!(!text.contains("进行中"), "{text}");
    });

    let before_books = crate::export::workbook_initializations();
    cx.update_window(window, |_, _, cx| {
        view.update(cx, |view, cx| {
            view.perform("exchange-format:xlsx", cx);
            view.perform("exchange-export-full", cx);
        });
    })
    .unwrap();
    assert!(cx.did_prompt_for_new_path(), "全量导出应打开保存对话框");
    let full_path = dir.join("full.xlsx");
    cx.simulate_new_path_selection(|_| Some(full_path.clone()));
    pump_exchange(cx, window, &view, |view, app| full_path.exists() && !view.exchange_running_now(app));
    let raw = std::fs::read(&full_path).expect("全量 xlsx");
    assert!(full_path.is_absolute());
    assert!(raw.starts_with(b"PK"), "全量 xlsx 应是 zip 包");
    assert!(crate::export::workbook_initializations() > before_books, "全量 xlsx 应走 rust_xlsxwriter");
    let sheet = crate::export::read_xlsx(&raw).expect("读取全量 xlsx");
    assert!(sheet.rows.iter().any(|row| row.iter().any(|cell| cell.as_deref() == Some("甲"))), "{sheet:?}");
    assert!(!view_has_partial(&dir));
    cx.update(|app| {
        let text = view.read(app).exchange_text(app);
        assert!(!view.read(app).exchange_running_now(app), "{text}");
        assert!(!text.contains("进行中"), "{text}");
        assert!(view.read(app).exchange_tasks(app).iter().any(|task| task.phase == Phase::Done && task.success), "{text}");
    });

    cx.update_window(window, |_, _, cx| {
        view.update(cx, |view, cx| view.perform("exchange-pick", cx));
    })
    .unwrap();
    assert!(cx.did_prompt_for_paths(), "导入应打开文件对话框");
    cx.simulate_path_prompt_response(|_| Some(vec![csv_opened.clone()]));
    pump_exchange(cx, window, &view, |view, app| view.exchange_text(app).contains("映射 name=name"));
    cx.update(|app| {
        let text = view.read(app).exchange_text(app);
        assert!(text.contains("映射 name=name"), "{text}");
        assert!(text.contains("预览阶段不写入"), "{text}");
    });
    assert_eq!(people_count(&source), 1, "确认前不得写库");

    cx.update_window(window, |_, _, cx| {
        view.update(cx, |view, cx| {
            view.perform("exchange-map:name=", cx);
            view.perform("exchange-confirm", cx);
        });
    })
    .unwrap();
    pump_exchange(cx, window, &view, |view, app| {
        view.exchange_tasks(app).iter().any(|task| task.kind == "import" && task.phase != Phase::Running)
    });
    cx.update(|app| {
        let text = view.read(app).exchange_text(app);
        let tasks = view.read(app).exchange_tasks(app);
        let import = tasks.iter().find(|task| task.kind == "import").expect(&text);
        assert_eq!(import.phase, Phase::Blocked, "{text}");
        assert!(!import.success, "{text}");
        assert!(import.detail.contains("必填"), "{text}");
        assert!(!text.contains("进行中"), "{text}");
        assert!(!view.read(app).exchange_running_now(app));
    });
    assert_eq!(people_count(&source), 1, "缺映射时确认也不得写库");

    cx.update_window(window, |_, _, cx| {
        view.update(cx, |view, cx| {
            view.perform("exchange-map:name=name", cx);
            view.perform("exchange-confirm", cx);
            view.sync_exchange_running(cx);
            assert!(view.model.exchange_running, "{}", view.exchange_text(cx));
            assert_eq!(evaluate(view.model.facts()), CloseKind::BlockedRunning);
            assert!(!view.model.allow_system_close());
            assert!(view.model.close_prompt.is_none());
            view.perform("exchange-task-cancel", cx);
        });
    })
    .unwrap();
    pump_exchange(cx, window, &view, |view, app| !view.exchange_running_now(app));
    cx.run_until_parked();
    cx.update(|app| {
        let text = view.read(app).exchange_text(app);
        let tasks = view.read(app).exchange_tasks(app);
        let import = tasks.iter().rev().find(|task| task.kind == "import").expect(&text);
        assert_eq!(import.phase, Phase::Cancelled, "{text}");
        assert!(!import.success, "{text}");
        assert!(import.detail.contains("已取消"), "{text}");
        assert!(!import.detail.contains("进行中"), "{text}");
        assert!(!text.contains("进行中"), "{text}");
        assert!(tasks.iter().all(|task| task.phase != Phase::Running), "{text}");
        assert!(!view.read(app).exchange_running_now(app));
    });
    assert_eq!(people_count(&source), 1, "取消后不得继续写入");

    cx.update_window(window, |_, _, cx| {
        view.update(cx, |view, cx| view.perform("exchange-migrate", cx));
    })
    .unwrap();
    assert!(cx.did_prompt_for_paths(), "跨库迁移应打开目标库");
    cx.simulate_path_prompt_response(|_| Some(vec![target_opened.clone()]));
    pump_exchange(cx, window, &view, |view, app| {
        view.exchange_tasks(app)
            .iter()
            .rev()
            .find(|task| task.kind == "migrate")
            .is_some_and(|task| task.phase != Phase::Running)
    });
    cx.update(|app| {
        let text = view.read(app).exchange_text(app);
        let tasks = view.read(app).exchange_tasks(app);
        let migrated = tasks.iter().rev().find(|task| task.kind == "migrate").expect(&text);
        assert_ne!(migrated.phase, Phase::Running, "{text}");
        assert!(matches!(migrated.phase, Phase::Done | Phase::Partial | Phase::Cancelled), "{text}");
        assert!(!text.contains("进行中"), "{text}");
        if migrated.phase == Phase::Done {
            assert!(migrated.success, "{text}");
        } else {
            assert!(!migrated.success, "{text}");
        }
    });
    assert_eq!(people_count(&target), 1, "迁移应只写入实际读到的行");

    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.sync_exchange_running(cx);
            assert!(!view.model.exchange_running);
            view.mark_table_dirty(cx);
        });
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|app| {
        view.update(app, |view, _| {
            assert!(!view.model.exchange_running);
            assert_eq!(evaluate(view.model.facts()), CloseKind::Confirm { uncommitted: true, draft: false });
        });
    });
    let _ = std::fs::remove_dir_all(&dir);
}

#[gpui_kit::test]
fn query_commands_refuse_a_hidden_query_while_a_table_item_is_active(cx: &mut gpui_kit::TestAppContext) {
    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-query-focus-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let (window, view) = open_workbench(cx, services, false);

    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.perform("new-query", cx);
            let query = view.model.active_tab.clone().expect("query item");
            view.model.set_query_target(&query, Some("notes".to_owned()), Some("笔记".to_owned()), Some("main".to_owned()), Some("sqlite".to_owned()));
            view.model.tabs.iter_mut().find(|tab| tab.id == query).unwrap().draft = "select   1".to_owned();
            // 直接改模型不经过命令：按事件到来整理一次（生产里由通知触发）。
            view.settle(cx);
        });
        window.render_frame(cx);
        view.update(cx, |view, cx| assert_eq!(view.query_draft(cx), "select   1"));
    })
    .unwrap();

    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.perform("open-table:notes|笔记|main||customers|sqlite|dev", cx);
        });
        window.render_frame(cx);
    })
    .unwrap();
    let (query, runs) = cx.update(|cx| {
        let view = view.read(cx);
        (view.model.tabs.iter().find(|tab| tab.kind.is_query()).unwrap().id.clone(), view.query_runs(cx))
    });
    assert_eq!(runs, 0);

    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            assert_eq!(view.model.tool, WorkTool::Table);
            view.perform("execute-query", cx);
            assert_eq!(view.query_runs(cx), runs, "表格工作项不能执行隐藏查询");
            assert!(view.model.notice_detail.contains("不是查询"), "{}", view.model.notice_detail);
            view.perform("format", cx);
            assert_eq!(view.query_draft(cx), "select   1", "表格工作项不能格式化隐藏查询");
            view.perform("run-current", cx);
            assert_eq!(view.query_runs(cx), runs);
            // 结果区与右侧面板的命令同样不得落在隐藏查询上。
            for id in ["pin-result", "toggle-side", "query-view:messages", "unpin:1", "log:0", "side-mode:object"] {
                view.model.notice_detail.clear();
                view.perform(id, cx);
                assert!(view.model.notice_detail.contains("不是查询"), "{id}: {}", view.model.notice_detail);
            }
        });
        window.render_frame(cx);
    })
    .unwrap();

    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, _| {
            assert!(view.model.activate_item(&query));
            assert_eq!(view.model.tool, WorkTool::Query);
        });
        window.render_frame(cx);
        view.update(cx, |view, cx| {
            view.perform("format", cx);
            assert!(view.query_draft(cx).starts_with("SELECT"), "{}", view.query_draft(cx));
            view.perform("execute-query", cx);
            assert_eq!(view.query_runs(cx), runs + 1, "回到查询工作项后应能执行");
        });
    })
    .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[gpui_kit::test]
fn exchange_source_buttons_route_through_the_shell(cx: &mut gpui_kit::TestAppContext) {
    use crate::app::views::export::ExchangeSourceKind;
    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-exchange-buttons-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench(cx, services_at(&dir), false);
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.perform("new-query", cx));
        window.render_frame(cx);
    })
    .unwrap();
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            let query = view.model.active_tab.clone().expect("query");
            view.model.set_query_target(&query, Some("notes".to_owned()), Some("笔记".to_owned()), Some("main".to_owned()), Some("sqlite".to_owned()));
            view.plant_query_result(vec!["label".to_owned()], vec![vec!["真实".to_owned()]], cx);
        });
        window.render_frame(cx);
    })
    .unwrap();

    let exchange = cx.update(|cx| view.read(cx).exchange.clone());
    // 面板按钮的真实路径：request_source → Workbench::perform，而不是只改面板自己。
    cx.update(|cx| exchange.update(cx, |pane, cx| pane.request_source("exchange-source:query", cx)));
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(view.read(cx).exchange_kind(cx), ExchangeSourceKind::Query);
        assert_eq!(view.read(cx).exchange_columns(cx), vec!["label".to_owned()]);
    });

    cx.update(|cx| exchange.update(cx, |pane, cx| pane.request_source("exchange-source:table", cx)));
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(view.read(cx).exchange_kind(cx), ExchangeSourceKind::Table);
        assert!(view.read(cx).exchange_columns(cx).is_empty(), "查询项下显式选表数据不得借用上一张表的列");
    });
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn spec_matches_table_compares_connection_database_table_and_schema() {
    use crate::app::shell::view::spec_matches_table;
    use crate::grid::commands::TableRef;

    fn table(connection: &str, database: &str, schema: &str, name: &str) -> TableRef {
        TableRef {
            connection_id: connection.to_owned(),
            database: database.to_owned(),
            schema: schema.to_owned(),
            table: name.to_owned(),
            engine: "mysql".to_owned(),
            include_foreign_keys: false,
        }
    }

    // MySQL 规格的 schema 段为空：只比较连接、库和表名。
    let mysql = "c1|应用|shop||orders|mysql|dev";
    assert!(spec_matches_table(mysql, &table("c1", "shop", "shop", "orders")));
    assert!(!spec_matches_table(mysql, &table("c1", "shop", "shop", "items")), "表名不同不能匹配");
    assert!(!spec_matches_table(mysql, &table("c2", "shop", "shop", "orders")), "连接不同不能匹配");
    assert!(!spec_matches_table(mysql, &table("c1", "warehouse", "shop", "orders")), "库不同不能匹配");

    // 规格写出 schema 时，schema 不同就是不匹配。
    let postgres = "c1|应用|shop|public|orders|postgres|dev";
    assert!(spec_matches_table(postgres, &table("c1", "shop", "public", "orders")));
    assert!(!spec_matches_table(postgres, &table("c1", "shop", "sales", "orders")), "schema 不同不能匹配");
}

#[gpui_kit::test]
fn exchange_source_follows_the_active_item_and_query_source_blocks_full_export(cx: &mut gpui_kit::TestAppContext) {
    cx.executor().allow_parking();
    use crate::app::views::export::ExchangeSourceKind;
    use crate::export::Phase;

    let dir = std::env::temp_dir().join(format!("rillsql-exchange-source-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let (window, view) = open_workbench(cx, services, false);

    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.perform("new-query", cx);
            let query = view.model.active_tab.clone().expect("query item");
            view.model.set_query_target(&query, Some("notes".to_owned()), Some("笔记".to_owned()), Some("main".to_owned()), Some("sqlite".to_owned()));
        });
        window.render_frame(cx);
    })
    .unwrap();

    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            // 先让查询工作项进入面板，再把结果写进其结果标签，模拟一次真实执行后的状态。
            view.plant_query_result(vec!["label".to_owned()], vec![vec!["真实".to_owned()]], cx);
            view.perform("open-exchange", cx);
        });
        window.render_frame(cx);
    })
    .unwrap();

    let query_generation = cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.exchange_kind(cx), ExchangeSourceKind::Query);
        assert_eq!(view.exchange_columns(cx), vec!["label".to_owned()]);
        let text = view.exchange_text(cx);
        assert!(text.contains("来源 当前查询结果"), "{text}");
        assert!(text.contains("目标 查询结果"), "{text}");
        assert!(text.contains("已读取 1 行"), "{text}");
        assert!(text.contains("全量导出不可用"), "{text}");
        view.exchange_generation(cx)
    });

    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.perform("exchange-format:json", cx);
            view.perform("exchange-range:100", cx);
        });
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        let view = view.read(cx);
        let text = view.exchange_text(cx);
        assert!(text.contains("格式 json"), "{text}");
        assert!(text.contains("范围 前 100 行"), "{text}");
    });

    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.perform("exchange-export-full", cx));
        window.render_frame(cx);
    })
    .unwrap();
    assert!(!cx.did_prompt_for_new_path(), "查询结果不应打开全量导出对话框");
    cx.update(|cx| {
        let view = view.read(cx);
        let tasks = view.exchange_tasks(cx);
        let task = tasks.last().expect("blocked task");
        assert_eq!(task.phase, Phase::Blocked, "{tasks:?}");
        assert!(!task.success, "{tasks:?}");
        assert!(task.detail.contains("查询结果"), "{tasks:?}");
        assert!(tasks.iter().all(|task| task.phase != Phase::Running), "{tasks:?}");
    });

    // 切到表工作项：来源、列与映射版本一起重建。
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.perform("open-table:notes|笔记|main||customers|sqlite|dev", cx));
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.exchange_kind(cx), ExchangeSourceKind::Table);
        assert!(view.exchange_columns(cx).is_empty());
        let text = view.exchange_text(cx);
        assert!(text.contains("来源 当前表数据"), "{text}");
        assert!(text.contains("目标 customers"), "{text}");
        assert!(!text.contains("目标 查询结果"), "{text}");
        assert!(view.exchange_generation(cx) > query_generation, "切换来源必须推进版本");
    });
    cx.update(|cx| {
        let stale: std::collections::BTreeMap<String, String> = [("name".to_owned(), "old".to_owned())].into_iter().collect();
        let accepted = view.update(cx, |view, cx| view.apply_exchange_mapping(query_generation, stale, cx));
        assert!(!accepted, "旧来源映射不能写回新来源");
    });

    // 回到查询工作项，来源自动回到查询结果。
    let query = cx.update(|cx| view.read(cx).model.tabs.iter().find(|tab| tab.kind.is_query()).unwrap().id.clone());
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            assert!(view.model.activate_item(&query));
            cx.notify();
        });
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.exchange_kind(cx), ExchangeSourceKind::Query);
        let text = view.exchange_text(cx);
        assert!(text.contains("来源 当前查询结果"), "{text}");
        assert!(text.contains("已读取 1 行"), "{text}");
    });
    let _ = std::fs::remove_dir_all(&dir);
}

#[gpui_kit::test]
fn exchange_loaded_export_omits_uncommitted_cells(cx: &mut gpui_kit::TestAppContext) {
    // 已加载导出在真实 Tokio 工作线程写文件，测试需要允许跨线程唤醒。
    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-exchange-staged-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let (window, view) = open_workbench(cx, services, false);

    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.mark_table_dirty(cx);
            view.perform("open-exchange", cx);
        });
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        let text = view.read(cx).exchange_text(cx);
        assert!(text.contains("来源 当前表数据"), "{text}");
        assert!(text.contains("目标 customers"), "{text}");
        assert!(text.contains("已读取 1 行"), "{text}");
        assert!(view.read(cx).table_uncommitted(cx));
    });

    cx.update_window(window, |_, _, cx| {
        view.update(cx, |view, cx| view.perform("exchange-export-loaded", cx));
    })
    .unwrap();
    assert!(cx.did_prompt_for_new_path(), "已读取导出应打开保存对话框");
    let path = dir.join("loaded.csv");
    cx.simulate_new_path_selection(|_| Some(path.clone()));
    pump_exchange(cx, window, &view, |view, app| path.exists() && !view.exchange_running_now(app));
    let text = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).into_owned();
    assert!(text.contains('甲'), "导出应包含已提交值：{text}");
    assert!(!text.contains('乙'), "导出不能包含未提交暂存：{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

fn view_has_partial(dir: &std::path::Path) -> bool {
    std::fs::read_dir(dir).unwrap().flatten().any(|entry| entry.file_name().to_string_lossy().contains(".partial"))
}

struct SqliteLock {
    release: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for SqliteLock {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn lock_sqlite(path: &std::path::Path) -> SqliteLock {
    let path = path.to_path_buf();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let thread = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async move {
            let pool = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&path).busy_timeout(std::time::Duration::from_secs(2)))
                .await
                .unwrap();
            let mut conn = pool.acquire().await.unwrap();
            sqlx::query("BEGIN EXCLUSIVE").execute(&mut *conn).await.unwrap();
            let _ = ready_tx.send(());
            let _ = release_rx.recv();
            let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
            drop(conn);
            pool.close().await;
        });
    });
    ready_rx.recv_timeout(std::time::Duration::from_secs(10)).expect("exclusive lock");
    SqliteLock { release: Some(release_tx), thread: Some(thread) }
}

fn pump_for(
    cx: &mut gpui_kit::TestAppContext,
    window: gpui_kit::AnyWindowHandle,
    view: &gpui_kit::Entity<super::view::Workbench>,
    limit: std::time::Duration,
    mut ready: impl FnMut(&super::view::Workbench, &gpui_kit::App) -> bool,
) -> bool {
    let deadline = std::time::Instant::now() + limit;
    loop {
        let done = cx.update(|app| ready(view.read(app), app));
        if done {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
        })
        .unwrap();
        cx.run_until_parked();
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[gpui_kit::test]
fn exchange_blocks_empty_mapping_stops_the_earlier_import_and_fails_only_that_task(cx: &mut gpui_kit::TestAppContext) {
    use crate::export::Phase;

    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-exchange-guard-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("source.sqlite");
    let target = dir.join("target.sqlite");
    let csv = dir.join("people.csv");
    std::fs::write(&csv, "name\n乙\n丙\n丁\n戊\n").unwrap();
    let services = std::thread::scope(|scope| {
        let source = source.clone();
        let target = target.clone();
        let dir = dir.clone();
        scope
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
                runtime.block_on(async move {
                    for (path, seed) in [(&source, true), (&target, false)] {
                        let pool = sqlx::sqlite::SqlitePoolOptions::new()
                            .max_connections(1)
                            .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(path).create_if_missing(true))
                            .await
                            .unwrap();
                        sqlx::query("CREATE TABLE people (name TEXT)").execute(&pool).await.unwrap();
                        if seed {
                            sqlx::query("INSERT INTO people (name) VALUES ('甲')").execute(&pool).await.unwrap();
                        }
                        pool.close().await;
                    }
                    let services = Arc::new(AppServices::new(Arc::new(Service::new_for_test(dir.join("connections.sqlite")))));
                    services.connections.open_sqlite_for_test("notes", "笔记", &source).await.unwrap();
                    services
                })
            })
            .join()
            .unwrap()
    });
    let (window, view) = open_workbench(cx, services, false);
    let csv_opened = csv.canonicalize().unwrap();
    let target_opened = target.canonicalize().unwrap();
    cx.update_window(window, |_, _, cx| {
        view.update(cx, |view, cx| {
            view.plant_exchange_sheet("notes|笔记|main||people|sqlite|local", vec!["name".into()], vec![vec![Some("甲".into())]], cx);
            view.perform("open-exchange", cx);
        });
    })
    .unwrap();

    let source_lock = lock_sqlite(&source);
    cx.update_window(window, |_, _, cx| {
        view.update(cx, |view, cx| view.perform("exchange-pick", cx));
    })
    .unwrap();
    assert!(cx.did_prompt_for_paths());
    cx.simulate_path_prompt_response(|_| Some(vec![csv_opened]));
    pump_exchange(cx, window, &view, |view, app| view.exchange_text(app).contains("还没有列映射"));
    cx.update_window(window, |_, _, cx| {
        view.update(cx, |view, cx| view.perform("exchange-confirm", cx));
    })
    .unwrap();
    cx.update(|app| {
        let text = view.read(app).exchange_text(app);
        let import = view.read(app).exchange_tasks(app).into_iter().find(|task| task.kind == "import").expect(&text);
        assert_eq!(import.phase, Phase::Blocked, "{text}");
        assert!(!import.success, "{text}");
        assert!(import.detail.contains("还没有列映射"), "{text}");
        assert!(!text.contains("进行中"), "{text}");
    });
    drop(source_lock);
    assert_eq!(people_count(&source), 1, "空映射确认不得写库");
    pump_exchange(cx, window, &view, |view, app| view.exchange_text(app).contains("映射 name=name"));
    assert_eq!(people_count(&source), 1, "映射生成后仍不得自动写库");

    let source_lock = lock_sqlite(&source);
    cx.update_window(window, |_, _, cx| {
        view.update(cx, |view, cx| view.perform("exchange-confirm", cx));
    })
    .unwrap();
    pump_exchange(cx, window, &view, |view, app| view.exchange_running_now(app));
    cx.update_window(window, |_, _, cx| {
        view.update(cx, |view, cx| {
            view.perform("exchange-confirm", cx);
            assert!(view.exchange_text(cx).contains("不能再次确认"), "{}", view.exchange_text(cx));
            view.perform("exchange-task-cancel", cx);
        });
    })
    .unwrap();
    drop(source_lock);
    pump_exchange(cx, window, &view, |view, app| {
        view.exchange_tasks(app).iter().any(|task| task.kind == "import" && task.detail.contains("也不是仍在运行"))
    });
    let settled = people_count(&source);
    assert!(settled < 5, "第一次确认还在写时取消，不得继续写入后续行，实际 {settled}");
    cx.run_until_parked();
    assert_eq!(people_count(&source), settled, "取消之后行数不得再增加");
    cx.update(|app| {
        let text = view.read(app).exchange_text(app);
        let import = view.read(app).exchange_tasks(app).into_iter().rev().find(|task| task.kind == "import").expect(&text);
        assert_eq!(import.phase, Phase::Cancelled, "{text}");
        assert!(!import.success, "{text}");
        assert!(import.detail.contains("已取消"), "{text}");
        assert!(!view.read(app).exchange_running_now(app), "{text}");
    });

    let target_lock = lock_sqlite(&target);
    let source_lock = lock_sqlite(&source);
    cx.update_window(window, |_, _, cx| {
        view.update(cx, |view, cx| view.perform("exchange-migrate", cx));
    })
    .unwrap();
    assert!(cx.did_prompt_for_paths());
    cx.simulate_path_prompt_response(|_| Some(vec![target_opened]));
    assert!(
        pump_for(cx, window, &view, std::time::Duration::from_secs(3), |view, app| {
            view.exchange_tasks(app).iter().any(|task| task.kind == "migrate" && task.phase == Phase::Running)
        }),
        "迁移应先进入进行中"
    );
    std::thread::sleep(std::time::Duration::from_millis(2500));
    cx.update_window(window, |_, _, cx| {
        view.update(cx, |view, cx| view.perform("exchange-confirm", cx));
    })
    .unwrap();
    assert!(
        // 目标库的 busy_timeout 是 5 秒，可能连续命中不止一次；CI 的 macOS 机器负载高时 8 秒不够。
        pump_for(cx, window, &view, std::time::Duration::from_secs(30), |view, app| {
            view.exchange_tasks(app).iter().any(|task| task.kind == "migrate" && task.phase == Phase::Failed)
        }),
        "锁定的目标库应让这次迁移失败"
    );
    cx.update(|app| {
        let text = view.read(app).exchange_text(app);
        let tasks = view.read(app).exchange_tasks(app);
        let migrated = tasks.iter().rev().find(|task| task.kind == "migrate").expect(&text);
        let import = tasks.iter().rev().find(|task| task.kind == "import").expect(&text);
        assert_eq!(migrated.phase, Phase::Failed, "{text}");
        assert!(!migrated.success, "{text}");
        assert_eq!(import.phase, Phase::Running, "{text}");
        assert!(migrated.id.starts_with("migrate-"), "{text}");
    });
    cx.update_window(window, |_, _, cx| {
        view.update(cx, |view, cx| view.perform("exchange-task-cancel", cx));
    })
    .unwrap();
    drop(target_lock);
    drop(source_lock);
    pump_for(cx, window, &view, std::time::Duration::from_secs(8), |view, app| !view.exchange_running_now(app));
    let _ = std::fs::remove_dir_all(&dir);
}

#[gpui_kit::test]
fn editor_keeps_undo_and_select_all_shortcuts_inside_the_workbench(cx: &mut gpui_kit::TestAppContext) {
    let dir = std::env::temp_dir().join(format!("rillsql-shell-undo-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench(cx, services_at(&dir), false);
    cx.update(crate::app::bind_keys);
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.perform("new-query", cx));
        window.render_frame(cx);
        assert!(view.read(cx).query_editor_focused(window, cx));
    })
    .unwrap();
    cx.update_window(window, |_, window, cx| {
        window.press("secondary-a", cx);
        window.input("select 42", cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        assert_eq!(view.read(cx).query_editor_text(cx), "select 42", "Ctrl+A 后输入应替换全文");
        window.press("secondary-z", cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        let undone = view.read(cx).query_editor_text(cx);
        assert_ne!(undone, "select 42", "Ctrl+Z 没有撤销");
        window.press(if cfg!(target_os = "macos") { "cmd-shift-z" } else { "ctrl-y" }, cx);
        window.render_frame(cx);
        assert_eq!(view.read(cx).query_editor_text(cx), "select 42", "重做没有恢复");
    })
    .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[gpui_kit::test]
fn command_shift_f_formats_sql_in_the_editor(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::component::input;
    let dir = std::env::temp_dir().join(format!("rillsql-shell-format-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench(cx, services_at(&dir), false);
    // 在任何平台上复现 macOS：gpui-base 把 ⌘⇧F 绑成替换，之后才是应用自己的绑定。
    cx.update(|cx| cx.bind_keys([gpui_kit::KeyBinding::new("cmd-shift-f", input::Replace, Some("Input"))]));
    cx.update(crate::app::bind_keys);
    cx.update(|cx| cx.bind_keys(crate::app::editor_format_bindings()));
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.perform("new-query", cx));
        window.render_frame(cx);
        assert!(view.read(cx).query_editor_focused(window, cx));
        fn resolve(key: &'static str, window: &gpui_kit::Window, cx: &gpui_kit::App) -> Option<Box<dyn gpui_kit::Action>> {
            let keystroke = gpui_kit::Keystroke::parse(key).unwrap();
            let (bindings, _) = cx.key_bindings().borrow().bindings_for_input(&[keystroke], &window.context_stack());
            bindings.first().map(|binding| binding.action().boxed_clone())
        }
        let format = resolve("cmd-shift-f", window, cx).expect("⌘⇧F 没有绑定");
        assert!(format.partial_eq(&crate::app::FormatSql), "⌘⇧F 解析成了 {}", format.name());
        let replace = resolve("cmd-alt-f", window, cx).expect("⌥⌘F 没有绑定");
        assert!(replace.partial_eq(&input::Replace), "⌥⌘F 解析成了 {}", replace.name());
    })
    .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// 构造一条已读取对象，字段顺序与 TreeObject 一致。
fn tree_object(connection_id: &str, connection_name: &str, engine: &str, database: &str, schema: &str, name: &str, kind: &str) -> crate::query::assist::TreeObject {
    crate::query::assist::TreeObject {
        connection_id: connection_id.to_owned(),
        connection_name: connection_name.to_owned(),
        engine: engine.to_owned(),
        database: database.to_owned(),
        schema: schema.to_owned(),
        name: name.to_owned(),
        kind: kind.to_owned(),
    }
}

#[test]
fn quick_open_lists_loaded_tables_and_views_and_opens_the_matching_target() {
    let mut model = WorkbenchModel::new(Shell::default());
    let orders = tree_object("pg", "销售库", "postgres", "shop", "public", "orders", "table");
    let report = tree_object("pg", "销售库", "postgres", "shop", "public", "order_view", "view");
    // Redis 的键不是关系型表，不能出现在快速打开里。
    let session = tree_object("redis", "缓存", "redis", "0", "", "session", "table");
    assert!(model.set_catalog(&[orders.clone(), report, session], ""));
    model.shell.palette_tables_only = true;

    let entries = model.palette_entries();
    let labels: Vec<&str> = entries.iter().map(|entry| entry.label.as_str()).collect();
    assert_eq!(labels, vec!["orders", "order_view"], "快速打开只列真实表 / 视图");
    assert!(entries.iter().all(|entry| matches!(entry.group, "table" | "view")), "命令和连接不应混入快速打开");
    assert_eq!(entries[0].group, "table", "表标注为表");
    assert_eq!(entries[1].group, "view", "视图有自己的类型，不再共用“表”");
    // 断开连接后旧索引不再冒充可打开对象；已删除连接即使状态还停在 Connected 也不能列出。
    model
        .connections
        .status
        .insert("pg".into(), crate::app::views::connections::LinkStatus::Disconnected);
    assert!(model.palette_entries().is_empty(), "断开连接后不列旧对象");
    model
        .connections
        .status
        .insert("pg".into(), crate::app::views::connections::LinkStatus::Connected);
    assert!(model.palette_entries().is_empty(), "连接记录已删除时不得列旧对象");
    model.connections.status.remove("pg");
    assert_eq!(model.palette_entries().len(), 2, "状态未知时保留已读取索引");

    let target = crate::app::views::query::object_spec(&orders, "");
    let wanted = format!("open-table:{target}");
    assert_eq!(entries[0].id, wanted);
    assert_eq!(entries[0].detail, "shop.public · 销售库");

    let effect = model.dispatch(&entries[0].id);
    assert!(matches!(effect, Some(crate::app::Effect::None)));
    assert_eq!(model.tabs.len(), 1);
    assert_eq!(model.tabs[0].kind, WorkItemKind::Table);
    assert_eq!(model.tabs[0].spec.as_deref(), Some(target.as_str()), "选中后打开的就是这个目标");
    assert_eq!(model.executions, 0, "快速打开不执行 SQL");
    assert!(model.pending.is_empty(), "快速打开不应排队其他任务");

    // 普通命令面板仍组合命令、连接和对象。
    model.shell.palette_tables_only = false;
    let all = model.palette_entries();
    assert!(all.iter().any(|entry| entry.group == "command"));
    assert!(all.iter().any(|entry| entry.group == "table" && entry.label == "orders"));
    assert_eq!(all.iter().filter(|entry| entry.group == "table").count(), 1, "表只算表");
    assert_eq!(all.iter().filter(|entry| entry.group == "view").count(), 1, "视图单独算一类");
}

#[test]
fn catalog_index_tolerates_future_object_kinds_without_quick_open_leak() {
    let mut model = WorkbenchModel::new(Shell::default());
    let orders = tree_object("pg", "销售库", "postgres", "shop", "public", "orders", "table");
    let trigger = tree_object("pg", "销售库", "postgres", "shop", "public", "orders_touch", "trigger");
    assert!(model.set_catalog(&[orders, trigger], ""));

    let objects = model.catalog_for("pg", "");
    let explorer: Vec<&str> = objects.iter().map(|object| object.name.as_str()).collect();
    assert_eq!(explorer, vec!["orders", "orders_touch"], "对象树保留触发器等其他关系型对象");
    let grouped: Vec<String> = model
        .object_groups("pg")
        .iter()
        .flat_map(|group| group.objects.iter().map(|object| object.name.clone()))
        .collect();
    assert_eq!(grouped, vec!["orders", "orders_touch"], "分组后仍保留触发器");

    model.shell.palette_tables_only = true;
    let entries = model.palette_entries();
    let palette: Vec<&str> = entries.iter().map(|entry| entry.label.as_str()).collect();
    assert_eq!(palette, vec!["orders"], "快速打开只列可打开的表 / 视图");
}

#[test]
fn object_filter_matches_names_and_keeps_empty_no_match_and_error_apart() {
    let mut model = WorkbenchModel::new(Shell::default());
    let orders = tree_object("pg", "销售库", "postgres", "shop", "public", "orders", "table");
    let people = tree_object("pg", "销售库", "postgres", "shop", "public", "people", "table");
    let access = tree_object("pg", "销售库", "postgres", "shop", "analytics", "access_log", "view");
    assert!(model.set_catalog(&[orders, people, access], ""));

    model.object_filter = "ORD".into();
    let groups = model.object_groups("pg");
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].objects.iter().map(|object| object.name.as_str()).collect::<Vec<_>>(), vec!["orders"]);
    assert_eq!(model.object_notice("pg"), None, "有命中时没有空态");

    model.object_filter = "missing".into();
    assert!(model.object_groups("pg").is_empty());
    assert_eq!(model.object_notice("pg"), Some("ui.explorer.noMatch"));

    model.object_filter.clear();
    assert_eq!(model.object_groups("pg").len(), 2, "清除对象筛选后目录恢复");

    let empty = WorkbenchModel::new(Shell::default());
    assert_eq!(empty.object_notice("pg"), Some("ui.explorer.noObjects"));

    model.object_filter = "missing".into();
    assert!(model.set_catalog(&[tree_object("pg", "销售库", "postgres", "shop", "public", "orders", "table")], "连接超时"));
    assert_eq!(model.object_notice("pg"), Some("ui.explorer.error"), "读取失败优先于筛选无命中");
    let texts = [
        model.text("ui.explorer.noObjects"),
        model.text("ui.explorer.noMatch"),
        model.text("ui.explorer.error"),
    ];
    assert_ne!(texts[0], texts[1]);
    assert_ne!(texts[1], texts[2]);
    assert_ne!(texts[0], texts[2]);
}

#[test]
fn connection_filter_and_object_filter_stay_independent() {
    let mut model = WorkbenchModel::new(Shell::default());
    let orders = tree_object("pg", "销售库", "postgres", "shop", "public", "orders", "table");
    let people = tree_object("pg", "销售库", "postgres", "shop", "public", "people", "table");
    assert!(model.set_catalog(&[orders, people], ""));

    model.object_filter = "orders".into();
    model.connections.filter_text = "no-such-connection".into();
    assert_eq!(model.object_groups("pg").len(), 1, "连接筛选不改变对象区结果");
    assert_eq!(model.catalog_for("pg", &model.object_filter).len(), 1);

    model.set_field(DraftField::Filter, "pg".into());
    assert_eq!(model.object_filter, "orders", "连接筛选不覆盖对象筛选");
    model.set_field(DraftField::ObjectFilter, "people".into());
    assert_eq!(model.connections.filter_text, "pg", "对象筛选不覆盖连接筛选");
    assert_eq!(model.field_value(DraftField::ObjectFilter), "people");
    assert_eq!(model.catalog_for("pg", &model.object_filter)[0].name, "people");
}

#[test]
fn hierarchy_follows_real_database_and_schema_and_skips_document_engines() {
    let a = tree_object("pg", "销售库", "postgres", "shop", "public", "orders", "table");
    let b = tree_object("pg", "销售库", "postgres", "shop", "analytics", "events", "view");
    let local = tree_object("lite", "本地", "sqlite", "main", "", "notes", "table");
    let redis = tree_object("redis", "缓存", "redis", "0", "", "session", "table");
    let mongo = tree_object("mongo", "文档", "mongodb", "app", "", "users", "table");
    let groups = super::model::group_objects(&[&a, &b, &local, &redis, &mongo]);

    assert_eq!(groups.len(), 3);
    assert_eq!((groups[0].database.as_str(), groups[0].schema.as_str()), ("shop", "public"));
    assert_eq!((groups[1].database.as_str(), groups[1].schema.as_str()), ("shop", "analytics"));
    assert_eq!((groups[2].database.as_str(), groups[2].schema.as_str()), ("main", ""), "SQLite 没有模式");
    assert!(groups.iter().all(|group| group.objects.iter().all(|object| object.name != "session" && object.name != "users")));
    assert!(!super::model::is_table_object(&redis));
    assert!(!super::model::relational_engine("MongoDB"));
}

#[test]
fn refreshed_catalog_feeds_palette_and_explorer_from_one_snapshot() {
    let mut model = WorkbenchModel::new(Shell::default());
    let orders = tree_object("pg", "销售库", "postgres", "shop", "public", "orders", "table");
    let people = tree_object("pg", "销售库", "postgres", "shop", "public", "people", "table");
    assert!(model.set_catalog(&[orders.clone(), people.clone()], ""));
    assert!(!model.set_catalog(&[orders, people], ""), "内容不变时不再重新分配");

    let explorer: Vec<String> = model.catalog_for("pg", "").iter().map(|object| object.name.clone()).collect();
    let palette: Vec<String> = model.catalog_entries().iter().map(|entry| entry.label.clone()).collect();
    assert_eq!(explorer, palette, "对象树与快速打开读同一份索引");

    assert!(model.set_catalog(&[], ""), "目录刷新为空时同一索引一起变空");
    assert!(model.catalog_entries().is_empty());
    assert!(model.object_groups("pg").is_empty());
}

#[test]
fn tree_retry_starts_only_the_refresh_task() {
    let mut model = WorkbenchModel::new(Shell::default());
    assert!(model.set_catalog(&[], "连接超时"));
    assert_eq!(model.palette_empty_key(), "ui.palette.error");
    assert_eq!(model.object_notice("pg"), Some("ui.explorer.error"));

    let tabs = model.tabs.len();
    assert!(model.dispatch("refresh-tree").is_none(), "刷新没有额外外壳副作用");
    assert_eq!(model.tabs.len(), tabs);
    assert_eq!(model.executions, 0);
    assert!(model.pending.is_empty(), "重试只发起对象读取");
    assert!(model.commands.is_empty(), "刷新不应顺带记录其他命令");

    let mut empty = WorkbenchModel::new(Shell::default());
    empty.shell.palette_tables_only = true;
    assert_eq!(empty.palette_empty_key(), "ui.palette.noObjects");
    assert!(empty.set_catalog(&[tree_object("pg", "销售库", "postgres", "shop", "public", "orders", "table")], ""));
    empty.palette_query = "missing".into();
    assert_eq!(empty.palette_entries().len(), 0);
    assert_eq!(empty.palette_empty_key(), "ui.explorer.noMatch");
}

fn wait_for_tree(
    cx: &mut gpui_kit::TestAppContext,
    window: gpui_kit::AnyWindowHandle,
    view: &gpui_kit::Entity<super::view::Workbench>,
) -> bool {
    let started = std::time::Instant::now();
    while started.elapsed() < std::time::Duration::from_secs(30) {
        cx.run_until_parked();
        let _ = cx.update_window(window, |_, window, cx| window.render_frame(cx));
        if cx.update(|cx| view.read(cx).model.catalog.iter().any(|object| object.name == "item")) {
            return true;
        }
        std::thread::park_timeout(std::time::Duration::from_millis(25));
    }
    false
}

#[gpui_kit::test]
fn quick_open_lists_the_loaded_tree_and_opens_without_running_sql(cx: &mut gpui_kit::TestAppContext) {
    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-shell-tree-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let database = dir.join("items.sqlite");
    let config_dir = dir.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("runtime");
        runtime.block_on(async move {
            let pool = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).create_if_missing(true))
                .await
                .unwrap();
            sqlx::query("CREATE TABLE item (id INTEGER PRIMARY KEY, label TEXT)").execute(&pool).await.unwrap();
            sqlx::query("CREATE VIEW item_view AS SELECT id, label FROM item").execute(&pool).await.unwrap();
            pool.close().await;
            let services = Arc::new(AppServices::new(Arc::new(Service::new_for_test(config_dir.join("connections.sqlite")))));
            services.connections.open_sqlite_for_test("notes", "笔记", &database).await.unwrap();
            let _ = tx.send(services);
        });
    });
    let services = rx.recv_timeout(std::time::Duration::from_secs(120)).expect("sqlite setup");
    let (window, view) = open_workbench(cx, services, false);
    cx.update_window(window, |_, _, cx| {
        view.update(cx, |view, cx| view.perform("refresh-tree", cx));
    })
    .unwrap();
    assert!(wait_for_tree(cx, window, &view), "对象索引没有同步");

    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.perform("quick-open", cx);
            let entries = view.model.palette_entries();
            let item = entries.iter().find(|entry| entry.label == "item").expect("快速打开没有 item");
            assert_eq!(item.group, "table");
            let report = entries.iter().find(|entry| entry.label == "item_view").expect("快速打开没有视图");
            assert_eq!(report.group, "view", "视图按自己的类型标注");
            assert!(entries.iter().all(|entry| matches!(entry.group, "table" | "view")), "快速打开只含表 / 视图");
            assert!(view.model.text("shell.paletteGroup.views") != view.model.text("shell.paletteGroup.tables"), "表与视图标注文案不同");
            let id = item.id.clone();
            let spec = id.strip_prefix("open-table:").expect("open-table 目标").to_owned();
            view.perform(&id, cx);
            let tab = view.model.active_tab().expect("打开的表工作项");
            assert_eq!(tab.kind, WorkItemKind::Table);
            assert_eq!(tab.spec.as_deref(), Some(spec.as_str()));
            assert!(!view.query_executed(cx), "快速打开不应执行 SQL");
        });
        window.render_frame(cx);
    })
    .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

fn history_run(id: &str, sql: &str, connection: &str, status: &str) -> HistoryEntry {
    HistoryEntry {
        id: id.into(),
        sql: sql.into(),
        connection_id: connection.into(),
        connection_name: format!("conn-{connection}"),
        database_name: "app".into(),
        engine: "postgres".into(),
        started_at: "2026-11-07T08:00:00Z".into(),
        finished_at: None,
        status: status.into(),
        elapsed_ms: Some(12),
        summary: "1 行".into(),
    }
}

/// R13：历史列表 Shift / ⌘ 多选，⌘C 多行得到含表头的 TSV，右键带复制值 / 行 / 列 / TSV / CSV / JSON。
/// 只改选中和写剪贴板，不载入、不执行。
#[test]
fn history_rows_multi_select_and_copy_like_read_only_tables() {
    let mut model = WorkbenchModel::new(Shell::default());
    model.shell.page = AppPage::History;
    model.history.runs = vec![
        history_run("a", "select 1", "live", "ok"),
        history_run("b", "select\t2", "live", "error"),
        history_run("c", "select 3", "other", "ok"),
        history_run("d", "select 4", "other", "ok"),
    ];
    let loads = model.loads;
    let _ = model.dispatch("history-pick:a");
    assert_eq!(model.history.marked_ids(), ["a"]);
    let _ = model.dispatch("history-copy");
    assert_eq!(model.copied, "select 1", "单行 ⌘C 仍是 SQL 本身");
    let _ = model.dispatch("history-pick-extend:c");
    assert_eq!(model.history.marked_ids(), ["a", "b", "c"]);
    assert_eq!(model.history.selected_run.as_deref(), Some("c"), "当前行跟到扩展的一端");
    let _ = model.dispatch("history-pick-toggle:b");
    assert_eq!(model.history.marked_ids(), ["a", "c"]);
    let _ = model.dispatch("history-copy");
    let lines: Vec<&str> = model.copied.lines().collect();
    assert_eq!(lines.len(), 3, "{:?}", model.copied);
    assert!(lines[1].starts_with("select 1\tconn-live\tapp\tok\t") && lines[2].starts_with("select 3\t"), "{lines:?}");

    let _ = model.dispatch("menu-history");
    let ids: Vec<&str> = model.menu.as_ref().unwrap().items.iter().map(|item| item.id).collect();
    for id in ["history-load", "read-table-copy-value", "read-table-copy-row", "read-table-copy-column", "read-table-copy-tsv", "read-table-copy-csv", "read-table-copy-json"] {
        assert!(ids.contains(&id), "{id} in {ids:?}");
    }
    model.menu = None;
    let _ = model.dispatch("history-copy-as:column");
    assert_eq!(model.copied, "select 1\nselect 3", "列 = 选中行的 SQL");
    let _ = model.dispatch("history-copy-as:value");
    assert_eq!(model.copied, "select 3", "值 = 当前行的 SQL");
    let _ = model.dispatch("history-copy-as:csv");
    assert!(model.copied.starts_with("\"SQL\",") && model.copied.contains("\"select 3\""), "{}", model.copied);
    // Shift+↓ 从当前行扩展；普通上下结束多选。
    let _ = model.dispatch("history-pick:b");
    let _ = model.dispatch("history-extend-next");
    assert_eq!(model.history.marked_ids(), ["b", "c"]);
    assert_eq!(model.history_key(super::menus::NavKey::ArrowDown), None);
    assert_eq!(model.history.marked_ids(), ["d"]);
    assert_eq!(model.loads, loads, "选择与复制都不载入");
    assert_eq!(model.executions, 0);
    assert_eq!(model.pending_len(), 0);
}

#[test]
fn history_search_filter_preview_copy_delete_and_clear_are_reversible() {
    let mut model = WorkbenchModel::new(Shell::default());
    model.shell.page = AppPage::History;
    model.history.runs = vec![
        history_run("a", "select 1", "live", "ok"),
        history_run("b", "select 2", "live", "error"),
        history_run("c", "delete from t", "other", "ok"),
    ];
    assert_eq!(model.history.runs_phase(), ListPhase::Rows);

    model.set_field(DraftField::HistorySearch, "select".into());
    assert_eq!(model.history.filtered_runs().len(), 2);
    model.set_field(DraftField::HistorySearch, "missing".into());
    assert_eq!(model.history.runs_phase(), ListPhase::NoMatch);
    let _ = model.dispatch("history-filter-reset");
    assert!(model.history.filter_text.is_empty());
    assert_eq!(model.history.runs_phase(), ListPhase::Rows);

    let _ = model.dispatch("history-status-failed");
    assert_eq!(model.history.filter_status, "failed");
    assert_eq!(model.history.filtered_runs().len(), 1);
    let _ = model.dispatch("history-status-ok");
    assert_eq!(model.history.filtered_runs().len(), 2);
    let _ = model.dispatch("history-status-all");
    assert!(model.history.filter_status.is_empty());

    model.history.selected_run = Some("c".into());
    let _ = model.dispatch("history-filter-connection");
    assert_eq!(model.history.filter_connection, "other");
    assert_eq!(model.history.filtered_runs().len(), 1);
    let _ = model.dispatch("history-filter-reset");
    assert_eq!(model.history.filtered_runs().len(), 3);

    model.history.selected_run = Some("a".into());
    model.history.preview_open = false;
    let _ = model.dispatch("history-detail");
    assert!(model.history.preview_open);
    let plan = model.history.preview_plan(&model.known_ids()).expect("preview");
    assert_eq!(plan.sql, "select 1");
    assert!(!plan.execute);
    let _ = model.dispatch("history-preview-close");
    assert!(!model.history.preview_open);

    let _ = model.dispatch("history-copy");
    assert_eq!(model.copied, "select 1");

    let _ = model.dispatch("history-clear");
    assert!(model.history.confirm_clear);
    assert_eq!(model.notice_key, Some("history.page.clearConfirm"));
    assert_eq!(model.pending_len(), 0);
    let _ = model.dispatch("history-clear-cancel");
    assert!(!model.history.confirm_clear);
    let _ = model.dispatch("history-clear");
    let _ = model.dispatch("history-clear");
    assert!(!model.history.confirm_clear);
    assert_eq!(model.take_pending(), Some(PendingJob::ClearHistory));

    model.history.runs.clear();
    let _ = model.dispatch("history-clear");
    assert_eq!(model.notice_key, Some("ui.history.empty"));
    assert_eq!(model.pending_len(), 0);

    model.history.runs = vec![history_run("gone", "select 42", "deleted", "ok")];
    model.history.selected_run = Some("gone".into());
    let loads = model.loads;
    let _ = model.dispatch("history-load");
    assert_eq!(model.loads, loads + 1);
    let tab = model.active_tab().expect("loaded query");
    assert_eq!(tab.draft, "select 42");
    assert_eq!(tab.connection_id, None);
    assert_eq!(model.notice_key, Some("history.plan.deletedNotice"));
    assert_eq!(model.history.selected_run.as_deref(), Some("gone"));

    model.history.selected_run = None;
    let _ = model.dispatch("history-delete");
    assert_eq!(model.notice_key, Some("ui.history.delete"));
    assert_eq!(model.pending_len(), 0);
}

#[tokio::test]
async fn saved_query_rename_delete_cancel_and_deleted_connection_keep_sql() {
    let dir = std::env::temp_dir().join(format!("rillsql-history-saved-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let mut model = WorkbenchModel::new(Shell::default());
    let _ = model.dispatch("new-query");
    query_item(&mut model).draft = "select 1".into();
    model.history.save_name = "   ".into();
    let _ = model.dispatch("history-save-named");
    assert_eq!(model.notice_key, Some("history.page.needName"));
    assert_eq!(model.pending_len(), 0);
    model.history.save_name = "计数".into();
    let _ = model.dispatch("history-save-named");
    let token = model.begin_owner(&services.tasks, "history");
    let done = jobs::execute_job(&services, request_of(&mut model, token)).await;
    apply_done(&mut model, &services, "history", done);
    assert_eq!(model.history.saved.len(), 1);
    assert!(model.history.failure_key.is_none());
    let first = model.history.saved[0].id.clone();

    model.history.section = HistorySection::Saved;
    model.history.selected_saved = Some(first.clone());
    let _ = model.dispatch("history-rename");
    assert!(model.history.rename_open);
    assert_eq!(model.history.rename_text, "计数");
    model.set_field(DraftField::RenameName, "   ".into());
    let _ = model.dispatch("history-rename-confirm");
    assert_eq!(model.notice_key, Some("history.page.needName"));
    assert_eq!(model.pending_len(), 0);
    assert_eq!(model.history.saved[0].name, "计数");

    model.set_field(DraftField::RenameName, "新名字".into());
    let _ = model.dispatch("history-rename-confirm");
    let token = model.begin_owner(&services.tasks, "history");
    let done = jobs::execute_job(&services, request_of(&mut model, token)).await;
    apply_done(&mut model, &services, "history", done);
    assert_eq!(model.history.saved[0].name, "新名字");
    assert!(!model.history.rename_open);
    assert!(model.history.failure_key.is_none());

    query_item(&mut model).draft = "select 2".into();
    model.history.save_name = "第二个".into();
    let _ = model.dispatch("history-save-named");
    let token = model.begin_owner(&services.tasks, "history");
    let done = jobs::execute_job(&services, request_of(&mut model, token)).await;
    apply_done(&mut model, &services, "history", done);
    assert_eq!(model.history.saved.len(), 2);
    let first = model.history.saved.iter().find(|item| item.name == "新名字").expect("first").id.clone();
    model.history.selected_saved = Some(first);
    let _ = model.dispatch("history-rename");
    model.set_field(DraftField::RenameName, "第二个".into());
    let _ = model.dispatch("history-rename-confirm");
    let token = model.begin_owner(&services.tasks, "history");
    let done = jobs::execute_job(&services, request_of(&mut model, token)).await;
    apply_done(&mut model, &services, "history", done);
    assert_eq!(model.notice_key, Some("history.page.failed"));
    assert_eq!(model.history.failure_key, Some("history.page.failed"));
    assert!(!model.history.failure_detail.is_empty());
    assert!(model.history.saved.iter().any(|item| item.name == "新名字"));
    assert_eq!(model.history.saved.len(), 2);

    let remove = model.history.saved.iter().find(|item| item.name == "第二个").expect("second").id.clone();
    model.history.selected_saved = Some(remove.clone());
    let _ = model.dispatch("saved-delete");
    assert!(model.history.confirm_delete);
    assert_eq!(model.pending_len(), 0);
    assert_eq!(model.history.saved.len(), 2);
    // 确认层打开时钉住目标：中途换选中不改变真正要删的那一条，作业也只认钉住的 id。
    let other = model.history.saved.iter().find(|item| item.id != remove).expect("other").id.clone();
    model.history.selected_saved = Some(other);
    assert_eq!(model.history.delete_target().as_deref(), Some(remove.as_str()), "确认目标是打开确认时那一条");
    let token = model.begin_owner(&services.tasks, "history");
    let pinned_request = job_request(&model, PendingJob::DeleteSaved, token);
    assert_eq!(pinned_request.saved_id.as_deref(), Some(remove.as_str()), "删除作业不跟着选中漂移");
    model.history.selected_saved = Some(remove.clone());
    let _ = model.dispatch("saved-delete-cancel");
    assert!(!model.history.confirm_delete);
    assert_eq!(model.history.saved.len(), 2);
    let _ = model.dispatch("saved-delete");
    let _ = model.dispatch("saved-delete");
    assert!(!model.history.confirm_delete);
    let token = model.begin_owner(&services.tasks, "history");
    let done = jobs::execute_job(&services, request_of(&mut model, token)).await;
    apply_done(&mut model, &services, "history", done);
    assert_eq!(model.history.saved.len(), 1);
    assert!(model.history.failure_key.is_none());

    model.history.selected_saved = Some(model.history.saved[0].id.clone());
    let sql = model.history.saved[0].sql.clone();
    let _ = model.dispatch("history-load");
    let tab = model.active_tab().expect("loaded saved query");
    assert_eq!(tab.draft, sql);
    assert_eq!(tab.draft, "select 1");
    assert_eq!(tab.connection_id, None);
    assert_eq!(model.notice_key, Some("history.plan.deletedNotice"));
    assert_eq!(model.history.saved[0].sql, "select 1");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn empty_no_match_and_failure_states_stay_apart_and_keep_entries() {
    let dir = std::env::temp_dir().join(format!("rillsql-history-states-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let mut model = WorkbenchModel::new(Shell::default());
    assert_eq!(model.history.runs_phase(), ListPhase::Empty);
    assert_eq!(model.history.saved_phase(), ListPhase::Empty);

    model.history.runs = vec![history_run("a", "select 1", "live", "ok")];
    model.history.filter_text = "nothing".into();
    assert_eq!(model.history.runs_phase(), ListPhase::NoMatch);
    model.history.filter_text.clear();

    let token = model.begin_owner(&services.tasks, "history");
    apply_done(&mut model, &services, "history", jobs::JobDone::Notice(token, Err("磁盘只读".into())));
    assert_eq!(model.history.runs_phase(), ListPhase::Failed);
    assert_eq!(model.history.runs.len(), 1);
    assert_eq!(model.notice_key, Some("history.page.failed"));
    assert_eq!(model.notice_detail, "磁盘只读");

    let kept = model.history.runs.clone();
    let token = model.begin_owner(&services.tasks, "history");
    apply_done(&mut model, &services, "history", jobs::JobDone::History(token, Ok((kept, Vec::new()))));
    assert!(model.history.failure_key.is_none());
    assert_eq!(model.history.runs_phase(), ListPhase::Rows);
    assert_eq!(model.history.saved_phase(), ListPhase::Empty);

    model.history.saved = vec![crate::query::SavedQuery {
        id: "s1".into(),
        name: "计数".into(),
        sql: "select 1".into(),
        connection_id: "live".into(),
        connection_name: "conn-live".into(),
        database_name: "app".into(),
        engine: "postgres".into(),
        updated_at: "2026-11-07T00:00:00Z".into(),
    }];
    model.history.filter_text = "nothing".into();
    assert_eq!(model.history.saved_phase(), ListPhase::NoMatch);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn every_public_setting_moves_both_ways_applies_immediately_and_persists() {
    let dir = std::env::temp_dir().join(format!("rillsql-settings-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let mut model = WorkbenchModel::new(Shell::default());
    let illegal = ShellSettings {
        theme: "neon".into(),
        density: "comfy".into(),
        editor_font: 999,
        editor_theme: "neon".into(),
        locale: "fr".into(),
        page_size: 0,
        preview_kb: 999,
        object_page: -1,
        null_copy: "bogus".into(),
        query_timeout_sec: -5,
        auto_check_updates: true,
        empty_display: "hint".into(),
        index_naming: crate::ddl::naming::IndexNaming::default(),
    };
    model.apply_settings(&illegal);
    assert_eq!(model.settings.theme, "system");
    assert_eq!(model.settings.density, "compact");
    assert_eq!(model.settings.editor_font, 24);
    assert_eq!(model.settings.editor_theme, "auto");
    assert_eq!(model.settings.locale, "zh-CN");
    assert_eq!(model.settings.page_size, 1);
    assert_eq!(model.settings.preview_kb, 64);
    assert_eq!(model.settings.object_page, 1);
    assert_eq!(model.settings.null_copy, "NULL");
    assert_eq!(model.settings.query_timeout_sec, 0);
    assert_eq!(model.shell.theme, ThemePreference::System);
    assert_eq!(model.shell.density, crate::app::Density::Compact);

    let _ = model.dispatch("theme-light");
    assert_eq!(model.shell.theme, ThemePreference::Light);
    assert_eq!(model.settings.theme, "light");
    let _ = model.dispatch("theme-dark");
    assert_eq!(model.shell.theme, ThemePreference::Dark);
    let _ = model.dispatch("theme-system");
    assert_eq!(model.shell.theme, ThemePreference::System);

    let _ = model.dispatch("density-standard");
    assert_eq!(model.shell.density, crate::app::Density::Standard);
    let _ = model.dispatch("density-relaxed");
    assert_eq!(model.shell.density, crate::app::Density::Relaxed);
    let _ = model.dispatch("locale-en");
    assert_eq!(model.shell.locale, Locale::En);
    let _ = model.dispatch("locale-zh");
    assert_eq!(model.shell.locale, Locale::ZhCn);

    model.settings.set_editor_font(10);
    let _ = model.dispatch("font-down");
    assert_eq!(model.settings.editor_font, 10);
    let _ = model.dispatch("font-up");
    assert_eq!(model.settings.editor_font, 11);

    model.settings.editor_theme = "auto".into();
    let _ = model.dispatch("editor-theme-prev");
    assert_eq!(model.settings.editor_theme, "mono");
    let _ = model.dispatch("editor-theme-next");
    assert_eq!(model.settings.editor_theme, "auto");
    // 设置页直接摆出全部配色：点哪个就是哪个，未知值回落到跟随主题。
    let _ = model.dispatch("editor-theme-set-warm");
    assert_eq!(model.settings.editor_theme, "warm");
    assert!(crate::app::views::settings::is_settings_command("editor-theme-set-warm"), "选配色要写盘");
    let _ = model.dispatch("editor-theme-set-neon");
    assert_eq!(model.settings.editor_theme, "auto");

    model.settings.set_page_size(1);
    let _ = model.dispatch("page-size-down");
    assert_eq!(model.settings.page_size, 1);
    let _ = model.dispatch("page-size-up");
    assert_eq!(model.settings.page_size, 51);
    let _ = model.dispatch("page-size-down");
    assert_eq!(model.settings.page_size, 1);

    model.settings.set_preview_kb(1);
    let _ = model.dispatch("preview-down");
    assert_eq!(model.settings.preview_kb, 1);
    let _ = model.dispatch("preview-up");
    assert_eq!(model.settings.preview_kb, 2);

    model.settings.set_object_page(1);
    let _ = model.dispatch("object-page-down");
    assert_eq!(model.settings.object_page, 1);
    let _ = model.dispatch("object-page-up");
    assert_eq!(model.settings.object_page, 51);

    model.settings.null_copy = "NULL".into();
    let _ = model.dispatch("null-prev");
    assert_eq!(model.settings.null_copy, "N");
    let _ = model.dispatch("null-next");
    assert_eq!(model.settings.null_copy, "NULL");
    model.settings.query_timeout_sec = 0;
    let _ = model.dispatch("timeout-prev");
    assert_eq!(model.settings.query_timeout_sec, 60);
    let _ = model.dispatch("timeout-cycle");
    assert_eq!(model.settings.query_timeout_sec, 0);
    let _ = model.dispatch("timeout-15");
    assert_eq!(model.settings.query_timeout_sec, 15);

    assert!(model.settings.auto_check_updates);
    let _ = model.dispatch("toggle-updates");
    assert!(!model.settings.auto_check_updates);
    let _ = model.dispatch("toggle-updates");
    assert!(model.settings.auto_check_updates);

    let state = model.snapshot();
    assert_eq!(state.shell, model.settings.to_settings());
    super::model::persist_state(&services, &state).await.unwrap();
    let loaded = services.load_state().await.unwrap();
    let mut restored = WorkbenchModel::new(Shell::default());
    restored.apply_settings(&loaded.shell);
    restored.restore_session(&loaded.session);
    assert_eq!(restored.settings, model.settings);
    assert_eq!(restored.shell.theme, ThemePreference::System);
    assert_eq!(restored.shell.density, crate::app::Density::Relaxed);
    assert_eq!(restored.shell.locale, Locale::ZhCn);

    let stored = AppState::new(illegal, SessionState::default());
    std::fs::write(services.state.path(), crate::state::to_bytes(&stored).unwrap()).unwrap();
    let loaded = services.load_state().await.unwrap();
    assert_eq!(loaded.shell.theme, "neon");
    let mut normalized = WorkbenchModel::new(Shell::default());
    normalized.apply_settings(&loaded.shell);
    assert_eq!(normalized.settings.theme, "system");
    assert_eq!(normalized.settings.density, "compact");
    assert_eq!(normalized.settings.editor_font, 24);
    assert_eq!(normalized.settings.editor_theme, "auto");
    assert_eq!(normalized.settings.locale, "zh-CN");
    assert_eq!(normalized.settings.page_size, 1);
    assert_eq!(normalized.settings.preview_kb, 64);
    assert_eq!(normalized.settings.object_page, 1);
    assert_eq!(normalized.settings.null_copy, "NULL");
    assert_eq!(normalized.settings.query_timeout_sec, 0);
    assert_eq!(normalized.settings.to_settings(), ShellSettings {
        theme: "system".into(),
        density: "compact".into(),
        editor_font: 24,
        editor_theme: "auto".into(),
        locale: "zh-CN".into(),
        page_size: 1,
        preview_kb: 64,
        object_page: 1,
        null_copy: "NULL".into(),
        query_timeout_sec: 0,
        auto_check_updates: true,
        empty_display: "hint".into(),
        index_naming: crate::ddl::naming::IndexNaming::default(),
    });
    let _ = std::fs::remove_dir_all(&dir);
}

#[gpui_kit::test]
fn history_preview_uses_stacked_layout_when_content_is_narrow(cx: &mut gpui_kit::TestAppContext) {
    let dir = std::env::temp_dir().join(format!("rillsql-history-compact-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench(cx, services_at(&dir), false);
    cx.update(|cx| {
        view.update(cx, |view, _| {
            view.model.shell.page = AppPage::History;
            view.model.history.runs = vec![
                history_run("a", "select 1", "live", "ok"),
                history_run("b", "select 2", "live", "error"),
            ];
            view.model.history.selected_run = Some("a".into());
            view.model.history.preview_open = true;
        });
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let view = view.read(cx);
        assert!(super::chrome::history_stacks_preview(600.), "600 应走窄布局");
        assert!(!super::chrome::history_stacks_preview(900.), "900 仍并排");
        assert!(view.model.history.preview_open);
    })
    .unwrap();
    cx.update_window(window, |_, window, cx| {
        window.click("history-preview-close", cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| assert!(!view.read(cx).model.history.preview_open));
    let _ = std::fs::remove_dir_all(&dir);
}

#[gpui_kit::test]
fn history_and_settings_pages_render_and_apply_their_controls(cx: &mut gpui_kit::TestAppContext) {
    let dir = std::env::temp_dir().join(format!("rillsql-history-render-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let (window, view) = open_workbench(cx, Arc::clone(&services), false);
    cx.update(|cx| {
        view.update(cx, |view, _| {
            view.model.shell.page = AppPage::History;
            view.model.history.runs = vec![
                history_run("a", "select 1", "live", "ok"),
                history_run("b", "select 2", "live", "error"),
            ];
            view.model.history.selected_run = Some("a".into());
            view.model.history.preview_open = true;
        });
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        view.update(cx, |view, cx| view.perform("history-status-failed", cx));
        window.render_frame(cx);
        assert_eq!(view.read(cx).model.history.filter_status, "failed");
        assert_eq!(view.read(cx).model.history.filtered_runs().len(), 1);
        window.click("history-preview-close", cx);
        window.render_frame(cx);
        assert!(!view.read(cx).model.history.preview_open);
        window.click("history-filter-reset", cx);
        window.render_frame(cx);
        assert!(view.read(cx).model.history.filter_status.is_empty());
        view.update(cx, |view, cx| {
            view.perform("saved-queries", cx);
            view.model.history.saved = vec![crate::query::SavedQuery {
                id: "s1".into(),
                name: "计数".into(),
                sql: "select 1".into(),
                connection_id: "live".into(),
                connection_name: "conn-live".into(),
                database_name: "app".into(),
                engine: "postgres".into(),
                updated_at: "2026-11-07T00:00:00Z".into(),
            }];
            view.model.history.selected_saved = Some("s1".into());
            view.perform("history-rename", cx);
        });
        window.render_frame(cx);
        assert!(view.read(cx).model.history.rename_open);
        window.click("history-rename-cancel", cx);
        window.render_frame(cx);
        assert!(!view.read(cx).model.history.rename_open);
        view.update(cx, |view, cx| view.perform("saved-delete", cx));
        window.render_frame(cx);
        assert!(view.read(cx).model.history.confirm_delete);
        window.click("cmd-saved-delete-cancel", cx);
        window.render_frame(cx);
        assert!(!view.read(cx).model.history.confirm_delete);
        view.update(cx, |view, cx| {
            view.perform("settings", cx);
            // 设置页默认落在“外观与编辑器”，读取上限在“数据读取”分区里。
            view.perform("settings-section-data", cx);
        });
        window.render_frame(cx);
        let before = view.read(cx).model.settings.object_page;
        window.click("object-page-up", cx);
        window.render_frame(cx);
        assert!(view.read(cx).model.settings.object_page > before);
        window.click("object-page-down", cx);
        window.render_frame(cx);
        assert_eq!(view.read(cx).model.settings.object_page, before);
    })
    .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn auto_update_check_is_skipped_when_the_setting_is_off() {
    let mut model = WorkbenchModel::new(Shell::default());
    model.settings.auto_check_updates = false;
    assert!(!model.begin_update_check(), "关闭时不发起检查");
    assert_eq!(model.update_checks, 0);
    assert_eq!(model.update_job, None);
    // 手动检查是用户明确要查：不受“启动时自动检查”开关影响。
    assert!(model.dispatch("check-updates").is_some());
    assert_eq!(model.update_checks, 1, "手动检查照常发起");
    assert_eq!(model.update.stage, super::update::UpdateStage::Checking { manual: true });
    assert_eq!(model.update_job, Some(super::update::UpdateJob::Check));
    // 检查进行中不重复排任务。
    model.update_job = None;
    assert!(model.dispatch("check-updates").is_some());
    assert_eq!(model.update_checks, 1);
    assert_eq!(model.update_job, None);
    model.update.finish_check(Ok(None), "10:00".into());
    model.settings.auto_check_updates = true;
    assert!(model.begin_update_check());
    assert_eq!(model.update_checks, 2);
    assert_eq!(model.update.stage, super::update::UpdateStage::Checking { manual: false });
}

/// 发现新版本后：状态栏入口打开更新卡片；没有原地安装路线时不排下载，只能去发布页；
/// 有运行中的任务时安装被门禁挡住，原因留在卡片上，当前版本不动。
#[test]
fn update_offer_routes_to_the_card_and_respects_the_install_gate() {
    use crate::release::check::{InstallRoute, UpdateOffer};
    use crate::release::PlatformUpdate;
    let mut model = WorkbenchModel::new(Shell::default());
    let offer = UpdateOffer {
        version: "9.0.0".into(),
        notes: String::new(),
        pub_date: None,
        platform: Some(PlatformUpdate { target: "linux-x86_64".into(), version: "9.0.0".into(), url: "https://example.test/a.AppImage".into(), signature: "c2ln".into(), format: "appimage" }),
        page_url: "https://github.com/sakullla/RillSQL/releases/tag/v9.0.0".into(),
    };
    assert!(model.begin_manual_update_check());
    // 视图取走检查任务、在后台读完清单后落结果。
    assert_eq!(model.update_job.take(), Some(super::update::UpdateJob::Check));
    model.update.finish_check(Ok(Some(offer)), "10:00".into());
    assert!(model.update.announces());

    assert!(model.dispatch("update-show").is_some());
    assert_eq!(model.shell.page, AppPage::Settings);
    assert_eq!(model.settings_section, crate::app::views::settings::SettingsSection::Local);

    // 开发构建与 deb 安装：只能去发布页，不排下载。
    model.update_route = InstallRoute::Manual;
    assert!(model.request_update_download().is_err());
    assert_eq!(model.update_job, None);

    model.update_route = InstallRoute::InPlace(std::path::PathBuf::from("/opt/RillSQL.AppImage"));
    let _ = model.dispatch("new-query");
    model.tabs[0].running = true;
    assert_eq!(model.request_update_download().unwrap_err(), "shell.update.busy");
    assert!(matches!(model.update.stage, super::update::UpdateStage::Failed { .. }), "挡住的原因写在更新卡片上");
    assert_eq!(model.update_job, None);

    model.tabs[0].running = false;
    model.tabs[0].draft.clear();
    assert!(model.dispatch("update-install").is_some());
    assert_eq!(model.update_job, Some(super::update::UpdateJob::Download));
    assert_eq!(model.update.stage, super::update::UpdateStage::Downloading { done: 0, total: None });
}

/// 历史页的四种非正常状态必须彼此可区分：读取中给骨架，失败给原因与重试，
/// 无数据与无命中用不同的标题和不同的主操作；主操作沿用 kit 的 38px 缩进。
/// 两页的分栏阈值都按内容宽度判断，而不是窗口宽度：同一个 900 宽窗口，
/// 侧栏收起时内容 848（并排），侧栏停靠时内容 618（堆叠）。这样侧栏开合的
/// 结果与"页面还剩多少可写宽度"一致，不会按窗口宽度给出错误的并排。
#[gpui_kit::test]
fn settings_and_history_breakpoints_follow_content_width(cx: &mut gpui_kit::TestAppContext) {
    let dir = std::env::temp_dir().join(format!("rillsql-breakpoints-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (width, content, side_by_side, stacks) in [
        (1360f32, 1074, true, false),
        (900f32, 848, true, false),
        (760f32, 708, false, true),
        (640f32, 588, false, true),
    ] {
        let (window, view) = open_workbench_sized(cx, services_at(&dir), false, width, 800.);
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let model = &view.read(cx).model;
            assert_eq!(model.viewport_width, width as i32, "{width}");
            assert_eq!(model.layout().content_width, content, "{width}: 内容宽度");
            assert_eq!(super::chrome::settings_side_by_side(content as f32), side_by_side, "{width}: 设置分栏");
            assert_eq!(super::chrome::history_stacks_preview(content as f32), stacks, "{width}: 历史堆叠");
        })
        .unwrap();
        if width == 900. {
            cx.update(|cx| view.update(cx, |view, _| view.model.shell.sidebar_open = true));
            cx.update_window(window, |_, window, cx| {
                window.render_frame(cx);
                let model = &view.read(cx).model;
                assert!(model.layout().content_width < 820, "侧栏停靠后内容宽度变小：{}", model.layout().content_width);
                assert!(!super::chrome::settings_side_by_side(model.layout().content_width as f32), "同一窗口改按内容宽度堆叠");
                assert!(super::chrome::history_stacks_preview(model.layout().content_width as f32));
            })
            .unwrap();
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[gpui_kit::test]
fn history_states_are_distinct_and_keep_the_state_action_inset(cx: &mut gpui_kit::TestAppContext) {
    let dir = std::env::temp_dir().join(format!("rillsql-history-phases-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench(cx, services_at(&dir), false);
    cx.update(|cx| {
        view.update(cx, |view, _| {
            view.model.shell.page = AppPage::History;
            view.model.history.begin_load();
        })
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("skeleton-state").is_some(), "读取中给首屏骨架");
        assert!(window.try_find("history-empty-query").is_none(), "读取中不能同时是无数据");
        assert!(window.try_find("history-retry").is_none(), "读取中不是失败");
    })
    .unwrap();

    cx.update(|cx| view.update(cx, |view, _| view.model.history.mark_failure("历史库只读".into())));
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let error = window.try_find("history-failure").expect("失败块");
        assert!(error.visible());
        let retry = window.try_find("history-retry").expect("失败要能重试");
        assert!(retry.visible());
        assert!(window.try_find("skeleton-state").is_none(), "失败不再显示骨架");
    })
    .unwrap();

    cx.update(|cx| view.update(cx, |view, _| view.model.history.apply_list(Vec::new(), Vec::new())));
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let block = window.try_find("empty-state").expect("无数据空态");
        assert!(block.visible());
        let detail = window.try_find("empty-state-detail").expect("空态说明");
        let action = window.try_find("history-empty-query").expect("空态主操作");
        let inset = f32::from(action.bounds().origin.x) - f32::from(detail.bounds().origin.x);
        assert!(
            (inset - crate::ui::kit::STATE_ACTION_INSET).abs() < 0.5,
            "主操作与说明左对齐，缩进 {}px，实际 {inset}",
            crate::ui::kit::STATE_ACTION_INSET
        );
        assert!(f32::from(detail.bounds().size.width) <= crate::ui::kit::STATE_DETAIL_MAX_W + 0.5, "说明按面板宽换行");
        assert!(window.try_find("history-empty-reset").is_none(), "无数据不是无命中");
    })
    .unwrap();

    cx.update(|cx| {
        view.update(cx, |view, _| {
            view.model.history.apply_list(
                vec![history_run("a", "select 1", "live", "ok"), history_run("b", "select 2", "live", "error")],
                Vec::new(),
            );
            view.model.history.filter_text = "missing".into();
        })
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("no-match-state").is_some(), "筛选无命中用无命中语义");
        assert!(window.try_find("history-empty-reset").is_some(), "无命中的主操作是清除筛选");
        assert!(window.try_find("empty-state").is_none(), "无命中不能画成无数据");
    })
    .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// 历史页键盘：方向键只在筛选后的可见行里走，回车载入编辑器但绝不执行，Delete 走删除确认。
#[gpui_kit::test]
fn history_keyboard_walks_rows_and_enter_only_loads(cx: &mut gpui_kit::TestAppContext) {
    let dir = std::env::temp_dir().join(format!("rillsql-history-keys-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench(cx, services_at(&dir), false);
    cx.update(|cx| {
        view.update(cx, |view, _| {
            view.model.shell.page = AppPage::History;
            view.model.history.apply_list(
                vec![
                    history_run("a", "select 1", "live", "ok"),
                    history_run("b", "select 2", "live", "error"),
                    history_run("c", "select 3", "live", "ok"),
                ],
                Vec::new(),
            );
            view.model.history.selected_run = Some("a".into());
        })
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let focus = view.read(cx).focus.clone();
        window.focus(&focus, cx);
        window.render_frame(cx);
        window.press("down", cx);
        assert_eq!(view.read(cx).model.history.selected_run.as_deref(), Some("b"), "下移一行");
        window.press("end", cx);
        assert_eq!(view.read(cx).model.history.selected_run.as_deref(), Some("c"), "End 到末行");
        window.press("home", cx);
        assert_eq!(view.read(cx).model.history.selected_run.as_deref(), Some("a"), "Home 回首行");
        // R13：列表里 ⌘C（其他平台 Ctrl+C）复制选中条目的 SQL。
        window.press(if cfg!(target_os = "macos") { "cmd-c" } else { "ctrl-c" }, cx);
        assert_eq!(view.read(cx).model.copied, "select 1", "⌘C 复制选中条目的 SQL");
        // Delete 与行上的删除按钮同一条命令：保存的查询先开确认层，不直接删；
        // 表格的标记删除只属于网格，不能在历史页里累计。
        view.update(cx, |view, _| {
            view.model.history.section = HistorySection::Saved;
            view.model.history.saved = vec![crate::query::SavedQuery {
                id: "s1".into(),
                name: "计数".into(),
                sql: "select 1".into(),
                connection_id: "live".into(),
                connection_name: "conn-live".into(),
                database_name: "app".into(),
                engine: "postgres".into(),
                updated_at: "2026-11-07T00:00:00Z".into(),
            }];
            view.model.history.selected_saved = Some("s1".into());
        });
        let marked = view.read(cx).model.shell.marked_delete;
        window.render_frame(cx);
        window.press("delete", cx);
        assert!(view.read(cx).model.history.confirm_delete, "Delete 先开确认层");
        assert_eq!(view.read(cx).model.shell.marked_delete, marked, "历史页的 Delete 不再冒充网格标记删除");
        view.update(cx, |view, cx| view.perform("saved-delete-cancel", cx));
        // 装了键位绑定时 Delete 以动作进来，落到同一条删除命令。
        view.update(cx, |view, cx| view.perform("mark-delete", cx));
        assert!(view.read(cx).model.history.confirm_delete, "动作入口同样落到删除确认");
        assert_eq!(view.read(cx).model.shell.marked_delete, marked, "动作入口也不累计网格指标");
        view.update(cx, |view, cx| view.perform("saved-delete-cancel", cx));
        view.update(cx, |view, _| {
            view.model.history.section = HistorySection::Runs;
            view.model.history.saved.clear();
            view.model.history.selected_saved = None;
            view.model.history.selected_run = Some("a".into());
        });

        let executions = view.read(cx).model.executions;
        let loads = view.read(cx).model.loads;
        window.press("enter", cx);
        window.render_frame(cx);
        assert_eq!(view.read(cx).model.loads, loads + 1, "回车只载入");
        assert_eq!(view.read(cx).model.executions, executions, "载入不得执行 SQL");
        assert_eq!(view.read(cx).model.active_tab().map(|tab| tab.draft.as_str()), Some("select 1"));
    })
    .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// 历史页的确认层（清空历史 / 删除保存查询）不接管键盘焦点，
/// 所以这些键必须在这里让路：换选中、载入换页、越过确认直接删除都不允许。
#[gpui_kit::test]
fn history_confirm_sheets_own_the_keyboard(cx: &mut gpui_kit::TestAppContext) {
    let dir = std::env::temp_dir().join(format!("rillsql-history-confirm-keys-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench(cx, services_at(&dir), false);
    let saved = |id: &str, name: &str, sql: &str| crate::query::SavedQuery {
        id: id.into(),
        name: name.into(),
        sql: sql.into(),
        connection_id: "live".into(),
        connection_name: "conn-live".into(),
        database_name: "app".into(),
        engine: "postgres".into(),
        updated_at: "2026-11-07T00:00:00Z".into(),
    };
    cx.update(|cx| {
        view.update(cx, |view, _| {
            view.model.shell.page = AppPage::History;
            view.model.history.apply_list(
                vec![history_run("a", "select 1", "live", "ok"), history_run("b", "select 2", "live", "ok")],
                vec![saved("s1", "第一条", "select 1"), saved("s2", "第二条", "select 2")],
            );
            view.model.history.section = HistorySection::Saved;
            view.model.history.selected_saved = Some("s1".into());
        })
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let focus = view.read(cx).focus.clone();
        window.focus(&focus, cx);
        window.render_frame(cx);
        // 打开删除确认：确认的是 s1。
        view.update(cx, |view, cx| view.perform("saved-delete", cx));
        assert!(view.read(cx).model.history.confirm_delete, "Delete 先开确认层");
        // 确认层里按方向键、回车、Delete：一律不生效。
        window.press("down", cx);
        window.press("enter", cx);
        window.press("delete", cx);
        window.render_frame(cx);
        {
            let model = view.read(cx);
            assert!(model.model.history.confirm_delete, "确认层不该被键位关掉或确认");
            assert_eq!(model.model.history.selected_saved.as_deref(), Some("s1"), "方向键不改选中");
            assert_eq!(model.model.history.delete_target().as_deref(), Some("s1"), "删除目标不漂移");
            assert_eq!(model.model.shell.page, AppPage::History, "回车不能在确认层下面载入换页");
            assert_eq!(model.model.pending_len(), 0, "确认前不排队任何删除");
        }
        // 取消后回到列表，确认层之外仍然是原来的键盘行为。
        view.update(cx, |view, cx| view.perform("saved-delete-cancel", cx));
        window.press("down", cx);
        assert_eq!(view.read(cx).model.history.selected_saved.as_deref(), Some("s2"), "取消后方向键恢复");

        // 清空历史开着时，Delete 不能直接把运行记录排进删除队列。
        view.update(cx, |view, _| {
            view.model.history.section = HistorySection::Runs;
            view.model.history.selected_run = Some("a".into());
        });
        view.update(cx, |view, cx| view.perform("history-clear", cx));
        assert!(view.read(cx).model.history.confirm_clear, "清空历史先开确认层");
        window.press("down", cx);
        window.press("delete", cx);
        window.render_frame(cx);
        {
            let model = view.read(cx);
            assert!(model.model.history.confirm_clear, "清空确认还在");
            assert_eq!(model.model.history.selected_run.as_deref(), Some("a"), "方向键不改选中");
            assert_eq!(model.model.pending_len(), 0, "确认前不排队清空或删除");
            assert_eq!(model.model.shell.page, AppPage::History);
        }
    })
    .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// 历史确认层开着时，侧栏有焦点的方向键与回车同样让路：
/// 确认层不接管键盘焦点，键落到对象树上就会去动另一个对象、甚至打开一张表。
#[gpui_kit::test]
fn history_confirm_sheet_also_yields_the_sidebar_keys(cx: &mut gpui_kit::TestAppContext) {
    let dir = std::env::temp_dir().join(format!("rillsql-history-confirm-sidebar-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench(cx, services_at(&dir), false);
    let saved = |id: &str, name: &str, sql: &str| crate::query::SavedQuery {
        id: id.into(),
        name: name.into(),
        sql: sql.into(),
        connection_id: "live".into(),
        connection_name: "conn-live".into(),
        database_name: "app".into(),
        engine: "postgres".into(),
        updated_at: "2026-11-07T00:00:00Z".into(),
    };
    cx.update(|cx| {
        view.update(cx, |view, _| {
            // 对象树里要有行可选：确认层下按方向键能不能动树，就看这两行。
            view.model.connections.rows.push(record_for("notes", "笔记", Engine::Sqlite));
            view.model.connections.rows.push(record_for("shop", "商城", Engine::Sqlite));
            view.model.shell.page = AppPage::History;
            view.model.shell.focus = crate::app::FocusTarget::Sidebar;
            view.model.shell.sidebar_open = true;
            view.model.history.apply_list(vec![history_run("a", "select 1", "live", "ok")], vec![saved("s1", "第一条", "select 1")]);
            view.model.history.section = HistorySection::Saved;
            view.model.history.selected_saved = Some("s1".into());
        })
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let focus = view.read(cx).focus.clone();
        window.focus(&focus, cx);
        window.render_frame(cx);
        // 打开删除确认：确认的是 s1，焦点却在侧栏。
        view.update(cx, |view, cx| view.perform("saved-delete", cx));
        assert!(view.read(cx).model.history.confirm_delete, "Delete 先开确认层");
        window.press("down", cx);
        window.press("enter", cx);
        window.render_frame(cx);
        {
            let model = view.read(cx);
            assert!(model.model.history.confirm_delete, "确认层不该被侧栏的键关掉或确认");
            assert_eq!(model.model.explorer_current_row(), None, "确认层开着时方向键不能动对象树");
            assert_eq!(model.model.shell.page, AppPage::History, "回车不能在确认层下面打开对象");
            assert_eq!(model.model.history.delete_target().as_deref(), Some("s1"), "删除目标不漂移");
        }
        // 取消之后侧栏的键恢复：方向键照常动对象树。
        view.update(cx, |view, cx| view.perform("saved-delete-cancel", cx));
        window.press("down", cx);
        window.render_frame(cx);
        assert!(
            view.read(cx).model.explorer_current_row().is_some(),
            "确认层关掉后侧栏的方向键恢复"
        );
    })
    .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// 一次已经被更晚的写入接管的失败不算设置页的答案：
/// 那份更新的快照已经写成功，旧的失败不能把它盖成"保存失败"。
#[gpui_kit::test]
fn a_superseded_settings_failure_does_not_overwrite_a_newer_result(cx: &mut gpui_kit::TestAppContext) {
    use crate::app::views::settings::SaveState;

    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-settings-superseded-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let (window, view) = open_workbench(cx, Arc::clone(&services), false);
    cx.update(|cx| view.update(cx, |view, cx| view.perform("settings", cx)));
    cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
    // 先真的落一次盘，拿到一个当前代次。
    cx.update(|cx| view.update(cx, |view, cx| view.perform("font-up", cx)));
    assert!(
        pump_until(cx, window, &view, |view, _| view.model.settings_save == SaveState::Saved),
        "第一次写入应当成功"
    );
    let newest = cx.update(|cx| view.read(cx).publish_generation());
    assert!(newest > 0, "写盘推进了代次");
    cx.update(|cx| {
        view.update(cx, |view, _| {
            view.model.settings_save = SaveState::Saving;
            view.apply_persist_settings(Ok(Ok(())), newest - 1, "{}".into());
            assert_eq!(view.model.settings_save, SaveState::Saving, "旧代次被跳过并不代表最新快照已经落盘");
            // 旧的那次失败晚到：代次已经不是它，结果不算数。
            view.apply_persist_settings(Ok(Err("磁盘写失败".into())), newest - 1, "{}".into());
            assert_eq!(view.model.settings_save, SaveState::Saving, "被接管的失败不改设置页状态");
            // 仍是最新一次写入的失败照旧显示出来。
            view.apply_persist_settings(Ok(Err("磁盘写失败".into())), newest, "{}".into());
            assert!(view.model.settings_save.failed(), "最新一次写入的失败要显示出来");
        })
    });
    let _ = std::fs::remove_dir_all(&dir);
}

/// 设置页：宽窗口左侧竖排分区导航、右侧表单，窄窗口折成顶部一行；
/// 切换分区只换右侧表单，不改任何设置值。
#[gpui_kit::test]
fn settings_sections_switch_and_nav_stacks_when_narrow(cx: &mut gpui_kit::TestAppContext) {
    let dir = std::env::temp_dir().join(format!("rillsql-settings-nav-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench_sized(cx, services_at(&dir), false, 1360., 800.);
    cx.update(|cx| view.update(cx, |view, cx| view.perform("settings", cx)));
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let nav = window.try_find("settings-nav").expect("分区导航");
        let form = window.try_find("settings-form").expect("分区表单");
        assert!((f32::from(nav.bounds().size.width) - 180.).abs() < 0.5, "宽窗口导航固定 180");
        assert!(f32::from(form.bounds().origin.x) >= f32::from(nav.bounds().right()) - 0.5, "表单在导航右侧");
        assert!(window.try_find("theme-dark").is_some(), "默认分区是外观与编辑器");
        assert!(window.try_find("object-page-up").is_none(), "别的分区不渲染控件");
        window.click("settings-section-data", cx);
        window.render_frame(cx);
        assert!(window.try_find("object-page-up").is_some(), "数据读取分区给出读取上限");
        assert!(window.try_find("theme-dark").is_none(), "一次只显示一个分区");
        window.click("settings-section-local", cx);
        window.render_frame(cx);
        assert!(window.try_find("settings-group-update").is_some());
        assert!(super::chrome::settings_side_by_side(1360.), "1360 并排");
    })
    .unwrap();
    cx.update(|cx| assert_eq!(view.read(cx).model.settings_section, crate::app::views::settings::SettingsSection::Local));

    let narrow_dir = std::env::temp_dir().join(format!("rillsql-settings-nav-narrow-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&narrow_dir);
    std::fs::create_dir_all(&narrow_dir).unwrap();
    let (narrow_window, narrow_view) = open_workbench_sized(cx, services_at(&narrow_dir), false, 640., 700.);
    cx.update(|cx| narrow_view.update(cx, |view, cx| view.perform("settings", cx)));
    cx.update_window(narrow_window, |_, window, cx| {
        window.render_frame(cx);
        let nav = window.try_find("settings-nav").expect("分区导航");
        let form = window.try_find("settings-form").expect("分区表单");
        assert!(!super::chrome::settings_side_by_side(640.), "640 不走并排");
        assert!(f32::from(nav.bounds().right()) <= 640.5, "导航不出窗口");
        assert!(f32::from(form.bounds().right()) <= 640.5, "表单不出窗口");
        assert!(f32::from(nav.bounds().origin.y) + f32::from(nav.bounds().size.height) <= f32::from(form.bounds().origin.y) + 0.5, "窄窗口导航在表单上方");
    })
    .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&narrow_dir);
}

/// 两页在浅/深色 × 紧凑/标准 × 1360/900/640 下的可测几何：行高随密度变化、
/// 页面不出窗口、设置导航在窄宽折到表单上方。
#[gpui_kit::test]
fn history_and_settings_geometry_holds_in_both_themes_densities_and_three_widths(cx: &mut gpui_kit::TestAppContext) {
    let dir = std::env::temp_dir().join(format!("rillsql-history-geometry-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for width in [1360f32, 900., 640.] {
        let (window, view) = open_workbench_sized(cx, services_at(&dir), false, width, 800.);
        let mut history_rows = Vec::new();
        let mut appearance_cards = Vec::new();
        for density in [Density::Compact, Density::Standard] {
            for theme in [ThemePreference::Light, ThemePreference::Dark] {
                cx.update(|cx| {
                    view.update(cx, |view, _| {
                        view.model.shell.density = density;
                        view.model.shell.theme = theme;
                        view.model.shell.page = AppPage::History;
                        view.model.history.section = HistorySection::Runs;
                        view.model.history.apply_list(
                            vec![history_run("a", "select 1", "live", "ok"), history_run("b", "select 2", "live", "error")],
                            Vec::new(),
                        );
                        view.model.history.selected_run = Some("a".into());
                        view.model.history.preview_open = false;
                    })
                });
                cx.update_window(window, |_, window, cx| {
                    window.render_frame(cx);
                    let row = window.find("run-a").bounds();
                    assert!(
                        f32::from(row.origin.x) >= -0.5 && f32::from(row.right()) <= width + 0.5,
                        "{width} {density:?} {theme:?}: 历史行不出窗口"
                    );
                    history_rows.push(f32::from(row.size.height));
                })
                .unwrap();
            }
            // 设置页：同一窗口切到设置，量分区导航与外观卡片。
            cx.update(|cx| view.update(cx, |view, cx| view.perform("settings", cx)));
            cx.update_window(window, |_, window, cx| {
                window.render_frame(cx);
                let nav = window.find("settings-nav").bounds();
                let form = window.find("settings-form").bounds();
                let card = window.find("settings-group-appearance").bounds();
                assert!(f32::from(nav.right()) <= width + 0.5, "{width}: 导航不出窗口");
                assert!(f32::from(form.right()) <= width + 0.5, "{width}: 表单不出窗口");
                assert!(f32::from(card.right()) <= width + 0.5, "{width}: 设置卡片不出窗口");
                if width >= 900. {
                    assert!((f32::from(nav.size.width) - 180.).abs() < 0.5, "{width}: 宽窗口导航固定 180，实际 {}", f32::from(nav.size.width));
                    assert!(f32::from(nav.origin.x) < f32::from(form.origin.x), "{width}: 导航在表单左侧");
                } else {
                    assert!(f32::from(nav.size.height) < 60., "{width}: 窄窗口导航折成一行，高 {}", f32::from(nav.size.height));
                    assert!(f32::from(nav.origin.y) < f32::from(form.origin.y), "{width}: 窄窗口导航在表单上方");
                }
                appearance_cards.push(f32::from(card.size.height));
            })
            .unwrap();
        }
        assert_eq!(history_rows.len(), 4);
        // 密度只改内边距与控件高：历史行 67.5 → 71.5（row_pad_y 5→7，上下各 +2）。
        assert!((history_rows[0] - 67.5).abs() < 0.5, "{width}: 紧凑历史行 67.5，实际 {:?}", history_rows);
        assert!((history_rows[2] - 71.5).abs() < 0.5, "{width}: 标准历史行 71.5，实际 {:?}", history_rows);
        assert!((history_rows[2] - history_rows[0] - 4.).abs() < 0.5, "{width}: 两种密度行高只差上下内边距");
        assert!(history_rows[0] == history_rows[1] && history_rows[2] == history_rows[3], "{width}: 主题不改变行高");
        // 三行设置卡片：每行 +2 下内边距、控件 26→30 = 24。
        assert!((appearance_cards[0] - 180.).abs() < 0.5, "{width}: 紧凑外观卡片 180，实际 {:?}", appearance_cards);
        assert!((appearance_cards[1] - 204.).abs() < 0.5, "{width}: 标准外观卡片 204，实际 {:?}", appearance_cards);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// 设置写盘失败必须在设置页与状态栏同时可见，修好路径后重试能真正写到本机。
#[gpui_kit::test]
fn failed_settings_write_is_visible_then_retry_persists(cx: &mut gpui_kit::TestAppContext) {
    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-settings-save-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let (window, view) = open_workbench(cx, Arc::clone(&services), false);
    cx.update(|cx| view.update(cx, |view, cx| view.perform("settings", cx)));
    cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();

    // 用目录占住状态文件路径：读回与写入都会失败，和磁盘只读同类。
    let state_path = services.state.path().to_path_buf();
    let _ = std::fs::remove_file(&state_path);
    std::fs::create_dir_all(&state_path).unwrap();
    cx.update(|cx| view.update(cx, |view, cx| view.perform("font-up", cx)));
    assert!(
        pump_until(cx, window, &view, |view, _| view.model.settings_save.failed()),
        "写盘失败要回到模型，而不是被后台任务吞掉"
    );
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("settings-retry-save").is_some(), "设置页给就地重试");
        assert!(window.try_find("status-settings-save").is_some(), "状态栏有全局反馈");
        assert!(view.read(cx).model.settings.editor_font > 10, "写盘失败不影响改动立即生效");
    })
    .unwrap();

    std::fs::remove_dir_all(&state_path).unwrap();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("settings-retry-save", cx);
    })
    .unwrap();
    assert!(
        pump_until(cx, window, &view, |view, _| view.model.settings_save == crate::app::views::settings::SaveState::Saved),
        "重试后应确认已保存"
    );
    let stored = jobs::runtime().block_on(services.load_state()).expect("状态文件已写入");
    let current_font = cx.update(|cx| view.read(cx).model.settings.editor_font);
    assert_eq!(stored.shell.editor_font, current_font, "写的是当前设置");
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("settings-retry-save").is_none(), "成功后不再显示重试");
        assert!(window.try_find("status-settings-save").is_none(), "状态栏提示同时消失");
    })
    .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// 设置写盘在飞时来了别的命令（它自己的快照也要落盘并推进同一个代次），
/// 这次设置改动仍然要落到“已保存”，不能因为代次被别人推开就永久停在保存中。
#[gpui_kit::test]
fn settings_save_completes_when_an_unrelated_command_lands_first(cx: &mut gpui_kit::TestAppContext) {
    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-settings-race-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let (window, view) = open_workbench(cx, Arc::clone(&services), false);
    cx.update(|cx| view.update(cx, |view, cx| view.perform("settings", cx)));
    cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();

    // 同一个同步片段里先改设置（进入保存中），紧接着跑一条无关命令：
    // 它的静默写盘会和这次设置写盘抢同一个代次与写盘闸门，
    // 而它自己不会给设置页任何反馈，所以只有这次设置写盘能结束“保存中”。
    cx.update(|cx| {
        view.update(cx, |view, cx| {
            view.perform("font-up", cx);
            view.perform("history", cx);
        })
    });
    assert!(
        pump_until(cx, window, &view, |view, _| view.model.settings_save == crate::app::views::settings::SaveState::Saved),
        "无关命令不能把设置写盘卡在保存中"
    );
    let stored = jobs::runtime().block_on(services.load_state()).expect("状态文件已写入");
    let current_font = cx.update(|cx| view.read(cx).model.settings.editor_font);
    assert_eq!(stored.shell.editor_font, current_font, "最终的设置改动仍然落到本机");
    let _ = std::fs::remove_dir_all(&dir);
}

#[gpui_kit::test]
fn public_settings_reach_the_query_pane_without_a_restart(cx: &mut gpui_kit::TestAppContext) {
    let dir = std::env::temp_dir().join(format!("rillsql-shell-settings-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let (window, view) = open_workbench(cx, Arc::clone(&services), false);
    cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();

    cx.update(|cx| {
        view.update(cx, |view, cx| {
            view.perform("settings", cx);
            view.perform("font-up", cx);
            view.perform("editor-theme-next", cx);
            view.perform("page-size-up", cx);
            view.perform("preview-up", cx);
            view.perform("object-page-up", cx);
            view.perform("null-N", cx);
        });
    });
    cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
    cx.update(|cx| {
        let query = view.read(cx).query.read(cx);
        assert_eq!(query.editor_font(), 14, "font-up 应下发到 QueryPane");
        assert_eq!(query.editor_theme(), "contrast");
        assert_eq!(query.page_size(), 150);
        assert_eq!(query.preview_kb(), 2);
        assert_eq!(query.object_page(), 550);
        assert_eq!(query.null_copy(), "N", "QueryPane 保留原始设置，由网格转换显示文本");
        assert_eq!(query.run_limit(), 150, "查询读取行数跟随 page_size");
    });

    // 关掉自动检查后，启动路径不再发起检查；手动“检查更新”照常检查（测试里不发真实请求）。
    cx.update(|cx| {
        view.update(cx, |view, cx| {
            view.model.settings.auto_check_updates = false;
            assert!(!view.model.begin_update_check());
            view.perform("check-updates", cx);
        });
    });
    cx.update(|cx| assert_eq!(view.read(cx).model.update_checks, 1));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn same_named_objects_in_two_connections_never_cross_targets() {
    let mut model = WorkbenchModel::new(Shell::default());
    let left = tree_object("c1", "销售库", "postgres", "shop", "public", "orders", "table");
    let right = tree_object("c2", "仓库库", "postgres", "shop", "public", "orders", "table");
    assert!(model.set_catalog(&[left.clone(), right.clone()], ""));

    // 环境只用于展示，不作为身份；这里按目录条目实际生成的规格比较。
    let left_spec = crate::app::views::query::object_spec(&left, "");
    let right_spec = crate::app::views::query::object_spec(&right, "");
    assert_ne!(left_spec, right_spec, "同名表在两个连接下必须是两个目标");

    // 快速打开列出两条，各自携带完整目标。
    model.shell.palette_tables_only = true;
    let entries = model.palette_entries();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].id, format!("open-table:{left_spec}"));
    assert_eq!(entries[1].id, format!("open-table:{right_spec}"));
    assert!(model.dispatch(&entries[0].id).is_some());
    assert!(model.dispatch(&entries[1].id).is_some());
    assert_eq!(model.tabs.len(), 2, "不同目标必须是两个工作项");
    assert_eq!(model.tabs[0].connection_id.as_deref(), Some("c1"));
    assert_eq!(model.tabs[1].connection_id.as_deref(), Some("c2"));

    // 对象树的“打开数据”只看选中的那个对象。
    model.object_spec = Some(right_spec.clone());
    assert!(model.dispatch("tree-open-data").is_some());
    assert_eq!(model.active_tab().unwrap().connection_id.as_deref(), Some("c2"), "同名对象不能串到另一个连接");
    model.object_spec = Some(left_spec.clone());
    assert!(model.dispatch("tree-open-data").is_some());
    assert_eq!(model.active_tab().unwrap().connection_id.as_deref(), Some("c1"));

    // 结构项也按完整目标去重：两边各一份，不共用另一边的结构页。
    assert!(model.dispatch(&format!("open-structure-for:{left_spec}")).is_some());
    assert!(model.dispatch(&format!("open-structure-for:{right_spec}")).is_some());
    let structures: Vec<_> = model.tabs.iter().filter(|tab| tab.kind == WorkItemKind::Structure).collect();
    assert_eq!(structures.len(), 2, "同名对象的结构项必须分开");
    assert_ne!(structures[0].spec, structures[1].spec);

    // 对象动作不再按“连接:对象名”反查：复制全名给限定名，而不是连接 id。
    model.object_spec = Some(left_spec);
    assert!(model.dispatch("tree-copy-name").is_some());
    assert_eq!(model.copied, "public.orders");
    // 生成到新查询只写草稿、带上该对象的目标，不执行。
    model.object_spec = Some(right_spec.clone());
    assert!(model.dispatch("tree-select-sql").is_some());
    let tab = model.active_tab().unwrap();
    assert_eq!(tab.kind, WorkItemKind::Query);
    assert_eq!(tab.connection_id.as_deref(), Some("c2"));
    assert_eq!(tab.database.as_deref(), Some("shop"));
    assert!(tab.draft.to_lowercase().contains("orders"), "{}", tab.draft);
    assert_eq!(model.executions, 0, "生成查询不执行");

    // 对象菜单的导入导出以所选对象为准：先打开该对象，再打开抽屉。
    model.exchange_open = false;
    model.object_spec = Some(right_spec.clone());
    assert!(model.dispatch("exchange-object").is_some());
    assert_eq!(model.active_tab().unwrap().kind, WorkItemKind::Table);
    assert_eq!(model.active_tab().unwrap().spec.as_deref(), Some(right_spec.as_str()));
    assert_eq!(model.active_tab().unwrap().connection_id.as_deref(), Some("c2"));
    assert!(model.exchange_open, "来源就是所选对象时才打开抽屉");
    // 没有对象上下文：说明缺什么，抽屉保持关闭。
    model.exchange_open = false;
    model.object_spec = None;
    assert!(model.dispatch("exchange-object").is_some());
    assert!(!model.exchange_open);
    assert_eq!(model.notice_key, Some("shell.tabs.needObject"));
}

#[test]
fn same_named_objects_in_one_connection_stay_apart_per_database() {
    let mut model = WorkbenchModel::new(Shell::default());
    let shop = tree_object("pg", "销售库", "postgres", "shop", "public", "orders", "table");
    let archive = tree_object("pg", "销售库", "postgres", "warehouse", "public", "orders", "table");
    assert!(model.set_catalog(&[shop.clone(), archive.clone()], ""));
    let shop_spec = crate::app::views::query::object_spec(&shop, "dev");
    let archive_spec = crate::app::views::query::object_spec(&archive, "dev");
    assert_ne!(shop_spec, archive_spec, "同连接跨库同名对象必须是两个目标");
    assert!(model.dispatch(&format!("open-table:{shop_spec}")).is_some());
    assert!(model.dispatch(&format!("open-table:{archive_spec}")).is_some());
    assert_eq!(model.tabs.len(), 2);
    assert_eq!(model.tabs[0].database.as_deref(), Some("shop"));
    assert_eq!(model.tabs[1].database.as_deref(), Some("warehouse"));

    // 网格与交换来源只认完整目标：库不同就不是同一张表。
    let table = crate::grid::commands::TableRef {
        connection_id: "pg".into(),
        database: "warehouse".into(),
        schema: "public".into(),
        table: "orders".into(),
        engine: "postgres".into(),
        include_foreign_keys: false,
    };
    assert!(!super::view::spec_matches_table(&shop_spec, &table));
    assert!(super::view::spec_matches_table(&archive_spec, &table));
}

#[test]
fn low_frequency_navigation_moves_to_the_palette_with_context() {
    let mut model = WorkbenchModel::new(Shell::default());
    let entry = |model: &WorkbenchModel, id: &str| {
        model.palette_entries().into_iter().find(|entry| entry.id == id).expect(id)
    };
    // 没有对象或结果时两个入口仍然可发现，并说明缺什么。
    assert_eq!(entry(&model, "compare").detail, model.text("compare.context.none"));
    assert_eq!(entry(&model, "open-exchange").detail, model.text("shell.palette.exchange.none"));

    let orders = tree_object("pg", "销售库", "postgres", "shop", "public", "orders", "table");
    assert!(model.set_catalog(std::slice::from_ref(&orders), ""));
    let spec = crate::app::views::query::object_spec(&orders, "dev");
    model.object_spec = Some(spec.clone());
    let compare = entry(&model, "compare");
    assert!(compare.detail.contains("orders") && compare.detail.contains("销售库"), "{}", compare.detail);
    // 导入导出按活动工作项说明来源：只有对象树选择时还不能说会导出哪张表。
    assert_eq!(entry(&model, "open-exchange").detail, model.text("shell.palette.exchange.none"));

    // 英文界面同样带上下文，不把键名当文案。
    model.shell.locale = Locale::En;
    let compare = entry(&model, "compare");
    assert!(compare.detail.contains("orders"), "{}", compare.detail);
    assert!(!compare.detail.contains("compare.context"), "{}", compare.detail);
    model.shell.locale = Locale::ZhCn;

    // 对比入口带上当前对象；工作台入口把原工作项与焦点一起带回来。
    assert!(model.dispatch(&format!("open-table:{spec}")).is_some());
    assert_eq!(model.shell.focus, crate::app::FocusTarget::Grid);
    assert!(entry(&model, "open-exchange").detail.contains("orders"), "表格工作项才是导出来源");
    let active = model.active_tab.clone();
    assert!(model.dispatch("compare").is_some());
    assert_eq!(model.shell.page, AppPage::Compare);
    assert_eq!(model.active_tab, active, "对比页不更换工作项");
    assert_eq!(model.compare_target().map(|target| target.name), Some("orders".to_owned()));
    assert!(model.dispatch("activity-workspace").is_some());
    assert_eq!(model.shell.page, AppPage::Workspace);
    assert_eq!(model.tool, WorkTool::Table);
    assert_eq!(model.shell.focus, crate::app::FocusTarget::Grid, "返回工作项要回到它自己的焦点");
    assert_eq!(model.active_tab, active);

    // 固定活动栏只剩四个入口，其余仍然能从命令面板到达。
    for id in crate::app::model::RAIL_TOP.iter().chain(crate::app::model::RAIL_BOTTOM.iter()) {
        assert!(model.dispatch(id).is_some(), "{id}");
    }
    for id in ["compare", "open-exchange", "help"] {
        assert!(model.palette_entries().iter().any(|entry| entry.id == id), "{id} 应留在命令面板");
        assert!(!crate::app::model::RAIL_TOP.contains(&id) && !crate::app::model::RAIL_BOTTOM.contains(&id));
    }
}

#[test]
fn palette_keyboard_reaches_compare_exchange_and_help_and_restores_focus() {
    let mut model = WorkbenchModel::new(Shell::default());
    model.shell.focus = crate::app::FocusTarget::Grid;
    let pick = |model: &mut WorkbenchModel, id: &str| {
        assert!(model.dispatch("command-palette").is_some());
        model.palette_index = model
            .palette_entries()
            .iter()
            .position(|entry| entry.id == id)
            .unwrap_or_else(|| panic!("{id} 应在命令面板里"));
        model.key(NavKey::Enter)
    };
    assert_eq!(pick(&mut model, "compare").as_deref(), Some("compare"));
    assert!(!model.shell.palette_open, "回车后面板关闭");
    assert_eq!(model.shell.page, AppPage::Compare);
    assert_eq!(model.shell.focus, crate::app::FocusTarget::Grid, "面板关闭后焦点回原处");

    assert_eq!(pick(&mut model, "open-exchange").as_deref(), Some("open-exchange"));
    assert!(model.exchange_open, "键盘同样能打开交换");
    assert!(!model.shell.palette_open);
    assert!(model.dispatch("open-exchange").is_some());
    assert!(!model.exchange_open, "再按一次关闭交换");

    assert_eq!(pick(&mut model, "help").as_deref(), Some("help"));
    assert!(model.shell.help_open, "帮助不再占固定入口，但仍能从面板打开");
    model.shell.close_overlay();
    assert_eq!(model.shell.focus, crate::app::FocusTarget::Grid);
}

#[gpui_kit::test]
fn object_command_binds_the_query_to_the_exact_same_named_object(cx: &mut gpui_kit::TestAppContext) {
    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-shell-object-target-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench(cx, services_at(&dir), false);
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.plant_same_name_objects(cx));
        window.render_frame(cx);
    })
    .unwrap();
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.perform("object:pg|销售库|shop|public|orders|postgres|dev", cx));
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        let tab = view.read(cx).model.active_tab().expect("绑定查询").clone();
        assert_eq!(tab.connection_id.as_deref(), Some("pg"));
        assert_eq!(tab.database.as_deref(), Some("shop"));
        assert!(tab.draft.to_lowercase().contains("orders"), "{}", tab.draft);
        assert!(!tab.running, "对象动作只写草稿，不执行");
    });
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.perform("object:pg2|仓库库|shop|public|orders|postgres|prod", cx));
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        let tab = view.read(cx).model.active_tab().expect("绑定查询").clone();
        assert_eq!(tab.connection_id.as_deref(), Some("pg2"), "同名对象不能复用另一个连接的目标");
        assert!(tab.draft.to_lowercase().contains("orders"), "{}", tab.draft);
    });
    // 对象已不在目录里：仍然有可读原因，不静默改开另一个同名对象。
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.perform("object:pg|销售库|shop|public|missing|postgres|dev", cx));
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        let note = view.read(cx).query_note(cx);
        assert!(note.contains("目录"), "{note}");
    });
    let _ = std::fs::remove_dir_all(&dir);
}

#[gpui_kit::test]
fn compare_and_exchange_return_to_the_same_item_and_keep_staged_cells(cx: &mut gpui_kit::TestAppContext) {
    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-shell-return-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench(cx, services_at(&dir), false);
    let spec = "notes|笔记|main||customers|sqlite|dev";
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.perform(&format!("open-table:{spec}"), cx));
        window.render_frame(cx);
    })
    .unwrap();
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.mark_table_dirty(cx);
            // 交换是覆盖在同一个工作项上的抽屉，打开不改变工作项。
            view.perform("open-exchange", cx);
        });
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        let view = view.read(cx);
        assert!(view.model.exchange_open);
        assert_eq!(view.model.active_tab().map(|tab| tab.spec.clone()), Some(Some(spec.to_owned())));
        assert!(view.table_uncommitted(cx), "打开交换不能丢暂存");
    });
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.perform("open-exchange", cx));
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        let view = view.read(cx);
        assert!(!view.model.exchange_open, "再按一次关闭交换");
        assert!(view.table_uncommitted(cx), "关闭交换后暂存仍在");
        assert_eq!(view.model.shell.page, AppPage::Workspace);
        assert_eq!(view.model.tool, WorkTool::Table);
        assert_eq!(view.model.shell.focus, crate::app::FocusTarget::Grid);
        assert_eq!(view.model.active_tab().map(|tab| tab.spec.clone()), Some(Some(spec.to_owned())));
    });

    // 对比页同样带当前对象；返回后回到原工作项、焦点和暂存。
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.perform("compare-object", cx));
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.model.shell.page, AppPage::Compare);
        let (context, note, _baseline, _target) = view.compare_state(cx);
        assert_eq!(context.as_deref(), Some("main.customers · 笔记"), "{note}");
        assert!(note.contains("customers"), "{note}");
    });
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.perform("compare-back", cx));
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.model.shell.page, AppPage::Workspace, "返回后回到原工作项页面");
        assert_eq!(view.model.tool, WorkTool::Table);
        assert_eq!(view.model.shell.focus, crate::app::FocusTarget::Grid);
        assert!(view.table_uncommitted(cx), "返回后暂存仍在");
    });
    let _ = std::fs::remove_dir_all(&dir);
}

/// 网格右键的“对比”必须带正在显示的表：对象树里另一行选择不能顶掉它。
#[gpui_kit::test]
fn grid_menu_compare_carries_the_displayed_table_not_the_tree_row(cx: &mut gpui_kit::TestAppContext) {
    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-shell-grid-compare-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench(cx, services_at(&dir), false);
    let shown = "notes|笔记|main||customers|sqlite|dev";
    let tree_row = "notes|笔记|main||orders|sqlite|dev";
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.perform(&format!("open-table:{shown}"), cx));
        window.render_frame(cx);
    })
    .unwrap();
    // 对象树里单击另一张表：只改选择，不换工作项，网格仍然显示 customers。
    cx.update(|cx| {
        view.update(cx, |view, _| view.model.object_spec = Some(tree_row.to_owned()));
        let view = view.read(cx);
        assert_eq!(view.model.tool, WorkTool::Table);
        assert_eq!(view.model.active_tab().and_then(|tab| tab.spec.clone()).as_deref(), Some(shown));
    });
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.perform("menu-grid", cx));
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        let view = view.read(cx);
        let menu = view.model.menu.as_ref().expect("网格菜单");
        let ids: Vec<&str> = menu.items.iter().map(|item| item.id).collect();
        // 网格菜单只放按格子发生的动作；对比库从对象树与命令面板进入，仍以显示中的表为目标。
        assert_eq!(menu.place, MenuPlace::Grid);
        assert!(!ids.contains(&"compare-object"), "网格菜单不能再复用对象树的范围动作：{ids:?}");
        assert!(ids.contains(&"copy-region") && ids.contains(&"grid-filter-value"), "{ids:?}");
    });
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.perform("compare-grid", cx));
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.model.shell.page, AppPage::Compare);
        let (context, note, _baseline, _target) = view.compare_state(cx);
        assert_eq!(context.as_deref(), Some("main.customers · 笔记"), "网格菜单要带正在显示的表：{note}");
        assert!(!note.contains("orders"), "树里的另一行不能当当前对象：{note}");
        assert_eq!(view.model.object_spec.as_deref(), Some(tree_row), "对象树选择保持不变");
    });
    let _ = std::fs::remove_dir_all(&dir);
}

#[gpui_kit::test]
fn narrow_window_keeps_the_four_entry_rail_and_low_frequency_commands(cx: &mut gpui_kit::TestAppContext) {
    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-shell-rail-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench_sized(cx, services_at(&dir), false, 640., 900.);
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.perform("refresh-tree", cx);
        });
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        view.update(cx, |view, _| {
            view.model.set_width(640);
            assert_eq!(
                view.model.painted().activity_width,
                super::layout::RAIL_WIDTH,
                "窄窗口活动栏与绘制同宽，不再挤压内容列"
            );
            assert!(view.model.painted().occupied_width() <= 640);
        });
    });
    cx.update_window(window, |_, window, _| {
        let expected: Vec<String> = crate::app::model::RAIL_TOP
            .iter()
            .chain(crate::app::model::RAIL_BOTTOM.iter())
            .map(|id| format!("cmd-{id}"))
            .collect();
        assert_eq!(
            expected,
            ["cmd-activity-workspace", "cmd-activity-connections", "cmd-activity-history", "cmd-activity-settings"]
        );
        for id in ["cmd-activity-workspace", "cmd-activity-connections", "cmd-activity-history", "cmd-activity-settings"] {
            assert!(window.try_find(id).is_some(), "{id} 在 640×900 仍要可达");
        }
        for gone in ["cmd-compare", "cmd-open-exchange", "cmd-help"] {
            assert!(window.try_find(gone).is_none(), "{gone} 不再是固定入口");
        }
    })
    .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------ 目录层级（T2）

fn nav_connection(engine: Engine, id: &str, name: &str, databases: Vec<crate::catalog::model::DatabaseNode>) -> crate::catalog::NavConnection {
    let mut connection = crate::catalog::NavConnection::new(id, name, engine, crate::catalog::model::ConnStatus::Connected);
    connection.databases = databases;
    connection
}

fn nav_database(name: &str, objects: Vec<crate::catalog::model::ObjectRow>) -> crate::catalog::model::DatabaseNode {
    let total = objects.len();
    crate::catalog::model::DatabaseNode {
        name: name.to_owned(),
        schemas: vec![crate::catalog::model::SchemaNode {
            name: if name == "shop" { "public".to_owned() } else { String::new() },
            objects,
            total,
            error: None,
            kind_reports: Vec::new(),
        }],
        error: None,
    }
}

fn nav_id(connection: &str, database: &str, schema: &str, object: &crate::catalog::model::ObjectRow) -> String {
    crate::catalog::hierarchy::object_node_id(connection, database, schema, object)
}

fn expand_all(model: &mut WorkbenchModel, ids: &[String]) {
    for id in ids {
        let _ = model.dispatch(&format!("nav-toggle:{id}"));
    }
}

#[test]
fn tree_actions_bind_to_the_row_and_routines_never_open_a_data_page() {
    use crate::catalog::model::{ObjectKind, ObjectRow};
    let orders = ObjectRow::table("orders", Some(4));
    let trigger = ObjectRow::fresh("orders_touch", ObjectKind::Trigger);
    let pg = nav_connection(
        Engine::Postgres,
        "pg",
        "销售库",
        vec![nav_database("shop", vec![orders.clone(), trigger.clone()])],
    );
    let mut model = WorkbenchModel::new(Shell::default());
    assert!(model.apply_navigation(&[pg], false));

    let conn = crate::catalog::hierarchy::connection_node_id("pg");
    let db = crate::catalog::hierarchy::database_node_id("pg", "shop");
    let schema = crate::catalog::hierarchy::schema_node_id("pg", "shop", "public");
    let group = crate::catalog::hierarchy::group_node_id("pg", "shop", "public", ObjectKind::Table);
    let trigger_group = crate::catalog::hierarchy::group_node_id("pg", "shop", "public", ObjectKind::Trigger);
    expand_all(&mut model, &[conn, db, schema, group, trigger_group]);
    let names: Vec<String> = model.nav_rows().iter().map(|row| row.name.clone()).collect();
    assert!(names.contains(&"orders".to_owned()), "{names:?}");
    assert!(names.contains(&"orders_touch".to_owned()));

    // 复制限定名按引擎层级，不带连接展示名。
    let orders_id = nav_id("pg", "shop", "public", &orders);
    let _ = model.dispatch(&format!("nav-focus:{orders_id}"));
    let _ = model.dispatch("tree-copy-name");
    assert_eq!(model.copied, "public.orders");

    // 打开数据页用的是这一行的完整目标。
    let _ = model.dispatch(&format!("nav-open-data:{orders_id}"));
    let tab = model.active_tab().expect("表工作项");
    assert_eq!(tab.kind, WorkItemKind::Table);
    assert!(tab.spec.as_deref().unwrap_or_default().starts_with("pg|销售库|shop|public|orders|postgres"));
    assert_eq!(model.executions, 0, "打开数据不执行 SQL");

    // 关系图只画这一行的库/模式范围，不用别的同名对象。
    let _ = model.dispatch(&format!("nav-open-diagram:{orders_id}"));
    assert_eq!(model.diagram_spec.as_deref(), Some("pg|shop|public|销售库"));
    assert_eq!(model.shell.page, AppPage::Diagram);
    model.shell.page = AppPage::Workspace;

    // 对象菜单里的“新建查询”绑定这一行，草稿为空且不执行。
    let _ = model.dispatch("tree-new-query");
    let bound = model.active_tab().cloned().expect("绑定查询");
    assert_eq!(bound.kind, WorkItemKind::Query);
    assert_eq!(bound.connection_id.as_deref(), Some("pg"));
    assert_eq!(bound.database.as_deref(), Some("shop"));
    assert!(bound.draft.is_empty());
    assert_eq!(model.executions, 0);

    // 触发器没有行数据：不打开表页，说明原因。
    let trigger_id = nav_id("pg", "shop", "public", &trigger);
    let tabs = model.tabs.len();
    let _ = model.dispatch(&format!("nav-open-data:{trigger_id}"));
    assert_eq!(model.tabs.len(), tabs, "触发器不能误开数据页");
    assert_eq!(model.notice_key, Some("catalog.tree.noData"));
}

/// 编辑器里 Ctrl/⌘+点击对象：树上展开并选中它；表再打开结构页，没有结构页的对象只定位，
/// 树里没有的名字说明原因、不打开任何东西。
#[test]
fn go_to_definition_locates_in_the_tree_and_opens_the_structure_page() {
    use crate::catalog::model::{ObjectKind, ObjectRow};
    let orders = ObjectRow::table("orders", Some(4));
    let sequence = ObjectRow::fresh("orders_seq", ObjectKind::Sequence);
    let pg = nav_connection(Engine::Postgres, "pg", "销售库", vec![nav_database("shop", vec![orders.clone(), sequence.clone()])]);
    let mut model = WorkbenchModel::new(Shell::default());
    assert!(model.apply_navigation(&[pg], false));
    let orders_id = nav_id("pg", "shop", "public", &orders);
    let group = crate::catalog::hierarchy::group_node_id("pg", "shop", "public", ObjectKind::Table);
    assert!(!model.nav.is_expanded(&group), "分组一开始是收起的");

    let structures = |model: &WorkbenchModel| model.tabs.iter().filter(|tab| tab.kind == WorkItemKind::Structure).count();
    model.shell.sidebar_open = false;
    assert!(model.go_to_definition("pg", "shop", "public", "orders", "table", None));
    assert_eq!(model.nav.focus(), Some(orders_id.as_str()), "树上选中这张表");
    assert!(model.shell.sidebar_open, "侧栏收起时跳转定义要把它打开");
    assert_eq!(model.reveal_node.as_deref(), Some(orders_id.as_str()), "对象树下一帧把选中行滚进可见区");
    assert!(model.nav.is_expanded(&group), "展开到它所在的分组");
    assert!(model.nav_rows().iter().any(|row| row.id == orders_id), "表在可见行里");
    let tab = model.active_tab().cloned().expect("结构页");
    assert_eq!(tab.kind, WorkItemKind::Structure);
    assert!(tab.spec.as_deref().unwrap_or_default().starts_with("pg|销售库|shop|public|orders"), "{:?}", tab.spec);
    assert_eq!(model.executions, 0, "跳转定义不执行 SQL");

    // 序列没有结构页：只定位。
    let before = structures(&model);
    let sequence_id = nav_id("pg", "shop", "public", &sequence);
    assert!(model.go_to_definition("pg", "shop", "public", "orders_seq", "sequence", None));
    assert_eq!(model.nav.focus(), Some(sequence_id.as_str()));
    assert_eq!(structures(&model), before, "没有结构页的对象不开结构页");

    // 树里没有：说明原因，焦点和工作项都不动。
    let tabs = model.tabs.len();
    assert!(!model.go_to_definition("pg", "shop", "public", "nosuch", "table", None));
    assert_eq!(model.notice_key, Some("query.definition.notInTree"));
    assert_eq!(model.nav.focus(), Some(sequence_id.as_str()));
    assert_eq!(model.tabs.len(), tabs);
}

#[test]
fn insertions_change_the_draft_only_and_keep_the_selected_target() {
    use crate::catalog::model::ObjectRow;
    let orders = ObjectRow::table("orders", Some(1));
    let pg = nav_connection(Engine::Postgres, "pg", "销售库", vec![nav_database("shop", vec![orders.clone()])]);
    let mut model = WorkbenchModel::new(Shell::default());
    model.apply_navigation(&[pg], false);
    let orders_id = nav_id("pg", "shop", "public", &orders);

    // 没有查询项时插入会开一条带文本的草稿，不执行。
    let _ = model.dispatch(&format!("nav-insert-object:{orders_id}"));
    let tab = model.active_tab().cloned().expect("查询草稿");
    assert_eq!(tab.kind, WorkItemKind::Query);
    assert_eq!(tab.draft, "public.orders");
    assert_eq!(model.executions, 0);
    assert!(model.pending.is_empty());

    // 已有草稿时追加，不覆盖原内容。
    let _ = model.dispatch("new-query");
    let active = model.active_tab.clone().expect("活动查询项");
    if let Some(tab) = model.tabs.iter_mut().find(|tab| tab.id == active) {
        tab.draft = "select 1".into();
    }
    let tabs = model.tabs.len();
    let _ = model.dispatch(&format!("nav-insert-select:{orders_id}"));
    let tab = model.active_tab().cloned().expect("查询项");
    assert!(tab.draft.starts_with("select 1"), "{}", tab.draft);
    assert!(tab.draft.contains("FROM public.orders"));
    assert_eq!(model.tabs.len(), tabs, "插入不新建工作项");
    assert_eq!(model.executions, 0, "插入不执行");
}

#[test]
fn stale_objects_refuse_actions_and_keep_the_selectable_row_message() {
    use crate::catalog::model::ObjectRow;
    let orders = ObjectRow::table("orders", Some(1));
    let pg = nav_connection(Engine::Postgres, "pg", "销售库", vec![nav_database("shop", vec![orders.clone()])]);
    let mut model = WorkbenchModel::new(Shell::default());
    model.apply_navigation(&[pg], false);
    let orders_id = nav_id("pg", "shop", "public", &orders);
    let tabs = model.tabs.len();
    // 目录刷新两次后对象被移除；动作要说明目标已不在目录里。
    let empty = nav_connection(Engine::Postgres, "pg", "销售库", vec![nav_database("shop", Vec::new())]);
    model.apply_navigation(std::slice::from_ref(&empty), false);
    model.apply_navigation(&[empty], false);
    assert!(model.nav_node(&orders_id).is_none());
    let _ = model.dispatch(&format!("nav-open-data:{orders_id}"));
    assert_eq!(model.tabs.len(), tabs);
    assert_eq!(model.notice_key, Some("catalog.tree.staleObject"));
}

#[test]
fn object_search_keeps_the_tree_shape_and_clearing_restores_the_tree() {
    use crate::catalog::model::ObjectRow;
    let orders = ObjectRow::table("orders", Some(1));
    let people = ObjectRow::table("people", Some(1));
    let pg = nav_connection(Engine::Postgres, "pg", "销售库", vec![nav_database("shop", vec![orders, people])]);
    let mut model = WorkbenchModel::new(Shell::default());
    model.apply_navigation(&[pg], false);
    let conn = crate::catalog::hierarchy::connection_node_id("pg");
    let _ = model.dispatch(&format!("nav-toggle:{conn}"));
    let before = model.nav_rows().len();
    assert!(before > 1, "展开后有子行");

    model.set_field(DraftField::ObjectFilter, "ord".into());
    let rows = model.nav_rows();
    let hits: Vec<_> = rows.iter().filter(|row| row.highlight.is_some()).collect();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].label, "orders");
    assert!(rows.iter().any(|row| row.label == "shop"), "命中对象所在的库仍在树上");
    assert!(rows.iter().all(|row| row.label != "people"), "未命中的对象隐藏");
    assert!(!model.nav_rows_for("pg").is_empty(), "资源管理器在连接下画出命中的分支");
    assert_eq!(model.nav_empty_key("pg"), Some("ui.explorer.noMatch"));

    model.set_field(DraftField::ObjectFilter, String::new());
    assert_eq!(model.nav_rows().len(), before, "清除搜索后目录恢复");
    assert_eq!(model.nav_rows()[0].kind, crate::catalog::hierarchy::NavKind::Connection);
    assert_eq!(model.nav_rows()[1].kind, crate::catalog::hierarchy::NavKind::Database);
}

#[test]
fn keyboard_navigation_expands_moves_and_collapses_without_running_sql() {
    use crate::catalog::model::ObjectRow;
    let orders = ObjectRow::table("orders", Some(1));
    let pg = nav_connection(Engine::Postgres, "pg", "销售库", vec![nav_database("shop", vec![orders])]);
    let mut model = WorkbenchModel::new(Shell::default());
    model.apply_navigation(&[pg], false);
    model.nav.focus_first();
    let connection_id = crate::catalog::hierarchy::connection_node_id("pg");
    assert_eq!(model.nav.focus(), Some(connection_id.as_str()));
    assert_eq!(model.nav_key(NavKey::ArrowUp), None);
    assert_eq!(model.nav.focus(), Some(connection_id.as_str()));
    // 右方向键展开连接，露出下一层且不执行 SQL。
    assert!(model.nav_step(true).is_none());
    assert!(model.nav_rows().len() > 1);
    assert_eq!(model.nav_key(NavKey::ArrowDown), None);
    assert_eq!(
        model.nav.focus(),
        Some(crate::catalog::hierarchy::database_node_id("pg", "shop").as_str())
    );
    // 左方向键在已展开连接上折叠。
    model.nav.set_focus(&connection_id);
    assert!(model.nav_step(false).is_none());
    assert_eq!(model.nav_rows().len(), 1);
    assert_eq!(model.executions, 0);
}

#[test]
fn diagram_and_specialized_engines_use_their_own_scope() {
    use crate::catalog::model::{ObjectKind, ObjectRow};
    let users = ObjectRow::fresh("users", ObjectKind::Table);
    let mut mongo = crate::catalog::NavConnection::new("docs", "文档", Engine::Mongodb, crate::catalog::model::ConnStatus::Connected);
    mongo.databases = vec![crate::catalog::model::DatabaseNode {
        name: "app".to_owned(),
        schemas: vec![crate::catalog::model::SchemaNode {
            name: String::new(),
            objects: vec![users.clone()],
            total: 1,
            error: None,
            kind_reports: Vec::new(),
        }],
        error: None,
    }];
    let mut model = WorkbenchModel::new(Shell::default());
    model.apply_navigation(&[mongo], false);
    let conn = crate::catalog::hierarchy::connection_node_id("docs");
    let db = crate::catalog::hierarchy::database_node_id("docs", "app");
    let _ = model.dispatch(&format!("nav-toggle:{conn}"));
    let _ = model.dispatch(&format!("nav-toggle:{db}"));
    let collection = crate::catalog::hierarchy::object_node_id("docs", "app", "", &users);
    let _ = model.dispatch(&format!("nav-focus:{collection}"));
    let command = model.nav_activate_command(&collection).expect("文档入口");
    let _ = model.dispatch(&command);
    let tab = model.active_tab().expect("MongoDB 工作项");
    assert_eq!(tab.kind, WorkItemKind::Mongo);
    assert_eq!(tab.spec.as_deref(), Some("docs|文档|app|users"));
    assert_eq!(model.executions, 0);
}

/// 一条真实 SQLite 连接（items.sqlite：表 item 与列 id/label）的工作台，供目录树渲染测试共用。
fn sqlite_notes_workbench(
    cx: &mut gpui_kit::TestAppContext,
    boot: bool,
) -> (gpui_kit::AnyWindowHandle, gpui_kit::Entity<super::view::Workbench>, std::path::PathBuf) {
    sqlite_notes_workbench_sized(cx, boot, 1360., 800.)
}

/// 同 `sqlite_notes_workbench`，按指定窗口尺寸打开（窄宽要测真实的覆盖式侧栏）。
fn sqlite_notes_workbench_sized(
    cx: &mut gpui_kit::TestAppContext,
    boot: bool,
    width: f32,
    height: f32,
) -> (gpui_kit::AnyWindowHandle, gpui_kit::Entity<super::view::Workbench>, std::path::PathBuf) {
    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-shell-nav-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let database = dir.join("items.sqlite");
    let config_dir = dir.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("runtime");
        runtime.block_on(async move {
            let pool = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).create_if_missing(true))
                .await
                .unwrap();
            sqlx::query("CREATE TABLE item (id INTEGER PRIMARY KEY, label TEXT NOT NULL)")
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query("CREATE INDEX item_label ON item (label)").execute(&pool).await.unwrap();
            pool.close().await;
            let services = Arc::new(AppServices::new(Arc::new(Service::new_for_test(config_dir.join("connections.sqlite")))));
            services.connections.open_sqlite_for_test("notes", "笔记", &database).await.unwrap();
            let _ = tx.send(services);
        });
    });
    let services = rx.recv_timeout(std::time::Duration::from_secs(120)).expect("sqlite setup");
    let (window, view) = open_workbench_sized(cx, services, boot, width, height);
    (window, view, dir)
}

/// 两个 SQLite 连接：`notes` 已打开，`audit` 只保存没打开（同一份库文件），
/// 用来测「运行 → 按需连接 → 重放执行」的完整真实路径。
fn two_sqlite_connections(
    cx: &mut gpui_kit::TestAppContext,
) -> (gpui_kit::AnyWindowHandle, gpui_kit::Entity<super::view::Workbench>, std::path::PathBuf) {
    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-shell-two-conn-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let database = dir.join("items.sqlite");
    let config_dir = dir.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("runtime");
        runtime.block_on(async move {
            let pool = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).create_if_missing(true))
                .await
                .unwrap();
            sqlx::query("CREATE TABLE item (id INTEGER PRIMARY KEY, label TEXT NOT NULL)")
                .execute(&pool)
                .await
                .unwrap();
            pool.close().await;
            let services = Arc::new(AppServices::new(Arc::new(Service::new_for_test(config_dir.join("connections.sqlite")))));
            services.connections.open_sqlite_for_test("notes", "笔记", &database).await.unwrap();
            // 只保存，不打开：给按需连接留出一条真的能连上的连接。
            let mut draft = crate::connections::model::ConnectionDraft::blank(Engine::Sqlite);
            draft.id = Some("audit".to_owned());
            draft.name = "审计".to_owned();
            draft.file_path = database.display().to_string();
            services
                .connections
                .save(draft, crate::connections::model::SecretInput::default())
                .await
                .unwrap();
            let _ = tx.send(services);
        });
    });
    let services = rx.recv_timeout(std::time::Duration::from_secs(120)).expect("sqlite setup");
    let (window, view) = open_workbench_sized(cx, services, true, 1360., 800.);
    (window, view, dir)
}

/// 启动后的外壳要同时读到连接列表和目录对象，资源管理器才会真的画出行。
fn wait_for_shell_ready(
    cx: &mut gpui_kit::TestAppContext,
    window: gpui_kit::AnyWindowHandle,
    view: &gpui_kit::Entity<super::view::Workbench>,
) {
    let started = std::time::Instant::now();
    while started.elapsed() < std::time::Duration::from_secs(30) {
        cx.run_until_parked();
        let _ = cx.update_window(window, |_, window, cx| window.render_frame(cx));
        let ready = cx.update(|cx| {
            let view = view.read(cx);
            !view.model.connections.rows.is_empty()
                && view
                    .model
                    .nav
                    .nodes()
                    .any(|node| node.kind == crate::catalog::hierarchy::NavKind::Object && node.name == "item")
        });
        if ready {
            return;
        }
        std::thread::park_timeout(std::time::Duration::from_millis(25));
    }
    panic!("外壳没有在超时内读到连接与对象");
}

/// 目录树必须从真实会话读出层级，并在展开表时按需读到列和索引。
#[gpui_kit::test]
fn explorer_tree_reads_real_levels_and_loads_columns_on_demand(cx: &mut gpui_kit::TestAppContext) {
    let (window, view, dir) = sqlite_notes_workbench(cx, false);
    cx.update_window(window, |_, _, cx| {
        view.update(cx, |view, cx| view.perform("refresh-tree", cx));
    })
    .unwrap();

    let object_id = wait_for_nav_object(cx, window, &view, "item");
    assert!(!object_id.is_empty(), "目录树没有读到表节点");
    cx.update_window(window, |_, _, cx| {
        view.update(cx, |view, cx| view.perform(&format!("nav-toggle:{object_id}"), cx));
    })
    .unwrap();
    assert!(wait_for_nav_columns(cx, window, &view, &object_id), "展开表后没有读到列");

    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            use crate::catalog::hierarchy::NavKind;
            let columns: Vec<String> = view
                .model
                .nav
                .nodes()
                .filter(|node| node.kind == NavKind::Column)
                .map(|node| node.name.clone())
                .collect();
            assert_eq!(columns, vec!["id", "label"], "{columns:?}");
            let groups: Vec<NavKind> = view
                .model
                .nav
                .nodes()
                .filter(|node| matches!(node.kind, NavKind::ColumnGroup | NavKind::IndexGroup))
                .map(|node| node.kind)
                .collect();
            assert!(groups.contains(&NavKind::ColumnGroup) && groups.contains(&NavKind::IndexGroup), "{groups:?}");
            // 表展开后列/索引分组出现在树上，再展开才列出列。
            let column_group = crate::catalog::hierarchy::column_group_id(&object_id);
            // 展开对象会连祖先一起展开，列分组才可见。
            view.model.nav.expand(&object_id);
            view.perform(&format!("nav-toggle:{column_group}"), cx);
            assert!(view
                .model
                .nav_rows()
                .iter()
                .any(|row| row.kind == NavKind::Column && row.name == "label"));
            // 插入列名只改草稿，不执行 SQL。
            let column_id = view
                .model
                .nav_rows()
                .into_iter()
                .find(|row| row.kind == crate::catalog::hierarchy::NavKind::Column && row.name == "label")
                .map(|row| row.id)
                .expect("label 列");
            view.perform(&format!("nav-insert-column:{object_id}\u{1}label"), cx);
            let _ = column_id;
            let tab = view.model.active_tab().cloned().expect("查询草稿");
            assert!(tab.draft.contains("label"), "{}", tab.draft);
            assert!(!view.query_executed(cx), "插入不执行");
        });
        window.render_frame(cx);
    })
    .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

fn wait_for_nav_object(
    cx: &mut gpui_kit::TestAppContext,
    window: gpui_kit::AnyWindowHandle,
    view: &gpui_kit::Entity<super::view::Workbench>,
    name: &str,
) -> String {
    let started = std::time::Instant::now();
    while started.elapsed() < std::time::Duration::from_secs(30) {
        cx.run_until_parked();
        let _ = cx.update_window(window, |_, window, cx| window.render_frame(cx));
        let found = cx.update(|cx| {
            view.read(cx)
                .model
                .nav
                .nodes()
                .find(|node| node.kind == crate::catalog::hierarchy::NavKind::Object && node.name == name)
                .map(|node| node.id.clone())
        });
        if let Some(id) = found {
            let _ = cx.update_window(window, |_, _, cx| {
                view.update(cx, |view, cx| {
                    let connection = crate::catalog::hierarchy::connection_node_id("notes");
                    view.perform(&format!("nav-toggle:{connection}"), cx);
                });
            });
            return id;
        }
        std::thread::park_timeout(std::time::Duration::from_millis(25));
    }
    String::new()
}

fn wait_for_nav_columns(
    cx: &mut gpui_kit::TestAppContext,
    window: gpui_kit::AnyWindowHandle,
    view: &gpui_kit::Entity<super::view::Workbench>,
    object_id: &str,
) -> bool {
    let started = std::time::Instant::now();
    while started.elapsed() < std::time::Duration::from_secs(30) {
        cx.run_until_parked();
        let _ = cx.update_window(window, |_, window, cx| window.render_frame(cx));
        let ready = cx.update(|cx| {
            view.read(cx)
                .model
                .nav
                .node(object_id)
                .map(|node| !node.children.is_empty() && !node.loading)
                .unwrap_or(false)
        });
        if ready {
            return true;
        }
        std::thread::park_timeout(std::time::Duration::from_millis(25));
    }
    false
}


// ------------------------------------------------------------ T2 修复回归

/// 资源管理器的鼠标路径必须真的把树展开：chevron 单击、双击、键盘读同一份状态。
#[gpui_kit::test]
fn sidebar_mouse_paths_expand_and_collapse_the_real_tree(cx: &mut gpui_kit::TestAppContext) {
    use crate::catalog::hierarchy::{connection_node_id, NavKind};
    use crate::catalog::model::ObjectKind;
    use gpui_kit::SharedString;

    let (window, view, dir) = sqlite_notes_workbench(cx, true);
    let connection = connection_node_id("notes");
    wait_for_shell_ready(cx, window, &view);
    // 测试用的 SQLite 连接已经打开；列表状态同步由启动任务负责，这里显式对齐。
    cx.update(|cx| {
        view.update(cx, |view, _| {
            view.model
                .connections
                .status
                .insert("notes".to_owned(), crate::app::views::connections::LinkStatus::Connected);
        });
    });
    let object_id = cx.update(|cx| {
        view.read(cx)
            .model
            .nav
            .nodes()
            .find(|node| node.kind == NavKind::Object && node.name == "item")
            .map(|node| node.id.clone())
            .expect("表对象节点")
    });

    // 先让启动路径的连接刷新跑完，再把连接折叠回去：之后资源管理器不该有任何子行。
    cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
    cx.update(|cx| {
        view.update(cx, |view, _| {
            view.model.nav.collapse(&connection);
            assert!(view.model.nav_rows_for("notes").is_empty());
        });
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("side-notes").expanded(), Some(false), "连接行默认折叠");
        assert!(window.try_find("sidebar").is_some());
    })
    .unwrap();

    // chevron 单击＝展开（不触发行点击，也不打开浏览器）。
    cx.update_window(window, |_, window, cx| {
        window.click("nav-chevron-notes", cx);
        window.render_frame(cx);
    })
    .unwrap();
    let rows = cx.update(|cx| view.read(cx).model.nav_rows_for("notes"));
    assert!(!rows.is_empty(), "chevron 单击没有在树上展开连接");
    assert!(cx.update(|cx| view.read(cx).model.nav_connection_expanded("notes")));
    let group = rows
        .iter()
        .find(|row| row.kind == NavKind::Group && row.object_kind == Some(ObjectKind::Table))
        .map(|row| row.id.clone())
        .expect("表分组行");
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("side-notes").expanded(), Some(true));
        // 渲染路径真的画出了连接下的行，不只是模型里有。
        assert!(window.try_find(SharedString::from(format!("nav-{group}"))).is_some(), "展开后没有渲染分组行");
    })
    .unwrap();

    // 键盘左键折叠同一份状态，chevron 再展开回来。
    cx.update(|cx| {
        view.update(cx, |view, _| {
            view.model.nav.set_focus(&connection);
            assert!(view.model.nav_step(false).is_none());
            assert!(view.model.nav_rows_for("notes").is_empty());
        });
    });
    cx.update_window(window, |_, window, cx| {
        window.click("nav-chevron-notes", cx);
        window.render_frame(cx);
        assert_eq!(window.find("side-notes").expanded(), Some(true));
        // 双击连接行折叠，再双击展开。
        window.double_click("side-notes", cx);
        window.render_frame(cx);
        assert_eq!(window.find("side-notes").expanded(), Some(false), "双击已展开的连接应折叠");
        window.double_click("side-notes", cx);
        window.render_frame(cx);
        assert_eq!(window.find("side-notes").expanded(), Some(true));
    })
    .unwrap();

    // 对象种类分组的 chevron 单击就能展开，表对象行真的出现在渲染结果里。
    cx.update_window(window, |_, window, cx| {
        window.click(SharedString::from(format!("nav-chevron-{group}")), cx);
        window.render_frame(cx);
    })
    .unwrap();
    let rows = cx.update(|cx| view.read(cx).model.nav_rows_for("notes"));
    assert!(
        rows.iter().any(|row| row.kind == NavKind::Object && row.name == "item"),
        "双击分组后应看到表对象：{rows:?}"
    );
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find(SharedString::from(format!("nav-{object_id}"))).is_some(),
            "对象行只出现在模型里，没有走到渲染路径"
        );
    })
    .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// 对象菜单的插入入口和导航命令一样，先收回编辑器草稿再追加。
#[gpui_kit::test]
fn object_menu_insertions_capture_the_live_editor_draft(cx: &mut gpui_kit::TestAppContext) {
    let (window, view, dir) = sqlite_notes_workbench(cx, true);
    wait_for_shell_ready(cx, window, &view);
    let object_id = cx.update(|cx| {
        view.read(cx)
            .model
            .nav
            .nodes()
            .find(|node| node.kind == crate::catalog::hierarchy::NavKind::Object && node.name == "item")
            .map(|node| node.id.clone())
            .expect("表对象节点")
    });

    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.perform("new-query", cx));
        window.render_frame(cx);
        assert!(view.read(cx).query_editor_focused(window, cx), "新查询的编辑器应有焦点");
    })
    .unwrap();
    // 刚输入的文字还在编辑器里，模型的草稿仍是旧内容。
    cx.update_window(window, |_, window, cx| window.input("select 1", cx)).unwrap();
    cx.run_until_parked();

    cx.update_window(window, |_, _, cx| {
        view.update(cx, |view, cx| {
            assert!(view.model.open_nav_menu(&object_id), "对象菜单");
            view.perform("tree-insert-select", cx);
        });
    })
    .unwrap();
    let draft = cx.update(|cx| view.read(cx).model.active_tab().cloned().expect("查询项").draft);
    assert!(draft.starts_with("select 1"), "菜单插入覆盖了刚输入的内容：{draft}");
    assert!(draft.contains("SELECT *"), "{draft}");
    assert!(!cx.update(|cx| view.read(cx).query_executed(cx)), "插入不执行");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 非对象行（库/模式/分组）的菜单和动作只属于这一行。
#[test]
fn non_object_rows_never_borrow_another_object() {
    use crate::catalog::model::{ObjectKind, ObjectRow};
    let orders = ObjectRow::table("orders", Some(2));
    let pg = nav_connection(Engine::Postgres, "pg", "销售库", vec![nav_database("shop", vec![orders.clone()])]);
    let mut model = WorkbenchModel::new(Shell::default());
    model.apply_navigation(&[pg], false);
    let schema = crate::catalog::hierarchy::schema_node_id("pg", "shop", "public");
    let group = crate::catalog::hierarchy::group_node_id("pg", "shop", "public", ObjectKind::Table);
    let orders_id = nav_id("pg", "shop", "public", &orders);

    // 先让上一张表成为焦点和 object_spec，再看范围行菜单。
    let _ = model.dispatch(&format!("nav-focus:{orders_id}"));
    assert!(model.object_spec.is_some());

    assert!(model.open_nav_menu(&schema));
    let ids: Vec<&str> = model.menu.as_ref().expect("菜单").items.iter().map(|item| item.id).collect();
    assert_eq!(
        ids,
        vec!["tree-new-query", "tree-copy-name", "tree-refresh-node", "tree-create-table", "compare-object", "open-admin"],
        "范围行只有自己的动作，外加新建表、对比库和管理页"
    );
    assert!(!ids.iter().any(|id| matches!(*id, "tree-open-data" | "tree-insert-select" | "tree-drop-object")));
    let tabs = model.tabs.len();
    let _ = model.dispatch("tree-open-data");
    assert_eq!(model.tabs.len(), tabs, "范围行不能打开上一张表");
    assert_eq!(model.notice_key, Some("catalog.tree.actionUnavailable"));
    assert_eq!(model.notice_detail, "public");

    // 分组行同样不借对象动作，复制名字也只复制这一行。
    assert!(model.open_nav_menu(&group));
    let _ = model.dispatch("tree-insert-select");
    assert!(!model.active_tab().is_some_and(|tab| tab.kind == WorkItemKind::Query && !tab.draft.is_empty()));
    assert_eq!(model.notice_key, Some("catalog.tree.actionUnavailable"));
    assert!(model.open_nav_menu(&schema));
    let _ = model.dispatch("tree-copy-name");
    assert_eq!(model.copied, "public");

    // 数据库行给出绑定这一行的空白查询，不写 SQL、不执行。
    let database = crate::catalog::hierarchy::database_node_id("pg", "shop");
    assert!(model.open_nav_menu(&database));
    let _ = model.dispatch("tree-new-query");
    let tab = model.active_tab().cloned().expect("绑定查询");
    assert_eq!(tab.kind, WorkItemKind::Query);
    assert_eq!(tab.database.as_deref(), Some("shop"));
    assert!(tab.draft.is_empty());
    assert_eq!(model.executions, 0);

    // 对象行仍然拿到完整的对象菜单。
    assert!(model.open_nav_menu(&orders_id));
    assert!(crate::app::shell::menus::flatten_items(&model.menu.as_ref().unwrap().items).iter().any(|item| item.id == "tree-select-sql"));
    let _ = model.dispatch("tree-copy-name");
    assert_eq!(model.copied, "public.orders");
}

/// 资源树 Ctrl/⌘ 多选：只有关系型表能选；正好两张时“对比表”可用，三张时禁用并说明原因；
/// 普通点击清空多选。表菜单没有“对比库”，库菜单仍有；Redis、Mongo 节点没有“对比表”。
#[test]
fn tree_multi_selection_drives_compare_tables() {
    use crate::catalog::model::{ConnStatus, DatabaseNode, ObjectKind, ObjectRow, SchemaNode};
    let orders = ObjectRow::table("orders", Some(2));
    let customers = ObjectRow::table("customers", Some(2));
    let items = ObjectRow::table("items", Some(2));
    let view = ObjectRow::fresh("v_orders", ObjectKind::View);
    let pg = nav_connection(Engine::Postgres, "pg", "销售库", vec![nav_database("shop", vec![orders.clone(), customers.clone(), items.clone(), view.clone()])]);
    let users = ObjectRow::fresh("users", ObjectKind::Table);
    let mut mongo = crate::catalog::NavConnection::new("docs", "文档", Engine::Mongodb, ConnStatus::Connected);
    mongo.databases = vec![DatabaseNode {
        name: "app".to_owned(),
        schemas: vec![SchemaNode { name: String::new(), objects: vec![users.clone()], total: 1, error: None, kind_reports: Vec::new() }],
        error: None,
    }];
    let redis = nav_connection(Engine::Redis, "rd", "缓存", vec![nav_database("0", Vec::new())]);
    let mut model = WorkbenchModel::new(Shell::default());
    model.apply_navigation(&[pg, mongo, redis], false);
    let orders_id = nav_id("pg", "shop", "public", &orders);
    let customers_id = nav_id("pg", "shop", "public", &customers);
    let items_id = nav_id("pg", "shop", "public", &items);
    let view_id = nav_id("pg", "shop", "public", &view);
    let collection = crate::catalog::hierarchy::object_node_id("docs", "app", "", &users);
    let entry = |model: &WorkbenchModel, id: &str| {
        crate::app::shell::menus::flatten_items(&model.menu.as_ref().expect("菜单").items).into_iter().find(|item| item.id == id).cloned()
    };

    // 单表右键：“对比表…”，没有“对比库”。
    assert!(model.open_nav_menu(&orders_id));
    assert!(entry(&model, "compare-table").is_some_and(|item| item.enabled && item.label_key == "catalog.tree.compareTable"));
    assert!(entry(&model, "compare-object").is_none(), "表菜单没有对比库");
    assert!(entry(&model, "compare-tables").is_none());

    // Ctrl/⌘ 点击两张表：“对比表”可用，目标按选中先后排列。
    let _ = model.dispatch(&format!("nav-select-toggle:{orders_id}"));
    let _ = model.dispatch(&format!("nav-select-toggle:{customers_id}"));
    assert_eq!(model.nav.selection(), vec![orders_id.clone(), customers_id.clone()]);
    assert!(model.nav.is_selected(&orders_id) && !model.nav.is_selected(&items_id));
    expand_all(
        &mut model,
        &[
            crate::catalog::hierarchy::connection_node_id("pg"),
            crate::catalog::hierarchy::database_node_id("pg", "shop"),
            crate::catalog::hierarchy::schema_node_id("pg", "shop", "public"),
            crate::catalog::hierarchy::group_node_id("pg", "shop", "public", ObjectKind::Table),
        ],
    );
    let rows = model.nav.visible();
    assert!(rows.iter().any(|row| row.id == orders_id && row.selected), "选中的表在树上标出");
    assert!(rows.iter().any(|row| row.id == items_id && !row.selected));
    assert!(model.open_nav_menu(&customers_id));
    let pair = entry(&model, "compare-tables").expect("对比表");
    assert!(pair.enabled, "{pair:?}");
    assert!(entry(&model, "compare-table").is_none() && entry(&model, "compare-object").is_none());
    let targets = model.selected_table_targets();
    assert_eq!(targets.iter().map(|target| target.name.as_str()).collect::<Vec<_>>(), vec!["orders", "customers"]);

    // 视图、Mongo 集合、Redis 键空间不能多选。
    let _ = model.dispatch(&format!("nav-select-toggle:{view_id}"));
    let _ = model.dispatch(&format!("nav-select-toggle:{collection}"));
    assert_eq!(model.nav.selection().len(), 2, "只有关系型表能多选");

    // 三张：禁用并说明原因。
    let _ = model.dispatch(&format!("nav-select-toggle:{items_id}"));
    assert!(model.open_nav_menu(&items_id));
    let pair = entry(&model, "compare-tables").expect("对比表");
    assert!(!pair.enabled && pair.reason_key == Some("catalog.tree.compareNeedsTwo"), "{pair:?}");
    // 再点一次取消选中，回到两张。
    let _ = model.dispatch(&format!("nav-select-toggle:{items_id}"));
    assert_eq!(model.nav.selection().len(), 2);

    // 普通点击清空多选。
    let _ = model.dispatch("nav-select-clear");
    assert!(model.nav.selection().is_empty());
    assert!(model.nav.visible().iter().all(|row| !row.selected));

    // 库菜单仍有“对比库”。
    let database = crate::catalog::hierarchy::database_node_id("pg", "shop");
    assert!(model.open_nav_menu(&database));
    assert!(entry(&model, "compare-object").is_some_and(|item| item.enabled));

    // Redis、Mongo 节点不显示“对比表”。
    let keyspace = model
        .nav
        .nodes()
        .find(|node| node.connection_id == "rd" && node.kind != crate::catalog::hierarchy::NavKind::Connection)
        .map(|node| node.id.clone())
        .expect("Redis 节点");
    for node in [collection.clone(), keyspace, crate::catalog::hierarchy::connection_node_id("rd")] {
        assert!(model.open_nav_menu(&node), "{node}");
        for id in ["compare-table", "compare-tables"] {
            assert!(entry(&model, id).is_none(), "{node} {id}");
        }
    }
}

/// 先点过一张表，再右键另一个库选“对比库”：对比带的是被右键的库，不是之前那张表。
#[gpui_kit::test]
fn compare_object_from_a_database_menu_uses_that_database_not_the_last_table(cx: &mut gpui_kit::TestAppContext) {
    let dir = std::env::temp_dir().join(format!("rillsql-shell-compare-scope-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench(cx, services_at(&dir), false);
    let orders = crate::catalog::model::ObjectRow::table("orders", Some(2));
    let pg = nav_connection(Engine::Postgres, "pg", "销售库", vec![nav_database("shop", vec![orders.clone()])]);
    let other = nav_connection(Engine::Postgres, "wh", "仓库", vec![nav_database("stock", Vec::new())]);
    let orders_id = nav_id("pg", "shop", "public", &orders);
    let stock = crate::catalog::hierarchy::database_node_id("wh", "stock");
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.model.apply_navigation(&[pg, other], false);
            // 先在表上右键（同步了 object_spec），然后关掉菜单。
            assert!(view.model.open_nav_menu(&orders_id));
            assert_eq!(view.model.object_target().map(|target| target.name), Some("orders".to_owned()));
            view.model.close_menu();
            // 再右键另一个连接里的库，选“对比库”。
            assert!(view.model.open_nav_menu(&stock));
            view.perform("compare-object", cx);
        });
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.model.shell.page, AppPage::Compare);
        let (context, note, _baseline, _target) = view.compare_state(cx);
        let context = context.unwrap_or_default();
        assert!(context.starts_with("stock") && context.contains("仓库"), "对比库必须带被右键的库：{context} / {note}");
        assert!(!context.contains("orders") && !context.contains("销售库"), "不能借用之前点过的表：{context}");
    });
    let _ = std::fs::remove_dir_all(&dir);
}

/// MongoDB 集合是文档：菜单和打开入口都不走关系型表/结构/关系图。
#[test]
fn document_objects_use_documents_not_relational_pages() {
    use crate::catalog::model::{ConnStatus, DatabaseNode, ObjectKind, ObjectRow, SchemaNode};
    let users = ObjectRow::fresh("users", ObjectKind::Table);
    let mut mongo = crate::catalog::NavConnection::new("docs", "文档", Engine::Mongodb, ConnStatus::Connected);
    mongo.databases = vec![DatabaseNode {
        name: "app".to_owned(),
        schemas: vec![SchemaNode {
            name: String::new(),
            objects: vec![users.clone()],
            total: 1,
            error: None,
            kind_reports: Vec::new(),
        }],
        error: None,
    }];
    let mut model = WorkbenchModel::new(Shell::default());
    model.apply_navigation(&[mongo], false);
    let collection = crate::catalog::hierarchy::object_node_id("docs", "app", "", &users);

    assert!(model.open_nav_menu(&collection));
    let items: Vec<_> = crate::app::shell::menus::flatten_items(&model.menu.as_ref().expect("菜单").items).into_iter().cloned().collect();
    let open_data = items.iter().find(|item| item.id == "tree-open-data").expect("数据入口");
    assert!(open_data.enabled, "集合仍然可以打开文档");
    let structure = items.iter().find(|item| item.id == "open-structure").expect("结构入口");
    assert!(!structure.enabled);
    assert_eq!(structure.reason_key, Some("catalog.tree.noStructure"));
    let diagram = items.iter().find(|item| item.id == "open-diagram").expect("关系图入口");
    assert!(!diagram.enabled);
    assert_eq!(diagram.reason_key, Some("diagram.unsupported"));

    // 打开数据走文档页，规格是集合身份而不是关系型表。
    let _ = model.dispatch("tree-open-data");
    let tab = model.active_tab().cloned().expect("MongoDB 工作项");
    assert_eq!(tab.kind, WorkItemKind::Mongo);
    assert_eq!(tab.spec.as_deref(), Some("docs|文档|app|users"));

    // 结构命令说明没有 SQL 结构，不开结构页。
    let structures = model.tabs.iter().filter(|tab| tab.kind == WorkItemKind::Structure).count();
    let _ = model.dispatch(&format!("nav-open-structure:{collection}"));
    assert_eq!(model.tabs.iter().filter(|tab| tab.kind == WorkItemKind::Structure).count(), structures);
    assert_eq!(model.notice_key, Some("catalog.tree.noStructure"));

    // 关系图同样拒绝，不用上一次的表画图。
    let _ = model.dispatch(&format!("nav-open-diagram:{collection}"));
    assert_eq!(model.notice_key, Some("diagram.unsupported"));
    assert_eq!(model.shell.page, AppPage::Workspace);

    // 导入导出与对比只适用于关系型对象。
    model.exchange_open = false;
    let _ = model.dispatch("exchange-object");
    assert!(!model.exchange_open);
    assert_eq!(model.notice_key, Some("catalog.tree.relationalOnly"));
}

/// 连接级目录读取失败：原因写在这条连接下面，重试只重读这一条。
#[gpui_kit::test]
fn connection_catalog_failure_shows_reason_and_retries_at_that_node(cx: &mut gpui_kit::TestAppContext) {
    use crate::app::shell::nav::NavRequest;
    use crate::app::views::connections::LinkStatus;
    use crate::catalog::model::ConnStatus;

    let (window, view, dir) = sqlite_notes_workbench(cx, true);
    wait_for_shell_ready(cx, window, &view);
    let connection = crate::catalog::hierarchy::connection_node_id("notes");
    cx.update(|cx| {
        view.update(cx, |view, _| {
            let mut failed = crate::catalog::NavConnection::new("notes", "笔记", Engine::Sqlite, ConnStatus::Connected);
            failed.error = Some("读取库表失败：数据库被锁定".to_owned());
            view.model.apply_navigation(&[failed], false);
            view.model.connections.status.insert("notes".to_owned(), LinkStatus::Connected);
            view.model.expand_nav_connection("notes");
        });
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("nav-conn-error-notes").visible(), "连接级失败没有留下原因行");
        let text = window
            .find("nav-conn-error-text-notes")
            .label()
            .map(str::to_owned)
            .unwrap_or_default();
        assert!(text.contains("数据库被锁定"), "原因没有显示：{text:?}");
        // 重试入口就在这条连接下面，标签是“重试”。
        let retry = window.find("nav-retry-conn-notes");
        assert!(retry.visible(), "连接级失败没有重试入口");
        assert_eq!(retry.label(), Some("重试"));
    })
    .unwrap();
    // 按钮走的是 act -> perform；这里直接走同一条命令，断言请求只落在这条连接上。
    cx.update(|cx| {
        view.update(cx, |view, _| {
            assert!(view.model.nav.node(&connection).is_some(), "连接节点还在树上");
            let _ = view.model.dispatch(&format!("nav-retry:{connection}"));
            let queued = view.model.nav_requests.back().cloned();
            assert_eq!(
                queued,
                Some(NavRequest::Refresh {
                    connection: Some("notes".to_owned())
                }),
                "重试没有落在这一条连接上"
            );
        });
    });
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn continuation_and_retry_resolve_to_the_storage_scope() {
    use crate::app::shell::nav::NavRequest;
    use crate::catalog::model::{DatabaseNode, ObjectKind, ObjectRow, SchemaNode};
    let objects = vec![ObjectRow::table("a", Some(1))];
    let pg = nav_connection(
        Engine::Postgres,
        "pg",
        "销售库",
        vec![DatabaseNode {
            name: "shop".to_owned(),
            schemas: vec![SchemaNode {
                name: "public".to_owned(),
                total: 3,
                objects,
                error: None,
                kind_reports: Vec::new(),
            }],
            error: None,
        }],
    );
    let mut model = WorkbenchModel::new(Shell::default());
    model.apply_navigation(&[pg], false);
    let group = crate::catalog::hierarchy::group_node_id("pg", "shop", "public", ObjectKind::Table);
    let schema = crate::catalog::hierarchy::schema_node_id("pg", "shop", "public");
    // 分组上的“读取更多”按所属模式续读，偏移量才和整页一致。
    let _ = model.dispatch(&format!("nav-more:{group}"));
    assert_eq!(model.take_nav_request(), Some(NavRequest::More(schema.clone())));

    // 种类分组读取失败后的重试只重读那一类，加载态也只落在这个分组。
    model.nav.set_error(&group, "函数读取失败");
    let _ = model.dispatch(&format!("nav-retry:{group}"));
    assert_eq!(model.take_nav_request(), Some(NavRequest::Group(group.clone())));
    assert!(model.nav.node(&group).unwrap().loading, "重试期间只有这个分组在加载");
    let _ = schema;

    // 成功回来只清这个分组的错误并补上对象；失败则把原因留在分组上。
    assert!(model.nav.set_kind_page(&group, &[ObjectRow::table("b", Some(1))], 4, None));
    let node = model.nav.node(&group).unwrap();
    assert!(node.error.is_none(), "重试成功后分组不能再留着旧错误");
    assert!(!node.loading);
    assert_eq!(node.total, 4);
    assert!(model.nav.node(&crate::catalog::hierarchy::object_node_id("pg", "shop", "public", &ObjectRow::table("b", Some(1)))).is_some());

    model.nav.begin_group(&group);
    assert!(!model.nav.set_kind_page(&group, &[], 0, Some("权限不足".to_owned())));
    let node = model.nav.node(&group).unwrap();
    assert_eq!(node.error.as_deref(), Some("权限不足"));
    assert!(!node.loading);
    assert!(node.children.len() > 1, "重试失败不能清掉已读对象");
}

fn sqlite_exec_on_file(path: &std::path::Path, sql: &'static str) {
    let path = path.to_path_buf();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async move {
            let pool = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(
                    sqlx::sqlite::SqliteConnectOptions::new()
                        .filename(&path)
                        .busy_timeout(std::time::Duration::from_secs(10)),
                )
                .await
                .expect("connect");
            sqlx::query(sql).execute(&pool).await.expect("execute");
            pool.close().await;
        });
    })
    .join()
    .expect("sqlite thread");
}

/// 分组重试必须真的发出一次读取：队列里的 Group 请求经 pump_nav 走到
/// spawn_nav_group 后读到新对象，并把这个分组的加载态/错误落下，兄弟分组不动。
#[gpui_kit::test]
fn queued_group_retry_dispatches_a_read_and_resolves_loading(cx: &mut gpui_kit::TestAppContext) {
    use crate::catalog::hierarchy::NavKind;
    use crate::catalog::model::ObjectKind;

    let (window, view, dir) = sqlite_notes_workbench(cx, true);
    wait_for_shell_ready(cx, window, &view);
    let database = dir.join("items.sqlite");

    // 启动刷新可能还在路上：先让排队的读取跑完，分组状态才稳定。
    let started = std::time::Instant::now();
    while started.elapsed() < std::time::Duration::from_secs(10) {
        cx.run_until_parked();
        let _ = cx.update_window(window, |_, window, cx| window.render_frame(cx));
        let settled = cx.update(|cx| view.read(cx).model.nav_requests.is_empty());
        if settled {
            break;
        }
        std::thread::park_timeout(std::time::Duration::from_millis(10));
    }

    // 表分组：真实 SQLite 连接里的 item 已经在这一类下面。
    let group = cx.update(|cx| {
        let tree = &view.read(cx).model.nav;
        let group = tree
            .nodes()
            .find(|node| node.kind == NavKind::Group && node.object_kind == Some(ObjectKind::Table))
            .map(|node| node.id.clone())
            .expect("表分组");
        let node = tree.node(&group).expect("分组节点");
        assert!(!node.children.is_empty(), "表分组里应有已读对象");
        assert!(!node.loading);
        group
    });

    // 兄弟分组的状态快照：重试只碰目标分组。
    let siblings: Vec<(String, usize, bool, Option<String>, usize)> = cx.update(|cx| {
        view.read(cx)
            .model
            .nav
            .nodes()
            .filter(|node| node.kind == NavKind::Group && node.id != group)
            .map(|node| (node.id.clone(), node.children.len(), node.loading, node.error.clone(), node.total))
            .collect()
    });

    // 数据库里多一张表：只有真的发了读取请求，重试后才能看到它。
    sqlite_exec_on_file(&database, "CREATE TABLE audit (id INTEGER PRIMARY KEY)");

    cx.update(|cx| {
        view.update(cx, |view, _| {
            view.model.nav.set_error(&group, "读取表失败");
            assert_eq!(view.model.nav.node(&group).and_then(|node| node.error.clone()).as_deref(), Some("读取表失败"));
        });
    });

    // 走和界面按钮同一条命令：排队 Group 请求，把加载态落在这个分组上。
    cx.update_window(window, |_, _, cx| {
        view.update(cx, |view, cx| view.perform(&format!("nav-retry:{group}"), cx));
    })
    .unwrap();
    let loading = cx.update(|cx| view.read(cx).model.nav.node(&group).expect("分组节点").loading);
    assert!(loading, "重试开始应只在这个分组上显示加载态");

    // 渲染循环驱动 pump_nav；读取没被发起的话加载态永远不会落下。
    let started = std::time::Instant::now();
    let mut resolved = false;
    while started.elapsed() < std::time::Duration::from_secs(30) {
        cx.run_until_parked();
        let _ = cx.update_window(window, |_, window, cx| window.render_frame(cx));
        resolved = cx.update(|cx| {
            view.read(cx)
                .model
                .nav
                .node(&group)
                .map(|node| !node.loading)
                .unwrap_or(false)
        });
        if resolved {
            break;
        }
        std::thread::park_timeout(std::time::Duration::from_millis(20));
    }
    assert!(resolved, "排队的 Group 重试没有发出读取请求，分组一直停在加载态");

    cx.update(|cx| {
        let tree = &view.read(cx).model.nav;
        let node = tree.node(&group).expect("分组节点");
        assert!(node.error.is_none(), "重试成功后分组不能留着旧原因：{:?}", node.error);
        assert!(!node.loading);
        assert!(
            node.children
                .iter()
                .filter_map(|id| tree.node(id))
                .any(|child| child.name == "audit"),
            "重试没有真的读到数据库里的新表"
        );
        assert!(
            node.children
                .iter()
                .filter_map(|id| tree.node(id))
                .any(|child| child.name == "item"),
            "重试不能清掉已读对象"
        );
        for (id, children, loading, error, total) in &siblings {
            let sibling = tree.node(id).expect("兄弟分组还在树上");
            assert_eq!(&sibling.children.len(), children, "兄弟分组 {id} 的子节点被动了");
            assert_eq!(&sibling.loading, loading, "兄弟分组 {id} 的加载态被动了");
            assert_eq!(&sibling.error, error, "兄弟分组 {id} 的原因被动了");
            assert_eq!(&sibling.total, total, "兄弟分组 {id} 的总数被动了");
        }
    });
    let _ = std::fs::remove_dir_all(&dir);
}



// ------------------------------------------------------------ 显示矩阵与焦点（T7）

/// 1360/900/640 x 900：顶栏、活动栏、工作区与状态栏互不重叠，主操作、关闭与返回都在视口内。
#[gpui_kit::test]
fn display_matrix_keeps_primary_controls_visible_and_unoverlapped(cx: &mut gpui_kit::TestAppContext) {
    // 后台读取跑在进程级 tokio runtime 上，上一例的收尾任务可能在渲染期间回写；
    // 这里和 sqlite_notes_workbench 一样允许真实等待，避免把别人的尾活算成本例的不确定性。
    cx.executor().allow_parking();
    for (width, height) in [(1360., 900.), (900., 900.), (640., 900.)] {
        let dir = std::env::temp_dir().join(format!("rillsql-shell-matrix-{}-{}", std::process::id(), width as i32));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (window, view) = open_workbench_sized(cx, services_at(&dir), false, width, height);
        let tab_close = cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            view.update(cx, |view, cx| view.perform("new-query", cx));
            window.render_frame(cx);
            let root = window.find("workbench").bounds();
            assert!((f32::from(root.size.width) - width).abs() < 1., "{width}: workbench {}", f32::from(root.size.width));
            assert!((f32::from(root.size.height) - height).abs() < 1.);

            let rect = |id: &str| {
                let el = window.find(gpui_kit::SharedString::from(id.to_owned()));
                assert!(el.visible(), "{width}: {id} 应可见");
                el.bounds()
            };
            let inside = |label: &str, b: gpui_kit::Bounds<gpui_kit::Pixels>| {
                let left = f32::from(b.origin.x);
                let top = f32::from(b.origin.y);
                let right = left + f32::from(b.size.width);
                let bottom = top + f32::from(b.size.height);
                assert!(left >= -0.5 && top >= -0.5, "{width}: {label} 左上越界 {left},{top}");
                assert!(right <= width + 0.5, "{width}: {label} 右侧越界 {right}");
                assert!(bottom <= height + 0.5, "{width}: {label} 下方越界 {bottom}");
            };

            for id in ["toolbar", "activity", "workbench-content", "status", "top-new-query"] {
                inside(id, rect(id));
            }
            let toolbar = rect("toolbar");
            let activity = rect("activity");
            let content = rect("workbench-content");
            let status = rect("status");
            // 上中下四段只相接不重叠：控件不会被另一段盖住。
            assert!(f32::from(toolbar.origin.y + toolbar.size.height) <= f32::from(activity.origin.y) + 0.5, "{width}");
            assert!(f32::from(activity.origin.x + activity.size.width) <= f32::from(content.origin.x) + 0.5, "{width}");
            assert!(f32::from(content.origin.x + content.size.width) <= width + 0.5, "{width}");
            assert!(f32::from(content.origin.y + content.size.height) <= f32::from(status.origin.y) + 0.5, "{width}");
            assert!(f32::from(content.size.width) >= 160., "{width}: 工作区被活动栏/侧栏挤到 {}", f32::from(content.size.width));
            assert!(f32::from(activity.size.width) >= 52. - 0.5, "{width}: 活动栏保持完整宽度");

            let tab = view.read(cx).model.tabs.last().expect("查询标签").id.clone();
            format!("tab-close-{tab}")
        })
        .unwrap();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let el = window.find(gpui_kit::SharedString::from(tab_close.clone()));
            assert!(el.visible(), "{width}: 标签关闭按钮应可达");
            let b = el.bounds();
            assert!(f32::from(b.origin.x + b.size.width) <= width + 0.5);
            assert!(f32::from(b.origin.y + b.size.height) <= height + 0.5);
        })
        .unwrap();

        // 查询页：执行、连接选择、结果表格和消息页签在各宽度都可见、不重叠；
        // 右侧面板在 1360/900 紧靠编辑器，在 640 默认收起，打开再关上后布局回到原样。
        cx.update(|cx| {
            view.update(cx, |view, cx| {
                view.plant_query_result(vec!["id".to_owned(), "label".to_owned()], vec![vec!["1".to_owned(), "a".to_owned()]], cx);
            })
        });
        type Rect = gpui_kit::Bounds<gpui_kit::Pixels>;
        let check = |window: &mut gpui_kit::Window, id: &str| -> Rect {
            let el = window.find(gpui_kit::SharedString::from(id.to_owned()));
            assert!(el.visible(), "{width}: {id} 应可见");
            let b = el.bounds();
            let (left, top) = (f32::from(b.origin.x), f32::from(b.origin.y));
            let (right, bottom) = (left + f32::from(b.size.width), top + f32::from(b.size.height));
            assert!(left >= -0.5 && top >= -0.5, "{width}: {id} 左上越界 {left},{top}");
            assert!(right <= width + 0.5 && bottom <= height + 0.5, "{width}: {id} 越界 {right},{bottom}");
            b
        };
        let apart = |label: &str, a: Rect, b: Rect| {
            let overlap_x = f32::from(a.origin.x) + 0.5 < f32::from(b.origin.x + b.size.width) && f32::from(b.origin.x) + 0.5 < f32::from(a.origin.x + a.size.width);
            let overlap_y = f32::from(a.origin.y) + 0.5 < f32::from(b.origin.y + b.size.height) && f32::from(b.origin.y) + 0.5 < f32::from(a.origin.y + a.size.height);
            assert!(!(overlap_x && overlap_y), "{width}: {label} 重叠 {a:?} / {b:?}");
        };
        let narrow = width < 800.;
        let (editor_before, grid_before) = cx
            .update_window(window, |_, window, cx| {
                window.render_frame(cx);
                let run = check(window, "query-act-execute-query");
                let picker = check(window, "query-connection");
                let grid = check(window, "query-grid");
                let log = check(window, "query-log-tab");
                let editor = check(window, "query-editor");
                apart("执行/连接选择", run, picker);
                apart("执行/结果表格", run, grid);
                apart("连接选择/结果表格", picker, grid);
                apart("消息页签/结果表格", log, grid);
                apart("消息页签/执行", log, run);
                if narrow {
                    assert!(window.try_find("query-side-panel").is_none(), "{width}: 窄窗口里右侧面板默认收起");
                } else {
                    let side = check(window, "query-side-panel");
                    apart("右侧面板/编辑器", side, editor);
                    apart("右侧面板/结果表格", side, grid);
                    apart("右侧面板/连接选择", side, picker);
                    assert!(f32::from(side.origin.x) + 0.5 >= f32::from(editor.origin.x + editor.size.width), "{width}: 右侧面板在编辑器右边");
                }
                // 消息页签在任何宽度都能点开。
                window.click("query-log-tab", cx);
                window.render_frame(cx);
                check(window, "query-log");
                window.click("query-results-tab", cx);
                window.render_frame(cx);
                check(window, "query-grid");
                (editor, check(window, "query-grid"))
            })
            .unwrap();
        if narrow {
            cx.update_window(window, |_, window, cx| {
                window.click("query-side-toggle", cx);
                window.render_frame(cx);
                let side = check(window, "query-side-panel");
                // 打开时是浮层：编辑器与结果区不被挤开，执行与连接选择仍可见。
                assert_eq!(check(window, "query-editor"), editor_before, "{width}: 打开浮层不改变编辑器布局");
                let grid = check(window, "query-grid");
                apart("右侧浮层/结果表格", side, grid);
                apart("右侧浮层/执行", side, check(window, "query-act-execute-query"));
                apart("右侧浮层/连接选择", side, check(window, "query-connection"));
                window.click("query-side-close", cx);
                window.render_frame(cx);
                assert!(window.try_find("query-side-panel").is_none(), "{width}: 关上后收起");
                assert_eq!(check(window, "query-editor"), editor_before, "{width}: 关上后编辑器回到原布局");
                assert_eq!(check(window, "query-grid"), grid_before, "{width}: 关上后结果表格回到原布局");
            })
            .unwrap();
        }

        // “返回”与浮层关闭按钮同样在各宽度可达。
        cx.update(|cx| view.update(cx, |view, cx| view.perform("compare", cx)));
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let back = window.find("compare-back");
            assert!(back.visible(), "{width}: 返回按钮应可见");
            let b = back.bounds();
            assert!(f32::from(b.origin.x + b.size.width) <= width + 0.5);
            assert!(f32::from(b.origin.y + b.size.height) <= height + 0.5);
        })
        .unwrap();
        cx.update(|cx| view.update(cx, |view, cx| view.perform("help", cx)));
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let panel = window.find("palette").bounds();
            assert!(f32::from(panel.origin.x) >= 0. && f32::from(panel.origin.x + panel.size.width) <= width + 0.5);
            let close = window.find("help-close");
            assert!(close.visible(), "{width}: 关闭按钮应可见");
            let b = close.bounds();
            assert!(f32::from(b.origin.x + b.size.width) <= width + 0.5);
            assert!(f32::from(b.origin.y + b.size.height) <= height + 0.5);
        })
        .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// 语言、主题和密度切换后，已创建的输入框、对象树和命令面板立即跟着变，不需要重开窗口。
#[gpui_kit::test]
fn locale_theme_and_density_refresh_existing_inputs_tree_and_palette(cx: &mut gpui_kit::TestAppContext) {
    use crate::catalog::hierarchy::{database_node_id, group_node_id, schema_node_id};
    use crate::catalog::model::{ObjectKind, ObjectRow};
    use gpui_kit::component::ActiveTheme as _;

    // 同上：进程级 runtime 上的收尾任务不该被当成本例的不确定性。
    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-shell-refresh-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench_sized(cx, services_at(&dir), false, 900., 900.);
    let group = group_node_id("pg", "shop", "public", ObjectKind::Table);
    cx.update(|cx| {
        view.update(cx, |view, cx| {
            let orders = ObjectRow::table("orders", Some(1));
            let connection = nav_connection(Engine::Postgres, "pg", "销售库", vec![nav_database("shop", vec![orders])]);
            // 对象树只属于工作台页：先切页面，再挂上假连接，免得页面切换把树刷掉。
            view.perform("activity-workspace", cx);
            assert!(view.model.shell.sidebar_open, "工作台页要有资源管理器");
            // 资源管理器只在保存过的连接且连接成功时画树：假连接也要走同一条入口。
            view.model.connections.rows.push(ConnectionRecord {
                saved: SavedConnection {
                    id: "pg".into(),
                    name: "销售库".into(),
                    engine: Engine::Postgres,
                    env: "dev".into(),
                    host: Some("localhost".into()),
                    port: Some(5432),
                    database: Some("shop".into()),
                    username: Some("reader".into()),
                    file_path: None,
                    replica_set: None,
                    tls: None,
                    ssh: None,
                    password_saved: false,
                    ssh_password_saved: false,
                },
                ssh_host_key: None,
            });
            view.model.apply_navigation(&[connection], false);
            view.model
                .connections
                .status
                .insert("pg".to_owned(), crate::app::views::connections::LinkStatus::Connected);
            // 连接行自己先展开，库/模式按用户点击的同一批命令展开。
            view.model.expand_nav_connection("pg");
            for id in [database_node_id("pg", "shop"), schema_node_id("pg", "shop", "public")] {
                assert!(
                    view.model.dispatch(&format!("nav-toggle:{id}")).is_some(),
                    "{id} 没有接受展开命令"
                );
            }
            assert!(view.model.nav.node(&group).is_some(), "种类分组应在树上");
            cx.notify();
        });
    });
    let (zh_label, zh_placeholder, zh_height) = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let row = window.find(gpui_kit::SharedString::from(format!("nav-{group}")));
            (
                row.label().map(str::to_owned),
                window.find("field-ObjectFilter").label().map(str::to_owned),
                f32::from(row.bounds().size.height),
            )
        })
        .unwrap();
    assert!(zh_label.as_deref().is_some_and(|text| !text.is_empty() && text != "catalog.kind.table"), "分组标签要显示当前语言：{zh_label:?}");
    assert!(zh_placeholder.as_deref().is_some_and(|text| !text.is_empty()), "筛选框要有当前语言的可读名称");

    cx.update(|cx| view.update(cx, |view, cx| view.perform("locale-en", cx)));
    let (en_label, en_placeholder) = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            (
                window.find(gpui_kit::SharedString::from(format!("nav-{group}"))).label().map(str::to_owned),
                window.find("field-ObjectFilter").label().map(str::to_owned),
            )
        })
        .unwrap();
    assert_ne!(zh_label, en_label, "切换语言后已展开的树标签必须重画");
    assert_ne!(zh_placeholder, en_placeholder, "已创建输入框的占位文案必须更新");
    assert!(
        en_label
            .as_deref()
            .is_some_and(|text| !text.chars().any(|ch| ('\u{4e00}'..='\u{9fff}').contains(&ch))),
        "英文树标签残留中文：{en_label:?}"
    );

    // 命令面板打开后切语言，已有面板条目也立即换文案。
    cx.update(|cx| view.update(cx, |view, cx| view.perform("command-palette", cx)));
    let (palette_en, dark_before) = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            (window.find("pal-0").label().map(str::to_owned), cx.theme().is_dark())
        })
        .unwrap();
    cx.update(|cx| view.update(cx, |view, cx| view.perform("locale-zh", cx)));
    let palette_zh = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.find("pal-0").label().map(str::to_owned)
        })
        .unwrap();
    assert_ne!(palette_en, palette_zh, "已打开的命令面板条目要跟着语言更新");
    assert!(!dark_before);
    cx.update(|cx| {
        view.update(cx, |view, cx| {
            view.model.shell.close_overlay();
            view.perform("theme-dark", cx);
        })
    });
    let dark = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            cx.theme().is_dark()
        })
        .unwrap();
    assert!(dark, "theme-dark 要立即作用到已渲染界面");
    // 默认紧凑（规范 S9）；切到标准后已有树行变高（树 23 -> 30）。
    cx.update(|cx| view.update(cx, |view, cx| view.perform("density-standard", cx)));
    let standard_height = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            f32::from(window.find(gpui_kit::SharedString::from(format!("nav-{group}"))).bounds().size.height)
        })
        .unwrap();
    assert!(standard_height > zh_height, "密度切换要改变已有树行高度：{zh_height} -> {standard_height}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 查询页新增的消息页、右侧面板和结果表格里的记录视图，切换语言、主题和密度后立即跟着变。
#[gpui_kit::test]
fn query_log_side_panel_and_record_view_follow_locale_theme_and_density(cx: &mut gpui_kit::TestAppContext) {
    use crate::query::desk::StatementView;
    use crate::ui::kit::Palette;
    use gpui_kit::component::ActiveTheme as _;

    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-shell-query-refresh-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench_sized(cx, services_at(&dir), false, 1360., 900.);
    let statement = |sql: &str| StatementView {
        sql: sql.to_owned(),
        sent_sql: None,
        rewrite: None,
        rewrite_anchor: None,
        status: "ok".to_owned(),
        message: String::new(),
        columns: vec!["id".to_owned(), "label".to_owned()],
        rows: vec![vec![Some("1".to_owned()), Some("a".to_owned())], vec![Some("2".to_owned()), Some("b".to_owned())]],
        truncated: false,
        affected: None,
        elapsed_ms: Some(7),
        error_at: None,
        error_code: None,
        range: None,
        error_mark: None,
    };
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        view.update(cx, |view, cx| view.perform("new-query", cx));
        window.render_frame(cx);
        let query = view.read(cx).query.clone();
        query.update(cx, |pane, cx| pane.plant_run(vec![statement("SELECT id, label FROM item")], cx));
        window.render_frame(cx);
        // 记录视图在结果表格里打开；切到消息页再切回来它还开着。
        query.update(cx, |pane, cx| {
            let _ = pane.grid_command("grid-record-toggle", cx);
        });
        window.render_frame(cx);
    })
    .unwrap();

    #[derive(Debug, PartialEq)]
    struct Seen {
        log: String,
        log_entry: String,
        log_row_h: f32,
        side: String,
        side_mode: String,
        side_mode_h: f32,
        record: String,
        colors: Option<(gpui_kit::Hsla, gpui_kit::Hsla)>,
        palette: (gpui_kit::Hsla, gpui_kit::Hsla),
        dark: bool,
    }
    let seen = |cx: &mut gpui_kit::TestAppContext| -> Seen {
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("query-log-tab", cx);
            window.render_frame(cx);
            let label = |window: &mut gpui_kit::Window, id: &str| {
                window.find(gpui_kit::SharedString::from(id.to_owned())).label().unwrap_or_default().to_owned()
            };
            let log = label(window, "query-log");
            let log_entry = label(window, "query-log-0");
            let log_row_h = f32::from(window.find("query-log-0").bounds().size.height);
            let side = label(window, "query-side-panel");
            let side_mode = label(window, "query-side-mode-outline");
            let side_mode_h = f32::from(window.find("query-side-mode-outline").bounds().size.height);
            window.click("query-results-tab", cx);
            window.render_frame(cx);
            // 记录视图自己的按钮（关闭、下一行）带当前语言的可读名称。
            let record = format!("{} / {}", label(window, "grid-record-close"), label(window, "grid-record-next"));
            let palette = Palette::of(cx);
            let colors = view.read(cx).query.read(cx).drawn_colors();
            Seen { log, log_entry, log_row_h, side, side_mode, side_mode_h, record, colors, palette: (palette.surface, palette.chrome), dark: cx.theme().is_dark() }
        })
        .unwrap()
    };
    let cjk = |text: &str| text.chars().any(|ch| ('\u{4e00}'..='\u{9fff}').contains(&ch));

    let zh = seen(cx);
    assert!(cjk(&zh.log) && cjk(&zh.log_entry) && cjk(&zh.side_mode) && cjk(&zh.record), "初始是中文：{zh:?}");
    assert_eq!(zh.colors, Some(zh.palette), "消息页/右侧面板用当前主题的底色");
    assert!(!zh.dark);

    cx.update(|cx| view.update(cx, |view, cx| view.perform("locale-en", cx)));
    let en = seen(cx);
    for (name, before, after) in [
        ("消息页", &zh.log, &en.log),
        ("消息记录", &zh.log_entry, &en.log_entry),
        ("右侧面板", &zh.side, &en.side),
        ("右侧分段", &zh.side_mode, &en.side_mode),
        ("记录视图", &zh.record, &en.record),
    ] {
        assert_ne!(before, after, "{name} 切换语言后要立即重画");
        assert!(!cjk(after), "{name} 英文里残留中文：{after}");
    }
    assert!(en.log_entry.contains("Succeeded") && en.log_entry.contains("2 rows"), "{}", en.log_entry);

    cx.update(|cx| view.update(cx, |view, cx| view.perform("theme-dark", cx)));
    let dark = seen(cx);
    assert!(dark.dark, "主题要切到深色");
    assert_ne!(dark.palette, en.palette);
    assert_eq!(dark.colors, Some(dark.palette), "消息页/右侧面板立即换成深色底色");
    assert_eq!(dark.record, en.record, "记录视图在主题切换后仍在");

    // 默认是紧凑密度（规范 S9）；切到标准后行高与控件高都变大（标准 行 32 / 控件 30）。
    assert_eq!(dark.side_mode_h, crate::ui::kit::Metrics::compact().control_h, "默认紧凑：分段控件取紧凑控件高");
    cx.update(|cx| view.update(cx, |view, cx| view.perform("density-standard", cx)));
    let standard = seen(cx);
    assert!(standard.log_row_h > dark.log_row_h, "密度切换要改变消息行高：{} -> {}", dark.log_row_h, standard.log_row_h);
    assert!(standard.side_mode_h > dark.side_mode_h, "密度切换要改变右侧分段控件高度：{} -> {}", dark.side_mode_h, standard.side_mode_h);
    assert_eq!(standard.side_mode_h, crate::ui::kit::Metrics::standard().control_h, "分段控件取标准控件高");
    assert_eq!(standard.record, en.record, "记录视图在密度切换后仍在");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Windows 主线程默认只有 1 MB 栈；未优化构建里搭元素树的函数帧很大，查询页（工具条、右侧面板、
/// 消息页）和结果表格（含记录视图）曾经在调试版启动或打开记录视图时撑爆主线程栈。
/// 这里在同样 1 MB 的线程上把这些区域都画一遍：任何一处帧重新膨胀，进程就会栈溢出而失败。
#[test]
fn query_workspace_renders_within_the_windows_main_thread_stack() {
    const WINDOWS_MAIN_STACK: usize = 1024 * 1024;
    let worker = std::thread::Builder::new()
        .name("query-workspace-stack".to_owned())
        .stack_size(WINDOWS_MAIN_STACK)
        .spawn(|| {
            gpui::run_test_once(
                0,
                Box::new(|dispatcher| {
                    let mut cx = gpui_kit::TestAppContext::build(dispatcher, Some("query_workspace_renders_within_the_windows_main_thread_stack"));
                    let drawn = query_workspace_scenario(&mut cx);
                    cx.run_until_parked();
                    cx.update(|cx| {
                        cx.background_executor().forbid_parking();
                        cx.quit();
                    });
                    cx.run_until_parked();
                    drawn
                }),
            )
        })
        .expect("spawn 1 MB render thread");
    let drawn = worker.join().expect("查询页在 1 MB 栈上绘制不应失败");
    assert_eq!(drawn, [true; 6], "工具条、右侧大纲/对象、结果表格、记录视图和消息页都要真正画出来");
}

/// 打开查询页、放入结果、打开记录视图，再依次画右侧大纲、对象信息和消息页。
/// 返回各区域是否真正画出：(工具条, 大纲, 对象, 表格, 记录视图, 消息页)。
fn query_workspace_scenario(cx: &mut gpui_kit::TestAppContext) -> [bool; 6] {
    use crate::query::desk::StatementView;

    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-shell-query-stack-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench_sized(cx, services_at(&dir), false, 1360., 900.);
    let result = StatementView {
        sql: "SELECT id, label FROM item".to_owned(),
        sent_sql: None,
        rewrite: None,
        rewrite_anchor: None,
        status: "ok".to_owned(),
        message: String::new(),
        columns: vec!["id".to_owned(), "label".to_owned()],
        rows: vec![vec![Some("1".to_owned()), Some("a".to_owned())], vec![Some("2".to_owned()), Some("b".to_owned())]],
        truncated: false,
        affected: None,
        elapsed_ms: Some(3),
        error_at: None,
        error_code: None,
        range: None,
        error_mark: None,
    };
    let drawn = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            view.update(cx, |view, cx| view.perform("new-query", cx));
            window.render_frame(cx);
            let query = view.read(cx).query.clone();
            query.update(cx, |pane, cx| pane.plant_run(vec![result], cx));
            query.update(cx, |pane, cx| {
                let _ = pane.grid_command("grid-record-toggle", cx);
            });
            window.render_frame(cx);
            let toolbar = window.find("query-act-execute-query").visible() && window.find("query-connection").visible();
            let outline = window.find("query-outline").visible();
            let grid = window.find("query-grid").visible();
            let record = window.find("grid-record-close").visible();
            query.update(cx, |pane, cx| pane.command("side-mode:object", cx));
            window.render_frame(cx);
            let object = window.find("query-side-object").visible();
            window.click("query-log-tab", cx);
            window.render_frame(cx);
            let log = window.find("query-log").visible();
            [toolbar, outline, object, grid, record, log]
        })
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    drawn
}

/// 后台读取不挡其他页面：切换页面与输入照常，旧代次结果只被丢弃，最新上下文才写回。
#[test]
fn background_reads_keep_navigation_usable_and_latest_context_wins() {
    let mut model = WorkbenchModel::new(Shell::default());
    let _ = model.dispatch("new-query");
    let tab = model.tabs.last_mut().expect("查询项");
    tab.running = true;
    let tasks = crate::application::tasks::TaskBoard::new();
    let first = tasks.begin("query");
    // 读取还没回来时，用户仍能切页面、改筛选和切工作项。
    assert!(model.dispatch("settings").is_some());
    assert_eq!(model.shell.page, AppPage::Settings);
    model.set_field(DraftField::ObjectFilter, "订单".to_owned());
    assert_eq!(model.field_value(DraftField::ObjectFilter), "订单");
    assert!(model.dispatch("history").is_some());
    assert_eq!(model.shell.page, AppPage::History);
    // 用户连续切换上下文：前一个读取应过期，只有最新一次允许写回。
    tasks.supersede("query");
    let second = tasks.begin("query");
    assert!(matches!(
        tasks.accept("query", first, ()),
        crate::application::tasks::TaskOutcome::Stale { .. }
    ));
    assert!(matches!(
        tasks.accept("query", second, ()),
        crate::application::tasks::TaskOutcome::Current { .. }
    ));
    assert!(model.tabs.iter().any(|tab| tab.running), "运行态本身不阻塞导航");
}


// ------------------------------------------------------------ 对象树 DDL / 刷新 / 模板 / 危险操作（T2）

fn pg_saved(id: &str, env: &str) -> ConnectionRecord {
    ConnectionRecord {
        saved: SavedConnection {
            id: id.into(),
            name: "销售库".into(),
            engine: Engine::Postgres,
            env: env.into(),
            host: Some("localhost".into()),
            port: Some(5432),
            database: Some("shop".into()),
            username: Some("reader".into()),
            file_path: None,
            replica_set: None,
            tls: None,
            ssh: None,
            password_saved: false,
            ssh_password_saved: false,
        },
        ssh_host_key: None,
    }
}

fn pg_orders_model() -> (WorkbenchModel, String) {
    use crate::catalog::model::ObjectRow;
    let orders = ObjectRow::table("orders", Some(2));
    let pg = nav_connection(Engine::Postgres, "pg", "销售库", vec![nav_database("shop", vec![orders.clone(), ObjectRow::view("recent")])]);
    let mut model = WorkbenchModel::new(Shell::default());
    model.apply_navigation(&[pg], false);
    (model, nav_id("pg", "shop", "public", &orders))
}

fn order_columns() -> Vec<crate::catalog::hierarchy::ColumnInfo> {
    use crate::catalog::hierarchy::ColumnInfo;
    vec![
        ColumnInfo { name: "id".into(), data_type: "integer".into(), nullable: false, primary: true },
        ColumnInfo { name: "label".into(), data_type: "text".into(), nullable: true, primary: false },
    ]
}

/// 模板只写草稿：没有查询项时新开一个绑定这个对象连接/库的查询；列没读过就先读再插。
#[test]
fn sql_templates_insert_into_a_bound_draft_and_never_run() {
    use crate::app::shell::nav::NavRequest;
    let (mut model, orders) = pg_orders_model();
    assert!(model.tabs.is_empty());

    // 列还没读：先排队读列，插入等列回来。
    assert!(model.open_nav_menu(&orders));
    let _ = model.dispatch("tree-template-update");
    assert_eq!(model.take_nav_request(), Some(NavRequest::Details(orders.clone())));
    assert!(model.tabs.is_empty(), "列没回来之前不插入");
    model.nav_details_loaded(&orders, Ok(order_columns()), Ok(Vec::new()));
    let tab = model.active_tab().cloned().expect("新开的查询项");
    assert_eq!(tab.kind, WorkItemKind::Query);
    assert_eq!(tab.connection_id.as_deref(), Some("pg"));
    assert_eq!(tab.database.as_deref(), Some("shop"));
    assert_eq!(tab.draft, "UPDATE public.orders\nSET label = ?\nWHERE id = ?;");
    assert_eq!(model.executions, 0, "模板不执行");
    assert!(tab.last_executed_sql.is_none());

    // 已有查询项：列已读，直接追加到草稿，不新开。
    let tabs = model.tabs.len();
    assert!(model.open_nav_menu(&orders));
    let _ = model.dispatch("tree-template-insert");
    assert!(model.take_nav_request().is_none(), "列已读过不再读");
    assert_eq!(model.tabs.len(), tabs);
    let draft = model.active_tab().unwrap().draft.clone();
    assert!(draft.starts_with("UPDATE"), "{draft}");
    assert!(draft.ends_with("INSERT INTO public.orders (id, label)\nVALUES (?, ?);"), "{draft}");
    assert!(model.open_nav_menu(&orders));
    let _ = model.dispatch("tree-template-delete");
    assert!(model.active_tab().unwrap().draft.ends_with("DELETE FROM public.orders\nWHERE id = ?;"));
    assert_eq!(model.executions, 0);

    // 读列失败：说明原因，不插入。
    let (mut failing, orders) = pg_orders_model();
    assert!(failing.open_nav_menu(&orders));
    let _ = failing.dispatch("tree-template-insert");
    failing.nav_details_loaded(&orders, Err("权限不足".into()), Err("权限不足".into()));
    assert!(failing.tabs.is_empty());
    assert_eq!(failing.notice_key, Some("catalog.tree.templateNoColumns"));
    assert_eq!(failing.notice_detail, "权限不足");

    // 范围行不给模板：作用不到别的对象。
    let schema = crate::catalog::hierarchy::schema_node_id("pg", "shop", "public");
    assert!(model.open_nav_menu(&schema));
    assert!(model.menu.as_ref().unwrap().items.iter().all(|item| !item.id.starts_with("tree-template-")));
}

/// 刷新此节点：展开保持；读取失败留下原子项并把原因挂在节点上；分组整类读完时缺失对象先 stale 再移除。
#[test]
fn refreshing_a_node_keeps_expansion_children_and_marks_removed_objects() {
    use crate::app::shell::nav::NavRequest;
    use crate::catalog::hierarchy::{connection_node_id, database_node_id, group_node_id, schema_node_id};
    use crate::catalog::model::{ObjectKind, ObjectRow};
    let (mut model, orders) = pg_orders_model();
    let schema = schema_node_id("pg", "shop", "public");
    let tables = group_node_id("pg", "shop", "public", ObjectKind::Table);
    let views = group_node_id("pg", "shop", "public", ObjectKind::View);
    expand_all(&mut model, &[connection_node_id("pg"), database_node_id("pg", "shop"), schema.clone(), tables.clone(), views.clone()]);
    assert!(model.nav.is_expanded(&tables) && model.nav.is_expanded(&views));

    // 模式：从第一页重读，成功后两个分组仍展开，新对象补上。
    assert!(model.open_nav_menu(&schema));
    assert!(model.menu.as_ref().unwrap().items.iter().any(|item| item.id == "tree-refresh-node"));
    let _ = model.dispatch("tree-refresh-node");
    assert_eq!(model.take_nav_request(), Some(NavRequest::Reload(schema.clone())));
    assert!(model.nav.node(&schema).unwrap().loading);
    let audit = ObjectRow::table("audit", Some(0));
    let fresh = vec![ObjectRow::table("orders", Some(2)), audit.clone(), ObjectRow::view("recent")];
    model.nav.reload_scope(&schema, &fresh, 3, None, &[]);
    assert!(model.nav.is_expanded(&tables) && model.nav.is_expanded(&views), "刷新后展开保持");
    assert!(model.nav.node(&nav_id("pg", "shop", "public", &audit)).is_some());
    assert!(!model.nav.node(&schema).unwrap().loading);

    // 读取失败：原子项都在，原因挂在模式节点上。
    let children_before = model.nav.node(&tables).unwrap().children.clone();
    let _ = model.dispatch(&format!("nav-refresh-node:{schema}"));
    assert_eq!(model.take_nav_request(), Some(NavRequest::Reload(schema.clone())));
    model.nav.reload_scope(&schema, &[], 0, Some("连接中断".into()), &[]);
    assert_eq!(model.nav.node(&schema).unwrap().error.as_deref(), Some("连接中断"));
    assert_eq!(model.nav.node(&tables).unwrap().children, children_before, "失败不清空子树");
    assert!(model.nav.is_expanded(&tables) && model.nav.is_expanded(&views));

    // 种类分组：整类读完时没读到的对象先 stale，再刷新一次才移除。
    assert!(model.open_nav_menu(&tables));
    let _ = model.dispatch("tree-refresh-node");
    assert_eq!(model.take_nav_request(), Some(NavRequest::Group(tables.clone())));
    model.nav.set_kind_page(&tables, std::slice::from_ref(&audit), 1, None);
    assert!(model.nav.node(&orders).unwrap().stale, "第一次缺失先标 stale");
    assert!(model.nav.is_expanded(&tables));
    let _ = model.dispatch(&format!("nav-refresh-node:{tables}"));
    assert_eq!(model.take_nav_request(), Some(NavRequest::Group(tables.clone())));
    model.nav.set_kind_page(&tables, std::slice::from_ref(&audit), 1, None);
    assert!(model.nav.node(&orders).is_none(), "第二次仍缺失就移除");
    // 只读到一页（总数更大）时不判断删除。
    let _ = model.dispatch(&format!("nav-refresh-node:{tables}"));
    let _ = model.take_nav_request();
    model.nav.set_kind_page(&tables, &[], 5, None);
    assert!(!model.nav.node(&nav_id("pg", "shop", "public", &audit)).unwrap().stale);

    // 连接行：整条连接重读。
    let connection = connection_node_id("pg");
    let _ = model.dispatch(&format!("nav-refresh-node:{connection}"));
    assert_eq!(model.take_nav_request(), Some(NavRequest::Refresh { connection: Some("pg".into()) }));
}

/// 模板插在草稿光标处，前后补空行分隔；有选区时插在选区开头且不删选中的文本；
/// 光标不属于当前查询项时追加到末尾；插入对象名仍然追加到末尾，不受光标影响。
#[test]
fn sql_templates_insert_at_the_draft_cursor() {
    let (mut model, orders) = pg_orders_model();
    model.nav_details_loaded(&orders, Ok(order_columns()), Ok(Vec::new()));
    let _ = model.dispatch("new-query");
    let tab_id = model.active_tab().expect("查询项").id.clone();
    let delete = "DELETE FROM public.orders\nWHERE id = ?;";
    let set_draft = |model: &mut WorkbenchModel, text: &str| {
        model.tabs.iter_mut().find(|tab| tab.id == tab_id).unwrap().draft = text.to_owned();
    };
    let template = |model: &mut WorkbenchModel| {
        assert!(model.open_nav_menu(&orders));
        let _ = model.dispatch("tree-template-delete");
        model.active_tab().unwrap().draft.clone()
    };

    // 光标在两条语句之间：插在那里，前后各留一个空行。
    set_draft(&mut model, "SELECT 1;\nSELECT 2;");
    model.set_draft_cursor(&tab_id, "SELECT 1;\n".len(), "SELECT 1;\n".len());
    assert_eq!(template(&mut model), format!("SELECT 1;\n\n{delete}\n\nSELECT 2;"));
    assert_eq!(model.executions, 0, "模板不执行");

    // 选中了 “SELECT 2;”：插在选区开头，选中的文本原样保留。
    set_draft(&mut model, "SELECT 1;\nSELECT 2;");
    model.set_draft_cursor(&tab_id, "SELECT 1;\n".len(), "SELECT 1;\nSELECT 2;".len());
    assert_eq!(template(&mut model), format!("SELECT 1;\n\n{delete}\n\nSELECT 2;"));

    // 光标在开头：前面不加空行；已有的空行算数，不重复补。
    set_draft(&mut model, "\n\nSELECT 2;");
    model.set_draft_cursor(&tab_id, 0, 0);
    assert_eq!(template(&mut model), format!("{delete}\n\nSELECT 2;"));

    // 光标落在多字节字符中间：退到字符开头，不切坏文本。
    set_draft(&mut model, "-- 订单");
    model.set_draft_cursor(&tab_id, 4, 4);
    assert_eq!(template(&mut model), format!("-- \n\n{delete}\n\n订单"));

    // 连续插入：接在上一次插入的文本后面。
    set_draft(&mut model, "SELECT 1;");
    model.set_draft_cursor(&tab_id, 0, 0);
    let _ = template(&mut model);
    assert_eq!(template(&mut model), format!("{delete}\n\n{delete}\n\nSELECT 1;"));

    // 光标属于别的查询项：不按它插入，追加到当前草稿末尾。
    set_draft(&mut model, "SELECT 1;");
    model.set_draft_cursor("another-tab", 0, 0);
    assert_eq!(template(&mut model), format!("SELECT 1;\n\n{delete}"));

    // 插入对象名保持原来的追加行为，不看光标。
    set_draft(&mut model, "SELECT 1;");
    model.set_draft_cursor(&tab_id, 0, 0);
    assert!(model.open_nav_menu(&orders));
    let _ = model.dispatch("tree-insert-object");
    let draft = model.active_tab().unwrap().draft.clone();
    assert!(draft.starts_with("SELECT 1;"), "{draft}");
    assert!(draft.contains("orders"), "{draft}");
}

/// 种类读取失败：那一类已有的对象原样保留（不标 stale、再刷一次也不移除），原因挂在分组上；
/// 库/模式刷新和整树刷新都一样。
#[test]
fn a_failed_object_kind_keeps_its_children_on_scope_and_full_refresh() {
    use crate::app::shell::nav::NavRequest;
    use crate::catalog::hierarchy::{connection_node_id, database_node_id, group_node_id, schema_node_id};
    use crate::catalog::model::{DatabaseNode, KindReport, ObjectKind, ObjectRow, SchemaNode};
    let (mut model, orders) = pg_orders_model();
    let schema = schema_node_id("pg", "shop", "public");
    let tables = group_node_id("pg", "shop", "public", ObjectKind::Table);
    let views = group_node_id("pg", "shop", "public", ObjectKind::View);
    let recent = nav_id("pg", "shop", "public", &ObjectRow::view("recent"));
    expand_all(&mut model, &[connection_node_id("pg"), database_node_id("pg", "shop"), schema.clone(), tables.clone(), views.clone()]);
    let failed_views = vec![KindReport { kind: ObjectKind::View, total: 0, error: Some("没有权限读取视图".into()) }];

    for round in 0..2 {
        let _ = model.dispatch(&format!("nav-refresh-node:{schema}"));
        assert_eq!(model.take_nav_request(), Some(NavRequest::Reload(schema.clone())));
        // 视图这一类读取失败：行和总数里都没有它，所以“整页读完”也不能判它缺失。
        model.nav.reload_scope(&schema, &[ObjectRow::table("orders", Some(2))], 1, None, &failed_views);
        let kept = model.nav.node(&recent).unwrap_or_else(|| panic!("第 {round} 次：失败的种类不移除已有对象"));
        assert!(!kept.stale, "第 {round} 次：失败的种类不标 stale");
        assert_eq!(model.nav.node(&views).unwrap().error.as_deref(), Some("没有权限读取视图"));
        assert!(model.nav.node(&views).unwrap().children.contains(&recent));
        assert!(model.nav.node(&tables).unwrap().error.is_none());
        assert!(!model.nav.node(&orders).unwrap().stale);
        assert!(model.nav.is_expanded(&views) && model.nav.is_expanded(&tables));
        assert!(!model.nav.node(&schema).unwrap().loading);
    }
    // 这一类再读成功：错误清掉。
    let _ = model.dispatch(&format!("nav-refresh-node:{schema}"));
    let _ = model.take_nav_request();
    model.nav.reload_scope(&schema, &[ObjectRow::table("orders", Some(2)), ObjectRow::view("recent")], 2, None, &[]);
    assert!(model.nav.node(&views).unwrap().error.is_none());
    assert!(model.nav.node(&recent).is_some_and(|node| !node.stale));

    // 整树刷新里某一类读取失败：同样保留那一类的对象，原因挂在分组上。
    let pg = nav_connection(
        Engine::Postgres,
        "pg",
        "销售库",
        vec![DatabaseNode {
            name: "shop".into(),
            schemas: vec![SchemaNode {
                name: "public".into(),
                objects: vec![ObjectRow::table("orders", Some(2))],
                total: 1,
                error: None,
                kind_reports: failed_views.clone(),
            }],
            error: None,
        }],
    );
    for round in 0..2 {
        model.apply_navigation(std::slice::from_ref(&pg), false);
        let kept = model.nav.node(&recent).unwrap_or_else(|| panic!("整树第 {round} 次：失败的种类不移除已有对象"));
        assert!(!kept.stale);
        assert_eq!(model.nav.node(&views).unwrap().error.as_deref(), Some("没有权限读取视图"));
        assert!(model.nav.node(&views).unwrap().children.contains(&recent));
        assert!(model.nav.is_expanded(&views));
    }
}

/// 按整条连接重读的“刷新此节点”（连接行、PostgreSQL 库、SQLite 连接、MongoDB 库）：
/// 读取失败时原子树和展开都留着，原因挂在被点的节点上；成功时照常重建并清掉错误。
#[test]
fn a_failed_connection_level_node_refresh_keeps_the_subtree() {
    use crate::app::shell::nav::NavRequest;
    use crate::catalog::hierarchy::{connection_node_id, database_node_id, group_node_id, schema_node_id};
    use crate::catalog::model::{ObjectKind, ObjectRow};
    let ids = |model: &WorkbenchModel| model.nav_rows().into_iter().map(|row| row.id).collect::<Vec<_>>();
    let refresh = |model: &mut WorkbenchModel, node: &str, connection: &str| {
        let _ = model.dispatch(&format!("nav-refresh-node:{node}"));
        assert_eq!(model.take_nav_request(), Some(NavRequest::Refresh { connection: Some(connection.into()) }));
    };

    // PostgreSQL 连接行：连接级目录查询失败。
    let (mut model, orders) = pg_orders_model();
    let connection = connection_node_id("pg");
    let database = database_node_id("pg", "shop");
    let schema = schema_node_id("pg", "shop", "public");
    let tables = group_node_id("pg", "shop", "public", ObjectKind::Table);
    expand_all(&mut model, &[connection.clone(), database.clone(), schema.clone(), tables.clone()]);
    let before = ids(&model);
    assert!(before.contains(&orders));
    refresh(&mut model, &connection, "pg");
    let mut failed = nav_connection(Engine::Postgres, "pg", "销售库", Vec::new());
    failed.error = Some("读取库表失败：连接被重置".into());
    model.apply_navigation(&[failed], true);
    assert_eq!(ids(&model), before, "失败不清空子树，展开保持");
    assert_eq!(model.nav.node(&connection).unwrap().error.as_deref(), Some("读取库表失败：连接被重置"));
    assert_eq!(model.nav_loading, 0);
    assert!(model.nav_error.is_empty(), "原因在节点上，不在全局提示条");

    // PostgreSQL 库行：这个库读取失败；再失败一次也仍保留。
    for _ in 0..2 {
        refresh(&mut model, &database, "pg");
        let mut shop = nav_database("shop", Vec::new());
        shop.schemas.clear();
        shop.error = Some("权限不足".into());
        model.apply_navigation(&[nav_connection(Engine::Postgres, "pg", "销售库", vec![shop])], true);
        assert_eq!(ids(&model), before);
        assert_eq!(model.nav.node(&database).unwrap().error.as_deref(), Some("权限不足"));
    }
    // 库下的模式读取失败：同样保留，原因挂在被点的库上。
    refresh(&mut model, &database, "pg");
    let mut shop = nav_database("shop", Vec::new());
    shop.schemas[0].error = Some("模式读取失败".into());
    model.apply_navigation(&[nav_connection(Engine::Postgres, "pg", "销售库", vec![shop])], true);
    assert_eq!(ids(&model), before);
    assert_eq!(model.nav.node(&database).unwrap().error.as_deref(), Some("模式读取失败"));
    // 整个目录读取报错（例如列不出连接）：树不动，原因也挂到被点的节点上。
    refresh(&mut model, &connection, "pg");
    model.nav_failed("读取连接列表失败");
    assert_eq!(ids(&model), before);
    assert_eq!(model.nav.node(&connection).unwrap().error.as_deref(), Some("读取连接列表失败"));
    assert!(model.nav_node_refresh.is_empty());
    // 读取成功：照常重建，新对象出现，错误清掉，展开保持。
    refresh(&mut model, &database, "pg");
    let audit = ObjectRow::table("audit", Some(0));
    let fresh = nav_connection(
        Engine::Postgres,
        "pg",
        "销售库",
        vec![nav_database("shop", vec![audit.clone(), ObjectRow::table("orders", Some(2)), ObjectRow::view("recent")])],
    );
    model.apply_navigation(&[fresh], true);
    assert!(model.nav.node(&database).unwrap().error.is_none());
    assert!(model.nav.node(&connection).unwrap().error.is_none());
    assert!(ids(&model).contains(&nav_id("pg", "shop", "public", &audit)));
    assert!(model.nav.is_expanded(&tables));

    // SQLite：连接行是种类分组上面唯一的刷新入口。
    let customers = ObjectRow::table("customers", Some(1));
    let mut model = WorkbenchModel::new(Shell::default());
    model.apply_navigation(&[nav_connection(Engine::Sqlite, "lite", "本地", vec![nav_database("main", vec![customers.clone()])])], false);
    let root = connection_node_id("lite");
    let customers_id = nav_id("lite", "main", "", &customers);
    expand_all(&mut model, &[root.clone(), group_node_id("lite", "main", "", ObjectKind::Table)]);
    let before = ids(&model);
    assert!(before.contains(&customers_id));
    refresh(&mut model, &root, "lite");
    let mut failed = nav_connection(Engine::Sqlite, "lite", "本地", Vec::new());
    failed.error = Some("database is locked".into());
    model.apply_navigation(&[failed], true);
    assert_eq!(ids(&model), before);
    assert_eq!(model.nav.node(&root).unwrap().error.as_deref(), Some("database is locked"));

    // MongoDB 库行：按整条连接重读，失败时集合都留着。
    let users = ObjectRow::table("users", None);
    let mongo = nav_connection(Engine::Mongodb, "mg", "文档库", vec![nav_database("app", vec![users.clone()])]);
    model.apply_navigation(&[mongo], true);
    let app = database_node_id("mg", "app");
    let users_id = nav_id("mg", "app", "", &users);
    expand_all(&mut model, &[connection_node_id("mg"), app.clone()]);
    let before = ids(&model);
    assert!(before.contains(&users_id));
    refresh(&mut model, &app, "mg");
    let mut failed = nav_connection(Engine::Mongodb, "mg", "文档库", Vec::new());
    failed.error = Some("读取库表失败：server selection timeout".into());
    model.apply_navigation(&[failed], true);
    assert_eq!(ids(&model), before);
    assert_eq!(model.nav.node(&app).unwrap().error.as_deref(), Some("读取库表失败：server selection timeout"));
    assert!(model.nav.node(&customers_id).is_some(), "其它连接不受影响");
}

/// SQLite 连接行刷新：库列表读到了、对象读取失败（connection.error 为空，main 带错误）。
/// SQLite 没有库节点，按连接容器判断：保留子树，原因挂在连接行上；再失败一次仍保留。
#[test]
fn a_sqlite_object_read_failure_on_connection_refresh_keeps_the_subtree() {
    use crate::app::shell::nav::NavRequest;
    use crate::catalog::hierarchy::{connection_node_id, group_node_id};
    use crate::catalog::model::{ObjectKind, ObjectRow, SchemaNode};
    let ids = |model: &WorkbenchModel| model.nav_rows().into_iter().map(|row| row.id).collect::<Vec<_>>();
    let customers = ObjectRow::table("customers", Some(1));
    let mut model = WorkbenchModel::new(Shell::default());
    model.apply_navigation(&[nav_connection(Engine::Sqlite, "lite", "本地", vec![nav_database("main", vec![customers.clone()])])], false);
    let root = connection_node_id("lite");
    let customers_id = nav_id("lite", "main", "", &customers);
    expand_all(&mut model, &[root.clone(), group_node_id("lite", "main", "", ObjectKind::Table)]);
    let before = ids(&model);
    assert!(before.contains(&customers_id));
    for _ in 0..2 {
        let _ = model.dispatch(&format!("nav-refresh-node:{root}"));
        assert_eq!(model.take_nav_request(), Some(NavRequest::Refresh { connection: Some("lite".into()) }));
        let mut main = nav_database("main", Vec::new());
        main.schemas = vec![SchemaNode::empty("")];
        main.error = Some("database is locked".into());
        let failed = nav_connection(Engine::Sqlite, "lite", "本地", vec![main]);
        assert!(failed.error.is_none());
        model.apply_navigation(&[failed], true);
        assert_eq!(ids(&model), before, "失败不清空子树，展开保持");
        assert_eq!(model.nav.node(&root).unwrap().error.as_deref(), Some("database is locked"));
        assert!(model.nav_node_refresh.is_empty());
    }
    // 读取成功：照常重建，错误清掉。
    let _ = model.dispatch(&format!("nav-refresh-node:{root}"));
    assert!(model.take_nav_request().is_some());
    let orders = ObjectRow::table("orders", Some(0));
    let fresh = nav_connection(Engine::Sqlite, "lite", "本地", vec![nav_database("main", vec![customers.clone(), orders.clone()])]);
    model.apply_navigation(&[fresh], true);
    assert!(model.nav.node(&root).unwrap().error.is_none());
    assert!(ids(&model).contains(&nav_id("lite", "main", "", &orders)));
}

/// 连接行刷新成功时，树上还没有节点的库带着错误不算失败：PostgreSQL 新出现的非当前会话库、
/// SQLite 不显示的附加库读取失败，都照常重建、显示新对象，连接行不挂错误。
#[test]
fn a_connection_refresh_ignores_errors_on_databases_the_tree_does_not_show() {
    use crate::app::shell::nav::NavRequest;
    use crate::catalog::hierarchy::{connection_node_id, database_node_id, group_node_id, schema_node_id};
    use crate::catalog::model::{ObjectKind, ObjectRow, SchemaNode};
    let ids = |model: &WorkbenchModel| model.nav_rows().into_iter().map(|row| row.id).collect::<Vec<_>>();

    // PostgreSQL：shop 读取成功并新增 audit，另有一个新库 newdb（不在当前会话里，带错误、没有模式）。
    let (mut model, orders) = pg_orders_model();
    let connection = connection_node_id("pg");
    let shop = database_node_id("pg", "shop");
    let tables = group_node_id("pg", "shop", "public", ObjectKind::Table);
    expand_all(&mut model, &[connection.clone(), shop.clone(), schema_node_id("pg", "shop", "public"), tables.clone()]);
    assert!(ids(&model).contains(&orders));
    let audit = ObjectRow::table("audit", Some(0));
    for round in 0..2 {
        let _ = model.dispatch(&format!("nav-refresh-node:{connection}"));
        assert_eq!(model.take_nav_request(), Some(NavRequest::Refresh { connection: Some("pg".into()) }));
        let mut newdb = nav_database("newdb", Vec::new());
        newdb.schemas.clear();
        newdb.error = Some("当前会话连在 shop，没有另开连接读取 newdb".into());
        let fresh = nav_connection(
            Engine::Postgres,
            "pg",
            "销售库",
            vec![nav_database("shop", vec![audit.clone(), ObjectRow::table("orders", Some(2)), ObjectRow::view("recent")]), newdb],
        );
        model.apply_navigation(&[fresh], true);
        assert!(model.nav.node(&connection).unwrap().error.is_none(), "第 {round} 次：连接行不挂错误");
        assert!(model.nav_node_refresh.is_empty());
        let rows = ids(&model);
        assert!(rows.contains(&nav_id("pg", "shop", "public", &audit)), "第 {round} 次：新对象出现");
        assert!(rows.contains(&orders));
        let newdb_id = database_node_id("pg", "newdb");
        assert!(model.nav.node(&newdb_id).is_some(), "第 {round} 次：新库出现在树上");
        assert!(model.nav.is_expanded(&tables));
    }

    // SQLite：main 读取成功并新增 orders，附加库 aux 读取失败。
    let customers = ObjectRow::table("customers", Some(1));
    let mut model = WorkbenchModel::new(Shell::default());
    model.apply_navigation(&[nav_connection(Engine::Sqlite, "lite", "本地", vec![nav_database("main", vec![customers.clone()])])], false);
    let root = connection_node_id("lite");
    expand_all(&mut model, &[root.clone(), group_node_id("lite", "main", "", ObjectKind::Table)]);
    assert!(ids(&model).contains(&nav_id("lite", "main", "", &customers)));
    let orders = ObjectRow::table("orders", Some(0));
    for round in 0..2 {
        let _ = model.dispatch(&format!("nav-refresh-node:{root}"));
        assert_eq!(model.take_nav_request(), Some(NavRequest::Refresh { connection: Some("lite".into()) }));
        let mut aux = nav_database("aux", Vec::new());
        aux.schemas = vec![SchemaNode::empty("")];
        aux.error = Some("aux 不可读".into());
        let fresh = nav_connection(Engine::Sqlite, "lite", "本地", vec![nav_database("main", vec![customers.clone(), orders.clone()]), aux]);
        model.apply_navigation(&[fresh], true);
        assert!(model.nav.node(&root).unwrap().error.is_none(), "第 {round} 次：连接行不挂错误");
        assert!(model.nav_node_refresh.is_empty());
        let rows = ids(&model);
        assert!(rows.contains(&nav_id("lite", "main", "", &orders)), "第 {round} 次：新表出现");
        assert!(rows.contains(&nav_id("lite", "main", "", &customers)));
    }
}

/// 会话当前库以外的库：展开这一行按库读一次，先有加载态，成功后按层出现模式、分组与对象；
/// 失败只在这个库上留原因，重试从同一个库再读；当前库的树不受影响。
#[test]
fn a_database_outside_the_current_session_reads_on_expand_and_keeps_its_own_error() {
    use crate::app::shell::nav::NavRequest;
    use crate::catalog::hierarchy::{connection_node_id, database_node_id, group_node_id, schema_node_id};
    use crate::catalog::model::{ObjectKind, ObjectRow, SchemaNode};

    let mut pg = nav_connection(
        Engine::Postgres,
        "pg",
        "销售库",
        vec![nav_database("shop", vec![ObjectRow::table("orders", Some(2))])],
    );
    pg.outline_only = true;
    let mut audit = nav_database("audit", Vec::new());
    audit.schemas.clear();
    pg.databases.push(audit);
    let mut model = WorkbenchModel::new(Shell::default());
    model.apply_navigation(&[pg], false);
    let connection = connection_node_id("pg");
    let shop = database_node_id("pg", "shop");
    let audit = database_node_id("pg", "audit");
    expand_all(&mut model, &[connection.clone(), shop.clone()]);
    assert!(model.take_nav_request().is_none(), "当前库随连接一次读出，展开不再发读取");
    assert!(model.nav.node(&audit).is_some(), "别的库在树上是可展开的库行");

    // 展开别的库：排队一次按库读取，行上先出现加载态。
    expand_all(&mut model, std::slice::from_ref(&audit));
    assert_eq!(model.take_nav_request(), Some(NavRequest::Database(audit.clone())));
    let row = model.nav_rows().into_iter().find(|row| row.id == audit).unwrap();
    assert!(row.loading, "库行渲染加载指示");
    assert!(row.error.is_none());

    // 失败：原因只在这个库上；重试从同一个库再读，连接行与当前库不动。
    model.nav.set_database(&audit, Err("读取库表失败：permission denied".to_owned()));
    let node = model.nav.node(&audit).unwrap();
    assert_eq!(node.error.as_deref(), Some("读取库表失败：permission denied"));
    assert!(!node.loading);
    assert!(model.nav.node(&connection).unwrap().error.is_none());
    assert!(model.nav.node(&shop).unwrap().error.is_none());
    let _ = model.dispatch(&format!("nav-retry:{audit}"));
    assert_eq!(model.take_nav_request(), Some(NavRequest::Database(audit.clone())));
    assert!(model.nav.node(&audit).unwrap().loading);
    assert!(model.nav.node(&audit).unwrap().error.is_none(), "重试先清掉上一次的原因");

    // 成功：出现模式与未读的对象种类分组，对象仍按分组按需读。
    model.nav.set_database(&audit, Ok(vec![SchemaNode::empty("public")]));
    assert!(model.nav.node(&audit).unwrap().error.is_none());
    let schema = schema_node_id("pg", "audit", "public");
    assert!(model.nav_rows().iter().any(|row| row.id == schema), "按库读回的 schema 出现在树下");
    let tables = group_node_id("pg", "audit", "public", ObjectKind::Table);
    expand_all(&mut model, &[schema, tables.clone()]);
    assert_eq!(model.take_nav_request(), Some(NavRequest::Group(tables.clone())));
    let login = ObjectRow::table("login", Some(1));
    model.nav.set_kind_page(&tables, std::slice::from_ref(&login), 1, None);
    assert!(model.nav.node(&nav_id("pg", "audit", "public", &login)).is_some());
}

/// 危险操作：生产（按 tone，自建标签也算）要输对名字；第一次点执行只待确认；取消不发语句；
/// 失败保留树并显示原因；迟到的旧结果被丢弃。
#[test]
fn danger_confirm_layer_gates_prod_by_tone_and_keeps_the_tree_on_failure() {
    use crate::app::shell::danger::DangerStep;
    use crate::connections::model::EnvTag;
    use crate::ddl::{DdlEngine, Snapshot};
    let (mut model, orders) = pg_orders_model();
    model.connections.rows.push(pg_saved("pg", "live"));
    let tags = vec![EnvTag { id: "live".into(), label: "线上".into(), tone: "prod".into(), position: 9 }];
    let plan_for = |model: &WorkbenchModel| {
        let prompt = model.danger.as_ref().unwrap();
        let mut snapshot = Snapshot::bare(DdlEngine::Postgres, "shop", "public", "orders");
        snapshot.kind = "table".to_owned();
        crate::ddl::preview_plan(&snapshot, &prompt.action())
    };

    // 取消：不发送任何语句。
    assert!(model.open_nav_menu(&orders));
    let _ = model.dispatch("tree-drop-object");
    let (step, _) = model.take_danger_request().expect("打开即预览");
    assert_eq!(step, DangerStep::Preview);
    let _ = model.dispatch("danger-cancel");
    assert!(model.danger.is_none());
    assert!(model.take_danger_request().is_none());

    assert!(model.open_nav_menu(&orders));
    let _ = model.dispatch("tree-drop-object");
    let (_, prompt) = model.take_danger_request().unwrap();
    let plan = plan_for(&model);
    model.danger_planned(prompt.epoch, Some(tags.clone()), Ok(plan));
    let current = model.danger.as_ref().unwrap();
    assert!(current.prod, "自建标签 tone=prod 也按生产处理");
    assert_eq!(current.env_label, "线上");
    assert!(current.confirmed_sql().iter().any(|sql| sql.contains("DROP TABLE")));
    // 名字不对：执行不可用，点了也不待确认。
    let _ = model.dispatch("danger-execute");
    assert!(!model.danger.as_ref().unwrap().armed);
    model.set_field(DraftField::DangerConfirm, "order".into());
    let _ = model.dispatch("danger-execute");
    assert!(!model.danger.as_ref().unwrap().armed);
    model.set_field(DraftField::DangerConfirm, "orders".into());
    let _ = model.dispatch("danger-execute");
    assert!(model.danger.as_ref().unwrap().armed, "第一次只待确认");
    assert!(model.take_danger_request().is_none(), "第一次不发送语句");
    let _ = model.dispatch("danger-execute");
    let (step, prompt) = model.take_danger_request().expect("第二次执行");
    assert_eq!(step, DangerStep::Execute);

    // 失败：树不变，原因留在确认层，不刷新树。
    assert!(!model.danger_finished(prompt.epoch, Err("permission denied for table orders".into())));
    assert!(model.nav.node(&orders).is_some_and(|node| !node.stale), "失败不显示为已删除");
    assert_eq!(model.danger.as_ref().unwrap().error.as_deref(), Some("permission denied for table orders"));
    assert!(model.take_nav_request().is_none(), "失败不刷新树");

    // 迟到的旧结果不作用到当前确认层。
    assert!(!model.danger_finished(prompt.epoch.wrapping_sub(1), Err("旧结果".into())));
    assert_eq!(model.danger.as_ref().unwrap().error.as_deref(), Some("permission denied for table orders"));

    // 一条语句都没执行的回执不算成功，树不动。
    let _ = model.dispatch("danger-execute");
    let _ = model.dispatch("danger-execute");
    let (_, prompt) = model.take_danger_request().expect("重试执行");
    let receipt = crate::ddl::Receipt { blocked: Vec::new(), rule: String::new(), steps: Vec::new(), target: String::new(), rebuild: false };
    assert!(!model.danger_finished(prompt.epoch, Ok(receipt)));
    assert!(model.nav.node(&orders).is_some());

    // 非生产（dev tone）不需要输入名字；Esc/遮罩关闭等同取消。
    let (mut dev, orders) = pg_orders_model();
    dev.connections.rows.push(pg_saved("pg", "dev"));
    assert!(dev.open_nav_menu(&orders));
    let _ = dev.dispatch("tree-truncate-table");
    let (_, prompt) = dev.take_danger_request().unwrap();
    let plan = plan_for(&dev);
    dev.danger_planned(prompt.epoch, Some(vec![EnvTag { id: "dev".into(), label: "开发".into(), tone: "dev".into(), position: 0 }]), Ok(plan));
    assert!(!dev.danger.as_ref().unwrap().prod);
    assert!(dev.danger.as_ref().unwrap().execute_ready());
    dev.menu = None;
    let _ = dev.dismiss();
    assert!(dev.danger.is_none());
    assert!(dev.take_danger_request().is_none());
}

/// 查看 DDL：读不到定义的对象不开查看器；“复制 DDL”只在读到后写剪贴板。
#[test]
fn definition_requests_only_for_readable_objects() {
    use crate::app::shell::nav::NavRequest;
    use crate::catalog::model::{ObjectKind, ObjectRow};
    let (mut model, orders) = pg_orders_model();
    assert!(model.open_nav_menu(&orders));
    let _ = model.dispatch("tree-view-ddl");
    assert_eq!(model.take_nav_request(), Some(NavRequest::Definition(orders.clone())));
    let epoch = model.ddl_view.as_ref().unwrap().epoch;
    model.definition_loaded(epoch, Ok(crate::catalog::definition::ObjectDdl { text: "CREATE TABLE orders (id int);".into(), rebuilt: true, missing: vec!["CHECK".into()] }));
    let view = model.ddl_view.clone().unwrap();
    assert!(view.rebuilt && !view.copy_only);
    let _ = model.dispatch("ddl-copy");
    assert_eq!(model.take_clipboard().as_deref(), Some("CREATE TABLE orders (id int);"));
    let _ = model.dispatch("ddl-close");
    assert!(model.ddl_view.is_none());

    // 复制 DDL：失败不碰剪贴板。
    assert!(model.open_nav_menu(&orders));
    let _ = model.dispatch("tree-copy-ddl");
    let _ = model.take_nav_request();
    let epoch = model.ddl_view.as_ref().unwrap().epoch;
    model.definition_loaded(epoch, Err("读取定义失败：权限不足".into()));
    assert!(model.ddl_view.is_none());
    assert!(model.take_clipboard().is_none());
    assert_eq!(model.notice_key, Some("catalog.ddl.failed"));

    // 扩展读不到定义：菜单里不出现 DDL 项，直接派发也不开查看器。
    let extension = ObjectRow::fresh("pgcrypto", ObjectKind::Extension);
    let pg = nav_connection(Engine::Postgres, "pg", "销售库", vec![nav_database("shop", vec![extension.clone()])]);
    let mut ext = WorkbenchModel::new(Shell::default());
    ext.apply_navigation(&[pg], false);
    let id = nav_id("pg", "shop", "public", &extension);
    assert!(ext.open_nav_menu(&id));
    assert!(!crate::app::shell::menus::flatten_items(&ext.menu.as_ref().unwrap().items).iter().any(|item| item.id == "tree-view-ddl"), "没有可读定义就不给 DDL 项");
    let _ = ext.dispatch(&format!("nav-view-ddl:{id}"));
    assert!(ext.ddl_view.is_none());
    assert!(ext.take_nav_request().is_none());
    assert_eq!(ext.notice_key, Some("catalog.tree.noDefinition"));
}

/// 等工作台满足条件；后台读取在 tokio 上完成，渲染循环负责把请求发出去并回填。
fn pump_until(
    cx: &mut gpui_kit::TestAppContext,
    window: gpui_kit::AnyWindowHandle,
    view: &gpui_kit::Entity<super::view::Workbench>,
    done: impl Fn(&super::view::Workbench, &gpui_kit::App) -> bool,
) -> bool {
    let started = std::time::Instant::now();
    while started.elapsed() < std::time::Duration::from_secs(30) {
        cx.run_until_parked();
        let _ = cx.update_window(window, |_, window, cx| window.render_frame(cx));
        if cx.update(|cx| done(view.read(cx), cx)) {
            return true;
        }
        std::thread::park_timeout(std::time::Duration::from_millis(20));
    }
    false
}

/// 真实 SQLite：查看 DDL 的文本含全部列名，复制后剪贴板与查看器一致；
/// 删除表经确认层两次点击后执行，表从树上消失；结构页删表成功后树也去掉它。
#[gpui_kit::test]
fn sqlite_tree_ddl_viewer_and_drop_go_through_the_confirm_layer(cx: &mut gpui_kit::TestAppContext) {
    let (window, view, dir) = sqlite_notes_workbench(cx, true);
    wait_for_shell_ready(cx, window, &view);
    let database = dir.join("items.sqlite");
    sqlite_exec_on_file(&database, "CREATE TABLE scratch (id INTEGER PRIMARY KEY, body TEXT)");
    sqlite_exec_on_file(&database, "CREATE TABLE doomed (id INTEGER PRIMARY KEY)");
    cx.update_window(window, |_, _, cx| view.update(cx, |view, cx| view.perform("refresh-tree", cx))).unwrap();
    let item = wait_for_nav_object(cx, window, &view, "item");
    let scratch = wait_for_nav_object(cx, window, &view, "scratch");
    let doomed = wait_for_nav_object(cx, window, &view, "doomed");

    // 查看 DDL → 查看器文本含全部列名 → 复制后剪贴板一致。
    cx.update_window(window, |_, _, cx| {
        view.update(cx, |view, cx| {
            assert!(view.model.open_nav_menu(&item));
            view.perform("tree-view-ddl", cx);
        })
    })
    .unwrap();
    assert!(pump_until(cx, window, &view, |view, _| view.model.ddl_view.as_ref().is_some_and(|ddl| !ddl.loading)), "定义没有读回来");
    let ddl = cx.update(|cx| view.read(cx).model.ddl_view.clone().unwrap());
    let text = ddl.text.clone().unwrap_or_else(|| panic!("定义文本：{:?}", ddl.error));
    assert!(text.contains("id") && text.contains("label"), "{text}");
    assert!(!ddl.rebuilt, "SQLite 给的是原文");
    let shown = cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.find("ddl-viewer-text").bounds()
    })
    .unwrap();
    assert!(f32::from(shown.size.height) > 0., "查看器没有画出来");
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.perform("ddl-copy", cx));
        window.render_frame(cx);
    })
    .unwrap();
    let clipboard = cx.update(|cx| cx.read_from_clipboard().and_then(|item| item.text()));
    assert_eq!(clipboard.as_deref(), Some(text.as_str()), "剪贴板与查看器文本不一致");
    // 原文没有“重建”标注；重建的定义标明来源并列出缺少的内容。
    let plain = cx.update_window(window, |_, window, _| window.try_find("ddl-viewer-rebuilt").is_some()).unwrap();
    assert!(!plain, "原文不应标成重建");
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            if let Some(ddl) = view.model.ddl_view.as_mut() {
                ddl.rebuilt = true;
                ddl.missing = vec!["CHECK".to_owned()];
            }
            cx.notify();
        });
        window.render_frame(cx);
    })
    .unwrap();
    let rebuilt = cx.update_window(window, |_, window, _| window.find("ddl-viewer-rebuilt").bounds()).unwrap();
    assert!(f32::from(rebuilt.size.height) > 0., "重建定义要显示来源标注");
    cx.update_window(window, |_, _, cx| view.update(cx, |view, cx| view.perform("ddl-close", cx))).unwrap();

    // 复制 DDL 直接写剪贴板，内容与查看器同源。
    cx.update(|cx| cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string("旧内容".to_owned())));
    cx.update_window(window, |_, _, cx| {
        view.update(cx, |view, cx| {
            assert!(view.model.open_nav_menu(&item));
            view.perform("tree-copy-ddl", cx);
        })
    })
    .unwrap();
    assert!(pump_until(cx, window, &view, |view, _| view.model.ddl_view.is_none()), "复制 DDL 没有完成");
    let clipboard = cx.update(|cx| cx.read_from_clipboard().and_then(|item| item.text()));
    assert_eq!(clipboard.as_deref(), Some(text.as_str()));

    // 删除表：预览是 DROP，第一次点击只待确认，第二次执行，表从树上消失。
    cx.update_window(window, |_, _, cx| {
        view.update(cx, |view, cx| {
            assert!(view.model.open_nav_menu(&scratch));
            view.perform("tree-drop-object", cx);
        })
    })
    .unwrap();
    assert!(
        pump_until(cx, window, &view, |view, _| view.model.danger.as_ref().is_some_and(|prompt| !prompt.loading)),
        "没有生成计划"
    );
    let prompt = cx.update(|cx| view.read(cx).model.danger.clone().unwrap());
    assert!(prompt.error.is_none(), "{:?}", prompt.error);
    assert!(!prompt.prod, "local 标签不是生产");
    assert!(prompt.confirmed_sql().iter().any(|line| line.contains("DROP TABLE")), "{:?}", prompt.confirmed_sql());
    let shown = cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.find("danger-confirm").bounds()
    })
    .unwrap();
    assert!(f32::from(shown.size.width) > 0.);
    // 英文界面：确认层的风险文字也是英文（计划按中文生成，显示时按句型换成英文），对象名不变。
    let risk_labels = |cx: &mut gpui_kit::TestAppContext| {
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            (0..)
                .map_while(|index| window.try_find(gpui_kit::SharedString::from(format!("danger-risk-{index}"))))
                .map(|risk| risk.label().unwrap_or_default().to_owned())
                .collect::<Vec<_>>()
        })
        .unwrap()
    };
    let zh_risks = risk_labels(cx);
    assert!(!zh_risks.is_empty() && zh_risks.iter().any(|text| crate::ddl::english::has_cjk(text)), "中文界面显示原文：{zh_risks:?}");
    cx.update(|cx| view.update(cx, |view, cx| view.perform("locale-en", cx)));
    let en_risks = risk_labels(cx);
    assert_eq!(en_risks.len(), zh_risks.len());
    for text in &en_risks {
        assert!(!crate::ddl::english::has_cjk(text), "英文确认层残留中文：{text}");
    }
    assert!(en_risks.iter().any(|text| text.contains("scratch")), "对象名保留：{en_risks:?}");
    cx.update(|cx| view.update(cx, |view, cx| view.perform("locale-zh", cx)));
    cx.update_window(window, |_, _, cx| view.update(cx, |view, cx| view.perform("danger-execute", cx))).unwrap();
    for _ in 0..5 {
        cx.run_until_parked();
        let _ = cx.update_window(window, |_, window, cx| window.render_frame(cx));
    }
    assert!(cx.update(|cx| view.read(cx).model.danger.as_ref().is_some_and(|prompt| prompt.armed && !prompt.running)));
    let tables = sqlite_tables(&database);
    assert!(tables.contains(&"scratch".to_owned()), "第一次点击不执行：{tables:?}");
    cx.update_window(window, |_, _, cx| view.update(cx, |view, cx| view.perform("danger-execute", cx))).unwrap();
    assert!(pump_until(cx, window, &view, |view, _| view.model.danger.as_ref().is_none_or(|prompt| !prompt.running)), "执行没有完成");
    let leftover = cx.update(|cx| view.read(cx).model.danger.as_ref().and_then(|prompt| prompt.error.clone()));
    assert!(leftover.is_none(), "执行失败：{leftover:?}");
    assert!(!sqlite_tables(&database).contains(&"scratch".to_owned()));
    assert!(pump_until(cx, window, &view, |view, _| view.model.nav_requests.is_empty() && view.model.nav_loading == 0));
    assert!(cx.update(|cx| view.read(cx).model.nav.node(&scratch).is_none()), "删除后树里不该再有这张表");
    assert!(cx.update(|cx| view.read(cx).model.nav.node(&item).is_some()));

    // 结构页删表成功后，树里也没有这张表。
    let spec = cx.update(|cx| {
        let node = view.read(cx).model.nav.node(&doomed).cloned().unwrap();
        super::nav::spec_for(&node, "local").unwrap()
    });
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.perform(&format!("open-structure-for:{spec}"), cx));
        window.render_frame(cx);
        view.update(cx, |view, cx| {
            view.structure.update(cx, |pane, cx| {
                pane.session.section = crate::app::views::structure::Section::Danger;
                pane.preview(cx);
            });
        });
    })
    .unwrap();
    assert!(
        pump_until(cx, window, &view, |view, cx| !view.structure.read(cx).session.preview_sql().is_empty()),
        "结构页没有生成预览"
    );
    cx.update_window(window, |_, _, cx| {
        view.update(cx, |view, cx| {
            view.structure.update(cx, |pane, cx| {
                pane.confirm(cx);
                pane.confirm(cx);
            });
        })
    })
    .unwrap();
    assert!(pump_until(cx, window, &view, |view, _| view.model.nav.node(&doomed).is_none()), "结构页删表后树里还有这张表");
    assert!(!sqlite_tables(&database).contains(&"doomed".to_owned()));
    let _ = std::fs::remove_dir_all(&dir);
}

/// 真实 SQLite：在结构页改列名并执行成功后，结构从数据库重读——草稿以新列名为原名，
/// 不再把刚执行的改动显示成“1 处修改未应用”。
#[gpui_kit::test]
fn structure_reloads_after_a_successful_column_change(cx: &mut gpui_kit::TestAppContext) {
    let (window, view, dir) = sqlite_notes_workbench(cx, true);
    wait_for_shell_ready(cx, window, &view);
    let database = dir.join("items.sqlite");
    sqlite_exec_on_file(&database, "CREATE TABLE renameme (id INTEGER PRIMARY KEY, body TEXT)");
    cx.update_window(window, |_, _, cx| view.update(cx, |view, cx| view.perform("refresh-tree", cx))).unwrap();
    let table = wait_for_nav_object(cx, window, &view, "renameme");
    let spec = cx.update(|cx| {
        let node = view.read(cx).model.nav.node(&table).cloned().unwrap();
        super::nav::spec_for(&node, "local").unwrap()
    });
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.perform(&format!("open-structure-for:{spec}"), cx));
        window.render_frame(cx);
    })
    .unwrap();
    assert!(
        pump_until(cx, window, &view, |view, cx| view.structure.read(cx).session.kept_columns.iter().any(|column| column.name == "body")),
        "结构没有读回来"
    );
    cx.update_window(window, |_, _, cx| {
        view.update(cx, |view, cx| {
            view.structure.update(cx, |pane, cx| {
                let column = pane.session.kept_columns.iter_mut().find(|column| column.name == "body").unwrap();
                column.name = "note".into();
                pane.preview(cx);
            });
        })
    })
    .unwrap();
    assert!(pump_until(cx, window, &view, |view, cx| !view.structure.read(cx).session.preview_sql().is_empty()), "没有生成预览");
    assert_eq!(cx.update(|cx| view.read(cx).structure.read(cx).pending_changes()), 1);
    cx.update_window(window, |_, _, cx| view.update(cx, |view, cx| view.structure.update(cx, |pane, cx| pane.confirm(cx)))).unwrap();
    assert!(
        pump_until(cx, window, &view, |view, cx| {
            let pane = view.structure.read(cx);
            pane.session.kept_columns.iter().any(|column| column.original.as_deref() == Some("note")) && pane.pending_changes() == 0
        }),
        "执行成功后结构没有按数据库重读"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn sqlite_tables(path: &std::path::Path) -> Vec<String> {
    let path = path.to_path_buf();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
        runtime.block_on(async move {
            let pool = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&path))
                .await
                .expect("connect");
            let names: Vec<String> = sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'table'")
                .fetch_all(&pool)
                .await
                .expect("tables");
            pool.close().await;
            names
        })
    })
    .join()
    .expect("sqlite thread")
}

/// 英文界面：确认层走到“预览过期”和“连接未打开”两条程序自己的错误时，错误横幅是英文。
/// 错误文字来自真实的执行层与应用层（SQL 和预览不一致、连接 id 不存在），不在测试里手写。
#[gpui_kit::test]
fn english_danger_layer_errors_have_no_chinese(cx: &mut gpui_kit::TestAppContext) {
    let (window, view, dir) = sqlite_notes_workbench(cx, true);
    wait_for_shell_ready(cx, window, &view);
    let database = dir.join("items.sqlite");
    sqlite_exec_on_file(&database, "CREATE TABLE scratch (id INTEGER PRIMARY KEY)");
    cx.update_window(window, |_, _, cx| view.update(cx, |view, cx| view.perform("refresh-tree", cx))).unwrap();
    let scratch = wait_for_nav_object(cx, window, &view, "scratch");
    cx.update(|cx| view.update(cx, |view, cx| view.perform("locale-en", cx)));
    cx.update_window(window, |_, _, cx| {
        view.update(cx, |view, cx| {
            assert!(view.model.open_nav_menu(&scratch));
            view.perform("tree-drop-object", cx);
        })
    })
    .unwrap();
    assert!(pump_until(cx, window, &view, |view, _| view.model.danger.as_ref().is_some_and(|prompt| prompt.plan.is_some() && !prompt.loading)), "没有生成计划");
    let error_label = |cx: &mut gpui_kit::TestAppContext| {
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.try_find("danger-error").map(|banner| banner.label().unwrap_or_default().to_owned())
        })
        .unwrap()
    };
    assert!(error_label(cx).is_none());

    // 预览过期：确认的 SQL 和执行时重新生成的不一致，执行层拒绝。
    cx.update(|cx| {
        view.update(cx, |view, _| {
            let plan = view.model.danger.as_mut().and_then(|prompt| prompt.plan.as_mut()).unwrap();
            plan.sql = vec!["DROP TABLE \"scratch_elsewhere\";".to_owned()];
        })
    });
    for _ in 0..2 {
        cx.update_window(window, |_, _, cx| view.update(cx, |view, cx| view.perform("danger-execute", cx))).unwrap();
    }
    assert!(
        pump_until(cx, window, &view, |view, _| view.model.danger.as_ref().is_some_and(|prompt| !prompt.running && prompt.error.is_some())),
        "没有得到过期错误"
    );
    let zh = cx.update(|cx| view.read(cx).model.danger.as_ref().and_then(|prompt| prompt.error.clone()).unwrap());
    assert!(zh.contains("预览已过期"), "执行层给出的是过期错误：{zh}");
    let shown = error_label(cx).expect("错误横幅");
    assert!(!crate::ddl::english::has_cjk(&shown), "英文确认层错误残留中文：{shown}");
    assert!(shown.contains("out of date"), "{shown}");
    assert!(sqlite_tables(&database).contains(&"scratch".to_owned()), "过期时不执行");

    // 连接没有打开：应用层不另开连接，直接报错。
    cx.update(|cx| {
        view.update(cx, |view, cx| {
            view.model.danger.as_mut().unwrap().object.connection_id = "t7-closed-connection".to_owned();
            view.perform("danger-preview", cx);
        })
    });
    assert!(
        pump_until(cx, window, &view, |view, _| view.model.danger.as_ref().is_some_and(|prompt| !prompt.loading && prompt.error.is_some())),
        "没有得到连接未打开的错误"
    );
    let zh = cx.update(|cx| view.read(cx).model.danger.as_ref().and_then(|prompt| prompt.error.clone()).unwrap());
    assert!(zh.contains("没有打开"), "应用层给出的是连接未打开：{zh}");
    let shown = error_label(cx).expect("错误横幅");
    assert!(!crate::ddl::english::has_cjk(&shown), "英文确认层错误残留中文：{shown}");
    assert!(shown.contains("isn't open"), "{shown}");

    // 中文界面仍显示原文。
    cx.update(|cx| view.update(cx, |view, cx| view.perform("locale-zh", cx)));
    assert_eq!(error_label(cx).as_deref(), Some(zh.as_str()));
}

/// 英文界面：确认层打开后表已被别处删掉，再预览时读取层报“找不到表或视图”，横幅是英文。
/// 错误文字来自真实的读取层，不在测试里手写。
#[gpui_kit::test]
fn english_danger_layer_preview_of_dropped_table_has_no_chinese(cx: &mut gpui_kit::TestAppContext) {
    let (window, view, dir) = sqlite_notes_workbench(cx, true);
    wait_for_shell_ready(cx, window, &view);
    let database = dir.join("items.sqlite");
    sqlite_exec_on_file(&database, "CREATE TABLE vanishing (id INTEGER PRIMARY KEY)");
    cx.update_window(window, |_, _, cx| view.update(cx, |view, cx| view.perform("refresh-tree", cx))).unwrap();
    let vanishing = wait_for_nav_object(cx, window, &view, "vanishing");
    cx.update(|cx| view.update(cx, |view, cx| view.perform("locale-en", cx)));
    cx.update_window(window, |_, _, cx| {
        view.update(cx, |view, cx| {
            assert!(view.model.open_nav_menu(&vanishing));
            view.perform("tree-drop-object", cx);
        })
    })
    .unwrap();
    assert!(pump_until(cx, window, &view, |view, _| view.model.danger.as_ref().is_some_and(|prompt| prompt.plan.is_some() && !prompt.loading)), "没有生成计划");

    sqlite_exec_on_file(&database, "DROP TABLE vanishing");
    cx.update(|cx| view.update(cx, |view, cx| view.perform("danger-preview", cx)));
    assert!(
        pump_until(cx, window, &view, |view, _| view.model.danger.as_ref().is_some_and(|prompt| !prompt.loading
            && (prompt.error.is_some() || prompt.plan.as_ref().is_some_and(|plan| !plan.blocked.is_empty())))),
        "表删掉后预览没有得到错误或阻断"
    );
    let zh = cx.update(|cx| {
        let prompt = view.read(cx).model.danger.as_ref().unwrap();
        prompt.error.clone().unwrap_or_else(|| prompt.plan.as_ref().map(|plan| plan.blocked.join(" ")).unwrap_or_default())
    });
    assert!(zh.contains("找不到") && zh.contains("vanishing"), "读取层给出的是找不到表：{zh}");
    let banners = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            ["danger-error", "danger-blocked"]
                .into_iter()
                .filter_map(|id| window.try_find(id).map(|banner| banner.label().unwrap_or_default().to_owned()))
                .collect::<Vec<_>>()
        })
        .unwrap();
    assert!(!banners.is_empty(), "确认层没有显示错误横幅");
    for shown in &banners {
        assert!(!crate::ddl::english::has_cjk(shown), "英文确认层横幅残留中文：{shown}");
        assert!(shown.contains("vanishing") && shown.contains("not found"), "{shown}");
    }
}

/// 英文界面：对象定义查看器的重建标注与读取错误横幅没有中文。重建定义用真实的 rebuilt_definition
/// （PostgreSQL 与 SQL Server、含标识列和三类读取失败）的 missing 清单；错误来自真实的读取层（表已被别处删掉、
/// 连接没有可查询的数据库、种类没有定义）。生成 SQL 里的注释保持中文原样。
#[gpui_kit::test]
fn english_ddl_viewer_rebuilt_and_error_labels_have_no_chinese(cx: &mut gpui_kit::TestAppContext) {
    use crate::catalog::definition::{object_ddl, rebuilt_from_snapshot, DdlKey};
    use crate::catalog::model::ObjectKind;
    use crate::ddl::{Column, DdlEngine, Snapshot};
    let cjk = crate::ddl::english::has_cjk;
    let (window, view, dir) = sqlite_notes_workbench(cx, true);
    wait_for_shell_ready(cx, window, &view);
    let database = dir.join("items.sqlite");
    sqlite_exec_on_file(&database, "CREATE TABLE fading (id INTEGER PRIMARY KEY)");
    cx.update_window(window, |_, _, cx| view.update(cx, |view, cx| view.perform("refresh-tree", cx))).unwrap();
    let item = wait_for_nav_object(cx, window, &view, "item");
    let fading = wait_for_nav_object(cx, window, &view, "fading");
    cx.update(|cx| view.update(cx, |view, cx| view.perform("locale-en", cx)));
    let open = |cx: &mut gpui_kit::TestAppContext, node: &str| {
        cx.update_window(window, |_, _, cx| {
            view.update(cx, |view, cx| {
                assert!(view.model.open_nav_menu(node));
                view.perform("tree-view-ddl", cx);
            })
        })
        .unwrap();
        assert!(pump_until(cx, window, &view, |view, _| view.model.ddl_view.as_ref().is_some_and(|ddl| !ddl.loading)), "定义没有读回来");
        cx.update(|cx| view.read(cx).model.ddl_view.as_ref().unwrap().epoch)
    };
    let label = |cx: &mut gpui_kit::TestAppContext, id: &'static str| {
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.try_find(id).map(|banner| banner.label().unwrap_or_default().to_owned())
        })
        .unwrap()
    };

    // 重建定义：横幅逐项英文，编辑区里的 SQL 注释不改。
    let epoch = open(cx, &item);
    for engine in [DdlEngine::Postgres, DdlEngine::Sqlserver] {
        let mut snapshot = Snapshot::bare(engine, "shop", "public", "orders");
        snapshot.columns.push(Column {
            name: "id".into(),
            type_name: "integer".into(),
            nullable: false,
            default_sql: None,
            primary: true,
            auto: true,
            generated: false,
            default_constraint: None,
        });
        snapshot.index_error = Some("没有读到行数".into());
        snapshot.foreign_key_error = Some("permission denied for table orders".into());
        snapshot.comment_error = Some("元数据是空的".into());
        let ddl = rebuilt_from_snapshot(&snapshot);
        assert!(ddl.missing.len() >= 8, "{:?}", ddl.missing);
        assert!(ddl.missing.iter().all(|item| cjk(item)), "清单本身是中文：{:?}", ddl.missing);
        let sql = ddl.text.clone();
        cx.update(|cx| view.update(cx, |view, cx| {
            view.model.definition_loaded(epoch, Ok(ddl));
            cx.notify();
        }));
        let shown = label(cx, "ddl-viewer-rebuilt").expect("重建标注");
        assert!(!cjk(&shown), "英文查看器重建标注残留中文：{shown}");
        assert!(shown.contains("Not included: CHECK constraints, partitioning") && shown.contains("privileges") && shown.contains("triggers"), "{shown}");
        assert!(shown.contains("indexes (could not be read: The row count could not be read)"), "{shown}");
        assert!(shown.contains("foreign keys (could not be read: permission denied for table orders)"), "数据库原文不翻译：{shown}");
        let text = cx.update(|cx| view.read(cx).model.ddl_view.as_ref().and_then(|ddl| ddl.text.clone()).unwrap());
        assert_eq!(text, sql);
        assert!(text.contains("未包含：CHECK 约束"), "生成 SQL 的注释保持原样：{text}");
    }
    cx.update_window(window, |_, _, cx| view.update(cx, |view, cx| view.perform("ddl-close", cx))).unwrap();

    // 读取错误：表已被别处删掉，树还没刷新。
    sqlite_exec_on_file(&database, "DROP TABLE fading");
    let epoch = open(cx, &fading);
    let zh = cx.update(|cx| view.read(cx).model.ddl_view.as_ref().and_then(|ddl| ddl.error.clone()).expect("读取错误"));
    assert!(zh.starts_with("读取定义失败"), "读取层给出的是读取失败：{zh}");
    let shown = label(cx, "ddl-viewer-error").expect("错误横幅");
    assert!(!cjk(&shown), "英文查看器错误残留中文：{shown}");
    assert_eq!(shown, "Could not read the definition. No definition was returned");

    // 其它真实读取错误：连接没有可查询的数据库（视图与重建的表两条路径）、种类没有定义。
    let key = |kind| DdlKey { database: "shop".into(), schema: "public".into(), name: "orders".into(), kind, signature: String::new(), relation: String::new() };
    let session = crate::connections::CatalogSession::Scripted;
    for (engine, kind) in [(DdlEngine::Postgres, ObjectKind::View), (DdlEngine::Postgres, ObjectKind::Table), (DdlEngine::Postgres, ObjectKind::Extension), (DdlEngine::Sqlite, ObjectKind::Function)] {
        let result = futures::executor::block_on(object_ddl(&session, engine, &key(kind)));
        let message = result.clone().err().unwrap_or_else(|| panic!("{engine:?} {kind:?} 应当读不到"));
        assert!(cjk(&message), "读取层原文是中文：{message}");
        cx.update(|cx| view.update(cx, |view, cx| {
            view.model.definition_loaded(epoch, result);
            cx.notify();
        }));
        let shown = label(cx, "ddl-viewer-error").expect("错误横幅");
        assert!(!cjk(&shown), "英文查看器错误残留中文：{message} -> {shown}");
        assert_eq!(shown.matches("Could not read the definition").count(), 1, "前缀不重复：{shown}");
    }

    // 中文界面显示原文，只去掉重复的前缀。
    cx.update(|cx| view.update(cx, |view, cx| view.perform("locale-zh", cx)));
    let shown = label(cx, "ddl-viewer-error").expect("错误横幅");
    assert_eq!(shown.matches("读取定义失败").count(), 1, "{shown}");
    assert_eq!(shown, "读取定义失败。 sqlite 的function没有可读的定义");
    cx.update_window(window, |_, _, cx| view.update(cx, |view, cx| view.perform("ddl-close", cx))).unwrap();

    // 复制 DDL 失败：状态栏保留原文，英文界面显示英文且前缀不重复。
    cx.update(|cx| view.update(cx, |view, cx| view.perform("locale-en", cx)));
    cx.update_window(window, |_, _, cx| {
        view.update(cx, |view, cx| {
            assert!(view.model.open_nav_menu(&fading));
            view.perform("tree-copy-ddl", cx);
        })
    })
    .unwrap();
    assert!(pump_until(cx, window, &view, |view, _| view.model.ddl_view.is_none()), "复制 DDL 没有结束");
    let detail = cx.update(|cx| view.read(cx).model.notice_detail.clone());
    assert!(cjk(&detail), "模型里保留原文：{detail}");
    let status = label(cx, "status").expect("状态栏");
    assert_eq!(status, "Could not read the definition. No definition was returned");
}

#[test]
fn untagged_connections_show_no_env_pill() {
    assert!(!super::chrome::env_shown(""));
    assert!(!super::chrome::env_shown(crate::connections::model::ENV_NONE));
    assert!(super::chrome::env_shown("prod"));
    assert!(super::chrome::env_shown("t7-live"));
}

#[test]
fn custom_env_tags_never_show_a_dictionary_key() {
    use crate::connections::model::EnvTag;
    use crate::i18n::{text, Locale};
    assert_eq!(super::chrome::env_label(|key| text(Locale::En, key), "prod", &[]), "Prod");
    assert_eq!(super::chrome::env_label(|key| text(Locale::ZhCn, key), "dev", &[]), "开发");
    assert_eq!(super::chrome::env_label(|key| text(Locale::En, key), "t7-live", &[]), "t7-live");
    let tags = [EnvTag { id: "t7-live".into(), label: "线上".into(), tone: "prod".into(), position: 1 }];
    assert_eq!(super::chrome::env_label(|key| text(Locale::En, key), "t7-live", &tags), "线上");
    assert_eq!(super::chrome::env_tone("t7-live", &tags), "prod");
}

#[test]
fn connection_filters_open_a_menu_and_apply_the_chosen_id() {
    use super::model::ConnFilterMenu;
    let mut model = WorkbenchModel::new(Shell::default());
    assert!(model.dispatch("conn-filter-engine").is_some());
    assert_eq!(model.conn_filter_menu, Some(ConnFilterMenu::Engine));
    assert!(model.connections.filter_engine.is_none(), "打开下拉不循环筛选值");
    assert!(model.dispatch("conn-filter-engine:redis").is_some());
    assert_eq!(model.connections.filter_engine, Some(Engine::Redis));
    assert!(model.conn_filter_menu.is_none());
    assert!(model.dispatch("conn-filter-env").is_some());
    assert_eq!(model.conn_filter_menu, Some(ConnFilterMenu::Env));
    assert!(model.dispatch("conn-filter-env:t7-live").is_some());
    assert_eq!(model.connections.filter_env, "t7-live");
    assert!(model.dispatch("conn-filter-engine:").is_some());
    assert!(model.connections.filter_engine.is_none());
}

/// 640 宽时侧栏是盖在工作区上的覆盖层。原生截图发现右键树节点弹出的是下面编辑器的菜单
/// （点击穿透了覆盖层）。覆盖层必须挡住鼠标：右键、左键都只到树，不到编辑器。
#[gpui_kit::test]
fn narrow_overlay_sidebar_keeps_tree_clicks_off_the_editor_beneath(cx: &mut gpui_kit::TestAppContext) {
    use super::menus::MenuPlace;
    let (window, view, dir) = sqlite_notes_workbench_sized(cx, true, 640., 900.);
    wait_for_shell_ready(cx, window, &view);
    let item = wait_for_nav_object(cx, window, &view, "item");
    assert!(!item.is_empty(), "目录里没有读到 item");
    let row = gpui_kit::SharedString::from(format!("nav-{item}"));
    cx.update(|cx| view.update(cx, |view, cx| view.perform("new-query", cx)));
    // 让 item 这一行真正画在覆盖式侧栏里：侧栏打开，连接与表分组展开（同 sidebar_mouse_paths 的做法）。
    cx.update(|cx| {
        view.update(cx, |view, cx| {
            view.model
                .connections
                .status
                .insert("notes".to_owned(), crate::app::views::connections::LinkStatus::Connected);
            view.model.shell.sidebar_open = true;
            view.model.nav.expand(&crate::catalog::hierarchy::connection_node_id("notes"));
            let groups: Vec<String> = view
                .model
                .nav_rows_for("notes")
                .into_iter()
                .filter(|row| row.kind == crate::catalog::hierarchy::NavKind::Group)
                .map(|row| row.id)
                .collect();
            for group in groups {
                view.model.nav.expand(&group);
            }
            cx.notify();
        })
    });
    cx.run_until_parked();
    let shown = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.try_find(row.clone()).is_some_and(|row| row.visible())
        })
        .unwrap();
    assert!(shown, "item 行没有画出来");
    let (overlay, over_editor) = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let row = window.find(row.clone()).bounds();
            let editor = window.find("query-editor").bounds();
            let centre = row.center();
            (view.read(cx).model.painted().sidebar_overlay, editor.contains(&centre))
        })
        .unwrap();
    assert!(overlay, "640 宽侧栏应是覆盖层");
    assert!(over_editor, "测试前提：树行正好盖在编辑器上方");

    cx.update_window(window, |_, window, cx| window.right_click(row.clone(), cx)).unwrap();
    let place = cx.update(|cx| view.read(cx).model.menu.as_ref().map(|menu| menu.place));
    assert_eq!(place, Some(MenuPlace::ObjectTree), "右键树节点要弹出对象树菜单，不能穿透成编辑器菜单");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 原生 640 截图发现连接页右侧空态的说明整行撑出面板，左边压到连接列表、右边被窗口裁掉。
/// 三种宽度下说明都要在右侧面板里换行：不越过连接列表，也不越出窗口。
#[gpui_kit::test]
fn connections_empty_state_wraps_inside_its_panel_at_every_width(cx: &mut gpui_kit::TestAppContext) {
    for width in [1360., 900., 640.] {
        let dir = std::env::temp_dir().join(format!("rillsql-shell-conn-empty-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let (window, view) = open_workbench_sized(cx, services_at(&dir), false, width, 900.);
        cx.update(|cx| view.update(cx, |view, cx| view.perform("connections", cx)));
        let (detail, body_left, window_width) = cx
            .update_window(window, |_, window, cx| {
                window.render_frame(cx);
                let details: Vec<_> = window.find_all("empty-state-detail").into_iter().filter(|detail| detail.visible()).collect();
                assert_eq!(details.len(), 1, "{width}: 启动台应只有一个空态说明");
                // 启动台是单栏：空态整块落在页面主体里，左缘对齐活动栏右侧。
                let rail = window.find("activity").bounds();
                (details[0].bounds(), f32::from(rail.origin.x + rail.size.width), f32::from(window.find("workbench").bounds().size.width))
            })
            .unwrap();
        let left = f32::from(detail.origin.x);
        let right = f32::from(detail.origin.x + detail.size.width);
        assert!(left >= body_left, "{width}: 空态说明左缘 {left} 压到活动栏（右缘 {body_left}）");
        assert!(right <= window_width + 0.5, "{width}: 空态说明右缘 {right} 越出窗口 {window_width}");
        assert!(f32::from(detail.size.width) <= 360.5, "{width}: 说明最宽 360");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn submenu_escape_returns_to_the_parent_and_widths_stay_in_range() {
    let (mut model, orders) = pg_orders_model();
    assert!(model.open_nav_menu(&orders));
    let sql = model.menu.as_ref().unwrap().items.iter().position(|item| item.id == "submenu-sql").expect("模板子菜单");
    model.menu.as_mut().unwrap().index = sql;
    assert!(model.key(NavKey::ArrowRight).is_none());
    assert!(model.menu.as_ref().unwrap().submenu.is_some());
    assert_eq!(model.menu.as_ref().unwrap().current().map(|item| item.id), Some("tree-select-sql"));
    model.menu.as_mut().unwrap().hover(0);
    assert!(model.menu.as_ref().unwrap().submenu.is_none(), "悬停没有子菜单的项要收起子菜单");
    model.menu.as_mut().unwrap().index = sql;
    let _ = model.menu.as_mut().unwrap().open_submenu();
    assert!(model.key(NavKey::ArrowLeft).is_none());
    assert!(model.menu.as_ref().unwrap().submenu.is_none());
    assert!(model.menu.is_some(), "左方向键只收子菜单");
    let _ = model.menu.as_mut().unwrap().open_submenu();
    assert!(model.key(NavKey::Escape).is_none());
    assert!(model.menu.is_some() && model.menu.as_ref().unwrap().submenu.is_none(), "Esc 先收子菜单");
    assert!(model.key(NavKey::Escape).is_none());
    assert!(model.menu.is_none(), "子菜单关上之后 Esc 才关整个菜单");

    model.set_sidebar_width(12.);
    assert_eq!(model.sidebar_width, super::layout::SIDEBAR_MIN);
    model.set_sidebar_width(900.);
    assert_eq!(model.sidebar_width, super::layout::SIDEBAR_MAX);
    model.reset_sidebar_width();
    assert_eq!(model.sidebar_width, super::layout::SIDEBAR_DEFAULT);
    model.viewport_width = 1360;
    model.set_history_preview_width(10.);
    assert_eq!(model.history_preview_width, super::layout::HISTORY_PREVIEW_MIN);
    model.set_history_preview_width(900.);
    assert_eq!(model.history_preview_width, super::layout::HISTORY_PREVIEW_MAX);
    let narrow = super::layout::clamp_history_preview(400., 900.);
    assert!(900. - narrow >= super::layout::HISTORY_LIST_MIN - 0.5, "并排时列表至少 240");
}

#[gpui_kit::test]
fn switching_to_settings_restores_focus_on_the_workbench_content(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::Focusable as _;
    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-focus-restore-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench(cx, services_at(&dir), false);
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.perform("settings", cx));
        window.render_frame(cx);
        let handle = view.read(cx).focus_handle(cx);
        assert!(handle.is_focused(window), "切到设置页后焦点应落在 #workbench-content");
        assert_eq!(view.read(cx).model.shell.page, AppPage::Settings);
    })
    .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[gpui_kit::test]
fn sidebar_connection_menu_opens_the_visible_editor(cx: &mut gpui_kit::TestAppContext) {
    let (window, view, dir) = sqlite_notes_workbench(cx, true);
    wait_for_shell_ready(cx, window, &view);
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, _| {
            view.model.shell.page = AppPage::Workspace;
            view.model.connections.editor = None;
            view.model.shell.sidebar_open = true;
        });
        window.render_frame(cx);
        window.right_click("side-notes", cx);
        window.render_frame(cx);
        window.click("ctx-conn-edit", cx);
        window.render_frame(cx);
        assert_eq!(view.read(cx).model.shell.page, AppPage::Connections);
        assert!(window.find("conn-form").visible());
        assert_eq!(view.read(cx).model.connections.editor.as_ref().unwrap().id.as_deref(), Some("notes"));
    }).unwrap();
    let _ = std::fs::remove_dir_all(dir);
}

#[gpui_kit::test]
fn newly_connected_navigation_is_dispatched_in_the_same_frame(cx: &mut gpui_kit::TestAppContext) {
    let (window, view, dir) = sqlite_notes_workbench(cx, false);
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, _| {
            view.model.connections.status.insert("notes".into(), crate::app::views::connections::LinkStatus::Connected);
        });
        window.render_frame(cx);
        assert!(view.read(cx).model.nav_requests.is_empty(), "连接成功后不能把目录任务留到下一次鼠标事件才派发");
    }).unwrap();
    let _ = std::fs::remove_dir_all(dir);
}

#[gpui_kit::test]
fn dock_reopen_reuses_existing_window_and_retargets_actions_after_close(cx: &mut gpui_kit::TestAppContext) {
    let dir = std::env::temp_dir().join(format!("rillsql-reopen-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let (window, view) = open_workbench(cx, Arc::clone(&services), false);
    let weak = view.downgrade();
    cx.update(|cx| crate::app::reopen_workbench(cx, |_| panic!("已有窗口不能再创建窗口")));
    cx.update_window(window, |_, window, _| window.remove_window()).unwrap();
    drop(view);
    cx.run_until_parked();
    assert!(weak.upgrade().is_none(), "全局菜单动作不能持有已关闭工作台及其结果集");
    let new_view = cx.update(|cx| {
        let mut opened = None;
        crate::app::reopen_workbench(cx, |cx| {
            let (_, view) = gpui_kit::open_window(gpui_kit::WindowOptions::default(), cx, |window, cx| {
                cx.new(|cx| super::view::Workbench::with_services(window, cx, services, false))
            }).unwrap();
            crate::app::install_workbench_actions(cx, &view);
            opened = Some(view);
        });
        opened.expect("关闭最后窗口后应重新打开")
    });
    let new_window = cx.update(|cx| cx.windows()[0]);
    cx.update_window(new_window, |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(crate::app::NewQuery), cx);
    }).unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        let calls = new_view.read(cx).model.commands.iter().filter(|id| id.as_str() == "new-query").count();
        assert_eq!(calls, 1, "Dock 重开后菜单只路由到新窗口一次");
    });
    let _ = std::fs::remove_dir_all(dir);
}

#[gpui_kit::test]
fn sidebar_copy_and_test_remain_usable_while_catalog_is_loading(cx: &mut gpui_kit::TestAppContext) {
    let (window, view, dir) = sqlite_notes_workbench(cx, true);
    wait_for_shell_ready(cx, window, &view);
    assert!(pump_until(cx, window, &view, |view, _| view.model.running_jobs == 0 && view.model.nav_loading == 0));
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, _| {
            view.model.shell.page = AppPage::Workspace;
            view.model.connections.editor = None;
            view.model.nav_loading = 1;
        });
        window.render_frame(cx);
        window.right_click("side-notes", cx);
        window.render_frame(cx);
        window.click("ctx-conn-duplicate", cx);
        window.render_frame(cx);
        assert!(window.find("conn-form").visible());
        let draft = view.read(cx).model.connections.editor.as_ref().unwrap();
        assert!(draft.id.is_none());
        // 副本的名字跟界面语言走（“… 副本” / “… copy”），不再固定拼一个英文后缀。
        let source = view.read(cx).model.connections.rows.iter().find(|row| row.saved.id == "notes").map(|row| row.saved.name.clone()).unwrap();
        assert_eq!(draft.name, i18n::fill(&view.read(cx).model.text("ui.conn.copyName"), &[("name", &source)]));
        // 模态是覆盖层：先关掉它，下面对象树的右键才有机会收到点击。
        view.update(cx, |view, cx| view.perform("conn-cancel", cx));
        view.update(cx, |view, _| view.model.shell.page = AppPage::Workspace);
        window.render_frame(cx);
        window.right_click("side-notes", cx);
        window.render_frame(cx);
        window.click("ctx-conn-test", cx);
        window.render_frame(cx);
        assert_eq!(view.read(cx).model.shell.page, AppPage::Connections);
        assert!(window.find("conn-form").visible());
    }).unwrap();
    assert!(pump_until(cx, window, &view, |view, _| view.model.connections.message_key == Some("connections.editor.test.ok")));
    let _ = std::fs::remove_dir_all(dir);
}

/// Run against the mysql and redis services in docker/compose.yaml.
#[gpui_kit::test]
#[ignore = "requires the MySQL and Redis development fixtures"]
fn query_editor_runs_mysql_and_redis_fixtures(cx: &mut gpui_kit::TestAppContext) {
    use crate::connections::model::{ConnectionDraft, Secret, SecretInput, TlsMode};

    let dir = std::env::temp_dir().join(format!("rillsql-query-fixtures-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let key = format!("rillsql:query-test:{}", uuid::Uuid::new_v4());
    let rows = jobs::runtime().block_on(async {
        for (engine, port, password) in [
            (Engine::Mysql, std::env::var("RILLSQL_MYSQL_PORT").unwrap_or_else(|_| "3306".into()), "rillsql"),
            (Engine::Redis, std::env::var("RILLSQL_REDIS_PORT").unwrap_or_else(|_| "6379".into()), "rillsql-redis"),
        ] {
            let id = engine.as_str();
            let mut draft = ConnectionDraft::blank(engine);
            draft.id = Some(id.into());
            draft.name = id.into();
            draft.host = "127.0.0.1".into();
            draft.port = port;
            draft.tls_mode = TlsMode::Off;
            draft.save_password = false;
            draft.database.clear();
            if engine == Engine::Mysql {
                draft.username = "rillsql".into();
            } else {
                draft.username.clear();
                draft.database = "8".into();
            }
            services.connections.save(draft, SecretInput::default()).await.unwrap();
            let opened = services.connections.open(id, SecretInput { password: Some(Secret::new(password)), ..Default::default() }).await.unwrap();
            assert!(opened.connected, "{:?}", opened.report);
        }
        let (redis, _) = services.connections.redis_session("redis").unwrap();
        redis::pipe().atomic()
            .cmd("HSET").arg(&key).arg("state").arg("ready").ignore()
            .cmd("EXPIRE").arg(&key).arg(120).ignore()
            .query_async::<()>(&mut *redis.lock().await).await.unwrap();
        services.list_connections().await.unwrap()
    });
    let (window, view) = open_workbench(cx, Arc::clone(&services), false);
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, _| {
            view.model.connections.rows = rows;
            for id in ["mysql", "redis"] {
                view.model.connections.status.insert(id.into(), LinkStatus::Connected);
            }
            view.model.open_bound_query("SELECT 1".into(), Some("mysql".into()), Some("mysql".into()), Some("rillsql".into()), Some("mysql".into()));
        });
        window.render_frame(cx);
        window.click("query-act-execute-query", cx);
    }).unwrap();
    assert!(pump_until(cx, window, &view, |view, cx| view.query_cell(0, 0, cx) == "1" && !view.model.active_tab().unwrap().running), "SELECT 1");
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.perform("nav-open-keyspace:redis\u{1}0", cx);
        });
        window.render_frame(cx);
    }).unwrap();
    assert!(pump_until(cx, window, &view, |view, cx| !view.redis.read(cx).running()));
    cx.update(|cx| view.update(cx, |view, cx| view.perform("redis-db-pick:8", cx)));
    assert!(pump_until(cx, window, &view, |view, cx| view.redis.read(cx).database() == "8" && !view.redis.read(cx).running()));
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.perform("new-query", cx);
            let id = view.model.active_tab.clone().unwrap();
            let tab = view.model.tabs.iter_mut().find(|tab| tab.id == id).unwrap();
            assert_eq!(tab.connection_id.as_deref(), Some("redis"));
            assert_eq!(tab.database.as_deref(), Some("8"));
            tab.draft = format!("HGET {key}");
        });
        window.render_frame(cx);
        window.click("query-act-execute-query", cx);
    }).unwrap();
    assert!(pump_until(cx, window, &view, |view, cx| view.query_cell(0, 0, cx).contains("wrong number of arguments") && !view.model.active_tab().unwrap().running));
    // A browser on db0 and a query tab on db8 must keep independent selections.
    jobs::runtime().block_on(crate::application::redis::select_db(&services.connections, "redis", 0)).unwrap();
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, _| view.model.active_mut().unwrap().draft = format!("HGET {key} state"));
        window.render_frame(cx);
        window.click("query-act-execute-query", cx);
    }).unwrap();
    assert!(pump_until(cx, window, &view, |view, cx| view.query_cell(0, 0, cx) == "ready" && !view.model.active_tab().unwrap().running), "HGET key field");
    let (browser, _) = services.connections.redis_session("redis").unwrap();
    let in_browser = jobs::runtime().block_on(async {
        redis::cmd("EXISTS").arg(&key).query_async::<bool>(&mut *browser.lock().await).await.unwrap()
    });
    assert!(!in_browser, "query changed the browser database");
    services.connections.disconnect("redis");
    services.connections.disconnect("mysql");
    let _ = std::fs::remove_dir_all(dir);
}

/// A6 适配器收口冒烟：MySQL / PostgreSQL / SQLite / Redis 各自经工作台完成连接、对象树浏览与查询。
/// 需要 docker/compose.yaml 的 mysql、postgres、redis 服务；SQLite 使用 docker/sqlite/notes.sqlite 的副本。
#[gpui_kit::test]
#[ignore = "requires the MySQL, PostgreSQL and Redis development fixtures"]
fn adapter_smoke_connects_browses_and_queries_fixture_engines(cx: &mut gpui_kit::TestAppContext) {
    use crate::catalog::hierarchy::{database_node_id, group_node_id, schema_node_id, NavKind};
    use crate::catalog::model::ObjectKind;
    use crate::connections::model::{ConnectionDraft, Secret, SecretInput, TlsMode};
    use gpui_kit::SharedString;
    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-adapter-smoke-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let notes = dir.join("notes.sqlite");
    std::fs::copy(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../docker/sqlite/notes.sqlite"), &notes).unwrap();
    let services = services_at(&dir);
    let rows = jobs::runtime().block_on(async {
        for (engine, port, user, database, password) in [
            (Engine::Mysql, "3306", "rillsql", "rillsql", "rillsql"),
            (Engine::Postgres, "5432", "postgres", "rillsql", "postgres"),
            (Engine::Redis, "6379", "", "0", "rillsql-redis"),
        ] {
            let id = engine.as_str();
            let mut draft = ConnectionDraft::blank(engine);
            draft.id = Some(id.into());
            draft.name = id.into();
            draft.host = "127.0.0.1".into();
            draft.port = port.into();
            draft.username = user.into();
            draft.database = database.into();
            draft.tls_mode = TlsMode::Off;
            draft.save_password = false;
            services.connections.save(draft, SecretInput::default()).await.unwrap();
            let opened = services.connections.open(id, SecretInput { password: Some(Secret::new(password)), ..Default::default() }).await.unwrap();
            assert!(opened.connected, "{id}: {:?}", opened.report);
        }
        let mut draft = ConnectionDraft::blank(Engine::Sqlite);
        draft.id = Some("sqlite".into());
        draft.name = "sqlite".into();
        draft.file_path = notes.display().to_string();
        services.connections.save(draft, SecretInput::default()).await.unwrap();
        let opened = services.connections.open("sqlite", SecretInput::default()).await.unwrap();
        assert!(opened.connected, "sqlite: {:?}", opened.report);
        services.list_connections().await.unwrap()
    });
    let (window, view) = open_workbench(cx, Arc::clone(&services), false);
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, _| {
            view.model.connections.rows = rows;
            for id in ["mysql", "postgres", "sqlite", "redis"] {
                view.model.connections.status.insert(id.into(), LinkStatus::Connected);
            }
        });
        window.render_frame(cx);
    }).unwrap();
    // 浏览：逐级展开到表分组，等对象页读回种子表。
    let paths: [(&str, Vec<String>, String, &str); 3] = [
        ("mysql", vec![database_node_id("mysql", "rillsql")], group_node_id("mysql", "rillsql", "", ObjectKind::Table), "customers"),
        (
            "postgres",
            vec![database_node_id("postgres", "rillsql"), schema_node_id("postgres", "rillsql", "public")],
            group_node_id("postgres", "rillsql", "public", ObjectKind::Table),
            "customers",
        ),
        ("sqlite", Vec::new(), group_node_id("sqlite", "main", "", ObjectKind::Table), "notes"),
    ];
    for (id, parents, group, table) in &paths {
        for node in parents.iter().chain(std::iter::once(group)) {
            assert!(pump_until(cx, window, &view, |view, _| view.model.nav.node(node).is_some_and(|node| !node.loading)), "{id}: node {node} appears");
            let expanded = cx.update(|cx| view.read(cx).model.nav.is_expanded(node));
            if !expanded {
                cx.update_window(window, |_, window, cx| {
                    window.click(SharedString::from(format!("nav-chevron-{node}")), cx);
                    window.render_frame(cx);
                }).unwrap();
            }
        }
        assert!(
            pump_until(cx, window, &view, |view, _| view.model.nav.nodes().any(|node| node.kind == NavKind::Object && node.connection_id == *id && node.name == *table)),
            "{id}: table {table} browsed"
        );
    }
    // 查询：每个关系型引擎在绑定连接的查询页执行一条读语句。
    for (id, database, sql, expected) in [
        ("mysql", "rillsql", "SELECT COUNT(*) FROM customers", "2"),
        ("postgres", "rillsql", "SELECT COUNT(*) FROM customers", "2"),
        ("sqlite", "main", "SELECT COUNT(*) FROM notes", "2"),
    ] {
        cx.update_window(window, |_, window, cx| {
            view.update(cx, |view, _| {
                view.model.open_bound_query(sql.into(), Some(id.into()), Some(id.into()), Some(database.into()), Some(id.into()));
            });
            window.render_frame(cx);
            window.click("query-act-execute-query", cx);
        }).unwrap();
        assert!(
            pump_until(cx, window, &view, |view, cx| view.query_cell(0, 0, cx) == expected && !view.model.active_tab().unwrap().running),
            "{id}: {sql}"
        );
    }
    // Redis：打开键空间浏览种子键，再在查询页执行命令。
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.perform("nav-open-keyspace:redis\u{1}0", cx));
        window.render_frame(cx);
    }).unwrap();
    assert!(pump_until(cx, window, &view, |view, cx| {
        let redis = view.redis.read(cx);
        !redis.running() && redis.keys().iter().any(|key| key.name == "rillsql:string:welcome")
    }), "redis keys browsed");
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, _| {
            view.model.open_bound_query("GET rillsql:string:welcome".into(), Some("redis".into()), Some("redis".into()), Some("0".into()), Some("redis".into()));
        });
        window.render_frame(cx);
        window.click("query-act-execute-query", cx);
    }).unwrap();
    assert!(
        pump_until(cx, window, &view, |view, cx| view.query_cell(0, 0, cx).contains("Redis 夹具") && !view.model.active_tab().unwrap().running),
        "redis GET"
    );
    for id in ["mysql", "postgres", "sqlite", "redis"] {
        services.connections.disconnect(id);
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[gpui_kit::test]
#[ignore = "requires scripts/verify-catalog-mysql.py on loopback port 33067"]
fn mysql_lazy_navigation_latency_fixture(cx: &mut gpui_kit::TestAppContext) {
    use crate::catalog::hierarchy::{database_node_id, group_node_id, NavKind};
    use crate::catalog::model::ObjectKind;
    use gpui_kit::SharedString;
    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-mysql-ui-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let task = Arc::clone(&services);
    let (tx, rx) = std::sync::mpsc::channel();
    jobs::runtime().spawn(async move {
        let mut draft = crate::connections::model::ConnectionDraft::blank(Engine::Mysql);
        draft.id = Some("fixture".into());
        draft.name = "Latency fixture".into();
        draft.host = "127.0.0.1".into();
        draft.port = "33067".into();
        draft.username = "fixture".into();
        draft.database = "shop".into();
        draft.tls_mode = crate::connections::model::TlsMode::Off;
        draft.save_password = false;
        task.connections.save(draft, crate::connections::model::SecretInput::default()).await.unwrap();
        let opened = task.connections.open("fixture", crate::connections::model::SecretInput { password: Some(crate::connections::model::Secret::new("fixture")), ..Default::default() }).await.unwrap();
        assert!(opened.connected, "{:?}", opened.report);
        tx.send(task.list_connections().await.unwrap()).unwrap();
    });
    let rows = rx.recv_timeout(std::time::Duration::from_secs(120)).expect("fixture setup");
    let (window, view) = open_workbench(cx, Arc::clone(&services), false);
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, _| {
            view.model.settings.object_page = 100;
            view.model.connections.rows = rows;
            view.model.connections.status.insert("fixture".into(), crate::app::views::connections::LinkStatus::Connected);
        });
        window.render_frame(cx);
        assert!(view.read(cx).model.nav_requests.is_empty());
    }).unwrap();
    let database = database_node_id("fixture", "shop");
    let group = group_node_id("fixture", "shop", "", ObjectKind::Table);
    assert!(pump_until(cx, window, &view, |view, _| view.model.nav.node(&group).is_some() && view.model.nav_loading == 0));
    cx.update(|cx| assert_eq!(view.read(cx).model.nav.nodes().filter(|node| node.kind == NavKind::Object).count(), 0));
    cx.update_window(window, |_, window, cx| {
        window.click(SharedString::from(format!("nav-chevron-{database}")), cx);
        window.render_frame(cx);
        window.click(SharedString::from(format!("nav-chevron-{group}")), cx);
    }).unwrap();
    assert!(pump_until(cx, window, &view, |view, _| view.model.nav.node(&group).is_some_and(|node| !node.loading && node.loaded == 100)));
    cx.update_window(window, |_, window, cx| {
        window.click(SharedString::from(format!("nav-more-{group}")), cx);
    }).unwrap();
    assert!(pump_until(cx, window, &view, |view, _| view.model.nav.node(&group).is_some_and(|node| !node.loading && node.loaded == 200)));
    cx.update(|cx| assert!(view.read(cx).query_has_object("table_000199", cx), "查询索引应复用已加载页面"));
    // 续读途中刷新多次：旧页取消，刷新合并为有限请求，最终只保留新一轮第一页。
    cx.update_window(window, |_, window, cx| {
        window.click(SharedString::from(format!("nav-more-{group}")), cx);
        view.update(cx, |view, cx| { for _ in 0..20 { view.perform("refresh-tree", cx); } });
        window.render_frame(cx);
        assert!(view.read(cx).model.nav_loading <= 1, "重复刷新不能排队二十份扫描");
    }).unwrap();
    assert!(pump_until(cx, window, &view, |view, _| view.model.nav_loading == 0 && view.model.nav_requests.is_empty() && view.model.nav.node(&group).is_some_and(|node| !node.loading && !node.unloaded && node.loaded == 100)));
    services.connections.disconnect("fixture");
    let _ = std::fs::remove_dir_all(dir);
}

#[gpui_kit::test]
fn exchange_rows_are_lazy_cached_and_released_when_hidden(cx: &mut gpui_kit::TestAppContext) {
    let dir = std::env::temp_dir().join(format!("rillsql-lazy-export-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench(cx, services_at(&dir), false);
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.plant_exchange_sheet("notes|Notes|main||items|sqlite|local", vec!["body".into()], (0..10_000).map(|_| vec![Some("x".repeat(256))]).collect(), cx);
        });
        window.render_frame(cx);
        assert!(view.read(cx).exchange.read(cx).source().rows.is_empty(), "隐藏面板不应保留整表副本");
        view.update(cx, |view, cx| view.perform("open-exchange", cx));
        window.render_frame(cx);
        let rows = &view.read(cx).exchange.read(cx).source().rows;
        assert_eq!(rows.len(), 10_000);
        let first = rows[0][0].as_ref().unwrap().as_ptr();
        view.update(cx, |_, cx| cx.notify());
        window.render_frame(cx);
        assert_eq!(view.read(cx).exchange.read(cx).source().rows[0][0].as_ref().unwrap().as_ptr(), first, "无数据变化的重绘不得复制整表");
        view.update(cx, |view, cx| view.perform("open-exchange", cx));
        window.render_frame(cx);
        assert!(view.read(cx).exchange.read(cx).source().rows.is_empty());
    }).unwrap();
    let _ = std::fs::remove_dir_all(dir);
}

// ---- T2：连接测试针对当前草稿 ----

/// 真实的空 SQLite 库：OpenFile 之后还要读 schema，随便写的字节不算一个库。
async fn sqlite_file(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&path).create_if_missing(true))
        .await
        .unwrap();
    pool.close().await;
    path
}

/// 新建草稿不保存也能测试；失败保留草稿与本次机密，并把错误落到具体字段。
#[tokio::test]
async fn connection_test_runs_on_the_unsaved_draft_and_keeps_it_on_failure() {
    let dir = std::env::temp_dir().join(format!("rillsql-draft-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let mut model = WorkbenchModel::new(Shell::default());
    let file = sqlite_file(&dir, "draft.sqlite").await;
    model.connections.start_new(Engine::Sqlite);
    model.connections.editor.as_mut().unwrap().file_path = file.display().to_string();

    let _ = model.dispatch("conn-test");
    let token = model.prepare_list_token(&services.tasks);
    let request = request_of(&mut model, token);
    assert!(
        request.draft.as_ref().is_some_and(|draft| draft.id.is_none()),
        "测试请求应带当前未保存的草稿"
    );
    let done = jobs::execute_job(&services, request).await;
    apply_done(&mut model, &services, "connections", done);
    assert_eq!(
        model.connections.message_key,
        Some("connections.editor.test.ok"),
        "{}",
        model.connections.message_detail
    );
    assert!(model.connections.editor.is_some(), "测试成功不关闭表单");
    assert!(model.connections.rows.is_empty(), "测试不保存连接配置");
    assert!(model.connections.issues.is_empty());

    // 换成打不开的文件：失败要留在表单里，错误落到 file_path 字段旁。
    let missing = dir.join("missing.sqlite");
    model.connections.editor.as_mut().unwrap().file_path = missing.display().to_string();
    model.connections.secrets.password = "typed-only-for-this-test".into();
    let _ = model.dispatch("conn-test");
    let token = model.prepare_list_token(&services.tasks);
    let done = jobs::execute_job(&services, request_of(&mut model, token)).await;
    apply_done(&mut model, &services, "connections", done);
    assert_eq!(
        model.connections.message_key,
        Some("connections.editor.test.failed"),
        "{}",
        model.connections.message_detail
    );
    assert!(model.connections.editor.is_some(), "失败不清空草稿");
    assert_eq!(model.connections.secrets.password, "typed-only-for-this-test", "失败不清空本次机密");
    let issue = model.connections.issues.first().expect("失败应关联到字段");
    assert_eq!(issue.field, "file_path");
    assert!(!issue.detail.is_empty(), "字段旁要保留失败原文");
    assert!(!serde_json::to_string(&model.snapshot()).unwrap().contains("typed-only-for-this-test"));
    assert!(model.connections.rows.is_empty());

    // 修好字段后，该字段的错误立即消失，可以原地重测。
    model.set_field(DraftField::File, file.display().to_string());
    assert!(model.connections.issues.is_empty(), "编辑字段后不再显示该字段的旧错误");
    let _ = model.dispatch("conn-test");
    let token = model.prepare_list_token(&services.tasks);
    let done = jobs::execute_job(&services, request_of(&mut model, token)).await;
    apply_done(&mut model, &services, "connections", done);
    assert_eq!(model.connections.message_key, Some("connections.editor.test.ok"));
    let _ = std::fs::remove_dir_all(dir);
}

/// 发起测试后继续编辑（或直接关掉表单）：迟到的结果不写回任何状态。
#[tokio::test]
async fn stale_connection_test_result_does_not_overwrite_new_edits() {
    let dir = std::env::temp_dir().join(format!("rillsql-draft-race-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let mut model = WorkbenchModel::new(Shell::default());
    let file = sqlite_file(&dir, "draft.sqlite").await;
    model.connections.start_new(Engine::Sqlite);
    model.connections.editor.as_mut().unwrap().file_path = file.display().to_string();

    let _ = model.dispatch("conn-test");
    let token = model.prepare_list_token(&services.tasks);
    let request = request_of(&mut model, token);
    // 请求发出后用户改了主机/文件，这次结果对应的已经是被丢弃的草稿。
    let missing = dir.join("edited-away.sqlite");
    model.connections.editor.as_mut().unwrap().file_path = missing.display().to_string();
    let done = jobs::execute_job(&services, request).await;
    apply_done(&mut model, &services, "connections", done);
    assert_ne!(
        model.connections.message_key,
        Some("connections.editor.test.ok"),
        "过期结果不能显示成当前草稿的结论"
    );
    assert!(model.connections.message_key.is_none(), "只收起“正在测试”");
    assert!(model.connections.issues.is_empty());

    // 表单关闭后到达的结果同样丢弃：重新开一份草稿，请求发出后再关掉表单。
    model.connections.start_new(Engine::Sqlite);
    model.connections.editor.as_mut().unwrap().file_path = file.display().to_string();
    let _ = model.dispatch("conn-test");
    let token = model.prepare_list_token(&services.tasks);
    let request = request_of(&mut model, token);
    model.connections.editor = None;
    let done = jobs::execute_job(&services, request).await;
    apply_done(&mut model, &services, "connections", done);
    assert!(model.connections.message_key.is_none());
    assert!(model.connections.issues.is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

/// 保存连接不等于连接建立：保存后最近使用仍为空，成功打开才写入。
#[tokio::test]
async fn saving_a_connection_does_not_count_as_a_successful_open() {
    let dir = std::env::temp_dir().join(format!("rillsql-save-no-open-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let mut model = WorkbenchModel::new(Shell::default());
    let file = sqlite_file(&dir, "notes.sqlite").await;
    model.connections.start_new(Engine::Sqlite);
    let draft = model.connections.editor.as_mut().unwrap();
    draft.name = "笔记".into();
    draft.file_path = file.display().to_string();
    let _ = model.dispatch("conn-save");
    let token = model.prepare_list_token(&services.tasks);
    let done = jobs::execute_job(&services, request_of(&mut model, token)).await;
    apply_done(&mut model, &services, "connections", done);
    assert_eq!(model.connections.rows.len(), 1);
    let id = model.connections.rows[0].saved.id.clone();
    assert!(services.connections.recent_ids(10).await.unwrap().is_empty(), "保存本身不连接");

    model.connections.selected = Some(id.clone());
    let _ = model.dispatch("conn-open");
    let token = model.prepare_list_token(&services.tasks);
    let done = jobs::execute_job(&services, request_of(&mut model, token)).await;
    apply_done(&mut model, &services, "connections", done);
    assert_eq!(services.connections.recent_ids(10).await.unwrap(), vec![id]);
    let _ = std::fs::remove_dir_all(dir);
}


// ------------------------------------------------------------ 连接启动台与全局模态（T3）

fn record_for(id: &str, name: &str, engine: Engine) -> ConnectionRecord {
    let mut draft = sample_draft(engine);
    draft.name = name.into();
    ConnectionRecord {
        saved: crate::connections::model::normalize(&draft, id.into()).unwrap(),
        ssh_host_key: None,
    }
}

fn seeded_model(rows: Vec<ConnectionRecord>) -> WorkbenchModel {
    let mut model = WorkbenchModel::new(Shell::default());
    model.connections.rows = rows;
    model.welcome.has_connections = !model.connections.rows.is_empty();
    model
}

/// 启动台、命令搜索、对象树空态和连接行打开的是同一个模态；关闭不保存并还焦来源。
#[test]
fn every_entry_opens_the_same_modal_and_cancel_leaves_no_draft() {
    let mut model = seeded_model(vec![record_for("pg", "Atlas", Engine::Postgres)]);

    // 命令搜索/菜单/快捷键：一条命令就是一份草稿、一个编辑面。
    let _ = model.dispatch("new-connection");
    assert!(model.conn_modal.open);
    assert_eq!(model.conn_modal.trigger.as_deref(), Some("new-connection"));
    assert!(model.connections.editor.as_ref().is_some_and(|draft| draft.id.is_none()));
    let restored = model.close_connection_modal().expect("开着才会关");
    assert_eq!(restored.0, crate::app::FocusTarget::Connections);
    assert!(!model.conn_modal.open);
    assert!(model.connections.editor.is_none(), "取消不保存也不留草稿");
    assert_eq!(model.connections.rows.len(), 1, "取消不动已保存配置");

    // 启动台主入口：记住触发控件，关闭后焦点回到这个按钮。
    let _ = model.dispatch("conn-new-launcher");
    assert_eq!(model.conn_modal.control, Some(ModalControl::LauncherNew));
    let _ = model.dispatch("conn-cancel");
    assert!(!model.conn_modal.open);
    assert_eq!(model.conn_modal.last_control, Some(ModalControl::LauncherNew));
    assert!(model.conn_modal.restore_origin, "视图下一帧把焦点还给触发按钮");

    // 连接行操作：草稿来自这一行，不先保存也不改已保存配置。
    model.connections.selected = Some("pg".into());
    let _ = model.dispatch("conn-edit");
    assert!(model.conn_modal.open);
    assert_eq!(model.connections.editor.as_ref().unwrap().id.as_deref(), Some("pg"));

    // 复制配置：草稿没有 id，保存时才会新建一条。
    let _ = model.dispatch("conn-duplicate");
    assert!(model.connections.editor.as_ref().unwrap().id.is_none());
    assert_eq!(model.connections.rows.len(), 1);
}

/// 真实的 Tab/Shift+Tab 属于 GPUI 的 focus trap（面板注册了 focus_trap）：
/// 按超过停靠点数量的次数也必须留在面板里，并且每个已渲染的控件都要被走到。
#[gpui_kit::test]
fn modal_tab_trap_keeps_focus_inside_and_visits_every_control(cx: &mut gpui_kit::TestAppContext) {
    let dir = std::env::temp_dir().join(format!("rillsql-modal-trap-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench(cx, services_at(&dir), false);
    cx.update(|cx| view.update(cx, |view, cx| view.perform("conn-engine:sqlite", cx)));
    cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();

    let (panel, controls, stops) = cx.update(|cx| {
        let view = view.read(cx);
        (
            view.modal_focus.clone(),
            view.modal_controls.keys().cloned().collect::<Vec<_>>(),
            view.model.modal_stops(),
        )
    });
    assert!(!controls.is_empty(), "模态控件应已登记");
    let expected: Vec<String> = stops.iter().filter_map(|stop| stop.control_id().map(str::to_owned)).collect();
    assert_eq!(controls.len(), expected.len(), "登记数与停靠点数应一致：{controls:?} vs {expected:?}");

    let mut seen: Vec<String> = Vec::new();
    for _ in 0..(stops.len() * 2) {
        cx.simulate_keystrokes(window, "tab");
        cx.update_window(window, |_, window, cx| {
            assert!(panel.contains_focused(window, cx), "Tab 逃出了模态面板");
            if let Some(focused) = window.focused(cx) {
                if let Some(id) = view.read(cx).modal_controls.iter().find(|(_, handle)| **handle == focused).map(|(id, _)| id.clone()) {
                    seen.push(id);
                }
            }
        })
        .unwrap();
    }
    for id in &expected {
        assert!(seen.contains(id), "Tab 走不到 {id}：{seen:?}");
    }

    // 反向同样留在面板里。
    for _ in 0..(stops.len() * 2) {
        cx.simulate_keystrokes(window, "shift-tab");
        cx.update_window(window, |_, window, cx| {
            assert!(panel.contains_focused(window, cx), "Shift+Tab 逃出了模态面板");
        })
        .unwrap();
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// 画出来的控件与 Tab 能到的控件是同一份清单：三种表单形状都核对一遍。
#[gpui_kit::test]
fn every_rendered_modal_control_is_a_tab_stop(cx: &mut gpui_kit::TestAppContext) {
    let dir = std::env::temp_dir().join(format!("rillsql-modal-rendered-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench(cx, services_at(&dir), false);
    let shapes = [
        ("postgres", Engine::Postgres),
        ("sqlite", Engine::Sqlite),
        ("mssql", Engine::Sqlserver),
    ];
    for (label, engine) in shapes {
        cx.update(|cx| {
            view.update(cx, |view, cx| {
                view.perform(&format!("conn-engine:{}", engine.as_str()), cx);
            })
        });
        if engine == Engine::Sqlserver {
            // 打开 TLS 校验 + CA 文件 + SSH：控件最多的那种形状。
            cx.update(|cx| {
                view.update(cx, |view, _| {
                    let draft = view.model.connections.editor.as_mut().unwrap();
                    draft.tls_mode = crate::connections::model::TlsMode::Verify;
                    draft.ca_mode = crate::connections::model::CaMode::File;
                    draft.ssh_on = true;
                    draft.ssh_auth = crate::connections::model::SshAuthKind::Key;
                })
            });
        }
        let (rendered, stops) = cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let view = view.read(cx);
            (view.model.conn_modal.rendered.clone(), view.model.modal_stops())
        })
        .unwrap();
        let expected: Vec<String> = stops.iter().filter_map(|stop| stop.control_id().map(str::to_owned)).collect();
        for id in &rendered {
            assert!(expected.contains(id), "{label}: 画出来的 {id} 不在 Tab 序列里");
        }
        assert_eq!(rendered.len(), expected.len(), "{label}: 画出来的控件数与停靠点数应一致");
        assert!(!rendered.is_empty(), "{label}: 模态应登记控件");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// 普通保存：关闭模态、不建连、不写最近使用。
#[tokio::test]
async fn plain_save_closes_the_modal_without_connecting() {
    let dir = std::env::temp_dir().join(format!("rillsql-t3-save-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let mut model = seeded_model(Vec::new());
    let file = sqlite_file(&dir, "notes.sqlite").await;
    let _ = model.dispatch("conn-engine:sqlite");
    assert!(model.conn_modal.open, "选引擎也是打开同一个模态");
    let draft = model.connections.editor.as_mut().unwrap();
    draft.name = "笔记".into();
    draft.file_path = file.display().to_string();

    let _ = model.dispatch("conn-save");
    assert!(!model.conn_modal.save_and_connect, "普通保存不连");
    let token = model.prepare_list_token(&services.tasks);
    let done = jobs::execute_job(&services, request_of(&mut model, token)).await;
    apply_done(&mut model, &services, "connections", done);

    assert!(!model.conn_modal.open, "保存成功关闭模态");
    assert_eq!(model.pending_len(), 0, "普通保存不排队连接");
    assert_eq!(model.notice_key, Some("common.save"));
    assert_eq!(model.connections.rows.len(), 1);
    assert!(services.connections.recent_ids(10).await.unwrap().is_empty(), "保存不算使用");
    let _ = std::fs::remove_dir_all(dir);
}

/// 保存并连接：保存成功后只排一次连接；连接失败保留动作供重试，不重复建连接。
#[tokio::test]
async fn save_and_connect_queues_one_open_and_keeps_the_action_for_retry() {
    let dir = std::env::temp_dir().join(format!("rillsql-t3-save-connect-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let mut model = seeded_model(Vec::new());
    let file = sqlite_file(&dir, "notes.sqlite").await;
    let _ = model.dispatch("conn-engine:sqlite");
    let draft = model.connections.editor.as_mut().unwrap();
    draft.name = "笔记".into();
    draft.file_path = file.display().to_string();

    let _ = model.dispatch("conn-save-connect");
    assert!(model.conn_modal.save_and_connect);
    let token = model.prepare_list_token(&services.tasks);
    let done = jobs::execute_job(&services, request_of(&mut model, token)).await;
    apply_done(&mut model, &services, "connections", done);

    assert!(!model.conn_modal.open);
    assert_eq!(model.connections.rows.len(), 1, "保存本身只写一条配置");
    assert_eq!(model.pending_len(), 1, "保存成功后只排一次连接");
    assert_eq!(model.take_pending(), Some(PendingJob::OpenConnection));
    let id = model.connections.rows[0].saved.id.clone();
    assert_eq!(model.connections.status.get(&id), Some(&LinkStatus::Connecting));
    assert_eq!(model.notice_key, Some("ui.conn.connecting"));

    // 连接失败：动作留到重试里，重试仍指向这条已保存的连接。
    model.connections.status.insert(id.clone(), LinkStatus::Failed);
    model.fail_pending_intents();
    assert!(model.connect_retry.is_some(), "失败不丢用户想做的事");
    assert!(model.retry_pending_connect());
    assert_eq!(model.connections.selected.as_deref(), Some(id.as_str()));
    assert_eq!(model.pending_len(), 1);
    assert_eq!(model.connections.rows.len(), 1, "重试不重复创建连接");
    let _ = std::fs::remove_dir_all(dir);
}

/// 测试失败：草稿与本次机密保留，错误落在字段旁，模态不关。
#[tokio::test]
async fn a_failed_test_keeps_the_draft_and_marks_the_field() {
    let dir = std::env::temp_dir().join(format!("rillsql-t3-test-fail-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let mut model = seeded_model(Vec::new());
    let _ = model.dispatch("conn-engine:sqlite");
    let missing = dir.join("missing.sqlite");
    model.connections.editor.as_mut().unwrap().file_path = missing.display().to_string();
    model.connections.secrets.password = "typed-only-here".into();

    let _ = model.dispatch("conn-test");
    let token = model.prepare_list_token(&services.tasks);
    let done = jobs::execute_job(&services, request_of(&mut model, token)).await;
    apply_done(&mut model, &services, "connections", done);

    assert!(model.conn_modal.open, "测试失败不关闭模态");
    assert!(model.connections.editor.is_some(), "失败保留草稿");
    assert_eq!(model.connections.secrets.password, "typed-only-here", "失败保留本次机密");
    assert!(!model.conn_modal.test_running, "失败收起“正在测试”");
    let issue = model.connections.issues.first().expect("失败关联字段");
    assert_eq!(issue.field, "file_path");
    assert!(!issue.detail.is_empty(), "字段旁保留原文");
    assert!(model.connections.rows.is_empty(), "测试不写配置");
    let _ = std::fs::remove_dir_all(dir);
}

/// 更早一次测试的迟到结果不能收走新一次仍在跑的指示。
#[tokio::test]
async fn an_older_test_result_cannot_clear_the_newer_running_indicator() {
    let dir = std::env::temp_dir().join(format!("rillsql-t3-test-race-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let mut model = seeded_model(Vec::new());
    let file = sqlite_file(&dir, "draft.sqlite").await;
    let _ = model.dispatch("conn-engine:sqlite");
    model.connections.editor.as_mut().unwrap().file_path = file.display().to_string();

    let _ = model.dispatch("conn-test");
    let first_token = model.prepare_list_token(&services.tasks);
    let first = request_of(&mut model, first_token);
    // 第二次测试取代第一次：它才是当前这一次。
    let _ = model.dispatch("conn-test");
    let second_token = model.prepare_list_token(&services.tasks);
    let second = request_of(&mut model, second_token);
    assert!(model.conn_modal.test_running && model.connections.test_running());

    let done = jobs::execute_job(&services, first).await;
    apply_done(&mut model, &services, "connections", done);
    assert!(model.conn_modal.test_running, "旧结果不能收走新一次的指示");
    assert!(model.connections.test_running());
    assert_ne!(model.connections.message_key, Some("connections.editor.test.ok"));

    let done = jobs::execute_job(&services, second).await;
    apply_done(&mut model, &services, "connections", done);
    assert!(!model.conn_modal.test_running);
    assert!(!model.connections.test_running());
    assert_eq!(model.connections.message_key, Some("connections.editor.test.ok"));
    let _ = std::fs::remove_dir_all(dir);
}

/// 打开数据/结构、树展开和连接行双击都先按需连接，连上后动作只重放一次。
#[test]
fn on_demand_connect_queues_the_action_and_replays_it_once() {
    let mut model = seeded_model(vec![record_for("pg", "Atlas", Engine::Postgres)]);
    model.object_spec = Some("pg|Atlas|app|public|users|postgres|dev".into());
    model.connections.selected = Some("pg".into());

    assert!(model.gate_on_demand("tree-open-data"), "未连接时先建连");
    assert_eq!(model.pending_len(), 1, "一次只排一个打开连接");
    assert_eq!(model.connections.status.get("pg"), Some(&LinkStatus::Connecting));
    assert_eq!(model.notice_key, Some("ui.conn.connecting"), "状态文字与指示点同时出现");
    assert_eq!(model.notice_detail, "Atlas");
    assert!(model.gate_on_demand("open-structure"), "在途连接复用在途操作而不是再开一次");
    assert_eq!(model.pending_len(), 1);
    assert_eq!(model.take_pending(), Some(PendingJob::OpenConnection));

    // 连上之后两条动作按顺序重放一次，之后就清空。
    model.connections.status.insert("pg".into(), LinkStatus::Connected);
    let ready = model.take_ready_intents();
    assert_eq!(ready.len(), 2);
    assert_eq!(ready[0].command(), Some("tree-open-data"));
    assert_eq!(ready[1].command(), Some("open-structure"));
    assert!(model.take_ready_intents().is_empty(), "只重放一次");

    // 已连接的入口直接执行，不再建连。
    assert!(!model.gate_on_demand("tree-open-data"));
    assert_eq!(model.pending_len(), 0, "已连接不再排队");
}

/// 启动台的筛选浮层靠近视口边缘时向内翻转，不会画出窗口。
#[test]
fn filter_popover_flips_inside_the_viewport() {
    let (x, _) = super::menus::place_popup(880., 300., 220., 200., 900., 600.);
    assert!(x + 220. <= 900. - 8. + 0.01, "右侧碰撞向左翻: {x}");
    assert!(x >= 8.);
    let (_, y) = super::menus::place_popup(100., 560., 220., 200., 900., 600.);
    assert!(y + 200. <= 600. - 8. + 0.01, "底部碰撞向上翻: {y}");
    assert!(y >= 8.);
}

/// 关系图页头的"返回"由外壳消费：回到原工作项，而不是留在一个点了没反应的空页上。
#[gpui_kit::test]
fn diagram_header_back_returns_to_the_previous_work_item(cx: &mut gpui_kit::TestAppContext) {
    // 新建查询会写一次会话，后台线程会唤醒调度器；放行这次停车，避免判成非确定性。
    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-diagram-back-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench_sized(cx, services_at(&dir), false, 1360., 800.);
    cx.update(|cx| {
        view.update(cx, |view, cx| {
            view.perform("new-query", cx);
            view.perform("open-diagram:notes|main||图A", cx);
        });
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| assert_eq!(view.read(cx).model.shell.page, AppPage::Diagram));

    cx.update_window(window, |_, window, cx| {
        window.click("diagram-back", cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        let model = &view.read(cx).model;
        assert_eq!(model.shell.page, AppPage::Workspace, "返回要回到原工作项");
        assert!(model.tabs.iter().any(|tab| tab.kind.is_query()), "原来的查询工作项还在");
        assert_eq!(model.diagram_spec.as_deref(), Some("notes|main||图A"), "图自己的范围留在图上");
    });
    let _ = std::fs::remove_dir_all(&dir);
}

/// 执行查询也要先有连接：未连接时排队一次连接，连上后重放这次执行，已连接直接执行。
#[test]
fn running_a_query_on_a_closed_connection_connects_then_replays_the_run() {
    let mut model = seeded_model(vec![record_for("pg", "Atlas", Engine::Postgres)]);
    model.open_bound_query(
        "select 1".into(),
        Some("pg".into()),
        Some("Atlas".into()),
        Some("app".into()),
        Some("postgres".into()),
    );
    assert!(model.gate_on_demand("execute-query"), "连接没打开时先建连");
    assert_eq!(model.pending_len(), 1, "只排一次打开连接");
    assert_eq!(model.notice_key, Some("ui.conn.connecting"));
    assert_eq!(model.take_pending(), Some(PendingJob::OpenConnection));

    model.connections.status.insert("pg".into(), LinkStatus::Connected);
    let ready = model.take_ready_intents();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].command(), Some("execute-query"), "连上后重放这一次执行");
    assert_eq!(ready[0].connection_id, "pg");

    assert!(!model.gate_on_demand("execute-query"), "已连接时照常执行，不再建连");
    assert_eq!(model.pending_len(), 0);
}

/// 执行当前语句/选区与整条执行走同一条按需连接续接。
#[test]
fn every_run_command_shares_the_same_gate() {
    let mut model = seeded_model(vec![record_for("pg", "Atlas", Engine::Postgres)]);
    model.open_bound_query(String::new(), Some("pg".into()), Some("Atlas".into()), None, Some("postgres".into()));
    for command in ["run-current", "run-selection"] {
        assert!(model.gate_on_demand(command), "{command} 也要先建连");
        let intent = model.take_ready_intents();
        assert!(intent.is_empty(), "未连接时先不重放");
        let queued = model.connect_intents.iter().find(|queued| queued.replay.as_deref() == Some(command));
        assert!(queued.is_some(), "{command} 应排队等待重放");
    }
}

/// 查询页自己的「运行」（工具条按钮与「更多」里的运行项）不能绕过按需连接这道门：
/// 未连接时只排一次打开连接，连上后这次执行被重放一次。
#[gpui_kit::test]
fn the_query_panes_own_run_button_gates_on_demand_connect(cx: &mut gpui_kit::TestAppContext) {
    let (window, view, dir) = two_sqlite_connections(cx);
    wait_for_shell_ready(cx, window, &view);
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, _| {
            // 「审计」这条连接只保存没打开：闸门应先建连，再把这次执行续上。
            view.model.open_bound_query(
                "select label from item".into(),
                Some("audit".into()),
                Some("审计".into()),
                Some("main".into()),
                Some("sqlite".into()),
            );
        });
        window.render_frame(cx);
    })
    .unwrap();
    let runs_before = cx.update(|cx| view.read(cx).query_runs(cx));

    // 工具条「运行」按下的就是查询页自己的命令入口，这里直接驱动它（不是 perform）。
    cx.update(|cx| {
        view.update(cx, |view, cx| view.query.update(cx, |pane, cx| pane.command("execute-query", cx)));
    });
    cx.update(|cx| {
        let runs = view.read(cx).query_runs(cx);
        view.update(cx, |view, _| {
            assert_eq!(view.model.connections.status.get("audit"), Some(&LinkStatus::Connecting), "未连接时先建连");
            // 走的是外壳的门：连接状态、提示与排队都只有 Workbench::perform 会写。
            assert_eq!(view.model.notice_key, Some("ui.conn.connecting"), "面板把运行交回了外壳的按需连接");
            let queued: Vec<&str> = view.model.connect_intents.iter().filter_map(|intent| intent.replay.as_deref()).collect();
            assert_eq!(queued, vec!["execute-query"], "这次执行排队等待重放，且只排队一次");
            assert_eq!(runs, runs_before, "没连上之前不执行");
        });
    });

    // 真的把这条连接打开：连上后这次执行被重放，且只重放一次。
    assert!(
        pump_until(cx, window, &view, |view, cx| {
            view.model.connections.status.get("audit") == Some(&LinkStatus::Connected) && view.query_runs(cx) == runs_before + 1
        }),
        "连上后这次执行要被重放一次"
    );
    cx.update(|cx| {
        let model = &view.read(cx).model;
        assert!(model.connect_intents.is_empty(), "重放完不留排队");
        assert_eq!(view.read(cx).query_runs(cx), runs_before + 1, "只重放一次，没有重复执行");
    });

    // 等这次执行收尾，工具条回到「运行」按钮。
    assert!(pump_until(cx, window, &view, |view, cx| !view.query.read(cx).busy()));
    // 工具条那颗「运行」按钮接的也是这条入口：已连接时直接执行，不再排连接。
    cx.update_window(window, |_, window, cx| window.click("query-act-execute-query", cx)).unwrap();
    assert!(
        pump_until(cx, window, &view, |view, cx| view.query_runs(cx) == runs_before + 2),
        "工具条的运行按钮走同一条执行路径"
    );
    // 「更多」菜单里的「执行当前语句」走同一个入口。
    assert!(pump_until(cx, window, &view, |view, cx| !view.query.read(cx).busy()));
    cx.update(|cx| {
        view.update(cx, |view, cx| view.query.update(cx, |pane, cx| pane.command("run-current", cx)));
    });
    assert!(
        pump_until(cx, window, &view, |view, cx| view.query_runs(cx) == runs_before + 3),
        "更多菜单的当前语句走同一条执行路径"
    );
    cx.update(|cx| {
        let model = &view.read(cx).model;
        assert!(model.connect_intents.is_empty(), "已连接时不再排连接");
    });
    // 大纲里每条语句旁的运行图标同样交回外壳；外壳要认得它并转发回来，不能静默丢掉。
    assert!(pump_until(cx, window, &view, |view, cx| !view.query.read(cx).busy()));
    cx.update(|cx| {
        view.update(cx, |view, cx| view.query.update(cx, |pane, cx| pane.command("outline-run:1", cx)));
    });
    assert!(
        pump_until(cx, window, &view, |view, cx| view.query_runs(cx) == runs_before + 4),
        "大纲的运行图标走同一条执行路径"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// 连接模态打开时，全局快捷键不能在背景里换页或开新标签；关闭后同一动作恢复。
#[gpui_kit::test]
fn the_connection_modal_blocks_global_shortcuts(cx: &mut gpui_kit::TestAppContext) {
    let (window, view, dir) = sqlite_notes_workbench(cx, true);
    wait_for_shell_ready(cx, window, &view);
    let tabs_before = cx.update(|cx| view.read(cx).model.tabs.len());
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.perform("new-connection", cx));
        window.render_frame(cx);
    })
    .unwrap();
    assert!(cx.update(|cx| view.read(cx).model.conn_modal.open), "模态开着");
    // Cmd+T / Cmd+Enter 这类全局动作不能在模态背后开标签或执行。
    cx.update_window(window, |_, window, cx| {
        window.dispatch_action(Box::new(crate::app::NewQuery), cx);
        window.dispatch_action(Box::new(crate::app::ExecuteQuery), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        let model = &view.read(cx).model;
        assert!(model.conn_modal.open, "快捷键不能关掉模态");
        assert_eq!(model.tabs.len(), tabs_before, "模态开着时不响应全局动作");
        assert!(!model.commands.iter().any(|id| id == "new-query" || id == "execute-query"), "动作没有进入工作区");
    });
    // 关掉模态后同一批动作恢复。
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.perform("conn-cancel", cx));
        window.render_frame(cx);
        window.dispatch_action(Box::new(crate::app::NewQuery), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        assert!(!view.read(cx).model.conn_modal.open);
        assert!(view.read(cx).model.tabs.len() > tabs_before, "关闭模态后快捷键照常");
    });
    let _ = std::fs::remove_dir_all(dir);
}

/// 保存并连接要带着刚输入的密码去连接：关闭模态会清空表单机密，不能把空密码交给 OpenConnection。
#[tokio::test]
async fn save_and_connect_carries_the_typed_password_into_open() {
    let dir = std::env::temp_dir().join(format!("rillsql-t3-secrets-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let mut model = seeded_model(vec![record_for("pg", "Atlas", Engine::Postgres)]);
    model.connections.selected = Some("pg".into());
    let _ = model.dispatch("conn-edit");
    assert!(model.conn_modal.open);
    // 这条配置没有保存过密码：本次输入的密码只在表单里。
    model.connections.editor.as_mut().unwrap().save_password = false;
    model.connections.secrets.password = "typed-only-here".into();

    let _ = model.dispatch("conn-save-connect");
    let token = model.prepare_list_token(&services.tasks);
    let done = jobs::execute_job(&services, request_of(&mut model, token)).await;
    apply_done(&mut model, &services, "connections", done);

    assert!(!model.conn_modal.open, "保存成功关闭模态");
    assert_eq!(model.pending_len(), 1);
    let job = model.take_pending().expect("保存并连接应排队一次打开连接");
    assert_eq!(job, PendingJob::OpenConnection);
    let request = jobs::job_request(&model, job, token);
    assert_eq!(request.secrets.password, "typed-only-here", "打开连接要用刚输入的密码");
    assert_eq!(request.selected.as_deref(), Some("pg"), "重试仍指向同一条连接");
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------ 连接分组与虚拟化对象浏览器（T4）

fn group(id: &str, name: &str, position: i64, collapsed: bool) -> crate::connections::groups::ConnectionGroup {
    crate::connections::groups::ConnectionGroup { id: id.into(), name: name.into(), position, collapsed }
}

fn grouped_model(rows: Vec<ConnectionRecord>, groups: Vec<crate::connections::groups::ConnectionGroup>, members: &[(&str, &str)]) -> WorkbenchModel {
    let mut model = seeded_model(rows);
    let members = members.iter().map(|(connection, group)| ((*connection).to_owned(), (*group).to_owned())).collect();
    model.apply_groups(groups, members);
    model
}

fn item_kinds(model: &WorkbenchModel) -> Vec<String> {
    model.explorer_rows().iter().map(|item| item.id()).collect()
}

#[test]
fn explorer_flat_rows_group_connections_and_keep_empty_groups_visible() {
    let rows = vec![
        record_for("c1", "Atlas", Engine::Postgres),
        record_for("c2", "Borealis", Engine::Postgres),
        record_for("c3", "Cygnus", Engine::Postgres),
    ];
    let groups = vec![group("g1", "空组", 0, false), group("g2", "生产", 1, false)];
    let model = grouped_model(rows, groups, &[("c1", "g2")]);
    let ids = item_kinds(&model);
    assert_eq!(
        ids,
        vec![
            "group:g1".to_owned(),
            "group:g2".to_owned(),
            "conn:c1".to_owned(),
            "conn:c2".to_owned(),
            "conn:c3".to_owned(),
        ],
        "分组在顶层连接之前，空组照样出现，成员连接在组标题下"
    );

    // 折叠只收起成员，标题与数量还在。
    let mut model = model;
    let _ = model.dispatch("group-toggle:g2");
    let ids = item_kinds(&model);
    assert_eq!(ids, vec!["group:g1".to_owned(), "group:g2".to_owned(), "conn:c2".to_owned(), "conn:c3".to_owned()]);
}

#[test]
fn explorer_ids_are_stable_and_dense_under_hundreds_of_connections() {
    let rows: Vec<ConnectionRecord> = (0..300)
        .map(|index| record_for(&format!("c{index:03}"), &format!("连接 {index:03}"), Engine::Postgres))
        .collect();
    let model = grouped_model(rows, vec![group("g1", "生产", 0, false)], &[("c000", "g1")]);
    let items = model.explorer_rows();
    assert_eq!(items.len(), 301, "一行分组 + 300 条连接（没有会话就没有树行）");
    let ids: std::collections::BTreeSet<String> = items.iter().map(|item| item.id()).collect();
    assert_eq!(ids.len(), items.len(), "同一序列里 id 不重复");
    assert_eq!(items[1].id(), "conn:c000");
    assert_eq!(items.last().unwrap().id(), "conn:c299");

    // 元素数量不随总节点线性增长：视口 480px 只创建视口内（含 overscan）的行。
    let heights = model.explorer_heights(&items);
    let window = super::explorer::window_rows(&heights, 0., 480., super::explorer::OVERSCAN_ROWS);
    let created = window.range.len();
    assert!(created < 40, "300 条连接时只创建 {created} 个元素");
    let small: Vec<ConnectionRecord> = (0..20)
        .map(|index| record_for(&format!("s{index:03}"), &format!("小 {index:03}"), Engine::Postgres))
        .collect();
    let small_model = grouped_model(small, Vec::new(), &[]);
    let small_items = small_model.explorer_rows();
    let small_heights = small_model.explorer_heights(&small_items);
    let small_window = super::explorer::window_rows(&small_heights, 0., 480., super::explorer::OVERSCAN_ROWS);
    assert!(
        created <= small_window.range.len() + 4,
        "视图元素数量与连接总数无关（300 条 {created}，20 条 {}）",
        small_window.range.len()
    );
}

#[test]
fn group_commands_share_one_pending_job_and_reject_empty_or_missing_targets() {
    let mut model = grouped_model(
        vec![record_for("c1", "Atlas", Engine::Postgres)],
        vec![group("g1", "生产", 0, false)],
        &[],
    );

    // 新建：本地校验名称，通过后才排队一条落库命令。
    let _ = model.dispatch("group-new");
    assert!(model.group_prompt.is_some());
    model.set_field(DraftField::GroupName, "  ".into());
    let _ = model.dispatch("group-save");
    assert!(model.group_prompt.as_ref().unwrap().error.is_some(), "空名当场拒绝");
    assert!(model.pending_group_op.is_none());
    model.set_field(DraftField::GroupName, "  夜间巡检 ".into());
    let _ = model.dispatch("group-save");
    assert_eq!(model.pending_group_op, Some(super::model::GroupOp::Save { id: None, name: "夜间巡检".into() }));
    assert_eq!(model.take_pending(), Some(PendingJob::GroupOp));
    model.pending_group_op = None;

    // 重命名：输入面板预填原名，确认后带上分组 id 走同一条保存命令。
    let _ = model.dispatch("group-rename:g1");
    assert_eq!(model.group_prompt.as_ref().unwrap().text, "生产");
    model.set_field(DraftField::GroupName, "生产库".into());
    let _ = model.dispatch("group-save");
    assert_eq!(
        model.pending_group_op,
        Some(super::model::GroupOp::Save { id: Some("g1".into()), name: "生产库".into() })
    );
    model.pending_group_op = None;
    model.pending.clear();

    // 折叠：本地立即生效，同一条命令负责持久化。
    let _ = model.dispatch("group-toggle:g1");
    assert!(model.groups[0].collapsed);
    assert_eq!(model.pending_group_op, Some(super::model::GroupOp::Collapsed { id: "g1".into(), collapsed: true }));
    assert_eq!(model.take_pending(), Some(PendingJob::GroupOp));
    model.pending_group_op = None;
    let _ = model.dispatch("group-toggle:g1");
    model.pending_group_op = None;
    model.pending.clear();

    // 归组与移回顶层：同一个命令，目标为空就是顶层。
    let _ = model.dispatch("group-assign:c1\u{1}g1");
    assert_eq!(
        model.pending_group_op,
        Some(super::model::GroupOp::Assign { connection: "c1".into(), group: Some("g1".into()) })
    );
    model.pending_group_op = None;
    let _ = model.dispatch("group-assign:c1\u{1}");
    assert_eq!(
        model.pending_group_op,
        Some(super::model::GroupOp::Assign { connection: "c1".into(), group: None })
    );
    model.pending_group_op = None;
    // 指向不存在的分组或连接：不产生命令，也就不可能把连接弄丢。
    let _ = model.dispatch("group-assign:c1\u{1}ghost");
    assert!(model.pending_group_op.is_none());
    let _ = model.dispatch("group-assign:nobody\u{1}g1");
    assert!(model.pending_group_op.is_none());

    // 删除先进确认层：说明影响，第二次确认才落库。
    let _ = model.dispatch("group-delete:g1");
    assert_eq!(model.group_delete.as_deref(), Some("g1"));
    let _ = model.dispatch("group-delete-cancel");
    assert!(model.group_delete.is_none());
    assert!(model.pending_group_op.is_none());
    let _ = model.dispatch("group-delete:g1");
    let _ = model.dispatch("group-delete-confirm");
    assert_eq!(model.pending_group_op, Some(super::model::GroupOp::Delete { id: "g1".into() }));
}

#[test]
fn unknown_group_ids_read_as_ungrouped_and_group_metadata_stays_out_of_connections() {
    let rows = vec![record_for("c1", "Atlas", Engine::Postgres), record_for("c2", "Borealis", Engine::Mysql)];
    let model = grouped_model(rows, vec![group("g1", "生产", 0, false)], &[("c1", "g1"), ("c2", "ghost")]);
    assert_eq!(model.group_of("c1"), Some("g1"));
    assert_eq!(model.group_of("c2"), None, "指向已删除分组按未分组读");
    let ids = item_kinds(&model);
    assert!(ids.contains(&"conn:c2".to_owned()), "未分组连接留在顶层");
    let layout = model.group_layout();
    assert_eq!(layout.ungrouped, vec!["c2".to_owned()]);
    // 连接定义本身没有分组字段：序列化里也不会出现分组 id。
    let blob = serde_json::to_string(&model.connections.rows[0].saved).unwrap();
    assert!(!blob.contains("g1"), "{blob}");
}

#[test]
fn explorer_keyboard_walks_groups_connections_and_tree_rows() {
    use crate::catalog::model::ObjectRow;
    let orders = ObjectRow::table("orders", Some(1));
    let pg = nav_connection(Engine::Postgres, "pg", "Atlas", vec![nav_database("shop", vec![orders])]);
    let mut model = grouped_model(vec![record_for("pg", "Atlas", Engine::Postgres)], vec![group("g1", "生产", 0, false)], &[("pg", "g1")]);
    model.connections.status.insert("pg".to_owned(), LinkStatus::Connected);
    model.apply_navigation(&[pg], false);
    let connection_id = crate::catalog::hierarchy::connection_node_id("pg");
    let _ = model.dispatch(&format!("nav-toggle:{connection_id}"));

    model.set_explorer_current(super::explorer::ExplorerRow::Group("g1".into()));
    // 回车折叠分组：本地状态与后台命令同时到位。
    let _ = model.nav_key(NavKey::Enter);
    assert!(model.groups[0].collapsed);
    assert_eq!(model.pending_group_op, Some(super::model::GroupOp::Collapsed { id: "g1".into(), collapsed: true }));
    model.pending_group_op = None;
    model.pending.clear();
    // 右方向键展开回来，下方向键落到组内连接行。
    let _ = model.nav_step(true);
    assert!(!model.groups[0].collapsed);
    model.pending_group_op = None;
    model.pending.clear();
    let _ = model.nav_key(NavKey::ArrowDown);
    assert_eq!(model.explorer_current_row(), Some(super::explorer::ExplorerRow::Connection("pg".into())));
    assert_eq!(model.connections.selected.as_deref(), Some("pg"));
    // 再往下就是这条连接的树行。
    let _ = model.nav_key(NavKey::ArrowDown);
    assert!(matches!(model.explorer_current_row(), Some(super::explorer::ExplorerRow::Node(_))));
    assert_eq!(model.executions, 0, "键盘导航不执行任何 SQL");
}

#[test]
fn searching_reads_only_loaded_nodes_and_clearing_restores_the_tree() {
    use crate::catalog::model::ObjectRow;
    let orders = ObjectRow::table("orders", Some(1));
    let mut pg = nav_connection(Engine::Postgres, "pg", "Atlas", vec![nav_database("shop", vec![orders]), nav_database("cold", Vec::new())]);
    // “cold”库还没展开过：范围总量为 0 表示从没读过，筛选不能去猜它的对象。
    pg.databases[1].schemas = Vec::new();
    let mut model = grouped_model(
        vec![record_for("pg", "Atlas", Engine::Postgres), record_for("other", "Borealis", Engine::Postgres)],
        vec![group("g1", "生产", 0, false), group("g2", "冷库组", 1, false)],
        &[("pg", "g1"), ("other", "g2")],
    );
    model.connections.status.insert("pg".to_owned(), LinkStatus::Connected);
    model.apply_navigation(&[pg], false);
    let connection_id = crate::catalog::hierarchy::connection_node_id("pg");
    let _ = model.dispatch(&format!("nav-toggle:{connection_id}"));
    let expanded = model.explorer_rows().len();

    model.set_field(DraftField::ObjectFilter, "ord".into());
    let rows = model.explorer_rows();
    let ids: Vec<String> = rows.iter().map(|item| item.id()).collect();
    assert!(ids.contains(&"conn:pg".to_owned()));
    let nav_labels: Vec<String> = rows
        .iter()
        .filter_map(|item| item.nav_row().map(|row| row.label.clone()))
        .collect();
    assert!(nav_labels.contains(&"shop".to_owned()), "命中对象的库作为祖先保留");
    assert!(nav_labels.contains(&"orders".to_owned()));
    assert!(!nav_labels.contains(&"cold".to_owned()), "没读过的库不参与筛选");
    assert!(ids.contains(&"group:g1".to_owned()), "有命中的分组标题保留");
    assert!(!ids.contains(&"group:g2".to_owned()), "没有命中的分组不出现");
    assert!(!ids.contains(&"conn:other".to_owned()));

    model.set_field(DraftField::ObjectFilter, String::new());
    assert_eq!(model.explorer_rows().len(), expanded, "清除筛选恢复原样");
}

#[test]
fn filtering_to_nothing_shows_one_clear_no_match_row() {
    let model_rows = vec![record_for("pg", "Atlas", Engine::Postgres)];
    let mut model = grouped_model(model_rows, vec![group("g1", "生产", 0, false)], &[("pg", "g1")]);
    model.set_field(DraftField::ObjectFilter, "zzz".into());
    let ids = item_kinds(&model);
    assert_eq!(ids, vec!["explorer-no-match".to_owned()], "无命中时只剩一条提示行");
    assert_eq!(model.nav_empty_key("pg"), Some("ui.explorer.noMatch"));
}

#[gpui_kit::test]
fn hundreds_of_connections_render_only_the_visible_explorer_rows(cx: &mut gpui_kit::TestAppContext) {
    let dir = std::env::temp_dir().join(format!("rillsql-shell-many-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let (window, view) = open_workbench_sized(cx, Arc::clone(&services), false, 1360., 800.);
    cx.update(|cx| {
        view.update(cx, |view, cx| {
            let rows: Vec<ConnectionRecord> = (0..300)
                .map(|index| record_for(&format!("c{index:03}"), &format!("连接 {index:03}"), Engine::Postgres))
                .collect();
            view.model.connections.rows = rows;
            view.model.apply_groups(vec![group("g1", "生产", 0, false)], [("c000".to_owned(), "g1".to_owned())].into_iter().collect());
            view.model.shell.sidebar_open = true;
            cx.notify();
        });
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("side-c000").is_some(), "视口内的连接行要画出来");
        assert!(window.try_find("side-group-g1").is_some(), "分组标题可见");
        let mut created = 0;
        for index in 0..300 {
            if window.try_find(format!("side-c{index:03}")).is_some() {
                created += 1;
            }
        }
        assert!(created > 0 && created < 60, "300 条连接只创建 {created} 个行元素");
        println!(
            "HUNDREDS top rows {created} viewport {} max_offset {}",
            f32::from(view.read(cx).explorer_scroll.bounds().size.height),
            f32::from(view.read(cx).explorer_scroll.max_offset().y)
        );
        // 行高就是密度 token：连接行 row_h、分组标题 tree_row_h，没有被 flex 压扁。
        assert_eq!(f32::from(window.try_find("side-c000").unwrap().bounds().size.height), 26.);
        assert_eq!(f32::from(window.try_find("side-group-g1").unwrap().bounds().size.height), 23.);
        assert!(window.try_find("side-c299").is_none(), "视口外的连接行不创建元素");
        // 滚到底：窗口跟着滚动重算，远处的行才被创建，顶部的行被回收。
        window.scroll(
            "explorer-list",
            gpui_kit::ScrollDelta::Pixels(gpui_kit::Point::new(gpui_kit::px(0.), gpui_kit::px(-9000.))),
            cx,
        );
        window.render_frame(cx);
        // 再滚一次并重绘一帧：滚动到边界后视图才把最后几行算进窗口。
        window.scroll(
            "explorer-list",
            gpui_kit::ScrollDelta::Pixels(gpui_kit::Point::new(gpui_kit::px(0.), gpui_kit::px(-9000.))),
            cx,
        );
        window.render_frame(cx);
        for _ in 0..2 {
            window.render_frame(cx);
        }
        let mut created = 0;
        for index in 0..300 {
            if window.try_find(format!("side-c{index:03}")).is_some() {
                created += 1;
            }
        }
        assert!(created < 60, "滚到底后仍只创建 {created} 个行元素");
    })
    .unwrap();
    let _ = std::fs::remove_dir_all(dir);
}
/// 分组标题右键与拖拽归组：两条鼠标路径都发同一条命令，落库只在后台一处。
#[gpui_kit::test]
fn group_row_menu_and_connection_drag_share_the_same_move_command(cx: &mut gpui_kit::TestAppContext) {
    let dir = std::env::temp_dir().join(format!("rillsql-shell-drag-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let (window, view) = open_workbench_sized(cx, Arc::clone(&services), false, 1360., 800.);
    cx.update(|cx| {
        view.update(cx, |view, cx| {
            view.model.connections.rows = vec![record_for("pg", "Atlas", Engine::Postgres)];
            view.model.apply_groups(
                vec![group("g1", "生产库", 0, false)],
                [("pg".to_owned(), "g1".to_owned())].into_iter().collect(),
            );
            view.model.shell.sidebar_open = true;
            cx.notify();
        });
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        // 分组标题右键：菜单属于被点的分组，命令带上稳定 id。
        window.right_click("side-group-g1", cx);
        assert!(window.try_find("group-chevron-g1").is_some(), "分组标题保留展开箭头");
        view.update(cx, |view, _| {
            let menu = view.model.menu.as_ref().expect("分组菜单");
            let commands: Vec<String> = super::menus::flatten_items(&menu.items).iter().map(|item| item.command()).collect();
            assert!(commands.contains(&"group-rename:g1".to_owned()), "{commands:?}");
            assert!(commands.contains(&"group-delete:g1".to_owned()), "{commands:?}");
            assert_eq!(view.model.group_menu.as_deref(), Some("g1"));
        });
        view.update(cx, |view, cx| {
            view.model.menu = None;
            cx.notify();
        });
        window.render_frame(cx);
        // 连接右键菜单里有“移动到分组”，子项带目标分组 id；已经在组内时另有移出项。
        window.right_click("side-pg", cx);
        view.update(cx, |view, _| {
            let menu = view.model.menu.as_ref().expect("连接菜单");
            let branch = menu.items.iter().find(|item| item.id == "submenu-move-group").expect("移动到分组子菜单");
            let commands: Vec<String> = branch.children.iter().map(|item| item.command()).collect();
            assert!(commands.contains(&"group-assign:pg\u{1}g1".to_owned()), "{commands:?}");
            assert!(commands.iter().any(|command| command == "group-assign:pg\u{1}"), "已在组内要有移出项：{commands:?}");
        });
        view.update(cx, |view, cx| {
            view.model.menu = None;
            cx.notify();
        });
        window.render_frame(cx);
    })
    .unwrap();
    let _ = std::fs::remove_dir_all(dir);
}
/// 拖拽归组走真实鼠标路径：从连接行拖到分组标题放手，落同一条 assign 命令。
#[gpui_kit::test]
fn dragging_a_connection_onto_a_group_row_queues_the_same_assign_command(cx: &mut gpui_kit::TestAppContext) {
    let dir = std::env::temp_dir().join(format!("rillsql-shell-dnd-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let (window, view) = open_workbench_sized(cx, Arc::clone(&services), false, 1360., 800.);
    // 工作台启动会在 tokio 侧登记连接状态；拖放触发后也可能排入分组任务。
    // 允许测试执行器停放并在拖放后排空，避免后台线程与前台断言交错被判为非确定。
    cx.executor().allow_parking();
    cx.update(|cx| {
        view.update(cx, |view, cx| {
            view.model.connections.rows = vec![record_for("pg", "Atlas", Engine::Postgres)];
            view.model.apply_groups(vec![group("g1", "生产库", 0, false)], Default::default());
            view.model.shell.sidebar_open = true;
            cx.notify();
        });
    });
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.drag_to("side-pg", "side-group-g1", cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
    view.read_with(cx, |view, _| {
        assert_eq!(
            view.model.pending_group_op,
            Some(super::model::GroupOp::Assign { connection: "pg".into(), group: Some("g1".into()) }),
            "拖拽与右键菜单落同一条命令"
        );
    });
    let _ = std::fs::remove_dir_all(dir);
}
/// 资源管理器行：长名称可读、状态写成文字、展开箭头在位，右键仍绑定这一行。
#[gpui_kit::test]
fn explorer_rows_keep_long_names_status_chevrons_and_object_actions(cx: &mut gpui_kit::TestAppContext) {
    let (window, view, dir) = two_sqlite_connections(cx);
    wait_for_shell_ready(cx, window, &view);
    let long_name = "客户订单归档与结算连接的完整名称".repeat(3);
    let status_text = cx.update(|cx| view.read(cx).model.text("connections.status.connected"));
    cx.update(|cx| {
        view.update(cx, |view, cx| {
            if let Some(row) = view.model.connections.rows.iter_mut().find(|row| row.saved.id == "notes") {
                row.saved.name = long_name.clone();
            }
            cx.notify();
        });
    });
    let object_id = wait_for_nav_object(cx, window, &view, "item");
    assert!(!object_id.is_empty(), "目录树读到表节点");
    {
        let connection = crate::catalog::hierarchy::connection_node_id("notes");
        let now = cx.update(|cx| view.read(cx).model.nav.is_expanded(&connection));
        cx.update_window(window, |_, _, cx| {
            view.update(cx, |view, cx| view.perform(&format!("nav-toggle:{connection}"), cx));
        })
        .unwrap();
        let after = cx.update(|cx| view.read(cx).model.nav.is_expanded(&connection));
        assert!(after, "nav-toggle 应展开连接（之前 {now}）");
    }
    // 展开对象种类分组，让对象行进入资源管理器。
    let groups: Vec<String> = cx.update(|cx| {
        view.read(cx)
            .model
            .nav
            .visible()
            .into_iter()
            .filter(|row| row.kind == crate::catalog::hierarchy::NavKind::Group)
            .map(|row| row.id)
            .collect()
    });
    for group in groups {
        cx.update_window(window, |_, _, cx| {
            view.update(cx, |view, cx| view.perform(&format!("nav-toggle:{group}"), cx));
        })
        .unwrap();
    }
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let row = window.try_find("side-notes").expect("连接行");
        let label = row.label().expect("无障碍名称").to_owned();
        assert!(label.contains(&long_name), "完整名称仍要可读：{label}");
        let description = row.description().expect("状态说明").to_owned();
        assert!(description.contains(&status_text), "状态不只靠圆点颜色：{description}");
        assert!(f32::from(row.bounds().size.width) <= f32::from(window.find("sidebar").bounds().size.width));
        assert!(window.try_find("nav-chevron-notes").is_some(), "连接行保留展开箭头");
        let object_row = gpui_kit::SharedString::from(format!("nav-{object_id}"));
        assert!(window.try_find(object_row.clone()).expect("对象行").visible());
        window.right_click(object_row, cx);
    })
    .unwrap();
    cx.update(|cx| {
        view.update(cx, |view, _| {
            assert_eq!(view.model.nav_menu_node.as_deref(), Some(object_id.as_str()), "菜单只属于被点的这一行");
            let menu = view.model.menu.as_ref().expect("对象菜单");
            let commands: Vec<String> = super::menus::flatten_items(&menu.items).iter().map(|item| item.command()).collect();
            assert!(commands.contains(&"tree-open-data".to_owned()), "{commands:?}");
            assert!(commands.contains(&"open-structure".to_owned()), "{commands:?}");
        });
    });
    let _ = std::fs::remove_dir_all(dir);
}
#[test]
fn empty_databases_and_connection_errors_have_their_own_explorer_rows() {
    use crate::catalog::model::ObjectRow;
    use crate::catalog::NavConnection;
    // 已连接但一条对象都没有：说明行用“没有对象”，不是错误、也不是无命中。
    let empty = NavConnection::new("idle", "空库连接", Engine::Postgres, crate::catalog::model::ConnStatus::Connected);
    let mut model = grouped_model(vec![record_for("idle", "空库连接", Engine::Postgres)], Vec::new(), &[]);
    model.connections.status.insert("idle".to_owned(), LinkStatus::Connected);
    model.apply_navigation(&[empty], false);
    let connection = crate::catalog::hierarchy::connection_node_id("idle");
    let _ = model.dispatch(&format!("nav-toggle:{connection}"));
    let notes: Vec<&'static str> = model
        .explorer_rows()
        .iter()
        .filter_map(|item| match item {
            super::explorer::ExplorerItem::Note { key, .. } => Some(*key),
            _ => None,
        })
        .collect();
    assert_eq!(notes, vec!["ui.explorer.noObjects"]);

    // 连接读取失败：原因只写在这一条连接下面，带重试；同连接的对象行照旧保留。
    let mut broken = NavConnection::new("pg", "Atlas", Engine::Postgres, crate::catalog::model::ConnStatus::Lost);
    broken.error = Some("连接已断开".to_owned());
    broken.databases = vec![nav_database("shop", vec![ObjectRow::table("orders", Some(1))])];
    let mut model = grouped_model(vec![record_for("pg", "Atlas", Engine::Postgres)], Vec::new(), &[]);
    model.apply_navigation(&[broken], false);
    let errors: Vec<String> = model
        .explorer_rows()
        .iter()
        .filter_map(|item| match item {
            super::explorer::ExplorerItem::ConnectionError { error, .. } => Some(error.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(errors, vec!["连接已断开".to_owned()]);
    // 目录整体失败：只留一条提示行，不在每条连接下重复。
    model.nav_error = "目录读取失败".to_owned();
    let first = model.explorer_rows().first().map(|item| item.id());
    assert_eq!(first.as_deref(), Some("catalog-error"));
}
/// 重启恢复：分组与折叠经配置库落盘，下次启动由 RefreshGroups 读回来。
#[tokio::test]
async fn groups_and_collapse_state_survive_a_restart_through_the_job_path() {
    let dir = std::env::temp_dir().join(format!("rillsql-shell-group-restart-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let database = dir.join("work.sqlite");
    let mut draft = crate::connections::model::ConnectionDraft::blank(Engine::Sqlite);
    draft.id = Some("work".to_owned());
    draft.name = "工作库".to_owned();
    draft.file_path = database.display().to_string();
    services.connections.save(draft, crate::connections::model::SecretInput::default()).await.unwrap();

    let mut model = WorkbenchModel::new(Shell::default());
    model.open_group_prompt(None);
    model.set_field(DraftField::GroupName, "夜间巡检".into());
    assert!(model.confirm_group_prompt());
    let token = model.begin_owner(&services.tasks, "groups");
    let request = request_of(&mut model, token);
    let done = jobs::execute_job(&services, request).await;
    apply_done(&mut model, &services, "groups", done);
    assert_eq!(model.groups.len(), 1, "新建分组落库后回到模型");
    let group_id = model.groups[0].id.clone();

    model.connections.rows = services.connections.list().await.unwrap();
    assert!(model.assign_connection_group("work", Some(&group_id)));
    let token = model.begin_owner(&services.tasks, "groups");
    let request = request_of(&mut model, token);
    let done = jobs::execute_job(&services, request).await;
    apply_done(&mut model, &services, "groups", done);
    assert_eq!(model.group_of("work"), Some(group_id.as_str()));
    assert!(model.toggle_group(&group_id));
    let token = model.begin_owner(&services.tasks, "groups");
    let request = request_of(&mut model, token);
    let done = jobs::execute_job(&services, request).await;
    apply_done(&mut model, &services, "groups", done);
    assert!(model.groups[0].collapsed);

    // “重启”：新的服务与模型，只靠启动那次 RefreshGroups 恢复。
    let restarted = services_at(&dir);
    let mut model = WorkbenchModel::new(Shell::default());
    model.connections.rows = restarted.connections.list().await.unwrap();
    let _ = model.dispatch("group-refresh");
    let token = model.begin_owner(&restarted.tasks, "groups");
    let request = request_of(&mut model, token);
    let done = jobs::execute_job(&restarted, request).await;
    apply_done(&mut model, &restarted, "groups", done);
    assert_eq!(model.groups.len(), 1, "重启后分组还在");
    assert_eq!(model.groups[0].name, "夜间巡检");
    assert!(model.groups[0].collapsed, "折叠状态一起恢复");
    assert_eq!(model.group_of("work"), Some(group_id.as_str()), "成员关系一起恢复");
    let ids: Vec<String> = model.explorer_rows().iter().map(|item| item.id()).collect();
    assert_eq!(ids, vec![format!("group:{group_id}")], "折叠的分组只留标题，成员连接不展开");
    let _ = std::fs::remove_dir_all(dir);
}
/// 三档宽度：对象浏览器的宽度、行高与元素数量都按密度与布局走，不横向溢出。
#[gpui_kit::test]
fn object_browser_geometry_holds_at_1360_900_and_640(cx: &mut gpui_kit::TestAppContext) {
    for (width, overlay) in [(1360., false), (900., false), (640., true)] {
        let dir = std::env::temp_dir().join(format!("rillsql-shell-geom-{}-{width}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let services = services_at(&dir);
        let (window, view) = open_workbench_sized(cx, Arc::clone(&services), false, width, 800.);
        cx.update(|cx| {
            view.update(cx, |view, cx| {
                view.model.connections.rows = (0..12)
                    .map(|index| record_for(&format!("c{index:02}"), &format!("客户订单归档连接 {index:02}"), Engine::Postgres))
                    .collect();
                view.model.shell.sidebar_open = true;
                cx.notify();
            });
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let sidebar = window.try_find("sidebar").expect("资源管理器");
            let sidebar_w = f32::from(sidebar.bounds().size.width);
            let expected = view.read(cx).model.sidebar_width;
            assert!((sidebar_w - expected).abs() < 1., "{width}: 侧栏宽 {sidebar_w}");
            assert!(f32::from(sidebar.bounds().origin.x) <= 64. + 1., "{width}: 侧栏贴着活动栏");
            let row = window.try_find("side-c00").expect("连接行");
            assert!(f32::from(row.bounds().size.width) <= sidebar_w, "{width}: 行不横向溢出");
            assert_eq!(f32::from(row.bounds().size.height), 26., "{width}: 紧凑行高");
            let list = window.try_find("explorer-list").expect("列表");
            assert!(f32::from(list.bounds().size.width) <= sidebar_w, "{width}: 列表不横向溢出");
            let mut created = 0;
            for index in 0..12 {
                if window.try_find(format!("side-c{index:02}")).is_some() {
                    created += 1;
                }
            }
            let overlay_now = view.read(cx).model.painted().sidebar_overlay;
            assert_eq!(overlay_now, overlay, "{width}: 是否覆盖式侧栏");
            println!(
                "WIDTH {width} sidebar {sidebar_w} list {} overlay {overlay} rows {created} row_h 26",
                f32::from(list.bounds().size.width)
            );
        })
        .unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }
}

// ---------------------------------------------------------------- 快速打开（T5）

fn saved_query_row(id: &str, name: &str, sql: &str) -> crate::query::SavedQuery {
    crate::query::SavedQuery {
        id: id.to_owned(),
        name: name.to_owned(),
        sql: sql.to_owned(),
        connection_id: "pg".to_owned(),
        connection_name: "销售库".to_owned(),
        database_name: "shop".to_owned(),
        engine: "postgres".to_owned(),
        updated_at: "2026-11-14T08:00:00Z".to_owned(),
    }
}

/// 一份加载过目录、且记了一条保存查询的模型：快速打开的全部来源都能出条目。
fn palette_model() -> WorkbenchModel {
    use crate::catalog::model::ObjectRow;
    let mut model = seeded_model(vec![record_for("pg", "销售库", Engine::Postgres)]);
    model.connections.status.insert("pg".into(), LinkStatus::Connected);
    let pg = nav_connection(
        Engine::Postgres,
        "pg",
        "销售库",
        vec![nav_database("shop", vec![ObjectRow::table("orders", Some(2)), ObjectRow::view("recent")])],
    );
    model.apply_navigation(&[pg], false);
    model.set_catalog(
        &[
            tree_object("pg", "销售库", "postgres", "shop", "public", "orders", "table"),
            tree_object("pg", "销售库", "postgres", "shop", "public", "recent", "view"),
        ],
        "",
    );
    model.history.saved = vec![saved_query_row("q1", "每日订单", "select 1;")];
    model
}

fn palette_groups(model: &WorkbenchModel) -> Vec<&'static str> {
    model.palette_entries().iter().map(|entry| entry.group).collect()
}

#[test]
fn palette_searches_connections_databases_tables_views_saved_queries_and_commands() {
    let entries = palette_model().palette_entries();
    let groups: Vec<&str> = entries.iter().map(|entry| entry.group).collect();
    for group in ["command", "connection", "database", "table", "view", "saved"] {
        assert!(groups.contains(&group), "快速打开缺类型 {group}");
    }

    // 每一类都能用名字搜到，并且带上下文和类型。
    for (query, group, label) in [
        ("order", "table", "orders"),
        ("rec", "view", "recent"),
        ("每日", "saved", "每日订单"),
        ("shop", "database", "shop"),
        ("销售", "connection", "销售库"),
    ] {
        let mut model = palette_model();
        model.palette_query = query.into();
        let found = model
            .palette_entries()
            .into_iter()
            .find(|entry| entry.group == group && entry.label == label)
            .unwrap_or_else(|| panic!("搜“{query}”没命中 {group}/{label}"));
        assert!(!found.detail.is_empty(), "{group}/{label} 要带上下文");
        assert_eq!(found.label_key, super::model::palette_kind_key(group));
    }

    // 上下文也参与模糊匹配（例如按库名找表），名字命中仍然排在前面。
    let mut model = palette_model();
    model.palette_query = "public".into();
    let labels: Vec<(String, &str)> = model.palette_entries().iter().map(|entry| (entry.label.clone(), entry.group)).collect();
    assert!(labels.iter().any(|(label, group)| label == "orders" && *group == "table"), "{labels:?}");
    assert!(labels.iter().any(|(label, group)| label == "recent" && *group == "view"), "{labels:?}");

    // 每行都带上下文与类型；六种类型的标注文案彼此不同。
    let entries = palette_model().palette_entries();
    let connection = entries.iter().find(|entry| entry.group == "connection").unwrap();
    assert_eq!(connection.detail, "PostgreSQL · app");
    let database = entries.iter().find(|entry| entry.group == "database").unwrap();
    assert_eq!(database.detail, "销售库");
    let table = entries.iter().find(|entry| entry.group == "table").unwrap();
    assert_eq!(table.detail, "shop.public · 销售库");
    let view = entries.iter().find(|entry| entry.group == "view").unwrap();
    assert_eq!(view.detail, "shop.public · 销售库");
    let saved = entries.iter().find(|entry| entry.group == "saved").unwrap();
    assert_eq!(saved.detail, "销售库 · shop");
    let mut texts: Vec<String> = ["command", "connection", "database", "table", "view", "saved"]
        .iter()
        .map(|group| model.text(super::model::palette_kind_key(group)))
        .collect();
    texts.sort();
    texts.dedup();
    assert_eq!(texts.len(), 6, "六种类型的标注文案不能重合");
}

#[test]
fn palette_command_shortcuts_match_the_shortcut_help_and_menu_source() {
    let entries = WorkbenchModel::new(Shell::default()).palette_entries();
    let mut with_chord = 0;
    for command in crate::app::COMMANDS {
        let entry = entries.iter().find(|entry| entry.id == command.id).expect("命令面板缺命令");
        let source = command
            .chord()
            .or_else(|| crate::app::model::SHORTCUTS.iter().find(|shortcut| shortcut.id == command.id).map(|shortcut| shortcut.chord()));
        assert_eq!(entry.shortcut, source, "{} 的快捷键要和菜单/快捷键表同源", command.id);
        with_chord += usize::from(entry.shortcut.is_some());
    }
    assert!(with_chord >= 5, "已登记快捷键的命令都要标出来：{with_chord}");
    // 非命令条目没有快捷键，不会借一条别人的键。
    assert!(entries.iter().filter(|entry| entry.group != "command").all(|entry| entry.shortcut.is_none()));
    // “>” 只留命令，快捷键照旧显示；搜索词按当前界面语言匹配。
    let mut model = WorkbenchModel::new(Shell::default());
    let new_query_label = model.text("shell.command.newQuery");
    model.palette_query = format!("> {new_query_label}");
    let commands = model.palette_entries();
    assert!(!commands.is_empty() && commands.iter().all(|entry| entry.group == "command"));
    assert_eq!(commands[0].id, "new-query", "名字命中排在最前");
    assert_eq!(commands[0].shortcut, Some("Mod+T"));
    // 英文界面同样按名字命中：">" 后面的空格不参与匹配。
    model.shell.locale = Locale::En;
    model.palette_query = "> new".into();
    let commands = model.palette_entries();
    assert!(commands.iter().any(|entry| entry.id == "new-query" && entry.shortcut == Some("Mod+T")), "{commands:?}");
}

#[test]
fn palette_opens_data_loads_saved_and_focuses_the_tree() {
    // 表：直接打开数据页，不执行 SQL。
    let mut model = palette_model();
    let table_id = model
        .palette_entries()
        .iter()
        .find(|entry| entry.group == "table")
        .expect("表行")
        .id
        .clone();
    assert!(table_id.starts_with("open-table:"), "{table_id}");
    assert!(model.dispatch(&table_id).is_some());
    let tab = model.active_tab().expect("打开的表工作项");
    assert_eq!(tab.kind, WorkItemKind::Table);
    assert!(tab.spec.as_deref().is_some_and(|spec| spec.contains("orders")));
    assert_eq!(model.executions, 0, "快速打开表不执行 SQL");

    // 保存查询：载入编辑器，不执行。
    let mut model = palette_model();
    let tabs_before = model.tabs.len();
    assert!(model.dispatch("palette-saved:q1").is_some());
    let tab = model.active_tab().expect("载入的查询草稿");
    assert!(tab.kind.is_query());
    assert_eq!(tab.draft, "select 1;");
    assert_eq!(tab.connection_id.as_deref(), Some("pg"), "载入时带上原来那条连接");
    assert_eq!(tab.database.as_deref(), Some("shop"));
    assert!(tab.last_executed_sql.is_none(), "载入不是执行");
    assert_eq!(model.executions, 0);
    assert_eq!(model.history.executions, 0);
    assert_eq!(model.loads, 1);
    assert_eq!(model.tabs.len(), tabs_before + 1, "只开一条草稿");
    assert_eq!(model.shell.page, AppPage::Workspace);
    assert_eq!(model.shell.focus, crate::app::FocusTarget::Editor);

    // 库：展开并聚焦对象树，不跳管理页。
    let mut model = palette_model();
    assert!(model.dispatch("palette-database:pg|shop").is_some());
    let database_node = crate::catalog::hierarchy::database_node_id("pg", "shop");
    assert_eq!(model.shell.page, AppPage::Workspace);
    assert_eq!(model.shell.focus, crate::app::FocusTarget::Sidebar);
    assert!(model.shell.sidebar_open);
    assert_eq!(model.nav.focus(), Some(database_node.as_str()));
    assert!(model.nav.is_expanded(&crate::catalog::hierarchy::connection_node_id("pg")));
    assert_eq!(model.reveal_row, Some(format!("nav:{database_node}")));
    assert_eq!(model.explorer_current_row(), Some(super::explorer::ExplorerRow::Node(database_node)));

    // 连接：展开并落在连接行上。
    let mut model = palette_model();
    assert!(model.dispatch("palette-connection:pg").is_some());
    assert_eq!(model.connections.selected.as_deref(), Some("pg"));
    assert_eq!(model.explorer_current_row(), Some(super::explorer::ExplorerRow::Connection("pg".into())));
    assert_eq!(model.reveal_row.as_deref(), Some("conn:pg"));
    assert_eq!(model.shell.page, AppPage::Workspace, "连接只在树里定位");

    // 树里没有这个库：说明原因，不假装定位成功。
    let mut model = palette_model();
    assert!(model.dispatch("palette-database:pg|nope").is_some());
    assert_eq!(model.notice_key, Some("ui.palette.databaseMissing"));
    assert_eq!(model.notice_detail, "nope");

    // 连接在面板打开后被删掉：同样给出原因，不静默失败。
    let mut model = palette_model();
    model.connections.rows.clear();
    assert!(model.dispatch("palette-connection:pg").is_some());
    assert_eq!(model.notice_key, Some("ui.palette.connectionMissing"));
    assert_eq!(model.notice_detail, "pg");
}

#[test]
fn palette_keyboard_walks_every_type_and_enter_runs_the_selected_row() {
    let mut model = palette_model();
    assert!(model.dispatch("command-palette").is_some());
    let entries = model.palette_entries();
    assert!(entries.len() > 3);
    assert_eq!(model.palette_index, 0);

    assert_eq!(model.key(NavKey::ArrowDown), None);
    assert_eq!(model.palette_index, 1);
    assert_eq!(model.key(NavKey::End), None);
    assert_eq!(model.palette_index, entries.len() - 1);
    assert_eq!(model.key(NavKey::Home), None);
    assert_eq!(model.palette_index, 0);
    // 上下键在混合结果里循环，不会跳过连接/库/对象。
    assert_eq!(model.key(NavKey::ArrowUp), None);
    assert_eq!(model.palette_index, entries.len() - 1);

    // 光标落到保存查询那一行，回车只载入不执行。
    let index = entries.iter().position(|entry| entry.group == "saved").expect("保存查询行");
    model.palette_index = index;
    let command = model.key(NavKey::Enter);
    assert_eq!(command.as_deref(), Some(entries[index].id.as_str()));
    assert!(!model.shell.palette_open, "回车后面板关闭");
    assert_eq!(model.active_tab().map(|tab| tab.draft.as_str()), Some("select 1;"));
    assert_eq!(model.executions, 0, "键盘选中保存查询也不执行");
}

#[test]
fn palette_targets_connect_first_when_the_connection_is_not_open() {
    let mut model = seeded_model(vec![record_for("pg", "Atlas", Engine::Postgres)]);
    let orders = tree_object("pg", "Atlas", "postgres", "shop", "public", "orders", "table");
    assert!(model.set_catalog(std::slice::from_ref(&orders), ""));
    let spec = crate::app::views::query::object_spec(&orders, "");

    // 已知但还没打开：先建连，动作排队，不直接打开。
    assert!(model.gate_on_demand(&format!("open-table:{spec}")), "未连接时先建连");
    assert_eq!(model.pending_len(), 1, "只排一次打开连接");
    assert_eq!(model.connections.status.get("pg"), Some(&LinkStatus::Connecting));
    let queued = model
        .connect_intents
        .iter()
        .find(|intent| intent.spec.as_deref() == Some(spec.as_str()))
        .expect("排队打开这个被选中的对象");
    assert_eq!(queued.action, super::conn_modal::ConnectAction::OpenData);
    assert!(model.tabs.is_empty(), "没连上之前不打开数据页");

    model.connections.status.insert("pg".into(), LinkStatus::Connected);
    let ready = model.take_ready_intents();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].command(), Some("tree-open-data"));
    assert_eq!(ready[0].spec.as_deref(), Some(spec.as_str()), "重放排队时记下的对象");
    assert!(model.take_ready_intents().is_empty(), "只重放一次");
    assert!(!model.gate_on_demand(&format!("open-table:{spec}")), "已连接照常打开");

    // 连接与库：同样先建连再重放定位命令；保存查询不需要连接。
    model.connections.status.insert("pg".into(), LinkStatus::Disconnected);
    assert!(model.gate_on_demand("palette-connection:pg"));
    assert!(model.gate_on_demand("palette-database:pg|shop"));
    let replays: Vec<&str> = model.connect_intents.iter().filter_map(|intent| intent.replay.as_deref()).collect();
    assert!(replays.contains(&"palette-connection:pg"), "{replays:?}");
    assert!(replays.contains(&"palette-database:pg|shop"), "{replays:?}");
    assert_eq!(model.pending_len(), 1, "在途连接复用在途操作");
    assert!(!model.gate_on_demand("palette-saved:q1"), "载入保存查询不建连");
    assert_eq!(model.pending_len(), 1);
}

#[test]
fn palette_never_invents_unloaded_objects_and_separates_no_match_from_failure() {
    // 连接可以搜到；从没读过的对象不会凭空出现。
    let mut model = seeded_model(vec![record_for("pg", "Atlas", Engine::Postgres)]);
    assert!(model.palette_entries().iter().any(|entry| entry.group == "connection" && entry.label == "Atlas"));
    assert!(model.palette_entries().iter().all(|entry| entry.group != "table" && entry.group != "database"));
    model.palette_query = "orders".into();
    assert!(model.palette_entries().is_empty(), "未加载的对象不能被伪造");
    assert_eq!(model.palette_empty_key(), "ui.palette.noMatch");
    assert_ne!(model.text("ui.palette.noMatch"), model.text("ui.palette.error"), "无命中与目录失败是两条文案");

    // 目录失败：同一处提示换成失败原因，并给出原因明细。
    assert!(model.set_catalog(&[], "连接超时"));
    assert_eq!(model.palette_empty_key(), "ui.palette.error");
    assert_eq!(model.palette_error(), "连接超时");

    // 清空搜索词后又能找到连接：无命中只是这一次搜索没有结果。
    assert!(model.set_catalog(&[], ""));
    model.palette_query.clear();
    assert!(model.palette_entries().iter().any(|entry| entry.group == "connection"));
    assert_eq!(model.palette_empty_key(), "ui.palette.empty");
}

#[test]
fn palette_drops_stale_rows_when_the_connection_goes_away_and_restores_them_after_reconnect() {
    let mut model = palette_model();
    assert!(palette_groups(&model).contains(&"table"));
    assert!(palette_groups(&model).contains(&"database"));

    // 断开：旧的目录快照不再冒充“当前可打开”，但连接本身还能搜到。
    model.connections.status.insert("pg".into(), LinkStatus::Disconnected);
    let after = palette_groups(&model);
    assert!(!after.contains(&"table") && !after.contains(&"view") && !after.contains(&"database"), "{after:?}");
    assert!(after.contains(&"connection"), "断开后仍能定位连接本身");
    assert!(after.contains(&"saved"), "保存的查询不依赖连接状态");

    // 重连：同一份已读索引回来。
    model.connections.status.insert("pg".into(), LinkStatus::Connected);
    assert!(palette_groups(&model).contains(&"table"));

    // 连接记录被删除：即使状态还停在已连接也不列旧对象。
    model.connections.rows.clear();
    let gone = palette_groups(&model);
    assert!(!gone.contains(&"table") && !gone.contains(&"database") && !gone.contains(&"connection"), "{gone:?}");

    // 目录刷新失败：旧对象与库一起清空，失败原因留在空态里。
    let mut model = palette_model();
    assert!(model.set_catalog(&[], "读取对象失败"));
    model.nav_failed("读取对象失败");
    assert!(!palette_groups(&model).contains(&"table"));
    assert_eq!(model.palette_empty_key(), "ui.palette.error");
    assert_eq!(model.palette_error(), "读取对象失败");
}

/// 快速打开渲染出来的每一行都带上下文与类型；640 窄窗下面板仍完整落在视口里。
#[gpui_kit::test]
fn quick_open_renders_context_and_type_for_every_result_kind(cx: &mut gpui_kit::TestAppContext) {
    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-shell-palette-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    // 对象索引走真实读取（SQLite 表 + 视图），保存的查询进真历史库：
    // 快速打开渲染的每一类都来自它平时读的那份数据。
    let database = dir.join("items.sqlite");
    let config_dir = dir.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("runtime");
        runtime.block_on(async move {
            let pool = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).create_if_missing(true))
                .await
                .unwrap();
            sqlx::query("CREATE TABLE item (id INTEGER PRIMARY KEY, label TEXT)").execute(&pool).await.unwrap();
            sqlx::query("CREATE VIEW item_view AS SELECT id, label FROM item").execute(&pool).await.unwrap();
            pool.close().await;
            let services = Arc::new(AppServices::new(Arc::new(Service::new_for_test(config_dir.join("connections.sqlite")))));
            services.connections.open_sqlite_for_test("notes", "笔记", &database).await.unwrap();
            crate::app::views::history::save_named(
                &services,
                &SaveRequest {
                    name: "每日订单".into(),
                    sql: "select 1;".into(),
                    connection_id: "notes".into(),
                    connection_name: "笔记".into(),
                    database_name: String::new(),
                    engine: "sqlite".into(),
                },
            )
            .await
            .unwrap();
            let _ = tx.send(services);
        });
    });
    let services = rx.recv_timeout(std::time::Duration::from_secs(120)).expect("sqlite setup");
    let (window, view) = open_workbench_sized(cx, services, false, 640., 800.);
    cx.update_window(window, |_, _, cx| {
        view.update(cx, |view, cx| view.perform("refresh-tree", cx));
    })
    .unwrap();
    assert!(wait_for_tree(cx, window, &view), "对象索引没有同步");
    cx.update(|cx| {
        view.update(cx, |view, cx| view.perform("command-palette", cx));
    });
    // 打开面板时补读历史库：等保存的查询落进清单，再取它渲染出来的行。
    let started = std::time::Instant::now();
    let mut ready = false;
    while started.elapsed() < std::time::Duration::from_secs(30) {
        cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
        cx.run_until_parked();
        ready = cx.update(|cx| view.read(cx).model.palette_entries().iter().any(|entry| entry.group == "saved"));
        if ready {
            break;
        }
        std::thread::park_timeout(std::time::Duration::from_millis(25));
    }
    assert!(ready, "保存的查询没有进入快速打开");

    let entries = cx.update(|cx| view.read(cx).model.palette_entries());
    let index_of = |group: &str| {
        entries
            .iter()
            .position(|entry| entry.group == group)
            .unwrap_or_else(|| {
                let seen: Vec<String> = entries.iter().map(|entry| format!("{}:{}", entry.group, entry.label)).collect();
                panic!("快速打开没有 {group} 行；实际 {seen:?}")
            })
    };
    let (table, view_row, saved) = (index_of("table"), index_of("view"), index_of("saved"));
    let texts = cx.update(|cx| {
        let model = &view.read(cx).model;
        (
            model.text("shell.paletteGroup.tables"),
            model.text("shell.paletteGroup.views"),
            model.text("shell.paletteGroup.savedQueries"),
        )
    });
    let (kind_table, kind_view, kind_saved, detail, palette, width) = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let kind = |index: usize| {
                window
                    .try_find(format!("pal-kind-{index}"))
                    .and_then(|element| element.label().map(str::to_owned))
            };
            let detail = |index: usize| {
                window
                    .try_find(format!("pal-detail-{index}"))
                    .map(|element| (f32::from(element.bounds().size.width), f32::from(element.bounds().size.height)))
            };
            let palette = window.find("palette").bounds();
            let width = f32::from(window.find("workbench").bounds().size.width);
            (kind(table), kind(view_row), kind(saved), detail(table), palette, width)
        })
        .unwrap();
    assert_eq!(kind_table.as_deref(), Some(texts.0.as_str()), "表行要标出“表”");
    assert_eq!(kind_view.as_deref(), Some(texts.1.as_str()), "视图行要标出“视图”");
    assert_eq!(kind_saved.as_deref(), Some(texts.2.as_str()), "保存查询行要标出“保存的查询”");
    let (detail_w, detail_h) = detail.expect("表行要画出上下文");
    assert!(detail_w > 0. && detail_h > 0., "上下文要真的占位：{detail_w}x{detail_h}");
    assert!(f32::from(palette.origin.x) >= 0.);
    assert!(f32::from(palette.origin.x + palette.size.width) <= width + 0.5, "640 窄窗下面板不越界");
    assert!(f32::from(palette.size.width) <= 620.);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 新建查询沿用上下文目标：唯一已打开的连接（含它唯一的库）直接带进来；
/// 多条已打开连接时不替用户猜；当前标签已有目标时沿用当前标签。
#[test]
fn blank_query_inherits_the_obvious_target() {
    use crate::app::views::connections::LinkStatus;
    let (mut model, _) = pg_orders_model();
    let mut pg = pg_saved("pg", "dev");
    pg.saved.database = None;
    model.connections.rows.push(pg);
    model.connections.status.insert("pg".into(), LinkStatus::Connected);
    assert!(model.dispatch("new-query").is_some());
    let tab = model.active_tab().unwrap();
    assert_eq!(tab.connection_id.as_deref(), Some("pg"));
    assert_eq!(tab.database.as_deref(), Some("shop"), "树上唯一的库作为默认库");
    assert_eq!(tab.engine.as_deref(), Some("postgres"));

    // 当前标签有目标：下一个新查询沿用它。
    assert!(model.dispatch("new-query").is_some());
    assert_eq!(model.active_tab().unwrap().connection_id.as_deref(), Some("pg"));

    // 两条已打开连接、当前标签没有目标：不猜。
    let mut model = WorkbenchModel::new(Shell::default());
    model.connections.rows.push(pg_saved("a", "dev"));
    model.connections.rows.push(pg_saved("b", "dev"));
    model.connections.status.insert("a".into(), LinkStatus::Connected);
    model.connections.status.insert("b".into(), LinkStatus::Connected);
    assert!(model.dispatch("new-query").is_some());
    assert_eq!(model.active_tab().unwrap().connection_id, None);
}

#[test]
fn new_query_keeps_the_redis_target_with_other_connections_open() {
    let mut model = WorkbenchModel::new(Shell::default());
    let mut redis = pg_saved("cache", "dev");
    redis.saved.engine = Engine::Redis;
    redis.saved.database = None;
    model.connections.rows.push(redis);
    model.connections.status.insert("cache".into(), LinkStatus::Connected);
    model.dispatch("new-query").unwrap();
    assert_eq!(model.active_tab().unwrap().database.as_deref(), Some("0"));

    model.connections.rows.push(pg_saved("pg", "dev"));
    model.connections.status.insert("pg".into(), LinkStatus::Connected);
    model.dispatch("open-redis:cache|缓存|8").unwrap();
    model.dispatch("new-query").unwrap();
    let tab = model.active_tab().unwrap();
    assert_eq!(tab.connection_id.as_deref(), Some("cache"));
    assert_eq!(tab.engine.as_deref(), Some("redis"));
    assert_eq!(tab.database.as_deref(), Some("8"));

    model.connections.selected = Some("cache".into());
    model.dispatch("conn-new-query").unwrap();
    let ready = model.take_ready_intents();
    assert_eq!(ready[0].action, super::conn_modal::ConnectAction::ConnectOnly);
    assert_eq!(model.active_tab().unwrap().database.as_deref(), Some("0"));
}

/// 命令面板的“检查更新”：打开设置里的更新卡片并立即发起手动检查（不看自动检查开关）。
#[test]
fn palette_check_updates_opens_the_card_and_checks() {
    let mut model = WorkbenchModel::new(Shell::default());
    model.settings.auto_check_updates = false;
    assert!(crate::app::COMMANDS.iter().any(|command| command.id == "check-updates-show"), "命令面板里能搜到检查更新");
    assert!(model.dispatch("check-updates-show").is_some());
    assert_eq!(model.shell.page, AppPage::Settings);
    assert_eq!(model.settings_section, crate::app::views::settings::SettingsSection::Local);
    assert_eq!(model.update.stage, super::update::UpdateStage::Checking { manual: true });
    assert_eq!(model.update_job, Some(super::update::UpdateJob::Check));
}

/// 恢复出来的查询标签只有连接、没有库：目录读到这条连接只有一个库时自动补上，执行不再被“请选择库”挡住。
#[test]
fn restored_query_tab_gets_the_only_database() {
    let (mut model, _) = pg_orders_model();
    assert!(model.dispatch("new-query").is_some());
    let id = model.active_tab.clone().unwrap();
    model.set_query_target(&id, Some("pg".into()), Some("销售库".into()), None, Some("postgres".into()));
    let orders = crate::catalog::model::ObjectRow::table("orders", Some(2));
    let pg = nav_connection(Engine::Postgres, "pg", "销售库", vec![nav_database("shop", vec![orders])]);
    model.apply_navigation(&[pg], false);
    assert_eq!(model.tabs.iter().find(|tab| tab.id == id).and_then(|tab| tab.database.clone()).as_deref(), Some("shop"));
}

/// 双击触发器、函数这类没有数据页的对象：打开它的定义查看器，而不是什么都不发生。
#[test]
fn double_clicking_a_trigger_or_routine_opens_its_definition() {
    use crate::catalog::model::{ObjectKind, ObjectRow};
    let trigger = ObjectRow::fresh("touch_updated", ObjectKind::Trigger);
    let function = ObjectRow::fresh("total_of", ObjectKind::Function);
    let table = ObjectRow::table("orders", Some(2));
    let pg = nav_connection(Engine::Postgres, "pg", "销售库", vec![nav_database("shop", vec![table.clone(), trigger.clone(), function.clone()])]);
    let mut model = WorkbenchModel::new(Shell::default());
    model.apply_navigation(&[pg], false);
    for row in [&trigger, &function] {
        let id = nav_id("pg", "shop", "public", row);
        assert_eq!(model.nav_toggle_command(&id), Some(format!("nav-view-ddl:{id}")), "{}", row.name);
    }
    let orders = nav_id("pg", "shop", "public", &table);
    assert_eq!(model.nav_toggle_command(&orders), Some(format!("nav-open-data:{orders}")), "表仍然打开数据");
}

/// 标签管理：新建要求名称、保存交给后台；删除先确认，删除后用到它的连接变成无标签、筛选复位。
#[test]
fn tag_manager_validates_saves_and_untags_connections_on_delete() {
    use super::model::TagJob;
    use crate::connections::model::EnvTag;
    let mut model = WorkbenchModel::new(Shell::default());
    let mut row = record_for("pg", "Atlas", Engine::Postgres);
    row.saved.env = "staging".into();
    model.connections.rows = vec![row];
    model.env_tags = vec![EnvTag { id: "staging".into(), label: "预发".into(), tone: "info".into(), position: 4 }];
    assert!(model.dispatch("tags-manage").is_some());
    model.dispatch("tag-new");
    model.dispatch("tag-save");
    assert!(model.tag_manager.as_ref().unwrap().error.is_some(), "空名称要拦下");
    assert!(model.tag_job.is_none());
    model.set_field(DraftField::TagLabel, "灰度".into());
    model.dispatch("tag-tone:info");
    model.dispatch("tag-save");
    assert_eq!(model.tag_job.take(), Some(TagJob::Save { id: None, label: "灰度".into(), tone: "info".into() }));

    assert_eq!(model.tag_usage("staging"), 1);
    model.connections.filter_env = "staging".into();
    model.dispatch("tag-delete:staging");
    assert!(model.tag_job.is_none(), "删除先确认");
    model.dispatch("tag-delete-confirm:staging");
    assert_eq!(model.tag_job.take(), Some(TagJob::Delete("staging".into())));
    model.finish_tag_job(&TagJob::Delete("staging".into()), Ok(Vec::new()));
    assert_eq!(model.connections.rows[0].saved.env, "none");
    assert!(model.connections.filter_env.is_empty());
    assert!(model.tag_manager.is_some(), "对话框留着继续管理");
    model.dismiss();
    assert!(model.tag_manager.is_none());
}

/// R12 标签面板：空名与重名就地报错不进后台；使用数可展开查看关联连接并逐条移出；
/// 删除确认显示受影响数，确认后这些连接变为无标签；新标签保存后筛选栏立即可用。
#[test]
fn tag_panel_reports_name_errors_inline_and_manages_tagged_connections() {
    use super::model::TagJob;
    use crate::connections::model::EnvTag;
    let mut model = WorkbenchModel::new(Shell::default());
    let mut rows = Vec::new();
    for (id, name) in [("c1", "订单"), ("c2", "库存"), ("c3", "日志"), ("c4", "报表")] {
        let mut row = record_for(id, name, Engine::Postgres);
        row.saved.env = "staging".into();
        rows.push(row);
    }
    rows.push(record_for("c5", "本地", Engine::Postgres));
    model.connections.rows = rows;
    let staging = EnvTag { id: "staging".into(), label: "预发".into(), tone: "info".into(), position: 4 };
    model.apply_env_tags(vec![staging.clone()]);
    model.dispatch("tags-manage");

    // 空名：就地报错（不经后台）。
    model.dispatch("tag-new");
    model.dispatch("tag-save");
    assert_eq!(model.tag_manager.as_ref().unwrap().error.as_deref(), Some(model.text("tags.nameRequired").as_str()));
    assert!(model.tag_job.is_none());
    // 重名（忽略首尾空白与大小写）：同样就地报错。
    model.set_field(DraftField::TagLabel, " 预发 ".into());
    model.dispatch("tag-save");
    let error = model.tag_manager.as_ref().unwrap().error.clone().expect("重名要报错");
    assert!(error.contains("预发"), "{error}");
    assert!(model.tag_job.is_none(), "重名不发后台任务");
    // 编辑自己时同名不算重名。
    model.dispatch("tag-edit:staging");
    model.dispatch("tag-save");
    assert_eq!(model.tag_job.take(), Some(TagJob::Save { id: Some("staging".into()), label: "预发".into(), tone: "info".into() }));
    let job = TagJob::Save { id: Some("staging".into()), label: "预发".into(), tone: "info".into() };
    model.finish_tag_job(&job, Ok(vec![staging.clone()]));

    // 新建标签：保存成功后立即进入标签表（筛选栏与连接表单都从这里取）。
    model.dispatch("tag-new");
    model.set_field(DraftField::TagLabel, "灰度".into());
    model.dispatch("tag-save");
    let job = model.tag_job.take().expect("保存进入后台");
    let gray = EnvTag { id: "gray".into(), label: "灰度".into(), tone: "dev".into(), position: 5 };
    model.finish_tag_job(&job, Ok(vec![staging.clone(), gray.clone()]));
    assert!(model.env_tags.iter().any(|tag| tag.id == "gray"), "新标签立即可筛选");
    assert!(model.tag_manager.as_ref().unwrap().editing.is_none());

    // 使用数展开：列出关联连接（按名称排序），逐条移出。
    assert_eq!(model.tag_usage("staging"), 4);
    model.dispatch("tag-expand:staging");
    assert_eq!(model.tag_manager.as_ref().unwrap().expanded.as_deref(), Some("staging"));
    let names: Vec<String> = model.tag_connections("staging").into_iter().map(|(_, name)| name).collect();
    assert_eq!(names.len(), 4);
    model.dispatch("tag-untag:c3");
    assert_eq!(model.tag_job.take(), Some(TagJob::Untag { connection: "c3".into() }));
    model.finish_tag_job(&TagJob::Untag { connection: "c3".into() }, Ok(vec![staging.clone(), gray.clone()]));
    assert_eq!(model.connections.rows.iter().find(|row| row.saved.id == "c3").unwrap().saved.env, "none");
    assert_eq!(model.tag_usage("staging"), 3);
    assert_eq!(model.tag_manager.as_ref().unwrap().expanded.as_deref(), Some("staging"), "移出后保持展开继续调整");
    model.dispatch("tag-expand:staging");
    assert!(model.tag_manager.as_ref().unwrap().expanded.is_none(), "再点一次收起");

    // 删除：确认前不动；确认后剩下的 3 条都变为无标签。
    model.dispatch("tag-delete:staging");
    assert!(model.tag_job.is_none());
    assert_eq!(model.tag_manager.as_ref().unwrap().confirm_delete.as_deref(), Some("staging"));
    let message = crate::i18n::fill(&model.text("tags.deleteConfirm"), &[("count", &model.tag_usage("staging").to_string())]);
    assert!(message.contains('3'), "{message}");
    model.dispatch("tag-delete-confirm:staging");
    let job = model.tag_job.take().unwrap();
    model.finish_tag_job(&job, Ok(vec![gray]));
    assert!(model.connections.rows.iter().all(|row| row.saved.env != "staging"));
    assert_eq!(model.tag_usage("staging"), 0);
}

/// R12 标签面板渲染：使用数可点开关联连接列表，每行有“移出此标签”按钮，点击进入后台移出。
#[gpui_kit::test]
fn tag_panel_lists_tagged_connections_with_remove_buttons(cx: &mut gpui_kit::TestAppContext) {
    use crate::connections::model::EnvTag;
    let dir = std::env::temp_dir().join(format!("rillsql-tag-panel-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench(cx, services_at(&dir), false);
    cx.update(|cx| {
        view.update(cx, |view, cx| {
            let mut rows = Vec::new();
            for (id, name) in [("c1", "订单"), ("c2", "库存")] {
                let mut row = record_for(id, name, Engine::Postgres);
                row.saved.env = "staging".into();
                rows.push(row);
            }
            view.model.connections.rows = rows;
            view.model.apply_env_tags(vec![EnvTag { id: "staging".into(), label: "预发".into(), tone: "info".into(), position: 4 }]);
            view.perform("tags-manage", cx);
        });
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("tag-untag-c1").is_none(), "默认不展开");
        window.click("tag-usage-staging", cx);
        window.render_frame(cx);
        assert!(window.find("tag-untag-c1").visible());
        assert!(window.find("tag-untag-c2").visible());
        window.click("tag-untag-c1", cx);
    })
    .unwrap();
    cx.update(|cx| {
        let model = &view.read(cx).model;
        let manager = model.tag_manager.as_ref().unwrap();
        assert!(manager.busy || model.tag_job.is_some(), "移出进入后台任务");
        assert_eq!(manager.expanded.as_deref(), Some("staging"));
    });
    let _ = std::fs::remove_dir_all(&dir);
}

/// R12 安全语义：把标签改成生产色后，连接的生产判断与已打开的危险确认随之更新；改回后恢复。
#[test]
fn changing_a_tag_tone_updates_production_judgement() {
    use super::model::TagJob;
    use crate::catalog::hierarchy::ObjectKey;
    use crate::catalog::model::ObjectKind;
    use crate::connections::model::EnvTag;
    let mut model = WorkbenchModel::new(Shell::default());
    let mut row = record_for("c1", "订单", Engine::Postgres);
    row.saved.env = "live".into();
    model.connections.rows = vec![row];
    let live = |tone: &str| EnvTag { id: "live".into(), label: "线上".into(), tone: tone.into(), position: 1 };
    model.apply_env_tags(vec![live("dev")]);
    assert!(!model.connection_is_prod("c1"));
    let key = ObjectKey {
        connection_id: "c1".into(),
        connection_name: "订单".into(),
        engine: Engine::Postgres,
        database: "app".into(),
        schema: "public".into(),
        name: "orders".into(),
        kind: ObjectKind::Table,
    };
    let mut prompt = super::danger::DangerPrompt::new(super::danger::DangerKind::DropTable, "n", key, "live");
    prompt.apply_tags(Some(&model.env_tags));
    model.danger = Some(prompt);
    assert!(!model.danger.as_ref().unwrap().prod);
    let revision = model.env_tags_revision;

    model.dispatch("tags-manage");
    model.dispatch("tag-edit:live");
    model.dispatch("tag-tone:prod");
    model.dispatch("tag-save");
    let job = model.tag_job.take().unwrap();
    assert_eq!(job, TagJob::Save { id: Some("live".into()), label: "线上".into(), tone: "prod".into() });
    model.finish_tag_job(&job, Ok(vec![live("prod")]));
    assert!(model.connection_is_prod("c1"), "改成生产色后按生产处理");
    assert!(model.danger.as_ref().unwrap().prod, "已打开的危险确认随之要求输入名称");
    assert!(model.env_tags_revision > revision, "视图据版本号刷新表数据/结构页的生产提示");

    model.finish_tag_job(&TagJob::Untag { connection: "c1".into() }, Ok(vec![live("prod")]));
    assert!(!model.connection_is_prod("c1"), "移出生产标签后不再按生产处理");
    let prompt = model.danger.as_ref().unwrap();
    assert!(!prompt.prod, "已打开的危险确认按连接当前的标签重算，不沿用打开时的标签");
    assert_eq!(prompt.env, crate::connections::model::ENV_NONE);
}

/// R14 启动页右键：连接行出现与连接管理一致的菜单，新建查询绑定该连接；最近执行行提供
/// 在新查询中打开、复制完整 SQL、在历史中查看与再次执行（写入语句只载入不执行）。
#[gpui_kit::test]
fn welcome_rows_open_context_menus_and_dispatch_their_actions(cx: &mut gpui_kit::TestAppContext) {
    let dir = std::env::temp_dir().join(format!("rillsql-welcome-menu-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench(cx, services_at(&dir), false);
    let read_sql = "-- 最近订单\nselect *\nfrom orders\nwhere id = 1";
    let write_sql = "delete from orders where id = 9";
    // 首词不是写入关键字、但会改数据的语句：同样只载入，不自动执行。
    let cte_write_sql = "WITH d AS (DELETE FROM orders WHERE id = 9 RETURNING *) SELECT * FROM d";
    let exec_sql = "EXEC dbo.purge_orders";
    cx.update(|cx| {
        view.update(cx, |view, cx| {
            view.model.connections.rows = vec![record_for("pg", "Atlas", Engine::Postgres)];
            view.model.history.runs = vec![
                history_run("r1", read_sql, "pg", "ok"),
                history_run("r2", write_sql, "pg", "ok"),
                history_run("r3", cte_write_sql, "pg", "ok"),
                history_run("r4", exec_sql, "pg", "ok"),
            ];
            cx.notify();
        });
    });
    let clear_tabs = |cx: &mut gpui_kit::TestAppContext| {
        cx.update(|cx| {
            view.update(cx, |view, cx| {
                view.model.tabs.clear();
                view.model.active_tab = None;
                view.model.shell.page = AppPage::Workspace;
                view.model.notice_key = None;
                cx.notify();
            })
        });
    };
    let menu_commands = |cx: &mut gpui_kit::TestAppContext| -> (MenuPlace, Vec<String>) {
        cx.update(|cx| {
            let model = &view.read(cx).model;
            let menu = model.menu.as_ref().expect("右键出现菜单");
            (menu.place, super::menus::flatten_items(&menu.items).iter().map(|item| item.command()).collect())
        })
    };

    // 1. 最近执行：复制 SQL 写入完整的多行语句，而不是行内显示的单行摘要。
    cx.update(|cx| cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string("旧内容".to_owned())));
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.right_click("welcome-run-r1", cx);
        window.render_frame(cx);
    })
    .unwrap();
    let (place, commands) = menu_commands(cx);
    assert_eq!(place, MenuPlace::HistoryEntry);
    assert_eq!(commands, ["welcome-run-open", "welcome-run-copy", "welcome-run-history", "welcome-run-again"]);
    cx.update_window(window, |_, window, cx| {
        window.click("ctx-welcome-run-copy", cx);
        window.render_frame(cx);
    })
    .unwrap();
    let clipboard = cx.update(|cx| cx.read_from_clipboard().and_then(|item| item.text()));
    assert_eq!(clipboard.as_deref(), Some(read_sql));
    assert!(cx.update(|cx| view.read(cx).model.menu.is_none()), "选中后菜单收起");

    // 2. 在新查询中打开：语句与连接绑定进入新的查询项，不执行。
    cx.update_window(window, |_, window, cx| {
        window.right_click("welcome-run-r1", cx);
        window.click("ctx-welcome-run-open", cx);
    })
    .unwrap();
    cx.update(|cx| {
        let model = &view.read(cx).model;
        let tab = model.active_tab().expect("新查询");
        assert!(tab.kind.is_query());
        assert_eq!((tab.draft.as_str(), tab.connection_id.as_deref()), (read_sql, Some("pg")));
        assert!(!model.connect_intents.iter().any(|intent| intent.replay.is_some()), "打开不执行");
    });
    clear_tabs(cx);

    // 3. 再次执行写入语句：只载入并提示确认，不发起执行。
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.right_click("welcome-run-r2", cx);
        window.click("ctx-welcome-run-again", cx);
    })
    .unwrap();
    cx.update(|cx| {
        let model = &view.read(cx).model;
        assert_eq!(model.active_tab().map(|tab| tab.draft.as_str()), Some(write_sql));
        assert_eq!(model.notice_key, Some("ui.welcome.runWriteLoaded"));
        assert!(!model.connect_intents.iter().any(|intent| intent.replay.is_some()), "写入语句不自动执行");
        assert!(!model.rerun_pending);
    });
    clear_tabs(cx);
    for (row, sql) in [("welcome-run-r3", cte_write_sql), ("welcome-run-r4", exec_sql)] {
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.right_click(row, cx);
            window.click("ctx-welcome-run-again", cx);
        })
        .unwrap();
        cx.update(|cx| {
            let model = &view.read(cx).model;
            assert_eq!(model.active_tab().map(|tab| tab.draft.as_str()), Some(sql));
            assert_eq!(model.notice_key, Some("ui.welcome.runWriteLoaded"), "{sql} 按写入处理");
            assert!(!model.connect_intents.iter().any(|intent| intent.replay.is_some()), "{sql} 不自动执行");
            assert!(!model.rerun_pending);
        });
        clear_tabs(cx);
    }

    // 4. 再次执行只读语句：走普通执行命令，连接未打开时先连上再重放这次执行。
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.right_click("welcome-run-r1", cx);
        window.click("ctx-welcome-run-again", cx);
    })
    .unwrap();
    cx.update(|cx| {
        let model = &view.read(cx).model;
        assert_eq!(model.active_tab().map(|tab| tab.draft.as_str()), Some(read_sql));
        let replays: Vec<&str> = model.connect_intents.iter().filter_map(|intent| intent.replay.as_deref()).collect();
        // 按整段脚本重放：记录里的多条语句都会再执行，不只光标所在的第一条。
        assert_eq!(replays, ["run-script"], "按需连接后按整段脚本重放执行");
    });
    clear_tabs(cx);
    cx.update(|cx| view.update(cx, |view, _| view.model.connect_intents.clear()));

    // 5. 连接行右键：与连接管理同一份菜单；新建查询打开绑定该连接的查询。
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.right_click("welcome-conn-pg", cx);
        window.render_frame(cx);
    })
    .unwrap();
    let (place, commands) = menu_commands(cx);
    assert_eq!(place, MenuPlace::ConnectionRow);
    for expected in ["conn-connect", "conn-new-query", "conn-edit", "conn-duplicate", "conn-test", "conn-delete"] {
        assert!(commands.iter().any(|command| command == expected), "缺少 {expected}：{commands:?}");
    }
    cx.update_window(window, |_, window, cx| {
        window.click("ctx-conn-new-query", cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        let tab = view.read(cx).model.active_tab().cloned().expect("新建查询");
        assert!(tab.kind.is_query());
        assert_eq!(tab.connection_id.as_deref(), Some("pg"));
    });
    clear_tabs(cx);

    // 6. 在历史中查看：切到历史页、选中这一条并打开预览。
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.right_click("welcome-run-r2", cx);
        window.click("ctx-welcome-run-history", cx);
    })
    .unwrap();
    cx.update(|cx| {
        let model = &view.read(cx).model;
        assert_eq!(model.shell.page, AppPage::History);
        assert_eq!(model.history.section, HistorySection::Runs);
        assert_eq!(model.history.selected_run.as_deref(), Some("r2"));
        assert!(model.history.preview_open);
    });
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------ 共享页签（T1）

/// 工作项页签迁移到共享组件后，类型图标、标题、关闭与新建在 1360/900/640 × 双主题 ×
/// 三档密度 × 中英文下共用同一条垂直中心，宽度不被裁切；关闭、右键、中键与键盘切换不回退。
#[gpui_kit::test]
fn work_item_tabs_share_one_geometry_and_keep_their_interactions(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::SharedString;

    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-shell-tabs-{}", uuid::Uuid::new_v4()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let center = |bounds: gpui_kit::Bounds<gpui_kit::Pixels>| f32::from(bounds.origin.y) + f32::from(bounds.size.height) / 2.;
    for width in [1360., 900., 640.] {
        let (window, view) = open_workbench_sized(cx, Arc::clone(&services), false, width, 900.);
        cx.update(|cx| {
            view.update(cx, |view, cx| {
                view.perform("new-query", cx);
                view.perform("new-query", cx);
                view.perform("new-query", cx);
            });
        });
        for theme in ["theme-light", "theme-dark"] {
            for density in ["density-compact", "density-standard", "density-relaxed"] {
                for locale in ["locale-zh", "locale-en"] {
                    cx.update(|cx| {
                        view.update(cx, |view, cx| {
                            view.perform(theme, cx);
                            view.perform(density, cx);
                            view.perform(locale, cx);
                        });
                    });
                    let note = format!("{width} {theme} {density} {locale}");
                    let tabs_h = cx.update(|cx| view.read(cx).metrics().tabs_h);
                    let (ids, active, titles) = cx.update(|cx| {
                        let model = &view.read(cx).model;
                        (
                            model.tabs.iter().map(|tab| tab.id.clone()).collect::<Vec<_>>(),
                            model.active_tab.clone(),
                            model.tabs.iter().map(|tab| tab.title.clone()).collect::<Vec<_>>(),
                        )
                    });
                    assert_eq!(ids.len(), 3, "{note}: 三个工作项");
                    let (content, strip, chips, news) = cx
                        .update_window(window, |_, window, cx| {
                            window.render_frame(cx);
                            let content = window.find("workbench-content").bounds();
                            let strip = window.find("tabs").bounds();
                            let chips = ids
                                .iter()
                                .map(|id| {
                                    (
                                        window.find(SharedString::from(format!("tab-{id}"))),
                                        window.find(SharedString::from(format!("tab-{id}-icon"))).bounds(),
                                        window.find(SharedString::from(format!("tab-{id}-title"))).bounds(),
                                        window.find(SharedString::from(format!("tab-close-{id}"))).bounds(),
                                    )
                                })
                                .collect::<Vec<_>>();
                            (content, strip, chips, window.find("tab-new").bounds())
                        })
                        .unwrap();
                    assert!((f32::from(strip.size.height) - tabs_h).abs() < 0.5, "{note}: 页签条高度跟随密度");
                    assert!(
                        (f32::from(strip.size.width) - f32::from(content.size.width)).abs() <= 1.5,
                        "{note}: 页签条占满工作区宽度（strip {} content {} x {}）",
                        f32::from(strip.size.width),
                        f32::from(content.origin.x),
                        f32::from(content.size.width)
                    );
                    assert!(f32::from(strip.origin.x + strip.size.width) <= width + 0.5, "{note}: 页签条不越出视口");
                    for (index, (chip, icon, title, close)) in chips.iter().enumerate() {
                        let chip_bounds = chip.bounds();
                        assert!((f32::from(chip_bounds.size.height) - f32::from(strip.size.height)).abs() < 1.5, "{note}: 页签撑满条高");
                        assert!(f32::from(chip_bounds.size.width) <= crate::ui::tabs::TAB_MAX_W + 0.5, "{note}: 长标题不撑破页签");
                        let middle = center(chip_bounds);
                        for (name, bounds) in [("类型图标", *icon), ("标题", *title), ("关闭", *close)] {
                            assert!((center(bounds) - middle).abs() < 0.5, "{note}: {name} 与页签同一垂直中心");
                        }
                        assert!(
                            (f32::from(title.size.height) - crate::ui::kit::line_height(crate::ui::kit::TypeScale::SM)).abs() < 0.5,
                            "{note}: 标题行高与组件一致"
                        );
                        assert!(
                            f32::from(title.origin.x + title.size.width) <= f32::from(close.origin.x) + 0.5,
                            "{note}: 关闭按钮不压标题"
                        );
                        assert_eq!(chip.label(), Some(titles[index].as_str()), "{note}: 截断后仍能取完整标题");
                        let selected = chip.selected();
                        assert_eq!(selected, Some(active.as_deref() == Some(ids[index].as_str())), "{note}: 只有当前工作项选中");
                    }
                    assert!((f32::from(news.size.width) - crate::ui::kit::HitArea::CONTROL).abs() < 0.5, "{note}: 新建命中宽度");
                    assert!((center(news) - center(chips[0].0.bounds())).abs() < 0.5, "{note}: 新建与页签内容同一垂直中心");
                    assert!(f32::from(news.origin.x + news.size.width) <= f32::from(strip.origin.x + strip.size.width) + 0.5, "{note}: 新建动作留在页签条内");
                    assert!(f32::from(news.origin.x + news.size.width) <= width + 0.5, "{note}: 新建动作留在视口内");
                }
            }
        }
        // 交互边界：右键先激活再弹菜单、中键关闭、关闭按钮走同一份关闭确认、键盘循环切换。
        let (first, second) = cx.update(|cx| {
            let model = &view.read(cx).model;
            (model.tabs[0].id.clone(), model.tabs[1].id.clone())
        });
        // 中键关闭：先做，避免右键菜单的浮层挡住页签。
        cx.update_window(window, |_, window, cx| {
            window.click_with_options(
                SharedString::from(format!("tab-{second}")),
                gpui_kit::test::ClickOptions::new().with_button(gpui_kit::MouseButton::Middle),
                cx,
            )
        })
        .unwrap();
        let remaining = cx.update(|cx| view.read(cx).model.tabs.len());
        assert_eq!(remaining, 2, "{width}: 中键关闭该工作项");
        // 右键非激活标签只打开该标签的上下文菜单，不切换当前激活页。
        let active_before_menu = cx.update(|cx| view.read(cx).model.active_tab.clone());
        cx.update_window(window, |_, window, cx| window.right_click(SharedString::from(format!("tab-{first}")), cx)).unwrap();
        let (active, target, place) = cx.update(|cx| {
            let model = &view.read(cx).model;
            (model.active_tab.clone(), model.tab_menu_target.clone(), model.menu.as_ref().map(|menu| menu.place))
        });
        assert_eq!(active, active_before_menu, "{width}: 右键非激活标签不切换激活页");
        assert_eq!(target.as_deref(), Some(first.as_str()), "{width}: 右键菜单目标指向被点中的标签");
        assert_eq!(place, Some(MenuPlace::TabBar), "{width}: 右键弹出页签菜单");
        cx.update(|cx| view.update(cx, |view, _| {
            view.model.close_menu();
        }));
        let close_id = cx.update(|cx| view.read(cx).model.tabs[0].id.clone());
        cx.update_window(window, |_, window, cx| window.click(SharedString::from(format!("tab-close-{close_id}")), cx)).unwrap();
        let closed = cx.update(|cx| view.read(cx).model.tabs.len());
        assert_eq!(closed, 1, "{width}: 关闭按钮走既有关闭确认并关掉工作项");
        cx.update(|cx| view.update(cx, |view, cx| view.perform("new-query", cx)));
        let before = cx.update(|cx| view.read(cx).model.active_tab.clone());
        cx.update(|cx| view.update(cx, |view, cx| view.perform("next-tab", cx)));
        let after = cx.update(|cx| view.read(cx).model.active_tab.clone());
        assert_ne!(before, after, "{width}: 键盘 Ctrl+Tab 切换工作项");
        let active_id = cx.update(|cx| view.read(cx).model.active_tab.clone().unwrap_or_default());
        let rendered = cx
            .update_window(window, |_, window, cx| {
                window.render_frame(cx);
                let chip = window.find(SharedString::from(format!("tab-{active_id}")));
                (chip.selected(), chip.label().map(str::to_owned))
            })
            .unwrap();
        assert_eq!(rendered.0, Some(true), "{width}: 键盘切换后的当前页签表达选中");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// 页签条是缓存视图：新建页签的第一帧还量不出它的位置，确认可见的那一帧要靠页签条
/// 自己被通知才会重建。这里每帧之间只让事件跑完（不手动通知），新页签仍要在第二帧内确认可见。
#[gpui_kit::test]
fn a_new_tab_in_the_cached_strip_is_revealed_on_the_next_frame(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::SharedString;

    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-shell-tab-reveal-{}", uuid::Uuid::new_v4()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let (window, view) = open_workbench_sized(cx, Arc::clone(&services), false, 640., 900.);
    for _ in 0..12 {
        cx.update(|cx| view.update(cx, |view, cx| view.perform("new-query", cx)));
        cx.run_until_parked();
        let _ = cx.update_window(window, |_, window, cx| window.render_frame(cx));
        cx.run_until_parked();
    }
    let strips = cx.update(|cx| view.read(cx).strip_count.get());
    let _ = cx.update_window(window, |_, window, cx| window.render_frame(cx));
    cx.run_until_parked();
    let active = cx.update(|cx| view.read(cx).model.active_tab.clone().expect("新建后的工作项"));
    let inside = cx
        .update_window(window, |_, window, _| {
            let list = window.find("tab-list").bounds();
            let chip = window.find(SharedString::from(format!("tab-{active}"))).bounds();
            f32::from(chip.origin.x) >= f32::from(list.origin.x) - 0.5
                && f32::from(chip.origin.x + chip.size.width) <= f32::from(list.origin.x + list.size.width) + 0.5
        })
        .unwrap();
    assert!(inside, "新建的页签在下一帧就进入可视区");
    assert!(cx.update(|cx| view.read(cx).tab_scroll_pending).is_none(), "确认可见后结束滚动请求");
    assert!(cx.update(|cx| view.read(cx).strip_count.get()) >= strips, "页签条按需重建");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 640 宽、工作项多到页签条放不下时，新建和 Ctrl+Tab 之后的当前页签要自动进入
/// tab-list 的可视区，尾部新建动作始终留在视口内（R12：当前项到达）。
#[gpui_kit::test]
fn crowded_work_item_tabs_scroll_the_current_one_into_view(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::SharedString;

    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-shell-tab-scroll-{}", uuid::Uuid::new_v4()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let (window, view) = open_workbench_sized(cx, Arc::clone(&services), false, 640., 900.);
    cx.update(|cx| {
        view.update(cx, |view, cx| {
            for _ in 0..9 {
                view.perform("new-query", cx);
            }
        });
    });
    // 滚动的目标位置要画完一帧才量得出来，页签条会自己再要一帧确认；
    // 这里跟着渲染两帧，等同于真实窗口里连续两帧。
    let measure = |window: &mut gpui_kit::Window, cx: &mut gpui_kit::App, active: &str| {
        window.render_frame(cx);
        window.render_frame(cx);
        let list = window.find("tab-list").bounds();
        let chip = window.find(SharedString::from(format!("tab-{active}"))).bounds();
        let new_tab = window.find("tab-new").bounds();
        let inside = f32::from(chip.origin.x) >= f32::from(list.origin.x) - 0.5
            && f32::from(chip.origin.x + chip.size.width) <= f32::from(list.origin.x + list.size.width) + 0.5;
        (inside, new_tab)
    };
    // 新建后的当前项：9 个页签已经超出 640 宽，当前项必须已经在可视区里。
    let active = cx.update(|cx| view.read(cx).model.active_tab.clone().expect("新建后的工作项"));
    let (inside, new_tab) = cx.update_window(window, |_, window, cx| measure(window, cx, &active)).unwrap();
    assert!(inside, "新建后的当前页签落在 tab-list 可视区内");
    assert!(f32::from(new_tab.origin.x + new_tab.size.width) <= 640.5, "尾部新建动作仍在视口内");
    let offset = cx.update(|cx| f32::from(view.read(cx).tab_scroll.offset().x));
    assert!(offset < -0.5, "页签条确实横向滚动了（offset {offset}）");
    // 两帧确认之后滚动请求必须自己结束，否则每帧都会再要一帧重绘。
    let pending = cx.update(|cx| view.read(cx).tab_scroll_pending);
    assert!(pending.is_none(), "确认进入视口后清掉 tab_scroll_pending");
    // Ctrl+Tab 绕一圈回到最后一个：切换后的当前项同样要在可视区里。
    for _ in 0..10 {
        cx.update(|cx| view.update(cx, |view, cx| view.perform("next-tab", cx)));
    }
    let active = cx.update(|cx| view.read(cx).model.active_tab.clone().expect("切换后的工作项"));
    let (inside, new_tab) = cx.update_window(window, |_, window, cx| measure(window, cx, &active)).unwrap();
    assert!(inside, "Ctrl+Tab 之后的当前页签落在 tab-list 可视区内");
    assert!(f32::from(new_tab.origin.x + new_tab.size.width) <= 640.5, "切换后尾部新建动作仍在视口内");
    // 回到第一个工作项：列表要能滚回左边，当前项同样可见。
    for _ in 0..3 {
        cx.update(|cx| view.update(cx, |view, cx| view.perform("prev-tab", cx)));
    }
    let active = cx.update(|cx| view.read(cx).model.active_tab.clone().expect("回退后的工作项"));
    let (inside, _) = cx.update_window(window, |_, window, cx| measure(window, cx, &active)).unwrap();
    assert!(inside, "回到前面的工作项后当前页签同样可见");
    let _ = std::fs::remove_dir_all(&dir);
}


/// 每个进历史页的入口都重读一次本机历史：刚执行的语句不必手动点“刷新”就能看到；
/// 已经在排队时不重复排。
#[test]
fn entering_history_from_any_entry_rereads_local_history_once() {
    for entry in ["activity-history", "welcome-history", "history", "saved-queries"] {
        let mut model = WorkbenchModel::new(Shell::default());
        let _ = model.drain_pending();
        let _ = model.dispatch(entry);
        assert_eq!(model.shell.page, AppPage::History, "{entry} 进历史页");
        assert_eq!(model.drain_pending(), vec![PendingJob::RefreshHistory], "{entry} 重读一次历史");
        let _ = model.dispatch("activity-workspace");
        let _ = model.dispatch(entry);
        let _ = model.dispatch(entry);
        assert_eq!(model.drain_pending(), vec![PendingJob::RefreshHistory], "{entry} 排队中不重复");
    }
}

#[gpui_kit::test]
fn palette_scope_chips_and_clear_filter_and_scroll_offscreen_selection(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::SharedString;

    // 1. 模型层：范围筛选芯片、> 前缀与一键清空过滤词。
    let mut model = palette_model();
    assert!(model.dispatch("command-palette").is_some());
    assert_eq!(model.effective_palette_scope(), super::model::PaletteScope::All);

    assert!(model.dispatch("palette-scope:objects").is_some());
    assert_eq!(model.effective_palette_scope(), super::model::PaletteScope::Objects);
    assert!(
        model.palette_entries().iter().all(|e| matches!(e.group, "table" | "view")),
        "表与视图范围只保留对象"
    );

    assert!(model.dispatch("palette-scope:connections").is_some());
    assert!(
        model.palette_entries().iter().all(|e| matches!(e.group, "connection" | "database")),
        "连接范围只保留连接与库"
    );

    assert!(model.dispatch("palette-scope:saved").is_some());
    assert!(model.palette_entries().iter().all(|e| e.group == "saved"), "保存查询范围只保留保存的查询");

    assert!(model.dispatch("palette-scope:commands").is_some());
    assert!(model.palette_entries().iter().all(|e| e.group == "command"), "命令范围只保留命令");

    model.palette_query = "nonexistent-needle".into();
    assert!(model.palette_entries().is_empty());
    assert!(model.dispatch("palette-clear").is_some());
    assert!(model.palette_query.is_empty());
    assert_eq!(model.effective_palette_scope(), super::model::PaletteScope::All);
    assert!(!model.palette_entries().is_empty(), "清空后恢复全部结果");

    // 2. 渲染层：超过 40 条结果时，End / 上键循环选到末项仍能渲染并滚进可视区。
    let dir = std::env::temp_dir().join(format!("rillsql-palette-scroll-{}", uuid::Uuid::new_v4()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench_sized(cx, services_at(&dir), false, 640., 680.);
    cx.update(|cx| {
        view.update(cx, |view, cx| {
            view.model.connections.rows = vec![record_for("pg", "Atlas", Engine::Postgres)];
            view.model.connections.status.insert("pg".into(), crate::app::views::connections::LinkStatus::Connected);
            let tables: Vec<_> = (0..45)
                .map(|ix| tree_object("pg", "Atlas", "postgres", "shop", "public", &format!("tbl_{ix:02}"), "table"))
                .collect();
            let nav_conn = crate::catalog::NavConnection {
                id: "pg".into(),
                name: "Atlas".into(),
                engine: Engine::Postgres,
                status: crate::catalog::model::ConnStatus::Connected,
                default_database: Some("shop".into()),
                databases: Vec::new(),
                outline_only: false,
                error: None,
            };
            view.query.update(cx, |pane, cx| pane.adopt_navigation(&[nav_conn], tables.clone(), cx));
            let _ = view.model.set_catalog(&tables, "");
            view.perform("command-palette", cx);
        });
    });
    let total = cx.update(|cx| view.read(cx).model.palette_entries().len());
    assert!(total > 50, "总条目应超过旧的 40 条上限：{total}");
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.press("end", cx);
        window.render_frame(cx);
        window.render_frame(cx);
        let last_id = SharedString::from(format!("pal-{}", total - 1));
        let row = window.find(last_id);
        assert_eq!(row.selected(), Some(true), "末项真实渲染且为选中态");
        let list_b = window.find("palette-list").bounds();
        let row_b = row.bounds();
        assert!(
            f32::from(row_b.origin.y + row_b.size.height) <= f32::from(list_b.origin.y + list_b.size.height) + 1.5,
            "End 跳到末项后列表自动滚动到可视区（row_bottom={}, list_bottom={}）",
            f32::from(row_b.origin.y + row_b.size.height),
            f32::from(list_b.origin.y + list_b.size.height),
        );
    })
    .unwrap();
    let offset_y = cx.update(|cx| f32::from(view.read(cx).palette_scroll.offset().y));
    assert!(offset_y < -1.0, "palette-list 实际发生了纵向滚动：{offset_y}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ddl_viewer_supports_retry_and_opening_in_query_editor_without_executing() {
    let (mut model, node_id) = pg_orders_model();
    let mut pg = pg_saved("pg", "dev");
    pg.saved.database = Some("shop".into());
    model.connections.rows.push(pg);
    model.connections.status.insert("pg".into(), crate::app::views::connections::LinkStatus::Connected);

    assert!(model.open_nav_menu(&node_id));
    assert!(model.dispatch("tree-view-ddl").is_some());
    let first_epoch = model.ddl_view.as_ref().expect("打开定义查看器").epoch;
    while model.take_nav_request().is_some() {}

    // 读取失败后可一键重试：重新进入 loading 并入队 NavRequest::Definition。
    model.definition_loaded(first_epoch, Err("读取定义失败：连接闪断".into()));
    assert!(model.ddl_view.as_ref().unwrap().error.is_some());
    assert!(model.dispatch("ddl-retry").is_some());
    let retried = model.ddl_view.as_ref().expect("仍在定义查看器");
    assert!(retried.loading);
    assert!(retried.epoch > first_epoch);
    let second_epoch = retried.epoch;
    assert_eq!(
        model.take_nav_request(),
        Some(super::nav::NavRequest::Definition(node_id.clone())),
        "重试重新派发 NavRequest::Definition"
    );

    // 读取成功后点击“在查询中打开”：关闭查看器、带连接/库打开草稿，绝不自动执行。
    let ddl = crate::catalog::definition::ObjectDdl {
        text: "CREATE TABLE public.orders (id bigint PRIMARY KEY);".into(),
        rebuilt: false,
        missing: Vec::new(),
    };
    model.definition_loaded(second_epoch, Ok(ddl));
    let before_exec = model.executions;
    assert!(model.dispatch("ddl-open-query").is_some());
    assert!(model.ddl_view.is_none(), "转入查询编辑器后关闭定义查看器");
    let tab = model.active_tab().expect("新建查询工作项");
    assert_eq!(tab.draft, "CREATE TABLE public.orders (id bigint PRIMARY KEY);");
    assert_eq!(tab.connection_id.as_deref(), Some("pg"));
    assert_eq!(tab.database.as_deref(), Some("shop"));
    assert_eq!(model.executions, before_exec, "打开 DDL 到编辑器绝不自动执行");
}

#[gpui_kit::test]
fn modals_handle_escape_and_isolate_background_page_navigation(cx: &mut gpui_kit::TestAppContext) {
    let dir = std::env::temp_dir().join(format!("rillsql-modal-keys-{}", uuid::Uuid::new_v4()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench(cx, services_at(&dir), false);
    cx.update(|cx| {
        view.update(cx, |view, _| {
            view.model.connections.rows = vec![
                record_for("c1", "Alpha", Engine::Postgres),
                record_for("c2", "Beta", Engine::Mysql),
            ];
            view.model.connections.selected = Some("c1".into());
            view.model.shell.page = AppPage::Connections;
        });
    });

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let focus = view.read(cx).focus.clone();
        window.focus(&focus, cx);
        window.render_frame(cx);

        // 1. 标签管理层：第一次 Esc 退出内联新建编辑，第二次 Esc 关闭标签管理模态；打开期间下键不动背后的连接选中项。
        view.update(cx, |view, cx| {
            view.perform("tags-manage", cx);
            view.perform("tag-new", cx);
        });
        window.render_frame(cx);
        assert!(view.read(cx).model.tag_manager.as_ref().unwrap().editing.is_some());
        window.press("down", cx);
        assert_eq!(view.read(cx).model.connections.selected.as_deref(), Some("c1"), "模态期间下键不穿透到连接列表");
        window.press("escape", cx);
        window.render_frame(cx);
        assert!(view.read(cx).model.tag_manager.is_some(), "第一次 Esc 只收起内联编辑");
        assert!(view.read(cx).model.tag_manager.as_ref().unwrap().editing.is_none());
        window.focus(&focus, cx);
        window.press("escape", cx);
        window.render_frame(cx);
        assert!(view.read(cx).model.tag_manager.is_none(), "第二次 Esc 关闭标签管理对话框");

        // 2. 快捷键帮助弹层：Esc 直接关闭。
        view.update(cx, |view, cx| view.perform("help", cx));
        window.render_frame(cx);
        assert!(view.read(cx).model.shell.help_open);
        window.press("escape", cx);
        window.render_frame(cx);
        assert!(!view.read(cx).model.shell.help_open, "Esc 关闭快捷键帮助");

        // 3. 删除连接确认层打开时，背景连接列表不响应下键切换，Esc 取消删除确认。
        view.update(cx, |view, cx| view.perform("conn-delete", cx));
        window.render_frame(cx);
        assert!(view.read(cx).model.connections.confirm_delete);
        window.press("down", cx);
        assert_eq!(view.read(cx).model.connections.selected.as_deref(), Some("c1"), "删除确认期间不改背后选中的连接");
        window.press("escape", cx);
        window.render_frame(cx);
        assert!(!view.read(cx).model.connections.confirm_delete);
    })
    .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[gpui_kit::test]
fn settings_steppers_disable_at_min_and_max_bounds_and_sidebar_toggle_shows_selected(cx: &mut gpui_kit::TestAppContext) {
    use crate::app::views::settings::{FONT_RANGE, OBJECT_PAGE_RANGE, PAGE_SIZE_RANGE, PREVIEW_RANGE};

    let dir = std::env::temp_dir().join(format!("rillsql-settings-bounds-{}", uuid::Uuid::new_v4()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench(cx, services_at(&dir), false);

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let initially_open = view.read(cx).model.shell.sidebar_open;
        if initially_open {
            assert_eq!(window.find("top-sidebar").selected(), Some(true));
        } else {
            assert_ne!(window.find("top-sidebar").selected(), Some(true));
        }
        view.update(cx, |view, cx| view.perform("sidebar", cx));
        window.render_frame(cx);
        if !initially_open {
            assert_eq!(window.find("top-sidebar").selected(), Some(true), "顶栏侧栏开关反映当前开合状态");
        } else {
            assert_ne!(window.find("top-sidebar").selected(), Some(true), "顶栏侧栏开关反映当前开合状态");
        }

        // 外观分区：字号下限与上限（禁用的步进按钮不可点、不触发越界写盘；反方向可用且正常步进）。
        view.update(cx, |view, cx| {
            view.perform("settings", cx);
            view.perform("settings-section-appearance", cx);
            view.model.settings.editor_font = FONT_RANGE.0;
            view.model.settings_save = crate::app::views::settings::SaveState::Idle;
            cx.notify();
        });
        window.render_frame(cx);
        window.click("font-down", cx);
        window.render_frame(cx);
        assert_eq!(view.read(cx).model.settings.editor_font, FONT_RANGE.0, "最小字号处减号已禁用，点击不响应");
        assert_eq!(
            view.read(cx).model.settings_save,
            crate::app::views::settings::SaveState::Idle,
            "禁用按钮不触发无意义的写盘"
        );
        window.click("font-up", cx);
        window.render_frame(cx);
        assert!(view.read(cx).model.settings.editor_font > FONT_RANGE.0, "加号仍可用");

        view.update(cx, |view, cx| {
            view.model.settings.editor_font = FONT_RANGE.1;
            view.model.settings_save = crate::app::views::settings::SaveState::Idle;
            cx.notify();
        });
        window.render_frame(cx);
        window.click("font-up", cx);
        window.render_frame(cx);
        assert_eq!(view.read(cx).model.settings.editor_font, FONT_RANGE.1, "最大字号处加号已禁用，点击不响应");
        assert_eq!(view.read(cx).model.settings_save, crate::app::views::settings::SaveState::Idle);

        // 数据读取分区：分页、预览、对象分页上下限同样禁用对应方向点击。
        view.update(cx, |view, cx| {
            view.perform("settings-section-data", cx);
            view.model.settings.page_size = PAGE_SIZE_RANGE.0;
            view.model.settings.preview_kb = PREVIEW_RANGE.1;
            view.model.settings.object_page = OBJECT_PAGE_RANGE.0;
            view.model.settings_save = crate::app::views::settings::SaveState::Idle;
            cx.notify();
        });
        window.render_frame(cx);
        window.click("page-size-down", cx);
        window.click("preview-up", cx);
        window.click("object-page-down", cx);
        window.render_frame(cx);
        assert_eq!(
            view.read(cx).model.settings_save,
            crate::app::views::settings::SaveState::Idle,
            "边界禁用按钮点击均被拦截，不触发空保存"
        );
        window.click("page-size-up", cx);
        window.render_frame(cx);
        assert!(view.read(cx).model.settings.page_size > PAGE_SIZE_RANGE.0, "非边界方向正常步进");
    })
    .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[gpui_kit::test]
fn connections_page_keyboard_navigation_and_copy_address(cx: &mut gpui_kit::TestAppContext) {
    let dir = std::env::temp_dir().join(format!("rillsql-conn-keys-{}", uuid::Uuid::new_v4()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench_sized(cx, services_at(&dir), false, 1024., 720.);
    cx.update(|cx| {
        view.update(cx, |view, _| {
            let mut r1 = record_for("c1", "Alpha", Engine::Postgres);
            r1.saved.host = Some("db.alpha.internal".into());
            r1.saved.port = Some(5432);
            let r2 = record_for("c2", "Beta", Engine::Mysql);
            let r3 = record_for("c3", "Gamma", Engine::Redis);
            view.model.connections.rows = vec![r1, r2, r3];
            view.model.connections.selected = Some("c1".into());
            view.model.shell.page = AppPage::Connections;
        });
    });

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let focus = view.read(cx).focus.clone();
        window.focus(&focus, cx);
        window.render_frame(cx);

        // 复制连接详情地址。
        window.click("conn-detail-copy-addr", cx);
        window.render_frame(cx);
        assert_eq!(view.read(cx).model.copied, "db.alpha.internal:5432");
        assert_eq!(view.read(cx).model.notice_key, Some("conn.page.copiedAddress"));

        // 方向键与 Home/End 在连接启动台列表里移动选中项。
        window.press("down", cx);
        assert_eq!(view.read(cx).model.connections.selected.as_deref(), Some("c2"));
        window.press("end", cx);
        assert_eq!(view.read(cx).model.connections.selected.as_deref(), Some("c3"));
        window.press("home", cx);
        assert_eq!(view.read(cx).model.connections.selected.as_deref(), Some("c1"));
    })
    .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// 粘贴一整段连接串就直接填好字段；逐字输入半截地址不解析；显式“解析”失败时错误落在字段下面。
#[test]
fn pasting_a_connection_url_fills_the_form_and_parse_errors_stay_on_the_field() {
    let mut model = seeded_model(Vec::new());
    let _ = model.dispatch("new-connection");
    model.set_field(DraftField::Url, "postgres://app:s3cret@db.example.test:6543/shop".into());
    let draft = model.connections.editor.clone().expect("草稿");
    assert_eq!(draft.host, "db.example.test", "粘贴后不用再点解析");
    assert_eq!(draft.port, "6543");
    assert_eq!(draft.database, "shop");
    assert_eq!(draft.username, "app");
    assert_eq!(model.connections.secrets.password, "s3cret");

    // 逐字输入：每次只多一个字符，不解析，已填好的字段不被半截地址冲掉。
    model.set_field(DraftField::Url, String::new());
    for (ix, _) in "mysql://".char_indices() {
        model.set_field(DraftField::Url, "mysql://"[..=ix].to_owned());
    }
    assert_eq!(model.connections.editor.as_ref().unwrap().host, "db.example.test");

    // 显式解析失败：问题挂在 url 字段上，不再只丢进状态栏。
    model.set_field(DraftField::Url, "not a url".into());
    let _ = model.dispatch("conn-apply-url");
    assert!(model.connections.issues.iter().any(|issue| issue.field == "url"), "{:?}", model.connections.issues);
    // 改动连接串后旧错误清掉。
    model.set_field(DraftField::Url, "not a url!".into());
    assert!(!model.connections.issues.iter().any(|issue| issue.field == "url"));
}

/// 连接失败的原因按连接留在资源管理器里：连接行下面写出原因，给出重试和编辑；连上后原因消失。
#[test]
fn a_failed_connection_shows_its_reason_with_retry_and_edit_in_the_explorer() {
    use super::explorer::ExplorerItem;
    let mut model = seeded_model(vec![record_for("pg", "Atlas", Engine::Postgres)]);
    model.connections.status.insert("pg".into(), LinkStatus::Failed);
    model.connections.failures.insert("pg".into(), "password authentication failed for user \"app\"".into());
    let rows = model.explorer_rows();
    assert!(
        rows.iter().any(|row| matches!(row, ExplorerItem::ConnectFailed { connection_id, reason } if connection_id == "pg" && reason.contains("password authentication failed"))),
        "{rows:?}"
    );

    let _ = model.dispatch("nav-connect-retry:pg");
    assert_eq!(model.connections.status.get("pg"), Some(&LinkStatus::Connecting), "重试就是再连一次");
    assert!(model.connect_intents.iter().any(|intent| intent.connection_id == "pg"));

    model.connections.status.insert("pg".into(), LinkStatus::Failed);
    let _ = model.dispatch("nav-connect-edit:pg");
    assert!(model.conn_modal.open, "编辑直接打开这条连接的表单");
    assert_eq!(model.connections.editor.as_ref().and_then(|draft| draft.id.as_deref()), Some("pg"));

    // 连上之后不再挂着旧原因。
    model.connections.status.insert("pg".into(), LinkStatus::Connected);
    assert!(!model.explorer_rows().iter().any(|row| matches!(row, ExplorerItem::ConnectFailed { .. })));
}

/// 连接行右键：已连接时第一项是“断开连接”，没连上时是“连接”，两者不同时出现。
#[test]
fn the_connection_menu_offers_disconnect_only_when_connected() {
    let mut model = seeded_model(vec![record_for("pg", "Atlas", Engine::Postgres)]);
    model.connections.selected = Some("pg".into());
    model.open_connection_menu();
    let ids: Vec<&str> = model.menu.as_ref().unwrap().items.iter().map(|item| item.id).collect();
    assert_eq!(ids.first(), Some(&"conn-connect"), "{ids:?}");
    assert!(!ids.contains(&"conn-disconnect"));

    model.connections.status.insert("pg".into(), LinkStatus::Connected);
    model.open_connection_menu();
    let ids: Vec<&str> = model.menu.as_ref().unwrap().items.iter().map(|item| item.id).collect();
    assert_eq!(ids.first(), Some(&"conn-disconnect"), "{ids:?}");
    assert!(!ids.contains(&"conn-connect"), "已连接时不再给“连接”");
}

/// 表数据页与表结构页一键互切，欢迎工作台不再重复渲染左侧同款连接筛选器。
#[test]
fn table_and_structure_views_switch_directly_on_active_object() {
    let mut model = seeded_model(vec![record_for("pg", "Atlas", Engine::Postgres)]);
    model.connections.status.insert("pg".into(), LinkStatus::Connected);
    let _ = model.dispatch("open-table:pg|Atlas|shop|public|table|orders|postgres|dev");
    assert_eq!(model.active_tab().map(|tab| tab.kind), Some(WorkItemKind::Table));

    let _ = model.dispatch("switch-to-structure");
    assert_eq!(model.active_tab().map(|tab| tab.kind), Some(WorkItemKind::Structure));
    assert!(model.active_tab().and_then(|tab| tab.spec.as_deref()).is_some_and(|spec| spec.contains("|orders|")));

    let _ = model.dispatch("switch-to-data");
    assert_eq!(model.active_tab().map(|tab| tab.kind), Some(WorkItemKind::Table));
}

#[test]
fn opening_a_connected_connection_returns_to_the_workspace_and_reveals_it() {
    let mut model = seeded_model(vec![record_for("c1", "Alpha", Engine::Mysql), record_for("c2", "Beta", Engine::Postgres)]);
    model.shell.page = AppPage::Connections;
    model.connections.status.insert("c1".into(), LinkStatus::Connected);
    let _ = model.dispatch("connection:c1");
    assert_eq!(model.shell.page, AppPage::Workspace, "已连接时“打开”要回到工作台，不能停在连接管理页");
    assert!(model.shell.sidebar_open);
    assert_eq!(model.connections.selected.as_deref(), Some("c1"));
    assert!(model.nav_connection_expanded("c1"), "资源管理器里这条连接要展开");

    // 没连上的连接：仍停在连接管理页（连接动作由按需连接接管）。
    model.shell.page = AppPage::Connections;
    let _ = model.dispatch("connection:c2");
    assert_eq!(model.shell.page, AppPage::Connections);
    assert_eq!(model.connections.selected.as_deref(), Some("c2"));
}

#[gpui_kit::test]
fn tab_strip_double_click_url_enter_collapse_all_and_back_navigation_work_cleanly(cx: &mut gpui_kit::TestAppContext) {
    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-ux-flows-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench_sized(cx, services_at(&dir), false, 1200., 800.);

    // 1) 新建首个查询后，双击标签栏右侧空白区新建第二个查询标签。
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.perform("new-query", cx));
        window.render_frame(cx);
        assert_eq!(view.read(cx).model.tabs.len(), 1);
        window.double_click("tabs-empty", cx);
        window.render_frame(cx);
        assert_eq!(view.read(cx).model.tabs.len(), 2, "双击标签条空白区应新建查询标签");
    })
    .unwrap();

    // 2) 进入历史页与设置页时，页头提供“返回工作台”按钮，点击后回到原工作项且不翻转侧栏。
    let sidebar_before = cx.update(|cx| view.read(cx).model.shell.sidebar_open);
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.perform("settings", cx));
        window.render_frame(cx);
        assert!(window.find("settings-back").visible());
        window.click("settings-back", cx);
        window.render_frame(cx);
        assert_eq!(view.read(cx).model.shell.page, AppPage::Workspace);
        assert_eq!(view.read(cx).model.shell.sidebar_open, sidebar_before, "返回工作台不误切换侧栏开关");
    })
    .unwrap();

    // 3) 状态栏快捷键按钮一键打开快捷键面板。
    cx.update_window(window, |_, window, cx| {
        window.click("status-help", cx);
        window.render_frame(cx);
        assert!(view.read(cx).model.shell.help_open, "状态栏键盘图标应打开快捷键帮助");
        view.update(cx, |view, cx| view.perform("help", cx));
        window.render_frame(cx);
    })
    .unwrap();

    // 4) 连接模态 URL 输入框按 Enter 直接解析填充主机、端口、用户与库，不关闭模态。
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.perform("new-connection", cx);
            view.model.set_field(super::model::DraftField::Url, "postgres://alice@db.internal:5433/analytics".into());
        });
        window.render_frame(cx);
        window.press("enter", cx);
        window.render_frame(cx);
        let model = &view.read(cx).model;
        assert!(model.conn_modal.open, "URL 回车解析不应关闭模态");
        let draft = model.connections.editor.as_ref().expect("草稿仍在");
        assert_eq!(draft.host, "db.internal");
        assert_eq!(draft.port, "5433");
        assert_eq!(draft.username, "alice");
        assert_eq!(draft.database, "analytics");
        view.update(cx, |view, cx| view.perform("conn-cancel", cx));
    })
    .unwrap();

    // 5) 资源管理器“全部折叠”一键收起已展开节点。
    cx.update(|cx| {
        view.update(cx, |view, cx| {
            view.model.expand_nav_connection("c1");
            assert!(view.model.nav.has_expanded());
            view.perform("collapse-tree", cx);
            assert!(!view.model.nav.has_expanded(), "collapse-tree 应清空全部展开节点");
        });
    });

    let _ = std::fs::remove_dir_all(&dir);
}

/// 连接缺密码：直接弹出密码框；输入后用这次的密码重连，失败时挂起的动作随之重放。
/// 密码被服务端拒绝时再次弹出并标明“被拒绝”；连接表单开着时不弹（表单自己有密码栏）。
#[tokio::test]
async fn a_missing_password_prompts_and_reconnects_with_the_typed_one() {
    use crate::app::shell::conn_modal::{ConnectAction, ConnectIntent};
    use crate::app::shell::model::DraftField;
    let dir = std::env::temp_dir().join(format!("rillsql-password-prompt-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let mut draft = sample_draft(Engine::Postgres);
    draft.name = "Atlas".into();
    draft.save_password = false;
    let saved = services.connections.save(draft, crate::connections::model::SecretInput::default()).await.unwrap();
    let id = saved.connection.id.clone();
    let mut model = seeded_model(services.connections.list().await.unwrap());

    let intent = ConnectIntent::new(id.clone(), ConnectAction::Expand, None);
    model.queue_connect(intent.clone());
    let token = model.prepare_list_token(&services.tasks);
    let done = jobs::execute_job(&services, request_of(&mut model, token)).await;
    apply_done(&mut model, &services, "connections", done);
    let prompt = model.password_prompt.clone().expect("缺密码时弹出密码框");
    assert_eq!(prompt.id, id);
    assert!(!prompt.rejected, "没有密码不是密码被拒");
    assert_eq!(model.focused_field, Some(DraftField::PromptPassword));
    assert!(model.connect_retry.is_some(), "失败的动作挂着等重放");

    // 空密码不提交。
    let _ = model.dispatch("password-submit");
    assert!(model.password_prompt.is_some());
    assert_eq!(model.pending_len(), 0);

    model.set_field(DraftField::PromptPassword, "typed-secret".into());
    assert_eq!(model.field_value(DraftField::PromptPassword), "typed-secret");
    assert!(!model.password_prompt.as_ref().unwrap().save_password, "默认不写入钥匙串");
    let _ = model.dispatch("password-toggle-save");
    assert!(model.password_prompt.as_ref().unwrap().save_password, "可一键勾选记住到系统钥匙串");
    assert!(!format!("{:?}", model.password_prompt).contains("typed-secret"), "调试输出不带密码");
    let _ = model.dispatch("password-submit");
    assert!(model.password_prompt.is_none());
    assert!(model.connect_retry.is_none(), "挂起的动作随这次连接重放");
    assert_eq!(model.connect_intents, vec![intent]);
    assert_eq!(model.connections.status.get(&id), Some(&LinkStatus::Connecting));
    let token = model.prepare_list_token(&services.tasks);
    let request = request_of(&mut model, token);
    assert_eq!(request.job, PendingJob::OpenConnection);
    assert_eq!(request.secrets.password, "typed-secret", "这次连接用输入的密码");
    assert!(request.secrets.save_password, "勾选后把保存意图一并传给连接任务");

    // 被拒绝：再弹一次并说明原因。
    model.connections.credential_prompt = Some((id.clone(), true));
    let prompt = model.connections.credential_prompt.take().unwrap();
    model.open_password_prompt(&prompt.0, prompt.1);
    assert!(model.password_prompt.as_ref().is_some_and(|prompt| prompt.rejected && prompt.text.is_empty()));
    let _ = model.dispatch("password-cancel");
    assert!(model.password_prompt.is_none());
    assert_ne!(model.focused_field, Some(DraftField::PromptPassword));

    // 连接表单开着：不叠一个密码框。
    model.conn_modal.open = true;
    model.open_password_prompt(&id, false);
    assert!(model.password_prompt.is_none());
    let _ = std::fs::remove_dir_all(dir);
}

/// 测试连接缺密码：表单里的密码占位说明“没存、连接时再问”，测试失败后键盘移到密码框，
/// 不停在顶上的连接串上。
#[tokio::test]
async fn a_failed_test_points_the_keyboard_at_the_blamed_field() {
    let dir = std::env::temp_dir().join(format!("rillsql-test-focus-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let mut draft = sample_draft(Engine::Postgres);
    draft.save_password = false;
    let saved = services.connections.save(draft, crate::connections::model::SecretInput::default()).await.unwrap();
    let id = saved.connection.id.clone();
    let mut model = seeded_model(services.connections.list().await.unwrap());
    assert!(!model.connections.rows.iter().any(|row| row.saved.id == id && row.saved.password_saved));
    model.connections.selected = Some(id.clone());
    model.connections.edit_selected();
    model.conn_modal.open_from(crate::app::model::FocusTarget::Connections, None, "test");
    model.conn_modal.focus_first_pending = false;
    let _ = model.dispatch("conn-test");
    let token = model.prepare_list_token(&services.tasks);
    let done = jobs::execute_job(&services, request_of(&mut model, token)).await;
    apply_done(&mut model, &services, "connections", done);
    let issue = model.connections.issues.first().expect("缺密码要落到字段上");
    assert_eq!(super::chrome::issue_field(&issue.field), Some(crate::app::shell::model::DraftField::Password), "{}", issue.field);
    assert!(model.conn_modal.focus_first_pending, "测试失败后把键盘交给出错的字段");
    for name in ["password", "credentials", "host", "port", "username", "database", "file_path", "ca_file", "ssh_host", "ssh_port", "ssh_user", "ssh_password"] {
        assert!(super::chrome::issue_field(name).is_some(), "{name} 没有对应的输入框");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn credential_failures_are_recognised_by_stage_not_by_driver_code() {
    use crate::app::views::connections::credential_failure;
    use crate::connections::stages::{StageKind, StageStatus, StageView, TestReport, TestStatus};
    let report = |kind: StageKind, code: &str| TestReport {
        status: TestStatus::Err,
        fail_stage: None,
        hint: None,
        summary: None,
        fingerprint: None,
        algorithm: None,
        stages: vec![StageView { kind, label: String::new(), status: StageStatus::Err, message: String::new(), code: Some(code.to_owned()) }],
        code: Some(code.to_owned()),
    };
    assert_eq!(credential_failure(&report(StageKind::DatabaseAuthentication, "conn.password_required")), Some(false));
    assert_eq!(credential_failure(&report(StageKind::DatabaseAuthentication, "conn.auth_failed")), Some(true));
    // PostgreSQL 的认证失败落在通用阶段失败码上，同样算被拒绝。
    assert_eq!(credential_failure(&report(StageKind::DatabaseAuthentication, "conn.stage_failed")), Some(true));
    assert_eq!(credential_failure(&report(StageKind::Network, "conn.network_failed")), None);
    assert_eq!(credential_failure(&report(StageKind::SshAuthentication, "conn.ssh_auth_failed")), None);
}

#[test]
fn tree_tooltips_do_not_repeat_the_node_name() {
    use crate::app::shell::chrome::nav_row_tooltip;
    assert_eq!(nav_row_tooltip("127.0.0.1 / rillsql", "rillsql"), "127.0.0.1 / rillsql");
    assert_eq!(nav_row_tooltip("127.0.0.1 / rillsql / shop", "shop"), "127.0.0.1 / rillsql / shop");
    assert_eq!(nav_row_tooltip("pg / shop / public / orders", "orders"), "pg / shop / public / orders");
    assert_eq!(nav_row_tooltip("pg / shop / public / orders.id", "id"), "pg / shop / public / orders.id");
    // 路径不以自己结尾（分组节点）时照常拼上名字。
    assert_eq!(nav_row_tooltip("pg / shop / public", "表"), "pg / shop / public / 表");
}

#[gpui_kit::test]
fn large_palette_virtualizes_rows_and_end_reaches_the_last_object(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::SharedString;

    let dir = std::env::temp_dir().join(format!("rillsql-palette-large-{}", uuid::Uuid::new_v4()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench_sized(cx, services_at(&dir), false, 640., 680.);
    cx.update(|cx| {
        view.update(cx, |view, cx| {
            view.model.connections.rows = vec![record_for("pg", "Atlas", Engine::Postgres)];
            view.model.connections.status.insert("pg".into(), crate::app::views::connections::LinkStatus::Connected);
            let tables: Vec<_> = (0..6000)
                .map(|ix| tree_object("pg", "Atlas", "postgres", "shop", "public", &format!("tbl_{ix:02}"), "table"))
                .collect();
            let nav_conn = crate::catalog::NavConnection {
                id: "pg".into(),
                name: "Atlas".into(),
                engine: Engine::Postgres,
                status: crate::catalog::model::ConnStatus::Connected,
                default_database: Some("shop".into()),
                databases: Vec::new(),
                outline_only: false,
                error: None,
            };
            view.query.update(cx, |pane, cx| pane.adopt_navigation(&[nav_conn], tables.clone(), cx));
            let _ = view.model.set_catalog(&tables, "");
            view.perform("command-palette", cx);
        });
    });
    let total = cx.update(|cx| view.read(cx).model.palette_entries().len());
    assert!(total > 50, "总条目应超过旧的 40 条上限：{total}");
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find(SharedString::from("pal-3000")).is_none(), "不为屏幕外的几千条候选创建元素");
        window.press("end", cx);
        window.render_frame(cx);
        window.render_frame(cx);
        let last_id = SharedString::from(format!("pal-{}", total - 1));
        let row = window.find(last_id);
        assert_eq!(row.selected(), Some(true), "末项真实渲染且为选中态");
        let list_b = window.find("palette-list").bounds();
        let row_b = row.bounds();
        assert!(
            f32::from(row_b.origin.y + row_b.size.height) <= f32::from(list_b.origin.y + list_b.size.height) + 1.5,
            "End 跳到末项后列表自动滚动到可视区（row_bottom={}, list_bottom={}）",
            f32::from(row_b.origin.y + row_b.size.height),
            f32::from(list_b.origin.y + list_b.size.height),
        );
    })
    .unwrap();
    let offset_y = cx.update(|cx| f32::from(view.read(cx).palette_virtual_scroll.0.borrow().base_handle.offset().y));
    assert!(offset_y < -1.0, "palette-list 实际发生了纵向滚动：{offset_y}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 填了一半的表单不因为一次 Esc / 取消就没了：有未保存的修改时先问一句。
/// 空表单、只选了类型或环境、没动过的已有连接都直接关，不多问。
#[test]
fn closing_a_filled_form_asks_before_discarding() {
    let mut model = seeded_model(vec![record_for("pg", "Atlas", Engine::Postgres)]);

    let _ = model.dispatch("new-connection");
    assert!(!model.conn_dirty());
    model.connections.change_engine(Engine::Mysql);
    model.connections.editor.as_mut().unwrap().env = "prod".into();
    assert!(!model.conn_dirty(), "只选了类型和环境不算填过");
    let _ = model.dispatch("conn-escape");
    assert!(!model.conn_modal.open);

    let _ = model.dispatch("new-connection");
    model.set_field(DraftField::Host, "db.example.test".into());
    assert!(model.conn_dirty());
    let _ = model.dispatch("conn-escape");
    assert!(model.conn_modal.open && model.conn_modal.discard_armed, "第一次只问，不关");
    assert!(model.conn_modal.focus_discard_pending, "键盘交给“继续编辑”");
    let ids: Vec<String> = model.modal_stops().iter().filter_map(|stop| stop.control_id().map(str::to_owned)).collect();
    assert!(ids.iter().any(|id| id == super::conn_modal::DISCARD_KEEP) && ids.iter().any(|id| id == super::conn_modal::DISCARD_CONFIRM));

    // 取消按钮和关闭图标不替用户作答；再按一次 Esc 是“继续编辑”。
    let _ = model.dispatch("conn-cancel");
    let _ = model.dispatch("conn-modal-close");
    assert!(model.conn_modal.open && model.conn_modal.discard_armed);
    let _ = model.dispatch("conn-escape");
    assert!(model.conn_modal.open && !model.conn_modal.discard_armed);
    assert_eq!(model.connections.editor.as_ref().unwrap().host, "db.example.test", "草稿原样留着");
    assert!(!model.modal_stops().iter().any(|stop| stop.control_id() == Some(super::conn_modal::DISCARD_KEEP)));

    // 继续输入就收起确认；只输入了密码也算填过。
    let _ = model.dispatch("conn-cancel");
    assert!(model.conn_modal.discard_armed);
    model.set_field(DraftField::Username, "app".into());
    assert!(!model.conn_modal.discard_armed);
    let _ = model.dispatch("conn-cancel");
    let _ = model.dispatch("conn-discard");
    assert!(!model.conn_modal.open && model.connections.editor.is_none(), "确认后才丢");
    let _ = model.dispatch("new-connection");
    model.set_field(DraftField::Password, "s3cret".into());
    assert!(model.conn_dirty());
    let _ = model.dispatch("conn-discard");

    // 编辑已有连接：没改直接关；改了才问，“继续编辑”留在表单里。
    model.connections.selected = Some("pg".into());
    let _ = model.dispatch("conn-edit");
    let _ = model.dispatch("conn-cancel");
    assert!(!model.conn_modal.open);
    let _ = model.dispatch("conn-edit");
    model.set_field(DraftField::Host, "other.example.test".into());
    let _ = model.dispatch("conn-cancel");
    assert!(model.conn_modal.open && model.conn_modal.discard_armed);
    let _ = model.dispatch("conn-discard-keep");
    assert!(model.conn_modal.open && !model.conn_modal.discard_armed);
    assert!(model.conn_modal.focus_first_pending, "确认条收起后键盘回到表单");
    assert_eq!(model.connections.rows[0].saved.host.as_deref(), Some("db.example.test"), "已保存的配置没有被动过");
}

/// 连按两次回车或连点两下保存：第一次的结果回来之前不再接受第二次，
/// 否则同一份新建草稿会存成两条连接。失败后按钮要能再点。
#[tokio::test]
async fn a_second_save_is_ignored_until_the_first_one_answers() {
    let dir = std::env::temp_dir().join(format!("rillsql-double-save-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let mut model = seeded_model(Vec::new());
    let file = sqlite_file(&dir, "notes.sqlite").await;
    let _ = model.dispatch("conn-engine:sqlite");
    let draft = model.connections.editor.as_mut().unwrap();
    draft.name = "笔记".into();
    draft.file_path = file.display().to_string();

    let _ = model.dispatch("conn-save");
    let _ = model.dispatch("conn-save-connect");
    let _ = model.dispatch("conn-save");
    assert_eq!(model.pending_len(), 1, "只排了一次保存");
    assert!(model.conn_modal.saving);
    assert!(!model.conn_modal.save_and_connect, "被忽略的第二次不改写第一次的意图");
    let _ = model.dispatch("conn-escape");
    assert!(model.conn_modal.open && !model.conn_modal.discard_armed, "保存在路上时 Esc 不弹确认");
    let token = model.prepare_list_token(&services.tasks);
    let done = jobs::execute_job(&services, request_of(&mut model, token)).await;
    apply_done(&mut model, &services, "connections", done);
    assert_eq!(model.connections.rows.len(), 1, "只存出一条");
    assert!(!model.conn_modal.open && !model.conn_modal.saving);

    // 保存失败：模态留着、原因写在表单里，保存重新可用。
    let _ = model.dispatch("new-connection");
    model.conn_modal.saving = true;
    let token = model.prepare_list_token(&services.tasks);
    apply_done(&mut model, &services, "connections", jobs::JobDone::Notice(token, Err("磁盘已满".into())));
    assert!(model.conn_modal.open && !model.conn_modal.saving);
    assert_eq!(model.connections.message_detail, "磁盘已满");
    let _ = std::fs::remove_dir_all(dir);
}

/// 表单上有“保存密码”开关：以前没有，复制或导入的连接（不带密码）编辑时输入的密码
/// 会被静静丢掉，永远存不上。关掉开关则把已存的那份删掉。
#[tokio::test]
async fn the_save_password_switch_decides_whether_the_password_is_kept() {
    let dir = std::env::temp_dir().join(format!("rillsql-save-password-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let mut draft = sample_draft(Engine::Postgres);
    draft.save_password = false;
    let id = services.connections.save(draft, crate::connections::model::SecretInput::default()).await.unwrap().connection.id;
    let mut model = seeded_model(services.connections.list().await.unwrap());
    let saved = |model: &WorkbenchModel| model.connections.rows.iter().find(|row| row.saved.id == id).is_some_and(|row| row.saved.password_saved);
    assert!(!saved(&model));

    model.connections.selected = Some(id.clone());
    let _ = model.dispatch("conn-edit");
    assert!(!model.connections.editor.as_ref().unwrap().save_password);
    assert!(model.modal_stops().iter().any(|stop| stop.control_id() == Some(super::conn_modal::SAVE_PASSWORD)), "开关是表单里的一个停靠点");
    let _ = model.dispatch("conn-save-password-toggle");
    assert!(model.connections.editor.as_ref().unwrap().save_password);
    assert!(model.conn_dirty(), "拨开关也是一处修改");
    model.set_field(DraftField::Password, "s3cret".into());
    let _ = model.dispatch("conn-save");
    let token = model.prepare_list_token(&services.tasks);
    let done = jobs::execute_job(&services, request_of(&mut model, token)).await;
    apply_done(&mut model, &services, "connections", done);
    assert!(saved(&model), "打开开关后输入的密码存进了钥匙串");

    let _ = model.dispatch("conn-edit");
    assert!(model.connections.editor.as_ref().unwrap().save_password, "开关显示实际状态");
    let _ = model.dispatch("conn-save-password-toggle");
    let _ = model.dispatch("conn-save");
    let token = model.prepare_list_token(&services.tasks);
    let done = jobs::execute_job(&services, request_of(&mut model, token)).await;
    apply_done(&mut model, &services, "connections", done);
    assert!(!saved(&model), "关掉开关后已存的密码被删除");
    let _ = std::fs::remove_dir_all(dir);
}

/// 副本按界面语言命名并避开重名；上一张表单粘贴的连接串（可能带密码）不留到下一张表单；
/// 连接串解析成功有就地提示，连接串再被改动就收起。
#[test]
fn duplicates_are_named_uniquely_and_the_url_box_starts_empty() {
    let mut model = seeded_model(vec![record_for("pg", "Atlas", Engine::Postgres)]);
    let first = i18n::fill(&model.text("ui.conn.copyName"), &[("name", "Atlas")]);
    assert_ne!(first, "Atlas copy", "默认界面是中文，副本名不该是英文后缀");
    model.connections.rows.push(record_for("pg2", &first, Engine::Postgres));

    let _ = model.dispatch("new-connection");
    model.set_field(DraftField::Url, "postgres://app:s3cret@db.example.test:5432/shop".into());
    assert!(model.conn_modal.url_applied, "粘贴的连接串解析成功");
    assert_eq!(model.connections.editor.as_ref().unwrap().host, "db.example.test");
    model.set_field(DraftField::Url, "postgres://app:s3cret@db.example.test:5432/sho".into());
    assert!(!model.conn_modal.url_applied, "连接串又被改了：提示收起");
    let _ = model.dispatch("conn-apply-url");
    assert!(model.conn_modal.url_applied, "点“解析”同样有提示");
    let _ = model.dispatch("conn-discard");
    assert!(model.field_value(DraftField::Url).is_empty(), "关闭后不留连接串");

    model.connections.selected = Some("pg".into());
    model.set_field(DraftField::Url, "postgres://app:s3cret@db.example.test/shop".into());
    let _ = model.dispatch("conn-duplicate");
    let draft = model.connections.editor.as_ref().unwrap();
    assert_eq!(draft.name, format!("{first} 2"), "已有同名副本时编号");
    assert!(draft.id.is_none());
    assert!(model.field_value(DraftField::Url).is_empty(), "打开另一张表单时连接串框是空的");
    assert!(!model.conn_modal.url_applied);
    assert!(!model.conn_dirty(), "没动过的副本关掉不用问");
    let _ = model.dispatch("conn-cancel");
    assert!(!model.conn_modal.open);
}

/// 测试结论只说屏幕上这份配置：成功后改了地址，结论就收起；字段不合法时不发请求，
/// 键盘落到第一个要改的字段上；服务端拒绝凭据的结论在改了用户名或密码后收起。
#[tokio::test]
async fn a_test_verdict_is_retired_when_the_draft_changes() {
    let dir = std::env::temp_dir().join(format!("rillsql-verdict-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let mut model = seeded_model(Vec::new());
    let file = sqlite_file(&dir, "notes.sqlite").await;
    let _ = model.dispatch("conn-engine:sqlite");
    model.conn_modal.focus_first_pending = false;
    let _ = model.dispatch("conn-test");
    assert_eq!(model.pending_len(), 0, "没选文件不发请求");
    assert!(model.conn_modal.focus_first_pending, "键盘落到要改的字段上");
    let _ = model.dispatch("conn-save");
    assert_eq!(model.pending_len(), 0);
    assert!(!model.conn_modal.saving, "没通过校验的保存不占着按钮");

    model.set_field(DraftField::File, file.display().to_string());
    let _ = model.dispatch("conn-test");
    let token = model.prepare_list_token(&services.tasks);
    let done = jobs::execute_job(&services, request_of(&mut model, token)).await;
    apply_done(&mut model, &services, "connections", done);
    assert_eq!(model.connections.message_key, Some("connections.editor.test.ok"));
    let report = model.connections.test_report.clone().expect("留着各阶段的结果");
    assert!(report.stages.len() > 1);

    model.set_field(DraftField::Name, "笔记".into());
    assert!(model.connections.test_report.is_none(), "草稿改了，上一次的结论不再成立");
    assert_eq!(model.connections.message_key, None);

    model.connections.issues.push(crate::app::views::connections::FieldIssue::new("credentials", "connections.status.error"));
    model.set_field(DraftField::Password, "retry".into());
    assert!(model.connections.issues.is_empty(), "改了密码，“凭据被拒”的结论收起");
    let _ = std::fs::remove_dir_all(dir);
}

/// 真实绘制：换类型保留已填字段；保存密码开关可点；测试结论与放弃确认都画在页脚上方、
/// 面板之内（表单再长也不在滚动区里）；Esc → 确认 → Esc 继续编辑 → 放弃；
/// 画出来的控件与 Tab 序列一致；Cmd/Ctrl+Enter 就是“保存并连接”。
#[gpui_kit::test]
fn the_modal_keeps_verdicts_and_the_discard_prompt_above_the_footer(cx: &mut gpui_kit::TestAppContext) {
    use crate::connections::stages::{StageKind, StageStatus, StageView, TestReport, TestStatus};
    let dir = std::env::temp_dir().join(format!("rillsql-modal-strip-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench(cx, services_at(&dir), false);
    cx.update(|cx| view.update(cx, |view, cx| view.perform("new-connection", cx)));
    let bottom = |snapshot: &gpui_kit::test::ElementSnapshot| f32::from(snapshot.bounds().origin.y + snapshot.bounds().size.height);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("conn-modal-status").is_none(), "没有结论时不占地方");
        view.update(cx, |view, _| {
            view.model.set_field(DraftField::Host, "db.example.test".into());
            view.model.set_field(DraftField::Username, "app".into());
        });
        window.render_frame(cx);
        window.click("conn-engine-mariadb", cx);
        window.render_frame(cx);
        {
            let draft = view.read(cx).model.connections.editor.clone().unwrap();
            assert_eq!(draft.engine, Engine::Mariadb);
            assert_eq!((draft.host.as_str(), draft.username.as_str(), draft.port.as_str()), ("db.example.test", "app", "3306"), "换类型不清空表单");
        }

        assert!(window.find("conn-save-password").visible());
        window.click("conn-save-password", cx);
        window.render_frame(cx);
        assert!(!view.read(cx).model.connections.editor.as_ref().unwrap().save_password);
        window.click("conn-save-password", cx);
        window.render_frame(cx);
        assert!(view.read(cx).model.connections.editor.as_ref().unwrap().save_password);

        // 最长的表单形状 + 一条失败结论：结论仍在面板里、页脚按钮正上方。
        view.update(cx, |view, _| {
            let draft = view.model.connections.editor.as_mut().unwrap();
            draft.tls_mode = crate::connections::model::TlsMode::Verify;
            draft.ca_mode = CaMode::File;
            draft.ssh_on = true;
            let stage = |kind: StageKind, status: StageStatus, message: &str| StageView { kind, label: kind.label_zh().to_owned(), status, message: message.to_owned(), code: None };
            view.model.connections.test_report = Some(TestReport {
                status: TestStatus::Err,
                fail_stage: Some(StageKind::DatabaseAuthentication.label_zh().to_owned()),
                hint: Some("检查用户名和密码".into()),
                summary: None,
                fingerprint: None,
                algorithm: None,
                stages: vec![
                    stage(StageKind::Network, StageStatus::Ok, "已连接"),
                    stage(StageKind::Tls, StageStatus::Ok, "已加密"),
                    stage(StageKind::DatabaseAuthentication, StageStatus::Err, "Access denied for user 'app'"),
                    stage(StageKind::ServerVersion, StageStatus::Skipped, "未执行"),
                ],
                code: None,
            });
            view.model.connections.message_key = Some("connections.editor.test.failed");
        });
        window.render_frame(cx);
        let verdict = window.find("conn-test-verdict");
        assert!(verdict.visible());
        let label = verdict.label().unwrap_or_default().to_string();
        assert!(label.contains(StageKind::DatabaseAuthentication.label_zh()) && label.contains("Access denied") && label.contains("检查用户名和密码"), "{label}");
        let save = window.find("conn-save-connect");
        assert!(save.visible());
        assert!(bottom(&verdict) <= f32::from(save.bounds().origin.y) + 0.5, "结论在页脚按钮上方");
        assert!(bottom(&save) <= 800.5, "页脚仍在窗口内");
        let test = window.find("conn-test");
        let cancel = window.find("conn-cancel");
        assert!(f32::from(test.bounds().origin.x) < f32::from(cancel.bounds().origin.x), "测试在左，离开表单的动作在右");
        assert!(f32::from(cancel.bounds().origin.x) < f32::from(save.bounds().origin.x));

        // 改了字段：结论收起。
        view.update(cx, |view, _| view.model.set_field(DraftField::Username, "root".into()));
        window.render_frame(cx);
        assert!(window.try_find("conn-test-verdict").is_none());

        // Esc：先问；确认条的两个按钮进入 Tab 序列，画出来的与序列一致。
        window.press("escape", cx);
        window.render_frame(cx);
        let prompt = window.find("conn-discard-prompt");
        assert!(prompt.visible() && view.read(cx).model.conn_modal.open);
        assert!(bottom(&prompt) <= f32::from(window.find("conn-save-connect").bounds().origin.y) + 0.5);
        {
            let view = view.read(cx);
            let expected: Vec<String> = view.model.modal_stops().iter().filter_map(|stop| stop.control_id().map(str::to_owned)).collect();
            let rendered = view.model.conn_modal.rendered.clone();
            assert!(rendered.iter().any(|id| id == "conn-discard-keep") && rendered.iter().any(|id| id == "conn-discard"));
            assert_eq!(rendered.len(), expected.len(), "{rendered:?} vs {expected:?}");
        }
        // 键盘在“继续编辑”上：回车留下来，不丢表单。
        window.press("enter", cx);
        window.render_frame(cx);
        assert!(view.read(cx).model.conn_modal.open && window.try_find("conn-discard-prompt").is_none());
        window.press("escape", cx);
        window.render_frame(cx);
        window.press("escape", cx);
        window.render_frame(cx);
        assert!(view.read(cx).model.conn_modal.open && window.try_find("conn-discard-prompt").is_none(), "再按 Esc 是继续编辑");
        assert_eq!(view.read(cx).model.connections.editor.as_ref().unwrap().host, "db.example.test");

        // Cmd/Ctrl+Enter：保存并连接。这份草稿缺 CA 文件，所以停在校验上，但确实是那条命令。
        window.dispatch_action(Box::new(crate::app::ExecuteQuery), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        {
            let model = &view.read(cx).model;
            assert!(model.conn_modal.save_and_connect, "Cmd/Ctrl+Enter 走的是保存并连接");
            assert!(model.connections.issues.iter().any(|issue| issue.field == "ca_file"));
            assert!(!model.commands.iter().any(|id| id == "execute-query"), "没有落到背后的查询上");
        }
        window.press("escape", cx);
        window.render_frame(cx);
        window.click("conn-discard", cx);
        window.render_frame(cx);
        assert!(!view.read(cx).model.conn_modal.open, "放弃修改后关闭");
        assert!(view.read(cx).model.connections.rows.is_empty());
    })
    .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn localized_group_tooltips_and_english_verdict_stages_never_leak_raw_slugs_or_chinese() {
    use crate::app::shell::chrome::localized_nav_tooltip;
    use crate::connections::error_codes;
    use crate::connections::stages::{
        localize_report_hint, localize_report_summary, localize_stage_label, localize_stage_message,
        StageKind, StageStatus, StageView, TestReport, TestStatus,
    };
    use crate::i18n::Locale;

    assert_eq!(localized_nav_tooltip("Shop 商城主库 / main / view", "view", "视图", true), "Shop 商城主库 / main / 视图");
    assert_eq!(localized_nav_tooltip("Shop 商城主库 / main / view", "view", "Views", true), "Shop 商城主库 / main / Views");
    assert_eq!(localized_nav_tooltip("pg / shop / public / orders", "orders", "orders", false), "pg / shop / public / orders");

    let stage = StageView {
        kind: StageKind::DatabaseAuthentication,
        label: StageKind::DatabaseAuthentication.label_zh().to_owned(),
        status: StageStatus::Err,
        message: "请填写数据库密码".to_owned(),
        code: Some(error_codes::CONN_PASSWORD_REQUIRED.to_owned()),
    };
    let report = TestReport {
        status: TestStatus::Err,
        fail_stage: Some(stage.label.clone()),
        hint: Some("凭据库没有可用密码，本次也没有输入。".to_owned()),
        summary: None,
        fingerprint: None,
        algorithm: None,
        stages: vec![stage.clone()],
        code: Some(error_codes::CONN_PASSWORD_REQUIRED.to_owned()),
    };
    assert_eq!(localize_stage_label(&stage, Locale::En), "Authenticate database");
    assert_eq!(localize_stage_message(&stage, Locale::En), "Database password is required");
    assert!(localize_report_hint(&report, Locale::En).unwrap().contains("keychain"));
    assert_eq!(localize_report_summary("外键检查：已开启", Locale::En), "Foreign keys: enabled");
    assert_eq!(localize_stage_label(&stage, Locale::ZhCn), StageKind::DatabaseAuthentication.label_zh());
}

#[test]
fn backdrop_dismiss_guards_dirty_modal_and_explorer_connection_menu_reaches_relational_tools() {
    use crate::catalog::hierarchy::connection_node_id;
    use crate::catalog::model::{ObjectKind, ObjectRow};
    let mut model = seeded_model(vec![record_for("shop", "Shop", Engine::Sqlite)]);
    model.connections.status.insert("shop".into(), LinkStatus::Connected);
    let sqlite_nav = nav_connection(
        Engine::Sqlite,
        "shop",
        "Shop",
        vec![nav_database("main", vec![ObjectRow::fresh("orders", ObjectKind::Table)])],
    );
    model.apply_navigation(&[sqlite_nav], false);
    model.nav.expand(&connection_node_id("shop"));

    // 工作台新建连接按钮不切走底页；未改动时点遮罩直接关闭，改过字段后点遮罩先弹放弃确认且不关模态。
    assert_eq!(model.shell.page, crate::app::AppPage::Workspace);
    let _ = model.dispatch("conn-new-sidebar");
    assert_eq!(model.shell.page, crate::app::AppPage::Workspace, "工作台新建连接保持在工作台");
    let _ = model.dismiss();
    assert!(!model.conn_modal.open, "未填内容的模态点遮罩即关");

    let _ = model.dispatch("conn-new-sidebar");
    model.set_field(DraftField::Host, "db.internal".into());
    let _ = model.dismiss();
    assert!(model.conn_modal.open && model.conn_modal.discard_armed, "填过内容的模态点遮罩先确认");
    assert!(model.has_blocking_dialog(), "确认期间仍视为阻塞弹窗，不还焦底页");
    let _ = model.dispatch("conn-discard");
    assert!(!model.conn_modal.open);

    // 密码框与删除确认同样归 dismiss 关闭。
    model.open_password_prompt("shop", false);
    assert!(model.password_prompt.is_some());
    let _ = model.dispatch("password-toggle-save");
    assert!(model.password_prompt.as_ref().unwrap().save_password);
    let _ = model.dismiss();
    assert!(model.password_prompt.is_none());

    // 资源管理器已连接关系型库的连接行右键菜单带刷新、建表、关系图、对比与管理页，并携带该库身份。
    model.connections.selected = Some("shop".into());
    let _ = model.dispatch("menu-explorer-connection");
    let ids: Vec<&str> = model.menu.as_ref().expect("menu").items.iter().map(|item| item.id).collect();
    for expected in ["conn-disconnect", "tree-refresh-node", "tree-create-table", "open-diagram", "compare-object", "open-admin"] {
        assert!(ids.contains(&expected), "资源管理器连接菜单应包含 {expected}: {ids:?}");
    }
    assert_eq!(model.nav_scope_target().map(|t| t.connection_id), Some("shop".to_owned()));
}

#[gpui_kit::test]
fn portal_dialog_failure_falls_back_to_sqlite_modal_with_friendly_guidance(cx: &mut gpui_kit::TestAppContext) {
    let dir = std::env::temp_dir().join(format!("rillsql-portal-fallback-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench(cx, services_at(&dir), false);
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.finish_open_prompt::<&str>(
                Ok(Err("Portal request failed: org.freedesktop.DBus.Error.ServiceUnknown: Portal not provided")),
                cx,
            );
        });
        window.render_frame(cx);
        let model = &view.read(cx).model;
        assert!(model.conn_modal.open, "Portal 失败后自动降级打开 SQLite 连接模态框");
        assert_eq!(model.connections.editor.as_ref().map(|d| d.engine), Some(Engine::Sqlite));
        assert_eq!(model.notice_key, Some("ui.sqlite.portalFallback"));
        assert!(window.find("conn-issue-file_path").visible(), "文件路径框下方直接给出友好说明");
    })
    .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn opening_sql_file_preserves_filename_title_and_connection_target() {
    let mut model = seeded_model(vec![record_for("uat", "qsyn-uat", Engine::Mysql)]);
    model.connections.status.insert("uat".into(), LinkStatus::Connected);
    let _ = model.dispatch("new-query");
    if let Some(tab) = model.tabs.last_mut() {
        tab.connection_id = Some("uat".into());
        tab.connection_name = Some("qsyn-uat".into());
        tab.database = Some("qsyn3_tms_business2".into());
    }

    model.open_sql_file(
        std::path::Path::new("/tmp/driver_ranking_report.sql"),
        "SELECT * FROM workshop_driver_ranking LIMIT 20;".into(),
    );
    let id = model.active_tab.clone().expect("active sql tab id");
    let opened = model.tabs.iter().find(|t| t.id == id).expect("opened sql tab");
    assert_eq!(opened.title, "driver_ranking_report.sql");
    assert_eq!(opened.path.as_deref(), Some("/tmp/driver_ranking_report.sql"));
    assert_eq!(opened.connection_id.as_deref(), Some("uat"), "继承当前活动数据库目标");
    assert_eq!(opened.database.as_deref(), Some("qsyn3_tms_business2"));

    // 重复打开同一个路径的 .sql 文件只聚焦并刷新内容，不产生重复标签页；切换语言也不抹掉文件名。
    let tabs_before = model.tabs.len();
    model.open_sql_file(
        std::path::Path::new("/tmp/driver_ranking_report.sql"),
        "SELECT id, driver_name FROM workshop_driver_ranking;".into(),
    );
    let id_again = model.active_tab.clone().expect("active sql tab id");
    assert_eq!(id_again, id);
    assert_eq!(model.tabs.len(), tabs_before);
    let _ = model.dispatch("locale-en");
    let after_locale = model.tabs.iter().find(|t| t.id == id).expect("opened sql tab");
    assert_eq!(after_locale.title, "driver_ranking_report.sql");
}

/// 搜索结果：命中少的库直接展开，命中多的库先收起并标出命中数；搜索期间任何层级都能手动折叠 / 展开，
/// 清除搜索后回到原来的展开现场。
#[test]
fn nav_search_opens_small_branches_collapses_large_ones_and_stays_toggleable() {
    use crate::catalog::hierarchy::SEARCH_AUTO_OPEN;
    use crate::catalog::model::{ObjectKind, ObjectRow};
    let mut model = seeded_model(vec![record_for("main", "Main", Engine::Mysql)]);
    model.connections.status.insert("main".into(), LinkStatus::Connected);
    let few: Vec<ObjectRow> = vec![ObjectRow::fresh("waybill_info", ObjectKind::Table), ObjectRow::fresh("orders", ObjectKind::Table)];
    let many: Vec<ObjectRow> = (0..SEARCH_AUTO_OPEN + 5).map(|i| ObjectRow::fresh(format!("waybill_log_{i}"), ObjectKind::Table)).collect();
    model.apply_navigation(&[nav_connection(Engine::Mysql, "main", "Main", vec![nav_database("small", few), nav_database("big", many)])], false);
    let conn = crate::catalog::hierarchy::connection_node_id("main");
    model.nav_search("waybill");
    let rows = model.nav_rows_for("main");
    let small = rows.iter().find(|row| row.label == "small").expect("命中少的库在结果里");
    let big = rows.iter().find(|row| row.label == "big").expect("命中多的库在结果里");
    assert!(small.expanded, "命中少的库默认展开");
    assert!(!big.expanded, "命中多的库默认收起");
    assert_eq!(big.search_hits, SEARCH_AUTO_OPEN + 5, "收起的库标出命中数");
    assert!(rows.iter().any(|row| row.label == "waybill_info"));
    assert!(!rows.iter().any(|row| row.label.starts_with("waybill_log_")), "收起的库不把几十张表铺出来");

    // 搜索期间可以折叠展开的库、展开收起的库。
    let small_id = small.id.clone();
    let big_id = big.id.clone();
    let _ = model.dispatch(&format!("nav-toggle:{small_id}"));
    let _ = model.dispatch(&format!("nav-toggle:{big_id}"));
    let rows = model.nav_rows_for("main");
    assert!(!rows.iter().any(|row| row.label == "waybill_info"), "搜索中折叠生效");
    assert!(rows.iter().any(|row| row.label.starts_with("waybill_log_")), "搜索中展开生效");
    assert!(model.nav_connection_expanded("main"), "连接在搜索中默认展开");
    model.toggle_nav_connection("main");
    assert!(model.nav_rows_for("main").is_empty(), "搜索中连接也能收起");

    model.nav_clear_search();
    assert!(!model.nav.is_expanded(&big_id), "清除搜索后不残留搜索里的展开");
    let _ = conn;
}

/// 跟随系统主题：窗口外观事件经 ApplyTheme 落到实际绘制主题，显式偏好不被覆盖，
/// 重复外观不重复应用，草稿、标签和后台任务现场原样保留。
/// GPUI 的 TestWindow::simulate_appearance_change 只在 gpui crate 内可达，这里直接驱动
/// 订阅回调调用的同一个落点 sync_window_appearance。
#[gpui_kit::test]
fn system_appearance_events_drive_follow_system_theme_without_touching_the_workspace(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::component::ActiveTheme as _;
    use gpui_kit::WindowAppearance;

    let dir = std::env::temp_dir().join(format!("rillsql-shell-appearance-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench(cx, services_at(&dir), false);
    cx.update(crate::app::bind_keys);
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.perform("new-query", cx);
            view.perform("new-query", cx);
        });
        window.render_frame(cx);
        window.press("secondary-a", cx);
        window.input("select 7", cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();

    let applied = std::rc::Rc::new(std::cell::Cell::new(0usize));
    let _theme_watch = cx.update(|cx| {
        let applied = std::rc::Rc::clone(&applied);
        cx.observe_global::<gpui_kit::component::Theme>(move |_| applied.set(applied.get() + 1))
    });
    let snapshot = |cx: &mut gpui_kit::TestAppContext| {
        cx.update(|cx| {
            let view = view.read(cx);
            (
                view.query_editor_text(cx),
                view.model.active_tab.clone(),
                view.model.tabs.iter().map(|tab| tab.id.clone()).collect::<Vec<_>>(),
                view.model.pending.len(),
                view.model.shell.page,
                view.model.settings.theme.clone(),
            )
        })
    };
    let before = snapshot(cx);
    assert_eq!(before.0, "select 7");
    assert_eq!(before.5, "system");
    let event = |cx: &mut gpui_kit::TestAppContext, appearance: WindowAppearance| {
        cx.update_window(window, |_, window, cx| {
            view.update(cx, |view, cx| view.sync_window_appearance(appearance, cx));
            window.render_frame(cx);
        })
        .unwrap();
        cx.run_until_parked();
    };
    let state = |cx: &mut gpui_kit::TestAppContext| {
        cx.update(|cx| (view.read(cx).shell().prefers_dark, view.read(cx).shell().resolved_theme(), cx.theme().is_dark()))
    };
    assert!(!cx.update(|cx| cx.theme().is_dark()), "测试窗口初始为浅色外观");

    // System：Light → Dark / VibrantDark / VibrantLight 都跟着走，同一明暗重复到达不再应用。
    event(cx, WindowAppearance::Dark);
    assert_eq!(state(cx), (true, crate::app::ResolvedTheme::Dark, true), "系统变深后跟随系统必须立即变深");
    let after_dark = applied.get();
    assert!(after_dark > 0, "外观变化必须走 ApplyTheme 更新组件库主题");
    event(cx, WindowAppearance::VibrantDark);
    event(cx, WindowAppearance::Dark);
    assert_eq!(applied.get(), after_dark, "同一明暗的重复外观不重复应用主题");
    event(cx, WindowAppearance::VibrantLight);
    assert_eq!(state(cx), (false, crate::app::ResolvedTheme::Light, false), "Vibrant 浅色同样切回浅色");

    // 显式偏好：系统外观只被记录，不改变解析主题；切回 System 时用最近一次外观。
    cx.update(|cx| view.update(cx, |view, cx| view.perform("theme-light", cx)));
    let explicit = applied.get();
    event(cx, WindowAppearance::Dark);
    assert_eq!(state(cx), (true, crate::app::ResolvedTheme::Light, false), "显式浅色不被系统深色覆盖");
    assert_eq!(applied.get(), explicit, "显式偏好下外观事件不应用主题");
    cx.update(|cx| view.update(cx, |view, cx| view.perform("theme-system", cx)));
    assert_eq!(state(cx), (true, crate::app::ResolvedTheme::Dark, true), "切回跟随系统立即使用最近的系统深色");
    cx.update(|cx| view.update(cx, |view, cx| view.perform("theme-dark", cx)));
    event(cx, WindowAppearance::Light);
    assert_eq!(state(cx), (false, crate::app::ResolvedTheme::Dark, true), "显式深色不被系统浅色覆盖");
    cx.update(|cx| view.update(cx, |view, cx| view.perform("theme-system", cx)));
    assert_eq!(state(cx), (false, crate::app::ResolvedTheme::Light, false), "切回跟随系统使用最近的系统浅色");
    cx.run_until_parked();

    // 外观事件前后，草稿、活动标签、工作项和排队作业不变；只有显式切换改了主题设置。
    event(cx, WindowAppearance::Dark);
    let after = snapshot(cx);
    assert_eq!(after.0, before.0, "草稿保留");
    assert_eq!(after.1, before.1, "活动标签保留");
    assert_eq!(after.2, before.2, "工作项不重建");
    assert_eq!(after.3, before.3, "不重放或新增后台作业");
    assert_eq!(after.4, before.4, "页面不变");
    assert_eq!(after.5, "system");
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------- 对象树悬停动作与命中标记（T4）

/// 对象树夹具里的稳定节点 id：长名称表、普通表、视图和读取失败的表。
struct ObjectTreeIds {
    long: String,
    long_name: String,
    orders: String,
    recent: String,
    audit: String,
    expanded: Vec<String>,
}

/// 一条已连上的 PostgreSQL 连接，shop/public 下有长名称表、普通表、视图和一张读取失败的表；
/// 树沿用户点击的同一批命令展开到对象行。目录只在模型里，不访问任何数据库。
/// 外观走 T3 的系统外观同步入口（跟随系统偏好），浅/深色是真实应用到组件库的主题。
fn object_tree_workbench(
    cx: &mut gpui_kit::TestAppContext,
    width: f32,
    dark: bool,
) -> (gpui_kit::AnyWindowHandle, gpui_kit::Entity<super::view::Workbench>, std::path::PathBuf, ObjectTreeIds) {
    use crate::catalog::hierarchy::{connection_node_id, database_node_id, group_node_id, schema_node_id};
    use crate::catalog::model::{ObjectKind, ObjectRow};
    let dir = std::env::temp_dir().join(format!("rillsql-shell-tree-hover-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench_sized(cx, services_at(&dir), false, width, 800.);
    let long_name = format!("{}waybill", "customer_order_settlement_archive_history_".repeat(2));
    let long = ObjectRow::table(long_name.clone(), Some(12));
    let orders = ObjectRow::table("orders", Some(4));
    let recent = ObjectRow::view("recent");
    let audit = ObjectRow::table("waybill_audit", Some(1));
    let ids = ObjectTreeIds {
        long: nav_id("pg", "shop", "public", &long),
        long_name,
        orders: nav_id("pg", "shop", "public", &orders),
        recent: nav_id("pg", "shop", "public", &recent),
        audit: nav_id("pg", "shop", "public", &audit),
        expanded: vec![
            connection_node_id("pg"),
            database_node_id("pg", "shop"),
            schema_node_id("pg", "shop", "public"),
            group_node_id("pg", "shop", "public", ObjectKind::Table),
            group_node_id("pg", "shop", "public", ObjectKind::View),
        ],
    };
    let connection = nav_connection(Engine::Postgres, "pg", "销售库", vec![nav_database("shop", vec![long, orders, recent, audit])]);
    let expand = ids.expanded[1..].to_vec();
    let audit_id = ids.audit.clone();
    cx.update(|cx| {
        view.update(cx, |view, cx| {
            view.perform("activity-workspace", cx);
            view.model.connections.rows = vec![record_for("pg", "销售库", Engine::Postgres)];
            view.model.connections.status.insert("pg".to_owned(), LinkStatus::Connected);
            view.model.apply_navigation(&[connection], false);
            view.model.expand_nav_connection("pg");
            for id in &expand {
                assert!(view.model.dispatch(&format!("nav-toggle:{id}")).is_some(), "{id} 没有接受展开命令");
            }
            assert!(view.model.nav.set_error(&audit_id, "permission denied for table waybill_audit"));
            view.model.shell.sidebar_open = true;
            if dark {
                view.sync_window_appearance(gpui_kit::WindowAppearance::Dark, cx);
            }
            cx.notify();
        });
    });
    {
        use gpui_kit::component::ActiveTheme as _;
        assert_eq!(cx.update(|cx| cx.theme().is_dark()), dark, "{width}: 主题经外观同步应用");
    }
    (window, view, dir, ids)
}

fn ensure_sidebar_open(cx: &mut gpui_kit::TestAppContext, view: &gpui_kit::Entity<super::view::Workbench>) {
    cx.update(|cx| {
        view.update(cx, |view, cx| {
            if !view.model.shell.sidebar_open {
                view.model.shell.sidebar_open = true;
                cx.notify();
            }
        })
    });
}

/// 表名默认用满行宽；悬停快捷动作只在真实鼠标悬停时浮现，点击打开的是被悬停那一行的
/// 数据或结构，不展开这一行也不落到别的对象上。覆盖 1360/900/640 × 浅/深色。
#[gpui_kit::test]
fn object_tree_hover_actions_open_the_hovered_object_at_every_width_and_theme(cx: &mut gpui_kit::TestAppContext) {
    for width in [1360f32, 900., 640.] {
        for dark in [false, true] {
            let (window, view, dir, ids) = object_tree_workbench(cx, width, dark);
            let tag = format!("{width}/{}", if dark { "dark" } else { "light" });
            let row_id = gpui_kit::SharedString::from(format!("nav-{}", ids.long));
            let data_id = gpui_kit::SharedString::from(format!("nav-quick-data-{}", ids.long));
            let struct_id = gpui_kit::SharedString::from(format!("nav-quick-struct-{}", ids.long));
            cx.update_window(window, |_, window, cx| {
                window.render_frame(cx);
                let sidebar = window.find("sidebar").bounds();
                let row = window.find(row_id.clone()).bounds();
                let name = window.find(format!("nav-name-{}", ids.long)).bounds();
                assert!(f32::from(row.right()) <= f32::from(sidebar.right()) + 0.5, "{tag}: 对象行不横向溢出侧栏");
                assert!(f32::from(row.right()) <= width + 0.5, "{tag}: 对象行不出窗口");
                // 行尾没有状态时，名称区一直延伸到行内边距：快捷按钮不常驻占宽。
                assert!(
                    f32::from(name.right()) >= f32::from(row.right()) - 4.5,
                    "{tag}: 长表名应使用可用行宽，名称区 {:?} 行 {:?}",
                    name,
                    row
                );
                assert!(!window.find(data_id.clone()).visible(), "{tag}: 未悬停时不显示打开数据");
                assert!(!window.find(struct_id.clone()).visible(), "{tag}: 未悬停时不显示查看结构");

                window.hover(row_id.clone(), cx);
                let data = window.find(data_id.clone());
                let structure = window.find(struct_id.clone());
                assert!(data.visible() && structure.visible(), "{tag}: 悬停后两个快捷动作可见");
                let (data, structure) = (data.bounds(), structure.bounds());
                for bounds in [data, structure] {
                    assert!(
                        f32::from(bounds.left()) >= f32::from(name.left()) - 0.5 && f32::from(bounds.right()) <= f32::from(name.right()) + 0.5,
                        "{tag}: 快捷动作留在名称区内 {bounds:?} / {name:?}"
                    );
                    assert!(f32::from(bounds.right()) <= f32::from(sidebar.right()) + 0.5, "{tag}: 快捷动作不出侧栏");
                }
                assert!(f32::from(data.right()) <= f32::from(structure.left()) + 0.5, "{tag}: 数据在前、结构在后");
                // 悬停另一行时，上一行的快捷动作收起，只有当前行的出现。
                window.hover(format!("nav-{}", ids.orders), cx);
                assert!(!window.find(data_id.clone()).visible(), "{tag}: 离开后长表名行的动作收起");
                assert!(window.find(format!("nav-quick-data-{}", ids.orders)).visible(), "{tag}: 当前悬停行的动作出现");
                // 视图同样有打开数据/结构；读取失败的对象只给重试，不给快捷打开。
                window.hover(format!("nav-{}", ids.recent), cx);
                assert!(window.find(format!("nav-quick-struct-{}", ids.recent)).visible(), "{tag}: 视图也有快捷动作");
                assert!(window.try_find(format!("nav-quick-data-{}", ids.audit)).is_none(), "{tag}: 失败对象没有快捷打开");

                window.hover(row_id.clone(), cx);
                window.click(data_id.clone(), cx);
            })
            .unwrap();
            cx.update(|cx| {
                let view = view.read(cx);
                let tab = view.model.active_tab().expect("打开数据工作项");
                assert_eq!(tab.kind, WorkItemKind::Table, "{tag}: 快捷数据打开表格页");
                let spec = tab.spec.clone().unwrap_or_default();
                assert!(spec.contains(&format!("|{}|", ids.long_name)), "{tag}: 打开的是被悬停的长表名：{spec}");
                assert!(!view.model.nav.is_expanded(&ids.long), "{tag}: 点快捷动作不展开这一行");
                assert_eq!(view.model.executions, 0, "{tag}: 打开数据不执行查询语句");
            });
            ensure_sidebar_open(cx, &view);
            cx.update_window(window, |_, window, cx| {
                window.render_frame(cx);
                let orders_row = gpui_kit::SharedString::from(format!("nav-{}", ids.orders));
                window.hover(orders_row, cx);
                window.click(format!("nav-quick-struct-{}", ids.orders), cx);
            })
            .unwrap();
            cx.update(|cx| {
                let view = view.read(cx);
                let tab = view.model.active_tab().expect("结构工作项");
                assert_eq!(tab.kind, WorkItemKind::Structure, "{tag}: 快捷结构打开结构页");
                let spec = tab.spec.clone().unwrap_or_default();
                assert!(spec.contains("|orders|"), "{tag}: 结构属于被悬停的 orders：{spec}");
                assert!(!view.model.nav.is_expanded(&ids.orders), "{tag}: 点快捷结构不展开 orders");
                let opened: Vec<_> = view
                    .model
                    .tabs
                    .iter()
                    .filter(|tab| matches!(tab.kind, WorkItemKind::Table | WorkItemKind::Structure))
                    .filter_map(|tab| tab.spec.clone())
                    .collect();
                assert_eq!(opened.len(), 2, "{tag}: 两次点击只开两个对应工作项：{opened:?}");
                assert!(!opened.iter().any(|spec| spec.contains("|recent|") || spec.contains("|waybill_audit|")), "{tag}: 没有误开其他对象");
            });
            let _ = cx.update_window(window, |_, window, _| window.remove_window());
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// 共存边界：加载中的对象行，悬停动作只盖住名称末尾、不压住加载指示；搜索时长名称末尾的
/// 命中段完整可见，容器的命中数在名称之外且不出侧栏；读取失败的对象只给错误与重试。
/// 右键命中数落在这一行的菜单上；清除搜索后回到搜索前的展开现场。覆盖 1360/900/640 × 浅/深色。
#[gpui_kit::test]
fn object_tree_search_hits_loading_errors_and_long_names_coexist_with_hover_actions(cx: &mut gpui_kit::TestAppContext) {
    use crate::catalog::hierarchy::ColumnInfo;
    for width in [1360f32, 900., 640.] {
        for dark in [false, true] {
            let (window, view, dir, ids) = object_tree_workbench(cx, width, dark);
            let tag = format!("{width}/{}", if dark { "dark" } else { "light" });
            let orders = ids.orders.clone();
            let schema = ids.expanded[2].clone();
            let before_expanded: Vec<bool> = cx.update(|cx| {
                view.update(cx, |view, cx| {
                    // orders 已读到列，正在重读索引：行尾有加载指示，悬停动作仍可用。
                    let column = |name: &str| ColumnInfo { name: name.to_owned(), data_type: "text".to_owned(), nullable: true, primary: false };
                    assert!(view.model.nav.set_details(&orders, Ok(vec![column("id"), column("note")]), Ok(Vec::new())));
                    assert!(view.model.nav.begin_details(&orders), "orders 进入加载中");
                    cx.notify();
                    ids.expanded.iter().chain([&ids.orders, &ids.long]).map(|id| view.model.nav.is_expanded(id)).collect()
                })
            });
            cx.update_window(window, |_, window, cx| {
                window.render_frame(cx);
                let sidebar = window.find("sidebar").bounds();
                let row_id = gpui_kit::SharedString::from(format!("nav-{}", ids.orders));
                let row = window.find(row_id.clone()).bounds();
                let name = window.find(format!("nav-name-{}", ids.orders)).bounds();
                assert!(f32::from(row.right()) <= f32::from(sidebar.right()) + 0.5, "{tag}: 加载中的行不溢出");
                assert!(f32::from(name.right()) < f32::from(row.right()) - 10., "{tag}: 加载指示在名称区之外占位 {name:?} / {row:?}");
                window.hover(row_id, cx);
                let quick = window.find(format!("nav-quick-{}", ids.orders));
                assert!(quick.visible(), "{tag}: 加载中仍可悬停打开");
                assert!(f32::from(quick.bounds().right()) <= f32::from(name.right()) + 0.5, "{tag}: 悬停动作不压住加载指示");

                // 读取失败的对象：错误文字和重试都在侧栏内，错误不压住重试，没有快捷打开。
                let error = window.find(format!("nav-err-{}", ids.audit));
                assert!(error.visible(), "{tag}: 节点错误可见");
                let retry = window.find(format!("nav-retry-{}", ids.audit));
                assert!(retry.visible() && f32::from(retry.bounds().right()) <= f32::from(sidebar.right()) + 0.5, "{tag}: 重试在侧栏内");
                assert!(f32::from(error.bounds().right()) <= f32::from(retry.bounds().left()) + 0.5, "{tag}: 错误文字不压住重试");
                window.hover(format!("nav-{}", ids.audit), cx);
                assert!(window.try_find(format!("nav-quick-data-{}", ids.audit)).is_none(), "{tag}: 失败对象没有快捷打开");

                window.click("field-ObjectFilter", cx);
                window.input("waybill", cx);
                window.render_frame(cx);
            })
            .unwrap();
            assert_eq!(cx.update(|cx| view.read(cx).model.nav.query().to_owned()), "waybill", "{tag}: 筛选框输入进入树搜索");
            cx.update_window(window, |_, window, cx| {
                window.render_frame(cx);
                let sidebar = window.find("sidebar").bounds();
                // 长名称：命中段在名称末尾，前缀省略，命中段完整留在名称区内。
                let long_name = window.find(format!("nav-name-{}", ids.long)).bounds();
                let hit = window.find(format!("nav-match-{}", ids.long));
                assert!(hit.visible(), "{tag}: 长名称末尾的命中段可见");
                let hit = hit.bounds();
                assert!(
                    f32::from(hit.left()) >= f32::from(long_name.left()) - 0.5 && f32::from(hit.right()) <= f32::from(long_name.right()) + 0.5,
                    "{tag}: 命中段不被裁出名称区 {hit:?} / {long_name:?}"
                );
                assert!(f32::from(hit.size.width) > 20., "{tag}: 命中段有完整宽度 {hit:?}");
                // 悬停长名称：快捷动作仍在名称区内，命中行照样可打开。
                window.hover(format!("nav-{}", ids.long), cx);
                assert!(window.find(format!("nav-quick-data-{}", ids.long)).visible(), "{tag}: 命中行可悬停打开");
                // 失败对象在搜索里也保留错误与重试。
                assert!(window.find(format!("nav-err-{}", ids.audit)).visible(), "{tag}: 搜索中节点错误可见");
                let audit_name = window.find(format!("nav-name-{}", ids.audit)).bounds();
                let audit_error = window.find(format!("nav-err-{}", ids.audit)).bounds();
                assert!(
                    f32::from(audit_name.size.width) >= 40. && audit_name.size.width >= audit_error.size.width,
                    "{tag}: 节点错误不把对象名挤没：名称 {audit_name:?} 错误 {audit_error:?}"
                );
                assert!(window.find(format!("nav-match-{}", ids.audit)).visible(), "{tag}: 失败对象的命中段可见");

                // 容器上的命中数：在名称区之外，不出侧栏。
                let hits = window.find(format!("nav-hits-{schema}"));
                assert!(hits.visible(), "{tag}: 模式行标出命中数");
                let hits = hits.bounds();
                let name = window.find(format!("nav-name-{schema}")).bounds();
                assert!(f32::from(name.right()) <= f32::from(hits.left()) + 0.5, "{tag}: 命中数不压在名称上");
                assert!(f32::from(hits.right()) <= f32::from(sidebar.right()) + 0.5, "{tag}: 命中数不出侧栏");

                // 在命中数上右键：菜单属于这一行，并且画在视口里。
                window.right_click(format!("nav-hits-{schema}"), cx);
                window.render_frame(cx);
                let item = window.find("ctx-tree-copy-name");
                assert!(item.visible(), "{tag}: 右键菜单项可见");
                assert!(f32::from(item.bounds().right()) <= width + 0.5, "{tag}: 菜单不出窗口");
            })
            .unwrap();
            cx.update(|cx| {
                view.update(cx, |view, cx| {
                    assert_eq!(view.model.nav_menu_node.as_deref(), Some(schema.as_str()), "{tag}: 菜单只属于被右键的模式行");
                    assert!(view.model.menu.is_some(), "{tag}: 菜单已打开");
                    view.model.menu = None;
                    cx.notify();
                })
            });
            // 清除搜索（选中全部再删除）：回到搜索前的展开现场。
            cx.update_window(window, |_, window, cx| {
                window.render_frame(cx);
                window.click("field-ObjectFilter", cx);
                window.press("secondary-a", cx);
                window.press("backspace", cx);
                window.render_frame(cx);
            })
            .unwrap();
            cx.update(|cx| {
                let view = view.read(cx);
                assert!(view.model.nav.query().is_empty(), "{tag}: 清空筛选框结束树搜索");
                let after: Vec<bool> = ids.expanded.iter().chain([&ids.orders, &ids.long]).map(|id| view.model.nav.is_expanded(id)).collect();
                assert_eq!(after, before_expanded, "{tag}: 清除搜索后恢复原展开现场");
            });
            cx.update_window(window, |_, window, cx| {
                window.render_frame(cx);
                assert!(window.try_find(format!("nav-match-{}", ids.long)).is_none(), "{tag}: 清除后不残留高亮");
                assert!(window.try_find(format!("nav-hits-{schema}")).is_none(), "{tag}: 清除后不残留命中数");
                assert!(window.find(format!("nav-{}", ids.recent)).visible(), "{tag}: 未命中的视图重新出现");
                assert!(window.find(format!("nav-{}", ids.orders)).visible(), "{tag}: 未命中的 orders 重新出现");
            })
            .unwrap();
            let _ = cx.update_window(window, |_, window, _| window.remove_window());
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// 对象树只有一个 Tab 停靠点：点行把焦点交给树容器，悬停图标不是焦点目标；
/// 键盘上下移动后回车打开数据，命令面板的“结构”打开同一个对象的结构页。
#[gpui_kit::test]
fn object_tree_keeps_one_tab_stop_and_keyboard_opens_data_and_structure(cx: &mut gpui_kit::TestAppContext) {
    let (window, view, dir, ids) = object_tree_workbench(cx, 1360., false);
    cx.update(crate::app::bind_keys);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click(format!("nav-{}", ids.recent), cx);
        assert_eq!(window.find("explorer-list").focused(), Some(true), "点行后焦点在树容器上");
        window.hover(format!("nav-{}", ids.orders), cx);
        let quick = window.find(format!("nav-quick-data-{}", ids.orders));
        assert!(quick.visible(), "悬停图标出现");
        assert_eq!(quick.focused(), None, "悬停图标没有焦点绑定，不是 Tab 停靠点");
        assert_eq!(window.find(format!("nav-quick-struct-{}", ids.orders)).focused(), None);
    })
    .unwrap();
    // 键盘上移直到落在 orders 上（树行顺序由目录决定，测试不假定具体步数）。
    for _ in 0..8 {
        let focus = cx.update(|cx| view.read(cx).model.nav.focus().map(str::to_owned));
        if focus.as_deref() == Some(ids.orders.as_str()) {
            break;
        }
        cx.update_window(window, |_, window, cx| window.press("up", cx)).unwrap();
    }
    assert_eq!(cx.update(|cx| view.read(cx).model.nav.focus().map(str::to_owned)), Some(ids.orders.clone()), "方向键在树内移动到 orders");
    cx.update_window(window, |_, window, cx| {
        assert_eq!(window.find("explorer-list").focused(), Some(true), "方向键期间焦点仍在树容器");
        window.press("enter", cx);
    })
    .unwrap();
    cx.update(|cx| {
        let view = view.read(cx);
        let tab = view.model.active_tab().expect("回车打开数据");
        assert_eq!(tab.kind, WorkItemKind::Table);
        assert!(tab.spec.clone().unwrap_or_default().contains("|orders|"), "回车打开键盘选中的 orders");
    });
    // 键盘打开结构：命令面板里的结构命令使用树上选中的对象。
    let label = cx.update(|cx| view.read(cx).model.text("shell.command.newStructure"));
    cx.update_window(window, |_, window, cx| {
        window.press("secondary-k", cx);
        window.input(&label, cx);
        window.render_frame(cx);
    })
    .unwrap();
    let first = cx.update(|cx| view.read(cx).model.palette_entries().first().map(|entry| entry.id.clone()));
    assert_eq!(first.as_deref(), Some("open-structure"), "命令面板首项就是结构命令");
    cx.update_window(window, |_, window, cx| window.press("enter", cx)).unwrap();
    cx.update(|cx| {
        let view = view.read(cx);
        let tab = view.model.active_tab().expect("结构工作项");
        assert_eq!(tab.kind, WorkItemKind::Structure, "键盘路径打开结构页");
        assert!(tab.spec.clone().unwrap_or_default().contains("|orders|"), "结构属于树上选中的 orders");
    });
    let _ = cx.update_window(window, |_, window, _| window.remove_window());
    let _ = std::fs::remove_dir_all(dir);
}

#[gpui_kit::test]
fn connections_page_renders_failure_callout_and_supports_group_drag_and_modal_group_picker(cx: &mut gpui_kit::TestAppContext) {
    let dir = std::env::temp_dir().join(format!("rillsql-conn-ux-{}", uuid::Uuid::new_v4()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench_sized(cx, services_at(&dir), false, 1280., 800.);
    cx.update(|cx| {
        view.update(cx, |view, _| {
            let mut r1 = record_for("uat", "qsyn-uat", Engine::Mysql);
            r1.saved.host = Some("192.168.92.210".into());
            r1.saved.port = Some(3306);
            r1.saved.username = Some("qsyn3_user".into());
            r1.saved.env = "test".into();
            let r2 = record_for("logs", "logs_2", Engine::Sqlite);
            view.model.connections.rows = vec![r1, r2];
            view.model.connections.selected = Some("uat".into());
            view.model.connections.status.insert("uat".into(), LinkStatus::Failed);
            view.model.connections.failures.insert("uat".into(), "Input/output error: invalid peer certificate: UnknownIssuer".into());
            view.model.apply_groups(vec![group("g_qs", "青山", 0, false)], Default::default());
            view.model.shell.page = AppPage::Connections;
        });
    });

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("conn-detail-failure").visible(), "右侧详情栏直接展示连接失败原因");
        assert!(window.find("launcher-group-empty-g_qs").visible(), "空分组展示拖放区与快捷新建入口");

        // 在空分组上点「在此分组新建连接」，模态打开且默认选中该分组，控件登记与 Tab 停靠点完全一致。
        window.click("launcher-group-empty-add-g_qs", cx);
        window.render_frame(cx);
        {
            let v = view.read(cx);
            assert!(v.model.conn_modal.open);
            assert_eq!(v.model.conn_modal.draft_group.as_deref(), Some("g_qs"));
            let expected: Vec<String> = v.model.modal_stops().iter().filter_map(|stop| stop.control_id().map(str::to_owned)).collect();
            assert_eq!(v.model.conn_modal.rendered.len(), expected.len(), "{:?} vs {:?}", v.model.conn_modal.rendered, expected);
            assert!(v.model.conn_modal.rendered.iter().any(|id| id == "conn-group-opt-g_qs"));
            assert!(v.model.conn_modal.rendered.iter().any(|id| id == "conn-env-none"));
        }
        window.click("conn-cancel", cx);
        window.render_frame(cx);

        // 将未分组连接卡片拖入空分组区，触发归组任务。
        window.drag_to("conn-row-uat", "launcher-group-empty-g_qs", cx);
    })
    .unwrap();
    let op = cx.update(|cx| view.update(cx, |view, _| view.model.take_pending_group_op()));
    assert_eq!(
        op,
        Some(crate::app::shell::model::GroupOp::Assign {
            connection: "uat".into(),
            group: Some("g_qs".into()),
        })
    );
    let _ = cx.update_window(window, |_, window, _| window.remove_window());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn table_rename_and_drop_sync_open_tabs_and_invalidate_tree_details() {
    use crate::catalog::hierarchy::{column_group_id, connection_node_id, database_node_id, group_node_id, schema_node_id, ColumnInfo};
    use crate::catalog::model::{ObjectKind, ObjectRow};

    let mut model = seeded_model(vec![record_for("pg", "Atlas", Engine::Postgres)]);
    model.connections.status.insert("pg".into(), LinkStatus::Connected);
    let orders_obj = ObjectRow::table("orders", Some(10));
    let nav = nav_connection(Engine::Postgres, "pg", "Atlas", vec![nav_database("shop", vec![orders_obj.clone()])]);
    model.apply_navigation(&[nav], false);
    for id in [
        connection_node_id("pg"),
        database_node_id("pg", "shop"),
        schema_node_id("pg", "shop", "public"),
        group_node_id("pg", "shop", "public", ObjectKind::Table),
    ] {
        model.nav.expand(&id);
    }
    let table_node = crate::catalog::hierarchy::object_node_id("pg", "shop", "public", &orders_obj);
    let cols_group = column_group_id(&table_node);
    model.nav.expand(&table_node);
    assert!(model.nav.set_details(
        &table_node,
        Ok(vec![ColumnInfo {
            name: "old_col".into(),
            data_type: "int4".into(),
            nullable: false,
            primary: true,
        }]),
        Ok(Vec::new()),
    ));
    model.nav.expand(&cols_group);
    assert!(model.nav.visible().iter().any(|r| r.label == "old_col"));

    let spec = "pg|Atlas|shop|public|orders|postgres|dev";
    let _ = model.dispatch(&format!("open-table:{spec}"));
    let _ = model.dispatch(&format!("open-structure-for:{spec}"));

    // 表结构变更后清理旧字段缓存并排队重新加载详情。
    model.table_structure_changed("pg", "shop", "public", "orders", false);
    assert!(!model.nav.visible().iter().any(|r| r.label == "old_col"));

    // 表重命名后已打开的数据与结构标签页同步跟随新表名。
    model.table_renamed("pg", "shop", "public", "orders", "orders_v2");
    for tab in &model.tabs {
        if matches!(tab.kind, WorkItemKind::Table | WorkItemKind::Structure) {
            assert_eq!(tab.title, "orders_v2");
            assert!(tab.spec.as_deref().unwrap_or_default().contains("|orders_v2|"));
        }
    }

    // 删除表后自动收起已失效的干净数据与结构标签页。
    model.object_dropped("pg", "shop", "public", "orders_v2", ObjectKind::Table);
    assert!(model.tabs.iter().all(|tab| !matches!(tab.kind, WorkItemKind::Table | WorkItemKind::Structure)));
}

// ---------------------------------------------------------------- R1 对象树搜索清除

/// 已展开到对象的 PG 连接；搜索“ord”时服务端另外命中一张未分页的表和一个没读过的库。
fn search_clear_model() -> (WorkbenchModel, usize, Vec<super::model::SearchBatch>, Vec<String>) {
    use crate::catalog::hierarchy::{connection_node_id, database_node_id, group_node_id, schema_node_id};
    use crate::catalog::model::{ObjectKind, ObjectRow};
    let orders = ObjectRow::table("orders", Some(1));
    let people = ObjectRow::table("people", Some(1));
    let pg = nav_connection(Engine::Postgres, "pg", "销售库", vec![nav_database("shop", vec![orders, people])]);
    let mut model = WorkbenchModel::new(Shell::default());
    model.connections.rows = vec![record_for("pg", "销售库", Engine::Postgres)];
    model.connections.status.insert("pg".to_owned(), LinkStatus::Connected);
    model.apply_navigation(&[pg], false);
    expand_all(
        &mut model,
        &[
            connection_node_id("pg"),
            database_node_id("pg", "shop"),
            schema_node_id("pg", "shop", "public"),
            group_node_id("pg", "shop", "public", ObjectKind::Table),
        ],
    );
    let before = model.nav_rows().len();
    let archive = ObjectRow::table("order_archive", Some(9));
    let cold = ObjectRow::table("order_cold", Some(3));
    let provisional = vec![
        nav_id("pg", "shop", "public", &archive),
        database_node_id("pg", "cold"),
        nav_id("pg", "cold", "public", &cold),
    ];
    let hits = vec![(
        "pg".to_owned(),
        "销售库".to_owned(),
        Engine::Postgres,
        vec![("shop".to_owned(), "public".to_owned(), archive), ("cold".to_owned(), "public".to_owned(), cold)],
    )];
    (model, before, hits, provisional)
}

fn queued_searches(model: &WorkbenchModel) -> (usize, usize) {
    let search = model.nav_requests.iter().filter(|req| matches!(req, super::nav::NavRequest::Search(_))).count();
    let cancel = model.nav_requests.iter().filter(|req| matches!(req, super::nav::NavRequest::CancelSearch)).count();
    (search, cancel)
}

#[test]
fn explorer_clear_filter_command_ends_search_drops_queued_reads_and_removes_merged_nodes() {
    let (mut model, before, hits, provisional) = search_clear_model();
    let expanded_before: Vec<bool> = model.nav_rows().iter().map(|row| row.expanded).collect();
    model.set_field(DraftField::ObjectFilter, "ord".into());
    assert_eq!(queued_searches(&model), (1, 0), "输入后排队一次服务端检索");
    let generation = model.nav_search_generation;
    assert!(model.apply_search_hits(generation, "ord", hits));
    for id in &provisional {
        assert!(model.nav_node(id).is_some(), "{id}: 搜索命中合入树");
    }
    assert!(model.nav_rows().iter().any(|row| row.label == "order_archive"));

    // 无命中行与框内按钮共用的命令。
    let _ = model.dispatch("explorer-clear-filter");
    assert!(model.object_filter.is_empty());
    assert_eq!(model.field_value(DraftField::ObjectFilter), "");
    assert!(model.nav.query().is_empty(), "清除后 nav.query 为空");
    assert!(!model.nav_searching());
    assert_eq!(queued_searches(&model), (0, 1), "排队的检索被丢弃，并通知视图中止进行中的检索");
    for id in &provisional {
        assert!(model.nav_node(id).is_none(), "{id}: 仅由搜索合入的节点被撤除");
    }
    assert_eq!(model.nav.search_provisional_count(), 0);
    assert_eq!(model.nav_rows().len(), before, "树回到搜索前");
    let expanded_after: Vec<bool> = model.nav_rows().iter().map(|row| row.expanded).collect();
    assert_eq!(expanded_after, expanded_before, "恢复搜索前展开");
    assert!(model.nav_rows().iter().all(|row| row.highlight.is_none()), "不残留高亮");
}

#[test]
fn late_search_results_after_clearing_are_discarded() {
    let (mut model, before, hits, provisional) = search_clear_model();
    model.set_field(DraftField::ObjectFilter, "ord".into());
    let stale = model.nav_search_generation;
    // 全删：输入框以空值送达，同样走清除入口。
    model.set_field(DraftField::ObjectFilter, String::new());
    assert!(!model.apply_search_hits(stale, "ord", hits.clone()), "清除后迟到的结果被丢弃");
    assert_eq!(model.nav_rows().len(), before);
    assert!(provisional.iter().all(|id| model.nav_node(id).is_none()));

    // 重新输入同一个词：上一轮的迟到结果代数不符，仍然丢弃；本轮结果正常合入。
    model.set_field(DraftField::ObjectFilter, "ord".into());
    assert!(!model.apply_search_hits(stale, "ord", hits.clone()), "旧代数的结果不合入新一轮搜索");
    assert!(provisional.iter().all(|id| model.nav_node(id).is_none()));
    let current = model.nav_search_generation;
    assert!(model.apply_search_hits(current, "ord", hits));
    assert!(provisional.iter().all(|id| model.nav_node(id).is_some()));
}

#[test]
fn nodes_loaded_normally_during_search_survive_clearing() {
    use crate::catalog::hierarchy::database_node_id;
    let (mut model, _, hits, _) = search_clear_model();
    model.set_field(DraftField::ObjectFilter, "ord".into());
    let generation = model.nav_search_generation;
    assert!(model.apply_search_hits(generation, "ord", hits));
    // 用户在搜索期间真的读了“cold”库：库节点不再是临时节点。
    let cold = database_node_id("pg", "cold");
    let _ = model.nav.set_database(&cold, Ok(Vec::new()));
    let _ = model.dispatch("explorer-clear-filter");
    assert!(model.nav_node(&cold).is_some(), "正常读取过的库在清除后保留");
}

#[test]
fn twenty_type_and_clear_cycles_always_restore_the_tree() {
    let (mut model, before, hits, provisional) = search_clear_model();
    for cycle in 0..20 {
        while model.take_nav_request().is_some() {}
        let word = if cycle % 2 == 0 { "ord" } else { "order" };
        model.set_field(DraftField::ObjectFilter, "o".into());
        model.set_field(DraftField::ObjectFilter, word.into());
        assert_eq!(queued_searches(&model), (1, 0), "{cycle}: 只保留最后一次检索");
        let generation = model.nav_search_generation;
        assert!(model.apply_search_hits(generation, word, hits.clone()), "{cycle}");
        match cycle % 3 {
            0 => model.set_field(DraftField::ObjectFilter, String::new()),
            1 => {
                let _ = model.dispatch("explorer-clear-filter");
            }
            _ => {
                assert!(model.clear_object_filter());
            }
        }
        assert!(model.nav.query().is_empty(), "{cycle}: query 为空");
        assert!(model.object_filter.is_empty(), "{cycle}: 筛选词为空");
        assert_eq!(queued_searches(&model), (0, 1), "{cycle}: 无排队检索");
        assert_eq!(model.nav.search_provisional_count(), 0, "{cycle}");
        assert!(provisional.iter().all(|id| model.nav_node(id).is_none()), "{cycle}: 合入节点撤除");
        assert_eq!(model.nav_rows().len(), before, "{cycle}: 树恢复");
        assert!(!model.apply_search_hits(generation, word, hits.clone()), "{cycle}: 迟到结果丢弃");
    }
}

/// 真实输入框：Esc 与无命中行的清除按钮都走同一个清除入口，框内文字随之清空。
#[gpui_kit::test]
fn escape_and_no_match_button_clear_the_object_filter_in_the_window(cx: &mut gpui_kit::TestAppContext) {
    let (window, view, dir, ids) = object_tree_workbench(cx, 1360., false);
    let before: Vec<bool> = ids.expanded.iter().map(|id| cx.update(|cx| view.read(cx).model.nav.is_expanded(id))).collect();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("field-ObjectFilter", cx);
        window.input("waybill", cx);
        window.render_frame(cx);
    })
    .unwrap();
    assert_eq!(cx.update(|cx| view.read(cx).model.nav.query().to_owned()), "waybill");
    cx.update_window(window, |_, window, cx| {
        window.press("escape", cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        let view = view.read(cx);
        assert!(view.model.object_filter.is_empty(), "Esc 清空筛选词");
        assert!(view.model.nav.query().is_empty(), "Esc 结束树搜索");
        let after: Vec<bool> = ids.expanded.iter().map(|id| view.model.nav.is_expanded(id)).collect();
        assert_eq!(after, before, "Esc 后恢复展开");
    });

    // 无命中：行内按钮清除，框内文字同步清空（再输入时从空框开始）。
    cx.update_window(window, |_, window, cx| {
        window.click("field-ObjectFilter", cx);
        window.input("zzzz", cx);
        window.render_frame(cx);
    })
    .unwrap();
    assert_eq!(cx.update(|cx| view.read(cx).model.nav.query().to_owned()), "zzzz");
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("explorer-clear-filter", cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        let view = view.read(cx);
        assert!(view.model.object_filter.is_empty(), "按钮清空筛选词");
        assert!(view.model.nav.query().is_empty(), "按钮结束树搜索");
    });
    cx.update_window(window, |_, window, cx| {
        window.click("field-ObjectFilter", cx);
        window.input("ord", cx);
        window.render_frame(cx);
    })
    .unwrap();
    assert_eq!(cx.update(|cx| view.read(cx).model.nav.query().to_owned()), "ord", "框内文字已被清空，不残留 zzzz");
    let _ = std::fs::remove_dir_all(dir);
}

// ---------------------------------------------------------------- R5 打开文件合并

/// 顶栏、欢迎页、连接页、命令表与菜单命令里只剩一个“打开文件”入口。
#[gpui_kit::test]
fn every_surface_offers_exactly_one_open_file_entry(cx: &mut gpui_kit::TestAppContext) {
    let dir = std::env::temp_dir().join(format!("rillsql-open-entry-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench(cx, services_at(&dir), false);
    let ids: Vec<&str> = crate::app::COMMANDS.iter().map(|command| command.id).collect();
    assert!(ids.contains(&"open-file"));
    assert!(!ids.contains(&"open-sqlite") && !ids.contains(&"open-sql-file"), "命令表不再有两个打开入口");
    assert_eq!(crate::app::Shell::command("open-file").and_then(|command| command.chord()), Some("Mod+O"));
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("top-open-file").visible(), "顶栏有打开文件");
        for gone in ["top-sqlite", "top-open-sql", "welcome-card-sqlite", "welcome-card-open-sql"] {
            assert!(window.try_find(gone).is_none(), "{gone} 已合并");
        }
        assert!(window.find("welcome-card-open-file").visible(), "欢迎页只有一张打开文件卡片");
        view.update(cx, |view, cx| view.perform("activity-connections", cx));
        window.render_frame(cx);
        assert!(window.find("launcher-open-file").visible(), "连接页有打开文件");
        for gone in ["launcher-sqlite", "launcher-empty-sqlite"] {
            assert!(window.try_find(gone).is_none(), "{gone} 已合并");
        }
    })
    .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// 打开文件：文件头认出的数据库直接建连接；认不出的询问打开方式，Esc 跳过、选 SQL 进编辑器、选数据库建连接。
#[gpui_kit::test]
fn open_file_routes_by_header_and_asks_for_unknown_files(cx: &mut gpui_kit::TestAppContext) {
    let dir = std::env::temp_dir().join(format!("rillsql-open-file-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench(cx, services_at(&dir), false);
    let pick = |cx: &mut gpui_kit::TestAppContext, path: std::path::PathBuf| {
        cx.update_window(window, |_, _, cx| view.update(cx, |view, cx| view.perform("open-file", cx))).unwrap();
        assert!(cx.did_prompt_for_paths());
        cx.simulate_path_prompt_response(move |options| {
            assert!(options.files && !options.directories && options.multiple);
            Some(vec![path.clone()])
        });
        cx.run_until_parked();
    };

    // 扩展名不对，但文件头是数据库：直接建本地文件连接，不询问。
    let mut header = b"SQLite format 3\0".to_vec();
    header.extend_from_slice(&[0x10, 0x00, 0x01, 0x01]);
    let backup = dir.join("backup.bin");
    std::fs::write(&backup, &header).unwrap();
    pick(cx, backup.canonicalize().unwrap());
    cx.update(|cx| {
        let model = &view.read(cx).model;
        assert!(model.open_choices.is_empty(), "认得出的文件不询问");
        let draft = model.connections.editor.as_ref().expect("数据库文件建连接");
        assert_eq!(draft.engine, crate::adapters::registry::LOCAL_FILE);
        assert_eq!(draft.file_path, backup.canonicalize().unwrap().display().to_string());
    });

    // 认不出的文件：弹出询问；Esc 跳过，不打开任何东西。
    let notes = dir.join("notes.txt");
    std::fs::write(&notes, "select 42;\n").unwrap();
    let notes = notes.canonicalize().unwrap();
    pick(cx, notes.clone());
    let tabs = cx.update(|cx| view.read(cx).model.tabs.len());
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("cmd-open-as-sql").visible() && window.find("cmd-open-as-database").visible(), "询问打开方式");
        window.press("escape", cx);
        window.render_frame(cx);
        assert!(window.try_find("cmd-open-as-sql").is_none(), "Esc 关闭询问");
    })
    .unwrap();
    cx.update(|cx| {
        let model = &view.read(cx).model;
        assert!(model.open_choices.is_empty());
        assert_eq!(model.tabs.len(), tabs, "跳过的文件不打开");
    });

    // 选“作为 SQL 文本打开”：内容进查询编辑器。
    pick(cx, notes.clone());
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("cmd-open-as-sql", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        let model = &view.read(cx).model;
        assert!(model.open_choices.is_empty());
        assert!(model.tabs.iter().any(|tab| tab.title.contains("notes")), "文本进入查询编辑器：{:?}", model.tabs.iter().map(|tab| tab.title.clone()).collect::<Vec<_>>());
    });

    // 选“作为数据库打开”：按本地文件数据库建连接。
    pick(cx, notes.clone());
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("cmd-open-as-database", cx);
    })
    .unwrap();
    cx.update(|cx| {
        let draft = view.read(cx).model.connections.editor.as_ref().cloned().expect("按数据库打开");
        assert_eq!(draft.file_path, notes.display().to_string());
    });

    // 拖放走同一套规则：认不出、又不是导入格式的文件同样询问打开方式，不进导入预览。
    let bare = dir.join("no-extension");
    std::fs::write(&bare, "select 7;\n").unwrap();
    cx.update(|cx| {
        view.update(cx, |view, cx| view.open_dropped_or_picked_paths(&[notes.clone(), bare.clone()], true, cx));
        let model = &view.read(cx).model;
        assert_eq!(model.open_choices.len(), 2, "拖入的未知文件进入询问队列");
        assert!(!model.exchange_open, "未知文件不打开导入预览");
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("cmd-open-as-sql").visible() && window.find("cmd-open-as-database").visible(), "拖放也询问打开方式");
        window.press("escape", cx);
        window.render_frame(cx);
        window.press("escape", cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| assert!(view.read(cx).model.open_choices.is_empty()));
    // 表格文件仍走导入预览，不询问。
    let csv = dir.join("rows.csv");
    std::fs::write(&csv, "name\n甲\n").unwrap();
    cx.update(|cx| {
        view.update(cx, |view, cx| view.open_dropped_or_picked_paths(std::slice::from_ref(&csv), true, cx));
        let model = &view.read(cx).model;
        assert!(model.open_choices.is_empty(), "CSV 不询问");
        assert!(model.exchange_open, "CSV 打开导入预览");
    });
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------- R6 快捷键注册表与面板

#[test]
fn shortcut_registry_drives_commands_menus_and_the_help_panel() {
    // 命令表的组合键全部来自注册表；注册表里有命令的项都能被外壳执行。
    for command in crate::app::COMMANDS {
        assert_eq!(command.chord(), crate::app::keymap::command_chord(command.id), "{}", command.id);
    }
    let mut model = WorkbenchModel::new(Shell::default());
    for shortcut in crate::app::keymap::SHORTCUTS {
        if let Some(command) = shortcut.command {
            assert!(model.dispatch(command).is_some(), "{} 的命令 {command} 没有处理者", shortcut.id);
            model.shell.close_overlay();
            model.close_prompt = None;
        }
    }
    // 每一条按键绑定都出现在面板数据里：面板列出全部条目。
    model.help_query.clear();
    model.help_group = None;
    let listed: Vec<&str> = model.help_sections().iter().flat_map(|(_, items)| items.iter().map(|item| item.id)).collect();
    for shortcut in crate::app::keymap::SHORTCUTS {
        assert!(listed.contains(&shortcut.id), "{} 没有出现在快捷键面板里", shortcut.id);
    }
    assert_eq!(listed.len(), crate::app::keymap::SHORTCUTS.len());
    assert!(listed.contains(&"open-file"), "⌘O 打开文件登记在面板里");
}

#[test]
fn help_panel_searches_names_and_keys_and_preselects_the_focused_group() {
    let mut model = WorkbenchModel::new(Shell::default());
    model.shell.focus = crate::app::FocusTarget::Editor;
    let _ = model.dispatch("help");
    assert!(model.shell.help_open);
    assert_eq!(model.help_group, Some("editor"), "编辑器里打开面板预选编辑器分组");
    let sections = model.help_sections();
    assert_eq!(sections.len(), 1);
    assert_eq!(sections[0].0, "editor");
    model.shell.close_overlay();

    model.shell.focus = crate::app::FocusTarget::Grid;
    let _ = model.dispatch("help");
    assert_eq!(model.help_group, Some("table"));
    model.shell.close_overlay();
    model.shell.focus = crate::app::FocusTarget::Sidebar;
    let _ = model.dispatch("help");
    assert_eq!(model.help_group, Some("explorer"));
    model.shell.close_overlay();
    // 键空间工作台：焦点不在编辑器/表格/对象树时预选键空间分组。
    let tool = model.tool;
    model.tool = super::model::WorkTool::Redis;
    model.shell.focus = crate::app::FocusTarget::Palette;
    let _ = model.dispatch("help");
    assert_eq!(model.help_group, Some("keyspace"));
    assert!(model.help_sections().iter().all(|(group, items)| *group == "keyspace" && items.len() == 5));
    model.tool = tool;

    // 搜索动作名：跨分组查找，不受预选分组限制。
    model.set_field(DraftField::HelpQuery, "注释".into());
    let ids: Vec<&str> = model.help_sections().iter().flat_map(|(_, items)| items.iter().map(|item| item.id)).collect();
    assert_eq!(ids, ["toggle-comment", "toggle-block-comment"]);
    // 搜索按键：符号或文字都可以。
    for query in ["Mod+/", "ctrl+/", "⌘/"] {
        model.set_field(DraftField::HelpQuery, query.into());
        let ids: Vec<&str> = model.help_sections().iter().flat_map(|(_, items)| items.iter().map(|item| item.id)).collect();
        assert!(ids.contains(&"toggle-comment"), "{query}: {ids:?}");
    }
    model.set_field(DraftField::HelpQuery, "zzzz-no-such".into());
    assert!(model.help_sections().is_empty());
    // 分组导航。
    model.set_field(DraftField::HelpQuery, String::new());
    let _ = model.dispatch("help-group:table");
    assert_eq!(model.help_group, Some("table"));
    assert!(model.help_sections().iter().all(|(group, _)| *group == "table"));
    let _ = model.dispatch("help-group-all");
    assert_eq!(model.help_sections().len(), crate::app::keymap::GROUPS.len());
    assert!(model.dispatch("help-group:nope").is_none(), "未知分组不接受");
}

/// 真实窗口：面板有搜索框、分组导航，搜“注释”只剩一行；列表能滚到最后一项。
#[gpui_kit::test]
fn help_panel_renders_search_groups_and_scrolls_to_the_end(cx: &mut gpui_kit::TestAppContext) {
    let dir = std::env::temp_dir().join(format!("rillsql-help-panel-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench_sized(cx, services_at(&dir), false, 1100., 600.);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        view.update(cx, |view, cx| view.perform("help", cx));
        window.render_frame(cx);
        assert!(window.find("palette").visible(), "面板打开");
        assert!(window.find("field-HelpQuery").visible(), "顶部有搜索框");
        window.render_frame(cx);
        // 打开即可输入：焦点已在搜索框。
        assert!(view.read(cx).fields.is_focused(DraftField::HelpQuery, window, cx), "打开面板后焦点在搜索框");
        window.input("Mod+K", cx);
    })
    .unwrap();
    assert_eq!(cx.update(|cx| view.read(cx).model.help_query.clone()), "Mod+K", "打开面板后直接输入进入搜索框");
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("help-row-command-palette").visible(), "按键也能搜索");
        for group in crate::app::keymap::GROUPS {
            assert!(window.find(format!("help-group-{group}")).visible(), "{group} 分组导航");
        }
        window.click("help-group-all", cx);
        window.render_frame(cx);
        let list = window.find("help-list").bounds();
        let panel = window.find("palette").bounds();
        assert!(f32::from(list.bottom()) <= f32::from(panel.bottom()) + 0.5, "列表在面板内");
        window.scroll(
            "help-list",
            gpui_kit::ScrollDelta::Pixels(gpui_kit::Point::new(gpui_kit::px(0.), gpui_kit::px(-9000.))),
            cx,
        );
        window.render_frame(cx);
        let last = crate::app::keymap::SHORTCUTS.last().unwrap().id;
        let row = window.find(format!("help-row-{last}"));
        assert!(row.visible(), "滚到底后最后一项可见");
        let list = window.find("help-list").bounds();
        assert!(f32::from(row.bounds().bottom()) <= f32::from(list.bottom()) + 0.5, "最后一项完整显示在列表内");

        window.click("field-HelpQuery", cx);
        window.input("注释", cx);
    })
    .unwrap();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.find("help-row-toggle-comment").visible(),
            "搜“注释”命中注释快捷键：{:?}",
            view.read(cx).model.help_query
        );
        assert!(window.try_find("help-row-format").is_none(), "其余项被筛掉");
        // 命中的字在动作名里高亮；按动作名命中时组合键不高亮。
        let hit = window.find("help-match-toggle-comment");
        assert!(hit.visible(), "动作名里的命中段高亮可见");
        assert_eq!(hit.label(), Some("注释"), "高亮的就是搜索词");
        let row = window.find("help-row-toggle-comment").bounds();
        let hit = hit.bounds();
        assert!(f32::from(hit.left()) >= f32::from(row.left()) - 0.5 && f32::from(hit.right()) <= f32::from(row.right()) + 0.5, "高亮在行内");
        assert!(window.try_find("help-key-match-toggle-comment").is_none(), "按名称命中时不高亮按键");
    })
    .unwrap();
    assert_eq!(cx.update(|cx| view.read(cx).model.help_query.clone()), "注释");
    // 按按键搜索：组合键里命中的部分高亮。
    cx.update(|cx| view.update(cx, |view, _| view.model.help_query = "Mod+/".into()));
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let hit = window.find("help-key-match-toggle-comment");
        assert!(hit.visible(), "组合键里的命中段高亮可见");
        let expected = if cfg!(target_os = "macos") { "⌘/" } else { "Ctrl+/" };
        assert_eq!(hit.label(), Some(expected));
        assert!(window.try_find("help-match-toggle-comment").is_none(), "动作名未命中时不高亮名称");
    })
    .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// 注册表生成的绑定真的生效：⌘/Ctrl+O 打开文件对话框，Mod+K 打开命令面板。
#[gpui_kit::test]
fn registry_bindings_resolve_to_their_actions(cx: &mut gpui_kit::TestAppContext) {
    let dir = std::env::temp_dir().join(format!("rillsql-keymap-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let (window, _view) = open_workbench(cx, services_at(&dir), false);
    cx.update(crate::app::bind_keys);
    let mac = cfg!(target_os = "macos");
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let resolve = |key: &'static str, window: &gpui_kit::Window, cx: &gpui_kit::App| {
            let keystroke = gpui_kit::Keystroke::parse(key).unwrap();
            let (bindings, _) = cx.key_bindings().borrow().bindings_for_input(&[keystroke], &window.context_stack());
            bindings.first().map(|binding| binding.action().boxed_clone())
        };
        let open = resolve(if mac { "cmd-o" } else { "ctrl-o" }, window, cx).expect("打开文件已绑定");
        assert!(open.partial_eq(&crate::app::OpenFile), "{}", open.name());
        let palette = resolve(if mac { "cmd-k" } else { "ctrl-k" }, window, cx).expect("命令面板已绑定");
        assert!(palette.partial_eq(&crate::app::CommandPalette), "{}", palette.name());
    })
    .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// R19：常用字段模板随应用状态启动恢复并下发给结构页；结构页保存模板后外壳写盘，再次启动（重启）仍在。
#[gpui_kit::test]
fn column_templates_restore_on_boot_and_persist_after_edit(cx: &mut gpui_kit::TestAppContext) {
    use crate::ddl::templates::{ColumnTemplate, TemplateColumn, TemplateStore};
    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-shell-templates-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let audit = |name: &str| ColumnTemplate {
        id: format!("custom:{name}"),
        name: name.to_owned(),
        engines: Vec::new(),
        columns: vec![TemplateColumn { name: "created_user_id".into(), type_name: "bigint".into(), nullable: true, ..TemplateColumn::default() }],
    };
    let mut seeded = TemplateStore::default();
    seeded.upsert(audit("审计字段")).unwrap();
    let state = AppState::new(ShellSettings::default(), SessionState::default()).with_templates(seeded.clone());
    std::fs::write(services.state.path(), crate::state::to_bytes(&state).unwrap()).unwrap();

    let wait = |cx: &mut gpui_kit::TestAppContext, window: gpui_kit::AnyWindowHandle, done: &dyn Fn(&mut gpui_kit::App) -> bool| {
        let started = std::time::Instant::now();
        while started.elapsed() < std::time::Duration::from_secs(30) {
            cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
            cx.run_until_parked();
            if cx.update(|cx| done(cx)) {
                return true;
            }
            std::thread::park_timeout(std::time::Duration::from_millis(25));
        }
        false
    };

    let (window, view) = open_workbench(cx, Arc::clone(&services), true);
    let restored = wait(cx, window, &|cx| {
        let view = view.read(cx);
        view.model.column_templates == seeded && view.structure.read(cx).templates() == &seeded
    });
    assert!(restored, "启动时恢复模板并下发给结构页");

    // 结构页里新增一个模板：经 sink 交回外壳并写盘。
    let mut edited = seeded.clone();
    edited.upsert(audit("操作人")).unwrap();
    let structure = cx.update(|cx| view.read(cx).structure.clone());
    let edited_for_pane = edited.clone();
    cx.update(|cx| {
        structure.update(cx, |pane, cx| {
            pane.set_templates(edited_for_pane);
            pane.open_template_manager();
            pane.toggle_builtin_hidden("builtin:uuid", cx);
        })
    });
    let mut expected = edited.clone();
    expected.set_hidden("builtin:uuid", true);
    let path = services.state.path().to_path_buf();
    let written = wait(cx, window, &|_| {
        std::fs::read(&path).ok().and_then(|bytes| crate::state::parse_stored(&bytes).ok()).is_some_and(|state| state.column_templates == expected)
    });
    assert!(written, "模板改动写入 app-state.json");
    cx.update_window(window, |_, window, _| window.remove_window()).unwrap();
    cx.update(|_| drop(view));

    // 重启：新的工作台从同一份状态文件读回。
    let (window, view) = open_workbench(cx, Arc::clone(&services), true);
    let restored = wait(cx, window, &|cx| view.read(cx).structure.read(cx).templates() == &expected);
    assert!(restored, "重启后自定义模板仍在");
    cx.update_window(window, |_, window, _| window.remove_window()).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// 资源管理器没有当前行（没有连接、或树焦点不在可见行上）时回车什么也不做；
/// 原先转给 nav_key(Enter) 又回到 explorer_enter，原生应用里按回车直接栈溢出退出。
#[test]
fn enter_without_an_explorer_row_is_a_no_op_not_a_recursion() {
    let mut model = WorkbenchModel::new(Shell::default());
    assert!(model.explorer_current_row().is_none());
    assert_eq!(model.nav_key(NavKey::Enter), None);
    let mut model = grouped_model(vec![record_for("pg", "Atlas", Engine::Postgres)], Vec::new(), &[]);
    model.nav.set_focus("missing-node");
    assert!(model.explorer_current_row().is_none());
    assert_eq!(model.nav_key(NavKey::Enter), None);
}

/// 结果区（页签头、消息页）右键弹结果区菜单而不是编辑器菜单；菜单在右键点遮罩、滚动、
/// 切换标签时关闭；结果区菜单的“全屏”转发到当前查询标签。
#[gpui_kit::test]
fn result_area_right_click_opens_its_own_menu_and_menus_close_on_scroll_right_click_and_tab_switch(cx: &mut gpui_kit::TestAppContext) {
    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-shell-results-menu-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench(cx, services_at(&dir), false);
    let place = |view: &gpui_kit::Entity<super::view::Workbench>, cx: &mut gpui_kit::App| view.read(cx).model.menu.as_ref().map(|menu| menu.place);
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.perform("new-query", cx);
            view.perform("new-query", cx);
        });
        window.render_frame(cx);
        view.update(cx, |view, cx| view.plant_query_result(vec!["id".into()], vec![vec!["1".into()], vec!["2".into()]], cx));
        window.render_frame(cx);
        // 结果页签头右键：结果区菜单。
        window.right_click("query-results-head", cx);
        window.render_frame(cx);
        assert_eq!(place(&view, cx), Some(MenuPlace::Results));
        let ids: Vec<&str> = view.read(cx).model.menu.as_ref().unwrap().items.iter().map(|item| item.id).collect();
        assert_eq!(ids, vec!["result-copy-sql", "result-copy-messages", "result-maximize", "result-collapse", "result-close"]);
        // 右键点遮罩：菜单关闭，不再弹别的菜单。
        window.right_click("context-backdrop", cx);
        window.render_frame(cx);
        assert_eq!(place(&view, cx), None, "右键点遮罩关闭菜单");
        // 滚动：菜单关闭。
        window.right_click("query-results-head", cx);
        window.render_frame(cx);
        assert_eq!(place(&view, cx), Some(MenuPlace::Results));
        window.scroll("context-backdrop", gpui_kit::ScrollDelta::Lines(gpui_kit::point(0., 3.)), cx);
        window.render_frame(cx);
        assert_eq!(place(&view, cx), None, "滚动关闭菜单");
        // 消息页同样是结果区菜单。
        view.update(cx, |view, cx| view.perform("query-view:messages", cx));
        window.render_frame(cx);
        window.right_click("query-log", cx);
        window.render_frame(cx);
        assert_eq!(place(&view, cx), Some(MenuPlace::Results), "消息区右键不再落到编辑器菜单");
        // 切换标签：菜单关闭。
        view.update(cx, |view, cx| view.perform("next-tab", cx));
        window.render_frame(cx);
        assert_eq!(place(&view, cx), None, "切换标签关闭菜单");
        // 编辑器里右键仍是编辑器菜单。
        view.update(cx, |view, cx| view.perform("menu-editor", cx));
        assert_eq!(place(&view, cx), Some(MenuPlace::Editor));
        assert!(view.update(cx, |view, _| view.model.close_menu()));
        assert_eq!(place(&view, cx), None);
        // 结果区菜单的“收起”经外壳转发到当前查询标签。
        view.update(cx, |view, cx| view.perform("result-collapse", cx));
        window.render_frame(cx);
        assert_eq!(view.read(cx).query.read(cx).result_layout(), crate::app::views::query::ResultLayout::Collapsed);
        assert!(window.try_find("query-results-collapsed").is_some());
        window.right_click("query-results-collapsed", cx);
        window.render_frame(cx);
        assert_eq!(place(&view, cx), Some(MenuPlace::Results), "收起细栏右键也是结果区菜单");
    })
    .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// 结果全屏时网格拿着键盘：Esc 退出全屏且选区不变；查看面板开着时 Esc 只关面板、仍在全屏。
/// 走真实按键路径（网格 key_down → 外壳根节点），不直接调 handle_escape。
#[gpui_kit::test]
fn escape_in_maximized_results_keeps_selection_and_closes_inner_panels_first(cx: &mut gpui_kit::TestAppContext) {
    use crate::app::views::query::ResultLayout;
    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-shell-results-escape-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench(cx, services_at(&dir), false);
    let layout = |view: &gpui_kit::Entity<super::view::Workbench>, cx: &gpui_kit::App| view.read(cx).query.read(cx).result_layout();
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.perform("new-query", cx));
        window.render_frame(cx);
        view.update(cx, |view, cx| view.plant_query_result(vec!["id".into()], vec![vec!["1".into()], vec!["2".into()], vec!["3".into()]], cx));
        window.render_frame(cx);
        let query = view.read(cx).query.clone();
        query.update(cx, |pane, cx| pane.select_grid_cell(2, 0, cx));
        view.update(cx, |view, cx| view.perform("result-maximize", cx));
        window.render_frame(cx);
        assert_eq!(layout(&view, cx), ResultLayout::Maximized);
        let focus = query.read(cx).grid_focus_handle(cx);
        window.focus(&focus, cx);
        window.render_frame(cx);

        // 查看面板开着：Esc 只关面板，仍在全屏。
        query.update(cx, |pane, cx| {
            let _ = pane.grid_command("grid-value-open", cx);
        });
        window.render_frame(cx);
        assert!(query.read(cx).grid_viewer_open(cx));
        window.focus(&focus, cx);
        window.press("escape", cx);
        window.render_frame(cx);
        assert!(!query.read(cx).grid_viewer_open(cx), "Esc 关掉查看面板");
        assert_eq!(layout(&view, cx), ResultLayout::Maximized, "关查看面板的那次 Esc 不退出全屏");

        // 网格没有别的事可做：Esc 退出全屏，选区不变。
        window.focus(&focus, cx);
        window.press("escape", cx);
        window.render_frame(cx);
        assert_eq!(layout(&view, cx), ResultLayout::Normal, "Esc 退出全屏");
        assert_eq!(query.read(cx).grid_focus_cell(cx), Some((2, 0)), "退出全屏时选区保留");

        // 普通布局下 Esc 仍按原样清掉选区。
        window.focus(&focus, cx);
        window.press("escape", cx);
        window.render_frame(cx);
        assert_eq!(layout(&view, cx), ResultLayout::Normal);
        assert_eq!(query.read(cx).grid_focus_cell(cx), None, "普通布局下 Esc 清选区");
    })
    .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// 只读查询结果的单元格右键：写入项保留但禁用，悬停说明会话给出的具体原因；行号格右键是整行菜单。
#[gpui_kit::test]
fn query_result_cell_and_row_header_menus_differ_and_explain_readonly(cx: &mut gpui_kit::TestAppContext) {
    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-shell-grid-areas-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench(cx, services_at(&dir), false);
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.perform("new-query", cx));
        window.render_frame(cx);
        view.update(cx, |view, cx| view.plant_query_result(vec!["id".into(), "name".into()], vec![vec!["1".into(), "a".into()], vec!["2".into(), "b".into()]], cx));
        window.render_frame(cx);
        window.right_click(gpui_kit::ElementId::named_usize("grid-cell-1", 1), cx);
        window.render_frame(cx);
        let cell: Vec<(String, bool, Option<String>)> = view
            .read(cx)
            .model
            .menu
            .as_ref()
            .expect("单元格菜单")
            .items
            .iter()
            .map(|item| (item.id.to_owned(), item.enabled, item.reason.clone()))
            .collect();
        assert_eq!(view.read(cx).model.menu.as_ref().map(|menu| menu.place), Some(MenuPlace::Grid));
        let edit = cell.iter().find(|item| item.0 == "edit-cell").expect("只读结果也列出编辑");
        assert!(!edit.1);
        assert!(edit.2.as_deref().is_some_and(|reason| !reason.is_empty()), "禁用原因来自会话：{edit:?}");
        assert!(cell.iter().any(|item| item.0 == "grid-filter-value"));
        assert!(window.try_find("ctx-edit-cell").is_some());
        view.update(cx, |view, _| {
            view.model.close_menu();
        });
        window.render_frame(cx);
        window.right_click(gpui_kit::ElementId::named_usize("grid-cell-0", 0), cx);
        window.render_frame(cx);
        let row: Vec<&str> = view.read(cx).model.menu.as_ref().expect("行头菜单").items.iter().map(|item| item.id).collect();
        assert!(row.contains(&"grid-record-toggle") && row.contains(&"mark-delete"), "{row:?}");
        assert!(!row.contains(&"edit-cell") && !row.contains(&"grid-filter-value"), "行头没有按格的动作：{row:?}");
    })
    .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// R5：固定标签排在左侧、随会话保存（旧会话按未固定读取）、批量关闭时跳过；
/// “关闭全部未修改”只关不需要确认的标签，有草稿和正在执行的留下；菜单项随状态切换文案和可用性。
#[test]
fn pinned_tabs_and_close_unmodified_follow_the_tab_menu_and_the_session() {
    let mut model = WorkbenchModel::new(Shell::default());
    for _ in 0..4 {
        assert!(model.dispatch("new-query").is_some());
    }
    let ids: Vec<String> = model.tabs.iter().map(|tab| tab.id.clone()).collect();
    model.tabs[1].draft = "select 1".into();
    model.tabs[2].running = true;
    let tab_menu = |model: &mut WorkbenchModel| {
        model.open_tab_menu();
        let items = model.menu.as_ref().expect("页签菜单").items.clone();
        model.menu = None;
        items
    };
    let find = |items: &[crate::app::shell::menus::MenuItem], id: &str| items.iter().find(|item| item.id == id).cloned().expect(id);

    // 固定最后一个：它挪到最左侧，菜单改成“取消固定”。
    assert!(model.activate_item(&ids[3]));
    let items = tab_menu(&mut model);
    assert_eq!(find(&items, "tab-pin").label_key, "shell.tabs.pin");
    assert!(find(&items, "tab-close-unmodified").enabled, "有空查询可关");
    assert!(model.dispatch("tab-pin").is_some());
    assert_eq!(model.tabs[0].id, ids[3]);
    assert!(model.tabs[0].pinned);
    assert_eq!(find(&tab_menu(&mut model), "tab-pin").label_key, "shell.tabs.unpin");

    // 关闭全部未修改：空查询关掉；有草稿、正在执行、固定的都留下，也不弹确认。
    assert!(model.dispatch("tab-close-unmodified").is_some());
    let left: Vec<&str> = model.tabs.iter().map(|tab| tab.id.as_str()).collect();
    assert_eq!(left, [ids[3].as_str(), ids[1].as_str(), ids[2].as_str()]);
    assert!(model.close_prompt.is_none());
    let items = tab_menu(&mut model);
    let none = find(&items, "tab-close-unmodified");
    assert!(!none.enabled);
    assert_eq!(none.reason_key, Some("shell.tabs.noneUnmodified"), "禁用时说明原因");

    // 批量关闭跳过固定标签。
    model.tabs.iter_mut().for_each(|tab| {
        tab.running = false;
        tab.draft.clear();
    });
    assert!(model.activate_item(&ids[1]));
    assert!(model.dispatch("tab-close-all").is_some());
    assert_eq!(model.tabs.iter().map(|tab| tab.id.as_str()).collect::<Vec<_>>(), [ids[3].as_str()], "关闭全部留下固定标签");

    // 会话：固定状态写出并读回；旧会话没有这一项时按未固定读取；乱序的固定标签收拢到左侧。
    assert!(model.dispatch("new-query").is_some());
    let fresh = model.active_tab.clone().unwrap();
    let bytes = crate::state::to_bytes(&model.snapshot()).unwrap();
    let parsed = crate::state::parse_stored(&bytes).unwrap();
    let mut restored = WorkbenchModel::new(Shell::default());
    restored.restore_session(&parsed.session);
    assert!(restored.is_pinned(&ids[3]));
    assert!(!restored.is_pinned(&fresh));
    let mut session = parsed.session.clone();
    session.tabs.reverse();
    restored.restore_session(&session);
    assert_eq!(restored.tabs[0].id, ids[3], "固定标签恢复后仍在最左侧");
    let mut legacy: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(legacy["session"]["pinned"].is_array(), "固定标签写进会话");
    legacy["session"].as_object_mut().unwrap().remove("pinned");
    let old = crate::state::parse_stored(&serde_json::to_vec(&legacy).unwrap()).unwrap();
    restored.restore_session(&old.session);
    assert!(restored.tabs.iter().all(|tab| !tab.pinned), "旧会话按未固定读取");

    // 取消固定：放到固定区之后的第一个。
    assert!(model.set_pinned(&fresh, true));
    assert_eq!(model.tabs.iter().map(|tab| tab.id.as_str()).collect::<Vec<_>>(), [ids[3].as_str(), fresh.as_str()]);
    assert!(model.set_pinned(&ids[3], false));
    assert_eq!(model.tabs.iter().map(|tab| tab.id.as_str()).collect::<Vec<_>>(), [fresh.as_str(), ids[3].as_str()]);
    assert!(!model.set_pinned(&ids[3], false), "状态没变不算变化");
}

/// R6：作废的目录读取回来时，只有节点上没有更新的读取在途才收起加载态。
#[test]
fn a_dropped_nav_read_clears_loading_unless_a_newer_read_is_in_flight() {
    use super::view::nav_read_drop_clears;
    assert!(nav_read_drop_clears(false, true), "没被取消（连接版本变了）：登记的就是自己，收起");
    assert!(nav_read_drop_clears(true, false), "被取消且没有别的读取：收起");
    assert!(!nav_read_drop_clears(true, true), "被同一节点更新的读取顶掉：留给新读取收尾");
}

fn plain_workbench(cx: &mut gpui_kit::TestAppContext) -> (gpui_kit::AnyWindowHandle, gpui_kit::Entity<super::view::Workbench>) {
    let dir = std::env::temp_dir().join(format!("rillsql-tabs-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = Arc::new(AppServices::new(Arc::new(Service::new_for_test(dir.join("connections.sqlite")))));
    cx.update(gpui_kit::init);
    cx.update(|cx| {
        let bounds = gpui_kit::Bounds { origin: gpui_kit::Point::default(), size: gpui_kit::size(gpui_kit::px(1100.), gpui_kit::px(720.)) };
        let (window, view) = gpui_kit::open_window(
            gpui_kit::WindowOptions { window_bounds: Some(gpui_kit::WindowBounds::Windowed(bounds)), ..Default::default() },
            cx,
            |window, cx| cx.new(|cx| super::view::Workbench::with_services(window, cx, services, false)),
        )
        .expect("workbench");
        (window, view)
    })
}

/// R5/R6：300 个查询标签只建一个编辑器；空闲重绘不再做整理（同步只由事件触发）；
/// 标签列表按标题、连接、库搜索，选中后切到对应标签。
#[gpui_kit::test]
fn hundreds_of_query_tabs_stay_lazy_idle_frames_do_not_settle_and_the_tab_list_switches(cx: &mut gpui_kit::TestAppContext) {
    let (window, view) = plain_workbench(cx);
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            for _ in 0..300 {
                view.perform("new-query", cx);
            }
            let first = view.model.tabs[0].id.clone();
            assert!(view.model.activate_item(&first));
            view.model.tabs[0].connection_name = Some("Atlas".to_owned());
            view.model.tabs[0].database = Some("orders".to_owned());
            view.settle(cx);
        });
        window.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
    let _ = cx.update_window(window, |_, window, cx| window.render_frame(cx));
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.model.tabs.iter().filter(|tab| tab.kind.is_query()).count(), 300);
        assert_eq!(view.query.read(cx).editor_count(), 1, "只给当前标签建编辑器");
    });

    // 空闲：没有事件时重绘只构建元素，不再跑整理。
    let settled = cx.update(|cx| view.read(cx).settle_count.get());
    let rendered = cx.update(|cx| view.read(cx).render_count.get());
    for _ in 0..3 {
        cx.update_window(window, |_, window, cx| {
            view.update(cx, |_, cx| cx.notify());
            window.render_frame(cx);
        })
        .unwrap();
        cx.run_until_parked();
    }
    let (settles, renders) = cx.update(|cx| (view.read(cx).settle_count.get() - settled, view.read(cx).render_count.get() - rendered));
    assert!(renders >= 3, "每次通知都重绘：{renders}");
    assert_eq!(settles, 3, "每次事件整理一次，整理本身不再引出下一轮：{settles} / {renders}");
    let before = cx.update(|cx| view.read(cx).settle_count.get());
    for _ in 0..3 {
        let _ = cx.update_window(window, |_, window, cx| window.render_frame(cx));
        cx.run_until_parked();
    }
    assert_eq!(cx.update(|cx| view.read(cx).settle_count.get()), before, "没有事件的重绘不整理");

    // 标签列表：按连接名搜索到第一个标签，回车切过去。
    let last = cx.update(|cx| view.read(cx).model.tabs.last().unwrap().id.clone());
    let first = cx.update(|cx| view.read(cx).model.tabs[0].id.clone());
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            assert!(view.model.activate_item(&last));
            view.toggle_tab_picker(window, cx);
        });
        window.render_frame(cx);
        let picker = view.read(cx).tab_picker.as_ref().map(|(picker, _)| picker.clone()).expect("标签列表打开");
        assert_eq!(picker.read(cx).model().items().len(), 300);
        picker.update(cx, |picker, cx| picker.set_query("atlas", cx));
        assert_eq!(picker.read(cx).model().matches().len(), 1, "按连接名搜索");
        picker.update(cx, |picker, cx| picker.set_query("orders", cx));
        assert_eq!(picker.read(cx).model().matches().len(), 1, "按库名搜索");
        picker.update(cx, |picker, cx| {
            assert!(picker.press("enter", cx));
        });
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        let view = view.read(cx);
        assert!(view.tab_picker.is_none(), "选中后关闭");
        assert_eq!(view.model.active_tab.as_deref(), Some(first.as_str()));
        assert!(view.query.read(cx).editor_count() <= 2, "访问过的标签才有编辑器");
    });
}

/// 一波事件里连续整理到了上限时不丢后面的通知：补排一次延后整理（每帧最多一次），
/// 后到的回执不用等到下一个无关事件才处理。
#[gpui_kit::test]
fn settle_cap_defers_one_more_settle_instead_of_dropping(cx: &mut gpui_kit::TestAppContext) {
    let (_window, view) = plain_workbench(cx);
    cx.run_until_parked();
    let (s0, d0) = cx.update(|cx| (view.read(cx).settle_count.get(), view.read(cx).deferred_settle_count.get()));
    cx.update(|cx| {
        view.update(cx, |view, cx| {
            for _ in 0..(super::view::SETTLE_PASSES + 3) {
                view.settle_from_event(cx);
            }
            assert_eq!(view.settle_count.get() - s0, super::view::SETTLE_PASSES as usize, "上限以内照常整理");
        });
    });
    let (s1, d1) = cx.update(|cx| (view.read(cx).settle_count.get(), view.read(cx).deferred_settle_count.get()));
    assert_eq!(d1 - d0, 1, "超过上限的几次合并成一次延后整理");
    assert_eq!(s1 - s0, super::view::SETTLE_PASSES as usize + 1);
}

/// R6：编辑器光标闪烁只把工作台标脏、不通知它：工作台照常重绘，但页签条是缓存视图，
/// 几百个页签不随每次闪烁重建；工作台真被通知时页签条跟着重建。
#[gpui_kit::test]
fn caret_blink_redraws_do_not_rebuild_the_tab_strip(cx: &mut gpui_kit::TestAppContext) {
    let (window, view) = plain_workbench(cx);
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            for _ in 0..300 {
                view.perform("new-query", cx);
            }
        });
        window.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
    let _ = cx.update_window(window, |_, window, cx| window.render_frame(cx));
    assert!(cx.update_window(window, |_, window, cx| view.read(cx).query_editor_focused(window, cx)).unwrap(), "编辑器有焦点，光标在闪");
    let counts = |cx: &mut gpui_kit::TestAppContext| cx.update(|cx| {
        let view = view.read(cx);
        (view.render_count.get(), view.settle_count.get(), view.strip_count.get())
    });
    let (r0, s0, t0) = counts(cx);
    for _ in 0..4 {
        cx.executor().advance_clock(std::time::Duration::from_millis(1000));
        cx.run_until_parked();
    }
    let (r1, s1, t1) = counts(cx);
    assert!(r1 > r0, "光标闪烁让工作台重绘：{r0} -> {r1}");
    assert_eq!(s1, s0, "闪烁不是事件，不整理");
    assert_eq!(t1, t0, "页签条沿用缓存，不随闪烁重建");
    // 工作台被通知（状态可能变了）：页签条重建。
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |_, cx| cx.notify());
        window.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert!(counts(cx).2 > t1, "工作台被通知后页签条重建");
}

/// R6：光标静置 `CARET_IDLE_SECS` 秒后停止闪烁，窗口静置时工作台不再每 500ms 重绘；按键后重新开始闪烁。
#[gpui_kit::test]
fn an_idle_caret_stops_blinking_and_the_workbench_stops_redrawing(cx: &mut gpui_kit::TestAppContext) {
    let (window, view) = plain_workbench(cx);
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.perform("new-query", cx));
        window.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
    let _ = cx.update_window(window, |_, window, cx| window.render_frame(cx));
    assert!(cx.update_window(window, |_, window, cx| view.read(cx).query_editor_focused(window, cx)).unwrap(), "编辑器有焦点");
    assert_eq!(
        cx.update(|cx| gpui_kit::component::ActiveTheme::theme(cx).cursor_blink.idle_timeout()),
        Some(std::time::Duration::from_secs(crate::ui::kit::CARET_IDLE_SECS)),
        "主题里装了静置停闪策略"
    );
    let renders = |cx: &mut gpui_kit::TestAppContext| cx.update(|cx| view.read(cx).render_count.get());
    let r0 = renders(cx);
    for _ in 0..3 {
        cx.executor().advance_clock(std::time::Duration::from_millis(1000));
        cx.run_until_parked();
    }
    assert!(renders(cx) > r0, "刚聚焦时光标在闪");
    for _ in 0..crate::ui::kit::CARET_IDLE_SECS {
        cx.executor().advance_clock(std::time::Duration::from_millis(1000));
        cx.run_until_parked();
    }
    let r1 = renders(cx);
    for _ in 0..10 {
        cx.executor().advance_clock(std::time::Duration::from_millis(1000));
        cx.run_until_parked();
    }
    assert_eq!(renders(cx), r1, "静置后不再因光标闪烁重绘");
}

/// R8：设置页“数据”分区编辑索引命名模板，下方示例实时更新；改动写盘，结构页拿到新模板。
#[gpui_kit::test]
fn index_naming_templates_are_edited_in_settings_and_reach_the_structure_pane(cx: &mut gpui_kit::TestAppContext) {
    use crate::app::views::settings::SaveState;

    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-index-naming-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let (window, view) = open_workbench(cx, Arc::clone(&services), false);
    cx.update(|cx| {
        view.update(cx, |view, cx| {
            view.perform("settings", cx);
            view.perform("settings-section-data", cx);
        })
    });
    let (group, preview, normal) = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            (
                window.try_find("settings-group-index-naming").is_some(),
                window.try_find("settings-index-naming-preview").is_some(),
                window.try_find("field-IndexNamingNormal").is_some(),
            )
        })
        .unwrap();
    assert!(group && preview && normal, "数据分区有索引命名模板与示例");
    assert_eq!(cx.update(|cx| view.read(cx).model.field_value(DraftField::IndexNamingNormal)), crate::ddl::naming::DEFAULT_NORMAL);

    cx.update_window(window, |_, window, cx| {
        window.click("field-IndexNamingJoiner", cx);
        window.input("$", cx);
        window.render_frame(cx);
    })
    .unwrap();
    let joiner = cx.update(|cx| view.read(cx).model.settings.index_naming.joiner.clone());
    assert!(joiner.contains('$'), "输入落到设置：{joiner:?}");
    assert!(
        pump_until(cx, window, &view, |view, cx| view.model.settings_save == SaveState::Saved
            && view.structure.read(cx).index_naming().joiner == view.model.settings.index_naming.joiner),
        "模板改动写盘，并下发到结构页"
    );
    let stored = cx.update(|cx| view.read(cx).model.settings.to_settings().index_naming);
    assert_eq!(stored.joiner, joiner);
    let _ = std::fs::remove_dir_all(&dir);
}

/// R14：Redis 键浏览器里双击、回车或「在新标签中固定打开」留下的请求由外壳取走：
/// 新建一个固定的 Redis 标签（标题是键名），面板按带键的规格直接选中这个键；同一个键再请求一次只切过去，不重复建标签。
#[gpui_kit::test]
fn redis_open_requests_open_a_pinned_key_tab(cx: &mut gpui_kit::TestAppContext) {
    cx.executor().allow_parking();
    let dir = std::env::temp_dir().join(format!("rillsql-shell-redis-pin-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench(cx, services_at(&dir), false);
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.perform("open-redis:missing|缓存|0", cx));
        window.render_frame(cx);
    })
    .unwrap();
    let browse_tab = cx.update(|cx| view.read(cx).model.active_tab.clone().unwrap());
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.redis.update(cx, |pane, cx| {
                pane.plant_key("a|b");
                pane.command("redis-open-tab:0", cx);
            });
        });
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        let view = view.read(cx);
        let spec = "missing|缓存|0|a|b";
        let tab = view.model.tabs.iter().find(|tab| tab.spec.as_deref() == Some(spec)).expect("请求被取走并新建了标签");
        assert!(tab.pinned, "新标签是固定的");
        assert_eq!(tab.title, "a|b", "标题是键名，键名里可以有 |");
        assert_eq!(view.model.active_tab.as_deref(), Some(tab.id.as_str()), "并切到这个标签");
        assert_eq!(view.model.tabs[0].id, tab.id, "固定标签排在最左侧");
        assert_ne!(tab.id, browse_tab);
        assert_eq!(view.model.redis_spec.as_deref(), Some(spec));
        assert_eq!(view.redis.read(cx).selected(), "a|b", "面板按带键的规格选中这个键");
    });
    let count = cx.update(|cx| view.read(cx).model.tabs.len());
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.redis.update(cx, |pane, cx| {
                pane.plant_key("a|b");
                pane.command("redis-open-tab", cx);
            });
        });
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| assert_eq!(view.read(cx).model.tabs.len(), count, "同一个键不重复建标签"));
    let _ = std::fs::remove_dir_all(dir);
}

/// R14：双击查询标签就地重命名：回车确认、Esc 取消、清空恢复自动编号；改过的名字写进会话并能恢复，
/// 不再被「查询 n」自动编号覆盖。只改标题，不碰草稿。
#[test]
fn query_tab_rename_confirms_cancels_and_survives_the_session() {
    let mut model = WorkbenchModel::new(Shell::default());
    model.dispatch("new-query");
    model.dispatch("new-query");
    let id = model.active_tab.clone().unwrap();
    model.tabs.iter_mut().find(|tab| tab.id == id).unwrap().draft = "select 1".into();
    let auto = model.active_tab().unwrap().title.clone();
    assert!(model.begin_tab_rename());
    assert_eq!(model.field_value(DraftField::TabTitle), auto, "输入框带着原标题");
    model.set_field(DraftField::TabTitle, "  月报  ".into());
    model.dispatch("tab-rename-confirm");
    assert_eq!(model.active_tab().unwrap().title, "月报");
    assert_eq!(model.active_tab().unwrap().draft, "select 1", "重命名不碰草稿");
    model.dispatch("new-query");
    assert_eq!(model.tabs.iter().find(|tab| tab.id == id).unwrap().title, "月报", "新开标签重新编号时不覆盖改过的名字");

    let _ = model.activate_item(&id);
    assert!(model.begin_tab_rename());
    model.set_field(DraftField::TabTitle, "不要".into());
    model.dispatch("tab-rename-cancel");
    assert_eq!(model.active_tab().unwrap().title, "月报", "Esc 取消");

    let mut restored = WorkbenchModel::new(Shell::default());
    restored.restore_session(&model.snapshot().session);
    assert_eq!(restored.tabs.iter().find(|tab| tab.id == id).unwrap().title, "月报", "会话恢复后保留改过的名字");
    assert!(restored.renamed_tabs.contains(&id));

    let _ = model.activate_item(&id);
    assert!(model.begin_tab_rename());
    model.set_field(DraftField::TabTitle, "   ".into());
    model.dispatch("tab-rename-confirm");
    assert_eq!(model.active_tab().unwrap().title, auto, "清空恢复自动编号");

    model.dispatch("open-redis:cache|缓存|0");
    assert!(!model.begin_tab_rename(), "非查询标签不能改名");
}

/// R14：原生窗口里双击查询标签出现重命名输入框，回车确认。
#[gpui_kit::test]
fn double_clicking_a_query_tab_renames_it(cx: &mut gpui_kit::TestAppContext) {
    let dir = std::env::temp_dir().join(format!("rillsql-shell-tab-rename-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench(cx, services_at(&dir), false);
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.perform("new-query", cx));
        window.render_frame(cx);
    })
    .unwrap();
    let id = cx.update(|cx| view.read(cx).model.active_tab.clone().unwrap());
    cx.update_window(window, |_, window, cx| window.double_click(format!("tab-{id}"), cx)).unwrap();
    cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
    let shown = cx.update_window(window, |_, window, _| window.try_find("tab-rename").is_some()).unwrap();
    assert!(shown, "双击后出现重命名输入框");
    cx.update(|cx| view.update(cx, |view, _| view.model.set_field(DraftField::TabTitle, "对账".into())));
    cx.update(|cx| view.update(cx, |view, cx| view.perform("tab-rename-confirm", cx)));
    cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.model.active_tab().unwrap().title, "对账");
        assert!(view.model.tab_rename.is_none());
    });
    let _ = std::fs::remove_dir_all(dir);
}

/// 重命名输入框失焦（点到编辑器或别的区域）时按当前输入确认，不一直挂在标签栏上。
#[gpui_kit::test]
fn tab_rename_confirms_when_the_input_loses_focus(cx: &mut gpui_kit::TestAppContext) {
    let dir = std::env::temp_dir().join(format!("rillsql-shell-tab-rename-blur-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench(cx, services_at(&dir), false);
    // 失焦事件只在活动窗口里派发（与原生窗口一致）。
    cx.update_window(window, |_, window, _| window.activate_window()).unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.perform("new-query", cx));
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| view.update(cx, |view, cx| view.perform("tab-rename", cx)));
    cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
    let focused = cx.update_window(window, |_, window, cx| view.update(cx, |view, cx| view.fields.is_focused(DraftField::TabTitle, window, cx))).unwrap();
    assert!(focused, "重命名输入框打开即拿到焦点");
    cx.update(|cx| view.update(cx, |view, _| view.model.set_field(DraftField::TabTitle, "月报".into())));
    // 焦点移到别处（等同点击编辑器或其他面板）。
    cx.update_window(window, |_, window, cx| {
        let other = view.read(cx).overview_focus.clone();
        window.focus(&other, cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        let view = view.read(cx);
        assert!(view.model.tab_rename.is_none(), "失焦后结束重命名");
        assert_eq!(view.model.active_tab().unwrap().title, "月报", "按当前输入确认");
    });
    let found = cx.update_window(window, |_, window, _| window.try_find("tab-rename").is_some()).unwrap();
    assert!(!found, "输入框不再显示");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn tab_reorder_respects_pinned_boundary_and_alt_1_to_9_switches_tabs() {
    let mut model = WorkbenchModel::new(Shell::default());
    assert!(model.dispatch("new-query").is_some());
    let a = model.active_tab.clone().unwrap();
    assert!(model.dispatch("new-query").is_some());
    let b = model.active_tab.clone().unwrap();
    assert!(model.dispatch("new-query").is_some());
    let c = model.active_tab.clone().unwrap();

    // 普通标签拖拽重排。
    assert!(model.reorder_tab(&c, &a));
    let ids: Vec<&str> = model.tabs.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, vec![c.as_str(), a.as_str(), b.as_str()]);
    assert!(!model.reorder_tab(&a, &a), "同标签拖放不改动");
    assert!(!model.reorder_tab(&a, "missing"), "未知标签忽略");

    // 固定区与普通区之间不可跨区拖放。
    assert!(model.set_pinned(&c, true));
    assert!(!model.reorder_tab(&c, &a), "固定标签不能拖入普通区");
    assert!(!model.reorder_tab(&a, &c), "普通标签不能拖入固定区");

    // Alt+1..9 切换对应序号标签，越界静默忽略。
    assert!(model.dispatch("select-tab-1").is_some());
    assert_eq!(model.active_tab.as_deref(), Some(c.as_str()));
    assert!(model.dispatch("select-tab-2").is_some());
    assert_eq!(model.active_tab.as_deref(), Some(a.as_str()));
    assert!(model.dispatch("select-tab-3").is_some());
    assert_eq!(model.active_tab.as_deref(), Some(b.as_str()));
    assert!(model.dispatch("select-tab-9").is_some());
    assert_eq!(model.active_tab.as_deref(), Some(b.as_str()), "越界序号保持当前标签不变");

    // keymap 注册了 9 条 Alt+1..9 绑定。
    let nth = crate::app::keymap::shortcut("nth-tab").expect("nth-tab");
    assert_eq!(crate::app::keymap::bindings_of(nth, false).len(), 9);
    assert_eq!(crate::app::keymap::bindings_of(nth, true).len(), 9);
}

#[test]
fn blend_opaque_produces_fully_opaque_selection_surface() {
    for p in [crate::ui::kit::Palette::light(), crate::ui::kit::Palette::dark()] {
        let blended = crate::ui::kit::blend_opaque(p.chrome, p.selection);
        assert!((blended.a - 1.0).abs() < 1e-5, "合成后的选中底色必须完全不透明，避免悬停按钮透字");
    }
}

#[test]
fn palette_ranks_by_match_score_before_group_rank_when_query_is_non_empty() {
    let mut model = WorkbenchModel::new(Shell::default());
    // 放入一个表名精确匹配 "settings" 的对象，与命令组的设置相关项竞争；有查询词时精确匹配应排在最前。
    let exact = tree_object("pg", "Prod", "postgres", "app", "public", "settings", "table");
    let sub = tree_object("pg", "Prod", "postgres", "app", "public", "user_settings_log", "table");
    assert!(model.set_catalog(&[exact, sub], ""));
    model.shell.dispatch("command-palette");
    model.set_field(DraftField::PaletteQuery, "settings".into());
    let entries = model.palette_entries();
    assert!(!entries.is_empty());
    assert_eq!(entries[0].label, "settings", "精确匹配应优先于低匹配度的其他分组条目");
}

#[gpui_kit::test]
fn topbar_context_status_no_target_picker_and_unified_tab_dirty_dots(cx: &mut gpui_kit::TestAppContext) {
    let dir = std::env::temp_dir().join(format!("rillsql-shell-r1-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench(cx, services_at(&dir), false);
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.perform("new-query", cx);
            view.perform("new-query", cx);
            let q_id = view.model.tabs[0].id.clone();
            if let Some(tab) = view.model.tabs.iter_mut().find(|t| t.id == q_id) {
                tab.draft = "SELECT 42".into();
            }
            // 第二个查询标签保持无目标，验证状态栏“无目标”点击可打开目标选择器。
            let second_id = view.model.tabs[1].id.clone();
            let _ = view.model.activate_item(&second_id);
            if let Some(tab) = view.model.tabs.iter_mut().find(|t| t.id == second_id) {
                tab.connection_id = None;
                tab.connection_name = None;
                tab.database = None;
            }
            cx.notify();
        });
        window.render_frame(cx);
    })
    .unwrap();

    // 顶栏 aria_label 修正且标签计数可见。
    let (toolbar_label, picker_count) = cx
        .update_window(window, |_, window, _| {
            (
                window.find("toolbar").label().map(str::to_owned),
                window.try_find("tab-picker-count").is_some(),
            )
        })
        .unwrap();
    assert_ne!(toolbar_label.as_deref(), Some("新建查询"), "顶栏 aria_label 不再误写为新建查询");
    assert!(picker_count, "多标签时标签列表按钮显示计数");

    // 点击“无目标”状态栏按钮应打开查询目标选择器。
    cx.update_window(window, |_, window, cx| {
        window.click("status-target", cx);
        window.render_frame(cx);
    })
    .unwrap();
    let popover_open = cx.update_window(window, |_, window, _| window.try_find("query-target-popover").is_some()).unwrap();
    assert!(popover_open, "状态栏无目标分支点击应打开目标选择器");

    // 绑定生产连接后顶栏呈现活动目标 pill。
    cx.update(|cx| {
        view.update(cx, |view, cx| {
            let mut rec = record_for("prod-1", "ProdDB", Engine::Postgres);
            rec.saved.env = "prod".into();
            view.model.connections.rows.push(rec);
            if let Some(tab) = view.model.active_mut() {
                tab.connection_id = Some("prod-1".into());
                tab.connection_name = Some("ProdDB".into());
                tab.database = Some("app".into());
                tab.engine = Some("postgres".into());
            }
            cx.notify();
        });
    });
    let top_target = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.try_find("top-target").and_then(|el| el.label().map(str::to_owned))
        })
        .unwrap();
    assert_eq!(top_target.as_deref(), Some("ProdDB / app"), "顶栏应呈现当前活动目标");

    let _ = std::fs::remove_dir_all(dir);
}

#[gpui_kit::test]
fn object_tree_filter_focus_protection_skipped_notice_and_shift_enter_structure(cx: &mut gpui_kit::TestAppContext) {
    let (window, view, dir, ids) = object_tree_workbench(cx, 1200., false);
    // 添加一条未连接连接，验证搜索时给出跳过提示。
    cx.update(|cx| {
        view.update(cx, |view, cx| {
            view.model.connections.rows.push(record_for("offline-1", "OfflineDB", Engine::Postgres));
            cx.notify();
        });
    });

    // Mod+Shift+L (explorer-search) 打开侧栏并聚焦对象树搜索框。
    cx.update(|cx| view.update(cx, |view, cx| view.perform("explorer-search", cx)));
    let filter_focused = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            view.update(cx, |view, cx| view.fields.is_focused(DraftField::ObjectFilter, window, cx))
        })
        .unwrap();
    assert!(filter_focused, "explorer-search 快捷键应聚焦对象树搜索框");

    // 聚焦搜索框时按方向键/回车不触发对象树导航或打开表。
    let before_tabs = cx.update(|cx| view.read(cx).model.tabs.len());
    let before_current = cx.update(|cx| view.read(cx).model.explorer_current.clone());
    cx.update_window(window, |_, window, cx| {
        window.press("down", cx);
        window.press("enter", cx);
        window.render_frame(cx);
    })
    .unwrap();
    let (after_tabs, after_current) = cx.update(|cx| (view.read(cx).model.tabs.len(), view.read(cx).model.explorer_current.clone()));
    assert_eq!(after_tabs, before_tabs, "搜索框聚焦时按回车不应误打开表");
    assert_eq!(after_current, before_current, "搜索框聚焦时按方向键不应移动对象树行");

    // 输入过滤词后提示跳过的未连接连接。
    cx.update(|cx| view.update(cx, |view, cx| {
        view.model.set_field(DraftField::ObjectFilter, "zzz_no_hit".into());
        cx.notify();
    }));
    let (skipped_count, has_skipped_banner) = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            (view.read(cx).model.nav_search_skipped_count(), window.try_find("explorer-search-skipped").is_some())
        })
        .unwrap();
    assert!(skipped_count >= 1, "应统计被跳过的未连接连接");
    assert!(has_skipped_banner, "应渲染跳过未连接连接的明确提示");

    // 清除过滤，焦点停在表节点，按 Shift+Enter 直接打开表结构。
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.model.set_field(DraftField::ObjectFilter, String::new());
            view.model.shell.focus = crate::app::FocusTarget::Sidebar;
            view.model.explorer_current = Some(super::explorer::ExplorerRow::Node(ids.orders.clone()));
            cx.notify();
        });
        let nav_focus = view.read(cx).nav_focus.clone();
        window.focus(&nav_focus, cx);
        window.render_frame(cx);
        window.press("shift-enter", cx);
        window.render_frame(cx);
    })
    .unwrap();
    let active_kind = cx.update(|cx| view.read(cx).model.active_tab().map(|t| t.kind));
    assert_eq!(active_kind, Some(WorkItemKind::Structure), "对象树表节点按 Shift+Enter 应直接打开表结构");

    let _ = std::fs::remove_dir_all(dir);
}

#[gpui_kit::test]
fn palette_highlights_cycles_scopes_with_tab_and_restores_focus_to_structure_pane(cx: &mut gpui_kit::TestAppContext) {
    let dir = std::env::temp_dir().join(format!("rillsql-shell-r3-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let (window, view) = open_workbench(cx, services_at(&dir), false);
    let spec = "notes|笔记|main||customers|sqlite|dev";
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.perform(&format!("open-structure-for:{spec}"), cx);
            view.perform("command-palette", cx);
        });
        window.render_frame(cx);
    })
    .unwrap();

    // 输入查询词后渲染命中高亮。
    cx.update(|cx| view.update(cx, |view, cx| {
        view.model.set_field(DraftField::PaletteQuery, "查询".into());
        cx.notify();
    }));
    let has_highlight = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.try_find("pal-match-0").is_some()
        })
        .unwrap();
    assert!(has_highlight, "命令面板命中项应渲染高亮片段");

    // Tab / Shift+Tab 切换命令面板分组。
    cx.update_window(window, |_, window, cx| {
        window.press("tab", cx);
        window.render_frame(cx);
    })
    .unwrap();
    let scope_after_tab = cx.update(|cx| view.read(cx).model.effective_palette_scope());
    assert_eq!(scope_after_tab, super::model::PaletteScope::Objects, "Tab 应切到下一个分组");

    cx.update_window(window, |_, window, cx| {
        window.press("shift-tab", cx);
        window.render_frame(cx);
    })
    .unwrap();
    let scope_after_shift_tab = cx.update(|cx| view.read(cx).model.effective_palette_scope());
    assert_eq!(scope_after_shift_tab, super::model::PaletteScope::All, "Shift+Tab 应切回上一个分组");

    // Esc 关闭浮层后焦点正确还原到 StructurePane 的 FocusHandle。
    cx.update_window(window, |_, window, cx| {
        window.press("escape", cx);
        window.render_frame(cx);
    })
    .unwrap();
    let structure_focused = cx
        .update_window(window, |_, window, cx| {
            let structure = view.read(cx).structure.clone();
            let handle = structure.update(cx, |pane, cx| pane.focus_handle(cx));
            handle.is_focused(window)
        })
        .unwrap();
    assert!(structure_focused, "关闭命令面板后焦点应还原到活动表结构视图 StructurePane");

    let _ = std::fs::remove_dir_all(dir);
}
