//! 对象概览统计（R3）：各引擎 SQL 生成、行形状解析、SQLite 实读与统计缺失时的名称退回。

use super::model::{Engine, ObjectKind};
use super::stats::{read_object_stats, read_stats_with, rows_from_stats, stats_level, stats_sql, StatsLevel};
use crate::connections::CatalogSession;

fn row(cells: &[Option<&str>]) -> Vec<Option<String>> {
    cells.iter().map(|cell| cell.map(str::to_owned)).collect()
}

#[test]
fn levels_follow_each_engine_hierarchy() {
    assert_eq!(stats_level(Engine::Mysql, "", ""), StatsLevel::Databases);
    assert_eq!(stats_level(Engine::Mysql, "shop", ""), StatsLevel::Objects);
    assert_eq!(stats_level(Engine::Mariadb, "shop", ""), StatsLevel::Objects);
    assert_eq!(stats_level(Engine::Postgres, "", ""), StatsLevel::Databases);
    assert_eq!(stats_level(Engine::Postgres, "shop", ""), StatsLevel::Schemas);
    assert_eq!(stats_level(Engine::Postgres, "shop", "public"), StatsLevel::Objects);
    assert_eq!(stats_level(Engine::Sqlserver, "shop", ""), StatsLevel::Schemas);
    assert_eq!(stats_level(Engine::Sqlserver, "shop", "dbo"), StatsLevel::Objects);
    // SQLite 没有库层：连接级直接列对象。
    assert_eq!(stats_level(Engine::Sqlite, "", ""), StatsLevel::Objects);
}

#[test]
fn mysql_stats_read_information_schema_tables() {
    let databases = stats_sql(Engine::Mysql, StatsLevel::Databases, "", "").unwrap();
    assert!(databases.contains("information_schema.schemata"));
    assert!(databases.contains("data_length") && databases.contains("index_length"));
    assert!(databases.contains("COUNT(t.table_name)"));
    assert!(databases.contains("'performance_schema'"), "系统库不列出");
    let objects = stats_sql(Engine::Mysql, StatsLevel::Objects, "e6'shop", "").unwrap();
    assert!(objects.contains("FROM information_schema.tables WHERE table_schema = 'e6''shop'"), "{objects}");
    for column in ["table_rows", "data_length + index_length", "table_comment", "update_time"] {
        assert!(objects.contains(column), "{column}: {objects}");
    }
    assert!(objects.contains("ORDER BY table_name"));
    // MariaDB 复用 MySQL 家族的统计。
    assert_eq!(stats_sql(Engine::Mariadb, StatsLevel::Objects, "e6'shop", "").unwrap(), objects);
    assert!(stats_sql(Engine::Mysql, StatsLevel::Schemas, "shop", "").is_none(), "MySQL 没有模式层");
}

#[test]
fn postgres_stats_use_pg_class_total_size_and_obj_description() {
    let databases = stats_sql(Engine::Postgres, StatsLevel::Databases, "", "").unwrap();
    assert!(databases.contains("pg_database_size(d.datname)"));
    assert!(databases.contains("has_database_privilege"), "没有 CONNECT 权限的库不让整条查询失败");
    let schemas = stats_sql(Engine::Postgres, StatsLevel::Schemas, "shop", "").unwrap();
    assert!(schemas.contains("pg_namespace") && schemas.contains("pg_total_relation_size"));
    assert!(schemas.contains("NOT LIKE 'pg_%'"));
    let objects = stats_sql(Engine::Postgres, StatsLevel::Objects, "shop", "sa'les").unwrap();
    assert!(objects.contains("n.nspname = 'sa''les'"), "{objects}");
    for part in ["pg_class", "reltuples", "pg_total_relation_size(c.oid)", "obj_description(c.oid, 'pg_class')"] {
        assert!(objects.contains(part), "{part}: {objects}");
    }
    assert!(objects.contains("c.reltuples >= 0"), "从未 ANALYZE 的 -1 不当行数");
    // 分区父表没有自身存储：大小与行数汇总叶子分区，而不是对父表本身取 pg_total_relation_size（恒为 0）。
    assert!(objects.contains("NOT c.relispartition"), "分区不单列：{objects}");
    assert!(objects.contains("WITH RECURSIVE tree(relid)") && objects.contains("pg_inherits"), "{objects}");
    assert!(objects.contains("SUM(pg_total_relation_size(l.oid))"), "{objects}");
    assert!(objects.contains("WHEN c.relkind = 'p' THEN part.bytes"), "{objects}");
    assert!(objects.contains("WHEN c.relkind = 'p' THEN part.estimate"), "{objects}");
    assert!(objects.contains("bool_and(l.reltuples >= 0)"), "有叶子没估算时行数留空：{objects}");
    assert!(!objects.contains("c.relkind IN ('r', 'p', 'm') THEN pg_total_relation_size(c.oid)"), "父表自身大小恒为 0：{objects}");
    assert!(!objects.contains("pg_partition_tree"), "兼容 PG10/11：{objects}");
}

