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
        assert_eq!(caps.transfer_group, adapter.transfer().is_some(), "{engine:?} TransferDialect 声明与提供不一致");
        assert_eq!(caps.transaction_group, adapter.transaction().is_some(), "{engine:?} TransactionDialect 声明与提供不一致");
        // 关系型引擎有对比/导出方言与冲突识别；专项引擎没有。
        assert_eq!(caps.relational, caps.transfer_group, "{engine:?} 关系面与对比/导出组应一致");
        assert_eq!(caps.relational, caps.transaction_group, "{engine:?} 关系面与事务方言组应一致");
        assert_eq!(caps.relational, caps.error_rules_group, "{engine:?} 关系面与错误规则组应一致");

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

#[test]
fn every_engine_has_a_connector_and_url_schemes() {
    let mut schemes = Vec::new();
    for engine in ENGINES {
        let adapter = adapter(engine);
        assert!(adapter.connector().is_some(), "{engine:?} 需要连接器");
        let own = adapter.meta().url_schemes();
        assert!(!own.is_empty(), "{engine:?} 至少认一个连接串 scheme");
        for scheme in own {
            assert_eq!(scheme.name, scheme.name.to_ascii_lowercase());
            assert_eq!(registry::by_url_scheme(scheme.name).map(|(found, _)| found), Some(engine));
            schemes.push(scheme.name);
        }
    }
    let total = schemes.len();
    schemes.sort_unstable();
    schemes.dedup();
    assert_eq!(schemes.len(), total, "scheme 不能被两个引擎同时认领");
    assert_eq!(registry::url_schemes().count(), total);
    assert!(registry::by_url_scheme("oracle").is_none());
}

#[test]
fn connection_form_metadata_keeps_the_previous_rules() {
    use super::{FormField, UrlStyle, DEFAULT_GENERAL_FIELDS};
    let mut order = registry::CONNECTION_FORM_ORDER.to_vec();
    order.sort_by_key(|engine| variant_index(*engine));
    assert_eq!(order, ENGINES.to_vec(), "表单顺序覆盖每个引擎一次");
    for engine in ENGINES {
        let meta = adapter(engine).meta();
        let local = matches!(engine, Engine::Sqlite | Engine::Redis | Engine::Mongodb);
        assert_eq!(meta.defaults_to_local(), local, "{engine:?}");
        assert_eq!(meta.reconnect_needs_saved_password(), !local, "{engine:?}");
        assert_eq!(meta.username_optional(), matches!(engine, Engine::Redis | Engine::Mongodb), "{engine:?}");
        assert_eq!(meta.database_is_index(), engine == Engine::Redis, "{engine:?}");
        assert_eq!(meta.uses_replica_set(), engine == Engine::Mongodb, "{engine:?}");
        assert_eq!(adapter(engine).capabilities().database_scoped_session, engine == Engine::Postgres, "{engine:?}");
        assert_eq!(meta.url_style() == UrlStyle::FilePath, meta.file_based(), "{engine:?}");
    }
    assert_eq!(adapter(Engine::Redis).meta().default_database(), "0");
    assert_eq!(
        adapter(Engine::Redis).meta().general_fields(),
        &[FormField::Host, FormField::Port, FormField::Database, FormField::Password, FormField::Username, FormField::SavePassword]
    );
    assert!(adapter(Engine::Mongodb).meta().general_fields().contains(&FormField::ReplicaSet));
    assert_eq!(adapter(Engine::Postgres).meta().general_fields(), DEFAULT_GENERAL_FIELDS);
    assert!(adapter(Engine::Redis).meta().password_optional("acl-user"));
    assert!(adapter(Engine::Mongodb).meta().password_optional("  "));
    assert!(!adapter(Engine::Mongodb).meta().password_optional("root"));
    assert!(!adapter(Engine::Mysql).meta().password_optional(""));
    assert_eq!(super::workbench_badge_id(Workbench::Redis), "redis");
    assert_eq!(super::workbench_badge_id(Workbench::Mongo), "mongodb");
}

#[tokio::test]
async fn cancel_transport_delegates_to_the_adapter_canceller() {
    use super::session::{CancelTransport, NoCancel};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let none = CancelTransport::default();
    assert!(none.health().is_none() && !none.via_ssh() && none.sqlite_flag().is_none());
    assert!(none.deliver().await.unwrap_err().contains("没有可用来取消的数据库通道"));

    let health = Arc::new(crate::connections::SessionHealth::default());
    let silent = CancelTransport::new(Arc::new(NoCancel), Arc::clone(&health));
    assert!(silent.health().is_some(), "没有取消通道的会话仍带健康句柄");
    assert!(silent.deliver().await.is_err());

    let flag = Arc::new(AtomicBool::new(false));
    let local = CancelTransport::new(Arc::new(super::sqlite::connect::InterruptFlag(Arc::clone(&flag))), health);
    assert!(Arc::ptr_eq(&local.sqlite_flag().unwrap(), &flag));
    assert_eq!(local.deliver().await, Ok(false), "本地中断不另开传输");
    assert!(flag.load(Ordering::SeqCst));
}

