//! Root GPUI view: shell controls, SQL editor prototype, virtual grid, files.
#![cfg_attr(not(test), allow(dead_code, unused_imports))]

use gpui_kit::component::{ActiveTheme as _, Theme, ThemeMode};
use gpui_kit::{
    App, AppContext as _, Context, Entity, ExternalPaths, FocusHandle, Focusable, IntoElement,
    ParentElement as _, PathPromptOptions, Render, SharedString, Styled as _, Window,
    WindowAppearance, div, prelude::*, px,
};

use crate::app::{
    accept_drop_path, accept_open_path, accept_save_path, install_menus, shared_controls, AppPage,
    Density, Effect, FilePrompt, FocusTarget, Locale, ResolvedTheme, Shell, ThemePreference,
};
use crate::ui::{GridFixture, GridPrototype, SqlEditor};

pub struct Foundation {
    shell: Shell,
    focus: FocusHandle,
    editor: Entity<SqlEditor>,
    read_only_grid: Entity<GridPrototype>,
    editable_grid: Entity<GridPrototype>,
    show_readonly_grid: bool,
    /// 独立窗口保留原型工具条。外壳嵌入时关掉，避免两套工具条。
    pub show_chrome: bool,
    status: SharedString,
    pending: Option<Effect>,
}

impl Foundation {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut shell = Shell::default();
        let _ = shell.set_prefers_dark(matches!(
            window.appearance(),
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        ));
        apply_theme(&shell, cx);
        Self {
            shell,
            focus: cx.focus_handle(),
            editor: cx.new(|cx| SqlEditor::new(window, cx)),
            read_only_grid: cx.new(|cx| GridPrototype::new(GridFixture::ReadOnly, window, cx)),
            editable_grid: cx.new(|cx| GridPrototype::new(GridFixture::Editable, window, cx)),
            show_readonly_grid: false,
            show_chrome: true,
            status: SharedString::from("GPUI"),
            pending: None,
        }
    }

    /// 外壳持有命令状态。这里只接收已经分发过的效果，不再次 dispatch。
    pub fn adopt(&mut self, shell: Shell, effect: Effect, cx: &mut Context<Self>) {
        self.shell = shell;
        self.queue(effect, cx);
    }

    pub fn shell(&self) -> &Shell {
        &self.shell
    }

    pub fn shell_mut(&mut self) -> &mut Shell {
        &mut self.shell
    }

    pub fn editor(&self) -> &Entity<SqlEditor> {
        &self.editor
    }

    pub fn grid(&self, readonly: bool) -> &Entity<GridPrototype> {
        if readonly {
            &self.read_only_grid
        } else {
            &self.editable_grid
        }
    }

    pub fn perform(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(effect) = self.shell.dispatch(id) else {
            return;
        };
        self.queue(effect, cx);
    }

    pub fn set_locale(&mut self, locale: Locale, cx: &mut Context<Self>) {
        let effect = self.shell.set_locale(locale);
        self.queue(effect, cx);
    }

    pub fn set_density(&mut self, density: Density, cx: &mut Context<Self>) {
        self.shell.set_density(density);
        cx.notify();
    }

    fn queue(&mut self, effect: Effect, cx: &mut Context<Self>) {
        match effect {
            Effect::ApplyTheme => apply_theme(&self.shell, cx),
            Effect::RefreshMenus => install_menus(cx, &self.shell),
            Effect::PromptOpen => self.prompt_open(cx),
            Effect::PromptSave => self.prompt_save(cx),
            Effect::EditCell => {
                self.active_grid().update(cx, |grid, cx| {
                    let _ = grid.begin_edit(cx);
                });
            }
            Effect::CancelEdit => {
                self.active_grid().update(cx, |grid, cx| {
                    let _ = grid.cancel_edit(cx);
                });
            }
            Effect::CommitEdit => {
                self.active_grid().update(cx, |grid, cx| {
                    let _ = grid.commit_edit(cx);
                });
            }
            other => self.pending = Some(other),
        }
        cx.notify();
    }

    fn flush_pending(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(effect) = self.pending.take() else {
            return;
        };
        match effect {
            Effect::FocusEditor => self.editor.update(cx, |editor, cx| editor.focus(window, cx)),
            Effect::ShowCompletions => self
                .editor
                .update(cx, |editor, cx| editor.show_completions(window, cx)),
            Effect::CopyRegion | Effect::CopySqlValue => {
                let _ = self.active_grid().update(cx, |grid, cx| grid.copy_tsv(cx));
            }
            Effect::SelectAllCells => {
                let rows = self.active_grid().read(cx).rows(cx).saturating_sub(1);
                self.active_grid().update(cx, |grid, cx| {
                    grid.select(0, 0, false, cx);
                    grid.select(rows, 3, true, cx);
                });
            }
            Effect::None
            | Effect::ApplyTheme
            | Effect::RefreshMenus
            | Effect::PromptOpen
            | Effect::PromptSave
            | Effect::ExecuteSelection
            | Effect::ExecuteCurrent
            | Effect::ToggleComment
            | Effect::FormatSql
            | Effect::ExplainSql
            | Effect::MarkDelete
            | Effect::EditCell
            | Effect::CancelEdit
            | Effect::CommitEdit => {}
        }
    }

    fn active_grid(&self) -> Entity<GridPrototype> {
        if self.show_readonly_grid {
            self.read_only_grid.clone()
        } else {
            self.editable_grid.clone()
        }
    }

    pub fn prompt_open(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(SharedString::from(self.shell.text("shell.command.openSqlite"))),
        });
        cx.spawn(async move |this, cx| {
            let message = receiver.await;
            cx.update(|cx| {
                let _ = this.update(cx, |view, cx| view.on_open_prompt(message, cx));
            });
        })
        .detach();
    }

    pub fn prompt_save(&mut self, cx: &mut Context<Self>) {
        let suggested = match &self.shell.pending_file {
            Some(FilePrompt::Save { suggested_name }) => suggested_name.clone(),
            _ => "query.sql".to_string(),
        };
        let directory = std::env::temp_dir();
        let receiver = cx.prompt_for_new_path(&directory, Some(&suggested));
        cx.spawn(async move |this, cx| {
            let message = receiver.await;
            cx.update(|cx| {
                let _ = this.update(cx, |view, cx| view.on_save_prompt(message, cx));
            });
        })
        .detach();
    }

    /// `Ok(None)` is cancel. A closed channel or a platform `Err` is a visible failure.
    fn on_open_prompt<E: std::fmt::Display>(
        &mut self,
        message: Result<Result<Option<Vec<std::path::PathBuf>>, E>, futures::channel::oneshot::Canceled>,
        cx: &mut Context<Self>,
    ) {
        match message {
            Ok(Ok(Some(paths))) => self.finish_open(paths, cx),
            Ok(Ok(None)) => {}
            Ok(Err(error)) => self.fail_prompt(error.to_string(), cx),
            Err(error) => self.fail_prompt(error.to_string(), cx),
        }
    }

    fn on_save_prompt<E: std::fmt::Display>(
        &mut self,
        message: Result<Result<Option<std::path::PathBuf>, E>, futures::channel::oneshot::Canceled>,
        cx: &mut Context<Self>,
    ) {
        match message {
            Ok(Ok(Some(path))) => self.finish_save(path, cx),
            Ok(Ok(None)) => {}
            Ok(Err(error)) => self.fail_prompt(error.to_string(), cx),
            Err(error) => self.fail_prompt(error.to_string(), cx),
        }
    }

    fn fail_prompt(&mut self, error: String, cx: &mut Context<Self>) {
        self.status = SharedString::from(error);
        cx.notify();
    }

    pub fn finish_open(&mut self, paths: Vec<std::path::PathBuf>, cx: &mut Context<Self>) {
        if let Some(path) = paths.into_iter().next() {
            match accept_open_path(&path) {
                Ok(path) => {
                    self.status = SharedString::from(path.display().to_string());
                    self.shell.note_opened(path);
                }
                Err(error) => self.status = SharedString::from(error),
            }
        }
        cx.notify();
    }

    pub fn finish_save(&mut self, path: std::path::PathBuf, cx: &mut Context<Self>) {
        match accept_save_path(&path) {
            Ok(path) => {
                self.status = SharedString::from(path.display().to_string());
                self.shell.note_saved(path);
            }
            Err(error) => self.status = SharedString::from(error),
        }
        cx.notify();
    }

    pub fn accept_drop(&mut self, paths: &ExternalPaths, cx: &mut Context<Self>) {
        for path in paths.paths() {
            match accept_drop_path(path) {
                Ok(path) => {
                    self.status = SharedString::from(path.display().to_string());
                    self.shell.note_drop(path);
                }
                Err(error) => self.shell.note_rejected_drop(error),
            }
        }
        cx.notify();
    }
}

