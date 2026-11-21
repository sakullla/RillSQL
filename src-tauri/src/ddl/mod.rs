//! 在已经打开的会话上预览并逐条执行结构变更。不新开数据库连接。

pub mod commands;
pub(crate) mod english;
pub(crate) mod exec;
pub(crate) mod introspect;
pub(crate) mod model;
pub(crate) mod naming;
pub(crate) mod plan;
pub(crate) mod sql;
pub(crate) mod templates;

pub(crate) use exec::Receipt;
pub(crate) use introspect::{load, load_comments, load_partial};
#[allow(unused_imports)]
pub(crate) use model::{
    Action, Column, ColumnDraft, DefaultMode, DefaultSpec, DependentSql, Engine as DdlEngine, ForeignKey, ForeignKeyDraft, IncomingForeignKey, Index,
    IndexColumn, IndexColumnDraft, Plan, Request, SequenceState, Snapshot, StepStatus, TableOptions,
};
pub(crate) use plan::plan as preview_plan;
pub(crate) use plan::{fk_sql_for_display, index_sql_for_display, is_no_change, rebuilt_definition};

pub fn module_id() -> &'static str {
    let _linked = (
        std::any::type_name_of_val(&commands::ddl_describe),
        std::any::type_name_of_val(&commands::ddl_preview),
        std::any::type_name_of_val(&commands::ddl_execute),
        std::any::type_name_of_val(&plan::plan),
        std::any::type_name_of_val(&exec::preview),
        std::any::type_name_of_val(&exec::execute),
        std::any::type_name_of_val(&load),
        std::any::type_name_of_val(&load_partial),
    );
    "ddl"
}

#[cfg(test)]
mod tests;
