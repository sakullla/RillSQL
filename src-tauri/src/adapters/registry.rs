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

/// 目录树节点挂到连接之前的占位引擎（建好节点后立即改成所属连接的引擎）。
pub const PLACEHOLDER: Engine = Engine::Sqlite;

/// 查询编辑器没有绑定连接（或绑定的不是 SQL 引擎）时，切分、大纲与导出按这个方言处理。
pub const UNBOUND_DIALECT: Engine = Engine::Postgres;

/// 网格/编辑器目标的引擎字符串解析不出时按这个引擎生成 SQL 与折叠。
pub const UNBOUND_GRID: Engine = Engine::Sqlite;

/// 有 SQL 语法组的引擎原样返回；未知或非 SQL 引擎按 [`UNBOUND_DIALECT`]。
pub fn sql_engine(id: &str) -> Engine {
    parse(id).filter(|engine| adapter(*engine).syntax().is_some()).unwrap_or(UNBOUND_DIALECT)
}

/// 网格目标的引擎：解析不出时按 [`UNBOUND_GRID`]。
pub fn grid_engine(id: &str) -> Engine {
    parse(id).unwrap_or(UNBOUND_GRID)
}

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

/// 连接表单与连接页的引擎排列顺序。
pub const CONNECTION_FORM_ORDER: [Engine; 7] = [
    Engine::Postgres,
    Engine::Mysql,
    Engine::Mariadb,
    Engine::Sqlserver,
    Engine::Sqlite,
    Engine::Redis,
    Engine::Mongodb,
];

/// 按连接串 scheme（小写、不含冒号）找引擎与 scheme 描述。
pub fn by_url_scheme(scheme: &str) -> Option<(Engine, super::UrlScheme)> {
    ENGINES.into_iter().find_map(|engine| {
        adapter(engine).meta().url_schemes().iter().find(|item| item.name == scheme).map(|item| (engine, *item))
    })
}

/// 全部引擎认的连接串 scheme。
pub fn url_schemes() -> impl Iterator<Item = super::UrlScheme> {
    ENGINES.into_iter().flat_map(|engine| adapter(engine).meta().url_schemes().iter().copied())
}

/// 使用某种专项工作台的引擎（徽标等按它取元数据）。
pub fn workbench_engine(workbench: super::Workbench) -> Option<Engine> {
    ENGINES.into_iter().find(|engine| adapter(*engine).meta().workbench() == workbench)
}
