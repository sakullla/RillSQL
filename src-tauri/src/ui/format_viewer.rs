//! 共享的多格式查看与编辑底座。识别、树、校验和十六进制都是纯函数；
//! GPUI 面板只把这些结果画出来，供网格、记录视图、Mongo 和 Redis 挂载。

use std::collections::BTreeSet;

use base64::Engine;
use chrono::Datelike;
use gpui_kit::component::input::{Editor, EditorState, InputEvent, InputState};
use gpui_kit::component::Disableable as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, ClipboardItem, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, TestSupportExt as _, Window, div, px, relative,
};

use crate::i18n::{self, Locale};
use crate::ui::kit::{self, Palette, Radius, TypeScale};

/// 面板能切换的六种视图。手动选择不改原始文本。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataFormat {
    Json,
    Xml,
    Yaml,
    Sql,
    Text,
    Hex,
}

impl DataFormat {
    pub const ALL: [DataFormat; 6] = [Self::Json, Self::Xml, Self::Yaml, Self::Sql, Self::Text, Self::Hex];
}

/// 列名、SQL 类型、原始字节和文本。检测只读这些输入。
#[derive(Clone, Copy, Debug)]
pub struct DetectInput<'a> {
    pub column_hint: Option<&'a str>,
    pub type_hint: Option<&'a str>,
    pub bytes: Option<&'a [u8]>,
    pub text: &'a str,
}

/// 整段文本刚好是合法 UUID，或落在 1970..=2100 的 Unix 时间。提示单独复制，不回写字段。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScalarHintKind {
    Uuid { version: usize },
    Unix { millis: bool, utc: String, local: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScalarHint {
    pub kind: ScalarHintKind,
}

/// 大字段有没有完整原值。预览态禁止编辑和提交。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TruncationState {
    Full,
    Preview { total_bytes: u64, can_load_full: bool },
    Loading,
    Error(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditMode {
    ReadOnly,
    Editable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsonDiagnostic {
    pub line: usize,
    pub column: usize,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsonScalarKind {
    String,
    Number,
    Bool,
    Null,
}

/// 扁平化之后当前能看见的一个 JSON 节点。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsonTreeNode {
    pub path: String,
    pub key: String,
    pub depth: usize,
    pub badge: Option<String>,
    pub scalar: Option<String>,
    pub scalar_kind: Option<JsonScalarKind>,
    pub expandable: bool,
    pub expanded: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct YamlView {
    pub text: String,
    pub json_compatible: bool,
    /// 不是严格 YAML 库时指向双语说明；JSON 兼容时为 None。
    pub limitation_key: Option<&'static str>,
}

pub const YAML_NOT_STRICT_KEY: &str = "format.yaml.notStrict";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TruncationBanner {
    pub warning: bool,
    pub load_full: bool,
    pub edits_disabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApplyBlock {
    Preview,
    Loading,
    Truncated(String),
    ReadOnly,
    HexReadOnly,
    InvalidJson(JsonDiagnostic),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommitResult {
    Applied(String),
    Blocked { draft: String, block: ApplyBlock },
}

/// 打开面板时的输入。`text` 是原始值，检测不会改它。
pub struct ViewerSpec {
    pub text: String,
    pub column_hint: Option<String>,
    pub type_hint: Option<String>,
    pub bytes: Option<Vec<u8>>,
    pub truncation: TruncationState,
    pub mode: EditMode,
    pub locale: Locale,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FormatViewerEvent {
    LoadFull,
    Commit(String),
}

/// 可挂载的格式面板。树、校验和草稿都留在这里，调用方只订阅加载完整值和提交。
pub struct FormatViewer {
    source: String,
    draft: String,
    column_hint: Option<String>,
    type_hint: Option<String>,
    bytes: Option<Vec<u8>>,
    detected: DataFormat,
    manual: Option<DataFormat>,
    truncation: TruncationState,
    mode: EditMode,
    locale: Locale,
    code_view: bool,
    wrap: bool,
    collapsed: BTreeSet<String>,
    filter: String,
    editor_sync: String,
    last_error: Option<JsonDiagnostic>,
    editor: Entity<EditorState>,
    filter_input: Entity<InputState>,
    _editor_sub: Subscription,
    _filter_sub: Subscription,
}

impl EventEmitter<FormatViewerEvent> for FormatViewer {}

impl FormatViewer {
    pub fn new(spec: ViewerSpec, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let detected = detect_format(DetectInput {
            column_hint: spec.column_hint.as_deref(),
            type_hint: spec.type_hint.as_deref(),
            bytes: spec.bytes.as_deref(),
            text: &spec.text,
        });
        let draft = spec.text.clone();
        let editor = cx.new(|cx| {
            EditorState::new(window, cx).language(editor_language(detected)).line_number(true).soft_wrap(true).default_value(draft.clone())
        });
        let filter_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(SharedString::from(i18n::text(spec.locale, "format.filter.placeholder"))).default_value("")
        });
        let editor_sub = cx.subscribe(&editor, |this, state, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.draft = state.read(cx).value().to_string();
                this.editor_sync = this.draft.clone();
                this.refresh_validation();
                cx.notify();
            }
        });
        let filter_sub = cx.subscribe(&filter_input, |this, state, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.filter = state.read(cx).value().to_string();
                cx.notify();
            }
        });
        let mut viewer = Self {
            source: spec.text,
            draft: draft.clone(),
            column_hint: spec.column_hint,
            type_hint: spec.type_hint,
            bytes: spec.bytes,
            detected,
            manual: None,
            truncation: spec.truncation,
            mode: spec.mode,
            locale: spec.locale,
            code_view: detected != DataFormat::Json,
            wrap: true,
            collapsed: BTreeSet::new(),
            filter: String::new(),
            editor_sync: draft,
            last_error: None,
            editor,
            filter_input,
            _editor_sub: editor_sub,
            _filter_sub: filter_sub,
        };
        viewer.refresh_validation();
        viewer
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn draft(&self) -> &str {
        &self.draft
    }

    pub fn detected_format(&self) -> DataFormat {
        self.detected
    }

    pub fn manual_format(&self) -> Option<DataFormat> {
        self.manual
    }

    pub fn format(&self) -> DataFormat {
        effective_format(self.detected, self.manual)
    }

    pub fn truncation(&self) -> &TruncationState {
        &self.truncation
    }

    pub fn mode(&self) -> EditMode {
        self.mode
    }

    pub fn validation(&self) -> Option<&JsonDiagnostic> {
        self.last_error.as_ref()
    }

    pub fn hint(&self) -> Option<ScalarHint> {
        scalar_hint(&self.draft)
    }

    /// 预览、加载中、失败、只读和十六进制都不能改草稿。非法 JSON 仍可继续改。
    pub fn edits_enabled(&self) -> bool {
        !editing_locked(&self.truncation) && self.mode == EditMode::Editable && self.format() != DataFormat::Hex
    }

    pub fn set_manual_format(&mut self, manual: Option<DataFormat>, _window: &mut Window, cx: &mut Context<Self>) {
        self.manual = manual;
        if self.format() != DataFormat::Json {
            self.code_view = true;
        }
        let language = editor_language(self.format()).to_owned();
        self.editor.update(cx, |state, cx| state.set_highlighter(language, cx));
        self.refresh_validation();
        cx.notify();
    }

    pub fn set_wrap(&mut self, wrap: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.wrap = wrap;
        self.editor.update(cx, |state, cx| state.set_soft_wrap(wrap, window, cx));
        cx.notify();
    }

    pub fn set_locale(&mut self, locale: Locale, window: &mut Window, cx: &mut Context<Self>) {
        self.locale = locale;
        let placeholder = SharedString::from(i18n::text(locale, "format.filter.placeholder"));
        self.filter_input.update(cx, |state, cx| state.set_placeholder(placeholder, window, cx));
        cx.notify();
    }

    /// 完整原值到位后替换草稿，并按新文本重新识别。手动格式选择保留。
    pub fn replace_full(&mut self, text: String, bytes: Option<Vec<u8>>, _window: &mut Window, cx: &mut Context<Self>) {
        self.bytes = bytes;
        self.detected = detect_format(DetectInput {
            column_hint: self.column_hint.as_deref(),
            type_hint: self.type_hint.as_deref(),
            bytes: self.bytes.as_deref(),
            text: &text,
        });
        self.source = text.clone();
        self.draft = text;
        self.truncation = TruncationState::Full;
        self.collapsed.clear();
        self.last_error = None;
        let language = editor_language(self.format()).to_owned();
        self.editor.update(cx, |state, cx| state.set_highlighter(language, cx));
        self.refresh_validation();
        cx.notify();
    }

    pub fn commit(&mut self, cx: &mut Context<Self>) -> CommitResult {
        let result = commit_draft(self.format(), self.mode, &self.truncation, &self.draft);
        match &result {
            CommitResult::Applied(text) => {
                self.last_error = None;
                cx.emit(FormatViewerEvent::Commit(text.clone()));
            }
            CommitResult::Blocked { block: ApplyBlock::InvalidJson(error), .. } => {
                self.last_error = Some(error.clone());
            }
            CommitResult::Blocked { .. } => {}
        }
        cx.notify();
        result
    }

    pub fn pretty(&mut self, cx: &mut Context<Self>) {
        self.apply_text_result(pretty_json(&self.draft), cx);
    }

    pub fn minify(&mut self, cx: &mut Context<Self>) {
        self.apply_text_result(minify_json(&self.draft), cx);
    }

    pub fn reformat(&mut self, cx: &mut Context<Self>) {
        if !self.edits_enabled() {
            return;
        }
        let next = match self.format() {
            DataFormat::Json => return self.pretty(cx),
            DataFormat::Xml => format_xml(&self.draft),
            DataFormat::Sql => format_sql(&self.draft),
            DataFormat::Yaml => yaml_view(&self.draft).text,
            DataFormat::Text | DataFormat::Hex => return,
        };
        self.draft = next;
        self.last_error = None;
        cx.notify();
    }

    pub fn toggle_node(&mut self, path: &str, cx: &mut Context<Self>) {
        if !self.collapsed.remove(path) {
            self.collapsed.insert(path.to_owned());
        }
        cx.notify();
    }

    pub fn expand_all(&mut self, cx: &mut Context<Self>) {
        self.collapsed.clear();
        cx.notify();
    }

    pub fn collapse_all(&mut self, cx: &mut Context<Self>) {
        self.collapsed.clear();
        if let Ok(value) = parse_json(&self.draft) {
            collect_containers(&value, "$", &mut self.collapsed);
        }
        cx.notify();
    }

    fn apply_text_result(&mut self, result: Result<String, JsonDiagnostic>, cx: &mut Context<Self>) {
        if !self.edits_enabled() {
            return;
        }
        match result {
            Ok(text) => {
                self.draft = text;
                self.last_error = None;
            }
            Err(error) => self.last_error = Some(error),
        }
        cx.notify();
    }

    fn refresh_validation(&mut self) {
        self.last_error = if self.format() == DataFormat::Json { validate_json(&self.draft).err() } else { None };
    }

    fn push_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let draft = self.draft.clone();
        kit::sync_editor(&self.editor, &mut self.editor_sync, &draft, window, cx);
    }

    fn text(&self, key: &str) -> SharedString {
        SharedString::from(i18n::text(self.locale, key))
    }
}

impl Render for FormatViewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.push_draft(window, cx);
        let p = Palette::of(cx);
        let metrics = kit::Metrics::of(cx);
        let entity = cx.entity();
        let format = self.format();
        let locked = !self.edits_enabled();
        let banner = banner_line(self, &entity, &p);
        let hint = hint_line(self, &entity, &p);
        let body = if format == DataFormat::Hex {
            hex_body(self, &p)
        } else if format == DataFormat::Json && !self.code_view {
            tree_body(self, &entity, &p)
        } else {
            code_body(self, &p, locked)
        };
        div()
            .id("format-viewer")
            .size_full()
            .min_w(px(0.))
            .min_h(px(0.))
            .flex()
            .flex_col()
            .bg(p.surface)
            .child(toolbar(self, &entity, &p, &metrics, locked))
            .children(banner)
            .children(hint)
            .child(body)
    }
}

