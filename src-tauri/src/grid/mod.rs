//! 表数据浏览、逐条提交和 CSV 导出。没有真实连接时由夹具执行。

mod export;
mod sql;
mod store;

pub fn module_id() -> &'static str {
    let _linked = (
        std::any::type_name::<export::ExportPlan>(),
        std::any::type_name::<store::TableStore>(),
        std::any::type_name_of_val(&sql::select_export_gate),
        std::any::type_name_of_val(&store::query_page),
        std::any::type_name_of_val(&store::commit_changes),
        std::any::type_name_of_val(&export::plan_export),
        std::any::type_name_of_val(&export::touch_api),
        std::any::type_name_of_val(&export::start_export),
        std::any::type_name_of_val(&store::customers_fixture),
        std::any::type_name_of_val(&store::nokey_fixture),
        std::any::type_name_of_val(&store::touch_api),
    );
    let _ = store::touch_api();
    let _ = export::touch_api();
    "grid"
}

#[cfg(test)]
mod tests;
