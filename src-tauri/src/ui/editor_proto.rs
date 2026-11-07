//! SQL editor prototype on GPUI Kit's Editor.
#![cfg_attr(not(test), allow(dead_code, unused_imports))]
//! Dialect rules stay out of this gate; the prototype checks IME, long text,
//! selection, completion, lint, folding and undo against the real control.

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;

use gpui_kit::base::input::{Diagnostic, DiagnosticSeverity};
use gpui_kit::component::input::{
    CompletionProvider, Editor, EditorState, FoldRange, InputEvent, Rope,
};
use gpui_kit::{
    App, AppContext as _, Context, Entity, EntityInputHandler as _, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, Styled as _, Task, Window, div, px,
};
use lsp_types::{CompletionContext, CompletionItem, CompletionResponse};

use crate::app::accept_versioned;

pub struct SqlEditor {
    state: Entity<EditorState>,
    version: Rc<Cell<u64>>,
    extra: Rc<RefCell<Vec<String>>>,
    stale_lint: u32,
    stale_completion: u32,
    folded_lines: u32,
    _subscription: gpui_kit::Subscription,
}

struct SqlSuggest {
    version: Rc<Cell<u64>>,
    extra: Rc<RefCell<Vec<String>>>,
}

impl SqlEditor {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let version = Rc::new(Cell::new(1));
        let extra = Rc::new(RefCell::new(Vec::new()));
        let state = cx.new(|cx| {
            let mut state = EditorState::new(window, cx)
                .language("sql")
                .line_number(true)
                .folding(true)
                .default_value("select 1;\n");
            state.lsp_mut().completion_provider = Some(Rc::new(SqlSuggest {
                version: version.clone(),
                extra: extra.clone(),
            }));
            state
        });
        let subscription = cx.subscribe(&state, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.version.set(this.version.get().wrapping_add(1));
                cx.notify();
            }
        });
        Self {
            state,
            version,
            extra,
            stale_lint: 0,
            stale_completion: 0,
            folded_lines: 0,
            _subscription: subscription,
        }
    }

    pub fn version(&self) -> u64 {
        self.version.get()
    }

    pub fn stale_lint(&self) -> u32 {
        self.stale_lint
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.state.update(cx, |state, cx| state.focus(window, cx));
    }

    pub fn value(&self, cx: &App) -> String {
        self.state.read(cx).value().to_string()
    }

    pub fn selected_text(&self, cx: &App) -> String {
        self.state.read(cx).selected_text().to_string()
    }

    pub fn set_sql(&mut self, sql: &str, window: &mut Window, cx: &mut Context<Self>) {
        let version = self.version.clone();
        self.state.update(cx, |state, cx| {
            state.set_value(sql, window, cx);
        });
        version.set(version.get().wrapping_add(1));
    }

    pub fn selected_range(&self, cx: &App) -> Range<usize> {
        self.state.read(cx).selected_range()
    }

    pub fn select_range(&self, range: Range<usize>, cx: &mut App) {
        self.state.update(cx, |state, cx| {
            state.set_selected_range(range, cx);
        });
    }

    /// Apply lint only when `response_version` is still the editor's version.
    pub fn apply_lint(
        &mut self,
        response_version: u64,
        message: &str,
        range: Range<usize>,
        cx: &mut Context<Self>,
    ) -> bool {
        if !accept_versioned(self.version.get(), response_version) {
            self.stale_lint += 1;
            return false;
        }
        self.state.update(cx, |state, _| {
            let text = state.text().clone();
            let Some(set) = state.diagnostics_mut() else {
                return;
            };
            set.reset(&text);
            let start = offset_to_position(&text, range.start);
            let end = offset_to_position(&text, range.end.min(text.len()));
            set.push(
                Diagnostic::new(start..end, message).with_severity(DiagnosticSeverity::Error),
            );
        });
        true
    }

    pub fn lint_message(&self, cx: &App) -> Option<String> {
        self.state.read(cx).diagnostics().and_then(|set| {
            set.iter().next().map(|entry| entry.diagnostic.message.to_string())
        })
    }

    pub fn show_completions(&self, window: &mut Window, cx: &mut App) {
        self.state
            .update(cx, |state, cx| state.show_completions(window, cx));
    }

    pub fn completion_labels(&self, cx: &App) -> Vec<String> {
        self.state
            .read(cx)
            .completion_menu_state()
            .items
            .iter()
            .map(|item| item.label.clone())
            .collect()
    }

    pub fn completion_open(&self, cx: &App) -> bool {
        self.state.read(cx).completion_menu_state().open
    }

    /// Insert a completion that was computed for `response_version`.
    pub fn apply_completion_insert(
        &mut self,
        response_version: u64,
        label: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !accept_versioned(self.version.get(), response_version) {
            self.stale_completion += 1;
            return false;
        }
        let item = completion_item(label);
        self.state.update(cx, |state, cx| {
            let range = state.selected_range();
            state.insert_completion(&item, range, window, cx);
        });
        true
    }

    /// Extra labels come from the dialect module. The editor still only displays them.
    pub fn set_extra_suggestions(&self, labels: Vec<String>) {
        *self.extra.borrow_mut() = labels;
    }

    pub fn apply_folds(&mut self, ranges: &[(usize, usize)], cx: &mut Context<Self>) -> bool {
        if ranges.is_empty() {
            return false;
        }
        let folded = ranges.iter().copied().map(|(start, end)| FoldRange::new(start, end)).collect();
        self.state.update(cx, |state, cx| {
            state.apply_highlighter_fold_candidates(folded, cx);
        });
        true
    }

    pub fn clear_lint(&mut self, response_version: u64, cx: &mut Context<Self>) -> bool {
        if !accept_versioned(self.version.get(), response_version) {
            self.stale_lint += 1;
            return false;
        }
        self.state.update(cx, |state, _| {
            let text = state.text().clone();
            if let Some(set) = state.diagnostics_mut() {
                set.reset(&text);
            }
        });
        true
    }

    pub fn fold_first_block(&mut self, cx: &mut Context<Self>) -> bool {
        let folded = self.state.update(cx, |state, cx| {
            state.apply_highlighter_fold_candidates(vec![FoldRange::new(0, 3)], cx);
            state.display_map().is_fold_candidate(0)
        });
        if folded {
            self.folded_lines = 1;
        }
        folded
    }

    pub fn fold_candidate(&self, line: usize, cx: &App) -> bool {
        self.state.read(cx).display_map().is_fold_candidate(line)
    }

    pub fn is_folded(&self, line: usize, cx: &App) -> bool {
        self.state.read(cx).display_map().is_folded_at(line)
    }

    pub fn line_hidden(&self, line: usize, cx: &App) -> bool {
        self.state.read(cx).display_map().is_buffer_line_hidden(line)
    }
}