fn toolbar(viewer: &FormatViewer, entity: &Entity<FormatViewer>, p: &Palette, metrics: &kit::Metrics, locked: bool) -> AnyElement {
    let mut formats = div().flex().flex_wrap().gap(px(4.));
    for format in DataFormat::ALL {
        let selected = viewer.format() == format;
        let target = entity.clone();
        let label = viewer.text(format_key(format));
        formats = formats.child(
            kit::interactive_states(div().id(SharedString::from(format!("format-pick-{format:?}"))), p, false)
                .test_support()
                .role(gpui_kit::Role::Button)
                .aria_label(label.clone())
                .px(px(8.))
                .h(px((metrics.control_h - 4.).max(22.)))
                .flex()
                .items_center()
                .rounded(px(Radius::CONTROL))
                .text_size(px(TypeScale::SM))
                .when(selected, |this| this.bg(p.accent_soft).text_color(p.accent_text).font_weight(gpui_kit::FontWeight::MEDIUM))
                .when(!selected, |this| this.text_color(p.text_muted))
                .child(label)
                .on_click(move |_, window, cx| {
                    target.update(cx, |this, cx| this.set_manual_format(Some(format), window, cx));
                }),
        );
    }
    let mut actions = div().flex().flex_wrap().items_center().gap(px(4.));
    if viewer.format() == DataFormat::Json {
        actions = actions.child(mode_chip(entity, viewer, p, false)).child(mode_chip(entity, viewer, p, true));
        actions = actions.child(text_button(entity, "format-pretty", viewer.text("format.action.pretty"), locked, |this, cx| this.pretty(cx)));
        actions = actions.child(text_button(entity, "format-minify", viewer.text("format.action.minify"), locked, |this, cx| this.minify(cx)));
        actions = actions.child(text_button(entity, "format-expand", viewer.text("format.action.expandAll"), false, |this, cx| this.expand_all(cx)));
        actions = actions.child(text_button(entity, "format-collapse", viewer.text("format.action.collapseAll"), false, |this, cx| this.collapse_all(cx)));
    } else if viewer.format() != DataFormat::Hex {
        actions = actions.child(text_button(entity, "format-reformat", viewer.text("format.action.pretty"), locked, |this, cx| this.reformat(cx)));
    }
    if viewer.format() != DataFormat::Hex && (viewer.format() != DataFormat::Json || viewer.code_view) {
        actions = actions.child(wrap_chip(entity, viewer, p));
    }
    actions = actions.child(text_button(entity, "format-copy", viewer.text("format.action.copyAll"), false, |this, cx| {
        let text = if this.format() == DataFormat::Hex { hex_dump_source(&this.draft, this.bytes.as_deref()) } else { this.draft.clone() };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }));
    let apply_locked = apply_block(viewer.format(), viewer.mode, &viewer.truncation, &viewer.draft).is_some();
    actions = actions.child(text_button(entity, "format-apply", viewer.text("format.action.apply"), apply_locked, |this, cx| {
        let _ = this.commit(cx);
    }));
    div()
        .flex_none()
        .px(px(metrics.pane_pad()))
        .py(px(6.))
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(8.))
        .border_b_1()
        .border_color(p.line)
        .child(formats)
        .child(actions)
        .into_any_element()
}

