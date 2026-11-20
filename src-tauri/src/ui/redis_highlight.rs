//! Redis 命令的词法高亮：查询编辑器（连接 Redis 时）与 Redis 工作台命令栏共用一套分类。
//!
//! 按行处理：每行第一个词是命令，在命令目录（`adapters/redis/complete.rs`）里才按关键字着色，
//! 不认识的命令按普通文本显示；双词命令的第二个词、`EX`/`NX`/`WITHSCORES` 这类选项也按关键字；
//! 数字、带引号的字符串分别着色；带键命令的第一个参数按键着色，其余参数按普通参数。
//! 颜色取 `apply_sql_syntax` 已有的高亮名称，深浅主题跟着高亮主题自动切换。

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::base::input::{EditorState, FoldRange, HighlightStyleResolver, InputEdit, InputHighlighter, InputHighlighterFactory, Rope};
use gpui_kit::component::highlighter::SyntaxHighlighter;
use gpui_kit::{Context, HighlightStyle, SharedString, Task, Window};

use crate::adapters::redis::complete::COMMANDS;

/// 编辑器里 Redis 命令的语言名（不与组件库的语法名冲突，组件库没有这门语言）。
pub const REDIS_LANGUAGE: &str = "redis-cli";

/// 词法分类。`Plain` 不着色。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RedisTokenKind {
    Command,
    Keyword,
    Key,
    Argument,
    Number,
    String,
    Comment,
    Plain,
}

