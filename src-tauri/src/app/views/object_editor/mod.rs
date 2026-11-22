//! 对象编辑器：事件、函数/存储过程与触发器的统一编辑入口。
//!
//! 这里只放编辑器的目标、草稿状态与类型登记：表单渲染、预览确认与结果展示复用既有的
//! DDL 计划（plan）与 Receipt 视图，不另建执行或 SQL 拼接路径。各对象的语句生成在
//! adapters/mysql/object_ddl.rs，读回在 adapters/mysql/object_read.rs。

pub mod view;

pub use view::{EditorRequest, ObjectEditorPane};

use crate::catalog::model::ObjectKind;
use crate::ddl::model::{EventDraft, RoutineDraft, TriggerDraft};
use crate::i18n::{self, Locale};

/// 本轮对象编辑器能打开的对象类型。表与视图仍走结构页与定义页。
pub const EDITABLE_KINDS: [ObjectKind; 4] = [ObjectKind::Event, ObjectKind::Function, ObjectKind::Procedure, ObjectKind::Trigger];

/// 对象编辑器是否负责这类对象。
pub fn editable(kind: ObjectKind) -> bool {
    EDITABLE_KINDS.contains(&kind)
}

/// 标题的 i18n 键。没有专门文案的对象返回通用键，不拼未知字符串。
pub fn title_key(kind: ObjectKind) -> &'static str {
    match kind {
        ObjectKind::Event => "objectEditor.event",
        ObjectKind::Function => "objectEditor.function",
        ObjectKind::Procedure => "objectEditor.procedure",
        ObjectKind::Trigger => "objectEditor.trigger",
        _ => "objectEditor.title",
    }
}

/// 编辑器打开的目标：哪个库、哪类对象、叫什么。新建时 name 为空。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ObjectEditorTarget {
    pub database: String,
    pub kind: Option<ObjectKind>,
    pub name: String,
}

impl ObjectEditorTarget {
    /// 新建对象：还没有名字，保存前由草稿决定。
    pub fn new_object(database: &str, kind: ObjectKind) -> Self {
        Self { database: database.to_owned(), kind: Some(kind), name: String::new() }
    }

    /// 打开已有对象。
    pub fn existing(database: &str, kind: ObjectKind, name: &str) -> Self {
        Self { database: database.to_owned(), kind: Some(kind), name: name.to_owned() }
    }

    /// 是否在改已有对象。新建的草稿没有 original。
    pub fn is_new(&self) -> bool {
        self.name.is_empty()
    }

    pub fn title(&self, locale: Locale) -> String {
        let key = self.kind.map(title_key).unwrap_or("objectEditor.title");
        i18n::text(locale, key)
    }
}

/// 编辑器里的草稿。一类对象一个草稿，保存时只提交这一份。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ObjectEditorDraft {
    Event(EventDraft),
    Routine(RoutineDraft),
    Trigger(TriggerDraft),
}

impl ObjectEditorDraft {
    /// 草稿对应的对象类型。函数与存储过程分开，不能互相替换。
    pub fn kind(&self) -> ObjectKind {
        match self {
            ObjectEditorDraft::Event(_) => ObjectKind::Event,
            ObjectEditorDraft::Routine(routine) => match routine.kind {
                crate::ddl::model::RoutineKind::Function => ObjectKind::Function,
                crate::ddl::model::RoutineKind::Procedure => ObjectKind::Procedure,
            },
            ObjectEditorDraft::Trigger(_) => ObjectKind::Trigger,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            ObjectEditorDraft::Event(event) => event.name.trim(),
            ObjectEditorDraft::Routine(routine) => routine.name.trim(),
            ObjectEditorDraft::Trigger(trigger) => trigger.name.trim(),
        }
    }

    /// 是否在改已有对象。
    pub fn is_edit(&self) -> bool {
        match self {
            ObjectEditorDraft::Event(event) => event.original.is_some(),
            ObjectEditorDraft::Routine(routine) => routine.original.is_some(),
            ObjectEditorDraft::Trigger(trigger) => trigger.original.is_some(),
        }
    }

