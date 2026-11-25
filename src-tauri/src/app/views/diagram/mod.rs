//! 只读关系图。表卡片、外键连线、悬停、遗漏说明和文字清单都不写库。

use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::{
    canvas, AppContext as _, Context, Focusable as _, FontWeight, InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, ParentElement as _, PathBuilder,
    Render, ScrollDelta, ScrollEvent, SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div, px,
};

use crate::ui::kit::{self, Palette, Radius, Space, Tip as _, TypeScale};

use crate::application::tasks::{TaskOutcome, TaskToken};
use crate::application::AppServices;
use crate::diagram::{Diagram, DiagramEdge, DiagramTable};
use crate::i18n::{self, Locale};

const ZOOM_MIN: f32 = 0.4;
const ZOOM_MAX: f32 = 2.0;

const OWNER: &str = "diagram";

/// 双击或菜单请求打开表。只记下目标，不执行 SQL；外壳稍后取走。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagramOpen {
    pub connection_id: String,
    pub database: String,
    pub schema: String,
    pub table: String,
    pub structure: bool,
}

struct DragPan {
    x: f32,
    y: f32,
    pan_x: f32,
    pan_y: f32,
    moved: bool,
}

/// 拖动一张卡片：按下时的指针与卡片原偏移；没拖动就当作一次单击（选中）。
struct CardDrag {
    table: String,
    x: f32,
    y: f32,
    dx: f32,
    dy: f32,
    moved: bool,
}

pub struct DiagramPane {
    services: Arc<AppServices>,
    locale: Locale,
    connection_id: String,
    database: String,
    schema: String,
    label: String,
    diagram: Option<Arc<Diagram>>,
    error: String,
    loading: bool,
    hovered: Option<String>,
    /// 单击选中的卡片：关联表与连线保持高亮，直到点画布空白或 Esc。
    selected: Option<String>,
    token: Option<TaskToken>,
    applied: String,
    zoom: f32,
    pan_x: f32,
    pan_y: f32,
    search: String,
    /// 搜索命中的全部表（按卡片顺序）；highlight 是其中当前那一个。
    matches: Vec<String>,
    match_index: usize,
    highlight: Option<String>,
    center_pending: bool,
    /// 下一帧要居中的表（菜单“定位”）。
    center_target: Option<String>,
    navigation: Option<DiagramOpen>,
    /// 打开请求被外壳取走之后，文字清单仍保留这一行，测试和悬停清单能看到。
    open_note: Option<String>,
    /// 右键/菜单键请求的表级菜单：外壳取走后用对象树同一册菜单弹在指针处。
    menu_request: Option<(String, Option<gpui_kit::Point<gpui_kit::Pixels>>)>,
    back: bool,
    drag: Option<DragPan>,
    card_drag: Option<CardDrag>,
    /// 用户拖过的卡片相对自动布局的偏移（未缩放坐标）。
    offsets: std::collections::HashMap<String, (f32, f32)>,
    /// 展开显示全部列的长表。
    expanded: std::collections::HashSet<String>,
    content_w: f32,
    content_h: f32,
    content_x0: f32,
    content_y0: f32,
    /// 画板在窗口里的位置与大小：滚轮按指针缩放、适应视口都按它算。
    board: std::rc::Rc<std::cell::Cell<gpui_kit::Bounds<gpui_kit::Pixels>>>,
    search_input: Option<EntityInput>,
}

struct EntityInput {
    state: gpui_kit::Entity<InputState>,
    sync: String,
    _sub: gpui_kit::Subscription,
}

impl DiagramPane {
    pub fn new(services: Arc<AppServices>) -> Self {
        Self {
            services,
            locale: Locale::ZhCn,
            connection_id: String::new(),
            database: String::new(),
            schema: String::new(),
            label: String::new(),
            diagram: None,
            error: String::new(),
            loading: false,
            hovered: None,
            selected: None,
            token: None,
            applied: String::new(),
            zoom: 1.,
            pan_x: 0.,
            pan_y: 0.,
            search: String::new(),
            matches: Vec::new(),
            match_index: 0,
            highlight: None,
            center_pending: false,
            center_target: None,
            navigation: None,
            open_note: None,
            menu_request: None,
            back: false,
            drag: None,
            card_drag: None,
            offsets: std::collections::HashMap::new(),
            expanded: std::collections::HashSet::new(),
            content_w: 0.,
            content_h: 0.,
            content_x0: 0.,
            content_y0: 0.,
            board: std::rc::Rc::new(std::cell::Cell::new(gpui_kit::Bounds::default())),
            search_input: None,
        }
    }

    pub fn take_back(&mut self) -> bool {
        let back = self.back;
        self.back = false;
        back
    }

    pub fn take_open(&mut self) -> Option<DiagramOpen> {
        self.navigation.take()
    }

    /// 外壳取走一次表级菜单请求：目标表（连接/库/模式/表名）与指针位置（键盘打开时为空）。
    pub fn take_menu(&mut self) -> Option<(DiagramOpen, Option<gpui_kit::Point<gpui_kit::Pixels>>)> {
        let (table, at) = self.menu_request.take()?;
        Some((
            DiagramOpen {
                connection_id: self.connection_id.clone(),
                database: self.database.clone(),
                schema: self.schema.clone(),
                table,
                structure: false,
            },
            at,
        ))
    }

    /// 复制用的限定名：有模式时带上模式。
    fn qualified(&self, table: &str) -> String {
        if self.schema.is_empty() { table.to_owned() } else { format!("{}.{table}", self.schema) }
    }

    pub fn set_locale(&mut self, locale: Locale) {
        self.locale = locale;
    }

    pub fn loading(&self) -> bool {
        self.loading
    }

    pub fn open_spec(&mut self, spec: &str, cx: &mut Context<Self>) {
        if spec == self.applied && (self.diagram.is_some() || self.loading || !self.error.is_empty()) {
            return;
        }
        let Some(target) = parse_spec(spec) else {
            // 没有目标不是“读取失败”：走无目标空态并给出下一步，不用错误色和重试按钮。
            self.clear_target();
            self.applied = spec.to_owned();
            cx.notify();
            return;
        };
        self.applied = spec.to_owned();
        self.connection_id = target.0;
        self.database = target.1;
        self.schema = target.2;
        self.label = target.3;
        self.reset_canvas();
        self.diagram = None;
        self.error.clear();
        self.read(cx);
    }