#[test]
fn catalog_shape_keeps_the_previous_rules() {
    use super::{catalog_of, qualified_object, sample_select};
    for engine in ENGINES {
        let caps = adapter(engine).capabilities();
        assert!(caps.catalog_group, "{engine:?} 每个引擎都有目录方言");
        assert_eq!(caps.shows_databases, engine != Engine::Sqlite, "{engine:?}");
        assert_eq!(caps.shows_schemas, matches!(engine, Engine::Postgres | Engine::Sqlserver), "{engine:?}");
        assert_eq!(caps.shows_foreign_keys, engine == Engine::Sqlite, "{engine:?}");
        assert_eq!(
            caps.outline_navigation,
            matches!(engine, Engine::Mysql | Engine::Mariadb | Engine::Postgres | Engine::Sqlserver),
            "{engine:?}"
        );
        let catalog = catalog_of(engine);
        assert_eq!(catalog.diagram_statements().is_empty(), !caps.relational, "{engine:?} 关系型引擎才有关系图");
        assert_eq!(catalog.table_is_rebuilt(), matches!(engine, Engine::Postgres | Engine::Sqlserver), "{engine:?}");
        assert_eq!(catalog.listed_kinds().is_empty(), engine == Engine::Redis, "{engine:?}");
        assert_eq!(catalog.metadata_sql().objects.is_empty(), !caps.relational, "{engine:?}");
    }
    // 限定名的容器段：MySQL 家族用库，单文件引擎没有，其余用模式。
    assert_eq!(qualified_object(Engine::Mysql, "shop", "ignored", "orders"), "shop.orders");
    assert_eq!(qualified_object(Engine::Mariadb, "shop", "", "order"), "shop.`order`");
    assert_eq!(qualified_object(Engine::Sqlite, "main", "x", "notes"), "notes");
    assert_eq!(qualified_object(Engine::Postgres, "shop", "public", "User"), "public.\"User\"");
    assert_eq!(qualified_object(Engine::Sqlserver, "shop", "", "orders"), "orders");
    assert_eq!(sample_select(Engine::Postgres, "public.orders", 100), "SELECT *\nFROM public.orders\nLIMIT 100;");
    assert_eq!(sample_select(Engine::Redis, "k", 5), "SELECT *\nFROM k\nLIMIT 5;");
}

/// 会话与引擎对不上时，各目录读取给出同一句原因，不按会话类型猜引擎。
#[tokio::test]
async fn catalog_reads_refuse_a_session_of_another_engine() {
    use crate::catalog::model::ObjectKind;
    use crate::catalog::read::{read_catalog_outline, read_catalog_session, read_definition, read_object_kind_page, read_object_page};
    use crate::connections::CatalogSession;
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().in_memory(true))
        .await
        .unwrap();
    let session = CatalogSession::Sqlite { pool, foreign_keys: None };
    for engine in [Engine::Mysql, Engine::Postgres, Engine::Sqlserver, Engine::Mongodb, Engine::Redis] {
        let body = read_catalog_session(engine, &session, 100, &[]).await;
        assert_eq!(body.error.as_deref(), Some("读取库表失败：连接类型和引擎不一致"), "{engine:?}");
        let page = read_object_page(&session, engine, "main", "", 0, 10).await;
        assert_eq!(page.error.as_deref(), Some("读取库表失败：连接类型和引擎不一致"), "{engine:?}");
        let kind = read_object_kind_page(&session, engine, "main", "", ObjectKind::Table, 0, 10).await;
        assert_eq!(kind.error.as_deref(), Some("读取库表失败：连接类型和引擎不一致"), "{engine:?}");
        let definition = read_definition(&session, engine, "main", "", "t", "table", "", "").await.unwrap_err();
        assert_eq!(definition, "读取定义失败：连接类型和引擎不一致", "{engine:?}");
    }
    // Redis 本来就没有库层级的概览读取：和迁移前一样按不一致报出。
    let outline = read_catalog_outline(Engine::Redis, &session).await;
    assert_eq!(outline.error.as_deref(), Some("读取库表失败：连接类型和引擎不一致"));
    let sqlite = read_catalog_outline(Engine::Sqlite, &session).await;
    assert!(sqlite.error.is_none(), "{:?}", sqlite.error);
    assert_eq!(sqlite.databases.first().map(|db| db.name.as_str()), Some("main"));
}

// ---- A3：类型规则与 DDL 方言 ----

/// 关系型引擎都有类型规则与 DDL 方言，专项引擎都没有（结构编辑经能力组门控）。
#[test]
fn structure_editing_follows_the_relational_capability() {
    for engine in ENGINES {
        let adapter = adapter(engine);
        let relational = adapter.capabilities().relational;
        assert_eq!(adapter.types().is_some(), relational, "{engine:?} 类型规则");
        assert_eq!(adapter.ddl().is_some(), relational, "{engine:?} DDL 方言");
    }
    assert_eq!(super::ddl::no_structure_editing(Engine::Redis), "Redis 没有结构编辑。");
    assert_eq!(super::ddl::no_structure_editing(Engine::Mongodb), "MongoDB 没有结构编辑。");
}