impl RedisTokenKind {
    /// 对应的高亮名称（与 SQL 配色共用）；Plain 为 None。
    pub fn highlight(self) -> Option<&'static str> {
        match self {
            RedisTokenKind::Command | RedisTokenKind::Keyword => Some("keyword"),
            RedisTokenKind::Key => Some("variable.special"),
            RedisTokenKind::Argument => Some("variable"),
            RedisTokenKind::Number => Some("number"),
            RedisTokenKind::String => Some("string"),
            RedisTokenKind::Comment => Some("comment"),
            RedisTokenKind::Plain => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RedisToken {
    pub range: Range<usize>,
    pub kind: RedisTokenKind,
}

/// 常见的选项与子句关键字（大小写不敏感）。
const OPTIONS: &[&str] = &[
    "EX", "PX", "EXAT", "PXAT", "NX", "XX", "KEEPTTL", "GET", "GT", "LT", "CH", "INCR", "WITHSCORES", "WITHSCORE", "LIMIT", "MATCH", "COUNT",
    "TYPE", "BY", "ASC", "DESC", "ALPHA", "STORE", "REV", "BYSCORE", "BYLEX", "MAXLEN", "MINID", "NOMKSTREAM", "BLOCK", "STREAMS", "GROUP",
    "MKSTREAM", "JUSTID", "FORCE", "IDLE", "RETRYCOUNT", "LEFT", "RIGHT", "BEFORE", "AFTER", "RANK", "WEIGHTS", "AGGREGATE", "SUM", "MIN",
    "MAX", "WITHVALUES", "WITHCOORD", "WITHDIST", "WITHHASH", "FROMMEMBER", "FROMLONLAT", "BYRADIUS", "BYBOX", "M", "KM", "FT", "MI",
    "SAMPLES", "RESET", "FULL", "LEN", "IDX", "MINMATCHLEN", "SCHEDULE", "ASYNC", "SYNC", "REPLACE", "COPY", "AUTH", "AUTH2", "KEYS", "DB",
];

fn spec_known(word: &str) -> bool {
    COMMANDS.iter().any(|spec| {
        spec.name.eq_ignore_ascii_case(word)
            || spec.name.len() > word.len() && spec.name.as_bytes()[word.len()] == b' ' && spec.name[..word.len()].eq_ignore_ascii_case(word)
    })
}

fn compound_known(first: &str, second: &str) -> bool {
    let two = format!("{first} {second}");
    COMMANDS.iter().any(|spec| spec.name.eq_ignore_ascii_case(&two))
}

fn takes_key(first: &str, second: Option<&str>) -> bool {
    if let Some(second) = second {
        let two = format!("{first} {second}");
        if let Some(spec) = COMMANDS.iter().find(|spec| spec.name.eq_ignore_ascii_case(&two)) {
            return spec.takes_key;
        }
    }
    COMMANDS.iter().find(|spec| spec.name.eq_ignore_ascii_case(first)).is_some_and(|spec| spec.takes_key)
}

fn is_number(word: &str) -> bool {
    let body = word.strip_prefix(['-', '+']).unwrap_or(word);
    !body.is_empty()
        && (body.parse::<f64>().is_ok() && body.chars().all(|ch| ch.is_ascii_digit() || ch == '.' || ch == 'e' || ch == 'E' || ch == '-' || ch == '+')
            || body.eq_ignore_ascii_case("inf"))
}

/// 一行里的原始词：（字节范围，是否带引号）。未闭合的引号到行尾。
fn split_words(line: &str, base: usize) -> Vec<(Range<usize>, bool)> {
    let bytes = line.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let ch = bytes[at];
        if ch.is_ascii_whitespace() {
            at += 1;
            continue;
        }
        let start = at;
        if ch == b'"' || ch == b'\'' {
            at += 1;
            while at < bytes.len() {
                if bytes[at] == b'\\' && ch == b'"' {
                    at += 2;
                    continue;
                }
                if bytes[at] == ch {
                    at += 1;
                    break;
                }
                at += 1;
            }
            let end = at.min(bytes.len());
            out.push((base + start..base + end, true));
            continue;
        }
        while at < bytes.len() && !bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        out.push((base + start..base + at, false));
    }
    out
}

/// 词法分析：逐行处理多行、多条命令。返回的记号按位置排序，互不重叠。
pub fn lex(text: &str) -> Vec<RedisToken> {
    let mut tokens = Vec::new();
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let body = line.trim_end_matches(['\n', '\r']);
        let trimmed = body.trim_start();
        if trimmed.starts_with('#') || trimmed.starts_with("//") {
            let start = offset + (body.len() - trimmed.len());
            tokens.push(RedisToken { range: start..offset + body.len(), kind: RedisTokenKind::Comment });
            offset += line.len();
            continue;
        }
        let words = split_words(body, offset);
        let word = |range: &Range<usize>| &text[range.clone()];
        if let Some((first_range, first_quoted)) = words.first() {
            let first = word(first_range);
            let known = !first_quoted && spec_known(first);
            tokens.push(RedisToken { range: first_range.clone(), kind: if known { RedisTokenKind::Command } else { RedisTokenKind::Plain } });
            let second = words.get(1).filter(|(_, quoted)| !quoted).map(|(range, _)| word(range));
            let compound = known && second.is_some_and(|second| compound_known(first, second));
            let keyed = known && takes_key(first, second.filter(|_| compound));
            let mut key_seen = false;
            for (index, (range, quoted)) in words.iter().enumerate().skip(1) {
                let text_word = word(range);
                let kind = if !known {
                    RedisTokenKind::Plain
                } else if index == 1 && compound {
                    RedisTokenKind::Keyword
                } else if *quoted {
                    if keyed && !key_seen {
                        key_seen = true;
                        RedisTokenKind::Key
                    } else {
                        RedisTokenKind::String
                    }
                } else if keyed && !key_seen {
                    key_seen = true;
                    RedisTokenKind::Key
                } else if is_number(text_word) {
                    RedisTokenKind::Number
                } else if OPTIONS.iter().any(|option| option.eq_ignore_ascii_case(text_word)) {
                    RedisTokenKind::Keyword
                } else {
                    RedisTokenKind::Argument
                };
                tokens.push(RedisToken { range: range.clone(), kind });
            }
        }
        offset += line.len();
    }
    tokens
}

/// 把记号转成覆盖 `range` 的有序样式段；没有记号的地方用默认样式补齐。
pub fn style_runs(tokens: &[RedisToken], range: &Range<usize>, resolver: &dyn HighlightStyleResolver) -> Vec<(Range<usize>, HighlightStyle)> {
    let mut runs = Vec::new();
    let mut at = range.start;
    for token in tokens {
        if token.range.end <= range.start || token.range.start >= range.end {
            continue;
        }
        let Some(name) = token.kind.highlight() else { continue };
        let start = token.range.start.max(range.start);
        let end = token.range.end.min(range.end);
        if start > at {
            runs.push((at..start, HighlightStyle::default()));
        }
        runs.push((start..end, resolver.style(name).unwrap_or_default()));
        at = end;
    }
    if at < range.end {
        runs.push((at..range.end, HighlightStyle::default()));
    }
    runs
}

