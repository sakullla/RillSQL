//! 工作台的新增动作：页签切换、扫描控制、分隔符/上限偏好、模板操作（抽样、复制模式、内存估算、
//! 标记变量段、批量删除/TTL）、分页取值与诊断页（慢日志、信息、大键、客户端、Pub/Sub、节点）。

use super::*;

impl RedisPane {
    /// 处理新增命令；返回 true 表示已处理（调用方负责重绘）。
    pub(super) fn command_extra(&mut self, id: &str, cx: &mut Context<Self>) -> bool {
        if let Some(page) = id.strip_prefix("redis-page:").and_then(Page::from_id) {
            self.open_page(page, cx);
        } else if id == "redis-scan-cancel" {
            self.cancel_scan();
        } else if id == "redis-prefs-toggle" {
            self.prefs_open = !self.prefs_open;
            if self.prefs_open {
                self.sep_input = self.prefs.separators_text();
                self.cap_input = self.prefs.scan_cap.to_string();
                self.focus_request = Some(Field::Separators);
            }
        } else if id == "redis-prefs-close" {
            self.prefs_open = false;
        } else if id == "redis-prefs-save" {
            self.save_keyspace_prefs(cx);
        } else if let Some(template) = id.strip_prefix("redis-tpl:") {
            if !self.open_templates.remove(template) {
                self.open_templates.insert(template.to_owned());
            }
            self.invalidate_rows();
        } else if let Some(template) = id.strip_prefix("redis-tpl-copy:") {
            let pattern = keyspace::template_pattern(template, &self.prefs.separators);
            self.copy_text(pattern, "redis.copied.pattern", cx);
        } else if let Some(template) = id.strip_prefix("redis-tpl-filter:") {
            self.pattern = keyspace::template_pattern(template, &self.prefs.separators);
            self.scan_from_start(cx);
        } else if let Some(template) = id.strip_prefix("redis-tpl-memory:") {
            self.estimate_template(template.to_owned(), cx);
        } else if let Some(template) = id.strip_prefix("redis-tpl-delete:") {
            let keys = self.template_members(template);
            self.stage_batch(template.to_owned(), keys, BatchOp::Unlink);
        } else if let Some(template) = id.strip_prefix("redis-tpl-ttl:") {
            self.batch_ttl_for = Some(template.to_owned());
            self.batch_ttl_input.clear();
            self.focus_request = Some(Field::BatchTtl);
        } else if id == "redis-batch-ttl-apply" {
            let Some(template) = self.batch_ttl_for.clone() else { return true };
            match parse_ttl(&self.batch_ttl_input) {
                Ok(seconds) => {
                    let keys = self.template_members(&template);
                    self.batch_ttl_for = None;
                    self.stage_batch(template, keys, BatchOp::Ttl(seconds));
                }
                Err(()) => self.show_error(self.text("redis.tab.ttlInvalid")),
            }
        } else if id == "redis-batch-ttl-close" {
            self.batch_ttl_for = None;
        } else if id == "redis-batch-cancel" {
            if let Some(batch) = &self.batch {
                batch.cancel.store(true, Ordering::SeqCst);
            }
        } else if id == "redis-batch-dismiss" {
            if self.batch.as_ref().is_some_and(|batch| batch.progress.finished) {
                self.batch = None;
            }
        } else if let Some(rest) = id.strip_prefix("redis-tpl-mark:") {
            self.mark_segment(rest, cx);
        } else if id == "redis-tpl-rules-reset" {
            self.prefs.rules.clear();
            self.persist_prefs(cx);
            self.invalidate_rows();
        } else if id == "redis-value-more" {
            self.load_more_value(cx);
        } else if id == "redis-diag-refresh" {
            self.load_page(self.page, true, cx);
        } else if let Some(node) = id.strip_prefix("redis-diag-node:") {
            self.diag.node = if node == "*" { String::new() } else { node.to_owned() };
            self.diag.slow_loaded = false;
            self.diag.info_loaded = false;
            self.diag.clients_loaded = false;
            self.load_page(self.page, true, cx);
        } else if let Some(by) = id.strip_prefix("redis-slow-sort:") {
            let by_time = by == "time";
            if self.diag.slow_by_time == by_time {
                self.diag.slow_asc = !self.diag.slow_asc;
            } else {
                self.diag.slow_by_time = by_time;
                self.diag.slow_asc = false;
            }
            self.sort_slowlog();
        } else if id == "redis-slow-reset" {
            let node = self.diag.node.clone();
            self.confirm_prompt = i18n::fill(&self.text("redis.confirm.pending"), &[("command", "SLOWLOG RESET")]);
            self.confirm = Some(Confirm::SlowlogReset { node });
        } else if let Some(index) = id.strip_prefix("redis-client-kill:") {
            if let Some(client) = index.parse::<usize>().ok().and_then(|index| self.diag.clients.get(index)).cloned() {
                self.confirm_prompt = i18n::fill(&self.text("redis.confirm.pending"), &[("command", &format!("CLIENT KILL ID {}", client.id))]);
                self.confirm = Some(Confirm::ClientKill { node: client.node.clone(), id: client.id.clone(), addr: client.addr.clone() });
            }
        } else if id == "redis-client-kill-none" {
            self.diag.error = self.text("redis.clients.pickFirst");
        } else if id == "redis-big-start" || id == "redis-big-start-empty" {
            self.start_big_keys(cx);
        } else if id == "redis-big-cancel" {
            if let Some(cancel) = &self.diag.big_cancel {
                cancel.store(true, Ordering::SeqCst);
            }
            self.diag.big_running = false;
        } else if id == "redis-big-memory" {
            self.diag.big_memory = !self.diag.big_memory;
        } else if let Some(index) = id.strip_prefix("redis-big-open:") {
            if let Some(item) = index.parse::<usize>().ok().and_then(|index| self.diag.big.get(index)).cloned() {
                self.page = Page::Keys;
                self.select_key(item.key, cx);
            }
        } else if id == "redis-sub-start" {
            self.start_subscribe(cx);
        } else if id == "redis-sub-stop" {
            self.stop_subscribe();
        } else if id == "redis-sub-clear" {
            self.diag.messages.clear();
        } else if let Some(index) = id.strip_prefix("redis-pubsub-copy:") {
            if let Some(payload) = index.parse::<usize>().ok().and_then(|ix| self.diag.messages.get(ix)).map(|msg| msg.payload.clone()) {
                self.copy_text(payload, "redis.copied.value", cx);
            }
        } else if let Some(index) = id.strip_prefix("redis-pubsub-fill-channel:") {
            if let Some(channel) = index.parse::<usize>().ok().and_then(|ix| self.diag.messages.get(ix)).map(|msg| msg.channel.clone()) {
                self.diag.pub_channel = channel;
                self.focus_request = Some(Field::PubMessage);
            }
        } else if let Some(section) = id.strip_prefix("redis-info-section:") {
            self.diag.info_section = if section == "*" { String::new() } else { section.to_owned() };
        } else if id == "redis-pub-send" {
            self.ask_publish(cx);
        } else {
            return false;
        }
        true
    }

