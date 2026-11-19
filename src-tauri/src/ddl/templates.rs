//! 常用字段模板（R19 / ADR-7）：内置模板（时间戳、软删除、状态备注、UUID）按引擎的类型规则展开，可隐藏；
//! 用户模板含多个字段与适用引擎，持久化在应用状态（`state::AppState::column_templates`）。
//! 新建表与修改表的“常用字段”菜单都从 [`TemplateStore::applicable`] 取同一份列表。

use serde::{Deserialize, Serialize};

use crate::adapters::ddl::types_named;
use crate::adapters::registry;

use super::model::{ColumnDraft, DefaultSpec};

/// 模板里的一个字段。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateColumn {
    pub name: String,
    pub type_name: String,
    pub nullable: bool,
    #[serde(default)]
    pub default: DefaultSpec,
    #[serde(default)]
    pub comment: String,
}

impl TemplateColumn {
    pub fn to_draft(&self) -> ColumnDraft {
        ColumnDraft {
            original: None,
            name: self.name.trim().to_owned(),
            type_name: self.type_name.trim().to_owned(),
            nullable: self.nullable,
            default: self.default.clone(),
            comment: self.comment.trim().to_owned(),
            primary: false,
            auto: false,
            dropped: false,
        }
    }
}

/// 一个常用字段模板。`engines` 为空表示适用于所有引擎；否则按引擎 id 匹配。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnTemplate {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub engines: Vec<String>,
    pub columns: Vec<TemplateColumn>,
}

impl ColumnTemplate {
    pub fn is_builtin(&self) -> bool {
        is_builtin_id(&self.id)
    }

    /// 适用于这个引擎（引擎字符串按注册表解析后比较）。
    pub fn applies_to(&self, engine: &str) -> bool {
        if self.engines.is_empty() {
            return true;
        }
        let wanted = registry::parse(engine);
        self.engines.iter().any(|item| match (registry::parse(item), wanted) {
            (Some(left), Some(right)) => left == right,
            _ => item.eq_ignore_ascii_case(engine.trim()),
        })
    }

    pub fn drafts(&self) -> Vec<ColumnDraft> {
        self.columns.iter().map(TemplateColumn::to_draft).collect()
    }
}

/// 模板校验不通过的原因（i18n 键）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TemplateProblem {
    EmptyName,
    DuplicateName,
    NoColumns,
    EmptyColumn,
    DuplicateColumn,
    EmptyType,
}

impl TemplateProblem {
    pub fn key(self) -> &'static str {
        match self {
            TemplateProblem::EmptyName => "structure.templates.err.emptyName",
            TemplateProblem::DuplicateName => "structure.templates.err.duplicateName",
            TemplateProblem::NoColumns => "structure.templates.err.noColumns",
            TemplateProblem::EmptyColumn => "structure.templates.err.emptyColumn",
            TemplateProblem::DuplicateColumn => "structure.templates.err.duplicateColumn",
            TemplateProblem::EmptyType => "structure.templates.err.emptyType",
        }
    }
}

/// 持久化的模板状态：用户模板（按用户排定的顺序）与被隐藏的内置模板 id。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateStore {
    #[serde(default)]
    pub custom: Vec<ColumnTemplate>,
    #[serde(default)]
    pub hidden: Vec<String>,
}

pub const BUILTIN_TIMESTAMPS: &str = "builtin:timestamps";
pub const BUILTIN_SOFT_DELETE: &str = "builtin:softDelete";
pub const BUILTIN_STATUS_REMARK: &str = "builtin:statusRemark";
pub const BUILTIN_UUID: &str = "builtin:uuid";

/// 内置模板 id 与名称的 i18n 键，按菜单顺序。
pub const BUILTIN: [(&str, &str); 4] = [
    (BUILTIN_TIMESTAMPS, "structure.preset.timestamps"),
    (BUILTIN_SOFT_DELETE, "structure.preset.softDelete"),
    (BUILTIN_STATUS_REMARK, "structure.preset.statusRemark"),
    (BUILTIN_UUID, "structure.preset.uuid"),
];

pub fn is_builtin_id(id: &str) -> bool {
    id.starts_with("builtin:")
}