fn mode_chip(entity: &Entity<FormatViewer>, viewer: &FormatViewer, p: &Palette, code: bool) -> AnyElement {
    let selected = viewer.code_view == code;
    let key = if code { "format.view.code" } else { "format.view.tree" };
    let id = if code { "format-view-code" } else { "format-view-tree" };
    let label = viewer.text(key);
    let target = entity.clone();
    kit::interactive_states(div().id(id), p, false)
        .test_support()
        .role(gpui_kit::Role::Button)
        .aria_label(label.clone())
        .px(px(8.))
        .h(px(22.))
        .flex()
        .items_center()
        .rounded(px(Radius::CONTROL))
        .text_size(px(TypeScale::SM))
        .when(selected, |this| this.bg(p.surface).text_color(p.text))
        .when(!selected, |this| this.text_color(p.text_muted))
        .child(label)
        .on_click(move |_, _, cx| {
            target.update(cx, |this, cx| {
                this.code_view = code;
                cx.notify();
            });
        })
        .into_any_element()
}

fn wrap_chip(entity: &Entity<FormatViewer>, viewer: &FormatViewer, p: &Palette) -> AnyElement {
    let selected = viewer.wrap;
    let label = viewer.text("format.action.wrap");
    let target = entity.clone();
    kit::interactive_states(div().id("format-wrap"), p, false)
        .test_support()
        .role(gpui_kit::Role::Button)
        .aria_label(label.clone())
        .px(px(8.))
        .h(px(22.))
        .flex()
        .items_center()
        .rounded(px(Radius::CONTROL))
        .text_size(px(TypeScale::SM))
        .when(selected, |this| this.bg(p.surface).text_color(p.text))
        .when(!selected, |this| this.text_color(p.text_muted))
        .child(label)
        .on_click(move |_, window, cx| {
            target.update(cx, |this, cx| {
                let next = !this.wrap;
                this.set_wrap(next, window, cx);
            });
        })
        .into_any_element()
}

fn text_button(
    entity: &Entity<FormatViewer>,
    id: impl Into<gpui_kit::ElementId>,
    label: SharedString,
    disabled: bool,
    action: impl Fn(&mut FormatViewer, &mut Context<FormatViewer>) + 'static,
) -> gpui_kit::component::button::Button {
    let target = entity.clone();
    kit::tool_button(id, None, label, move |_, _, cx| {
        target.update(cx, |this, cx| action(this, cx));
    })
    .disabled(disabled)
}

fn banner_line(viewer: &FormatViewer, entity: &Entity<FormatViewer>, p: &Palette) -> Option<AnyElement> {
    let banner = truncation_banner(&viewer.truncation);
    if !banner.warning && !banner.load_full {
        return None;
    }
    let (message, can_load) = match &viewer.truncation {
        TruncationState::Preview { total_bytes, can_load_full } => {
            let text = i18n::fill(&i18n::text(viewer.locale, "format.banner.preview"), &[("bytes", &total_bytes.to_string())]);
            let extra = if *can_load_full { String::new() } else { format!(" {}", i18n::text(viewer.locale, "format.banner.noLocator")) };
            (format!("{text}{extra}"), *can_load_full)
        }
        TruncationState::Loading => (i18n::text(viewer.locale, "format.banner.loading"), false),
        TruncationState::Error(message) => (message.clone(), true),
        TruncationState::Full => return None,
    };
    let load = viewer.text("format.action.loadFull");
    Some(
        div()
            .flex_none()
            .px(px(12.))
            .py(px(8.))
            .flex()
            .items_center()
            .gap(px(8.))
            .bg(p.warn_soft)
            .text_color(p.warn)
            .text_size(px(TypeScale::SM))
            .child(div().flex_1().min_w(px(0.)).child(SharedString::from(message)))
            .when(banner.load_full, |this| {
                this.child(text_button(entity, "format-load-full", load, !can_load, |_, cx| {
                    cx.emit(FormatViewerEvent::LoadFull);
                }))
            })
            .into_any_element(),
    )
}

fn hint_line(viewer: &FormatViewer, entity: &Entity<FormatViewer>, p: &Palette) -> Option<AnyElement> {
    let hint = viewer.hint()?;
    let text = scalar_hint_text(viewer.locale, &hint);
    let copy = text.clone();
    let label = viewer.text("format.action.copyValue");
    Some(
        div()
            .flex_none()
            .px(px(12.))
            .py(px(6.))
            .flex()
            .items_center()
            .gap(px(8.))
            .bg(p.sunken)
            .border_b_1()
            .border_color(p.line)
            .text_size(px(TypeScale::SM))
            .text_color(p.text_muted)
            .child(div().flex_1().min_w(px(0.)).child(SharedString::from(text)))
            .child(text_button(entity, "format-copy-hint", label, false, move |_, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
            }))
            .into_any_element(),
    )
}

fn code_body(viewer: &FormatViewer, p: &Palette, locked: bool) -> AnyElement {
    let mut column = div().flex_1().min_h(px(0.)).flex().flex_col().bg(p.sunken);
    if viewer.format() == DataFormat::Yaml && !yaml_view(&viewer.draft).json_compatible {
        column = column.child(
            div()
                .flex_none()
                .px(px(12.))
                .py(px(6.))
                .text_size(px(TypeScale::SM))
                .text_color(p.text_muted)
                .child(viewer.text(YAML_NOT_STRICT_KEY)),
        );
    }
    if let Some(error) = &viewer.last_error {
        let message = i18n::fill(
            &i18n::text(viewer.locale, "format.error.json"),
            &[("line", &error.line.to_string()), ("column", &error.column.to_string()), ("message", &error.message)],
        );
        column = column.child(
            div().flex_none().px(px(12.)).py(px(6.)).text_size(px(TypeScale::SM)).text_color(p.danger).child(SharedString::from(message)),
        );
    }
    column
        .child(
            div().flex_1().min_h(px(120.)).child(
                Editor::new(&viewer.editor)
                    .h(relative(1.))
                    .bordered(false)
                    .readonly(locked)
                    .aria_label(viewer.text("format.editor.label")),
            ),
        )
        .into_any_element()
}

