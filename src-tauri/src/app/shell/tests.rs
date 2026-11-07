//! 外壳、连接、历史、设置和关闭守卫。不连接远程数据库。

use std::sync::Arc;

use crate::app::shell::jobs::{self, apply_done, job_request};
use crate::app::shell::menus::{menu_items, MenuPlace, MenuTarget, NavKey};
use crate::app::views::connections::{sample_draft, with_tls_ssh, Secrets};
use crate::app::views::history::HistorySection;
use crate::app::{dictionary_keys, translate, AppPage, Locale, ThemePreference};
use crate::application::AppServices;
use crate::connections::model::Engine;
use crate::connections::Service;
use crate::i18n;
use crate::query::SaveRequest;
use crate::state::{AppState, SessionState, ShellSettings};

use super::model::WorkbenchModel;
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

#[test]
fn dictionaries_match_the_existing_shell_keys() {
    for key in dictionary_keys() {
        assert_eq!(i18n::text(i18n::Locale::ZhCn, key), translate(Locale::ZhCn, key), "{key}");
        assert_eq!(i18n::text(i18n::Locale::En, key), translate(Locale::En, key), "{key}");
    }
}

#[test]
fn copy_fits_every_width_theme_and_locale() {
    let themes = [ThemePreference::Light, ThemePreference::Dark];
    let locales = [i18n::Locale::ZhCn, i18n::Locale::En];
    let pages = [AppPage::Workspace, AppPage::Connections, AppPage::History, AppPage::Settings];
    for width in [1360, 900, 640] {
        for sidebar in [true, false] {
            for theme in themes {
                for locale in locales {
                    for page in pages {
                        let mut model = WorkbenchModel::new(Shell::default());
                        model.set_width(width);
                        model.shell.sidebar_open = sidebar;
                        model.shell.theme = theme;
                        model.shell.page = page;
                        if locale == i18n::Locale::En {
                            model.shell.locale = Locale::En;
                        }
                        if page == AppPage::Workspace {
                            model.tabs.clear();
                        }
                        assert!(model.missing_copy(locale).is_empty(), "{:?} {:?}", page, model.missing_copy(locale));
                        assert_eq!(model.copy_overflow(locale), 0, "{width} {page:?} {locale:?}");
                        assert!(model.layout().content_width >= 160 || width < 160);
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
    assert!(model.request_update_install().is_err());
    assert!(!model.release_close);
    model.close_prompt = None;
    model.tabs[0].draft.clear();
    model.tabs[0].uncommitted_cells = true;
    assert!(model.request_update_install().unwrap_err().contains("pending"));
    model.tabs[0].uncommitted_cells = false;
    model.tabs[0].running = true;
    assert_eq!(model.request_update_install().unwrap_err(), "shell.update.busy");
    assert!(!model.allow_system_close());
    assert!(model.close_prompt.is_none());
}

#[tokio::test]
async fn seven_engines_round_trip_without_remote_sockets_and_history_does_not_run() {
    let dir = std::env::temp_dir().join(format!("rillsql-shell-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let services = services_at(&dir);
    let mut model = WorkbenchModel::new(Shell::default());
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
        let request = request_of(&mut model, token);
        let done = jobs::execute_job(&services, request).await;
        apply_done(&mut model, &services, "connections", done);
    }
    assert_eq!(model.connections.rows.len(), 7, "saved {:?}", model.connections.rows.iter().map(|row| row.saved.engine).collect::<Vec<_>>());
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
    let done = jobs::execute_job(&services, request_of(&mut model, token)).await;
    apply_done(&mut model, &services, "connections", done);
    assert_eq!(model.connections.rows.len(), 8);

    model.connections.selected = Some(sqlite_id.clone());
    let _ = model.dispatch("conn-connect");
    let token = model.prepare_list_token(&services.tasks);
    let done = jobs::execute_job(&services, request_of(&mut model, token)).await;
    apply_done(&mut model, &services, "connections", done);
    assert!(matches!(model.connections.status.get(&sqlite_id), Some(crate::app::views::connections::LinkStatus::Connected)), "{:?}", model.connections.message_detail);

    let _ = model.dispatch("conn-disconnect");
    let token = model.prepare_list_token(&services.tasks);
    let done = jobs::execute_job(&services, request_of(&mut model, token)).await;
    apply_done(&mut model, &services, "connections", done);
    assert!(matches!(model.connections.status.get(&sqlite_id), Some(crate::app::views::connections::LinkStatus::Disconnected)));
    let _ = model.dispatch("conn-test");
    let token = model.prepare_list_token(&services.tasks);
    let done = jobs::execute_job(&services, request_of(&mut model, token)).await;
    apply_done(&mut model, &services, "connections", done);
    assert_eq!(model.connections.message_key, Some("connections.editor.test.ok"), "{}", model.connections.message_detail);

    let export = dir.join("connections.json");
    model.export_path = Some(export.display().to_string());
    let _ = model.dispatch("conn-export");
    let token = model.prepare_list_token(&services.tasks);
    let done = jobs::execute_job(&services, request_of(&mut model, token)).await;
    apply_done(&mut model, &services, "connections", done);
    assert!(export.is_file());
    let blob = std::fs::read_to_string(&export).unwrap();
    assert!(!blob.contains("sshPassphrase"));
    assert!(!blob.to_ascii_lowercase().contains("\"password\""));

    model.connections.selected = Some(model.connections.rows[0].saved.id.clone());
    model.connections.confirm_delete = true;
    let _ = model.dispatch("conn-delete");
    let token = model.prepare_list_token(&services.tasks);
    let done = jobs::execute_job(&services, request_of(&mut model, token)).await;
    apply_done(&mut model, &services, "connections", done);
    assert_eq!(model.connections.rows.len(), 7);

    let fresh = services_at(&dir.join("import"));
    std::fs::create_dir_all(dir.join("import")).unwrap();
    let mut imported = WorkbenchModel::new(Shell::default());
    imported.import_path = Some(export.display().to_string());
    let _ = imported.dispatch("conn-import");
    let token = imported.prepare_list_token(&fresh.tasks);
    let done = jobs::execute_job(&fresh, request_of(&mut imported, token)).await;
    apply_done(&mut imported, &fresh, "connections", done);
    assert!(imported.connections.rows.len() >= 7, "{}", imported.connections.rows.len());

    let _ = model.dispatch("new-query");
    model.tabs[0].draft = "select 1".into();
    model.history.save_name = "计数".into();
    model.history.section = HistorySection::Saved;
    let before = model.executions;
    let _ = model.dispatch("history-save-named");
    let token = model.begin_owner(&services.tasks, "history");
    let done = jobs::execute_job(&services, request_of(&mut model, token)).await;
    apply_done(&mut model, &services, "history", done);
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

    model.settings.density = "relaxed".into();
    model.settings.theme = "dark".into();
    model.settings.locale = "en".into();
    model.settings.editor_font = 18;
    model.settings.editor_theme = "warm".into();
    model.settings.apply_shell(&mut model.shell);
    let state = model.snapshot();
    assert!(!serde_json::to_string(&state).unwrap().contains("\"password\""));
    super::model::persist_state(&services, &state).await.unwrap();
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
    cx.update_window(window.into(), |_, window, cx| {
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
            assert_eq!(view.model.copy_overflow(i18n::Locale::ZhCn), 0);
            assert_eq!(view.model.copy_overflow(i18n::Locale::En), 0);
        });
    });
    let _ = std::fs::remove_dir_all(&dir);
}