impl Focusable for Foundation {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Foundation {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.flush_pending(window, cx);
        let pad = px(self.shell.density.control_padding() as f32);
        let readonly = self.show_readonly_grid;
        let editor = self.editor().clone();
        let grid = self.grid(readonly).clone();
        let palette_command = Shell::command("command-palette");
        let palette_shortcut = Shell::shortcut("command-palette");
        let has_editor_control = shared_controls()
            .iter()
            .any(|control| control.id == "editor" && !control.role.is_empty());
        let summary = format!(
            "{:?} {} {} {} {} {}",
            self.shell.resolved_theme(),
            self.shell.locale.code(),
            page_name(self.shell.page),
            palette_command.and_then(|command| command.chord).unwrap_or(""),
            palette_shortcut
                .map(|shortcut| format!("{} {}", shortcut.group, shortcut.chord))
                .unwrap_or_default(),
            if has_editor_control { self.status.as_ref() } else { "" }
        );
        let chrome = self.show_chrome;
        let sidebar = self.shell.sidebar_open;
        let sidebar_label = self.shell.text("shell.command.toggleSidebar");
        let palette = self.shell.palette_open;
        let help = self.shell.help_open;
        let overlay = if help {
            self.shell.text("shell.command.help")
        } else {
            self.shell.text("shell.shortcuts.commandPalette")
        };
        let new_query = self.shell.text("shell.command.newQuery");
        let open_label = self.shell.text("shell.command.openSqlite");
        let save_label = self.shell.text("shell.command.saveQuery");
        div()
            .id("foundation")
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .p(pad)
            .gap(pad)
            .on_drop({
                let view = cx.entity();
                move |paths: &ExternalPaths, _window, cx| {
                    view.update(cx, |this, cx| this.accept_drop(paths, cx));
                }
            })
            .when(chrome, |this| {
                this.child(
                    div()
                        .id("toolbar")
                        .flex()
                        .gap(pad)
                        .child(action_button("new-query", new_query))
                        .child(action_button("open-sqlite", open_label))
                        .child(action_button("save-query", save_label))
                        .child(action_button("theme-dark", self.shell.text("shell.theme.dark")))
                        .child(action_button("theme-light", self.shell.text("shell.theme.light")))
                        .child(action_button(
                            "command-palette",
                            self.shell.text("shell.shortcuts.commandPalette"),
                        )),
                )
            })
            .when(chrome && sidebar, |this| this.child(text_line(sidebar_label)))
            .child(editor)
            .child(grid)
            .when(chrome && (palette || help), |this| {
                this.child(div().id("palette").key_context("Palette").child(text_line(overlay)))
            })
            .when(chrome, |this| this.child(div().id("status").child(SharedString::from(summary))))
    }
}

