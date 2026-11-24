//! Redis 键浏览器的界面层。
//!
//! 布局：左侧键空间（搜索与图标按钮一行、类型分段与视图切换一行、键列表、SCAN 页脚），右侧键详情
//! （两行头部：键名与类型、元信息；集合类型是左表右值的可拖动分栏），底部命令控制台。
//! 写删确认与新建键是盖在面板上的对话框：确认就近出现在视线中央，不再落到底部控制台里。

use super::*;
use crate::ui::kit::{ChipTone, ControlSize, IconSize, ScrollAreaExt as _, Space};
use crate::ui::read_table::ReadColumn;
use gpui_kit::{hsla, relative};

/// 值区里条目表的默认宽度占比、最窄宽度，以及右栏查看器至少留的宽度。
const VALUE_SPLIT_RATIO: f32 = 0.42;
const VALUE_TABLE_MIN_W: f32 = 240.;
const VALUE_VIEWER_MIN_W: f32 = 280.;
/// 详情区窄于这个宽度时，条目表与查看器改为上下分栏；头部次要操作全部收进「更多」。
const VALUE_STACK_W: f32 = 640.;

#[derive(Clone, Copy)]
pub(super) enum Kind {
    Primary,
    Secondary,
    Tool,
    Danger,
    DangerSoft,
}

/// 类型 chip 的色调：只区分类型，低饱和，比键名弱。
pub(super) fn type_tone(kind: &str) -> ChipTone {
    match kind {
        "string" => ChipTone::Info,
        "hash" => ChipTone::Ok,
        "list" => ChipTone::Warn,
        "set" => ChipTone::Accent,
        "zset" => ChipTone::Danger,
        _ => ChipTone::Neutral,
    }
}

/// 类型的显示名（Hash、String…），未知类型原样显示。
fn type_display(kind: &str, pane: &RedisPane) -> String {
    match kind {
        "string" => pane.text("redis.type.string"),
        "hash" => pane.text("redis.type.hash"),
        "list" => pane.text("redis.type.list"),
        "set" => pane.text("redis.type.set"),
        "zset" => pane.text("redis.type.zset"),
        "stream" => pane.text("redis.type.stream"),
        other => other.to_owned(),
    }
}

/// 键行的类型圆点：颜色与顶部类型筛选的圆点一致，类型全名在无障碍标签和悬停里。
const TYPE_DOT: f32 = 8.;
/// 键列表行右侧留白：让开浮动滚动条拇指（ADR-8 的 gutter），计数和 TTL 不被压住。
const KEY_ROW_PR: f32 = kit::SCROLLBAR_GUTTER + Space::SM;

fn type_dot(kind: &str, p: &Palette) -> AnyElement {
    let (color, _) = type_tone(kind).colors(p);
    div().flex_none().size(px(TYPE_DOT)).rounded_full().bg(color).into_any_element()
}

/// 键行、模板行与详情头的类型标签：固定宽度的短名（STR / HASH / LIST…），颜色与类型筛选的圆点一致，
/// 一列里上下对齐，比圆点更容易一眼认出类型。
pub(super) fn type_tag(kind: &str, m: &kit::Metrics, p: &Palette) -> AnyElement {
    div()
        .id("redis-type-tag")
        .test_support()
        .flex_none()
        .child(kit::type_badge(SharedString::from(tables::type_short_label(kind)), type_tone(kind), m, p))
        .into_any_element()
}

pub(super) fn cmd_click(entity: &Entity<RedisPane>, id: impl Into<String>) -> impl Fn(&ClickEvent, &mut Window, &mut gpui_kit::App) + 'static {
    let entity = entity.clone();
    let id: String = id.into();
    move |_, _, cx| {
        let id = id.clone();
        entity.update(cx, |pane, cx| pane.command(&id, cx));
    }
}

pub(super) fn action(id: impl Into<String>, icon: Option<IconName>, label: &str, kind: Kind, entity: &Entity<RedisPane>) -> kit::ActionButton {
    let id: String = id.into();
    let click = cmd_click(entity, id.clone());
    let label = SharedString::from(label.to_owned());
    let id = SharedString::from(id);
    match kind {
        Kind::Primary => kit::primary_button(id, icon, label, click),
        Kind::Secondary => kit::secondary_button(id, icon, label, click),
        Kind::Tool => kit::tool_button(id, icon, label, click),
        Kind::Danger => kit::danger_button(id, icon, label, click),
        Kind::DangerSoft => kit::danger_soft_button(id, icon, label, click),
    }
}

pub(super) fn icon_btn(id: impl Into<String>, icon: IconName, label: &str, entity: &Entity<RedisPane>) -> kit::IconButton {
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

/// 条目表一行：条目下标与各列文字。
pub(super) struct EntryLine {
    pub(super) index: usize,
    cells: Vec<SharedString>,
    /// 排序键（数值列按数值比较）。
    sort: (f64, String),
}

/// 选中条目交给右栏查看器的内容：下标、标题、原始值、能否写回。
type InspectedEntry = (usize, String, String, Option<EntryTarget>);

/// 条目表首列（字段、下标、分数、ID）按内容测量的上下限，与测量时最多看的行数。
const ENTRY_FIRST_MAX_W: f32 = 360.;
const ENTRY_FIRST_MIN_W: f32 = 56.;
const ENTRY_MEASURE_ROWS: usize = 300;
/// 值列至少留的宽度（再窄就横向滚动）。
const ENTRY_VALUE_MIN_W: f32 = 160.;
/// 条目表除了数据列之外占的宽度：行头、竖向滚动条让位与边框。
const ENTRY_TABLE_CHROME_W: f32 = kit::SCROLLBAR_GUTTER + Space::XL + Space::XS;

/// 分隔线宽度。
const DIVIDER_W: f32 = 1.;
/// 等宽字体的字宽估计（相对字号）：只用来判断命令行是否放得下，放不下就不叠加高亮层。
const MONO_ADVANCE: f32 = 0.62;

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
            Some(Confirm::Batch { preview, template, .. }) => {
                let mut lines = vec![preview.command.clone(), format!("# {template}")];
                lines.extend(preview.samples.iter().map(|key| format!("  {}", quote_arg(key))));
                if preview.count > preview.samples.len() {
                    lines.push(format!("  … +{}", preview.count - preview.samples.len()));
                }
                lines.join("\n")
            }
            Some(Confirm::SlowlogReset { node }) => if node.is_empty() { "SLOWLOG RESET".to_owned() } else { format!("SLOWLOG RESET  # {node}") },
            Some(Confirm::ClientKill { id, addr, .. }) => format!("CLIENT KILL ID {id}  # {addr}"),
            Some(Confirm::Publish { channel, message }) => format!("PUBLISH {} {}", quote_arg(channel), quote_arg(message)),
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
            Some(Confirm::Batch { op: BatchOp::Unlink, .. }) | Some(Confirm::SlowlogReset { .. }) | Some(Confirm::ClientKill { .. }) => true,
            _ => false,
        }
    }

    pub(super) fn count_label(&self, detail: &RedisKeyDetail) -> String {
        let (key, n) = match &detail.value {
            RedisValue::String { text } => ("redis.count.string", text.chars().count()),
            RedisValue::Hash { entries } => ("redis.count.hash", entries.len()),
            RedisValue::List { items } => ("redis.count.list", items.len()),
            RedisValue::Set { items } => ("redis.count.set", items.len()),
            RedisValue::ZSet { entries } => ("redis.count.zset", entries.len()),
            RedisValue::Stream { messages } => ("redis.count.stream", messages.len()),
            RedisValue::Json { text } => ("redis.count.string", text.chars().count()),
            RedisValue::None {} => return String::new(),
        };
        let base = i18n::fill(&self.text(key), &[("n", &n.to_string())]);
        // 分页读取的集合：已读条数 / 总数。
        match detail.total {
            Some(total) if detail.next.is_some() || (total as usize) > n && !matches!(detail.value, RedisValue::String { .. } | RedisValue::Json { .. }) => {
                format!("{base} / {}", i18n::fill(&self.text("redis.count.total"), &[("n", &total.to_string())]))
            }
            _ => base,
        }
    }
}

impl Render for RedisPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::of(cx);
        let m = kit::Metrics::of(cx);
        let entity = cx.entity();
        // 面板宽度：用上一帧量到的实际宽度（不含应用侧栏）；第一帧还没量到时退回窗口宽度。
        let viewport_w = self.pane_w.get().unwrap_or_else(|| f32::from(window.viewport_size().width));
        let viewport_h = f32::from(window.viewport_size().height);
        self.viewport_w = viewport_w;
        let narrow = viewport_w < kit::NARROW_PAGE_WIDTH;
        let show_keys = self.keys_visible(viewport_w);
        let keys_page = self.page == Page::Keys;
        let top_bar = self.render_top_bar(&p, &m, &entity);
        let diag_page = if keys_page { None } else { Some(stack_frame(|| self.render_diag_page(&p, &m, &entity, window, cx))) };

        let sidebar = if keys_page && (!narrow || show_keys) {
            Some(stack_frame(|| self.render_sidebar(&p, &m, &entity, narrow, viewport_w, window, cx)))
        } else {
            None
        };
        let max_side = (viewport_w * 0.5).clamp(SIDEBAR_MIN_W, SIDEBAR_MAX_W);
        let detail_w = if narrow { viewport_w } else { (viewport_w - self.sidebar_w.clamp(SIDEBAR_MIN_W, max_side)).max(0.) };
        let _ = viewport_h;
        let detail = if keys_page && (!narrow || !show_keys) {
            Some(stack_frame(|| self.render_detail(&p, &m, &entity, detail_w, window, cx)))
        } else {
            None
        };
        let command_error = self.render_command_error(&p, &m, &entity);
        let new_layer = if self.new_open { Some(stack_frame(|| self.render_new_key_dialog(&p, &m, &entity, window, cx))) } else { None };
        let mono = cx.theme().mono_font_family.clone();
        let confirm_layer = if self.confirm.is_some() { Some(stack_frame(|| self.render_confirm_dialog(&p, &m, &entity, mono, window, cx))) } else { None };
        let menu_layer = self.render_context_menu(&p, &entity);

        if let Some(field) = self.focus_request.take() {
            if self.fields.contains(field) {
                self.fields.focus(field, window, cx);
            }
        }

        stack_frame(|| {
            let mut body = div().flex_1().min_h(px(0.)).min_w(px(0.)).flex();
            let root = div()
                .id("redis-pane")
                .test_support()
                .relative()
                .size_full()
                .flex()
                .flex_col()
                .bg(p.surface)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        let menu = this.context_menu.take().is_some();
                        let db = std::mem::take(&mut this.db_picker_open);
                        let types = std::mem::take(&mut this.type_picker_open);
                        if menu || db || types {
                            cx.notify();
                        }
                    }),
                )
                .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                    if let Some((start_x, start_w)) = this.sidebar_drag_start {
                        let delta = f32::from(event.position.x) - start_x;
                        let max_w = (this.viewport_w * 0.5).clamp(SIDEBAR_MIN_W, SIDEBAR_MAX_W);
                        this.sidebar_w = (start_w + delta).clamp(SIDEBAR_MIN_W, max_w);
                        cx.notify();
                    }
                    if let Some((start_x, start_w)) = this.value_drag_start {
                        let delta = f32::from(event.position.x) - start_x;
                        let max = (detail_w - VALUE_VIEWER_MIN_W).max(VALUE_TABLE_MIN_W);
                        let next = (start_w + delta).clamp(VALUE_TABLE_MIN_W, max);
                        this.prefs.value_split = next.round() as u32;
                        cx.notify();
                    }
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseUpEvent, _, cx| {
                        if this.sidebar_drag_start.take().is_some() {
                            cx.notify();
                        }
                        // 分隔条位置写进 Redis 偏好（按连接保存）。
                        if this.value_drag_start.take().is_some() {
                            this.persist_prefs(cx);
                            cx.notify();
                        }
                    }),
                )
                .on_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, _, cx| this.key_down(event, cx)));

            if let Some(page) = diag_page {
                body = body.flex_col().child(page);
            } else if narrow {
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
                body = body
                    .flex_col()
                    .child(strip)
                    .child(div().id("redis-narrow-body").flex_1().min_h(px(0.)).flex().children(content));
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
                body = body
                    .children(sidebar)
                    .child(splitter)
                    .child(div().flex_1().min_w(px(0.)).h_full().flex().flex_col().children(detail));
            }
            // 量面板宽度：宽度变了就重画一次，分栏比例、窄屏切换都按面板自己的宽度算。
            let probe = {
                let cell = Rc::clone(&self.pane_w);
                let owner = entity.downgrade();
                gpui_kit::canvas(
                    move |area, _, cx| {
                        let width = f32::from(area.size.width);
                        if cell.get().is_none_or(|seen| (seen - width).abs() > 0.5) {
                            cell.set(Some(width));
                            owner.update(cx, |_, cx| cx.notify()).ok();
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full()
            };
            root.child(probe).child(top_bar).children(command_error).child(body).children(new_layer).children(confirm_layer).children(menu_layer).into_any_element()
        })
    }
}

