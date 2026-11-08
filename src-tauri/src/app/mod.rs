mod files;
mod model;
pub mod shell;
pub mod views;

use gpui_kit::{
    App, AppContext as _, Bounds, Menu, MenuItem, Point, SharedString, TitlebarOptions,
    WindowBounds, WindowDecorations, WindowOptions, actions, point, px, size,
};

use gpui_kit::component::input;

use crate::ui::Foundation;

pub use files::{accept_drop_path, accept_open_path, accept_save_path};
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
        OpenStructure,
        CompareDatabases,
        NewConnection,
        OpenSqlite,
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
    gpui_platform::application().with_assets(gpui_kit::assets::AllAssets).run(move |cx| {
        gpui_kit::init(cx);
        bind_keys(cx);
        let (_window, root) = gpui_kit::open_window(window_options(), cx, |window, cx| {
            cx.new(|cx| Workbench::new(window, cx))
        })
        .expect("open GPUI window");
        let shell = root.read(cx).shell().clone();
        install_workbench_actions(cx, &root);
        install_menus(cx, &shell);
        if dialog {
            root.update(cx, |view, cx| view.perform("open-sqlite", cx));
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

fn window_options() -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin: Point::default(),
            size: size(px(1280.), px(820.)),
        })),
        // 顶栏自己画成标题栏：macOS 透明标题栏保留红绿灯，Windows 去掉系统标题栏，
        // Linux 用客户端装饰（GNOME Wayland 不提供服务端标题栏）。
        titlebar: Some(TitlebarOptions {
            title: Some(SharedString::from("RillSQL")),
            appears_transparent: true,
            traffic_light_position: Some(point(px(14.), px(15.))),
        }),
        app_owns_titlebar_drag: true,
        window_decorations: Some(WindowDecorations::Client),
        window_min_size: Some(size(px(640.), px(420.))),
        ..Default::default()
    }
}

pub(crate) fn bind_keys(cx: &mut App) {
    macro_rules! chord {
        ($key:literal) => {
            if cfg!(target_os = "macos") {
                concat!("cmd-", $key)
            } else {
                concat!("ctrl-", $key)
            }
        };
        (shift $key:literal) => {
            if cfg!(target_os = "macos") {
                concat!("cmd-shift-", $key)
            } else {
                concat!("ctrl-shift-", $key)
            }
        };
    }
    cx.bind_keys([
        gpui_kit::KeyBinding::new(chord!("k"), CommandPalette, None),
        gpui_kit::KeyBinding::new(chord!("p"), QuickOpen, None),
        gpui_kit::KeyBinding::new(chord!("t"), NewQuery, None),
        gpui_kit::KeyBinding::new(chord!("w"), CloseTab, None),
        gpui_kit::KeyBinding::new(chord!("b"), ToggleSidebar, None),
        gpui_kit::KeyBinding::new("ctrl-tab", NextTab, None),
        gpui_kit::KeyBinding::new("ctrl-shift-tab", PrevTab, None),
        gpui_kit::KeyBinding::new(chord!(","), OpenSettings, None),
        gpui_kit::KeyBinding::new(chord!("enter"), ExecuteQuery, None),
        gpui_kit::KeyBinding::new(chord!(shift "enter"), ExecuteCurrent, None),
        gpui_kit::KeyBinding::new(chord!("."), RequestCancel, None),
        gpui_kit::KeyBinding::new("ctrl-space", ShowAutocomplete, None),
        gpui_kit::KeyBinding::new(chord!("/"), ToggleComment, None),
        gpui_kit::KeyBinding::new(chord!("j"), ToggleResults, None),
        gpui_kit::KeyBinding::new(chord!(shift "f"), FormatSql, None),
        gpui_kit::KeyBinding::new(chord!(shift "e"), ExplainSql, None),
        gpui_kit::KeyBinding::new(chord!("s"), CommitEdit, None),
        gpui_kit::KeyBinding::new("escape", Dismiss, Some("Palette")),
    ]);
    #[cfg(target_os = "macos")]
    cx.bind_keys(editor_format_bindings());
    // gpui-base 只给 Windows/Linux 绑了 Ctrl+Y 重做；补上常见的 Ctrl+Shift+Z。
    #[cfg(not(target_os = "macos"))]
    cx.bind_keys([gpui_kit::KeyBinding::new("ctrl-shift-z", input::Redo, Some("Input"))]);
}

/// gpui-base 在 macOS 编辑器里把 ⌘⇧F 绑成“替换”，盖住了格式化 SQL。
/// 同一上下文里后注册的绑定优先，这里把 ⌘⇧F 还给格式化，替换改用 ⌥⌘F。
#[cfg(any(target_os = "macos", test))]
pub(crate) fn editor_format_bindings() -> Vec<gpui_kit::KeyBinding> {
    const EDITOR: &str = "Input && mode == editor";
    vec![
        gpui_kit::KeyBinding::new("cmd-shift-f", FormatSql, Some(EDITOR)),
        gpui_kit::KeyBinding::new("cmd-alt-f", input::Replace, Some(EDITOR)),
    ]
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
    bind!(OpenStructure, "open-structure");
    bind!(CompareDatabases, "compare");
    bind!(NewConnection, "new-connection");
    bind!(OpenSqlite, "open-sqlite");
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
    bind!(OpenStructure, "open-structure");
    bind!(CompareDatabases, "compare");
    bind!(NewConnection, "new-connection");
    bind!(OpenSqlite, "open-sqlite");
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
                let effect = view.model.dismiss();
                view.apply_effect(effect, cx);
                cx.notify();
            });
        });
    }
    let root = root.clone();
    cx.on_action(move |_: &Quit, cx: &mut App| {
        root.update(cx, |view, cx| view.perform("quit-window", cx));
    });
}

pub fn install_menus(cx: &mut App, shell: &Shell) {
    let label = |key: &'static str| shell.text(key).to_string();
    cx.set_menus([
        Menu::new(label("shell.menu.file")).items([
            MenuItem::action(label("shell.command.newQuery"), NewQuery),
            MenuItem::action(label("shell.command.openSqlite"), OpenSqlite),
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