    /// 草稿与目标是否对得上：类型一致、名字一致。对不上时不能保存，避免把函数写成存储过程。
    pub fn matches(&self, target: &ObjectEditorTarget) -> bool {
        target.kind == Some(self.kind()) && (target.is_new() || target.name == self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ddl::model::RoutineKind;

    /// 四类对象都能编辑；表与视图仍走结构页与定义页。
    #[test]
    fn only_proprietary_objects_open_in_the_object_editor() {
        for kind in EDITABLE_KINDS {
            assert!(editable(kind), "{kind:?}");
        }
        for kind in [ObjectKind::Table, ObjectKind::View, ObjectKind::Sequence, ObjectKind::Enum, ObjectKind::Domain] {
            assert!(!editable(kind), "{kind:?}");
        }
    }

    /// 标题键在中英文里都有文案，不会把键名直接显示给用户。
    #[test]
    fn title_keys_resolve_in_both_locales() {
        for kind in EDITABLE_KINDS {
            let key = title_key(kind);
            for locale in [Locale::ZhCn, Locale::En] {
                let text = i18n::text(locale, key);
                assert!(!text.is_empty(), "{key}");
                assert_ne!(text, key, "{key} 缺文案");
            }
        }
        let target = ObjectEditorTarget::existing("app", ObjectKind::Trigger, "audit");
        assert_ne!(target.title(Locale::ZhCn), "objectEditor.trigger");
        assert!(!target.is_new());
    }

    /// 草稿类型与目标必须一致：函数不能被当成存储过程保存。
    #[test]
    fn drafts_only_save_against_a_matching_target() {
        let target = ObjectEditorTarget::existing("app", ObjectKind::Function, "total");
        let routine = RoutineDraft { kind: RoutineKind::Function, original: Some("total".to_owned()), name: "total".to_owned(), ..RoutineDraft::default() };
        assert!(ObjectEditorDraft::Routine(routine.clone()).matches(&target));
        assert!(ObjectEditorDraft::Routine(routine.clone()).is_edit());
        let procedure = RoutineDraft { kind: RoutineKind::Procedure, ..routine };
        assert!(!ObjectEditorDraft::Routine(procedure).matches(&target));

        let new_target = ObjectEditorTarget::new_object("app", ObjectKind::Event);
        let event = EventDraft { name: "purge".to_owned(), ..EventDraft::default() };
        let draft = ObjectEditorDraft::Event(event);
        assert!(draft.matches(&new_target));
        assert_eq!(draft.name(), "purge");
        assert!(!draft.is_edit());
    }
}
// ---------------------------------------------------------------- 表单模型

/// 表单里的动作：保存（新建或替换）、删除、启停（只对事件）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorVerb {
    Save,
    Drop,
    Enable,
    Disable,
}

impl EditorVerb {
    /// 动作标签对应的 i18n 键。
    pub fn label_key(self) -> &'static str {
        match self {
            EditorVerb::Save => "common.save",
            EditorVerb::Drop => "structure.section.danger",
            EditorVerb::Enable | EditorVerb::Disable => "admin.action.event",
        }
    }
}

/// 对象编辑器的表单状态。这里是唯一的事实来源：文本框只把输入写回它，
/// 语句由 ddl 的计划层生成（不在这里拼 SQL），执行走既有的确认链。
#[derive(Clone, Debug)]
pub struct ObjectEditorSession {
    pub locale: Locale,
    pub connection_id: String,
    pub database: String,
    /// 模式：MySQL 与 MariaDB 为空，只为和 Request 的字段对齐。
    pub schema: String,
    pub engine: String,
    pub kind: ObjectKind,
    /// 目标对象名；改已有对象时与读回的名字一致。
    pub name: String,
    /// 触发器的目标表。
    pub table: String,
    pub timing_after: bool,
    pub event_kind: crate::ddl::model::TriggerEvent,
    /// 事件：单次（AT）或周期（EVERY）。
    pub one_time: bool,
    pub at: String,
    pub every: String,
    pub unit: String,
    pub starts: String,
    pub ends: String,
    pub enabled: bool,
    pub preserve: bool,
    pub comment: String,
    /// 例程参数，每行一个：模式 名字 类型（函数的模式留空）。
    pub params: String,
    pub returns: String,
    pub body: String,
    pub verb: EditorVerb,
    /// 已读到的对象状态。None 表示还没打开或读失败。
    pub detail: Option<crate::catalog::objects::ObjectDetail>,
    pub detail_error: String,
    pub plan: Option<crate::ddl::Plan>,
    pub receipt: Option<crate::ddl::Receipt>,
    pub note: String,
}

impl Default for ObjectEditorSession {
    fn default() -> Self {
        Self {
            locale: Locale::ZhCn,
            connection_id: String::new(),
            database: String::new(),
            schema: String::new(),
            engine: String::new(),
            kind: ObjectKind::Event,
            name: String::new(),
            table: String::new(),
            timing_after: false,
            event_kind: crate::ddl::model::TriggerEvent::Insert,
            one_time: true,
            at: String::new(),
            every: "1".to_owned(),
            unit: "DAY".to_owned(),
            starts: String::new(),
            ends: String::new(),
            enabled: false,
            preserve: true,
            comment: String::new(),
            params: String::new(),
            returns: String::new(),
            body: String::new(),
            verb: EditorVerb::Save,
            detail: None,
            detail_error: String::new(),
            plan: None,
            receipt: None,
            note: String::new(),
        }
    }
}