impl RedisPane {
    pub(super) fn key_down(&mut self, event: &gpui_kit::KeyDownEvent, cx: &mut Context<Self>) {
        let key = match event.keystroke.key.as_str() {
            "arrowup" => "up",
            "arrowdown" => "down",
            other => other,
        };
        if key == "escape" {
            // 由近及远逐层收起：浮层 → 确认 → 对话框 → 表单 → 编辑条。
            let handled = if self.context_menu.is_some() || self.complete_open || self.db_picker_open || self.type_picker_open {
                self.context_menu = None;
                self.complete_open = false;
                self.db_picker_open = false;
                self.type_picker_open = false;
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
            } else if self.ttl_open || self.rename_open || self.prefs_open || self.batch_ttl_for.is_some() {
                self.ttl_open = false;
                self.rename_open = false;
                self.prefs_open = false;
                self.batch_ttl_for = None;
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
        let hint = self.text("redis.tab.patternHint");
        let pattern = self.redis_input(Field::Pattern, "redis-pattern", "redis.tab.patternLabel", &hint, window, cx);
        let max_side = (viewport_w * 0.5).clamp(SIDEBAR_MIN_W, SIDEBAR_MAX_W);
        let side_w = if narrow { viewport_w } else { self.sidebar_w.clamp(SIDEBAR_MIN_W, max_side) };
        let pad = m.pane_pad();

        // 第一行：搜索框 + 图标按钮（不折行）。模式改动但未执行 SCAN 时高亮过滤按钮提示回车/点击执行。
        let pattern_dirty = self.pattern.trim() != self.scanned_pattern;
        let filter_btn = icon_btn("redis-filter", IconName::CornerDownLeft, &self.text("redis.tab.filter"), entity).selected(pattern_dirty);
        let search = div()
            .id("redis-tools-search")
            .test_support()
            .flex()
            .flex_nowrap()
            .items_center()
            .gap(px(Space::XXS))
            .child(
                div().flex_1().min_w(px(0.)).child(
                    kit::text_input("redis-pattern", &pattern, self.text("redis.tab.patternLabel"))
                        .small()
                        .cleanable(true)
                        .prefix(kit::input_prefix(IconName::Search, IconSize::INLINE, p.text_faint)),
                ),
            )
            .child(filter_btn)
            .child(icon_btn("redis-new-key", IconName::Plus, &self.text("redis.action.newKey"), entity).primary());
        let prefs = if self.prefs_open { Some(self.render_prefs_panel(p, entity, window, cx)) } else { None };
        let batch_ttl = if self.batch_ttl_for.is_some() { Some(self.render_batch_ttl_bar(p, entity, window, cx)) } else { None };

        let header = div()
            .flex_none()
            .px(px(pad))
            .pt(px(Space::MD))
            .pb(px(Space::MD))
            .flex()
            .flex_col()
            .gap(px(Space::MD))
            .border_b_1()
            .border_color(p.line)
            .child(search)
            .children(prefs)
            .child(self.render_type_bar(p, entity))
            .children(self.render_batch_banner(p, entity))
            .children(batch_ttl);

        let rows = self.key_rows();
        let templates = if self.view_mode == ViewMode::Template { Some(self.templates()) } else { None };
        let list = stack_frame(|| self.render_key_list(p, m, entity, side_w, rows, templates, window, cx));
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

    /// 键空间偏好：分隔符（空格分开，可多个）与扫描上限。按连接保存。
    fn render_prefs_panel(&mut self, p: &Palette, entity: &Entity<Self>, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let seps = self.redis_input(Field::Separators, "redis-prefs-seps", "redis.prefs.separators", ": |", window, cx);
        let cap = self.redis_input(Field::ScanCap, "redis-prefs-cap", "redis.prefs.cap", "200000", window, cx);
        div()
            .id("redis-prefs")
            .test_support()
            .p(px(8.))
            .rounded(px(Radius::PANEL))
            .bg(p.sunken)
            .border_1()
            .border_color(p.line_soft)
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(div().text_size(px(TypeScale::XS)).font_weight(FontWeight::SEMIBOLD).text_color(p.text_muted).child(SharedString::from(self.text("redis.prefs.title"))))
            .child(kit::form_row(self.text("redis.prefs.separators"), kit::text_input("redis-prefs-seps", &seps, self.text("redis.prefs.separators")).small(), p))
            .child(kit::form_row(self.text("redis.prefs.cap"), kit::text_input("redis-prefs-cap", &cap, self.text("redis.prefs.cap")).small(), p))
            .child(div().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(SharedString::from(self.text("redis.prefs.hint"))))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(6.))
                    .when(!self.prefs.rules.is_empty(), |this| {
                        this.child(action("redis-tpl-rules-reset", None, &i18n::fill(&self.text("redis.prefs.resetRules"), &[("n", &self.prefs.rules.len().to_string())]), Kind::Tool, entity).small())
                    })
                    .child(action("redis-prefs-close", None, &self.text("common.cancel"), Kind::Secondary, entity).small())
                    .child(action("redis-prefs-save", Some(IconName::Check), &self.text("redis.prefs.save"), Kind::Primary, entity).small()),
            )
            .into_any_element()
    }

    /// 模板批量 TTL：输入秒数（留空 = 移除过期）后进确认。
    fn render_batch_ttl_bar(&mut self, p: &Palette, entity: &Entity<Self>, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let hint = self.text("redis.ttl.hint");
        let ttl = self.redis_input(Field::BatchTtl, "redis-batch-ttl", "redis.tab.ttlLabel", &hint, window, cx);
        let template = self.batch_ttl_for.clone().unwrap_or_default();
        div()
            .id("redis-batch-ttl-bar")
            .test_support()
            .p(px(8.))
            .rounded(px(Radius::PANEL))
            .bg(p.sunken)
            .border_1()
            .border_color(p.line_soft)
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(div().truncate().text_size(px(TypeScale::XS)).text_color(p.text_muted).child(SharedString::from(i18n::fill(&self.text("redis.tpl.batchTtlFor"), &[("template", &template)]))))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(div().flex_1().min_w(px(0.)).child(kit::text_input("redis-batch-ttl", &ttl, self.text("redis.tab.ttlLabel")).small()))
                    .child(action("redis-batch-ttl-apply", Some(IconName::Check), &self.text("redis.tab.setTtl"), Kind::Primary, entity).small())
                    .child(icon_btn("redis-batch-ttl-close", IconName::X, &self.text("common.cancel"), entity)),
            )
            .into_any_element()
    }

    /// 批量操作进度：已处理 / 总数、失败数，进行中可取消，完成后可关闭。
    fn render_batch_banner(&self, p: &Palette, entity: &Entity<Self>) -> Option<AnyElement> {
        let batch = self.batch.as_ref()?;
        let progress = batch.progress;
        let word = match batch.op {
            BatchOp::Unlink => self.text("redis.tpl.batchDelete"),
            BatchOp::Ttl(_) => self.text("redis.tpl.batchTtl"),
        };
        let state = if !progress.finished {
            self.text("redis.batch.running")
        } else if progress.cancelled {
            self.text("redis.batch.cancelled")
        } else {
            self.text("redis.batch.done")
        };
        let text = i18n::fill(
            &self.text("redis.batch.progress"),
            &[("op", &word), ("state", &state), ("done", &progress.done.to_string()), ("total", &progress.total.to_string()), ("failed", &progress.failed.to_string())],
        );
        let ratio = if progress.total == 0 { 0. } else { (progress.done + progress.failed) as f32 / progress.total as f32 };
        let button = if progress.finished {
            icon_btn("redis-batch-dismiss", IconName::X, &self.text("redis.error.dismiss"), entity).into_any_element()
        } else {
            action("redis-batch-cancel", Some(IconName::CircleStop), &self.text("common.cancel"), Kind::Secondary, entity).small().into_any_element()
        };
        Some(
            div()
                .id("redis-batch")
                .test_support()
                .role(gpui_kit::Role::Status)
                .aria_label(SharedString::from(format!("{text} · {}", batch.template)))
                .tooltip_text(SharedString::from(batch.template.clone()))
                .p(px(8.))
                .rounded(px(Radius::PANEL))
                .bg(if progress.failed > 0 { p.warn_soft } else { p.sunken })
                .flex()
                .items_center()
                .gap(px(8.))
                .child(div().flex_1().min_w(px(0.)).child(super::pages::progress_bar(&text, ratio, !progress.finished, p)))
                .child(button)
                .into_any_element(),
        )
    }

