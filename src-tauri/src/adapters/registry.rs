//! 适配器注册表：唯一允许对 `Engine` 穷举匹配、以及把引擎字符串解析成 `Engine` 的地方。

use super::{mariadb, mongodb, mysql, postgres, redis, sqlite, sqlserver, EngineAdapter};
use crate::connections::model::Engine;

/// 全部已注册引擎（顺序即界面默认展示顺序）。注册表完整性测试校验它覆盖 `Engine` 的每个变体。
pub const ENGINES: [Engine; 7] = [
    Engine::Mysql,
    Engine::Mariadb,
    Engine::Postgres,
    Engine::Sqlite,
    Engine::Sqlserver,
    Engine::Redis,
    Engine::Mongodb,
];

/// 取得引擎的适配器。
pub fn adapter(engine: Engine) -> &'static dyn EngineAdapter {
    match engine {
        Engine::Mysql => &mysql::MysqlAdapter,
        Engine::Mariadb => &mariadb::MariadbAdapter,
        Engine::Postgres => &postgres::PostgresAdapter,
        Engine::Sqlite => &sqlite::SqliteAdapter,
        Engine::Sqlserver => &sqlserver::SqlserverAdapter,
        Engine::Redis => &redis::RedisAdapter,
        Engine::Mongodb => &mongodb::MongodbAdapter,
    }
}

/// 按稳定 id（持久化/DTO 字符串）解析引擎。
pub fn parse(id: &str) -> Option<Engine> {
    ENGINES.into_iter().find(|engine| adapter(*engine).meta().id() == id)
}