fn hex_body(viewer: &FormatViewer, p: &Palette) -> AnyElement {
    let dump = hex_dump_source(&viewer.draft, viewer.bytes.as_deref());
    div()
        .id("format-hex-dump")
        .flex_1()
        .min_h(px(0.))
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap(px(8.))
        .p(px(12.))
        .bg(p.sunken)
        .text_size(px(TypeScale::SM))
        .text_color(p.text)
        .child(div().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(viewer.text("format.hex.title")))
        .child(SharedString::from(dump))
        .into_any_element()
}

fn tree_body(viewer: &FormatViewer, entity: &Entity<FormatViewer>, p: &Palette) -> AnyElement {
    let parsed = parse_json(&viewer.draft);
    let mut column = div().flex_1().min_h(px(0.)).flex().flex_col().bg(p.surface);
    column = column.child(
        div()
            .flex_none()
            .px(px(8.))
            .py(px(6.))
            .child(kit::text_input("format-filter", &viewer.filter_input, viewer.text("format.filter.placeholder"))),
    );
    match parsed {
        Ok(value) => {
            let nodes = visible_json_nodes(&value, &viewer.collapsed, &viewer.filter);
            if nodes.is_empty() {
                column = column.child(div().p(px(12.)).text_size(px(TypeScale::SM)).text_color(p.text_muted).child(viewer.text("format.tree.empty")));
            }
            let mut list = div().id("format-json-tree").flex_1().min_h(px(0.)).overflow_y_scroll();
            for node in nodes {
                list = list.child(tree_row(entity, viewer, p, &node));
            }
            column = column.child(list);
        }
        Err(error) => {
            let message = i18n::fill(
                &i18n::text(viewer.locale, "format.error.json"),
                &[("line", &error.line.to_string()), ("column", &error.column.to_string()), ("message", &error.message)],
            );
            column = column.child(div().p(px(12.)).text_size(px(TypeScale::SM)).text_color(p.danger).child(SharedString::from(message)));
        }
    }
    column.into_any_element()
}

fn tree_row(entity: &Entity<FormatViewer>, viewer: &FormatViewer, p: &Palette, node: &JsonTreeNode) -> AnyElement {
    let path = node.path.clone();
    let value_label = viewer.text("format.action.copyValue");
    let path_label = viewer.text("format.action.copyPath");
    let value_path = path.clone();
    let copied_path = path.clone();
    let toggle_path = path.clone();
    let value_id = format!("format-copy-value-{value_path}");
    let path_id = format!("format-copy-path-{copied_path}");
    let expandable = node.expandable;
    let toggle = entity.clone();
    div()
        .flex()
        .items_center()
        .gap(px(6.))
        .pl(px(8. + node.depth as f32 * 14.))
        .pr(px(8.))
        .h(px(26.))
        .min_w(px(0.))
        .hover(|style| style.bg(p.hover))
        .child(
            kit::interactive_states(div().id(SharedString::from(format!("format-toggle-{path}"))), p, !expandable)
                .w(px(16.))
                .text_size(px(TypeScale::SM))
                .text_color(p.text_faint)
                .child(if expandable { if node.expanded { "▾" } else { "▸" } } else { "" })
                .when(expandable, move |this| {
                    this.on_click(move |_, _, cx| {
                        toggle.update(cx, |this, cx| this.toggle_node(&toggle_path, cx));
                    })
                }),
        )
        .child(div().text_size(px(TypeScale::SM)).text_color(p.text).child(SharedString::from(node.key.clone())))
        .children(node.badge.clone().map(|badge| {
            div()
                .px(px(4.))
                .rounded(px(Radius::TAG))
                .bg(p.accent_soft)
                .text_size(px(TypeScale::XS))
                .text_color(p.accent_text)
                .child(SharedString::from(badge))
        }))
        .children(node.scalar.clone().map(|scalar| {
            let color = node.scalar_kind.map(|kind| scalar_color(kind, p)).unwrap_or(p.text);
            div().flex_1().min_w(px(0.)).truncate().text_size(px(TypeScale::SM)).text_color(color).child(SharedString::from(scalar))
        }))
        .child(text_button(entity, value_id, value_label, false, move |this, cx| {
            if let Ok(value) = parse_json(&this.draft) {
                if let Some(text) = copy_node_value(&value, &value_path) {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
            }
        }))
        .child(text_button(entity, path_id, path_label, false, move |_, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(copied_path.clone()));
        }))
        .into_any_element()
}

fn scalar_color(kind: JsonScalarKind, p: &Palette) -> gpui_kit::Hsla {
    match kind {
        JsonScalarKind::String => p.info,
        JsonScalarKind::Number => p.ok,
        JsonScalarKind::Bool => p.accent_text,
        JsonScalarKind::Null => p.text_faint,
    }
}

// ---------------------------------------------------------------- 纯函数

pub fn format_key(format: DataFormat) -> &'static str {
    match format {
        DataFormat::Json => "format.name.json",
        DataFormat::Xml => "format.name.xml",
        DataFormat::Yaml => "format.name.yaml",
        DataFormat::Sql => "format.name.sql",
        DataFormat::Text => "format.name.text",
        DataFormat::Hex => "format.name.hex",
    }
}

pub fn editor_language(format: DataFormat) -> &'static str {
    match format {
        DataFormat::Json => "json",
        DataFormat::Xml => "xml",
        DataFormat::Yaml => "yaml",
        DataFormat::Sql => "sql",
        DataFormat::Text | DataFormat::Hex => "text",
    }
}

/// 自动识别。`manual` 交给 [`effective_format`]，这里不改输入。
pub fn detect_format(input: DetectInput<'_>) -> DataFormat {
    if is_binary_envelope(input.text) {
        return DataFormat::Hex;
    }
    if let Some(format) = hint_format(input.type_hint.unwrap_or("")).or_else(|| hint_format(input.column_hint.unwrap_or(""))) {
        return format;
    }
    let body = unwrap_text_envelope(input.text).unwrap_or(input.text);
    let binary_bytes = input.bytes.is_some_and(|bytes| bytes.iter().copied().any(is_binary_byte));
    let binary_text = body.chars().any(|ch| ch.is_control() && ch != '\t' && ch != '\n' && ch != '\r');
    if binary_bytes || binary_text {
        return DataFormat::Hex;
    }
    detect_content(body)
}

pub fn effective_format(detected: DataFormat, manual: Option<DataFormat>) -> DataFormat {
    manual.unwrap_or(detected)
}

pub fn scalar_hint(raw: &str) -> Option<ScalarHint> {
    let text = raw.trim();
    if text.is_empty() {
        return None;
    }
    if let Ok(parsed) = uuid::Uuid::parse_str(text) {
        return Some(ScalarHint { kind: ScalarHintKind::Uuid { version: parsed.get_version_num() } });
    }
    unix_hint(text)
}

pub fn scalar_hint_text(locale: Locale, hint: &ScalarHint) -> String {
    match &hint.kind {
        ScalarHintKind::Uuid { version } => i18n::fill(&i18n::text(locale, "format.hint.uuid"), &[("version", &version.to_string())]),
        ScalarHintKind::Unix { utc, local, .. } => i18n::fill(&i18n::text(locale, "format.hint.time"), &[("utc", utc), ("local", local)]),
    }
}