#[test]
fn sqlserver_stats_use_partition_stats_and_extended_properties_scoped_to_the_database() {
    let databases = stats_sql(Engine::Sqlserver, StatsLevel::Databases, "", "").unwrap();
    assert!(databases.contains("sys.databases") && databases.contains("sys.master_files"));
    let schemas = stats_sql(Engine::Sqlserver, StatsLevel::Schemas, "sales", "").unwrap();
    assert!(schemas.contains("[sales].sys.dm_db_partition_stats"), "读非当前库按三段名限定：{schemas}");
    let objects = stats_sql(Engine::Sqlserver, StatsLevel::Objects, "sales", "dbo").unwrap();
    assert!(objects.contains("[sales].sys.objects"), "{objects}");
    assert!(objects.contains("[sales].sys.dm_db_partition_stats"));
    assert!(objects.contains("[sales].sys.extended_properties") && objects.contains("MS_Description"));
    assert!(objects.contains("s.name = 'dbo'"));
    assert!(objects.contains("o.type IN ('U', 'V')"));
}

#[test]
fn sqlite_stats_read_dbstat_for_the_main_database_at_connection_level() {
    let objects = stats_sql(Engine::Sqlite, StatsLevel::Objects, "", "").unwrap();
    assert!(objects.contains("\"main\".sqlite_master"), "{objects}");
    assert!(objects.contains("dbstat('main')"));
    let attached = stats_sql(Engine::Sqlite, StatsLevel::Objects, "arch\"ive", "").unwrap();
    assert!(attached.contains("\"arch\"\"ive\".sqlite_master"), "{attached}");
    assert!(stats_sql(Engine::Sqlite, StatsLevel::Databases, "", "").is_none());
}

#[test]
fn stats_rows_parse_estimates_sizes_and_unknowns_without_inventing_numbers() {
    let databases = rows_from_stats(
        StatsLevel::Databases,
        &[row(&[Some("shop"), Some("12"), Some("1048576.0000")]), row(&[Some("empty"), Some("0"), None]), row(&[None, Some("1"), Some("2")])],
    );
    assert_eq!(databases.len(), 2, "没有名字的行跳过");
    assert_eq!(databases[0].name, "shop");
    assert_eq!(databases[0].tables, Some(12));
    assert_eq!(databases[0].size, Some(1_048_576), "MySQL SUM 的 DECIMAL 取整");
    assert_eq!(databases[1].size, None);
    let objects = rows_from_stats(
        StatsLevel::Objects,
        &[
            row(&[Some("orders"), Some("table"), Some("1500"), Some("65536"), Some("订单"), Some("2026-11-01 08:00:00")]),
            row(&[Some("paid"), Some("view"), None, None, Some(""), None]),
            row(&[Some("fresh"), Some("table"), Some("-1"), Some("8192"), None, None]),
        ],
    );
    assert_eq!(objects[0].kind, Some(ObjectKind::Table));
    assert_eq!((objects[0].rows, objects[0].size), (Some(1500), Some(65536)));
    assert_eq!(objects[0].comment, "订单");
    assert_eq!(objects[0].updated, "2026-11-01 08:00:00");
    assert_eq!(objects[1].kind, Some(ObjectKind::View));
    assert_eq!((objects[1].rows, objects[1].size), (None, None));
    assert_eq!(objects[2].rows, None, "负数估算是未知，不是行数");
}

async fn sqlite_session() -> (CatalogSession, std::path::PathBuf) {
    let path = std::env::temp_dir().join(format!("rillsql-overview-{}.sqlite", uuid::Uuid::new_v4()));
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&path).create_if_missing(true))
        .await
        .unwrap();
    for sql in [
        "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT)",
        "CREATE INDEX notes_body ON notes(body)",
        "INSERT INTO notes (body) VALUES ('a'), ('b')",
        "CREATE VIEW recent AS SELECT * FROM notes",
        "CREATE TRIGGER notes_touch AFTER UPDATE ON notes BEGIN SELECT 1; END",
    ] {
        sqlx::query(sql).execute(&pool).await.unwrap();
    }
    (CatalogSession::Sqlite { pool, foreign_keys: Some(false) }, path)
}

