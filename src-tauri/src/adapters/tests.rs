//! 适配器层单测：各方言 SqlSyntax 的引号/保留字规则（由原 sql_ident 迁入）与注册表完整性。

use super::registry::{self, ENGINES};
use super::{adapter, qualified, quote_always, quote_ident, quote_ident_named, Workbench};
use crate::connections::model::Engine;

const SQL_ENGINES: [Engine; 5] = [Engine::Mysql, Engine::Mariadb, Engine::Postgres, Engine::Sqlite, Engine::Sqlserver];

/// 新增 `Engine` 变体时这里无法编译，提醒同时登记到 `registry::ENGINES` 与 `registry::adapter`。
fn variant_index(engine: Engine) -> usize {
    match engine {
        Engine::Mysql => 0,
        Engine::Mariadb => 1,
        Engine::Postgres => 2,
        Engine::Sqlite => 3,
        Engine::Sqlserver => 4,
        Engine::Redis => 5,
        Engine::Mongodb => 6,
    }
}

#[test]
fn registry_covers_every_engine_once() {
    let mut seen = [false; 7];
    for engine in ENGINES {
        let index = variant_index(engine);
        assert!(!seen[index], "{engine:?} 在 ENGINES 中重复");
        seen[index] = true;
    }
    assert!(seen.iter().all(|seen| *seen), "ENGINES 未覆盖全部 Engine 变体");
}

#[test]
fn every_engine_has_a_consistent_adapter() {
    let mut ids = Vec::new();
    for engine in ENGINES {
        let adapter = adapter(engine);
        let meta = adapter.meta();
        assert_eq!(meta.engine(), engine, "适配器映射错位");
        assert!(!meta.id().is_empty() && !meta.label().is_empty());
        assert_eq!(registry::parse(meta.id()), Some(engine), "id 必须能解析回同一引擎");
        assert_eq!(Engine::parse(meta.id()), Ok(engine));
        assert_eq!(engine.as_str(), meta.id());
        ids.push(meta.id());

        let caps = adapter.capabilities();
        assert_eq!(caps.sql_syntax_group, adapter.syntax().is_some(), "{engine:?} SqlSyntax 声明与提供不一致");
        assert_eq!(caps.type_rules_group, adapter.types().is_some(), "{engine:?} TypeRules 声明与提供不一致");
        assert_eq!(caps.ddl_group, adapter.ddl().is_some(), "{engine:?} DdlDialect 声明与提供不一致");
        assert_eq!(caps.catalog_group, adapter.catalog().is_some(), "{engine:?} CatalogDialect 声明与提供不一致");
        assert_eq!(caps.error_rules_group, adapter.errors().is_some(), "{engine:?} ErrorRules 声明与提供不一致");
        assert_eq!(caps.connector_group, adapter.connector().is_some(), "{engine:?} Connector 声明与提供不一致");

        // 关系型引擎一定有 SQL 语法，并使用关系型工作台；非关系型反之。
        assert_eq!(caps.relational, caps.sql_syntax_group, "{engine:?} 关系面与 SQL 语法组应一致");
        assert_eq!(caps.relational, meta.workbench() == Workbench::Relational, "{engine:?} 工作台类型与关系面不一致");
        // 文件型引擎没有默认端口，远程引擎都有。
        assert_eq!(meta.file_based(), meta.default_port().is_none(), "{engine:?} 文件型与默认端口不一致");
    }
    let mut unique = ids.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), ids.len(), "引擎 id 重复");
    assert_eq!(registry::parse("unknown"), None);
    assert!(Engine::parse("unknown").is_err());
}

