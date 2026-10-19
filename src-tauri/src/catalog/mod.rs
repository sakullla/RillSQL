//! 按引擎读取库表：PostgreSQL 有 schema，MySQL / MariaDB 只有库，SQLite 直接列对象。

pub mod commands;
mod model;
mod read;
mod sqlite;
mod tree;

pub use model::{
    kind_sql, metadata_sql, schema_status, Engine, SchemaNode, SchemaStatus, DEFAULT_OBJECT_PAGE,
};
pub use sqlite::{read_sqlite_file, read_sqlite_pool};
pub use commands::{catalog_definition, catalog_page, catalog_refresh, catalog_sessions};
pub use tree::{
    direct_objects, object_path, reconcile_objects, refresh_connection, refresh_schema,
    search_objects, take_page, visible_database_names, visible_schema_names,
};

pub fn module_id() -> &'static str {
    let _linked = (
        std::any::type_name::<SchemaNode>(),
        std::any::type_name_of_val(&metadata_sql),
        std::any::type_name_of_val(&schema_status),
        std::any::type_name_of_val(&read_sqlite_file),
        std::any::type_name_of_val(&read_sqlite_pool),
        std::any::type_name_of_val(&object_path),
        std::any::type_name_of_val(&reconcile_objects),
        std::any::type_name_of_val(&refresh_schema),
        std::any::type_name_of_val(&refresh_connection),
        std::any::type_name_of_val(&search_objects),
        std::any::type_name_of_val(&take_page),
        std::any::type_name_of_val(&visible_database_names),
        std::any::type_name_of_val(&visible_schema_names),
        std::any::type_name_of_val(&direct_objects),
        std::any::type_name_of_val(&catalog_refresh),
        std::any::type_name_of_val(&catalog_page),
        std::any::type_name_of_val(&catalog_definition),
        std::any::type_name_of_val(&catalog_sessions),
        std::any::type_name_of_val(&read::read_catalog_session),
        std::any::type_name_of_val(&read::read_catalog_outline),
        std::any::type_name_of_val(&read::read_object_page),
        std::any::type_name_of_val(&read::read_definition),
        std::any::type_name_of_val(&kind_sql),
    );
    let _ = (
        DEFAULT_OBJECT_PAGE,
        Engine::Sqlite,
        SchemaStatus::Ready,
        model::ConnStatus::Connecting,
        model::ConnStatus::Error,
        model::ConnStatus::Lost,
        _linked,
    );
    "catalog"
}

#[cfg(test)]
mod tests;
