//! Redis 键浏览器的界面层。
//!
//! 布局：左侧键空间（连接与逻辑库、模式过滤、类型筛选、键列表、SCAN 页脚），右侧键详情
//! （键名与元信息、TTL / 重命名编辑条、按类型渲染的条目表与结构化查看器），底部命令控制台。
//! 写删确认与新建键是盖在面板上的对话框：确认就近出现在视线中央，不再落到底部控制台里。

use super::*;
use gpui_kit::{hsla, Hsla};

/// 键列表行里键名的等宽字宽估计（12px 等宽字体约 0.6em）。只用来决定中间省略的字符数，
/// 估多估少都有 `truncate()` 兜底，不会撑出侧栏。
const MONO_CHAR_W: f32 = 7.2;
/// 类型徽标宽度：键列表与详情头部共用，让键名列左缘对齐。
const TYPE_BADGE_W: f32 = 38.;
/// 键列表里同时渲染的行数上限；更多的键用模式过滤或命名空间树收窄。
const KEY_ROWS_MAX: usize = 500;
/// 条目表一次渲染的行数上限。
const ENTRY_ROWS_MAX: usize = 2000;

#[derive(Clone, Copy)]
enum Kind {
    Primary,
    Secondary,
    Subtle,
    Tool,
    Danger,
    DangerSoft,
}

#[derive(Clone, Copy)]
enum Tone {
    Text,
    Muted,
    Accent,
}

struct EntryCol {
    label: String,
    width: Option<f32>,
    tone: Tone,
}

struct EntryAction {
    cmd: String,
    icon: IconName,
    label: String,
}

struct EntryRow {
    index: usize,
    id: String,
    cells: Vec<String>,
    actions: Vec<EntryAction>,
    menu: Vec<ContextItem>,
}

/// 选中条目交给结构化查看器的内容：下标、标题、原始值、能否写回。
type InspectedEntry = (usize, String, String, Option<EntryTarget>);

pub(super) fn type_colors(kind: &str, p: &Palette) -> (Hsla, Hsla) {
    match kind {
        "string" => (p.info, p.info.opacity(0.14)),
        "hash" => (p.ok, p.ok_soft),
        "list" => (p.warn, p.warn_soft),
        "set" => (p.accent_text, p.accent_soft),
        "zset" => (p.danger, p.danger_soft),
        "stream" => (p.text, p.active),
        _ => (p.text_muted, p.hover),
    }
}

fn type_code(kind: &str) -> &'static str {
    match kind {
        "string" => "STR",
        "hash" => "HASH",
        "list" => "LIST",
        "set" => "SET",
        "zset" => "ZSET",
        "stream" => "XS",
        "none" => "—",
        _ => "?",
    }
}

/// 定宽类型徽标：同一列里所有键名左缘对齐，颜色区分六种类型。
fn type_badge(kind: &str, p: &Palette) -> AnyElement {
    let (fg, bg) = type_colors(kind, p);
    div()
        .flex_none()
        .w(px(TYPE_BADGE_W))
        .h(px(17.))
        .rounded(px(Radius::TAG))
        .bg(bg)
        .text_color(fg)
        .text_size(px(TypeScale::XS))
        .font_weight(FontWeight::SEMIBOLD)
        .flex()
        .items_center()
        .justify_center()
        .child(type_code(kind))
        .into_any_element()
}

fn cmd_click(entity: &Entity<RedisPane>, id: impl Into<String>) -> impl Fn(&ClickEvent, &mut Window, &mut gpui_kit::App) + 'static {
    let entity = entity.clone();
    let id: String = id.into();
    move |_, _, cx| {
        let id = id.clone();
        entity.update(cx, |pane, cx| pane.command(&id, cx));
    }
}

fn action(id: impl Into<String>, icon: Option<IconName>, label: &str, kind: Kind, entity: &Entity<RedisPane>) -> kit::ActionButton {
    let id: String = id.into();
    let click = cmd_click(entity, id.clone());
    let label = SharedString::from(label.to_owned());
    let id = SharedString::from(id);
    match kind {
        Kind::Primary => kit::primary_button(id, icon, label, click),
        Kind::Secondary => kit::secondary_button(id, icon, label, click),
        Kind::Subtle => kit::subtle_button(id, icon, label, click),
        Kind::Tool => kit::tool_button(id, icon, label, click),
        Kind::Danger => kit::danger_button(id, icon, label, click),
        Kind::DangerSoft => kit::danger_soft_button(id, icon, label, click),
    }
}

fn icon_btn(id: impl Into<String>, icon: IconName, label: &str, entity: &Entity<RedisPane>) -> kit::IconButton {
    let id: String = id.into();
    let click = cmd_click(entity, id.clone());
    kit::icon_button(SharedString::from(id), icon, SharedString::from(label.to_owned()), click).small()
}

fn quote_arg(arg: &str) -> String {
    if arg.is_empty() || arg.chars().any(|ch| ch.is_whitespace() || ch == '"' || ch == '\'') {
        format!("\"{}\"", arg.replace('"', "\\\""))
    } else {
        arg.to_owned()
    }
}

fn content_tag(selected: &str, index: usize, raw: &str) -> String {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    raw.hash(&mut hasher);
    format!("{selected}:{index}:{:x}", hasher.finish())
}

impl RedisPane {
    /// 确认框里展示的那条命令：用户最终确认的就是它。
    pub(super) fn confirm_preview(&self) -> String {
        match &self.confirm {
            Some(Confirm::Delete { key }) => format!("DEL {}", quote_arg(key)),
            Some(Confirm::Rename { from, to }) => format!("RENAME {} {}", quote_arg(from), quote_arg(to)),
            Some(Confirm::Exec { line, .. }) => line.trim().to_owned(),
            Some(Confirm::Ttl { key, seconds: Some(seconds) }) => format!("EXPIRE {} {seconds}", quote_arg(key)),
            Some(Confirm::Ttl { key, seconds: None }) => format!("PERSIST {}", quote_arg(key)),
            Some(Confirm::Mutation { mutation, .. }) => keys::mutation_preview(mutation),
            None => String::new(),
        }
    }

    /// 删除类确认用危险色按钮；普通写入用主色，避免每次写一个字段都像在删库。
    pub(super) fn confirm_destructive(&self) -> bool {
        match &self.confirm {
            Some(Confirm::Delete { .. }) => true,
            Some(Confirm::Exec { command, .. }) => {
                let word = command.split_whitespace().next().unwrap_or("").to_ascii_uppercase();
                matches!(
                    word.as_str(),
                    "DEL" | "UNLINK" | "FLUSHDB" | "FLUSHALL" | "SHUTDOWN" | "HDEL" | "LREM" | "SREM" | "ZREM" | "XDEL" | "LTRIM"
                        | "SPOP" | "LPOP" | "RPOP" | "GETDEL" | "ZREMRANGEBYSCORE" | "ZREMRANGEBYRANK" | "ZREMRANGEBYLEX" | "XTRIM"
                        | "SWAPDB" | "MIGRATE"
                )
            }
            Some(Confirm::Mutation { mutation, .. }) => matches!(
                mutation,
                RedisMutation::HashDel { .. }
                    | RedisMutation::ListRem { .. }
                    | RedisMutation::SetRem { .. }
                    | RedisMutation::ZSetRem { .. }
                    | RedisMutation::StreamDel { .. }
                    | RedisMutation::CreateKey { overwrite: true, .. }
            ),
            _ => false,
        }
    }

    fn count_label(&self, detail: &RedisKeyDetail) -> String {
        let (key, n) = match &detail.value {
            RedisValue::String { text } => ("redis.count.string", text.chars().count()),
            RedisValue::Hash { entries } => ("redis.count.hash", entries.len()),
            RedisValue::List { items } => ("redis.count.list", items.len()),
            RedisValue::Set { items } => ("redis.count.set", items.len()),
            RedisValue::ZSet { entries } => ("redis.count.zset", entries.len()),
            RedisValue::Stream { messages } => ("redis.count.stream", messages.len()),
            RedisValue::None {} => return String::new(),
        };
        i18n::fill(&self.text(key), &[("n", &n.to_string())])
    }
}

impl Render for RedisPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::of(cx);
        let m = kit::Metrics::of(cx);
        let entity = cx.entity();
        let viewport_w = f32::from(window.viewport_size().width);
        let viewport_h = f32::from(window.viewport_size().height);
        self.viewport_w = viewport_w;
        let narrow = viewport_w < kit::NARROW_PAGE_WIDTH;
        let show_keys = self.keys_visible(viewport_w);

        let sidebar = if !narrow || show_keys {
            Some(stack_frame(|| self.render_sidebar(&p, &m, &entity, narrow, viewport_w, window, cx)))
        } else {
            None
        };
        let detail = if !narrow || !show_keys {
            Some(stack_frame(|| self.render_detail(&p, &m, &entity, viewport_h, window, cx)))
        } else {
            None
        };
        let console = stack_frame(|| self.render_console(&p, &m, &entity, window, cx));
        let new_layer = if self.new_open { Some(stack_frame(|| self.render_new_key_dialog(&p, &m, &entity, window, cx))) } else { None };
        let mono = cx.theme().mono_font_family.clone();
        let confirm_layer = if self.confirm.is_some() { Some(stack_frame(|| self.render_confirm_dialog(&p, &m, &entity, mono))) } else { None };
        let menu_layer = self.render_context_menu(&p, &entity);

        if let Some(field) = self.focus_request.take() {
            if self.fields.contains(field) {
                self.fields.focus(field, window, cx);
            }
        }

        stack_frame(|| {
            let mut root = div()
                .id("redis-pane")
                .test_support()
                .relative()
                .size_full()
                .flex()
                .bg(p.surface)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        let menu = this.context_menu.take().is_some();
                        let db = std::mem::take(&mut this.db_picker_open);
                        if menu || db {
                            cx.notify();
                        }
                    }),
                )
                .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                    if let Some((start_x, start_w)) = this.sidebar_drag_start {
                        let delta = f32::from(event.position.x) - start_x;
                        let max_w = (this.viewport_w * 0.5).clamp(SIDEBAR_MIN_W, SIDEBAR_MAX_W);
                        this.sidebar_w = (start_w + delta).clamp(SIDEBAR_MIN_W, max_w);
                        cx.notify();
                    }
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseUpEvent, _, cx| {
                        if this.sidebar_drag_start.take().is_some() {
                            cx.notify();
                        }
                    }),
                )
                .on_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, _, cx| this.key_down(event, cx)));

            if narrow {
                let (toggle_icon, toggle_label) = if show_keys {
                    (IconName::ChevronRight, self.text("redis.nav.value"))
                } else {
                    (IconName::ChevronLeft, self.text("redis.nav.keys"))
                };
                let strip = div()
                    .id("redis-nav-strip")
                    .test_support()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(m.pad))
                    .px(px(m.pane_pad()))
                    .h(px(m.bar_h()))
                    .border_b_1()
                    .border_color(p.line)
                    .bg(p.chrome)
                    .child(action("redis-nav-toggle", Some(toggle_icon), &toggle_label, Kind::Tool, &entity))
                    .when(!show_keys, |this| {
                        this.child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .truncate()
                                .font_family(cx.theme().mono_font_family.clone())
                                .text_size(px(TypeScale::SM))
                                .text_color(p.text_muted)
                                .child(SharedString::from(self.selected.clone())),
                        )
                    });
                let content = if show_keys { sidebar } else { detail };
                root = root
                    .flex_col()
                    .child(strip)
                    .child(div().id("redis-narrow-body").flex_1().min_h(px(0.)).flex().children(content))
                    .child(console);
            } else {
                let dragging = self.sidebar_drag_start.is_some();
                let splitter = div()
                    .id("redis-sidebar-splitter")
                    .test_support()
                    .group("redis-splitter")
                    .flex_none()
                    .w(px(kit::RESIZE_HANDLE_W))
                    .ml(px(-kit::RESIZE_HANDLE_W / 2.))
                    .mr(px(-kit::RESIZE_HANDLE_W / 2.))
                    .h_full()
                    .flex()
                    .justify_center()
                    .cursor_col_resize()
                    .child(
                        div()
                            .w(px(1.))
                            .h_full()
                            .bg(if dragging { p.accent } else { p.line })
                            .group_hover("redis-splitter", |s| s.bg(p.accent.opacity(0.65))),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            if event.click_count >= 2 {
                                this.sidebar_w = SIDEBAR_DEFAULT_W;
                                this.sidebar_drag_start = None;
                            } else {
                                this.sidebar_drag_start = Some((f32::from(event.position.x), this.sidebar_w));
                            }
                            cx.notify();
                        }),
                    );
                root = root
                    .children(sidebar)
                    .child(splitter)
                    .child(div().flex_1().min_w(px(0.)).h_full().flex().flex_col().children(detail).child(console));
            }
            root.children(new_layer).children(confirm_layer).children(menu_layer).into_any_element()
        })
    }
}