impl ObjectEditorSession {
    pub fn new(locale: Locale) -> Self {
        Self { locale, ..Self::default() }
    }

    /// 换连接/库/引擎：旧对象的草稿与预览都不再适用。
    pub fn rebind(&mut self, connection_id: &str, database: &str, engine: &str) {
        if self.connection_id == connection_id && self.database == database && self.engine == engine {
            return;
        }
        let locale = self.locale;
        let kind = self.kind;
        *self = Self { locale, kind, ..Self::default() };
        self.connection_id = connection_id.to_owned();
        self.database = database.to_owned();
        self.engine = engine.to_owned();
    }

    /// 是否在改已有对象（读到了服务器上的定义）。
    pub fn is_edit(&self) -> bool {
        self.detail.is_some()
    }

    /// 切换对象类型：换类型就换一份草稿，避免把事件主体当成触发器主体提交。
    pub fn set_kind(&mut self, kind: ObjectKind) {
        if self.kind == kind {
            return;
        }
        let locale = self.locale;
        let connection_id = self.connection_id.clone();
        let database = self.database.clone();
        let engine = self.engine.clone();
        *self = Self { locale, kind, ..Self::default() };
        self.connection_id = connection_id;
        self.database = database;
        self.engine = engine;
    }

    /// 清除预览与结果：改任何输入都要重新预览。
    pub fn cancel(&mut self) {
        self.plan = None;
        self.receipt = None;
    }

    /// 文本框当前值。键与界面上渲染的文本框一一对应。
    pub fn field(&self, key: &'static str) -> String {
        match key {
            "name" => self.name.clone(),
            "table" => self.table.clone(),
            "at" => self.at.clone(),
            "every" => self.every.clone(),
            "unit" => self.unit.clone(),
            "starts" => self.starts.clone(),
            "ends" => self.ends.clone(),
            "comment" => self.comment.clone(),
            "params" => self.params.clone(),
            "returns" => self.returns.clone(),
            _ => String::new(),
        }
    }

    /// 写回一个文本框的值，并作废这次预览。
    pub fn set_field(&mut self, key: &'static str, value: String) {
        match key {
            "name" => self.name = value,
            "table" => self.table = value,
            "at" => self.at = value,
            "every" => self.every = value,
            "unit" => self.unit = value,
            "starts" => self.starts = value,
            "ends" => self.ends = value,
            "comment" => self.comment = value,
            "params" => self.params = value,
            "returns" => self.returns = value,
            _ => {}
        }
        self.cancel();
    }

    /// 用读回的对象状态起稿：已读到的属性原样带进表单，改的时候不会丢。
    pub fn seed(&mut self, detail: &crate::catalog::objects::ObjectDetail) {
        use crate::catalog::objects::ObjectDetail;
        use crate::ddl::model::EventSchedule;
        match detail {
            ObjectDetail::Event(state) => {
                self.name = state.name.clone();
                self.body = state.body.clone();
                self.enabled = state.enabled;
                self.preserve = state.on_completion == crate::ddl::model::EventCompletion::Preserve;
                self.comment = state.comment.clone();
                match &state.schedule {
                    EventSchedule::OneTime { at } => {
                        self.one_time = true;
                        self.at = at.clone();
                    }
                    EventSchedule::Recurring { every, unit, starts, ends } => {
                        self.one_time = false;
                        self.every = every.clone();
                        self.unit = unit.clone();
                        self.starts = starts.clone();
                        self.ends = ends.clone();
                    }
                }
            }
            ObjectDetail::Routine(state) => {
                self.name = state.name.clone();
                self.body = state.body.clone();
                self.returns = state.returns.clone();
                self.comment = state.comment.clone();
                self.params = params_text(&state.params);
            }
            ObjectDetail::Trigger(state) => {
                self.name = state.name.clone();
                self.table = state.table.clone();
                self.body = state.body.clone();
                self.timing_after = state.timing == crate::ddl::model::TriggerTiming::After;
                self.event_kind = state.event;
            }
        }
        self.apply_detail(detail.clone());
        self.cancel();
        self.note.clear();
    }

    /// 只把读回的对象状态记下来：表单、回执与提示都不动。
    ///
    /// 执行之后的刷新走这条路径——服务器状态用来核对，不是用来覆盖用户刚提交的那份草稿，
    /// 也不会把成功提示或失败原因冲掉。首次打开仍走 seed()，那时才要把定义填进表单。
    pub fn apply_detail(&mut self, detail: crate::catalog::objects::ObjectDetail) {
        self.detail = Some(detail);
        self.detail_error.clear();
    }