/// 查询编辑器与命令栏共用的高亮器：每次编辑整段重新分类（命令行很短，按行词法足够快）。
#[derive(Default)]
pub struct RedisHighlighter {
    tokens: Vec<RedisToken>,
}

impl RedisHighlighter {
    pub fn refresh(&mut self, text: &str) {
        self.tokens = lex(text);
    }
}

impl InputHighlighter for RedisHighlighter {
    fn language(&self) -> SharedString {
        SharedString::new_static(REDIS_LANGUAGE)
    }

    fn update(&mut self, _edit: Option<InputEdit>, text: &Rope, _folding: bool, _window: &mut Window, _cx: &mut Context<EditorState>) {
        self.refresh(&text.to_string());
    }

    fn styles(&self, range: &Range<usize>, resolver: &dyn HighlightStyleResolver) -> Vec<(Range<usize>, HighlightStyle)> {
        style_runs(&self.tokens, range, resolver)
    }

    fn fold_ranges(&self, _text: &Rope) -> Vec<FoldRange> {
        Vec::new()
    }
}

/// 换回 SQL 等其他语言时用组件库的语法高亮。
///
/// 组件库默认的输入高亮适配器不对外开放，装上 Redis 工厂后这里接手：小文本在编辑当下限时解析（与组件库同样 2ms），
/// 解析不完或文本较大时，停止输入 150ms 后在后台整段解析再换上新树，编辑过程不在主线程上整段重解析。
/// 折叠候选从当前语法树取，换回 SQL 后折叠照常可用。
struct SyntaxFallback {
    inner: Rc<RefCell<SyntaxHighlighter>>,
    parse_task: Option<Task<()>>,
}

/// 编辑当下同步解析的文本上限，与组件库的默认适配器一致。
const SYNC_PARSE_MAX_BYTES: usize = 256 * 1024;
const SYNC_PARSE_TIMEOUT: Duration = Duration::from_millis(2);
const PARSE_DEBOUNCE: Duration = Duration::from_millis(150);

impl SyntaxFallback {
    fn new(language: &str) -> Self {
        Self { inner: Rc::new(RefCell::new(SyntaxHighlighter::new(language))), parse_task: None }
    }

    /// 停止输入一段时间后，在后台用新的高亮器整段解析，完成后换上并刷新折叠候选。
    fn schedule_parse(&mut self, text: &Rope, folding: bool, window: &mut Window, cx: &mut Context<EditorState>) {
        let inner = Rc::clone(&self.inner);
        let language = inner.borrow().language().to_string();
        let text = text.clone();
        self.parse_task = Some(cx.spawn_in(window, async move |entity, cx| {
            cx.background_executor().timer(PARSE_DEBOUNCE).await;
            let parsed = cx
                .background_executor()
                .spawn(async move {
                    let mut highlighter = SyntaxHighlighter::new(&language);
                    highlighter.update(None, &text, None);
                    highlighter
                })
                .await;
            *inner.borrow_mut() = parsed;
            let folds = if folding { fold_candidates(&inner.borrow()) } else { Vec::new() };
            let _ = entity.update(cx, |state, cx| state.apply_highlighter_fold_candidates(folds, cx));
        }));
    }
}

/// 语法树里跨三行及以上的具名节点作为折叠候选（与组件库默认适配器的规则一致）。
fn fold_candidates(highlighter: &SyntaxHighlighter) -> Vec<FoldRange> {
    let Some(tree) = highlighter.tree() else { return Vec::new() };
    let root = tree.root_node();
    let mut ranges = Vec::new();
    let mut cursor = root.walk();
    let mut stack: Vec<_> = root.named_children(&mut cursor).collect();
    stack.reverse();
    while let Some(node) = stack.pop() {
        let start = node.start_position().row;
        let end = node.end_position().row;
        if end.saturating_sub(start) < 2 {
            continue;
        }
        ranges.push(FoldRange::new(start, end));
        let mut cursor = node.walk();
        let children: Vec<_> = node.named_children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
    }
    ranges.sort_by_key(|range| range.start_line);
    ranges.dedup_by_key(|range| range.start_line);
    ranges
}