impl Render for SqlEditor {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("sql-editor")
            .h(px(240.))
            .w_full()
            .child(Editor::new(&self.state).h(px(240.)).aria_label("SQL"))
    }
}

impl SqlSuggest {
    fn items(&self, text: &Rope, offset: usize) -> Vec<CompletionItem> {
        let word = word_before(text, offset);
        let mut items = Vec::new();
        for keyword in ["SELECT", "FROM", "WHERE", "INSERT", "UPDATE"] {
            if word.is_empty() || keyword.to_ascii_lowercase().starts_with(&word.to_ascii_lowercase())
            {
                items.push(completion_item(keyword));
            }
        }
        for label in self.extra.borrow().iter() {
            if items.iter().any(|item| item.label == *label) {
                continue;
            }
            if word.is_empty() || label.to_ascii_lowercase().starts_with(&word.to_ascii_lowercase()) {
                items.push(completion_item(label));
            }
        }
        items
    }
}

impl CompletionProvider for SqlSuggest {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        _trigger: CompletionContext,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Task<gpui_kit::Result<CompletionResponse>> {
        let started = self.version.get();
        let items = self.items(text, offset);
        let version = self.version.clone();
        Task::ready(Ok(CompletionResponse::Array(
            if accept_versioned(version.get(), started) {
                items
            } else {
                Vec::new()
            },
        )))
    }

    fn is_completion_trigger(&self, _offset: usize, new_text: &str, _cx: &mut App) -> bool {
        new_text
            .chars()
            .any(|ch| ch.is_ascii_alphabetic() || ch == '.')
    }
}

fn completion_item(label: &str) -> CompletionItem {
    CompletionItem {
        label: label.to_string(),
        insert_text: Some(label.to_string()),
        ..CompletionItem::default()
    }
}

fn word_before(text: &Rope, offset: usize) -> String {
    let offset = offset.min(text.len());
    let head = text.slice(0..offset).to_string();
    head.chars()
        .rev()
        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
        .collect::<String>()
        .chars()
        .rev()
        .collect()
}

fn offset_to_position(text: &Rope, offset: usize) -> lsp_types::Position {
    let offset = offset.min(text.len());
    let prefix = text.slice(0..offset).to_string();
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() as u32;
    let character = prefix
        .rsplit_once('\n')
        .map(|(_, rest)| rest.encode_utf16().count())
        .unwrap_or_else(|| prefix.encode_utf16().count()) as u32;
    lsp_types::Position { line, character }
}

