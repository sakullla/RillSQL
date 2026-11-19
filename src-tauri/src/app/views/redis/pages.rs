//! 顶栏（连接/逻辑库或集群 + 页签）与诊断页：慢日志、信息、大键、客户端、Pub/Sub、节点。

use super::view::{action, cmd_click, icon_btn, Kind};
use super::*;
use gpui_kit::Hsla;

/// 诊断表的一列：标题与宽度（None 占满剩余）。
struct Col {
    label: String,
    width: Option<f32>,
    sort: Option<&'static str>,
}

fn col(label: String, width: Option<f32>) -> Col {
    Col { label, width, sort: None }
}

fn cell(width: Option<f32>) -> gpui_kit::Div {
    match width {
        Some(w) => div().flex_none().w(px(w)).min_w(px(0.)).truncate(),
        None => div().flex_1().min_w(px(0.)).truncate(),
    }
}

impl RedisPane {
    // ------------------------------------------------------------------ 顶栏

    pub(super) fn render_top_bar(&self, p: &Palette, m: &kit::Metrics, entity: &Entity<Self>) -> AnyElement {
        let identity = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(kit::engine_badge(crate::adapters::workbench_badge_id(crate::adapters::Workbench::Redis), 22.))
            .child(
                div()
                    .max_w(px(220.))
                    .truncate()
                    .text_size(px(TypeScale::MD))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.text)
                    .child(SharedString::from(self.connection_name.clone())),
            )
            .when(self.prod_target, |this| this.child(kit::pill(SharedString::from(self.text("redis.prod.badge")), p.danger, p.danger_soft)));
        let scope = if self.topology.cluster {
            let masters = self.topology.masters().len();
            div()
                .id("redis-cluster-chip")
                .test_support()
                .h(px(24.))
                .px(px(8.))
                .flex()
                .items_center()
                .gap(px(5.))
                .rounded(px(Radius::CONTROL))
                .bg(p.accent_soft)
                .text_color(p.accent_text)
                .text_size(px(TypeScale::XS))
                .font_weight(FontWeight::MEDIUM)
                .child(kit::icon(IconName::Network, 12., p.accent_text))
                .child(SharedString::from(i18n::fill(&self.text("redis.cluster.chip"), &[("n", &masters.to_string())])))
                .into_any_element()
        } else {
            self.render_db_button(p, entity)
        };
        let mut tabs = div().flex_1().min_w(px(0.)).flex().items_center().gap(px(2.)).overflow_hidden();
        for page in Page::ALL {
            if page == Page::Nodes && !self.topology.cluster {
                continue;
            }
            let id = format!("redis-page:{}", page.id());
            let active = self.page == page;
            tabs = tabs.child(
                kit::interactive_states(div().id(SharedString::from(id.clone())), p, false)
                    .test_support()
                    .role(gpui_kit::Role::Tab)
                    .aria_label(SharedString::from(self.text(page.label_key())))
                    .aria_selected(active)
                    .flex_none()
                    .h(px(m.bar_h() - 4.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .text_size(px(TypeScale::SM))
                    .border_b_2()
                    .when(active, |this| this.border_color(p.accent).text_color(p.text).font_weight(FontWeight::MEDIUM))
                    .when(!active, |this| this.border_color(gpui_kit::transparent_black()).text_color(p.text_muted).hover(|s| s.text_color(p.text)))
                    .child(SharedString::from(self.text(page.label_key())))
                    .on_click(cmd_click(entity, id)),
            );
        }
        div()
            .id("redis-top-bar")
            .test_support()
            .flex_none()
            .h(px(m.bar_h() + 6.))
            .px(px(m.pane_pad()))
            .flex()
            .items_center()
            .gap(px(12.))
            .border_b_1()
            .border_color(p.line)
            .bg(p.chrome)
            .child(identity)
            .child(scope)
            .when(self.topology.fallback.is_some(), |this| {
                this.child(div().id("redis-cluster-fallback").test_support().child(kit::pill(SharedString::from(self.text("redis.cluster.fallbackChip")), p.warn, p.warn_soft)))
            })
            .child(tabs)
            .into_any_element()
    }

    // ------------------------------------------------------------------ 诊断页

    pub(super) fn render_diag_page(&mut self, p: &Palette, m: &kit::Metrics, entity: &Entity<Self>, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let mono = cx.theme().mono_font_family.clone();
        let body = match self.page {
            Page::Slowlog => self.render_slowlog(p, m, entity, &mono),
            Page::Info => self.render_info(p, m, entity, &mono),
            Page::BigKeys => self.render_big_keys(p, m, entity, &mono),
            Page::Clients => self.render_clients(p, m, entity, &mono),
            Page::PubSub => self.render_pubsub(p, m, entity, &mono, window, cx),
            Page::Nodes => self.render_nodes(p, m, entity, &mono),
            Page::Keys => div().into_any_element(),
        };
        let error = (!self.diag.error.is_empty()).then(|| {
            div().px(px(m.pane_pad())).pt(px(8.)).child(kit::banner(kit::BannerTone::Danger, IconName::CircleAlert, self.diag.error.clone(), None, p))
        });
        div()
            .id(SharedString::from(format!("redis-diag-{}", self.page.id())))
            .test_support()
            .flex_1()
            .min_h(px(0.))
            .min_w(px(0.))
            .flex()
            .flex_col()
            .bg(p.surface)
            .children(error)
            .child(body)
            .into_any_element()
    }

    /// 页内工具条：标题、集群节点选择、右侧操作。
    fn diag_toolbar(&self, p: &Palette, m: &kit::Metrics, entity: &Entity<Self>, title_key: &str, node_all: bool, actions: Vec<AnyElement>) -> gpui_kit::Div {
        let mut nodes = div().flex().items_center().gap(px(4.)).flex_wrap();
        if self.topology.cluster {
            if node_all {
                nodes = nodes.child(kit::filter_chip(
                    "redis-diag-node:*",
                    self.text("redis.diag.allNodes"),
                    None,
                    None,
                    None,
                    self.diag.node.is_empty(),
                    p,
                    cmd_click(entity, "redis-diag-node:*"),
                ));
            }
            for (index, node) in self.topology.masters().into_iter().enumerate() {
                let addr = node.addr();
                let id = format!("redis-diag-node:{addr}");
                let selected = self.diag.node == addr || (!node_all && self.diag.node.is_empty() && index == 0);
                nodes = nodes.child(kit::filter_chip(SharedString::from(id.clone()), addr, None, None, None, selected, p, cmd_click(entity, id)));
            }
        }
        let mut bar = div()
            .flex_none()
            .px(px(m.pane_pad()))
            .py(px(8.))
            .flex()
            .items_center()
            .gap(px(10.))
            .border_b_1()
            .border_color(p.line_soft)
            .child(kit::title(SharedString::from(self.text(title_key)), TypeScale::MD, p))
            .child(div().flex_1().min_w(px(0.)).child(nodes))
            .when(self.diag.busy, |this| this.child(kit::inline_loading(SharedString::from(self.text("redis.diag.loading")), m, p)));
        for item in actions {
            bar = bar.child(item);
        }
        bar
    }

    fn table_header(&self, cols: &[Col], p: &Palette, m: &kit::Metrics, entity: &Entity<Self>) -> AnyElement {
        let mut header = div()
            .flex_none()
            .h(px(m.row_h - 2.))
            .px(px(m.pane_pad()))
            .flex()
            .items_center()
            .gap(px(12.))
            .bg(p.chrome)
            .border_b_1()
            .border_color(p.line)
            .text_size(px(TypeScale::XS))
            .font_weight(FontWeight::MEDIUM)
            .text_color(p.text_muted);
        for item in cols {
            let label = cell(item.width).child(SharedString::from(item.label.clone()));
            header = match item.sort {
                Some(sort) => {
                    let id = format!("redis-slow-sort:{sort}");
                    header.child(
                        div()
                            .id(SharedString::from(id.clone()))
                            .test_support()
                            .role(gpui_kit::Role::Button)
                            .aria_label(SharedString::from(item.label.clone()))
                            .cursor_pointer()
                            .hover(|s| s.text_color(p.text))
                            .when_some(item.width, |this, w| this.flex_none().w(px(w)))
                            .when(item.width.is_none(), |this| this.flex_1().min_w(px(0.)))
                            .child(label)
                            .on_click(cmd_click(entity, id)),
                    )
                }
                None => header.child(label),
            };
        }
        header.into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn table_row(
        &self,
        id: String,
        cells: Vec<(Option<f32>, String, Hsla)>,
        p: &Palette,
        m: &kit::Metrics,
        mono: &SharedString,
        tail: Option<AnyElement>,
        click: Option<(&Entity<Self>, String)>,
    ) -> AnyElement {
        let mut row = div()
            .id(SharedString::from(id))
            .test_support()
            .role(gpui_kit::Role::Row)
            .flex_none()
            .h(px(m.row_h))
            .px(px(m.pane_pad()))
            .flex()
            .items_center()
            .gap(px(12.))
            .border_b_1()
            .border_color(p.line_soft)
            .hover(|s| s.bg(p.hover))
            .font_family(mono.clone())
            .text_size(px(TypeScale::SM));
        for (width, text, color) in cells {
            row = row.child(cell(width).text_color(color).child(SharedString::from(text)));
        }
        if let Some((entity, command)) = click {
            row = row.cursor_pointer().on_click(cmd_click(entity, command));
        }
        row.children(tail).into_any_element()
    }

    fn render_slowlog(&self, p: &Palette, m: &kit::Metrics, entity: &Entity<Self>, mono: &SharedString) -> AnyElement {
        let reset = action("redis-slow-reset", Some(IconName::Trash), &self.text("redis.slowlog.reset"), Kind::DangerSoft, entity).small().into_any_element();
        let refresh = icon_btn("redis-diag-refresh", IconName::RefreshCw, &self.text("redis.tab.refresh"), entity).into_any_element();
        let toolbar = self.diag_toolbar(p, m, entity, "redis.page.slowlog", true, vec![refresh, reset]);
        let arrow = |by_time: bool| {
            if self.diag.slow_by_time == by_time {
                if self.diag.slow_asc { " ↑" } else { " ↓" }
            } else {
                ""
            }
        };
        let cols = vec![
            col("#".into(), Some(56.)),
            Col { label: format!("{}{}", self.text("redis.slowlog.time"), arrow(true)), width: Some(150.), sort: Some("time") },
            Col { label: format!("{}{}", self.text("redis.slowlog.duration"), arrow(false)), width: Some(110.), sort: Some("duration") },
            col(self.text("redis.slowlog.command"), None),
            col(self.text("redis.slowlog.client"), Some(150.)),
        ];
        let mut body = div().id("redis-slowlog-rows").test_support().flex_1().min_h(px(0.)).overflow_y_scroll().flex().flex_col();
        if self.diag.slowlog.is_empty() && self.diag.slow_loaded {
            body = body.child(kit::empty_state(IconName::Timer, SharedString::from(self.text("redis.slowlog.empty")), SharedString::from(""), p));
        }
        for (index, entry) in self.diag.slowlog.iter().enumerate() {
            let at = chrono::DateTime::from_timestamp(entry.at, 0).map(|time| time.with_timezone(&chrono::Local).format("%m-%d %H:%M:%S").to_string()).unwrap_or_default();
            let ms = format!("{:.2} ms", entry.micros as f64 / 1000.);
            let color = if entry.micros >= 100_000 { p.danger } else if entry.micros >= 10_000 { p.warn } else { p.text };
            body = body.child(self.table_row(
                format!("redis-slow-row:{index}"),
                vec![
                    (Some(56.), entry.id.to_string(), p.text_faint),
                    (Some(150.), at, p.text_muted),
                    (Some(110.), ms, color),
                    (None, entry.command.clone(), p.text),
                    (Some(150.), if self.topology.cluster { format!("{} · {}", entry.client, entry.node) } else { entry.client.clone() }, p.text_muted),
                ],
                p,
                m,
                mono,
                None,
                None,
            ));
        }
        div().flex_1().min_h(px(0.)).flex().flex_col().child(toolbar).child(self.table_header(&cols, p, m, entity)).child(body).into_any_element()
    }

    fn render_info(&self, p: &Palette, m: &kit::Metrics, entity: &Entity<Self>, mono: &SharedString) -> AnyElement {
        let refresh = icon_btn("redis-diag-refresh", IconName::RefreshCw, &self.text("redis.tab.refresh"), entity).into_any_element();
        let toolbar = self.diag_toolbar(p, m, entity, "redis.page.info", false, vec![refresh]);
        let metrics = crate::adapters::redis::diag::key_metrics(&self.diag.info);
        let mut cards = div().flex().flex_wrap().gap(px(8.)).px(px(m.pane_pad())).py(px(10.));
        for (label, value) in metrics {
            cards = cards.child(
                kit::card(p)
                    .w(px(150.))
                    .px(px(10.))
                    .py(px(8.))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(kit::eyebrow(SharedString::from(self.text(label)), p))
                    .child(kit::metric_value(SharedString::from(value), p)),
            );
        }
        let mut sections = div().flex().flex_col().gap(px(10.)).px(px(m.pane_pad())).pb(px(16.));
        for section in &self.diag.info {
            let mut table = kit::card(p).flex().flex_col().overflow_hidden();
            for (key, value) in &section.items {
                table = table.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .px(px(10.))
                        .min_h(px(24.))
                        .border_b_1()
                        .border_color(p.line_soft)
                        .font_family(mono.clone())
                        .text_size(px(TypeScale::XS))
                        .child(div().flex_none().w(px(260.)).truncate().text_color(p.text_muted).child(SharedString::from(key.clone())))
                        .child(div().flex_1().min_w(px(0.)).truncate().text_color(p.text).child(SharedString::from(value.clone()))),
                );
            }
            sections = sections.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(kit::eyebrow(SharedString::from(if section.name.is_empty() { "—".to_owned() } else { section.name.clone() }), p))
                    .child(table),
            );
        }
        div()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .flex_col()
            .child(toolbar)
            .child(div().id("redis-info-scroll").test_support().flex_1().min_h(px(0.)).overflow_y_scroll().child(cards).child(sections))
            .into_any_element()
    }