    pub fn command(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(name) = id.strip_prefix("diagram-hover:") {
            self.hovered = Some(name.to_owned());
        } else if id == "diagram-hover-clear" {
            self.hovered = None;
        } else if id == "diagram-retry" && !self.connection_id.is_empty() {
            self.error.clear();
            self.read(cx);
        } else if id == "diagram-back" {
            self.back = true;
        } else if id == "diagram-zoom-in" {
            self.zoom_around_center(self.zoom + 0.1);
        } else if id == "diagram-zoom-out" {
            self.zoom_around_center(self.zoom - 0.1);
        } else if id == "diagram-zoom-reset" {
            self.zoom = 1.;
            self.pan_x = 0.;
            self.pan_y = 0.;
        } else if id == "diagram-layout-reset" {
            self.offsets.clear();
            self.expanded.clear();
        } else if id == "diagram-search-clear" {
            self.clear_search();
        } else if id == "diagram-search-next" {
            self.step_match(1);
        } else if id == "diagram-search-prev" {
            self.step_match(-1);
        } else if id == "diagram-menu-close" || id == "diagram-deselect" {
            self.selected = None;
            self.menu_request = None;
        } else if let Some(name) = id.strip_prefix("diagram-open-data:") {
            self.request_open(name, false);
        } else if let Some(name) = id.strip_prefix("diagram-open-structure:") {
            self.request_open(name, true);
        } else if let Some(name) = id.strip_prefix("diagram-menu:") {
            self.open_card_menu(name, None);
        } else if let Some(name) = id.strip_prefix("diagram-select:") {
            self.select(name);
        } else if let Some(name) = id.strip_prefix("diagram-focus:") {
            // 菜单里的“定位并高亮关联表”：选中（不切换）并把它移到视野中央。
            if self.knows(name) {
                self.selected = Some(name.to_owned());
                self.center_target = Some(name.to_owned());
            }
        } else if let Some(name) = id.strip_prefix("diagram-expand:") {
            if !self.expanded.remove(name) {
                self.expanded.insert(name.to_owned());
            }
        } else if let Some(name) = id.strip_prefix("diagram-copy-name:") {
            if self.knows(name) {
                cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(self.qualified(name)));
            }
        }
        cx.notify();
    }

    fn reset_canvas(&mut self) {
        self.hovered = None;
        self.zoom = 1.;
        self.pan_x = 0.;
        self.pan_y = 0.;
        self.search.clear();
        self.matches.clear();
        self.match_index = 0;
        self.highlight = None;
        self.center_pending = false;
        self.navigation = None;
        self.menu_request = None;
        self.selected = None;
        self.drag = None;
        self.card_drag = None;
        self.offsets.clear();
        self.expanded.clear();
    }

    fn knows(&self, table: &str) -> bool {
        self.diagram.as_ref().is_some_and(|diagram| diagram.tables.iter().any(|item| item.name == table))
    }

    /// 选中一张卡片（再点一次取消）：关联表与连线保持高亮。
    fn select(&mut self, table: &str) {
        if !self.knows(table) {
            return;
        }
        if self.selected.as_deref() == Some(table) {
            self.selected = None;
        } else {
            self.selected = Some(table.to_owned());
        }
    }

    /// 以画板中心为锚点缩放：按钮与键盘缩放时视野中央的卡片不跑开。
    fn zoom_around_center(&mut self, zoom: f32) {
        let board = self.board.get();
        let w = board.size.width.as_f32();
        let h = board.size.height.as_f32();
        if w <= 1. || h <= 1. {
            self.zoom = clamp_zoom(zoom);
            return;
        }
        self.zoom_at(zoom, w / 2., h / 2.);
    }

    /// 以画板内一点为锚点缩放（滚轮缩放按指针位置）。
    fn zoom_at(&mut self, zoom: f32, x: f32, y: f32) {
        let next = clamp_zoom(zoom);
        let ratio = next / self.zoom.max(0.01);
        self.pan_x = x - (x - self.pan_x) * ratio;
        self.pan_y = y - (y - self.pan_y) * ratio;
        self.zoom = next;
    }

    /// 搜索命中之间前后跳转（Enter / Shift+Enter），并把当前那张居中。
    fn step_match(&mut self, step: i32) {
        if self.matches.is_empty() {
            return;
        }
        let len = self.matches.len() as i32;
        self.match_index = ((self.match_index as i32 + step).rem_euclid(len)) as usize;
        self.highlight = self.matches.get(self.match_index).cloned();
        self.center_pending = self.highlight.is_some();
    }

    fn clear_target(&mut self) {
        self.connection_id.clear();
        self.database.clear();
        self.schema.clear();
        self.label.clear();
        self.diagram = None;
        self.loading = false;
        self.error.clear();
        self.reset_canvas();
    }

    /// 记下要打开的表。不连接数据库，也不发送 SQL。
    pub fn request_open(&mut self, table: &str, structure: bool) {
        if !self.knows(table) {
            return;
        }
        self.menu_request = None;
        self.navigation = Some(DiagramOpen {
            connection_id: self.connection_id.clone(),
            database: self.database.clone(),
            schema: self.schema.clone(),
            table: table.to_owned(),
            structure,
        });
        let kind = if structure { "open-structure" } else { "open-data" };
        self.open_note = Some(format!("{kind} {table}"));
    }

    fn focus_search(&mut self, query: String) {
        self.search = query;
        let needle = self.search.trim().to_lowercase();
        self.match_index = 0;
        if needle.is_empty() {
            self.matches.clear();
            self.highlight = None;
            self.center_pending = false;
            return;
        }
        // 全名相等的表排在最前，其次是前缀匹配，最后是包含：搜完整表名时直接落到那张表。
        let mut matches: Vec<(u8, String)> = self
            .diagram
            .as_ref()
            .map(|diagram| {
                diagram
                    .tables
                    .iter()
                    .filter_map(|table| {
                        let name = table.name.to_lowercase();
                        let rank = if name == needle { 0 } else if name.starts_with(&needle) { 1 } else if name.contains(&needle) { 2 } else { return None };
                        Some((rank, table.name.clone()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        matches.sort_by_key(|(rank, _)| *rank);
        self.matches = matches.into_iter().map(|(_, name)| name).collect();
        self.highlight = self.matches.first().cloned();
        self.center_pending = self.highlight.is_some();
    }

    fn fit_view(&mut self, viewport_w: f32, viewport_h: f32) {
        if self.content_w <= 1. || self.content_h <= 1. {
            self.zoom = 1.;
            self.pan_x = 0.;
            self.pan_y = 0.;
            return;
        }
        // 画板已经画过一次就按画板大小算（不含页头、侧栏），否则退回窗口大小。
        let board = self.board.get();
        let (w, h) = if board.size.width.as_f32() > 1. { (board.size.width.as_f32(), board.size.height.as_f32()) } else { (viewport_w, viewport_h) };
        let zx = (w - 24.).max(80.) / self.content_w;
        let zy = (h - 24.).max(80.) / self.content_h;
        self.zoom = clamp_zoom(zx.min(zy));
        // 内容放不满的那一边居中。
        self.pan_x = ((w - self.content_w * self.zoom) / 2.).max(0.) - self.content_x0 * self.zoom;
        self.pan_y = ((h - self.content_h * self.zoom) / 2.).clamp(0., 12.) - self.content_y0 * self.zoom;
    }

    fn begin_drag(&mut self, x: f32, y: f32) {
        self.drag = Some(DragPan { x, y, pan_x: self.pan_x, pan_y: self.pan_y, moved: false });
    }

    fn begin_card_drag(&mut self, table: &str, x: f32, y: f32) {
        let (dx, dy) = self.offsets.get(table).copied().unwrap_or((0., 0.));
        self.card_drag = Some(CardDrag { table: table.to_owned(), x, y, dx, dy, moved: false });
    }

    /// 拖动中的指针移动：卡片拖动优先，否则平移画布。返回是否改了画面。
    fn pointer_move(&mut self, x: f32, y: f32) -> bool {
        if let Some(drag) = self.card_drag.as_mut() {
            let dx = x - drag.x;
            let dy = y - drag.y;
            if !drag.moved && dx.abs() < 3. && dy.abs() < 3. {
                return false;
            }
            drag.moved = true;
            let zoom = self.zoom.max(0.01);
            let offset = (drag.dx + dx / zoom, drag.dy + dy / zoom);
            let table = drag.table.clone();
            self.offsets.insert(table, offset);
            return true;
        }
        if self.drag.is_some() {
            self.move_drag(x, y);
            return true;
        }
        false
    }

    /// 松开指针：没拖动的卡片按下算一次单击（选中/取消），没拖动的画布按下清掉选中。
    fn pointer_up(&mut self) {
        if let Some(drag) = self.card_drag.take() {
            if !drag.moved {
                self.select(&drag.table);
            }
        } else if let Some(drag) = self.drag.as_ref() {
            if !drag.moved {
                self.selected = None;
            }
        }
        self.drag = None;
    }

    fn move_drag(&mut self, x: f32, y: f32) {
        let Some(drag) = &self.drag else { return };
        let dx = x - drag.x;
        let dy = y - drag.y;
        if !drag.moved && dx.abs() < 3. && dy.abs() < 3. {
            return;
        }
        self.pan_x = drag.pan_x + dx;
        self.pan_y = drag.pan_y + dy;
        if let Some(drag) = self.drag.as_mut() {
            drag.moved = true;
        }
    }

    fn end_drag(&mut self) {
        self.drag = None;
    }

    fn read(&mut self, cx: &mut Context<Self>) {
        self.services.tasks.supersede(OWNER);
        let token = self.services.tasks.begin(OWNER);
        self.token = Some(token);
        self.loading = true;
        let services = Arc::clone(&self.services);
        let connection_id = self.connection_id.clone();
        let database = self.database.clone();
        let schema = self.schema.clone();
        let view = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::app::views::grid::runtime_for_views().spawn(async move {
                let result = crate::application::work::read_diagram(&services.connections, &connection_id, &database, &schema).await;
                let outcome = services.tasks.accept(OWNER, token, result);
                let _ = tx.send(outcome);
            });
            if let Ok(outcome) = rx.await {
                cx.update(|cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |pane, cx| {
                            pane.finish(token, outcome);
                            cx.notify();
                        });
                    }
                });
            }
        })
        .detach();
    }

    fn finish(&mut self, token: TaskToken, outcome: TaskOutcome<Result<Diagram, String>>) {
        if self.token != Some(token) {
            return;
        }
        self.loading = false;
        match outcome {
            TaskOutcome::Current { value: Ok(diagram), .. } => {
                self.error.clear();
                self.diagram = Some(Arc::new(diagram));
            }
            TaskOutcome::Current { value: Err(message), .. } => {
                self.diagram = None;
                let reason = if message.is_empty() { i18n::text(self.locale, "diagram.failed") } else { message };
                // 错误必须带目标，否则用户无法判断是哪张库/模式读取失败。
                self.error = if self.label.is_empty() {
                    reason
                } else if self.locale == Locale::En {
                    format!("{}: {reason}", self.label)
                } else {
                    format!("{}：{reason}", self.label)
                };
            }
            TaskOutcome::Stale { .. } | TaskOutcome::Cancelled { .. } => {}
        }
    }

    /// 当前这一页该显示哪一种状态。渲染与测试共用这一份判断，不再各写一套三态。
    fn state(&self) -> PaneState {
        if self.loading {
            PaneState::Loading
        } else if !self.error.is_empty() {
            PaneState::Error
        } else if self.connection_id.is_empty() {
            PaneState::NeedTarget
        } else if self.diagram.as_ref().is_some_and(|diagram| diagram.tables.is_empty()) {
            PaneState::Empty
        } else if self.diagram.is_some() {
            PaneState::Cards
        } else {
            PaneState::NeedTarget
        }
    }

    /// 卡片上的键盘等价操作：Enter = 双击（打开数据），Shift+Enter = 看结构，
    /// 菜单键 / Shift+F10 = 右键菜单，Esc = 收起菜单与悬停。返回是否已处理（决定要不要停冒泡）。
    fn card_key(&mut self, table: &str, key: &str, shift: bool) -> bool {
        match key {
            "enter" => {
                self.hovered = Some(table.to_owned());
                self.request_open(table, shift);
                true
            }
            "contextmenu" | "menu" => self.open_card_menu(table, None),
            "f10" if shift => self.open_card_menu(table, None),
            " " | "space" => {
                self.select(table);
                true
            }
            "escape" => {
                self.hovered = None;
                self.menu_request = None;
                self.selected.take().is_some()
            }
            _ => false,
        }
    }

    /// 右键/菜单键：选中这张卡片并请外壳弹出表级菜单。
    fn open_card_menu(&mut self, table: &str, at: Option<gpui_kit::Point<gpui_kit::Pixels>>) -> bool {
        if !self.knows(table) {
            return false;
        }
        self.selected = Some(table.to_owned());
        self.menu_request = Some((table.to_owned(), at));
        true
    }

    /// 页面内的键盘动作（从卡片、菜单按钮冒泡到根）：Esc 收起菜单与悬停，+/- 缩放，0 复位。
    fn pane_key(&mut self, key: &str) -> bool {
        match key {
            "escape" => {
                let menu = self.menu_request.take().is_some();
                let selected = self.selected.take().is_some();
                let hover = self.hovered.take().is_some();
                menu || selected || hover
            }
            "=" | "+" => {
                self.zoom_around_center(self.zoom + 0.1);
                true
            }
            "-" => {
                self.zoom_around_center(self.zoom - 0.1);
                true
            }
            "0" => {
                self.zoom = 1.;
                self.pan_x = 0.;
                self.pan_y = 0.;
                true
            }
            _ => false,
        }
    }

    /// 清空搜索条件。只改模型，**不动** `field.sync`：search_field 靠
    /// `field.sync != wanted` 判断要不要把模型值写回输入控件，这里若把 sync 一起清掉，
    /// 两者都变成空串，写回就不会发生，控件里留着旧文字（清除按钮和框内 Esc 都会失效）。
    fn clear_search(&mut self) {
        self.search.clear();
        self.matches.clear();
        self.match_index = 0;
        self.highlight = None;
        self.center_pending = false;
    }

    /// 搜索框聚焦时自己吃掉字符：页面快捷键（+/-/0、Esc）不能顺手改画布。
    fn search_focused(&self, window: &Window, cx: &gpui_kit::App) -> bool {
        self.search_input.as_ref().is_some_and(|field| field.state.read(cx).focus_handle(cx).is_focused(window))
    }

    pub fn column_of(&self, name: &str) -> Option<usize> {
        let diagram = self.diagram.as_ref()?;
        layout_columns(&diagram.tables, &diagram.edges).into_iter().position(|column| column.iter().any(|table| table.name == name))
    }

    pub fn lines(&self) -> Vec<String> {
        // 只列实际画出来的文字：只读说明 `diagram.readonly` 只在标题悬停提示里，不常驻。
        let mut lines = Vec::new();
        if !self.label.is_empty() {
            lines.push(self.label.clone());
        }
        if self.loading {
            lines.push(i18n::text(self.locale, "diagram.loading"));
        }
        if !self.error.is_empty() {
            lines.push(self.error.clone());
        }
        let Some(diagram) = &self.diagram else {
            if !self.loading && self.error.is_empty() && self.connection_id.is_empty() {
                lines.push(i18n::text(self.locale, "diagram.needTarget"));
            }
            return lines;
        };
        if let Some(notice) = &diagram.notice {
            lines.push(notice.clone());
        }
        if !diagram.omitted.is_empty() {
            lines.push(i18n::text(self.locale, "diagram.omitted"));
            for item in &diagram.omitted {
                lines.push(format!("omitted {} {}", item.name, item.message));
            }
        }
        let columns = layout_columns(&diagram.tables, &diagram.edges);
        let foreign = foreign_columns(diagram);
        lines.push(format!("zoom {}", (self.zoom * 100.).round() as i32));
        if let Some(name) = &self.highlight {
            lines.push(format!("highlight {name}"));
        }
        if let Some(note) = &self.open_note {
            lines.push(note.clone());
        }
        if let Some((name, _)) = &self.menu_request {
            lines.push(format!("menu {name}"));
        }
        if let Some(name) = &self.selected {
            lines.push(format!("selected {name}"));
        }
        if !self.search.trim().is_empty() {
            match &self.highlight {
                Some(name) => lines.push(format!("search-match {name}")),
                None => lines.push(format!("search-no-match {}", self.search.trim())),
            }
        }
        if let Some(open) = &self.navigation {
            let kind = if open.structure { "open-structure" } else { "open-data" };
            lines.push(format!("{kind} {}", open.table));
        }
        let focus = self.hovered.as_deref().or(self.selected.as_deref());
        for (index, column) in columns.iter().enumerate() {
            for table in column {
                let mark = hover_mark(focus, &table.name, diagram);
                lines.push(format!("column {index} {mark} {}", table.name));
                for column in &table.columns {
                    let fk = if foreign.contains(&(table.name.as_str(), column.name.as_str())) { "fk" } else { "column" };
                    lines.push(format!("{fk} {}.{}", table.name, column.name));
                    if column.primary {
                        lines.push(format!("pk {}.{}", table.name, column.name));
                    }
                    if !column.type_name.is_empty() {
                        lines.push(format!("type {}.{} {}", table.name, column.name, column.type_name));
                    }
                }
                for note in &table.outside {
                    lines.push(format!("outside {}", note));
                }
            }
        }
        if !diagram.edges.is_empty() {
            lines.push(i18n::text(self.locale, "diagram.foreignKeys"));
        }
        for edge in &diagram.edges {
            let mark = if edge_hot(focus, edge) { "hot-wire" } else if focus.is_some() { "dim-wire" } else { "wire" };
            lines.push(format!(
                "{mark} arrow {}.{} -> {}.{}",
                edge.from_table,
                edge.from_columns.join(", "),
                edge.to_table,
                edge.to_columns.join(", ")
            ));
        }
        lines
    }
}

/// 关系图页的四种状态。每种状态在渲染里都带一条可执行的下一步。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PaneState {
    /// 正在读取：局部加载，读完后卡片逐列出现。
    Loading,
    /// 读取失败：错误块 + 重试。
    Error,
    /// 没有目标：说明先去选库/模式，并给出打开资源管理器的键位。
    NeedTarget,
    /// 这个范围里没有表：说明为什么空，并给出重新读取。
    Empty,
    /// 有表：画布。
    Cards,
}

fn clamp_zoom(zoom: f32) -> f32 {
    zoom.clamp(ZOOM_MIN, ZOOM_MAX)
}

fn column_index(table: &DiagramTable, name: &str) -> Option<usize> {
    table.columns.iter().position(|column| column.name == name)
}