    /// 把表单变成动作。动作名与目标名都来自这份草稿，SQL 由 ddl 计划层生成。
    pub fn action(&self) -> Result<crate::ddl::Action, String> {
        use crate::ddl::model::{Action, EventCompletion, EventDraft, EventSchedule, RoutineDraft, RoutineKind, TriggerDraft, TriggerTiming};
        if self.name.trim().is_empty() {
            return Err("先填对象名。".to_owned());
        }
        let original = self.detail.as_ref().map(|detail| detail.name().to_owned());
        match self.kind {
            ObjectKind::Event => {
                let schedule = if self.one_time {
                    EventSchedule::OneTime { at: self.at.clone() }
                } else {
                    EventSchedule::Recurring {
                        every: self.every.clone(),
                        unit: self.unit.clone(),
                        starts: self.starts.clone(),
                        ends: self.ends.clone(),
                    }
                };
                let event = EventDraft {
                    original: original.clone(),
                    name: self.name.trim().to_owned(),
                    schedule,
                    body: self.body.clone(),
                    enabled: self.enabled,
                    on_completion: if self.preserve { EventCompletion::Preserve } else { EventCompletion::NotPreserve },
                    comment: self.comment.clone(),
                };
                Ok(match self.verb {
                    EditorVerb::Save if original.is_some() => Action::AlterEvent { event },
                    EditorVerb::Save => Action::CreateEvent { event },
                    EditorVerb::Drop => Action::DropEvent { event: self.name.trim().to_owned() },
                    EditorVerb::Enable => Action::SetEventEnabled { event: self.name.trim().to_owned(), enabled: true },
                    EditorVerb::Disable => Action::SetEventEnabled { event: self.name.trim().to_owned(), enabled: false },
                })
            }
            ObjectKind::Function | ObjectKind::Procedure => {
                let kind = if self.kind == ObjectKind::Function { RoutineKind::Function } else { RoutineKind::Procedure };
                let routine = RoutineDraft {
                    kind,
                    original: original.clone(),
                    name: self.name.trim().to_owned(),
                    params: parse_params(&self.params, kind)?,
                    returns: if kind == RoutineKind::Function { self.returns.clone() } else { String::new() },
                    body: self.body.clone(),
                    comment: self.comment.clone(),
                };
                Ok(match self.verb {
                    // 修改已有例程是 DROP + CREATE；新建就是 CREATE。
                    EditorVerb::Save if original.is_some() => Action::ReplaceRoutine { routine },
                    EditorVerb::Save => Action::CreateRoutine { routine },
                    EditorVerb::Drop => Action::DropRoutine { routine_kind: kind, name: self.name.trim().to_owned() },
                    _ => return Err("例程没有启停动作。".to_owned()),
                })
            }
            ObjectKind::Trigger => {
                let trigger = TriggerDraft {
                    original: original.clone(),
                    name: self.name.trim().to_owned(),
                    table: self.table.trim().to_owned(),
                    timing: if self.timing_after { TriggerTiming::After } else { TriggerTiming::Before },
                    event: self.event_kind,
                    body: self.body.clone(),
                };
                Ok(match self.verb {
                    EditorVerb::Save if original.is_some() => Action::ReplaceTrigger { trigger },
                    EditorVerb::Save => Action::CreateTrigger { trigger },
                    EditorVerb::Drop => Action::DropTrigger { trigger: self.name.trim().to_owned() },
                    _ => return Err("触发器没有启停动作。".to_owned()),
                })
            }
            _ => Err("这里只编辑事件、函数、存储过程与触发器。".to_owned()),
        }
    }

    /// 预览过的语句：确认时提交给执行链，与计划逐字比对。
    pub fn confirmed_sql(&self) -> Option<Vec<String>> {
        self.plan.as_ref().map(|plan| plan.steps.iter().map(|step| step.sql.clone()).collect())
    }
}