    fn render_big_keys(&self, p: &Palette, m: &kit::Metrics, entity: &Entity<Self>, mono: &SharedString) -> AnyElement {
        let mut actions = vec![
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(kit::switch_visual(self.diag.big_memory, p))
                .child(div().text_size(px(TypeScale::XS)).text_color(p.text_muted).child(SharedString::from(self.text("redis.bigkeys.memory"))))
                .id("redis-big-memory")
                .test_support()
                .role(gpui_kit::Role::Switch)
                .aria_label(SharedString::from(self.text("redis.bigkeys.memory")))
                .cursor_pointer()
                .on_click(cmd_click(entity, "redis-big-memory"))
                .into_any_element(),
        ];
        if self.diag.big_running {
            actions.push(action("redis-big-cancel", Some(IconName::CircleStop), &self.text("common.cancel"), Kind::Secondary, entity).small().into_any_element());
        } else {
            actions.push(action("redis-big-start", Some(IconName::Play), &self.text("redis.bigkeys.start"), Kind::Primary, entity).small().into_any_element());
        }
        let toolbar = self.diag_toolbar(p, m, entity, "redis.page.bigkeys", false, actions);
        let progress = (self.diag.big_running || self.diag.big_scanned > 0).then(|| {
            let text = match self.diag.big_total {
                Some(total) => i18n::fill(&self.text("redis.scan.progress"), &[("n", &self.diag.big_scanned.to_string()), ("total", &total.to_string())]),
                None => i18n::fill(&self.text("redis.scan.read"), &[("n", &self.diag.big_scanned.to_string())]),
            };
            let ratio = self.diag.big_total.filter(|total| *total > 0).map(|total| (self.diag.big_scanned as f32 / total as f32).min(1.)).unwrap_or(0.);
            div().px(px(m.pane_pad())).py(px(6.)).child(progress_bar(&text, ratio, self.diag.big_running, p))
        });
        let cols = vec![
            col("#".into(), Some(40.)),
            col(self.text("redis.key.name"), None),
            col(self.text("redis.new.type"), Some(60.)),
            col(self.text("redis.bigkeys.size"), Some(110.)),
            col(self.text("redis.bigkeys.memoryCol"), Some(110.)),
        ];
        let mut body = div().id("redis-big-rows").test_support().flex_1().min_h(px(0.)).overflow_y_scroll().flex().flex_col();
        if self.diag.big.is_empty() && !self.diag.big_running {
            body = body.child(kit::empty_state(IconName::ChartBar, SharedString::from(self.text("redis.bigkeys.empty")), SharedString::from(self.text("redis.bigkeys.hint")), p));
        }
        for (index, item) in self.diag.big.iter().enumerate() {
            let memory = item.memory.map(keyspace::human_bytes).unwrap_or_else(|| "—".to_owned());
            body = body.child(
                self.table_row(
                    format!("redis-big-open:{index}"),
                    vec![
                        (Some(40.), (index + 1).to_string(), p.text_faint),
                        (None, item.key.clone(), p.text),
                        (Some(60.), item.key_type.clone(), super::view::type_colors(&item.key_type, p).0),
                        (Some(110.), item.size.to_string(), p.text),
                        (Some(110.), memory, p.text_muted),
                    ],
                    p,
                    m,
                    mono,
                    None,
                    Some((entity, format!("redis-big-open:{index}"))),
                ),
            );
        }
        div().flex_1().min_h(px(0.)).flex().flex_col().child(toolbar).children(progress).child(self.table_header(&cols, p, m, entity)).child(body).into_any_element()
    }

