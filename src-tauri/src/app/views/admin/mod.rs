//! 数据库管理。SQLite / PostgreSQL / MySQL / MariaDB 的动作仍经 DDL 预览。

use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, Entity, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, TestSupportExt as _, Window, div, px, relative,
};

use crate::ui::kit::{self, FieldSpec, Fields, Palette, Radius, TypeScale};

use crate::application::AppServices;
use crate::app::views::grid::{operation_banner, GridTarget};
use crate::ddl::english::localize;
use crate::ddl::{preview_plan, Action, Plan, Request, Snapshot};
use crate::i18n::{self, Locale};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdminSections {
    pub pragma: bool,
    pub attached: bool,
    pub maintenance: bool,
    pub extensions: bool,
    pub sequences: bool,
    pub events: bool,
    pub partitions: bool,
    pub enums: bool,
    pub domains: bool,
}

pub fn sections_for(engine: &str) -> AdminSections {
    let sqlite = engine == "sqlite";
    let postgres = engine == "postgres";
    let mysql = engine == "mysql" || engine == "mariadb";
    AdminSections {
        pragma: sqlite,
        attached: sqlite,
        maintenance: sqlite,
        extensions: postgres,
        sequences: postgres,
        events: mysql,
        partitions: mysql,
        enums: postgres,
        domains: postgres,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdminOp {
    Vacuum,
    Reindex { target: Option<String> },
    SetForeignKeys { enabled: bool },
    SetPragma { pragma: String, value: String },
    Attach { path: String, name: String },
    SetEvent { event: String, enabled: bool },
    AlterSequence { sequence: String, start: Option<i64>, increment: Option<i64>, min_value: Option<i64>, max_value: Option<i64>, cycle: Option<bool> },
    AddEnum { enum_name: String, value: String },
    AlterDomain { domain: String, default_sql: String, not_null: Option<bool> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SequenceChoice {
    pub name: String,
    pub start: i64,
    pub increment: i64,
    pub min_value: i64,
    pub max_value: i64,
    pub cycle: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventChoice {
    pub name: String,
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SectionFault {
    pub title_key: &'static str,
    pub message: String,
}

/// 管理页提示。键和原文分开存，显示时再按当前语言给出。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Note {
    #[default]
    Empty,
    Key(&'static str),
    Raw(Vec<String>),
}

impl Note {
    fn raw(message: String) -> Self {
        Note::Raw(vec![message])
    }

    pub fn text(&self, locale: Locale) -> String {
        match self {
            Note::Empty => String::new(),
            Note::Key(key) => i18n::text(locale, key),
            Note::Raw(parts) => parts.iter().map(|part| localize(part, locale)).collect::<Vec<_>>().join(" "),
        }
    }

    pub fn is_empty(&self) -> bool {
        match self {
            Note::Empty => true,
            Note::Key(_) => false,
            Note::Raw(parts) => parts.iter().all(|part| part.is_empty()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdminInventory {
    pub connected: bool,
    /// 清单读取已经结束（读到的可能仍是离线结果）；读取途中为 false，页面据此区分骨架与增量指示。
    pub probed: bool,
    /// 读取来源（引擎 · 库）。
    pub source: String,
    /// 来源后面追加的一句说明（i18n 键），按当前语言渲染。
    pub note_key: Option<&'static str>,
    pub sequences: Vec<String>,
    pub sequence_choices: Vec<SequenceChoice>,
    pub events: Vec<String>,
    pub event_choices: Vec<EventChoice>,
    pub extensions: Vec<String>,
    pub partitions: Vec<String>,
    pub attached: Vec<String>,
    pub enums: Vec<String>,
    pub domains: Vec<String>,
    pub journal_mode: String,
    pub user_version: String,
    pub foreign_keys_on: Option<bool>,
    pub faults: Vec<SectionFault>,
}

impl AdminInventory {
    /// 来源与说明按界面语言拼成一行。
    pub fn source_text(&self, locale: Locale) -> String {
        match self.note_key {
            Some(key) => format!("{} · {}", self.source, i18n::text(locale, key)),
            None => self.source.clone(),
        }
    }

    pub fn offline(target: &GridTarget) -> Self {
        Self {
            connected: false,
            probed: false,
            source: format!("{} · {}", target.engine, target.database),
            note_key: Some("admin.preview.unconnected"),
            sequences: Vec::new(),
            sequence_choices: Vec::new(),
            events: Vec::new(),
            event_choices: Vec::new(),
            extensions: Vec::new(),
            partitions: Vec::new(),
            attached: Vec::new(),
            enums: Vec::new(),
            domains: Vec::new(),
            journal_mode: String::new(),
            user_version: String::new(),
            foreign_keys_on: None,
            faults: Vec::new(),
        }
    }
}

pub struct AdminSession {
    pub target: GridTarget,
    pub operation: AdminOp,
    pub reindex_target: String,
    pub attach_path: String,
    pub attach_name: String,
    pub foreign_keys: bool,
    pub pragma_name: String,
    pub pragma_value: String,
    pub sequence_name: String,
    pub sequence_step: String,
    pub sequence_start: String,
    pub sequence_min: String,
    pub sequence_max: String,
    pub sequence_cycle: bool,
    pub sequence_cycle_set: bool,
    pub event_name: String,
    pub event_enabled: bool,
    pub enum_name: String,
    pub enum_value: String,
    pub domain_name: String,
    pub domain_default: String,
    pub domain_not_null: Option<bool>,
    pub inventory: AdminInventory,
    preview: Option<Plan>,
    live: bool,
    open: bool,
    pub locale: Locale,
    pub note: Note,
}

impl AdminSession {
    pub fn new(target: GridTarget) -> Self {
        let inventory = AdminInventory::offline(&target);
        Self {
            target,
            operation: AdminOp::Vacuum,
            reindex_target: String::new(),
            attach_path: String::new(),
            attach_name: String::new(),
            foreign_keys: true,
            pragma_name: "user_version".to_owned(),
            pragma_value: String::new(),
            sequence_name: String::new(),
            sequence_step: String::new(),
            sequence_start: String::new(),
            sequence_min: String::new(),
            sequence_max: String::new(),
            sequence_cycle: false,
            sequence_cycle_set: false,
            event_name: String::new(),
            event_enabled: false,
            enum_name: String::new(),
            enum_value: String::new(),
            domain_name: String::new(),
            domain_default: String::new(),
            domain_not_null: None,
            inventory,
            preview: None,
            live: false,
            open: false,
            locale: Locale::ZhCn,
            note: Note::Empty,
        }
    }

    pub fn note_text(&self) -> String {
        self.note.text(self.locale)
    }

    pub fn apply_draft(&mut self, kind: &str) {
        let fallback = self.operation.clone();
        self.operation = match kind {
            "vacuum" => AdminOp::Vacuum,
            "reindex" => AdminOp::Reindex { target: if self.reindex_target.is_empty() { None } else { Some(self.reindex_target.clone()) } },
            "foreign" => AdminOp::SetForeignKeys { enabled: self.foreign_keys },
            "pragma" => AdminOp::SetPragma { pragma: self.pragma_name.clone(), value: self.pragma_value.clone() },
            "attach" => AdminOp::Attach { path: self.attach_path.clone(), name: self.attach_name.clone() },
            "sequence" => AdminOp::AlterSequence {
                sequence: self.sequence_name.clone(),
                start: parse_i64(&self.sequence_start),
                increment: parse_i64(&self.sequence_step),
                min_value: parse_i64(&self.sequence_min),
                max_value: parse_i64(&self.sequence_max),
                cycle: self.sequence_cycle_set.then_some(self.sequence_cycle),
            },
            "event" => AdminOp::SetEvent { event: self.event_name.clone(), enabled: self.event_enabled },
            "enum" => AdminOp::AddEnum { enum_name: self.enum_name.clone(), value: self.enum_value.clone() },
            "domain" => AdminOp::AlterDomain { domain: self.domain_name.clone(), default_sql: self.domain_default.clone(), not_null: self.domain_not_null },
            _ => fallback,
        };
        self.cancel();
    }

    pub fn sections(&self) -> AdminSections {
        sections_for(&self.target.engine)
    }

    pub fn banner(&self) -> String {
        operation_banner(&self.target)
    }

    pub fn request(&self) -> Result<Request, String> {
        crate::application::ddl::request(self.target.database.clone(), self.target.schema.clone(), &self.target.engine, self.action())
    }

    pub fn action(&self) -> Action {
        match &self.operation {
            AdminOp::Vacuum => Action::Vacuum,
            AdminOp::Reindex { target } => Action::Reindex { target: target.clone() },
            AdminOp::SetForeignKeys { enabled } => Action::SetForeignKeys { enabled: *enabled },
            AdminOp::SetPragma { pragma, value } => Action::SetPragma { pragma: pragma.clone(), value: value.clone() },
            AdminOp::Attach { path, name } => Action::AttachDatabase { path: path.clone(), name: name.clone() },
            AdminOp::SetEvent { event, enabled } => Action::SetEventEnabled { event: event.clone(), enabled: *enabled },
            AdminOp::AlterSequence { sequence, start, increment, min_value, max_value, cycle } => Action::AlterSequence {
                sequence: sequence.clone(),
                start: *start,
                increment: *increment,
                min_value: *min_value,
                max_value: *max_value,
                cycle: *cycle,
            },
            AdminOp::AddEnum { enum_name, value } => Action::AddEnumValue { enum_name: enum_name.clone(), value: value.clone(), before: None },
            AdminOp::AlterDomain { domain, default_sql, not_null } => Action::AlterDomain {
                domain: domain.clone(),
                default_sql: Some(default_sql.clone()),
                not_null: *not_null,
            },
        }
    }

    pub fn arm_live(&mut self, plan: Plan) {
        self.preview = Some(plan);
        self.live = true;
        self.open = true;
        self.note = Note::Empty;
    }

    pub fn arm_fixture(&mut self, snapshot: &Snapshot) {
        let plan = preview_plan(snapshot, &self.action());
        self.preview = Some(plan);
        self.live = false;
        self.open = true;
        self.note = Note::Key("admin.preview.unconnected");
    }

    pub fn preview_open(&self) -> bool {
        self.open
    }

    pub fn preview_sql(&self) -> Vec<String> {
        self.preview.as_ref().map(|plan| plan.steps.iter().map(|step| step.sql.clone()).collect()).unwrap_or_default()
    }

    pub fn preview_risks(&self) -> Vec<(String, String)> {
        self.preview
            .as_ref()
            .map(|plan| plan.risks.iter().map(|risk| (risk.level.clone(), localize(&risk.text, self.locale))).collect())
            .unwrap_or_default()
    }

    pub fn preview_rule(&self) -> String {
        self.preview.as_ref().map(|plan| localize(&plan.rule, self.locale)).unwrap_or_default()
    }

    pub fn preview_target(&self) -> String {
        self.preview.as_ref().map(|plan| plan.target.clone()).unwrap_or_default()
    }

    pub fn blocked(&self) -> Vec<String> {
        self.preview.as_ref().map(|plan| plan.blocked.iter().map(|text| localize(text, self.locale)).collect()).unwrap_or_default()
    }

    /// 确认层按危险色呈现的条件：预览计划里带 danger 级风险。
    /// 只影响配色与提示措辞，执行仍要预览后确认。
    pub fn preview_dangerous(&self) -> bool {
        self.preview.as_ref().is_some_and(|plan| plan.risks.iter().any(|risk| risk.level == "danger"))
    }

    pub fn cancel(&mut self) {
        self.open = false;
    }

    pub fn confirm_sql(&self) -> Result<Vec<String>, String> {
        self.confirm_steps().map_err(|note| note.text(self.locale))
    }

    fn confirm_steps(&self) -> Result<Vec<String>, Note> {
        if !self.open {
            return Err(Note::Key("structure.preview.need"));
        }
        if !self.live {
            return Err(Note::Key("admin.preview.unconnected"));
        }
        let plan = self.preview.as_ref().ok_or(Note::Key("structure.preview.need"))?;
        if !plan.blocked.is_empty() {
            return Err(Note::Raw(plan.blocked.clone()));
        }
        Ok(plan.steps.iter().map(|step| step.sql.clone()).collect())
    }
}

fn parse_i64(text: &str) -> Option<i64> {
    let text = text.trim();
    if text.is_empty() { None } else { text.parse().ok() }
}

pub struct AdminPane {
    pub session: AdminSession,
    services: Arc<AppServices>,
    fields: Fields<&'static str>,
    /// 当前绑定到的工作项 id；关闭后重开会拿到新 id，必须重置草稿。
    item_id: String,
    probing: bool,
    probe_gen: u64,
    link_open: Option<bool>,
    /// 离线时“立即连接”交给外壳。
    pub connect_request: Option<String>,
}

impl AdminPane {
    pub fn new(services: Arc<AppServices>) -> Self {
        Self {
            session: AdminSession::new(GridTarget::default()),
            services,
            fields: Fields::default(),
            item_id: String::new(),
            probing: false,
            probe_gen: 0,
            link_open: None,
            connect_request: None,
        }
    }

    /// 兼容调用：不区分工作项实例，只按目标绑定。
    pub fn bind(&mut self, target: GridTarget) {
        self.bind_item(target, "");
    }

    /// 同一工作项切工具不丢草稿；关闭后重开（新 item id）或换目标才重建。
    pub fn bind_item(&mut self, target: GridTarget, item: &str) {
        if self.session.target == target && self.item_id == item {
            return;
        }
        self.item_id = item.to_owned();
        let locale = self.session.locale;
        self.session = AdminSession::new(target);
        self.session.locale = locale;
        self.fields = Fields::default();
        self.probing = false;
        self.probe_gen = self.probe_gen.wrapping_add(1);
        self.link_open = None;
        self.connect_request = None;
    }

    pub fn take_connect_request(&mut self) -> Option<String> {
        self.connect_request.take()
    }

    /// 连接建立或断开后清掉探测闩，下一帧重新读取清单。
    pub fn note_connection(&mut self, connected: bool, cx: &mut Context<Self>) {
        if self.link_open == Some(connected) && (self.session.inventory.probed || self.probing) {
            return;
        }
        self.link_open = Some(connected);
        self.refresh(cx);
    }

    /// 重新读取清单：不清"已读过"，旧清单留着，页面顶部给一行增量指示，读完再整体替换；
    /// 只有换目标/换工作项（`bind_item`）才回到首屏骨架。
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.probing = false;
        self.request_inventory(cx);
    }

    fn request_inventory(&mut self, cx: &mut Context<Self>) {
        if self.session.target.connection_id.is_empty() || self.probing {
            return;
        }
        self.probing = true;
        self.probe_gen = self.probe_gen.wrapping_add(1);
        let generation = self.probe_gen;
        self.refresh_inventory(generation, cx);
    }

    fn pick_sequence(&mut self, index: usize) {
        let Some(choice) = self.session.inventory.sequence_choices.get(index).cloned() else {
            return;
        };
        self.session.sequence_name = choice.name;
        self.session.sequence_start = choice.start.to_string();
        self.session.sequence_step = choice.increment.to_string();
        self.session.sequence_min = choice.min_value.to_string();
        self.session.sequence_max = choice.max_value.to_string();
        self.session.sequence_cycle = choice.cycle;
        self.session.sequence_cycle_set = true;
        self.session.apply_draft("sequence");
    }

    fn toggle_event(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(choice) = self.session.inventory.event_choices.get(index).cloned() else {
            return;
        };
        self.session.event_name = choice.name;
        self.session.event_enabled = !choice.enabled;
        self.session.apply_draft("event");
        self.preview(cx);
    }

    pub fn type_into(&mut self, field: &str, text: &str) {
        let slot = match field {
            "reindex" => &mut self.session.reindex_target,
            "attach_path" => &mut self.session.attach_path,
            "attach_name" => &mut self.session.attach_name,
            "pragma_value" => &mut self.session.pragma_value,
            "sequence" => &mut self.session.sequence_name,
            "sequence_step" => &mut self.session.sequence_step,
            "sequence_start" => &mut self.session.sequence_start,
            "sequence_min" => &mut self.session.sequence_min,
            "sequence_max" => &mut self.session.sequence_max,
            "event" => &mut self.session.event_name,
            "enum" => &mut self.session.enum_name,
            "enum_value" => &mut self.session.enum_value,
            "domain" => &mut self.session.domain_name,
            "domain_default" => &mut self.session.domain_default,
            _ => return,
        };
        if text == "backspace" {
            slot.pop();
        } else {
            slot.push_str(text);
        }
        self.session.cancel();
    }

    pub fn choose(&mut self, kind: &str) {
        if kind == "foreign" {
            self.session.foreign_keys = !self.session.foreign_keys;
        }
        self.session.apply_draft(kind);
    }

    pub fn refresh_inventory(&mut self, generation: u64, cx: &mut Context<Self>) {
        let services = Arc::clone(&self.services);
        let target = self.session.target.clone();
        let expected = target.clone();
        let view = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::app::views::grid::runtime_for_views().spawn(async move {
                let inventory = read_inventory(&services.connections, &target).await;
                let _ = tx.send(inventory);
            });
            if let Ok(inventory) = rx.await {
                cx.update(|cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |this, cx| {
                            if this.probe_gen != generation || this.session.target != expected {
                                return;
                            }
                            this.probing = false;
                            this.link_open = Some(inventory.connected);
                            this.session.inventory = inventory;
                            if !this.session.inventory.connected {
                                this.session.note = match this.session.inventory.note_key {
                                    Some(key) => Note::Key(key),
                                    None => Note::Empty,
                                };
                            }
                            cx.notify();
                        });
                    }
                });
            }
        })
        .detach();
    }

    pub fn set_locale(&mut self, locale: Locale) {
        self.session.locale = locale;
    }

    pub fn preview(&mut self, cx: &mut Context<Self>) {
        let Ok(request) = self.session.request() else {
            return;
        };
        if !self.session.sections().allows(&self.session.operation) {
            self.session.note = Note::Key("admin.section.none");
            cx.notify();
            return;
        }
        let services = Arc::clone(&self.services);
        let target = self.session.target.clone();
        let connection = self.session.target.connection_id.clone();
        let view = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::app::views::grid::runtime_for_views().spawn(async move {
                let result = crate::application::ddl::preview_request(&services.connections, &connection, &request).await;
                let _ = tx.send(result);
            });
            if let Ok(result) = rx.await {
                cx.update(|cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |this, cx| {
                            if this.session.target != target {
                                this.session.note = Note::Key("admin.note.previewDropped");
                                cx.notify();
                                return;
                            }
                            match result {
                                Ok(plan) => this.session.arm_live(plan),
                                Err(message) => this.session.note = Note::raw(message),
                            }
                            cx.notify();
                        });
                    }
                });
            }
        })
        .detach();
    }

    pub fn confirm(&mut self, cx: &mut Context<Self>) {
        let sql = match self.session.confirm_steps() {
            Ok(sql) => sql,
            Err(note) => {
                self.session.note = note;
                cx.notify();
                return;
            }
        };
        let Ok(request) = self.session.request() else {
            return;
        };
        let services = Arc::clone(&self.services);
        let target = self.session.target.clone();
        let connection = self.session.target.connection_id.clone();
        let view = cx.entity().downgrade();
        let locale = self.session.locale;
        cx.spawn(async move |_, cx| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::app::views::grid::runtime_for_views().spawn(async move {
                let refused = crate::application::ddl::execute_request(&services.connections, &connection, &request, &sql, false).await;
                let done = if refused.is_err() {
                    crate::application::ddl::execute_request(&services.connections, &connection, &request, &sql, true).await
                } else {
                    Err(i18n::text(locale, "admin.note.unconfirmed"))
                };
                let _ = tx.send(done);
            });
            if let Ok(result) = rx.await {
                cx.update(|cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |this, cx| {
                            if this.session.target != target {
                                this.session.note = Note::Key("admin.note.executeDropped");
                                cx.notify();
                                return;
                            }
                            this.session.note = match result {
                                Ok(receipt) if receipt.blocked.is_empty() => {
                                    this.session.open = false;
                                    Note::Key("structure.preview.done")
                                }
                                Ok(receipt) => Note::Raw(receipt.blocked),
                                Err(message) => {
                                    let zh = i18n::text(Locale::ZhCn, "admin.note.unconfirmed");
                                    let en = i18n::text(Locale::En, "admin.note.unconfirmed");
                                    if message == zh || message == en { Note::Key("admin.note.unconfirmed") } else { Note::raw(message) }
                                }
                            };
                            cx.notify();
                        });
                    }
                });
            }
        })
        .detach();
    }
}