/// 参数原文：每行一个「模式 名字 类型」。函数的参数不写模式。
pub fn params_text(params: &[crate::ddl::model::RoutineParamState]) -> String {
    use crate::ddl::model::ParamMode;
    params
        .iter()
        .map(|param| {
            let mode = match param.mode {
                ParamMode::In => "",
                ParamMode::Out => "OUT ",
                ParamMode::InOut => "INOUT ",
            };
            format!("{mode}{} {}", param.name, param.type_name)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 解析参数行。空行跳过；缺名字或缺类型时给出具体行号，不把半行参数发给服务器。
pub fn parse_params(text: &str, kind: crate::ddl::model::RoutineKind) -> Result<Vec<crate::ddl::model::RoutineParamDraft>, String> {
    use crate::ddl::model::{ParamMode, RoutineKind, RoutineParamDraft};
    let mut params = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut words = line.split_whitespace();
        let first = words.next().unwrap_or_default();
        let (mode, name) = match first.to_ascii_uppercase().as_str() {
            "IN" => (ParamMode::In, words.next().unwrap_or_default()),
            "OUT" => (ParamMode::Out, words.next().unwrap_or_default()),
            "INOUT" => (ParamMode::InOut, words.next().unwrap_or_default()),
            _ => (ParamMode::In, first),
        };
        if name.is_empty() {
            return Err(format!("第 {} 个参数缺名字。", index + 1));
        }
        if kind == RoutineKind::Function && mode != ParamMode::In {
            return Err(format!("第 {} 个参数：函数的参数只能是 IN。", index + 1));
        }
        let type_name: String = words.collect::<Vec<_>>().join(" ");
        if type_name.is_empty() {
            return Err(format!("第 {} 个参数缺类型。", index + 1));
        }
        params.push(RoutineParamDraft { mode, name: name.to_owned(), type_name });
    }
    Ok(params)
}
// ---------------------------------------------------------------- 回执与失败判定

/// 逐步回执是否算完成：被拦下、失败、跳过、未知、回滚都不算。
///
/// 只有全部 Ok 才允许显示统一的“已执行”，与结构页、管理页一致。
pub fn receipt_all_ok(receipt: &crate::ddl::Receipt) -> bool {
    // 和结构页、管理页、危险操作层同一处判定：只有每一条都 Ok 才算完成。
    crate::app::views::structure::receipt::ReceiptOutcome::of(receipt).is_success()
}

/// 一步的执行状态：用固定的技术标识，不跟着界面语言变，免得和服务端原文混在一起看不懂。
pub fn step_status_text(status: crate::ddl::StepStatus, locale: Locale) -> &'static str {
    crate::app::views::structure::receipt::step_text(status, locale)
}

/// 执行失败后要不要保留草稿：除了“只删除成功”，其它结果都留着。
///
/// 删除成功时对象已经不在了，草稿再留着只会让人以为还能再删一次。
pub fn keeps_draft(verb: EditorVerb, receipt: Option<&crate::ddl::Receipt>) -> bool {
    match (verb, receipt) {
        (EditorVerb::Drop, Some(receipt)) => {
            crate::app::views::structure::receipt::ReceiptOutcome::of(receipt).keeps_draft()
        }
        _ => true,
    }
}

/// 一次执行的结局。只有这三种，界面据此决定留什么、读什么、关不关。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecuteOutcome {
    /// 全部步骤 Ok：这次改动已经在服务器上生效。
    Done,
    /// 执行链直接返回错误，连逐步回执都没有。
    Failed,
    /// 有回执但没全部成功：可能做了一半（例如 DROP 成功、CREATE 失败）。
    Partial,
}

impl ExecuteOutcome {
    /// 这个结局自带的说明文案键。失败没有：原始错误由调用方原样写进提示。
    pub fn note_key(self) -> Option<&'static str> {
        match self {
            ExecuteOutcome::Done => Some("structure.preview.done"),
            ExecuteOutcome::Partial => Some("objectEditor.partial"),
            ExecuteOutcome::Failed => None,
        }
    }
}

/// 执行结束后界面该做什么。纯决策：不碰会话、不发起 IO，可以直接单测。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AfterExecute {
    pub outcome: ExecuteOutcome,
    /// 读回对象真实定义，且只更新 detail：不清表单、不清回执、不动提示。
    pub refresh_detail: bool,
    /// 表单草稿留着：失败后还能改了再试。为 false 时窗口收起——那是对象已经不存在的删除成功。
    pub keep_draft: bool,
    /// 让外壳刷新这个对象：对象树与定义页跟着变。
    pub changed: bool,
}

/// 执行后的决策。`receipt` 为 None 表示执行链直接报错，没有逐步回执。
///
/// 留不留草稿只由 keeps_draft 推出来，界面不再另写一套「什么时候算失败」，
/// 免得两处判断各说一套、一半成功时窗口照样被收掉。
pub fn after_execute(verb: EditorVerb, receipt: Option<&crate::ddl::Receipt>) -> AfterExecute {
    let keep_draft = keeps_draft(verb, receipt);
    let Some(receipt) = receipt else {
        // 服务器或连接层直接拒绝：一行输入都没丢，读回真实状态用来核对，不关窗口。
        return AfterExecute { outcome: ExecuteOutcome::Failed, refresh_detail: true, keep_draft, changed: false };
    };
    if receipt_all_ok(receipt) {
        // 全部成功：保存要读回生效后的定义；删除后对象已经不在，读了只会得到错误。
        return AfterExecute { outcome: ExecuteOutcome::Done, refresh_detail: keep_draft, keep_draft, changed: true };
    }
    // 没全部成功：回执和草稿都留着，还要读回真实状态——可能只做了一半。
    AfterExecute { outcome: ExecuteOutcome::Partial, refresh_detail: true, keep_draft, changed: true }
}

