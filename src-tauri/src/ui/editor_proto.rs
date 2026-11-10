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
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, EntityInputHandler as _, Focusable as _, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, Styled as _, Task, Window, div, px, rgb,
};
use lsp_types::{CompletionContext, CompletionItem, CompletionItemKind, CompletionResponse, CompletionTextEdit, TextEdit};

use crate::app::accept_versioned;
use crate::app::views::settings;
use crate::i18n::Locale;
use crate::query::assist::CompletionScope;
use crate::query::complete;

pub struct SqlEditor {
    state: Entity<EditorState>,
    version: Rc<Cell<u64>>,
    extra: Rc<RefCell<Vec<(String, String)>>>,
    scope: Rc<RefCell<Option<CompletionScope>>>,
    /// 补全详情与状态说明按当前语言生成；切换语言后新请求立即生效。
    locale: Rc<Cell<Locale>>,
    /// 最近一次请求的元数据状态说明；失败、部分失败或空目录时非空。
    note: Rc<RefCell<Option<String>>>,
    stale_lint: u32,
    stale_completion: u32,
    folded_lines: u32,
    /// 设置里的编辑器字号与配色；渲染时直接作用到 Editor 的文本样式。
    font_size: f32,
    theme: String,
    _subscription: gpui_kit::Subscription,
}

struct SqlSuggest {
    version: Rc<Cell<u64>>,
    extra: Rc<RefCell<Vec<(String, String)>>>,
    scope: Rc<RefCell<Option<CompletionScope>>>,
    locale: Rc<Cell<Locale>>,
    note: Rc<RefCell<Option<String>>>,
    request: Rc<Cell<u64>>,
}

