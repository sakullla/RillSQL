//! 真实服务器上的表结构验收（R2 / R3 / R4 / R8 / R9）。
//!
//! 这些用例只在 scripts/verify-mysql-table.py 启动的 MySQL 与 MariaDB 夹具上运行，默认被
//! #[ignore] 跳过：单元测试不能替代真实服务器上的增删改与读回。脚本核对每个引擎都真的跑过，
//! 并把实际版本写进 docs/verification/2026-11-21-mysql-proprietary-support/table-live.md。
//!
//! 端口来自环境变量：RILLSQL_TABLE_MYSQL_PORT / RILLSQL_TABLE_MARIADB_PORT（默认 3426 / 3427）。

use std::sync::Arc;

use crate::application::ddl;
use crate::application::AppServices;
use crate::connections::model::{ConnectionDraft, Engine, Secret, SecretInput, TlsMode};
use crate::connections::Service;
use crate::ddl::model::{
    Action, ColumnDraft, DefaultSpec, EnumSetDraft, EnumSetKind, GeneratedColumnDraft, GeneratedStorage, PartitionDef, PartitionDraft,
    PartitionMethod, PartitionOp, Plan, Snapshot, StepStatus, TableOptions,
};

/// 夹具里的库名（docker/init/mysql-table.sql 会建好；脚本再执行一次也是 IF NOT EXISTS）。
const DATABASE: &str = "rillsql_proprietary";

/// 一个真实服务器夹具：连接已经保存并打开。
struct Live {
    services: Arc<AppServices>,
    id: String,
    engine: Engine,
}

