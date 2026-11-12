//! SQL editor prototype on GPUI Kit's Editor.
#![cfg_attr(not(test), allow(dead_code, unused_imports))]
//! Dialect rules stay out of this gate; the prototype checks IME, long text,
//! selection, completion, lint, folding and undo against the real control.

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;

use gpui_kit::base::input::{Diagnostic, DiagnosticSeverity};
use gpui_kit::component::input::{
    CompletionProvider, DefinitionProvider, Editor, EditorState, FoldRange, GutterDecoration, GutterDecorationCollection, HoverProvider,
    InputEvent, RangeDecoration, RangeDecorationCollection, RangeDecorationStyle, Rope,
};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    anchored, deferred, point, Anchor, App, AppContext as _, Context, Entity, EntityInputHandler as _, Focusable as _, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString, Styled as _, Task, Window, div, px, rgb,
};
use lsp_types::{
    CompletionContext, CompletionItem, CompletionItemKind, CompletionResponse, CompletionTextEdit, Documentation, Hover, HoverContents,
    LocationLink, MarkupContent, MarkupKind, TextEdit,
};

use crate::app::accept_versioned;
use crate::app::views::settings;
use crate::i18n::{self, Locale};
use crate::query::assist::{CompletionScope, EntryInfo};
use crate::query::complete::{self, Resolved};
use crate::query::functions;
use crate::ui::kit::{Palette, Radius, TypeScale};

/// Ctrl/⌘+点击跳转定义用的自定义 URI 方案：`rillsql-object://<连接>/<库>/<模式>/<对象>?kind=<种类>`。
pub const OBJECT_SCHEME: &str = "rillsql-object";

/// 跳转定义解析出的目录对象。连接与库来自查询标签的目标，模式、名字和种类来自目录。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectLink {
    pub connection_id: String,
    pub database: String,
    pub schema: String,
    pub name: String,
    pub kind: String,
}

impl ObjectLink {
    pub fn uri(&self) -> String {
        format!(
            "{OBJECT_SCHEME}://{}/{}/{}/{}?kind={}",
            encode_segment(&self.connection_id),
            encode_segment(&self.database),
            encode_segment(&self.schema),
            encode_segment(&self.name),
            encode_segment(&self.kind)
        )
    }

    /// 只认本方案的 URI；段数不对或编码坏了返回 None。
    pub fn parse(uri: &str) -> Option<Self> {
        let rest = uri.strip_prefix(OBJECT_SCHEME)?.strip_prefix("://")?;
        let (path, kind) = match rest.split_once("?kind=") {
            Some((path, kind)) => (path, decode_segment(kind)?),
            None => (rest, String::new()),
        };
        let parts: Vec<&str> = path.split('/').collect();
        let [connection_id, database, schema, name] = parts.as_slice() else {
            return None;
        };
        let name = decode_segment(name)?;
        if name.is_empty() {
            return None;
        }
        Some(Self {
            connection_id: decode_segment(connection_id)?,
            database: decode_segment(database)?,
            schema: decode_segment(schema)?,
            name,
            kind,
        })
    }
}

