//! 外壳、连接、历史、设置和关闭守卫。不连接远程数据库。

use std::sync::Arc;

use crate::app::shell::jobs::{self, apply_done, job_request};
use crate::app::shell::menus::{menu_items, MenuPlace, MenuTarget, NavKey};
use crate::app::views::connections::{sample_draft, with_tls_ssh, Secrets};
use crate::app::views::history::HistorySection;
use crate::app::{dictionary_keys, translate, AppPage, Locale, ThemePreference};
use crate::application::AppServices;
use crate::connections::model::{CaMode, Engine, SshAuthKind};
use crate::connections::Service;
use crate::i18n;
use crate::query::SaveRequest;
use crate::state::{AppState, SessionState, ShellSettings};

use super::model::{DraftField, PendingJob, WorkItemKind, WorkTool, WorkbenchModel};
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
fn tab_context_menu_targets_the_activated_item() {
    let mut model = WorkbenchModel::new(Shell::default());
    let customers = "notes|笔记|main||customers|sqlite|dev";
    assert!(model.dispatch(&format!("open-table:{customers}")).is_some());
    let table_id = model.active_tab.clone().expect("table");
    assert!(model.dispatch(&format!("open-structure-for:{customers}")).is_some());
    assert_eq!(model.tool, WorkTool::Structure);

    assert!(model.tab_menu_for(&table_id));
    assert_eq!(model.active_tab.as_deref(), Some(table_id.as_str()));
    assert_eq!(model.tool, WorkTool::Table);
    assert_eq!(model.shell.page, AppPage::Workspace);
    assert_eq!(model.table_spec.as_deref(), Some(customers));
    assert!(model.menu.is_some(), "右键应打开以当前项为基准的标签菜单");
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
    let done = jobs::execute_job(&services, request_of(&mut model, token)).await;
    apply_done(&mut model, &services, "connections", done);
    assert_eq!(model.connections.rows.len(), before_delete - 1);

    let broken = dir.join("broken.json");
    std::fs::write(&broken, b"{").unwrap();
    let kept = model.connections.rows.len();
    model.import_path = Some(broken.display().to_string());
    model.bundle_passphrase.clear();
    let _ = model.dispatch("conn-import");
    let token = model.prepare_list_token(&services.tasks);
    let done = jobs::execute_job(&services, request_of(&mut model, token)).await;
    apply_done(&mut model, &services, "connections", done);
    assert_eq!(model.connections.rows.len(), kept);
    assert_eq!(services.list_connections().await.unwrap().len(), kept);
    let skip_file = dir.join("skip.json");
    std::fs::write(&skip_file, r#"{"format":"rillsql-connections","version":1,"connections":[{"name":"跳过我","engine":"oracle"}]}"#).unwrap();
    model.import_path = Some(skip_file.display().to_string());
    let _ = model.dispatch("conn-import");
    let token = model.prepare_list_token(&services.tasks);
    let done = jobs::execute_job(&services, request_of(&mut model, token)).await;
    apply_done(&mut model, &services, "connections", done);
    assert_eq!(model.connections.rows.len(), kept);
    assert!(!model.import_notes.is_empty(), "{:?}", model.import_notes);
    assert_eq!(model.notice_key, Some("connections.importReport.skippedTitle"));

    model.bundle_passphrase = "correct-horse".into();
    let secret_export = dir.join("connections.rillsqlcx");
    model.export_path = Some(secret_export.display().to_string());
    let _ = model.dispatch("conn-export");
    let token = model.prepare_list_token(&services.tasks);
    let done = jobs::execute_job(&services, request_of(&mut model, token)).await;
    apply_done(&mut model, &services, "connections", done);
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
            assert!(frame.toolbar_wrap);
            assert!(frame.occupied_width() <= 640);
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
        view.update(cx, |view, cx| view.perform("conn-import", cx));
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
            assert_eq!(request.bundle_passphrase.as_deref(), Some("correct-horse"));
            assert_eq!(request.import_path.as_deref(), Some(path.as_str()));
        });
    });

    let sqlite = dir.join("open.sqlite");
    std::fs::write(&sqlite, b"sqlite").unwrap();
    let sqlite_opened = sqlite.canonicalize().unwrap();
    cx.update_window(window, |_, _, cx| {
        view.update(cx, |view, cx| view.perform("open-sqlite", cx));
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
            view.perform("object:notes:customers", cx);
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
    pump_exchange(cx, window, &view, |_, _| loaded_path.exists());
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
        pump_for(cx, window, &view, std::time::Duration::from_secs(8), |view, app| {
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
