//! 只读关系图。表卡片、外键连线、悬停、遗漏说明和文字清单都不写库。

use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::{
    canvas, AppContext as _, ClickEvent, Context, Focusable as _, FontWeight, InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, ParentElement as _, PathBuilder,
    Render, ScrollDelta, ScrollEvent, SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div, px,
};

use crate::ui::kit::{self, Palette, Radius, Tip as _, TypeScale};

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

pub struct DiagramPane {
    services: Arc<AppServices>,
    locale: Locale,
    connection_id: String,
    database: String,
    schema: String,
    label: String,
    diagram: Option<Diagram>,
    error: String,
    loading: bool,
    hovered: Option<String>,
    token: Option<TaskToken>,
    applied: String,
    zoom: f32,
    pan_x: f32,
    pan_y: f32,
    search: String,
    highlight: Option<String>,
    center_pending: bool,
    navigation: Option<DiagramOpen>,
    /// 打开请求被外壳取走之后，文字清单仍保留这一行，测试和悬停清单能看到。
    open_note: Option<String>,
    card_menu: Option<String>,
    back: bool,
    drag: Option<DragPan>,
    content_w: f32,
    content_h: f32,
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
            token: None,
            applied: String::new(),
            zoom: 1.,
            pan_x: 0.,
            pan_y: 0.,
            search: String::new(),
            highlight: None,
            center_pending: false,
            navigation: None,
            open_note: None,
            card_menu: None,
            back: false,
            drag: None,
            content_w: 0.,
            content_h: 0.,
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
            self.zoom = clamp_zoom(self.zoom + 0.1);
        } else if id == "diagram-zoom-out" {
            self.zoom = clamp_zoom(self.zoom - 0.1);
        } else if id == "diagram-zoom-reset" {
            self.zoom = 1.;
            self.pan_x = 0.;
            self.pan_y = 0.;
        } else if id == "diagram-search-clear" {
            self.clear_search();
        } else if id == "diagram-menu-close" {
            self.card_menu = None;
        } else if let Some(name) = id.strip_prefix("diagram-open-data:") {
            self.request_open(name, false);
        } else if let Some(name) = id.strip_prefix("diagram-open-structure:") {
            self.request_open(name, true);
        } else if let Some(name) = id.strip_prefix("diagram-menu:") {
            self.card_menu = Some(name.to_owned());
        }
        cx.notify();
    }

    fn reset_canvas(&mut self) {
        self.hovered = None;
        self.zoom = 1.;
        self.pan_x = 0.;
        self.pan_y = 0.;
        self.search.clear();
        self.highlight = None;
        self.center_pending = false;
        self.navigation = None;
        self.card_menu = None;
        self.drag = None;
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
        let known = self.diagram.as_ref().is_some_and(|diagram| diagram.tables.iter().any(|item| item.name == table));
        if !known {
            return;
        }
        self.card_menu = None;
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
        let needle = self.search.trim().to_ascii_lowercase();
        if needle.is_empty() {
            self.highlight = None;
            self.center_pending = false;
            return;
        }
        self.highlight = self.diagram.as_ref().and_then(|diagram| {
            diagram.tables.iter().find(|table| table.name.to_ascii_lowercase().contains(&needle)).map(|table| table.name.clone())
        });
        self.center_pending = self.highlight.is_some();
    }

    fn fit_view(&mut self, viewport_w: f32, viewport_h: f32) {
        if self.content_w <= 1. || self.content_h <= 1. {
            self.zoom = 1.;
            self.pan_x = 0.;
            self.pan_y = 0.;
            return;
        }
        let zx = (viewport_w - 24.).max(80.) / self.content_w;
        let zy = (viewport_h - 24.).max(80.) / self.content_h;
        self.zoom = clamp_zoom(zx.min(zy));
        self.pan_x = 12.;
        self.pan_y = 12.;
    }

    fn begin_drag(&mut self, x: f32, y: f32) {
        self.drag = Some(DragPan { x, y, pan_x: self.pan_x, pan_y: self.pan_y, moved: false });
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
                self.diagram = Some(diagram);
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
            "contextmenu" | "menu" => self.open_card_menu(table),
            "f10" if shift => self.open_card_menu(table),
            "escape" => {
                self.hovered = None;
                self.card_menu.take().is_some()
            }
            _ => false,
        }
    }

    fn open_card_menu(&mut self, table: &str) -> bool {
        let known = self.diagram.as_ref().is_some_and(|diagram| diagram.tables.iter().any(|item| item.name == table));
        if !known {
            return false;
        }
        self.card_menu = Some(table.to_owned());
        true
    }

    /// 页面内的键盘动作（从卡片、菜单按钮冒泡到根）：Esc 收起菜单与悬停，+/- 缩放，0 复位。
    fn pane_key(&mut self, key: &str) -> bool {
        match key {
            "escape" => {
                let menu = self.card_menu.take().is_some();
                let hover = self.hovered.take().is_some();
                menu || hover
            }
            "=" | "+" => {
                self.zoom = clamp_zoom(self.zoom + 0.1);
                true
            }
            "-" => {
                self.zoom = clamp_zoom(self.zoom - 0.1);
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

    fn clear_search(&mut self) {
        self.search.clear();
        self.highlight = None;
        self.center_pending = false;
        if let Some(field) = self.search_input.as_mut() {
            field.sync.clear();
        }
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
        if let Some(name) = &self.card_menu {
            lines.push(format!("menu {name}"));
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
        for (index, column) in columns.iter().enumerate() {
            for table in column {
                let mark = hover_mark(self.hovered.as_deref(), &table.name, diagram);
                lines.push(format!("column {index} {mark} {}", table.name));
                for column in &table.columns {
                    let fk = if foreign.contains(&format!("{}.{}", table.name, column.name)) { "fk" } else { "column" };
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
            let mark = if edge_hot(self.hovered.as_deref(), edge) { "hot-wire" } else if self.hovered.is_some() { "dim-wire" } else { "wire" };
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

fn layout_columns(tables: &[DiagramTable], edges: &[DiagramEdge]) -> Vec<Vec<DiagramTable>> {
    let known: Vec<&str> = tables.iter().map(|table| table.name.as_str()).collect();
    let mut columns: Vec<Vec<DiagramTable>> = Vec::new();
    for table in tables {
        let level = depth(&table.name, &known, edges, &mut Vec::new());
        while columns.len() <= level {
            columns.push(Vec::new());
        }
        columns[level].push(table.clone());
    }
    columns
}

fn depth(name: &str, known: &[&str], edges: &[DiagramEdge], path: &mut Vec<String>) -> usize {
    if path.iter().any(|item| item == name) {
        return 0;
    }
    path.push(name.to_owned());
    let mut deepest = 0usize;
    for edge in edges {
        if edge.from_table != name || edge.from_table == edge.to_table || !known.contains(&edge.to_table.as_str()) {
            continue;
        }
        if path.iter().any(|item| item == &edge.to_table) {
            continue;
        }
        deepest = deepest.max(1 + depth(&edge.to_table, known, edges, path));
    }
    path.pop();
    deepest
}

fn foreign_columns(diagram: &Diagram) -> Vec<String> {
    diagram.edges.iter().flat_map(|edge| edge.from_columns.iter().map(|column| format!("{}.{}", edge.from_table, column))).collect()
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

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{AppContext as _, Point, TestAppContext, WindowBounds, WindowOptions, px, size};

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
        pane.diagram = Some(Diagram {
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
        });
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
        let (services, database, master_before, rows_before) = rx.recv_timeout(std::time::Duration::from_secs(30)).expect("diagram setup");
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
        pane.diagram = Some(sample_diagram());
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
        assert_eq!(pane.card_menu.as_deref(), Some("orders"));
        assert!(pane.card_key("orders", "escape", false), "Esc 收起菜单");
        assert!(pane.card_menu.is_none());
        assert!(pane.card_key("orders", "f10", true), "Shift+F10 = 右键菜单");
        assert_eq!(pane.card_menu.as_deref(), Some("orders"));
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
        pane.card_menu = Some("orders".into());
        pane.hovered = Some("orders".into());
        assert!(pane.pane_key("escape"));
        assert!(pane.card_menu.is_none() && pane.hovered.is_none());
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
        pane.diagram = Some(empty);
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
                    pane.diagram = Some(sample_diagram());
                }
                "empty" => {
                    pane.connection_id = "notes".into();
                    pane.database = "shop".into();
                    pane.label = "笔记 / shop".into();
                    let mut diagram = sample_diagram();
                    diagram.tables.clear();
                    diagram.edges.clear();
                    pane.diagram = Some(diagram);
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
                window.click("diagram-search-clear", cx);
                window.render_frame(cx);
            })
            .unwrap();
            assert!(cx.update(|cx| pane.read(cx).search.is_empty()), "{tag}：清除搜索后恢复现场");

            // 纯键盘：Tab 停在卡片上按 Enter，走与双击相同的“打开表数据”请求。
            let mut opened = false;
            for _ in 0..40 {
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

const CARD_W: f32 = 220.;
const GAP_X: f32 = 90.;
const GAP_Y: f32 = 24.;
const HEAD_H: f32 = 34.;
const ROW_H: f32 = 22.;
const PAD: f32 = 32.;

fn card_height(table: &DiagramTable) -> f32 {
    HEAD_H + ROW_H * table.columns.len().max(1) as f32 + 10. + if table.outside.is_empty() { 0. } else { ROW_H }
}

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
                if matches!(event, InputEvent::Change) {
                    let text = input.read(cx).value().to_string();
                    if let Some(field) = this.search_input.as_mut() {
                        field.sync = text.clone();
                    }
                    this.focus_search(text);
                    cx.notify();
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

impl Render for DiagramPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::of(cx);
        let metrics = kit::Metrics::of(cx);
        let entity = cx.entity();
        let locale = self.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let search = self.search_field(window, cx);
        let summary = SharedString::from(self.lines().join("\n"));
        let diagram = self.diagram.clone();
        let hovered = self.hovered.clone();
        let highlight = self.highlight.clone();
        let menu_table = self.card_menu.clone();
        let zoom = self.zoom;
        let mono = cx.theme().mono_font_family.clone();
        let title = SharedString::from(if self.label.is_empty() { i18n::text(locale, "catalog.tree.diagram") } else { self.label.clone() });
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
        let zoom_reset = {
            let entity = entity.clone();
            kit::tool_button("diagram-zoom-reset", None, t("diagram.zoom.reset"), move |_, _, cx| {
                entity.update(cx, |pane, cx| pane.command("diagram-zoom-reset", cx))
            })
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
        let back = {
            let entity = entity.clone();
            kit::tool_button("diagram-back", Some(IconName::ArrowLeft), t("compare.back"), move |_, _, cx| {
                entity.update(cx, |pane, cx| pane.command("diagram-back", cx))
            })
        };

        // 只读说明放进标题的悬停提示，不常驻占一行。页头高度和左右边距走统一令牌。
        // 窄窗口把搜索框与缩放/返回换到第二行：页头不撑宽，页面本身不出现横向滚动。
        let viewport_w = window.viewport_size().width.as_f32();
        let pad_metrics = metrics.for_viewport(viewport_w);
        let narrow = viewport_w < kit::NARROW_PAGE_WIDTH;
        let layout_pad = pad_metrics.page_pad;
        let search_box = if narrow {
            div().flex_1().min_w(px(0.)).child(kit::text_input("diagram-search", &search, t("diagram.search")))
        } else {
            div().w(px(180.)).child(kit::text_input("diagram-search", &search, t("diagram.search")))
        };
        let header = if narrow {
            div()
                .id("diagram-header")
                .flex_none()
                .flex()
                .flex_col()
                .bg(p.surface)
                .border_b_1()
                .border_color(p.line)
                .child(div().id("diagram-title").tooltip_text(t("diagram.readonly")).child(kit::page_header(title, &[], &pad_metrics, &p)))
                .child(
                    div()
                        .id("diagram-tools")
                        .test_support()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .px(px(layout_pad))
                        .pb(px(8.))
                        .child(search_box)
                        .child(zoom_out)
                        .child(zoom_reset)
                        .child(zoom_in)
                        .child(zoom_fit)
                        .child(back),
                )
        } else {
            div().id("diagram-header").flex_none().flex().flex_col().child(
                div().id("diagram-title").tooltip_text(t("diagram.readonly")).child(
                    kit::page_header(title, &[], &pad_metrics, &p)
                        .child(search_box)
                        .child(zoom_out)
                        .child(zoom_reset)
                        .child(zoom_in)
                        .child(zoom_fit)
                        .child(back),
                ),
            )
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
                .gap(px(6.))
                .child(kit::kbd(kit::shortcut("Ctrl+B"), &p))
                .child(div().text_size(px(TypeScale::SM)).text_color(p.text_muted).child(t("shell.command.toggleSidebar"))),
            &p,
        )
        .into_any_element();
        let mut body = div().id("diagram-scroll").flex_1().min_h(px(0.)).overflow_hidden().bg(p.chrome);
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
            {
                // 搜索无命中只加一条提示条：画布保持可见，清空条件即恢复现场。
                if !self.search.trim().is_empty() && highlight.is_none() {
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
                            .gap(px(8.))
                            .px(px(layout_pad))
                            .py(px(6.))
                            .bg(p.surface)
                            .border_b_1()
                            .border_color(p.line)
                            .child(kit::icon(IconName::SearchX, 13., p.text_muted))
                            .child(div().text_size(px(TypeScale::SM)).text_color(p.text_muted).child(t("diagram.searchNoMatch")))
                            .child(clear),
                    );
                }
                let columns = layout_columns(&diagram.tables, &diagram.edges);
                let foreign = foreign_columns(diagram);
                // 卡片位置：按列从左到右，被引用的表在左。先算未缩放坐标，再平移和缩放。
                let mut places: std::collections::HashMap<String, (f32, f32, f32)> = std::collections::HashMap::new();
                let mut width = 0f32;
                let mut height = 0f32;
                for (index, column) in columns.iter().enumerate() {
                    let x = PAD + index as f32 * (CARD_W + GAP_X);
                    let mut y = PAD;
                    for table in column {
                        let h = card_height(table);
                        places.insert(table.name.clone(), (x, y, h));
                        y += h + GAP_Y;
                    }
                    width = width.max(x + CARD_W + PAD);
                    height = height.max(y + PAD);
                }
                self.content_w = width;
                self.content_h = height;
                if self.center_pending {
                    self.center_pending = false;
                    if let Some(name) = highlight.clone() {
                        if let Some(&(x, y, h)) = places.get(&name) {
                            let view = window.viewport_size();
                            self.pan_x = view.width.as_f32() * 0.45 - (x + CARD_W * 0.5) * zoom;
                            self.pan_y = view.height.as_f32() * 0.35 - (y + h * 0.5) * zoom;
                        }
                    }
                }
                let pan_x = self.pan_x;
                let pan_y = self.pan_y;
                for place in places.values_mut() {
                    place.0 = pan_x + place.0 * zoom;
                    place.1 = pan_y + place.1 * zoom;
                    place.2 *= zoom;
                }
                let board_w = (pan_x + width * zoom).max(400.);
                let board_h = (pan_y + height * zoom).max(300.);
                let card_w = CARD_W * zoom;
                let head_h = HEAD_H * zoom;
                let row_h = ROW_H * zoom;
                let board_entity = entity.clone();
                let mut canvas_box = div()
                    .id("diagram-board")
                    .relative()
                    .w(px(board_w))
                    .h(px(board_h))
                    .on_scroll({
                        let board_entity = board_entity.clone();
                        move |event: &ScrollEvent, _, cx| {
                            let steps = match event.delta {
                                ScrollDelta::Pixels(point) => point.y.as_f32() / 80.,
                                ScrollDelta::Lines(point) => point.y,
                            };
                            if steps.abs() < 0.01 {
                                return;
                            }
                            board_entity.update(cx, |pane, cx| {
                                pane.zoom = clamp_zoom(pane.zoom + steps * 0.1);
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
                    .on_mouse_move({
                        let board_entity = board_entity.clone();
                        move |event: &MouseMoveEvent, _, cx| {
                            board_entity.update(cx, |pane, cx| {
                                if pane.drag.is_some() {
                                    pane.move_drag(event.position.x.as_f32(), event.position.y.as_f32());
                                    cx.notify();
                                }
                            });
                        }
                    })
                    .on_mouse_up(MouseButton::Left, {
                        let board_entity = board_entity.clone();
                        move |_: &gpui_kit::MouseUpEvent, _, cx| {
                            board_entity.update(cx, |pane, cx| {
                                pane.end_drag();
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
                    });
                // 连线
                let mut wires: Vec<(gpui_kit::Point<gpui_kit::Pixels>, gpui_kit::Point<gpui_kit::Pixels>, bool)> = Vec::new();
                for edge in &diagram.edges {
                    let (Some(from), Some(to)) = (places.get(&edge.from_table), places.get(&edge.to_table)) else { continue };
                    let from_table = diagram.tables.iter().find(|table| table.name == edge.from_table);
                    let row = from_table.and_then(|table| edge.from_columns.first().and_then(|column| column_index(table, column))).unwrap_or(0);
                    let start_y = from.1 + (HEAD_H + ROW_H * row as f32 + ROW_H / 2. + 5.) * zoom;
                    let end_y = to.1 + head_h / 2.;
                    let (start_x, end_x) = if from.0 > to.0 { (from.0, to.0 + card_w) } else if from.0 < to.0 { (from.0 + card_w, to.0) } else { (from.0 + card_w, to.0 + card_w) };
                    wires.push((gpui_kit::point(px(start_x), px(start_y)), gpui_kit::point(px(end_x), px(end_y)), edge_hot(hovered.as_deref(), edge)));
                }
                let dim = hovered.is_some();
                let (wire, hot_wire, dim_wire) = (p.line_strong, p.accent, p.line);
                canvas_box = canvas_box.child(
                    canvas(
                        move |_, _, _| (),
                        move |bounds, _, window, _| {
                            for (start, end, hot) in &wires {
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
                            }
                        },
                    )
                    .absolute()
                    .inset_0(),
                );
                for column in &columns {
                    for table in column {
                        let Some((x, y, _)) = places.get(&table.name).copied() else { continue };
                        let name = table.name.clone();
                        let mark = hover_mark(hovered.as_deref(), &name, diagram);
                        let label = SharedString::from(format!("{mark} {}", table.name));
                        let hovered_name = name.clone();
                        let card_entity = entity.clone();
                        let (mut border, head_bg, opacity) = match mark {
                            "hot" => (p.accent, p.accent_soft, 1.),
                            "linked" => (p.accent, p.surface, 1.),
                            "dim" => (p.line, p.surface, 0.45),
                            _ => (p.line_strong, p.surface, 1.),
                        };
                        if highlight.as_deref() == Some(table.name.as_str()) {
                            border = p.accent;
                        }
                        let mut rows = div().flex().flex_col().py(px(5. * zoom));
                        for column in &table.columns {
                            let fk = foreign.contains(&format!("{}.{}", table.name, column.name));
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
                                    .px(px(12.))
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .text_size(px(TypeScale::SM))
                                    .child(kit::icon(icon, 11., icon_color))
                                    .child(div().truncate().font_family(mono.clone()).when(fk, |this| this.text_color(p.accent_text)).child(SharedString::from(column.name.clone())))
                                    .when(!column.type_name.is_empty(), |this| {
                                        this.child(
                                            div()
                                                .ml_auto()
                                                .truncate()
                                                .text_size(px(TypeScale::XS))
                                                .text_color(p.text_faint)
                                                .child(SharedString::from(column.type_name.clone())),
                                        )
                                    }),
                            );
                        }
                        if table.columns.is_empty() {
                            rows = rows.child(div().h(px(ROW_H)).px(px(12.)).text_size(px(TypeScale::SM)).text_color(p.text_faint).child("—"));
                        }
                        for note in &table.outside {
                            rows = rows.child(div().h(px(ROW_H)).px(px(12.)).flex().items_center().gap(px(6.)).text_size(px(TypeScale::XS)).text_color(p.warn).child(kit::icon(IconName::ExternalLink, 11., p.warn)).child(div().truncate().child(SharedString::from(note.clone()))));
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
                                .border_1()
                                .border_color(border)
                                .bg(p.surface)
                                .shadow_sm()
                                .overflow_hidden()
                                .child(
                                    div()
                                        .h(px(head_h))
                                        .px(px(12.))
                                        .flex()
                                        .items_center()
                                        .gap(px(8.))
                                        .bg(head_bg)
                                        .border_b_1()
                                        .border_color(p.line)
                                        .child(kit::icon(IconName::Table, 13., p.accent))
                                        .child(div().truncate().font_weight(FontWeight::SEMIBOLD).child(SharedString::from(table.name.clone()))),
                                )
                                .child(rows)
                                .on_mouse_down(MouseButton::Left, {
                                    let name = name.clone();
                                    let card_entity = card_entity.clone();
                                    move |event: &MouseDownEvent, _, cx| {
                                        // 双击次数在按下事件上。点下去就记下打开请求，不等点击合成，也不发 SQL。
                                        if event.click_count >= 2 {
                                            card_entity.update(cx, |pane, cx| {
                                                pane.request_open(&name, false);
                                                cx.notify();
                                            });
                                            cx.stop_propagation();
                                        }
                                    }
                                })
                                .on_mouse_up(MouseButton::Left, {
                                    let name = name.clone();
                                    let card_entity = card_entity.clone();
                                    move |event: &gpui_kit::MouseUpEvent, _, cx| {
                                        if event.click_count >= 2 {
                                            card_entity.update(cx, |pane, cx| {
                                                pane.request_open(&name, false);
                                                cx.notify();
                                            });
                                        }
                                    }
                                })
                                .on_click({
                                    let name = name.clone();
                                    let card_entity = card_entity.clone();
                                    move |event: &ClickEvent, _, cx| {
                                        let double = event.click_count() >= 2 && !event.is_secondary();
                                        let secondary = event.is_secondary();
                                        if !double && !secondary {
                                            return;
                                        }
                                        card_entity.update(cx, |pane, cx| {
                                            if double {
                                                pane.request_open(&name, false);
                                            } else {
                                                pane.card_menu = Some(name.clone());
                                            }
                                            cx.notify();
                                        });
                                    }
                                })
                                .on_hover(move |inside, _, cx| {
                                    let hovered_name = hovered_name.clone();
                                    card_entity.update(cx, |pane, cx| {
                                        if *inside {
                                            pane.hovered = Some(hovered_name);
                                        } else if pane.hovered.as_deref() == Some(hovered_name.as_str()) {
                                            pane.hovered = None;
                                        }
                                        cx.notify();
                                    });
                                })
                                .on_key_down({
                                    // 卡片是 Tab 停靠点：Enter / Shift+Enter 打开，菜单键或 Shift+F10 出菜单，Esc 收起。
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
                        canvas_box = canvas_box.child(kit::interactive(card, &p, false).opacity(opacity));
                    }
                }
                body = body.child(canvas_box);
                if let Some(name) = menu_table.clone() {
                    let open_data = {
                        let entity = entity.clone();
                        let table = name.clone();
                        kit::tool_button(SharedString::from(format!("diagram-open-data:{table}")), Some(IconName::Table), t("diagram.openData"), move |_, _, cx| {
                            let table = table.clone();
                            entity.update(cx, |pane, cx| pane.command(&format!("diagram-open-data:{table}"), cx));
                        })
                    };
                    let open_structure = {
                        let entity = entity.clone();
                        let table = name.clone();
                        kit::tool_button(SharedString::from(format!("diagram-open-structure:{table}")), Some(IconName::Columns3), t("diagram.openStructure"), move |_, _, cx| {
                            let table = table.clone();
                            entity.update(cx, |pane, cx| pane.command(&format!("diagram-open-structure:{table}"), cx));
                        })
                    };
                    let menu_close = {
                        let entity = entity.clone();
                        kit::icon_button("diagram-menu-close", IconName::X, t("common.close"), move |_, _, cx| {
                            entity.update(cx, |pane, cx| pane.command("diagram-menu-close", cx))
                        })
                    };
                    let mut bar = div()
                        .id("diagram-card-menu")
                        .test_support()
                        .flex_none()
                        .px(px(layout_pad))
                        .py(px(8.))
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(8.))
                        .bg(p.surface)
                        .border_t_1()
                        .border_color(p.line)
                        .child(div().text_size(px(TypeScale::SM)).child(SharedString::from(name)))
                        .child(open_data)
                        .child(open_structure)
                        .child(menu_close);
                    if !narrow {
                        // 菜单同时标出键盘等价键位：Enter 打开数据、Shift+Enter 看结构、Esc 收起。
                        bar = bar.child(
                            div()
                                .id("diagram-card-keys")
                                .ml_auto()
                                .flex()
                                .items_center()
                                .gap(px(4.))
                                .child(kit::kbd(kit::shortcut("Enter"), &p))
                                .child(div().text_size(px(TypeScale::XS)).text_color(p.text_muted).child(t("diagram.openData")))
                                .child(div().w(px(8.)))
                                .child(kit::kbd(kit::shortcut("Shift+Enter"), &p))
                                .child(div().text_size(px(TypeScale::XS)).text_color(p.text_muted).child(t("diagram.openStructure")))
                                .child(div().w(px(8.)))
                                .child(kit::kbd("Esc", &p))
                                .child(div().text_size(px(TypeScale::XS)).text_color(p.text_muted).child(t("common.close"))),
                        );
                    }
                    body = body.child(bar);
                }
            }
        } else {
            // 兜底与 NeedTarget 同一份呈现，不再出现第五种状态。
            body = body.child(div().size_full().flex().flex_col().items_start().justify_center().px(px(layout_pad)).child(need_target_state));
        }

        let mut footer = div().flex_none().flex().flex_col();
        if let Some(diagram) = diagram.as_ref() {
            if !diagram.omitted.is_empty() || diagram.notice.is_some() {
                let mut notes = div().px(px(metrics.page_pad)).py(px(8.)).flex().flex_col().gap(px(4.)).border_t_1().border_color(p.line).text_size(px(TypeScale::SM)).text_color(p.warn);
                if let Some(notice) = &diagram.notice {
                    notes = notes.child(SharedString::from(notice.clone()));
                }
                for item in &diagram.omitted {
                    notes = notes.child(SharedString::from(format!("{} · {} {}", i18n::text(locale, "diagram.omitted"), item.name, item.message)));
                }
                footer = footer.child(notes);
            }
            for edge in &diagram.edges {
                let mark = if edge_hot(hovered.as_deref(), edge) { "hot-wire" } else if hovered.is_some() { "dim-wire" } else { "wire" };
                let label = SharedString::from(format!("{mark} {}.{} -> {}.{}", edge.from_table, edge.from_columns.join(", "), edge.to_table, edge.to_columns.join(", ")));
                footer = footer.child(div().id(SharedString::from(format!("diagram-wire:{}:{}:{}", edge.name, edge.from_table, edge.to_table))).hidden().aria_label(label));
            }
        }

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
            .child(div().id("diagram-summary").hidden().child(summary))
    }
}