fn long_sql() -> (String, Range<usize>) {
    let mut sql = String::new();
    for index in 0..4_000 {
        sql.push_str(&format!("select {index} as n, '中文' as label;\n"));
    }
    let marker = "/*MARKER-长SQL*/";
    let start = sql.len();
    sql.push_str(marker);
    let range = start..start + marker.len();
    sql.push('\n');
    for index in 0..4_000 {
        sql.push_str(&format!("select {index} as tail;\n"));
    }
    (sql, range)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{Point, TestAppContext, WindowBounds, WindowOptions, px, size};

    fn open(cx: &mut TestAppContext) -> (gpui_kit::AnyWindowHandle, Entity<SqlEditor>) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            let bounds = gpui_kit::Bounds {
                origin: Point::default(),
                size: size(px(800.), px(480.)),
            };
            let (window, editor) = gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                cx,
                |window, cx| cx.new(|cx| SqlEditor::new(window, cx)),
            )
            .expect("editor window");
            (window.into(), editor)
        })
    }

    #[gpui_kit::test]
    fn chinese_ime_long_sql_selection_completion_lint_fold_and_undo(cx: &mut TestAppContext) {
        let (window, editor) = open(cx);
        cx.update_window(window, |_, window, cx| {
            editor.update(cx, |editor, cx| editor.focus(window, cx));
            window.render_frame(cx);
            editor.update(cx, |editor, cx| {
                editor.state.update(cx, |state, cx| {
                    state.replace_and_mark_text_in_range(
                        None,
                        "zhong",
                        Some(5..5),
                        window,
                        cx,
                    );
                    state.replace_and_mark_text_in_range(None, "中文", Some(2..2), window, cx);
                    state.unmark_text(window, cx);
                });
            });
        })
        .unwrap();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let value = editor.read(cx).value(cx);
            assert!(value.contains("中文"), "{value}");
            window.press("secondary-z", cx);
        })
        .unwrap();
        cx.update_window(window, |_, window, cx| {
            let value = editor.read(cx).value(cx);
            assert!(!value.contains("中文"), "undo should drop the committed preedit: {value}");
            let (sql, range) = long_sql();
            editor.update(cx, |editor, cx| editor.set_sql(&sql, window, cx));
            editor.update(cx, |editor, cx| editor.select_range(range.clone(), cx));
            assert_eq!(editor.read(cx).selected_text(cx), "/*MARKER-长SQL*/");
            let version = editor.read(cx).version();
            assert!(editor.update(cx, |editor, cx| {
                editor.apply_lint(version, "未闭合的字符串", 0..6, cx)
            }));
            assert_eq!(
                editor.read(cx).lint_message(cx).as_deref(),
                Some("未闭合的字符串")
            );
            assert!(!editor.update(cx, |editor, cx| {
                editor.apply_lint(version.saturating_sub(1), "过期诊断", 0..1, cx)
            }));
            assert_eq!(editor.read(cx).stale_lint(), 1);
            assert_eq!(
                editor.read(cx).lint_message(cx).as_deref(),
                Some("未闭合的字符串")
            );
            let current = editor.read(cx).version();
            assert!(editor.update(cx, |editor, cx| {
                editor.apply_completion_insert(current, "SELECT", window, cx)
            }));
            assert!(editor.read(cx).value(cx).contains("SELECT"));
            assert!(!editor.update(cx, |editor, cx| {
                editor.apply_completion_insert(current.saturating_sub(1), "STALE_WORD", window, cx)
            }));
            assert!(!editor.read(cx).value(cx).contains("STALE_WORD"));
            assert_eq!(editor.read(cx).stale_completion, 1);
            // Let the highlighter consume the pending edit before installing
            // the fixture fold, or the next frame replaces those candidates.
            window.render_frame(cx);
            assert!(editor.update(cx, |editor, cx| editor.fold_first_block(cx)));
            assert!(editor.read(cx).fold_candidate(0, cx));
            editor.update(cx, |editor, cx| editor.select_range(0..0, cx));
            window.render_frame(cx);
            window.click(("fold-icon", 0usize), cx);
            assert!(editor.read(cx).is_folded(0, cx), "gutter click should fold line 0");
            assert!(
                editor.read(cx).line_hidden(1, cx),
                "fold 0..3 hides the following line"
            );
            window.click(("fold-icon", 0usize), cx);
            assert!(!editor.read(cx).is_folded(0, cx));
            assert!(!editor.read(cx).line_hidden(1, cx));
        })
        .unwrap();

        cx.update_window(window, |_, window, cx| {
            editor.update(cx, |editor, cx| {
                editor.set_sql("SEL", window, cx);
                editor.focus(window, cx);
                editor.show_completions(window, cx);
            });
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(window, |_, _, cx| {
            assert!(
                editor.read(cx).completion_open(cx),
                "completion menu should open for SEL"
            );
            assert!(
                editor.read(cx).completion_labels(cx).iter().any(|label| label == "SELECT"),
                "{:?}",
                editor.read(cx).completion_labels(cx)
            );
        })
        .unwrap();
    }

    #[test]
    fn suggest_matches_a_prefix_without_a_window() {
        let rope = Rope::from("sel");
        let suggest = SqlSuggest { version: Rc::new(Cell::new(1)), extra: Rc::new(RefCell::new(Vec::new())) };
        let labels: Vec<_> = suggest.items(&rope, 3)
            .into_iter()
            .map(|item| item.label)
            .collect();
        assert_eq!(labels, vec!["SELECT".to_string()]);
    }
}
