//! 对象编辑器的 GPUI 视图：事件、函数/存储过程与触发器的表单、预览、执行与逐步回执。
//!
//! 只做三件事：把表单写进 ObjectEditorSession、把草稿交给 ddl 计划层生成 SQL、把
//! Plan / Receipt 显示出来。不在这里拼 SQL，也不另建执行路径。

use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Editor, EditorState, InputEvent};
use gpui_kit::component::Sizable as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, TestSupportExt as _, Window, div, px, relative,
};

use crate::app::model::ObjectTarget;
use crate::application::AppServices;
use crate::catalog::model::ObjectKind;
use crate::connections::model::Engine;
use crate::ddl::model::{TriggerEvent, TriggerTiming};
use crate::i18n::{self, Locale};
use crate::ui::kit::ScrollAreaExt as _;
use crate::ui::kit::{self, FieldSpec, Fields, Palette, Radius, Space, TypeScale};

use super::{editable, title_key, EditorVerb, ObjectEditorSession};

/// 打开编辑器时要知道的目标：哪个连接、哪个库、哪类对象、叫什么。
pub struct EditorRequest {
    pub connection_id: String,
    pub database: String,
    pub schema: String,
    pub engine: String,
    pub kind: ObjectKind,
    /// 为空表示新建。
    pub name: String,
}

impl EditorRequest {
    /// 从对象树的目标起一份请求。
    pub fn of_object(target: &ObjectTarget, kind: ObjectKind) -> Self {
        Self {
            connection_id: target.connection_id.clone(),
            database: target.database.clone(),
            schema: target.schema.clone(),
            engine: target.engine.clone(),
            kind,
            name: target.name.clone(),
        }
    }

    /// 在某个库里新建一个对象。
    pub fn blank(connection_id: &str, database: &str, schema: &str, engine: &str, kind: ObjectKind) -> Self {
        Self {
            connection_id: connection_id.to_owned(),
            database: database.to_owned(),
            schema: schema.to_owned(),
            engine: engine.to_owned(),
            kind,
            name: String::new(),
        }
    }
}

/// 表单里的开关项：点击只改草稿，语句留到预览时由计划层生成。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorToggle {
    OneTime,
    Recurring,
    Enabled,
    Preserve,
    Before,
    After,
    Event(TriggerEvent),
}

pub struct ObjectEditorPane {
    pub session: ObjectEditorSession,
    services: Arc<AppServices>,
    fields: Fields<&'static str>,
    body_editor: Option<Entity<EditorState>>,
    body_sync: String,
    body_sub: Option<Subscription>,
    open: bool,
    loading: bool,
    previewing: bool,
    executing: bool,
    /// 每次打开或换目标自增：旧目标的异步回执不再写进新会话。
    epoch: u64,
    /// 执行成功后要让外壳刷新的对象：连接、库、种类、名字。
    changed: Option<(String, String, ObjectKind, String)>,
}