    // ------------------------------------------------------------------ 页签

    fn open_page(&mut self, page: Page, cx: &mut Context<Self>) {
        if page == Page::Nodes && !self.topology.cluster {
            return;
        }
        self.page = page;
        if page == Page::Console {
            // 切到控制台就可以直接打字。
            self.focus_request = Some(Field::Command);
        }
        self.load_page(page, false, cx);
    }

    /// 页签第一次打开时读取；`force` 为刷新。
    pub(super) fn load_page(&mut self, page: Page, force: bool, cx: &mut Context<Self>) {
        if self.connection_id.is_empty() {
            return;
        }
        let connections = Arc::clone(&self.services.connections);
        let connection_id = self.connection_id.clone();
        let node = self.diag.node.clone();
        match page {
            Page::Keys => {
                if force {
                    self.scan_from_start(cx);
                }
            }
            Page::Slowlog if force || !self.diag.slow_loaded => {
                self.diag_begin();
                self.run_bg(
                    cx,
                    async move { crate::application::redis::slowlog(connections.as_ref(), &connection_id, node, 128).await },
                    |pane, result, _| {
                        pane.diag.busy = false;
                        pane.diag.slow_loaded = true;
                        match result {
                            Ok(entries) => {
                                pane.diag.slowlog = entries;
                                pane.sort_slowlog();
                            }
                            Err(message) => pane.diag.error = message,
                        }
                    },
                );
            }
            Page::Info if force || !self.diag.info_loaded => {
                self.diag_begin();
                self.run_bg(
                    cx,
                    async move { crate::application::redis::info(connections.as_ref(), &connection_id, node).await },
                    |pane, result, _| {
                        pane.diag.busy = false;
                        pane.diag.info_loaded = true;
                        match result {
                            Ok(sections) => pane.diag.info = sections,
                            Err(message) => pane.diag.error = message,
                        }
                    },
                );
            }
            Page::Clients if force || !self.diag.clients_loaded => {
                self.diag_begin();
                self.run_bg(
                    cx,
                    async move { crate::application::redis::clients(connections.as_ref(), &connection_id, node).await },
                    |pane, result, _| {
                        pane.diag.busy = false;
                        pane.diag.clients_loaded = true;
                        match result {
                            Ok(clients) => pane.diag.clients = clients,
                            Err(message) => pane.diag.error = message,
                        }
                    },
                );
            }
            Page::Nodes if force || !self.diag.nodes_loaded => {
                self.diag_begin();
                self.run_bg(
                    cx,
                    async move {
                        let topology = crate::application::redis::topology(connections.as_ref(), &connection_id).await?;
                        let sizes = crate::application::redis::node_key_counts(connections.as_ref(), &connection_id).await.unwrap_or_default();
                        Ok::<_, String>((topology, sizes))
                    },
                    |pane, result, _| {
                        pane.diag.busy = false;
                        pane.diag.nodes_loaded = true;
                        match result {
                            Ok((topology, sizes)) => {
                                pane.topology = topology;
                                pane.diag.node_sizes = sizes;
                            }
                            Err(message) => pane.diag.error = message,
                        }
                    },
                );
            }
            _ => {}
        }
    }

