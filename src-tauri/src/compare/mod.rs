//! 同引擎结构与数据对比。只生成脚本，不执行。

pub mod commands;
mod data;
pub(crate) mod model;
mod read;
pub(crate) mod structure;

pub use data::diff_data;
pub use structure::{diff_structure, pair_data_refusal};
#[allow(unused_imports)]
pub(crate) use model::{
    DataDiff, DataReport, DataRequest, DataTableRequest, DataTableSummary, DiffItem, FieldDiff, KeyChoice, SideRequest, StructureReport, StructureRequest, TableChoice, TablePair,
};
pub(crate) use read::{
    is_cancelled as compare_is_cancelled, list_schemas, progress_of as compare_progress, read_data, read_structure, request_cancel as request_compare_cancel, timeout_message,
    Progress as CompareProgress, Stage as CompareStage,
};

pub fn module_id() -> &'static str {
    let _linked = (
        std::any::type_name_of_val(&commands::compare_structure),
        std::any::type_name_of_val(&commands::compare_data),
        std::any::type_name_of_val(&commands::compare_cancel),
        std::any::type_name_of_val(&commands::compare_schemas),
        std::any::type_name_of_val(&diff_structure),
        std::any::type_name_of_val(&diff_data),
    );
    "compare"
}

#[cfg(test)]
mod tests;
