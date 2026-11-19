mod files;
pub mod keymap;
mod model;
pub mod shell;
pub mod views;

use gpui_kit::{
    App, AppContext as _, Bounds, Menu, MenuItem, Pixels, SharedString, TitlebarOptions,
    WindowBounds, WindowDecorations, WindowOptions, actions, point, px, size,
};

use gpui_kit::component::input;

use crate::ui::Foundation;

pub use files::{accept_drop_path, accept_open_path, accept_save_path, read_file_header, route_open_path, OpenRoute};
pub use model::{
    accept_versioned, dictionary_keys, shared_controls, translate, AppPage, CommandSpec, Density, Effect, FilePrompt, FocusTarget, Locale,
    ResolvedTheme, Shell, ThemePreference, COMMANDS,
};
pub use shell::Workbench;

pub const GPUI_KIT_VERSION: &str = "0.9.0";
pub const GPUI_PRE_VERSION: &str = "0.3.11";

actions!(
    rillsql_shell,
    [
        Quit,
        CommandPalette,
        QuickOpen,
        NewQuery,
        ExecuteQuery,
        ExecuteCurrent,
        ExecuteScript,
        OpenStructure,
        CompareDatabases,
        NewConnection,
        OpenFile,
        ShowHistory,
        SaveQuery,
        SavedQueries,
        ShowConnections,
        OpenSettings,
        ToggleSidebar,
        ThemeLight,
        ThemeDark,
        ThemeSystem,
        ShowHelp,
        CloseTab,
        NextTab,
        PrevTab,
        RequestCancel,
        ShowAutocomplete,
        ToggleComment,
        ToggleResults,
        FormatSql,
        ExplainSql,
        LeaveEditor,
        SelectAllCells,
        EditCell,
        CancelEdit,
        MarkDelete,
        CommitEdit,
        CopyRegion,
        CopySqlValue,
        Dismiss,
    ]
);

pub fn run() {
    let smoke = std::env::args().any(|arg| arg == "--gpui-smoke");
    let dialog = std::env::args().any(|arg| arg == "--gpui-native-dialog");
    println!(
        "gpui-window: starting kit={GPUI_KIT_VERSION} pre={GPUI_PRE_VERSION} os={} arch={}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    // gpui_platform is the 0.3.11 snapshot (Win32/DirectWrite, X11/Wayland, macOS font-kit).
    let application = gpui_platform::application().with_assets(gpui_kit::assets::AllAssets);
    application.on_reopen(|cx| reopen_workbench(cx, |cx| { open_workbench_window(cx); }));
    application.run(move |cx| {
        gpui_kit::init(cx);
        bind_keys(cx);
        let root = open_workbench_window(cx);
        if dialog {
            // 原生文件对话框冒烟：走唯一的“打开文件”入口。
            root.update(cx, |view, cx| view.perform("open-file", cx));
        }
        if smoke || dialog {
            cx.spawn(async move |cx| {
                cx.update(|cx| cx.quit());
            })
            .detach();
        }
    });
    println!("gpui-window: stopped os={}", std::env::consts::OS);
}

struct WorkbenchServices(std::sync::Arc<crate::application::AppServices>);
impl gpui_kit::Global for WorkbenchServices {}

fn open_workbench_window(cx: &mut App) -> gpui_kit::Entity<Workbench> {
    if !cx.has_global::<WorkbenchServices>() {
        cx.set_global(WorkbenchServices(std::sync::Arc::new(crate::application::AppServices::from_state(&crate::connections::commands::ConnectionsState::new()))));
    }
    let services = std::sync::Arc::clone(&cx.global::<WorkbenchServices>().0);
    let options = window_options(cx);
    let (window, root) = gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| Workbench::with_services(window, cx, services, true))
    }).expect("open GPUI window");
    let shell = root.read(cx).shell().clone();
    install_workbench_actions(cx, &root);
    install_menus(cx, &shell);
    let _ = window.update(cx, |_, window, _| window.activate_window());
    cx.activate(true);
    root
}