    fn diag_begin(&mut self) {
        self.diag.busy = true;
        self.diag.error.clear();
    }

    pub(super) fn sort_slowlog(&mut self) {
        let by = if self.diag.slow_by_time { SlowSort::Time } else { SlowSort::Duration };
        crate::adapters::redis::diag::sort_slowlog(&mut self.diag.slowlog, by, !self.diag.slow_asc);
    }

    pub(super) fn run_slowlog_reset(&mut self, node: String, cx: &mut Context<Self>) {
        let connections = Arc::clone(&self.services.connections);
        let connection_id = self.connection_id.clone();
        self.diag_begin();
        self.run_bg(
            cx,
            async move { crate::application::redis::slowlog_reset(connections.as_ref(), &connection_id, node, true).await },
            |pane, result, cx| {
                pane.diag.busy = false;
                match result {
                    Ok(()) => {
                        pane.note = pane.text("redis.slowlog.cleared");
                        pane.load_page(Page::Slowlog, true, cx);
                    }
                    Err(message) => pane.diag.error = message,
                }
            },
        );
    }

    pub(super) fn run_client_kill(&mut self, node: String, id: String, cx: &mut Context<Self>) {
        let connections = Arc::clone(&self.services.connections);
        let connection_id = self.connection_id.clone();
        self.diag_begin();
        self.run_bg(
            cx,
            async move { crate::application::redis::client_kill(connections.as_ref(), &connection_id, node, id, true).await },
            |pane, result, cx| {
                pane.diag.busy = false;
                match result {
                    Ok(_) => {
                        pane.note = pane.text("redis.clients.killed");
                        pane.load_page(Page::Clients, true, cx);
                    }
                    Err(message) => pane.diag.error = message,
                }
            },
        );
    }

