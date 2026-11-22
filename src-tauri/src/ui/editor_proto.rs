//! SQL editor prototype on GPUI Kit's Editor.
#![cfg_attr(not(test), allow(dead_code, unused_imports))]
//! Dialect rules stay out of this gate; the prototype checks IME, long text,
//! selection, completion, lint, folding and undo against the real control.

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;

use gpui_kit::base::input::{Diagnostic, DiagnosticSeverity};
use gpui_kit::component::input::{
    BufferPoint, CompletionProvider, DefinitionProvider, Editor, EditorState, FoldRange, GutterDecoration, GutterDecorationCollection,
    HoverProvider, InputEvent, RangeDecoration, RangeDecorationCollection, RangeDecorationStyle, Rope, RopeExt as _,
};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    deferred, point, AnyElement, App, AppContext as _, AvailableSpace, Bounds, ClipboardItem, Context, Element, ElementId, Entity,
    EntityInputHandler as _, Focusable as _, FontWeight, GlobalElementId, InspectorElementId, InteractiveElement as _, IntoElement,
    LayoutId, ParentElement as _, Pixels, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Task, TestSupportExt as _, Window, div, px, rgb,
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
    let engine = crate::adapters::query::sql_dialect_id(engine);
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
        EntryInfo::Object { schema, name, kind, comment, columns } => {
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
            if !columns.is_empty() {
                const MAX_DOC_COLS: usize = 8;
                let mut col_lines = vec![format!("**{}**", i18n::fill(&t("query.info.columnsHeader"), &[("count", &columns.len().to_string())]))];
                for col in columns.iter().take(MAX_DOC_COLS) {
                    let mut parts = vec![code_span(&col.name)];
                    if !col.type_name.trim().is_empty() {
                        parts.push(escape_markdown(col.type_name.trim()));
                    }
                    if col.primary {
                        parts.push(t("query.side.primary"));
                    }
                    if col.foreign_key {
                        parts.push(t("query.side.foreign"));
                    }
                    if col.nullable == Some(false) && !col.primary {
                        parts.push(t("query.info.notNull"));
                    }
                    if !col.comment.trim().is_empty() {
                        parts.push(format!("— {}", escape_markdown(col.comment.trim())));
                    }
                    col_lines.push(format!("- {}", parts.join(" · ")));
                }
                if columns.len() > MAX_DOC_COLS {
                    let rest = columns.len() - MAX_DOC_COLS;
                    col_lines.push(format!("_{}_", i18n::fill(&t("query.info.moreColumns"), &[("count", &rest.to_string())])));
                }
                lines.push(col_lines.join("\n"));
            }
        }
        EntryInfo::Function { signature } => lines.push(code_span(signature)),
    }
    lines.join("\n\n")
}

/// Markdown 行内代码的反引号（不是 SQL 标识符引号）。
const TICK: char = '\u{60}';

fn code_span(text: &str) -> String {
    format!("{TICK}{}{TICK}", text.replace(TICK, "'"))
}