/// 每个引擎专属的库级动作只由所属方言给计划；其它引擎请求时由所属方言给出拒绝原因，不生成语句。
/// 管理页分区与方言实际提供的动作一致。
#[test]
fn object_actions_have_one_owning_dialect_and_admin_sections_agree() {
    use crate::ddl::model::{Action, Snapshot};
    let actions = [
        ("settings", Action::SetForeignKeys { enabled: true }),
        ("settings", Action::SetPragma { pragma: "user_version".into(), value: "1".into() }),
        ("attached", Action::AttachDatabase { path: "/tmp/x.sqlite".into(), name: "x".into() }),
        ("maintenance", Action::Vacuum),
        ("maintenance", Action::Reindex { target: None }),
        ("sequences", Action::AlterSequence { sequence: "s".into(), start: None, increment: Some(2), min_value: None, max_value: None, cycle: None }),
        ("enums", Action::AddEnumValue { enum_name: "e".into(), value: "v".into(), before: None }),
        ("domains", Action::AlterDomain { domain: "d".into(), default_sql: None, not_null: Some(true) }),
        ("events", Action::SetEventEnabled { event: "ev".into(), enabled: true }),
    ];
    for (section, action) in &actions {
        let mut owners = 0;
        for engine in SQL_ENGINES {
            let dialect = super::ddl::ddl_of(engine).unwrap();
            let snapshot = Snapshot::bare(engine, "app", "public", "t");
            let owned = dialect.plan_object_action(&snapshot, action).is_some();
            assert_eq!(owned, dialect.refuse_object_action(&snapshot, action).is_some(), "{engine:?} {action:?}");
            let sections = dialect.admin_sections();
            let shown = match *section {
                "settings" => sections.settings,
                "attached" => sections.attached,
                "maintenance" => sections.maintenance,
                "sequences" => sections.sequences,
                "enums" => sections.enums,
                "domains" => sections.domains,
                _ => sections.events,
            };
            assert_eq!(owned, shown, "{engine:?} 管理分区 {section} 与 {action:?} 的归属不一致");
            if owned {
                owners += 1;
            } else {
                let refused = crate::ddl::preview_plan(&snapshot, action);
                assert!(!refused.blocked.is_empty() && refused.steps.is_empty(), "{engine:?} {action:?} 应被拒绝：{refused:?}");
            }
        }
        // MySQL 与 MariaDB 共用事件动作，其它动作只有一个引擎。
        let expected = if matches!(action, Action::SetEventEnabled { .. }) { 2 } else { 1 };
        assert_eq!(owners, expected, "{action:?} 的所属方言数");
    }
}

/// 结构页按引擎字符串取候选：解析不出的按通用写法，专项引擎没有外键动作；与按 Engine 取的一致。
#[test]
fn named_type_rules_fall_back_to_the_generic_rules() {
    use super::ddl::{fk_actions, fk_actions_named, types_named, FULL_FK_ACTIONS};
    assert_eq!(types_named("oracle").type_choices(), types_named("sqlite").type_choices());
    assert_eq!(types_named("redis").default_column_type(), "TEXT");
    assert_eq!(fk_actions_named("oracle"), FULL_FK_ACTIONS);
    assert!(fk_actions_named("redis").is_empty() && fk_actions_named("mongodb").is_empty());
    for engine in ENGINES {
        assert_eq!(fk_actions_named(adapter(engine).meta().id()), fk_actions(engine), "{engine:?}");
    }
    assert_eq!(types_named("mariadb").preset_types(), types_named("mysql").preset_types());
}

/// 通用词法认的标识符引号来自各方言：双引号、反引号与方括号各一次。
#[test]
fn identifier_quotes_are_collected_from_every_dialect() {
    let quotes = super::identifier_quotes();
    assert_eq!(quotes.len(), 3, "{quotes:?}");
    for pair in [('"', '"'), ('`', '`'), ('[', ']')] {
        assert!(quotes.contains(&pair), "{pair:?} 不在 {quotes:?}");
    }
    use crate::ddl::sql::mentions;
    assert!(mentions("CREATE VIEW v AS SELECT \"Total\" FROM t", "Total"));
    assert!(mentions("CREATE VIEW v AS SELECT `Total` FROM t", "Total"));
    assert!(mentions("CREATE VIEW v AS SELECT [Total] FROM t", "Total"));
    assert!(!mentions("CREATE VIEW v AS SELECT total_count FROM t", "total"));
}

/// 结构页“查看数据”的取样语句：按引擎层级限定，T-SQL 用 TOP。
#[test]
fn first_rows_select_follows_the_dialect() {
    let sample = |engine: Engine| super::first_rows_select(engine, &super::qualified_object(engine, "shop", "dbo", "orders"), 100);
    assert_eq!(sample(Engine::Sqlserver), "SELECT TOP 100 * FROM dbo.orders;");
    assert_eq!(sample(Engine::Mysql), "SELECT * FROM shop.orders LIMIT 100;");
    assert_eq!(sample(Engine::Sqlite), "SELECT * FROM orders LIMIT 100;");
    assert_eq!(sample(Engine::Postgres), "SELECT * FROM dbo.orders LIMIT 100;");
}