/// LaunchServices / Dock 再次打开：已有窗口直接激活，关闭了最后一个窗口则重新创建。
pub(crate) fn reopen_workbench(cx: &mut App, open: impl FnOnce(&mut App)) {
    if let Some(window) = cx.windows().first().copied() {
        let _ = window.update(cx, |_, window, _| window.activate_window());
    } else {
        open(cx);
    }
    cx.activate(true);
}

struct WorkbenchActionTarget(gpui_kit::WeakEntity<Workbench>);
impl gpui_kit::Global for WorkbenchActionTarget {}

const WORKBENCH_DEFAULT_WIDTH: f32 = 1280.0;
const WORKBENCH_DEFAULT_HEIGHT: f32 = 820.0;
const WORKBENCH_MIN_WIDTH: f32 = 640.0;
const WORKBENCH_MIN_HEIGHT: f32 = 420.0;
const WORKBENCH_WORK_AREA_MARGIN: f32 = 24.0;

/// 根据显示器的可见工作区（扣除任务栏 / 程序坞）计算居中的初始窗口矩形。
/// 在 125% / 150% 高分屏笔记本上自动留出边距并收敛尺寸，避免窗口贴死在左上角 (0, 0)。
pub(crate) fn centered_workbench_bounds(visible_bounds: Option<Bounds<Pixels>>) -> Bounds<Pixels> {
    let default_size = size(px(WORKBENCH_DEFAULT_WIDTH), px(WORKBENCH_DEFAULT_HEIGHT));
    let Some(visible) = visible_bounds else {
        return Bounds {
            origin: point(px(80.), px(60.)),
            size: default_size,
        };
    };
    let avail_w = visible.size.width.as_f32();
    let avail_h = visible.size.height.as_f32();
    if avail_w < WORKBENCH_MIN_WIDTH * 0.5 || avail_h < WORKBENCH_MIN_HEIGHT * 0.5 {
        return Bounds {
            origin: point(px(80.), px(60.)),
            size: default_size,
        };
    }

    let max_w = (avail_w - WORKBENCH_WORK_AREA_MARGIN * 2.0).max(WORKBENCH_MIN_WIDTH.min(avail_w));
    let max_h = (avail_h - WORKBENCH_WORK_AREA_MARGIN * 2.0).max(WORKBENCH_MIN_HEIGHT.min(avail_h));
    let width = WORKBENCH_DEFAULT_WIDTH.min(max_w).round();
    let height = WORKBENCH_DEFAULT_HEIGHT.min(max_h).round();

    let origin_x = visible.origin.x.as_f32() + ((avail_w - width) * 0.5).max(0.0).round();
    let origin_y = visible.origin.y.as_f32() + ((avail_h - height) * 0.5).max(0.0).round();

    Bounds {
        origin: point(px(origin_x), px(origin_y)),
        size: size(px(width), px(height)),
    }
}

fn window_options(cx: &App) -> WindowOptions {
    let display = cx.primary_display().or_else(|| cx.displays().into_iter().next());
    let bounds = centered_workbench_bounds(display.as_ref().map(|d| d.visible_bounds()));
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        display_id: display.as_ref().map(|d| d.id()),
        // 顶栏自己画成标题栏：macOS 透明标题栏保留红绿灯，Windows 去掉系统标题栏，
        // Linux 用客户端装饰（GNOME Wayland 不提供服务端标题栏）。
        titlebar: Some(TitlebarOptions {
            title: Some(SharedString::from("RillSQL")),
            appears_transparent: true,
            traffic_light_position: Some(point(px(14.), px(15.))),
        }),
        app_owns_titlebar_drag: true,
        window_decorations: Some(WindowDecorations::Client),
        window_min_size: Some(size(px(WORKBENCH_MIN_WIDTH), px(WORKBENCH_MIN_HEIGHT))),
        ..Default::default()
    }
}

/// 全局按键绑定全部来自快捷键注册表（`keymap::SHORTCUTS`），这里不另外手写。
pub(crate) fn bind_keys(cx: &mut App) {
    cx.bind_keys(keymap::key_bindings());
}