impl AdminSections {
    fn allows(self, operation: &AdminOp) -> bool {
        match operation {
            AdminOp::Vacuum | AdminOp::Reindex { .. } => self.maintenance,
            AdminOp::SetForeignKeys { .. } | AdminOp::SetPragma { .. } => self.pragma,
            AdminOp::Attach { .. } => self.attached,
            AdminOp::SetEvent { .. } => self.events,
            AdminOp::AlterSequence { .. } => self.sequences,
            AdminOp::AddEnum { .. } => self.enums,
            AdminOp::AlterDomain { .. } => self.domains,
        }
    }
}

impl AdminPane {
    fn set_value(&mut self, field: &'static str, value: String) {
        let slot = match field {
            "reindex" => &mut self.session.reindex_target,
            "attach_path" => &mut self.session.attach_path,
            "attach_name" => &mut self.session.attach_name,
            "pragma_value" => &mut self.session.pragma_value,
            "sequence" => &mut self.session.sequence_name,
            "sequence_step" => &mut self.session.sequence_step,
            "sequence_start" => &mut self.session.sequence_start,
            "sequence_min" => &mut self.session.sequence_min,
            "sequence_max" => &mut self.session.sequence_max,
            "event" => &mut self.session.event_name,
            "enum" => &mut self.session.enum_name,
            "enum_value" => &mut self.session.enum_value,
            "domain" => &mut self.session.domain_name,
            "domain_default" => &mut self.session.domain_default,
            _ => return,
        };
        *slot = value;
    }