/// 内置模板按引擎的类型规则展开。`name` 是 i18n 键。
pub fn builtin_template(engine: &str, id: &str) -> Option<ColumnTemplate> {
    let (id, key) = BUILTIN.iter().find(|(item, _)| *item == id)?;
    let types = types_named(engine).preset_types();
    let col = |name: &str, kind: &str, nullable: bool, default: DefaultSpec| TemplateColumn {
        name: name.to_owned(),
        type_name: kind.to_owned(),
        nullable,
        default,
        comment: String::new(),
    };
    let expression = || DefaultSpec::Expression(types.timestamp_default.to_owned());
    let columns = match *id {
        BUILTIN_TIMESTAMPS => vec![col("created_at", types.timestamp, false, expression()), col("updated_at", types.timestamp, false, expression())],
        BUILTIN_SOFT_DELETE => vec![col("deleted_at", types.timestamp, true, DefaultSpec::None)],
        BUILTIN_STATUS_REMARK => vec![
            col("status", types.short_text, false, DefaultSpec::Literal("active".to_owned())),
            col("remark", types.note, true, DefaultSpec::None),
        ],
        _ => vec![col("uuid", types.uuid, false, DefaultSpec::None)],
    };
    Some(ColumnTemplate { id: (*id).to_owned(), name: (*key).to_owned(), engines: Vec::new(), columns })
}

impl TemplateStore {
    /// 菜单里的模板：未隐藏的内置模板在前，随后是适用于该引擎的用户模板（按用户排定的顺序）。
    pub fn applicable(&self, engine: &str) -> Vec<ColumnTemplate> {
        let mut items: Vec<ColumnTemplate> =
            BUILTIN.iter().filter(|(id, _)| !self.is_hidden(id)).filter_map(|(id, _)| builtin_template(engine, id)).collect();
        items.extend(self.custom.iter().filter(|template| template.applies_to(engine)).cloned());
        items
    }

    pub fn is_hidden(&self, id: &str) -> bool {
        self.hidden.iter().any(|item| item == id)
    }

    pub fn set_hidden(&mut self, id: &str, hidden: bool) {
        self.hidden.retain(|item| item != id);
        if hidden && is_builtin_id(id) {
            self.hidden.push(id.to_owned());
        }
    }

    pub fn find(&self, id: &str) -> Option<&ColumnTemplate> {
        self.custom.iter().find(|template| template.id == id)
    }

    /// 新增或更新一个用户模板（按 id）。校验不通过时不改动并给出原因。
    pub fn upsert(&mut self, template: ColumnTemplate) -> Result<(), TemplateProblem> {
        if let Some(problem) = self.problem(&template) {
            return Err(problem);
        }
        let mut template = template;
        template.name = template.name.trim().to_owned();
        for column in &mut template.columns {
            column.name = column.name.trim().to_owned();
            column.type_name = column.type_name.trim().to_owned();
        }
        match self.custom.iter_mut().find(|item| item.id == template.id) {
            Some(slot) => *slot = template,
            None => self.custom.push(template),
        }
        Ok(())
    }

    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.custom.len();
        self.custom.retain(|template| template.id != id);
        before != self.custom.len()
    }

    /// 用户模板上下移动一位。越界时不动，返回是否移动。
    pub fn move_by(&mut self, id: &str, delta: isize) -> bool {
        let Some(index) = self.custom.iter().position(|template| template.id == id) else {
            return false;
        };
        let next = index as isize + delta;
        if next < 0 || next >= self.custom.len() as isize {
            return false;
        }
        self.custom.swap(index, next as usize);
        true
    }

    /// 校验模板：名称非空且不与其它用户模板重名，至少一个字段，字段名非空、不重名，类型非空。
    pub fn problem(&self, template: &ColumnTemplate) -> Option<TemplateProblem> {
        let name = template.name.trim();
        if name.is_empty() {
            return Some(TemplateProblem::EmptyName);
        }
        if self.custom.iter().any(|item| item.id != template.id && item.name.trim().eq_ignore_ascii_case(name)) {
            return Some(TemplateProblem::DuplicateName);
        }
        if template.columns.is_empty() {
            return Some(TemplateProblem::NoColumns);
        }
        let mut seen: Vec<String> = Vec::new();
        for column in &template.columns {
            let column_name = column.name.trim();
            if column_name.is_empty() {
                return Some(TemplateProblem::EmptyColumn);
            }
            if column.type_name.trim().is_empty() {
                return Some(TemplateProblem::EmptyType);
            }
            let lower = column_name.to_lowercase();
            if seen.contains(&lower) {
                return Some(TemplateProblem::DuplicateColumn);
            }
            seen.push(lower);
        }
        None
    }
}

/// 把模板字段加进草稿：与已有字段重名（不分大小写）的跳过，返回 `(加入的字段名, 跳过的字段名)`。
pub fn apply_template(drafts: &mut Vec<ColumnDraft>, template: &ColumnTemplate) -> (Vec<String>, Vec<String>) {
    let mut added = Vec::new();
    let mut skipped = Vec::new();
    for draft in template.drafts() {
        if drafts.iter().any(|existing| !existing.dropped && existing.name.trim().eq_ignore_ascii_case(draft.name.trim())) {
            skipped.push(draft.name.clone());
        } else {
            added.push(draft.name.clone());
            drafts.push(draft);
        }
    }
    (added, skipped)
}