impl RedisPane {
    fn key_down(&mut self, event: &gpui_kit::KeyDownEvent, cx: &mut Context<Self>) {
        let key = match event.keystroke.key.as_str() {
            "arrowup" => "up",
            "arrowdown" => "down",
            other => other,
        };
        if key == "escape" {
            // 由近及远逐层收起：浮层 → 确认 → 对话框 → 表单 → 编辑条。
            let handled = if self.context_menu.is_some() || self.complete_open || self.db_picker_open {
                self.context_menu = None;
                self.complete_open = false;
                self.db_picker_open = false;
                true
            } else if self.confirm.is_some() {
                self.command("redis-cancel", cx);
                true
            } else if self.new_open {
                self.command("redis-new-close", cx);
                true
            } else if self.entry_form_open {
                self.command("redis-entry-form-close", cx);
                true
            } else if self.ttl_open || self.rename_open {
                self.ttl_open = false;
                self.rename_open = false;
                true
            } else {
                false
            };
            if handled {
                cx.stop_propagation();
                cx.notify();
            }
            return;
        }
        if self.focused != Some(Field::Command) {
            return;
        }
        let (_, _, completions) = self.current_completions();
        let comp_count = completions.len().min(12);
        if comp_count > 0 && key == "tab" {
            let idx = self.complete_index.min(comp_count - 1);
            if self.accept_completion(idx) {
                cx.stop_propagation();
                cx.notify();
                return;
            }
        }
        if key == "up" || key == "down" {
            if comp_count > 0 {
                if key == "down" {
                    self.complete_index = (self.complete_index + 1) % comp_count;
                } else {
                    self.complete_index = if self.complete_index == 0 { comp_count - 1 } else { self.complete_index - 1 };
                }
            } else {
                self.history_step(key == "up");
            }
            cx.stop_propagation();
            cx.notify();
        }
    }

    // ------------------------------------------------------------------ 侧栏