    fn value_of(&self, field: &'static str) -> String {
        match field {
            "reindex" => self.session.reindex_target.clone(),
            "attach_path" => self.session.attach_path.clone(),
            "attach_name" => self.session.attach_name.clone(),
            "pragma_value" => self.session.pragma_value.clone(),
            "sequence" => self.session.sequence_name.clone(),
            "sequence_step" => self.session.sequence_step.clone(),
            "sequence_start" => self.session.sequence_start.clone(),
            "sequence_min" => self.session.sequence_min.clone(),
            "sequence_max" => self.session.sequence_max.clone(),
            "event" => self.session.event_name.clone(),
            "enum" => self.session.enum_name.clone(),
            "enum_value" => self.session.enum_value.clone(),
            "domain" => self.session.domain_name.clone(),
            "domain_default" => self.session.domain_default.clone(),
            _ => String::new(),
        }
    }

    fn admin_field(&mut self, field: &'static str, placeholder: &str, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let value = self.value_of(field);
        let state = self.fields.bind(field, &value, FieldSpec { placeholder, masked: false }, window, cx, |this: &mut Self, field, value, _| this.set_value(field, value));
        div().flex_1().min_w(px(0.)).child(kit::text_input(SharedString::from(format!("admin-field-{field}")), &state, placeholder.to_owned()).small()).into_any_element()
    }
}

#[allow(clippy::too_many_arguments)]
fn op_card(
    id: &'static str,
    kind: &'static str,
    icon: IconName,
    title: SharedString,
    inputs: Vec<AnyElement>,
    inventory: Vec<String>,
    // 读过清单而这节是空的时，块内给出的下一步（图标、标题键、说明键）；读取中或离线时为 None。
    empty: Option<(IconName, &'static str, &'static str)>,
    entity: &Entity<AdminPane>,
    locale: Locale,
    p: &Palette,
) -> AnyElement {
    let entity = entity.clone();
    let mut row = div().flex().items_center().gap(px(8.));
    for input in inputs {
        row = row.child(input);
    }
    let preview = kit::secondary_button(id, Some(IconName::Eye), SharedString::from(i18n::text(locale, "structure.action.preview")), move |_, _, cx| {
        entity.update(cx, |pane, cx| {
            pane.choose(kind);
            pane.preview(cx);
        });
    })
    .accessibility_label(title.clone());
    let mut card = kit::card(p)
        .p(px(14.))
        .flex()
        .flex_col()
        .gap(px(10.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(div().flex_none().size(px(30.)).rounded(px(Radius::PANEL)).bg(p.accent_soft).flex().items_center().justify_center().child(kit::icon(icon, 15., p.accent_text)))
                .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).child(title))
                .child(preview),
        );
    if row_has_children(&inventory) {
        let mut list = div().flex().flex_wrap().gap(px(6.));
        for item in inventory {
            list = list.child(kit::pill(SharedString::from(item), p.text_muted, p.hover));
        }
        card = card.child(list);
    } else if let Some((icon, heading, detail)) = empty {
        // 空的不是整页而是这一节：空态放在本节里，下一句就说清怎么把它填上，表单与预览入口仍在下方。
        card = card.child(
            div().id(SharedString::from(format!("{id}-empty"))).test_support().child(kit::empty_state(
                icon,
                SharedString::from(i18n::text(locale, heading)),
                SharedString::from(i18n::text(locale, detail)),
                p,
            )),
        );
    }
    card.child(row).into_any_element()
}

fn row_has_children(items: &[String]) -> bool {
    !items.is_empty()
}