/// A4：网格与查询的方言门面按引擎 id 解析，未知或非 SQL 引擎保持原来的回退。
#[test]
fn grid_and_query_facades_resolve_through_the_registry() {
    use super::query::{
        dialect_statement_keywords, is_identifier_quote, is_quote_byte, is_quote_char, known_sql_engine, label_named, lexis_named, sql_dialect_id,
        sql_syntax, syntax_named, system_object_prefixes,
    };
    use super::Lexis;

    for engine in SQL_ENGINES {
        let id = engine.as_str();
        assert!(known_sql_engine(id), "{id}");
        assert_eq!(sql_dialect_id(id), id);
        assert_eq!(registry::sql_engine(id), engine);
        assert_eq!(registry::grid_engine(id), engine);
        assert!(!syntax_named(id).unwrap().functions().is_empty(), "{id} 有函数签名表");
        assert!(!syntax_named(id).unwrap().dialect_words().is_empty(), "{id} 有方言关键字");
    }
    for id in ["redis", "mongodb", "", "oracle"] {
        assert!(!known_sql_engine(id), "{id}");
        assert_eq!(sql_dialect_id(id), registry::UNBOUND_DIALECT.as_str());
        assert_eq!(lexis_named(id), Lexis::UNKNOWN);
    }
    assert_eq!(registry::grid_engine("redis"), Engine::Redis);
    assert_eq!(registry::grid_engine("oracle"), registry::UNBOUND_GRID);

    // 词法：MySQL 家族认 # 注释与 DELIMITER；T-SQL 认括号标识符与 GO；其余标准写法。
    assert!(lexis_named("mysql").hash_comments && lexis_named("mariadb").delimiter_command);
    assert!(lexis_named("sqlserver").transact && !lexis_named("sqlserver").delimiter_command);
    // PG 只多一项：E'…' 与 standard_conforming_strings 让字符串里的反斜杠读法有歧义。
    assert_eq!(lexis_named("postgres"), Lexis { backslash_ambiguous: true, ..Lexis::default() });
    assert!(lexis_named("mysql").backslash_ambiguous && lexis_named("mariadb").backslash_ambiguous);
    assert!(!lexis_named("sqlite").backslash_ambiguous && !lexis_named("sqlserver").backslash_ambiguous);
    assert_eq!(label_named("sqlserver"), "SQL Server");
    assert_eq!(label_named("oracle"), "");

    // 通用词法的引号字符：字符串单引号与开闭相同的标识符引号；任一方言的标识符引号含 T-SQL 的方括号。
    let quotes: Vec<char> = (0u8..128).map(char::from).filter(|ch| is_quote_char(*ch)).collect();
    assert_eq!(quotes, vec!['"', '\'', '\u{60}']);
    assert!(is_quote_byte(b'"') && !is_quote_byte(0xe4) && !is_quote_byte(b'['));
    let identifier: Vec<char> = (0u8..128).map(char::from).filter(|ch| is_identifier_quote(*ch)).collect();
    assert_eq!(identifier, vec!['"', '[', ']', '\u{60}']);

    assert!(dialect_statement_keywords().any(|word| word == "PRAGMA"));
    let prefixes: Vec<&str> = system_object_prefixes().collect();
    assert!(prefixes.contains(&"pg_") && prefixes.contains(&"sqlite_"), "{prefixes:?}");

    // 非 SQL 引擎落到占位语法：原样投影、标准双引号。
    let none = sql_syntax(Engine::Redis);
    assert_eq!(none.text_cast("x"), "x");
    assert_eq!(none.quote_ident("order"), "\"order\"");
    assert_eq!(none.page_tail(10, 0, false), " LIMIT 10");
}

/// A4：网格投影、分页与字面量按方言生成。
#[test]
fn grid_projection_follows_the_dialect() {
    use super::query::sql_syntax;
    let tail = |engine: Engine, offset: u64, ordered: bool| sql_syntax(engine).page_tail(5, offset, ordered);
    assert_eq!(tail(Engine::Postgres, 0, false), " LIMIT 5");
    assert_eq!(tail(Engine::Mysql, 10, true), " LIMIT 5 OFFSET 10");
    assert_eq!(tail(Engine::Sqlserver, 10, true), " OFFSET 10 ROWS FETCH NEXT 5 ROWS ONLY");
    assert_eq!(tail(Engine::Sqlserver, 0, false), " ORDER BY (SELECT NULL) OFFSET 0 ROWS FETCH NEXT 5 ROWS ONLY");
    assert_eq!(sql_syntax(Engine::Sqlserver).text_literal("it's"), "N'it''s'");
    assert_eq!(sql_syntax(Engine::Mysql).text_literal("it's"), "'it''s'");
    assert_eq!(sql_syntax(Engine::Postgres).contains_operator(), "ILIKE");
    assert_eq!(sql_syntax(Engine::Sqlite).contains_operator(), "LIKE");
    assert_eq!(sql_syntax(Engine::Mariadb).insert_defaults("t"), "INSERT INTO t () VALUES ()");
    assert_eq!(sql_syntax(Engine::Postgres).insert_defaults("t"), "INSERT INTO t DEFAULT VALUES");
    assert_eq!(sql_syntax(Engine::Sqlite).implicit_order("table"), Some("rowid"));
    assert_eq!(sql_syntax(Engine::Sqlite).implicit_order("view"), None);
    assert_eq!(sql_syntax(Engine::Postgres).binary_literal("ff"), "'\\xff'");
    assert_eq!(sql_syntax(Engine::Sqlserver).binary_literal("ff"), "0xff");
    assert_eq!(sql_syntax(Engine::Sqlite).binary_literal("ff"), "X'ff'");
    assert_eq!(sql_syntax(Engine::Sqlserver).count_expression(), "CAST(COUNT_BIG(*) AS BIGINT)");
    // MySQL 与 MariaDB 共用语法，只在超时变量和计划格式上不同。
    assert_eq!(sql_syntax(Engine::Mysql).timeout_statements(1500), vec!["SET SESSION MAX_EXECUTION_TIME = 1500".to_owned()]);
    assert_eq!(sql_syntax(Engine::Mariadb).timeout_statements(1500), vec!["SET SESSION max_statement_time = 1.5".to_owned()]);
    assert_eq!(sql_syntax(Engine::Mysql).explain("SELECT 1"), "EXPLAIN FORMAT=TREE SELECT 1");
    assert_eq!(sql_syntax(Engine::Mariadb).explain("SELECT 1"), "EXPLAIN FORMAT=JSON SELECT 1");
}