impl ObjectEditorPane {
    pub fn new(services: Arc<AppServices>) -> Self {
        Self {
            session: ObjectEditorSession::default(),
            services,
            fields: Fields::default(),
            body_editor: None,
            body_sync: String::new(),
            body_sub: None,
            open: false,
            loading: false,
            previewing: false,
            executing: false,
            epoch: 0,
            changed: None,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn set_locale(&mut self, locale: Locale) {
        self.session.locale = locale;
    }

    pub fn take_changed(&mut self) -> Option<(String, String, ObjectKind, String)> {
        self.changed.take()
    }

    /// 关闭编辑器：草稿与预览一起丢掉，不留半份状态给下一次打开。
    pub fn close(&mut self) {
        self.open = false;
        self.epoch += 1;
        self.loading = false;
        self.previewing = false;
        self.executing = false;
        self.session.cancel();
        self.session.note.clear();
        self.body_editor = None;
        self.body_sync.clear();
        self.body_sub = None;
    }

    /// 打开编辑器。已有对象先从服务器读回真实状态；新建只起一份空草稿。
    pub fn open_request(&mut self, request: EditorRequest, cx: &mut Context<Self>) {
        if !editable(request.kind) {
            self.session.note = "这里只编辑事件、函数、存储过程与触发器。".to_owned();
            return;
        }
        let locale = self.session.locale;
        let new_object = request.name.trim().is_empty();
        self.session = ObjectEditorSession {
            locale,
            connection_id: request.connection_id,
            database: request.database,
            schema: request.schema,
            engine: request.engine,
            kind: request.kind,
            name: request.name.trim().to_owned(),
            ..ObjectEditorSession::default()
        };
        self.open = true;
        self.epoch += 1;
        self.previewing = false;
        self.executing = false;
        self.loading = !new_object;
        self.body_editor = None;
        self.body_sync.clear();
        self.body_sub = None;
        if !new_object {
            self.load(cx);
        }
        cx.notify();
    }

    /// 首次打开时读回对象定义并填进表单。执行后不要走这条路径：seed 会覆盖草稿并清掉回执。
    pub fn load(&mut self, cx: &mut Context<Self>) {
        self.read_detail(true, cx);
    }

    /// 执行结束后读回对象真实状态：只更新 detail，不清表单、不清回执、不动提示、不关窗口。
    pub fn refresh_detail(&mut self, cx: &mut Context<Self>) {
        self.read_detail(false, cx);
    }

    /// 读回对象当前状态。读失败只把原始原因记进 detail_error，不动服务器。
    ///
    /// `seed_form` 决定读回之后要不要用服务器状态填表单：首次打开要，执行后不要。
    fn read_detail(&mut self, seed_form: bool, cx: &mut Context<Self>) {
        if self.session.name.trim().is_empty() {
            return;
        }
        let Ok(engine) = Engine::parse(&self.session.engine) else {
            self.session.detail_error = format!("不认识的引擎：{}", self.session.engine);
            self.loading = false;
            cx.notify();
            return;
        };
        let services = Arc::clone(&self.services);
        let connection = self.session.connection_id.clone();
        let database = self.session.database.clone();
        let kind = self.session.kind;
        let name = self.session.name.clone();
        let expected = name.clone();
        let epoch = self.epoch;
        self.loading = true;
        let pane = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::app::views::grid::runtime_for_views().spawn(async move {
                let result = match services.connections.catalog_session(&connection) {
                    Some(session) => crate::catalog::objects::read_object_detail(&session, engine, &database, kind, &name).await,
                    None => Err("这条连接还没有打开，没有另开新的连接。".to_owned()),
                };
                let _ = tx.send(result);
            });
            let Ok(result) = rx.await else { return };
            cx.update(|cx| {
                let Some(pane) = pane.upgrade() else { return };
                pane.update(cx, |this, cx| {
                    if this.epoch != epoch || this.session.kind != kind || this.session.name != expected {
                        return;
                    }
                    this.loading = false;
                    match result {
                        Ok(detail) if seed_form => {
                            // 首次打开：读回的结构化状态填进表单；主体由编辑器在下一帧按模型同步。
                            this.session.seed(&detail);
                        }
                        Ok(detail) => {
                            // 执行后：只记下服务器现状用来核对，表单、回执与提示都保持原样。
                            this.session.apply_detail(detail);
                        }
                        Err(message) => {
                            this.session.detail = None;
                            this.session.detail_error = message;
                        }
                    }
                    cx.notify();
                });
            });
        })
        .detach();
    }

    /// 表单里的一个文本框写回模型。任何输入都作废上一次预览。
    fn on_field(pane: &mut Self, key: &'static str, value: String, cx: &mut Context<Self>) {
        pane.session.set_field(key, value);
        if key == "name" {
            // 改了名字就不再是读回来的那个对象：清掉原对象状态，避免拿旧名去改新名。
            pane.session.detail = None;
            pane.session.detail_error.clear();
        }
        cx.notify();
    }

    /// 换对象种类：换一份草稿，避免把事件主体当成触发器主体提交。
    pub fn choose_kind(&mut self, kind: ObjectKind, cx: &mut Context<Self>) {
        if !editable(kind) {
            return;
        }
        let connection_id = self.session.connection_id.clone();
        let database = self.session.database.clone();
        let schema = self.session.schema.clone();
        let engine = self.session.engine.clone();
        self.session.set_kind(kind);
        self.session.connection_id = connection_id;
        self.session.database = database;
        self.session.schema = schema;
        self.session.engine = engine;
        self.body_sync = String::new();
        cx.notify();
    }

