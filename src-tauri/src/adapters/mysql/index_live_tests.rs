//! 真实服务器上的索引验收（R11）：读取索引类型、前缀长度、注释与可见性；新建、修改、删除索引都走预览再执行。
//!
//! 默认被 #[ignore] 跳过，需要 `docker compose -f docker/compose.yaml up -d mysql mariadb`。
//! 端口：RILLSQL_LIVE_MYSQL_PORT / RILLSQL_LIVE_MARIADB_PORT（默认 3306 / 3307）；
//! 只跑部分引擎时设 RILLSQL_INDEX_LIVE_ENGINES=mysql 或 mariadb（默认两个都跑）。

use std::sync::Arc;

use crate::application::ddl;
use crate::application::AppServices;
use crate::connections::model::{ConnectionDraft, Engine, Secret, SecretInput, TlsMode};
use crate::connections::Service;
use crate::ddl::model::{Action, ColumnDraft, DefaultSpec, IndexColumnDraft, IndexDraft, IndexKind, Plan, Snapshot, StepStatus, TableOptions};

const DATABASE: &str = "rillsql";

struct Live {
    services: Arc<AppServices>,
    id: String,
    engine: Engine,
    port: String,
}

impl Live {
    async fn connect(engine: Engine, port: String) -> Self {
        let dir = std::env::temp_dir().join(format!("rillsql-index-live-{}-{}", engine.as_str(), uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("临时目录");
        let services = Arc::new(AppServices::new(Arc::new(Service::new_for_test(dir.join("connections.sqlite")))));
        let id = format!("index-live-{}", engine.as_str());
        let mut draft = ConnectionDraft::blank(engine);
        draft.id = Some(id.clone());
        draft.name = id.clone();
        draft.host = "127.0.0.1".into();
        draft.port = port.clone();
        draft.username = "root".into();
        draft.database = DATABASE.into();
        draft.tls_mode = TlsMode::Off;
        draft.save_password = false;
        services.connections.save(draft, SecretInput::default()).await.expect("保存连接");
        let opened = services
            .connections
            .open(&id, SecretInput { password: Some(Secret::new("root")), ..Default::default() })
            .await
            .expect("打开连接");
        assert!(opened.connected, "{:?}", opened.report);
        Self { services, id, engine, port }
    }

    fn engine_name(&self) -> &'static str {
        match self.engine {
            Engine::Mariadb => "mariadb",
            _ => "mysql",
        }
    }

    async fn describe(&self, table: &str) -> Snapshot {
        ddl::describe_saved(&self.services.connections, &self.id, DATABASE.into(), String::new(), self.engine_name(), table)
            .await
            .unwrap_or_else(|err| panic!("读不到 {table}: {err}"))
    }

    /// 预览与执行走同一条链：执行提交的 SQL 就是预览里的语句。
    async fn run(&self, action: Action) -> Plan {
        let request = ddl::request(DATABASE.into(), String::new(), self.engine_name(), action).expect("请求");
        let plan = ddl::preview_request(&self.services.connections, &self.id, &request).await.expect("预览");
        assert!(plan.blocked.is_empty(), "预览被阻断：{:?}", plan.blocked);
        let sql: Vec<String> = plan.steps.iter().map(|step| step.sql.clone()).collect();
        let receipt = ddl::execute_request(&self.services.connections, &self.id, &request, &sql, true).await.expect("执行");
        assert!(receipt.blocked.is_empty(), "执行被阻断：{:?}", receipt.blocked);
        assert!(receipt.steps.iter().all(|step| step.status == StepStatus::Ok), "执行失败：{:?}", receipt.steps);
        for step in &plan.steps {
            println!("index-live: {} {}", self.engine_name(), step.sql);
        }
        plan
    }

    /// 测试夹具自己的语句（把索引设为不可见）：不经过结构编辑的预览。
    async fn raw(&self, sql: &str) {
        use mysql_async::prelude::Queryable;
        let url = format!("mysql://root:root@127.0.0.1:{}/{DATABASE}", self.port);
        let pool = mysql_async::Pool::new(url.as_str());
        let mut conn = pool.get_conn().await.expect("夹具连接");
        conn.query_drop(sql).await.unwrap_or_else(|err| panic!("{sql}: {err}"));
        drop(conn);
        pool.disconnect().await.ok();
    }
}

fn column(name: &str, type_name: &str) -> ColumnDraft {
    ColumnDraft { name: name.to_owned(), type_name: type_name.to_owned(), nullable: true, default: DefaultSpec::None, ..ColumnDraft::default() }
}

fn key(name: &str, descending: bool, prefix: Option<u32>) -> IndexColumnDraft {
    IndexColumnDraft { name: name.to_owned(), descending, prefix }
}

#[test]
#[ignore = "需要 docker/compose.yaml 的 MySQL（3306）与 MariaDB（3307）"]
fn index_kinds_prefix_comment_and_visibility_round_trip_on_real_servers() {
    let wanted = std::env::var("RILLSQL_INDEX_LIVE_ENGINES").unwrap_or_else(|_| "mysql,mariadb".to_owned());
    let mut engines = Vec::new();
    if wanted.contains("mysql") {
        engines.push((Engine::Mysql, std::env::var("RILLSQL_LIVE_MYSQL_PORT").unwrap_or_else(|_| "3306".to_owned())));
    }
    if wanted.contains("mariadb") {
        engines.push((Engine::Mariadb, std::env::var("RILLSQL_LIVE_MARIADB_PORT").unwrap_or_else(|_| "3307".to_owned())));
    }
    assert!(!engines.is_empty(), "RILLSQL_INDEX_LIVE_ENGINES 没有选中任何引擎");
    crate::app::shell::jobs::runtime().block_on(async move {
        for (engine, port) in engines {
            round_trip(Live::connect(engine, port).await).await;
        }
    });
}

async fn round_trip(live: Live) {
    let table = format!("t_index_{}_{}", live.engine.as_str(), std::process::id());
    let mut id = column("id", "bigint");
    id.nullable = false;
    id.primary = true;
    id.auto = true;
    live.run(Action::CreateTable {
        name: table.clone(),
        columns: vec![id, column("title", "varchar(200)"), column("body", "text"), column("code", "varchar(40)")],
        options: TableOptions::default(),
    })
    .await;

    // 新建：普通索引带前缀、降序与注释；全文索引。
    live.run(Action::CreateIndex {
        table: table.clone(),
        name: "idx_title".to_owned(),
        unique: false,
        columns: vec![key("title", true, Some(20)), key("code", false, None)],
        index_kind: IndexKind::Normal,
        comment: "按标题查".to_owned(),
    })
    .await;
    live.run(Action::CreateIndex {
        table: table.clone(),
        name: "ft_body".to_owned(),
        unique: false,
        columns: vec![key("body", false, None)],
        index_kind: IndexKind::Fulltext,
        comment: String::new(),
    })
    .await;
    let snapshot = live.describe(&table).await;
    assert!(snapshot.index_error.is_none(), "{:?}", snapshot.index_error);
    let primary = snapshot.indexes.iter().find(|index| index.name == "PRIMARY").expect("主键");
    assert_eq!(primary.kind, IndexKind::Primary);
    let title = snapshot.indexes.iter().find(|index| index.name == "idx_title").expect("idx_title");
    assert_eq!(title.kind, IndexKind::Normal);
    assert_eq!(title.method.as_deref(), Some("BTREE"));
    assert_eq!(title.columns[0].prefix, Some(20));
    assert!(title.columns[0].descending, "{:?}", title.columns);
    assert_eq!(title.columns[1].prefix, None);
    assert_eq!(title.comment.as_deref(), Some("按标题查"));
    assert_eq!(title.visible, Some(true), "MySQL 8.0+ / MariaDB 10.6+ 读得到可见性");
    let body = snapshot.indexes.iter().find(|index| index.name == "ft_body").expect("ft_body");
    assert_eq!(body.kind, IndexKind::Fulltext);
    assert_eq!(body.method.as_deref(), Some("FULLTEXT"));
    println!("index-live: {} 读回 {:?}", live.engine_name(), snapshot.indexes.iter().map(|index| (&index.name, index.kind, &index.comment, index.visible)).collect::<Vec<_>>());

    // 不可见索引：读回 visible = false，修改后仍不可见。
    let hide = match live.engine {
        Engine::Mariadb => format!("ALTER TABLE `{table}` ALTER INDEX `idx_title` IGNORED"),
        _ => format!("ALTER TABLE `{table}` ALTER INDEX `idx_title` INVISIBLE"),
    };
    live.raw(&hide).await;
    let hidden = live.describe(&table).await;
    assert_eq!(hidden.indexes.iter().find(|index| index.name == "idx_title").and_then(|index| index.visible), Some(false));

    // 修改：一条 ALTER TABLE，换成唯一索引、改名、改注释。
    let plan = live
        .run(Action::AlterIndex {
            table: table.clone(),
            old: "idx_title".to_owned(),
            new: IndexDraft { name: "uq_code".to_owned(), kind: IndexKind::Unique, columns: vec![key("code", false, None), key("title", false, Some(30))], comment: "唯一".to_owned() },
        })
        .await;
    assert_eq!(plan.steps.len(), 1, "MySQL 家族一条语句修改索引");
    let altered = live.describe(&table).await;
    assert!(altered.indexes.iter().all(|index| index.name != "idx_title"));
    let unique = altered.indexes.iter().find(|index| index.name == "uq_code").expect("uq_code");
    assert_eq!(unique.kind, IndexKind::Unique);
    assert_eq!(unique.columns.iter().map(|column| (column.name.as_str(), column.prefix)).collect::<Vec<_>>(), vec![("code", None), ("title", Some(30))]);
    assert_eq!(unique.comment.as_deref(), Some("唯一"));
    assert_eq!(unique.visible, Some(false), "修改后保持不可见");

    // 删除。
    live.run(Action::DropIndex { table: table.clone(), name: "uq_code".to_owned() }).await;
    live.run(Action::DropIndex { table: table.clone(), name: "ft_body".to_owned() }).await;
    let dropped = live.describe(&table).await;
    assert_eq!(dropped.indexes.iter().map(|index| index.name.as_str()).collect::<Vec<_>>(), vec!["PRIMARY"]);
    live.run(Action::DropTable { table }).await;
    println!("index-live: {} 全部用例完成", live.engine_name());
}

/// 联合主键（R13）：用通用夹具 shop.order_lines 的副本，把单列主键经预览改成联合主键、再调换顺序；
/// 读回后字段页（列上的主键标记与主键序号）和索引页（PRIMARY 的列顺序）都显示联合主键。
#[test]
#[ignore = "需要 docker/compose.yaml 的 MySQL（3306）与 MariaDB（3307），以及 mysql-ux-round3.sql 的 shop.order_lines"]
fn composite_primary_key_change_goes_through_preview_on_real_servers() {
    let wanted = std::env::var("RILLSQL_INDEX_LIVE_ENGINES").unwrap_or_else(|_| "mysql,mariadb".to_owned());
    let mut engines = Vec::new();
    if wanted.contains("mysql") {
        engines.push((Engine::Mysql, std::env::var("RILLSQL_LIVE_MYSQL_PORT").unwrap_or_else(|_| "3306".to_owned())));
    }
    if wanted.contains("mariadb") {
        engines.push((Engine::Mariadb, std::env::var("RILLSQL_LIVE_MARIADB_PORT").unwrap_or_else(|_| "3307".to_owned())));
    }
    assert!(!engines.is_empty(), "RILLSQL_INDEX_LIVE_ENGINES 没有选中任何引擎");
    crate::app::shell::jobs::runtime().block_on(async move {
        for (engine, port) in engines {
            composite_primary_key(Live::connect(engine, port).await).await;
        }
    });
}

fn with_primary_key(snapshot: &Snapshot, order: &[&str]) -> Vec<ColumnDraft> {
    let mut drafts = crate::ddl::plan::identity_drafts(snapshot);
    for draft in &mut drafts {
        let position = order.iter().position(|name| *name == draft.name);
        draft.primary = position.is_some();
        draft.pk_order = position.map(|value| value as u16);
    }
    drafts
}

fn primary_columns(snapshot: &Snapshot) -> Vec<String> {
    snapshot.indexes.iter().find(|index| index.name == "PRIMARY").map(|index| index.columns.iter().map(|column| column.name.clone()).collect()).unwrap_or_default()
}

async fn composite_primary_key(live: Live) {
    let table = format!("order_lines_pk_{}_{}", live.engine.as_str(), std::process::id());
    live.raw(&format!("DROP TABLE IF EXISTS `{table}`")).await;
    live.raw(&format!("CREATE TABLE `{table}` LIKE shop.order_lines")).await;
    live.raw(&format!("INSERT INTO `{table}` SELECT * FROM shop.order_lines")).await;
    // 起点：单列主键 sku。
    live.raw(&format!("ALTER TABLE `{table}` DROP PRIMARY KEY, ADD PRIMARY KEY (sku)")).await;
    let single = live.describe(&table).await;
    assert_eq!(primary_columns(&single), vec!["sku"]);

    // 单列主键改成联合主键：一条 DROP PRIMARY KEY, ADD PRIMARY KEY，带危险确认的风险说明。
    let plan = live.run(Action::AlterColumns { table: table.clone(), columns: with_primary_key(&single, &["order_id", "line_no"]) }).await;
    assert_eq!(plan.steps.len(), 1);
    assert!(plan.steps[0].sql.ends_with("DROP PRIMARY KEY, ADD PRIMARY KEY (order_id, line_no);"), "{}", plan.steps[0].sql);
    assert!(plan.risks.iter().any(|risk| risk.level == "danger"), "{:?}", plan.risks);
    let composite = live.describe(&table).await;
    assert_eq!(primary_columns(&composite), vec!["order_id", "line_no"], "索引页：PRIMARY 按新顺序");
    let flagged: Vec<&str> = composite.columns.iter().filter(|column| column.primary).map(|column| column.name.as_str()).collect();
    assert_eq!(flagged, vec!["order_id", "line_no"], "字段页：两列都标为主键");
    let drafts = crate::ddl::plan::identity_drafts(&composite);
    let orders: Vec<(&str, Option<u16>)> = drafts.iter().filter(|draft| draft.primary).map(|draft| (draft.name.as_str(), draft.pk_order)).collect();
    assert_eq!(orders, vec![("order_id", Some(0)), ("line_no", Some(1))], "字段页的主键序号");

    // 只调换主键顺序。
    let plan = live.run(Action::AlterColumns { table: table.clone(), columns: with_primary_key(&composite, &["line_no", "order_id"]) }).await;
    assert!(plan.steps[0].sql.ends_with("DROP PRIMARY KEY, ADD PRIMARY KEY (line_no, order_id);"), "{}", plan.steps[0].sql);
    let swapped = live.describe(&table).await;
    assert_eq!(primary_columns(&swapped), vec!["line_no", "order_id"]);
    let drafts = crate::ddl::plan::identity_drafts(&swapped);
    let orders: Vec<(&str, Option<u16>)> = drafts.iter().filter(|draft| draft.primary).map(|draft| (draft.name.as_str(), draft.pk_order)).collect();
    assert_eq!(orders, vec![("order_id", Some(1)), ("line_no", Some(0))]);

    live.run(Action::DropTable { table }).await;
    println!("pk-live: {} 联合主键用例完成", live.engine_name());
}
