//! MongoDB 集合文档浏览器（R12）：库/集合清单、文档分页（JSON 过滤带定位提示）、文档 CRUD 与删除确认。
//! 不做聚合管道构建器、GridFS 与索引管理 UI；JSON 编辑一律先校验后落库。

pub mod commands;
pub(crate) mod docs;

pub fn module_id() -> &'static str {
    let _linked = (
        std::any::type_name_of_val(&commands::mongo_list_databases),
        std::any::type_name_of_val(&commands::mongo_find_documents),
        std::any::type_name_of_val(&docs::find_documents),
        std::any::type_name::<docs::DocumentsPage>(),
        std::any::type_name::<docs::CountMode>(),
    );
    "mongo"
}

#[cfg(test)]
mod tests;