    fn render_clients(&self, p: &Palette, m: &kit::Metrics, entity: &Entity<Self>, mono: &SharedString) -> AnyElement {
        let refresh = icon_btn("redis-diag-refresh", IconName::RefreshCw, &self.text("redis.tab.refresh"), entity).into_any_element();
        let toolbar = self.diag_toolbar(p, m, entity, "redis.page.clients", true, vec![refresh]);
        let cols = vec![
            col("id".into(), Some(64.)),
            col(self.text("redis.clients.addr"), Some(170.)),
            col(self.text("redis.clients.name"), Some(120.)),
            col(self.text("redis.clients.age"), Some(70.)),
            col(self.text("redis.clients.idle"), Some(70.)),
            col("db".into(), Some(40.)),
            col(self.text("redis.clients.cmd"), None),
            col(String::new(), Some(28.)),
        ];
        let mut body = div().id("redis-client-rows").test_support().flex_1().min_h(px(0.)).overflow_y_scroll().flex().flex_col();
        for (index, client) in self.diag.clients.iter().enumerate() {
            let kill = icon_btn(format!("redis-client-kill:{index}"), IconName::Unplug, &self.text("redis.clients.kill"), entity).xsmall().into_any_element();
            body = body.child(self.table_row(
                format!("redis-client-row:{index}"),
                vec![
                    (Some(64.), client.id.clone(), p.text_faint),
                    (Some(170.), client.addr.clone(), p.text),
                    (Some(120.), client.name.clone(), p.text_muted),
                    (Some(70.), format!("{}s", client.age), p.text_muted),
                    (Some(70.), format!("{}s", client.idle), p.text_muted),
                    (Some(40.), client.db.clone(), p.text_muted),
                    (None, if self.topology.cluster { format!("{} · {}", client.cmd, client.node) } else { client.cmd.clone() }, p.text),
                ],
                p,
                m,
                mono,
                Some(div().flex_none().w(px(28.)).child(kill).into_any_element()),
                None,
            ));
        }
        div().flex_1().min_h(px(0.)).flex().flex_col().child(toolbar).child(self.table_header(&cols, p, m, entity)).child(body).into_any_element()
    }