#[tokio::test]
async fn sqlite_connection_overview_lists_tables_and_views_with_sizes_or_marks_stats_missing() {
    let (session, path) = sqlite_session().await;
    let body = read_object_stats(Engine::Sqlite, &session, "", "").await.unwrap();
    assert_eq!(body.level, StatsLevel::Objects);
    let names: Vec<_> = body.rows.iter().map(|row| (row.name.as_str(), row.kind)).collect();
    assert_eq!(names, vec![("notes", Some(ObjectKind::Table)), ("recent", Some(ObjectKind::View))], "触发器不进概览");
    match &body.missing {
        // 带 dbstat 的构建：表的大小含它的索引页，视图没有大小。
        None => {
            assert!(body.rows[0].size.is_some_and(|size| size >= 2 * 4096), "{:?}", body.rows[0]);
            assert_eq!(body.rows[1].size, None);
        }
        // 没有 dbstat：仍列出名称并说明统计缺失。
        Some(reason) => {
            assert!(!reason.trim().is_empty());
            assert!(body.rows.iter().all(|row| row.size.is_none()));
        }
    }
    if let CatalogSession::Sqlite { pool, .. } = &session {
        pool.close().await;
    }
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn failed_stats_fall_back_to_names_and_name_the_reason() {
    let (session, path) = sqlite_session().await;
    // 统计语句失败（权限、版本、缺虚表）：名称照样列出，统计全空并带上原因。
    let body = read_stats_with(Engine::Sqlite, &session, StatsLevel::Objects, "", "", Some("SELECT * FROM no_such_stats".to_owned())).await.unwrap();
    let names: Vec<_> = body.rows.iter().map(|row| (row.name.as_str(), row.kind)).collect();
    assert_eq!(names, vec![("notes", Some(ObjectKind::Table)), ("recent", Some(ObjectKind::View))]);
    assert!(body.rows.iter().all(|row| row.size.is_none() && row.rows.is_none()));
    assert!(body.missing.as_deref().is_some_and(|reason| reason.contains("no_such_stats")), "{:?}", body.missing);
    // 引擎不提供这一层统计：同样退回名称，原因是不支持。
    let unsupported = read_stats_with(Engine::Sqlite, &session, StatsLevel::Objects, "main", "", None).await.unwrap();
    assert_eq!(unsupported.rows.len(), 2);
    assert_eq!(unsupported.missing.as_deref(), Some("catalog.overview.statsUnsupported"));
    // 会话类型和引擎对不上时统计与名称都读不到：明确失败，不给空表冒充“没有对象”。
    let error = read_object_stats(Engine::Mysql, &session, "shop", "").await.unwrap_err();
    assert!(!error.trim().is_empty());
    if let CatalogSession::Sqlite { pool, .. } = &session {
        pool.close().await;
    }
    let _ = std::fs::remove_file(path);
}

// ------------------------------------------------------------ docker 夹具（可选运行）
//
// `docker compose -f docker/compose.yaml up -d` 起了 MySQL / PostgreSQL 时验证真实统计：
// 种子库 rillsql 的 customers（2 行）与 orders（3 行）要读到估计行数与数据+索引大小。
// 夹具不在线时跳过并写明原因；连上了却读不到统计才算失败。

struct LiveFixture {
    id: &'static str,
    engine: Engine,
    port: u16,
    user: &'static str,
    password: &'static str,
    /// 刷新统计信息的语句（只更新统计，不改数据）。
    analyze: &'static [&'static str],
    schema: &'static str,
}

const LIVE_FIXTURES: &[LiveFixture] = &[
    LiveFixture {
        id: "live-stats-mysql",
        engine: Engine::Mysql,
        port: 3306,
        user: "root",
        password: "root",
        analyze: &["ANALYZE TABLE rillsql.customers, rillsql.orders"],
        schema: "",
    },
    LiveFixture {
        id: "live-stats-pg",
        engine: Engine::Postgres,
        port: 5432,
        user: "postgres",
        password: "postgres",
        analyze: &["ANALYZE customers", "ANALYZE orders"],
        schema: "public",
    },
];