impl Render for AdminPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 清单是一次异步读取：读取途中不再重复发起，读完（成功或离线）才允许下一次。
        if !self.probing && !self.session.inventory.probed && !self.session.target.connection_id.is_empty() {
            self.request_inventory(cx);
        }
        let p = Palette::of(cx);
        let metrics = kit::Metrics::of(cx);
        let locale = self.session.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let sections = self.session.sections();
        let entity = cx.entity();
        let inventory = self.session.inventory.clone();
        // 首屏读取给骨架，已读过内容的刷新只在顶部给一行增量指示；两者都优先于空态。
        let probing = self.probing;
        let ready = inventory.probed;
        // 只有读过清单的已连接目标才谈得上"这一节是空的"；离线目标另走连接空态。
        let listed = ready && inventory.connected;
        let empty = move |icon: IconName, heading: &'static str, detail: &'static str| listed.then_some((icon, heading, detail));
        let mut cards = div().flex().flex_col().gap(px(12.));
        if sections.maintenance {
            cards = cards.child(op_card("admin-vacuum", "vacuum", IconName::BrushCleaning, t("admin.action.vacuum"), Vec::new(), Vec::new(), None, &entity, locale, &p));
            let target = self.admin_field("reindex", &i18n::text(locale, "ui.conn.optional"), window, cx);
            cards = cards.child(op_card("admin-reindex", "reindex", IconName::ListRestart, t("admin.action.reindex"), vec![target], Vec::new(), None, &entity, locale, &p));
        }
        if sections.pragma {
            let mut pragma_bits = Vec::new();
            if !inventory.journal_mode.is_empty() {
                pragma_bits.push(format!("journal_mode {}", inventory.journal_mode));
            }
            if !inventory.user_version.is_empty() {
                pragma_bits.push(format!("user_version {}", inventory.user_version));
            }
            if let Some(enabled) = inventory.foreign_keys_on {
                pragma_bits.push(format!("foreign_keys {}", if enabled { "ON" } else { "OFF" }));
            }
            let value = self.admin_field("pragma_value", "user_version", window, cx);
            cards = cards
                .child(op_card("admin-pragma", "pragma", IconName::SlidersHorizontal, t("admin.action.pragma"), vec![value], pragma_bits, None, &entity, locale, &p))
                .child(op_card(
                    "admin-foreign",
                    "foreign",
                    IconName::Link,
                    SharedString::from(format!("{} · {}", i18n::text(locale, "admin.action.foreignKeys"), if self.session.foreign_keys { "ON" } else { "OFF" })),
                    Vec::new(),
                    Vec::new(),
                    None,
                    &entity,
                    locale,
                    &p,
                ));
        }
        if sections.attached {
            let path = self.admin_field("attach_path", "/path/to/other.sqlite", window, cx);
            let name = self.admin_field("attach_name", "other", window, cx);
            cards = cards.child(op_card(
                "admin-attach",
                "attach",
                IconName::Paperclip,
                t("admin.action.attach"),
                vec![path, name],
                inventory.attached.clone(),
                empty(IconName::Paperclip, "admin.empty.attached", "admin.empty.attachedHint"),
                &entity,
                locale,
                &p,
            ));
        }
        if sections.sequences {
            let name = self.admin_field("sequence", "orders_id_seq", window, cx);
            let start = self.admin_field("sequence_start", "1", window, cx);
            let step = self.admin_field("sequence_step", "1", window, cx);
            let min_value = self.admin_field("sequence_min", "1", window, cx);
            let max_value = self.admin_field("sequence_max", "1", window, cx);
            let mut picks = div().flex().flex_wrap().gap(px(6.));
            for (index, choice) in inventory.sequence_choices.iter().enumerate() {
                let pick_entity = entity.clone();
                picks = picks.child(kit::secondary_button(SharedString::from(format!("admin-pick-seq-{index}")), None, SharedString::from(choice.name.clone()), move |_, _, cx| {
                    pick_entity.update(cx, |pane, cx| {
                        pane.pick_sequence(index);
                        cx.notify();
                    });
                }));
            }
            cards = cards.child(picks).child(op_card(
                "admin-sequence",
                "sequence",
                IconName::ListOrdered,
                t("admin.action.sequence"),
                vec![name, start, step, min_value, max_value],
                inventory.sequences.clone(),
                empty(IconName::ListOrdered, "admin.empty.sequences", "admin.empty.sequencesHint"),
                &entity,
                locale,
                &p,
            ));
        }
        if sections.events {
            let name = self.admin_field("event", "event_name", window, cx);
            let mut toggles = div().flex().flex_wrap().gap(px(6.));
            for (index, choice) in inventory.event_choices.iter().enumerate() {
                let toggle_entity = entity.clone();
                let label = SharedString::from(format!("{} · {}", choice.name, if choice.enabled { "ENABLED" } else { "DISABLED" }));
                toggles = toggles.child(kit::secondary_button(SharedString::from(format!("admin-event-toggle-{index}")), None, label, move |_, _, cx| {
                    toggle_entity.update(cx, |pane, cx| pane.toggle_event(index, cx));
                }));
            }
            cards = cards.child(toggles).child(op_card(
                "admin-event",
                "event",
                IconName::CalendarClock,
                t("admin.action.event"),
                vec![name],
                inventory.events.clone(),
                empty(IconName::CalendarClock, "admin.empty.events", "admin.empty.eventsHint"),
                &entity,
                locale,
                &p,
            ));
        }
        if sections.enums {
            let name = self.admin_field("enum", "status", window, cx);
            let value = self.admin_field("enum_value", "value", window, cx);
            cards = cards.child(op_card(
                "admin-enum",
                "enum",
                IconName::Braces,
                t("catalog.kind.enum"),
                vec![name, value],
                inventory.enums.clone(),
                empty(IconName::Braces, "admin.empty.enums", "admin.empty.enumsHint"),
                &entity,
                locale,
                &p,
            ));
        }
        if sections.domains {
            let name = self.admin_field("domain", "domain", window, cx);
            let value = self.admin_field("domain_default", "DEFAULT", window, cx);
            cards = cards.child(op_card(
                "admin-domain",
                "domain",
                IconName::SlidersHorizontal,
                t("catalog.kind.domain"),
                vec![name, value],
                inventory.domains.clone(),
                empty(IconName::SlidersHorizontal, "admin.empty.domains", "admin.empty.domainsHint"),
                &entity,
                locale,
                &p,
            ));
        }
        // 分区读取失败：与其它页面同一套错误态（原因 + 当场可点的重试），不再自绘一张危险色卡片。
        for (index, fault) in inventory.faults.iter().enumerate() {
            let retry_entity = entity.clone();
            let retry = kit::secondary_button(SharedString::from(format!("admin-retry-{index}")), Some(IconName::RefreshCw), t("admin.action.retry"), move |_, _, cx| {
                retry_entity.update(cx, |pane, cx| pane.refresh(cx));
            })
            .into_any_element();
            cards = cards.child(
                div().id(SharedString::from(format!("admin-fault-{index}"))).test_support().child(kit::error_state_with_action(
                    t(fault.title_key),
                    SharedString::from(localize(&fault.message, locale)),
                    retry,
                    &p,
                )),
            );
        }
        if sections.extensions || sections.partitions {
            let mut card = kit::card(&p).p(px(14.)).flex().flex_col().gap(px(10.)).child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(div().flex_none().size(px(30.)).rounded(px(Radius::PANEL)).bg(p.accent_soft).flex().items_center().justify_center().child(kit::icon(IconName::Gauge, 15., p.accent_text)))
                    .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).child(t("admin.section.extras"))),
            );
            let items: Vec<String> = inventory.extensions.iter().chain(inventory.partitions.iter()).cloned().collect();
            if !items.is_empty() {
                let mut list = div().flex().flex_wrap().gap(px(6.));
                for name in items {
                    list = list.child(kit::pill(SharedString::from(name), p.text_muted, p.hover));
                }
                card = card.child(list);
            } else if listed {
                card = card.child(div().id("admin-extras-empty").test_support().child(kit::empty_state(IconName::Gauge, t("admin.empty.extras"), t("admin.empty.extrasHint"), &p)));
            }
            cards = cards.child(card);
        }
        if !sections.pragma && !sections.maintenance && !sections.sequences && !sections.events && !sections.extensions && !sections.partitions && !sections.attached && !sections.enums && !sections.domains {
            // 没有数据库级操作也是一句"下一步"：换成能出现的引擎，而不是一块空白。
            cards = cards.child(kit::empty_state(IconName::Gauge, t("ui.tool.admin"), t("admin.section.noneHint"), &p));
        }
        let mut column = div()
            .id("admin-pane")
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .px(px(metrics.page_pad))
                    .pt(px(metrics.page_pad_y))
                    .pb(px(metrics.pane_pad()))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .border_b_1()
                    .border_color(p.line)
                    .child(kit::engine_badge(&self.session.target.engine, 30.))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .child(kit::title(t("ui.tool.admin"), TypeScale::LG, &p))
                            .child(div().id("admin-banner").truncate().text_size(px(TypeScale::SM)).text_color(p.text_muted).child(SharedString::from(self.session.banner()))),
                    )
                    // 同一个位置：读取中换成一行转圈，读完换回来源说明。探测有可见加载态，表头也不会因插入而抖动。
                    .when(probing, |header| {
                        header.child(div().id("admin-probing").test_support().max_w(px(320.)).child(kit::inline_loading(t("admin.probe.loading"), &metrics, &p)))
                    })
                    .when(!probing, |header| {
                        header.child(div().id("admin-source").text_size(px(TypeScale::XS)).text_color(p.text_faint).max_w(px(320.)).truncate().child(SharedString::from(inventory.source_text(locale))))
                    })
                    .child(kit::secondary_button("admin-refresh", Some(IconName::RefreshCw), t("admin.action.refresh"), {
                        let entity = entity.clone();
                        move |_, _, cx| entity.update(cx, |pane, cx| pane.refresh(cx))
                    }))
                    .when(!inventory.connected, |header| {
                        let entity = entity.clone();
                        header.child(kit::primary_button("admin-connect", None, t("admin.action.connect"), move |_, _, cx| {
                            entity.update(cx, |pane, cx| {
                                if !pane.session.target.connection_id.is_empty() {
                                    pane.connect_request = Some(pane.session.target.connection_id.clone());
                                }
                                cx.notify();
                            });
                        }))
                    }),
            );
        let note = self.session.note.text(locale);
        // 首屏读取给骨架、刷新已读内容只给一行增量指示、读完而未连接给带"立即连接"的空态；
        // 三者都排在卡片之前，卡片本身（表单与预览入口）在任何状态下都保留，管理能力不因状态而消失。
        let mut states = div().flex().flex_col().gap(px(metrics.section_gap()));
        if probing && !ready {
            states = states.child(kit::skeleton_state(3, &metrics, &p));
        } else if probing {
            states = states.child(div().id("admin-refreshing").test_support().child(kit::inline_loading(t("admin.probe.refreshing"), &metrics, &p)));
        }
        if ready && !probing && !inventory.connected {
            let connect_entity = entity.clone();
            let connect = kit::primary_button("admin-body-connect", None, t("admin.action.connect"), move |_, _, cx| {
                connect_entity.update(cx, |pane, cx| {
                    if !pane.session.target.connection_id.is_empty() {
                        pane.connect_request = Some(pane.session.target.connection_id.clone());
                    }
                    cx.notify();
                });
            })
            .into_any_element();
            states = states.child(div().id("admin-offline").test_support().child(kit::empty_state_with_action(
                IconName::Link,
                t("admin.offline.heading"),
                t("admin.offline.detail"),
                connect,
                &p,
            )));
        }
        column = column
            .child(div().id("admin-body").flex_1().min_h(px(0.)).overflow_y_scroll().p(px(metrics.page_pad)).child(div().max_w(px(760.)).mx_auto().flex().flex_col().gap(px(metrics.section_gap())).child(states).child(cards).when(!note.is_empty(), |this| {
                this.child(div().px(px(12.)).py(px(8.)).rounded(px(Radius::PANEL)).bg(p.hover).text_size(px(TypeScale::SM)).child(SharedString::from(note)))
            })))
            .child(div().hidden().child(admin_button("admin-preview", "structure.action.preview", false, entity.clone(), locale)));
        if self.session.open {
            let blocked = self.session.blocked();
            // 危险操作醒目化：计划里有 danger 级风险时，提示条与确认按钮都换成危险色；
            // 没有 danger 级风险的管理操作也给一句"确认后立即执行、数据库不提供撤销"。
            let dangerous = self.session.preview_dangerous();
            column = column.child(
                div().absolute().inset_0().bg(gpui_kit::hsla(0., 0., 0., 0.25)).flex().items_center().justify_center().child(
                    div()
                        .id("admin-preview-dialog")
                        .role(gpui_kit::Role::Dialog)
                        .aria_label(t("structure.preview.title"))
                        .occlude()
                        .w(px(600.))
                        .max_w(relative(0.92))
                        .p(px(metrics.card_pad()))
                        .flex()
                        .flex_col()
                        .gap(px(12.))
                        .bg(p.raised)
                        .border_1()
                        .border_color(p.line)
                        .rounded(px(Radius::PANEL))
                        .shadow_lg()
                        .child(kit::title(t("structure.preview.title"), TypeScale::LG, &p))
                        .child(kit::muted(SharedString::from(self.session.banner()), &p))
                        .child(
                            div()
                                .id("admin-preview-sql")
                                .max_h(px(260.))
                                .overflow_y_scroll()
                                .p(px(12.))
                                .rounded(px(Radius::PANEL))
                                .bg(p.chrome)
                                .border_1()
                                .border_color(p.line)
                                .font_family(cx.theme().mono_font_family.clone())
                                .text_size(px(TypeScale::SM))
                                .child(SharedString::from(self.session.preview_sql().join("\n"))),
                        )
                        .child(kit::muted(SharedString::from(self.session.preview_target()), &p))
                        .when(!self.session.preview_risks().is_empty(), |this| {
                            let mut risks = div().flex().flex_col().gap(px(4.));
                            for (level, text) in self.session.preview_risks() {
                                let (fg, bg) = if level == "danger" { (p.danger, p.danger_soft) } else if level == "warn" { (p.warn, p.warn_soft) } else { (p.text_muted, p.hover) };
                                risks = risks.child(div().px(px(10.)).py(px(6.)).rounded(px(Radius::PANEL)).bg(bg).text_color(fg).text_size(px(TypeScale::SM)).child(SharedString::from(text)));
                            }
                            this.child(risks)
                        })
                        .when(!self.session.preview_rule().is_empty(), |this| this.child(kit::muted(SharedString::from(self.session.preview_rule()), &p)))
                        .when(!blocked.is_empty(), |this| this.child(div().px(px(12.)).py(px(8.)).rounded(px(Radius::PANEL)).bg(p.danger_soft).text_color(p.danger).text_size(px(TypeScale::SM)).child(SharedString::from(blocked.join(" ")))))
                        // 危险操作说明固定占一行：有 danger 级风险用危险色，其余用提醒色，措辞不同。
                        .child(
                            div()
                                .id(if dangerous { "admin-preview-danger" } else { "admin-preview-irreversible" })
                                .test_support()
                                .px(px(12.))
                                .py(px(8.))
                                .rounded(px(Radius::PANEL))
                                .bg(if dangerous { p.danger_soft } else { p.warn_soft })
                                .text_color(if dangerous { p.danger } else { p.warn })
                                .text_size(px(TypeScale::SM))
                                .child(t(if dangerous { "admin.preview.danger" } else { "admin.preview.irreversible" })),
                        )
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap(px(8.))
                                .child(admin_button("admin-cancel", "structure.action.cancel", false, entity.clone(), locale))
                                .child(if dangerous {
                                    let owner = entity.clone();
                                    kit::danger_button("admin-confirm", Some(IconName::Check), t("structure.action.confirm"), move |_, _, cx| {
                                        owner.update(cx, |pane, cx| pane.confirm(cx));
                                    })
                                    .into_any_element()
                                } else {
                                    admin_button("admin-confirm", "structure.action.confirm", true, entity, locale).into_any_element()
                                }),
                        ),
                ),
            );
        }
        column
    }
}