    fn start_big_keys(&mut self, cx: &mut Context<Self>) {
        if self.connection_id.is_empty() || self.diag.big_running {
            return;
        }
        if let Some(cancel) = &self.diag.big_cancel {
            cancel.store(true, Ordering::SeqCst);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.diag.big_cancel = Some(Arc::clone(&cancel));
        self.diag.big_running = true;
        self.diag.big_done = false;
        self.diag.big_scanned = 0;
        self.diag.big_total = None;
        self.diag.big.clear();
        self.diag.error.clear();
        let connections = Arc::clone(&self.services.connections);
        let connection_id = self.connection_id.clone();
        let with_memory = self.diag.big_memory;
        self.run_stream(
            cx,
            move |send| {
                Box::pin(async move {
                    crate::application::redis::big_keys(connections.as_ref(), &connection_id, 50, with_memory, cancel, send).await;
                })
            },
            |pane: &mut Self, events: Vec<BigKeyEvent>, _| {
                for event in events {
                    match event {
                        BigKeyEvent::Progress { scanned, total, top } => {
                            pane.diag.big_scanned = scanned;
                            pane.diag.big_total = total;
                            pane.diag.big = top;
                        }
                        BigKeyEvent::Finished { scanned, top, cancelled } => {
                            pane.diag.big_scanned = scanned;
                            pane.diag.big = top;
                            pane.diag.big_running = false;
                            pane.diag.big_done = !cancelled;
                        }
                        BigKeyEvent::Failed(message) => {
                            pane.diag.big_running = false;
                            pane.diag.error = message;
                        }
                    }
                }
            },
        );
    }

    fn start_subscribe(&mut self, cx: &mut Context<Self>) {
        let channels: Vec<String> = self.diag.sub_input.split_whitespace().map(str::to_owned).collect();
        if channels.is_empty() {
            self.diag.error = self.text("redis.pubsub.needChannel");
            return;
        }
        self.stop_subscribe();
        let cancel = Arc::new(AtomicBool::new(false));
        self.diag.sub_cancel = Some(Arc::clone(&cancel));
        self.diag.sub_running = true;
        self.diag.sub_ready = false;
        self.diag.error.clear();
        let connections = Arc::clone(&self.services.connections);
        let connection_id = self.connection_id.clone();
        self.run_stream(
            cx,
            move |send| {
                Box::pin(async move {
                    crate::application::redis::subscribe(connections.as_ref(), &connection_id, channels, cancel, send).await;
                })
            },
            |pane: &mut Self, events: Vec<Result<PubSubMessage, String>>, _| {
                if !pane.diag.sub_running {
                    return;
                }
                for event in events {
                    match event {
                        Ok(message) if message.channel.is_empty() => pane.diag.sub_ready = true,
                        Ok(message) => {
                            let at = chrono::Local::now().format("%H:%M:%S").to_string();
                            pane.diag.messages.push_front(ReceivedMessage { channel: message.channel, payload: message.payload, at });
                            pane.diag.messages.truncate(PUBSUB_KEEP);
                        }
                        Err(message) => {
                            pane.diag.error = message;
                            pane.diag.sub_running = false;
                        }
                    }
                }
            },
        );
    }

    fn stop_subscribe(&mut self) {
        if let Some(cancel) = self.diag.sub_cancel.take() {
            cancel.store(true, Ordering::SeqCst);
        }
        self.diag.sub_running = false;
        self.diag.sub_ready = false;
    }

    /// 发布：生产库先确认（会触发订阅方的业务动作），其余直接发。
    fn ask_publish(&mut self, cx: &mut Context<Self>) {
        let channel = self.diag.pub_channel.trim().to_owned();
        if channel.is_empty() {
            self.diag.error = self.text("redis.pubsub.needChannel");
            return;
        }
        let message = self.diag.pub_message.clone();
        if self.prod_target {
            self.confirm_prompt = i18n::fill(&self.text("redis.confirm.pending"), &[("command", &format!("PUBLISH {channel}"))]);
            self.confirm = Some(Confirm::Publish { channel, message });
            return;
        }
        self.run_publish(channel, message, cx);
    }

    pub(super) fn run_publish(&mut self, channel: String, message: String, cx: &mut Context<Self>) {
        let connections = Arc::clone(&self.services.connections);
        let connection_id = self.connection_id.clone();
        self.run_bg(
            cx,
            async move { crate::application::redis::publish(connections.as_ref(), &connection_id, channel, message).await },
            |pane, result, _| match result {
                Ok(receivers) => {
                    pane.note = i18n::fill(&pane.text("redis.pubsub.published"), &[("n", &receivers.to_string())]);
                    pane.diag.pub_message.clear();
                    pane.diag.error.clear();
                }
                Err(message) => pane.diag.error = message,
            },
        );
    }

    // ------------------------------------------------------------------ 偏好

    fn save_keyspace_prefs(&mut self, cx: &mut Context<Self>) {
        let cap = self.cap_input.trim();
        if !cap.is_empty() {
            match cap.parse::<usize>() {
                Ok(value) => self.prefs.scan_cap = value,
                Err(_) => {
                    self.show_error(self.text("redis.prefs.capInvalid"));
                    return;
                }
            }
        }
        self.prefs.set_separators_text(&self.sep_input);
        self.prefs_open = false;
        self.expanded.clear();
        self.open_templates.clear();
        if self.view_mode == ViewMode::Tree {
            self.expand_top_level();
        }
        self.invalidate_rows();
        self.persist_prefs(cx);
        // 上限放宽后可以从续扫位置继续。
        if self.scan.capped && self.keys.len() < self.prefs.scan_cap {
            self.load_keys(cx);
        }
    }

    pub(super) fn persist_prefs(&mut self, cx: &mut Context<Self>) {
        if self.connection_id.is_empty() {
            return;
        }
        let connections = Arc::clone(&self.services.connections);
        let connection_id = self.connection_id.clone();
        let prefs = self.prefs.clone();
        self.run_bg(
            cx,
            async move { crate::application::redis::save_prefs(connections.as_ref(), &connection_id, &prefs).await },
            |pane, result, _| {
                if let Err(message) = result {
                    pane.show_error(message);
                }
            },
        );
    }

    // ------------------------------------------------------------------ 模板

    /// `seg:flag:template`：把模板第 seg 段标为变量（1）或固定（0），规则按连接保存。
    fn mark_segment(&mut self, rest: &str, cx: &mut Context<Self>) {
        let mut parts = rest.splitn(3, ':');
        let (Some(seg), Some(flag), Some(template)) = (parts.next(), parts.next(), parts.next()) else { return };
        let (Ok(index), variable) = (seg.parse::<usize>(), flag == "1") else { return };
        let Some(sample) = self.template_members(template).into_iter().next() else { return };
        let Some(rule) = keyspace::rule_for(&sample, &self.prefs.separators, &self.prefs.rules, index, variable) else { return };
        keyspace::upsert_rule(&mut self.prefs.rules, rule);
        self.open_templates.clear();
        self.invalidate_rows();
        self.persist_prefs(cx);
    }

    /// 抽样 MEMORY USAGE × 键数，标注为估算。
    fn estimate_template(&mut self, template: String, cx: &mut Context<Self>) {
        let members = self.template_members(&template);
        if members.is_empty() {
            return;
        }
        let count = members.len();
        let samples = keyspace::sample(&members, self.prefs.memory_samples);
        let connections = Arc::clone(&self.services.connections);
        let connection_id = self.connection_id.clone();
        self.template_memory.insert(template.clone(), None);
        self.run_bg(
            cx,
            async move { crate::application::redis::memory_samples(connections.as_ref(), &connection_id, samples).await },
            move |pane, result, _| match result {
                Ok(bytes) => {
                    pane.template_memory.insert(template, keyspace::estimate_memory(&bytes, count));
                }
                Err(message) => {
                    pane.template_memory.remove(&template);
                    pane.show_error(message);
                }
            },
        );
    }

    /// 批量操作先进确认框：数量、样例与命令摘要就是预览。
    fn stage_batch(&mut self, template: String, keys: Vec<String>, op: BatchOp) {
        if keys.is_empty() {
            self.show_error(self.text("redis.batch.empty"));
            return;
        }
        if self.batch.as_ref().is_some_and(|batch| !batch.progress.finished) {
            self.show_error(self.text("redis.batch.busy"));
            return;
        }
        let preview = batch_ops::preview(&keys, op);
        let key = match op {
            BatchOp::Unlink => "redis.batch.deletePrompt",
            BatchOp::Ttl(_) => "redis.batch.ttlPrompt",
        };
        self.confirm_prompt = i18n::fill(&self.text(key), &[("n", &preview.count.to_string()), ("template", &template)]);
        let db = self.current_db();
        self.confirm = Some(Confirm::Batch { template, keys, op, preview, db });
        self.note.clear();
        self.error.clear();
    }

    /// 批量操作是否还在进行。
    pub(super) fn batch_running(&self) -> bool {
        self.batch.as_ref().is_some_and(|batch| !batch.progress.finished)
    }

    /// 当前逻辑库号（解析不了按 0）。
    pub(super) fn current_db(&self) -> u8 {
        self.database.trim().parse::<u8>().unwrap_or(0)
    }

    pub(super) fn run_batch(&mut self, template: String, keys: Vec<String>, op: BatchOp, db: u8, cx: &mut Context<Self>) {
        if self.batch_running() {
            self.show_error(self.text("redis.batch.busy"));
            return;
        }
        // 确认框开着时库已经换了（比如控制台 SELECT）：键名来自旧库的索引，不在新库上执行。
        if db != self.current_db() {
            self.show_error(self.text("redis.batch.dbChanged"));
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.batch = Some(BatchRun { template, op, progress: BatchProgress { total: keys.len(), ..BatchProgress::default() }, cancel: Arc::clone(&cancel) });
        let connections = Arc::clone(&self.services.connections);
        let connection_id = self.connection_id.clone();
        let done_keys = keys.clone();
        self.run_stream(
            cx,
            move |send| {
                Box::pin(async move {
                    let outcome = crate::application::redis::batch(connections.as_ref(), &connection_id, crate::application::redis::BatchRequest { db, keys, op }, true, cancel, |progress| send(Ok(progress))).await;
                    if let Err(message) = outcome {
                        send(Err(message));
                    }
                })
            },
            |pane: &mut Self, events: Vec<Result<BatchProgress, String>>, cx| {
                for event in events {
                    match event {
                        Ok(progress) => {
                            if let Some(batch) = &mut pane.batch {
                                batch.progress = progress;
                            }
                            if progress.finished {
                                pane.finish_batch(cx);
                            }
                        }
                        Err(message) => {
                            if let Some(batch) = &mut pane.batch {
                                batch.progress.finished = true;
                            }
                            pane.show_error(message);
                        }
                    }
                }
            },
        );
        self.batch_keys = done_keys;
    }

    /// 批量删除完成：已处理的键从索引移除（取消时只移除确实处理过的批次之前的部分无法精确区分，整体重扫）。
    fn finish_batch(&mut self, cx: &mut Context<Self>) {
        let Some(batch) = self.batch.clone() else { return };
        let keys = std::mem::take(&mut self.batch_keys);
        match batch.op {
            BatchOp::Unlink if !batch.progress.cancelled && batch.progress.failed == 0 => {
                let gone: std::collections::HashSet<String> = keys.into_iter().collect();
                if gone.contains(&self.selected) {
                    self.selected.clear();
                    self.detail = None;
                    self.placement = None;
                }
                self.drop_keys(&gone);
            }
            BatchOp::Unlink => self.scan_from_start(cx),
            BatchOp::Ttl(seconds) => {
                let ttl = seconds.map(|secs| secs * 1000).unwrap_or(-1);
                let set: std::collections::HashSet<String> = keys.into_iter().collect();
                for key in self.keys.iter_mut().filter(|key| set.contains(&key.name)) {
                    key.ttl_ms = Some(ttl);
                }
                self.invalidate_rows();
            }
        }
        self.refresh_keyspace_counts(cx);
        if !self.selected.is_empty() {
            let name = self.selected.clone();
            self.select_key(name, cx);
        }
    }

    pub(super) fn open_template_context_menu(&mut self, template: String, at: Point<Pixels>, cx: &mut Context<Self>) {
        let mut items = vec![
            ContextItem { id: format!("redis-tpl:{template}"), icon: IconName::Eye, label: self.text("redis.tpl.sample"), danger: false },
            ContextItem { id: format!("redis-tpl-filter:{template}"), icon: IconName::ListFilter, label: self.text("redis.tpl.filter"), danger: false },
            ContextItem { id: format!("redis-tpl-copy:{template}"), icon: IconName::Copy, label: self.text("redis.tpl.copy"), danger: false },
            ContextItem { id: format!("redis-tpl-memory:{template}"), icon: IconName::Gauge, label: self.text("redis.tpl.memory"), danger: false },
        ];
        for (index, (segment, variable)) in keyspace::template_segments(&template, &self.prefs.separators).into_iter().enumerate() {
            if keyspace::template_segments(&template, &self.prefs.separators).len() < 2 {
                break;
            }
            let (label_key, flag, icon) = if variable { ("redis.tpl.markFixed", 0, IconName::Pin) } else { ("redis.tpl.markVariable", 1, IconName::Asterisk) };
            items.push(ContextItem {
                id: format!("redis-tpl-mark:{index}:{flag}:{template}"),
                icon,
                label: i18n::fill(&self.text(label_key), &[("n", &(index + 1).to_string()), ("segment", &segment)]),
                danger: false,
            });
        }
        items.push(ContextItem { id: format!("redis-tpl-ttl:{template}"), icon: IconName::Timer, label: self.text("redis.tpl.batchTtl"), danger: false });
        items.push(ContextItem { id: format!("redis-tpl-delete:{template}"), icon: IconName::Trash, label: self.text("redis.tpl.batchDelete"), danger: true });
        self.context_menu = Some(ContextMenuState { at, items });
        cx.notify();
    }

    // ------------------------------------------------------------------ 写回核对

    /// 应用修改前先读一次服务端当前值，与读取时的值比较指纹：一致才进确认框，不一致提示刷新（不写）。
    pub(super) fn guarded_stage(&mut self, mutation: RedisMutation, probe: keys::ValueProbe, original: String, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            self.show_error(self.text("redis.tab.needKey"));
            return;
        }
        if self.reading && self.detail_key != self.selected {
            self.show_error(self.text("redis.value.reading"));
            return;
        }
        if let Err(message) = keys::prepare_mutation(&mutation, true) {
            self.show_error(message);
            return;
        }
        let expected = keys::value_fingerprint(Some(&original));
        self.guard_generation = self.guard_generation.wrapping_add(1);
        let generation = self.guard_generation;
        self.stale_value = false;
        self.note = self.text("redis.value.checking");
        let connections = Arc::clone(&self.services.connections);
        let connection_id = self.connection_id.clone();
        self.run_bg(
            cx,
            {
                let probe = probe.clone();
                async move { crate::application::redis::probe_value(connections.as_ref(), &connection_id, probe).await }
            },
            move |pane, result, _| {
                if pane.guard_generation == generation {
                    pane.finish_guard(mutation, probe, expected, result);
                }
            },
        );
    }

    /// 核对结果：值没变 → 进确认框（确认后写入前再核对一次）；变了 → 提示刷新，不进确认；读失败 → 报错。
    pub(super) fn finish_guard(&mut self, mutation: RedisMutation, probe: keys::ValueProbe, expected: u64, result: Result<Option<String>, String>) {
        match result {
            Ok(current) if keys::value_fingerprint(current.as_deref()) == expected => {
                self.note.clear();
                self.stage_if_key_guarded(mutation, Some((probe, expected)));
            }
            Ok(_) => {
                self.stale_value = true;
                self.show_error(self.text("redis.value.stale"));
            }
            Err(message) => self.show_error(message),
        }
    }

    /// 大字符串「加载全部」：GET 整个值，到达后换掉预览（查看器解除只读）。
    pub(super) fn load_full_string(&mut self, cx: &mut Context<Self>) {
        let key = self.selected.clone();
        if key.is_empty() {
            return;
        }
        let connections = Arc::clone(&self.services.connections);
        let connection_id = self.connection_id.clone();
        self.run_bg(
            cx,
            async move { crate::application::redis::read_full_string(connections.as_ref(), &connection_id, key.clone()).await.map(|text| (key, text)) },
            |pane, result: Result<(String, String), String>, cx| match result {
                Ok((key, text)) if key == pane.selected && pane.detail_key == key => {
                    if let Some(detail) = pane.detail.as_deref() {
                        let mut next = detail.clone();
                        next.total = Some(text.len() as u64);
                        next.value = RedisValue::String { text };
                        pane.detail = Some(Arc::new(next));
                        pane.reload_viewer = true;
                    }
                }
                Ok(_) => {}
                Err(message) => {
                    if let Some(viewer) = pane.string_viewer.clone() {
                        viewer.update(cx, |viewer, cx| viewer.set_truncation(crate::ui::TruncationState::Error(message.clone()), cx));
                    }
                    pane.show_error_from(ErrorSource::Detail, message);
                }
            },
        );
    }

    // ------------------------------------------------------------------ 值分页与集群

    fn load_more_value(&mut self, cx: &mut Context<Self>) {
        let Some(detail) = self.detail.clone() else { return };
        let Some(token) = detail.next.clone() else { return };
        if self.value_loading {
            return;
        }
        self.value_loading = true;
        let connections = Arc::clone(&self.services.connections);
        let connection_id = self.connection_id.clone();
        let key = self.selected.clone();
        let key_type = detail.key_type.clone();
        self.run_bg(
            cx,
            async move { crate::application::redis::read_page(connections.as_ref(), &connection_id, key.clone(), key_type, Some(token)).await.map(|page| (key, page)) },
            |pane, result: Result<(String, ValuePage), String>, _| {
                pane.value_loading = false;
                match result {
                    Ok((key, page)) if key == pane.selected => {
                        if let Some(detail) = pane.detail.as_deref() {
                            let mut next = detail.clone();
                            keys::append_page(&mut next.value, page.value);
                            next.next = page.next;
                            pane.detail = Some(Arc::new(next));
                        }
                    }
                    Ok(_) => {}
                    Err(message) => pane.show_error_from(ErrorSource::Detail, message),
                }
            },
        );
    }

    /// 集群里多键命令跨槽：发送前就拦下并说明（不静默失败）。返回 true 表示已拦下。
    pub(super) fn crossslot_refused(&mut self, command: &str, args: &[String]) -> bool {
        let (word, expanded) = exec::expand_command(command, args);
        match exec::crossslot_message(&word, &expanded) {
            Some(message) => {
                self.show_error(message);
                true
            }
            None => false,
        }
    }
}