/// gpui-base 在 macOS 编辑器里把 ⌘⇧F 绑成“替换”，盖住了格式化 SQL。
/// 同一上下文里后注册的绑定优先：注册表的 macOS 绑定把 ⌘⇧F 还给格式化，替换改用 ⌥⌘F。
#[cfg(test)]
pub(crate) fn editor_format_bindings() -> Vec<gpui_kit::KeyBinding> {
    ["format", "replace"]
        .into_iter()
        .filter_map(keymap::shortcut)
        .flat_map(|shortcut| keymap::bindings_of(shortcut, true))
        .collect()
}

pub fn install_actions(cx: &mut App, root: &gpui_kit::Entity<Foundation>) {
    macro_rules! bind {
        ($action:ty, $id:expr) => {{
            let root = root.clone();
            cx.on_action(move |_: &$action, cx: &mut App| {
                root.update(cx, |view, cx| view.perform($id, cx));
            });
        }};
    }
    bind!(CommandPalette, "command-palette");
    bind!(QuickOpen, "quick-open");
    bind!(NewQuery, "new-query");
    bind!(ExecuteQuery, "execute-query");
    bind!(ExecuteCurrent, "run-current");
    bind!(ExecuteScript, "run-script");
    bind!(OpenStructure, "open-structure");
    bind!(CompareDatabases, "compare");
    bind!(NewConnection, "new-connection");
    bind!(OpenFile, "open-file");
    bind!(ShowHistory, "history");
    bind!(SaveQuery, "save-query");
    bind!(SavedQueries, "saved-queries");
    bind!(ShowConnections, "connections");
    bind!(OpenSettings, "settings");
    bind!(ToggleSidebar, "sidebar");
    bind!(ThemeLight, "theme-light");
    bind!(ThemeDark, "theme-dark");
    bind!(ThemeSystem, "theme-system");
    bind!(ShowHelp, "help");
    bind!(CloseTab, "close-tab");
    bind!(NextTab, "next-tab");
    bind!(PrevTab, "prev-tab");
    bind!(RequestCancel, "request-cancel");
    bind!(ShowAutocomplete, "autocomplete");
    bind!(ToggleComment, "toggle-comment");
    bind!(ToggleResults, "toggle-results");
    bind!(FormatSql, "format");
    bind!(ExplainSql, "explain");
    bind!(LeaveEditor, "leave-editor");
    bind!(SelectAllCells, "select-all-cells");
    bind!(EditCell, "edit-cell");
    bind!(CancelEdit, "cancel-edit");
    bind!(MarkDelete, "mark-delete");
    bind!(CommitEdit, "commit");
    bind!(CopyRegion, "copy-region");
    bind!(CopySqlValue, "copy-sql-value");
    {
        let root = root.clone();
        cx.on_action(move |_: &Dismiss, cx: &mut App| {
            root.update(cx, |view, cx| {
                view.shell_mut().close_overlay();
                cx.notify();
            });
        });
    }
    cx.on_action(|_: &Quit, cx: &mut App| {
        cx.quit();
    });
}