pub fn editing_locked(truncation: &TruncationState) -> bool {
    !matches!(truncation, TruncationState::Full)
}

pub fn truncation_banner(state: &TruncationState) -> TruncationBanner {
    match state {
        TruncationState::Full => TruncationBanner { warning: false, load_full: false, edits_disabled: false },
        TruncationState::Preview { .. } => TruncationBanner { warning: true, load_full: true, edits_disabled: true },
        TruncationState::Loading => TruncationBanner { warning: true, load_full: false, edits_disabled: true },
        TruncationState::Error(_) => TruncationBanner { warning: true, load_full: true, edits_disabled: true },
    }
}

pub fn apply_block(format: DataFormat, mode: EditMode, truncation: &TruncationState, draft: &str) -> Option<ApplyBlock> {
    match truncation {
        TruncationState::Preview { .. } => return Some(ApplyBlock::Preview),
        TruncationState::Loading => return Some(ApplyBlock::Loading),
        TruncationState::Error(message) => return Some(ApplyBlock::Truncated(message.clone())),
        TruncationState::Full => {}
    }
    if mode != EditMode::Editable {
        return Some(ApplyBlock::ReadOnly);
    }
    if format == DataFormat::Hex {
        return Some(ApplyBlock::HexReadOnly);
    }
    if format == DataFormat::Json {
        if let Err(error) = validate_json(draft) {
            return Some(ApplyBlock::InvalidJson(error));
        }
    }
    None
}

pub fn commit_draft(format: DataFormat, mode: EditMode, truncation: &TruncationState, draft: &str) -> CommitResult {
    match apply_block(format, mode, truncation, draft) {
        Some(block) => CommitResult::Blocked { draft: draft.to_owned(), block },
        None => CommitResult::Applied(draft.to_owned()),
    }
}

pub fn parse_json(text: &str) -> Result<serde_json::Value, JsonDiagnostic> {
    serde_json::from_str(text).map_err(json_diagnostic)
}

pub fn validate_json(text: &str) -> Result<(), JsonDiagnostic> {
    parse_json(text).map(|_| ())
}

pub fn pretty_json(text: &str) -> Result<String, JsonDiagnostic> {
    serde_json::to_string_pretty(&parse_json(text)?).map_err(json_diagnostic)
}

pub fn minify_json(text: &str) -> Result<String, JsonDiagnostic> {
    serde_json::to_string(&parse_json(text)?).map_err(json_diagnostic)
}

pub fn visible_json_nodes(value: &serde_json::Value, collapsed: &BTreeSet<String>, filter: &str) -> Vec<JsonTreeNode> {
    let mut out = Vec::new();
    let _ = walk_json(value, "$", "$", 0, collapsed, filter.trim(), &mut out);
    out
}

pub fn value_at_path<'a>(value: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    if path == "$" {
        return Some(value);
    }
    let rest = path.strip_prefix('$')?;
    walk_path(value, rest)
}

pub fn copy_node_value(value: &serde_json::Value, path: &str) -> Option<String> {
    let node = value_at_path(value, path)?;
    Some(match node {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Null => "null".to_owned(),
        other => serde_json::to_string(other).unwrap_or_default(),
    })
}

pub fn yaml_view(text: &str) -> YamlView {
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(value) => YamlView {
            text: serde_json::to_string_pretty(&value).unwrap_or_else(|_| text.to_owned()),
            json_compatible: true,
            limitation_key: None,
        },
        Err(_) => YamlView { text: text.replace('\t', "  "), json_compatible: false, limitation_key: Some(YAML_NOT_STRICT_KEY) },
    }
}

pub fn format_xml(input: &str) -> String {
    if !input.contains('<') {
        return input.trim().to_owned();
    }
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::new();
    let mut depth = 0usize;
    let mut index = 0usize;
    while index < chars.len() {
        if chars[index] != '<' {
            let start = index;
            while index < chars.len() && chars[index] != '<' {
                index += 1;
            }
            let text: String = chars[start..index].iter().collect();
            let text = text.trim();
            if !text.is_empty() {
                push_xml_line(&mut out, depth, text);
            }
            continue;
        }
        let end = xml_tag_end(&chars, index);
        let tag: String = chars[index..=end].iter().collect();
        let closing = tag.starts_with("</");
        let declaration = tag.starts_with("<?") || tag.starts_with("<!");
        let self_closing = tag.ends_with("/>") || declaration;
        if closing {
            depth = depth.saturating_sub(1);
        }
        push_xml_line(&mut out, depth, tag.trim());
        if !closing && !self_closing {
            depth += 1;
        }
        index = end + 1;
    }
    out.trim_end().to_owned()
}

pub fn format_sql(input: &str) -> String {
    let tokens = sql_tokens(input);
    let mut out = String::new();
    let mut depth = 0i32;
    for token in tokens {
        if token == ")" {
            depth -= 1;
        }
        let clause = depth == 0 && is_sql_clause(&token);
        if clause && !out.is_empty() {
            out.push('\n');
        } else if sql_needs_gap(&out, &token) {
            out.push(' ');
        }
        out.push_str(&token);
        if token == "(" {
            depth += 1;
        }
    }
    out.trim().to_owned()
}

pub fn hex_dump(bytes: &[u8]) -> String {
    const HEX_WIDTH: usize = 16 * 2 + 15;
    let mut lines = Vec::new();
    for (row, chunk) in bytes.chunks(16).enumerate() {
        let mut hex = String::new();
        let mut ascii = String::new();
        for (index, byte) in chunk.iter().enumerate() {
            if index > 0 {
                hex.push(' ');
            }
            hex.push_str(&format!("{byte:02x}"));
            ascii.push(if byte.is_ascii_graphic() || *byte == b' ' { char::from(*byte) } else { '.' });
        }
        lines.push(format!("{:08x} | {:<width$} | {ascii}", row * 16, hex, width = HEX_WIDTH));
    }
    lines.join("\n")
}

pub fn hex_dump_source(text: &str, raw: Option<&[u8]>) -> String {
    if let Some(raw) = raw {
        return hex_dump(raw);
    }
    if is_binary_envelope(text) {
        return String::new();
    }
    if let Some(bytes) = decode_hex_text(text) {
        return hex_dump(&bytes);
    }
    if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(text.trim()) {
        if !bytes.is_empty() {
            return hex_dump(&bytes);
        }
    }
    hex_dump(text.as_bytes())
}

fn json_diagnostic(error: serde_json::Error) -> JsonDiagnostic {
    JsonDiagnostic { line: error.line(), column: error.column(), message: error.to_string() }
}

fn hint_format(hint: &str) -> Option<DataFormat> {
    if hint.is_empty() {
        return None;
    }
    let mut token = String::new();
    let mut tokens = Vec::new();
    for ch in hint.chars() {
        if ch.is_ascii_alphanumeric() {
            token.push(ch.to_ascii_lowercase());
        } else if !token.is_empty() {
            tokens.push(std::mem::take(&mut token));
        }
    }
    if !token.is_empty() {
        tokens.push(token);
    }
    for token in tokens {
        match token.as_str() {
            "json" | "jsonb" => return Some(DataFormat::Json),
            "xml" => return Some(DataFormat::Xml),
            "bytea" | "blob" | "tinyblob" | "mediumblob" | "longblob" | "binary" | "varbinary" | "image" => return Some(DataFormat::Hex),
            _ => {}
        }
    }
    None
}

