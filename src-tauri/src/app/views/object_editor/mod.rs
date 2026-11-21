//! 对象编辑器：事件、函数/存储过程与触发器的统一编辑入口。
//!
//! 这里只放编辑器的目标、草稿状态与类型登记：表单渲染、预览确认与结果展示复用既有的
//! DDL 计划（plan）与 Receipt 视图，不另建执行或 SQL 拼接路径。各对象的语句生成在
//! adapters/mysql/object_ddl.rs，读回在 adapters/mysql/object_read.rs。

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
