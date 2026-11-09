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

    // 连接表单在 640 宽度下也要能看到并点到测试/保存。
    cx.update(|cx| view.update(cx, |view, cx| view.perform("new-connection", cx)));
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let width = f32::from(window.find("workbench").bounds().size.width);
        for id in ["conn-test", "conn-save"] {
            let button = window.find(id);
            assert!(button.visible(), "{id} 应可见");
            assert!(f32::from(button.bounds().origin.x + button.bounds().size.width) <= width + 0.5);
            assert!(f32::from(button.bounds().origin.y + button.bounds().size.height) <= 901.);
        }
    })
    .unwrap();

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
        assert!(f32::from(palette.size.width) <= 560.);
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
    pump_exchange(cx, window, &view, |_, _| path.exists());
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
            assert_eq!(view.model.painted().activity_width, 40, "窄窗口活动栏更窄");
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
fn object_search_uses_complete_path_and_clearing_restores_the_tree() {
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
    let hits = model.nav_rows();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].label.contains("shop") && hits[0].label.contains("public"), "完整路径：{}", hits[0].label);
    assert!(hits[0].highlight.is_some(), "命中要高亮");
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
    let (window, view) = open_workbench(cx, services, boot);
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

    // 双击对象种类分组展开，表对象行真的出现在渲染结果里。
    cx.update_window(window, |_, window, cx| {
        window.double_click(SharedString::from(format!("nav-{group}")), cx);
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
    assert_eq!(ids, vec!["tree-new-query", "tree-copy-name"], "范围行只有自己的动作");
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
    assert!(model.menu.as_ref().unwrap().items.iter().any(|item| item.id == "tree-insert-select"));
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
    let items = model.menu.as_ref().expect("菜单").items.clone();
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