    fn render_pubsub(&mut self, p: &Palette, m: &kit::Metrics, entity: &Entity<Self>, mono: &SharedString, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let channels = self.redis_input(Field::SubChannels, "redis-sub-channels", "redis.pubsub.channels", "news.* alerts", window, cx);
        let pub_channel = self.redis_input(Field::PubChannel, "redis-pub-channel", "redis.pubsub.channel", "alerts", window, cx);
        let pub_message = self.redis_input(Field::PubMessage, "redis-pub-message", "redis.pubsub.message", "hello", window, cx);
        let toolbar = self.diag_toolbar(p, m, entity, "redis.page.pubsub", false, Vec::new());
        let subscribe = div()
            .px(px(m.pane_pad()))
            .py(px(8.))
            .flex()
            .items_center()
            .gap(px(8.))
            .child(div().flex_1().min_w(px(160.)).child(kit::text_input("redis-sub-channels", &channels, self.text("redis.pubsub.channels")).small()))
            .child(if self.diag.sub_running {
                action("redis-sub-stop", Some(IconName::CircleStop), &self.text("redis.pubsub.stop"), Kind::Secondary, entity).small()
            } else {
                action("redis-sub-start", Some(IconName::Radio), &self.text("redis.pubsub.subscribe"), Kind::Primary, entity).small()
            })
            .child(icon_btn("redis-sub-clear", IconName::Trash, &self.text("redis.console.clear"), entity));
        let status = div().px(px(m.pane_pad())).pb(px(6.)).text_size(px(TypeScale::XS)).text_color(p.text_muted).child(SharedString::from(
            if self.diag.sub_running && self.diag.sub_ready {
                i18n::fill(&self.text("redis.pubsub.listening"), &[("n", &self.diag.messages.len().to_string())])
            } else if self.diag.sub_running {
                self.text("redis.pubsub.connecting")
            } else {
                self.text("redis.pubsub.idle")
            },
        ));
        let mut list = div().id("redis-pubsub-messages").test_support().flex_1().min_h(px(0.)).overflow_y_scroll().flex().flex_col().mx(px(m.pane_pad())).rounded(px(Radius::PANEL)).border_1().border_color(p.line);
        for (index, message) in self.diag.messages.iter().enumerate() {
            list = list.child(
                div()
                    .id(SharedString::from(format!("redis-pubsub-msg:{index}")))
                    .flex_none()
                    .px(px(10.))
                    .py(px(4.))
                    .flex()
                    .gap(px(10.))
                    .border_b_1()
                    .border_color(p.line_soft)
                    .font_family(mono.clone())
                    .text_size(px(TypeScale::XS))
                    .child(div().flex_none().w(px(180.)).truncate().text_color(p.accent_text).child(SharedString::from(message.channel.clone())))
                    .child(div().flex_1().min_w(px(0.)).text_color(p.text).child(SharedString::from(one_line(&message.payload)))),
            );
        }
        let publish = div()
            .px(px(m.pane_pad()))
            .py(px(8.))
            .flex()
            .items_center()
            .gap(px(8.))
            .border_t_1()
            .border_color(p.line_soft)
            .child(div().w(px(180.)).child(kit::text_input("redis-pub-channel", &pub_channel, self.text("redis.pubsub.channel")).small()))
            .child(div().flex_1().min_w(px(160.)).child(kit::text_input("redis-pub-message", &pub_message, self.text("redis.pubsub.message")).small()))
            .child(action("redis-pub-send", Some(IconName::Send), &self.text("redis.pubsub.publish"), Kind::Primary, entity).small());
        div().flex_1().min_h(px(0.)).flex().flex_col().child(toolbar).child(subscribe).child(status).child(list).child(publish).into_any_element()
    }