fn live_unreachable(err: &str) -> bool {
    let text = err.to_ascii_lowercase();
    err.contains("没有打开会话")
        || text.contains("connection refused")
        || text.contains("error connecting")
        || text.contains("os error 61")
        || text.contains("os error 10061")
        || text.contains("timed out")
}

async fn live_open(services: &crate::application::AppServices, fixture: &LiveFixture) -> Result<CatalogSession, String> {
    let mut draft = crate::connections::model::ConnectionDraft::blank(fixture.engine);
    draft.id = Some(fixture.id.to_owned());
    draft.name = fixture.id.to_owned();
    draft.host = "127.0.0.1".to_owned();
    draft.port = fixture.port.to_string();
    draft.database = "rillsql".to_owned();
    draft.username = fixture.user.to_owned();
    draft.tls_mode = crate::connections::model::TlsMode::Off;
    draft.save_password = true;
    services.save_connection(draft, Some(fixture.password.to_owned()), None, None).await.map_err(|err| err.message())?;
    let opened = services.open_connection(fixture.id, Some(fixture.password.to_owned()), None, None).await?;
    if !opened.connected {
        return Err(format!("没有打开会话：{}", opened.report.summary.unwrap_or_default()));
    }
    services.connections.catalog_session(fixture.id).ok_or_else(|| "没有打开会话".to_owned())
}

async fn live_check(fixture: &LiveFixture, session: &CatalogSession) -> Result<String, String> {
    for sql in fixture.analyze {
        session.text_rows(sql).await?;
    }
    let databases = read_object_stats(fixture.engine, session, "", "").await?;
    let seed = databases.rows.iter().find(|row| row.name == "rillsql").ok_or_else(|| format!("库清单里没有 rillsql：{:?}", databases.rows))?;
    if databases.missing.is_some() || seed.size.is_none() {
        return Err(format!("连接级统计缺失：{:?} {:?}", databases.missing, seed));
    }
    if fixture.engine == Engine::Postgres {
        let schemas = read_object_stats(fixture.engine, session, "rillsql", "").await?;
        let public = schemas.rows.iter().find(|row| row.name == "public").ok_or_else(|| format!("模式清单里没有 public：{:?}", schemas.rows))?;
        if public.tables.unwrap_or(0) < 2 || public.size.is_none() {
            return Err(format!("public 模式统计不对：{public:?}"));
        }
    } else if seed.tables.unwrap_or(0) < 2 {
        return Err(format!("rillsql 表数量不对：{seed:?}"));
    }
    let objects = read_object_stats(fixture.engine, session, "rillsql", fixture.schema).await?;
    if let Some(missing) = &objects.missing {
        return Err(format!("对象统计缺失：{missing}"));
    }
    let row = |name: &str| objects.rows.iter().find(|row| row.name == name).cloned().ok_or_else(|| format!("没有 {name}：{:?}", objects.rows));
    let customers = row("customers")?;
    let orders = row("orders")?;
    if customers.kind != Some(ObjectKind::Table) || customers.size.unwrap_or(0) == 0 || orders.size.unwrap_or(0) == 0 {
        return Err(format!("大小字段缺失：{customers:?} {orders:?}"));
    }
    if customers.rows != Some(2) || orders.rows != Some(3) {
        return Err(format!("估计行数与种子不符（ANALYZE 之后应为 2 / 3）：{:?} {:?}", customers.rows, orders.rows));
    }
    let mut line = format!("{:?}: customers {:?} 行 {:?} 字节，orders {:?} 行 {:?} 字节", fixture.engine, customers.rows, customers.size, orders.rows, orders.size);
    if fixture.engine == Engine::Postgres {
        line.push_str(&live_check_pg_partitions(session).await?);
    }
    Ok(line)
}