    pub(super) fn render_db_button(&self, p: &Palette, m: &kit::Metrics, entity: &Entity<Self>) -> AnyElement {
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
            .h(px(m.chip_h()))
            .px(px(Space::SM))
            .flex()
            .items_center()
            .gap(px(Space::XS))
            .rounded(px(Radius::TAG))
            // 在连接区块里：不再单独画边框，展开时用强调底色，平时悬停变亮。
            .when(open, |this| this.bg(p.accent_soft).text_color(p.accent_text))
            .when(!open, |this| this.text_color(p.text).hover(|s| s.bg(p.hover)))
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

    /// 第二行：可横向滚动的类型分段（只列出已读到的类型，加「全部」，每项带计数）+ 视图切换。不折行。
    /// 每种类型在已读到的键里的个数（按固定顺序，含 0）。
    fn type_counts(&self) -> Vec<(&'static str, &'static str, usize)> {
        let mut counts: Vec<(&'static str, &'static str, usize)> = vec![
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
        counts
    }

    /// 键列表的第二行：类型下拉（选中立即生效）、视图切换、刷新、偏好、新建键。一行放下，不折行。
    fn render_type_bar(&self, p: &Palette, entity: &Entity<Self>) -> AnyElement {
        let open = self.type_picker_open;
        let counts = self.type_counts();
        let (label, dot, count) = match counts.iter().find(|(kind, _, _)| *kind == self.type_filter) {
            Some((kind, key, n)) => (self.text(key), Some(type_tone(kind).colors(p).0), *n),
            None => (self.text("redis.type.all"), None, self.keys.len()),
        };
        let filtered = !self.type_filter.is_empty();
        let button = kit::interactive_states(div().id("redis-type-toggle"), p, false)
            .test_support()
            .role(gpui_kit::Role::Button)
            .aria_label(SharedString::from(format!("{} {label}", self.text("redis.type.filter"))))
            .aria_expanded(open)
            .tooltip_text(SharedString::from(self.text("redis.type.filter")))
            .h(px(24.))
            .px(px(Space::SM))
            .flex()
            .items_center()
            .gap(px(Space::XS))
            .rounded(px(Radius::CONTROL))
            .border_1()
            .when(open || filtered, |this| this.border_color(p.accent.opacity(0.5)).bg(p.accent_soft).text_color(p.accent_text))
            .when(!open && !filtered, |this| {
                this.border_color(p.line).bg(if p.dark { p.raised } else { p.surface }).text_color(p.text).hover(|s| s.bg(p.hover).border_color(p.line_strong))
            })
            .text_size(px(TypeScale::XS))
            .font_weight(FontWeight::MEDIUM)
            .when_some(dot, |this, color| this.child(div().size(px(TYPE_DOT)).rounded_full().bg(color)))
            .when(dot.is_none(), |this| this.child(kit::icon(IconName::ListFilter, 12., p.text_muted)))
            .child(SharedString::from(label))
            .child(div().font_features(kit::tabular_figures()).text_color(if filtered { p.accent_text } else { p.text_faint }).child(SharedString::from(count.to_string())))
            .child(kit::icon(IconName::ChevronDown, 11., if open || filtered { p.accent_text } else { p.text_muted }))
            .on_click(cmd_click(entity, "redis-type-toggle"));
        let picker = div()
            .relative()
            .flex_none()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(button)
            .when(open, |this| {
                this.child(
                    div().absolute().top_full().left_0().child(
                        deferred(anchored().anchor(Anchor::TopLeft).snap_to_window_with_margin(px(8.)).child(self.type_picker_panel(p, entity, &counts))).with_priority(15),
                    ),
                )
            });
        let has_active_filter = filtered || !self.scanned_pattern.is_empty();
        div()
            .id("redis-tools-types")
            .test_support()
            .flex()
            .flex_nowrap()
            .items_center()
            .gap(px(Space::XXS))
            .child(picker)
            .when(has_active_filter, |this| {
                this.child(icon_btn("redis-clear-filter", IconName::X, &self.text("redis.noMatch.clear"), entity).xsmall())
            })
            .child(div().flex_1())
            .child(self.render_view_toggle(p, entity))
            .child(div().flex_none().w(px(DIVIDER_W)).h(px(IconSize::INLINE)).mx(px(Space::XXS)).bg(p.line))
            .child(icon_btn("redis-refresh", IconName::RefreshCw, &self.text("redis.tab.refresh"), entity).xsmall())
            .child(icon_btn("redis-prefs-toggle", IconName::Settings2, &self.text("redis.prefs.title"), entity).xsmall().selected(self.prefs_open))
            .into_any_element()
    }

    /// 类型下拉面板：每行圆点 + 类型名 + 个数，当前项打勾；没有键的类型变淡但仍可选。
    fn type_picker_panel(&self, p: &Palette, entity: &Entity<Self>, counts: &[(&'static str, &'static str, usize)]) -> AnyElement {
        let row = |id: String, dot: Option<gpui_kit::Hsla>, label: String, n: usize, active: bool| {
            kit::interactive_states(div().id(SharedString::from(id.clone())), p, false)
                .test_support()
                .role(gpui_kit::Role::MenuItem)
                .aria_label(SharedString::from(label.clone()))
                .aria_selected(active)
                .h(px(28.))
                .px(px(Space::SM))
                .flex()
                .items_center()
                .gap(px(Space::SM))
                .rounded(px(Radius::CONTROL))
                .text_size(px(TypeScale::SM))
                .text_color(if n == 0 && !active { p.text_faint } else { p.text })
                .when(active, |this| this.bg(p.selection))
                .when(!active, |this| this.hover(|s| s.bg(p.hover)))
                .child(div().flex_none().w(px(TYPE_DOT)).h(px(TYPE_DOT)).rounded_full().when_some(dot, |this, color| this.bg(color)))
                .child(div().flex_1().child(SharedString::from(label)))
                .child(div().flex_none().text_size(px(TypeScale::XS)).font_features(kit::tabular_figures()).text_color(p.text_faint).child(SharedString::from(n.to_string())))
                .child(div().flex_none().w(px(12.)).when(active, |this| this.child(kit::icon(IconName::Check, 12., p.accent_text))))
                .on_click(cmd_click(entity, id))
        };
        let mut panel = div()
            .id("redis-type-picker")
            .test_support()
            .occlude()
            .mt(px(4.))
            .w(px(200.))
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
            .child(row("redis-type-all".into(), None, self.text("redis.type.all"), self.keys.len(), self.type_filter.is_empty()));
        for (kind, key, n) in counts {
            panel = panel.child(row(format!("redis-type-{kind}"), Some(type_tone(kind).colors(p).0), self.text(key), *n, self.type_filter == *kind));
        }
        panel.into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_key_list(
        &mut self,
        p: &Palette,
        m: &kit::Metrics,
        entity: &Entity<Self>,
        side_w: f32,
        rows: Rc<Vec<KeyRow>>,
        templates: Option<Rc<Vec<TemplateGroup>>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let scan_failed = self.error_source == ErrorSource::Scan && !self.error.is_empty();
        let pattern_active = !self.pattern.trim().is_empty();
        let visible = rows.len();
        let state = list_state(self.busy || self.scan.running, scan_failed, self.keys.len(), visible, pattern_active, self.done);
        let focus = self.key_focus.get_or_insert_with(|| cx.focus_handle()).clone();

        let mut list = div()
            .id("redis-keys")
            .test_support()
            .track_focus(&focus)
            .on_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, _, cx| this.key_list_key(event, cx)))
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .px(px(Space::SM))
            .py(px(Space::SM));
        match state {
            ListState::Loading => return list.child(kit::skeleton_state(8, m, p)).into_any_element(),
            ListState::Error => {
                let retry = action("redis-retry-scan", Some(IconName::RefreshCw), &self.text("common.retry"), Kind::Secondary, entity).into_any_element();
                return list
                    .child(kit::error_state_with_action(
                        SharedString::from(self.text("redis.error.scan.heading")),
                        SharedString::from(self.error.clone()),
                        retry,
                        p,
                    ))
                    .into_any_element();
            }
            ListState::NoMatch => {
                let clear = action("redis-clear-filter", Some(IconName::ListFilter), &self.text("redis.noMatch.clear"), Kind::Secondary, entity).into_any_element();
                return list
                    .child(kit::no_match_state_with_action(
                        IconName::Search,
                        SharedString::from(self.text("redis.noMatch.heading")),
                        SharedString::from(self.text("redis.noMatch.detail")),
                        clear,
                        p,
                    ))
                    .into_any_element();
            }
            ListState::Empty => {
                let create = action("redis-new-key", Some(IconName::Plus), &self.text("redis.action.newKey"), Kind::Primary, entity).into_any_element();
                return list
                    .child(kit::empty_state_with_action(
                        IconName::Key,
                        SharedString::from(self.text("redis.empty.heading")),
                        SharedString::from(self.text("redis.empty.detail")),
                        create,
                        p,
                    ))
                    .into_any_element();
            }
            ListState::Rows => {
                // 读取进度只在页脚；列表里不插加载行，已读到的键不会被推着下移。
                if visible == 0 && !self.scan.running && !self.busy {
                    list = list.child(
                        div()
                            .id("redis-keys-blank")
                            .test_support()
                            .px(px(Space::MD))
                            .py(px(Space::XS))
                            .text_size(px(TypeScale::XS))
                            .text_color(p.text_faint)
                            .child(SharedString::from(self.text("redis.tab.emptyList"))),
                    );
                }
            }
        }
        if visible == 0 {
            return list.into_any_element();
        }
        let _ = window;
        // 虚拟列表：只渲染可见行，键数没有上限（内存索引上限由偏好控制）。
        let owner = entity.downgrade();
        let row_entity = entity.clone();
        // 虚拟列表不是 overflow 容器：在同大小的 relative 父层上叠一条可见滚动条（ADR-8）。
        list.child(
            div()
                .relative()
                .flex()
                .flex_col()
                .flex_1()
                .min_h(px(0.))
                .child(
                    gpui_kit::uniform_list("redis-key-rows", visible, move |range, window, cx| {
                        let rows = Rc::clone(&rows);
                        let templates = templates.clone();
                        let entity = row_entity.clone();
                        owner
                            .update(cx, |this, cx| {
                                range.map(|index| this.key_row_element(&rows[index], templates.as_deref(), side_w, &entity, window, cx)).collect::<Vec<_>>()
                            })
                            .unwrap_or_default()
                    })
                    .track_scroll(&self.key_scroll)
                    .flex_1(),
                )
                .child(kit::scrollbar_overlay("redis-key-rows-bar", &self.key_scroll, kit::ScrollbarAxis::Vertical)),
        )
        .into_any_element()
    }

    /// 键列表的键盘：回车在新标签中固定打开选中的键；上下 / Home / End 移动选中并滚入视口；Delete 删除选中键。
    pub(super) fn key_list_key(&mut self, event: &gpui_kit::KeyDownEvent, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        if event.keystroke.modifiers.modified() {
            return;
        }
        match key {
            "enter" if !self.selected.is_empty() => {
                cx.stop_propagation();
                self.command("redis-open-tab", cx);
            }
            "delete" | "backspace" if !self.selected.is_empty() && self.confirm.is_none() => {
                cx.stop_propagation();
                self.command("redis-delete", cx);
            }
            "up" | "down" | "home" | "end" => {
                let rows = self.key_rows();
                let keys: Vec<(usize, usize)> = rows
                    .iter()
                    .enumerate()
                    .filter_map(|(row_ix, row)| if let KeyRow::Key { index, .. } = row { Some((row_ix, *index)) } else { None })
                    .collect();
                if keys.is_empty() {
                    return;
                }
                let at = keys.iter().position(|(_, index)| self.keys.get(*index).is_some_and(|k| k.name == self.selected));
                let next = match (at, key) {
                    (_, "home") => 0,
                    (_, "end") => keys.len() - 1,
                    (None, _) => 0,
                    (Some(at), "up") => at.saturating_sub(1),
                    (Some(at), _) => (at + 1).min(keys.len() - 1),
                };
                let (row_ix, key_ix) = keys[next];
                self.key_scroll.scroll_to_item(row_ix, gpui_kit::ScrollStrategy::Nearest);
                cx.stop_propagation();
                self.command(&format!("redis-select:{key_ix}"), cx);
            }
            _ => {}
        }
    }

    /// 列表的一行（虚拟列表按需调用）。行高、悬停与选中和资源管理器一致。
    fn key_row_element(&mut self, row: &KeyRow, templates: Option<&Vec<TemplateGroup>>, side_w: f32, entity: &Entity<Self>, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let m = kit::Metrics::of(cx);
        let row_h = m.tree_row_h;
        let indent = |depth: usize| Space::SM + depth as f32 * Space::LG;
        match row {
            KeyRow::Folder { prefix, segment, depth, count, open } => {
                let id = format!("redis-folder:{prefix}");
                let menu_prefix = prefix.clone();
                let menu_entity = entity.clone();
                kit::interactive_states(div().id(SharedString::from(id.clone())), &p, false)
                    .test_support()
                    .role(gpui_kit::Role::Button)
                    .aria_label(SharedString::from(prefix.clone()))
                    .aria_expanded(*open)
                    .w_full()
                    .h(px(row_h))
                    .pl(px(indent(*depth)))
                    .pr(px(KEY_ROW_PR))
                    .flex()
                    .items_center()
                    .gap(px(Space::SM))
                    .rounded(px(Radius::CONTROL))
                    .text_size(px(TypeScale::SM))
                    .text_color(p.text)
                    .hover(|style| style.bg(p.hover))
                    .child(kit::icon(if *open { IconName::ChevronDown } else { IconName::ChevronRight }, IconSize::INLINE, p.text_faint))
                    .child(kit::icon(if *open { IconName::FolderOpen } else { IconName::Folder }, IconSize::INLINE, p.accent_text))
                    .child(div().flex_1().min_w(px(0.)).truncate().font_weight(FontWeight::MEDIUM).child(SharedString::from(segment.clone())))
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
                    .on_click(cmd_click(entity, id))
                    .into_any_element()
            }
            KeyRow::Template { group, open } => {
                let Some(item) = templates.and_then(|groups| groups.get(*group)) else { return div().h(px(row_h)).into_any_element() };
                let template = item.template.clone();
                let id = format!("redis-tpl:{template}");
                let menu_entity = entity.clone();
                let menu_template = template.clone();
                let memory = match self.template_memory.get(&template) {
                    Some(Some(bytes)) => Some(i18n::fill(&self.text("redis.tpl.memoryEstimate"), &[("size", &keyspace::human_bytes(*bytes))])),
                    Some(None) => Some(self.text("redis.tpl.memoryLoading")),
                    None => None,
                };
                let dominant = item.types.first().map(|(kind, _)| kind.clone()).unwrap_or_default();
                kit::interactive_states(div().id(SharedString::from(id.clone())), &p, false)
                    .test_support()
                    .role(gpui_kit::Role::Button)
                    .aria_label(SharedString::from(format!("{template} {}", item.count)))
                    .aria_expanded(*open)
                    .w_full()
                    .h(px(row_h))
                    .pl(px(Space::SM))
                    .pr(px(KEY_ROW_PR))
                    .flex()
                    .items_center()
                    .gap(px(Space::SM))
                    .rounded(px(Radius::CONTROL))
                    .text_size(px(TypeScale::SM))
                    .text_color(p.text)
                    .hover(|style| style.bg(p.hover))
                    .child(kit::icon(if *open { IconName::ChevronDown } else { IconName::ChevronRight }, IconSize::INLINE, p.text_faint))
                    .child(type_tag(&dominant, &m, &p))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .font_family(cx.theme().mono_font_family.clone())
                            .font_weight(FontWeight::MEDIUM)
                            .child(SharedString::from(template.clone())),
                    )
                    .when_some(memory, |this, memory| {
                        this.child(div().flex_none().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(SharedString::from(memory)))
                    })
                    .child(
                        div()
                            .flex_none()
                            .px(px(Space::XS))
                            .rounded(px(Radius::TAG))
                            .bg(p.sunken)
                            .text_size(px(TypeScale::XS))
                            .font_features(kit::tabular_figures())
                            .text_color(p.text_muted)
                            .child(SharedString::from(item.count.to_string())),
                    )
                    .on_mouse_down(MouseButton::Right, move |event: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        let template = menu_template.clone();
                        let pos = event.position;
                        menu_entity.update(cx, |pane, cx| pane.open_template_context_menu(template, pos, cx));
                    })
                    .on_click(cmd_click(entity, id))
                    .into_any_element()
            }
            KeyRow::TemplateMore { template, shown, total } => {
                let id = format!("redis-tpl-filter:{template}");
                let label = i18n::fill(&self.text("redis.tpl.more"), &[("shown", &shown.to_string()), ("total", &total.to_string())]);
                kit::interactive_states(div().id(SharedString::from(format!("redis-tpl-more:{template}"))), &p, false)
                    .test_support()
                    .role(gpui_kit::Role::Button)
                    .aria_label(SharedString::from(label.clone()))
                    .w_full()
                    .h(px(row_h))
                    .pl(px(indent(1)))
                    .pr(px(KEY_ROW_PR))
                    .flex()
                    .items_center()
                    .gap(px(Space::XS))
                    .rounded(px(Radius::CONTROL))
                    .text_size(px(TypeScale::XS))
                    .text_color(p.accent_text)
                    .hover(|style| style.bg(p.hover))
                    .child(kit::icon(IconName::ListFilter, IconSize::INLINE, p.accent_text))
                    .child(div().flex_1().min_w(px(0.)).truncate().child(SharedString::from(label)))
                    .on_click(cmd_click(entity, id))
                    .into_any_element()
            }
            KeyRow::Key { index, display, depth } => {
                let Some(key) = self.keys.get(*index).cloned() else { return div().h(px(row_h)).into_any_element() };
                let index = *index;
                let selected = key.name == self.selected;
                let gone = self.gone.contains(&key.name);
                let id = format!("redis-select:{index}");
                let ttl_label = describe_ttl(self.locale, key.ttl_ms);
                let label = format!("{} {} {}", key.key_type, key.name, ttl_label);
                let tooltip = format!("{}\n{} · {}", key.name, type_display(&key.key_type, self), ttl_label);
                let menu_entity = entity.clone();
                let ttl = key.ttl_ms.filter(|ttl| *ttl >= 0);
                let ttl_text = ttl.map(short_ttl);
                // 键名用界面字体：前缀弱化、放不下时从左侧省略，末段（ID 等区分同前缀键的部分）完整保留；悬停给全名、类型与 TTL。
                let font = gpui_kit::Font { family: cx.theme().font_family.clone(), ..Default::default() };
                let ttl_w = ttl_text.as_deref().map(|text| Self::measure_ui(window, &font, TypeScale::XS, text) + Space::MD).unwrap_or(0.);
                let fixed = Space::SM * 2. + KEY_ROW_PR + ControlSize::BADGE_W + Space::SM + Space::XS;
                let available = (side_w - fixed - indent(*depth) - ttl_w).max(0.);
                let shown = self.fit_key_name(window, &font, TypeScale::SM, display, available);
                let open_entity = entity.clone();
                kit::interactive_states(div().id(SharedString::from(id.clone())), &p, false)
                    .test_support()
                    .role(gpui_kit::Role::Button)
                    .aria_label(SharedString::from(label))
                    .aria_selected(selected)
                    .tooltip_text(SharedString::from(tooltip))
                    .w_full()
                    .h(px(row_h))
                    .pl(px(indent(*depth)))
                    .pr(px(KEY_ROW_PR))
                    .flex()
                    .items_center()
                    .gap(px(Space::SM))
                    .rounded(px(Radius::CONTROL))
                    .border_l_2()
                    .when(selected, |this| this.border_color(p.accent).bg(p.selection))
                    .when(!selected, |this| this.border_color(gpui_kit::transparent_black()).hover(|style| style.bg(p.hover)))
                    .child(type_tag(&key.key_type, &m, &p))
                    .child(
                        div()
                            .id(SharedString::from(format!("redis-key-name:{index}")))
                            .test_support()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .items_center()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_size(px(TypeScale::SM))
                            .when(gone, |this| this.line_through())
                            .when(!shown.prefix.is_empty(), |this| {
                                this.child(
                                    div()
                                        .id(SharedString::from(format!("redis-key-prefix:{index}")))
                                        .test_support()
                                        .flex_none()
                                        .text_color(p.text_faint)
                                        .child(SharedString::from(shown.prefix.clone())),
                                )
                            })
                            .child(
                                div()
                                    .id(SharedString::from(format!("redis-key-last:{index}")))
                                    .test_support()
                                    .flex_none()
                                    .text_color(if gone { p.text_faint } else { p.text })
                                    .when(selected, |this| this.font_weight(FontWeight::MEDIUM))
                                    .child(SharedString::from(shown.last.clone())),
                            ),
                    )
                    .when_some(ttl.zip(ttl_text), |this, (ms, text)| {
                        this.child(
                            div()
                                .flex_none()
                                .text_size(px(TypeScale::XS))
                                .font_features(kit::tabular_figures())
                                .text_color(if ms < 60_000 { p.warn } else { p.text_faint })
                                .child(SharedString::from(text)),
                        )
                    })
                    .on_mouse_down(MouseButton::Right, move |event: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        let pos = event.position;
                        menu_entity.update(cx, |pane, cx| pane.open_key_context_menu(index, pos, cx));
                    })
                    .on_click(move |event: &ClickEvent, window, cx| {
                        // 单击选中；双击在新标签中固定打开。
                        let id = if event.click_count() >= 2 { format!("redis-open-tab:{index}") } else { format!("redis-select:{index}") };
                        open_entity.update(cx, |pane, cx| {
                            if let Some(focus) = pane.key_focus.clone() {
                                window.focus(&focus, cx);
                            }
                            pane.command(&id, cx);
                        });
                    })
                    .into_any_element()
            }
        }
    }

    /// 列表视图切换（树 / 模板 / 平铺）：放在列表底栏右端，不和类型筛选抢一行。
    fn render_view_toggle(&self, p: &Palette, entity: &Entity<Self>) -> AnyElement {
        div()
            .flex_none()
            .flex()
            .items_center()
            .p(px(Space::XXS))
            .gap(px(Space::XXS))
            .rounded(px(Radius::CONTROL))
            .bg(p.sunken)
            .child(icon_btn("redis-view-tree", IconName::ListTree, &self.text("redis.view.tree"), entity).xsmall().selected(self.view_mode == ViewMode::Tree))
            .child(icon_btn("redis-view-template", IconName::Shapes, &self.text("redis.view.template"), entity).xsmall().selected(self.view_mode == ViewMode::Template))
            .child(icon_btn("redis-view-flat", IconName::List, &self.text("redis.view.flat"), entity).xsmall().selected(self.view_mode == ViewMode::Flat))
            .into_any_element()
    }

    /// 页脚：扫描进度（已读取 / DBSIZE）、取消、上限提示与继续扫描。
    fn render_key_footer(&self, p: &Palette, m: &kit::Metrics, entity: &Entity<Self>) -> AnyElement {
        let scan_failed = self.error_source == ErrorSource::Scan && !self.error.is_empty();
        let bar = div()
            .id("redis-scan-footer")
            .test_support()
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
                .child(kit::icon(IconName::CircleAlert, 13., p.danger))
                .child(div().id("redis-list-error").test_support().flex_1().min_w(px(0.)).truncate().text_size(px(TypeScale::XS)).text_color(p.danger).child(SharedString::from(self.error.clone())))
                .child(action("redis-retry-scan", Some(IconName::RefreshCw), &self.text("common.retry"), Kind::Tool, entity))
                .into_any_element();
        }
        let read = self.keys.len().to_string();
        let mut text = match self.scan.total {
            Some(total) => i18n::fill(&self.text("redis.scan.progress"), &[("n", &read), ("total", &total.to_string())]),
            None => i18n::fill(&self.text("redis.scan.read"), &[("n", &read)]),
        };
        if !self.type_filter.is_empty() {
            let visible = self.keys.iter().filter(|key| key.key_type == self.type_filter).count();
            text = format!("{text} · {}", i18n::fill(&self.text("redis.count.filtered"), &[("shown", &visible.to_string()), ("n", &read)]));
        }
        let state = if self.scan.running {
            self.text("redis.scan.running")
        } else if self.scan.capped {
            i18n::fill(&self.text("redis.scan.capped"), &[("cap", &self.prefs.scan_cap.to_string())])
        } else if self.done {
            self.text("redis.list.allLoaded")
        } else if self.scan.cancelled {
            self.text("redis.scan.cancelled")
        } else {
            self.text("redis.list.moreLeft")
        };
        let pattern_note = (!self.pattern.trim().is_empty()).then(|| self.text("redis.scan.patternNote"));
        let summary = match pattern_note {
            Some(note) => format!("{text} · {state} · {note}"),
            None => format!("{text} · {state}"),
        };
        let ratio = self.scan.total.filter(|total| *total > 0).map(|total| (self.keys.len() as f32 / total as f32).min(1.)).unwrap_or(if self.done { 1. } else { 0. });
        bar.child(
            div()
                .id("redis-scan-progress")
                .test_support()
                .role(gpui_kit::Role::Status)
                .aria_label(SharedString::from(summary.clone()))
                .flex_1()
                .min_w(px(0.))
                .child(super::pages::progress_bar(&summary, ratio, self.scan.running, p)),
        )
        .when(self.scan.running, |this| this.child(action("redis-scan-cancel", Some(IconName::CircleStop), &self.text("common.cancel"), Kind::Tool, entity)))
        .when(!self.scan.running && !self.done && !self.scan.capped, |this| {
            this.child(action("redis-more", Some(IconName::ChevronsDown), &self.text("redis.tab.loadMore"), Kind::Tool, entity).disabled(self.busy))
        })
        .when(self.scan.capped, |this| this.child(kit::icon(IconName::TriangleAlert, 13., p.warn)))
        .into_any_element()
    }

    // ------------------------------------------------------------------ 详情

    fn render_detail(&mut self, p: &Palette, m: &kit::Metrics, entity: &Entity<Self>, detail_w: f32, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let col = div().flex().flex_col().flex_1().min_w(px(0.)).min_h(px(0.)).bg(p.surface);
        if self.selected.is_empty() {
            let mono = cx.theme().mono_font_family.clone();
            return col.child(self.render_keyspace_overview(p, m, entity, mono)).into_any_element();
        }

        let header = self.render_detail_header(p, m, entity, detail_w, cx);
        let ttl_bar = if self.ttl_open { Some(self.render_ttl_bar(p, m, entity, window, cx)) } else { None };
        let rename_bar = if self.rename_open { Some(self.render_rename_bar(p, m, entity, window, cx)) } else { None };

        let detail_error = self.error_source == ErrorSource::Detail && !self.error.is_empty();
        // 读取中保留上一份内容（换键不清空、不出骨架）；只有还没有任何内容时才画骨架。
        let body = if detail_error && !self.reading {
            let retry = action("redis-retry-read", Some(IconName::RefreshCw), &self.text("redis.retry.read"), Kind::Secondary, entity).into_any_element();
            kit::error_state_with_action(
                SharedString::from(self.text("redis.error.detail.heading")),
                SharedString::from(self.error.clone()),
                retry,
                p,
            )
            .into_any_element()
        } else if self.detail.is_some() {
            stack_frame(|| self.value_body(p, m, entity, detail_w, window, cx))
        } else if self.reading || self.busy {
            kit::skeleton_state(5, m, p).into_any_element()
        } else {
            div()
                .p(px(m.pane_pad()))
                .child(kit::empty_state(IconName::Key, SharedString::from(self.text("redis.value.pickHeading")), SharedString::from(self.text("redis.tab.pickKey")), p))
                .into_any_element()
        };

        col.child(header)
            .children(ttl_bar)
            .children(rename_bar)
            .child(div().id("redis-value").test_support().flex_1().min_h(px(0.)).flex().flex_col().child(body))
            .into_any_element()
    }

    /// 没有选中键时的值区：当前库的概览（类型分布、过期、主要键模式），点一下就能筛选；
    /// 还没有读到任何键时退回原来的空态。
    fn render_keyspace_overview(&mut self, p: &Palette, m: &kit::Metrics, entity: &Entity<Self>, mono: SharedString) -> AnyElement {
        let create = action("redis-new-key-value", Some(IconName::Plus), &self.text("redis.action.newKey"), Kind::Primary, entity).into_any_element();
        if self.keys.is_empty() {
            return div()
                .flex_1()
                .min_w(px(0.))
                .p(px(m.pane_pad() * 2.))
                .child(kit::empty_state_with_action(
                    IconName::Key,
                    SharedString::from(self.text("redis.empty.pick.heading")),
                    SharedString::from(self.text("redis.tab.pickKey")),
                    create,
                    p,
                ))
                .into_any_element();
        }
        let read = self.keys.len();
        let counts: Vec<_> = self.type_counts().into_iter().filter(|(_, _, n)| *n > 0).collect();
        let expiring = self.keys.iter().filter(|key| key.ttl_ms.is_some_and(|ttl| ttl >= 0)).count();
        let templates = self.templates();
        let total = self.scan.total.map(|n| n as usize).unwrap_or(read).max(read);
        let section = |title: String| div().text_size(px(TypeScale::XS)).font_weight(FontWeight::SEMIBOLD).text_color(p.text_muted).child(SharedString::from(title));

        // 类型分布：一条按比例着色的条，下面是可点的图例。
        let mut bar = div().h(px(8.)).w_full().flex().rounded(px(Radius::TAG)).overflow_hidden().bg(p.sunken);
        for (kind, _, n) in &counts {
            bar = bar.child(div().h_full().flex_basis(relative(*n as f32 / read as f32)).flex_shrink_0().bg(type_tone(kind).colors(p).0));
        }
        let mut legend = div().flex().flex_wrap().gap(px(Space::XS));
        for (kind, key, n) in &counts {
            let id = format!("redis-type-{kind}");
            let share = *n as f32 * 100. / read as f32;
            let percent = if share > 0. && share < 1. { "<1%".to_owned() } else { format!("{}%", share.round()) };
            legend = legend.child(
                kit::interactive_states(div().id(SharedString::from(format!("redis-overview-{kind}"))), p, false)
                    .test_support()
                    .role(gpui_kit::Role::Button)
                    .aria_label(SharedString::from(format!("{} {n}", self.text(key))))
                    .h(px(28.))
                    .px(px(Space::SM))
                    .flex()
                    .items_center()
                    .gap(px(Space::SM))
                    .rounded(px(Radius::CONTROL))
                    .border_1()
                    .border_color(p.line_soft)
                    .hover(|s| s.bg(p.hover).border_color(p.line))
                    .text_size(px(TypeScale::SM))
                    .child(type_dot(kind, p))
                    .child(div().text_color(p.text).child(SharedString::from(self.text(key))))
                    .child(div().font_features(kit::tabular_figures()).text_color(p.text_muted).child(SharedString::from(n.to_string())))
                    .child(div().font_features(kit::tabular_figures()).text_size(px(TypeScale::XS)).text_color(p.text_faint).child(SharedString::from(percent)))
                    .on_click(cmd_click(entity, id)),
            );
        }

        // 主要键模式：前几个模板，点一下按模式重新扫描。
        let mut patterns = div().flex().flex_col();
        // 只有一个键的「模式」不算模式，不列出来。
        let groups: Vec<_> = templates.iter().filter(|group| group.count > 1).take(6).collect();
        for group in &groups {
            let id = format!("redis-tpl-filter:{}", group.template);
            let dominant = group.types.first().map(|(kind, _)| kind.clone()).unwrap_or_default();
            let share = group.count as f32 / read as f32;
            patterns = patterns.child(
                kit::interactive_states(div().id(SharedString::from(format!("redis-overview-tpl:{}", group.template))), p, false)
                    .test_support()
                    .role(gpui_kit::Role::Button)
                    .aria_label(SharedString::from(group.template.clone()))
                    .tooltip_text(SharedString::from(self.text("redis.tpl.filter")))
                    .h(px(30.))
                    .px(px(Space::SM))
                    .flex()
                    .items_center()
                    .gap(px(Space::SM))
                    .rounded(px(Radius::CONTROL))
                    .hover(|s| s.bg(p.hover))
                    .child(type_dot(&dominant, p))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .font_family(mono.clone())
                            .text_size(px(TypeScale::SM))
                            .text_color(p.text)
                            .child(SharedString::from(group.template.clone())),
                    )
                    .child(div().flex_none().w(px(80.)).h(px(4.)).rounded_full().bg(p.sunken).child(div().h_full().w(relative(share.min(1.))).rounded_full().bg(p.accent.opacity(0.55))))
                    .child(div().flex_none().w(px(48.)).flex().justify_end().font_features(kit::tabular_figures()).text_size(px(TypeScale::XS)).text_color(p.text_muted).child(SharedString::from(group.count.to_string())))
                    .on_click(cmd_click(entity, id)),
            );
        }

        let stat_card = |label: String, value: String, tone: Option<gpui_kit::Hsla>| {
            kit::card(p)
                .flex_1()
                .min_w(px(ControlSize::PICKER_MIN_W / 2.))
                .px(px(Space::LG))
                .py(px(Space::MD))
                .flex()
                .flex_col()
                .gap(px(Space::XXS))
                .child(kit::eyebrow(SharedString::from(label), p))
                .child(
                    div()
                        .text_size(px(TypeScale::LG))
                        .font_weight(FontWeight::SEMIBOLD)
                        .font_features(kit::tabular_figures())
                        .text_color(tone.unwrap_or(p.text))
                        .child(SharedString::from(value)),
                )
        };
        let persistent = read.saturating_sub(expiring);
        let stats = div()
            .flex()
            .flex_wrap()
            .gap(px(Space::MD))
            .child(stat_card(self.text("redis.overview.total"), total.to_string(), None))
            .child(stat_card(self.text("redis.overview.read"), read.to_string(), None))
            .child(stat_card(self.text("redis.overview.expiring"), expiring.to_string(), (expiring > 0).then_some(p.info)))
            .child(stat_card(self.text("redis.overview.neverExpire"), persistent.to_string(), None));
        let heading = if self.topology.cluster {
            i18n::fill(&self.text("redis.overview.cluster"), &[("n", &self.topology.masters().len().to_string())])
        } else {
            format!("db{}", self.database)
        };
        let console = action("redis-console", Some(IconName::SquareTerminal), &self.text("redis.page.console"), Kind::Secondary, entity).into_any_element();
        let quick_pages = div()
            .flex()
            .flex_wrap()
            .gap(px(Space::SM))
            .child(action("redis-page:info", Some(IconName::Gauge), &self.text("redis.page.info"), Kind::Tool, entity).small())
            .child(action("redis-page:slowlog", Some(IconName::Clock), &self.text("redis.page.slowlog"), Kind::Tool, entity).small())
            .child(action("redis-page:bigkeys", Some(IconName::ChartBar), &self.text("redis.page.bigkeys"), Kind::Tool, entity).small());
        div()
            .id("redis-overview")
            .test_support()
            .flex_1()
            .min_h(px(0.))
            .scroll_area_y()
            .child(
                div()
                    .max_w(px(720.))
                    .px(px(m.pane_pad() * 2.))
                    .py(px(Space::XL))
                    .flex()
                    .flex_col()
                    .gap(px(Space::XL))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(Space::MD))
                            .child(
                                div()
                                    .flex_1()
                                    .flex()
                                    .flex_col()
                                    .gap(px(Space::XXS))
                                    .child(div().text_size(px(TypeScale::LG)).font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(SharedString::from(heading)))
                                    .child(div().text_size(px(TypeScale::SM)).text_color(p.text_muted).child(SharedString::from(self.text("redis.overview.hint")))),
                            )
                            .child(console)
                            .child(create),
                    )
                    .child(stats)
                    .child(div().flex().flex_col().gap(px(Space::SM)).child(section(self.text("redis.overview.types"))).child(bar).child(legend))
                    .when(!groups.is_empty(), |this| this.child(div().flex().flex_col().gap(px(Space::XS)).child(section(self.text("redis.overview.patterns"))).child(patterns)))
                    .child(quick_pages),
            )
            .into_any_element()
    }

    /// 两行头部：键名与类型 chip、右侧主要操作（刷新、TTL、删除、更多）；第二行是 TTL、大小、长度等元信息。
    /// 读取中只在底边画一条细进度线。窄窗口下 TTL 与删除也收进「更多」。
    fn render_detail_header(&self, p: &Palette, m: &kit::Metrics, entity: &Entity<Self>, detail_w: f32, cx: &mut Context<Self>) -> AnyElement {
        // 头部总是描述当前选中的键：类型来自键索引；元信息只在详情已经属于这个键时显示。
        let current = (self.detail_key == self.selected).then_some(self.detail.as_deref()).flatten();
        let type_name = current
            .map(|d| d.key_type.clone())
            .or_else(|| self.key_pos.get(&self.selected).and_then(|&index| self.keys.get(index)).map(|key| key.key_type.clone()))
            .unwrap_or_default();
        let gone = self.gone.contains(&self.selected);
        let ttl_ms = current.and_then(|d| d.ttl_ms);
        let size = current.map(|d| self.count_label(d)).unwrap_or_default();
        let expiring = ttl_ms.is_some_and(|ms| ms >= 0);
        let urgent = ttl_ms.is_some_and(|ms| (0..60_000).contains(&ms));
        let overflow = detail_w < VALUE_STACK_W;

        let more_entity = entity.clone();
        let more = kit::icon_button("redis-more-actions", IconName::Ellipsis, SharedString::from(self.text("redis.detail.more")), move |event: &ClickEvent, _, cx| {
            let at = event.position();
            more_entity.update(cx, |pane, cx| pane.open_more_menu(at, overflow, cx));
        })
        .small();
        // 键操作统一成一组图标按钮（同一尺寸、同一底色），放在标题行右端；复制键名紧跟在键名后面。
        let mut actions = div()
            .id("redis-detail-actions")
            .test_support()
            .role(gpui_kit::Role::Group)
            .aria_label(SharedString::from(self.text("redis.detail.actions")))
            .flex_none()
            .flex()
            .items_center()
            .p(px(Space::XXS))
            .gap(px(Space::XXS))
            .rounded(px(Radius::CONTROL))
            .bg(p.sunken)
            .child(icon_btn("redis-refresh-key", IconName::RefreshCw, &self.text("redis.tab.refresh"), entity));
        if !overflow {
            actions = actions
                // 编辑条开着时它的提交按钮占用 `redis-ttl`，头部按钮改为收起。
                .child(icon_btn(if self.ttl_open { "redis-ttl-toggle" } else { "redis-ttl" }, IconName::Timer, &self.text("redis.tab.setTtl"), entity).selected(self.ttl_open))
                .child(icon_btn("redis-delete", IconName::Trash, &self.text("redis.tab.remove"), entity));
        }
        actions = actions.child(more);

        // 标题行：键名完整显示（最多两行）+ 复制键名；元信息在下面单独一行 chips。
        let mono = cx.theme().mono_font_family.clone();
        let type_label = type_display(&type_name, self);
        let title = div()
            .id("redis-detail-title-row")
            .test_support()
            .flex()
            .items_start()
            .gap(px(Space::SM))
            .min_w(px(0.))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .items_start()
                    .gap(px(Space::XS))
                    .child(
                        div()
                            .id("redis-key-title")
                            .test_support()
                            .min_w(px(0.))
                            .line_clamp(2)
                            .font_family(mono)
                            .text_size(px(TypeScale::MD))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(if gone { p.text_faint } else { p.text })
                            .when(gone, |this| this.line_through())
                            .tooltip_text(SharedString::from(self.selected.clone()))
                            .child(SharedString::from(self.selected.clone())),
                    )
                    .child(icon_btn("redis-copy-key", IconName::Copy, &self.text("redis.copy.keyName"), entity).xsmall()),
            )
            .child(actions);
        let type_chip = (!type_name.is_empty()).then(|| {
            div()
                .id("redis-type-chip")
                .test_support()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(Space::XS))
                .text_size(px(TypeScale::XS))
                .text_color(p.text_muted)
                .child(type_tag(&type_name, m, p))
                .child(SharedString::from(type_label))
        });

        let (ttl_fg, ttl_bg) = if urgent {
            (p.warn, p.warn_soft)
        } else if expiring {
            (p.info, p.accent_soft)
        } else {
            (p.text_muted, p.sunken)
        };
        let ttl_chip = kit::interactive_states(div().id("redis-ttl-chip"), p, false)
            .test_support()
            .role(gpui_kit::Role::Button)
            .aria_label(SharedString::from(self.text("redis.tab.setTtl")))
            .tooltip_text(SharedString::from(self.text("redis.tab.setTtl")))
            .h(px(m.chip_h()))
            .px(px(Space::SM))
            .flex()
            .items_center()
            .gap(px(Space::XS))
            .rounded(px(Radius::TAG))
            .bg(ttl_bg)
            .text_color(ttl_fg)
            .text_size(px(TypeScale::XS))
            .font_features(kit::tabular_figures())
            .hover(|s| s.opacity(0.85))
            .child(kit::icon(if expiring { IconName::Timer } else { IconName::Infinity }, IconSize::INLINE, ttl_fg))
            .child(SharedString::from(describe_ttl(self.locale, ttl_ms)))
            .on_click(cmd_click(entity, "redis-ttl-toggle"));
        let memory = current.and_then(|d| d.memory).map(|bytes| i18n::fill(&self.text("redis.value.memory"), &[("size", &keyspace::human_bytes(bytes))]));
        let encoding = current.and_then(|d| d.encoding.clone()).filter(|name| !name.is_empty()).map(|name| i18n::fill(&self.text("redis.value.encoding"), &[("name", &name)]));
        let meta_chip = |id: &'static str, text: String| {
            div()
                .id(id)
                .test_support()
                .flex_none()
                .h(px(m.chip_h()))
                .px(px(Space::SM))
                .flex()
                .items_center()
                .rounded(px(Radius::TAG))
                .bg(p.sunken)
                .text_size(px(TypeScale::XS))
                .font_features(kit::tabular_figures())
                .text_color(p.text_muted)
                .child(SharedString::from(text))
        };
        let weak = |text: String| div().flex_none().text_size(px(TypeScale::XS)).font_features(kit::tabular_figures()).text_color(p.text_muted).child(SharedString::from(text));
        let placement = self.placement.clone().map(|place| {
            let mut row = div().id("redis-key-placement").test_support().flex().items_center().gap(px(Space::MD));
            row = row.child(weak(i18n::fill(&self.text("redis.cluster.slot"), &[("n", &place.slot.to_string())])));
            if let Some(node) = place.node {
                row = row.child(weak(i18n::fill(&self.text("redis.cluster.node"), &[("addr", &node)])));
            }
            if let Some(tag) = place.hash_tag {
                row = row.child(weak(i18n::fill(&self.text("redis.cluster.hashTag"), &[("tag", &tag)])));
            }
            row
        });
        let meta = div()
            .id("redis-detail-meta")
            .test_support()
            .h(px(m.chip_h()))
            .flex()
            .flex_nowrap()
            .overflow_hidden()
            .items_center()
            .gap(px(Space::SM))
            .children(type_chip)
            .when(current.is_some(), |this| this.child(ttl_chip))
            .when(!size.is_empty(), |this| this.child(meta_chip("redis-size-chip", size)))
            .when_some(memory, |this, text| this.child(meta_chip("redis-memory-chip", text)))
            .when_some(encoding, |this, text| this.child(meta_chip("redis-encoding-chip", text)))
            .when(gone, |this| this.child(kit::chip(SharedString::from(self.text("redis.keys.missing")), ChipTone::Danger, m, p)))
            .children(placement);

        let progress = self.reading.then(|| {
            div()
                .id("redis-detail-loading")
                .test_support()
                .role(gpui_kit::Role::Status)
                .aria_label(SharedString::from(self.text("redis.value.loading")))
                .absolute()
                .left_0()
                .right_0()
                .bottom_0()
                .h(px(Space::XXS))
                .bg(p.accent.opacity(0.55))
        });

        div()
            .id("redis-detail-header")
            .test_support()
            .relative()
            .flex_none()
            .px(px(m.pane_pad()))
            .pt(px(Space::MD))
            .pb(px(Space::MD))
            .flex()
            .flex_col()
            .gap(px(Space::SM))
            .border_b_1()
            .border_color(p.line)
            .child(title)
            .child(meta)
            .children(progress)
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
        let preview = match parse_ttl(&self.ttl_input) {
            Ok(Some(secs)) => Some((i18n::fill(&self.text("redis.ttl.preview"), &[("t", &long_ttl(secs.saturating_mul(1000)))]), p.info)),
            Ok(None) => Some((self.text("redis.keys.neverExpire"), p.text_faint)),
            Err(()) => Some((self.text("redis.tab.ttlInvalid"), p.warn)),
        };
        let mut presets = div().flex().items_center().gap(px(Space::XXS));
        for (secs, key) in [(60, "redis.ttl.1m"), (300, "redis.ttl.5m"), (3600, "redis.ttl.1h"), (86_400, "redis.ttl.1d"), (604_800, "redis.ttl.7d")] {
            presets = presets.child(action(format!("redis-ttl-preset:{secs}"), None, &self.text(key), Kind::Tool, entity).small());
        }
        if expiring {
            presets = presets.child(action("redis-ttl-preset:persist", Some(IconName::Infinity), &self.text("redis.action.removeTtl"), Kind::Tool, entity).small());
        }
        Self::edit_bar(p, m)
            .child(div().text_size(px(TypeScale::XS)).font_weight(FontWeight::MEDIUM).text_color(p.text_muted).child(SharedString::from(self.text("redis.tab.ttlLabel"))))
            .child(div().w(px(ControlSize::PICKER_MIN_W * 0.7)).child(kit::text_input("redis-ttl-field", &ttl, self.text("redis.tab.ttlLabel")).small()))
            .when_some(preview, |this, (text, color)| {
                this.child(div().text_size(px(TypeScale::XS)).font_features(kit::tabular_figures()).text_color(color).child(SharedString::from(text)))
            })
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

    fn value_body(&mut self, p: &Palette, m: &kit::Metrics, entity: &Entity<Self>, detail_w: f32, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(detail) = self.detail.clone() else {
            return div().text_size(px(TypeScale::SM)).child(SharedString::from(self.text("redis.tab.pickKey"))).into_any_element();
        };
        // 读取中显示的是上一个键的内容：压暗一点，提示还没换过来。
        let stale = self.reading && self.detail_key != self.selected;
        let body = match &detail.value {
            RedisValue::String { text } => {
                let preview = keys::string_is_preview(&detail);
                let viewer = self.ensure_string_viewer(text, preview.then_some(detail.total.unwrap_or(0)), window, cx);
                // 空字符串（R20）：编辑区本身留空，上方一行弱化提示说明这是空字符串、不是没读到。
                let empty_hint = text.is_empty().then(|| {
                    div()
                        .id("redis-string-empty")
                        .debug_selector(|| "redis-string-empty".to_owned())
                        .flex_none()
                        .px(px(m.pane_pad()))
                        .py(px(Space::XS))
                        .text_size(px(TypeScale::XS))
                        .text_color(p.text_faint)
                        .child(SharedString::from(self.text("redis.value.emptyString")))
                });
                div().flex_1().min_h(px(0.)).flex().flex_col().children(empty_hint).child(div().flex_1().min_h(px(0.)).child(viewer)).into_any_element()
            }
            RedisValue::Json { text } => {
                let viewer = self.ensure_string_viewer(text, None, window, cx);
                div().id("redis-json-value").test_support().flex_1().min_h(px(0.)).flex().flex_col().child(div().flex_1().min_h(px(0.)).child(viewer)).into_any_element()
            }
            RedisValue::None {} => div()
                .id("redis-key-gone")
                .test_support()
                .flex_1()
                .p(px(m.pane_pad() * 2.))
                .child(kit::empty_state_with_action(
                    IconName::Key,
                    SharedString::from(self.text("redis.keys.missing")),
                    SharedString::from(self.text("redis.value.gone")),
                    action("redis-refresh", Some(IconName::RefreshCw), &self.text("redis.tab.refresh"), Kind::Secondary, entity).into_any_element(),
                    p,
                ))
                .into_any_element(),
            _ => stack_frame(|| self.collection_body(&detail, p, m, entity, detail_w, window, cx)),
        };
        div().flex_1().min_h(px(0.)).flex().flex_col().when(stale, |this| this.opacity(0.6)).child(body).into_any_element()
    }

    /// 集合条目：筛选、排序后的行（条目下标 + 各列文字）、列定义、添加按钮文案。
    pub(super) fn entry_lines(&self, detail: &RedisKeyDetail) -> (Vec<ReadColumn>, Vec<EntryLine>, String) {
        let needle = self.hash_query.trim().to_lowercase();
        let hit = |text: &str| needle.is_empty() || text.to_lowercase().contains(&needle);
        // 空字符串（R20）就是空单元格，复制得到空文本；其余折成一行显示。
        let cell = |text: &str| match value_cell(text) {
            ValueCell::Empty => SharedString::default(),
            ValueCell::Text(line) => SharedString::from(line),
        };
        let numeric = |text: &str| text.trim().parse::<f64>().unwrap_or(f64::NAN);
        let mut lines = Vec::new();
        let (columns, add) = match &detail.value {
            RedisValue::Hash { entries } => {
                for (index, entry) in entries.iter().enumerate().filter(|(_, e)| hit(&e.field) || hit(&e.value)) {
                    lines.push(EntryLine { index, cells: vec![cell(&entry.field), cell(&entry.value)], sort: (f64::NAN, entry.field.clone()) });
                }
                (vec![ReadColumn::new(self.text("redis.value.field")).width(180.), ReadColumn::new(self.text("redis.value.value")).width(260.)], self.text("redis.hash.add"))
            }
            RedisValue::List { items } => {
                for (index, value) in items.iter().enumerate().filter(|(i, v)| hit(v) || hit(&i.to_string())) {
                    lines.push(EntryLine { index, cells: vec![SharedString::from(index.to_string()), cell(value)], sort: (f64::NAN, value.clone()) });
                }
                (vec![ReadColumn::new(self.text("redis.entry.index")).width(64.).right(), ReadColumn::new(self.text("redis.value.value")).width(320.)], self.text("redis.list.add"))
            }
            RedisValue::Set { items } => {
                for (index, member) in items.iter().enumerate().filter(|(_, m)| hit(m)) {
                    lines.push(EntryLine { index, cells: vec![cell(member)], sort: (f64::NAN, member.clone()) });
                }
                (vec![ReadColumn::new(self.text("redis.value.member")).width(360.)], self.text("redis.set.add"))
            }
            RedisValue::ZSet { entries } => {
                for (index, entry) in entries.iter().enumerate().filter(|(_, e)| hit(&e.member) || hit(&e.score)) {
                    lines.push(EntryLine { index, cells: vec![SharedString::from(entry.score.clone()), cell(&entry.member)], sort: (numeric(&entry.score), entry.member.clone()) });
                }
                (vec![ReadColumn::new(self.text("redis.value.score")).width(110.).right(), ReadColumn::new(self.text("redis.value.member")).width(280.)], self.text("redis.zset.add"))
            }
            RedisValue::Stream { messages } => {
                for (index, message) in messages.iter().enumerate() {
                    let summary = message.fields.iter().map(|f| format!("{}={}", f.field, f.value)).collect::<Vec<_>>().join("  ");
                    if hit(&message.id) || hit(&summary) {
                        lines.push(EntryLine { index, cells: vec![SharedString::from(message.id.clone()), cell(&summary)], sort: (f64::NAN, message.id.clone()) });
                    }
                }
                (vec![ReadColumn::new(self.text("redis.stream.id")).width(170.).mono(), ReadColumn::new(self.text("redis.value.value")).width(280.)], self.text("redis.stream.add"))
            }
            RedisValue::String { .. } | RedisValue::Json { .. } | RedisValue::None {} => (Vec::new(), String::new()),
        };
        if self.entry_sort != 0 {
            lines.sort_by(|a, b| {
                let order = match (a.sort.0.is_nan(), b.sort.0.is_nan()) {
                    (false, false) => a.sort.0.total_cmp(&b.sort.0),
                    _ => a.sort.1.cmp(&b.sort.1),
                };
                if self.entry_sort == 2 { order.reverse() } else { order }
            });
        }
        (columns, lines, add)
    }

    /// 当前选中条目交给右栏查看器的内容。
    fn inspected_entry(&self, detail: &RedisKeyDetail) -> Option<InspectedEntry> {
        let index = self.entry_focus?;
        match &detail.value {
            RedisValue::Hash { entries } => entries.get(index).map(|e| (index, e.field.clone(), e.value.clone(), Some(EntryTarget::Hash(e.field.clone())))),
            RedisValue::List { items } => items.get(index).map(|v| (index, format!("#{index}"), v.clone(), Some(EntryTarget::List(index as i64)))),
            RedisValue::Set { items } => items.get(index).map(|v| (index, v.clone(), v.clone(), None)),
            RedisValue::ZSet { entries } => entries.get(index).map(|e| (index, format!("{} · {}", e.member, e.score), e.member.clone(), None)),
            RedisValue::Stream { messages } => {
                let message = messages.get(index)?;
                let json = self.stream_payload_json(&index.to_string())?;
                Some((index, message.id.clone(), json, None))
            }
            RedisValue::String { .. } | RedisValue::Json { .. } | RedisValue::None {} => None,
        }
    }

    /// Hash / List / Set / ZSet / Stream：左边条目表（搜索、排序、多选、⌘C、已加载 / 总数），右边 FormatViewer，
    /// 中间分隔条可拖动，位置存进 Redis 偏好；详情区太窄时改为上下分栏。
    #[allow(clippy::too_many_arguments)]
    fn collection_body(
        &mut self,
        detail: &Arc<RedisKeyDetail>,
        p: &Palette,
        m: &kit::Metrics,
        entity: &Entity<Self>,
        detail_w: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let hint = self.text("redis.entry.filter");
        let query = self.redis_input(Field::HashQuery, "redis-hash-filter", "redis.entry.filter", &hint, window, cx);
        let (mut columns, lines, add_label) = self.entry_lines(detail);
        let total_loaded = match &detail.value {
            RedisValue::Hash { entries } => entries.len(),
            RedisValue::List { items } | RedisValue::Set { items } => items.len(),
            RedisValue::ZSet { entries } => entries.len(),
            RedisValue::Stream { messages } => messages.len(),
            _ => 0,
        };
        let stacked = detail_w < VALUE_STACK_W;
        // 条目表能用的宽度：值面板收起或上下分栏时是整个详情区，否则是左栏。
        let table_w = if self.prefs.inspector_hidden || stacked { detail_w } else { self.value_left_w(detail_w) };
        let data_stamp = {
            use std::hash::{Hash as _, Hasher as _};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            (Arc::as_ptr(detail) as usize).hash(&mut hasher);
            self.hash_query.trim().hash(&mut hasher);
            self.entry_sort.hash(&mut hasher);
            matches!(self.locale, Locale::En).hash(&mut hasher);
            hasher.finish()
        };
        self.fit_entry_columns(&mut columns, &lines, data_stamp, table_w, window, cx);
        let stamp = {
            use std::hash::{Hash as _, Hasher as _};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            data_stamp.hash(&mut hasher);
            columns.iter().map(|column| column.width.round() as i32).collect::<Vec<_>>().hash(&mut hasher);
            hasher.finish()
        };
        let rows: Vec<Vec<SharedString>> = lines.iter().map(|line| line.cells.clone()).collect();
        let mapping: Vec<usize> = lines.iter().map(|line| line.index).collect();
        // 映射相同也要看详情是否换了：两个字段数相同的 Hash 之间切换，映射一样，但表里的选中必须回到当前条目。
        let same_detail = self.entry_rows_detail.as_ref().is_some_and(|seen| Arc::ptr_eq(seen, detail));
        let view = (self.selected.clone(), detail.key_type.clone(), self.hash_query.trim().to_owned(), self.entry_sort);
        // 只有「加载更多」落地的追加页（finish_value_page 记下的 旧详情 → 新详情，且旧详情正是表上画着的那份）
        // 才按追加处理：滚动位置不动、不回弹到选中行。换键再回来、刷新、写后重读即使条目变多也按重建处理。
        // 判断不看可见行：有筛选时新页可能一条都不匹配（可见行不变），有排序时新行会插到中间。
        let page_landed = !same_detail
            && self.entry_append.as_ref().is_some_and(|(base, landed)| {
                Arc::ptr_eq(landed, detail) && self.entry_rows_detail.as_ref().is_some_and(|seen| Arc::ptr_eq(seen, base))
            });
        if !same_detail {
            self.entry_append = None;
        }
        let appended = page_landed
            && self.entry_rows_view.as_ref() == Some(&view)
            && total_loaded >= self.entry_rows_loaded
            && self.tables.contains_key(tables::ENTRY_TABLE);
        let changed = !appended && (self.entry_rows != mapping || !same_detail || !self.tables.contains_key(tables::ENTRY_TABLE));
        // 追加页里当前条目换了行号（排序视图）：选中跟过去，但不滚动。
        let moved_focus = if appended {
            self.entry_focus.and_then(|index| {
                let old = self.entry_rows.iter().position(|row| *row == index);
                let new = mapping.iter().position(|row| *row == index);
                new.filter(|new| old != Some(*new))
            })
        } else {
            None
        };
        self.entry_rows = mapping;
        self.entry_rows_detail = Some(Arc::clone(detail));
        self.entry_rows_view = Some(view);
        self.entry_rows_loaded = total_loaded;
        let empty = if total_loaded == 0 { self.text("redis.value.empty") } else { self.text("redis.entry.noMatch") };
        let table = self.table_slot_with(tables::ENTRY_TABLE, stamp, move |_| (columns, rows), Some(SharedString::from(empty)), appended, window, cx);
        if changed {
            // 换了键或筛选：表里的选中跟着当前条目（默认第一行）。
            let target = self.entry_focus.and_then(|index| self.entry_rows.iter().position(|row| *row == index)).or((!self.entry_rows.is_empty()).then_some(0));
            if let Some(row) = target {
                self.entry_focus = self.entry_rows.get(row).copied();
                table.update(cx, |table, cx| table.select_row(row, false, cx));
            }
        } else if let Some(row) = moved_focus {
            table.update(cx, |table, cx| table.select_row_keep_view(row, cx));
        }

        let loaded = match detail.total {
            Some(total) => i18n::fill(&self.text("redis.count.loaded"), &[("n", &total_loaded.to_string()), ("total", &total.to_string())]),
            None => i18n::fill(&self.text("redis.count.loadedOnly"), &[("n", &total_loaded.to_string())]),
        };
        let count_text = if self.hash_query.trim().is_empty() {
            loaded
        } else {
            format!("{} · {loaded}", i18n::fill(&self.text("redis.count.matched"), &[("n", &self.entry_rows.len().to_string())]))
        };
        let focus = self.entry_focus;
        let (edit_id, del_id) = match (&detail.value, focus) {
            (RedisValue::Hash { .. }, Some(i)) => (Some(format!("redis-hash-edit:{i}")), Some(format!("redis-hash-del:{i}"))),
            (RedisValue::List { .. }, Some(i)) => (Some(format!("redis-list-edit:{i}")), Some(format!("redis-list-rem:{i}"))),
            (RedisValue::ZSet { .. }, Some(i)) => (Some(format!("redis-zset-edit:{i}")), Some(format!("redis-zrem:{i}"))),
            (RedisValue::Set { .. }, Some(i)) => (None, Some(format!("redis-set-rem:{i}"))),
            (RedisValue::Stream { .. }, Some(i)) => (None, Some(format!("redis-xdel:{i}"))),
            _ => (None, None),
        };
        let sort_icon = match self.entry_sort {
            1 => IconName::ArrowUpNarrowWide,
            2 => IconName::ArrowDownWideNarrow,
            _ => IconName::ArrowUpDown,
        };
        let toolbar = div()
            .id("redis-entry-tools")
            .test_support()
            .flex_none()
            .flex()
            .flex_nowrap()
            .items_center()
            .gap(px(Space::XS))
            .px(px(Space::MD))
            .py(px(Space::SM))
            .border_b_1()
            .border_color(p.line_soft)
            .child(
                div().flex_1().min_w(px(0.)).child(
                    kit::text_input("redis-hash-filter", &query, self.text("redis.entry.filter"))
                        .small()
                        .cleanable(true)
                        .prefix(kit::input_prefix(IconName::Search, IconSize::INLINE, p.text_faint)),
                ),
            )
            .child(icon_btn("redis-entry-sort", sort_icon, &self.text("redis.entry.sort"), entity).selected(self.entry_sort != 0))
            .when_some(edit_id, |this, id| this.child(icon_btn(id, IconName::Pencil, &self.text("redis.entry.edit"), entity)))
            .when_some(del_id, |this, id| this.child(icon_btn(id, IconName::Trash, &self.text("redis.entry.delete"), entity)))
            .child(
                icon_btn("redis-entry-form", if self.entry_form_open { IconName::ChevronUp } else { IconName::Plus }, &add_label, entity)
                    .selected(self.entry_form_open),
            )
            .child(div().flex_none().w(px(DIVIDER_W)).h(px(IconSize::INLINE)).bg(p.line))
            .child(
                icon_btn(
                    "redis-inspector-toggle",
                    IconName::PanelRight,
                    &self.text(if self.prefs.inspector_hidden { "redis.inspector.show" } else { "redis.inspector.hide" }),
                    entity,
                )
                .selected(!self.prefs.inspector_hidden),
            );
        let footer = div()
            .id("redis-entry-count")
            .test_support()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(Space::SM))
            .px(px(Space::MD))
            .h(px(m.bar_h()))
            .border_t_1()
            .border_color(p.line_soft)
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
            .when_some(self.value_more_error.clone(), |this, reason| {
                // 加载更多失败：原因 + 重试，已读到的条目和滚动位置都不动。
                this.child(kit::icon(IconName::CircleAlert, IconSize::INLINE, p.danger)).child(
                    div()
                        .id("redis-value-more-error")
                        .test_support()
                        .flex_shrink(1.)
                        .min_w(px(0.))
                        .max_w(relative(0.5))
                        .truncate()
                        .text_size(px(TypeScale::XS))
                        .text_color(p.danger)
                        .tooltip_text(SharedString::from(reason.clone()))
                        .child(SharedString::from(i18n::fill(&self.text("redis.value.moreFailed"), &[("reason", &reason)]))),
                )
            })
            .when(detail.next.is_some(), |this| {
                let failed = self.value_more_error.is_some();
                let (icon, label) = if failed { (IconName::RefreshCw, "common.retry") } else if self.value_loading { (IconName::ChevronsDown, "redis.value.loading") } else { (IconName::ChevronsDown, "redis.value.more") };
                this.child(action("redis-value-more", Some(icon), &self.text(label), Kind::Tool, entity).small().loading(self.value_loading).disabled(self.value_loading))
            })
            .when(detail.next.is_none() && total_loaded > 0, |this| {
                let text = i18n::fill(&self.text("redis.value.allLoaded"), &[("n", &total_loaded.to_string())]);
                this.child(
                    div()
                        .id("redis-value-all-loaded")
                        .test_support()
                        .role(gpui_kit::Role::Status)
                        .aria_label(SharedString::from(text.clone()))
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(Space::XS))
                        .text_size(px(TypeScale::XS))
                        .font_features(kit::tabular_figures())
                        .text_color(p.text_faint)
                        .child(kit::icon(IconName::Check, IconSize::INLINE, p.text_faint))
                        .child(SharedString::from(text)),
                )
            });
        let form = if self.entry_form_open { Some(self.render_entry_form(detail, p, entity, window, cx)) } else { None };
        // 没有条目 / 筛选没命中：用统一的状态块替换表格（添加入口在工具栏，筛选框就在上面）。
        let table_area = if self.entry_rows.is_empty() {
            let block = if total_loaded == 0 {
                kit::empty_state(IconName::Inbox, SharedString::from(self.text("redis.entry.emptyHeading")), SharedString::from(self.text("redis.value.empty")), p).into_any_element()
            } else {
                kit::no_match_state(IconName::Search, SharedString::from(self.text("redis.entry.noMatchHeading")), SharedString::from(self.text("redis.entry.noMatch")), p).into_any_element()
            };
            div().id("redis-entries-state").test_support().flex_1().min_h(px(0.)).p(px(Space::MD)).child(block).into_any_element()
        } else {
            div().flex_1().min_h(px(0.)).child(table).into_any_element()
        };
        let left = div()
            .id("redis-entries")
            .test_support()
            .flex()
            .flex_col()
            .min_w(px(0.))
            .min_h(px(0.))
            .child(toolbar)
            .children(form)
            .child(table_area)
            .child(footer);

        if self.prefs.inspector_hidden {
            // 值面板收起：条目表占满整个值区。
            return div().id("redis-value-split").test_support().flex_1().min_h(px(0.)).flex().child(left.flex_1()).into_any_element();
        }
        let inspected = self.inspected_entry(detail);
        let right = self.render_entry_inspector(inspected, p, window, cx);

        if stacked {
            return div()
                .id("redis-value-split")
                .test_support()
                .flex_1()
                .min_h(px(0.))
                .flex()
                .flex_col()
                .child(left.h(relative(0.45)).flex_none().border_b_1().border_color(p.line))
                .child(div().flex_1().min_h(px(0.)).flex().flex_col().child(right))
                .into_any_element();
        }
        let left_w = self.value_left_w(detail_w);
        let dragging = self.value_drag_start.is_some();
        let splitter = div()
            .id("redis-value-splitter")
            .test_support()
            .group("redis-value-splitter")
            .flex_none()
            .w(px(kit::RESIZE_HANDLE_W))
            .ml(px(-kit::RESIZE_HANDLE_W / 2.))
            .mr(px(-kit::RESIZE_HANDLE_W / 2.))
            .h_full()
            .flex()
            .justify_center()
            .cursor_col_resize()
            .child(div().w(px(DIVIDER_W)).h_full().bg(if dragging { p.accent } else { p.line }).group_hover("redis-value-splitter", |s| s.bg(p.accent.opacity(0.65))))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    if event.click_count >= 2 {
                        this.prefs.value_split = 0;
                        this.value_drag_start = None;
                        this.persist_prefs(cx);
                    } else {
                        this.value_drag_start = Some((f32::from(event.position.x), left_w));
                    }
                    cx.notify();
                }),
            );
        div()
            .id("redis-value-split")
            .test_support()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .child(left.flex_none().w(px(left_w)))
            .child(splitter)
            .child(div().flex_1().min_w(px(0.)).flex().flex_col().child(right))
            .into_any_element()
    }

    /// 条目表列宽：首列（字段、下标、分数、ID）按内容测量，上限 [`ENTRY_FIRST_MAX_W`]；
    /// 值列占满剩下的宽度。测量结果按数据戳缓存，拖分隔条只重算值列。
    pub(super) fn fit_entry_columns(&mut self, columns: &mut [ReadColumn], lines: &[EntryLine], stamp: u64, table_w: f32, window: &Window, cx: &gpui_kit::App) {
        let available = (table_w - ENTRY_TABLE_CHROME_W).max(0.);
        match columns {
            [] => {}
            [only] => only.width = available.max(ENTRY_VALUE_MIN_W),
            [first, rest @ ..] => {
                let width = match self.entry_first_w {
                    Some((seen, width)) if seen == stamp => width,
                    _ => {
                        let family = if first.mono { cx.theme().mono_font_family.clone() } else { cx.theme().font_family.clone() };
                        let font = gpui_kit::Font { family, ..Default::default() };
                        let measure = |text: &str| Self::measure_ui(window, &font, TypeScale::SM, text);
                        let widest = lines
                            .iter()
                            .take(ENTRY_MEASURE_ROWS)
                            .filter_map(|line| line.cells.first())
                            .map(|cell| measure(cell))
                            .fold(measure(&first.title) + Space::LG, f32::max);
                        let width = (widest + Space::MD * 2. + Space::XS).clamp(ENTRY_FIRST_MIN_W, ENTRY_FIRST_MAX_W);
                        self.entry_first_w = Some((stamp, width));
                        width
                    }
                };
                first.width = width;
                let share = ((available - width) / rest.len().max(1) as f32).max(ENTRY_VALUE_MIN_W);
                for column in rest {
                    column.width = share;
                }
            }
        }
    }

    /// 左栏宽度：偏好里保存的宽度（0 = 默认比例），夹在表格最窄与查看器最窄之间。
    pub(super) fn value_left_w(&self, detail_w: f32) -> f32 {
        let saved = self.prefs.value_split as f32;
        let wanted = if saved > 0. { saved } else { detail_w * VALUE_SPLIT_RATIO };
        let max = (detail_w - VALUE_VIEWER_MIN_W).max(VALUE_TABLE_MIN_W);
        wanted.clamp(VALUE_TABLE_MIN_W, max)
    }

    fn render_entry_form(&mut self, detail: &RedisKeyDetail, p: &Palette, entity: &Entity<Self>, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let mode_label = match &detail.value {
            RedisValue::Hash { entries } if !self.entry_field.is_empty() && entries.iter().any(|e| e.field == self.entry_field) => {
                i18n::fill(&self.text("redis.entry.editing"), &[("name", &self.entry_field)])
            }
            RedisValue::List { .. } if self.entry_index.trim().parse::<usize>().is_ok() => {
                i18n::fill(&self.text("redis.entry.editing"), &[("name", &format!("#{}", self.entry_index.trim()))])
            }
            RedisValue::ZSet { entries } if !self.entry_member.is_empty() && entries.iter().any(|e| e.member == self.entry_member) => {
                i18n::fill(&self.text("redis.entry.editing"), &[("name", &self.entry_member)])
            }
            _ => self.text("redis.entry.adding"),
        };
        let header = div()
            .w_full()
            .flex()
            .items_center()
            .gap(px(Space::SM))
            .child(div().flex_1().min_w(px(0.)).truncate().text_size(px(TypeScale::XS)).font_weight(FontWeight::SEMIBOLD).text_color(p.text_muted).child(SharedString::from(mode_label)))
            .child(icon_btn("redis-entry-form-close", IconName::X, &self.text("redis.entry.close"), entity).xsmall());
        let fields_row = div().w_full().flex().flex_wrap().items_center().gap(px(Space::SM));
        let field_w = |w: f32| div().w(px(w)).min_w(px(0.));
        let grow = || div().flex_1().min_w(px(VALUE_TABLE_MIN_W / 2.));
        let fields_row = match &detail.value {
            RedisValue::Hash { .. } => {
                let field = self.redis_input(Field::EntryField, "redis-hash-field", "redis.value.field", &self.text("redis.value.field"), window, cx);
                let value = self.redis_input(Field::EntryValue, "redis-hash-value", "redis.value.value", &self.text("redis.value.value"), window, cx);
                fields_row
                    .child(field_w(VALUE_TABLE_MIN_W * 0.6).child(kit::text_input("redis-hash-field", &field, self.text("redis.value.field")).small()))
                    .child(grow().child(kit::text_input("redis-hash-value", &value, self.text("redis.value.value")).small()))
                    .child(action("redis-hash-add", Some(IconName::Check), &self.text("redis.hash.save"), Kind::Primary, entity).small())
            }
            RedisValue::List { .. } => {
                let value = self.redis_input(Field::EntryValue, "redis-list-value", "redis.value.value", &self.text("redis.value.value"), window, cx);
                let fields_row = fields_row.child(grow().child(kit::text_input("redis-list-value", &value, self.text("redis.value.value")).small()));
                match self.entry_index.trim().parse::<usize>() {
                    Ok(index) => fields_row.child(
                        action(
                            format!("redis-list-set:{index}"),
                            Some(IconName::Check),
                            &i18n::fill(&self.text("redis.list.setAt"), &[("index", &index.to_string())]),
                            Kind::Primary,
                            entity,
                        )
                        .small(),
                    ),
                    Err(_) => fields_row
                        .child(action("redis-list-lpush", Some(IconName::ArrowUp), &self.text("redis.list.lpush"), Kind::Secondary, entity).small())
                        .child(action("redis-list-rpush", Some(IconName::ArrowDown), &self.text("redis.list.rpush"), Kind::Primary, entity).small()),
                }
            }
            RedisValue::Set { .. } => {
                let member = self.redis_input(Field::EntryMember, "redis-set-member", "redis.value.member", &self.text("redis.value.member"), window, cx);
                fields_row
                    .child(grow().child(kit::text_input("redis-set-member", &member, self.text("redis.value.member")).small()))
                    .child(action("redis-set-add", Some(IconName::Check), &self.text("redis.set.add"), Kind::Primary, entity).small())
            }
            RedisValue::ZSet { .. } => {
                let score = self.redis_input(Field::EntryScore, "redis-zset-score", "redis.value.score", &self.text("redis.value.score"), window, cx);
                let member = self.redis_input(Field::EntryMember, "redis-zset-member", "redis.value.member", &self.text("redis.value.member"), window, cx);
                fields_row
                    .child(field_w(VALUE_TABLE_MIN_W * 0.4).child(kit::text_input("redis-zset-score", &score, self.text("redis.value.score")).small()))
                    .child(grow().child(kit::text_input("redis-zset-member", &member, self.text("redis.value.member")).small()))
                    .child(action("redis-zadd", Some(IconName::Check), &self.text("redis.zset.add"), Kind::Primary, entity).small())
            }
            RedisValue::Stream { .. } => {
                let id = self.redis_input(Field::StreamId, "redis-stream-id", "redis.stream.id", "*", window, cx);
                let field = self.redis_input(Field::StreamField, "redis-stream-field", "redis.value.field", &self.text("redis.value.field"), window, cx);
                let value = self.redis_input(Field::StreamValue, "redis-stream-value", "redis.value.value", &self.text("redis.value.value"), window, cx);
                fields_row
                    .child(field_w(VALUE_TABLE_MIN_W * 0.5).child(kit::text_input("redis-stream-id", &id, self.text("redis.stream.id")).small()))
                    .child(field_w(VALUE_TABLE_MIN_W * 0.6).child(kit::text_input("redis-stream-field", &field, self.text("redis.value.field")).small()))
                    .child(grow().child(kit::text_input("redis-stream-value", &value, self.text("redis.value.value")).small()))
                    .child(action("redis-xadd", Some(IconName::Check), &self.text("redis.stream.add"), Kind::Primary, entity).small())
            }
            RedisValue::String { .. } | RedisValue::Json { .. } | RedisValue::None {} => fields_row,
        };
        div()
            .id("redis-entry-form")
            .test_support()
            .flex_none()
            .px(px(Space::MD))
            .py(px(Space::SM))
            .bg(p.sunken)
            .border_b_1()
            .border_color(p.line_soft)
            .flex()
            .flex_col()
            .gap(px(Space::XS))
            .child(header)
            .child(fields_row)
            .into_any_element()
    }

    /// 右栏：FormatViewer 的工具条就是右栏头部（标题 = 当前字段名或成员名），没有单独的「结构化值查看器」标题。
    fn render_entry_inspector(&mut self, inspected: Option<InspectedEntry>, p: &Palette, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some((idx, label, raw, target)) = inspected else {
            return div()
                .id("redis-entry-pick")
                .test_support()
                .flex_1()
                .p(px(Space::XL))
                .bg(p.chrome)
                .child(kit::empty_state(
                    IconName::PanelRight,
                    SharedString::from(self.text("redis.inspector.pickHeading")),
                    SharedString::from(self.text("redis.inspector.pickEntry")),
                    p,
                ))
                .into_any_element();
        };
        let read_only = target.is_none();
        let tag = content_tag(&self.selected, idx, &raw);
        let viewer = self.ensure_entry_viewer(&tag, &label, &raw, target, window, cx);
        // 只读条目（Set 成员、ZSet 成员、Stream 消息）：查看器上方一条统一的只读提示，不让人以为能直接改。
        let notice = read_only.then(|| {
            div()
                .id("redis-entry-readonly")
                .test_support()
                .flex_none()
                .child(kit::banner(kit::BannerTone::Info, IconName::Lock, SharedString::from(self.text("redis.inspector.readOnly")), None, p))
        });
        div().id("redis-entry-inspector").test_support().flex_1().min_h(px(0.)).flex().flex_col().children(notice).child(viewer).into_any_element()
    }

    /// 字符串 / JSON 的查看器：复用同一个实体，换键或值变了用 `set_content` 原地换内容。
    /// `preview_total`：大字符串只读了前缀时为总字节数（预览态，可「加载全部」）。
    fn ensure_string_viewer(&mut self, text: &str, preview_total: Option<u64>, window: &mut Window, cx: &mut Context<Self>) -> Entity<FormatViewer> {
        let key = self.selected.clone();
        let spec = |locale| ViewerSpec {
            text: text.to_owned(),
            column_hint: Some(key.clone()),
            type_hint: Some("string".into()),
            bytes: None,
            truncation: match preview_total {
                Some(total_bytes) => TruncationState::Preview { total_bytes, can_load_full: true },
                None => TruncationState::Full,
            },
            mode: EditMode::Editable,
            locale,
        };
        match self.string_viewer.clone() {
            None => {
                let viewer = cx.new(|cx| FormatViewer::new(spec(self.locale), window, cx));
                let sub = cx.subscribe(&viewer, |this, viewer, event, cx| match event {
                    FormatViewerEvent::Commit(value) => {
                        if this.selected.is_empty() {
                            return;
                        }
                        let value = value.clone();
                        let original = viewer.read(cx).source().to_owned();
                        let key = this.selected.clone();
                        let json = this.detail.as_deref().is_some_and(|detail| matches!(detail.value, RedisValue::Json { .. }));
                        let (mutation, probe) = if json {
                            (RedisMutation::JsonSet { key: key.clone(), value }, keys::ValueProbe::Json { key })
                        } else {
                            (RedisMutation::SetString { key: key.clone(), value }, keys::ValueProbe::String { key })
                        };
                        this.guarded_stage(mutation, probe, original, cx);
                        cx.notify();
                    }
                    FormatViewerEvent::LoadFull => {
                        viewer.update(cx, |viewer, cx| viewer.set_truncation(TruncationState::Loading, cx));
                        this.load_full_string(cx);
                    }
                    FormatViewerEvent::Close => {}
                });
                self.string_sub = Some(sub);
                self.string_viewer = Some(viewer);
            }
            Some(viewer) => {
                let key_changed = self.viewer_key != key;
                if key_changed || self.reload_viewer {
                    // 同一个键重读：没改过，或改过的草稿已写进 Redis（读回来和草稿一致）才换成最新值。
                    let (dirty, draft) = {
                        let current = viewer.read(cx);
                        (current.dirty(), current.draft().to_owned())
                    };
                    if key_changed || !dirty || draft == text {
                        let locale = self.locale;
                        viewer.update(cx, |viewer, cx| viewer.set_content(spec(locale), window, cx));
                    }
                }
            }
        }
        self.viewer_key = key;
        self.reload_viewer = false;
        self.string_viewer.clone().expect("string viewer")
    }

    /// 集合条目的查看器（自动识别 JSON / XML / YAML / Hex）：同一个实体，换条目用 `set_content`。
    /// Hash 字段值与 List 元素可直接改，「应用」先核对值没被别人改过，再走与其它写操作相同的确认；其余只读。
    fn ensure_entry_viewer(&mut self, tag: &str, title: &str, text: &str, target: Option<EntryTarget>, window: &mut Window, cx: &mut Context<Self>) -> Entity<FormatViewer> {
        let spec = ViewerSpec {
            text: text.to_owned(),
            column_hint: Some(title.to_owned()),
            type_hint: Some("text".into()),
            bytes: None,
            truncation: TruncationState::Full,
            mode: if target.is_some() { EditMode::Editable } else { EditMode::ReadOnly },
            locale: self.locale,
        };
        let title = Some(SharedString::from(title.to_owned()));
        let close_label = SharedString::from(self.text("redis.inspector.hide"));
        match self.entry_viewer.clone() {
            None => {
                let viewer = cx.new(|cx| {
                    let mut viewer = FormatViewer::new(spec, window, cx);
                    viewer.set_title(title, cx);
                    viewer.set_closable(Some(close_label.clone()), cx);
                    viewer
                });
                let sub = cx.subscribe(&viewer, |this, viewer, event, cx| {
                    if matches!(event, FormatViewerEvent::Close) {
                        this.command("redis-inspector-toggle", cx);
                        return;
                    }
                    let FormatViewerEvent::Commit(value) = event else { return };
                    let key = this.selected.clone();
                    let original = viewer.read(cx).source().to_owned();
                    let (mutation, probe) = match this.entry_target.clone() {
                        Some(EntryTarget::Hash(field)) => (
                            RedisMutation::HashSet { key: key.clone(), field: field.clone(), value: value.clone() },
                            keys::ValueProbe::HashField { key, field },
                        ),
                        Some(EntryTarget::List(index)) => (RedisMutation::ListSet { key: key.clone(), index, value: value.clone() }, keys::ValueProbe::ListIndex { key, index }),
                        None => return,
                    };
                    this.guarded_stage(mutation, probe, original, cx);
                    cx.notify();
                });
                self.entry_viewer = Some(viewer);
                self.entry_viewer_sub = Some(sub);
            }
            Some(viewer) => {
                if self.entry_viewer_tag != tag || self.entry_target != target {
                    viewer.update(cx, |viewer, cx| {
                        viewer.set_content(spec, window, cx);
                        viewer.set_title(title, cx);
                    });
                }
                viewer.update(cx, |viewer, cx| viewer.set_closable(Some(close_label), cx));
            }
        }
        self.entry_viewer_tag = tag.to_owned();
        self.entry_target = target;
        self.entry_viewer.clone().expect("entry viewer")
    }

    /// 命令栏输入框：输入框本身的文字透明，上面叠一层按 Redis 词法着色的 StyledText（与查询编辑器同一套分类）。
    /// 文字超出可见宽度（输入框会横向滚动）或为空时不叠加，直接显示输入框自己的文字。
    fn command_field(&self, command: &Entity<InputState>, p: &Palette, mono: &SharedString, cx: &mut Context<Self>) -> AnyElement {
        use gpui_kit::component::input::HighlightStyleResolver as _;
        let text = self.command_input.clone();
        // 可用宽度：控制台占满整页，减去两侧边距、提示符与执行按钮。
        let available = self.viewport_w - Space::XL * 2. - IconSize::RAIL * 2. - ControlSize::PICKER_MIN_W / 2.;
        let fits = !text.is_empty() && !text.contains('\n') && text.chars().count() as f32 * TypeScale::SM * MONO_ADVANCE < available;
        let tokens = if fits { crate::ui::redis_highlight::lex(&text) } else { Vec::new() };
        let theme = &cx.theme().highlight_theme;
        let highlights: Vec<(std::ops::Range<usize>, gpui_kit::HighlightStyle)> = tokens
            .iter()
            .filter_map(|token| {
                let name = token.kind.highlight()?;
                theme.style(name).map(|style| (token.range.clone(), style))
            })
            .collect();
        let input = kit::text_input("redis-command", command, self.text("redis.tab.commandLabel"))
            .small()
            .cleanable(true)
            .font_family(mono.clone())
            .text_size(px(TypeScale::SM))
            .when(fits, |this| this.text_color(gpui_kit::transparent_black()));
        let overlay = fits.then(|| {
            div()
                .id("redis-command-highlight")
                .test_support()
                .absolute()
                .top_0()
                .bottom_0()
                .left(px(Space::MD + DIVIDER_W))
                .right(px(Space::XXL))
                .flex()
                .items_center()
                .overflow_hidden()
                .whitespace_nowrap()
                .font_family(mono.clone())
                .text_size(px(TypeScale::SM))
                .text_color(p.text)
                .child(gpui_kit::StyledText::new(SharedString::from(text.clone())).with_highlights(highlights))
        });
        div().id("redis-command-field").test_support().relative().flex_1().min_w(px(0.)).child(input).children(overlay).into_any_element()
    }

    /// 状态行：最近一次操作的提示（读屏与测试按 `redis-status` 取当前状态）。放在顶部栏右端。
    pub(super) fn render_status(&self, p: &Palette) -> AnyElement {
        div()
            .id("redis-status")
            .test_support()
            .role(gpui_kit::Role::Status)
            .aria_label(SharedString::from(self.status_line()))
            .flex_none()
            .min_w(px(16.))
            .max_w(px(280.))
            .min_h(px(16.))
            .flex()
            .items_center()
            .gap(px(Space::XS))
            .text_size(px(TypeScale::XS))
            // 执行状态是次要信息：比页签更弱。
            .text_color(p.text_faint)
            .when(!self.note.is_empty(), |this| this.child(kit::icon(IconName::CircleCheck, 12., p.ok.opacity(0.8))))
            .child(div().min_w(px(0.)).truncate().child(SharedString::from(self.note.clone())))
            .into_any_element()
    }

    /// 键页上的操作错误（删除、改 TTL 等失败）：统一的错误块，可关闭。
    pub(super) fn render_command_error(&self, p: &Palette, m: &kit::Metrics, entity: &Entity<Self>) -> Option<AnyElement> {
        if self.error_source != ErrorSource::Command || self.error.is_empty() || self.new_open {
            return None;
        }
        let keys_blocked = self.error.contains("KEYS");
        let actions = div()
            .flex()
            .items_center()
            .gap(px(Space::SM))
            .when(self.stale_value, |this| {
                this.child(action("redis-stale-refresh", Some(IconName::RefreshCw), &self.text("redis.retry.read"), Kind::Primary, entity).small())
            })
            .when(keys_blocked, |this| this.child(action("redis-use-scan", Some(IconName::Search), &self.text("redis.console.useScan"), Kind::Primary, entity).small()))
            .child(icon_btn("redis-error-dismiss", IconName::X, &self.text("redis.error.dismiss"), entity))
            .into_any_element();
        Some(
            div()
                .id("error-state")
                .test_support()
                .flex_none()
                .px(px(m.pane_pad()))
                .pt(px(Space::SM))
                .child(kit::banner(kit::BannerTone::Danger, IconName::CircleAlert, format!("{} · {}", self.text("redis.error.command.heading"), self.error), Some(actions), p))
                .into_any_element(),
        )
    }

    /// 控制台页：上面是执行记录（提示符 + 命令 + 输出，和 redis-cli 一样从上往下排，新结果自动滚到底），
    /// 下面是命令输入。错误直接写在对应命令下面。
    pub(super) fn render_console(&mut self, p: &Palette, m: &kit::Metrics, entity: &Entity<Self>, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let command = self.redis_input(Field::Command, "redis-command", "redis.tab.commandLabel", "GET key", window, cx);
        let mono = cx.theme().mono_font_family.clone();
        let pad = m.pane_pad();
        let spec = complete::lookup_command(&self.command_input);
        let prompt = |db: &str| format!("db{db}>");

        let mut log = div()
            .id("redis-console-output")
            .test_support()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .flex_col()
            .px(px(pad))
            .py(px(Space::SM))
            .font_family(mono.clone())
            .text_size(px(TypeScale::SM));
        if self.console_log.is_empty() && self.console_pending.is_none() {
            let mut chips = div().flex().flex_wrap().gap(px(Space::XS));
            for cmd in ["PING", "DBSIZE", "INFO server", "SLOWLOG GET 10", "CLIENT LIST", "CONFIG GET maxmemory"] {
                let id = format!("redis-console-quick:{cmd}");
                chips = chips.child(
                    kit::interactive_states(div().id(SharedString::from(id.clone())), p, false)
                        .test_support()
                        .role(gpui_kit::Role::Button)
                        .aria_label(SharedString::from(cmd))
                        .px(px(Space::SM))
                        .py(px(Space::XS))
                        .rounded(px(Radius::CONTROL))
                        .border_1()
                        .border_color(p.line)
                        .bg(p.chrome)
                        .hover(|s| s.bg(p.hover).border_color(p.line_strong))
                        .font_family(mono.clone())
                        .text_size(px(TypeScale::XS))
                        .text_color(p.text)
                        .child(SharedString::from(cmd))
                        .on_click(cmd_click(entity, id)),
                );
            }
            log = log.child(
                div()
                    .id("redis-console-empty")
                    .test_support()
                    .py(px(Space::XL))
                    .flex()
                    .flex_col()
                    .gap(px(Space::MD))
                    .font_family(cx.theme().font_family.clone())
                    .child(div().text_size(px(TypeScale::SM)).text_color(p.text_muted).child(SharedString::from(self.text("redis.console.emptyHint"))))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(Space::XS))
                            .child(kit::eyebrow(SharedString::from(self.text("redis.console.quickTitle")), p))
                            .child(chips),
                    ),
            );
        }
        for (index, entry) in self.console_log.iter().enumerate() {
            let elapsed = entry.elapsed.map(|d| if d.as_millis() >= 1 { format!("{} ms", d.as_millis()) } else { format!("{} µs", d.as_micros()) });
            let tokens = crate::ui::redis_highlight::lex(&entry.line);
            let theme = &cx.theme().highlight_theme;
            let highlights: Vec<(std::ops::Range<usize>, gpui_kit::HighlightStyle)> = tokens
                .iter()
                .filter_map(|token| theme.style(token.kind.highlight()?).map(|style| (token.range.clone(), style)))
                .collect();
            let group = SharedString::from(format!("redis-console-entry-{index}"));
            log = log.child(
                div()
                    .id(SharedString::from(format!("redis-console-entry:{index}")))
                    .test_support()
                    .group(group.clone())
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap(px(Space::XXS))
                    .py(px(Space::XS))
                    .when(index > 0, |this| this.border_t_1().border_color(p.line_soft))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(Space::SM))
                            .min_h(px(m.chip_h()))
                            .child(div().flex_none().text_color(p.text_faint).child(SharedString::from(prompt(&entry.db))))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .truncate()
                                    .text_color(p.text)
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(gpui_kit::StyledText::new(SharedString::from(entry.line.clone())).with_highlights(highlights)),
                            )
                            .when_some(elapsed, |this, text| {
                                this.child(
                                    div()
                                        .flex_none()
                                        .font_family(cx.theme().font_family.clone())
                                        .text_size(px(TypeScale::XS))
                                        .font_features(kit::tabular_figures())
                                        .text_color(p.text_faint)
                                        .child(SharedString::from(text)),
                                )
                            })
                            .child(
                                div()
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .gap(px(Space::XXS))
                                    .invisible()
                                    .group_hover(group.clone(), |s| s.visible())
                                    .child(icon_btn(format!("redis-console-rerun:{index}"), IconName::CornerDownLeft, &self.text("redis.console.rerun"), entity).xsmall())
                                    .child(icon_btn(format!("redis-console-copy:{index}"), IconName::Copy, &self.text("redis.console.copyOutput"), entity).xsmall()),
                            ),
                    )
                    .child(
                        div()
                            .pl(px(Space::MD))
                            .border_l_2()
                            .border_color(if entry.ok { p.line } else { p.danger.opacity(0.6) })
                            .text_color(if entry.ok { p.text_muted } else { p.danger })
                            .child(SharedString::from(if entry.output.is_empty() { "(empty)".to_owned() } else { entry.output.clone() })),
                    ),
            );
        }
        if let Some((pending_line, pending_db, _)) = &self.console_pending {
            log = log.child(
                div()
                    .id("redis-console-pending")
                    .test_support()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap(px(Space::XXS))
                    .py(px(Space::XS))
                    .when(!self.console_log.is_empty(), |this| this.border_t_1().border_color(p.line_soft))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(Space::SM))
                            .min_h(px(m.chip_h()))
                            .child(div().flex_none().text_color(p.text_faint).child(SharedString::from(prompt(pending_db))))
                            .child(div().flex_1().min_w(px(0.)).truncate().text_color(p.text).font_weight(FontWeight::MEDIUM).child(SharedString::from(pending_line.clone())))
                            .child(kit::inline_loading(SharedString::from(self.text("redis.console.running")), m, p)),
                    ),
            );
        }
        let log = div().relative().flex_1().min_h(px(0.)).flex().flex_col().child(log.scroll_area_y().scroll_handle(&self.console_scroll));

        let (_, _, completions) = self.current_completions();
        let popup = (!completions.is_empty()).then(|| {
            let active = self.complete_index.min(completions.len() - 1);
            let mut popup = div()
                .id("redis-complete-list")
                .test_support()
                .mx(px(pad))
                .mb(px(Space::XS))
                .max_h(px(176.))
                .scroll_area_y()
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
            popup.child(div().px(px(8.)).pt(px(2.)).text_size(px(TypeScale::XS)).text_color(p.text_faint).child(SharedString::from(self.text("redis.console.completeHint"))))
        });

        let input_row = div()
            .flex_none()
            .px(px(pad))
            .pt(px(Space::SM))
            .flex()
            .items_center()
            .gap(px(Space::SM))
            .child(div().flex_none().font_family(mono.clone()).text_size(px(TypeScale::SM)).text_color(p.accent_text).child(SharedString::from(prompt(&self.database))))
            .child(self.command_field(&command, p, &mono, cx))
            .child(action("redis-exec", Some(IconName::Play), &self.text("redis.tab.execute"), Kind::Primary, entity).small());
        let hint_row = div()
            .flex_none()
            .min_h(px(22.))
            .px(px(pad))
            .pt(px(Space::XS))
            .pb(px(Space::SM))
            .flex()
            .items_center()
            .gap(px(6.))
            .text_size(px(TypeScale::XS))
            .text_color(p.text_faint)
            .when_some(spec, |this, spec| {
                let (fg, bg, label) = if spec.is_write() { (p.warn, p.warn_soft, "WRITE") } else { (p.ok, p.ok_soft, "READ") };
                this.child(kit::pill(SharedString::from(label), fg, bg)).child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .truncate()
                        .font_family(mono.clone())
                        .text_color(p.text_muted)
                        .child(SharedString::from(format!("{} — {}", spec.syntax, spec.summary(self.locale)))),
                )
            })
            .when(spec.is_none(), |this| this.child(div().flex_1()))
            .child(div().flex_none().child(SharedString::from(self.text("redis.console.keysHint"))));

        let tools = div()
            .flex()
            .items_center()
            .gap(px(Space::XXS))
            .child(icon_btn("redis-console-copy", IconName::Copy, &self.text("redis.console.copyAll"), entity).disabled(self.console_log.is_empty()))
            .child(icon_btn("redis-console-clear", IconName::Trash, &self.text("redis.console.clear"), entity).disabled(self.console_log.is_empty()))
            .into_any_element();
        div()
            .id("redis-console")
            .test_support()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .flex_col()
            .child(self.diag_toolbar(p, m, entity, None, false, vec![tools]))
            .child(log)
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .border_t_1()
                    .border_color(p.line)
                    .bg(p.chrome)
                    .children(popup)
                    .child(input_row)
                    .child(hint_row),
            )
            .into_any_element()
    }

    // ------------------------------------------------------------------ 对话框

    fn render_confirm_dialog(&mut self, p: &Palette, m: &kit::Metrics, entity: &Entity<Self>, mono: SharedString, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let destructive = self.confirm_destructive();
        let write_only = matches!(self.confirm, Some(Confirm::Mutation { .. }) | Some(Confirm::Ttl { .. }) | Some(Confirm::Batch { .. }));
        let ready = self.confirm_ready();
        // 生产库：输入连接名才可确认（与对象树的危险操作同一规则）。
        let typed = if self.prod_target {
            let expected = self.confirm_expected();
            let input = self.redis_input(Field::ConfirmTyped, "redis-confirm-typed", "redis.prod.typeName", &expected, window, cx);
            Some(
                div()
                    .id("redis-confirm-prod")
                    .test_support()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .p(px(8.))
                    .rounded(px(Radius::CONTROL))
                    .bg(p.danger_soft)
                    .child(div().text_size(px(TypeScale::XS)).text_color(p.danger).child(SharedString::from(i18n::fill(&self.text("redis.prod.typeName"), &[("name", &expected)]))))
                    .child(kit::text_input("redis-confirm-typed", &input, self.text("redis.prod.typeName")).small())
                    .when(self.confirm_typed_error, |this| {
                        this.child(div().text_size(px(TypeScale::XS)).text_color(p.danger).child(SharedString::from(self.text("redis.prod.mismatch"))))
                    }),
            )
        } else {
            None
        };
        let lead = if write_only { self.text("redis.confirm.writeLead") } else { self.confirm_prompt.clone() };
        let (icon_name, fg, bg) = if destructive {
            (IconName::TriangleAlert, p.danger, p.danger_soft)
        } else {
            (IconName::ShieldCheck, p.accent_text, p.accent_soft)
        };
        let title = if destructive { self.text("redis.confirm.dangerTitle") } else { self.text("redis.confirm.title") };
        // 危险确认用 AlertDialog：读屏先读警示；卡片外壳与其它确认框一致。
        let card = kit::modal_surface("redis-confirm-dialog", SharedString::from(self.confirm_prompt.clone()), kit::ModalWidth::CONFIRM, p)
            .test_support()
            .role(gpui_kit::Role::AlertDialog)
            .p(px(m.card_pad() + Space::XS))
            .gap(px(Space::LG))
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
                    .scroll_area_y()
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
            .children(typed)
            .child(
                kit::modal_actions()
                    .pt(px(Space::XS))
                    .child(action("redis-cancel", None, &self.text("common.cancel"), Kind::Secondary, entity).small())
                    .child(
                        action(
                            "redis-confirm",
                            Some(if destructive { IconName::Trash } else { IconName::Check }),
                            &self.text("redis.tab.confirmExecute"),
                            if destructive { Kind::Danger } else { Kind::Primary },
                            entity,
                        )
                        .small()
                        .disabled(!ready),
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

        let new_ttl_preview = (!self.new_ttl.trim().is_empty()).then(|| match parse_ttl(&self.new_ttl) {
            Ok(Some(secs)) => (i18n::fill(&self.text("redis.ttl.preview"), &[("t", &long_ttl(secs.saturating_mul(1000)))]), p.info),
            Ok(None) => (self.text("redis.keys.neverExpire"), p.text_faint),
            Err(()) => (self.text("redis.tab.ttlInvalid"), p.warn),
        });
        let ttl_control = div()
            .flex()
            .items_center()
            .gap(px(Space::SM))
            .child(div().flex_1().min_w(px(0.)).child(kit::text_input("redis-new-ttl", &ttl, self.text("redis.tab.ttlLabel"))))
            .when_some(new_ttl_preview, |this, (text, color)| {
                this.child(div().flex_none().text_size(px(TypeScale::XS)).font_features(kit::tabular_figures()).text_color(color).child(SharedString::from(text)))
            });
        let mut form = div()
            .flex()
            .flex_col()
            .gap(px(Space::LG))
            .child(kit::form_row(self.text("redis.new.type"), kit::segmented_sized(types, p, m.control_h - Space::XS), p))
            .child(kit::form_row(self.text("redis.key.name"), kit::text_input("redis-new-name", &key, self.text("redis.key.name")), p))
            .child(kit::form_row(self.text("redis.new.ttlOptional"), ttl_control, p));
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
            Field::Separators => self.sep_input.clone(),
            Field::ScanCap => self.cap_input.clone(),
            Field::BatchTtl => self.batch_ttl_input.clone(),
            Field::ConfirmTyped => self.confirm_typed.clone(),
            Field::SubChannels => self.diag.sub_input.clone(),
            Field::PubChannel => self.diag.pub_channel.clone(),
            Field::PubMessage => self.diag.pub_message.clone(),
            Field::InfoQuery => self.info_query.clone(),
            Field::ClientsQuery => self.diag.clients_query.clone(),
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
                    Field::Separators => this.sep_input = value,
                    Field::ScanCap => this.cap_input = value,
                    Field::BatchTtl => this.batch_ttl_input = value,
                    Field::ConfirmTyped => {
                        this.confirm_typed = value;
                        this.confirm_typed_error = false;
                    }
                    Field::SubChannels => this.diag.sub_input = value,
                    Field::PubChannel => this.diag.pub_channel = value,
                    Field::PubMessage => this.diag.pub_message = value,
                    Field::InfoQuery => this.info_query = value,
                    Field::ClientsQuery => this.diag.clients_query = value,
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
                    Field::Separators | Field::ScanCap => "redis-prefs-save".into(),
                    Field::BatchTtl => "redis-batch-ttl-apply".into(),
                    // 生产库的确认只能点按钮，回车不提交。
                    Field::ConfirmTyped | Field::InfoQuery | Field::ClientsQuery => return,
                    Field::SubChannels => "redis-sub-start".into(),
                    Field::PubChannel | Field::PubMessage => "redis-pub-send".into(),
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
        pane.confirm = Some(Confirm::Mutation { mutation: RedisMutation::HashDel { key: "h".into(), fields: vec!["f".into()] }, key: "h".into(), guard: None });
        assert!(pane.confirm_destructive());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn entry_viewer_tag_changes_with_content_not_only_length() {
        assert_ne!(content_tag("k", 0, "abc"), content_tag("k", 0, "abd"), "等长但内容不同的值必须重建查看器");
        assert_eq!(content_tag("k", 1, "x"), content_tag("k", 1, "x"));
    }
}