pub fn install_workbench_actions(cx: &mut App, root: &gpui_kit::Entity<Workbench>) {
    let installed = cx.has_global::<WorkbenchActionTarget>();
    cx.set_global(WorkbenchActionTarget(root.downgrade()));
    if installed { return; }
    macro_rules! bind {
        ($action:ty, $id:expr) => {{
            cx.on_action(move |_: &$action, cx: &mut App| {
                let root = match cx.global::<WorkbenchActionTarget>().0.upgrade() {
                    Some(root) => Some(root),
                    // 窗口已经关掉、应用还在程序坞里：程序坞菜单里的“新建查询 / 新建连接”等
                    // 先把工作台窗口开回来再执行，而不是点了没反应。
                    None if DOCK_COMMANDS.contains(&$id) => Some(open_workbench_window(cx)),
                    None => None,
                };
                if let Some(root) = root {
                    root.update(cx, |view, cx| {
                        // 连接模态打开时，快捷键不能在背景里换页、开标签或执行查询；
                        // 模态自己需要的 Esc/Tab/回车不走全局动作。
                        if view.model.action_blocked_by_modal($id) {
                            // 连接表单里的 Cmd/Ctrl+Enter 是“保存并连接”：不必先把手移到页脚按钮上。
                            if $id == "execute-query" {
                                view.perform("conn-save-connect", cx);
                            }
                            cx.notify();
                            return;
                        }
                        view.perform($id, cx);
                    });
                }
            });
        }};
    }
    bind!(CommandPalette, "command-palette");
    bind!(QuickOpen, "quick-open");
    bind!(NewQuery, "new-query");
    bind!(ExecuteQuery, "execute-query");
    bind!(ExecuteCurrent, "run-current");
    bind!(ExecuteScript, "run-script");
    bind!(OpenStructure, "open-structure");
    bind!(CompareDatabases, "compare");
    bind!(NewConnection, "new-connection");
    bind!(OpenFile, "open-file");
    bind!(ShowHistory, "history");
    bind!(SaveQuery, "save-query");
    bind!(SavedQueries, "saved-queries");
    bind!(ShowConnections, "connections");
    bind!(OpenSettings, "settings");
    bind!(ToggleSidebar, "sidebar");
    bind!(ThemeLight, "theme-light");
    bind!(ThemeDark, "theme-dark");
    bind!(ThemeSystem, "theme-system");
    bind!(ShowHelp, "help");
    bind!(CloseTab, "close-tab");
    bind!(NextTab, "next-tab");
    bind!(PrevTab, "prev-tab");
    bind!(RequestCancel, "request-cancel");
    bind!(ShowAutocomplete, "autocomplete");
    bind!(ToggleComment, "toggle-comment");
    bind!(ToggleResults, "toggle-results");
    bind!(FormatSql, "format");
    bind!(ExplainSql, "explain");
    bind!(LeaveEditor, "leave-editor");
    bind!(SelectAllCells, "select-all-cells");
    bind!(EditCell, "edit-cell");
    bind!(CancelEdit, "cancel-edit");
    bind!(MarkDelete, "mark-delete");
    bind!(CommitEdit, "commit");
    bind!(CopyRegion, "copy-region");
    bind!(CopySqlValue, "copy-sql-value");
    {
        cx.on_action(move |_: &Dismiss, cx: &mut App| {
            let Some(root) = cx.global::<WorkbenchActionTarget>().0.upgrade() else { return };
            root.update(cx, |view, cx| {
                let effect = view.model.dismiss();
                view.apply_effect(effect, cx);
                cx.notify();
            });
        });
    }
    cx.on_action(move |_: &Quit, cx: &mut App| {
        if let Some(root) = cx.global::<WorkbenchActionTarget>().0.upgrade() {
            root.update(cx, |view, cx| view.perform("quit-window", cx));
        } else {
            cx.quit();
        }
    });
}

/// 程序坞菜单里的命令：应用没有窗口时也能直接用它们把工作台带回来。
const DOCK_COMMANDS: [&str; 5] = ["new-query", "open-file", "new-connection", "connections", "history"];

/// macOS 程序坞图标的右键菜单：常用入口一步直达。
fn install_dock_menu(cx: &mut App, shell: &Shell) {
    let label = |key: &'static str| shell.text(key).to_string();
    cx.set_dock_menu(vec![
        MenuItem::action(label("shell.command.newQuery"), NewQuery),
        MenuItem::action(label("shell.command.openFile"), OpenFile),
        MenuItem::action(label("shell.command.newConnection"), NewConnection),
        MenuItem::separator(),
        MenuItem::action(label("shell.command.connections"), ShowConnections),
        MenuItem::action(label("shell.command.history"), ShowHistory),
    ]);
}