#[cfg(test)]
mod receipt_tests {
    use super::*;
    use crate::ddl::model::StepReceipt;
    use crate::ddl::{Receipt, StepStatus};

    fn step(status: StepStatus) -> StepReceipt {
        StepReceipt { sql: "CREATE EVENT e ON SCHEDULE AT CURRENT_TIMESTAMP DO SELECT 1".to_owned(), label: "create".to_owned(), status, message: String::new() }
    }

    fn receipt(steps: Vec<StepReceipt>) -> Receipt {
        Receipt { blocked: Vec::new(), rule: String::new(), steps, target: "app.e".to_owned(), rebuild: false }
    }

    /// 只有全部 Ok 才算完成：DROP + CREATE 的两步里第二步失败时不能显示“已执行”。
    #[test]
    fn only_all_ok_steps_count_as_done() {
        assert!(receipt_all_ok(&receipt(vec![step(StepStatus::Ok)])));
        assert!(receipt_all_ok(&receipt(vec![step(StepStatus::Ok), step(StepStatus::Ok)])));
        for status in [StepStatus::Failed, StepStatus::Skipped, StepStatus::Unknown, StepStatus::RolledBack] {
            assert!(!receipt_all_ok(&receipt(vec![step(StepStatus::Ok), step(status)])), "{status:?}");
        }
        let mut blocked = receipt(Vec::new());
        blocked.blocked.push("目标表不存在".to_owned());
        assert!(!receipt_all_ok(&blocked), "被拦下不算执行过");
        assert!(!receipt_all_ok(&receipt(Vec::new())), "没有执行任何语句不算完成");
    }

    /// 失败保留草稿，删除成功才收起表单。
    #[test]
    fn drafts_stay_unless_the_drop_succeeded() {
        let ok = receipt(vec![step(StepStatus::Ok)]);
        let failed = receipt(vec![step(StepStatus::Failed)]);
        assert!(keeps_draft(EditorVerb::Save, Some(&failed)));
        assert!(keeps_draft(EditorVerb::Drop, Some(&failed)));
        assert!(!keeps_draft(EditorVerb::Drop, Some(&ok)));
        assert!(keeps_draft(EditorVerb::Save, Some(&ok)), "保存成功后表单留着，继续改同一份定义");
        assert!(keeps_draft(EditorVerb::Drop, None), "还没执行，草稿当然留着");
    }

    /// 执行后的决策：成功只读回不回填，失败留草稿，部分完成留回执并读回真实状态。
    #[test]
    fn the_outcome_decides_what_survives_the_execution() {
        let ok = receipt(vec![step(StepStatus::Ok)]);
        let half = receipt(vec![step(StepStatus::Ok), step(StepStatus::Failed)]);
        let mut blocked = receipt(Vec::new());
        blocked.blocked.push("目标表不存在".to_owned());

        // 保存成功：提示与回执留着，只读回真实定义；窗口不收，外壳刷新。
        let saved = after_execute(EditorVerb::Save, Some(&ok));
        assert_eq!(saved.outcome, ExecuteOutcome::Done);
        assert!(saved.keep_draft, "保存成功后表单留着继续改同一份定义");
        assert!(saved.refresh_detail && saved.changed, "保存后读回生效后的定义");

        // 删除成功：对象已经不在，草稿不该留（界面据此收窗口），也不用再读。
        let dropped = after_execute(EditorVerb::Drop, Some(&ok));
        assert_eq!(dropped.outcome, ExecuteOutcome::Done);
        assert!(!dropped.keep_draft, "只有删除成功才收起表单");
        assert!(!dropped.refresh_detail);

        // 执行链直接报错：没有回执，草稿全留，读回真实状态核对。
        let failed = after_execute(EditorVerb::Save, None);
        assert_eq!(failed.outcome, ExecuteOutcome::Failed);
        assert!(failed.keep_draft && failed.refresh_detail);
        assert_eq!(failed.outcome.note_key(), None, "原始错误由调用方写进提示，不换文案");
        assert!(after_execute(EditorVerb::Drop, None).keep_draft, "没执行过当然留着草稿");
        assert!(after_execute(EditorVerb::Drop, None).refresh_detail);

        // 没全部成功：回执与草稿都留，窗口不收，照样读回真实状态。
        for (label, failing) in [("第二步失败", &half), ("被拦下", &blocked)] {
            let partial = after_execute(EditorVerb::Save, Some(failing));
            assert_eq!(partial.outcome, ExecuteOutcome::Partial, "{label}");
            assert!(partial.keep_draft && partial.refresh_detail, "{label}");
            let partial_drop = after_execute(EditorVerb::Drop, Some(failing));
            assert!(partial_drop.keep_draft, "{label} 删除失败也留草稿");
        }

        // 结局自带的文案在两种语言里都有，不会把键名显示给用户。
        for outcome in [ExecuteOutcome::Done, ExecuteOutcome::Partial] {
            let key = outcome.note_key().expect("有文案键");
            for locale in [Locale::ZhCn, Locale::En] {
                let text = i18n::text(locale, key);
                assert!(!text.is_empty(), "{key}");
                assert_ne!(text, key, "{key} 缺文案");
            }
        }
    }