    fn render_nodes(&self, p: &Palette, m: &kit::Metrics, entity: &Entity<Self>, mono: &SharedString) -> AnyElement {
        let refresh = icon_btn("redis-diag-refresh", IconName::RefreshCw, &self.text("redis.tab.refresh"), entity).into_any_element();
        let toolbar = self.diag_toolbar(p, m, entity, "redis.page.nodes", false, vec![refresh]);
        let cols = vec![
            col(self.text("redis.nodes.addr"), Some(170.)),
            col(self.text("redis.nodes.role"), Some(80.)),
            col(self.text("redis.nodes.status"), Some(100.)),
            col(self.text("redis.nodes.slots"), None),
            col(self.text("redis.nodes.slotCount"), Some(80.)),
            col(self.text("redis.nodes.keys"), Some(90.)),
        ];
        let mut body = div().id("redis-node-rows").test_support().flex_1().min_h(px(0.)).overflow_y_scroll().flex().flex_col();
        for (index, node) in self.topology.nodes.iter().enumerate() {
            let status = node.status();
            let color = match status {
                "ok" => p.ok,
                "pfail" => p.warn,
                _ => p.danger,
            };
            let keys = self.diag.node_sizes.iter().find(|(addr, _)| *addr == node.addr()).map(|(_, n)| n.to_string()).unwrap_or_else(|| "—".to_owned());
            let role = if node.master { self.text("redis.nodes.master") } else { self.text("redis.nodes.replica") };
            body = body.child(self.table_row(
                format!("redis-node-row:{index}"),
                vec![
                    (Some(170.), node.addr(), p.text),
                    (Some(80.), role, if node.master { p.accent_text } else { p.text_muted }),
                    (Some(100.), status.to_owned(), color),
                    (None, node.slot_text(), p.text_muted),
                    (Some(80.), node.slot_total().to_string(), p.text),
                    (Some(90.), keys, p.text),
                ],
                p,
                m,
                mono,
                None,
                None,
            ));
        }
        div().flex_1().min_h(px(0.)).flex().flex_col().child(toolbar).child(self.table_header(&cols, p, m, entity)).child(body).into_any_element()
    }
}

/// 进度条：说明文字 + 细条（未知总数时只显示文字）。
pub(super) fn progress_bar(text: &str, ratio: f32, running: bool, p: &Palette) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .text_size(px(TypeScale::XS))
                .font_features(kit::tabular_figures())
                .text_color(p.text_muted)
                .child(kit::status_dot(if running { p.warn } else { p.ok }))
                .child(div().min_w(px(0.)).truncate().child(SharedString::from(text.to_owned()))),
        )
        .child(
            div()
                .h(px(3.))
                .w_full()
                .rounded_full()
                .bg(p.sunken)
                .child(div().h_full().rounded_full().bg(p.accent).w(gpui_kit::relative(ratio.clamp(0., 1.)))),
        )
        .into_any_element()
}
