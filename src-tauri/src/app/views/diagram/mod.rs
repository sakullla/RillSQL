//! 只读关系图。表卡片、外键连线、悬停、遗漏说明和文字清单都不写库。

use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    canvas, Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, PathBuilder, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    TestSupportExt as _, Window, div, px,
};

use crate::ui::kit::{self, Palette, Radius, Tip as _, TypeScale};

use crate::application::tasks::{TaskOutcome, TaskToken};
use crate::application::AppServices;
use crate::diagram::{Diagram, DiagramEdge, DiagramTable};
use crate::i18n::{self, Locale};

const OWNER: &str = "diagram";

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
        }
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
            self.clear_target();
            self.error = i18n::text(self.locale, "diagram.needTarget");
            self.applied = spec.to_owned();
            cx.notify();
            return;
        };
        self.applied = spec.to_owned();
        self.connection_id = target.0;
        self.database = target.1;
        self.schema = target.2;
        self.label = target.3;
        self.hovered = None;
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
        }
        cx.notify();
    }

    fn clear_target(&mut self) {
        self.connection_id.clear();
        self.database.clear();
        self.schema.clear();
        self.label.clear();
        self.diagram = None;
        self.loading = false;
        self.hovered = None;
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

    pub fn column_of(&self, name: &str) -> Option<usize> {
        let diagram = self.diagram.as_ref()?;
        layout_columns(&diagram.tables, &diagram.edges).into_iter().position(|column| column.iter().any(|table| table.name == name))
    }

    pub fn lines(&self) -> Vec<String> {
        let mut lines = vec![i18n::text(self.locale, "diagram.readonly")];
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
        for (index, column) in columns.iter().enumerate() {
            for table in column {
                let mark = hover_mark(self.hovered.as_deref(), &table.name, diagram);
                lines.push(format!("column {index} {mark} {}", table.name));
                for name in &table.columns {
                    let fk = if foreign.contains(&format!("{}.{}", table.name, name)) { "fk" } else { "column" };
                    lines.push(format!("{fk} {}.{}", table.name, name));
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
                "{mark} {}.{} -> {}.{}",
                edge.from_table,
                edge.from_columns.join(", "),
                edge.to_table,
                edge.to_columns.join(", ")
            ));
        }
        lines
    }
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
                assert!(text.contains("只读"), "{text}");
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
            window.hover("diagram-pad", cx);
            view.update(cx, |view, cx| {
                let text = view.diagram_text(cx);
                assert!(text.contains("card orders"), "{text}");
                assert!(!text.contains("hot orders"), "{text}");
                assert!(!text.contains("hot-wire"), "{text}");
                assert!(text.contains("wire") && text.contains("missing_customers"), "{text}");
            });
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

impl Render for DiagramPane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::of(cx);
        let entity = cx.entity();
        let locale = self.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let summary = SharedString::from(self.lines().join("\n"));
        let diagram = self.diagram.clone();
        let hovered = self.hovered.clone();
        let mono = cx.theme().mono_font_family.clone();

        let header = div()
            .flex_none()
            .px(px(20.))
            .py(px(12.))
            .flex()
            .items_center()
            .gap(px(12.))
            .border_b_1()
            .border_color(p.line)
            .child(div().size(px(30.)).rounded(px(Radius::PANEL)).bg(p.accent_soft).flex().items_center().justify_center().child(kit::icon(IconName::Workflow, 16., p.accent_text)))
            .child(
                // 只读说明放进标题的悬停提示，不常驻占一行。
                div()
                    .id("diagram-title")
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .tooltip_text(t("diagram.readonly"))
                    .child(kit::title(SharedString::from(if self.label.is_empty() { i18n::text(locale, "catalog.tree.diagram") } else { self.label.clone() }), TypeScale::LG, &p)),
            )
            .when_some(diagram.as_ref(), |this, diagram| {
                this.child(kit::pill(SharedString::from(format!("{} · {}", diagram.tables.len(), diagram.edges.len())), p.text_muted, p.hover))
            })
            .child(div().id("diagram-pad").test_support().w(px(8.)).h(px(28.)));

        let mut body = div().id("diagram-scroll").flex_1().min_h(px(0.)).overflow_scroll().bg(p.chrome);
        if self.loading {
            body = body.child(kit::loading_state(t("diagram.loading"), &p).size_full());
        } else if !self.error.is_empty() {
            let retry = {
                let entity = entity.clone();
                kit::secondary_button("diagram-retry", Some(IconName::RefreshCw), t("common.retry"), move |_, _, cx| {
                    entity.update(cx, |pane, cx| pane.command("diagram-retry", cx))
                })
            };
            body = body.child(
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(10.))
                    .child(kit::error_state(t("diagram.failed"), SharedString::from(self.error.clone()), &p))
                    .when(!self.connection_id.is_empty(), |this| this.child(retry)),
            );
        } else if let Some(diagram) = diagram.as_ref() {
            if diagram.tables.is_empty() {
                body = body.child(div().size_full().flex().items_center().justify_center().child(kit::empty_state(IconName::Workflow, t("diagram.empty"), SharedString::from(diagram.notice.clone().unwrap_or_default()), &p)));
            } else {
                let columns = layout_columns(&diagram.tables, &diagram.edges);
                let foreign = foreign_columns(diagram);
                // 卡片位置：按列从左到右，被引用的表在左。
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
                let mut canvas_box = div().relative().w(px(width.max(400.))).h(px(height.max(300.)));
                // 连线
                let mut wires: Vec<(gpui_kit::Point<gpui_kit::Pixels>, gpui_kit::Point<gpui_kit::Pixels>, bool)> = Vec::new();
                for edge in &diagram.edges {
                    let (Some(from), Some(to)) = (places.get(&edge.from_table), places.get(&edge.to_table)) else { continue };
                    let from_table = diagram.tables.iter().find(|table| table.name == edge.from_table);
                    let row = from_table
                        .and_then(|table| edge.from_columns.first().and_then(|column| table.columns.iter().position(|name| name == column)))
                        .unwrap_or(0);
                    let start_y = from.1 + HEAD_H + ROW_H * row as f32 + ROW_H / 2. + 5.;
                    let end_y = to.1 + HEAD_H / 2.;
                    let (start_x, end_x) = if from.0 > to.0 { (from.0, to.0 + CARD_W) } else if from.0 < to.0 { (from.0 + CARD_W, to.0) } else { (from.0 + CARD_W, to.0 + CARD_W) };
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
                        let (border, head_bg, opacity) = match mark {
                            "hot" => (p.accent, p.accent_soft, 1.),
                            "linked" => (p.accent, p.surface, 1.),
                            "dim" => (p.line, p.surface, 0.45),
                            _ => (p.line_strong, p.surface, 1.),
                        };
                        let mut rows = div().flex().flex_col().py(px(5.));
                        for column_name in &table.columns {
                            let fk = foreign.contains(&format!("{}.{}", table.name, column_name));
                            rows = rows.child(
                                div()
                                    .h(px(ROW_H))
                                    .px(px(12.))
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .text_size(px(TypeScale::SM))
                                    .child(kit::icon(if fk { IconName::Link } else { IconName::Dot }, 11., if fk { p.accent } else { p.text_faint }))
                                    .child(div().truncate().font_family(mono.clone()).when(fk, |this| this.text_color(p.accent_text)).child(SharedString::from(column_name.clone()))),
                            );
                        }
                        if table.columns.is_empty() {
                            rows = rows.child(div().h(px(ROW_H)).px(px(12.)).text_size(px(TypeScale::SM)).text_color(p.text_faint).child("—"));
                        }
                        for note in &table.outside {
                            rows = rows.child(div().h(px(ROW_H)).px(px(12.)).flex().items_center().gap(px(6.)).text_size(px(TypeScale::XS)).text_color(p.warn).child(kit::icon(IconName::ExternalLink, 11., p.warn)).child(div().truncate().child(SharedString::from(note.clone()))));
                        }
                        canvas_box = canvas_box.child(
                            div()
                                .id(SharedString::from(format!("diagram-card:{name}")))
                                .test_support()
                                .role(gpui_kit::Role::Button)
                                .aria_label(label)
                                .absolute()
                                .left(px(x))
                                .top(px(y))
                                .w(px(CARD_W))
                                .opacity(opacity)
                                .rounded(px(Radius::PANEL))
                                .border_1()
                                .border_color(border)
                                .bg(p.surface)
                                .shadow_sm()
                                .overflow_hidden()
                                .child(
                                    div()
                                        .h(px(HEAD_H))
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
                                }),
                        );
                    }
                }
                body = body.child(canvas_box);
            }
        } else {
            body = body.child(div().size_full().flex().items_center().justify_center().child(kit::empty_state(IconName::Workflow, t("catalog.tree.diagram"), t("diagram.needTarget"), &p)));
        }

        let mut footer = div().flex_none().flex().flex_col();
        if let Some(diagram) = diagram.as_ref() {
            if !diagram.omitted.is_empty() || diagram.notice.is_some() {
                let mut notes = div().px(px(20.)).py(px(8.)).flex().flex_col().gap(px(4.)).border_t_1().border_color(p.line).text_size(px(TypeScale::SM)).text_color(p.warn);
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
            .child(header)
            .child(body)
            .child(footer)
            .child(div().id("diagram-summary").hidden().child(summary))
    }
}