/// 分区父表的大小与估计行数要等于全部叶子分区之和（含二级分区），分区本身不单列；用临时模式，读完即删。
async fn live_check_pg_partitions(session: &CatalogSession) -> Result<String, String> {
    const SCHEMA: &str = "rillsql_overview_parts";
    let setup = [
        format!("DROP SCHEMA IF EXISTS {SCHEMA} CASCADE"),
        format!("CREATE SCHEMA {SCHEMA}"),
        format!("CREATE TABLE {SCHEMA}.m (id int, d date) PARTITION BY RANGE (d)"),
        format!("CREATE TABLE {SCHEMA}.m_2025 PARTITION OF {SCHEMA}.m FOR VALUES FROM ('2025-01-01') TO ('2026-01-01')"),
        format!("CREATE TABLE {SCHEMA}.m_2026 PARTITION OF {SCHEMA}.m FOR VALUES FROM ('2026-01-01') TO ('2027-01-01') PARTITION BY LIST (id)"),
        format!("CREATE TABLE {SCHEMA}.m_2026_a PARTITION OF {SCHEMA}.m_2026 FOR VALUES IN (1, 2, 3)"),
        format!("CREATE TABLE {SCHEMA}.m_2026_b PARTITION OF {SCHEMA}.m_2026 DEFAULT"),
        format!("CREATE INDEX ON {SCHEMA}.m (id)"),
        format!("CREATE TABLE {SCHEMA}.empty_parent (id int) PARTITION BY RANGE (id)"),
        format!("INSERT INTO {SCHEMA}.m SELECT g, DATE '2025-03-01' FROM generate_series(1, 500) g"),
        format!("INSERT INTO {SCHEMA}.m SELECT g % 7, DATE '2026-03-01' FROM generate_series(1, 700) g"),
        format!("ANALYZE {SCHEMA}.m"),
    ];
    let outcome = async {
        for sql in &setup {
            session.text_rows(sql).await?;
        }
        let expected = session
            .text_rows(&format!("SELECT SUM(pg_total_relation_size(c.oid))::text FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = '{SCHEMA}' AND c.relkind = 'r'"))
            .await?
            .first()
            .and_then(|row| row.first().cloned().flatten())
            .and_then(|text| text.parse::<u64>().ok())
            .ok_or_else(|| "读不到叶子分区大小之和".to_owned())?;
        let body = read_object_stats(Engine::Postgres, session, "rillsql", SCHEMA).await?;
        if let Some(missing) = &body.missing {
            return Err(format!("分区模式统计缺失：{missing}"));
        }
        let names: Vec<_> = body.rows.iter().map(|row| row.name.as_str()).collect();
        if names != ["empty_parent", "m"] {
            return Err(format!("分区不该单列：{names:?}"));
        }
        let empty = &body.rows[0];
        let parent = &body.rows[1];
        if parent.size != Some(expected) || expected == 0 {
            return Err(format!("分区父表大小应为叶子之和 {expected}：{parent:?}"));
        }
        if parent.rows != Some(1200) {
            return Err(format!("分区父表估计行数应为叶子之和 1200：{parent:?}"));
        }
        if empty.size != Some(0) || empty.rows != Some(0) {
            return Err(format!("没有分区的父表应为 0：{empty:?}"));
        }
        let schemas = read_object_stats(Engine::Postgres, session, "rillsql", "").await?;
        let total = schemas.rows.iter().find(|row| row.name == SCHEMA).and_then(|row| row.size);
        if total != Some(expected) {
            return Err(format!("模式级合计 {total:?} 与对象级 {expected} 不一致"));
        }
        Ok(format!("；分区父表 m {:?} 行 {:?} 字节", parent.rows, parent.size))
    }
    .await;
    let _ = session.text_rows(&format!("DROP SCHEMA IF EXISTS {SCHEMA} CASCADE")).await;
    outcome
}

#[tokio::test]
async fn live_fixtures_report_row_estimates_and_sizes() {
    let dir = std::env::temp_dir().join(format!("rillsql-live-stats-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = crate::application::AppServices::new(std::sync::Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite"))));
    let mut seen = Vec::new();
    let mut failed = Vec::new();
    for fixture in LIVE_FIXTURES {
        let outcome = match tokio::time::timeout(std::time::Duration::from_secs(20), live_open(&services, fixture)).await {
            Ok(Ok(session)) => live_check(fixture, &session).await,
            Ok(Err(err)) if live_unreachable(&err) => {
                eprintln!("跳过 {:?} 概览统计（夹具不在线）：{err}", fixture.engine);
                continue;
            }
            Ok(Err(err)) => Err(err),
            Err(_) => {
                eprintln!("跳过 {:?} 概览统计（连接超时）", fixture.engine);
                continue;
            }
        };
        match outcome {
            Ok(line) => seen.push(line),
            Err(err) => failed.push(format!("{:?}: {err}", fixture.engine)),
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    if seen.is_empty() && failed.is_empty() {
        eprintln!("跳过概览统计：本机没有 MySQL / PostgreSQL 夹具");
        return;
    }
    assert!(failed.is_empty(), "已看到：{}\n失败：{}", seen.join(" | "), failed.join(" | "));
    println!("{}", seen.join("\n"));
}