fn parse_spec(spec: &str) -> Option<(String, String, String, String)> {
    let parts: Vec<&str> = spec.split('|').collect();
    if parts.len() < 4 || parts[0].is_empty() {
        return None;
    }
    Some((parts[0].to_owned(), parts[1].to_owned(), parts[2].to_owned(), parts[3].to_owned()))
}

/// 关系图只读打开对象的连接/库/模式范围；非关系型引擎没有可画的层级。
/// 解析走与其它入口一致的一份对象身份，避免 连接/库/模式 各写一套。
pub fn spec_from_object(spec: &str) -> Option<String> {
    let target = crate::app::model::ObjectTarget::parse(spec)?;
    if !target.is_relational() {
        return None;
    }
    let label = if target.connection_name.is_empty() {
        target.connection_id.clone()
    } else {
        target.connection_name.clone()
    };
    Some(format!(
        "{}|{}|{}|{label}",
        target.connection_id, target.database, target.schema
    ))
}

fn layout_columns<'a>(tables: &'a [DiagramTable], edges: &[DiagramEdge]) -> Vec<Vec<&'a DiagramTable>> {
    let known: Vec<&str> = tables.iter().map(|table| table.name.as_str()).collect();
    let mut columns: Vec<Vec<&DiagramTable>> = Vec::new();
    for table in tables {
        let level = depth(&table.name, &known, edges, &mut Vec::new());
        while columns.len() <= level {
            columns.push(Vec::new());
        }
        columns[level].push(table);
    }
    columns
}

fn depth<'a>(name: &'a str, known: &[&str], edges: &'a [DiagramEdge], path: &mut Vec<&'a str>) -> usize {
    if path.contains(&name) {
        return 0;
    }
    path.push(name);
    let mut deepest = 0usize;
    for edge in edges {
        if edge.from_table != name || edge.from_table == edge.to_table || !known.contains(&edge.to_table.as_str()) {
            continue;
        }
        if path.contains(&edge.to_table.as_str()) {
            continue;
        }
        deepest = deepest.max(1 + depth(&edge.to_table, known, edges, path));
    }
    path.pop();
    deepest
}

fn foreign_columns(diagram: &Diagram) -> std::collections::HashSet<(&str, &str)> {
    diagram.edges.iter().flat_map(|edge| edge.from_columns.iter().map(|column| (edge.from_table.as_str(), column.as_str()))).collect()
}

fn hover_mark(hovered: Option<&str>, name: &str, diagram: &Diagram) -> &'static str {
    match hovered {
        Some(hot) if hot == name => "hot",
        Some(hot) if linked(hot, name, diagram) => "linked",
        Some(_) => "dim",
        None => "card",
    }
}

fn linked(hovered: &str, name: &str, diagram: &Diagram) -> bool {
    diagram.edges.iter().any(|edge| (edge.from_table == hovered && edge.to_table == name) || (edge.to_table == hovered && edge.from_table == name))
}

fn edge_hot(hovered: Option<&str>, edge: &DiagramEdge) -> bool {
    hovered.is_some_and(|name| name == edge.from_table || name == edge.to_table)
}

/// 长表只显示主键、外键与前几列，其余折叠成一行“+N 列”。返回要画的列下标（原顺序）与折叠数。
fn visible_columns(table: &DiagramTable, foreign: &std::collections::HashSet<(&str, &str)>, expanded: bool) -> (Vec<usize>, usize) {
    let total = table.columns.len();
    if expanded || total <= COLLAPSE_AT {
        return ((0..total).collect(), 0);
    }
    let mut keep: Vec<usize> = (0..total)
        .filter(|&index| {
            let column = &table.columns[index];
            column.primary || foreign.contains(&(table.name.as_str(), column.name.as_str()))
        })
        .collect();
    for index in 0..total {
        if keep.len() >= SHOW_ROWS {
            break;
        }
        if !keep.contains(&index) {
            keep.push(index);
        }
    }
    keep.sort_unstable();
    let hidden = total - keep.len();
    (keep, hidden)
}

/// 卡片高度（未缩放）：表头 + 显示的列 + 折叠行 + 范围外外键说明。
fn card_height_for(table: &DiagramTable, shown: usize, folded: bool) -> f32 {
    HEAD_H + ROW_H * shown.max(1) as f32 + 10. + ROW_H * table.outside.len() as f32 + if folded { ROW_H } else { 0. }
}