fn encode_segment(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn decode_segment(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = text.get(index + 1..index + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// 签名提示的内容：函数名、形参、返回值和当前高亮的形参（超出固定参数时没有高亮）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignatureHint {
    pub name: String,
    pub params: Vec<String>,
    pub variadic: bool,
    pub returns: String,
    pub active: Option<usize>,
    /// 左括号在全文里的字节偏移；Esc 关闭后同一对括号里不再弹出。
    pub open: usize,
}

/// 光标所在的已知函数调用的签名提示。光标不在括号里、函数未知或不写括号的函数都没有。
pub fn signature_at(engine: &str, text: &str, cursor: usize) -> Option<SignatureHint> {
    let call = functions::call_context(text, cursor)?;
    let sig = functions::call_signature(engine, &call.name)?;
    Some(SignatureHint {
        name: sig.name.to_owned(),
        params: sig.params.iter().map(|param| (*param).to_owned()).collect(),
        variadic: sig.variadic,
        returns: sig.returns.to_owned(),
        active: sig.active_param(call.argument),
        open: call.open,
    })
}

/// 光标所在语句的范围。光标落在语句之间的空白里、或草稿只有一条语句时没有高亮。
pub fn statement_at(engine: &str, text: &str, cursor: usize) -> Option<Range<usize>> {
    // 与大纲同一套切分：未绑定或未知引擎按 postgres 切。
    let engine = if crate::query::assist::known_engine(engine) { engine } else { "postgres" };
    let pieces = crate::query::split_sql(engine, text).pieces;
    if pieces.len() < 2 {
        return None;
    }
    pieces.iter().find(|piece| piece.start <= cursor && cursor <= piece.end).map(|piece| piece.start..piece.end)
}

/// 补全详情、悬停和对象信息共用的说明文本（Markdown）。只写读到的元数据；读不到的项明说。
pub fn info_markdown(info: &EntryInfo, locale: Locale) -> String {
    let t = |key: &str| i18n::text(locale, key);
    let fill = |key: &str, name: &str, value: &str| i18n::fill(&t(key), &[(name, &escape_markdown(value))]);
    let mut lines = Vec::new();
    match info {
        EntryInfo::Column { source, column } => {
            let type_text = if column.type_name.trim().is_empty() { t("query.info.typeUnknown") } else { fill("query.info.type", "type", &column.type_name) };
            let nullable = match column.nullable {
                Some(true) => t("query.info.nullable"),
                Some(false) => t("query.info.notNull"),
                None => t("query.info.nullUnknown"),
            };
            let mut flags = vec![nullable];
            if column.primary {
                flags.push(t("query.side.primary"));
            }
            if column.foreign_key {
                flags.push(t("query.side.foreign"));
            }
            lines.push(format!("{} · {type_text} · {}", code_span(&column.name), flags.join(" · ")));
            if !source.is_empty() {
                lines.push(fill("query.info.source", "source", source));
            }
            if !column.comment.trim().is_empty() {
                lines.push(fill("query.info.comment", "comment", &column.comment));
            }
        }
        EntryInfo::Object { schema, name, kind, comment } => {
            let qualified = if schema.is_empty() { name.clone() } else { format!("{schema}.{name}") };
            let kind_key = match kind.to_ascii_lowercase().as_str() {
                "table" => "query.complete.kind.table",
                "view" | "materialized" => "query.complete.kind.view",
                "function" => "query.complete.kind.function",
                "procedure" => "query.complete.kind.procedure",
                "trigger" => "query.complete.kind.trigger",
                "sequence" => "query.complete.kind.sequence",
                _ => "query.complete.kind.object",
            };
            lines.push(format!("{} · {}", code_span(&qualified), t(kind_key)));
            if !schema.is_empty() {
                lines.push(fill("query.info.schema", "schema", schema));
            }
            if !comment.trim().is_empty() {
                lines.push(fill("query.info.comment", "comment", comment));
            }
        }
        EntryInfo::Function { signature } => lines.push(code_span(signature)),
    }
    lines.join("\n\n")
}

fn code_span(text: &str) -> String {
    format!("`{}`", text.replace('`', "'"))
}

fn escape_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if matches!(ch, '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '#' | '|') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// 悬停内容：先按语句解析对象/列；认不出时，紧跟左括号的已知内置函数给签名。其余没有悬停。
pub fn hover_info(scope: &CompletionScope, text: &str, offset: usize) -> Option<(Range<usize>, EntryInfo)> {
    if let Some(found) = complete::resolve(scope, text, offset) {
        return Some((found.span.start..found.span.end, found.target.info()));
    }
    let offset = offset.min(text.len());
    if !text.is_char_boundary(offset) {
        return None;
    }
    let is_word = |ch: char| ch.is_ascii_alphanumeric() || ch == '_';
    let start = offset - text[..offset].chars().rev().take_while(|ch| is_word(*ch)).map(char::len_utf8).sum::<usize>();
    let end = offset + text[offset..].chars().take_while(|ch| is_word(*ch)).map(char::len_utf8).sum::<usize>();
    if start == end || !text[end..].starts_with('(') {
        return None;
    }
    let sig = functions::call_signature(&scope.engine, &text[start..end])?;
    Some((start..end, EntryInfo::Function { signature: sig.text() }))
}

/// 跳转定义的目标：对象本身，或列所在的目录对象。目标连接/库未知、解析不到时为 None。
pub fn definition_link(scope: &CompletionScope, target: Option<&(String, String)>, text: &str, offset: usize) -> Option<(Range<usize>, ObjectLink)> {
    let (connection_id, database) = target?;
    let found = complete::resolve(scope, text, offset)?;
    let (schema, name, kind) = match found.target {
        Resolved::Object { schema, name, kind, .. } => (schema, name, kind),
        Resolved::Column { schema, table, .. } => {
            let object = scope
                .objects
                .iter()
                .find(|object| object.name.eq_ignore_ascii_case(&table) && object.schema.eq_ignore_ascii_case(&schema))?;
            (object.schema.clone(), object.name.clone(), object.kind.clone())
        }
    };
    Some((
        found.span.start..found.span.end,
        ObjectLink { connection_id: connection_id.clone(), database: database.clone(), schema, name, kind },
    ))
}

/// 跳转定义的出口：拿到解析出的对象后交给外壳。
pub type ObjectHandler = Rc<dyn Fn(ObjectLink, &mut App)>;

type ObjectSink = Rc<RefCell<Option<ObjectHandler>>>;

/// show_document 的处理：本方案的 URI 交给外壳定位；其它 URI 走编辑器默认行为。
fn follow_uri(sink: &ObjectSink, uri: &str, cx: &mut App) -> bool {
    if !uri.starts_with(OBJECT_SCHEME) {
        return false;
    }
    // 认不出的本方案 URI 也算处理过：不能退回到“在当前文档里跳到 0 行”。
    if let Some(link) = ObjectLink::parse(uri) {
        let handler = sink.borrow().clone();
        if let Some(handler) = handler {
            handler(link, cx);
        }
    }
    true
}

pub struct SqlEditor {
    state: Entity<EditorState>,
    version: Rc<Cell<u64>>,
    extra: Rc<RefCell<Vec<(String, String)>>>,
    scope: Rc<RefCell<Option<CompletionScope>>>,
    /// 补全详情与状态说明按当前语言生成；切换语言后新请求立即生效。
    locale: Rc<Cell<Locale>>,
    /// 最近一次请求的元数据状态说明；失败、部分失败或空目录时非空。
    note: Rc<RefCell<Option<String>>>,
    /// 跳转定义用的连接与库；查询标签没有目标时为 None，不生成链接。
    link_target: Rc<RefCell<Option<(String, String)>>>,
    /// 跳转定义的出口：查询页把它接到外壳的树定位/结构页。
    object_sink: ObjectSink,
    /// 当前签名提示；光标离开括号、Esc 或函数未知时为 None。
    signature: Option<SignatureHint>,
    /// Esc 关掉的那对括号（左括号偏移）；光标离开或换了调用后失效。
    dismissed: Option<usize>,
    /// 光标所在语句的范围（低对比底色）。
    current_statement: Option<Range<usize>>,
    /// 上次按光标计算时的 (版本, 光标, 长度, 深色)，同一帧内只算一次。
    tracked: Option<(u64, usize, usize, bool)>,
    statement_marks: RangeDecorationCollection,
    error_marks: RangeDecorationCollection,
    error_gutter: GutterDecorationCollection,
    /// 当前错误标记的范围；下一次编辑或重新执行时清除。
    errors: Vec<Range<usize>>,
    /// 执行错误和静态 lint 的原文，悬停与诊断共用。
    facts: Rc<RefCell<HoverFacts>>,
    stale_lint: u32,
    stale_completion: u32,
    folded_lines: u32,
    /// 设置里的编辑器字号与配色；渲染时直接作用到 Editor 的文本样式。
    font_size: f32,
    theme: String,
    _subscription: gpui_kit::Subscription,
    /// Tab 接受补全：弹层打开时把 Tab 当成 Enter，其它时候仍然缩进。
    _accept_tab: gpui_kit::Subscription,
    /// Esc 关闭签名提示。
    _dismiss_signature: gpui_kit::Subscription,
    /// 光标移动后重算当前语句高亮和签名提示。
    _track: gpui_kit::Subscription,
}

/// 悬停要优先展示的诊断：执行错误和静态 lint，都带着原文。
#[derive(Default)]
struct HoverFacts {
    errors: Vec<(Range<usize>, String)>,
    lint: Vec<(Range<usize>, String)>,
}

struct SqlHover {
    scope: Rc<RefCell<Option<CompletionScope>>>,
    locale: Rc<Cell<Locale>>,
    facts: Rc<RefCell<HoverFacts>>,
}

fn covers(range: &Range<usize>, offset: usize) -> bool {
    !range.is_empty() && offset >= range.start && offset < range.end
}

/// 执行错误优先于静态 lint；同一类里取最短的那一段。
fn diagnostic_at(facts: &HoverFacts, offset: usize) -> Option<(Range<usize>, String)> {
    let pick = |items: &[(Range<usize>, String)]| {
        items
            .iter()
            .filter(|(range, message)| !message.is_empty() && covers(range, offset))
            .min_by_key(|(range, _)| range.end.saturating_sub(range.start))
            .cloned()
    };
    pick(&facts.errors).or_else(|| pick(&facts.lint))
}

fn diagnostic_markdown(message: &str, locale: Locale) -> String {
    format!("**{}**\n\n{}", i18n::text(locale, "query.log.error"), escape_markdown(message))
}

struct SqlDefinition {
    scope: Rc<RefCell<Option<CompletionScope>>>,
    target: Rc<RefCell<Option<(String, String)>>>,
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
        let facts = Rc::new(RefCell::new(HoverFacts::default()));
        let note = Rc::new(RefCell::new(None));
        let request = Rc::new(Cell::new(0));
        let link_target: Rc<RefCell<Option<(String, String)>>> = Rc::new(RefCell::new(None));
        let object_sink: ObjectSink = Rc::new(RefCell::new(None));
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
            state.lsp_mut().hover_provider = Some(Rc::new(SqlHover { scope: scope.clone(), locale: locale.clone(), facts: facts.clone() }));
            state.lsp_mut().definition_provider = Some(Rc::new(SqlDefinition { scope: scope.clone(), target: link_target.clone() }));
            let sink = object_sink.clone();
            state.lsp_mut().show_document = Some(Rc::new(move |params, _window, cx| follow_uri(&sink, params.uri.as_str(), cx)));
            state
        });
        // 装饰集合一开始就建好：槽位常驻，出现错误标记时文本不会横移。
        let (statement_marks, error_marks, error_gutter) = state.update(cx, |state, cx| {
            (
                state.create_range_decorations_collection(Vec::new(), cx),
                state.create_range_decorations_collection(Vec::new(), cx),
                state.create_gutter_decorations_collection(Vec::new(), cx),
            )
        });
        let subscription = cx.subscribe(&state, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.version.set(this.version.get().wrapping_add(1));
                // 错误标记属于上一次执行；草稿一改就不再可信。静态诊断的偏移也一起失效。
                this.clear_errors(cx);
                this.facts.borrow_mut().lint.clear();
                let _ = this.track(cx);
                cx.notify();
            }
        });
        let track = cx.observe(&state, |this, _, cx| {
            if this.track(cx) {
                cx.notify();
            }
        });
        let this = cx.weak_entity();
        let dismiss_signature = cx.intercept_keystrokes({
            move |event, window, cx| {
                if event.keystroke.key != "escape" || event.keystroke.modifiers.modified() {
                    return;
                }
                let _ = this.update(cx, |editor, cx| {
                    if editor.signature.is_some() && editor.is_focused(window, cx) && !editor.completion_open(cx) {
                        editor.dismiss_signature(cx);
                    }
                });
            }
        });
        // 弹层由编辑器自己画，键盘接受原本只绑了 Enter；Tab 在这里补上。
        let accept_tab = cx.intercept_keystrokes({
            let state = state.downgrade();
            move |event, window, cx| {
                if event.keystroke.key != "tab" || event.keystroke.modifiers.modified() {
                    return;
                }
                // 只在这个编辑器的弹层打开、并且焦点还在它上面时才改写 Tab。
                let ours = state
                    .update(cx, |state, cx| {
                        state.completion_menu_state().open && state.focus_handle(cx).is_focused(window)
                    })
                    .unwrap_or(false);
                if !ours {
                    return;
                }
                cx.stop_propagation();
                // 走和 Enter 同一条路径：接受当前选中项并收起弹层。
                window.dispatch_action(Box::new(gpui_kit::base::input::Enter { secondary: false, shift: false }), cx);
            }
        });
        Self {
            state,
            version,
            extra,
            scope,
            locale,
            note,
            link_target,
            object_sink,
            signature: None,
            dismissed: None,
            current_statement: None,
            tracked: None,
            statement_marks,
            error_marks,
            error_gutter,
            errors: Vec::new(),
            facts,
            stale_lint: 0,
            stale_completion: 0,
            folded_lines: 0,
            font_size: 13.0,
            theme: "auto".to_owned(),
            _subscription: subscription,
            _accept_tab: accept_tab,
            _dismiss_signature: dismiss_signature,
            _track: track,
        }
    }

    fn engine(&self) -> String {
        self.scope.borrow().as_ref().map(|scope| scope.engine.clone()).unwrap_or_default()
    }

    /// 光标移动或文本变化后重算当前语句高亮和签名提示；同一状态只算一次。
    fn track(&mut self, cx: &mut Context<Self>) -> bool {
        let (text, cursor) = {
            let state = self.state.read(cx);
            (state.text().clone(), state.cursor())
        };
        let dark = cx.theme().is_dark();
        let key = (self.version.get(), cursor, text.len(), dark);
        let previous = self.tracked;
        if previous == Some(key) {
            return false;
        }
        self.tracked = Some(key);
        let value = text.to_string();
        let engine = self.engine();
        let current = statement_at(&engine, &value, cursor);
        if current != self.current_statement || previous.is_some_and(|(_, _, _, was)| was != dark) {
            self.current_statement = current.clone();
            let mut color = Palette::of(cx).accent;
            color.a = if dark { 0.10 } else { 0.06 };
            let marks = current
                .map(|range| vec![RangeDecoration::new(range).with_style(RangeDecorationStyle::Fill).with_color(color)])
                .unwrap_or_default();
            self.statement_marks.set(marks, cx);
        }
        let mut hint = signature_at(&engine, &value, cursor);
        match (&hint, self.dismissed) {
            (Some(found), Some(open)) if found.open == open => hint = None,
            (_, Some(_)) => self.dismissed = None,
            _ => {}
        }
        if hint == self.signature {
            return false;
        }
        self.signature = hint;
        true
    }

    /// Esc：关掉这对括号的签名提示，直到光标离开或进入另一个调用。
    pub fn dismiss_signature(&mut self, cx: &mut Context<Self>) {
        if let Some(hint) = self.signature.take() {
            self.dismissed = Some(hint.open);
            cx.notify();
        }
    }

    /// 当前签名提示（测试和外壳读取）。
    pub fn signature(&self) -> Option<&SignatureHint> {
        self.signature.as_ref()
    }

    /// 光标所在语句的高亮范围；随编辑移动，没有高亮时为 None。
    pub fn statement_highlight(&self, cx: &App) -> Option<Range<usize>> {
        self.statement_marks.get_ranges(cx).into_iter().next()
    }

    /// 执行失败的位置：每个范围同时画在行号旁和文本底色上。
    /// 兼容只给范围的调用；悬停没有对应消息时仍显示表/列说明。
    pub fn mark_errors(&mut self, ranges: Vec<Range<usize>>, cx: &mut Context<Self>) {
        self.mark_error_messages(ranges.into_iter().map(|range| (range, String::new())).collect(), cx);
    }

    /// 执行失败的位置和错误原文。悬停命中范围时展示这段原文。
    pub fn mark_error_messages(&mut self, marked: Vec<(Range<usize>, String)>, cx: &mut Context<Self>) {
        let palette = Palette::of(cx);
        let mut fill = palette.danger;
        fill.a = 0.16;
        let ranges: Vec<Range<usize>> = marked.iter().map(|(range, _)| range.clone()).collect();
        let gutter = ranges.iter().cloned().map(|range| GutterDecoration::new(range).with_color(palette.danger)).collect();
        let marks = ranges.iter().cloned().map(|range| RangeDecoration::new(range).with_style(RangeDecorationStyle::Fill).with_color(fill)).collect();
        self.error_gutter.set(gutter, cx);
        self.error_marks.set(marks, cx);
        self.errors = ranges;
        self.facts.borrow_mut().errors = marked;
        cx.notify();
    }

    pub fn clear_errors(&mut self, cx: &mut Context<Self>) {
        if self.errors.is_empty() && self.facts.borrow().errors.is_empty() {
            return;
        }
        self.errors.clear();
        self.facts.borrow_mut().errors.clear();
        self.error_gutter.clear(cx);
        self.error_marks.clear(cx);
        cx.notify();
    }

    /// 当前的错误标记范围（随编辑跟踪）。
    pub fn error_ranges(&self, cx: &App) -> Vec<Range<usize>> {
        if self.errors.is_empty() {
            return Vec::new();
        }
        self.error_marks.get_ranges(cx)
    }

    /// 跳转定义生成链接用的连接与库。
    pub fn set_link_target(&self, target: Option<(String, String)>) {
        let mut slot = self.link_target.borrow_mut();
        if *slot != target {
            *slot = target;
        }
    }

    /// Ctrl/⌘+点击对象后的出口。
    pub fn set_object_sink(&self, sink: ObjectHandler) {
        *self.object_sink.borrow_mut() = Some(sink);
    }

    /// 与 Ctrl/⌘+点击同一条路径：按偏移解析链接，再交给 show_document 处理。解析不到时什么也不做。
    pub fn follow_definition_at(&self, offset: usize, cx: &mut App) -> bool {
        let text = self.value(cx);
        let link = {
            let scope = self.scope.borrow();
            let target = self.link_target.borrow();
            scope.as_ref().and_then(|scope| definition_link(scope, target.as_ref(), &text, offset))
        };
        match link {
            Some((_, link)) => follow_uri(&self.object_sink, &link.uri(), cx),
            None => false,
        }
    }

    /// 悬停在偏移处会显示的说明（与悬停提供者同一份数据）；没有时为 None。
    /// 执行错误和静态 lint 优先于表、列、函数文档。
    pub fn hover_text(&self, offset: usize, cx: &App) -> Option<String> {
        if let Some((_, message)) = diagnostic_at(&self.facts.borrow(), offset) {
            return Some(diagnostic_markdown(&message, self.locale.get()));
        }
        let text = self.value(cx);
        let scope = self.scope.borrow();
        let (_, info) = hover_info(scope.as_ref()?, &text, offset)?;
        Some(info_markdown(&info, self.locale.get()))
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

    /// 在当前光标或选区插入文本。`None` 表示用当前选区（字节偏移），
    /// 由 `EditorState::replace_text_in_range` 记入撤销栈；随后聚焦，Ctrl+Z 才能落到这次插入。
    pub fn insert_at_selection(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            state.replace_text_in_range(None, text, window, cx);
            state.focus(window, cx);
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
        self.apply_lint_all(response_version, &[(range, message.to_owned())], cx)
    }

    /// 写入全部静态诊断，不再只保留第一条。版本过期时一条都不写。
    pub fn apply_lint_all(
        &mut self,
        response_version: u64,
        findings: &[(Range<usize>, String)],
        cx: &mut Context<Self>,
    ) -> bool {
        if !accept_versioned(self.version.get(), response_version) {
            self.stale_lint += 1;
            return false;
        }
        self.facts.borrow_mut().lint = findings.to_vec();
        self.state.update(cx, |state, _| {
            let text = state.text().clone();
            let Some(set) = state.diagnostics_mut() else {
                return;
            };
            set.reset(&text);
            for (range, message) in findings {
                let start = offset_to_position(&text, range.start);
                let end = offset_to_position(&text, range.end.min(text.len()));
                set.push(Diagnostic::new(start..end, message.clone()).with_severity(DiagnosticSeverity::Error));
            }
        });
        true
    }

    /// 当前全部静态诊断原文，按写入顺序。
    pub fn lint_messages(&self) -> Vec<String> {
        self.facts.borrow().lint.iter().map(|(_, message)| message.clone()).collect()
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
            // 有选区时替换选区；否则替换光标所在的整个词（光标停在词中间也不留半截）。
            let start = if selected.is_empty() { selected.end - word_before(&text, selected.end).len() } else { selected.start };
            let end = if selected.is_empty() { word_end(&text, selected.end) } else { selected.end };
            let range = offset_to_position(&text, start)..offset_to_position(&text, end);
            let item = completion_edit(label, insert, None, range, None);
            state.insert_completion(&item, selected, window, cx);
        });
        true
    }

    pub fn completion_insert(&self, label: &str, cx: &App) -> Option<String> {
        self.state.read(cx).completion_menu_state().items.iter().find(|item| item.label == label).and_then(|item| item.insert_text.clone())
    }

    /// 补全项的详情文档（Markdown）；没有元数据的候选为 None。
    pub fn completion_documentation(&self, label: &str, cx: &App) -> Option<String> {
        self.state.read(cx).completion_menu_state().items.iter().find(|item| item.label == label).and_then(|item| match item.documentation.as_ref()? {
            Documentation::String(text) => Some(text.clone()),
            Documentation::MarkupContent(content) => Some(content.value.clone()),
        })
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
        self.facts.borrow_mut().lint.clear();
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _ = self.track(cx);
        let (background, foreground) = self.theme_colors(cx.theme().is_dark());
        let focused = self.is_focused(window, cx);
        let popup = self.signature.clone().filter(|_| focused).map(|hint| self.signature_popup(&hint, cx));
        div()
            .id("sql-editor")
            .relative()
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
            .children(popup)
    }
}

