//! 在已经打开的会话上预览并逐条执行结构变更。不新开数据库连接。

pub mod commands;
mod exec;
mod introspect;
mod model;
mod plan;
mod sql;

pub fn module_id() -> &'static str {
    let _linked = (
        std::any::type_name_of_val(&commands::ddl_describe),
        std::any::type_name_of_val(&commands::ddl_preview),
        std::any::type_name_of_val(&commands::ddl_execute),
        std::any::type_name_of_val(&plan::plan),
        std::any::type_name_of_val(&exec::preview),
        std::any::type_name_of_val(&exec::execute),
    );
    "ddl"
}

#[cfg(test)]
mod tests;
