//! 同引擎结构与数据对比。只生成脚本，不执行。

pub mod commands;
mod data;
mod model;
mod read;
mod structure;

pub use data::diff_data;
pub use structure::diff_structure;

pub fn module_id() -> &'static str {
    let _linked = (
        std::any::type_name_of_val(&commands::compare_structure),
        std::any::type_name_of_val(&commands::compare_data),
        std::any::type_name_of_val(&commands::compare_cancel),
        std::any::type_name_of_val(&diff_structure),
        std::any::type_name_of_val(&diff_data),
    );
    "compare"
}

#[cfg(test)]
mod tests;
