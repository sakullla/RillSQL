//! 本机夹具上的结构比较和关系图。连接走 AppServices，临时对象比较后删除，不改种子行。

use std::sync::Arc;

use mysql_async::prelude::Queryable;

use crate::application::{AppServices, TaskBoard, TaskOutcome};
use crate::app::shell::WorkbenchModel;
use crate::app::Shell;
use crate::compare::{SideRequest, StructureRequest};
use crate::connections::model::{ConnectionDraft, Engine, TlsMode};
use crate::connections::CatalogSession;

struct LiveEngine {
    id: &'static str,
    name: &'static str,
    engine: Engine,
    engine_name: &'static str,
    port: u16,
    user: &'static str,
    password: &'static str,
    tls: TlsMode,
    /// Schema：同一库里的两个临时 schema。Database：两个临时库。
    schema_scope: bool,
    diagram_schema: &'static str,
    customers: i64,
}

#[tokio::test]
async fn live_engines_compare_a_made_difference_and_read_the_diagram() {
    let dir = std::env::temp_dir().join(format!("rillsql-live-compare-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
    let engines = [
        LiveEngine {
            id: "live-pg",
            name: "夹具 PostgreSQL",
            engine: Engine::Postgres,
            engine_name: "postgres",
            port: 5432,
            user: "postgres",
            password: "postgres",
            tls: TlsMode::Off,
            schema_scope: true,
            diagram_schema: "public",
            customers: 2,
        },
        LiveEngine {
            id: "live-mysql",
            name: "夹具 MySQL",
            engine: Engine::Mysql,
            engine_name: "mysql",
            port: 3306,
            user: "root",
            password: "root",
            tls: TlsMode::Off,
            schema_scope: false,
            diagram_schema: "",
            customers: 2,
        },
        LiveEngine {
            id: "live-maria",
            name: "夹具 MariaDB",
            engine: Engine::Mariadb,
            engine_name: "mariadb",
            port: 3307,
            user: "root",
            password: "root",
            tls: TlsMode::Off,
            schema_scope: false,
            diagram_schema: "",
            customers: 2,
        },
        LiveEngine {
            id: "live-mssql",
            name: "夹具 SQL Server",
            engine: Engine::Sqlserver,
            engine_name: "sqlserver",
            port: 1433,
            user: "sa",
            password: "Rillsql-Passw0rd!",
            tls: TlsMode::Require,
            schema_scope: true,
            diagram_schema: "dbo",
            customers: 3,
        },
    ];
    let mut seen = Vec::new();
    let mut errors = Vec::new();
    for spec in &engines {
        match exercise(&services, spec).await {
            Ok(line) => seen.push(line),
            Err(err) => errors.push(format!("{}: {err}", spec.engine_name)),
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    if seen.is_empty() && errors.iter().all(|err| fixture_unreachable(err)) {
        eprintln!("跳过结构比较：本机没有数据库夹具");
        return;
    }
    assert!(errors.is_empty(), "已看到：{}\n失败：{}", seen.join(" | "), errors.join(" | "));
    println!("{}", seen.join("\n"));
}

fn fixture_unreachable(err: &str) -> bool {
    let text = err.to_ascii_lowercase();
    err.contains("没有打开会话")
        || err.contains("会话没有建立")
        || text.contains("connection refused")
        || text.contains("error connecting")
        || text.contains("os error 61")
        || text.contains("os error 10061")
}

async fn exercise(services: &AppServices, spec: &LiveEngine) -> Result<String, String> {
    open_fixture(services, spec).await?;
    let session = services.connections.catalog_session(spec.id).ok_or_else(|| "打开后没有目录会话".to_owned())?;
    let outcome = async {
        let current = scalar(&session, current_database_sql(spec.engine_name)).await?;
        if !current.eq_ignore_ascii_case("rillsql") {
            return Err(format!("连到的库是 {current}，不是夹具库 rillsql"));
        }
        let customers = count(&session, &count_sql(spec.engine_name, "customers")).await?;
        let orders = count(&session, &count_sql(spec.engine_name, "orders")).await?;
        if customers != spec.customers {
            return Err(format!("customers 行数是 {customers}，夹具种子应为 {}", spec.customers));
        }
        let suffix = format!("{:x}", uuid::Uuid::new_v4().as_u128());
        let suffix = &suffix[..8];
        let baseline_name = format!("t7a{suffix}");
        let target_name = format!("t7b{suffix}");
        let prepared = async {
            create_side(&session, spec.engine_name, &baseline_name, true).await?;
            create_side(&session, spec.engine_name, &target_name, false).await?;
            let baseline_note = count(&session, &note_column_sql(spec.engine_name, &baseline_name)).await?;
            if baseline_note != 1 {
                return Err(format!("基准临时表没有 note 列（{baseline_note}）"));
            }
            let (baseline, target) = sides(spec, &current, &baseline_name, &target_name);
            let tasks = TaskBoard::new();
            let token = tasks.begin("compare");
            let request = StructureRequest {
                id: format!("live-{}", spec.engine_name),
                baseline,
                target,
            };
            let outcome = crate::application::work::compare_structure(&services.connections, &tasks, "compare", token, &request).await;
            let report = match outcome {
                TaskOutcome::Current { value, .. } => value,
                TaskOutcome::Cancelled { .. } => return Err("结构比较被标成已取消".to_owned()),
                TaskOutcome::Stale { .. } => return Err("结构比较被标成过期".to_owned()),
            };
            if report.refused.is_some() || report.incomplete {
                return Err(format!("比较没有给出完整差异：refused={:?} incomplete={} notes={:?}", report.refused, report.incomplete, report.notes));
            }
            let blob = format!("{:?} {} {}", report.changed, report.script, report.only_baseline.iter().map(|item| item.name.clone()).collect::<Vec<_>>().join(" "));
            if !blob.contains("t7_marker") || !blob.to_ascii_lowercase().contains("note") {
                return Err(format!("没有看见临时列差异：{blob}"));
            }
            let mut model = WorkbenchModel::new(Shell::default());
            let editor_database = if spec.schema_scope { current.clone() } else { target_name.clone() };
            model.open_bound_query(report.script.clone(), Some(spec.id.to_owned()), Some(spec.name.to_owned()), Some(editor_database), Some(spec.engine_name.to_owned()));
            let tab = model.active_tab().ok_or_else(|| "脚本没有进入查询标签".to_owned())?;
            if !tab.draft.contains("t7_marker") || tab.last_executed_sql.is_some() || tab.running || model.executions != 0 {
                return Err("变更脚本没有只停在查询编辑器".to_owned());
            }
            let still_missing = count(&session, &note_column_sql(spec.engine_name, &target_name)).await?;
            if still_missing != 0 {
                return Err(format!("对照临时表出现了 note 列，脚本被执行了（{still_missing}）"));
            }
            let diagram = crate::application::work::read_diagram(&services.connections, spec.id, &current, spec.diagram_schema).await?;
            let has = |name: &str| diagram.tables.iter().any(|table| table.name.eq_ignore_ascii_case(name));
            if !has("customers") || !has("orders") {
                return Err(format!("关系图缺少种子表：{:?}", diagram.tables.iter().map(|table| table.name.as_str()).collect::<Vec<_>>()));
            }
            if diagram.tables.iter().any(|table| table.name.eq_ignore_ascii_case("paid_orders") || table.name.eq_ignore_ascii_case("count_paid_orders")) {
                return Err("关系图把视图或过程画成了表".to_owned());
            }
            let linked = diagram.edges.iter().any(|edge| {
                edge.from_table.eq_ignore_ascii_case("orders")
                    && edge.to_table.eq_ignore_ascii_case("customers")
                    && edge.from_columns.iter().any(|column| column.eq_ignore_ascii_case("customer_id"))
            });
            if !linked {
                return Err(format!("关系图没有 orders.customer_id → customers：{:?}", diagram.edges));
            }
            let customers_after = count(&session, &count_sql(spec.engine_name, "customers")).await?;
            let orders_after = count(&session, &count_sql(spec.engine_name, "orders")).await?;
            if customers_after != customers || orders_after != orders {
                return Err(format!("种子行被改动 customers {customers}->{customers_after} orders {orders}->{orders_after}"));
            }
            let snippet = report
                .script
                .lines()
                .find(|line| line.to_ascii_lowercase().contains("note") || line.contains("t7_marker"))
                .unwrap_or("")
                .chars()
                .take(160)
                .collect::<String>();
            Ok(format!(
                "{} 已连接 {}，结构差异 t7_marker.note（{}），关系图 {} 张表、orders→customers 外键在，种子 customers={} orders={}",
                spec.engine_name,
                current,
                snippet,
                diagram.tables.len(),
                customers,
                orders
            ))
        }
        .await;
        let cleaned = cleanup_scopes(&session, spec.engine_name, &[&target_name, &baseline_name]).await;
        match (prepared, cleaned) {
            (Ok(line), Ok(())) => Ok(line),
            (Ok(line), Err(err)) => Err(format!("{line}；清理失败 {err}")),
            (Err(err), Ok(())) => Err(err),
            (Err(err), Err(cleanup_err)) => Err(format!("{err}；清理失败 {cleanup_err}")),
        }
    }
    .await;
    services.disconnect(spec.id);
    outcome
}

async fn open_fixture(services: &AppServices, spec: &LiveEngine) -> Result<(), String> {
    let mut draft = ConnectionDraft::blank(spec.engine);
    draft.id = Some(spec.id.to_owned());
    draft.name = spec.name.to_owned();
    draft.host = "127.0.0.1".to_owned();
    draft.port = spec.port.to_string();
    draft.database = "rillsql".to_owned();
    draft.username = spec.user.to_owned();
    draft.tls_mode = spec.tls;
    draft.save_password = true;
    services
        .save_connection(draft, Some(spec.password.to_owned()), None, None)
        .await
        .map_err(|err| err.message())?;
    let opened = services
        .open_connection(spec.id, Some(spec.password.to_owned()), None, None)
        .await?;
    if !opened.connected || services.connections.catalog_session(spec.id).is_none() {
        let stages = opened
            .report
            .stages
            .iter()
            .map(|stage| format!("{}:{:?}:{}", stage.label, stage.status, stage.message))
            .collect::<Vec<_>>()
            .join(" | ");
        return Err(format!(
            "没有打开会话：{} {}",
            opened.report.summary.unwrap_or_default(),
            stages
        ));
    }
    Ok(())
}

fn sides(spec: &LiveEngine, current: &str, baseline_name: &str, target_name: &str) -> (SideRequest, SideRequest) {
    let (baseline_db, baseline_schema, target_db, target_schema) = if spec.schema_scope {
        (current.to_owned(), baseline_name.to_owned(), current.to_owned(), target_name.to_owned())
    } else {
        (baseline_name.to_owned(), String::new(), target_name.to_owned(), String::new())
    };
    let side = |database, schema| SideRequest {
        connection_id: spec.id.to_owned(),
        database,
        schema,
        engine: spec.engine_name.to_owned(),
    };
    (side(baseline_db, baseline_schema), side(target_db, target_schema))
}

fn current_database_sql(engine: &str) -> &'static str {
    match engine {
        "postgres" => "SELECT current_database()::text",
        "sqlserver" => "SELECT DB_NAME()",
        _ => "SELECT DATABASE()",
    }
}

fn count_sql(engine: &str, table: &str) -> String {
    match engine {
        "postgres" => format!("SELECT COUNT(*)::text FROM {table}"),
        "sqlserver" => format!("SELECT CONVERT(nvarchar(40), COUNT(*)) FROM {table}"),
        _ => format!("SELECT CAST(COUNT(*) AS CHAR) FROM {table}"),
    }
}

fn note_column_sql(engine: &str, scope: &str) -> String {
    match engine {
        "postgres" => format!(
            "SELECT COUNT(*)::text FROM information_schema.columns WHERE table_schema = '{scope}' AND table_name = 't7_marker' AND column_name = 'note'"
        ),
        "sqlserver" => format!(
            "SELECT CONVERT(nvarchar(40), COUNT(*)) FROM sys.columns c JOIN sys.tables t ON t.object_id = c.object_id JOIN sys.schemas s ON s.schema_id = t.schema_id WHERE s.name = N'{scope}' AND t.name = N't7_marker' AND c.name = N'note'"
        ),
        _ => format!(
            "SELECT CAST(COUNT(*) AS CHAR) FROM information_schema.columns WHERE table_schema = '{scope}' AND table_name = 't7_marker' AND column_name = 'note'"
        ),
    }
}

async fn cleanup_scopes(session: &CatalogSession, engine: &str, names: &[&str]) -> Result<(), String> {
    let mut errors = Vec::new();
    for name in names {
        if let Err(err) = drop_scope(session, engine, name).await {
            errors.push(format!("{name}: {err}"));
        }
    }
    if errors.is_empty() { Ok(()) } else { Err(errors.join("；")) }
}

async fn drop_scope(session: &CatalogSession, engine: &str, name: &str) -> Result<(), String> {
    match engine {
        "postgres" => exec(session, &format!("DROP SCHEMA IF EXISTS {name} CASCADE")).await,
        "sqlserver" => {
            let exists = count(session, &format!("SELECT CONVERT(nvarchar(40), CASE WHEN SCHEMA_ID(N'{name}') IS NULL THEN 0 ELSE 1 END)")).await?;
            if exists == 0 {
                return Ok(());
            }
            exec(session, &format!("DROP TABLE IF EXISTS {name}.t7_marker")).await?;
            exec(session, &format!("DROP SCHEMA {name}")).await
        }
        _ => exec(session, &format!("DROP DATABASE IF EXISTS {name}")).await,
    }
}

async fn create_side(session: &CatalogSession, engine: &str, name: &str, with_note: bool) -> Result<(), String> {
    match engine {
        "postgres" => {
            exec(session, &format!("CREATE SCHEMA {name}")).await?;
            let columns = if with_note { "id integer PRIMARY KEY, note text" } else { "id integer PRIMARY KEY" };
            exec(session, &format!("CREATE TABLE {name}.t7_marker ({columns})")).await
        }
        "sqlserver" => {
            exec(session, &format!("CREATE SCHEMA {name}")).await?;
            let columns = if with_note { "id int NOT NULL PRIMARY KEY, note nvarchar(40) NULL" } else { "id int NOT NULL PRIMARY KEY" };
            exec(session, &format!("CREATE TABLE {name}.t7_marker ({columns})")).await
        }
        _ => {
            exec(session, &format!("CREATE DATABASE {name}")).await?;
            let columns = if with_note { "id int PRIMARY KEY, note varchar(40)" } else { "id int PRIMARY KEY" };
            exec(session, &format!("CREATE TABLE {name}.t7_marker ({columns})")).await
        }
    }
}

async fn count(session: &CatalogSession, sql: &str) -> Result<i64, String> {
    let text = scalar(session, sql).await?;
    text.trim().parse::<i64>().map_err(|_| format!("计数无效 {text}"))
}

async fn scalar(session: &CatalogSession, sql: &str) -> Result<String, String> {
    match session {
        CatalogSession::Postgres(client) => {
            let row = client.query_one(sql, &[]).await.map_err(|err| err.to_string())?;
            row.try_get(0).map_err(|err| err.to_string())
        }
        CatalogSession::Mysql(pool) => mysql_scalar(pool, sql).await,
        CatalogSession::Sqlserver(client) => {
            let (_names, rows) = crate::grid::sqlserver_rows(client, sql).await?;
            match rows.first().and_then(|row| row.first()) {
                Some(crate::grid::Cell::Text(text)) => Ok(text.clone()),
                _ => Err("没有计数".to_owned()),
            }
        }
        _ => Err("这不是已打开的关系库会话".to_owned()),
    }
}

async fn exec(session: &CatalogSession, sql: &str) -> Result<(), String> {
    match session {
        CatalogSession::Postgres(client) => {
            client.execute(sql, &[]).await.map(|_| ()).map_err(|err| err.to_string())
        }
        CatalogSession::Mysql(pool) => {
            let mut conn = pool.get_conn().await.map_err(|err| err.to_string())?;
            conn.query_drop(sql).await.map_err(|err| err.to_string())
        }
        // CREATE SCHEMA 不能包进 sp_executesql。simple_query 单独成批。
        CatalogSession::Sqlserver(client) => crate::grid::sqlserver_rows(client, sql).await.map(|_| ()),
        _ => Err("这不是已打开的关系库会话".to_owned()),
    }
}

async fn mysql_scalar(pool: &mysql_async::Pool, sql: &str) -> Result<String, String> {
    let mut conn = pool.get_conn().await.map_err(|err| err.to_string())?;
    let mut result = conn.query_iter(sql).await.map_err(|err| err.to_string())?;
    let row = result.next().await.map_err(|err| err.to_string())?.ok_or_else(|| "没有计数".to_owned())?;
    let text = match row.as_ref(0) {
        Some(mysql_async::Value::Bytes(bytes)) => String::from_utf8(bytes.clone()).map_err(|err| err.to_string())?,
        Some(mysql_async::Value::Int(number)) => number.to_string(),
        Some(mysql_async::Value::UInt(number)) => number.to_string(),
        Some(other) => other.as_sql(true),
        None => return Err("没有计数列".to_owned()),
    };
    while result.next().await.map_err(|err| err.to_string())?.is_some() {}
    Ok(text)
}