impl SqlEditor {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let version = Rc::new(Cell::new(1));
        let extra = Rc::new(RefCell::new(Vec::new()));
        let scope = Rc::new(RefCell::new(None));
        let locale = Rc::new(Cell::new(Locale::ZhCn));
        let note = Rc::new(RefCell::new(None));
        let request = Rc::new(Cell::new(0));
        let state = cx.new(|cx| {
            let mut state = EditorState::new(window, cx)
                .language("sql")
                .line_number(true)
                .folding(true)
                .default_value("select 1;\n");
            state.lsp_mut().completion_provider = Some(Rc::new(SqlSuggest {
                version: version.clone(),
                extra: extra.clone(),
                scope: scope.clone(),
                locale: locale.clone(),
                note: note.clone(),
                request: request.clone(),
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
            scope,
            locale,
            note,
            stale_lint: 0,
            stale_completion: 0,
            folded_lines: 0,
            font_size: 13.0,
            theme: "auto".to_owned(),
            _subscription: subscription,
        }
    }

    /// 设置里的查询编辑器字号；渲染时作为 Editor 的文本尺寸。
    pub fn font_size(&self) -> i64 {
        self.font_size.round() as i64
    }

    pub fn set_font_size(&mut self, size: i64) {
        self.font_size = size.clamp(settings::FONT_RANGE.0, settings::FONT_RANGE.1) as f32;
    }

    /// 当前编辑器配色档位（auto / contrast / warm / cool / mono）。
    pub fn theme(&self) -> &str {
        &self.theme
    }

    pub fn set_theme(&mut self, theme: &str) {
        self.theme = settings::normalize_editor_theme(theme).to_owned();
    }

    /// 当前档位在给定明暗下的背景和前景，与渲染使用的是同一份映射。
    pub fn theme_colors(&self, dark: bool) -> (u32, u32) {
        settings::editor_theme_colors(&self.theme, dark)
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

    pub fn is_focused(&self, window: &Window, cx: &App) -> bool {
        self.state.read(cx).focus_handle(cx).is_focused(window)
    }

    /// 光标前连续的标识符，作为补全前缀。
    pub fn completion_prefix(&self, cx: &App) -> String {
        let value = self.value(cx);
        let mut end = self.selected_range(cx).end.min(value.len());
        while end > 0 && !value.is_char_boundary(end) {
            end -= 1;
        }
        value[..end]
            .chars()
            .rev()
            .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
            .collect::<String>()
            .chars()
            .rev()
            .collect()
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
        insert: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !accept_versioned(self.version.get(), response_version) {
            self.stale_completion += 1;
            return false;
        }
        self.state.update(cx, |state, cx| {
            let selected = state.selected_range();
            let text = state.text().clone();
            // 有选区时替换选区；否则替换光标前已打出的单词。
            let start = if selected.is_empty() { selected.end - word_before(&text, selected.end).len() } else { selected.start };
            let range = offset_to_position(&text, start)..offset_to_position(&text, selected.end);
            let item = completion_edit(label, insert, None, range, None);
            state.insert_completion(&item, selected, window, cx);
        });
        true
    }

    pub fn completion_insert(&self, label: &str, cx: &App) -> Option<String> {
        self.state.read(cx).completion_menu_state().items.iter().find(|item| item.label == label).and_then(|item| item.insert_text.clone())
    }

    /// 方言模块给出的 (显示名, 插入文本)。编辑器按插入文本写入。
    pub fn set_extra_suggestions(&self, items: Vec<(String, String)>) {
        *self.extra.borrow_mut() = items;
    }

    /// 补全详情与状态说明跟随当前语言。
    pub fn set_locale(&mut self, locale: Locale) {
        self.locale.set(locale);
    }

    /// 最近一次补全请求的元数据状态说明；正常且已读时为空。
    pub fn completion_note(&self) -> Option<String> {
        self.note.borrow().clone()
    }

    /// 打字补全的上下文（方言、对象、列）。与当前快照相同时不替换。
    pub fn set_completion_scope(&self, scope: Option<CompletionScope>) {
        let mut slot = self.scope.borrow_mut();
        if *slot != scope {
            *slot = scope;
        }
    }

    pub fn apply_folds(&mut self, ranges: &[(usize, usize)], cx: &mut Context<Self>) -> bool {
        let folded = ranges.iter().copied().map(|(start, end)| FoldRange::new(start, end)).collect();
        self.state.update(cx, |state, cx| {
            state.apply_highlighter_fold_candidates(folded, cx);
        });
        !ranges.is_empty()
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
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (background, foreground) = self.theme_colors(cx.theme().is_dark());
        div()
            .id("sql-editor")
            .size_full()
            .text_size(px(self.font_size))
            .text_color(rgb(foreground))
            .child(
                Editor::new(&self.state)
                    .h(gpui_kit::relative(1.))
                    .bordered(false)
                    .aria_label("SQL")
                    .text_size(px(self.font_size))
                    .bg(rgb(background))
                    .text_color(rgb(foreground)),
            )
    }
}

impl SqlSuggest {
    /// 只要候选；测试和单次查询用它，不关心请求代次。
    fn items(&self, text: &Rope, offset: usize) -> Vec<CompletionItem> {
        let (_, items) = self.respond(text, offset);
        items
    }

    /// 一次补全请求：整段语句、光标、编辑器版本和元数据快照都参与计算。
    /// 返回请求代次，迟到结果用 deliver 丢弃。
    fn respond(&self, text: &Rope, offset: usize) -> (u64, Vec<CompletionItem>) {
        let request = self.request.get().wrapping_add(1);
        self.request.set(request);
        let offset = offset.min(text.len());
        let word = word_before(text, offset);
        let lower = word.to_ascii_lowercase();
        let locale = self.locale.get();
        *self.note.borrow_mut() = None;
        let mut items: Vec<CompletionItem> = Vec::new();
        {
            let scope = self.scope.borrow();
            match scope.as_ref() {
                Some(scope) => {
                    let statement = text.slice(0..text.len()).to_string();
                    let planned = complete::plan(scope, &statement, offset, locale);
                    *self.note.borrow_mut() = planned.note.clone();
                    // 只替换当前词；限定符和前后文本保持原样。
                    let start = planned.span.start.min(offset);
                    let end = planned.span.end.min(text.len());
                    let range = offset_to_position(text, start)..offset_to_position(text, end);
                    for entry in &planned.candidates {
                        items.push(completion_edit(
                            &entry.label,
                            &entry.insert,
                            completion_kind(entry.kind),
                            range.clone(),
                            Some(entry.detail.clone()),
                        ));
                    }
                }
                None => {
                    let range = offset_to_position(text, offset - word.len())..offset_to_position(text, offset);
                    for keyword in FALLBACK_KEYWORDS {
                        if lower.is_empty() || keyword.to_ascii_lowercase().starts_with(&lower) {
                            items.push(completion_edit(keyword, keyword, Some(CompletionItemKind::KEYWORD), range.clone(), None));
                        }
                    }
                }
            }
        }
        let range = offset_to_position(text, offset - word.len())..offset_to_position(text, offset);
        for (label, insert) in self.extra.borrow().iter() {
            if items.iter().any(|item| item.label == *label) {
                continue;
            }
            if lower.is_empty() || label.to_ascii_lowercase().starts_with(&lower) || insert.to_ascii_lowercase().starts_with(&lower) {
                items.push(completion_edit(label, insert, None, range.clone(), None));
            }
        }
        // 已经打完整的唯一候选不必再弹出。
        if items.len() == 1 && items[0].insert_text.as_deref().is_some_and(|insert| insert.eq_ignore_ascii_case(&word)) {
            items.clear();
        }
        (request, items)
    }

    /// 只有最新请求且编辑器版本未变时才交付候选；迟到结果一律丢弃。
    fn deliver(&self, version: u64, request: u64, items: Vec<CompletionItem>) -> Vec<CompletionItem> {
        if !accept_versioned(self.version.get(), version) || self.request.get() != request {
            return Vec::new();
        }
        items
    }
}

const FALLBACK_KEYWORDS: [&str; 12] = ["SELECT", "FROM", "WHERE", "INSERT", "UPDATE", "DELETE", "JOIN", "GROUP BY", "ORDER BY", "LIMIT", "VALUES", "SET"];

fn completion_kind(kind: &str) -> Option<CompletionItemKind> {
    Some(match kind {
        "keyword" => CompletionItemKind::KEYWORD,
        "snippet" => CompletionItemKind::SNIPPET,
        "column" => CompletionItemKind::FIELD,
        "condition" => CompletionItemKind::OPERATOR,
        "cte" => CompletionItemKind::CLASS,
        "database" | "schema" => CompletionItemKind::MODULE,
        "view" => CompletionItemKind::INTERFACE,
        "function" | "procedure" => CompletionItemKind::FUNCTION,
        _ => CompletionItemKind::STRUCT,
    })
}

fn completion_edit(
    label: &str,
    insert: &str,
    kind: Option<CompletionItemKind>,
    range: Range<lsp_types::Position>,
    detail: Option<String>,
) -> CompletionItem {
    CompletionItem {
        label: label.to_string(),
        kind,
        detail,
        insert_text: Some(insert.to_string()),
        filter_text: Some(insert.to_string()),
        text_edit: Some(CompletionTextEdit::Edit(TextEdit { range: lsp_types::Range { start: range.start, end: range.end }, new_text: insert.to_string() })),
        ..CompletionItem::default()
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
        let (request, items) = self.respond(text, offset);
        let items = self.deliver(started, request, items);
        Task::ready(Ok(CompletionResponse::Array(items)))
    }

    fn is_completion_trigger(&self, _offset: usize, new_text: &str, _cx: &mut App) -> bool {
        // IME 组合和中文正文不触发弹层，避免打断输入。
        new_text.chars().any(|ch| ch.is_ascii_alphabetic() || ch == '.')
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
            (window, editor)
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
                editor.apply_completion_insert(current, "SELECT", "SELECT", window, cx)
            }));
            assert!(editor.read(cx).value(cx).contains("SELECT"));
            assert!(!editor.update(cx, |editor, cx| {
                editor.apply_completion_insert(current.saturating_sub(1), "STALE_WORD", "STALE_WORD", window, cx)
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

    fn suggest(scope: Option<CompletionScope>, extra: Vec<(String, String)>) -> SqlSuggest {
        SqlSuggest {
            version: Rc::new(Cell::new(1)),
            extra: Rc::new(RefCell::new(extra)),
            scope: Rc::new(RefCell::new(scope)),
            locale: Rc::new(Cell::new(Locale::ZhCn)),
            note: Rc::new(RefCell::new(None)),
            request: Rc::new(Cell::new(0)),
        }
    }

    fn catalog_scope() -> CompletionScope {
        use crate::query::assist::{CatalogObject, CompletionSource, MetadataState};
        CompletionScope {
            engine: "sqlite".to_owned(),
            objects: vec![CatalogObject { name: "orders".to_owned(), schema: String::new(), kind: "table".to_owned() }],
            sources: vec![CompletionSource {
                alias: String::new(),
                label: "orders".to_owned(),
                columns: vec!["id".to_owned(), "customer".to_owned(), "status".to_owned()],
            }],
            provider: true,
            databases: vec!["main".to_owned()],
            schemas: Vec::new(),
            frequency: Vec::new(),
            state: MetadataState::Ready,
            foreign_keys: Vec::new(),
        }
    }

    #[test]
    fn suggest_matches_a_prefix_without_a_window() {
        let rope = Rope::from("sel");
        let plain = suggest(None, Vec::new());
        let labels: Vec<_> = plain.items(&rope, 3).into_iter().map(|item| item.label).collect();
        assert_eq!(labels, vec!["SELECT".to_string()]);
        let snippet = Rope::from("select");
        let with_snippet = suggest(None, vec![("select-from".to_owned(), "SELECT  FROM ".to_owned())]);
        let item = with_snippet.items(&snippet, 6).into_iter().find(|item| item.label == "select-from").expect("snippet");
        assert_eq!(item.insert_text.as_deref(), Some("SELECT  FROM "));
    }

    #[test]
    fn typed_completion_uses_the_catalog_and_replaces_the_prefix() {
        let provider = suggest(Some(catalog_scope()), Vec::new());
        let rope = Rope::from("select * from ord");
        let items = provider.items(&rope, rope.len());
        let item = items.iter().find(|item| item.label == "orders").expect("打字时应列出当前库的表");
        let Some(CompletionTextEdit::Edit(edit)) = item.text_edit.as_ref() else { panic!("缺少替换范围") };
        assert_eq!(edit.range.start, lsp_types::Position { line: 0, character: 14 });
        assert_eq!(edit.range.end, lsp_types::Position { line: 0, character: 17 });
        assert_eq!(edit.new_text, "orders");

        // 限定符只补列名，替换范围不覆盖别名和点号。
        let qualified = "select * from orders o where o.st";
        let rope = Rope::from(qualified);
        let items = provider.items(&rope, rope.len());
        let item = items.iter().find(|item| item.label == "status").expect("别名后只给该表的列");
        let Some(CompletionTextEdit::Edit(edit)) = item.text_edit.as_ref() else { panic!("缺少替换范围") };
        assert_eq!(edit.range.start, lsp_types::Position { line: 0, character: 31 });
        assert_eq!(edit.range.end, lsp_types::Position { line: 0, character: 33 });
    }

    #[test]
    fn stale_and_ime_requests_never_clobber_candidates() {
        let provider = suggest(Some(catalog_scope()), Vec::new());
        let first = Rope::from("select * from ord");
        let (request, items) = provider.respond(&first, first.len());
        assert!(!items.is_empty());
        let second = Rope::from("select * from orde");
        let _ = provider.respond(&second, second.len());
        assert!(provider.deliver(1, request, items.clone()).is_empty(), "旧请求的候选必须被丢弃");
        let (newest_request, newest) = provider.respond(&second, second.len());
        assert!(!provider.deliver(1, newest_request, newest).is_empty(), "最新请求应正常交付");

        let ime = Rope::from("select 中文");
        let (_, items) = provider.respond(&ime, ime.len());
        assert!(items.is_empty(), "IME/中文正文里不弹候选");
    }

    #[test]
    fn metadata_failure_keeps_keywords_and_shows_a_readable_note() {
        use crate::query::assist::MetadataState;
        let mut broken = catalog_scope();
        broken.state = MetadataState::Failed("读取库表失败：连接中断".to_owned());
        broken.objects.clear();
        broken.sources.clear();
        let provider = suggest(Some(broken), Vec::new());
        let rope = Rope::from("select * from ");
        let (_, items) = provider.respond(&rope, rope.len());
        assert!(items.iter().any(|item| item.label == "SELECT"), "元数据失败时仍给关键字");
        assert!(!items.iter().any(|item| item.label == "orders"));
        let note = provider.note.borrow().clone().expect("失败状态必须说明原因");
        assert!(note.contains("连接中断"), "{note}");
    }

    #[gpui_kit::test]
    fn accepting_a_completion_replaces_the_typed_word(cx: &mut TestAppContext) {
        let (window, editor) = open(cx);
        cx.update_window(window, |_, window, cx| {
            editor.update(cx, |editor, cx| {
                editor.set_sql("sel", window, cx);
                editor.select_range(3..3, cx);
                let version = editor.version();
                assert!(editor.apply_completion_insert(version, "SELECT", "SELECT", window, cx));
            });
            assert_eq!(editor.read(cx).value(cx), "SELECT");
            // 接受补全只替换当前词，撤销回到补全前的那一步。
            window.press("secondary-z", cx);
            assert_eq!(editor.read(cx).value(cx), "sel");
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn popup_lists_candidates_and_escape_returns_focus(cx: &mut TestAppContext) {
        let (window, editor) = open(cx);
        cx.update_window(window, |_, window, cx| {
            editor.update(cx, |editor, cx| {
                editor.set_sql("select * from ord", window, cx);
                editor.focus(window, cx);
                editor.set_completion_scope(Some(catalog_scope()));
                editor.select_range(17..17, cx);
                editor.show_completions(window, cx);
            });
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| {
            assert!(editor.read(cx).completion_open(cx), "有候选时弹层应打开");
            let labels = editor.read(cx).completion_labels(cx);
            assert!(labels.iter().any(|label| label == "orders"), "{labels:?}");
            assert!(editor.read(cx).is_focused(window, cx), "弹层打开时焦点仍在编辑器");
            window.press("escape", cx);
            assert!(!editor.read(cx).completion_open(cx), "Esc 关闭弹层");
            assert!(editor.read(cx).is_focused(window, cx), "Esc 之后焦点回到编辑器");
            assert_eq!(editor.read(cx).value(cx), "select * from ord", "Esc 不改草稿");
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn ime_and_non_ascii_text_do_not_trigger_the_popup(cx: &mut TestAppContext) {
        let (window, editor) = open(cx);
        cx.update_window(window, |_, window, cx| {
            editor.update(cx, |editor, cx| {
                editor.set_sql("select * from ord", window, cx);
                editor.focus(window, cx);
                editor.set_completion_scope(Some(catalog_scope()));
            });
            let provider = suggest(Some(catalog_scope()), Vec::new());
            assert!(!provider.is_completion_trigger(0, "中", cx));
            assert!(!provider.is_completion_trigger(0, " ", cx));
            assert!(provider.is_completion_trigger(0, "o", cx));
            assert!(provider.is_completion_trigger(0, ".", cx));
            window.render_frame(cx);
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn font_size_and_theme_follow_settings(cx: &mut TestAppContext) {
        let (window, editor) = open(cx);
        cx.update(|cx| {
            assert_eq!(editor.read(cx).font_size(), 13);
            assert_eq!(editor.read(cx).theme(), "auto");
            editor.update(cx, |editor, _| {
                editor.set_font_size(20);
                editor.set_theme("warm");
            });
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
        })
        .unwrap();
        cx.update(|cx| {
            assert_eq!(editor.read(cx).font_size(), 20, "渲染帧后字号仍是设置值");
            assert_eq!(editor.read(cx).theme(), "warm");
            assert_ne!(editor.read(cx).theme_colors(false), settings::editor_theme_colors("auto", false));
            editor.update(cx, |editor, _| editor.set_font_size(999));
            assert_eq!(editor.read(cx).font_size(), 24, "字号按设置范围归一");
        });
    }
}