pub fn install_menus(cx: &mut App, shell: &Shell) {
    install_dock_menu(cx, shell);
    let label = |key: &'static str| shell.text(key).to_string();
    cx.set_menus([
        Menu::new(label("shell.menu.file")).items([
            MenuItem::action(label("shell.command.newQuery"), NewQuery),
            MenuItem::action(label("shell.command.openFile"), OpenFile),
            MenuItem::action(label("shell.command.saveQuery"), SaveQuery),
            MenuItem::separator(),
            MenuItem::action(label("shell.command.quit"), Quit),
        ]),
        Menu::new(label("shell.menu.edit")).items([
            MenuItem::action(label("shell.shortcuts.undo"), input::Undo),
            MenuItem::action(label("shell.shortcuts.redo"), input::Redo),
            MenuItem::separator(),
            MenuItem::action(label("shell.command.cut"), input::Cut),
            MenuItem::action(label("shell.command.copy"), input::Copy),
            MenuItem::action(label("shell.command.paste"), input::Paste),
            MenuItem::action(label("shell.command.selectAll"), input::SelectAll),
            MenuItem::separator(),
            MenuItem::action(label("shell.shortcuts.find"), input::Search),
            MenuItem::action(label("shell.shortcuts.replace"), input::Replace),
            MenuItem::separator(),
            MenuItem::action(label("shell.shortcuts.format"), FormatSql),
            MenuItem::action(label("shell.shortcuts.autocomplete"), ShowAutocomplete),
        ]),
        Menu::new(label("shell.menu.view")).items([
            MenuItem::action(label("shell.command.toggleSidebar"), ToggleSidebar),
            MenuItem::action(label("shell.command.themeLight"), ThemeLight),
            MenuItem::action(label("shell.command.themeDark"), ThemeDark),
            MenuItem::action(label("shell.command.themeSystem"), ThemeSystem),
            MenuItem::action(label("shell.command.settings"), OpenSettings),
            MenuItem::action(label("shell.command.help"), ShowHelp),
        ]),
        Menu::new(label("shell.menu.query")).items([
            MenuItem::action(label("shell.command.execute"), ExecuteQuery),
            MenuItem::action(label("shell.shortcuts.runCurrent"), ExecuteCurrent),
            MenuItem::action(label("shell.shortcuts.runScript"), ExecuteScript),
            MenuItem::action(label("shell.shortcuts.requestCancel"), RequestCancel),
            MenuItem::separator(),
            MenuItem::action(label("shell.command.history"), ShowHistory),
            MenuItem::action(label("shell.command.savedQueries"), SavedQueries),
            MenuItem::action(label("shell.command.connections"), ShowConnections),
            MenuItem::action(label("shell.command.newConnection"), NewConnection),
            MenuItem::action(label("shell.command.newStructure"), OpenStructure),
            MenuItem::action(label("shell.command.compare"), CompareDatabases),
        ]),
    ]);
}

#[cfg(test)]
mod window_bounds_tests {
    use super::*;

    #[test]
    fn centers_workbench_in_standard_and_hidpi_work_areas_instead_of_top_left_zero() {
        // 1920x1080 @ 100%（底部任务栏 48px）
        let fhd = centered_workbench_bounds(Some(Bounds {
            origin: point(px(0.), px(0.)),
            size: size(px(1920.), px(1032.)),
        }));
        assert_eq!(fhd.origin, point(px(320.), px(106.)));
        assert_eq!(fhd.size, size(px(1280.), px(820.)));

        // 1920x1080 @ 150% 笔记本逻辑工作区（1280x672）：留出四周间距并居中，绝不贴死在 (0, 0)
        let hidpi_150 = centered_workbench_bounds(Some(Bounds {
            origin: point(px(0.), px(0.)),
            size: size(px(1280.), px(672.)),
        }));
        assert_eq!(hidpi_150.origin, point(px(24.), px(24.)));
        assert_eq!(hidpi_150.size, size(px(1232.), px(624.)));

        // 顶部/左侧任务栏或副屏负坐标也能准确落在可见工作区中心
        let offset_display = centered_workbench_bounds(Some(Bounds {
            origin: point(px(-1920.), px(40.)),
            size: size(px(1920.), px(1040.)),
        }));
        assert_eq!(offset_display.origin, point(px(-1600.), px(150.)));
        assert_eq!(offset_display.size, size(px(1280.), px(820.)));

        // 无显示器回退也不落回 (0, 0)
        let fallback = centered_workbench_bounds(None);
        assert_eq!(fallback.origin, point(px(80.), px(60.)));
        assert_eq!(fallback.size, size(px(1280.), px(820.)));
    }
}