    #[allow(clippy::too_many_arguments)]
    fn render_sidebar(
        &mut self,
        p: &Palette,
        m: &kit::Metrics,
        entity: &Entity<Self>,
        narrow: bool,
        viewport_w: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let pattern = self.redis_input(Field::Pattern, "redis-pattern", "redis.tab.patternLabel", "user:*", window, cx);
        let max_side = (viewport_w * 0.5).clamp(SIDEBAR_MIN_W, SIDEBAR_MAX_W);
        let side_w = if narrow { viewport_w } else { self.sidebar_w.clamp(SIDEBAR_MIN_W, max_side) };
        let pad = m.pane_pad();

        let identity = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(kit::engine_badge("redis", 22.))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .text_size(px(TypeScale::MD))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.text)
                    .child(SharedString::from(self.connection_name.clone())),
            )
            .child(self.render_db_button(p, entity));

        let search = div()
            .flex()
            .items_center()
            .gap(px(2.))
            .child(
                div().flex_1().min_w(px(0.)).child(
                    kit::text_input("redis-pattern", &pattern, self.text("redis.tab.patternLabel"))
                        .small()
                        .cleanable(true)
                        .prefix(kit::input_prefix(IconName::Search, 13., p.text_faint)),
                ),
            )
            .child(icon_btn("redis-filter", IconName::ListFilter, &self.text("redis.tab.filter"), entity))
            .child(icon_btn("redis-refresh", IconName::RefreshCw, &self.text("redis.tab.refresh"), entity))
            .child(icon_btn("redis-new-key", IconName::Plus, &self.text("redis.action.newKey"), entity).primary());

        let header = div()
            .flex_none()
            .px(px(pad))
            .pt(px(10.))
            .pb(px(8.))
            .flex()
            .flex_col()
            .gap(px(8.))
            .border_b_1()
            .border_color(p.line)
            .child(identity)
            .child(search)
            .child(self.render_type_bar(p, entity));

        let list = stack_frame(|| self.render_key_list(p, m, entity, side_w, cx));
        let footer = self.render_key_footer(p, m, entity);

        div()
            .h_full()
            .flex()
            .flex_col()
            .bg(p.chrome)
            .when(narrow, |this| this.w_full().min_w(px(0.)))
            .when(!narrow, |this| this.flex_none().w(px(side_w)))
            .child(header)
            .child(list)
            .child(footer)
            .into_any_element()
    }

    fn render_db_button(&self, p: &Palette, entity: &Entity<Self>) -> AnyElement {
        let open = self.db_picker_open;
        let count = self
            .database
            .parse::<u8>()
            .ok()
            .and_then(|db| self.db_counts.iter().find(|item| item.db == db))
            .map(|item| item.keys);
        let button = kit::interactive_states(div().id("redis-db-toggle"), p, false)
            .test_support()
            .role(gpui_kit::Role::Button)
            .aria_label(SharedString::from(format!("{} db{}", self.text("redis.db.switch"), self.database)))
            .aria_expanded(open)
            .tooltip_text(SharedString::from(self.text("redis.db.switch")))
            .h(px(24.))
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(5.))
            .rounded(px(Radius::CONTROL))
            .border_1()
            .when(open, |this| this.border_color(p.accent).bg(p.accent_soft).text_color(p.accent_text))
            .when(!open, |this| {
                this.border_color(p.line)
                    .bg(if p.dark { p.raised } else { p.surface })
                    .text_color(p.text)
                    .hover(|s| s.bg(p.hover).border_color(p.line_strong))
            })
            .text_size(px(TypeScale::XS))
            .font_weight(FontWeight::MEDIUM)
            .child(kit::icon(IconName::Database, 12., if open { p.accent_text } else { p.text_muted }))
            .child(SharedString::from(format!("db{}", self.database)))
            .when_some(count, |this, n| {
                this.child(
                    div()
                        .font_features(kit::tabular_figures())
                        .text_color(if open { p.accent_text } else { p.text_faint })
                        .child(SharedString::from(n.to_string())),
                )
            })
            .child(kit::icon(IconName::ChevronDown, 11., if open { p.accent_text } else { p.text_muted }))
            .on_click(cmd_click(entity, "redis-db-toggle"));
        div()
            .relative()
            .flex_none()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(button)
            .when(open, |this| {
                this.child(
                    div().absolute().top_full().right_0().child(
                        deferred(
                            anchored()
                                .anchor(Anchor::TopRight)
                                .snap_to_window_with_margin(px(8.))
                                .child(self.db_picker_panel(p, entity)),
                        )
                        .with_priority(15),
                    ),
                )
            })
            .into_any_element()
    }

    fn db_picker_panel(&self, p: &Palette, entity: &Entity<Self>) -> AnyElement {
        let mut grid = div().flex().flex_wrap().gap(px(4.));
        for db in 0..16u8 {
            let active = self.database == db.to_string();
            let count = self.db_counts.iter().find(|item| item.db == db).map(|item| item.keys);
            let has_keys = count.is_some_and(|n| n > 0);
            let label = match count {
                Some(n) if n > 0 => format!("db{db} ({n})"),
                _ => format!("db{db}"),
            };
            let sub = match count {
                Some(n) if n > 0 => i18n::fill(&self.text("redis.db.keysCount"), &[("n", &n.to_string())]),
                _ if self.db_counts.is_empty() => "—".to_owned(),
                _ => self.text("redis.db.emptyCount"),
            };
            let cmd = format!("redis-db-pick:{db}");
            grid = grid.child(
                kit::interactive_states(div().id(SharedString::from(cmd.clone())), p, false)
                    .test_support()
                    .role(gpui_kit::Role::Button)
                    .aria_label(SharedString::from(label))
                    .aria_selected(active)
                    .w(px(64.))
                    .py(px(5.))
                    .px(px(8.))
                    .flex()
                    .flex_col()
                    .gap(px(1.))
                    .rounded(px(Radius::CONTROL))
                    .border_1()
                    .when(active, |this| this.border_color(p.accent).bg(p.accent_soft))
                    .when(!active, |this| this.border_color(p.line_soft).hover(|s| s.bg(p.hover).border_color(p.line)))
                    .child(
                        div()
                            .text_size(px(TypeScale::SM))
                            .font_weight(if active || has_keys { FontWeight::SEMIBOLD } else { FontWeight::NORMAL })
                            .text_color(if active {
                                p.accent_text
                            } else if has_keys {
                                p.text
                            } else {
                                p.text_muted
                            })
                            .child(SharedString::from(format!("db{db}"))),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(px(TypeScale::XS))
                            .font_features(kit::tabular_figures())
                            .text_color(if active { p.accent_text } else { p.text_faint })
                            .child(SharedString::from(sub)),
                    )
                    .on_click(cmd_click(entity, cmd)),
            );
        }
        div()
            .id("redis-db-picker")
            .test_support()
            .occlude()
            .mt(px(4.))
            .w(px(296.))
            .p(px(8.))
            .rounded(px(Radius::PANEL))
            .bg(p.raised)
            .border_1()
            .border_color(p.line_strong)
            .shadow_lg()
            .flex()
            .flex_col()
            .gap(px(6.))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .px(px(2.))
                    .text_size(px(TypeScale::XS))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.text_muted)
                    .child(SharedString::from(self.text("redis.db.title"))),
            )
            .child(grid)
            .into_any_element()
    }

    /// 类型筛选只列出已读到的类型（加"全部"），每项带计数；视图切换在同一行右端。
    fn render_type_bar(&self, p: &Palette, entity: &Entity<Self>) -> AnyElement {
        let mut counts: Vec<(&str, &str, usize)> = vec![
            ("string", "redis.type.string", 0),
            ("hash", "redis.type.hash", 0),
            ("list", "redis.type.list", 0),
            ("set", "redis.type.set", 0),
            ("zset", "redis.type.zset", 0),
            ("stream", "redis.type.stream", 0),
        ];
        for key in &self.keys {
            if let Some(slot) = counts.iter_mut().find(|(kind, _, _)| *kind == key.key_type) {
                slot.2 += 1;
            }
        }
        let mut chips = div().flex_1().min_w(px(0.)).flex().flex_wrap().gap(px(4.));
        chips = chips.child(kit::filter_chip(
            "redis-type-all",
            self.text("redis.type.all"),
            None,
            None,
            Some(self.keys.len()),
            self.type_filter.is_empty(),
            p,
            cmd_click(entity, "redis-type-all"),
        ));
        for (kind, label_key, n) in counts {
            let active = self.type_filter == kind;
            if n == 0 && !active {
                continue;
            }
            let id = format!("redis-type-{kind}");
            let (dot, _) = type_colors(kind, p);
            chips = chips.child(kit::filter_chip(
                SharedString::from(id.clone()),
                self.text(label_key),
                None,
                Some(dot),
                Some(n),
                active,
                p,
                cmd_click(entity, id),
            ));
        }
        let toggle = div()
            .flex_none()
            .flex()
            .items_center()
            .p(px(2.))
            .gap(px(2.))
            .rounded(px(Radius::CONTROL))
            .bg(p.sunken)
            .child(icon_btn("redis-view-flat", IconName::List, &self.text("redis.view.flat"), entity).xsmall().selected(!self.tree_view))
            .child(icon_btn("redis-view-tree", IconName::ListTree, &self.text("redis.view.tree"), entity).xsmall().selected(self.tree_view));
        div().flex().items_start().gap(px(6.)).child(chips).child(toggle).into_any_element()
    }

    fn render_key_list(&self, p: &Palette, m: &kit::Metrics, entity: &Entity<Self>, side_w: f32, cx: &mut Context<Self>) -> AnyElement {
        let mono = cx.theme().mono_font_family.clone();
        let key_rows = self.build_key_rows();
        let scan_failed = self.error_source == ErrorSource::Scan && !self.error.is_empty();
        let pattern_active = !self.pattern.trim().is_empty();
        let visible = key_rows.len();
        let state = list_state(self.busy, scan_failed, self.keys.len(), visible, pattern_active, self.done);

        let mut list = div()
            .id("redis-keys")
            .test_support()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .px(px(6.))
            .py(px(6.))
            .gap(px(1.));
        match state {
            ListState::Loading => list = list.child(kit::skeleton_state(8, m, p)),
            ListState::Error => {
                let retry = action("redis-retry-scan", Some(IconName::RefreshCw), &self.text("common.retry"), Kind::Secondary, entity).into_any_element();
                list = list.child(kit::error_state_with_action(
                    SharedString::from(self.text("redis.error.scan.heading")),
                    SharedString::from(self.error.clone()),
                    retry,
                    p,
                ));
            }
            ListState::NoMatch => {
                let clear = action("redis-clear-filter", Some(IconName::ListFilter), &self.text("redis.noMatch.clear"), Kind::Secondary, entity).into_any_element();
                list = list.child(kit::no_match_state_with_action(
                    IconName::Search,
                    SharedString::from(self.text("redis.noMatch.heading")),
                    SharedString::from(self.text("redis.noMatch.detail")),
                    clear,
                    p,
                ));
            }
            ListState::Empty => {
                let create = action("redis-new-key", Some(IconName::Plus), &self.text("redis.action.newKey"), Kind::Primary, entity).into_any_element();
                list = list.child(kit::empty_state_with_action(
                    IconName::Key,
                    SharedString::from(self.text("redis.empty.heading")),
                    SharedString::from(self.text("redis.empty.detail")),
                    create,
                    p,
                ));
            }
            ListState::Rows => {
                if self.busy {
                    list = list.child(div().px(px(6.)).child(kit::inline_loading(SharedString::from(self.text("redis.list.busy")), m, p)));
                } else if visible == 0 {
                    list = list.child(
                        div()
                            .id("redis-keys-blank")
                            .test_support()
                            .px(px(8.))
                            .py(px(4.))
                            .text_size(px(TypeScale::XS))
                            .text_color(p.text_faint)
                            .child(SharedString::from(self.text("redis.tab.emptyList"))),
                    );
                }
            }
        }

        // 键名列可用宽度：侧栏宽 − 列表内边距 − 行内边距 − 类型徽标 − 间距 − 行尾 TTL。
        let base_w = side_w - 12. - 16. - TYPE_BADGE_W - 8.;
        for row in key_rows.into_iter().take(KEY_ROWS_MAX) {
            match row {
                KeyRow::Folder { prefix, segment, depth, count, open } => {
                    let id = format!("redis-folder:{prefix}");
                    let menu_prefix = prefix.clone();
                    let menu_entity = entity.clone();
                    list = list.child(
                        kit::interactive_states(div().id(SharedString::from(id.clone())), p, false)
                            .test_support()
                            .role(gpui_kit::Role::Button)
                            .aria_label(SharedString::from(prefix.clone()))
                            .aria_expanded(open)
                            .flex_none()
                            .h(px(m.tree_row_h))
                            .pl(px(6. + depth as f32 * 14.))
                            .pr(px(8.))
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .rounded(px(Radius::CONTROL))
                            .text_size(px(TypeScale::SM))
                            .text_color(p.text)
                            .hover(|style| style.bg(p.hover))
                            .child(kit::icon(if open { IconName::ChevronDown } else { IconName::ChevronRight }, 12., p.text_faint))
                            .child(kit::icon(if open { IconName::FolderOpen } else { IconName::Folder }, 13., p.accent_text))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .truncate()
                                    .font_family(mono.clone())
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(SharedString::from(segment)),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(px(TypeScale::XS))
                                    .font_features(kit::tabular_figures())
                                    .text_color(p.text_faint)
                                    .child(SharedString::from(count.to_string())),
                            )
                            .on_mouse_down(MouseButton::Right, move |event: &MouseDownEvent, _, cx| {
                                cx.stop_propagation();
                                let pfx = menu_prefix.clone();
                                let pos = event.position;
                                menu_entity.update(cx, |pane, cx| pane.open_folder_context_menu(pfx, pos, cx));
                            })
                            .on_click(cmd_click(entity, id)),
                    );
                }
                KeyRow::Key { index, display, depth } => {
                    let key = &self.keys[index];
                    let selected = key.name == self.selected;
                    let id = format!("redis-select:{index}");
                    let label = format!("{} {} {}", key.key_type, key.name, describe_ttl(self.locale, key.ttl_ms));
                    let menu_entity = entity.clone();
                    let ttl = key.ttl_ms.filter(|ttl| *ttl >= 0);
                    let ttl_w = if ttl.is_some() { 34. } else { 0. };
                    let avail = (base_w - depth as f32 * 14. - ttl_w).max(0.);
                    let shown = middle_ellipsis(&display, (avail / MONO_CHAR_W).floor() as usize);
                    list = list.child(
                        kit::interactive_states(div().id(SharedString::from(id.clone())), p, false)
                            .test_support()
                            .role(gpui_kit::Role::Button)
                            .aria_label(SharedString::from(label))
                            .aria_selected(selected)
                            .tooltip_text(SharedString::from(key.name.clone()))
                            .flex_none()
                            .h(px(m.tree_row_h + 2.))
                            .pl(px(6. + depth as f32 * 14.))
                            .pr(px(8.))
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .rounded(px(Radius::CONTROL))
                            .when(selected, |this| this.bg(p.selection))
                            .when(!selected, |this| this.hover(|style| style.bg(p.hover)))
                            .child(type_badge(&key.key_type, p))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .truncate()
                                    .font_family(mono.clone())
                                    .text_size(px(TypeScale::SM))
                                    .text_color(p.text)
                                    .when(selected, |this| this.font_weight(FontWeight::MEDIUM))
                                    .child(SharedString::from(shown)),
                            )
                            .when_some(ttl, |this, ms| {
                                this.child(
                                    div()
                                        .flex_none()
                                        .text_size(px(TypeScale::XS))
                                        .font_features(kit::tabular_figures())
                                        .text_color(if ms < 60_000 { p.warn } else { p.text_faint })
                                        .child(SharedString::from(short_ttl(ms))),
                                )
                            })
                            .on_mouse_down(MouseButton::Right, move |event: &MouseDownEvent, _, cx| {
                                cx.stop_propagation();
                                let pos = event.position;
                                menu_entity.update(cx, |pane, cx| pane.open_key_context_menu(index, pos, cx));
                            })
                            .on_click(cmd_click(entity, id)),
                    );
                }
            }
        }
        list.into_any_element()
    }

    fn render_key_footer(&self, p: &Palette, m: &kit::Metrics, entity: &Entity<Self>) -> AnyElement {
        let scan_failed = self.error_source == ErrorSource::Scan && !self.error.is_empty();
        let bar = div()
            .flex_none()
            .min_h(px(m.bar_h()))
            .px(px(m.pane_pad()))
            .py(px(4.))
            .flex()
            .items_center()
            .gap(px(6.))
            .border_t_1()
            .border_color(p.line);
        if scan_failed && !self.keys.is_empty() {
            return bar
                .id("redis-list-error")
                .test_support()
                .child(kit::icon(IconName::CircleAlert, 13., p.danger))
                .child(div().flex_1().min_w(px(0.)).truncate().text_size(px(TypeScale::XS)).text_color(p.danger).child(SharedString::from(self.error.clone())))
                .child(action("redis-retry-scan", Some(IconName::RefreshCw), &self.text("common.retry"), Kind::Tool, entity))
                .into_any_element();
        }
        let total = i18n::fill(&self.text("redis.db.keysCount"), &[("n", &self.keys.len().to_string())]);
        let shown = if self.type_filter.is_empty() {
            None
        } else {
            let visible = self.keys.iter().filter(|key| key.key_type == self.type_filter).count();
            Some(i18n::fill(&self.text("redis.count.filtered"), &[("shown", &visible.to_string()), ("n", &self.keys.len().to_string())]))
        };
        let progress = if self.done { self.text("redis.list.allLoaded") } else { self.text("redis.list.moreLeft") };
        let summary = match shown {
            Some(shown) => format!("{shown} · {progress}"),
            None => format!("{total} · {progress}"),
        };
        bar.child(
            div()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .items_center()
                .gap(px(5.))
                .child(kit::status_dot(if self.done { p.ok } else { p.warn }))
                .child(
                    div()
                        .min_w(px(0.))
                        .truncate()
                        .text_size(px(TypeScale::XS))
                        .font_features(kit::tabular_figures())
                        .text_color(p.text_muted)
                        .child(SharedString::from(summary)),
                ),
        )
        .when(!self.done, |this| {
            this.child(action("redis-more", Some(IconName::ChevronsDown), &self.text("redis.tab.loadMore"), Kind::Tool, entity).disabled(self.busy))
        })
        .into_any_element()
    }

    // ------------------------------------------------------------------ 详情

    fn render_detail(&mut self, p: &Palette, m: &kit::Metrics, entity: &Entity<Self>, viewport_h: f32, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let col = div().flex().flex_col().flex_1().min_w(px(0.)).min_h(px(0.)).bg(p.surface);
        if self.selected.is_empty() {
            let create = action("redis-new-key-value", Some(IconName::Plus), &self.text("redis.action.newKey"), Kind::Primary, entity).into_any_element();
            return col
                .child(
                    div().flex_1().min_w(px(0.)).p(px(m.pane_pad() * 2.)).child(kit::empty_state_with_action(
                        IconName::Key,
                        SharedString::from(self.text("redis.empty.pick.heading")),
                        SharedString::from(self.text("redis.tab.pickKey")),
                        create,
                        p,
                    )),
                )
                .into_any_element();
        }

        let header = self.render_detail_header(p, m, entity, cx);
        let ttl_bar = if self.ttl_open { Some(self.render_ttl_bar(p, m, entity, window, cx)) } else { None };
        let rename_bar = if self.rename_open { Some(self.render_rename_bar(p, m, entity, window, cx)) } else { None };

        let detail_error = self.error_source == ErrorSource::Detail && !self.error.is_empty();
        let body = if self.detail.is_some() {
            stack_frame(|| self.value_body(p, m, entity, viewport_h, window, cx))
        } else if detail_error {
            let retry = action("redis-retry-read", Some(IconName::RefreshCw), &self.text("redis.retry.read"), Kind::Secondary, entity).into_any_element();
            kit::error_state_with_action(
                SharedString::from(self.text("redis.error.detail.heading")),
                SharedString::from(self.error.clone()),
                retry,
                p,
            )
            .into_any_element()
        } else if self.busy {
            kit::skeleton_state(5, m, p).into_any_element()
        } else {
            div().text_size(px(TypeScale::SM)).text_color(p.text_muted).child(SharedString::from(self.text("redis.tab.pickKey"))).into_any_element()
        };

        col.child(header)
            .children(ttl_bar)
            .children(rename_bar)
            .child(
                div()
                    .id("redis-value")
                    .test_support()
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .px(px(m.pane_pad()))
                    .py(px(10.))
                    .child(body),
            )
            .into_any_element()
    }

    fn render_detail_header(&self, p: &Palette, m: &kit::Metrics, entity: &Entity<Self>, cx: &mut Context<Self>) -> AnyElement {
        let mono = cx.theme().mono_font_family.clone();
        let detail = self.detail.as_deref();
        let type_name = detail.map(|d| d.key_type.clone()).unwrap_or_default();
        let ttl_ms = detail.and_then(|d| d.ttl_ms);
        let size = detail.map(|d| self.count_label(d)).unwrap_or_default();
        let expiring = ttl_ms.is_some_and(|ms| ms >= 0);
        let urgent = ttl_ms.is_some_and(|ms| (0..60_000).contains(&ms));

        let title = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .min_w(px(0.))
            .when(!type_name.is_empty(), |this| this.child(type_badge(&type_name, p)))
            .child(
                div()
                    .id("redis-key-title")
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .font_family(mono)
                    .text_size(px(TypeScale::LG))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.text)
                    .tooltip_text(SharedString::from(self.selected.clone()))
                    .child(SharedString::from(self.selected.clone())),
            )
            .child(icon_btn("redis-copy-key", IconName::Copy, &self.text("redis.copy.keyName"), entity))
            .child(icon_btn("redis-refresh-key", IconName::RefreshCw, &self.text("redis.tab.refresh"), entity));

        let (ttl_fg, ttl_bg) = if urgent {
            (p.warn, p.warn_soft)
        } else if expiring {
            (p.info, p.info.opacity(0.12))
        } else {
            (p.text_muted, p.sunken)
        };
        let ttl_chip = kit::interactive_states(div().id("redis-ttl-chip"), p, false)
            .test_support()
            .role(gpui_kit::Role::Button)
            .aria_label(SharedString::from(self.text("redis.tab.setTtl")))
            .tooltip_text(SharedString::from(self.text("redis.tab.setTtl")))
            .h(px(22.))
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(5.))
            .rounded(px(Radius::CONTROL))
            .bg(ttl_bg)
            .text_color(ttl_fg)
            .text_size(px(TypeScale::XS))
            .font_weight(FontWeight::MEDIUM)
            .font_features(kit::tabular_figures())
            .hover(|s| s.opacity(0.85))
            .child(kit::icon(if expiring { IconName::Timer } else { IconName::Infinity }, 12., ttl_fg))
            .child(SharedString::from(describe_ttl(self.locale, ttl_ms)))
            .on_click(cmd_click(entity, "redis-ttl-toggle"));

        let meta = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(6.))
            .child(ttl_chip)
            .when(!size.is_empty(), |this| {
                this.child(
                    div()
                        .h(px(22.))
                        .px(px(8.))
                        .flex()
                        .items_center()
                        .rounded(px(Radius::CONTROL))
                        .bg(p.sunken)
                        .text_size(px(TypeScale::XS))
                        .font_features(kit::tabular_figures())
                        .text_color(p.text_muted)
                        .child(SharedString::from(size)),
                )
            })
            .child(div().flex_1())
            .child(
                if self.ttl_open {
                    action("redis-ttl-toggle", Some(IconName::Timer), &self.text("redis.tab.setTtl"), Kind::Subtle, entity)
                } else {
                    action("redis-ttl", Some(IconName::Timer), &self.text("redis.tab.setTtl"), Kind::Tool, entity)
                }
                .small(),
            )
            .child(
                if self.rename_open {
                    action("redis-rename-toggle", Some(IconName::Pencil), &self.text("redis.tab.rename"), Kind::Subtle, entity)
                } else {
                    action("redis-rename", Some(IconName::Pencil), &self.text("redis.tab.rename"), Kind::Tool, entity)
                }
                .small(),
            )
            .child(action("redis-copy", Some(IconName::Copy), &self.text("redis.copy.keyValue"), Kind::Tool, entity).small())
            .child(action("redis-delete", Some(IconName::Trash), &self.text("redis.tab.remove"), Kind::DangerSoft, entity).small());

        div()
            .flex_none()
            .px(px(m.pane_pad()))
            .pt(px(10.))
            .pb(px(8.))
            .flex()
            .flex_col()
            .gap(px(8.))
            .border_b_1()
            .border_color(p.line)
            .child(title)
            .child(meta)
            .into_any_element()
    }

    fn edit_bar(p: &Palette, m: &kit::Metrics) -> gpui_kit::Div {
        div()
            .flex_none()
            .mx(px(m.pane_pad()))
            .mt(px(8.))
            .px(px(10.))
            .py(px(6.))
            .rounded(px(Radius::PANEL))
            .bg(p.sunken)
            .border_1()
            .border_color(p.line_soft)
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(8.))
    }

    fn render_ttl_bar(&mut self, p: &Palette, m: &kit::Metrics, entity: &Entity<Self>, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let hint = self.text("redis.ttl.hint");
        let ttl = self.redis_input(Field::Ttl, "redis-ttl-field", "redis.tab.ttlLabel", &hint, window, cx);
        let expiring = self.detail_ttl().is_some_and(|ms| ms >= 0);
        let mut presets = div().flex().items_center().gap(px(2.));
        for (secs, key) in [(60, "redis.ttl.1m"), (3600, "redis.ttl.1h"), (86_400, "redis.ttl.1d")] {
            presets = presets.child(action(format!("redis-ttl-preset:{secs}"), None, &self.text(key), Kind::Tool, entity).small());
        }
        if expiring {
            presets = presets.child(action("redis-ttl-preset:persist", Some(IconName::Infinity), &self.text("redis.action.removeTtl"), Kind::Tool, entity).small());
        }
        Self::edit_bar(p, m)
            .child(div().text_size(px(TypeScale::XS)).font_weight(FontWeight::MEDIUM).text_color(p.text_muted).child(SharedString::from(self.text("redis.tab.ttlLabel"))))
            .child(div().w(px(180.)).child(kit::text_input("redis-ttl-field", &ttl, self.text("redis.tab.ttlLabel")).small()))
            .child(presets)
            .child(div().flex_1())
            .child(action("redis-ttl", Some(IconName::Check), &self.text("redis.tab.setTtl"), Kind::Primary, entity).small())
            .child(icon_btn("redis-ttl-close", IconName::X, &self.text("common.cancel"), entity))
            .into_any_element()
    }

    fn render_rename_bar(&mut self, p: &Palette, m: &kit::Metrics, entity: &Entity<Self>, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let rename = self.redis_input(Field::Rename, "redis-rename-field", "redis.tab.rename", "new:key", window, cx);
        Self::edit_bar(p, m)
            .child(div().text_size(px(TypeScale::XS)).font_weight(FontWeight::MEDIUM).text_color(p.text_muted).child(SharedString::from(self.text("redis.rename.label"))))
            .child(div().flex_1().min_w(px(160.)).child(kit::text_input("redis-rename-field", &rename, self.text("redis.tab.rename")).small()))
            .child(action("redis-rename", Some(IconName::Check), &self.text("redis.tab.rename"), Kind::Primary, entity).small())
            .child(icon_btn("redis-rename-close", IconName::X, &self.text("common.cancel"), entity))
            .into_any_element()
    }

    // ------------------------------------------------------------------ 值区域

    fn value_body(&mut self, p: &Palette, m: &kit::Metrics, entity: &Entity<Self>, viewport_h: f32, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(detail) = self.detail.clone() else {
            return div().text_size(px(TypeScale::SM)).child(SharedString::from(self.text("redis.tab.pickKey"))).into_any_element();
        };
        match &detail.value {
            RedisValue::String { text } => {
                let viewer = self.ensure_string_viewer(text, window, cx);
                div()
                    .flex_1()
                    .min_h(px(200.))
                    .flex()
                    .flex_col()
                    .p(px(8.))
                    .rounded(px(Radius::PANEL))
                    .border_1()
                    .border_color(p.line)
                    .bg(p.surface)
                    .child(div().flex_1().min_h(px(0.)).child(viewer))
                    .into_any_element()
            }
            RedisValue::None {} => kit::empty_state(IconName::Key, SharedString::from(self.text("redis.keys.missing")), SharedString::from(self.text("redis.value.none")), p).into_any_element(),
            _ => stack_frame(|| self.collection_body(&detail, p, m, entity, viewport_h, window, cx)),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn collection_body(
        &mut self,
        detail: &RedisKeyDetail,
        p: &Palette,
        m: &kit::Metrics,
        entity: &Entity<Self>,
        viewport_h: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let query = self.redis_input(Field::HashQuery, "redis-hash-filter", "redis.entry.filter", "", window, cx);
        let needle = self.hash_query.trim().to_lowercase();
        let hit = |text: &str| needle.is_empty() || text.to_lowercase().contains(&needle);
        let copy = self.text("common.copy");
        let fmt = self.text("redis.menu.formatView");
        let item = |id: String, icon: IconName, label: &str, danger: bool| ContextItem { id, icon, label: label.to_owned(), danger };
        let act = |cmd: String, icon: IconName, label: &str| EntryAction { cmd, icon, label: label.to_owned() };

        let mut rows = Vec::new();
        let (cols, total, add_label, inspected): (Vec<EntryCol>, usize, String, Option<InspectedEntry>) = match &detail.value {
            RedisValue::Hash { entries } => {
                let (edit, del, copy_field) = (self.text("redis.hash.edit"), self.text("redis.hash.delete"), self.text("redis.copy.field"));
                for (i, entry) in entries.iter().enumerate() {
                    if !(hit(&entry.field) || hit(&entry.value)) {
                        continue;
                    }
                    rows.push(EntryRow {
                        index: i,
                        id: format!("redis-hash-row:{i}"),
                        cells: vec![one_line(&entry.field), one_line(&entry.value)],
                        actions: vec![
                            act(format!("redis-hash-copy:{i}"), IconName::Copy, &copy),
                            act(format!("redis-hash-edit:{i}"), IconName::Pencil, &edit),
                            act(format!("redis-hash-del:{i}"), IconName::Trash, &del),
                        ],
                        menu: vec![
                            item(format!("redis-entry-inspect:{i}"), IconName::Eye, &fmt, false),
                            item(format!("redis-hash-copy:{i}"), IconName::Copy, &self.text("redis.copy.keyValue"), false),
                            item(format!("redis-hash-copy-field:{i}"), IconName::Tag, &copy_field, false),
                            item(format!("redis-hash-edit:{i}"), IconName::Pencil, &edit, false),
                            item(format!("redis-hash-del:{i}"), IconName::Trash, &del, true),
                        ],
                    });
                }
                let inspected = self.entry_focus.and_then(|i| entries.get(i).map(|e| (i, e.field.clone(), e.value.clone(), Some(EntryTarget::Hash(e.field.clone())))));
                (
                    vec![
                        EntryCol { label: self.text("redis.value.field"), width: Some(220.), tone: Tone::Text },
                        EntryCol { label: self.text("redis.value.value"), width: None, tone: Tone::Muted },
                    ],
                    entries.len(),
                    self.text("redis.hash.add"),
                    inspected,
                )
            }
            RedisValue::List { items } => {
                let (edit, del) = (self.text("redis.list.edit"), self.text("redis.list.remove"));
                for (i, value) in items.iter().enumerate() {
                    if !(hit(value) || hit(&i.to_string())) {
                        continue;
                    }
                    rows.push(EntryRow {
                        index: i,
                        id: format!("redis-list-row:{i}"),
                        cells: vec![i.to_string(), one_line(value)],
                        actions: vec![
                            act(format!("redis-list-copy:{i}"), IconName::Copy, &copy),
                            act(format!("redis-list-edit:{i}"), IconName::Pencil, &edit),
                            act(format!("redis-list-rem:{i}"), IconName::Trash, &del),
                        ],
                        menu: vec![
                            item(format!("redis-entry-inspect:{i}"), IconName::Eye, &fmt, false),
                            item(format!("redis-list-copy:{i}"), IconName::Copy, &copy, false),
                            item(format!("redis-list-edit:{i}"), IconName::Pencil, &edit, false),
                            item(format!("redis-list-rem:{i}"), IconName::Trash, &del, true),
                        ],
                    });
                }
                let inspected = self.entry_focus.and_then(|i| items.get(i).map(|v| (i, format!("#{i}"), v.clone(), Some(EntryTarget::List(i as i64)))));
                (
                    vec![
                        EntryCol { label: self.text("redis.entry.index"), width: Some(64.), tone: Tone::Muted },
                        EntryCol { label: self.text("redis.value.value"), width: None, tone: Tone::Text },
                    ],
                    items.len(),
                    self.text("redis.list.add"),
                    inspected,
                )
            }
            RedisValue::Set { items } => {
                let (copy_member, del) = (self.text("redis.copy.member"), self.text("redis.set.remove"));
                for (i, member) in items.iter().enumerate() {
                    if !hit(member) {
                        continue;
                    }
                    rows.push(EntryRow {
                        index: i,
                        id: format!("redis-set-row:{i}"),
                        cells: vec![one_line(member)],
                        actions: vec![
                            act(format!("redis-set-copy:{i}"), IconName::Copy, &copy_member),
                            act(format!("redis-set-rem:{i}"), IconName::Trash, &del),
                        ],
                        menu: vec![
                            item(format!("redis-entry-inspect:{i}"), IconName::Eye, &fmt, false),
                            item(format!("redis-set-copy:{i}"), IconName::Copy, &copy_member, false),
                            item(format!("redis-set-rem:{i}"), IconName::Trash, &del, true),
                        ],
                    });
                }
                let member = self.text("redis.value.member");
                let inspected = self.entry_focus.and_then(|i| items.get(i).map(|v| (i, format!("{member} #{i}"), v.clone(), None)));
                (vec![EntryCol { label: member.clone(), width: None, tone: Tone::Text }], items.len(), self.text("redis.set.add"), inspected)
            }
            RedisValue::ZSet { entries } => {
                let (copy_member, edit, del) = (self.text("redis.copy.member"), self.text("redis.zset.edit"), self.text("redis.zset.remove"));
                for (i, entry) in entries.iter().enumerate() {
                    if !(hit(&entry.member) || hit(&entry.score)) {
                        continue;
                    }
                    rows.push(EntryRow {
                        index: i,
                        id: format!("redis-zset-row:{i}"),
                        cells: vec![entry.score.clone(), one_line(&entry.member)],
                        actions: vec![
                            act(format!("redis-zset-copy:{i}"), IconName::Copy, &copy_member),
                            act(format!("redis-zset-edit:{i}"), IconName::Pencil, &edit),
                            act(format!("redis-zrem:{i}"), IconName::Trash, &del),
                        ],
                        menu: vec![
                            item(format!("redis-entry-inspect:{i}"), IconName::Eye, &fmt, false),
                            item(format!("redis-zset-copy:{i}"), IconName::Copy, &copy_member, false),
                            item(format!("redis-zset-edit:{i}"), IconName::Pencil, &edit, false),
                            item(format!("redis-zrem:{i}"), IconName::Trash, &del, true),
                        ],
                    });
                }
                let inspected = self.entry_focus.and_then(|i| entries.get(i).map(|e| (i, format!("score = {}", e.score), e.member.clone(), None)));
                (
                    vec![
                        EntryCol { label: self.text("redis.value.score"), width: Some(120.), tone: Tone::Accent },
                        EntryCol { label: self.text("redis.value.member"), width: None, tone: Tone::Text },
                    ],
                    entries.len(),
                    self.text("redis.zset.add"),
                    inspected,
                )
            }
            RedisValue::Stream { messages } => {
                let (copy_value, copy_id, del) = (self.text("redis.copy.keyValue"), self.text("redis.copy.id"), self.text("redis.stream.delete"));
                for (i, message) in messages.iter().enumerate() {
                    let summary = message.fields.iter().map(|f| format!("{}={}", f.field, f.value)).collect::<Vec<_>>().join("  ");
                    if !(hit(&message.id) || hit(&summary)) {
                        continue;
                    }
                    rows.push(EntryRow {
                        index: i,
                        id: format!("redis-stream-row:{i}"),
                        cells: vec![message.id.clone(), one_line(&summary)],
                        actions: vec![
                            act(format!("redis-stream-copy:{i}"), IconName::Copy, &copy_value),
                            act(format!("redis-xdel:{i}"), IconName::Trash, &del),
                        ],
                        menu: vec![
                            item(format!("redis-entry-inspect:{i}"), IconName::Eye, &fmt, false),
                            item(format!("redis-stream-copy:{i}"), IconName::Copy, &copy_value, false),
                            item(format!("redis-stream-copy-id:{i}"), IconName::Tag, &copy_id, false),
                            item(format!("redis-xdel:{i}"), IconName::Trash, &del, true),
                        ],
                    });
                }
                let inspected = self.entry_focus.and_then(|i| {
                    let msg = messages.get(i)?;
                    let json = self.stream_payload_json(&i.to_string())?;
                    Some((i, msg.id.clone(), json, None))
                });
                (
                    vec![
                        EntryCol { label: self.text("redis.stream.id"), width: Some(180.), tone: Tone::Accent },
                        EntryCol { label: self.text("redis.value.value"), width: None, tone: Tone::Muted },
                    ],
                    messages.len(),
                    self.text("redis.stream.add"),
                    inspected,
                )
            }
            RedisValue::String { .. } | RedisValue::None {} => (Vec::new(), 0, String::new(), None),
        };

        let shown = rows.len();
        let count_text = if needle.is_empty() {
            self.count_label(detail)
        } else {
            i18n::fill(&self.text("redis.count.filtered"), &[("shown", &shown.to_string()), ("n", &total.to_string())])
        };
        let toolbar = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(
                div().w(px(240.)).min_w(px(140.)).flex_shrink(1.).child(
                    kit::text_input("redis-hash-filter", &query, self.text("redis.entry.filter"))
                        .small()
                        .cleanable(true)
                        .prefix(kit::input_prefix(IconName::Search, 13., p.text_faint)),
                ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .text_size(px(TypeScale::XS))
                    .font_features(kit::tabular_figures())
                    .text_color(p.text_muted)
                    .child(SharedString::from(count_text)),
            )
            .child(
                action(
                    "redis-entry-form",
                    Some(if self.entry_form_open { IconName::ChevronUp } else { IconName::Plus }),
                    &add_label,
                    if self.entry_form_open { Kind::Subtle } else { Kind::Secondary },
                    entity,
                )
                .small(),
            );
        let form = if self.entry_form_open { Some(self.render_entry_form(detail, p, entity, window, cx)) } else { None };

        // 条目表按内容取高，最多占值区域的四成多一点；剩下的高度都给结构化查看器。
        let body_h = (viewport_h - 300.).max(320.);
        let cap = (body_h * 0.45).max(150.);
        let content_h = (m.row_h - 2.) + shown.max(1) as f32 * m.row_h + 2.;
        let table_h = content_h.min(cap);
        let table = self.render_entry_table(&cols, rows, total, table_h, p, m, entity, cx);

        let inspector = self.render_entry_inspector(inspected, p, window, cx);
        div()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(toolbar)
            .children(form)
            .child(table)
            .child(inspector)
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_entry_table(
        &self,
        cols: &[EntryCol],
        rows: Vec<EntryRow>,
        total: usize,
        height: f32,
        p: &Palette,
        m: &kit::Metrics,
        entity: &Entity<Self>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mono = cx.theme().mono_font_family.clone();
        let actions_w = rows.first().map(|row| row.actions.len()).unwrap_or(3) as f32 * 24.;
        let cell = |width: Option<f32>| match width {
            Some(w) => div().flex_none().w(px(w)).min_w(px(0.)),
            None => div().flex_1().min_w(px(0.)),
        };
        let mut header = div()
            .flex_none()
            .h(px(m.row_h - 2.))
            .px(px(10.))
            .flex()
            .items_center()
            .gap(px(12.))
            .bg(p.chrome)
            .border_b_1()
            .border_color(p.line)
            .text_size(px(TypeScale::XS))
            .font_weight(FontWeight::MEDIUM)
            .text_color(p.text_muted);
        for col in cols {
            header = header.child(cell(col.width).truncate().child(SharedString::from(col.label.clone())));
        }
        header = header.child(div().flex_none().w(px(actions_w)));

        let mut body = div().id("redis-entries-scroll").test_support().flex_1().min_h(px(0.)).overflow_y_scroll().flex().flex_col();
        if rows.is_empty() {
            let message = if total == 0 { self.text("redis.value.empty") } else { self.text("redis.entry.noMatch") };
            body = body.child(div().px(px(10.)).py(px(8.)).text_size(px(TypeScale::SM)).text_color(p.text_muted).child(SharedString::from(message)));
        }
        for row in rows.into_iter().take(ENTRY_ROWS_MAX) {
            let focused = self.entry_focus == Some(row.index);
            let group = SharedString::from(format!("redis-entry-{}", row.index));
            let mut el = kit::interactive_states(div().id(SharedString::from(row.id)), p, false)
                .test_support()
                .group(group.clone())
                .role(gpui_kit::Role::Row)
                .aria_selected(focused)
                .flex_none()
                .h(px(m.row_h))
                .px(px(10.))
                .flex()
                .items_center()
                .gap(px(12.))
                .border_b_1()
                .border_color(p.line_soft)
                .when(focused, |s| s.bg(p.selection))
                .when(!focused, |s| s.hover(|h| h.bg(p.hover)));
            for (col, text) in cols.iter().zip(row.cells) {
                let color = match col.tone {
                    Tone::Text => p.text,
                    Tone::Muted => p.text_muted,
                    Tone::Accent => p.accent_text,
                };
                el = el.child(
                    cell(col.width)
                        .truncate()
                        .font_family(mono.clone())
                        .text_size(px(TypeScale::SM))
                        .text_color(color)
                        .when(matches!(col.tone, Tone::Accent), |c| c.font_features(kit::tabular_figures()))
                        .child(SharedString::from(text)),
                );
            }
            let mut actions = div()
                .flex_none()
                .w(px(actions_w))
                .flex()
                .items_center()
                .justify_end()
                .gap(px(2.))
                .when(!focused, |d| d.invisible().group_hover(group.clone(), |s| s.visible()));
            for item in row.actions {
                let click = cmd_click(entity, item.cmd.clone());
                actions = actions.child(kit::icon_button(SharedString::from(item.cmd), item.icon, SharedString::from(item.label), click).xsmall());
            }
            let menu_entity = entity.clone();
            let menu = row.menu;
            el = el
                .child(actions)
                .on_mouse_down(MouseButton::Right, move |event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    let pos = event.position;
                    let items = menu.clone();
                    menu_entity.update(cx, |pane, cx| pane.open_entry_context_menu(items, pos, cx));
                })
                .on_click(cmd_click(entity, format!("redis-entry-inspect:{}", row.index)));
            body = body.child(el);
        }
        div()
            .id("redis-entries")
            .test_support()
            .flex_none()
            .h(px(height))
            .flex()
            .flex_col()
            .rounded(px(Radius::PANEL))
            .border_1()
            .border_color(p.line)
            .overflow_hidden()
            .bg(p.surface)
            .child(header)
            .child(body)
            .into_any_element()
    }

    fn render_entry_form(&mut self, detail: &RedisKeyDetail, p: &Palette, entity: &Entity<Self>, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let form = div()
            .id("redis-entry-form")
            .test_support()
            .flex_none()
            .p(px(8.))
            .rounded(px(Radius::PANEL))
            .bg(p.sunken)
            .border_1()
            .border_color(p.line_soft)
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(8.));
        let close = icon_btn("redis-entry-form-close", IconName::X, &self.text("redis.entry.close"), entity);
        let form = match &detail.value {
            RedisValue::Hash { .. } => {
                let field = self.redis_input(Field::EntryField, "redis-hash-field", "redis.value.field", &self.text("redis.value.field"), window, cx);
                let value = self.redis_input(Field::EntryValue, "redis-hash-value", "redis.value.value", &self.text("redis.value.value"), window, cx);
                form.child(div().w(px(200.)).child(kit::text_input("redis-hash-field", &field, self.text("redis.value.field")).small()))
                    .child(div().flex_1().min_w(px(180.)).child(kit::text_input("redis-hash-value", &value, self.text("redis.value.value")).small()))
                    .child(action("redis-hash-add", Some(IconName::Check), &self.text("redis.hash.save"), Kind::Primary, entity).small())
            }
            RedisValue::List { .. } => {
                let value = self.redis_input(Field::EntryValue, "redis-list-value", "redis.value.value", &self.text("redis.value.value"), window, cx);
                let form = form.child(div().flex_1().min_w(px(180.)).child(kit::text_input("redis-list-value", &value, self.text("redis.value.value")).small()));
                match self.entry_index.trim().parse::<usize>() {
                    Ok(index) => form.child(
                        action(
                            format!("redis-list-set:{index}"),
                            Some(IconName::Check),
                            &i18n::fill(&self.text("redis.list.setAt"), &[("index", &index.to_string())]),
                            Kind::Primary,
                            entity,
                        )
                        .small(),
                    ),
                    Err(_) => form
                        .child(action("redis-list-lpush", Some(IconName::ArrowUp), &self.text("redis.list.lpush"), Kind::Secondary, entity).small())
                        .child(action("redis-list-rpush", Some(IconName::ArrowDown), &self.text("redis.list.rpush"), Kind::Primary, entity).small()),
                }
            }
            RedisValue::Set { .. } => {
                let member = self.redis_input(Field::EntryMember, "redis-set-member", "redis.value.member", &self.text("redis.value.member"), window, cx);
                form.child(div().flex_1().min_w(px(180.)).child(kit::text_input("redis-set-member", &member, self.text("redis.value.member")).small()))
                    .child(action("redis-set-add", Some(IconName::Check), &self.text("redis.set.add"), Kind::Primary, entity).small())
            }
            RedisValue::ZSet { .. } => {
                let score = self.redis_input(Field::EntryScore, "redis-zset-score", "redis.value.score", &self.text("redis.value.score"), window, cx);
                let member = self.redis_input(Field::EntryMember, "redis-zset-member", "redis.value.member", &self.text("redis.value.member"), window, cx);
                form.child(div().w(px(110.)).child(kit::text_input("redis-zset-score", &score, self.text("redis.value.score")).small()))
                    .child(div().flex_1().min_w(px(160.)).child(kit::text_input("redis-zset-member", &member, self.text("redis.value.member")).small()))
                    .child(action("redis-zadd", Some(IconName::Check), &self.text("redis.zset.add"), Kind::Primary, entity).small())
            }
            RedisValue::Stream { .. } => {
                let id = self.redis_input(Field::StreamId, "redis-stream-id", "redis.stream.id", "*", window, cx);
                let field = self.redis_input(Field::StreamField, "redis-stream-field", "redis.value.field", &self.text("redis.value.field"), window, cx);
                let value = self.redis_input(Field::StreamValue, "redis-stream-value", "redis.value.value", &self.text("redis.value.value"), window, cx);
                form.child(div().w(px(130.)).child(kit::text_input("redis-stream-id", &id, self.text("redis.stream.id")).small()))
                    .child(div().w(px(150.)).child(kit::text_input("redis-stream-field", &field, self.text("redis.value.field")).small()))
                    .child(div().flex_1().min_w(px(160.)).child(kit::text_input("redis-stream-value", &value, self.text("redis.value.value")).small()))
                    .child(action("redis-xadd", Some(IconName::Check), &self.text("redis.stream.add"), Kind::Primary, entity).small())
            }
            RedisValue::String { .. } | RedisValue::None {} => form,
        };
        form.child(close).into_any_element()
    }

    fn render_entry_inspector(&mut self, inspected: Option<InspectedEntry>, p: &Palette, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some((idx, label, raw, target)) = inspected else {
            return div()
                .flex_1()
                .min_h(px(120.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(Radius::PANEL))
                .border_1()
                .border_color(p.line_soft)
                .bg(p.chrome)
                .text_size(px(TypeScale::SM))
                .text_color(p.text_faint)
                .child(SharedString::from(self.text("redis.inspector.pickEntry")))
                .into_any_element();
        };
        let editable = target.is_some();
        let tag = content_tag(&self.selected, idx, &raw);
        let viewer = self.ensure_entry_viewer(&tag, &label, &raw, target, window, cx);
        div()
            .id("redis-entry-inspector")
            .test_support()
            .flex_1()
            .min_h(px(200.))
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(kit::icon(IconName::Braces, 13., p.text_faint))
                    .child(div().flex_none().text_size(px(TypeScale::XS)).font_weight(FontWeight::SEMIBOLD).text_color(p.text_muted).child(SharedString::from(self.text("redis.inspector.title"))))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_size(px(TypeScale::XS))
                            .text_color(p.accent_text)
                            .child(SharedString::from(label)),
                    )
                    .when(editable, |this| {
                        this.child(div().flex_none().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(SharedString::from(self.text("redis.inspector.editHint"))))
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .p(px(8.))
                    .rounded(px(Radius::PANEL))
                    .border_1()
                    .border_color(p.line)
                    .bg(p.surface)
                    .child(viewer),
            )
            .into_any_element()
    }

    fn ensure_string_viewer(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) -> Entity<FormatViewer> {
        let key = self.selected.clone();
        if self.string_viewer.is_none() || self.viewer_key != key {
            let spec = ViewerSpec {
                text: text.to_owned(),
                column_hint: Some(key.clone()),
                type_hint: Some("string".into()),
                bytes: None,
                truncation: TruncationState::Full,
                mode: EditMode::Editable,
                locale: self.locale,
            };
            let viewer = cx.new(|cx| FormatViewer::new(spec, window, cx));
            let sub = cx.subscribe(&viewer, |this, _, event, cx| {
                if let FormatViewerEvent::Commit(value) = event {
                    if this.selected.is_empty() {
                        return;
                    }
                    let value = value.clone();
                    this.stage_mutation(RedisMutation::SetString { key: this.selected.clone(), value });
                    cx.notify();
                }
            });
            self.string_sub = Some(sub);
            self.string_viewer = Some(viewer);
            self.viewer_key = key;
            self.reload_viewer = false;
        } else if self.reload_viewer {
            if let Some(viewer) = &self.string_viewer {
                // 没改过，或者改过的草稿已经写进 Redis（读回来和草稿一致）：都换成最新值，「应用」回到干净状态。
                let (draft, source) = (viewer.read(cx).draft().to_owned(), viewer.read(cx).source().to_owned());
                if draft == source || draft == text {
                    viewer.update(cx, |viewer, cx| viewer.replace_full(text.to_owned(), None, window, cx));
                }
            }
            self.reload_viewer = false;
        }
        self.string_viewer.clone().expect("string viewer")
    }

    /// 集合条目级结构化查看器（自动识别 JSON / XML / YAML / Hex）。Hash 字段值与 List 元素可直接改，
    /// 「应用」后走与其它写操作相同的确认；其余条目只读。
    fn ensure_entry_viewer(&mut self, tag: &str, title_hint: &str, text: &str, target: Option<EntryTarget>, window: &mut Window, cx: &mut Context<Self>) -> Entity<FormatViewer> {
        if self.entry_viewer.is_none() || self.entry_viewer_tag != tag || self.entry_target != target {
            let spec = ViewerSpec {
                text: text.to_owned(),
                column_hint: Some(title_hint.to_owned()),
                type_hint: Some("text".into()),
                bytes: None,
                truncation: TruncationState::Full,
                mode: if target.is_some() { EditMode::Editable } else { EditMode::ReadOnly },
                locale: self.locale,
            };
            let viewer = cx.new(|cx| FormatViewer::new(spec, window, cx));
            let sub = cx.subscribe(&viewer, |this, _, event, cx| {
                let FormatViewerEvent::Commit(value) = event else { return };
                let key = this.selected.clone();
                let mutation = match this.entry_target.clone() {
                    Some(EntryTarget::Hash(field)) => RedisMutation::HashSet { key, field, value: value.clone() },
                    Some(EntryTarget::List(index)) => RedisMutation::ListSet { key, index, value: value.clone() },
                    None => return,
                };
                this.stage_if_key(mutation);
                cx.notify();
            });
            self.entry_viewer = Some(viewer);
            self.entry_viewer_sub = Some(sub);
            self.entry_viewer_tag = tag.to_owned();
            self.entry_target = target;
        }
        self.entry_viewer.clone().expect("entry viewer")
    }

    // ------------------------------------------------------------------ 控制台

    fn render_console(&mut self, p: &Palette, m: &kit::Metrics, entity: &Entity<Self>, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let command = self.redis_input(Field::Command, "redis-command", "redis.tab.commandLabel", "GET key", window, cx);
        let mono = cx.theme().mono_font_family.clone();
        let pad = m.pane_pad();
        let spec = complete::lookup_command(&self.command_input);
        // 状态行常驻（读屏与测试都按它取当前状态）；没有语法提示时它占满整行并靠右，有提示时只留最小宽。
        let status = div()
            .id("redis-status")
            .test_support()
            .role(gpui_kit::Role::Status)
            .aria_label(SharedString::from(self.status_line()))
            .min_h(px(16.))
            .when(spec.is_none() || !self.console_open, |this| this.flex_1().min_w(px(0.)).justify_end())
            .when(spec.is_some() && self.console_open, |this| this.flex_none().min_w(px(16.)).max_w(px(320.)))
            .flex()
            .items_center()
            .gap(px(4.))
            .text_size(px(TypeScale::XS))
            .text_color(p.text_muted)
            .when(!self.note.is_empty(), |this| this.child(kit::icon(IconName::CircleCheck, 12., p.ok)))
            .child(div().min_w(px(0.)).truncate().child(SharedString::from(self.note.clone())));

        let error = if self.error_source == ErrorSource::Command && !self.error.is_empty() && !self.new_open {
            let keys_blocked = self.error.contains("KEYS");
            let actions = div()
                .flex()
                .items_center()
                .gap(px(6.))
                .when(keys_blocked, |this| this.child(action("redis-use-scan", Some(IconName::Search), &self.text("redis.console.useScan"), Kind::Primary, entity).small()))
                .child(icon_btn("redis-error-dismiss", IconName::X, &self.text("redis.error.dismiss"), entity))
                .into_any_element();
            Some(
                div().id("error-state").test_support().mx(px(pad)).mt(px(8.)).child(kit::banner(
                    kit::BannerTone::Danger,
                    IconName::CircleAlert,
                    format!("{} · {}", self.text("redis.error.command.heading"), self.error),
                    Some(actions),
                    p,
                )),
            )
        } else {
            None
        };

        let mut console = div().flex_none().flex().flex_col().border_t_1().border_color(p.line).bg(p.chrome).children(error);

        if !self.console_open {
            return console
                .child(
                    div()
                        .h(px(m.bar_h()))
                        .px(px(pad))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(action("redis-console", Some(IconName::SquareTerminal), &self.text("redis.console.toggle"), Kind::Tool, entity).small())
                        .child(status),
                )
                .child(
                    div()
                        .hidden()
                        .child(kit::text_input("redis-command", &command, self.text("redis.tab.commandLabel")))
                        .child(action("redis-exec", Some(IconName::Play), &self.text("redis.tab.execute"), Kind::Primary, entity)),
                )
                .into_any_element();
        }

        if !self.reply.is_empty() {
            let shown = if self.pretty_reply.is_empty() { self.reply.clone() } else { self.pretty_reply.clone() };
            console = console.child(
                div()
                    .mx(px(pad))
                    .mt(px(8.))
                    .rounded(px(Radius::PANEL))
                    .border_1()
                    .border_color(p.line)
                    .bg(p.sunken)
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .h(px(26.))
                            .pl(px(10.))
                            .pr(px(4.))
                            .flex()
                            .items_center()
                            .gap(px(2.))
                            .child(div().flex_1().text_size(px(TypeScale::XS)).font_weight(FontWeight::SEMIBOLD).text_color(p.text_muted).child(SharedString::from(self.text("redis.console.output"))))
                            .child(icon_btn("redis-console-copy", IconName::Copy, &self.text("redis.console.copyOutput"), entity).xsmall())
                            .child(icon_btn("redis-console-clear", IconName::Trash, &self.text("redis.console.clear"), entity).xsmall()),
                    )
                    .child(
                        div()
                            .id("redis-console-output")
                            .test_support()
                            .max_h(px(160.))
                            .overflow_y_scroll()
                            .px(px(10.))
                            .pb(px(8.))
                            .font_family(mono.clone())
                            .text_size(px(TypeScale::XS))
                            .text_color(p.text)
                            .child(SharedString::from(shown)),
                    ),
            );
        }

        let (_, _, completions) = self.current_completions();
        if !completions.is_empty() {
            let active = self.complete_index.min(completions.len() - 1);
            let mut popup = div()
                .id("redis-complete-list")
                .test_support()
                .mx(px(pad))
                .mt(px(8.))
                .max_h(px(176.))
                .overflow_y_scroll()
                .p(px(4.))
                .rounded(px(Radius::PANEL))
                .bg(p.raised)
                .border_1()
                .border_color(p.line_strong)
                .shadow_sm()
                .flex()
                .flex_col()
                .gap(px(1.));
            for (ix, item) in completions.iter().take(12).enumerate() {
                let pick_id = format!("redis-complete-pick:{ix}");
                let selected = ix == active;
                let (badge_fg, badge_bg, badge_label) = match item.kind {
                    "key" => (p.ok, p.ok_soft, "KEY"),
                    "database" => (p.warn, p.warn_soft, "DB"),
                    _ => (p.accent_text, p.accent_soft, "CMD"),
                };
                popup = popup.child(
                    kit::interactive_states(div().id(SharedString::from(pick_id.clone())), p, false)
                        .test_support()
                        .role(gpui_kit::Role::ListItem)
                        .aria_selected(selected)
                        .h(px(26.))
                        .px(px(8.))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .rounded(px(Radius::CONTROL))
                        .when(selected, |s| s.bg(p.selection))
                        .when(!selected, |s| s.hover(|h| h.bg(p.hover)))
                        .child(div().flex_none().w(px(36.)).child(kit::pill(SharedString::from(badge_label), badge_fg, badge_bg)))
                        .child(div().flex_none().font_family(mono.clone()).text_size(px(TypeScale::SM)).font_weight(FontWeight::MEDIUM).child(SharedString::from(item.label.clone())))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .truncate()
                                .text_size(px(TypeScale::XS))
                                .text_color(p.text_muted)
                                .child(SharedString::from(format!("{} · {}", item.detail, item.summary))),
                        )
                        .on_click(cmd_click(entity, pick_id)),
                );
            }
            popup = popup.child(div().px(px(8.)).pt(px(2.)).text_size(px(TypeScale::XS)).text_color(p.text_faint).child(SharedString::from(self.text("redis.console.completeHint"))));
            console = console.child(popup);
        }

        console = console.child(
            div()
                .px(px(pad))
                .pt(px(8.))
                .pb(px(4.))
                .flex()
                .items_center()
                .gap(px(6.))
                .child(icon_btn("redis-console", IconName::PanelBottomClose, &self.text("redis.console.hide"), entity))
                .child(
                    div().flex_1().min_w(px(0.)).child(
                        kit::text_input("redis-command", &command, self.text("redis.tab.commandLabel"))
                            .small()
                            .cleanable(true)
                            .prefix(kit::input_prefix(IconName::ChevronRight, 13., p.accent_text)),
                    ),
                )
                .child(action("redis-exec", Some(IconName::Play), &self.text("redis.tab.execute"), Kind::Primary, entity).small()),
        );

        console
            .child(
                div()
                    .min_h(px(22.))
                    .px(px(pad))
                    .pb(px(6.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .when_some(spec, |this, spec| {
                        let (fg, bg, label) = if spec.is_write() { (p.warn, p.warn_soft, "WRITE") } else { (p.ok, p.ok_soft, "READ") };
                        this.child(kit::pill(SharedString::from(label), fg, bg)).child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .truncate()
                                .font_family(mono.clone())
                                .text_size(px(TypeScale::XS))
                                .text_color(p.text_muted)
                                .child(SharedString::from(format!("{} — {}", spec.syntax, spec.summary(self.locale)))),
                        )
                    })
                    .child(status),
            )
            .into_any_element()
    }

    // ------------------------------------------------------------------ 对话框

    fn render_confirm_dialog(&self, p: &Palette, m: &kit::Metrics, entity: &Entity<Self>, mono: SharedString) -> AnyElement {
        let destructive = self.confirm_destructive();
        let write_only = matches!(self.confirm, Some(Confirm::Mutation { .. }) | Some(Confirm::Ttl { .. }));
        let lead = if write_only { self.text("redis.confirm.writeLead") } else { self.confirm_prompt.clone() };
        let (icon_name, fg, bg) = if destructive {
            (IconName::TriangleAlert, p.danger, p.danger_soft)
        } else {
            (IconName::ShieldCheck, p.accent_text, p.accent_soft)
        };
        let title = if destructive { self.text("redis.confirm.dangerTitle") } else { self.text("redis.confirm.title") };
        let card = div()
            .id("redis-confirm-dialog")
            .test_support()
            .role(gpui_kit::Role::AlertDialog)
            .aria_label(SharedString::from(self.confirm_prompt.clone()))
            .w(px(480.))
            .max_w_full()
            .p(px(m.card_pad() + 4.))
            .flex()
            .flex_col()
            .gap(px(12.))
            .rounded(px(Radius::PANEL))
            .bg(p.surface)
            .border_1()
            .border_color(p.line)
            .shadow_lg()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(div().flex_none().size(px(30.)).rounded(px(Radius::CONTROL)).bg(bg).flex().items_center().justify_center().child(kit::icon(icon_name, 16., fg)))
                    .child(kit::title(SharedString::from(title), TypeScale::MD, p)),
            )
            .child(div().text_size(px(TypeScale::SM)).text_color(p.text).child(SharedString::from(lead)))
            .child(
                div()
                    .id("redis-confirm-preview")
                    .test_support()
                    .max_h(px(140.))
                    .overflow_y_scroll()
                    .px(px(10.))
                    .py(px(8.))
                    .rounded(px(Radius::CONTROL))
                    .bg(p.sunken)
                    .border_1()
                    .border_color(p.line_soft)
                    .font_family(mono)
                    .text_size(px(TypeScale::SM))
                    .text_color(p.text)
                    .child(SharedString::from(self.confirm_preview())),
            )
            .when(destructive && write_only, |this| {
                this.child(div().text_size(px(TypeScale::XS)).text_color(p.danger).child(SharedString::from(self.text("redis.confirm.irreversible"))))
            })
            .child(
                div()
                    .pt(px(4.))
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(action("redis-cancel", None, &self.text("common.cancel"), Kind::Secondary, entity).small())
                    .child(
                        action(
                            "redis-confirm",
                            Some(if destructive { IconName::Trash } else { IconName::Check }),
                            &self.text("redis.tab.confirmExecute"),
                            if destructive { Kind::Danger } else { Kind::Primary },
                            entity,
                        )
                        .small(),
                    ),
            );
        // 写删确认必须明确选择：点遮罩不取消，也不会穿透到下面的键列表。
        div()
            .id("redis-confirm-layer")
            .test_support()
            .absolute()
            .inset_0()
            .occlude()
            .bg(hsla(0., 0., 0., if p.dark { 0.45 } else { 0.28 }))
            .flex()
            .items_center()
            .justify_center()
            .p(px(24.))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .child(card)
            .into_any_element()
    }

    fn render_new_key_dialog(&mut self, p: &Palette, m: &kit::Metrics, entity: &Entity<Self>, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let ttl_hint = self.text("redis.ttl.hint");
        let key = self.redis_input(Field::NewKey, "redis-new-name", "redis.key.name", "user:1", window, cx);
        let ttl = self.redis_input(Field::NewTtl, "redis-new-ttl", "redis.tab.ttlLabel", &ttl_hint, window, cx);
        let field = self.redis_input(Field::NewField, "redis-new-field", "redis.value.field", "field", window, cx);
        let value = self.redis_input(Field::NewValue, "redis-new-value", "redis.value.value", "value", window, cx);
        let score = self.redis_input(Field::NewScore, "redis-new-score", "redis.value.score", "1", window, cx);
        let stream_id = if self.new_type == "stream" {
            Some(self.redis_input(Field::StreamId, "redis-stream-id", "redis.stream.id", "*", window, cx))
        } else {
            None
        };

        let mut types = Vec::new();
        for (kind, label_key) in [
            ("string", "redis.type.string"),
            ("hash", "redis.type.hash"),
            ("list", "redis.type.list"),
            ("set", "redis.type.set"),
            ("zset", "redis.type.zset"),
            ("stream", "redis.type.stream"),
        ] {
            let id = format!("redis-new-type-{kind}");
            types.push((SharedString::from(id.clone()), SharedString::from(self.text(label_key)), self.new_type == kind, kit::click(cmd_click(entity, id))));
        }
        let value_label = if matches!(self.new_type.as_str(), "set" | "zset") { self.text("redis.value.member") } else { self.text("redis.value.value") };
        let exists = self.error_source == ErrorSource::Command && (self.error.contains("已存在") || self.error.to_ascii_lowercase().contains("exist"));
        let error = (self.error_source == ErrorSource::Command && !self.error.is_empty()).then(|| kit::banner(kit::BannerTone::Danger, IconName::CircleAlert, self.error.clone(), None, p));

        let mut form = div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(kit::form_row(self.text("redis.new.type"), kit::segmented_sized(types, p, m.control_h - 4.), p))
            .child(kit::form_row(self.text("redis.key.name"), kit::text_input("redis-new-name", &key, self.text("redis.key.name")), p))
            .child(kit::form_row(self.text("redis.new.ttlOptional"), kit::text_input("redis-new-ttl", &ttl, self.text("redis.tab.ttlLabel")), p));
        if let Some(stream_id) = stream_id {
            form = form.child(kit::form_row(self.text("redis.stream.id"), kit::text_input("redis-stream-id", &stream_id, self.text("redis.stream.id")), p));
        }
        if matches!(self.new_type.as_str(), "hash" | "stream") {
            form = form.child(kit::form_row(self.text("redis.value.field"), kit::text_input("redis-new-field", &field, self.text("redis.value.field")), p));
        }
        if self.new_type == "zset" {
            form = form.child(kit::form_row(self.text("redis.value.score"), kit::text_input("redis-new-score", &score, self.text("redis.value.score")), p));
        }
        form = form.child(kit::form_row(value_label.clone(), kit::text_input("redis-new-value", &value, value_label), p)).children(error);

        let card = div()
            .id("redis-new-dialog")
            .test_support()
            .role(gpui_kit::Role::Dialog)
            .aria_label(SharedString::from(self.text("redis.action.newKey")))
            .w(px(480.))
            .max_w_full()
            .flex()
            .flex_col()
            .rounded(px(Radius::PANEL))
            .bg(p.surface)
            .border_1()
            .border_color(p.line)
            .shadow_lg()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .px(px(m.card_pad() + 4.))
                    .pt(px(m.card_pad()))
                    .pb(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .border_b_1()
                    .border_color(p.line_soft)
                    .child(kit::icon(IconName::KeyRound, 16., p.accent_text))
                    .child(div().flex_1().child(kit::title(SharedString::from(self.text("redis.action.newKey")), TypeScale::LG, p)))
                    .child(
                        div()
                            .text_size(px(TypeScale::XS))
                            .text_color(p.text_faint)
                            .child(SharedString::from(format!("{} · db{}", self.connection_name, self.database))),
                    )
                    .child(icon_btn("redis-new-close", IconName::X, &self.text("common.cancel"), entity)),
            )
            .child(div().px(px(m.card_pad() + 4.)).py(px(m.card_pad())).child(form))
            .child(
                div()
                    .px(px(m.card_pad() + 4.))
                    .py(px(10.))
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(8.))
                    .border_t_1()
                    .border_color(p.line_soft)
                    .bg(p.chrome)
                    .rounded_b(px(Radius::PANEL))
                    .child(action("redis-new-close", None, &self.text("common.cancel"), Kind::Secondary, entity).small())
                    .when(exists, |this| this.child(action("redis-overwrite", Some(IconName::TriangleAlert), &self.text("redis.new.overwrite"), Kind::DangerSoft, entity).small()))
                    .child(action("redis-create", Some(IconName::Plus), &self.text("redis.new.create"), Kind::Primary, entity).small()),
            );

        let close_entity = entity.clone();
        div()
            .id("redis-new-layer")
            .test_support()
            .absolute()
            .inset_0()
            .occlude()
            .bg(hsla(0., 0., 0., if p.dark { 0.4 } else { 0.22 }))
            .flex()
            .items_start()
            .justify_center()
            .px(px(24.))
            .pt(px(64.))
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                cx.stop_propagation();
                close_entity.update(cx, |pane, cx| pane.command("redis-new-close", cx));
            })
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .child(card)
            .into_any_element()
    }

    fn render_context_menu(&self, p: &Palette, entity: &Entity<Self>) -> Option<AnyElement> {
        let menu = self.context_menu.as_ref()?;
        let mut col = div()
            .id("redis-context-menu")
            .test_support()
            .occlude()
            .min_w(px(184.))
            .p(px(4.))
            .rounded(px(Radius::PANEL))
            .bg(p.raised)
            .border_1()
            .border_color(p.line_strong)
            .shadow_lg()
            .flex()
            .flex_col()
            .gap(px(1.))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation());
        for (ix, item) in menu.items.iter().enumerate() {
            if item.danger && ix > 0 {
                col = col.child(div().my(px(3.)).h(px(1.)).bg(p.line_soft));
            }
            col = col.child(
                kit::interactive_states(div().id(SharedString::from(format!("ctx-{}", item.id))), p, false)
                    .test_support()
                    .role(gpui_kit::Role::MenuItem)
                    .aria_label(SharedString::from(item.label.clone()))
                    .h(px(28.))
                    .px(px(8.))
                    .rounded(px(Radius::CONTROL))
                    .flex()
                    .items_center()
                    .hover(|s| if item.danger { s.bg(p.danger_soft) } else { s.bg(p.hover) })
                    .child(kit::popup_menu_row(Some(item.icon), SharedString::from(item.label.clone()), None, item.danger, p))
                    .on_click(cmd_click(entity, item.id.clone())),
            );
        }
        Some(
            deferred(anchored().position(menu.at).anchor(Anchor::TopLeft).snap_to_window_with_margin(px(8.)).child(col))
                .with_priority(20)
                .into_any_element(),
        )
    }

    // ------------------------------------------------------------------ 输入框

    pub(super) fn redis_input(&mut self, field: Field, _id: &'static str, label_key: &str, placeholder: &str, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        let value = match field {
            Field::Pattern => self.pattern.clone(),
            Field::Ttl => self.ttl_input.clone(),
            Field::Rename => self.rename_to.clone(),
            Field::Command => self.command_input.clone(),
            Field::NewKey => self.new_key.clone(),
            Field::NewTtl => self.new_ttl.clone(),
            Field::NewField => self.new_field.clone(),
            Field::NewValue => self.new_value.clone(),
            Field::NewScore => self.new_score.clone(),
            Field::EntryField => self.entry_field.clone(),
            Field::EntryValue => self.entry_value.clone(),
            Field::EntryMember => self.entry_member.clone(),
            Field::EntryScore => self.entry_score.clone(),
            Field::EntryIndex => self.entry_index.clone(),
            Field::StreamId => self.stream_id.clone(),
            Field::StreamField => self.stream_field.clone(),
            Field::StreamValue => self.stream_value.clone(),
            Field::HashQuery => self.hash_query.clone(),
            Field::SetQuery => self.set_query.clone(),
        };
        let _ = label_key;
        self.fields.bind_enter(
            field,
            &value,
            FieldSpec { placeholder, masked: false },
            window,
            cx,
            |this: &mut Self, field, value, _| {
                if this.confirm.is_some() && matches!(field, Field::Rename | Field::Command | Field::Ttl) {
                    return;
                }
                this.focused = Some(field);
                match field {
                    Field::Pattern => this.pattern = value,
                    Field::Ttl => this.ttl_input = value,
                    Field::Rename => this.rename_to = value,
                    Field::Command => {
                        this.complete_open = !value.trim().is_empty();
                        this.complete_index = 0;
                        this.command_input = value;
                        this.history_at = None;
                    }
                    Field::NewKey => this.new_key = value,
                    Field::NewTtl => this.new_ttl = value,
                    Field::NewField => this.new_field = value,
                    Field::NewValue => this.new_value = value,
                    Field::NewScore => this.new_score = value,
                    Field::EntryField => this.entry_field = value,
                    Field::EntryValue => this.entry_value = value,
                    Field::EntryMember => this.entry_member = value,
                    Field::EntryScore => this.entry_score = value,
                    Field::EntryIndex => this.entry_index = value,
                    Field::StreamId => this.stream_id = value,
                    Field::StreamField => this.stream_field = value,
                    Field::StreamValue => this.stream_value = value,
                    Field::HashQuery => this.hash_query = value,
                    Field::SetQuery => this.set_query = value,
                }
            },
            Some(|this: &mut Self, field, cx: &mut Context<Self>| {
                let kind = this.detail.as_ref().map(|detail| detail.key_type.clone()).unwrap_or_default();
                let id: String = match field {
                    Field::Pattern => "redis-filter".into(),
                    Field::Ttl => "redis-ttl".into(),
                    Field::Rename => "redis-rename".into(),
                    Field::Command => "redis-exec".into(),
                    Field::NewKey | Field::NewTtl | Field::NewField | Field::NewValue | Field::NewScore => "redis-create".into(),
                    Field::StreamId if this.new_open => "redis-create".into(),
                    Field::EntryField | Field::EntryValue | Field::EntryIndex => match (kind.as_str(), this.entry_index.trim().parse::<usize>()) {
                        // 正在编辑某个下标时，回车写回原位置，而不是再追加一个元素。
                        ("list", Ok(index)) => format!("redis-list-set:{index}"),
                        ("list", Err(_)) => "redis-list-rpush".into(),
                        _ => "redis-hash-add".into(),
                    },
                    Field::EntryMember | Field::EntryScore => {
                        if kind == "set" {
                            "redis-set-add".into()
                        } else {
                            "redis-zadd".into()
                        }
                    }
                    Field::StreamId | Field::StreamField | Field::StreamValue => "redis-xadd".into(),
                    Field::HashQuery | Field::SetQuery => "redis-entry-filter".into(),
                };
                this.command(&id, cx);
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirm_preview_and_tone_follow_the_staged_action() {
        let dir = std::env::temp_dir().join(format!("rillsql-redis-view-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let services = Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
        let mut pane = RedisPane::new(services);
        pane.confirm = Some(Confirm::Delete { key: "user 1".into() });
        assert_eq!(pane.confirm_preview(), "DEL \"user 1\"");
        assert!(pane.confirm_destructive(), "删键用危险色");
        pane.confirm = Some(Confirm::Ttl { key: "a".into(), seconds: Some(60) });
        assert_eq!(pane.confirm_preview(), "EXPIRE a 60");
        assert!(!pane.confirm_destructive(), "设 TTL 不是删除");
        pane.confirm = Some(Confirm::Exec { command: "SET".into(), args: vec!["a".into(), "1".into()], line: " SET a 1 ".into() });
        assert_eq!(pane.confirm_preview(), "SET a 1");
        assert!(!pane.confirm_destructive());
        pane.confirm = Some(Confirm::Exec { command: "FLUSHDB".into(), args: Vec::new(), line: "FLUSHDB".into() });
        assert!(pane.confirm_destructive());
        pane.confirm = Some(Confirm::Mutation { mutation: RedisMutation::HashDel { key: "h".into(), fields: vec!["f".into()] }, key: "h".into() });
        assert!(pane.confirm_destructive());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn entry_viewer_tag_changes_with_content_not_only_length() {
        assert_ne!(content_tag("k", 0, "abc"), content_tag("k", 0, "abd"), "等长但内容不同的值必须重建查看器");
        assert_eq!(content_tag("k", 1, "x"), content_tag("k", 1, "x"));
    }
}