// ---------------------------------------------------------------- A5：对比/导出/迁移方言

#[test]
fn conflict_wording_comes_from_each_engine_and_the_union_is_unchanged() {
    use super::is_conflict_error;
    let errors = |engine: Engine| adapter(engine).errors().expect("关系型引擎有错误规则");
    assert!(errors(Engine::Sqlite).is_conflict("unique constraint failed: items.id"));
    assert!(errors(Engine::Mysql).is_conflict("duplicate entry '1' for key 'items.primary'"));
    assert!(errors(Engine::Mariadb).is_conflict("duplicate entry '1' for key 'primary'"));
    assert!(errors(Engine::Postgres).is_conflict("duplicate key value violates unique constraint \"items_pkey\""));
    assert!(errors(Engine::Sqlserver).is_conflict("violation of primary key constraint 'pk_items'."));
    assert!(errors(Engine::Sqlserver).is_conflict("violation of unique key constraint 'uq_items'."));
    assert!(errors(Engine::Sqlserver).is_conflict("cannot insert duplicate key row in object 'dbo.items'"));
    // 措辞属于各自引擎：别的引擎不认。
    assert!(!errors(Engine::Sqlite).is_conflict("duplicate entry '1' for key 'primary'"));
    assert!(!errors(Engine::Mysql).is_conflict("unique constraint failed: items.id"));
    // 门面认任一引擎的措辞（迁移目标的报错由它所属的引擎给出），大小写不敏感。
    for message in [
        "UNIQUE constraint failed: items.id",
        "Duplicate entry '1' for key 'items.PRIMARY'",
        r#"duplicate key value violates unique constraint "items_pkey""#,
        "Violation of PRIMARY KEY constraint 'PK_items'.",
        "Violation of UNIQUE KEY constraint 'UQ_items'.",
    ] {
        assert!(is_conflict_error(message), "{message}");
    }
    for message in ["no such table: items", "FOREIGN KEY constraint failed", "CHECK constraint failed: length(code) <= 3"] {
        assert!(!is_conflict_error(message), "{message}");
    }
}

#[test]
fn transfer_facades_follow_capabilities() {
    use super::transfer::relational_engine;
    use super::{fixed_database_named, label_named, shows_schemas_named};
    assert_eq!(fixed_database_named("sqlite"), Some("main"));
    for engine in ["mysql", "mariadb", "postgres", "sqlserver", "redis", "mongodb", "unknown"] {
        assert_eq!(fixed_database_named(engine), None, "{engine}");
    }
    assert!(shows_schemas_named("postgres") && shows_schemas_named("sqlserver"));
    for engine in ["mysql", "mariadb", "sqlite", "redis", "unknown"] {
        assert!(!shows_schemas_named(engine), "{engine}");
    }
    assert_eq!(label_named("sqlserver"), "SQL Server");
    assert_eq!(label_named("postgres"), "PostgreSQL");
    assert_eq!(label_named("whatever"), "whatever");
    assert_eq!(relational_engine("mariadb"), Ok(Engine::Mariadb));
    assert_eq!(relational_engine("redis"), Err("不支持的数据库 redis".to_owned()));
    assert_eq!(relational_engine("x"), Err("不支持的数据库 x".to_owned()));
    assert_eq!(registry::file_engine(), Some(Engine::Sqlite));
    assert!(adapter(registry::FILE_EXPORT_DIALECT).capabilities().relational);
}

#[test]
fn identifier_quote_helpers_cover_every_dialect() {
    use super::{identifier_quote_close, is_symmetric_identifier_quote};
    assert_eq!(identifier_quote_close('"'), Some('"'));
    assert_eq!(identifier_quote_close('`'), Some('`'));
    assert_eq!(identifier_quote_close('['), Some(']'));
    assert_eq!(identifier_quote_close('\''), None);
    assert!(is_symmetric_identifier_quote('"') && is_symmetric_identifier_quote('`'));
    assert!(!is_symmetric_identifier_quote('[') && !is_symmetric_identifier_quote('\''));
}