fn is_binary_byte(byte: u8) -> bool {
    (byte < 0x20 && !matches!(byte, b'\t' | b'\n' | b'\r')) || byte == 0x7f
}

fn is_binary_envelope(text: &str) -> bool {
    let Some(body) = text.strip_prefix('\u{1}').and_then(|rest| rest.strip_prefix("b:")) else {
        return false;
    };
    !body.is_empty() && body.bytes().all(|byte| byte.is_ascii_digit())
}

fn unwrap_text_envelope(text: &str) -> Option<&str> {
    let body = text.strip_prefix('\u{1}')?.strip_prefix("t:")?;
    let (count, preview) = body.split_once(':')?;
    if count.is_empty() || !count.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some(preview)
}

fn detect_content(text: &str) -> DataFormat {
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    if trimmed.is_empty() {
        return DataFormat::Text;
    }
    let mut chars = trimmed.chars();
    match chars.next() {
        Some('{' | '[') => return DataFormat::Json,
        Some('<') => {
            if matches!(chars.next(), Some(ch) if ch.is_ascii_alphabetic() || matches!(ch, '?' | '!' | '/')) {
                return DataFormat::Xml;
            }
        }
        _ => {}
    }
    if looks_like_sql(trimmed) {
        DataFormat::Sql
    } else if looks_like_yaml(trimmed) {
        DataFormat::Yaml
    } else {
        DataFormat::Text
    }
}

fn looks_like_sql(text: &str) -> bool {
    let head = text.split(|ch: char| ch.is_whitespace() || ch == '(').find(|word| !word.is_empty()).unwrap_or("");
    matches!(
        head.to_ascii_lowercase().as_str(),
        "select" | "insert" | "update" | "delete" | "with" | "create" | "alter" | "drop" | "begin" | "explain" | "show" | "pragma" | "merge" | "truncate"
    )
}

fn looks_like_yaml(text: &str) -> bool {
    let trimmed = text.trim_start();
    if trimmed.starts_with("---") {
        return true;
    }
    trimmed.lines().any(|line| {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("- ") {
            return !rest.is_empty();
        }
        let Some((key, _)) = line.split_once(':') else {
            return false;
        };
        let key = key.trim();
        !key.is_empty() && key.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-') && key.chars().next().is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
    })
}

fn unix_hint(text: &str) -> Option<ScalarHint> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let value: i64 = text.parse().ok()?;
    const MILLIS_FLOOR: i64 = 100_000_000_000;
    if value >= MILLIS_FLOOR {
        let secs = value.div_euclid(1000);
        let nanos = u32::try_from(value.rem_euclid(1000)).ok()? * 1_000_000;
        return timestamp_hint(secs, nanos, true);
    }
    timestamp_hint(value, 0, false)
}

fn timestamp_hint(secs: i64, nanos: u32, millis: bool) -> Option<ScalarHint> {
    let utc = chrono::DateTime::<chrono::Utc>::from_timestamp(secs, nanos)?;
    let year = utc.year();
    if !(1970..=2100).contains(&year) {
        return None;
    }
    let local = utc.with_timezone(&chrono::Local);
    let pattern = "%Y-%m-%d %H:%M:%S";
    Some(ScalarHint {
        kind: ScalarHintKind::Unix { millis, utc: utc.format(pattern).to_string(), local: local.format(pattern).to_string() },
    })
}

fn walk_json(value: &serde_json::Value, path: &str, key: &str, depth: usize, collapsed: &BTreeSet<String>, filter: &str, out: &mut Vec<JsonTreeNode>) -> bool {
    let self_match = filter.is_empty() || node_matches(key, value, filter);
    let expandable = value.is_object() || value.is_array();
    let hide_children = expandable && collapsed.contains(path) && filter.is_empty();
    let mut children = Vec::new();
    let mut child_hit = false;
    if expandable && !hide_children {
        match value {
            serde_json::Value::Object(map) => {
                for (child_key, child) in map {
                    let child_path = json_key_path(path, child_key);
                    child_hit |= walk_json(child, &child_path, child_key, depth + 1, collapsed, filter, &mut children);
                }
            }
            serde_json::Value::Array(items) => {
                for (index, child) in items.iter().enumerate() {
                    let child_path = format!("{path}[{index}]");
                    let child_key = format!("[{index}]");
                    child_hit |= walk_json(child, &child_path, &child_key, depth + 1, collapsed, filter, &mut children);
                }
            }
            _ => {}
        }
    } else if expandable && filter.is_empty() {
        child_hit = true;
    }
    let visible = filter.is_empty() || self_match || child_hit;
    if !visible {
        return false;
    }
    let (scalar_kind, scalar) = scalar_parts(value);
    out.push(JsonTreeNode {
        path: path.to_owned(),
        key: key.to_owned(),
        depth,
        badge: container_badge(value),
        scalar,
        scalar_kind,
        expandable,
        expanded: expandable && !hide_children,
    });
    out.append(&mut children);
    self_match || child_hit || filter.is_empty()
}

fn node_matches(key: &str, value: &serde_json::Value, filter: &str) -> bool {
    let needle = filter.to_lowercase();
    if key.to_lowercase().contains(&needle) {
        return true;
    }
    match value {
        serde_json::Value::String(text) => text.to_lowercase().contains(&needle),
        serde_json::Value::Number(number) => number.to_string().contains(&needle),
        serde_json::Value::Bool(flag) => flag.to_string().contains(&needle),
        serde_json::Value::Null => "null".contains(&needle),
        _ => false,
    }
}

fn scalar_parts(value: &serde_json::Value) -> (Option<JsonScalarKind>, Option<String>) {
    match value {
        serde_json::Value::String(text) => (Some(JsonScalarKind::String), Some(text.clone())),
        serde_json::Value::Number(number) => (Some(JsonScalarKind::Number), Some(number.to_string())),
        serde_json::Value::Bool(flag) => (Some(JsonScalarKind::Bool), Some(flag.to_string())),
        serde_json::Value::Null => (Some(JsonScalarKind::Null), Some("null".to_owned())),
        _ => (None, None),
    }
}

fn container_badge(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Object(map) => Some(format!("{{{}}}", map.len())),
        serde_json::Value::Array(items) => Some(format!("[{}]", items.len())),
        _ => None,
    }
}

fn json_key_path(parent: &str, key: &str) -> String {
    if is_path_ident(key) {
        format!("{parent}.{key}")
    } else {
        format!("{parent}[{}]", serde_json::to_string(key).unwrap_or_else(|_| "\"\"".to_owned()))
    }
}

fn is_path_ident(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(ch) if ch.is_ascii_alphabetic() || ch == '_' => chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_'),
        _ => false,
    }
}

fn walk_path<'a>(value: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    if path.is_empty() {
        return Some(value);
    }
    let bytes = path.as_bytes();
    if bytes[0] == b'.' {
        let rest = &path[1..];
        let (key, tail) = split_path_key(rest);
        let child = value.as_object()?.get(key)?;
        return walk_path(child, tail);
    }
    if bytes[0] == b'[' {
        if path.starts_with("[\"") {
            let end = path[1..].find(']')?;
            let literal = &path[1..1 + end];
            let key = serde_json::from_str::<String>(literal).ok()?;
            let child = value.as_object()?.get(&key)?;
            return walk_path(child, &path[2 + end..]);
        }
        let end = path.find(']')?;
        let index: usize = path[1..end].parse().ok()?;
        let child = value.as_array()?.get(index)?;
        return walk_path(child, &path[end + 1..]);
    }
    None
}