fn action_button(id: &str, label: &str) -> impl IntoElement {
    let command = id.to_string();
    div()
        .id(SharedString::from(command.clone()))
        .role(gpui_kit::Role::Button)
        .child(SharedString::from(label.to_string()))
        .on_click(move |_event, _window, cx| {
            cx.dispatch_action(command_action(&command).as_ref());
        })
}

fn command_action(id: &str) -> Box<dyn gpui_kit::Action> {
    use crate::app::*;
    match id {
        "new-query" => Box::new(NewQuery),
        "open-sqlite" => Box::new(OpenSqlite),
        "save-query" => Box::new(SaveQuery),
        "theme-dark" => Box::new(ThemeDark),
        "theme-light" => Box::new(ThemeLight),
        "command-palette" => Box::new(CommandPalette),
        _ => Box::new(NewQuery),
    }
}

fn text_line(text: &str) -> impl IntoElement {
    div().child(SharedString::from(text.to_string()))
}

fn page_name(page: AppPage) -> &'static str {
    match page {
        AppPage::Workspace => "workspace",
        AppPage::Connections => "connections",
        AppPage::History => "history",
        AppPage::Settings => "settings",
        AppPage::Structure => "structure",
        AppPage::Compare => "compare",
        AppPage::Diagram => "diagram",
    }
}