fn admin_button(id: &'static str, key: &'static str, primary: bool, entity: Entity<AdminPane>, locale: Locale) -> gpui_kit::component::button::Button {
    let click = move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut gpui_kit::App| {
        entity.update(cx, |pane, cx| match id {
            "admin-preview" => pane.preview(cx),
            "admin-confirm" => pane.confirm(cx),
            _ => {
                pane.session.cancel();
                cx.notify();
            }
        });
    };
    let label = SharedString::from(i18n::text(locale, key));
    if primary {
        kit::primary_button(id, Some(IconName::Check), label, click)
    } else {
        kit::secondary_button(id, None, label, click)
    }
}

async fn read_inventory(service: &crate::connections::Service, target: &GridTarget) -> AdminInventory {
    let Some(session) = service.catalog_session(&target.connection_id) else {
        let mut inventory = AdminInventory::offline(target);
        inventory.probed = true;
        return inventory;
    };
    if matches!(session, crate::connections::CatalogSession::Scripted) {
        let mut inventory = AdminInventory::offline(target);
        inventory.probed = true;
        return inventory;
    }
    let Ok(engine) = crate::connections::model::Engine::parse(&target.engine) else {
        let mut inventory = AdminInventory::offline(target);
        inventory.probed = true;
        return inventory;
    };
    let extras = crate::catalog::read_extras(&session, engine, &target.database, &target.schema).await;
    let mut inventory = inventory_from_extras(target, &extras);
    if engine == crate::connections::model::Engine::Postgres {
        load_postgres_types(&session, engine, target, &mut inventory).await;
    }
    inventory.connected = true;
    inventory.probed = true;
    inventory.note_key = None;
    inventory
}

fn inventory_from_extras(target: &GridTarget, extras: &crate::catalog::ExtrasBody) -> AdminInventory {
    let mut inventory = AdminInventory::offline(target);
    inventory.connected = true;
    inventory.probed = true;
    inventory.note_key = None;
    inventory.source = format!("{} · {}", target.engine, target.database);
    if let Some(row) = &extras.pragmas.value {
        inventory.journal_mode = row.journal_mode.clone().unwrap_or_default();
        inventory.user_version = row.user_version.map(|value| value.to_string()).unwrap_or_default();
        inventory.foreign_keys_on = row.foreign_keys;
    }
    push_fault(&mut inventory.faults, "admin.action.pragma", &extras.pragmas.error);
    for row in &extras.extensions.items {
        let label = match &row.installed_version {
            Some(version) if !version.is_empty() => format!("{} {version}", row.name),
            _ => row.name.clone(),
        };
        inventory.extensions.push(label);
    }
    push_fault(&mut inventory.faults, "catalog.kind.extension", &extras.extensions.error);
    for row in &extras.sequences.items {
        inventory.sequences.push(row.name.clone());
        inventory.sequence_choices.push(SequenceChoice {
            name: row.name.clone(),
            start: row.start_value,
            increment: row.increment,
            min_value: row.min_value,
            max_value: row.max_value,
            cycle: row.cycle,
        });
    }
    push_fault(&mut inventory.faults, "catalog.kind.sequence", &extras.sequences.error);
    for row in &extras.events.items {
        let enabled = row.status.eq_ignore_ascii_case("enabled");
        inventory.events.push(format!("{} · {}", row.name, row.status));
        inventory.event_choices.push(EventChoice { name: row.name.clone(), enabled });
    }
    push_fault(&mut inventory.faults, "catalog.kind.event", &extras.events.error);
    for row in &extras.partitions.items {
        inventory.partitions.push(format!("{} {} ({})", row.table, row.method, row.partitions.len()));
    }
    push_fault(&mut inventory.faults, "admin.section.partitions", &extras.partitions.error);
    inventory.attached = extras.attached.items.iter().map(|row| row.name.clone()).collect();
    push_fault(&mut inventory.faults, "admin.section.attached", &extras.attached.error);
    inventory
}

fn push_fault(faults: &mut Vec<SectionFault>, title_key: &'static str, error: &Option<String>) {
    if let Some(message) = error {
        faults.push(SectionFault { title_key, message: message.clone() });
    }
}