fn split_path_key(path: &str) -> (&str, &str) {
    let end = path.find(['.', '[']).unwrap_or(path.len());
    (&path[..end], &path[end..])
}

fn collect_containers(value: &serde_json::Value, path: &str, into: &mut BTreeSet<String>) {
    if !(value.is_object() || value.is_array()) {
        return;
    }
    into.insert(path.to_owned());
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                collect_containers(child, &json_key_path(path, key), into);
            }
        }
        serde_json::Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                collect_containers(child, &format!("{path}[{index}]"), into);
            }
        }
        _ => {}
    }
}

fn xml_tag_end(chars: &[char], start: usize) -> usize {
    if chars[start..].starts_with(&['<', '!', '-', '-']) {
        return find_marker(chars, start, &['-', '-', '>']).unwrap_or(chars.len() - 1);
    }
    if chars[start..].starts_with(&['<', '!', '[', 'C', 'D', 'A', 'T', 'A', '[']) {
        return find_marker(chars, start, &[']', ']', '>']).unwrap_or(chars.len() - 1);
    }
    let mut quote = None;
    let mut index = start + 1;
    while index < chars.len() {
        let ch = chars[index];
        if let Some(current) = quote {
            if ch == current {
                quote = None;
            }
        } else if ch == '"' || ch == '\'' {
            quote = Some(ch);
        } else if ch == '>' {
            return index;
        }
        index += 1;
    }
    chars.len().saturating_sub(1)
}

fn find_marker(chars: &[char], start: usize, marker: &[char]) -> Option<usize> {
    if chars.len() < marker.len() {
        return None;
    }
    let mut index = start;
    while index + marker.len() <= chars.len() {
        if &chars[index..index + marker.len()] == marker {
            return Some(index + marker.len() - 1);
        }
        index += 1;
    }
    None
}

fn push_xml_line(out: &mut String, depth: usize, text: &str) {
    if !out.is_empty() {
        out.push('\n');
    }
    for _ in 0..depth {
        out.push_str("  ");
    }
    out.push_str(text);
}

fn sql_tokens(input: &str) -> Vec<String> {
    let chars: Vec<char> = input.chars().collect();
    let mut tokens = Vec::new();
    let mut index = 0usize;
    while index < chars.len() {
        let ch = chars[index];
        if ch.is_whitespace() {
            index += 1;
            continue;
        }
        if ch == '-' && chars.get(index + 1) == Some(&'-') {
            let start = index;
            index += 2;
            while index < chars.len() && chars[index] != '\n' {
                index += 1;
            }
            tokens.push(chars[start..index].iter().collect());
            continue;
        }
        if ch == '/' && chars.get(index + 1) == Some(&'*') {
            let start = index;
            index += 2;
            while index + 1 < chars.len() && !(chars[index] == '*' && chars[index + 1] == '/') {
                index += 1;
            }
            index = (index + 2).min(chars.len());
            tokens.push(chars[start..index].iter().collect());
            continue;
        }
        if ch == '\'' || ch == '"' || ch == '`' {
            let start = index;
            index += 1;
            while index < chars.len() {
                if chars[index] == ch {
                    index += 1;
                    if chars.get(index) == Some(&ch) {
                        index += 1;
                        continue;
                    }
                    break;
                }
                index += 1;
            }
            tokens.push(chars[start..index].iter().collect());
            continue;
        }
        if ch.is_ascii_alphanumeric() || ch == '_' {
            let start = index;
            index += 1;
            while index < chars.len() && (chars[index].is_ascii_alphanumeric() || chars[index] == '_') {
                index += 1;
            }
            tokens.push(chars[start..index].iter().collect());
            continue;
        }
        tokens.push(ch.to_string());
        index += 1;
    }
    tokens
}

fn is_sql_clause(token: &str) -> bool {
    matches!(
        token.to_ascii_lowercase().as_str(),
        "select" | "from" | "where" | "group" | "order" | "having" | "limit" | "offset" | "insert" | "values" | "update" | "set" | "delete" | "join"
            | "on" | "union" | "with" | "returning" | "and" | "or"
    )
}

fn sql_needs_gap(out: &str, token: &str) -> bool {
    if out.is_empty() || out.ends_with('\n') || out.ends_with('(') || out.ends_with('.') {
        return false;
    }
    !matches!(token, "(" | "," | ")" | "." | ";")
}