fn apply_theme(shell: &Shell, cx: &mut App) {
    let mode = match shell.resolved_theme() {
        ResolvedTheme::Light => ThemeMode::Light,
        ResolvedTheme::Dark => ThemeMode::Dark,
    };
    Theme::change(mode, None, cx);
}

#[allow(dead_code)]
fn _theme_preference_name(preference: ThemePreference) -> &'static str {
    match preference {
        ThemePreference::Light => "light",
        ThemePreference::Dark => "dark",
        ThemePreference::System => "system",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{
        FileDropEvent, InputEvent as _, Point, TestAppContext, WindowBounds, WindowOptions, point,
        px, size,
    };

    fn open(cx: &mut TestAppContext) -> (gpui_kit::AnyWindowHandle, Entity<Foundation>) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            let bounds = gpui_kit::Bounds {
                origin: Point::default(),
                size: size(px(900.), px(640.)),
            };
            let (window, view) = gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                cx,
                |window, cx| cx.new(|cx| Foundation::new(window, cx)),
            )
            .expect("foundation window");
            let shell = view.read(cx).shell().clone();
            crate::app::install_actions(cx, &view);
            crate::app::install_menus(cx, &shell);
            (window, view)
        })
    }

    #[gpui_kit::test]
    fn commands_theme_and_focus_round_trip_in_a_headless_window(cx: &mut TestAppContext) {
        let (window, view) = open(cx);
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            view.update(cx, |view, cx| view.perform("theme-dark", cx));
            assert_eq!(view.read(cx).shell().resolved_theme(), ResolvedTheme::Dark);
            view.update(cx, |view, cx| view.set_density(Density::Compact, cx));
            assert_eq!(view.read(cx).shell().density, Density::Compact);
            view.update(cx, |view, cx| view.perform("command-palette", cx));
            assert_eq!(view.read(cx).shell().focus, FocusTarget::Palette);
            view.update(cx, |view, cx| {
                view.shell_mut().close_overlay();
                cx.notify();
            });
            assert_eq!(view.read(cx).shell().focus, FocusTarget::Editor);
            view.update(cx, |view, cx| view.set_locale(Locale::En, cx));
            assert_eq!(view.read(cx).shell().text("shell.command.newQuery"), "New query");
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn native_prompt_and_file_drop_record_real_paths(cx: &mut TestAppContext) {
        let dir = std::env::temp_dir().join(format!("rillsql-gpui-drop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("拖放.sql");
        std::fs::write(&file, "select 1;\n").unwrap();
        let (window, view) = open(cx);
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            view.update(cx, |view, cx| view.perform("open-sqlite", cx));
        })
        .unwrap();
        assert!(cx.did_prompt_for_paths());
        let opened = file.canonicalize().unwrap();
        cx.simulate_path_prompt_response(|_| Some(vec![opened.clone()]));
        cx.run_until_parked();
        cx.update(|cx| {
            assert_eq!(view.read(cx).shell().opened_path.as_ref(), Some(&opened));
        });

        cx.update_window(window, |_, _, cx| {
            view.update(cx, |view, cx| view.perform("save-query", cx));
        })
        .unwrap();
        assert!(cx.did_prompt_for_new_path());
        let saved = dir.join("query.sql");
        cx.simulate_new_path_selection(|_| Some(saved.clone()));
        cx.run_until_parked();
        cx.update(|cx| {
            let recorded = view.read(cx).shell().saved_path.clone().unwrap();
            assert_eq!(recorded.file_name().unwrap(), "query.sql");
            assert!(recorded.is_absolute());
        });

        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let position = point(px(30.), px(30.));
            let paths = ExternalPaths([file.clone()].into_iter().collect());
            window.dispatch_event(
                FileDropEvent::Entered { position, paths }.to_platform_input(),
                cx,
            );
            window.dispatch_event(
                FileDropEvent::Submit { position }.to_platform_input(),
                cx,
            );
            window.render_frame(cx);
        })
        .unwrap();
        cx.update(|cx| {
            let dropped = view.read(cx).shell().dropped_paths.clone();
            assert_eq!(dropped, vec![opened]);
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[gpui_kit::test]
    fn prompt_cancel_is_quiet_and_dialog_errors_are_visible(cx: &mut TestAppContext) {
        let (window, view) = open(cx);
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            view.update(cx, |view, cx| view.perform("open-sqlite", cx));
        })
        .unwrap();
        assert!(cx.did_prompt_for_paths());
        cx.simulate_path_prompt_response(|_| None);
        cx.run_until_parked();
        cx.update(|cx| {
            assert!(view.read(cx).shell().opened_path.is_none());
            assert_eq!(view.read(cx).status.as_ref(), "GPUI");
        });

        let (sender, mut receiver) = futures::channel::oneshot::channel();
        sender
            .send(Err(std::io::Error::other("open picker failed")))
            .unwrap();
        let message = receiver.try_recv().unwrap().expect("open result");
        cx.update(|cx| {
            view.update(cx, |view, cx| view.on_open_prompt(Ok(message), cx));
            assert!(view.read(cx).status.as_ref().contains("open picker failed"));
            assert!(view.read(cx).shell().opened_path.is_none());
        });

        let (sender, mut receiver) = futures::channel::oneshot::channel::<
            Result<Option<Vec<std::path::PathBuf>>, std::io::Error>,
        >();
        drop(sender);
        let canceled = receiver.try_recv().expect_err("dropped sender cancels");
        cx.update(|cx| {
            view.update(cx, |view, cx| view.on_open_prompt::<std::io::Error>(Err(canceled), cx));
            assert!(view.read(cx).status.as_ref().contains("canceled"));
            assert!(view.read(cx).shell().opened_path.is_none());
        });

        cx.update_window(window, |_, _, cx| {
            view.update(cx, |view, cx| view.perform("save-query", cx));
        })
        .unwrap();
        assert!(cx.did_prompt_for_new_path());
        cx.simulate_new_path_selection(|_| None);
        cx.run_until_parked();
        cx.update(|cx| {
            assert!(view.read(cx).shell().saved_path.is_none());
            assert!(view.read(cx).status.as_ref().contains("canceled"));
        });

        let (sender, mut receiver) = futures::channel::oneshot::channel();
        sender
            .send(Err(std::io::Error::other("save picker failed")))
            .unwrap();
        let message = receiver.try_recv().unwrap().expect("save result");
        cx.update(|cx| {
            view.update(cx, |view, cx| view.on_save_prompt(Ok(message), cx));
            assert!(view.read(cx).status.as_ref().contains("save picker failed"));
            assert!(view.read(cx).shell().saved_path.is_none());
        });
    }
}