async fn load_postgres_types(session: &crate::connections::CatalogSession, engine: crate::connections::model::Engine, target: &GridTarget, inventory: &mut AdminInventory) {
    let enums = crate::catalog::read::read_object_kind_page(session, engine, &target.database, &target.schema, crate::catalog::model::ObjectKind::Enum, 0, 200).await;
    if let Some(message) = enums.error {
        inventory.faults.push(SectionFault { title_key: "catalog.kind.enum", message });
    } else {
        inventory.enums = enums.objects.into_iter().map(|row| row.name).collect();
    }
    let domains = crate::catalog::read::read_object_kind_page(session, engine, &target.database, &target.schema, crate::catalog::model::ObjectKind::Domain, 0, 200).await;
    if let Some(message) = domains.error {
        inventory.faults.push(SectionFault { title_key: "catalog.kind.domain", message });
    } else {
        inventory.domains = domains.objects.into_iter().map(|row| row.name).collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ddl::{DdlEngine, SequenceState, Snapshot};

    fn target(engine: &str) -> GridTarget {
        GridTarget {
            connection_id: "notes".into(),
            connection_name: "笔记".into(),
            database: "app".into(),
            schema: "public".into(),
            table: "customers".into(),
            engine: engine.into(),
            environment: "dev".into(),
        }
    }

    /// 英文界面：来源说明、预览风险、规则、阻断原因与提示都不含中文（连接名是用户数据，不在此列）。
    #[test]
    fn english_admin_copy_has_no_chinese() {
        let cjk = crate::ddl::english::has_cjk;
        let mut sqlite = target("sqlite");
        sqlite.database = "main".into();
        for (operation, pragma) in [
            (AdminOp::Vacuum, None),
            (AdminOp::Reindex { target: Some("orders".into()) }, None),
            (AdminOp::SetForeignKeys { enabled: true }, None),
            (AdminOp::SetPragma { pragma: "journal_mode".into(), value: "wal".into() }, None),
            (AdminOp::SetPragma { pragma: "user_version".into(), value: "3".into() }, Some("3")),
            (AdminOp::Attach { path: "/tmp/x.sqlite".into(), name: "main".into() }, None),
        ] {
            let mut session = AdminSession::new(sqlite.clone());
            session.locale = Locale::En;
            session.operation = operation.clone();
            let mut snapshot = Snapshot::bare(DdlEngine::Sqlite, "main", "", "main");
            snapshot.live_foreign_keys = Some(false);
            if let Some(value) = pragma {
                snapshot.pragmas.insert("user_version".into(), value.into());
            }
            session.arm_fixture(&snapshot);
            let mut texts: Vec<String> = session.preview_risks().into_iter().map(|(_, text)| text).collect();
            texts.extend(session.blocked());
            texts.push(session.preview_rule());
            texts.push(session.note_text());
            texts.push(session.inventory.source_text(Locale::En));
            texts.push(session.confirm_sql().unwrap_err());
            for text in texts {
                assert!(!cjk(&text), "{operation:?}: {text}");
            }
        }
        let offline = AdminInventory::offline(&sqlite);
        assert!(offline.source_text(Locale::ZhCn).contains("没有连接"), "中文界面仍是中文：{}", offline.source_text(Locale::ZhCn));
        assert!(offline.source.contains("sqlite"));
    }

    #[test]
    fn remote_admin_fixtures_stay_unconnected_and_sqlserver_has_no_section() {
        let sqlserver = sections_for("sqlserver");
        assert!(!sqlserver.pragma && !sqlserver.maintenance && !sqlserver.sequences && !sqlserver.events && !sqlserver.extensions && !sqlserver.partitions && !sqlserver.attached);

        let mut postgres = AdminSession::new(target("postgres"));
        postgres.operation = AdminOp::AlterSequence {
            sequence: "ids".into(),
            start: None,
            increment: Some(2),
            min_value: None,
            max_value: None,
            cycle: None,
        };
        let mut snapshot = Snapshot::bare(DdlEngine::Postgres, "app", "public", "ids");
        snapshot.sequence = Some(SequenceState { increment: Some(1), ..SequenceState::default() });
        postgres.arm_fixture(&snapshot);
        assert!(postgres.banner().contains("笔记") && postgres.banner().contains("dev") && postgres.banner().contains("postgres"), "{}", postgres.banner());
        assert!(postgres.blocked().is_empty(), "{:?}", postgres.blocked());
        assert!(postgres.preview_sql().join("\n").to_ascii_uppercase().contains("SEQUENCE"), "{:?}", postgres.preview_sql());
        assert!(postgres.confirm_sql().unwrap_err().contains("没有连接"));

        for engine in ["mysql", "mariadb"] {
            let mut session = AdminSession::new(target(engine));
            session.operation = AdminOp::SetEvent { event: "sweep".into(), enabled: false };
            let mut snapshot = Snapshot::bare(if engine == "mysql" { DdlEngine::Mysql } else { DdlEngine::Mariadb }, "app", "", "sweep");
            snapshot.event_enabled = Some(true);
            session.arm_fixture(&snapshot);
            assert!(session.sections().events, "{engine}");
            assert!(session.blocked().is_empty(), "{engine} {:?}", session.blocked());
            let sql = session.preview_sql().join("\n").to_ascii_uppercase();
            assert!(sql.contains("EVENT") && sql.contains("DISABLE"), "{engine} {sql}");
            assert!(session.confirm_sql().is_err(), "{engine}");
        }
    }

    #[test]
    fn admin_inputs_preview_the_requested_operation_and_unconnected_sources_refuse() {
        let dir = std::env::temp_dir().join(format!("rillsql-admin-draft-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let services = std::sync::Arc::new(crate::application::AppServices::new(std::sync::Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
        let mut pane = AdminPane::new(services);
        pane.bind(target("postgres"));
        assert!(!pane.session.inventory.connected);
        assert!(pane.session.inventory.source.contains("postgres"), "{}", pane.session.inventory.source);
        pane.type_into("reindex", "orders");
        pane.choose("reindex");
        match &pane.session.operation {
            AdminOp::Reindex { target } => assert_eq!(target.as_deref(), Some("orders")),
            other => panic!("{other:?}"),
        }
        pane.type_into("attach_path", "/tmp/other.sqlite");
        pane.type_into("attach_name", "other");
        pane.choose("attach");
        assert!(matches!(pane.session.operation, AdminOp::Attach { .. }));
        pane.choose("foreign");
        assert!(matches!(pane.session.operation, AdminOp::SetForeignKeys { enabled: false }));
        let mut snapshot = Snapshot::bare(DdlEngine::Postgres, "app", "public", "orders");
        pane.session.operation = AdminOp::Reindex { target: Some("orders".into()) };
        pane.session.arm_fixture(&snapshot);
        assert!(pane.session.confirm_sql().unwrap_err().contains("没有连接"));
        snapshot.name = "orders".into();
        pane.session.inventory.sequences.clear();
        assert!(pane.session.inventory.sequences.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[gpui_kit::test]
    fn failed_admin_confirm_keeps_the_preview_and_names_the_reason(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::AppContext as _;
        let dir = std::env::temp_dir().join(format!("rillsql-admin-fail-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let services = std::sync::Arc::new(crate::application::AppServices::new(std::sync::Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
        let pane = cx.update(|cx| cx.new(|_| AdminPane::new(services)));
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                pane.bind(target("sqlite"));
                let mut snapshot = Snapshot::bare(DdlEngine::Sqlite, "app", "", "customers");
                snapshot.name = "customers".into();
                pane.session.operation = AdminOp::Vacuum;
                pane.session.arm_fixture(&snapshot);
                assert!(pane.session.preview_open());
                pane.confirm(cx);
                assert!(pane.session.preview_open(), "失败后预览应保留，方便查看原因和重试");
                assert!(pane.session.note_text().contains("没有连接"), "{}", pane.session.note_text());
            });
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn admin_target_change_drops_the_previous_operation_and_preview() {
        let dir = std::env::temp_dir().join(format!("rillsql-admin-reset-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let services = std::sync::Arc::new(crate::application::AppServices::new(std::sync::Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
        let mut pane = AdminPane::new(services);
        pane.bind(target("postgres"));
        pane.type_into("reindex", "orders");
        pane.choose("reindex");
        let mut snapshot = Snapshot::bare(DdlEngine::Postgres, "app", "public", "orders");
        snapshot.name = "orders".into();
        pane.session.arm_fixture(&snapshot);
        assert!(pane.session.open);
        assert!(matches!(pane.session.operation, AdminOp::Reindex { .. }));

        let mut other = target("postgres");
        other.table = "customers_archive".into();
        pane.bind(other);
        assert_eq!(pane.session.target.table, "customers_archive");
        assert!(matches!(pane.session.operation, AdminOp::Vacuum), "换表后不能沿用上一个索引操作");
        assert!(pane.session.reindex_target.is_empty());
        assert!(!pane.session.open, "换表后不能沿用上一个预览");
        assert!(pane.session.note.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn sqlite_inventory_reads_attached_databases_and_remote_engines_stay_offline() {
        let dir = std::env::temp_dir().join(format!("rillsql-admin-inventory-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let database = dir.join("notes.sqlite");
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).create_if_missing(true))
            .await
            .unwrap();
        pool.close().await;
        let service = std::sync::Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")));
        service.open_sqlite_for_test("notes", "笔记", &database).await.unwrap();
        let inventory = read_inventory(
            &service,
            &GridTarget {
                connection_id: "notes".into(),
                connection_name: "笔记".into(),
                database: "main".into(),
                schema: String::new(),
                table: "customers".into(),
                engine: "sqlite".into(),
                environment: "dev".into(),
            },
        )
        .await;
        assert!(inventory.connected);
        assert!(inventory.attached.iter().any(|name| name == "main"), "{:?}", inventory.attached);
        let mut remote_target = target("postgres");
        remote_target.connection_id = "missing-postgres".into();
        let remote = read_inventory(&service, &remote_target).await;
        assert!(!remote.connected);
        assert!(remote.source.contains("postgres"), "{}", remote.source);
        assert!(remote.sequences.is_empty());
        assert!(remote.events.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn sqlite_pragma_cancel_keeps_user_version_and_confirm_writes_it() {
        let dir = std::env::temp_dir().join(format!("rillsql-admin-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let database = dir.join("notes.sqlite");
        let setup = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).create_if_missing(true))
            .await
            .unwrap();
        sqlx::query("CREATE TABLE customers (id INTEGER PRIMARY KEY)").execute(&setup).await.unwrap();
        setup.close().await;
        let service = std::sync::Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")));
        service.open_sqlite_for_test("notes", "笔记", &database).await.unwrap();
        let mut session = AdminSession::new(GridTarget {
            connection_id: "notes".into(),
            connection_name: "笔记".into(),
            database: "main".into(),
            schema: String::new(),
            table: "customers".into(),
            engine: "sqlite".into(),
            environment: "dev".into(),
        });
        assert!(session.sections().pragma && session.sections().maintenance);
        session.operation = AdminOp::SetPragma { pragma: "user_version".into(), value: "7".into() };
        let request = session.request().unwrap();
        let plan = crate::application::ddl::preview_request(&service, "notes", &request).await.unwrap();
        assert!(plan.blocked.is_empty(), "{:?}", plan.blocked);
        session.arm_live(plan);
        assert!(session.banner().contains("笔记") && session.banner().contains("dev") && session.banner().contains("sqlite"), "{}", session.banner());
        let sql = session.confirm_sql().unwrap();
        assert!(sql.iter().any(|item| item.contains("user_version")), "{sql:?}");
        session.cancel();
        assert!(session.confirm_sql().is_err());
        let refused = match crate::application::ddl::execute_request(&service, "notes", &request, &sql, false).await {
            Err(message) => message,
            Ok(_) => panic!("未确认的管理操作应当被拒绝"),
        };
        assert!(refused.contains("确认"), "{refused}");
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).read_only(true))
            .await
            .unwrap();
        let version: i64 = sqlx::query_scalar("PRAGMA user_version").fetch_one(&pool).await.unwrap();
        pool.close().await;
        assert_eq!(version, 0);
        session.open = true;
        crate::application::ddl::execute_request(&service, "notes", &request, &sql, true).await.unwrap();
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).read_only(true))
            .await
            .unwrap();
        let version: i64 = sqlx::query_scalar("PRAGMA user_version").fetch_one(&pool).await.unwrap();
        pool.close().await;
        assert_eq!(version, 7);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extras_from_a_real_engine_are_connected_and_section_errors_stay_local() {
        let mut extras = crate::catalog::ExtrasBody::default();
        extras.extensions.error = Some("读取扩展失败：permission denied".into());
        extras.partitions.error = Some("读取分区失败：permission denied".into());
        let inventory = inventory_from_extras(&target("postgres"), &extras);
        assert!(inventory.connected, "已连接的引擎不能再被写成离线");
        assert!(inventory.note_key.is_none());
        assert_eq!(inventory.faults.len(), 2);
        for fault in &inventory.faults {
            assert!(!crate::ddl::english::has_cjk(&localize(&fault.message, Locale::En)), "{}", fault.message);
        }
        let offline = AdminInventory::offline(&target("postgres"));
        assert!(!offline.connected);
    }

    // ---- 三态与危险操作呈现（T15）----

    fn state_services(tag: &str) -> (Arc<crate::application::AppServices>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("rillsql-admin-state-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let services = Arc::new(crate::application::AppServices::new(Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
        (services, dir)
    }

    fn state_window_options(width: f32) -> gpui_kit::WindowOptions {
        let bounds = gpui_kit::Bounds { origin: gpui_kit::Point::default(), size: gpui_kit::size(gpui_kit::px(width), gpui_kit::px(900.)) };
        gpui_kit::WindowOptions { window_bounds: Some(gpui_kit::WindowBounds::Windowed(bounds)), ..Default::default() }
    }

    /// 探测有可见加载态：首屏给骨架、已读内容的刷新只给一行增量指示；卡片与预览入口在任何状态下都在。
    /// 读完而未连接时换成带"立即连接"的空态——不是空白，也不是把管理入口藏起来。
    #[gpui_kit::test]
    fn probing_is_visible_and_offline_targets_get_a_connect_next_step(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::test::TestWindowExt;
        use gpui_kit::AppContext as _;
        let (services, dir) = state_services("probing");
        cx.update(gpui_kit::init);
        let mut pane = AdminPane::new(services.clone());
        pane.bind(target("sqlite"));
        // 首屏：还没有读完清单。
        pane.probing = true;
        let (window, pane) = cx
            .update(|cx| gpui_kit::open_window(state_window_options(900.), cx, |_window, cx| cx.new(|_| pane)))
            .expect("admin pane");
        let (skeleton, offline, error, header, cards) = cx
            .update_window(window, |_, window, cx| {
                window.render_frame(cx);
                (
                    window.try_find("skeleton-state").map(|block| block.visible()),
                    window.try_find("admin-offline").is_some(),
                    window.try_find("error-state").is_some(),
                    window.try_find("admin-probing").is_some(),
                    window.try_find("admin-vacuum").is_some(),
                )
            })
            .unwrap();
        assert_eq!(skeleton, Some(true), "首屏读取要给骨架");
        assert!(!offline, "读取中不能先判成未连接");
        assert!(!error, "读取中不是错误态");
        assert!(header, "表头要有可见的读取指示");
        assert!(cards, "加载中也不能收起管理入口");

        // 刷新（refresh）不清"已读过"（见 refresh_keeps_the_read_inventory）：所以刷新时状态就是
        // probing + probed，页面给增量指示而不是整块骨架；只有换目标/换工作项才回到首屏骨架。
        pane.update(cx, |pane, cx| {
            pane.session.inventory = inventory_from_extras(&target("sqlite"), &crate::catalog::ExtrasBody::default());
            assert!(pane.session.inventory.probed, "已读过的清单");
            pane.probing = true;
            cx.notify();
        });
        let (skeleton, offline, error, refreshing) = cx
            .update_window(window, |_, window, cx| {
                window.render_frame(cx);
                (
                    window.try_find("skeleton-state").is_some(),
                    window.try_find("admin-offline").is_some(),
                    window.try_find("error-state").is_some(),
                    window.try_find("admin-refreshing").is_some(),
                )
            })
            .unwrap();
        assert!(!skeleton, "已读内容的刷新不该整块变骨架");
        assert!(!offline, "已连接的目标不该出现连接空态");
        assert!(!error, "刷新不是错误");
        assert!(refreshing, "增量刷新要有一行可见指示");

        // 读完而没连接：连接空态带主操作，卡片与预览入口仍在。
        pane.update(cx, |pane, cx| {
            pane.session.inventory = AdminInventory::offline(&target("postgres"));
            pane.session.inventory.probed = true;
            pane.session.target = target("postgres");
            pane.probing = false;
            cx.notify();
        });
        let (empty, connect_label, ops) = cx
            .update_window(window, |_, window, cx| {
                window.render_frame(cx);
                (window.try_find("admin-offline").is_some(), window.find("admin-body-connect").label().map(str::to_owned), window.try_find("admin-sequence").is_some())
            })
            .unwrap();
        assert!(empty, "未连接的目标要给空态，不是空白");
        assert_eq!(connect_label.as_deref(), Some(i18n::text(Locale::ZhCn, "admin.action.connect").as_str()), "空态主操作是当场能做的下一步");
        assert!(ops, "未连接时操作卡（含预览入口）仍要在");
        cx.update_window(window, |_, window, cx| window.click("admin-body-connect", cx)).unwrap();
        let request = pane.update(cx, |pane, _| pane.take_connect_request());
        assert_eq!(request.as_deref(), Some("notes"), "空态主操作要真的发起连接");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 刷新不清"已读过"：`refresh` 只清"读取中"再发起一次读取，不清 `inventory.probed`。
    /// 因此"读取中 + 已读过的清单"是真能出现的状态，页面给的是增量指示而不是整块骨架；
    /// 首屏骨架只留给换目标/换工作项（`bind_item` 会重建会话）。
    #[gpui_kit::test]
    fn refresh_keeps_the_read_inventory(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::AppContext as _;
        let (services, dir) = state_services("refresh");
        cx.update(gpui_kit::init);
        let pane = cx.update(|cx| {
            cx.new(|_| {
                let mut pane = AdminPane::new(services.clone());
                pane.bind(target("sqlite"));
                pane.session.inventory = inventory_from_extras(&target("sqlite"), &crate::catalog::ExtrasBody::default());
                pane
            })
        });
        pane.update(cx, |pane, cx| {
            assert!(pane.session.inventory.probed);
            // 这条目标没有可读的连接：refresh 不会真的发起读取，正好只观察它对状态的影响。
            pane.session.target.connection_id.clear();
            pane.refresh(cx);
            assert!(pane.session.inventory.probed, "刷新不能把已读过的清单退回未读");
            assert!(!pane.probing, "没有可读的连接时不留在读取中");
            // 换目标才回到未读：首屏骨架只属于新目标。
            pane.bind_item(target("postgres"), "item-2");
            assert!(!pane.session.inventory.probed, "换目标要重新读清单");
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 分区读取失败用统一错误态（原因 + 重试），不再自绘危险色卡片。
    #[gpui_kit::test]
    fn inventory_faults_use_the_unified_error_state_with_a_retry_next_step(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::test::TestWindowExt;
        use gpui_kit::AppContext as _;
        let (services, dir) = state_services("faults");
        cx.update(gpui_kit::init);
        let mut extras = crate::catalog::ExtrasBody::default();
        extras.pragmas.error = Some("读取 PRAGMA 失败：database is locked".into());
        extras.attached.error = Some("读取附加库失败：permission denied".into());
        let mut pane = AdminPane::new(services.clone());
        pane.bind(target("sqlite"));
        pane.session.inventory = inventory_from_extras(&target("sqlite"), &extras);
        let (window, _pane) = cx
            .update(|cx| gpui_kit::open_window(state_window_options(900.), cx, |_window, cx| cx.new(|_| pane)))
            .expect("admin pane");
        let (errors, details, first, second, first_visible, retry, second_retry) = cx
            .update_window(window, |_, window, cx| {
                window.render_frame(cx);
                (
                    window.find_all("error-state").len(),
                    window.find_all("error-state-detail").len(),
                    window.within("admin-fault-0").try_find("error-state").is_some(),
                    window.within("admin-fault-1").try_find("error-state").is_some(),
                    // 首屏可见的错误态（后面的分区在滚动区里，不必同时可见）。
                    window.find_all("error-state").into_iter().filter(|block| block.visible()).count(),
                    window.find("admin-retry-0").label().map(str::to_owned),
                    window.find("admin-retry-1").label().map(str::to_owned),
                )
            })
            .unwrap();
        assert_eq!(errors, 2, "两个失败分区各一个统一错误态");
        assert_eq!(details, 2, "错误态都要写清原因");
        assert!(first && second, "错误态按分区落在自己的块里");
        assert!(first_visible > 0, "首屏要看得到错误态");
        let retry_label = i18n::text(Locale::ZhCn, "admin.action.retry");
        assert_eq!(retry.as_deref(), Some(retry_label.as_str()), "重试按钮文案要翻译过，不能是键名");
        assert_eq!(second_retry.as_deref(), Some(retry_label.as_str()), "每个失败分区都带自己的重试");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 空的 section 给上下文下一步，并把表单、预览入口留在原地。
    #[gpui_kit::test]
    fn empty_sections_carry_a_next_step_and_keep_their_forms(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::test::TestWindowExt;
        use gpui_kit::AppContext as _;
        let (services, dir) = state_services("empty");
        cx.update(gpui_kit::init);
        // 预览按钮的无障碍名是这一节的标题（op_card 的 accessibility_label），不是按钮上的"预览 SQL"。
        for (engine, empty_id, field_id, button_id, title_key) in [
            ("sqlite", "admin-attach-empty", "admin-field-attach_path", "admin-attach", "admin.action.attach"),
            ("postgres", "admin-sequence-empty", "admin-field-sequence", "admin-sequence", "admin.action.sequence"),
        ] {
            let mut pane = AdminPane::new(services.clone());
            pane.bind(target(engine));
            pane.session.inventory = inventory_from_extras(&target(engine), &crate::catalog::ExtrasBody::default());
            let (window, _pane) = cx
                .update(|cx| gpui_kit::open_window(state_window_options(900.), cx, |_window, cx| cx.new(|_| pane)))
                .expect("admin pane");
            let (empty, detail, field, preview) = cx
                .update_window(window, |_, window, cx| {
                    window.render_frame(cx);
                    (
                        window.try_find(empty_id).is_some(),
                        window.within(empty_id).find("empty-state-detail").visible(),
                        window.try_find(field_id).is_some(),
                        window.find(button_id).label().map(str::to_owned),
                    )
                })
                .unwrap();
            assert!(empty, "{engine}: 空 section 要有带下一步的空态");
            assert!(detail, "{engine}: 空态要写出下一步（说明文字可见）");
            assert!(field, "{engine}: 空态不能把输入框挤掉");
            assert_eq!(preview.as_deref(), Some(i18n::text(Locale::ZhCn, title_key).as_str()), "{engine}: 预览入口仍在，且无障碍名是这一节的标题");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 危险操作：计划里有 danger 级风险时确认层用危险色与不同措辞；普通管理操作也给一句"不能撤销"。
    #[gpui_kit::test]
    fn dangerous_previews_mark_the_confirm_and_explain_that_it_cannot_be_undone(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::test::TestWindowExt;
        use gpui_kit::AppContext as _;
        let (services, dir) = state_services("danger");
        cx.update(gpui_kit::init);
        let mut pane = AdminPane::new(services.clone());
        pane.bind(target("sqlite"));
        pane.session.inventory = inventory_from_extras(&target("sqlite"), &crate::catalog::ExtrasBody::default());
        let snapshot = Snapshot::bare(DdlEngine::Sqlite, "main", "", "main");
        pane.session.operation = AdminOp::Vacuum;
        pane.session.arm_fixture(&snapshot);
        assert!(!pane.session.preview_dangerous(), "VACUUM 不是 danger 级");
        let (window, pane) = cx
            .update(|cx| gpui_kit::open_window(state_window_options(900.), cx, |_window, cx| cx.new(|_| pane)))
            .expect("admin pane");
        let (notice, danger_notice, confirm) = cx
            .update_window(window, |_, window, cx| {
                window.render_frame(cx);
                (
                    window.try_find("admin-preview-irreversible").map(|block| block.visible()),
                    window.try_find("admin-preview-danger").is_some(),
                    window.find("admin-confirm").label().map(str::to_owned),
                )
            })
            .unwrap();
        assert_eq!(notice, Some(true), "预览层的确认说明要写清不能撤销");
        assert!(!danger_notice, "没有 danger 级风险时不用危险措辞");
        assert_eq!(confirm.as_deref(), Some(i18n::text(Locale::ZhCn, "structure.action.confirm").as_str()), "确认入口不能回退");

        // 危险计划走的是同一套预览：删除表自带 danger 级风险。
        let mut danger_snapshot = Snapshot::bare(DdlEngine::Sqlite, "main", "", "customers");
        danger_snapshot.name = "customers".into();
        let plan = preview_plan(&danger_snapshot, &Action::DropTable { table: "customers".into() });
        assert!(plan.risks.iter().any(|risk| risk.level == "danger"), "删表计划应当带 danger 级风险");
        pane.update(cx, |pane, cx| {
            pane.session.arm_live(plan);
            cx.notify();
        });
        let (dangerous, danger_notice, plain_notice, confirm) = cx
            .update_window(window, |_, window, cx| {
                window.render_frame(cx);
                (
                    pane.read(cx).session.preview_dangerous(),
                    window.try_find("admin-preview-danger").map(|block| block.visible()),
                    window.try_find("admin-preview-irreversible").is_some(),
                    window.find("admin-confirm").label().map(str::to_owned),
                )
            })
            .unwrap();
        assert!(dangerous, "danger 级风险要认出来");
        assert_eq!(danger_notice, Some(true), "danger 级风险换成危险措辞");
        assert!(!plain_notice, "危险与普通说明互斥");
        assert_ne!(
            i18n::text(Locale::ZhCn, "admin.preview.danger"),
            i18n::text(Locale::ZhCn, "admin.preview.irreversible"),
            "危险与普通管理操作的说明必须不同"
        );
        assert_eq!(confirm.as_deref(), Some(i18n::text(Locale::ZhCn, "structure.action.confirm").as_str()), "危险确认入口仍在");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 三档宽度 × 双主题 × 双密度：空态、说明与操作入口都不越出窗口右边界（R1/R4）。
    /// 本机没有原生截屏通道，这里给的是 headless 布局几何；像素截图由交付阶段在候选版本上取。
    #[gpui_kit::test]
    fn states_and_actions_stay_inside_1360_900_and_640_in_both_themes_and_densities(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::test::TestWindowExt;
        use gpui_kit::AppContext as _;
        let (services, dir) = state_services("widths");
        cx.update(gpui_kit::init);
        let setups = [
            ("浅色/紧凑", false, kit::Metrics::compact()),
            ("浅色/标准", false, kit::Metrics::standard()),
            ("深色/紧凑", true, kit::Metrics::compact()),
            ("深色/标准", true, kit::Metrics::standard()),
        ];
        for (label, dark, density) in setups {
            for width in [1360., 900., 640.] {
                cx.update(|cx| {
                    kit::apply_theme(dark, cx);
                    density.publish(cx);
                });
                let mut pane = AdminPane::new(services.clone());
                pane.bind(target("sqlite"));
                pane.session.inventory = inventory_from_extras(&target("sqlite"), &crate::catalog::ExtrasBody::default());
                let (window, _pane) = cx
                    .update(|cx| gpui_kit::open_window(state_window_options(width), cx, |_window, cx| cx.new(|_| pane)))
                    .expect("admin pane");
                let (empty, detail, preview, refresh) = cx
                    .update_window(window, |_, window, cx| {
                        window.render_frame(cx);
                        (
                            window.try_find("admin-attach-empty").is_some(),
                            window.find("empty-state-detail").bounds(),
                            window.find("admin-attach").bounds(),
                            window.find("admin-refresh").bounds(),
                        )
                    })
                    .unwrap();
                assert!(empty, "{label}/{width}: 空 section 要给下一步");
                println!("{label}/{width}: 空态说明 {detail:?} 预览 {preview:?} 刷新 {refresh:?}");
                for (name, bounds) in [("空态说明", detail), ("预览入口", preview), ("刷新", refresh)] {
                    assert!(bounds.size.width > px(0.), "{label}/{width}: {name} 要有宽度");
                    assert!(
                        bounds.origin.x + bounds.size.width <= px(width) + px(1.),
                        "{label}/{width}: {name} 越出右边界 {bounds:?}"
                    );
                }
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 新增的状态文案：两种语言都有、英文不含中文，且三类语义措辞互不相同。
    #[test]
    fn admin_state_copy_is_translated_and_state_specific() {
        let cjk = crate::ddl::english::has_cjk;
        let keys = [
            "admin.probe.loading",
            "admin.probe.refreshing",
            "admin.offline.heading",
            "admin.offline.detail",
            "admin.section.extras",
            "admin.section.noneHint",
            "admin.empty.attached",
            "admin.empty.attachedHint",
            "admin.empty.sequences",
            "admin.empty.sequencesHint",
            "admin.empty.events",
            "admin.empty.eventsHint",
            "admin.empty.enums",
            "admin.empty.enumsHint",
            "admin.empty.domains",
            "admin.empty.domainsHint",
            "admin.empty.extras",
            "admin.empty.extrasHint",
            "admin.preview.irreversible",
            "admin.preview.danger",
        ];
        for key in keys {
            let zh = i18n::text(Locale::ZhCn, key);
            let en = i18n::text(Locale::En, key);
            assert_ne!(zh, key, "{key}");
            assert_ne!(en, key, "{key}");
            assert!(!cjk(&en), "{key}: {en}");
        }
        assert_ne!(i18n::text(Locale::ZhCn, "admin.section.none"), i18n::text(Locale::ZhCn, "admin.section.noneHint"));
        assert_ne!(i18n::text(Locale::ZhCn, "admin.empty.attachedHint"), i18n::text(Locale::ZhCn, "admin.offline.detail"));
    }
}