    /// 执行后读回只更新 detail：成功提示、回执与用户刚提交的草稿都不动。
    #[test]
    fn reading_back_after_execute_keeps_form_receipt_and_note() {
        use crate::catalog::objects::ObjectDetail;
        use crate::ddl::model::EventSchedule;
        let mut session = ObjectEditorSession {
            kind: ObjectKind::Event,
            name: "purge".to_owned(),
            database: "app".to_owned(),
            body: "DELETE FROM sessions".to_owned(),
            note: "已按预览执行。".to_owned(),
            receipt: Some(receipt(vec![step(StepStatus::Ok)])),
            ..ObjectEditorSession::default()
        };
        // 服务器改写过的定义（主体、注释都不同）只进 detail，不进表单。
        let state = crate::ddl::model::EventState {
            name: "purge".to_owned(),
            schedule: EventSchedule::OneTime { at: "2026-01-01 00:00:00".to_owned() },
            body: "DELETE FROM sessions WHERE 1".to_owned(),
            enabled: false,
            on_completion: crate::ddl::model::EventCompletion::Preserve,
            comment: "服务器上的注释".to_owned(),
            definer: String::new(),
            scheduler_running: None,
        };
        session.apply_detail(ObjectDetail::Event(state));
        assert_eq!(session.body, "DELETE FROM sessions", "表单是用户刚提交的那份");
        assert_eq!(session.note, "已按预览执行。", "成功提示不能被读回清掉");
        assert!(session.receipt.is_some(), "回执不能被读回清掉");
        assert!(session.is_edit(), "读回之后按改已有对象处理");
        assert!(session.detail_error.is_empty());
    }

    /// 每类回执状态都有可读文本，不会把枚举名直接扔给用户。
    #[test]
    fn every_step_status_has_text() {
        for status in [StepStatus::Ok, StepStatus::Failed, StepStatus::Skipped, StepStatus::Unknown, StepStatus::RolledBack] {
            let text = step_status_text(status, Locale::ZhCn);
            assert!(!text.is_empty());
            assert!(!text.contains("StepStatus"));
        }
    }

    /// 启停是独立动作；例程与触发器没有启停，不能把这几类混成一个动作。
    #[test]
    fn verbs_map_to_the_right_object_actions() {
        use crate::ddl::Action;
        let mut event = ObjectEditorSession { kind: ObjectKind::Event, name: "purge".to_owned(), ..ObjectEditorSession::default() };
        event.verb = EditorVerb::Enable;
        assert!(matches!(event.action(), Ok(Action::SetEventEnabled { enabled: true, .. })));
        event.verb = EditorVerb::Disable;
        assert!(matches!(event.action(), Ok(Action::SetEventEnabled { enabled: false, .. })));
        // 新建是 CREATE，读到原对象之后才是 ALTER。
        event.verb = EditorVerb::Save;
        assert!(matches!(event.action(), Ok(Action::CreateEvent { .. })));

        let mut routine = ObjectEditorSession { kind: ObjectKind::Procedure, name: "p".to_owned(), ..ObjectEditorSession::default() };
        routine.verb = EditorVerb::Enable;
        assert!(routine.action().is_err(), "例程没有启停动作");
        let mut trigger = ObjectEditorSession { kind: ObjectKind::Trigger, name: "t".to_owned(), ..ObjectEditorSession::default() };
        trigger.verb = EditorVerb::Disable;
        assert!(trigger.action().is_err(), "触发器没有启停动作");

        // 没有名字不生成任何语句：空草稿不会变成一条没有对象名的 CREATE。
        let blank = ObjectEditorSession { kind: ObjectKind::Event, ..ObjectEditorSession::default() };
        assert!(blank.action().is_err());
    }

