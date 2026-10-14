//! 查询与历史：在已打开的会话上执行、取消，并只在用户发起执行时写入本机历史。

mod cancel;
pub mod commands;
mod exec;
mod history;
mod split;

pub use commands::{query_cancel, query_history, query_history_clear, query_poll, query_start, QueryState};

pub fn module_id() -> &'static str {
    let _linked = (
        std::any::type_name_of_val(&query_start),
        std::any::type_name_of_val(&query_poll),
        std::any::type_name_of_val(&query_cancel),
        std::any::type_name_of_val(&query_history),
        std::any::type_name_of_val(&query_history_clear),
        std::any::type_name_of_val(&split::split_sql),
        std::any::type_name_of_val(&cancel::deliver_cancel),
        std::any::type_name_of_val(&exec::run_batch),
    );
    let _ = _linked;
    "query"
}

#[cfg(test)]
mod tests;