#[test]
fn engine_metadata_is_unchanged_by_the_move() {
    let expected = [
        (Engine::Mysql, "mysql", "MySQL", Some(3306), true),
        (Engine::Mariadb, "mariadb", "MariaDB", Some(3306), true),
        (Engine::Postgres, "postgres", "PostgreSQL", Some(5432), true),
        (Engine::Sqlite, "sqlite", "SQLite", None, true),
        (Engine::Sqlserver, "sqlserver", "SQL Server", Some(1433), true),
        (Engine::Redis, "redis", "Redis", Some(6379), false),
        (Engine::Mongodb, "mongodb", "MongoDB", Some(27017), false),
    ];
    for (engine, id, label, port, relational) in expected {
        assert_eq!(engine.as_str(), id);
        assert_eq!(engine.label_zh(), label);
        assert_eq!(engine.default_port(), port);
        assert_eq!(engine.is_remote(), port.is_some());
        assert_eq!(engine.is_relational(), relational);
    }
    assert_eq!(adapter(Engine::Redis).meta().workbench(), Workbench::Redis);
    assert_eq!(adapter(Engine::Mongodb).meta().workbench(), Workbench::Mongo);
}

#[test]
fn mariadb_is_a_separate_entry_sharing_mysql_syntax() {
    let (mysql, mariadb) = (adapter(Engine::Mysql), adapter(Engine::Mariadb));
    assert_ne!(mysql.meta().id(), mariadb.meta().id());
    let (mysql, mariadb) = (mysql.syntax().unwrap(), mariadb.syntax().unwrap());
    assert!(std::ptr::eq(mysql.reserved_words(), mariadb.reserved_words()), "MariaDB 复用 MySQL 家族保留字表");
    for name in ["content", "order", "_binary", "price$", "we`ird"] {
        assert_eq!(mysql.quote_ident(name), mariadb.quote_ident(name));
    }
}

#[test]
fn reserved_tables_are_sorted_and_unique_for_binary_search() {
    for engine in SQL_ENGINES {
        let list = adapter(engine).syntax().unwrap().reserved_words();
        assert!(!list.is_empty());
        for pair in list.windows(2) {
            assert!(pair[0] < pair[1], "{engine:?}: {} 应排在 {} 之前", pair[0], pair[1]);
        }
        assert!(list.iter().all(|word| *word == word.to_ascii_uppercase()), "{engine:?} 保留字须为大写");
    }
}

#[test]
fn engines_without_sql_quote_with_standard_double_quotes() {
    for engine in [Engine::Redis, Engine::Mongodb] {
        assert!(adapter(engine).syntax().is_none());
        assert_eq!(quote_ident(engine, "content"), "\"content\"");
        assert_eq!(quote_always(engine, "a\"b"), "\"a\"\"b\"");
        assert_eq!(qualified(engine, &["", "db", "k"]), "\"db\".\"k\"");
    }
}

#[test]
fn plain_names_stay_bare_in_every_dialect() {
    for engine in SQL_ENGINES {
        assert_eq!(quote_ident(engine, "content"), "content");
        assert_eq!(quote_ident(engine, "etoe_alarm_rule_set"), "etoe_alarm_rule_set");
        assert_eq!(quote_ident(engine, "e6yun3_etoe_business"), "e6yun3_etoe_business");
    }
    for engine in [Engine::Postgres, Engine::Sqlite, Engine::Sqlserver] {
        assert_eq!(quote_ident(engine, "_tmp1"), "_tmp1");
    }
}

#[test]
fn mysql_family_quotes_leading_underscore_to_avoid_charset_introducers() {
    for engine in [Engine::Mysql, Engine::Mariadb] {
        for name in ["_binary", "_utf8mb4", "_latin1", "_tmp1", "_"] {
            assert_eq!(quote_ident(engine, name), format!("`{name}`"));
        }
        assert_eq!(quote_ident(engine, "tmp_1"), "tmp_1");
        assert_eq!(qualified(engine, &["db", "_binary"]), "db.`_binary`");
    }
}

#[test]
fn reserved_words_keep_quotes_regardless_of_case() {
    for word in ["order", "user", "select", "SELECT", "Order"] {
        assert_eq!(quote_ident(Engine::Mysql, word), format!("`{word}`"));
        assert_eq!(quote_ident(Engine::Mariadb, word), format!("`{word}`"));
        assert_eq!(quote_ident(Engine::Postgres, word), format!("\"{word}\""));
        assert_eq!(quote_ident(Engine::Sqlite, word), format!("\"{word}\""));
        assert_eq!(quote_ident(Engine::Sqlserver, word), format!("[{word}]"));
    }
    assert_eq!(quote_ident(Engine::Mysql, "key"), "`key`");
    assert_eq!(quote_ident(Engine::Sqlserver, "key"), "[key]");
}

