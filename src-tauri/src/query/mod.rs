//! 查询与历史：在已打开的会话上执行、取消，并只在用户发起执行时写入本机历史。
//! 按名称保存的查询单独落库，读取、保存、重命名和删除都不执行 SQL。

mod cancel;
pub mod commands;
mod exec;
mod history;
mod saved;
mod split;

pub use commands::{
    query_cancel, query_history, query_history_clear, query_poll, query_saved_delete, query_saved_list, query_saved_rename, query_saved_save, query_start, QueryState,
};
pub use split::split_sql;

pub fn module_id() -> &'static str {
    let _linked = (
        std::any::type_name_of_val(&query_start),
        std::any::type_name_of_val(&query_poll),
        std::any::type_name_of_val(&query_cancel),
        std::any::type_name_of_val(&query_history),
        std::any::type_name_of_val(&query_history_clear),
        std::any::type_name_of_val(&query_saved_list),
        std::any::type_name_of_val(&query_saved_save),
        std::any::type_name_of_val(&query_saved_rename),
        std::any::type_name_of_val(&query_saved_delete),
        std::any::type_name_of_val(&split_sql),
        std::any::type_name_of_val(&cancel::deliver_cancel),
        std::any::type_name_of_val(&exec::run_batch),
    );
    let _ = _linked;
    "query"
}

#[cfg(test)]
mod tests;