impl SqlEditor {
    /// 签名提示浮层：贴在光标上方，不压住下方的补全弹层；还没有光标几何时停在编辑器右上角。
    fn signature_popup(&self, hint: &SignatureHint, cx: &App) -> gpui_kit::AnyElement {
        let p = Palette::of(cx);
        let mut row = div()
            .id("sql-signature")
            .flex()
            .flex_wrap()
            .max_w(px(520.))
            .px(px(8.))
            .py(px(4.))
            .rounded(px(Radius::PANEL))
            .border_1()
            .border_color(p.line)
            .bg(p.raised)
            .shadow_md()
            .text_size(px(TypeScale::SM))
            .text_color(p.text_muted)
            .font_family(cx.theme().mono_font_family.clone())
            .child(div().text_color(p.text).child(SharedString::from(format!("{}(", hint.name))));
        for (index, param) in hint.params.iter().enumerate() {
            let active = hint.active == Some(index);
            if index > 0 {
                row = row.child(SharedString::from(", "));
            }
            row = row.child(
                div()
                    .id(("sql-signature-param", index))
                    .when(active, |this| this.text_color(p.accent_text).font_weight(FontWeight::SEMIBOLD).bg(p.accent_soft).rounded(px(Radius::CONTROL)))
                    .child(SharedString::from(param.clone())),
            );
        }
        if hint.variadic {
            row = row.child(SharedString::from(", …"));
        }
        row = row.child(SharedString::from(format!(") → {}", hint.returns)));
        match self.state.read(cx).prepaint_caret_geometry() {
            Some(geometry) => {
                let caret = geometry.cursor_bounds();
                let at = point(caret.origin.x - px(4.), caret.origin.y + geometry.scroll_offset().y - px(4.));
                deferred(anchored().position(at).anchor(Anchor::BottomLeft).snap_to_window().child(row)).with_priority(1).into_any_element()
            }
            None => div().absolute().top(px(4.)).right(px(8.)).child(row).into_any_element(),
        }
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
                        let mut item = completion_edit(
                            &entry.label,
                            &entry.insert,
                            completion_kind(entry.kind),
                            range.clone(),
                            Some(entry.detail.clone()),
                        );
                        // 详情面板：列的类型/可空/注释、对象的种类与模式、函数签名。
                        item.documentation = entry.info.as_ref().map(|info| {
                            Documentation::MarkupContent(MarkupContent { kind: MarkupKind::Markdown, value: info_markdown(info, locale) })
                        });
                        items.push(item);
                    }
                }
                None => {
                    let range = offset_to_position(text, offset - word.len())..offset_to_position(text, word_end(text, offset));
                    for keyword in FALLBACK_KEYWORDS {
                        if lower.is_empty() || keyword.to_ascii_lowercase().starts_with(&lower) {
                            items.push(completion_edit(keyword, keyword, Some(CompletionItemKind::KEYWORD), range.clone(), None));
                        }
                    }
                }
            }
        }
        let range = offset_to_position(text, offset - word.len())..offset_to_position(text, word_end(text, offset));
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