#[test]
fn compare_statements_are_written_by_each_dialect() {
    use super::transfer::{transfer_of, ViewReplace};
    use crate::compare::model::{Kind, ObjectDef, Side};
    let side = |engine: &str, database: &str, schema: &str| Side::ready(engine, database, schema, Vec::new());
    let mut trigger = ObjectDef::defined(Kind::Trigger, "touch", "");
    trigger.relation = "notes".to_owned();
    let mut function = ObjectDef::defined(Kind::Function, "total", "");
    function.signature = "integer".to_owned();

    let pg = side("postgres", "app", "public");
    assert_eq!(transfer_of(Engine::Postgres).drop_index(&pg, "notes", "notes_idx"), "DROP INDEX public.notes_idx");
    assert_eq!(transfer_of(Engine::Postgres).drop_trigger(&pg, &trigger), "DROP TRIGGER touch ON public.notes");
    assert_eq!(transfer_of(Engine::Postgres).drop_routine(&pg, "FUNCTION", &function), "DROP FUNCTION public.total(integer)");
    assert_eq!(transfer_of(Engine::Postgres).drop_default("public.notes", "Body"), r#"ALTER TABLE public.notes ALTER COLUMN "Body" DROP DEFAULT"#);
    assert_eq!(transfer_of(Engine::Postgres).view_replace(), ViewReplace::MatchingColumns);
    assert!(transfer_of(Engine::Postgres).in_place_types().is_some() && transfer_of(Engine::Postgres).breaks_view_routine_cycles());

    let my = side("mysql", "shop", "");
    assert_eq!(transfer_of(Engine::Mysql).drop_index(&my, "notes", "notes_idx"), "DROP INDEX notes_idx ON shop.notes");
    assert_eq!(transfer_of(Engine::Mysql).drop_trigger(&my, &trigger), "DROP TRIGGER shop.touch");
    assert_eq!(transfer_of(Engine::Mysql).set_default("shop.notes", "body", "'x'"), "ALTER TABLE shop.notes ALTER body SET DEFAULT 'x'");
    assert_eq!(transfer_of(Engine::Mysql).dump_auto_clause(), " AUTO_INCREMENT");
    assert!(!transfer_of(Engine::Mysql).replaces_routine_in_place());
    assert!(transfer_of(Engine::Mariadb).replaces_routine_in_place());

    let lite = side("sqlite", "main", "");
    assert_eq!(transfer_of(Engine::Sqlite).drop_index(&lite, "notes", "notes_idx"), "DROP INDEX notes_idx");
    assert!(transfer_of(Engine::Sqlite).inline_foreign_keys() && transfer_of(Engine::Sqlite).keeps_table_source());
    assert_eq!(transfer_of(Engine::Sqlite).view_replace(), ViewReplace::Never);

    let ms = side("sqlserver", "rillsql", "dbo");
    assert_eq!(transfer_of(Engine::Sqlserver).drop_index(&ms, "notes", "notes_idx"), "DROP INDEX notes_idx ON dbo.notes");
    assert_eq!(transfer_of(Engine::Sqlserver).drop_trigger(&ms, &trigger), "DROP TRIGGER touch");
    assert!(transfer_of(Engine::Sqlserver).drop_default("dbo.notes", "status").contains("sys.default_constraints"));

    // 专项引擎没有对比面：兜底按标准写法，读取拒绝。
    assert!(adapter(Engine::Redis).transfer().is_none());
    assert_eq!(transfer_of(Engine::Redis).drop_index(&side("redis", "", ""), "k", "i"), r#"DROP INDEX "i""#);
}

#[test]
fn compare_scope_refusals_belong_to_the_dialect() {
    use super::transfer::transfer_of;
    use crate::compare::model::Side;
    let side = |engine: &str, database: &str, schema: &str| Side::ready(engine, database, schema, Vec::new());
    assert_eq!(
        transfer_of(Engine::Postgres).compare_scope_refusal(&side("postgres", "app", "public"), &side("postgres", "app", " ")).as_deref(),
        Some("PostgreSQL 需要为基准和对照各选一个 schema，不生成脚本。")
    );
    assert_eq!(transfer_of(Engine::Postgres).compare_scope_refusal(&side("postgres", "app", "public"), &side("postgres", "app", "public")), None);
    assert_eq!(
        transfer_of(Engine::Sqlite).compare_scope_refusal(&side("sqlite", "main", ""), &side("sqlite", "aux", "")).as_deref(),
        Some("SQLite 只对比该连接的 main，不附加其他文件，不生成脚本。")
    );
    assert_eq!(transfer_of(Engine::Sqlite).compare_scope_refusal(&side("sqlite", "", ""), &side("sqlite", "main", "")), None);
    assert_eq!(transfer_of(Engine::Mysql).compare_scope_refusal(&side("mysql", "a", ""), &side("mysql", "b", "")), None);
}

#[tokio::test]
async fn sessions_are_matched_to_engines_and_sqlite_scopes_stay_on_main() {
    use super::transfer::transfer_of;
    use super::{engine_of_session, session_matches};
    use crate::connections::CatalogSession;
    let pool = sqlx::sqlite::SqlitePoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
    let session = CatalogSession::Sqlite { pool, foreign_keys: None };
    assert!(session_matches(Engine::Sqlite, &session));
    for engine in [Engine::Mysql, Engine::Mariadb, Engine::Postgres, Engine::Sqlserver, Engine::Redis, Engine::Mongodb] {
        assert!(!session_matches(engine, &session), "{engine:?}");
    }
    assert_eq!(engine_of_session(&session), Some(Engine::Sqlite));
    assert_eq!(engine_of_session(&CatalogSession::Scripted), None);

    let dialect = transfer_of(Engine::Sqlite);
    assert_eq!(dialect.dump_scope(&session, "", &[]).await, Ok((String::new(), vec![String::new()])));
    assert!(dialect.dump_scope(&session, "aux", &[]).await.unwrap_err().contains("当前连接打开的是 main，没有另开连接读取 aux"));
    assert_eq!(dialect.compare_scope(&session, "main", "").await, Ok(()));
    assert_eq!(dialect.compare_scope(&session, "aux", "").await, Err("SQLite 只对比该连接的 main，不附加其他文件".to_owned()));

    // 迁移写入：事务批量与自动提交单条都落在同一条连接上。
    session.execute_autocommit("CREATE TABLE items (id INTEGER PRIMARY KEY)").await.unwrap();
    session.insert_in_transaction("INSERT INTO items (id) VALUES (1), (2)").await.unwrap();
    let clash = session.insert_in_transaction("INSERT INTO items (id) VALUES (3), (1)").await.unwrap_err();
    assert!(super::is_conflict_error(&clash), "{clash}");
    let rows = session.text_rows("SELECT COUNT(*) FROM items").await.unwrap();
    assert_eq!(rows[0][0].as_deref(), Some("2"), "失败的批次整批回滚");
    assert!(CatalogSession::Scripted.insert_in_transaction("SELECT 1").await.is_err());
}

#[test]
fn ui_metadata_comes_from_every_adapter() {
    let mut marks = Vec::new();
    for engine in ENGINES {
        let meta = adapter(engine).meta();
        let (abbr, color) = meta.mark();
        assert_eq!(abbr.chars().count(), 2, "{engine:?} 徽标缩写为两个字母");
        assert!(color <= 0xff_ffff, "{engine:?} 品牌色是 0xRRGGBB");
        marks.push(abbr);
        // 连接串示例必须能被本引擎的 scheme 认出。
        let scheme = meta.url_example().split(':').next().unwrap();
        assert_eq!(registry::by_url_scheme(scheme).map(|(found, _)| found), Some(engine), "{engine:?} 连接串示例");
        // 别名与扩展名只给显示层/文件识别用，必须小写且能解析回本引擎。
        for alias in meta.aliases() {
            assert_eq!(*alias, alias.to_ascii_lowercase());
            assert_eq!(registry::parse_loose(alias), Some(engine), "{engine:?} 别名 {alias}");
        }
        assert_eq!(registry::parse_loose(&meta.id().to_ascii_uppercase()), Some(engine), "宽松解析不区分大小写");
        assert_eq!(meta.file_based(), !meta.file_extensions().is_empty(), "{engine:?} 只有文件型引擎有扩展名");
        for ext in meta.file_extensions() {
            assert_eq!(super::engine_for_file_extension(&ext.to_ascii_uppercase()), Some(engine));
        }
        assert_eq!(meta.file_based(), meta.file_signature().is_some(), "{engine:?} 只有文件型引擎有文件头魔数");
        if let Some(signature) = meta.file_signature() {
            assert_eq!(super::engine_for_file_header(signature), Some(engine), "{engine:?} 文件头认回本引擎");
            assert!(super::file_signature_len() >= signature.len());
        }
        assert!(!meta.database_hint_key().is_empty());
        assert_eq!(super::is_relational_named(meta.id()), adapter(engine).capabilities().relational);
        assert_eq!(engine.workbench(), meta.workbench());
    }
    marks.sort_unstable();
    marks.dedup();
    assert_eq!(marks.len(), ENGINES.len(), "徽标缩写互不相同");
    assert!(adapter(registry::LOCAL_FILE).meta().file_based(), "打开数据库文件建立的是文件型连接");
    assert!(adapter(registry::DEFAULT_ENGINE).capabilities().relational, "新建连接默认关系型引擎");
    assert_eq!(registry::parse_loose("unknown"), None);
    assert!(super::is_relational_named("unknown"), "认不出的名字按关系型处理");
    assert_eq!(super::engine_for_file_extension("txt"), None);
}

#[test]
fn ui_metadata_matches_the_previous_tables() {
    use super::FieldHint;
    let expected = [
        (Engine::Postgres, ("PG", 0x3b6ea5), "postgres://user@host:5432/db", "ui.conn.dbPlaceholderPg", FieldHint::Text("postgres")),
        (Engine::Mysql, ("MY", 0x0284a8), "mysql://user@host:3306/db", "ui.conn.dbPlaceholderMysql", FieldHint::Text("root")),
        (Engine::Mariadb, ("MA", 0xb46346), "mariadb://user@host:3306/db", "ui.conn.dbPlaceholderMysql", FieldHint::Text("root")),
        (Engine::Sqlite, ("SL", 0x1d82d6), "sqlite:///path/to/file.db", "ui.conn.optional", FieldHint::None),
        (Engine::Sqlserver, ("MS", 0xd63636), "sqlserver://user@host:1433/db", "ui.conn.dbPlaceholderMssql", FieldHint::Text("sa")),
        (Engine::Redis, ("RD", 0xdc3d35), "redis://host:6379/0", "ui.conn.redisDb", FieldHint::Key("ui.conn.redisAclHint")),
        (Engine::Mongodb, ("MG", 0x10a356), "mongodb://user@host:27017/db", "ui.conn.dbPlaceholderMongo", FieldHint::Key("ui.conn.optional")),
    ];
    for (engine, mark, url, db_key, user) in expected {
        let meta = adapter(engine).meta();
        assert_eq!(meta.mark(), mark, "{engine:?}");
        assert_eq!(meta.url_example(), url, "{engine:?}");
        assert_eq!(meta.database_hint_key(), db_key, "{engine:?}");
        assert_eq!(meta.username_hint(), user, "{engine:?}");
    }
    for (alias, engine) in [("postgresql", Engine::Postgres), ("mssql", Engine::Sqlserver), ("mongo", Engine::Mongodb), ("MongoDB", Engine::Mongodb)] {
        assert_eq!(registry::parse_loose(alias), Some(engine), "{alias}");
    }
    for ext in ["sqlite", "sqlite3", "db", "db3", "s3db", "DB"] {
        assert_eq!(super::engine_for_file_extension(ext), Some(Engine::Sqlite), "{ext}");
    }
    let mut header = b"SQLite format 3\0".to_vec();
    header.extend_from_slice(&[0x10, 0x00, 0x01, 0x01]);
    assert_eq!(super::engine_for_file_header(&header), Some(Engine::Sqlite), "SQLite 文件头");
    assert_eq!(super::engine_for_file_header(b"SQLite format 2"), None);
    assert_eq!(super::engine_for_file_header(b"select 1;"), None);
    assert_eq!(super::engine_for_file_header(b""), None);
    assert!(!Workbench::Mongo.has_query_editor());
    assert!(Workbench::Redis.has_query_editor() && Workbench::Relational.has_query_editor());
}

#[test]
fn ui_kit_badges_and_labels_read_engine_meta() {
    use crate::ui::kit::{engine_label, engine_mark};
    for engine in ENGINES {
        let meta = adapter(engine).meta();
        assert_eq!(engine_mark(meta.id()), meta.mark());
        assert_eq!(engine_label(meta.id()), meta.label());
    }
    assert_eq!(engine_label("postgresql"), "PostgreSQL");
    assert_eq!(engine_label("mssql"), "SQL Server");
    assert_eq!(engine_mark("mongo"), ("MG", 0x10a356));
    assert_eq!(engine_mark("oracle"), ("DB", 0x64748b));
    assert_eq!(engine_label("oracle"), "Database");
}

#[test]
fn capability_notes_list_engines_from_the_registry() {
    assert_eq!(super::transfer::schema_engines_note(), "只有 PostgreSQL 和 SQL Server 需要选择 schema");
    assert_eq!(super::labels_with(|caps| caps.database_scoped_session, " 和 "), "PostgreSQL");
    assert_eq!(super::labels_with(|caps| caps.relational && !caps.shows_databases, "、"), "SQLite");
}

#[test]
fn engine_specific_english_lives_in_the_adapters() {
    // 引擎专属句子由适配器给出英文：经 ddl::english 的通用查找仍能翻译。
    for (zh, en) in [
        ("SQLite 没有 TRUNCATE，这里用 DELETE FROM 逐行删除；AUTOINCREMENT 的计数不会重置，删除触发器会触发。", "SQLite has no TRUNCATE"),
        ("PostgreSQL 需要指定 schema（表所在的分组名），没有另开连接。", "PostgreSQL needs a schema"),
        ("只有 MySQL 与 MariaDB 有事件（event）对象，这里不生成语句。", "Only MySQL and MariaDB have event objects"),
        ("Redis 没有对象定义", "Redis has no object definitions"),
        ("Redis 没有结构编辑。", "Redis has no structure editing."),
    ] {
        assert!(crate::ddl::english::english(zh).starts_with(en), "{zh}");
    }
    let mut owners = 0;
    for engine in ENGINES {
        let meta = adapter(engine).meta();
        owners += usize::from(!meta.messages_en().is_empty());
        for (zh, en) in meta.messages_en().iter().chain(meta.missing_items_en()) {
            assert!(crate::ddl::english::has_cjk(zh) && !crate::ddl::english::has_cjk(en), "{engine:?}: {zh}");
        }
    }
    assert_eq!(owners, ENGINES.len(), "每个引擎都有自己的专属句子");
    assert_eq!(
        crate::ddl::english::localize_missing_item("主键的 CLUSTERED / NONCLUSTERED 选项（按 SQL Server 默认处理）", crate::i18n::Locale::En),
        "the primary key's CLUSTERED / NONCLUSTERED option (SQL Server's default is assumed)"
    );
}
