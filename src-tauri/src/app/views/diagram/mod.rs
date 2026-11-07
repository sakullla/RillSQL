//! 只读关系图。表卡片、外键连线、悬停、遗漏说明和文字清单都不写库。

use std::sync::Arc;

use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _,
    Window, div, px,
};

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
                self.error = if message.is_empty() { i18n::text(self.locale, "diagram.failed") } else { message };
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

pub fn spec_from_object(spec: &str) -> Option<String> {
    let parts: Vec<&str> = spec.split('|').collect();
    if parts.len() < 7 || parts[0].is_empty() {
        return None;
    }
    let engine = parts[5];
    if engine == "redis" || engine == "mongodb" {
        return None;
    }
    let label = if parts[1].is_empty() { parts[0] } else { parts[1] };
    Some(format!("{}|{}|{}|{label}", parts[0], parts[2], parts[3]))
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

impl Render for DiagramPane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let summary = SharedString::from(self.lines().join("\n"));
        let diagram = self.diagram.clone();
        let hovered = self.hovered.clone();
        let mut column = div().id("diagram-pane").w(px(480.)).flex().flex_col().gap(px(4.));
        column = column.child(div().id("diagram-pad").test_support().w(px(220.)).h(px(28.)));
        if let Some(diagram) = diagram.as_ref() {
            for table in &diagram.tables {
                let name = table.name.clone();
                let mark = hover_mark(hovered.as_deref(), &name, diagram);
                let label = SharedString::from(format!("{mark} {}", table.name));
                let hovered_name = name.clone();
                let card_entity = entity.clone();
                column = column.child(
                    div()
                        .id(SharedString::from(format!("diagram-card:{name}")))
                        .test_support()
                        .w(px(220.))
                        .min_h(px(36.))
                        .role(gpui_kit::Role::Button)
                        .aria_label(label.clone())
                        .child(label)
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
            for edge in &diagram.edges {
                let mark = if edge_hot(hovered.as_deref(), edge) { "hot-wire" } else if hovered.is_some() { "dim-wire" } else { "wire" };
                let label = SharedString::from(format!(
                    "{mark} {}.{} -> {}.{}",
                    edge.from_table,
                    edge.from_columns.join(", "),
                    edge.to_table,
                    edge.to_columns.join(", ")
                ));
                column = column.child(
                    div()
                        .id(SharedString::from(format!("diagram-wire:{}:{}:{}", edge.name, edge.from_table, edge.to_table)))
                        .aria_label(label.clone())
                        .child(label),
                );
            }
        }
        column.child(div().id("diagram-summary").child(summary))
    }
}