/// 自动布局（未缩放坐标）。有外键关系的表按引用层级从左到右分列（被引用的在左），
/// 一列太高就换到旁边一列；没有任何关系的表放在右侧，按瀑布流铺成接近横向屏幕比例的一片，
/// 而不是把几百张表竖着排成一条。
fn auto_layout(tables: &[DiagramTable], edges: &[DiagramEdge], heights: &std::collections::HashMap<String, f32>) -> std::collections::HashMap<String, (f32, f32)> {
    let known: Vec<&str> = tables.iter().map(|table| table.name.as_str()).collect();
    let mut related: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for edge in edges {
        if edge.from_table != edge.to_table && known.contains(&edge.from_table.as_str()) && known.contains(&edge.to_table.as_str()) {
            related.insert(edge.from_table.as_str());
            related.insert(edge.to_table.as_str());
        }
    }
    let height = |name: &str| heights.get(name).copied().unwrap_or(HEAD_H + ROW_H);
    let mut places = std::collections::HashMap::new();
    let mut levels: Vec<Vec<&DiagramTable>> = Vec::new();
    for table in tables.iter().filter(|table| related.contains(table.name.as_str())) {
        let level = depth(&table.name, &known, edges, &mut Vec::new());
        while levels.len() <= level {
            levels.push(Vec::new());
        }
        levels[level].push(table);
    }
    let mut x = PAD;
    for level in levels.iter().filter(|level| !level.is_empty()) {
        let mut y = PAD;
        for (index, table) in level.iter().enumerate() {
            let h = height(&table.name);
            if index > 0 && y + h > PAD + MAX_STACK_H {
                x += CARD_W + GAP_Y * 2.;
                y = PAD;
            }
            places.insert(table.name.clone(), (x, y));
            y += h + GAP_Y;
        }
        x += CARD_W + GAP_X;
    }
    let isolated: Vec<&DiagramTable> = tables.iter().filter(|table| !related.contains(table.name.as_str())).collect();
    if !isolated.is_empty() {
        let step = CARD_W + GRID_GAP;
        let total: f32 = isolated.iter().map(|table| height(&table.name) + GRID_GAP).sum();
        // 列数使整片宽高比接近 16:10：cols * step ≈ 1.6 * total / cols。
        let cols = ((1.6 * total / step).sqrt().ceil() as usize).clamp(1, isolated.len().min(12));
        let x0 = if places.is_empty() { PAD } else { x };
        let mut columns = vec![PAD; cols];
        for table in isolated {
            let (slot, top) = columns.iter().copied().enumerate().fold((0, f32::MAX), |best, (index, top)| if top < best.1 { (index, top) } else { best });
            places.insert(table.name.clone(), (x0 + slot as f32 * step, top));
            columns[slot] = top + height(&table.name) + GRID_GAP;
        }
    }
    places
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;

    #[test]
    fn layout_borrows_tables_and_foreign_columns_keep_qualified_names_distinct() {
        let diagram = Diagram {
            database: "main".into(),
            schema: String::new(),
            tables: vec![
                DiagramTable { name: "a.b".into(), columns: Vec::new(), outside: Vec::new() },
                DiagramTable { name: "parent".into(), columns: Vec::new(), outside: Vec::new() },
            ],
            edges: vec![DiagramEdge {
                name: "fk".into(),
                from_table: "a.b".into(),
                from_columns: vec!["c".into()],
                to_table: "parent".into(),
                to_columns: vec!["id".into()],
            }],
            omitted: Vec::new(),
            notice: None,
        };
        let columns = layout_columns(&diagram.tables, &diagram.edges);
        assert!(std::ptr::eq(columns[0][0], &diagram.tables[1]));
        assert!(std::ptr::eq(columns[1][0], &diagram.tables[0]));
        let foreign = foreign_columns(&diagram);
        assert!(foreign.contains(&("a.b", "c")));
        assert!(!foreign.contains(&("a", "b.c")), "dots inside identifiers cannot alias another column");
    }
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{Point, TestAppContext, WindowBounds, WindowOptions, px, size};

    #[test]
    fn failure_text_names_the_target_and_retry_stays_available() {
        let dir = std::env::temp_dir().join(format!("rillsql-diagram-error-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let services = std::sync::Arc::new(crate::application::AppServices::new(std::sync::Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
        let mut pane = DiagramPane::new(services);
        pane.connection_id = "notes".into();
        pane.label = "笔记 / main".into();
        pane.error = "笔记 / main：读取失败：权限不足".into();
        let text = pane.lines().join("\n");
        assert!(text.contains("笔记 / main"), "{text}");
        assert!(text.contains("读取失败：权限不足"), "{text}");
        pane.clear_target();
        assert!(pane.connection_id.is_empty());
        assert!(pane.label.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn typed_columns_zoom_and_open_request_do_not_run_sql() {
        let dir = std::env::temp_dir().join(format!("rillsql-diagram-open-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let services = std::sync::Arc::new(crate::application::AppServices::new(std::sync::Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
        let mut pane = DiagramPane::new(services);
        pane.connection_id = "notes".into();
        pane.database = "main".into();
        pane.diagram = Some(Arc::new(Diagram {
            database: "main".into(),
            schema: String::new(),
            tables: vec![DiagramTable {
                name: "orders".into(),
                columns: vec![
                    crate::diagram::DiagramColumn { name: "id".into(), type_name: "INTEGER".into(), primary: true },
                    crate::diagram::DiagramColumn { name: "customer_id".into(), type_name: "INTEGER".into(), primary: false },
                ],
                outside: Vec::new(),
            }],
            edges: Vec::new(),
            omitted: Vec::new(),
            notice: None,
        }));
        let text = pane.lines().join("\n");
        assert!(text.contains("pk orders.id"), "{text}");
        assert!(text.contains("type orders.id INTEGER"), "{text}");
        assert!(text.contains("column orders.customer_id"), "{text}");
        pane.focus_search("ord".into());
        assert_eq!(pane.highlight.as_deref(), Some("orders"));
        assert!(pane.center_pending);
        pane.zoom = 1.;
        pane.zoom = clamp_zoom(pane.zoom + 5.);
        assert_eq!(pane.zoom, 2.);
        pane.zoom = clamp_zoom(pane.zoom - 5.);
        assert_eq!(pane.zoom, 0.4);
        pane.request_open("orders", false);
        let open = pane.take_open().expect("导航请求");
        assert_eq!(open.table, "orders");
        assert!(!open.structure);
        assert!(pane.take_open().is_none());
        pane.request_open("missing", true);
        assert!(pane.take_open().is_none(), "不存在的表不能变成打开请求");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn object_spec_skips_non_relational_engines() {
        assert_eq!(spec_from_object("notes|笔记|main||customers|sqlite|dev").as_deref(), Some("notes|main||笔记"));
        assert!(spec_from_object("cache|缓存|0||key|redis|dev").is_none());
        assert!(spec_from_object("docs|文档|app||users|mongodb|dev").is_none());
    }

    #[gpui_kit::test]
    fn sqlite_diagram_shows_cards_wires_hover_and_does_not_write(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let dir = std::env::temp_dir().join(format!("rillsql-diagram-ui-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let database = dir.join("shop.sqlite");
        let config = dir.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        crate::app::views::grid::runtime_for_views().spawn(async move {
            let pool = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).create_if_missing(true))
                .await
                .unwrap();
            sqlx::query("CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT)").execute(&pool).await.unwrap();
            sqlx::query("CREATE TABLE orders (id INTEGER PRIMARY KEY, customer_id INTEGER, note TEXT, FOREIGN KEY (customer_id) REFERENCES customers(id))")
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query("CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT)").execute(&pool).await.unwrap();
            sqlx::query("CREATE TABLE invoices (id INTEGER PRIMARY KEY, customer_id INTEGER, FOREIGN KEY (customer_id) REFERENCES missing_customers(id))")
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query("INSERT INTO customers (id, name) VALUES (1, 'Ada')").execute(&pool).await.unwrap();
            sqlx::query("INSERT INTO orders (id, customer_id, note) VALUES (1, 1, 'one')").execute(&pool).await.unwrap();
            let master_before: Vec<(String, String)> = sqlx::query_as("SELECT type, name FROM sqlite_master ORDER BY type, name").fetch_all(&pool).await.unwrap();
            let rows_before: i64 = sqlx::query_scalar("SELECT count(*) FROM orders").fetch_one(&pool).await.unwrap();
            pool.close().await;
            let services = Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(config.join("connections.sqlite")))));
            services.connections.open_sqlite_for_test("notes", "笔记", &database).await.unwrap();
            let _ = tx.send((services, database, master_before, rows_before));
        });
        let (services, database, master_before, rows_before) = rx.recv_timeout(std::time::Duration::from_secs(120)).expect("diagram setup");
        cx.update(gpui_kit::init);
        let (window, view) = cx.update(|cx| {
            let bounds = gpui_kit::Bounds { origin: Point::default(), size: size(px(1200.), px(800.)) };
            let (window, view) = gpui_kit::open_window(
                WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
                cx,
                |window, cx| cx.new(|cx| crate::app::shell::Workbench::with_services(window, cx, services, false)),
            )
            .expect("workbench");
            (window, view)
        });
        cx.update_window(window, |_, _, cx| {
            view.update(cx, |view, cx| {
                view.model.object_spec = Some("cache|缓存|0||key|redis|dev".into());
                view.perform("open-diagram", cx);
                assert_eq!(view.model.notice_key, Some("diagram.unsupported"));
                view.model.object_spec = Some("notes|笔记|main||customers|sqlite|dev".into());
                view.perform("open-diagram", cx);
            });
        })
        .unwrap();
        assert!(pump(cx, window, &view, |view, app| {
            let text = view.diagram_text(app);
            text.contains("customers") && text.contains("orders") && text.contains("wire") && text.contains("missing_customers")
        }), "关系图没有显示表、连线或范围外的外键");
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("diagram-card:orders").is_some(), "订单表没有可悬停卡片");
            assert!(window.try_find("diagram-card:customers").is_some(), "客户表没有可悬停卡片");
            assert_eq!(window.find("diagram-card:orders").label(), Some("card orders"));
            view.update(cx, |view, cx| {
                let text = view.diagram_text(cx);
                assert!(!text.contains(&i18n::text(Locale::ZhCn, "diagram.readonly")), "只读说明只在悬停提示里，文字清单不列：{text}");
                assert!(text.contains("customers.id") || text.contains("fk orders.customer_id") || text.contains("orders.customer_id"), "{text}");
                assert!(text.contains("->") && text.contains("customers"), "{text}");
                assert!(text.contains("outside") && text.contains("missing_customers"), "{text}");
                assert!(text.contains("omitted") || text.contains("没有读到的表") || text.contains("missing_customers"), "{text}");
                assert!(view.diagram_column("customers", cx).unwrap() < view.diagram_column("orders", cx).unwrap(), "{text}");
            });
            window.hover("diagram-card:orders", cx);
            view.update(cx, |view, cx| {
                let hot = view.diagram_text(cx);
                assert!(hot.contains("hot orders"), "{hot}");
                assert!(hot.contains("linked customers"), "{hot}");
                assert!(hot.contains("hot-wire"), "{hot}");
                assert!(hot.contains("dim notes"), "{hot}");
            });
            window.double_click("diagram-card:orders", cx);
            view.update(cx, |view, cx| {
                let text = view.diagram_text(cx);
                assert!(text.contains("open-data orders"), "{text}");
            });
            if window.try_find("diagram-pad").is_some() {
                window.hover("diagram-pad", cx);
                view.update(cx, |view, cx| {
                    let text = view.diagram_text(cx);
                    assert!(text.contains("card orders"), "{text}");
                    assert!(!text.contains("hot orders"), "{text}");
                    assert!(!text.contains("hot-wire"), "{text}");
                    assert!(text.contains("wire") && text.contains("missing_customers"), "{text}");
                });
            }
            view.update(cx, |view, cx| view.perform("open-diagram:missing|main||缺", cx));
        })
        .unwrap();
        assert!(pump(cx, window, &view, |view, app| {
            let text = view.diagram_text(app);
            text.contains("还没有打开") && !text.contains("column 0 card customers")
        }), "失败的关系图看起来像读全了");
        let (tx, rx) = std::sync::mpsc::channel();
        crate::app::views::grid::runtime_for_views().spawn(async move {
            let pool = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).read_only(true))
                .await
                .unwrap();
            let master_after: Vec<(String, String)> = sqlx::query_as("SELECT type, name FROM sqlite_master ORDER BY type, name").fetch_all(&pool).await.unwrap();
            let rows_after: i64 = sqlx::query_scalar("SELECT count(*) FROM orders").fetch_one(&pool).await.unwrap();
            pool.close().await;
            let _ = tx.send((master_after, rows_after));
        });
        let (master_after, rows_after) = rx.recv_timeout(std::time::Duration::from_secs(30)).expect("reread");
        assert_eq!(master_before, master_after);
        assert_eq!(rows_before, rows_after);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn sample_diagram() -> Diagram {
        Diagram {
            database: "shop".into(),
            schema: String::new(),
            tables: vec![
                DiagramTable {
                    name: "customers".into(),
                    columns: vec![crate::diagram::DiagramColumn { name: "id".into(), type_name: "INTEGER".into(), primary: true }],
                    outside: Vec::new(),
                },
                DiagramTable {
                    name: "orders".into(),
                    columns: vec![
                        crate::diagram::DiagramColumn { name: "id".into(), type_name: "INTEGER".into(), primary: true },
                        crate::diagram::DiagramColumn { name: "customer_id".into(), type_name: "INTEGER".into(), primary: false },
                    ],
                    outside: Vec::new(),
                },
            ],
            edges: vec![DiagramEdge {
                name: "fk_orders_customer".into(),
                from_table: "orders".into(),
                from_columns: vec!["customer_id".into()],
                to_table: "customers".into(),
                to_columns: vec!["id".into()],
            }],
            omitted: Vec::new(),
            notice: None,
        }
    }

    fn sample_pane(tag: &str) -> (DiagramPane, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("rillsql-diagram-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let services = Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
        let mut pane = DiagramPane::new(services);
        pane.connection_id = "notes".into();
        pane.database = "shop".into();
        pane.label = "笔记 / shop".into();
        pane.diagram = Some(Arc::new(sample_diagram()));
        (pane, dir)
    }

    /// 卡片的键盘等价操作与鼠标手势一一对应：Enter = 双击打开数据，Shift+Enter = 看结构，
    /// 菜单键 / Shift+F10 = 右键菜单，Esc 收起；未知表不产生打开请求。
    #[test]
    fn card_keyboard_equivalents_match_the_mouse_gestures() {
        let (mut pane, dir) = sample_pane("card-keys");
        assert!(pane.card_key("orders", "enter", false), "Enter 由卡片处理");
        let open = pane.take_open().expect("Enter = 打开表数据");
        assert_eq!((open.table.as_str(), open.structure), ("orders", false));
        assert!(pane.card_key("orders", "enter", true), "Shift+Enter 由卡片处理");
        assert!(pane.take_open().expect("Shift+Enter = 看结构").structure);
        assert!(pane.card_key("orders", "contextmenu", false), "菜单键 = 右键菜单");
        assert_eq!(pane.menu_request.as_ref().map(|(name, _)| name.as_str()), Some("orders"));
        assert_eq!(pane.selected.as_deref(), Some("orders"), "打开菜单的卡片同时被选中");
        assert!(pane.card_key("orders", "escape", false), "Esc 收起菜单与选中");
        assert!(pane.menu_request.is_none() && pane.selected.is_none());
        assert!(pane.card_key("orders", "f10", true), "Shift+F10 = 右键菜单");
        let (target, at) = pane.take_menu().expect("外壳取走表级菜单请求");
        assert_eq!((target.connection_id.as_str(), target.database.as_str(), target.table.as_str()), ("notes", "shop", "orders"));
        assert!(at.is_none(), "键盘打开的菜单没有指针位置");
        assert!(pane.take_menu().is_none(), "菜单请求只取一次");
        assert!(pane.card_key("customers", " ", false), "空格选中卡片");
        assert_eq!(pane.selected.as_deref(), Some("customers"));
        assert!(!pane.card_key("orders", "f10", false), "单独 F10 不抢系统菜单");
        assert!(pane.card_key("missing", "enter", false));
        assert!(pane.take_open().is_none(), "不存在的表不能变成打开请求");
        assert!(!pane.take_back(), "键盘操作不触发返回");
        // 四态取值来自底座：可点卡片 hover/active 有底色、焦点环 2px 强调色、非禁用时可聚焦。
        let p = Palette::light();
        let style = kit::interaction_style(&p, false);
        assert_eq!(style.hover_bg, Some(p.hover));
        assert_eq!(style.active_bg, Some(p.active));
        assert_eq!(style.ring, Some(p.accent));
        assert_eq!(style.ring_width, kit::FOCUS_RING_W);
        assert!(style.focusable);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 页面级键盘：+/-/0 缩放，Esc 收起菜单与悬停。
    #[test]
    fn pane_keys_zoom_and_dismiss_without_touching_the_diagram() {
        let (mut pane, dir) = sample_pane("pane-keys");
        assert!(pane.pane_key("="));
        assert!((pane.zoom - 1.1).abs() < 1e-4, "{}", pane.zoom);
        assert!(pane.pane_key("-"));
        assert!((pane.zoom - 1.).abs() < 1e-4, "{}", pane.zoom);
        pane.pan_x = 40.;
        pane.pan_y = 24.;
        assert!(pane.pane_key("0"));
        assert_eq!((pane.pan_x, pane.pan_y, pane.zoom), (0., 0., 1.));
        pane.selected = Some("orders".into());
        pane.hovered = Some("orders".into());
        assert!(pane.pane_key("escape"));
        assert!(pane.selected.is_none() && pane.hovered.is_none());
        assert!(!pane.pane_key("x"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 四种状态各自的下一步与文字：无目标不显示成错误，空图给重新读取，失败给重试。
    #[test]
    fn four_states_carry_the_matching_next_step() {
        let (mut pane, dir) = sample_pane("states");
        assert_eq!(pane.state(), PaneState::Cards);
        assert!(pane.lines().iter().any(|line| line == "column 0 card customers"));

        let mut empty = sample_diagram();
        empty.tables.clear();
        empty.edges.clear();
        empty.notice = Some("这个库只有视图。".into());
        pane.diagram = Some(Arc::new(empty));
        assert_eq!(pane.state(), PaneState::Empty);
        let text = pane.lines().join("\n");
        assert!(text.contains("这个库只有视图。"), "空图要说明为什么空：{text}");

        pane.diagram = None;
        pane.error = "笔记 / shop：读取失败：权限不足".into();
        assert_eq!(pane.state(), PaneState::Error);
        assert!(pane.lines().join("\n").contains("权限不足"));

        pane.error.clear();
        pane.clear_target();
        assert_eq!(pane.state(), PaneState::NeedTarget);
        let text = pane.lines().join("\n");
        assert!(text.contains(&i18n::text(Locale::ZhCn, "diagram.needTarget")), "{text}");
        assert!(!text.contains(&i18n::text(Locale::ZhCn, "diagram.failed")), "没有目标不是读取失败：{text}");

        pane.loading = true;
        assert_eq!(pane.state(), PaneState::Loading);
        pane.loading = false;
        let _ = std::fs::remove_dir_all(&dir);
    }

    struct PaneHost {
        pane: gpui_kit::Entity<DiagramPane>,
    }

    impl Render for PaneHost {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div().id("pane-host").size_full().child(self.pane.clone())
        }
    }

    /// 打开一个指定宽度的关系图窗口。视口尺寸由窗口本身决定，不靠运行期 resize。
    fn open_diagram_window(cx: &mut TestAppContext, width: f32) -> (gpui_kit::AnyWindowHandle, gpui_kit::Entity<DiagramPane>) {
        let dir = std::env::temp_dir().join(format!("rillsql-diagram-widths-{}-{}", width as i32, uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let services = Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
        cx.update(|cx| {
            let pane = cx.new(|_| DiagramPane::new(services));
            let host = cx.new(|_| PaneHost { pane: pane.clone() });
            let bounds = gpui_kit::Bounds { origin: Point::default(), size: size(px(width), px(900.)) };
            let (window, _view) = gpui_kit::open_window(
                WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
                cx,
                |_, _cx| host,
            )
            .expect("关系图窗口");
            (window, pane)
        })
    }

    /// 读搜索输入控件里真正的文字（不是模型里的条件），用来钉住模型→控件的回写。
    fn search_control_text(pane: &gpui_kit::Entity<DiagramPane>, cx: &gpui_kit::App) -> Option<String> {
        pane.read(cx).search_input.as_ref().map(|field| field.state.read(cx).value().to_string())
    }

    /// 把关系图摆到指定状态：四态都不依赖异步读取，测试直接构造现场。
    fn fill_state(pane: &gpui_kit::Entity<DiagramPane>, state: &str, cx: &mut gpui_kit::App) {
        pane.update(cx, |pane, cx| {
            pane.error.clear();
            pane.loading = false;
            pane.navigation = None;
            pane.open_note = None;
            match state {
                "cards" => {
                    pane.connection_id = "notes".into();
                    pane.database = "shop".into();
                    pane.label = "笔记 / shop".into();
                    pane.diagram = Some(Arc::new(sample_diagram()));
                }
                "empty" => {
                    pane.connection_id = "notes".into();
                    pane.database = "shop".into();
                    pane.label = "笔记 / shop".into();
                    let mut diagram = sample_diagram();
                    diagram.tables.clear();
                    diagram.edges.clear();
                    pane.diagram = Some(Arc::new(diagram));
                }
                "error" => {
                    pane.diagram = None;
                    pane.error = "笔记 / shop：读取失败：权限不足".into();
                }
                "need-target" => pane.clear_target(),
                "loading" => {
                    pane.clear_target();
                    pane.connection_id = "notes".into();
                    pane.label = "笔记 / shop".into();
                    pane.loading = true;
                }
                _ => {}
            }
            cx.notify();
        });
    }

    /// 一个宽度 × 浅/深色 × 紧凑/标准：四态各自的下一步、卡片 Tab 可达与 Enter 等价操作、缩放可达。
    fn diagram_holds_at(cx: &mut TestAppContext, width: f32) {
        cx.update(gpui_kit::init);
        let (window, pane) = open_diagram_window(cx, width);
        let fill = |cx: &mut TestAppContext, state: &str| cx.update(|cx| fill_state(&pane, state, cx));
        let narrow = width < kit::NARROW_PAGE_WIDTH;
        for (dark, density) in [(false, "compact"), (false, "standard"), (true, "compact"), (true, "standard")] {
            cx.update(|cx| {
                kit::apply_theme(dark, cx);
                let metrics = if density == "compact" { kit::Metrics::compact() } else { kit::Metrics::standard() };
                metrics.publish(cx);
            });
            let tag = format!("{width}/{}/{density}", if dark { "dark" } else { "light" });
            cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();

            fill(cx, "cards");
            cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
            assert_eq!(cx.update(|cx| pane.read(cx).state()), PaneState::Cards, "{tag}");
            cx.update_window(window, |_, window, cx| {
                assert!(window.try_find("diagram-card:orders").is_some(), "{tag}：缺少订单卡片");
                assert!(window.try_find("diagram-card:customers").is_some(), "{tag}：缺少客户卡片");
                assert_eq!(window.try_find("diagram-tools").is_some(), narrow, "{tag}：只在窄宽换第二行工具条");
                window.click("diagram-zoom-in", cx);
                window.render_frame(cx);
            })
            .unwrap();
            assert!((cx.update(|cx| pane.read(cx).zoom) - 1.1).abs() < 1e-4, "{tag}：放大按钮在三档宽度都可用");

            // 搜索无命中：提示条 + 清除搜索，画布保持可见（“无命中”与“没有表”不是同一件事）。
            cx.update(|cx| {
                pane.update(cx, |pane, cx| {
                    pane.search = "zzz".into();
                    pane.highlight = None;
                    cx.notify();
                })
            });
            cx.update_window(window, |_, window, cx| {
                window.render_frame(cx);
                assert!(window.try_find("diagram-no-match").is_some(), "{tag}：搜索无命中要有提示条");
                assert!(window.try_find("diagram-card:orders").is_some(), "{tag}：无命中不隐藏画布");
            })
            .unwrap();
            // 条件要真的显示在输入框里（只看模型值会漏掉“控件没跟着改”的 bug）。
            assert_eq!(cx.update(|cx| search_control_text(&pane, cx)).as_deref(), Some("zzz"), "{tag}：搜索框要显示当前条件");
            cx.update_window(window, |_, window, cx| {
                window.click("diagram-search-clear", cx);
                window.render_frame(cx);
                assert!(window.try_find("diagram-no-match").is_none(), "{tag}：清除后不应再有无命中提示条");
                assert!(window.try_find("diagram-card:orders").is_some(), "{tag}：清除后画布仍在");
            })
            .unwrap();
            assert!(cx.update(|cx| pane.read(cx).search.is_empty()), "{tag}：清除搜索后模型条件为空");
            assert_eq!(
                cx.update(|cx| search_control_text(&pane, cx)).as_deref(),
                Some(""),
                "{tag}：清除搜索要把输入控件里的文字一起清掉（回写路径）"
            );

            // 纯键盘：Tab 停在卡片上按 Enter，走与双击相同的“打开表数据”请求。
            // 页头最多 6 个停靠点 + 2 张卡片，12 次足够；循环还有 15 秒墙钟上限，
            // 任何一次 frame/按键卡住都不会把这个用例拖成“挂住”，只会让它失败。
            let mut opened = false;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
            for _ in 0..12 {
                if std::time::Instant::now() > deadline {
                    break;
                }
                cx.update_window(window, |_, window, cx| {
                    window.focus_next(cx);
                    window.render_frame(cx);
                    window.press("enter", cx);
                })
                .unwrap();
                if cx.update(|cx| pane.read(cx).navigation.is_some()) {
                    opened = true;
                    break;
                }
            }
            assert!(opened, "{tag}：Tab 停在卡片后 Enter 必须能打开表");
            cx.update(|cx| {
                pane.update(cx, |pane, cx| {
                    pane.navigation = None;
                    pane.open_note = None;
                    cx.notify();
                })
            });

            fill(cx, "empty");
            cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
            assert_eq!(cx.update(|cx| pane.read(cx).state()), PaneState::Empty, "{tag}");
            cx.update_window(window, |_, window, _cx| {
                assert!(window.try_find("empty-state").is_some(), "{tag}：空图应有空态");
                assert!(window.try_find("state-action").is_some(), "{tag}：空图应有下一步");
                assert!(window.try_find("diagram-read-again").is_some(), "{tag}：空图应能重新读取");
            })
            .unwrap();

            fill(cx, "error");
            cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
            assert_eq!(cx.update(|cx| pane.read(cx).state()), PaneState::Error, "{tag}");
            cx.update_window(window, |_, window, _cx| {
                assert!(window.try_find("error-state").is_some(), "{tag}：失败应有错误态");
                assert!(window.try_find("error-state-detail").is_some(), "{tag}：失败要说明原因");
                assert!(window.try_find("diagram-retry").is_some(), "{tag}：失败应能重试");
            })
            .unwrap();

            fill(cx, "need-target");
            cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
            assert_eq!(cx.update(|cx| pane.read(cx).state()), PaneState::NeedTarget, "{tag}");
            cx.update_window(window, |_, window, _cx| {
                assert!(window.try_find("empty-state").is_some(), "{tag}：无目标是空态");
                assert!(window.try_find("error-state").is_none(), "{tag}：无目标不能用错误态");
                assert!(window.try_find("state-action").is_some(), "{tag}：无目标要给下一步");
            })
            .unwrap();

            fill(cx, "loading");
            cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
            assert_eq!(cx.update(|cx| pane.read(cx).state()), PaneState::Loading, "{tag}");
            cx.update_window(window, |_, window, _cx| {
                assert!(window.try_find("loading-state").is_some(), "{tag}：读取中应有加载态");
            })
            .unwrap();
        }
    }

    #[gpui_kit::test]
    fn diagram_four_states_and_keyboard_hold_at_1360(cx: &mut TestAppContext) {
        diagram_holds_at(cx, 1360.);
    }

    #[gpui_kit::test]
    fn diagram_four_states_and_keyboard_hold_at_900(cx: &mut TestAppContext) {
        diagram_holds_at(cx, 900.);
    }

    #[gpui_kit::test]
    fn diagram_four_states_and_keyboard_hold_at_640(cx: &mut TestAppContext) {
        diagram_holds_at(cx, 640.);
    }

    /// 单击选中卡片、点画布空白取消；右键请求表级菜单（外壳弹出）；拖卡片只移动它、不算单击。
    #[gpui_kit::test]
    fn card_clicks_select_drag_and_request_the_table_menu(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (window, pane) = open_diagram_window(cx, 1360.);
        cx.update(|cx| fill_state(&pane, "cards", cx));
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("diagram-card:orders", cx);
            window.render_frame(cx);
        })
        .unwrap();
        assert_eq!(cx.update(|cx| pane.read(cx).selected.clone()).as_deref(), Some("orders"), "单击选中卡片");
        let text = cx.update(|cx| pane.read(cx).lines().join("\n"));
        assert!(text.contains("hot orders") && text.contains("linked customers") && text.contains("hot-wire"), "选中后关联保持高亮：{text}");
        cx.update_window(window, |_, window, cx| {
            window.click_at("diagram-board", gpui_kit::point(px(6.), px(6.)), cx);
            window.render_frame(cx);
        })
        .unwrap();
        assert!(cx.update(|cx| pane.read(cx).selected.is_none()), "点画布空白取消选中");

        cx.update_window(window, |_, window, cx| {
            window.right_click("diagram-card:customers", cx);
            window.render_frame(cx);
        })
        .unwrap();
        let request = cx.update(|cx| pane.update(cx, |pane, _| pane.take_menu()));
        let (target, at) = request.expect("右键要请求表级菜单");
        assert_eq!(target.table, "customers");
        assert!(at.is_some(), "右键菜单弹在指针处");
        assert_eq!(cx.update(|cx| pane.read(cx).selected.clone()).as_deref(), Some("customers"));

        // 拖动卡片：只记偏移，不切换选中；重排布局清掉偏移。
        cx.update(|cx| {
            pane.update(cx, |pane, _| {
                pane.selected = None;
                pane.begin_card_drag("orders", 100., 100.);
                assert!(pane.pointer_move(160., 130.));
                pane.pointer_up();
                assert_eq!(pane.offsets.get("orders").copied(), Some((60., 30.)));
                assert!(pane.selected.is_none(), "拖动不是单击");
                pane.zoom = 2.;
                pane.begin_card_drag("orders", 0., 0.);
                assert!(pane.pointer_move(20., 0.));
                pane.pointer_up();
                assert_eq!(pane.offsets.get("orders").copied(), Some((70., 30.)), "偏移按缩放换算回画布坐标");
            });
        });
        cx.update(|cx| pane.update(cx, |pane, cx| pane.command("diagram-layout-reset", cx)));
        assert!(cx.update(|cx| pane.read(cx).offsets.is_empty()));

        // 滚轮平移画布，不缩放。
        let before = cx.update(|cx| (pane.read(cx).pan_y, pane.read(cx).zoom));
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.scroll("diagram-board", ScrollDelta::Pixels(gpui_kit::point(px(0.), px(-50.))), cx);
            window.render_frame(cx);
        })
        .unwrap();
        let after = cx.update(|cx| (pane.read(cx).pan_y, pane.read(cx).zoom));
        assert!((after.0 - (before.0 - 50.)).abs() < 0.5, "滚轮平移：{before:?} -> {after:?}");
        assert_eq!(after.1, before.1, "不按修饰键滚动不缩放");
    }

    /// 没有外键关系的表铺成多列，而不是一条竖排；长表折叠到键列与前几列。
    #[test]
    fn isolated_tables_spread_into_a_grid_and_long_tables_fold() {
        let tables: Vec<DiagramTable> = (0..30)
            .map(|index| DiagramTable {
                name: format!("t{index:02}"),
                columns: (0..20)
                    .map(|column| crate::diagram::DiagramColumn { name: format!("c{column}"), type_name: "int".into(), primary: column == 18 })
                    .collect(),
                outside: Vec::new(),
            })
            .collect();
        let foreign = std::collections::HashSet::new();
        let (rows, hidden) = visible_columns(&tables[0], &foreign, false);
        assert_eq!(rows.len(), SHOW_ROWS);
        assert!(rows.contains(&18), "主键列不能被折叠掉");
        assert_eq!(hidden, 20 - SHOW_ROWS);
        let (rows, hidden) = visible_columns(&tables[0], &foreign, true);
        assert_eq!((rows.len(), hidden), (20, 0), "展开后显示全部列");
        let heights: std::collections::HashMap<String, f32> = tables.iter().map(|table| (table.name.clone(), 300.)).collect();
        let places = auto_layout(&tables, &[], &heights);
        assert_eq!(places.len(), 30);
        let xs: std::collections::BTreeSet<i32> = places.values().map(|(x, _)| *x as i32).collect();
        assert!(xs.len() >= 4, "30 张无关系的表至少铺成 4 列：{xs:?}");
        let max_y = places.values().map(|(_, y)| *y).fold(0f32, f32::max);
        let max_x = places.values().map(|(x, _)| *x).fold(0f32, f32::max);
        assert!(max_x + CARD_W >= max_y * 0.8, "整体接近横向比例：宽 {max_x} 高 {max_y}");
    }

    fn pump(
        cx: &mut TestAppContext,
        window: gpui_kit::AnyWindowHandle,
        view: &gpui_kit::Entity<crate::app::shell::Workbench>,
        mut ready: impl FnMut(&crate::app::shell::Workbench, &gpui_kit::App) -> bool,
    ) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let done = cx.update(|app| ready(view.read(app), app));
            if done {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            cx.update_window(window, |_, window, cx| {
                window.render_frame(cx);
            })
            .unwrap();
            cx.run_until_parked();
            std::thread::sleep(std::time::Duration::from_millis(40));
        }
    }
}

const CARD_W: f32 = 240.;
const GAP_X: f32 = 96.;
const GAP_Y: f32 = 24.;
const GRID_GAP: f32 = 28.;
const HEAD_H: f32 = 34.;
const ROW_H: f32 = 22.;
/// 超过这么多列的表默认折叠；折叠后至少显示这么多行（键列一定在内）。
const COLLAPSE_AT: usize = 16;
const SHOW_ROWS: usize = 12;
/// 有关系的一层表堆到这么高就换列。
const MAX_STACK_H: f32 = 1400.;
const PAD: f32 = 32.;

fn paint_arrow(end: gpui_kit::Point<gpui_kit::Pixels>, toward: gpui_kit::Point<gpui_kit::Pixels>, color: gpui_kit::Hsla, window: &mut Window) {
    let dx = (end.x - toward.x).as_f32();
    let dy = (end.y - toward.y).as_f32();
    let len = (dx * dx + dy * dy).sqrt().max(1.);
    let ux = dx / len;
    let uy = dy / len;
    let px_n = -uy;
    let py_n = ux;
    let size = 9.;
    let wing = 4.5;
    let base_x = end.x.as_f32() - ux * size;
    let base_y = end.y.as_f32() - uy * size;
    let mut builder = PathBuilder::fill();
    builder.move_to(end);
    builder.line_to(gpui_kit::point(px(base_x + px_n * wing), px(base_y + py_n * wing)));
    builder.line_to(gpui_kit::point(px(base_x - px_n * wing), px(base_y - py_n * wing)));
    builder.close();
    if let Ok(path) = builder.build() {
        window.paint_path(path, color);
    }
}

impl DiagramPane {
    fn search_field(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui_kit::Entity<InputState> {
        if self.search_input.is_none() {
            let placeholder = SharedString::from(i18n::text(self.locale, "diagram.search"));
            let initial = self.search.clone();
            let state = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).default_value(initial.clone()));
            let subscription = cx.subscribe(&state, |this, input, event: &InputEvent, cx| {
                match event {
                    InputEvent::Change => {
                        let text = input.read(cx).value().to_string();
                        if let Some(field) = this.search_input.as_mut() {
                            field.sync = text.clone();
                        }
                        this.focus_search(text);
                        cx.notify();
                    }
                    // Enter / Shift+Enter 在命中之间前后跳转。
                    InputEvent::PressEnter { shift, .. } => {
                        this.step_match(if *shift { -1 } else { 1 });
                        cx.notify();
                    }
                    _ => {}
                }
            });
            self.search_input = Some(EntityInput { state, sync: self.search.clone(), _sub: subscription });
        }
        let wanted = self.search.clone();
        let field = self.search_input.as_mut().expect("search");
        if field.sync != wanted {
            field.sync = wanted.clone();
            field.state.update(cx, |state, cx| {
                if state.value().as_ref() != wanted {
                    state.set_value(wanted, window, cx);
                }
            });
        }
        field.state.clone()
    }
}