impl Live {
    async fn connect(engine: Engine, port: String) -> Self {
        let dir = std::env::temp_dir().join(format!("rillsql-table-live-{}-{}", engine.as_str(), uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("临时目录");
        let services = Arc::new(AppServices::new(Arc::new(Service::new_for_test(dir.join("connections.sqlite")))));
        let id = format!("live-{}", engine.as_str());
        let mut draft = ConnectionDraft::blank(engine);
        draft.id = Some(id.clone());
        draft.name = id.clone();
        draft.host = "127.0.0.1".into();
        draft.port = port;
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
        Self { services, id, engine }
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

    fn request(&self, action: Action) -> crate::ddl::Request {
        ddl::request(DATABASE.into(), String::new(), self.engine_name(), action).expect("请求")
    }

    async fn preview(&self, action: Action) -> Plan {
        let request = self.request(action);
        ddl::preview_request(&self.services.connections, &self.id, &request).await.expect("预览")
    }

    /// 预览与执行走同一条链：执行提交的 SQL 就是预览里的语句（不一致会被执行层拒绝）。
    async fn run(&self, action: Action) -> (Plan, crate::ddl::Receipt) {
        let request = self.request(action);
        let plan = ddl::preview_request(&self.services.connections, &self.id, &request).await.expect("预览");
        assert!(plan.blocked.is_empty(), "预览被阻断：{:?}", plan.blocked);
        let sql: Vec<String> = plan.steps.iter().map(|step| step.sql.clone()).collect();
        let receipt = ddl::execute_request(&self.services.connections, &self.id, &request, &sql, true).await.expect("执行");
        assert!(receipt.blocked.is_empty(), "执行被阻断：{:?}", receipt.blocked);
        (plan, receipt)
    }

    async fn create_table(&self, name: &str, columns: Vec<ColumnDraft>) {
        let (plan, receipt) = self.run(Action::CreateTable { name: name.to_owned(), columns, options: TableOptions::default() }).await;
        println!("table-live: {} 建表 {name} -> {}", self.engine_name(), plan.steps[0].sql.replace('\n', " "));
        assert!(receipt.steps.iter().all(|step| step.status == StepStatus::Ok), "建表 {name} 失败：{:?}", receipt.steps);
    }

    async fn drop_table(&self, name: &str) {
        let _ = self.run(Action::DropTable { table: name.to_owned() }).await;
    }
}

/// 服务器读回的生成表达式带方言的引号写法（MySQL 8 报 (`price` * `quantity`)），
/// 比较时按"去掉空白与引号"的写法对照，避免把引号风格当成差异。
fn normalized(expression: &str) -> String {
    expression
        .chars()
        .filter(|ch| !ch.is_whitespace() && *ch != '`' && *ch != '(' && *ch != ')')
        .collect::<String>()
        .to_ascii_lowercase()
}

fn plain_column(name: &str, type_name: &str) -> ColumnDraft {
    ColumnDraft {
        original: None,
        name: name.to_owned(),
        type_name: type_name.to_owned(),
        nullable: true,
        default: DefaultSpec::None,
        comment: String::new(),
        primary: false,
        auto: false,
        dropped: false,
        enum_set: None,
        generated: None,
    }
}

fn id_column() -> ColumnDraft {
    let mut id = plain_column("id", "bigint");
    id.nullable = false;
    id.primary = true;
    id.auto = true;
    id
}

/// 从读回的快照起一份完整草稿：改一项也要带上所有列，否则 precheck 会拦下漏改的列。
fn drafts_of(snapshot: &Snapshot) -> Vec<ColumnDraft> {
    snapshot
        .columns
        .iter()
        .map(|column| ColumnDraft {
            original: Some(column.name.clone()),
            name: column.name.clone(),
            type_name: column.type_name.clone(),
            nullable: column.nullable,
            default: crate::ddl::sql::existing_default(snapshot.engine, column.default_sql.as_deref()),
            comment: String::new(),
            primary: column.primary,
            auto: column.auto,
            dropped: false,
            enum_set: snapshot
                .enum_sets
                .get(&column.name)
                .map(|state| EnumSetDraft { kind: state.kind, values: state.values.clone() }),
            generated: snapshot
                .generation
                .get(&column.name)
                .map(|state| GeneratedColumnDraft { expression: state.expression.clone(), storage: state.storage }),
        })
        .collect()
}

/// 整轮验收：ENUM/SET、生成列、四类分区与失败路径都在真实服务器上走一遍。
#[test]
#[ignore = "需要 scripts/verify-mysql-table.py 启动的 MySQL 与 MariaDB 夹具"]
fn table_features_round_trip_on_real_servers() {
    let mysql_port = std::env::var("RILLSQL_TABLE_MYSQL_PORT").unwrap_or_else(|_| "3426".to_owned());
    let mariadb_port = std::env::var("RILLSQL_TABLE_MARIADB_PORT").unwrap_or_else(|_| "3427".to_owned());
    crate::app::shell::jobs::runtime().block_on(async move {
        for (engine, port) in [(Engine::Mysql, mysql_port), (Engine::Mariadb, mariadb_port)] {
            run_engine(engine, port).await;
        }
    });
}

async fn run_engine(engine: Engine, port: String) {
    let live = Live::connect(engine, port).await;
    let suffix = format!("{}_{}", engine.as_str(), std::process::id());
    enum_set_round_trip(&live, &suffix).await;
    generated_column_round_trip(&live, &suffix).await;
    partition_round_trip(&live, &suffix).await;
    partition_failure_keeps_the_definition(&live, &suffix).await;
    println!("table-live: {} 全部用例完成", live.engine_name());
}

/// R2：ENUM/SET 的候选值保持顺序与特殊字符，新增/修改/删除都读回确认。
async fn enum_set_round_trip(live: &Live, suffix: &str) {
    let table = format!("t2_enum_{suffix}");
    let backslash = format!("a{}b", char::from(92));
    let values = vec![String::new(), "it's".to_owned(), backslash.clone(), "a,b".to_owned(), "中文".to_owned()];
    let mut status = plain_column("status", "enum");
    status.enum_set = Some(EnumSetDraft { kind: EnumSetKind::Enum, values: values.clone() });
    let mut tags = plain_column("tags", "set");
    tags.enum_set = Some(EnumSetDraft { kind: EnumSetKind::Set, values: vec!["x".to_owned(), "y'.z".to_owned()] });
    live.create_table(&table, vec![id_column(), status, tags]).await;
    let snapshot = live.describe(&table).await;
    let read_back = snapshot.enum_sets.get("status").expect("读回 ENUM 候选值");
    assert_eq!(read_back.values, values, "{}: 候选值顺序与特殊字符要原样读回", live.engine_name());
    assert_eq!(read_back.kind, EnumSetKind::Enum);
    println!("table-live: {} ENUM 候选值读回 {:?}", live.engine_name(), read_back.values);
    let set_back = snapshot.enum_sets.get("tags").expect("读回 SET 候选值");
    assert_eq!(set_back.kind, EnumSetKind::Set);
    assert_eq!(set_back.values, vec!["x".to_owned(), "y'.z".to_owned()]);

    // 修改：追加一个候选值，读回反映修改。
    let mut drafts = drafts_of(&snapshot);
    let enum_set = drafts[1].enum_set.as_mut().expect("status 是 ENUM");
    enum_set.values.push("archived".to_owned());
    let (plan, receipt) = live.run(Action::AlterColumns { table: table.clone(), columns: drafts }).await;
    println!("table-live: {} 修改 ENUM -> {}", live.engine_name(), plan.steps[0].sql);
    assert!(receipt.steps.iter().all(|step| step.status == StepStatus::Ok), "{:?}", receipt.steps);
    let snapshot = live.describe(&table).await;
    assert_eq!(snapshot.enum_sets["status"].values, {
        let mut expected = values.clone();
        expected.push("archived".to_owned());
        expected
    });

    // 删除字段：读回不再包含它。
    let mut drafts = drafts_of(&snapshot);
    drafts[2].dropped = true;
    let (_, receipt) = live.run(Action::AlterColumns { table: table.clone(), columns: drafts }).await;
    assert!(receipt.steps.iter().all(|step| step.status == StepStatus::Ok), "{:?}", receipt.steps);
    let snapshot = live.describe(&table).await;
    assert!(!snapshot.enum_sets.contains_key("tags"), "{}: 删除后不该再有 SET 字段", live.engine_name());

    // 空候选值是客户端就能判断的无效定义：预览阻断，不发送语句。
    let mut drafts = drafts_of(&snapshot);
    drafts[1].enum_set.as_mut().expect("status 是 ENUM").values.clear();
    let plan = live.preview(Action::AlterColumns { table: table.clone(), columns: drafts }).await;
    assert!(plan.blocked.iter().any(|text| text.contains("至少要有一个候选值")), "{:?}", plan.blocked);
    assert!(plan.steps.is_empty());
    println!("table-live: {} 空候选值被阻断：{:?}", live.engine_name(), plan.blocked);
    live.drop_table(&table).await;
}

/// R3：生成列的表达式与存储方式读回一致；普通表达式默认值不被当成生成列。
async fn generated_column_round_trip(live: &Live, suffix: &str) {
    let table = format!("t2_gen_{suffix}");
    let mut price = plain_column("price", "int");
    price.nullable = false;
    price.default = DefaultSpec::Literal("0".to_owned());
    let mut quantity = plain_column("quantity", "int");
    quantity.nullable = false;
    quantity.default = DefaultSpec::Literal("1".to_owned());
    let mut virtual_total = plain_column("total", "int");
    virtual_total.generated = Some(GeneratedColumnDraft { expression: "price * quantity".to_owned(), storage: GeneratedStorage::Virtual });
    // 普通列 + 表达式默认值：它不是生成列，读回也不能出现在生成列里。
    let mut created = plain_column("created_at", "datetime");
    created.default = DefaultSpec::Expression("CURRENT_TIMESTAMP".to_owned());
    live.create_table(&table, vec![id_column(), price, quantity, virtual_total, created]).await;
    let snapshot = live.describe(&table).await;
    let generated = snapshot.generation.get("total").expect("读回 VIRTUAL 生成列");
    assert_eq!(normalized(&generated.expression), "price*quantity");
    assert_eq!(generated.storage, GeneratedStorage::Virtual);
    assert!(!snapshot.generation.contains_key("created_at"), "表达式默认值不能读成生成列");
    assert!(snapshot.columns.iter().find(|column| column.name == "created_at").is_some_and(|column| !column.generated));
    println!("table-live: {} VIRTUAL 生成列读回 {} ({})", live.engine_name(), generated.expression, generated.reported_storage);

    // 追加一个持久化生成列：关键字按方言（MySQL STORED / MariaDB PERSISTENT）。
    let mut drafts = drafts_of(&snapshot);
    let mut stored = plain_column("total_x2", "int");
    stored.generated = Some(GeneratedColumnDraft { expression: "total * 2".to_owned(), storage: GeneratedStorage::Stored });
    drafts.push(stored);
    let (plan, receipt) = live.run(Action::AlterColumns { table: table.clone(), columns: drafts }).await;
    println!("table-live: {} 新增持久化生成列 -> {}", live.engine_name(), plan.steps[0].sql);
    assert!(receipt.steps.iter().all(|step| step.status == StepStatus::Ok), "{:?}", receipt.steps);
    let snapshot = live.describe(&table).await;
    let stored = snapshot.generation.get("total_x2").expect("读回持久化生成列");
    assert_eq!(stored.storage, GeneratedStorage::Stored);
    assert_eq!(normalized(&stored.expression), "total*2");
    println!("table-live: {} 持久化生成列读回 ({})", live.engine_name(), stored.reported_storage);

    // 改表达式：读回反映修改。
    let mut drafts = drafts_of(&snapshot);
    drafts
        .iter_mut()
        .find(|draft| draft.original.as_deref() == Some("total_x2"))
        .and_then(|draft| draft.generated.as_mut())
        .expect("total_x2 草稿")
        .expression = "total * 3".to_owned();
    let (_, receipt) = live.run(Action::AlterColumns { table: table.clone(), columns: drafts }).await;
    assert!(receipt.steps.iter().all(|step| step.status == StepStatus::Ok), "{:?}", receipt.steps);
    let snapshot = live.describe(&table).await;
    assert_eq!(normalized(&snapshot.generation["total_x2"].expression), "total*3");

    // 删除生成列：DROP COLUMN 对生成列与普通列是同一条语句，读回不再包含它。
    // 有依赖时先删依赖列（total_x2 引用 total），被引用的情况留给服务器的原始错误。
    let mut drafts = drafts_of(&snapshot);
    drafts.iter_mut().find(|draft| draft.original.as_deref() == Some("total_x2")).expect("total_x2 草稿").dropped = true;
    let (plan, receipt) = live.run(Action::AlterColumns { table: table.clone(), columns: drafts }).await;
    println!("table-live: {} 删除持久化生成列 -> {}", live.engine_name(), plan.steps[0].sql);
    assert!(receipt.steps.iter().all(|step| step.status == StepStatus::Ok), "{:?}", receipt.steps);
    let snapshot = live.describe(&table).await;
    assert!(!snapshot.generation.contains_key("total_x2"), "{}: 删除后不该再有 total_x2", live.engine_name());

    let mut drafts = drafts_of(&snapshot);
    drafts.iter_mut().find(|draft| draft.original.as_deref() == Some("total")).expect("total 草稿").dropped = true;
    let (plan, receipt) = live.run(Action::AlterColumns { table: table.clone(), columns: drafts }).await;
    assert!(plan.blocked.is_empty(), "{}: 删除生成列不该被客户端阻断：{:?}", live.engine_name(), plan.blocked);
    println!("table-live: {} 删除 VIRTUAL 生成列 -> {}", live.engine_name(), plan.steps[0].sql);
    assert!(receipt.steps.iter().all(|step| step.status == StepStatus::Ok), "{:?}", receipt.steps);
    let snapshot = live.describe(&table).await;
    assert!(!snapshot.columns.iter().any(|column| column.name == "total"), "{}: 删除后不该再有 total 列", live.engine_name());
    assert!(!snapshot.generation.contains_key("total"), "{}: 删除后不该再有 total 的生成形态", live.engine_name());
    println!(
        "table-live: {} 生成列删除后读回不含 total，剩余列 {:?}",
        live.engine_name(),
        snapshot.columns.iter().map(|column| column.name.as_str()).collect::<Vec<_>>()
    );
    live.drop_table(&table).await;
}

/// 四类分区的建立、修改、维护与移除，每一步读回实际方案。
async fn partition_round_trip(live: &Live, suffix: &str) {
    let table = format!("t2_part_{suffix}");
    let mut id = id_column();
    id.auto = true;
    live.create_table(&table, vec![id]).await;

    let range = PartitionDraft {
        op: PartitionOp::Replace,
        method: PartitionMethod::Range,
        expression: "id".to_owned(),
        definitions: vec![
            PartitionDef { name: "p0".to_owned(), description: "100".to_owned() },
            PartitionDef { name: "p1".to_owned(), description: "200".to_owned() },
        ],
        ..PartitionDraft::default()
    };
    let (plan, receipt) = live.run(Action::AlterPartitions { table: table.clone(), partition: range }).await;
    println!("table-live: {} RANGE 建立 -> {}", live.engine_name(), plan.steps[0].sql);
    assert!(receipt.steps.iter().all(|step| step.status == StepStatus::Ok), "{:?}", receipt.steps);
    let snapshot = live.describe(&table).await;
    let scheme = snapshot.partitions.as_ref().expect("读回 RANGE 分区");
    assert_eq!(scheme.method, PartitionMethod::Range);
    assert_eq!(scheme.definitions.iter().map(|def| def.name.as_str()).collect::<Vec<_>>(), vec!["p0", "p1"]);

    // 新增分区（RANGE 上没有 MAXVALUE，可以继续追加）。
    let add = PartitionDraft {
        op: PartitionOp::Add,
        method: PartitionMethod::Range,
        definitions: vec![PartitionDef { name: "p2".to_owned(), description: "MAXVALUE".to_owned() }],
        ..PartitionDraft::default()
    };
    let (plan, receipt) = live.run(Action::AlterPartitions { table: table.clone(), partition: add }).await;
    println!("table-live: {} RANGE 新增 -> {}", live.engine_name(), plan.steps[0].sql);
    assert!(receipt.steps.iter().all(|step| step.status == StepStatus::Ok), "{:?}", receipt.steps);
    assert_eq!(live.describe(&table).await.partitions.expect("分区").definitions.len(), 3);

    // 删除分区。
    let drop = PartitionDraft {
        op: PartitionOp::Drop,
        method: PartitionMethod::Range,
        targets: vec!["p2".to_owned()],
        ..PartitionDraft::default()
    };
    let (plan, receipt) = live.run(Action::AlterPartitions { table: table.clone(), partition: drop }).await;
    println!("table-live: {} RANGE 删除 -> {}", live.engine_name(), plan.steps[0].sql);
    assert!(receipt.steps.iter().all(|step| step.status == StepStatus::Ok), "{:?}", receipt.steps);
    assert_eq!(live.describe(&table).await.partitions.expect("分区").definitions.len(), 2);

    // 重组分区：把 p1 拆成两段。
    let reorganize = PartitionDraft {
        op: PartitionOp::Reorganize,
        method: PartitionMethod::Range,
        targets: vec!["p1".to_owned()],
        definitions: vec![
            PartitionDef { name: "p1a".to_owned(), description: "150".to_owned() },
            PartitionDef { name: "p1b".to_owned(), description: "MAXVALUE".to_owned() },
        ],
        ..PartitionDraft::default()
    };
    let (plan, receipt) = live.run(Action::AlterPartitions { table: table.clone(), partition: reorganize }).await;
    println!("table-live: {} RANGE 重组 -> {}", live.engine_name(), plan.steps[0].sql);
    assert!(receipt.steps.iter().all(|step| step.status == StepStatus::Ok), "{:?}", receipt.steps);
    let names: Vec<String> = live
        .describe(&table)
        .await
        .partitions
        .expect("分区")
        .definitions
        .iter()
        .map(|def| def.name.clone())
        .collect();
    assert_eq!(names, vec!["p0".to_owned(), "p1a".to_owned(), "p1b".to_owned()]);

    // LIST。
    let list = PartitionDraft {
        op: PartitionOp::Replace,
        method: PartitionMethod::List,
        expression: "id".to_owned(),
        definitions: vec![
            PartitionDef { name: "p_low".to_owned(), description: "1,2,3".to_owned() },
            PartitionDef { name: "p_rest".to_owned(), description: "4,5,6".to_owned() },
        ],
        ..PartitionDraft::default()
    };
    let (plan, receipt) = live.run(Action::AlterPartitions { table: table.clone(), partition: list }).await;
    println!("table-live: {} LIST 建立 -> {}", live.engine_name(), plan.steps[0].sql);
    assert!(receipt.steps.iter().all(|step| step.status == StepStatus::Ok), "{:?}", receipt.steps);
    let scheme = live.describe(&table).await.partitions.expect("分区");
    assert_eq!(scheme.method, PartitionMethod::List);
    assert_eq!(scheme.definitions.len(), 2);

    // HASH：按数量建立，再按数量增、按数量减。
    let hash = PartitionDraft {
        op: PartitionOp::Replace,
        method: PartitionMethod::Hash,
        expression: "id".to_owned(),
        count: "4".to_owned(),
        ..PartitionDraft::default()
    };
    let (plan, receipt) = live.run(Action::AlterPartitions { table: table.clone(), partition: hash }).await;
    println!("table-live: {} HASH 建立 -> {}", live.engine_name(), plan.steps[0].sql);
    assert!(receipt.steps.iter().all(|step| step.status == StepStatus::Ok), "{:?}", receipt.steps);
    let scheme = live.describe(&table).await.partitions.expect("分区");
    assert_eq!(scheme.method, PartitionMethod::Hash);
    assert_eq!(scheme.definitions.len(), 4);

    let hash_add = PartitionDraft {
        op: PartitionOp::Add,
        method: PartitionMethod::Hash,
        count: "2".to_owned(),
        ..PartitionDraft::default()
    };
    let (plan, receipt) = live.run(Action::AlterPartitions { table: table.clone(), partition: hash_add }).await;
    println!("table-live: {} HASH 增加数量 -> {}", live.engine_name(), plan.steps[0].sql);
    assert!(receipt.steps.iter().all(|step| step.status == StepStatus::Ok), "{:?}", receipt.steps);
    assert_eq!(live.describe(&table).await.partitions.expect("分区").definitions.len(), 6);

    let hash_drop = PartitionDraft {
        op: PartitionOp::Drop,
        method: PartitionMethod::Hash,
        count: "3".to_owned(),
        ..PartitionDraft::default()
    };
    let (plan, receipt) = live.run(Action::AlterPartitions { table: table.clone(), partition: hash_drop }).await;
    println!("table-live: {} HASH 减少数量 -> {}", live.engine_name(), plan.steps[0].sql);
    assert!(receipt.steps.iter().all(|step| step.status == StepStatus::Ok), "{:?}", receipt.steps);
    assert_eq!(live.describe(&table).await.partitions.expect("分区").definitions.len(), 3);

    // KEY：按列清单建立。
    let key = PartitionDraft {
        op: PartitionOp::Replace,
        method: PartitionMethod::Key,
        columns: vec!["id".to_owned()],
        count: "3".to_owned(),
        ..PartitionDraft::default()
    };
    let (plan, receipt) = live.run(Action::AlterPartitions { table: table.clone(), partition: key }).await;
    println!("table-live: {} KEY 建立 -> {}", live.engine_name(), plan.steps[0].sql);
    assert!(receipt.steps.iter().all(|step| step.status == StepStatus::Ok), "{:?}", receipt.steps);
    let scheme = live.describe(&table).await.partitions.expect("分区");
    assert_eq!(scheme.method, PartitionMethod::Key);
    assert_eq!(scheme.definitions.len(), 3);

    // 移除分区方案：表可读回且不再显示分区。
    let remove = PartitionDraft { op: PartitionOp::Remove, method: PartitionMethod::Key, ..PartitionDraft::default() };
    let (plan, receipt) = live.run(Action::AlterPartitions { table: table.clone(), partition: remove }).await;
    println!("table-live: {} 移除分区 -> {}", live.engine_name(), plan.steps[0].sql);
    assert!(receipt.steps.iter().all(|step| step.status == StepStatus::Ok), "{:?}", receipt.steps);
    let snapshot = live.describe(&table).await;
    assert!(snapshot.partitions.is_none(), "{}: 移除后不该再有分区方案", live.engine_name());
    live.drop_table(&table).await;
}

/// R8：服务器拒绝的分区变更如实失败，结构保持原样；不适用的维护操作在预览前就阻断。
async fn partition_failure_keeps_the_definition(live: &Live, suffix: &str) {
    let table = format!("t2_part_fail_{suffix}");
    live.create_table(&table, vec![id_column()]).await;
    let range = PartitionDraft {
        op: PartitionOp::Replace,
        method: PartitionMethod::Range,
        expression: "id".to_owned(),
        definitions: vec![
            PartitionDef { name: "p0".to_owned(), description: "100".to_owned() },
            PartitionDef { name: "p_max".to_owned(), description: "MAXVALUE".to_owned() },
        ],
        ..PartitionDraft::default()
    };
    let (_, receipt) = live.run(Action::AlterPartitions { table: table.clone(), partition: range }).await;
    assert!(receipt.steps.iter().all(|step| step.status == StepStatus::Ok), "{:?}", receipt.steps);

    // 在 MAXVALUE 之后再加分区：服务器会拒绝，界面必须照实报错。
    let add = PartitionDraft {
        op: PartitionOp::Add,
        method: PartitionMethod::Range,
        definitions: vec![PartitionDef { name: "p_after".to_owned(), description: "MAXVALUE".to_owned() }],
        ..PartitionDraft::default()
    };
    let request = live.request(Action::AlterPartitions { table: table.clone(), partition: add });
    let plan = ddl::preview_request(&live.services.connections, &live.id, &request).await.expect("预览");
    assert!(plan.blocked.is_empty(), "这不是客户端能直接判断的错误：{:?}", plan.blocked);
    let sql: Vec<String> = plan.steps.iter().map(|step| step.sql.clone()).collect();
    let receipt = ddl::execute_request(&live.services.connections, &live.id, &request, &sql, true).await.expect("执行");
    let failed = receipt.steps.iter().find(|step| step.status == StepStatus::Failed).expect("服务器应该拒绝这条语句");
    assert!(!failed.message.trim().is_empty(), "失败必须保留服务器的原始错误");
    println!("table-live: {} 服务器拒绝新增分区：{}", live.engine_name(), failed.message);
    assert_eq!(live.describe(&table).await.partitions.expect("分区").definitions.len(), 2, "失败不该改动原方案");

    // 未分区的表上删除分区：客户端阻断，不发给服务器。
    live.drop_table(&table).await;
    let plain = format!("t2_part_plain_{suffix}");
    live.create_table(&plain, vec![id_column()]).await;
    let drop = PartitionDraft {
        op: PartitionOp::Drop,
        method: PartitionMethod::Range,
        targets: vec!["p0".to_owned()],
        ..PartitionDraft::default()
    };
    let plan = live.preview(Action::AlterPartitions { table: plain.clone(), partition: drop }).await;
    assert!(plan.blocked.iter().any(|text| text.contains("还没有分区方案")), "{:?}", plan.blocked);
    println!("table-live: {} 未分区的表被阻断：{:?}", live.engine_name(), plan.blocked);
    live.drop_table(&plain).await;
}
