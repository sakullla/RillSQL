//! 顶栏（连接/逻辑库或集群 + 页签）与诊断页：慢日志、信息、大键、客户端、Pub/Sub、节点。

use super::view::{action, cmd_click, icon_btn, Kind};
use crate::ui::kit::{ControlSize, IconSize, ScrollAreaExt as _, Space};
use crate::ui::read_table::{ReadColumn, ReadTable};
use super::*;

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
            .child(self.render_status(p))
            .into_any_element()
    }

    // ------------------------------------------------------------------ 诊断页

    pub(super) fn render_diag_page(&mut self, p: &Palette, m: &kit::Metrics, entity: &Entity<Self>, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let mono = cx.theme().mono_font_family.clone();
        let body = match self.page {
            Page::Slowlog => self.render_slowlog(p, m, entity, window, cx),
            Page::Info => self.render_info(p, m, entity, window, cx),
            Page::BigKeys => self.render_big_keys(p, m, entity, window, cx),
            Page::Clients => self.render_clients(p, m, entity, window, cx),
            Page::PubSub => self.render_pubsub(p, m, entity, &mono, window, cx),
            Page::Nodes => self.render_nodes(p, m, entity, window, cx),
            Page::Console => self.render_console(p, m, entity, window, cx),
            Page::Keys => div().into_any_element(),
        };
        let error = (!self.diag.error.is_empty()).then(|| {
            div().px(px(m.pane_pad())).pt(px(Space::MD)).child(kit::banner(kit::BannerTone::Danger, IconName::CircleAlert, self.diag.error.clone(), None, p))
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

    /// 页内工具条：左边是这一页的用法提示（页签已经写了页名，不再重复标题）和集群节点选择，右边是操作。
    pub(super) fn diag_toolbar(&self, p: &Palette, m: &kit::Metrics, entity: &Entity<Self>, hint: Option<String>, node_all: bool, actions: Vec<AnyElement>) -> gpui_kit::Div {
        let mut nodes = div().flex().items_center().gap(px(Space::XS)).flex_wrap();
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
            .min_h(px(m.bar_h() + Space::SM))
            .px(px(m.pane_pad()))
            .py(px(Space::XS))
            .flex()
            .items_center()
            .gap(px(Space::MD))
            .border_b_1()
            .border_color(p.line_soft)
            .when_some(hint, |this, hint| this.child(div().flex_none().max_w(px(420.)).truncate().text_size(px(TypeScale::XS)).text_color(p.text_muted).child(SharedString::from(hint))))
            .child(div().flex_1().min_w(px(0.)).child(nodes))
            .when(self.diag.busy, |this| this.child(kit::inline_loading(SharedString::from(self.text("redis.diag.loading")), m, p)));
        for item in actions {
            bar = bar.child(item);
        }
        bar
    }

    /// 一张诊断表占满剩余高度。
    fn table_area(id: &'static str, table: Entity<ReadTable>) -> AnyElement {
        div().id(id).test_support().flex_1().min_h(px(0.)).child(table).into_any_element()
    }

    fn render_slowlog(&mut self, p: &Palette, m: &kit::Metrics, entity: &Entity<Self>, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let arrow = |by_time: bool| {
            if self.diag.slow_by_time == by_time {
                if self.diag.slow_asc { " ↑" } else { " ↓" }
            } else {
                ""
            }
        };
        let sort_time = action("redis-slow-sort:time", Some(IconName::Clock), &format!("{}{}", self.text("redis.slowlog.time"), arrow(true)), Kind::Tool, entity).small().into_any_element();
        let sort_duration =
            action("redis-slow-sort:duration", Some(IconName::Timer), &format!("{}{}", self.text("redis.slowlog.duration"), arrow(false)), Kind::Tool, entity).small().into_any_element();
        let reset = action("redis-slow-reset", Some(IconName::Trash), &self.text("redis.slowlog.reset"), Kind::DangerSoft, entity).small().into_any_element();
        let refresh = icon_btn("redis-diag-refresh", IconName::RefreshCw, &self.text("redis.tab.refresh"), entity).into_any_element();
        let toolbar = self.diag_toolbar(p, m, entity, Some(self.text("redis.slowlog.hint")), true, vec![sort_time, sort_duration, refresh, reset]);
        let cluster = self.topology.cluster;
        let stamp = {
            let rows: Vec<(i64, i64, i64, &str, &str, &str)> =
                self.diag.slowlog.iter().map(|e| (e.id, e.at, e.micros, e.command.as_str(), e.client.as_str(), e.node.as_str())).collect();
            hash_of(&(rows, cluster, self.diag.slow_loaded))
        };
        let empty = SharedString::from(if self.diag.slow_loaded { self.text("redis.slowlog.empty") } else { self.text("redis.diag.loading") });
        let table = self.table_slot(
            "slowlog",
            stamp,
            move |this| {
                let columns = vec![
                    ReadColumn::new("#").width(56.).right(),
                    ReadColumn::new(this.text("redis.slowlog.time")).width(140.),
                    ReadColumn::new(this.text("redis.slowlog.duration")).width(100.).right(),
                    ReadColumn::new(this.text("redis.slowlog.command")).width(360.).mono(),
                    ReadColumn::new(this.text("redis.slowlog.client")).width(170.),
                ];
                let rows = this
                    .diag
                    .slowlog
                    .iter()
                    .map(|entry| {
                        let at = chrono::DateTime::from_timestamp(entry.at, 0).map(|time| time.with_timezone(&chrono::Local).format("%m-%d %H:%M:%S").to_string()).unwrap_or_default();
                        let client = if cluster { format!("{} · {}", entry.client, entry.node) } else { entry.client.clone() };
                        vec![entry.id.to_string(), at, format!("{:.2} ms", entry.micros as f64 / 1000.), entry.command.clone(), client].into_iter().map(SharedString::from).collect()
                    })
                    .collect();
                (columns, rows)
            },
            Some(empty),
            window,
            cx,
        );
        div().flex_1().min_h(px(0.)).flex().flex_col().child(toolbar).child(Self::table_area("redis-slowlog-rows", table)).into_any_element()
    }

    fn render_info(&mut self, p: &Palette, m: &kit::Metrics, entity: &Entity<Self>, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let refresh = icon_btn("redis-diag-refresh", IconName::RefreshCw, &self.text("redis.tab.refresh"), entity).into_any_element();
        let toolbar = self.diag_toolbar(p, m, entity, None, false, vec![refresh]);
        let metrics = crate::adapters::redis::diag::key_metrics(&self.diag.info);
        // 顶部关键指标卡片。
        let mut cards = div().flex().flex_wrap().gap(px(Space::MD)).px(px(m.pane_pad())).py(px(Space::MD));
        for (label, value) in metrics {
            cards = cards.child(
                kit::card(p)
                    .min_w(px(ControlSize::PICKER_MIN_W / 2.))
                    .px(px(Space::MD))
                    .py(px(Space::MD))
                    .flex()
                    .flex_col()
                    .gap(px(Space::XXS))
                    .child(kit::eyebrow(SharedString::from(self.text(label)), p))
                    .child(kit::metric_value(SharedString::from(value), p)),
            );
        }
        // 下方是一张虚拟化的只读表（分组 / 指标 / 值）：只有这一层滚动，滚轮不会被嵌套表格截走。
        let hint = self.text("redis.info.filter");
        let query_input = self.redis_input(Field::InfoQuery, "redis-info-filter", "redis.info.filter", &hint, window, cx);
        let query = self.info_query.trim().to_lowercase();
        let rows: Vec<Vec<SharedString>> = self
            .diag
            .info
            .iter()
            .flat_map(|section| {
                let group = if section.name.is_empty() { "—".to_owned() } else { section.name.clone() };
                section.items.iter().map(move |(key, value)| (group.clone(), key.clone(), value.clone()))
            })
            .filter(|(group, key, value)| query.is_empty() || [group, key, value].iter().any(|text| text.to_lowercase().contains(&query)))
            .map(|(group, key, value)| vec![SharedString::from(group), SharedString::from(key), SharedString::from(value)])
            .collect();
        let empty = if self.diag.info.is_empty() { None } else { Some(SharedString::from(self.text("redis.info.noMatch"))) };
        let table = self.table_slot(
            "info",
            hash_of(&rows),
            move |this| {
                let columns = vec![
                    ReadColumn::new(this.text("redis.info.group")).width(140.),
                    ReadColumn::new(this.text("redis.info.key")).width(280.).mono(),
                    ReadColumn::new(this.text("redis.info.value")).width(420.).mono(),
                ];
                (columns, rows)
            },
            empty,
            window,
            cx,
        );
        let filter = div().flex_none().px(px(m.pane_pad())).pb(px(Space::SM)).child(
            div().w(px(ControlSize::PICKER_MIN_W)).max_w_full().child(
                kit::text_input("redis-info-filter", &query_input, self.text("redis.info.filter"))
                    .small()
                    .cleanable(true)
                    .prefix(kit::input_prefix(IconName::Search, IconSize::INLINE, p.text_faint)),
            ),
        );
        div()
            .id("redis-info-scroll")
            .test_support()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .flex_col()
            .child(toolbar)
            .child(cards.flex_none())
            .child(filter)
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .mx(px(m.pane_pad()))
                    .mb(px(m.pane_pad()))
                    .border_1()
                    .border_color(p.line_soft)
                    .rounded(px(Radius::CONTROL))
                    .overflow_hidden()
                    .child(table),
            )
            .into_any_element()
    }

    fn render_big_keys(&mut self, p: &Palette, m: &kit::Metrics, entity: &Entity<Self>, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let mut actions = vec![
            div()
                .flex()
                .items_center()
                .gap(px(Space::SM))
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
        let toolbar = self.diag_toolbar(p, m, entity, None, false, actions);
        let progress = (self.diag.big_running || self.diag.big_scanned > 0).then(|| {
            let text = match self.diag.big_total {
                Some(total) => i18n::fill(&self.text("redis.scan.progress"), &[("n", &self.diag.big_scanned.to_string()), ("total", &total.to_string())]),
                None => i18n::fill(&self.text("redis.scan.read"), &[("n", &self.diag.big_scanned.to_string())]),
            };
            let ratio = self.diag.big_total.filter(|total| *total > 0).map(|total| (self.diag.big_scanned as f32 / total as f32).min(1.)).unwrap_or(0.);
            div().px(px(m.pane_pad())).py(px(Space::SM)).child(progress_bar(&text, ratio, self.diag.big_running, p))
        });
        if self.diag.big.is_empty() && !self.diag.big_running {
            let empty = kit::empty_state(IconName::ChartBar, SharedString::from(self.text("redis.bigkeys.empty")), SharedString::from(self.text("redis.bigkeys.hint")), p);
            return div().flex_1().min_h(px(0.)).flex().flex_col().child(toolbar).children(progress).child(div().id("redis-big-rows").test_support().flex_1().child(empty)).into_any_element();
        }
        let stamp = hash_of(&self.diag.big.iter().map(|item| (item.key.as_str(), item.key_type.as_str(), item.size, item.memory)).collect::<Vec<_>>());
        let table = self.table_slot(
            "bigkeys",
            stamp,
            |this| {
                let columns = vec![
                    ReadColumn::new("#").width(44.).right(),
                    ReadColumn::new(this.text("redis.key.name")).width(320.),
                    ReadColumn::new(this.text("redis.new.type")).width(70.),
                    ReadColumn::new(this.text("redis.bigkeys.size")).width(100.).right(),
                    ReadColumn::new(this.text("redis.bigkeys.memoryCol")).width(100.).right(),
                ];
                let rows = this
                    .diag
                    .big
                    .iter()
                    .enumerate()
                    .map(|(index, item)| {
                        let memory = item.memory.map(keyspace::human_bytes).unwrap_or_else(|| "—".to_owned());
                        vec![(index + 1).to_string(), item.key.clone(), item.key_type.clone(), item.size.to_string(), memory].into_iter().map(SharedString::from).collect()
                    })
                    .collect();
                (columns, rows)
            },
            None,
            window,
            cx,
        );
        let hint = div().flex_none().px(px(m.pane_pad())).py(px(Space::XS)).text_size(px(TypeScale::XS)).text_color(p.text_faint).child(SharedString::from(self.text("redis.bigkeys.openHint")));
        div().flex_1().min_h(px(0.)).flex().flex_col().child(toolbar).children(progress).child(hint).child(Self::table_area("redis-big-rows", table)).into_any_element()
    }

    fn render_clients(&mut self, p: &Palette, m: &kit::Metrics, entity: &Entity<Self>, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.table_focus_row("clients", cx).filter(|row| *row < self.diag.clients.len());
        let refresh = icon_btn("redis-diag-refresh", IconName::RefreshCw, &self.text("redis.tab.refresh"), entity).into_any_element();
        let kill = action(
            match selected {
                Some(row) => format!("redis-client-kill:{row}"),
                None => "redis-client-kill-none".to_owned(),
            },
            Some(IconName::Unplug),
            &self.text("redis.clients.kill"),
            Kind::DangerSoft,
            entity,
        )
        .small()
        .disabled(selected.is_none())
        .into_any_element();
        let toolbar = self.diag_toolbar(p, m, entity, Some(self.text("redis.clients.hint")), true, vec![refresh, kill]);
        let cluster = self.topology.cluster;
        let stamp = hash_of(&(
            self.diag.clients.iter().map(|c| (c.id.as_str(), c.addr.as_str(), c.name.as_str(), c.age.as_str(), c.idle.as_str(), c.db.as_str(), c.cmd.as_str(), c.node.as_str())).collect::<Vec<_>>(),
            cluster,
        ));
        let table = self.table_slot(
            "clients",
            stamp,
            move |this| {
                let columns = vec![
                    ReadColumn::new("id").width(64.).right(),
                    ReadColumn::new(this.text("redis.clients.addr")).width(170.).mono(),
                    ReadColumn::new(this.text("redis.clients.name")).width(120.),
                    ReadColumn::new(this.text("redis.clients.age")).width(70.).right(),
                    ReadColumn::new(this.text("redis.clients.idle")).width(70.).right(),
                    ReadColumn::new("db").width(44.).right(),
                    ReadColumn::new(this.text("redis.clients.cmd")).width(200.).mono(),
                ];
                let rows = this
                    .diag
                    .clients
                    .iter()
                    .map(|client| {
                        let cmd = if cluster { format!("{} · {}", client.cmd, client.node) } else { client.cmd.clone() };
                        vec![client.id.clone(), client.addr.clone(), client.name.clone(), format!("{}s", client.age), format!("{}s", client.idle), client.db.clone(), cmd]
                            .into_iter()
                            .map(SharedString::from)
                            .collect()
                    })
                    .collect();
                (columns, rows)
            },
            None,
            window,
            cx,
        );
        div().flex_1().min_h(px(0.)).flex().flex_col().child(toolbar).child(Self::table_area("redis-client-rows", table)).into_any_element()
    }

    /// Pub/Sub：上面订阅（专用连接），中间是收到的消息，下面发布。没有消息时给出下一步，而不是一个空框。
    fn render_pubsub(&mut self, p: &Palette, m: &kit::Metrics, entity: &Entity<Self>, mono: &SharedString, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let channels_hint = self.text("redis.pubsub.channelsHint");
        let channel_hint = self.text("redis.pubsub.channel");
        let message_hint = self.text("redis.pubsub.message");
        let channels = self.redis_input(Field::SubChannels, "redis-sub-channels", "redis.pubsub.channels", &channels_hint, window, cx);
        let pub_channel = self.redis_input(Field::PubChannel, "redis-pub-channel", "redis.pubsub.channel", &channel_hint, window, cx);
        let pub_message = self.redis_input(Field::PubMessage, "redis-pub-message", "redis.pubsub.message", &message_hint, window, cx);
        let (status_text, status_color) = if self.diag.sub_running && self.diag.sub_ready {
            (i18n::fill(&self.text("redis.pubsub.listening"), &[("n", &self.diag.messages.len().to_string())]), p.ok)
        } else if self.diag.sub_running {
            (self.text("redis.pubsub.connecting"), p.warn)
        } else {
            (self.text("redis.pubsub.idle"), p.text_faint)
        };
        let status = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(Space::XS))
            .text_size(px(TypeScale::XS))
            .text_color(p.text_muted)
            .child(div().size(px(8.)).rounded_full().bg(status_color))
            .child(SharedString::from(status_text));
        let subscribe = div()
            .flex_none()
            .px(px(m.pane_pad()))
            .py(px(Space::SM))
            .flex()
            .items_center()
            .gap(px(Space::SM))
            .border_b_1()
            .border_color(p.line_soft)
            .child(div().flex_1().min_w(px(160.)).child(kit::text_input("redis-sub-channels", &channels, self.text("redis.pubsub.channels")).small().prefix(kit::input_prefix(IconName::Radio, IconSize::INLINE, p.text_faint))))
            .child(if self.diag.sub_running {
                action("redis-sub-stop", Some(IconName::CircleStop), &self.text("redis.pubsub.stop"), Kind::Secondary, entity).small()
            } else {
                action("redis-sub-start", Some(IconName::Radio), &self.text("redis.pubsub.subscribe"), Kind::Primary, entity).small()
            })
            .child(status)
            .child(div().flex_1())
            .child(icon_btn("redis-sub-clear", IconName::Trash, &self.text("redis.console.clear"), entity).disabled(self.diag.messages.is_empty()));
        let body = if self.diag.messages.is_empty() {
            div()
                .id("redis-pubsub-messages")
                .test_support()
                .flex_1()
                .min_h(px(0.))
                .p(px(m.pane_pad() * 2.))
                .child(kit::empty_state(
                    IconName::Radio,
                    SharedString::from(self.text(if self.diag.sub_running { "redis.pubsub.waiting" } else { "redis.pubsub.emptyHeading" })),
                    SharedString::from(self.text("redis.pubsub.emptyDetail")),
                    p,
                ))
                .into_any_element()
        } else {
            let mut list = div().id("redis-pubsub-messages").test_support().flex_1().min_h(px(0.)).scroll_area_y().flex().flex_col().px(px(m.pane_pad())).py(px(Space::XS));
            for (index, message) in self.diag.messages.iter().enumerate() {
                list = list.child(
                    div()
                        .id(SharedString::from(format!("redis-pubsub-msg:{index}")))
                        .flex_none()
                        .py(px(Space::XS))
                        .flex()
                        .gap(px(Space::MD))
                        .when(index > 0, |this| this.border_t_1().border_color(p.line_soft))
                        .font_family(mono.clone())
                        .text_size(px(TypeScale::SM))
                        .child(div().flex_none().w(px(180.)).truncate().text_color(p.accent_text).child(SharedString::from(message.channel.clone())))
                        .child(div().flex_1().min_w(px(0.)).text_color(p.text).child(SharedString::from(one_line(&message.payload)))),
                );
            }
            list.into_any_element()
        };
        let publish = div()
            .flex_none()
            .px(px(m.pane_pad()))
            .py(px(Space::SM))
            .flex()
            .items_center()
            .gap(px(Space::SM))
            .border_t_1()
            .border_color(p.line)
            .bg(p.chrome)
            .child(div().w(px(180.)).child(kit::text_input("redis-pub-channel", &pub_channel, self.text("redis.pubsub.channel")).small()))
            .child(div().flex_1().min_w(px(160.)).child(kit::text_input("redis-pub-message", &pub_message, self.text("redis.pubsub.message")).small()))
            .child(action("redis-pub-send", Some(IconName::Send), &self.text("redis.pubsub.publish"), Kind::Primary, entity).small());
        div().flex_1().min_h(px(0.)).flex().flex_col().child(subscribe).child(body).child(publish).into_any_element()
    }

    fn render_nodes(&mut self, p: &Palette, m: &kit::Metrics, entity: &Entity<Self>, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let refresh = icon_btn("redis-diag-refresh", IconName::RefreshCw, &self.text("redis.tab.refresh"), entity).into_any_element();
        let toolbar = self.diag_toolbar(p, m, entity, None, false, vec![refresh]);
        let rows: Vec<Vec<String>> = self
            .topology
            .nodes
            .iter()
            .map(|node| {
                let keys = self.diag.node_sizes.iter().find(|(addr, _)| *addr == node.addr()).map(|(_, n)| n.to_string()).unwrap_or_else(|| "—".to_owned());
                let role = if node.master { self.text("redis.nodes.master") } else { self.text("redis.nodes.replica") };
                vec![node.addr(), role, node.status().to_owned(), node.slot_text(), node.slot_total().to_string(), keys]
            })
            .collect();
        let stamp = hash_of(&rows);
        let table = self.table_slot(
            "nodes",
            stamp,
            move |this| {
                let columns = vec![
                    ReadColumn::new(this.text("redis.nodes.addr")).width(170.).mono(),
                    ReadColumn::new(this.text("redis.nodes.role")).width(80.),
                    ReadColumn::new(this.text("redis.nodes.status")).width(90.),
                    ReadColumn::new(this.text("redis.nodes.slots")).width(240.).mono(),
                    ReadColumn::new(this.text("redis.nodes.slotCount")).width(80.).right(),
                    ReadColumn::new(this.text("redis.nodes.keys")).width(90.).right(),
                ];
                (columns, rows.into_iter().map(|row| row.into_iter().map(SharedString::from).collect()).collect())
            },
            None,
            window,
            cx,
        );
        div().flex_1().min_h(px(0.)).flex().flex_col().child(toolbar).child(Self::table_area("redis-node-rows", table)).into_any_element()
    }
}

fn hash_of<T: std::hash::Hash>(value: &T) -> u64 {
    use std::hash::Hasher as _;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
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