impl InputHighlighter for SyntaxFallback {
    fn language(&self) -> SharedString {
        self.inner.borrow().language().clone()
    }

    fn update(&mut self, _edit: Option<InputEdit>, text: &Rope, folding: bool, window: &mut Window, cx: &mut Context<EditorState>) {
        // 小文本当场限时解析；解析完就不再排后台任务。超时或大文本留着旧树显示，交给后台整段解析。
        if text.len() <= SYNC_PARSE_MAX_BYTES && self.inner.borrow_mut().update(None, text, Some(SYNC_PARSE_TIMEOUT)) {
            self.parse_task = None;
            return;
        }
        self.schedule_parse(text, folding, window, cx);
    }

    fn styles(&self, range: &Range<usize>, resolver: &dyn HighlightStyleResolver) -> Vec<(Range<usize>, HighlightStyle)> {
        self.inner.borrow().styles(range, resolver)
    }

    fn fold_ranges(&self, _text: &Rope) -> Vec<FoldRange> {
        fold_candidates(&self.inner.borrow())
    }
}

/// 编辑器的高亮工厂：`redis` 用 [`RedisHighlighter`]，其余语言交给组件库的语法高亮。
pub fn highlighter_factory() -> InputHighlighterFactory {
    Rc::new(|language: &str| -> Option<Box<dyn InputHighlighter>> {
        if language == REDIS_LANGUAGE {
            Some(Box::new(RedisHighlighter::default()))
        } else {
            Some(Box::new(SyntaxFallback::new(language)))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str) -> Vec<(&str, RedisTokenKind)> {
        lex(text).into_iter().map(|token| (&text[token.range], token.kind)).collect()
    }

    #[test]
    fn commands_keys_options_numbers_and_strings_are_classified() {
        use RedisTokenKind::*;
        assert_eq!(kinds("HGETALL user:1"), vec![("HGETALL", Command), ("user:1", Key)]);
        assert_eq!(
            kinds("set greeting \"hi there\" EX 60 nx"),
            vec![("set", Command), ("greeting", Key), ("\"hi there\"", String), ("EX", Keyword), ("60", Number), ("nx", Keyword)]
        );
        assert_eq!(kinds("ZADD board 1.5 alice"), vec![("ZADD", Command), ("board", Key), ("1.5", Number), ("alice", Argument)]);
        assert_eq!(kinds("CONFIG GET maxmemory"), vec![("CONFIG", Command), ("GET", Keyword), ("maxmemory", Argument)]);
        // 不认识的命令：整行按普通文本，不冒充关键字。
        assert_eq!(kinds("FROBNICATE a 1"), vec![("FROBNICATE", Plain), ("a", Plain), ("1", Plain)]);
        assert_eq!(kinds("'quoted' x"), vec![("'quoted'", Plain), ("x", Plain)]);
    }

    #[test]
    fn lines_are_lexed_independently_and_offsets_stay_in_the_full_text() {
        let text = "GET a\n# note\n  DEL \"b c\"\nunknown x";
        let tokens = lex(text);
        let shown: Vec<(&str, RedisTokenKind)> = tokens.iter().map(|token| (&text[token.range.clone()], token.kind)).collect();
        assert_eq!(
            shown,
            vec![
                ("GET", RedisTokenKind::Command),
                ("a", RedisTokenKind::Key),
                ("# note", RedisTokenKind::Comment),
                ("DEL", RedisTokenKind::Command),
                ("\"b c\"", RedisTokenKind::Key),
                ("unknown", RedisTokenKind::Plain),
                ("x", RedisTokenKind::Plain),
            ]
        );
        assert!(tokens.windows(2).all(|pair| pair[0].range.end <= pair[1].range.start), "记号有序且不重叠");
        // 未闭合的引号到行尾为止，不吞下一行。
        let open = lex("SET k \"abc\nGET z");
        assert_eq!(open.last().map(|token| token.kind), Some(RedisTokenKind::Key));
    }

    struct Names;
    impl HighlightStyleResolver for Names {
        fn style(&self, name: &str) -> Option<HighlightStyle> {
            let hue = match name {
                "keyword" => 0.1,
                "variable.special" => 0.2,
                "number" => 0.3,
                "string" => 0.4,
                _ => 0.5,
            };
            Some(HighlightStyle { color: Some(gpui_kit::hsla(hue, 0.5, 0.5, 1.)), ..Default::default() })
        }
    }

    #[test]
    fn style_runs_cover_the_range_and_keep_keyword_and_key_apart() {
        let text = "HGETALL user:1";
        let tokens = lex(text);
        let runs = style_runs(&tokens, &(0..text.len()), &Names);
        assert_eq!(runs.first().map(|run| run.0.clone()), Some(0..7));
        assert_eq!(runs.last().map(|run| run.0.end), Some(text.len()));
        assert!(runs.windows(2).all(|pair| pair[0].0.end == pair[1].0.start), "相邻段首尾相接，完整覆盖");
        let color_of = |at: usize| runs.iter().find(|run| run.0.contains(&at)).and_then(|run| run.1.color);
        assert_ne!(color_of(0), color_of(9), "命令与键颜色不同");
        assert_eq!(color_of(7), None, "空白不着色");
        // 子范围：只返回落在范围里的部分。
        let part = style_runs(&tokens, &(3..10), &Names);
        assert_eq!(part.first().map(|run| run.0.start), Some(3));
        assert_eq!(part.last().map(|run| run.0.end), Some(10));
        // 未知命令全部是默认样式。
        let plain = lex("NOPE a");
        assert!(style_runs(&plain, &(0..6), &Names).iter().all(|run| run.1 == HighlightStyle::default()));
    }

    /// 补全走同一份命令目录：输入 HGE 给出 HGET 和 HGETALL（查询编辑器的 redis_plan 与命令栏共用）。
    /// 用到的高亮名称在深浅两套 SQL 配色里都有颜色，且关键字与键在两套主题里都不同色。
    #[test]
    fn highlight_names_have_colors_in_both_themes() {
        use RedisTokenKind::*;
        for dark in [false, true] {
            for kind in [Command, Keyword, Key, Argument, Number, String, Comment] {
                let name = kind.highlight().unwrap();
                assert!(crate::ui::kit::sql_token_color(name, dark).is_some(), "{name} 在 dark={dark} 时没有颜色");
            }
            assert_ne!(
                crate::ui::kit::sql_token_color(Command.highlight().unwrap(), dark),
                crate::ui::kit::sql_token_color(Key.highlight().unwrap(), dark),
                "dark={dark}：命令与键颜色不同"
            );
        }
    }

    #[test]
    fn hge_completes_to_hget_and_hgetall() {
        let (_, _, items) = crate::adapters::redis::complete::complete_command_line("HGE", 3, &[], crate::i18n::Locale::ZhCn);
        let labels: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();
        assert!(labels.contains(&"HGET") && labels.contains(&"HGETALL"), "{labels:?}");
    }

    #[test]
    fn factory_serves_redis_and_falls_back_for_other_languages() {
        let factory = highlighter_factory();
        assert_eq!(factory(REDIS_LANGUAGE).map(|h| h.language()), Some(SharedString::new_static(REDIS_LANGUAGE)));
        assert!(factory("sql").is_some(), "换回 SQL 时仍有语法高亮");
    }

    /// 换回 SQL 后折叠候选照常给出（多行语句可以折叠），不是空表。
    #[test]
    fn sql_fallback_keeps_fold_candidates() {
        let mut highlighter = SyntaxHighlighter::new("sql");
        let text = Rope::from("select\n  a,\n  b,\n  c\nfrom t\nwhere a = 1;\n");
        assert!(highlighter.update(None, &text, None));
        let folds = fold_candidates(&highlighter);
        assert!(folds.iter().any(|range| range.start_line == 0 && range.end_line >= 2), "{folds:?}");
    }
}