    fn ensure_body_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<EditorState> {
        if let Some(editor) = self.body_editor.clone() {
            return editor;
        }
        let initial = self.session.body.clone();
        let editor = cx.new(|cx| EditorState::new(window, cx).language("sql").line_number(true).soft_wrap(true).default_value(initial));
        self.body_sync = self.session.body.clone();
        self.body_sub = Some(cx.subscribe(&editor, |this, state, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let text = state.read(cx).value().to_string();
                if this.session.body != text {
                    this.session.body = text;
                    this.session.cancel();
                    cx.notify();
                }
            }
        }));
        self.body_editor = Some(editor.clone());
        editor
    }

    /// 预览当前草稿。语句由 ddl 计划层生成，这里只把结果收进来。
    pub fn preview_with(&mut self, verb: EditorVerb, cx: &mut Context<Self>) {
        if self.previewing || self.executing {
            return;
        }
        self.session.verb = verb;
        let action = match self.session.action() {
            Ok(action) => action,
            Err(message) => {
                self.session.note = message;
                cx.notify();
                return;
            }
        };
        let request = match crate::application::ddl::request(
            self.session.database.clone(),
            self.session.schema.clone(),
            &self.session.engine,
            action,
        ) {
            Ok(request) => request,
            Err(message) => {
                self.session.note = message;
                cx.notify();
                return;
            }
        };
        let services = Arc::clone(&self.services);
        let connection = self.session.connection_id.clone();
        let epoch = self.epoch;
        let pane = cx.entity().downgrade();
        self.previewing = true;
        self.session.note.clear();
        cx.notify();
        cx.spawn(async move |_, cx| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::app::views::grid::runtime_for_views().spawn(async move {
                let result = crate::application::ddl::preview_request(&services.connections, &connection, &request).await;
                let _ = tx.send(result);
            });
            let Ok(result) = rx.await else { return };
            cx.update(|cx| {
                let Some(pane) = pane.upgrade() else { return };
                pane.update(cx, |this, cx| {
                    if this.epoch != epoch {
                        return;
                    }
                    this.previewing = false;
                    match result {
                        Ok(plan) => this.session.plan = Some(plan),
                        Err(message) => this.session.note = message,
                    }
                    cx.notify();
                });
            });
        })
        .detach();
    }

    /// 执行已确认的预览：语句必须与预览时逐字一致（由执行链再核对一次）。
    pub fn confirm(&mut self, cx: &mut Context<Self>) {
        if self.executing || self.previewing {
            return;
        }
        let Some(sql) = self.session.confirmed_sql() else {
            self.session.note = i18n::text(self.session.locale, "structure.preview.need");
            cx.notify();
            return;
        };
        let action = match self.session.action() {
            Ok(action) => action,
            Err(message) => {
                self.session.note = message;
                cx.notify();
                return;
            }
        };
        let request = match crate::application::ddl::request(
            self.session.database.clone(),
            self.session.schema.clone(),
            &self.session.engine,
            action,
        ) {
            Ok(request) => request,
            Err(message) => {
                self.session.note = message;
                cx.notify();
                return;
            }
        };
        let services = Arc::clone(&self.services);
        let connection = self.session.connection_id.clone();
        let changed_connection = connection.clone();
        let epoch = self.epoch;
        let kind = self.session.kind;
        let name = self.session.name.trim().to_owned();
        let database = self.session.database.clone();
        let pane = cx.entity().downgrade();
        self.executing = true;
        cx.notify();
        cx.spawn(async move |_, cx| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::app::views::grid::runtime_for_views().spawn(async move {
                let result = crate::application::ddl::execute_request(&services.connections, &connection, &request, &sql, true).await;
                let _ = tx.send(result);
            });
            let Ok(result) = rx.await else { return };
            cx.update(|cx| {
                let Some(pane) = pane.upgrade() else { return };
                pane.update(cx, |this, cx| {
                    if this.epoch != epoch {
                        return;
                    }
                    this.executing = false;
                    let verb = this.session.verb;
                    this.session.plan = None;
                    let target = (changed_connection, database, kind, name);
                    match result {
                        Ok(receipt) => {
                            let decision = super::after_execute(verb, Some(&receipt));
                            this.session.receipt = Some(receipt);
                            // 结局自带的文案直接写进提示，读回真实状态时不会被冲掉。
                            if let Some(key) = decision.outcome.note_key() {
                                this.session.note = i18n::text(this.session.locale, key);
                            }
                            this.settle_after_execute(decision, target, cx);
                        }
                        Err(message) => {
                            // 执行链直接报错：一份输入都不丢，原始错误原样留在提示里。
                            let decision = super::after_execute(verb, None);
                            this.session.receipt = None;
                            this.session.note = message;
                            this.settle_after_execute(decision, target, cx);
                        }
                    }
                    cx.notify();
                });
            });
        })
        .detach();
    }

    /// 执行后收尾：留草稿、收窗口、读回真实定义，只在这里落地。
    ///
    /// 决策来自 after_execute（纯函数，单测覆盖）：草稿不该留就意味着窗口收起，
    /// 其余情况一律保留表单与回执，只是再读一次服务器现状。
    fn settle_after_execute(&mut self, decision: super::AfterExecute, target: (String, String, ObjectKind, String), cx: &mut Context<Self>) {
        if decision.changed {
            self.changed = Some(target);
        }
        if !decision.keep_draft {
            self.close();
            return;
        }
        if decision.refresh_detail && !self.session.name.trim().is_empty() {
            self.refresh_detail(cx);
        }
    }

    // ---------------------------------------------------------------- 渲染

    fn heading(&self, entity: &Entity<Self>, p: &Palette) -> AnyElement {
        let locale = self.session.locale;
        let title = SharedString::from(i18n::text(locale, title_key(self.session.kind)));
        let close_owner = entity.clone();
        let mut head = div()
            .flex()
            .items_center()
            .gap(px(Space::SM))
            .pb(px(Space::SM))
            .border_b_1()
            .border_color(p.line_soft)
            .child(kit::icon(IconName::Code, kit::IconSize::CONTROL, p.accent))
            .child(kit::title(title, TypeScale::LG, p));
        if !self.session.database.is_empty() {
            head = head.child(kit::pill(SharedString::from(self.session.database.clone()), p.text_muted, p.sunken));
        }
        if self.session.is_edit() {
            let metrics = kit::Metrics::compact();
            head = head.child(kit::chip(
                SharedString::from(i18n::text(locale, "ui.structure.existing")),
                kit::ChipTone::Neutral,
                &metrics,
                p,
            ));
        }
        head.child(div().flex_1().min_w_0())
            .child(
                kit::icon_button(
                    "object-editor-header-close",
                    IconName::X,
                    SharedString::from(i18n::text(locale, "common.close")),
                    move |_, _, cx| {
                        close_owner.update(cx, |pane, cx| {
                            pane.close();
                            cx.notify();
                        });
                    },
                )
                .xsmall(),
            )
            .into_any_element()
    }

    fn kind_choices(&self, entity: &Entity<Self>, p: &Palette, metrics: &kit::Metrics, locale: Locale) -> AnyElement {
        let items = super::EDITABLE_KINDS
            .iter()
            .map(|&kind| {
                let owner = entity.clone();
                let selected = self.session.kind == kind;
                let label = SharedString::from(i18n::text(locale, title_key(kind)));
                let id = SharedString::from(format!("object-editor-kind-{}", kind_name(kind)));
                let click = kit::click(move |_, _, cx| {
                    owner.update(cx, |pane, cx| pane.choose_kind(kind, cx));
                });
                (id, label, selected, click)
            })
            .collect();
        kit::segmented_sized(items, p, metrics.control_h)
    }

    fn field(&mut self, key: &'static str, label: SharedString, placeholder: &'static str, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let value = self.session.field(key);
        let p = Palette::of(cx);
        let state = self.fields.bind(key, &value, FieldSpec { placeholder, masked: false }, window, cx, Self::on_field);
        div()
            .flex()
            .flex_col()
            .gap(px(Space::XXS))
            .child(kit::muted(label, &p))
            .child(kit::text_input(SharedString::from(format!("object-editor-field-{key}")), &state, placeholder.to_owned()).small())
            .into_any_element()
    }

    fn body_field(&mut self, label: SharedString, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let value = self.session.body.clone();
        let editor = self.ensure_body_editor(window, cx);
        // 模型改过主体（读回、换种类、换目标）就推给编辑器；模型没变时不碰输入框，免得冲掉刚打的字。
        kit::sync_editor(&editor, &mut self.body_sync, &value, window, cx);
        div()
            .flex()
            .flex_col()
            .gap(px(Space::XXS))
            .child(kit::muted(label, &p))
            .child(
                kit::sunken(&p)
                    .id("object-editor-body")
                    .test_support()
                    .h(px(Space::XXL * 8.))
                    .p(px(Space::SM))
                    .child(Editor::new(&editor).h(relative(1.)).bordered(false).aria_label(SharedString::from("body"))),
            )
            .into_any_element()
    }

    /// 开关只改草稿：任何一次点击都作废上一次预览，语句仍由计划层重新生成。
    fn apply_toggle(&mut self, toggle: EditorToggle, cx: &mut Context<Self>) {
        match toggle {
            EditorToggle::OneTime => self.session.one_time = true,
            EditorToggle::Recurring => self.session.one_time = false,
            EditorToggle::Enabled => self.session.enabled = !self.session.enabled,
            EditorToggle::Preserve => self.session.preserve = !self.session.preserve,
            EditorToggle::Before => self.session.timing_after = false,
            EditorToggle::After => self.session.timing_after = true,
            EditorToggle::Event(event) => self.session.event_kind = event,
        }
        self.session.cancel();
        cx.notify();
    }

    fn toggle(&self, id: &'static str, on: bool, label: SharedString, entity: &Entity<Self>, toggle: EditorToggle) -> AnyElement {
        let owner = entity.clone();
        let click = move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut gpui_kit::App| {
            owner.update(cx, |pane, cx| pane.apply_toggle(toggle, cx));
        };
        if on {
            kit::primary_button(id, None, label, click).small().into_any_element()
        } else {
            kit::subtle_button(id, None, label, click).small().into_any_element()
        }
    }

    fn event_form(&mut self, entity: &Entity<Self>, p: &Palette, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let locale = self.session.locale;
        let mut form = div().flex().flex_col().gap(px(Space::MD));
        form = form.child(self.field("name", SharedString::from(i18n::text(locale, "catalog.overview.name")), "e.g. purge_sessions", window, cx));
        // 单次 / 周期：只决定下面出现哪几个输入框，语句仍由计划层生成。
        let one_time = self.session.one_time;
        let metrics = kit::Metrics::of(cx);
        let one_time_owner = entity.clone();
        let recurring_owner = entity.clone();
        let schedule = kit::segmented_sized(
            vec![
                (
                    SharedString::from("object-editor-one-time"),
                    SharedString::from("AT"),
                    one_time,
                    kit::click(move |_, _, cx| one_time_owner.update(cx, |pane, cx| pane.apply_toggle(EditorToggle::OneTime, cx))),
                ),
                (
                    SharedString::from("object-editor-recurring"),
                    SharedString::from("EVERY"),
                    !one_time,
                    kit::click(move |_, _, cx| recurring_owner.update(cx, |pane, cx| pane.apply_toggle(EditorToggle::Recurring, cx))),
                ),
            ],
            p,
            metrics.control_h,
        );
        form = form.child(schedule);
        if one_time {
            form = form.child(self.field("at", SharedString::from("AT"), "2026-01-01 00:00:00", window, cx));
        } else {
            let mut recurring = div().flex().items_end().gap(px(Space::SM));
            recurring = recurring.child(self.field("every", SharedString::from("EVERY"), "1", window, cx));
            recurring = recurring.child(self.field("unit", SharedString::from("UNIT"), "DAY", window, cx));
            form = form.child(recurring);
            let mut window_row = div().flex().items_end().gap(px(Space::SM));
            window_row = window_row.child(self.field("starts", SharedString::from("STARTS"), "2026-01-01 00:00:00", window, cx));
            window_row = window_row.child(self.field("ends", SharedString::from("ENDS"), "2030-01-01 00:00:00", window, cx));
            form = form.child(window_row);
        }
        let enabled = self.session.enabled;
        let preserve = self.session.preserve;
        let mut flags = div().flex().items_center().gap(px(Space::SM));
        flags = flags.child(self.toggle(
            "object-editor-enabled",
            enabled,
            SharedString::from(i18n::text(locale, "admin.action.event")),
            entity,
            EditorToggle::Enabled,
        ));
        flags = flags.child(self.toggle(
            "object-editor-preserve",
            preserve,
            SharedString::from("ON COMPLETION PRESERVE"),
            entity,
            EditorToggle::Preserve,
        ));
        form = form.child(flags);
        form = form.child(self.field("comment", SharedString::from(i18n::text(locale, "structure.field.comment")), "comment", window, cx));
        form = form.child(self.body_field(SharedString::from("DO"), window, cx));
        if self.session.detail.as_ref().and_then(|detail| detail.scheduler_running()) == Some(false) {
            form = form.child(
                div()
                    .id("object-editor-scheduler-off")
                    .test_support()
                    .p(px(Space::MD))
                    .rounded(px(Radius::PANEL))
                    .bg(p.warn_soft)
                    .text_color(p.warn)
                    .text_size(px(TypeScale::SM))
                    .child(SharedString::from(i18n::text(locale, "objectEditor.schedulerOff"))),
            );
        }
        form.into_any_element()
    }

    fn routine_form(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let locale = self.session.locale;
        let function = self.session.kind == ObjectKind::Function;
        let mut form = div().flex().flex_col().gap(px(Space::MD));
        form = form.child(self.field("name", SharedString::from(i18n::text(locale, "catalog.overview.name")), "e.g. total_orders", window, cx));
        form = form.child(self.field(
            "params",
            SharedString::from(i18n::text(locale, "structure.type.params")),
            "IN name VARCHAR(64)",
            window,
            cx,
        ));
        if function {
            form = form.child(self.field("returns", SharedString::from("RETURNS"), "BIGINT", window, cx));
        }
        form = form.child(self.body_field(SharedString::from(i18n::text(locale, "structure.section.definition")), window, cx));
        form.into_any_element()
    }

    fn trigger_form(&mut self, entity: &Entity<Self>, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let locale = self.session.locale;
        let mut form = div().flex().flex_col().gap(px(Space::MD));
        form = form.child(self.field("name", SharedString::from(i18n::text(locale, "catalog.overview.name")), "e.g. audit_orders", window, cx));
        form = form.child(self.field(
            "table",
            SharedString::from(i18n::text(locale, "compare.table.namePlaceholder")),
            "orders",
            window,
            cx,
        ));
        let p = Palette::of(cx);
        let metrics = kit::Metrics::of(cx);
        let before = !self.session.timing_after;
        let before_owner = entity.clone();
        let after_owner = entity.clone();
        let timing = kit::segmented_sized(
            vec![
                (
                    SharedString::from("object-editor-before"),
                    SharedString::from(TriggerTiming::Before.keyword()),
                    before,
                    kit::click(move |_, _, cx| before_owner.update(cx, |pane, cx| pane.apply_toggle(EditorToggle::Before, cx))),
                ),
                (
                    SharedString::from("object-editor-after"),
                    SharedString::from(TriggerTiming::After.keyword()),
                    !before,
                    kit::click(move |_, _, cx| after_owner.update(cx, |pane, cx| pane.apply_toggle(EditorToggle::After, cx))),
                ),
            ],
            &p,
            metrics.control_h,
        );
        let event_items = [TriggerEvent::Insert, TriggerEvent::Update, TriggerEvent::Delete]
            .into_iter()
            .map(|event| {
                let on = self.session.event_kind == event;
                let id = match event {
                    TriggerEvent::Insert => "object-editor-insert",
                    TriggerEvent::Update => "object-editor-update",
                    TriggerEvent::Delete => "object-editor-delete",
                };
                let owner = entity.clone();
                (
                    SharedString::from(id),
                    SharedString::from(event.keyword()),
                    on,
                    kit::click(move |_, _, cx| owner.update(cx, |pane, cx| pane.apply_toggle(EditorToggle::Event(event), cx))),
                )
            })
            .collect();
        let events = kit::segmented_sized(event_items, &p, metrics.control_h);
        form = form.child(div().flex().flex_wrap().items_center().gap(px(Space::MD)).child(timing).child(events));
        form = form.child(self.body_field(SharedString::from("FOR EACH ROW"), window, cx));
        form.into_any_element()
    }

    fn actions(&self, entity: &Entity<Self>, locale: Locale) -> AnyElement {
        let mut row = div().flex().items_center().gap(px(Space::SM));
        let owner = entity.clone();
        row = row.child(kit::primary_button(
            "object-editor-preview",
            Some(IconName::Eye),
            SharedString::from(i18n::text(locale, "structure.action.preview")),
            move |_, _, cx| owner.update(cx, |pane, cx| pane.preview_with(EditorVerb::Save, cx)),
        ));
        if self.session.is_edit() {
            let owner = entity.clone();
            row = row.child(kit::danger_soft_button(
                "object-editor-drop",
                Some(IconName::Trash),
                SharedString::from(i18n::text(locale, "structure.section.danger")),
                move |_, _, cx| owner.update(cx, |pane, cx| pane.preview_with(EditorVerb::Drop, cx)),
            ));
        }
        if self.session.kind == ObjectKind::Event && self.session.is_edit() {
            // 启停是独立动作：走现成的 SetEventEnabled 计划，不重写整个事件定义。
            let enable = !self.session.enabled;
            let owner = entity.clone();
            let text = if enable { "ENABLE" } else { "DISABLE" };
            row = row.child(kit::secondary_button(
                "object-editor-enable",
                None,
                SharedString::from(text),
                move |_, _, cx| {
                    owner.update(cx, |pane, cx| {
                        let verb = if enable { EditorVerb::Enable } else { EditorVerb::Disable };
                        pane.preview_with(verb, cx);
                    })
                },
            ));
        }
        let owner = entity.clone();
        row = row.child(kit::subtle_button(
            "object-editor-close",
            None,
            SharedString::from(i18n::text(locale, "common.close")),
            move |_, _, cx| {
                owner.update(cx, |pane, cx| {
                    pane.close();
                    cx.notify();
                })
            },
        ));
        row.into_any_element()
    }

    fn risk_rows(&self, plan: &crate::ddl::Plan, p: &Palette, locale: Locale) -> Option<AnyElement> {
        let mut rows = div().flex().flex_col().gap(px(Space::XS));
        let mut any = false;
        // 替换（DROP + CREATE）在计划里是重建：先把对象可能已经不在的风险说清楚。
        if plan.rebuild || self.session.verb == EditorVerb::Drop {
            rows = rows.child(
                div()
                    .id("object-editor-replace-risk")
                    .test_support()
                    .p(px(Space::MD))
                    .rounded(px(Radius::PANEL))
                    .bg(p.danger_soft)
                    .text_color(p.danger)
                    .text_size(px(TypeScale::SM))
                    .child(SharedString::from(i18n::text(locale, "objectEditor.replaceRisk"))),
            );
            any = true;
        }
        for risk in &plan.risks {
            let (fg, bg) = if risk.level == "danger" {
                (p.danger, p.danger_soft)
            } else if risk.level == "warn" {
                (p.warn, p.warn_soft)
            } else {
                (p.text_muted, p.hover)
            };
            rows = rows.child(
                div()
                    .px(px(Space::SM))
                    .py(px(Space::XXS))
                    .rounded(px(Radius::PANEL))
                    .bg(bg)
                    .text_color(fg)
                    .text_size(px(TypeScale::SM))
                    .child(SharedString::from(risk.text.clone())),
            );
            any = true;
        }
        any.then(|| rows.into_any_element())
    }

    fn preview_panel(&self, entity: &Entity<Self>, p: &Palette, locale: Locale) -> AnyElement {
        let mut panel = kit::card(p).p(px(Space::MD)).flex().flex_col().gap(px(Space::SM));
        if let Some(plan) = self.session.plan.as_ref() {
            let mono = SharedString::from("monospace");
            for step in &plan.steps {
                panel = panel.child(
                    kit::sunken(p)
                        .id(SharedString::from(format!("object-editor-sql-{}", step.label)))
                        .max_h(px(Space::XXL * 8.))
                        .scroll_area_y()
                        .p(px(Space::SM))
                        .font_family(mono.clone())
                        .text_size(px(TypeScale::SM))
                        .child(SharedString::from(step.sql.clone())),
                );
            }
            if !plan.rule.is_empty() {
                panel = panel.child(kit::muted(SharedString::from(plan.rule.clone()), p));
            }
            if let Some(risks) = self.risk_rows(plan, p, locale) {
                panel = panel.child(risks);
            }
            if !plan.blocked.is_empty() {
                panel = panel.child(
                    div()
                        .id("object-editor-blocked")
                        .test_support()
                        .p(px(Space::MD))
                        .rounded(px(Radius::PANEL))
                        .bg(p.danger_soft)
                        .text_color(p.danger)
                        .text_size(px(TypeScale::SM))
                        .child(SharedString::from(plan.blocked.join(" "))),
                );
            }
            let owner = entity.clone();
            let cancel_owner = entity.clone();
            let blocked = plan.blocked.is_empty();
            panel = panel.child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(Space::SM))
                    .child(kit::secondary_button(
                        "object-editor-cancel",
                        None,
                        SharedString::from(i18n::text(locale, "structure.action.cancel")),
                        move |_, _, cx| {
                            cancel_owner.update(cx, |pane, cx| {
                                pane.session.cancel();
                                cx.notify();
                            })
                        },
                    ))
                    .when(blocked, |this| {
                        this.child(kit::primary_button(
                            "object-editor-confirm",
                            Some(IconName::Check),
                            SharedString::from(i18n::text(locale, "structure.action.confirm")),
                            move |_, _, cx| owner.update(cx, |pane, cx| pane.confirm(cx)),
                        ))
                    }),
            );
        }
        panel.into_any_element()
    }

    /// 逐步回执：与结构页、管理页、危险操作层同一个组件，前缀保持 object-editor。
    fn receipt_panel(&self, p: &Palette, locale: Locale) -> Option<AnyElement> {
        let receipt = self.session.receipt.as_ref()?;
        Some(crate::app::views::structure::receipt::panel("object-editor", receipt, p, locale))
    }

    fn panel(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let metrics = kit::Metrics::of(cx);
        let locale = self.session.locale;
        let entity = cx.entity().clone();
        let title = SharedString::from(i18n::text(locale, title_key(self.session.kind)));
        let mut body = div().flex().flex_col().gap(px(metrics.section_gap()));
        if self.session.is_edit() {
            // 编辑时对象名是身份，不在这里改：改名要新建再删旧的，不偷偷替换。
            body = body.child(kit::muted(SharedString::from(self.session.name.clone()), &p));
        } else {
            body = body.child(self.kind_choices(&entity, &p, &metrics, locale));
        }
        if self.loading {
            body = body.child(kit::inline_loading(i18n::text(locale, "ui.structure.loading"), &metrics, &p));
        }
        if !self.session.detail_error.is_empty() {
            body = body.child(
                div()
                    .id("object-editor-detail-error")
                    .test_support()
                    .p(px(Space::MD))
                    .rounded(px(Radius::PANEL))
                    .bg(p.danger_soft)
                    .text_color(p.danger)
                    .text_size(px(TypeScale::SM))
                    .child(SharedString::from(self.session.detail_error.clone())),
            );
        }
        body = body.child(match self.session.kind {
            ObjectKind::Event => self.event_form(&entity, &p, window, cx),
            ObjectKind::Function | ObjectKind::Procedure => self.routine_form(window, cx),
            ObjectKind::Trigger => self.trigger_form(&entity, window, cx),
            _ => div().into_any_element(),
        });
        body = body.child(self.actions(&entity, locale));
        if self.previewing {
            body = body.child(kit::inline_loading(i18n::text(locale, "structure.action.preview"), &metrics, &p));
        }
        if !self.session.note.is_empty() {
            body = body.child(
                kit::sunken(&p)
                    .id("object-editor-note")
                    .test_support()
                    .p(px(Space::MD))
                    .text_size(px(TypeScale::SM))
                    .text_color(p.text_muted)
                    .child(SharedString::from(self.session.note.clone())),
            );
        }
        if self.session.plan.is_some() {
            body = body.child(self.preview_panel(&entity, &p, locale));
        }
        if let Some(receipt) = self.receipt_panel(&p, locale) {
            body = body.child(receipt);
        }
        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui_kit::hsla(0., 0., 0., if p.dark { 0.45 } else { 0.25 }))
            .child(
                div()
                    .id("object-editor")
                    .role(gpui_kit::Role::Dialog)
                    .aria_label(title)
                    .occlude()
                    .w(px(Space::XXL * 30.))
                    .max_w(relative(0.92))
                    .max_h(relative(0.88))
                    .p(px(metrics.card_pad()))
                    .flex()
                    .flex_col()
                    .gap(px(Space::MD))
                    .bg(p.raised)
                    .border_1()
                    .border_color(if p.dark { p.line_soft } else { p.line })
                    .rounded(px(Radius::PANEL))
                    .shadow_lg()
                    .child(self.heading(&entity, &p))
                    .child(div().id("object-editor-scroll").flex_1().min_h(relative(0.)).scroll_area_y().child(body)),
            )
            .into_any_element()
    }
}

impl Render for ObjectEditorPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.open {
            return div().into_any_element();
        }
        self.panel(window, cx)
    }
}

/// 对象种类在元素 id 里的稳定写法（不随界面语言变）。
pub fn kind_name(kind: ObjectKind) -> &'static str {
    match kind {
        ObjectKind::Event => "event",
        ObjectKind::Function => "function",
        ObjectKind::Procedure => "procedure",
        ObjectKind::Trigger => "trigger",
        _ => "object",
    }
}