#[test]
fn postgres_quotes_names_with_uppercase_only() {
    assert_eq!(quote_ident(Engine::Postgres, "User"), "\"User\"");
    assert_eq!(quote_ident(Engine::Postgres, "OrderItems"), "\"OrderItems\"");
    assert_eq!(quote_ident(Engine::Postgres, "order_items"), "order_items");
    assert_eq!(quote_ident(Engine::Mysql, "OrderItems"), "OrderItems");
    assert_eq!(quote_ident(Engine::Sqlite, "OrderItems"), "OrderItems");
    assert_eq!(quote_ident(Engine::Sqlserver, "OrderItems"), "OrderItems");
}

#[test]
fn leading_digit_special_chars_and_unicode_are_quoted() {
    assert_eq!(quote_ident(Engine::Mysql, "1st"), "`1st`");
    assert_eq!(quote_ident(Engine::Sqlserver, "2024_sales"), "[2024_sales]");
    assert_eq!(quote_ident(Engine::Postgres, "9lives"), "\"9lives\"");
    assert_eq!(quote_ident(Engine::Mysql, "order items"), "`order items`");
    assert_eq!(quote_ident(Engine::Mysql, "a-b"), "`a-b`");
    assert_eq!(quote_ident(Engine::Mysql, "we`ird"), "`we``ird`");
    assert_eq!(quote_ident(Engine::Postgres, "we\"ird"), "\"we\"\"ird\"");
    assert_eq!(quote_ident(Engine::Sqlserver, "we]ird"), "[we]]ird]");
    assert_eq!(quote_ident(Engine::Sqlite, "a.b"), "\"a.b\"");
    assert_eq!(quote_ident(Engine::Mysql, "订单"), "`订单`");
    assert_eq!(quote_ident(Engine::Postgres, "café"), "\"café\"");
    assert_eq!(quote_ident(Engine::Sqlserver, "名字"), "[名字]");
    for engine in SQL_ENGINES {
        assert_ne!(quote_ident(engine, ""), "");
    }
}

#[test]
fn dollar_is_bare_only_for_mysql_family() {
    assert_eq!(quote_ident(Engine::Mysql, "price$"), "price$");
    assert_eq!(quote_ident(Engine::Mariadb, "price$"), "price$");
    assert_eq!(quote_ident(Engine::Postgres, "price$"), "\"price$\"");
    assert_eq!(quote_ident(Engine::Mysql, "$price"), "`$price`");
}

#[test]
fn quote_always_and_qualified() {
    assert_eq!(quote_always(Engine::Mysql, "content"), "`content`");
    assert_eq!(quote_always(Engine::Postgres, "content"), "\"content\"");
    assert_eq!(quote_always(Engine::Sqlserver, "content"), "[content]");
    assert_eq!(
        qualified(Engine::Mysql, &["e6yun3_etoe_business", "etoe_alarm_rule_set"]),
        "e6yun3_etoe_business.etoe_alarm_rule_set"
    );
    assert_eq!(qualified(Engine::Postgres, &["public", "User"]), "public.\"User\"");
    assert_eq!(qualified(Engine::Sqlserver, &["dbo", "order"]), "dbo.[order]");
    assert_eq!(qualified(Engine::Sqlite, &["", "orders"]), "orders");
}

#[test]
fn named_engines_follow_the_same_rules() {
    assert_eq!(quote_ident_named("mysql", "content"), "content");
    assert_eq!(quote_ident_named("postgres", "User"), "\"User\"");
    assert_eq!(quote_ident_named("sqlserver", "order"), "[order]");
    assert_eq!(quote_ident_named("unknown", "content"), "\"content\"");
}