use crate::ui::kit::stack_frame;

impl Render for DiagramPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::of(cx);
        let metrics = kit::Metrics::of(cx);
        let entity = cx.entity();
        let locale = self.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let search = self.search_field(window, cx);
        let diagram = self.diagram.clone();
        let hovered = self.hovered.clone();
        let selected = self.selected.clone();
        let highlight = self.highlight.clone();
        let zoom = self.zoom;
        let mono = cx.theme().mono_font_family.clone();
        // 标题写全范围：连接 / 库（.模式），不只是一个连接名。
        let scope = match (self.database.is_empty(), self.schema.is_empty()) {
            (false, false) => format!("{}.{}", self.database, self.schema),
            (false, true) => self.database.clone(),
            (true, false) => self.schema.clone(),
            (true, true) => String::new(),
        };
        let title = SharedString::from(if self.label.is_empty() {
            i18n::text(locale, "catalog.tree.diagram")
        } else if scope.is_empty() || self.label.ends_with(&scope) {
            self.label.clone()
        } else {
            format!("{} / {scope}", self.label)
        });
        let viewport_w = window.viewport_size().width.as_f32();
        let pad_metrics = metrics.for_viewport(viewport_w);
        let narrow = viewport_w < kit::NARROW_PAGE_WIDTH;
        let layout_pad = pad_metrics.page_pad;
        let searching = !self.search.trim().is_empty();
        let match_count = self.matches.len();
        let match_index = self.match_index;
        let customized = !self.offsets.is_empty() || !self.expanded.is_empty();
        let tools = stack_frame(|| {
            let zoom_out = {
                let entity = entity.clone();
                kit::icon_button("diagram-zoom-out", IconName::Minus, t("diagram.zoom.out"), move |_, _, cx| {
                    entity.update(cx, |pane, cx| pane.command("diagram-zoom-out", cx))
                })
            };
            let zoom_in = {
                let entity = entity.clone();
                kit::icon_button("diagram-zoom-in", IconName::Plus, t("diagram.zoom.in"), move |_, _, cx| {
                    entity.update(cx, |pane, cx| pane.command("diagram-zoom-in", cx))
                })
            };
            // 缩放比例就是复位按钮：一眼看到当前比例，点一下回到 100%。
            let zoom_reset = {
                let entity = entity.clone();
                kit::tool_button("diagram-zoom-reset", None, SharedString::from(format!("{}%", (zoom * 100.).round() as i32)), move |_, _, cx| {
                    entity.update(cx, |pane, cx| pane.command("diagram-zoom-reset", cx))
                })
                .tooltip(t("diagram.zoom.resetHint"))
            };
            let zoom_fit = {
                let entity = entity.clone();
                kit::tool_button("diagram-zoom-fit", Some(IconName::Maximize), t("diagram.zoom.fit"), move |_, window, cx| {
                    let size = window.viewport_size();
                    entity.update(cx, |pane, cx| {
                        pane.fit_view(size.width.as_f32(), size.height.as_f32());
                        cx.notify();
                    });
                })
            };
            let layout_reset = customized.then(|| {
                let entity = entity.clone();
                kit::tool_button("diagram-layout-reset", Some(IconName::RotateCcw), t("diagram.layoutReset"), move |_, _, cx| {
                    entity.update(cx, |pane, cx| pane.command("diagram-layout-reset", cx))
                })
            });
            let back = {
                let entity = entity.clone();
                kit::tool_button("diagram-back", Some(IconName::ArrowLeft), t("compare.back"), move |_, _, cx| {
                    entity.update(cx, |pane, cx| pane.command("diagram-back", cx))
                })
            };
            // 搜索框后面跟着命中计数与上一处/下一处；Enter / Shift+Enter 等价。
            let mut search_box = div().flex().items_center().gap(px(Space::XS));
            search_box = if narrow { search_box.flex_1().min_w(px(0.)) } else { search_box.flex_none() };
            let input = kit::text_input("diagram-search", &search, t("diagram.search"))
                .prefix(kit::input_prefix(IconName::Search, 13., p.text_faint))
                .cleanable(true);
            search_box = search_box.child(if narrow { div().flex_1().min_w(px(0.)).child(input) } else { div().w(px(220.)).child(input) });
            if searching && match_count > 0 {
                let prev = {
                    let entity = entity.clone();
                    kit::icon_button("diagram-search-prev", IconName::ChevronUp, t("grid.find.prev"), move |_, _, cx| {
                        entity.update(cx, |pane, cx| pane.command("diagram-search-prev", cx))
                    })
                    .xsmall()
                    .disabled(match_count < 2)
                };
                let next = {
                    let entity = entity.clone();
                    kit::icon_button("diagram-search-next", IconName::ChevronDown, t("grid.find.next"), move |_, _, cx| {
                        entity.update(cx, |pane, cx| pane.command("diagram-search-next", cx))
                    })
                    .xsmall()
                    .disabled(match_count < 2)
                };
                search_box = search_box
                    .child(
                        div()
                            .id("diagram-search-count")
                            .flex_none()
                            .text_size(px(TypeScale::XS))
                            .font_features(kit::tabular_figures())
                            .text_color(p.text_muted)
                            .child(SharedString::from(format!("{} / {match_count}", match_index + 1))),
                    )
                    .child(prev)
                    .child(next);
            }
            (search_box, zoom_out, zoom_reset, zoom_in, zoom_fit, layout_reset, back)
        });
        let stats = diagram.as_ref().map(|diagram| {
            SharedString::from(i18n::fill(
                &i18n::text(locale, "diagram.stats"),
                &[("tables", &diagram.tables.len().to_string()), ("keys", &diagram.edges.len().to_string())],
            ))
        });
        let stats_el = stats.clone().map(|text| div().flex_none().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(text));
        let header = if narrow {
            stack_frame(|| {
                let (search_box, zoom_out, zoom_reset, zoom_in, zoom_fit, layout_reset, back) = tools;
                div()
                    .id("diagram-header")
                    .flex_none()
                    .flex()
                    .flex_col()
                    .bg(p.surface)
                    .border_b_1()
                    .border_color(p.line)
                    .child(div().id("diagram-title").tooltip_text(t("diagram.readonly")).child(kit::page_header(title, &[], &pad_metrics, &p).children(stats_el)))
                    .child(
                        div()
                            .id("diagram-tools")
                            .test_support()
                            .flex()
                            .items_center()
                            .gap(px(Space::SM))
                            .px(px(layout_pad))
                            .pb(px(Space::MD))
                            .child(search_box)
                            .child(zoom_out)
                            .child(zoom_reset)
                            .child(zoom_in)
                            .child(zoom_fit)
                            .children(layout_reset)
                            .child(back),
                    )
                    .into_any_element()
            })
        } else {
            stack_frame(|| {
                let (search_box, zoom_out, zoom_reset, zoom_in, zoom_fit, layout_reset, back) = tools;
                div()
                    .id("diagram-header")
                    .flex_none()
                    .flex()
                    .flex_col()
                    .child(
                        div().id("diagram-title").tooltip_text(t("diagram.readonly")).child(
                            kit::page_header(title, &[], &pad_metrics, &p)
                                .children(stats_el)
                                .child(div().flex_1())
                                .child(search_box)
                                .child(div().flex().items_center().gap(px(Space::XXS)).child(zoom_out).child(zoom_reset).child(zoom_in))
                                .child(zoom_fit)
                                .children(layout_reset)
                                .child(back),
                        ),
                    )
                    .into_any_element()
            })
        };

        let pane_state = self.state();
        // 没有目标时的下一步：说明 + 打开资源管理器的键位。不是错误，也不用错误色。
        let need_target_state = kit::empty_state_with_action(
            IconName::ChartNetwork,
            t("catalog.tree.diagram"),
            t("diagram.needTarget"),
            div()
                .flex()
                .items_center()
                .gap(px(Space::SM))
                .child(kit::kbd(kit::shortcut("Ctrl+B"), &p))
                .child(div().text_size(px(TypeScale::SM)).text_color(p.text_muted).child(t("shell.command.toggleSidebar"))),
            &p,
        )
        .into_any_element();
        let mut body = div().id("diagram-scroll").relative().flex_1().min_h(px(0.)).overflow_hidden().bg(p.chrome).flex().flex_col();
        if matches!(pane_state, PaneState::Loading) {
            body = body.child(
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .px(px(layout_pad))
                    .child(kit::loading_state(t("diagram.loading"), &p)),
            );
        } else if matches!(pane_state, PaneState::Error) {
            let detail = SharedString::from(self.error.clone());
            let block = if self.connection_id.is_empty() {
                kit::error_state(t("diagram.failed"), detail, &p).into_any_element()
            } else {
                let retry = {
                    let entity = entity.clone();
                    kit::secondary_button("diagram-retry", Some(IconName::RefreshCw), t("common.retry"), move |_, _, cx| {
                        entity.update(cx, |pane, cx| pane.command("diagram-retry", cx))
                    })
                };
                // 重试放进状态块的动作位，与标题文字左对齐（38px 缩进），不再另起一行居中追加。
                kit::error_state_with_action(t("diagram.failed"), detail, retry, &p).into_any_element()
            };
            body = body.child(div().size_full().flex().flex_col().items_start().justify_center().px(px(layout_pad)).child(block));
        } else if matches!(pane_state, PaneState::Empty) {
            let notice = diagram.as_ref().and_then(|diagram| diagram.notice.clone()).filter(|notice| !notice.is_empty());
            let detail = SharedString::from(notice.unwrap_or_else(|| i18n::text(locale, "diagram.emptyDetail")));
            let reread = {
                let entity = entity.clone();
                kit::secondary_button("diagram-read-again", Some(IconName::RefreshCw), t("common.retry"), move |_, _, cx| {
                    entity.update(cx, |pane, cx| pane.command("diagram-retry", cx))
                })
            };
            body = body.child(
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .items_start()
                    .justify_center()
                    .px(px(layout_pad))
                    .child(kit::empty_state_with_action(IconName::Workflow, t("diagram.empty"), detail, reread, &p)),
            );
        } else if let Some(diagram) = diagram.as_ref() {
            // 搜索无命中只加一条提示条：画布保持可见，清空条件即恢复现场。
            if searching && highlight.is_none() {
                let clear = {
                    let entity = entity.clone();
                    kit::secondary_button("diagram-search-clear", None, t("diagram.searchClear"), move |_, _, cx| {
                        entity.update(cx, |pane, cx| pane.command("diagram-search-clear", cx))
                    })
                };
                body = body.child(
                    div()
                        .id("diagram-no-match")
                        .test_support()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(Space::MD))
                        .px(px(layout_pad))
                        .py(px(Space::SM))
                        .bg(p.surface)
                        .border_b_1()
                        .border_color(p.line)
                        .child(kit::icon(IconName::SearchX, 13., p.text_muted))
                        .child(div().text_size(px(TypeScale::SM)).text_color(p.text_muted).child(t("diagram.searchNoMatch")))
                        .child(clear),
                );
            }
            let foreign = foreign_columns(diagram);
            // 每张卡片要画的列（长表折叠）与未缩放高度。
            let mut shown: std::collections::HashMap<String, (Vec<usize>, usize)> = std::collections::HashMap::new();
            let mut heights: std::collections::HashMap<String, f32> = std::collections::HashMap::new();
            for table in &diagram.tables {
                let (rows, hidden) = visible_columns(table, &foreign, self.expanded.contains(&table.name));
                let folded = hidden > 0 || self.expanded.contains(&table.name) && table.columns.len() > COLLAPSE_AT;
                heights.insert(table.name.clone(), card_height_for(table, rows.len(), folded));
                shown.insert(table.name.clone(), (rows, hidden));
            }
            // 卡片位置：自动布局 + 用户拖过的偏移。先算未缩放坐标，再平移和缩放。
            let mut base = auto_layout(&diagram.tables, &diagram.edges, &heights);
            for (name, (dx, dy)) in &self.offsets {
                if let Some(place) = base.get_mut(name) {
                    place.0 += dx;
                    place.1 += dy;
                }
            }
            let (mut min_x, mut min_y, mut max_x, mut max_y) = (f32::MAX, f32::MAX, 0f32, 0f32);
            for (name, (x, y)) in &base {
                let h = heights.get(name).copied().unwrap_or(HEAD_H);
                min_x = min_x.min(*x);
                min_y = min_y.min(*y);
                max_x = max_x.max(x + CARD_W);
                max_y = max_y.max(y + h);
            }
            if base.is_empty() {
                (min_x, min_y) = (0., 0.);
            }
            // 内容范围（未缩放）：含用户拖到左上方的卡片，适应视口按它算。
            self.content_x0 = min_x.min(PAD) - PAD;
            self.content_y0 = min_y.min(PAD) - PAD;
            self.content_w = max_x + PAD - self.content_x0;
            self.content_h = max_y + PAD - self.content_y0;
            let center = self.center_target.take().or_else(|| if self.center_pending { highlight.clone() } else { None });
            if self.center_pending || center.is_some() {
                if let Some(name) = center {
                    if let Some(&(x, y)) = base.get(&name) {
                        let h = heights.get(&name).copied().unwrap_or(HEAD_H);
                        let board = self.board.get();
                        let (w, vh) = if board.size.width.as_f32() > 1. {
                            (board.size.width.as_f32(), board.size.height.as_f32())
                        } else {
                            let view = window.viewport_size();
                            (view.width.as_f32() * 0.8, view.height.as_f32() * 0.8)
                        };
                        self.pan_x = w * 0.5 - (x + CARD_W * 0.5) * zoom;
                        self.pan_y = vh * 0.4 - (y + h.min(400.) * 0.5) * zoom;
                    }
                }
                self.center_pending = false;
            }
            let pan_x = self.pan_x;
            let pan_y = self.pan_y;
            let places: std::collections::HashMap<String, (f32, f32, f32)> = base
                .iter()
                .map(|(name, (x, y))| {
                    let h = heights.get(name).copied().unwrap_or(HEAD_H);
                    (name.clone(), (pan_x + x * zoom, pan_y + y * zoom, h * zoom))
                })
                .collect();
            let card_w = CARD_W * zoom;
            let head_h = HEAD_H * zoom;
            let row_h = ROW_H * zoom;
            // 悬停优先，其次是选中的卡片：关联表与连线高亮，其余变淡。
            let focus = hovered.clone().or(selected.clone());
            let matches: std::collections::HashSet<String> = if searching { self.matches.iter().cloned().collect() } else { Default::default() };
            let dragging = self.drag.as_ref().is_some_and(|drag| drag.moved) || self.card_drag.as_ref().is_some_and(|drag| drag.moved);
            let board_entity = entity.clone();
            let board_cell = self.board.clone();
            let mut canvas_box = stack_frame(|| {
                div()
                    .id("diagram-board")
                    .test_support()
                    .relative()
                    .flex_1()
                    .min_h(px(0.))
                    .w_full()
                    .when(dragging, |this| this.cursor_grabbing())
                    // 滚轮/触控板平移画布；按住 ⌘/Ctrl 滚动时以指针为中心缩放。
                    .on_scroll({
                        let board_entity = board_entity.clone();
                        let board_cell = board_cell.clone();
                        move |event: &ScrollEvent, _, cx| {
                            let zooming = event.modifiers.secondary(cx) || event.modifiers.control;
                            let (dx, dy) = match event.delta {
                                ScrollDelta::Pixels(point) => (point.x.as_f32(), point.y.as_f32()),
                                ScrollDelta::Lines(point) => (point.x * 40., point.y * 40.),
                            };
                            if dx.abs() < 0.01 && dy.abs() < 0.01 {
                                return;
                            }
                            let origin = board_cell.get().origin;
                            let local_x = (event.position.x - origin.x).as_f32();
                            let local_y = (event.position.y - origin.y).as_f32();
                            board_entity.update(cx, |pane, cx| {
                                if zooming {
                                    let factor = (1. + dy / 400.).clamp(0.8, 1.25);
                                    pane.zoom_at(pane.zoom * factor, local_x, local_y);
                                } else {
                                    pane.pan_x += dx;
                                    pane.pan_y += dy;
                                }
                                cx.notify();
                            });
                        }
                    })
                    .on_mouse_down(MouseButton::Left, {
                        let board_entity = board_entity.clone();
                        move |event: &MouseDownEvent, _, cx| {
                            board_entity.update(cx, |pane, _| pane.begin_drag(event.position.x.as_f32(), event.position.y.as_f32()));
                        }
                    })
                    .on_mouse_down(MouseButton::Middle, {
                        let board_entity = board_entity.clone();
                        move |event: &MouseDownEvent, _, cx| {
                            board_entity.update(cx, |pane, _| pane.begin_drag(event.position.x.as_f32(), event.position.y.as_f32()));
                        }
                    })
            });
            canvas_box = stack_frame(|| {
                canvas_box
                    .on_mouse_move({
                        let board_entity = board_entity.clone();
                        move |event: &MouseMoveEvent, _, cx| {
                            board_entity.update(cx, |pane, cx| {
                                if pane.pointer_move(event.position.x.as_f32(), event.position.y.as_f32()) {
                                    cx.notify();
                                }
                            });
                        }
                    })
                    .on_mouse_up(MouseButton::Left, {
                        let board_entity = board_entity.clone();
                        move |_: &gpui_kit::MouseUpEvent, _, cx| {
                            board_entity.update(cx, |pane, cx| {
                                pane.pointer_up();
                                cx.notify();
                            });
                        }
                    })
                    .on_mouse_up(MouseButton::Middle, {
                        let board_entity = board_entity.clone();
                        move |_: &gpui_kit::MouseUpEvent, _, cx| {
                            board_entity.update(cx, |pane, cx| {
                                pane.end_drag();
                                cx.notify();
                            });
                        }
                    })
            });
            // 连线单独一帧，避免和画板事件叠在同一个未优化栈帧里。
            let wires_layer = stack_frame(|| {
                let row_center = |table: &str, column: Option<&String>| -> Option<f32> {
                    let source = diagram.tables.iter().find(|item| item.name == table)?;
                    let index = column_index(source, column?)?;
                    let (rows, _) = shown.get(table)?;
                    let visible = rows.iter().position(|row| *row == index)?;
                    Some(HEAD_H + 5. + ROW_H * visible as f32 + ROW_H / 2.)
                };
                let mut wires: Vec<(gpui_kit::Point<gpui_kit::Pixels>, gpui_kit::Point<gpui_kit::Pixels>, bool)> = Vec::new();
                for edge in &diagram.edges {
                    let (Some(from), Some(to)) = (places.get(&edge.from_table), places.get(&edge.to_table)) else { continue };
                    let start_y = from.1 + row_center(&edge.from_table, edge.from_columns.first()).unwrap_or(HEAD_H / 2.) * zoom;
                    let end_y = to.1 + row_center(&edge.to_table, edge.to_columns.first()).map(|y| y * zoom).unwrap_or(head_h / 2.);
                    let (start_x, end_x) = if from.0 > to.0 + card_w {
                        (from.0, to.0 + card_w)
                    } else if from.0 + card_w < to.0 {
                        (from.0 + card_w, to.0)
                    } else {
                        (from.0 + card_w, to.0 + card_w)
                    };
                    wires.push((gpui_kit::point(px(start_x), px(start_y)), gpui_kit::point(px(end_x), px(end_y)), edge_hot(focus.as_deref(), edge)));
                }
                let dim = focus.is_some();
                let (wire, hot_wire, dim_wire) = (p.line_strong, p.accent, p.line);
                let cell = board_cell.clone();
                canvas(
                    move |_, _, _| (),
                    move |bounds, _, window, _| {
                        cell.set(bounds);
                        // 高亮的连线最后画，压在其它线上面。
                        let mut ordered: Vec<&(gpui_kit::Point<gpui_kit::Pixels>, gpui_kit::Point<gpui_kit::Pixels>, bool)> = wires.iter().collect();
                        ordered.sort_by_key(|(_, _, hot)| *hot);
                        for (start, end, hot) in ordered {
                            let start = bounds.origin + *start;
                            let end = bounds.origin + *end;
                            let bend = ((end.x - start.x).abs() / 2.).max(px(40.));
                            let same_side = (start.x - end.x).abs() < px(1.);
                            let (c1, c2) = if same_side {
                                (gpui_kit::point(start.x + px(60.), start.y), gpui_kit::point(end.x + px(60.), end.y))
                            } else if start.x > end.x {
                                (gpui_kit::point(start.x - bend, start.y), gpui_kit::point(end.x + bend, end.y))
                            } else {
                                (gpui_kit::point(start.x + bend, start.y), gpui_kit::point(end.x - bend, end.y))
                            };
                            let mut builder = PathBuilder::stroke(px(if *hot { 2. } else { 1.3 }));
                            builder.move_to(start);
                            builder.cubic_bezier_to(end, c1, c2);
                            if let Ok(path) = builder.build() {
                                let color = if *hot { hot_wire } else if dim { dim_wire } else { wire };
                                window.paint_path(path, color);
                                paint_arrow(end, c2, color, window);
                            }
                            // 起点画一个小圆点，看得出线从哪一列出发。
                            let mut dot = PathBuilder::fill();
                            let r = px(2.5);
                            dot.move_to(gpui_kit::point(start.x - r, start.y));
                            dot.arc_to(gpui_kit::point(r, r), px(0.), false, true, gpui_kit::point(start.x + r, start.y));
                            dot.arc_to(gpui_kit::point(r, r), px(0.), false, true, gpui_kit::point(start.x - r, start.y));
                            if let Ok(path) = dot.build() {
                                window.paint_path(path, if *hot { hot_wire } else if dim { dim_wire } else { wire });
                            }
                        }
                    },
                )
                .absolute()
                .inset_0()
                .into_any_element()
            });
            canvas_box = canvas_box.child(wires_layer);
            for table in &diagram.tables {
                let Some((x, y, _)) = places.get(&table.name).copied() else { continue };
                let (rows_shown, hidden) = shown.get(&table.name).cloned().unwrap_or_default();
                let expanded = self.expanded.contains(&table.name);
                let card = stack_frame(|| {
                    let name = table.name.clone();
                    let mut mark = hover_mark(focus.as_deref(), &name, diagram);
                    if focus.is_none() && searching && !matches.is_empty() && !matches.contains(&name) {
                        mark = "dim";
                    }
                    let label = SharedString::from(format!("{mark} {}", table.name));
                    let hovered_name = name.clone();
                    let card_entity = entity.clone();
                    let is_selected = selected.as_deref() == Some(name.as_str());
                    let is_hit = highlight.as_deref() == Some(table.name.as_str());
                    let (mut border, head_bg, opacity) = match mark {
                        "hot" => (p.accent, p.accent_soft, 1.),
                        "linked" => (p.accent, p.surface, 1.),
                        "dim" => (p.line, p.surface, 0.4),
                        _ => (p.line_strong, p.raised, 1.),
                    };
                    if is_hit || is_selected {
                        border = p.accent;
                    }
                    let mut rows = div().flex().flex_col().py(px(5. * zoom));
                    for index in &rows_shown {
                        let column = &table.columns[*index];
                        let fk = foreign.contains(&(table.name.as_str(), column.name.as_str()));
                        let icon = if column.primary {
                            IconName::Key
                        } else if fk {
                            IconName::Link
                        } else {
                            IconName::Dot
                        };
                        let icon_color = if column.primary {
                            p.warn
                        } else if fk {
                            p.accent
                        } else {
                            p.text_faint
                        };
                        rows = rows.child(
                            div()
                                .h(px(row_h))
                                .px(px(12. * zoom.min(1.)))
                                .flex()
                                .items_center()
                                .gap(px(Space::SM))
                                .text_size(px(TypeScale::SM))
                                .child(kit::icon(icon, 11., icon_color))
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .font_family(mono.clone())
                                        .when(column.primary, |this| this.font_weight(FontWeight::MEDIUM))
                                        .when(fk, |this| this.text_color(p.accent_text))
                                        .child(SharedString::from(column.name.clone())),
                                )
                                .when(!column.type_name.is_empty(), |this| {
                                    this.child(
                                        div()
                                            .ml_auto()
                                            .flex_none()
                                            .max_w(px(96. * zoom))
                                            .truncate()
                                            .text_size(px(TypeScale::XS))
                                            .text_color(p.text_faint)
                                            .child(SharedString::from(column.type_name.clone())),
                                    )
                                }),
                        );
                    }
                    if table.columns.is_empty() {
                        rows = rows.child(div().h(px(row_h)).px(px(Space::LG)).text_size(px(TypeScale::SM)).text_color(p.text_faint).child("—"));
                    }
                    // 折叠的长表：一行“+N 列”，点一下展开；展开后可以收起。
                    if hidden > 0 || (expanded && table.columns.len() > COLLAPSE_AT) {
                        let toggle_entity = entity.clone();
                        let toggle_name = name.clone();
                        let text = if hidden > 0 {
                            i18n::fill(&i18n::text(locale, "diagram.moreColumns"), &[("count", &hidden.to_string())])
                        } else {
                            i18n::text(locale, "diagram.collapseColumns")
                        };
                        rows = rows.child(
                            div()
                                .id(SharedString::from(format!("diagram-expand:{name}")))
                                .h(px(row_h))
                                .px(px(Space::LG))
                                .flex()
                                .items_center()
                                .gap(px(Space::SM))
                                .cursor_pointer()
                                .text_size(px(TypeScale::XS))
                                .text_color(p.accent_text)
                                .hover(|this| this.bg(p.hover))
                                .child(kit::icon(if hidden > 0 { IconName::ChevronDown } else { IconName::ChevronUp }, 11., p.accent_text))
                                .child(SharedString::from(text))
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .on_click(move |_, _, cx| {
                                    let id = format!("diagram-expand:{toggle_name}");
                                    toggle_entity.update(cx, |pane, cx| pane.command(&id, cx));
                                }),
                        );
                    }
                    for note in &table.outside {
                        rows = rows.child(
                            div()
                                .h(px(row_h))
                                .px(px(Space::LG))
                                .flex()
                                .items_center()
                                .gap(px(Space::SM))
                                .text_size(px(TypeScale::XS))
                                .text_color(p.warn)
                                .child(kit::icon(IconName::ExternalLink, 11., p.warn))
                                .child(div().truncate().child(SharedString::from(note.clone()))),
                        );
                    }
                    let card = div()
                        .id(SharedString::from(format!("diagram-card:{name}")))
                        .test_support()
                        .role(gpui_kit::Role::Button)
                        .aria_label(label)
                        .absolute()
                        .left(px(x))
                        .top(px(y))
                        .w(px(card_w))
                        .rounded(px(Radius::PANEL))
                        .when(is_selected || is_hit, |this| this.border_2())
                        .when(!(is_selected || is_hit), |this| this.border_1())
                        .border_color(border)
                        .bg(p.surface)
                        .when(is_selected, |this| this.shadow_md())
                        .when(!is_selected, |this| this.shadow_sm())
                        .overflow_hidden()
                        .child(
                            div()
                                .id(SharedString::from(format!("diagram-card-head:{name}")))
                                .h(px(head_h))
                                .px(px(12. * zoom.min(1.)))
                                .flex()
                                .items_center()
                                .gap(px(Space::MD))
                                .bg(head_bg)
                                .border_b_1()
                                .border_color(p.line)
                                .cursor_grab()
                                .tooltip_text(SharedString::from(table.name.clone()))
                                .child(kit::icon(IconName::Table, 13., p.accent))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(px(TypeScale::MD))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(SharedString::from(table.name.clone())),
                                )
                                .child(
                                    div()
                                        .flex_none()
                                        .text_size(px(TypeScale::XS))
                                        .font_features(kit::tabular_figures())
                                        .text_color(p.text_faint)
                                        .child(SharedString::from(table.columns.len().to_string())),
                                ),
                        )
                        .child(rows)
                        .on_mouse_down(MouseButton::Left, {
                            let name = name.clone();
                            let card_entity = card_entity.clone();
                            move |event: &MouseDownEvent, _, cx| {
                                // 双击打开数据；单击按下开始拖动卡片（没拖动就是选中）。不平移画布、不发 SQL。
                                let double = event.click_count >= 2;
                                card_entity.update(cx, |pane, cx| {
                                    if double {
                                        pane.card_drag = None;
                                        pane.request_open(&name, false);
                                    } else {
                                        pane.begin_card_drag(&name, event.position.x.as_f32(), event.position.y.as_f32());
                                    }
                                    cx.notify();
                                });
                                cx.stop_propagation();
                            }
                        })
                        .on_mouse_down(MouseButton::Right, {
                            let name = name.clone();
                            let card_entity = card_entity.clone();
                            move |event: &MouseDownEvent, _, cx| {
                                // 右键：选中这张表并请外壳弹出表级菜单（与对象树同一册）。
                                card_entity.update(cx, |pane, cx| {
                                    pane.open_card_menu(&name, Some(event.position));
                                    cx.notify();
                                });
                                cx.stop_propagation();
                            }
                        })
                        .on_hover(move |inside, _, cx| {
                            let hovered_name = hovered_name.clone();
                            card_entity.update(cx, |pane, cx| {
                                if pane.card_drag.as_ref().is_some_and(|drag| drag.moved) {
                                    return;
                                }
                                if *inside {
                                    pane.hovered = Some(hovered_name);
                                } else if pane.hovered.as_deref() == Some(hovered_name.as_str()) {
                                    pane.hovered = None;
                                }
                                cx.notify();
                            });
                        })
                        .on_key_down({
                            // 卡片是 Tab 停靠点：Enter / Shift+Enter 打开，空格选中，菜单键或 Shift+F10 出菜单，Esc 收起。
                            let name = name.clone();
                            let key_entity = entity.clone();
                            move |event: &gpui_kit::KeyDownEvent, _, cx| {
                                let key = event.keystroke.key.as_str();
                                let shift = event.keystroke.modifiers.shift;
                                let handled = key_entity.update(cx, |pane, cx| {
                                    let handled = pane.card_key(&name, key, shift);
                                    if handled {
                                        cx.notify();
                                    }
                                    handled
                                });
                                if handled {
                                    cx.stop_propagation();
                                }
                            }
                        });
                    // hover / active / focus-visible / disabled 四态走底座，不再自己拼底色。
                    // 悬停时其余卡片要按既有 dim 变淡，所以不透明度放在底座之后设置。
                    kit::interactive(card, &p, false).opacity(opacity).into_any_element()
                });
                canvas_box = canvas_box.child(card);
            }
            body = body.child(canvas_box);
            // 画布左下角的操作提示：第一次用的人知道能拖、能滚、能右键。
            if !narrow {
                body = body.child(
                    div()
                        .id("diagram-hint")
                        .absolute()
                        .left(px(Space::LG))
                        .bottom(px(Space::MD))
                        .px(px(Space::MD))
                        .py(px(3.))
                        .rounded(px(Radius::CONTROL))
                        .bg(p.surface)
                        .border_1()
                        .border_color(p.line_soft)
                        .text_size(px(TypeScale::XS))
                        .text_color(p.text_faint)
                        .child(t("diagram.hint")),
                );
            }
        } else {
            // 兜底与 NeedTarget 同一份呈现，不再出现第五种状态。
            body = body.child(div().size_full().flex().flex_col().items_start().justify_center().px(px(layout_pad)).child(need_target_state));
        }

        let footer = stack_frame(|| {
            let mut footer = div().flex_none().flex().flex_col();
            if let Some(diagram) = diagram.as_ref() {
                if !diagram.omitted.is_empty() || diagram.notice.is_some() {
                    let mut notes = div().px(px(metrics.page_pad)).py(px(Space::MD)).flex().flex_col().gap(px(Space::XS)).border_t_1().border_color(p.line).text_size(px(TypeScale::SM)).text_color(p.warn);
                    if let Some(notice) = &diagram.notice {
                        notes = notes.child(SharedString::from(notice.clone()));
                    }
                    for item in &diagram.omitted {
                        notes = notes.child(SharedString::from(format!("{} · {} {}", i18n::text(locale, "diagram.omitted"), item.name, item.message)));
                    }
                    footer = footer.child(notes);
                }
                let focus = hovered.as_deref().or(selected.as_deref());
                for edge in &diagram.edges {
                    let mark = if edge_hot(focus, edge) { "hot-wire" } else if focus.is_some() { "dim-wire" } else { "wire" };
                    let label = SharedString::from(format!("{mark} {}.{} -> {}.{}", edge.from_table, edge.from_columns.join(", "), edge.to_table, edge.to_columns.join(", ")));
                    footer = footer.child(div().id(SharedString::from(format!("diagram-wire:{}:{}:{}", edge.name, edge.from_table, edge.to_table))).hidden().aria_label(label));
                }
            }
            footer.into_any_element()
        });

        stack_frame(|| {
            div()
                .id("diagram-pane")
                .size_full()
                .flex()
                .flex_col()
                .on_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, window, cx| {
                    // 键盘事件按“焦点元素 → 祖先”冒泡到这里：卡片自己先处理，菜单按钮、搜索框的也不误伤。
                    let key = event.keystroke.key.as_str();
                    if this.search_focused(window, cx) {
                        if key == "escape" {
                            this.clear_search();
                            cx.stop_propagation();
                            cx.notify();
                        }
                        return;
                    }
                    if this.pane_key(key) {
                        cx.stop_propagation();
                        cx.notify();
                    }
                }))
                .child(header)
                .child(body)
                .child(footer)
                .when(cfg!(test), |this| this.child(div().id("diagram-summary").hidden().child(SharedString::from(self.lines().join("\n")))))
        })
    }
}