    /// 参数原文按行解析：函数的参数只能是 IN，缺名字或类型时指出第几行。
    #[test]
    fn parameter_lines_are_parsed_with_the_routine_kind() {
        use crate::ddl::model::{ParamMode, RoutineKind};
        let params = parse_params("IN a INT\nOUT b VARCHAR(16)\n  \nINOUT c DECIMAL(10,2)", RoutineKind::Procedure).expect("过程参数");
        assert_eq!(params.len(), 3);
        assert_eq!(params[0].mode, ParamMode::In);
        assert_eq!(params[1].mode, ParamMode::Out);
        assert_eq!(params[2].mode, ParamMode::InOut);
        assert_eq!(params[2].type_name, "DECIMAL(10,2)");
        assert!(parse_params("OUT b INT", RoutineKind::Function).is_err(), "函数参数只能是 IN");
        assert!(parse_params("a", RoutineKind::Procedure).is_err(), "缺类型");
        assert!(parse_params("IN INT", RoutineKind::Procedure).is_err(), "缺名字");

        // 读回的参数原文里函数没有模式前缀。
        let state = vec![crate::ddl::model::RoutineParamState { position: 1, mode: ParamMode::In, name: "a".to_owned(), type_name: "INT".to_owned() }];
        assert_eq!(params_text(&state), "a INT");
    }

    /// 调度器状态只有事件有；未知不等于“会运行”。
    #[test]
    fn scheduler_state_is_reported_only_when_it_was_read() {
        use crate::catalog::objects::ObjectDetail;
        use crate::ddl::model::{EventSchedule, EventState, RoutineKind, RoutineState};
        let event = EventState {
            name: "purge".to_owned(),
            schedule: EventSchedule::OneTime { at: "2026-01-01 00:00:00".to_owned() },
            body: "SELECT 1".to_owned(),
            enabled: true,
            on_completion: crate::ddl::model::EventCompletion::Preserve,
            comment: String::new(),
            definer: String::new(),
            scheduler_running: Some(false),
        };
        assert_eq!(ObjectDetail::Event(event).scheduler_running(), Some(false));
        let routine = RoutineState {
            kind: RoutineKind::Function,
            name: "f".to_owned(),
            params: Vec::new(),
            returns: "INT".to_owned(),
            body: "RETURN 1".to_owned(),
            ..RoutineState::default()
        };
        assert_eq!(ObjectDetail::Routine(routine).scheduler_running(), None, "例程没有调度器");
    }

    #[gpui_kit::test]
    fn object_editor_renders_segmented_kind_and_form_controls(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::AppContext as _;
        use gpui_kit::test::TestWindowExt;
        use std::sync::Arc;
        use std::time::{SystemTime, UNIX_EPOCH};

        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
        let dir = std::env::temp_dir().join(format!("rillsql-object-editor-test-{stamp}"));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let services = Arc::new(crate::application::AppServices::new(Arc::new(
            crate::connections::Service::new_for_test(dir.join("connections.sqlite")),
        )));

        cx.update(gpui_kit::init);
        let (window, pane) = cx.update(|cx| {
            let bounds = gpui_kit::Bounds {
                origin: gpui_kit::Point::default(),
                size: gpui_kit::size(gpui_kit::px(900.), gpui_kit::px(700.)),
            };
            let (window, pane) = gpui_kit::open_window(
                gpui_kit::WindowOptions { window_bounds: Some(gpui_kit::WindowBounds::Windowed(bounds)), ..Default::default() },
                cx,
                |_window, cx| {
                    cx.new(|cx| {
                        let mut pane = ObjectEditorPane::new(services.clone());
                        pane.open_request(EditorRequest::blank("conn-1", "app", "", "mysql", ObjectKind::Event), cx);
                        pane
                    })
                },
            )
            .expect("object editor window");
            (window, pane)
        });

        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("object-editor-kind-event").visible(), "事件分段页签可见");
            assert!(window.find("object-editor-kind-trigger").visible(), "触发器分段页签可见");
            assert!(window.find("object-editor-one-time").visible(), "AT 分段选项可见");
            assert!(window.find("object-editor-recurring").visible(), "EVERY 分段选项可见");
            assert!(window.find("object-editor-body").visible(), "主体编辑区可见");
        })
        .unwrap();

        cx.update_window(window, |_, window, cx| {
            pane.update(cx, |pane, cx| pane.choose_kind(ObjectKind::Trigger, cx));
            window.render_frame(cx);
            assert!(window.find("object-editor-before").visible(), "BEFORE 分段选项可见");
            assert!(window.find("object-editor-after").visible(), "AFTER 分段选项可见");
            assert!(window.find("object-editor-insert").visible(), "INSERT 分段选项可见");
            assert!(window.find("object-editor-update").visible(), "UPDATE 分段选项可见");
            assert!(window.find("object-editor-delete").visible(), "DELETE 分段选项可见");
        })
        .unwrap();

        let _ = std::fs::remove_dir_all(&dir);
    }
}