fn decode_hex_text(text: &str) -> Option<Vec<u8>> {
    let stripped: String = text.chars().filter(|ch| !ch.is_ascii_whitespace()).collect();
    if stripped.is_empty() || stripped.len() % 2 != 0 || !stripped.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return None;
    }
    let bytes = stripped.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let mut index = 0;
    while index < bytes.len() {
        let hi = hex_value(bytes[index])?;
        let lo = hex_value(bytes[index + 1])?;
        out.push((hi << 4) | lo);
        index += 2;
    }
    Some(out)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp(year: i32, month: u32, day: u32, hour: u32, min: u32, sec: u32) -> i64 {
        chrono::NaiveDate::from_ymd_opt(year, month, day).unwrap().and_hms_opt(hour, min, sec).unwrap().and_utc().timestamp()
    }

    #[test]
    fn detection_uses_hints_bytes_and_prefixes_without_dropping_manual_override() {
        assert_eq!(detect_format(DetectInput { column_hint: None, type_hint: Some("jsonb"), bytes: None, text: "plain" }), DataFormat::Json);
        assert_eq!(detect_format(DetectInput { column_hint: Some("body_xml"), type_hint: None, bytes: None, text: "plain" }), DataFormat::Xml);
        assert_eq!(detect_format(DetectInput { column_hint: None, type_hint: Some("bytea"), bytes: None, text: "{\"a\":1}" }), DataFormat::Hex);
        assert_eq!(detect_format(DetectInput { column_hint: None, type_hint: Some("blob"), bytes: None, text: "abc" }), DataFormat::Hex);
        assert_eq!(detect_format(DetectInput { column_hint: None, type_hint: None, bytes: Some(&[0x00, b'A']), text: "A" }), DataFormat::Hex);
        assert_eq!(detect_format(DetectInput { column_hint: None, type_hint: None, bytes: None, text: "\u{1}b:4" }), DataFormat::Hex);
        assert_eq!(detect_format(DetectInput { column_hint: None, type_hint: None, bytes: None, text: "\u{1}t:20:{\"a\":1}" }), DataFormat::Json);
        assert_eq!(detect_format(DetectInput { column_hint: None, type_hint: None, bytes: None, text: "{\"a\":1}" }), DataFormat::Json);
        assert_eq!(detect_format(DetectInput { column_hint: None, type_hint: None, bytes: None, text: "<root/>" }), DataFormat::Xml);
        assert_eq!(detect_format(DetectInput { column_hint: None, type_hint: None, bytes: None, text: "---\nname: ada" }), DataFormat::Yaml);
        assert_eq!(detect_format(DetectInput { column_hint: None, type_hint: None, bytes: None, text: "SELECT id FROM t" }), DataFormat::Sql);
        assert_eq!(detect_format(DetectInput { column_hint: None, type_hint: None, bytes: None, text: "hello" }), DataFormat::Text);
        let detected = detect_format(DetectInput { column_hint: None, type_hint: Some("json"), bytes: None, text: "not json" });
        assert_eq!(effective_format(detected, Some(DataFormat::Text)), DataFormat::Text);
        assert_eq!(detected, DataFormat::Json);
    }

    #[test]
    fn scalar_hints_cover_uuid_and_unix_range_without_replacing_the_raw_value() {
        let raw = "550e8400-e29b-41d4-a716-446655440000";
        let hint = scalar_hint(raw).unwrap();
        assert_eq!(hint.kind, ScalarHintKind::Uuid { version: 4 });
        let text = scalar_hint_text(Locale::En, &hint);
        assert!(text.contains('4'), "{text}");
        assert_ne!(text, raw);
        assert_eq!(raw, "550e8400-e29b-41d4-a716-446655440000");
        assert!(scalar_hint("not-a-uuid").is_none());

        let seconds = scalar_hint("1700000000").unwrap();
        assert!(matches!(seconds.kind, ScalarHintKind::Unix { millis: false, .. }));
        let shown = scalar_hint_text(Locale::En, &seconds);
        assert!(shown.contains("UTC"), "{shown}");
        assert_ne!(shown, "1700000000");
        assert!(matches!(scalar_hint("1700000000000").unwrap().kind, ScalarHintKind::Unix { millis: true, .. }));
        assert!(scalar_hint("0").is_some());
        assert!(scalar_hint(&stamp(2100, 12, 31, 23, 59, 59).to_string()).is_some());
        assert!(scalar_hint(&stamp(2101, 1, 1, 0, 0, 0).to_string()).is_none());
        assert!(scalar_hint(&stamp(1969, 12, 31, 23, 59, 59).to_string()).is_none());
    }

    #[test]
    fn json_tree_paths_filter_and_collapse() {
        let value = serde_json::json!({"items":[{"sku":"A-1","qty":2}]});
        let nodes = visible_json_nodes(&value, &BTreeSet::new(), "");
        let sku = nodes.iter().find(|node| node.key == "sku").unwrap();
        assert_eq!(sku.path, "$.items[0].sku");
        assert_eq!(copy_node_value(&value, &sku.path).as_deref(), Some("A-1"));
        assert_eq!(value_at_path(&value, "$.items[0].sku").and_then(|node| node.as_str()), Some("A-1"));
        assert!(nodes.iter().any(|node| node.badge.as_deref() == Some("{1}")));
        assert!(nodes.iter().any(|node| node.badge.as_deref() == Some("[1]")));
        let filtered = visible_json_nodes(&value, &BTreeSet::new(), "sku");
        assert!(filtered.iter().any(|node| node.path == "$.items[0].sku"));
        assert!(filtered.iter().all(|node| node.path == "$" || node.path.starts_with("$.items")));
        let mut collapsed = BTreeSet::new();
        collapsed.insert("$.items".to_owned());
        let hidden = visible_json_nodes(&value, &collapsed, "");
        assert!(hidden.iter().all(|node| node.path != "$.items[0].sku"));
        collapsed.clear();
        collect_containers(&value, "$", &mut collapsed);
        assert!(collapsed.contains("$"));
        assert_eq!(visible_json_nodes(&value, &collapsed, "").len(), 1);
    }

    #[test]
    fn pretty_minify_and_json_errors_keep_line_and_column() {
        let pretty = pretty_json("{\"a\":1}").unwrap();
        assert!(pretty.contains('\n'), "{pretty}");
        assert_eq!(minify_json(&pretty).unwrap(), "{\"a\":1}");
        let bad = "{\n\"a\": ";
        let diag = validate_json(bad).unwrap_err();
        let raw = serde_json::from_str::<serde_json::Value>(bad).unwrap_err();
        assert_eq!((diag.line, diag.column), (raw.line(), raw.column()));
        assert!(diag.line >= 1 && diag.column >= 1);
        assert!(pretty_json(bad).is_err());
    }

    #[test]
    fn preview_locks_commit_and_invalid_json_keeps_the_draft() {
        let draft = "{\"a\":";
        let preview = TruncationState::Preview { total_bytes: 99, can_load_full: true };
        let banner = truncation_banner(&preview);
        assert!(banner.warning && banner.load_full && banner.edits_disabled);
        assert!(editing_locked(&preview));
        match commit_draft(DataFormat::Json, EditMode::Editable, &preview, draft) {
            CommitResult::Blocked { draft: kept, block: ApplyBlock::Preview } => assert_eq!(kept, draft),
            other => panic!("{other:?}"),
        }
        match commit_draft(DataFormat::Json, EditMode::Editable, &TruncationState::Full, draft) {
            CommitResult::Blocked { draft: kept, block: ApplyBlock::InvalidJson(error) } => {
                assert_eq!(kept, draft);
                assert!(error.line >= 1);
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            commit_draft(DataFormat::Json, EditMode::Editable, &TruncationState::Full, "{\"a\":1}"),
            CommitResult::Applied(text) if text == "{\"a\":1}"
        ));
        assert!(matches!(
            commit_draft(DataFormat::Hex, EditMode::Editable, &TruncationState::Full, "AA"),
            CommitResult::Blocked { block: ApplyBlock::HexReadOnly, .. }
        ));
        assert!(!editing_locked(&TruncationState::Full));
    }

    #[test]
    fn hex_dump_is_sixteen_bytes_wide_and_dots_non_printable() {
        let dump = hex_dump(b"Hello");
        assert!(dump.starts_with("00000000 | 48 65 6c 6c 6f"), "{dump}");
        assert!(dump.contains("Hello"), "{dump}");
        let hidden = hex_dump(&[0x00, b'\n', 0x7f]);
        assert!(hidden.contains("..."), "{hidden}");
        let wide: Vec<u8> = (0..32).collect();
        let dump = hex_dump(&wide);
        let lines: Vec<_> = dump.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("00 01 02"));
        assert!(lines[1].starts_with("00000010"));
        assert!(hex_dump_source("\u{1}b:4", None).is_empty());
    }

    #[test]
    fn xml_sql_and_yaml_format_without_claiming_a_yaml_parser() {
        assert_eq!(format_xml("<root><child>x</child></root>"), "<root>\n  <child>\n    x\n  </child>\n</root>");
        assert_eq!(format_sql("select a, b from t where id = 1 and name = 'x'"), "select a, b\nfrom t\nwhere id = 1\nand name = 'x'");
        let yaml = yaml_view("name: ada\nitems:\n  - a");
        assert!(!yaml.json_compatible);
        assert_eq!(yaml.limitation_key, Some(YAML_NOT_STRICT_KEY));
        assert!(yaml.text.contains("name: ada"));
        let json_yaml = yaml_view("{\"name\":\"ada\"}");
        assert!(json_yaml.json_compatible);
        assert!(json_yaml.limitation_key.is_none());
        assert!(json_yaml.text.contains("name"));
    }
}
