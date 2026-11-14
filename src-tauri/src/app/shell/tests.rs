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
    assert_eq!(sql, "SELECT \"id\" FROM \"main\".\"authors\" WHERE \"id\" = 1", "单键定位查询");
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
        Some("SELECT \"id\" FROM \"main\".\"authors\" WHERE \"id\" = 1"),
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
        view.update(cx, |view, _| {
            assert!(view.model.activate_item(&query));
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
    assert!(entries.iter().all(|entry| entry.group == "table"), "命令和连接不应混入快速打开");
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
    assert_eq!(all.iter().filter(|entry| entry.group == "table").count(), 2);
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
    let services = rx.recv_timeout(std::time::Duration::from_secs(30)).expect("sqlite setup");
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
            assert_eq!(report.group, "table");
            assert!(entries.iter().all(|entry| entry.group == "table"), "快速打开只含表 / 视图");
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
    model.history.selected_saved = Some(remove);
    let _ = model.dispatch("saved-delete");
    assert!(model.history.confirm_delete);
    assert_eq!(model.pending_len(), 0);
    assert_eq!(model.history.saved.len(), 2);
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
        view.update(cx, |view, cx| view.perform("settings", cx));
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
    assert!(model.dispatch("check-updates").is_some());
    assert_eq!(model.update_checks, 0, "命令入口也必须读取开关");
    model.settings.auto_check_updates = true;
    assert!(model.begin_update_check());
    assert_eq!(model.update_checks, 1);
    assert!(model.dispatch("check-updates").is_some());
    assert_eq!(model.update_checks, 2);
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
        assert_eq!(query.object_page(), 150);
        assert_eq!(query.null_copy(), "N", "QueryPane 保留原始设置，由网格转换显示文本");
        assert_eq!(query.run_limit(), 150, "查询读取行数跟随 page_size");
    });

    // 关掉自动检查后，启动路径和命令入口都不再发起检查。
    cx.update(|cx| {
        view.update(cx, |view, cx| {
            view.model.settings.auto_check_updates = false;
            view.perform("check-updates", cx);
        });
    });
    cx.update(|cx| assert_eq!(view.read(cx).model.update_checks, 0));
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
        assert!(ids.contains(&"compare-grid"), "{ids:?}");
        assert_eq!(menu.place, MenuPlace::Grid);
        assert!(!ids.contains(&"compare-object"), "网格菜单不能再复用对象树的范围动作：{ids:?}");
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
    assert_eq!(tab.draft, "\"public\".\"orders\"");
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
    assert!(tab.draft.contains("FROM \"public\".\"orders\""));
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
    let services = rx.recv_timeout(std::time::Duration::from_secs(30)).expect("sqlite setup");
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
    let services = rx.recv_timeout(std::time::Duration::from_secs(30)).expect("sqlite setup");
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
        vec!["tree-new-query", "tree-copy-name", "tree-refresh-node", "tree-create-table", "open-admin"],
        "范围行只有自己的动作，外加新建表和管理页"
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
    assert!(crate::app::shell::menus::flatten_items(&model.menu.as_ref().unwrap().items).iter().any(|item| item.id == "tree-insert-select"));
    let _ = model.dispatch("tree-copy-name");
    assert_eq!(model.copied, "public.orders");
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

            for id in ["toolbar", "activity", "workbench-content", "status", "top-new-query", "status-locale"] {
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
    assert_eq!(tab.draft, "UPDATE \"public\".\"orders\"\nSET \"label\" = ?\nWHERE \"id\" = ?;");
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
    assert!(draft.ends_with("INSERT INTO \"public\".\"orders\" (\"id\", \"label\")\nVALUES (?, ?);"), "{draft}");
    assert!(model.open_nav_menu(&orders));
    let _ = model.dispatch("tree-template-delete");
    assert!(model.active_tab().unwrap().draft.ends_with("DELETE FROM \"public\".\"orders\"\nWHERE \"id\" = ?;"));
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
    let delete = "DELETE FROM \"public\".\"orders\"\nWHERE \"id\" = ?;";
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

    // 扩展读不到定义：菜单项禁用，直接派发也不开查看器。
    let extension = ObjectRow::fresh("pgcrypto", ObjectKind::Extension);
    let pg = nav_connection(Engine::Postgres, "pg", "销售库", vec![nav_database("shop", vec![extension.clone()])]);
    let mut ext = WorkbenchModel::new(Shell::default());
    ext.apply_navigation(&[pg], false);
    let id = nav_id("pg", "shop", "public", &extension);
    assert!(ext.open_nav_menu(&id));
    let item = ext.menu.as_ref().unwrap().items.iter().find(|item| item.id == "tree-view-ddl").cloned().expect("DDL 项");
    assert!(!item.enabled);
    assert_eq!(item.reason_key, Some("catalog.tree.noDefinition"));
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
    assert_eq!(model.menu.as_ref().unwrap().current().map(|item| item.id), Some("tree-template-insert"));
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
        assert!(draft.name.ends_with(" copy"));
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
    let rows = rx.recv_timeout(std::time::Duration::from_secs(30)).expect("fixture setup");
    let (window, view) = open_workbench(cx, Arc::clone(&services), false);
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, _| {
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
    cx.update(|cx| {
        view.update(cx, |view, cx| {
            view.model.connections.rows = vec![record_for("pg", "Atlas", Engine::Postgres)];
            view.model.apply_groups(vec![group("g1", "生产库", 0, false)], Default::default());
            view.model.shell.sidebar_open = true;
            cx.notify();
        });
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.drag_to("side-pg", "side-group-g1", cx);
        window.render_frame(cx);
    })
    .unwrap();
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