fn escape_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if matches!(ch, '\\' | TICK | '*' | '_' | '[' | ']' | '<' | '>' | '#' | '|') {
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

/// track 的去重键：(文本版本, 光标, 选区起止, 文本长度, 深色)。
type TrackKey = (u64, usize, (usize, usize), usize, bool);

pub struct SqlEditor {
    state: Entity<EditorState>,
    /// 连接 Redis 时按 Redis 命令高亮；否则按 SQL。
    redis: bool,
    /// 已经装上带 Redis 分支的高亮工厂（第一次切到 Redis 时安装，之后一直沿用）。
    redis_factory: bool,
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
    /// 上次按光标计算时的 (版本, 光标, 选区, 长度, 深色)，同一帧内只算一次。
    tracked: Option<TrackKey>,
    /// 底部状态行：光标行列与选区大小，随 track 一起更新。
    caret: Caret,
    statement_marks: RangeDecorationCollection,
    error_marks: RangeDecorationCollection,
    /// 出错语句的整段底色（比错误位置本身浅一档）。
    error_statements: RangeDecorationCollection,
    error_gutter: GutterDecorationCollection,
    /// 当前错误标记的范围；下一次编辑或重新执行时清除。
    errors: Vec<Range<usize>>,
    /// 当前执行错误（行尾标记与错误弹层读取）；与 errors 同时清除。
    exec_errors: Vec<ExecError>,
    /// 展开着第几个行尾错误标记的错误弹层。
    lens_open: Option<usize>,
    /// 鼠标正停在第几个标记 / 第几个弹层上。标记和弹层之间隔着出错语句，
    /// 离开其中一个时延迟收起，移到另一个上就保持展开。
    lens_chip_hover: Option<usize>,
    lens_card_hover: Option<usize>,
    /// 点击标记后固定展开（再点一次收起），不依赖鼠标停留。
    lens_pinned: Option<usize>,
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

/// 一条执行错误：标记位置、所在语句、错误码与服务器原文。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExecError {
    /// 错误位置（驱动给的偏移/行；没有时是整条语句）。
    pub range: Range<usize>,
    /// 出错语句在草稿里的范围；铺语句底色用。
    pub statement: Option<Range<usize>>,
    /// 驱动给出的错误码（如 1064 / 42601）；认不出时为 None。
    pub code: Option<String>,
    pub message: String,
}

impl ExecError {
    /// 复制按钮写入剪贴板的完整文本：错误码 + 原文。
    pub fn copy_text(&self) -> String {
        match self.code.as_deref() {
            Some(code) if !self.message.contains(code) => format!("[{code}] {}", self.message),
            _ => self.message.clone(),
        }
    }

    /// 行尾标记里的简短说明：原文第一行，过长时截断。
    pub fn short(&self) -> String {
        let first = self.message.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("");
        let mut out: String = first.chars().take(SHORT_CHARS).collect();
        if first.chars().count() > SHORT_CHARS {
            out.push('…');
        }
        out
    }
}

/// 行尾错误标记最多显示的字符数。
const SHORT_CHARS: usize = 48;

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

/// 执行错误在诊断弹层里的说明：标题带错误码，正文是服务器原文。
fn exec_error_markdown(error: &ExecError, locale: Locale) -> String {
    let title = i18n::text(locale, "query.log.error");
    let title = match error.code.as_deref() {
        Some(code) => format!("{title} · {}", code_span(code)),
        None => title,
    };
    let body = if error.message.trim().is_empty() { String::new() } else { escape_markdown(&error.message) };
    format!("**{title}**\n\n{body}")
}

/// LSP 悬停的内容。诊断（执行错误、静态 lint）只经编辑器的诊断弹层展示：
/// 悬停在诊断范围里时这里不再给第二个弹层，两个弹层叠在一起、此起彼伏正是闪烁的来源。
fn hover_markdown(scope: Option<&CompletionScope>, facts: &HoverFacts, text: &str, offset: usize, locale: Locale) -> Option<(Range<usize>, String)> {
    if diagnostic_at(facts, offset).is_some() {
        return None;
    }
    let (span, info) = hover_info(scope?, text, offset)?;
    Some((span, info_markdown(&info, locale)))
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
            state.lsp_mut().completion_menu.max_width = px(440.);
            state.lsp_mut().set_completion_highlighter(Rc::new(highlight_sql_completion));
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
        let (statement_marks, error_statements, error_marks, error_gutter) = state.update(cx, |state, cx| {
            (
                state.create_range_decorations_collection(Vec::new(), cx),
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
            redis: false,
            redis_factory: false,
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
            caret: Caret::default(),
            statement_marks,
            error_marks,
            error_statements,
            error_gutter,
            errors: Vec::new(),
            exec_errors: Vec::new(),
            lens_open: None,
            lens_chip_hover: None,
            lens_card_hover: None,
            lens_pinned: None,
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

    /// 按连接类型切换高亮语言：Redis 用命令词法高亮，其余回到 SQL。从未连过 Redis 的编辑器不动组件库的默认高亮。
    pub fn set_redis(&mut self, redis: bool, cx: &mut Context<Self>) {
        if self.redis == redis {
            return;
        }
        self.redis = redis;
        let install = redis && !self.redis_factory;
        self.redis_factory |= install;
        self.state.update(cx, |state, cx| {
            if install {
                state.set_highlighter_factory(super::redis_highlight::highlighter_factory(), cx);
            }
            state.set_highlighter(if redis { super::redis_highlight::REDIS_LANGUAGE } else { "sql" }, cx);
        });
        cx.notify();
    }

    pub fn is_redis(&self) -> bool {
        self.redis
    }

    /// 编辑器当前的高亮语言名（测试用）。
    pub fn language(&self, cx: &App) -> SharedString {
        self.state.read(cx).language_name()
    }

    fn engine(&self) -> String {
        self.scope.borrow().as_ref().map(|scope| scope.engine.clone()).unwrap_or_default()
    }

    /// 光标移动或文本变化后重算当前语句高亮和签名提示；同一状态只算一次。
    fn track(&mut self, cx: &mut Context<Self>) -> bool {
        let (text, cursor, selection) = {
            let state = self.state.read(cx);
            (state.text().clone(), state.cursor(), state.selected_range())
        };
        let dark = cx.theme().is_dark();
        let key = (self.version.get(), cursor, (selection.start, selection.end), text.len(), dark);
        let previous = self.tracked;
        if previous == Some(key) {
            return false;
        }
        self.tracked = Some(key);
        let value = text.to_string();
        // 状态行只在光标或选区真的变了时才要求重绘；文本变化本来就会重绘。
        let caret = caret_of(&value, selection, cursor);
        let caret_moved = caret != self.caret;
        self.caret = caret;
        let engine = self.engine();
        let current = statement_at(&engine, &value, cursor);
        if current != self.current_statement || previous.is_some_and(|(_, _, _, _, was)| was != dark) {
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
            return caret_moved;
        }
        self.signature = hint;
        true
    }

    /// 底部状态行读到的光标与选区（测试和查询页读取）。
    pub fn caret(&self) -> Caret {
        self.caret
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

    /// 执行失败的位置和错误原文（没有错误码与语句范围的旧入口）。
    pub fn mark_error_messages(&mut self, marked: Vec<(Range<usize>, String)>, cx: &mut Context<Self>) {
        let errors = marked.into_iter().map(|(range, message)| ExecError { range, message, ..ExecError::default() }).collect();
        self.mark_exec_errors(errors, cx);
    }

    /// 执行错误的呈现：行号侧标记 + 出错语句底色 + 错误位置底色与波浪线 + 行尾简短标记。
    /// 原文只经编辑器的诊断弹层展示（与静态 lint 同一个弹层），行尾标记展开后可以复制。
    pub fn mark_exec_errors(&mut self, errors: Vec<ExecError>, cx: &mut Context<Self>) {
        let palette = Palette::of(cx);
        let mut fill = palette.danger;
        fill.a = 0.18;
        let mut soft = palette.danger;
        soft.a = 0.07;
        let ranges: Vec<Range<usize>> = errors.iter().map(|error| error.range.clone()).collect();
        let gutter = ranges.iter().cloned().map(|range| GutterDecoration::new(range).with_color(palette.danger)).collect();
        let marks = ranges.iter().cloned().map(|range| RangeDecoration::new(range).with_style(RangeDecorationStyle::Fill).with_color(fill)).collect();
        let statements = errors
            .iter()
            .filter_map(|error| error.statement.clone().filter(|statement| !statement.is_empty()))
            .map(|range| RangeDecoration::new(range).with_style(RangeDecorationStyle::Fill).with_color(soft))
            .collect();
        self.error_gutter.set(gutter, cx);
        self.error_marks.set(marks, cx);
        self.error_statements.set(statements, cx);
        self.errors = ranges;
        let locale = self.locale.get();
        self.facts.borrow_mut().errors = errors.iter().map(|error| (error.range.clone(), exec_error_markdown(error, locale))).collect();
        self.exec_errors = errors;
        self.reset_lens();
        self.sync_diagnostics(cx);
        cx.notify();
    }

    pub fn clear_errors(&mut self, cx: &mut Context<Self>) {
        if self.errors.is_empty() && self.facts.borrow().errors.is_empty() {
            return;
        }
        self.errors.clear();
        self.exec_errors.clear();
        self.reset_lens();
        self.facts.borrow_mut().errors.clear();
        self.error_gutter.clear(cx);
        self.error_marks.clear(cx);
        self.error_statements.clear(cx);
        self.sync_diagnostics(cx);
        cx.notify();
    }

    /// 当前执行错误（行尾标记、错误弹层与复制读取同一份）。
    pub fn exec_errors(&self) -> &[ExecError] {
        &self.exec_errors
    }

    /// 诊断弹层的唯一来源：执行错误在前、静态 lint 在后，一起写进编辑器的诊断集合。
    fn sync_diagnostics(&mut self, cx: &mut Context<Self>) {
        let facts = self.facts.borrow();
        let locale = self.locale.get();
        let entries: Vec<(Range<usize>, String)> = facts
            .errors
            .iter()
            .cloned()
            .chain(facts.lint.iter().map(|(range, message)| (range.clone(), diagnostic_markdown(message, locale))))
            .collect();
        drop(facts);
        self.state.update(cx, |state, _| {
            let text = state.text().clone();
            let Some(set) = state.diagnostics_mut() else {
                return;
            };
            set.reset(&text);
            for (range, message) in entries {
                if range.is_empty() || message.is_empty() {
                    continue;
                }
                let start = offset_to_position(&text, range.start);
                let end = offset_to_position(&text, range.end.min(text.len()));
                set.push(Diagnostic::new(start..end, message).with_severity(DiagnosticSeverity::Error));
            }
        });
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
        if diagnostic_at(&self.facts.borrow(), offset).is_some() {
            return self.diagnostic_text(offset, cx);
        }
        self.lsp_hover_text(offset, cx)
    }

    /// LSP 悬停提供者在偏移处给出的内容（诊断范围里为 None：诊断只走诊断弹层）。
    pub fn lsp_hover_text(&self, offset: usize, cx: &App) -> Option<String> {
        let text = self.value(cx);
        let scope = self.scope.borrow();
        hover_markdown(scope.as_ref(), &self.facts.borrow(), &text, offset, self.locale.get()).map(|(_, markdown)| markdown)
    }

    /// 诊断弹层在偏移处显示的内容（编辑器诊断集合里的那一条）。
    pub fn diagnostic_text(&self, offset: usize, cx: &App) -> Option<String> {
        let state = self.state.read(cx);
        let set = state.diagnostics()?;
        set.for_offset(offset).map(|entry| entry.diagnostic.message.to_string())
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

    /// 一次性替换一个字节范围，并把选区恢复成 replace 之后的样子。
    /// 整个替换只进撤销栈一次：一次 Ctrl/Cmd+Z 就回到替换前。
    pub fn replace_bytes(
        &mut self,
        range: Range<usize>,
        text: &str,
        selection: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let applied = self.state.update(cx, |state, cx| {
            // replace_text_in_range 收的是 UTF-16 偏移，先按当前文本换算。
            let value = state.value();
            let start = utf16_offset(&value, range.start);
            let end = utf16_offset(&value, range.end);
            state.replace_text_in_range(Some(start..end), text, window, cx);
            state.set_selected_range(selection, cx);
            state.focus(window, cx);
            true
        });
        if applied {
            self.clear_errors(cx);
        }
        applied
    }

    /// Ctrl/⌘+/：切换选区触及行的行注释；没有选区时作用于光标所在语句的全部逻辑行。
    /// 整段连续行只替换一次（一次撤销即可还原），不执行查询；输入法组合输入期间不动作。
    pub fn toggle_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.composing(window, cx) {
            return false;
        }
        let value = self.value(cx);
        let selection = self.selected_range(cx);
        let engine = crate::adapters::query::sql_dialect_id(&self.engine());
        let Some(edit) = crate::query::toggle_statement_comment(engine, &value, selection) else {
            return false;
        };
        self.replace_bytes(edit.range, &edit.text, edit.selection, window, cx)
    }

    /// Ctrl/⌘+Shift+/：切换块注释。有选区时包裹/去掉选区，没有选区时作用于光标所在的块注释或语句。
    pub fn toggle_block_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.composing(window, cx) {
            return false;
        }
        let value = self.value(cx);
        let selection = self.selected_range(cx);
        let engine = crate::adapters::query::sql_dialect_id(&self.engine());
        let Some(edit) = crate::query::toggle_block_comment(engine, &value, selection) else {
            return false;
        };
        self.replace_bytes(edit.range, &edit.text, edit.selection, window, cx)
    }

    fn composing(&self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.state.update(cx, |state, cx| state.marked_text_range(window, cx).is_some())
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
        self.sync_diagnostics(cx);
        true
    }

    /// 当前全部静态诊断原文，按写入顺序。
    pub fn lint_messages(&self) -> Vec<String> {
        self.facts.borrow().lint.iter().map(|(_, message)| message.clone()).collect()
    }

    /// 第一条静态诊断的原文；只有它确实写进了编辑器的诊断集合时才算数。
    pub fn lint_message(&self, cx: &App) -> Option<String> {
        let first = self.facts.borrow().lint.first().map(|(_, message)| message.clone())?;
        let written = self.state.read(cx).diagnostics().is_some_and(|set| !set.is_empty());
        written.then_some(first)
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
        self.sync_diagnostics(cx);
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
        let lenses = self.error_lenses(cx);
        let status = self.caret_status(cx);
        div()
            .id("sql-editor")
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .text_size(px(self.font_size))
            .text_color(rgb(foreground))
            .child(
                div().flex_1().min_h(px(0.)).child(
                    Editor::new(&self.state)
                        .h(gpui_kit::relative(1.))
                        .bordered(false)
                        .aria_label("SQL")
                        .text_size(px(self.font_size))
                        .bg(rgb(background))
                        .text_color(rgb(foreground)),
                ),
            )
            .child(status)
            .children(lenses)
            .children(popup)
    }
}

/// 贴着编辑器文本定位的浮层。位置在本帧编辑器 prepaint 之后计算（作为 deferred 子元素，
/// 它的 prepaint 排在编辑器之后），读到的是本帧的光标几何与滚动偏移，不用上一帧的布局：
/// 滚动时不会先跳到旧位置再跳回来。几何缺失或目标不在可见区时整块不画（不回退到固定位置）。
/// 给定浮层尺寸，返回它的左上角（窗口坐标）；None 表示这一帧不显示。
type Placer = Box<dyn Fn(&EditorState, gpui_kit::Size<Pixels>) -> Option<gpui_kit::Point<Pixels>>>;

/// 外壳此刻有没有盖在工作区上的浮层（模态、命令面板、右键菜单……）。浮层是普通层，
/// 编辑器的 [`Floating`] 是 deferred 绘制、会画在它们上面，所以有浮层时这一帧不画。
/// 按窗口记录：一个工作台窗口的浮层只影响该窗口里的编辑器，不会藏掉其他窗口的错误标记。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShellOverlayOpen {
    covered: std::collections::HashSet<gpui::WindowId>,
}

impl gpui_kit::Global for ShellOverlayOpen {}

impl ShellOverlayOpen {
    /// 外壳每帧报告当前窗口是否有盖在工作区上的浮层。状态未变时不写全局，避免无谓的观察者通知。
    pub fn set(window: &Window, covered: bool, cx: &mut App) {
        let id = window.window_handle().window_id();
        if cx.try_global::<Self>().is_some_and(|state| state.covered.contains(&id) == covered) {
            return;
        }
        let state = cx.default_global::<Self>();
        if covered {
            state.covered.insert(id);
        } else {
            state.covered.remove(&id);
        }
    }

    pub fn is_covered(window: &Window, cx: &App) -> bool {
        let id = window.window_handle().window_id();
        cx.try_global::<Self>().is_some_and(|state| state.covered.contains(&id))
    }
}

fn shell_overlay_open(window: &Window, cx: &App) -> bool {
    ShellOverlayOpen::is_covered(window, cx)
}

struct Floating {
    id: ElementId,
    state: Entity<EditorState>,
    place: Placer,
    child: Option<AnyElement>,
}

impl Floating {
    fn new(
        id: impl Into<ElementId>,
        state: Entity<EditorState>,
        child: impl IntoElement,
        place: impl Fn(&EditorState, gpui_kit::Size<Pixels>) -> Option<gpui_kit::Point<Pixels>> + 'static,
    ) -> Self {
        Self { id: id.into(), state, place: Box::new(place), child: Some(child.into_any_element()) }
    }
}

impl IntoElement for Floating {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Floating {
    type RequestLayoutState = ();
    type PrepaintState = bool;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        let mut empty = div().into_any_element();
        (empty.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> bool {
        let Some(child) = self.child.as_mut() else {
            return false;
        };
        if shell_overlay_open(window, cx) {
            return false;
        }
        let size = child.layout_as_root(AvailableSpace::min_size(), window, cx);
        let Some(origin) = (self.place)(self.state.read(cx), size) else {
            return false;
        };
        window.with_absolute_element_offset(origin, |window| child.prepaint(window, cx));
        true
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        visible: &mut bool,
        window: &mut Window,
        cx: &mut App,
    ) {
        if *visible {
            if let Some(child) = self.child.as_mut() {
                child.paint(window, cx);
            }
        }
    }
}

/// 字节偏移在本帧的显示行顶边（窗口坐标）与行高。以本帧光标为基准按显示行差推算，
/// 软换行与折叠都由编辑器自己的显示映射换算。光标几何还没有、或这一行被折叠隐藏时为 None。
fn row_top(state: &EditorState, offset: usize) -> Option<(Pixels, Pixels)> {
    let geometry = state.prepaint_caret_geometry()?;
    let text = state.text();
    let map = state.display_map();
    let display_row = |at: usize| {
        let at = at.min(text.len());
        let point = text.offset_to_point(at);
        map.buffer_pos_to_display_pos(BufferPoint::new(point.row, point.column)).row
    };
    let line = text.offset_to_point(offset.min(text.len())).row;
    if map.is_buffer_line_hidden(line) {
        return None;
    }
    let caret_row = display_row(state.cursor()) as f32;
    let target_row = display_row(offset) as f32;
    let line_height = geometry.line_height();
    let caret_top = geometry.cursor_bounds().origin.y;
    Some((caret_top + line_height * (target_row - caret_row), line_height))
}

/// 浮层能放在编辑器可见区里（上下都不越界）。
fn inside_input(state: &EditorState, top: Pixels, height: Pixels) -> bool {
    let Some(geometry) = state.prepaint_caret_geometry() else {
        return false;
    };
    let input = geometry.input_bounds();
    top >= input.origin.y && top + height <= input.origin.y + input.size.height
}

/// 离开错误标记或弹层后，等这么久再收起（够鼠标从标记移到隔着语句的弹层上）。
const LENS_CLOSE_DELAY: std::time::Duration = std::time::Duration::from_millis(400);

/// 错误弹层的顶边：出错语句（从首个显示行顶边到最后一个显示行底边）下方在编辑器里放得下就放下方，
/// 否则上方放得下放上方；都放不下时仍放在语句下方，越过编辑器下缘压在结果区上，而不压在语句上。
/// 语句的下缘已经滚到可见区以下时贴着编辑器下缘之外。
fn popover_top(statement: (Pixels, Pixels), height: Pixels, input: (Pixels, Pixels)) -> Pixels {
    let gap = px(4.);
    let (first, last) = statement;
    let (input_top, input_bottom) = input;
    let below = last + gap;
    if below + height <= input_bottom {
        return below;
    }
    let above = first - gap - height;
    if above >= input_top {
        return above;
    }
    below.min(input_bottom)
}

fn kit_icon_alert(color: gpui_kit::Hsla) -> AnyElement {
    crate::ui::kit::icon(gpui_kit::assets::IconName::TriangleAlert, 11., color).into_any_element()
}

/// 编辑器底部状态行要显示的光标位置与选区大小。行列从 1 开始，列按字符数计。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Caret {
    pub line: usize,
    pub col: usize,
    /// 选中的字符数；没有选区时为 0。
    pub chars: usize,
    /// 选区跨过的行数；没有选区时为 0。
    pub lines: usize,
}

/// 按字节偏移算出光标行列与选区大小。偏移越界或不在字符边界上时向前收到最近的边界。
pub fn caret_of(text: &str, selection: Range<usize>, head: usize) -> Caret {
    let floor = |offset: usize| {
        let mut offset = offset.min(text.len());
        while !text.is_char_boundary(offset) {
            offset -= 1;
        }
        offset
    };
    let head = floor(head);
    let before = &text[..head];
    let line_start = before.rfind('\n').map(|at| at + 1).unwrap_or(0);
    let line = before.matches('\n').count() + 1;
    let col = before[line_start..].chars().count() + 1;
    let (start, end) = (floor(selection.start.min(selection.end)), floor(selection.start.max(selection.end)));
    if start == end {
        return Caret { line, col, chars: 0, lines: 0 };
    }
    let picked = &text[start..end];
    let chars = picked.chars().count();
    // 选区停在下一行行首时，那一行没有被选中任何字符，不算进去。
    let lines = picked.trim_end_matches('\n').matches('\n').count() + 1;
    Caret { line, col, chars, lines }
}

impl SqlEditor {
    /// 底部状态行：右对齐的“已选…”与“行 n，列 m”。比编辑器正文低一档，不和代码抢视线。
    fn caret_status(&self, cx: &App) -> gpui_kit::AnyElement {
        let p = Palette::of(cx);
        let locale = self.locale.get();
        let caret = self.caret;
        let position = i18n::fill(&i18n::text(locale, "query.editor.caret"), &[("line", &caret.line.max(1).to_string()), ("col", &caret.col.max(1).to_string())]);
        let selection = match caret.lines {
            0 => None,
            1 => Some(i18n::fill(&i18n::text(locale, "query.editor.selection"), &[("chars", &caret.chars.to_string())])),
            lines => Some(i18n::fill(&i18n::text(locale, "query.editor.selectionLines"), &[("chars", &caret.chars.to_string()), ("lines", &lines.to_string())])),
        };
        div()
            .id("sql-editor-status")
            .flex_none()
            .h(px(22.))
            .px(px(10.))
            .flex()
            .items_center()
            .justify_end()
            .gap(px(14.))
            .border_t_1()
            .border_color(p.line)
            .bg(p.chrome)
            .text_size(px(TypeScale::XS))
            .text_color(p.text_faint)
            .children(selection.map(|text| div().id("sql-editor-selection").text_color(p.text_muted).child(SharedString::from(text))))
            .child(div().id("sql-editor-caret").test_support().child(SharedString::from(position)))
            .into_any_element()
    }
}

impl SqlEditor {
    fn reset_lens(&mut self) {
        self.lens_open = None;
        self.lens_chip_hover = None;
        self.lens_card_hover = None;
        self.lens_pinned = None;
    }

    /// 第几个错误弹层正在显示（测试读取）。
    pub fn error_popover_open(&self) -> Option<usize> {
        self.lens_open.or(self.lens_pinned)
    }

    /// 标记或弹层的悬停变化。移上去立即展开；移开后延迟一小段再看两者是否都已离开，
    /// 鼠标从标记移到弹层（中间隔着出错语句）不会让弹层先消失。
    fn lens_hover(&mut self, index: usize, card: bool, hovered: bool, cx: &mut Context<Self>) {
        let slot = if card { &mut self.lens_card_hover } else { &mut self.lens_chip_hover };
        if hovered {
            *slot = Some(index);
            if self.lens_open != Some(index) {
                self.lens_open = Some(index);
                cx.notify();
            }
            // 标记与弹层遮住了编辑器的鼠标移动：鼠标从波浪线移到这里时，编辑器自己的诊断弹层收不到
            // “离开”，会和错误弹层同时挂在屏上（原生截图里两个弹层并存）。同一时刻只留一个。
            self.state.update(cx, |state, cx| state.clear_diagnostic_popover(cx));
            return;
        }
        if *slot == Some(index) {
            *slot = None;
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(LENS_CLOSE_DELAY).await;
            let _ = this.update(cx, |this, cx| {
                if this.lens_open == Some(index) && this.lens_chip_hover != Some(index) && this.lens_card_hover != Some(index) {
                    this.lens_open = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// 行尾错误标记：出错行右侧一个简短标记（错误码 + 原文首行）。鼠标移上去或点击展开错误弹层：
    /// 错误码、完整信息和复制按钮。弹层放在出错语句整段的下方（放不下时放在上方，都放不下时放到
    /// 编辑器下缘之外），贴着编辑器右缘，不压住出错语句的任何一行。
    fn error_lenses(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        if self.exec_errors.is_empty() {
            return Vec::new();
        }
        let p = Palette::of(cx);
        let locale = self.locale.get();
        let mut seen_lines: Vec<usize> = Vec::new();
        let text = self.state.read(cx).text().clone();
        let shown = self.error_popover_open();
        let mut out = Vec::new();
        for (index, error) in self.exec_errors.iter().enumerate() {
            let line = text.offset_to_point(error.range.start.min(text.len())).row;
            if seen_lines.contains(&line) {
                continue;
            }
            seen_lines.push(line);
            let label = match error.code.as_deref() {
                Some(code) => format!("{code} · {}", error.short()),
                None => error.short(),
            };
            let chip = div()
                .id(("sql-error-lens-chip", index))
                .test_support()
                .flex()
                .items_center()
                .gap(px(4.))
                .px(px(6.))
                .h(px(18.))
                .rounded(px(Radius::CONTROL))
                .bg(p.danger_soft)
                .text_color(p.danger)
                .text_size(px(TypeScale::XS))
                .cursor_pointer()
                .occlude()
                .on_hover(cx.listener(move |this, hovered: &bool, _, cx| this.lens_hover(index, false, *hovered, cx)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.lens_pinned = if this.lens_pinned == Some(index) { None } else { Some(index) };
                    if this.lens_pinned.is_some() {
                        this.state.update(cx, |state, cx| state.clear_diagnostic_popover(cx));
                    }
                    if this.lens_pinned.is_none() && this.lens_open == Some(index) && this.lens_chip_hover != Some(index) {
                        this.lens_open = None;
                    }
                    cx.notify();
                }))
                .child(kit_icon_alert(p.danger))
                .child(SharedString::from(label));
            let offset = error.range.start;
            let floating = Floating::new(("sql-error-lens-anchor", index), self.state.clone(), chip, move |state, size| {
                let (top, line_height) = row_top(state, offset)?;
                let geometry = state.prepaint_caret_geometry()?;
                let input = geometry.input_bounds();
                let chip_top = top + (line_height - px(18.)).max(px(0.)) / 2.;
                if !inside_input(state, chip_top, px(18.)) {
                    return None;
                }
                // 右缘留出纵向滚动条的位置。
                let x = (input.origin.x + input.size.width - size.width - px(14.)).max(input.origin.x);
                Some(point(x, chip_top))
            });
            out.push(deferred(floating).with_priority(1).into_any_element());
            if shown == Some(index) {
                let card = self.error_popover(index, error, locale, cx);
                let statement = error.statement.clone().unwrap_or_else(|| error.range.clone());
                let floating = Floating::new(("sql-error-popover-anchor", index), self.state.clone(), card, move |state, size| {
                    let geometry = state.prepaint_caret_geometry()?;
                    let input = geometry.input_bounds();
                    // 错误标记本身已经滚出可见区时不弹（与标记同进同出）。
                    let (top, line_height) = row_top(state, offset)?;
                    let chip_top = top + (line_height - px(18.)).max(px(0.)) / 2.;
                    if !inside_input(state, chip_top, px(18.)) {
                        return None;
                    }
                    let (first, _) = row_top(state, statement.start).unwrap_or((top, line_height));
                    let (last, last_height) = row_top(state, statement.end).unwrap_or((top, line_height));
                    let span = (first, last + last_height);
                    let y = popover_top(span, size.height, (input.origin.y, input.origin.y + input.size.height));
                    let x = (input.origin.x + input.size.width - size.width - px(14.)).max(input.origin.x);
                    Some(point(x, y))
                });
                out.push(deferred(floating).with_priority(2).into_any_element());
            }
        }
        out
    }

    fn error_popover(&self, index: usize, error: &ExecError, locale: Locale, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let copy_text = error.copy_text();
        let title = match error.code.as_deref() {
            Some(code) => i18n::fill(&i18n::text(locale, "query.errors.codeTitle"), &[("code", code)]),
            None => i18n::text(locale, "query.log.error"),
        };
        div()
            .id(("sql-error-popover", index))
            .test_support()
            .occlude()
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| this.lens_hover(index, true, *hovered, cx)))
            .flex()
            .flex_col()
            .gap(px(6.))
            .max_w(px(460.))
            .p(px(8.))
            .rounded(px(Radius::PANEL))
            .border_1()
            .border_color(p.danger)
            .bg(p.raised)
            .shadow_md()
            .text_size(px(TypeScale::SM))
            .text_color(p.text)
            .child(div().id(("sql-error-popover-title", index)).font_weight(FontWeight::SEMIBOLD).text_color(p.danger).child(SharedString::from(title)))
            .child(div().id(("sql-error-popover-message", index)).child(SharedString::from(error.message.clone())))
            .child(
                div().flex().justify_end().child(crate::ui::kit::secondary_button(
                    ("sql-error-popover-copy", index),
                    Some(gpui_kit::assets::IconName::Copy),
                    i18n::text(locale, "query.errors.copy"),
                    move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copy_text.clone())),
                )),
            )
            .into_any_element()
    }

    /// 签名提示浮层：贴在光标上方，不压住下方的补全弹层；还没有光标几何时不显示。
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
        // 位置在本帧编辑器 prepaint 之后算：光标几何已经是本帧的（含滚动），不再额外叠加滚动偏移；
        // 几何缺失时不画，不跳到固定位置。
        let floating = Floating::new("sql-signature-anchor", self.state.clone(), row, |state, size| {
            let geometry = state.prepaint_caret_geometry()?;
            let caret = geometry.cursor_bounds();
            let input = geometry.input_bounds();
            if caret.origin.y < input.origin.y || caret.origin.y > input.origin.y + input.size.height {
                return None;
            }
            let x = (caret.origin.x - px(4.)).max(input.origin.x).min((input.origin.x + input.size.width - size.width).max(input.origin.x));
            let above = caret.origin.y - px(4.) - size.height;
            // 上方放不下时放到光标行下方，仍然不压住光标所在行。
            let y = if above >= px(0.) { above } else { caret.origin.y + geometry.line_height() + px(4.) };
            Some(point(x, y))
        });
        deferred(floating).with_priority(1).into_any_element()
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
                    // 超过 8 条候选时弹层右侧会出现纵向滚动条；给右侧详情预留 1em 安全边距，防止滚动条压住文字。
                    let scroll_gutter = planned.candidates.len() > 8;
                    for entry in &planned.candidates {
                        let detail = if entry.detail.is_empty() {
                            None
                        } else if scroll_gutter {
                            Some(format!("{}\u{2003}", entry.detail))
                        } else {
                            Some(entry.detail.clone())
                        };
                        let mut item = completion_edit(
                            &entry.label,
                            &entry.insert,
                            completion_kind(entry.kind),
                            range.clone(),
                            detail,
                        );
                        // 详情面板：仅当确实有列结构、注释、模式或函数签名等增值信息时才展开右侧卡片，
                        // 避免没有任何附加元数据的表重复弹出仅写着 "`logs` · 表" 的冗余空气泡。
                        item.documentation = entry.info.as_ref().filter(|info| has_meaningful_doc(info)).map(|info| {
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

fn has_meaningful_doc(info: &EntryInfo) -> bool {
    match info {
        EntryInfo::Column { .. } | EntryInfo::Function { .. } => true,
        EntryInfo::Object { schema, comment, columns, .. } => {
            !columns.is_empty() || !comment.trim().is_empty() || !schema.trim().is_empty()
        }
    }
}

/// SQL 补全项匹配高亮：优先在裸标识符（跳过 `schema.` 前缀）中寻找词首、下划线分段或连续子串命中，
/// 退化为字符序列命中；绝不把未命中的整段标签全部染蓝。
pub fn highlight_sql_completion(query: &str, item: &CompletionItem) -> Vec<Range<usize>> {
    let raw = if !item.label.contains('.') {
        query.rsplit_once('.').map(|(_, tail)| tail).unwrap_or(query)
    } else {
        query
    };
    let q = raw.trim();
    if q.is_empty() {
        return Vec::new();
    }
    let (bare_offset, bare) = match item.label.rsplit_once('.') {
        Some((head, tail)) if !q.contains('.') => (head.len() + 1, tail),
        _ => (0, item.label.as_str()),
    };
    if let Some(span) = contiguous_match_in(bare, q) {
        return std::iter::once((bare_offset + span.start)..(bare_offset + span.end)).collect();
    }
    if let Some(span) = contiguous_match_in(&item.label, q) {
        return std::iter::once(span).collect();
    }
    let subseq = subseq_ranges_in(bare, q);
    if !subseq.is_empty() {
        return subseq.into_iter().map(|r| (bare_offset + r.start)..(bare_offset + r.end)).collect();
    }
    gpui_kit::base::input::highlight_characters(q, item)
}

fn contiguous_match_in(haystack: &str, needle: &str) -> Option<Range<usize>> {
    let q_chars: Vec<char> = needle.chars().collect();
    if q_chars.is_empty() {
        return None;
    }
    let h_chars: Vec<(usize, char)> = haystack.char_indices().collect();
    let last_start = h_chars.len().checked_sub(q_chars.len())?;
    let eq_at = |start: usize| {
        q_chars
            .iter()
            .zip(h_chars[start..].iter())
            .all(|(qc, (_, hc))| qc.eq_ignore_ascii_case(hc) || qc.to_lowercase().eq(hc.to_lowercase()))
    };
    let span_at = |start: usize| {
        let byte_start = h_chars[start].0;
        let (last_ix, last_ch) = h_chars[start + q_chars.len() - 1];
        byte_start..(last_ix + last_ch.len_utf8())
    };
    if eq_at(0) {
        return Some(span_at(0));
    }
    for start in 1..=last_start {
        if h_chars[start - 1].1 == '_' && eq_at(start) {
            return Some(span_at(start));
        }
    }
    for start in 1..=last_start {
        if eq_at(start) {
            return Some(span_at(start));
        }
    }
    None
}

fn subseq_ranges_in(haystack: &str, needle: &str) -> Vec<Range<usize>> {
    let mut ranges: Vec<Range<usize>> = Vec::new();
    let mut rest = haystack;
    let mut base = 0usize;
    for qc in needle.chars() {
        let Some((idx, matched)) = rest
            .char_indices()
            .find(|(_, hc)| qc.eq_ignore_ascii_case(hc) || qc.to_lowercase().eq(hc.to_lowercase()))
        else {
            return Vec::new();
        };
        let start = base + idx;
        let end = start + matched.len_utf8();
        let next = idx + matched.len_utf8();
        base += next;
        rest = &rest[next..];
        if let Some(last) = ranges.last_mut() {
            if last.end == start {
                last.end = end;
                continue;
            }
        }
        ranges.push(start..end);
    }
    ranges
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
        filter_text: None,
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
        // IME 组合和中文正文不触发弹层，避免打断输入；下划线和数字继续刷新当前标识符补全。
        new_text.chars().any(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '.')
    }
}

impl HoverProvider for SqlHover {
    fn hover(&self, text: &Rope, offset: usize, _window: &mut Window, _cx: &mut App) -> Task<gpui_kit::Result<Option<Hover>>> {
        let locale = self.locale.get();
        let scope = self.scope.borrow();
        let value = text.to_string();
        // 诊断范围里没有悬停（诊断弹层已经在显示）；认不出的名字也没有，不弹空气泡。
        let hover = hover_markdown(scope.as_ref(), &self.facts.borrow(), &value, offset, locale).map(|(span, markdown)| Hover {
            contents: HoverContents::Markup(MarkupContent { kind: MarkupKind::Markdown, value: markdown }),
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

/// 字节偏移换成 UTF-16 偏移（编辑器输入接口用的单位）；落在字符中间时按字符边界往回取。
fn utf16_offset(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while at > 0 && !text.is_char_boundary(at) {
        at -= 1;
    }
    text[..at].encode_utf16().count()
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

    #[test]
    fn caret_counts_lines_and_characters_not_bytes() {
        let text = "select 1;\n-- 中文注释\nselect 2;";
        // 光标在第二行“中文”之后：列按字符计，不按 UTF-8 字节。
        let head = text.find("注释").unwrap();
        assert_eq!(caret_of(text, head..head, head), Caret { line: 2, col: 6, chars: 0, lines: 0 });
        assert_eq!(caret_of(text, 0..0, 0), Caret { line: 1, col: 1, chars: 0, lines: 0 });
        // 选中第一整行连同换行：停在下一行行首，那一行不算进选区。
        let first = text.find('\n').unwrap() + 1;
        assert_eq!(caret_of(text, 0..first, first), Caret { line: 2, col: 1, chars: 10, lines: 1 });
        // 反向选区与越界、非字符边界的偏移都收到合法位置。
        let all = caret_of(text, text.len() + 5..0, 0);
        assert_eq!((all.chars, all.lines), (text.chars().count(), 3));
        let inside = text.find("中").unwrap() + 1;
        assert_eq!(caret_of(text, inside..inside, inside).col, 4);
    }

    #[gpui_kit::test]
    fn the_status_line_follows_the_cursor_and_selection(cx: &mut TestAppContext) {
        let (window, editor) = open(cx);
        cx.update_window(window, |_, window, cx| {
            editor.update(cx, |editor, cx| editor.set_sql("select 1;\nselect 22;", window, cx));
            window.render_frame(cx);
            editor.update(cx, |editor, cx| editor.select_range(10..16, cx));
            window.render_frame(cx);
            assert_eq!(editor.read(cx).caret(), Caret { line: 2, col: 7, chars: 6, lines: 1 });
            editor.update(cx, |editor, cx| editor.select_range(3..3, cx));
            window.render_frame(cx);
            assert_eq!(editor.read(cx).caret(), Caret { line: 1, col: 4, chars: 0, lines: 0 });
        })
        .unwrap();
    }

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

    /// R12：连接 Redis 时查询编辑器装上 Redis 高亮（语言名 redis），换回 SQL 连接时回到 sql；
    /// 从没连过 Redis 的编辑器不动组件库默认的高亮工厂。
    #[gpui_kit::test]
    fn redis_targets_switch_the_editor_to_redis_highlighting(cx: &mut TestAppContext) {
        let (window, editor) = open(cx);
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            editor.update(cx, |editor, cx| {
                assert!(!editor.is_redis());
                assert_eq!(editor.language(cx).as_ref(), "sql");
                editor.set_sql("HGETALL user:1", window, cx);
                editor.set_redis(true, cx);
                assert!(editor.is_redis() && editor.redis_factory);
                assert_eq!(editor.language(cx).as_ref(), super::super::redis_highlight::REDIS_LANGUAGE);
            });
            window.render_frame(cx);
            editor.update(cx, |editor, cx| {
                editor.set_redis(false, cx);
                assert_eq!(editor.language(cx).as_ref(), "sql", "换回 SQL 连接");
            });
            window.render_frame(cx);
        })
        .unwrap();
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
            // 单一诊断弹层：执行错误与 lint 都在编辑器诊断集合里，LSP 悬停在诊断范围内不再给第二个弹层。
            assert_eq!(editor.read(cx).diagnostic_text(bad, cx).as_deref(), Some(hover.as_str()));
            assert_eq!(editor.read(cx).diagnostic_text(orders, cx).as_deref(), Some(lint.as_str()));
            assert!(editor.read(cx).lsp_hover_text(bad, cx).is_none(), "执行错误不再走悬停弹层");
            assert!(editor.read(cx).lsp_hover_text(orders, cx).is_none(), "lint 不再走悬停弹层");
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
        cx.update_window(window, |_, _window, cx| {
            let value = editor.read(cx).value(cx);
            assert!(!value.contains("/*x*/"), "插入走编辑器撤销栈：{value}");
            assert!(value.starts_with("select"), "{value}");
        })
        .unwrap();
    }

    /// 执行错误：错误码进诊断弹层标题，语句铺底色，行尾标记展开后可复制；光标移动不重建诊断。
    #[gpui_kit::test]
    fn exec_errors_carry_code_statement_and_copy_text(cx: &mut TestAppContext) {
        let (window, editor) = open(cx);
        cx.update_window(window, |_, window, cx| {
            let sql = "select 1;\nselect bad\n  from orders;";
            editor.update(cx, |editor, cx| {
                editor.set_sql(sql, window, cx);
                editor.focus(window, cx);
            });
            window.render_frame(cx);
            let bad = sql.find("bad").unwrap();
            let statement = sql.find("select bad").unwrap()..sql.len();
            let error = ExecError { range: bad..bad + 3, statement: Some(statement), code: Some("1054".to_owned()), message: "Unknown column 'bad' in 'field list'".to_owned() };
            assert_eq!(error.copy_text(), "[1054] Unknown column 'bad' in 'field list'");
            assert_eq!(error.short(), "Unknown column 'bad' in 'field list'");
            editor.update(cx, |editor, cx| editor.mark_exec_errors(vec![error.clone()], cx));
            window.render_frame(cx);
            assert_eq!(editor.read(cx).error_ranges(cx), vec![bad..bad + 3]);
            assert_eq!(editor.read(cx).exec_errors(), &[error]);
            let shown = editor.read(cx).diagnostic_text(bad, cx).expect("诊断弹层内容");
            assert!(shown.contains("1054") && shown.contains("Unknown column"), "{shown}");
            // 光标在错误行上来回移动、重绘：诊断与标记保持不变（不会被清掉再画回来）。
            for at in [bad, bad + 1, sql.len(), 0] {
                editor.update(cx, |editor, cx| editor.select_range(at..at, cx));
                window.render_frame(cx);
                assert_eq!(editor.read(cx).error_ranges(cx), vec![bad..bad + 3]);
                assert!(editor.read(cx).diagnostic_text(bad, cx).is_some());
            }
            let long = ExecError { message: "x".repeat(80), ..ExecError::default() };
            assert_eq!(long.short().chars().count(), SHORT_CHARS + 1, "行尾标记截断并带省略号");
            editor.update(cx, |editor, cx| editor.clear_errors(cx));
            assert!(editor.read(cx).diagnostic_text(bad, cx).is_none(), "清除后诊断一起清掉");
        })
        .unwrap();
    }

    /// 错误弹层的位置：语句下方放得下放下方，否则上方，都放不下越过编辑器下缘；任何情况都不与语句重叠。
    #[test]
    fn the_error_popover_is_placed_clear_of_the_failing_statement() {
        let input = (px(0.), px(400.));
        // 下方放得下。
        assert_eq!(popover_top((px(40.), px(100.)), px(120.), input), px(104.));
        // 下方放不下、上方放得下。
        assert_eq!(popover_top((px(200.), px(380.)), px(120.), input), px(76.));
        // 上下都放不下：放在语句下方，越过编辑器下缘。
        assert_eq!(popover_top((px(60.), px(330.)), px(120.), input), px(334.));
        // 语句下缘已经滚出可见区：贴着编辑器下缘之外。
        assert_eq!(popover_top((px(20.), px(900.)), px(120.), input), px(400.));
        for (statement, height) in [((px(40.), px(100.)), px(120.)), ((px(200.), px(380.)), px(120.)), ((px(60.), px(330.)), px(120.)), ((px(20.), px(900.)), px(120.))] {
            let top = popover_top(statement, height, input);
            let visible = (statement.0.max(input.0), statement.1.min(input.1));
            assert!(top + height <= visible.0 || top >= visible.1, "弹层压住了可见的语句行：{statement:?} -> {top:?}");
        }
    }

    /// 外壳有浮层（快捷键面板、模态……）时，行尾错误标记不画：它是 deferred 绘制，
    /// 原生截图里曾压在快捷键面板上面。浮层关掉后照常显示。
    #[gpui_kit::test]
    fn error_lenses_stay_under_shell_overlays(cx: &mut TestAppContext) {
        let (window, editor) = open(cx);
        let sql = "select * from missing_table;";
        let error = ExecError { range: 14..27, statement: Some(0..28), code: Some("42P01".to_owned()), message: "relation \"missing_table\" does not exist".to_owned() };
        cx.update_window(window, |_, window, cx| {
            editor.update(cx, |editor, cx| editor.set_sql(sql, window, cx));
            window.render_frame(cx);
            editor.update(cx, |editor, cx| editor.mark_exec_errors(vec![error.clone()], cx));
            window.render_frame(cx);
            assert!(window.try_find(("sql-error-lens-chip", 0usize)).is_some(), "没有浮层时画出行尾标记");
            ShellOverlayOpen::set(window, true, cx);
            editor.update(cx, |_, cx| cx.notify());
            window.render_frame(cx);
            assert!(window.try_find(("sql-error-lens-chip", 0usize)).is_none(), "浮层打开时标记不能压在上面");
            ShellOverlayOpen::set(window, false, cx);
            editor.update(cx, |_, cx| cx.notify());
            window.render_frame(cx);
            assert!(window.try_find(("sql-error-lens-chip", 0usize)).is_some());
        })
        .unwrap();
    }

    /// 浮层状态按窗口记录：一个窗口打开快捷键面板时，另一个窗口的行尾错误标记照常显示。
    #[gpui_kit::test]
    fn a_shell_overlay_hides_error_lenses_only_in_its_own_window(cx: &mut TestAppContext) {
        let (first, first_editor) = open(cx);
        let (second, second_editor) = open(cx);
        let sql = "select * from missing_table;";
        let error = ExecError { range: 14..27, statement: Some(0..28), code: Some("42P01".to_owned()), message: "relation \"missing_table\" does not exist".to_owned() };
        for (window, editor) in [(first, &first_editor), (second, &second_editor)] {
            cx.update_window(window, |_, window, cx| {
                editor.update(cx, |editor, cx| editor.set_sql(sql, window, cx));
                window.render_frame(cx);
                editor.update(cx, |editor, cx| editor.mark_exec_errors(vec![error.clone()], cx));
                window.render_frame(cx);
                assert!(window.try_find(("sql-error-lens-chip", 0usize)).is_some());
            })
            .unwrap();
        }
        cx.update_window(second, |_, window, cx| {
            ShellOverlayOpen::set(window, true, cx);
            second_editor.update(cx, |_, cx| cx.notify());
            window.render_frame(cx);
            assert!(window.try_find(("sql-error-lens-chip", 0usize)).is_none(), "有浮层的窗口不画标记");
        })
        .unwrap();
        cx.update_window(first, |_, window, cx| {
            first_editor.update(cx, |_, cx| cx.notify());
            window.render_frame(cx);
            assert!(window.try_find(("sql-error-lens-chip", 0usize)).is_some(), "另一个窗口的浮层不能藏掉这里的标记");
        })
        .unwrap();
    }

    /// R9：悬停或点击行尾标记展开错误弹层；弹层不压住多行出错语句的任何一行；
    /// 鼠标从标记移到弹层（中间隔着语句）时弹层不消失，离开两者后才收起；点击标记后固定展开。
    #[gpui_kit::test]
    fn the_error_popover_stays_clear_of_the_statement_and_survives_moving_onto_it(cx: &mut TestAppContext) {
        let (window, editor) = open(cx);
        let sql = "select 1;\ninsert into orders(id, name)\nvalues (1, 'a'),\n       (1, 'b');\nselect 2;";
        let start = sql.find("insert").unwrap();
        let end = sql.find("'b');").unwrap() + "'b');".len();
        let error = ExecError { range: start..end, statement: Some(start..end), code: Some("23505".to_owned()), message: "duplicate key value violates unique constraint \"orders_pkey\"".to_owned() };
        cx.update_window(window, |_, window, cx| {
            editor.update(cx, |editor, cx| {
                editor.set_sql(sql, window, cx);
                editor.focus(window, cx);
            });
            window.render_frame(cx);
            editor.update(cx, |editor, cx| editor.mark_exec_errors(vec![error.clone()], cx));
            window.render_frame(cx);
            // 鼠标先停在波浪线上：编辑器自己的诊断弹层出现。
            let squiggle = gpui_kit::base::input::DiagnosticEntry {
                range: start..end,
                diagnostic: Diagnostic::new(offset_to_position(&editor.read(cx).state.read(cx).text().clone(), start)..offset_to_position(&editor.read(cx).state.read(cx).text().clone(), end), "duplicate key"),
            };
            editor.update(cx, |editor, cx| editor.state.update(cx, |state, cx| state.present_diagnostic(squiggle, cx)));
            assert!(editor.read(cx).state.read(cx).diagnostic_popover().is_some());
            window.hover(("sql-error-lens-chip", 0usize), cx);
            window.render_frame(cx);
            assert_eq!(editor.read(cx).error_popover_open(), Some(0), "悬停标记展开弹层");
            // 标记遮住了编辑器的鼠标移动：诊断弹层收不到“离开”，由错误弹层收起它，屏上只留一个。
            assert!(editor.read(cx).state.read(cx).diagnostic_popover().is_none(), "两个弹层不能同时挂着");
            window.render_frame(cx);
            let card = window.find(("sql-error-popover", 0usize)).bounds();
            let state = editor.read(cx).state.read(cx);
            let (first, _) = row_top(state, start).expect("语句首行几何");
            let (last, height) = row_top(state, end).expect("语句末行几何");
            let card_top = card.origin.y;
            let card_bottom = card.origin.y + card.size.height;
            assert!(card_top >= last + height || card_bottom <= first, "弹层压住了出错语句：card {card:?}, statement {first:?}..{:?}", last + height);
        })
        .unwrap();
        // 从标记移到弹层：标记先离开，延迟到了之后弹层仍在（鼠标在弹层上）。
        cx.update_window(window, |_, window, cx| {
            window.hover(("sql-error-popover", 0usize), cx);
            window.render_frame(cx);
        })
        .unwrap();
        cx.executor().advance_clock(LENS_CLOSE_DELAY * 2);
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(editor.read(cx).error_popover_open(), Some(0), "鼠标在弹层上时不收起");
            window.click(("sql-error-popover-copy", 0usize), cx);
            assert_eq!(cx.read_from_clipboard().and_then(|item| item.text()), Some(error.copy_text()), "复制按钮复制错误码与原文");
            // 两者都离开：延迟后收起。
            window.hover("sql-editor-caret", cx);
            window.render_frame(cx);
        })
        .unwrap();
        cx.executor().advance_clock(LENS_CLOSE_DELAY * 2);
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(editor.read(cx).error_popover_open(), None, "离开标记和弹层后收起");
            assert!(window.try_find(("sql-error-popover", 0usize)).is_none());
            // 点击标记固定展开，鼠标离开也不收起；再点一次收起。
            window.click(("sql-error-lens-chip", 0usize), cx);
            window.hover("sql-editor-caret", cx);
            window.render_frame(cx);
        })
        .unwrap();
        cx.executor().advance_clock(LENS_CLOSE_DELAY * 2);
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(editor.read(cx).error_popover_open(), Some(0), "点击后固定展开");
            assert!(window.find(("sql-error-popover", 0usize)).visible());
            window.click(("sql-error-lens-chip", 0usize), cx);
            window.hover("sql-editor-caret", cx);
            window.render_frame(cx);
        })
        .unwrap();
        cx.executor().advance_clock(LENS_CLOSE_DELAY * 2);
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(editor.read(cx).error_popover_open(), None, "再点一次收起");
        })
        .unwrap();
    }

    /// 无选区的 ⌘/ 作用于光标所在语句的全部逻辑行；⌘⇧/ 切换块注释；各自一次撤销还原。
    #[gpui_kit::test]
    fn caret_statement_comments_and_block_comments_undo_in_one_step(cx: &mut TestAppContext) {
        let (window, editor) = open(cx);
        let sql = "SELECT 1;\nINSERT INTO t(a,b)\nSELECT a,b FROM s;\nSELECT 3;";
        let caret = sql.find("FROM s").unwrap();
        cx.update_window(window, |_, window, cx| {
            editor.update(cx, |editor, cx| {
                editor.set_sql(sql, window, cx);
                editor.focus(window, cx);
                editor.select_range(caret..caret, cx);
            });
            window.render_frame(cx);
            assert!(editor.update(cx, |editor, cx| editor.toggle_comment(window, cx)));
            assert_eq!(editor.read(cx).value(cx), "SELECT 1;\n-- INSERT INTO t(a,b)\n-- SELECT a,b FROM s;\nSELECT 3;");
            window.press("secondary-z", cx);
        })
        .unwrap();
        cx.update_window(window, |_, window, cx| {
            assert_eq!(editor.read(cx).value(cx), sql, "一次撤销还原整条语句");
            editor.update(cx, |editor, cx| editor.select_range(caret..caret, cx));
            assert!(editor.update(cx, |editor, cx| editor.toggle_block_comment(window, cx)));
            assert_eq!(editor.read(cx).value(cx), "SELECT 1;\n/* INSERT INTO t(a,b)\nSELECT a,b FROM s; */\nSELECT 3;");
            window.press("secondary-z", cx);
        })
        .unwrap();
        cx.update_window(window, |_, _window, cx| {
            assert_eq!(editor.read(cx).value(cx), sql, "块注释也是一次撤销");
        })
        .unwrap();
    }

    /// Ctrl/⌘+/：整段连续行一次替换，一次撤销就回到切换前；IME 组合输入期间不动作。
    #[gpui_kit::test]
    fn toggling_line_comments_replaces_the_region_once_and_skips_ime(cx: &mut TestAppContext) {
        let (window, editor) = open(cx);
        let sql = "SELECT 1;\n\n  SELECT 2;";
        cx.update_window(window, |_, window, cx| {
            editor.update(cx, |editor, cx| {
                editor.set_sql(sql, window, cx);
                editor.focus(window, cx);
                editor.select_range(0..sql.len(), cx);
            });
            window.render_frame(cx);
            assert!(editor.update(cx, |editor, cx| editor.toggle_comment(window, cx)));
            let commented = editor.read(cx).value(cx);
            assert_eq!(commented, "-- SELECT 1;\n\n  -- SELECT 2;", "空行不变，缩进保留");
            assert_eq!(editor.read(cx).selected_text(cx), commented, "整段仍在选区里");
            // 一次撤销还原整个区域，不是逐行撤销。
            window.press("secondary-z", cx);
        })
        .unwrap();
        cx.update_window(window, |_, window, cx| {
            assert_eq!(editor.read(cx).value(cx), sql, "一次撤销回到切换前");
            // 重做键随平台不同：macOS 是 Cmd+Shift+Z，Windows/Linux 的编辑器是 Ctrl+Y。
            #[cfg(target_os = "macos")]
            window.press("secondary-shift-z", cx);
            #[cfg(not(target_os = "macos"))]
            window.press("secondary-y", cx);
        })
        .unwrap();
        cx.update_window(window, |_, window, cx| {
            let commented = editor.read(cx).value(cx);
            assert_eq!(commented, "-- SELECT 1;\n\n  -- SELECT 2;", "一次重做恢复整段注释");
            // 再次切换是去掉这一层：先选整段，范围与第一次相同。
            editor.update(cx, |editor, cx| {
                editor.select_range(0..commented.len(), cx);
                assert!(editor.toggle_comment(window, cx));
            });
            assert_eq!(editor.read(cx).value(cx), sql);
            // 输入法组合输入期间不响应命令。
            editor.update(cx, |editor, cx| {
                editor.state.update(cx, |state, cx| {
                    state.replace_and_mark_text_in_range(None, "zhong", Some(0..0), window, cx);
                });
            });
            assert!(!editor.update(cx, |editor, cx| editor.toggle_comment(window, cx)), "组合输入期间不动作");
        })
        .unwrap();
    }

    #[test]
    fn from_single_char_filters_noise_highlights_exact_match_and_formats_object_columns() {
        use crate::query::assist::{CatalogObject, ColumnMeta, CompletionSource, MetadataState};
        let scope = CompletionScope {
            engine: "sqlite".to_owned(),
            objects: vec![
                CatalogObject { name: "_sqlx_migrations".into(), schema: String::new(), kind: "table".into(), comment: String::new() },
                CatalogObject { name: "logs".into(), schema: String::new(), kind: "table".into(), comment: String::new() },
                CatalogObject { name: "bare_empty".into(), schema: String::new(), kind: "table".into(), comment: String::new() },
            ],
            sources: vec![CompletionSource {
                alias: String::new(),
                label: "logs".into(),
                columns: vec![
                    ColumnMeta { name: "id".into(), type_name: "INTEGER".into(), nullable: Some(false), comment: String::new(), primary: true, foreign_key: false },
                    ColumnMeta { name: "level".into(), type_name: "TEXT".into(), nullable: Some(false), comment: "日志级别".into(), primary: false, foreign_key: false },
                ],
            }],
            provider: true,
            databases: vec!["main".into()],
            schemas: Vec::new(),
            frequency: Vec::new(),
            state: MetadataState::Ready,
            foreign_keys: Vec::new(),
        };
        let provider = suggest(Some(scope), Vec::new());

        // `SELECT * FROM l`：logs 排第一；不混入 SELECT / NULL / VALUES / DELETE / ALTER / TABLE 或内部迁移表。
        let rope = Rope::from("SELECT * FROM l");
        let items = provider.items(&rope, rope.len());
        assert_eq!(items.first().map(|i| i.label.as_str()), Some("logs"));
        for noise in ["SELECT", "NULL", "VALUES", "DELETE", "ALTER", "TABLE", "_sqlx_migrations"] {
            assert!(!items.iter().any(|i| i.label == noise), "单字符 l 不应带出无关候选 {noise}: {:?}", items.iter().map(|i| &i.label).collect::<Vec<_>>());
        }
        // 高亮只染命中的 `l`（0..1），绝不把整个单词染蓝；filter_text 保持为 None。
        let logs = &items[0];
        assert!(logs.filter_text.is_none());
        assert_eq!(highlight_sql_completion("l", logs).as_slice(), std::slice::from_ref(&(0..1)));
        let doc = match logs.documentation.as_ref().expect("已读到列的表应展示字段卡片") {
            Documentation::MarkupContent(mc) => &mc.value,
            Documentation::String(s) => s,
        };
        assert!(doc.contains("`id`") && doc.contains("INTEGER") && doc.contains("`level`") && doc.contains("日志级别"), "{doc}");

        // 没有任何列、模式与注释的对象不弹出重复右侧空卡片。
        let rope_bare = Rope::from("SELECT * FROM bare_emp");
        let bare_items = provider.items(&rope_bare, rope_bare.len());
        let bare = bare_items.iter().find(|i| i.label == "bare_empty").expect("bare_empty");
        assert!(bare.documentation.is_none(), "无附加元数据的表不重复弹冗余空气泡");

        // 下划线分段输入 `mig` 仍能精准匹配 `_sqlx_migrations` 并高亮 `mig`。
        let rope_mig = Rope::from("SELECT * FROM mig");
        let mig_items = provider.items(&rope_mig, rope_mig.len());
        let mig = mig_items.iter().find(|i| i.label == "_sqlx_migrations").expect("_sqlx_migrations");
        assert_eq!(highlight_sql_completion("mig", mig).as_slice(), std::slice::from_ref(&(6..9)));

        // 带 schema 前缀的对象在输入裸表名前缀时高亮表名段而非 schema 段。
        let pg_item = CompletionItem { label: "public.orders".into(), ..Default::default() };
        assert_eq!(highlight_sql_completion("ord", &pg_item).as_slice(), std::slice::from_ref(&(7..10)));
    }
}