impl HoverProvider for SqlHover {
    fn hover(&self, text: &Rope, offset: usize, _window: &mut Window, _cx: &mut App) -> Task<gpui_kit::Result<Option<Hover>>> {
        let locale = self.locale.get();
        if let Some((span, message)) = diagnostic_at(&self.facts.borrow(), offset) {
            return Task::ready(Ok(Some(Hover {
                contents: HoverContents::Markup(MarkupContent { kind: MarkupKind::Markdown, value: diagnostic_markdown(&message, locale) }),
                range: Some(lsp_types::Range { start: offset_to_position(text, span.start), end: offset_to_position(text, span.end) }),
            })));
        }
        let scope = self.scope.borrow();
        let Some(scope) = scope.as_ref() else {
            return Task::ready(Ok(None));
        };
        let value = text.to_string();
        // 认不出的名字没有悬停，不弹空气泡。
        let hover = hover_info(scope, &value, offset).map(|(span, info)| Hover {
            contents: HoverContents::Markup(MarkupContent { kind: MarkupKind::Markdown, value: info_markdown(&info, locale) }),
            range: Some(lsp_types::Range { start: offset_to_position(text, span.start), end: offset_to_position(text, span.end) }),
        });
        Task::ready(Ok(hover))
    }
}

impl DefinitionProvider for SqlDefinition {
    fn definitions(&self, text: &Rope, offset: usize, _window: &mut Window, _cx: &mut App) -> Task<gpui_kit::Result<Vec<LocationLink>>> {
        let scope = self.scope.borrow();
        let target = self.target.borrow();
        let value = text.to_string();
        let links = scope
            .as_ref()
            .and_then(|scope| definition_link(scope, target.as_ref(), &value, offset))
            .and_then(|(span, link)| {
                let uri = link.uri().parse::<lsp_types::Uri>().ok()?;
                Some(LocationLink {
                    origin_selection_range: Some(lsp_types::Range { start: offset_to_position(text, span.start), end: offset_to_position(text, span.end) }),
                    target_uri: uri,
                    target_range: lsp_types::Range::default(),
                    target_selection_range: lsp_types::Range::default(),
                })
            })
            .into_iter()
            .collect();
        Task::ready(Ok(links))
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

/// 光标之后当前词的结尾；替换范围据此覆盖整个词。
fn word_end(text: &Rope, offset: usize) -> usize {
    let offset = offset.min(text.len());
    let tail = text.slice(offset..text.len()).to_string();
    offset
        + tail
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
            .map(char::len_utf8)
            .sum::<usize>()
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
        use crate::query::assist::{CatalogObject, ColumnMeta, CompletionSource, MetadataState};
        CompletionScope {
            engine: "sqlite".to_owned(),
            objects: vec![CatalogObject { name: "orders".to_owned(), schema: String::new(), kind: "table".to_owned(), comment: String::new() }],
            sources: vec![CompletionSource {
                alias: String::new(),
                label: "orders".to_owned(),
                columns: vec![ColumnMeta::named("id"), ColumnMeta::named("customer"), ColumnMeta::named("status")],
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

    fn open_popup(window: gpui_kit::AnyWindowHandle, editor: &Entity<SqlEditor>, sql: &str, cx: &mut TestAppContext) {
        cx.update_window(window, |_, window, cx| {
            editor.update(cx, |editor, cx| {
                editor.set_sql(sql, window, cx);
                editor.focus(window, cx);
                editor.set_completion_scope(Some(catalog_scope()));
                let end = sql.len();
                editor.select_range(end..end, cx);
                editor.show_completions(window, cx);
            });
        })
        .unwrap();
        cx.run_until_parked();
    }

    #[gpui_kit::test]
    fn tab_and_enter_both_accept_the_selected_completion(cx: &mut TestAppContext) {
        let (window, editor) = open(cx);
        open_popup(window, &editor, "select * from ord", cx);
        cx.update_window(window, |_, window, cx| {
            assert!(editor.read(cx).completion_open(cx), "弹层应打开");
            window.press("tab", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(window, |_, _, cx| {
            let value = editor.read(cx).value(cx);
            assert_eq!(value, "select * from orders", "Tab 应接受当前候选");
            assert!(!editor.read(cx).completion_open(cx), "接受后弹层收起");
        })
        .unwrap();

        // Enter 仍然接受，行为没被 Tab 抢走。
        open_popup(window, &editor, "select * from ord", cx);
        cx.update_window(window, |_, window, cx| {
            window.press("enter", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(window, |_, _, cx| {
            assert_eq!(editor.read(cx).value(cx), "select * from orders");
            assert!(!editor.read(cx).completion_open(cx));
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn arrows_move_the_selected_completion_and_escape_returns_focus(cx: &mut TestAppContext) {
        let (window, editor) = open(cx);
        open_popup(window, &editor, "select * from ord", cx);
        cx.update_window(window, |_, window, cx| {
            let labels = editor.read(cx).completion_labels(cx);
            assert!(labels.len() >= 2, "{labels:?}");
            // 下移一项再接受：走的必须是选中的那一项，而不是固定第一项。
            window.press("down", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| {
            window.press("enter", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(window, |_, _, cx| {
            assert_eq!(editor.read(cx).value(cx), "select * from ORDER", "方向键要移动选中项");
        })
        .unwrap();

        // 上移回来再接受，仍然是第一项；Esc 不改草稿并回到编辑器焦点。
        open_popup(window, &editor, "select * from ord", cx);
        cx.update_window(window, |_, window, cx| {
            window.press("up", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| {
            window.press("escape", cx);
            assert!(!editor.read(cx).completion_open(cx));
            assert!(editor.read(cx).is_focused(window, cx), "关闭后焦点回编辑器");
            assert_eq!(editor.read(cx).value(cx), "select * from ord", "Esc 不改草稿");
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn tab_without_a_popup_does_not_insert_a_completion(cx: &mut TestAppContext) {
        let (window, editor) = open(cx);
        cx.update_window(window, |_, window, cx| {
            editor.update(cx, |editor, cx| {
                editor.set_sql("sel", window, cx);
                editor.focus(window, cx);
                editor.set_completion_scope(Some(catalog_scope()));
                editor.select_range(3..3, cx);
            });
            window.press("tab", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(window, |_, _, cx| {
            assert!(!editor.read(cx).value(cx).contains("SELECT"), "没有弹层时 Tab 不写候选");
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn accepting_mid_word_replaces_the_whole_word(cx: &mut TestAppContext) {
        let (window, editor) = open(cx);
        cx.update_window(window, |_, window, cx| {
            editor.update(cx, |editor, cx| {
                editor.set_sql("selec", window, cx);
                editor.select_range(3..3, cx);
                let version = editor.version();
                assert!(editor.apply_completion_insert(version, "SELECT", "SELECT", window, cx));
            });
            assert_eq!(editor.read(cx).value(cx), "SELECT", "光标在词中间时整词替换");
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

    /// 带类型、可空和注释的目录：补全详情、悬停和跳转定义都从这里取。
    fn typed_scope() -> CompletionScope {
        use crate::query::assist::ColumnMeta;
        let mut scope = catalog_scope();
        scope.objects[0].comment = "订单表".to_owned();
        scope.sources[0].columns = vec![
            ColumnMeta { name: "id".to_owned(), type_name: "INTEGER".to_owned(), nullable: Some(false), comment: String::new(), primary: false, foreign_key: false },
            ColumnMeta { name: "customer".to_owned(), type_name: "TEXT".to_owned(), nullable: Some(true), comment: "下单客户".to_owned(), primary: false, foreign_key: false },
            ColumnMeta::named("status"),
        ];
        scope
    }

    #[test]
    fn completion_detail_shows_type_and_nullability() {
        let provider = suggest(Some(typed_scope()), Vec::new());
        let text = "select  from orders";
        let rope = Rope::from(text);
        let items = provider.items(&rope, 7);
        let doc = |label: &str| {
            items
                .iter()
                .find(|item| item.label == label)
                .and_then(|item| match item.documentation.as_ref()? {
                    Documentation::MarkupContent(content) => Some(content.value.clone()),
                    Documentation::String(text) => Some(text.clone()),
                })
                .unwrap_or_else(|| panic!("{label} 没有详情：{:?}", items.iter().map(|item| &item.label).collect::<Vec<_>>()))
        };
        let customer = doc("customer");
        assert!(customer.contains("TEXT"), "{customer}");
        assert!(customer.contains(&i18n::text(Locale::ZhCn, "query.info.nullable")), "{customer}");
        assert!(customer.contains("下单客户"), "{customer}");
        let id = doc("id");
        assert!(id.contains("INTEGER") && id.contains(&i18n::text(Locale::ZhCn, "query.info.notNull")), "{id}");
        // 没读到类型的列明说没读到，不编造类型。
        let status = doc("status");
        assert!(status.contains(&i18n::text(Locale::ZhCn, "query.info.typeUnknown")), "{status}");
        let english = info_markdown(
            &EntryInfo::Column { source: "orders".to_owned(), column: typed_scope().sources[0].columns[1].clone() },
            Locale::En,
        );
        assert!(english.contains("Nullable") && english.contains("TEXT"), "{english}");
    }

    #[test]
    fn hover_resolves_tables_and_columns_and_ignores_unknown_names() {
        let scope = typed_scope();
        let text = "select customer from orders where nosuch = 1";
        let at = |needle: &str| text.find(needle).unwrap() + 1;
        let (span, info) = hover_info(&scope, text, at("orders")).expect("悬停表名要有说明");
        assert_eq!(&text[span], "orders");
        let shown = info_markdown(&info, Locale::ZhCn);
        assert!(shown.contains("orders") && shown.contains("订单表"), "{shown}");
        let (_, column) = hover_info(&scope, text, at("customer")).expect("悬停列名要有说明");
        assert!(info_markdown(&column, Locale::ZhCn).contains("TEXT"));
        assert!(hover_info(&scope, text, at("nosuch")).is_none(), "认不出的名字没有悬停");
        assert!(hover_info(&scope, text, at("select")).is_none(), "关键字没有悬停");
        // 紧跟左括号的已知函数给签名，未知函数没有。
        let call = "select coalesce(customer, 'x'), mystery(1) from orders";
        let (_, function) = hover_info(&scope, call, call.find("coalesce").unwrap() + 2).expect("内置函数悬停");
        assert!(matches!(function, EntryInfo::Function { .. }));
        assert!(hover_info(&scope, call, call.find("mystery").unwrap() + 2).is_none());
    }

    #[test]
    fn definition_links_round_trip_and_skip_unknown_targets() {
        let scope = typed_scope();
        let target = ("conn/1".to_owned(), "main db".to_owned());
        let text = "select o.customer from orders o where nosuch = 1";
        let (span, link) = definition_link(&scope, Some(&target), text, text.find("orders").unwrap() + 2).expect("表名可以跳转");
        assert_eq!(&text[span], "orders");
        assert_eq!(link, ObjectLink { connection_id: "conn/1".into(), database: "main db".into(), schema: String::new(), name: "orders".into(), kind: "table".into() });
        let uri = link.uri();
        assert!(uri.parse::<lsp_types::Uri>().is_ok(), "{uri}");
        assert_eq!(ObjectLink::parse(&uri), Some(link.clone()));
        // 列跳到它所在的表。
        let (_, column) = definition_link(&scope, Some(&target), text, text.find("customer").unwrap() + 2).expect("列跳到表");
        assert_eq!(column.name, "orders");
        assert!(definition_link(&scope, Some(&target), text, text.find("nosuch").unwrap() + 2).is_none(), "认不出的名字不跳");
        assert!(definition_link(&scope, None, text, text.find("orders").unwrap() + 2).is_none(), "标签没有目标时不生成链接");
        assert_eq!(ObjectLink::parse("https://example.com"), None);
        assert_eq!(ObjectLink::parse("rillsql-object://a/b/c"), None);
    }

    #[test]
    fn signature_hint_tracks_the_active_parameter() {
        let text = "select substr(name, 2, 3) from orders";
        let open = text.find('(').unwrap();
        let first = signature_at("sqlite", text, open + 1).expect("括号里给签名");
        assert_eq!(first.name.to_ascii_lowercase(), "substr");
        assert_eq!(first.active, Some(0));
        assert_eq!(first.open, open);
        assert_eq!(signature_at("sqlite", text, text.find(", 2").unwrap() + 2).unwrap().active, Some(1));
        assert_eq!(signature_at("sqlite", text, text.find(", 3").unwrap() + 2).unwrap().active, Some(2));
        assert!(signature_at("sqlite", text, text.find(')').unwrap() + 1).is_none(), "离开括号后关闭");
        assert!(signature_at("sqlite", "select mystery(1", 16).is_none(), "未知函数没有提示");
    }

    #[test]
    fn current_statement_highlight_skips_whitespace_between_statements() {
        let text = "select 1;\n\nselect 2;\n\nselect 3;";
        let second = text.find("select 2").unwrap();
        let range = statement_at("sqlite", text, second + 3).expect("光标在第二条语句里");
        assert!(text[range.clone()].contains("select 2") && !text[range].contains("select 1"));
        assert_eq!(statement_at("sqlite", text, text.find("\n\n").unwrap() + 1), None, "语句之间的空白不高亮");
        assert_eq!(statement_at("sqlite", "select 1", 3), None, "只有一条语句时不高亮");
        // 中文开头的语句不能切在字符中间。
        let chinese = "中文;
select 1;";
        assert!(statement_at("sqlite", chinese, chinese.len()).is_some());
    }

    #[gpui_kit::test]
    fn editor_signature_highlight_errors_and_definition(cx: &mut TestAppContext) {
        let (window, editor) = open(cx);
        let followed: Rc<RefCell<Vec<ObjectLink>>> = Rc::new(RefCell::new(Vec::new()));
        cx.update_window(window, |_, window, cx| {
            let sink = followed.clone();
            editor.update(cx, |editor, cx| {
                editor.set_completion_scope(Some(typed_scope()));
                editor.set_link_target(Some(("notes".to_owned(), "main".to_owned())));
                editor.set_object_sink(Rc::new(move |link, _| sink.borrow_mut().push(link)));
                editor.set_sql("select coalesce(", window, cx);
                editor.focus(window, cx);
                editor.select_range(16..16, cx);
            });
            window.render_frame(cx);
            let hint = editor.read(cx).signature().cloned().expect("已知函数后输入左括号要有签名提示");
            assert_eq!(hint.active, Some(0));
            window.press("escape", cx);
            window.render_frame(cx);
            assert!(editor.read(cx).signature().is_none(), "Esc 关闭签名提示");
        })
        .unwrap();
        cx.update_window(window, |_, window, cx| {
            let text = "select 1;\n\nselect customer from orders;\n\nselect 3;";
            editor.update(cx, |editor, cx| editor.set_sql(text, window, cx));
            let inside = text.find("customer").unwrap();
            editor.update(cx, |editor, cx| editor.select_range(inside..inside, cx));
            window.render_frame(cx);
            let highlight = editor.read(cx).statement_highlight(cx).expect("光标所在语句高亮");
            assert!(text[highlight].starts_with("select customer"));
            let gap = text.find("\n\n").unwrap() + 1;
            editor.update(cx, |editor, cx| editor.select_range(gap..gap, cx));
            window.render_frame(cx);
            assert_eq!(editor.read(cx).statement_highlight(cx), None, "空白里不高亮");

            // 悬停与跳转定义走同一份解析。
            let orders = text.find("orders").unwrap() + 1;
            let hover = editor.read(cx).hover_text(orders, cx).expect("悬停表名");
            assert!(hover.contains("orders"), "{hover}");
            assert!(editor.read(cx).hover_text(text.find("select 3").unwrap() + 7, cx).is_none());
            assert!(editor.update(cx, |editor, cx| editor.follow_definition_at(orders, cx)));
            assert!(!editor.update(cx, |editor, cx| editor.follow_definition_at(1, cx)), "关键字不跳转");
            assert_eq!(followed.borrow().len(), 1);
            assert_eq!(followed.borrow()[0].name, "orders");

            // 错误标记：画上之后，下一次编辑就清掉。
            let failed = text.find("select customer").unwrap()..text.find("orders;").unwrap() + 6;
            editor.update(cx, |editor, cx| editor.mark_errors(vec![failed.clone()], cx));
            window.render_frame(cx);
            assert_eq!(editor.read(cx).error_ranges(cx), vec![failed]);
            editor.update(cx, |editor, cx| {
                editor.state.update(cx, |state, cx| state.replace_text_in_range(Some(0..0), "-- ", window, cx));
            });
        })
        .unwrap();
        // 编辑事件在这一轮更新结束后才送达订阅者。
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(editor.read(cx).error_ranges(cx).is_empty(), "编辑后错误标记清除");
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn hover_shows_every_diagnostic_and_insert_stays_on_the_undo_stack(cx: &mut TestAppContext) {
        let (window, editor) = open(cx);
        cx.update_window(window, |_, window, cx| {
            editor.update(cx, |editor, cx| editor.set_sql("select bad from orders;", window, cx));
            window.render_frame(cx);
            let text = editor.read(cx).value(cx);
            let bad = text.find("bad").unwrap();
            let orders = text.find("orders").unwrap();
            editor.update(cx, |editor, cx| {
                editor.mark_error_messages(vec![(bad..bad + 3, "no such column: bad".to_owned())], cx);
                let version = editor.version();
                assert!(editor.apply_lint_all(
                    version,
                    &[(0..6, "引号没有闭合。".to_owned()), (orders..orders + "orders".len(), "块注释没有闭合。".to_owned())],
                    cx,
                ));
                assert!(!editor.apply_lint_all(version.saturating_sub(1), &[(0..1, "过期".to_owned())], cx));
            });
            let hover = editor.read(cx).hover_text(bad, cx).expect("错误范围要有悬停");
            assert!(hover.contains("no such column: bad"), "执行错误优先于文档：{hover}");
            assert!(hover.contains(&i18n::text(Locale::ZhCn, "query.log.error")), "{hover}");
            let lint = editor.read(cx).hover_text(orders, cx).expect("第二条静态诊断也要能悬停");
            assert!(lint.contains("块注释没有闭合。"), "{lint}");
            assert_eq!(editor.read(cx).lint_messages(), vec!["引号没有闭合。".to_owned(), "块注释没有闭合。".to_owned()]);
            assert_eq!(editor.read(cx).lint_message(cx).as_deref(), Some("引号没有闭合。"));
            editor.update(cx, |editor, cx| {
                editor.select_range(0..0, cx);
                editor.insert_at_selection("/*x*/", window, cx);
            });
        })
        .unwrap();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(editor.read(cx).value(cx).starts_with("/*x*/"), "{}", editor.read(cx).value(cx));
            window.press("secondary-z", cx);
        })
        .unwrap();
        cx.update_window(window, |_, window, cx| {
            let value = editor.read(cx).value(cx);
            assert!(!value.contains("/*x*/"), "插入走编辑器撤销栈：{value}");
            assert!(value.starts_with("select"), "{value}");
        })
        .unwrap();
    }
}
