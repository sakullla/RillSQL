//! 跨库数据迁移执行器（R8）。
//! 源端按页流式读（复用 grid 的 select_list 投影，二进制按十六进制文本）；写入按目标列的类型
//! 决定字面量（与导入同策略），整批放进一个事务，整批失败才降级逐行重放来分类冲突与单行失败。
//! 不建表：目标表必须已经存在（结构编辑器/全库导出负责建表）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use crate::connections::CatalogSession;
use crate::dump::dump::data_order_clause;
use crate::grid::{
    describe, fetch_cells, qualified_name, quote_ident, select_list, sql_cell, value_family, Cell, Column, Engine,
    TableSchema, ValueFamily,
};

/// 每页读取（也是每批提交）的行数，与 dump 的数据页同粒度。
const PAGE_ROWS: u64 = 500;

/// 批量的上限：避免一条多行 INSERT 超过 MySQL max_allowed_packet 一类的语句长度限制。
const MAX_BATCH_ROWS: u64 = 2000;

/// 报告里最多保留的失败行标识数（计数不受限，只截样本）。
const FAILED_SAMPLE: usize = 200;

/// 报告里最多保留的首批错误明细。
const FIRST_ERRORS: usize = 20;

/// 逐行失败连续累计到这个次数就探活一次，把连接级故障和单行数据问题分开。
const ALIVE_CHECK_EVERY: u32 = 50;

// ---------------------------------------------------------------- 取消注册表（grid/live.rs、dump 同模式）

pub fn request_cancel(id: &str) {
    let mut jobs = jobs().lock().expect("migrate jobs");
    if let Some(flag) = jobs.get(id) {
        flag.store(true, Ordering::Relaxed);
    } else {
        jobs.insert(id.to_owned(), Arc::new(AtomicBool::new(true)));
    }
}

fn jobs() -> &'static Mutex<std::collections::HashMap<String, Arc<AtomicBool>>> {
    static JOBS: OnceLock<Mutex<std::collections::HashMap<String, Arc<AtomicBool>>>> = OnceLock::new();
    JOBS.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

fn begin_job(id: &str) -> Arc<AtomicBool> {
    let mut jobs = jobs().lock().expect("migrate jobs");
    if let Some(flag) = jobs.get(id) {
        return Arc::clone(flag);
    }
    let flag = Arc::new(AtomicBool::new(false));
    jobs.insert(id.to_owned(), Arc::clone(&flag));
    flag
}

fn finish_job(id: &str) {
    jobs().lock().expect("migrate jobs").remove(id);
}

// ---------------------------------------------------------------- 请求与结果

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrateTableRef {
    pub connection_id: String,
    /// 引擎名（与目录树同源），MariaDB 与 MySQL 共用会话形态。
    pub engine: String,
    pub database: String,
    #[serde(default)]
    pub schema: String,
    pub table: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnPair {
    pub source: String,
    pub target: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrateOptions {
    /// 每批提交的行数；0 按默认页粒度，越界值取上限。
    #[serde(default)]
    pub batch_rows: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrateRequest {
    /// 任务中心里的任务 id，进度事件与取消都按它对号。
    pub id: String,
    pub source: MigrateTableRef,
    pub target: MigrateTableRef,
    /// 源列名 → 目标列名。空列表按同名自动映射。
    #[serde(default)]
    pub mapping: Vec<ColumnPair>,
    /// UI 固定传 `options: null`：serde(default) 只覆盖字段缺失、不接受显式 null，
    /// 所以这里用 Option 承接，取值时统一落到默认值。
    #[serde(default)]
    pub options: Option<MigrateOptions>,
}

impl MigrateRequest {
    /// 生效选项：None（显式 null 或整个字段缺失）都按默认页粒度执行。
    pub(crate) fn effective_options(&self) -> MigrateOptions {
        self.options.clone().unwrap_or_default()
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrateDto {
    pub status: String,
    pub success: bool,
    pub message: String,
    pub inserted: u64,
    pub skipped: u64,
    pub failed: u64,
    /// 失败行的主键（无主键时用行号）样本。
    pub failed_ids: Vec<String>,
    /// 首批失败的具体错误（标识 + 原因）。
    pub first_errors: Vec<String>,
    /// 类型兼容与目标列缺口的警告；警告不阻断。
    pub warnings: Vec<String>,
}

/// `migrate_progress` 事件的载荷：rows 是已处理的行数（插入+跳过+失败）。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrateProgress {
    pub id: String,
    pub rows: u64,
    pub inserted: u64,
    pub skipped: u64,
    pub failed: u64,
}

// ---------------------------------------------------------------- 类型兼容（纯函数）

/// 源列 → 目标列的兼容提示：None 完全兼容；Some(原因) 是警告，尽力转换不阻断。
/// 值的实际写法由目标列类型决定（sql_cell），这里的警告覆盖"会被拒绝/会丢信息"的组合。
pub(crate) fn pair_note(source_type: &str, target_type: &str) -> Option<String> {
    use ValueFamily as F;
    let source = value_family(source_type);
    let target = value_family(target_type);
    let note = match (source, target) {
        (F::Unknown, _) | (_, F::Unknown) => "有一侧类型未知，写入前先确认。".to_owned(),
        (F::Raw, _) | (_, F::Raw) => "有一侧类型不能判断写回方式，写入前先确认。".to_owned(),
        (F::Geometry, F::Geometry) => return None,
        (F::Geometry, _) | (_, F::Geometry) => "几何类型没有跨引擎的通用转换，这些行通常会计入失败。".to_owned(),
        _ if source == target => return length_note(target_type),
        // 文本是兜底方向：除几何外都能按文本尽力转换。
        (F::Binary, F::Text) => "二进制将按十六进制文本写入。".to_owned(),
        (F::Bool | F::Integer | F::Decimal | F::Float | F::Date | F::Time | F::Datetime, F::Text) => {
            return length_note(target_type)
        }
        (F::Text, F::Bool) => "只有 t/f/true/false/0/1/yes/no 形状的值能转成布尔。".to_owned(),
        (F::Text, F::Integer | F::Decimal | F::Float) => "文本值必须是数字才会被目标接受。".to_owned(),
        (F::Text, F::Date | F::Time | F::Datetime) => "文本值必须符合目标引擎的日期时间格式。".to_owned(),
        (F::Text, F::Binary) => "文本值必须是偶数长度的十六进制才能写成二进制。".to_owned(),
        (F::Integer, F::Decimal | F::Float) => return length_note(target_type),
        (F::Integer, F::Bool) => "只有 0/1 能转成布尔，其余值会计入失败。".to_owned(),
        (F::Decimal | F::Float, F::Integer) => "小数进整数列会被拒绝或截断，不会静默截断。".to_owned(),
        (F::Decimal, F::Float) | (F::Float, F::Decimal) => "定点与浮点互换有精度损失。".to_owned(),
        (F::Decimal | F::Float, F::Bool) => "小数转布尔不可靠，这些行通常会计入失败。".to_owned(),
        (F::Bool, F::Integer | F::Decimal | F::Float) => "布尔写数值列在部分引擎（如 PostgreSQL）会被拒绝。".to_owned(),
        (F::Date, F::Datetime) => return length_note(target_type),
        (F::Datetime, F::Date) => "日期时间进日期列会丢失时间部分。".to_owned(),
        (F::Date, F::Time) | (F::Time, F::Date) => "日期与时刻语义不同，值可能被拒绝。".to_owned(),
        (F::Time, F::Datetime) | (F::Datetime, F::Time) => "时刻与日期时间语义不同，值可能被拒绝。".to_owned(),
        (F::Date | F::Time | F::Datetime, F::Integer | F::Decimal | F::Float | F::Bool) => {
            "日期时间没有数值转换，这些行通常会计入失败。".to_owned()
        }
        (F::Integer | F::Decimal | F::Float | F::Bool, F::Date | F::Time | F::Datetime) => {
            "数值没有日期时间格式，这些行通常会计入失败。".to_owned()
        }
        (F::Binary, F::Bool | F::Integer | F::Decimal | F::Float | F::Date | F::Time | F::Datetime) => {
            "二进制写不进这些类型，这些行通常会计入失败。".to_owned()
        }
        (F::Bool | F::Integer | F::Decimal | F::Float | F::Date | F::Time | F::Datetime, F::Binary) => {
            "这些类型写不进二进制列，这些行通常会计入失败。".to_owned()
        }
        // 编译器不把带守卫的分支计入穷尽：跨家族的组合上面都已显式列出，这里只剩同家族对。
        _ => return length_note(target_type),
    };
    Some(note)
}

/// 目标列带长度限制（如 varchar(50)）时提示：超长会计入失败，不会静默截断。
fn length_note(target_type: &str) -> Option<String> {
    if target_type.contains('(') {
        Some("目标列限长，超长行会计入失败而不会被截断。".to_owned())
    } else {
        None
    }
}

/// 主键/唯一键冲突的识别：三族引擎对同一错误的措辞不同。
pub(crate) fn is_conflict_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("duplicate entry")
        || lower.contains("duplicate key")
        || lower.contains("unique constraint")
        || lower.contains("violation of primary key constraint")
        || lower.contains("violation of unique key constraint")
}

fn engine_matches(session: &CatalogSession, engine: Engine) -> bool {
    matches!(
        (engine, session),
        (Engine::Sqlite, CatalogSession::Sqlite { .. })
            | (Engine::Mysql | Engine::Mariadb, CatalogSession::Mysql(_))
            | (Engine::Postgres, CatalogSession::Postgres(_))
            | (Engine::Sqlserver, CatalogSession::Sqlserver(_))
    )
}

/// 先精确匹配再忽略大小写，与前端 suggestMapping 的列名匹配同规则。
fn find_column<'a>(columns: &'a [Column], name: &str) -> Option<&'a Column> {
    columns
        .iter()
        .find(|column| column.name == name)
        .or_else(|| columns.iter().find(|column| column.name.eq_ignore_ascii_case(name)))
}

// ---------------------------------------------------------------- 语句构造（纯函数）

/// 一行 Cells 按目标列类型落成 VALUES 元组：字面量由目标列的 typeName 决定（与导入同策略）。
fn row_tuple(engine: Engine, columns: &[Column], cells: &[Cell]) -> String {
    let literals = columns
        .iter()
        .enumerate()
        .map(|(index, column)| sql_cell(engine, Some(column), cells.get(index).unwrap_or(&Cell::Null)))
        .collect::<Vec<_>>();
    format!("({})", literals.join(", "))
}

/// 多行合成一条 INSERT：整批一个原子单位，失败由调用方降级逐行重放。
fn batch_insert(engine: Engine, qualified: &str, columns: &[Column], rows: &[Vec<Cell>]) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let names = columns
        .iter()
        .map(|column| quote_ident(engine, &column.name))
        .collect::<Vec<_>>()
        .join(", ");
    let tuples = rows.iter().map(|cells| row_tuple(engine, columns, cells)).collect::<Vec<_>>().join(", ");
    format!("INSERT INTO {qualified} ({names}) VALUES {tuples}")
}

/// 失败行的标识：优先源表主键的值，没有可用的键就用行号（读取按键列稳定排序，行号可复现）。
fn row_identity(key_positions: &[usize], ordinal: u64, cells: &[Cell]) -> String {
    if key_positions.is_empty() {
        return format!("行#{ordinal}");
    }
    key_positions
        .iter()
        .map(|&index| match cells.get(index) {
            Some(Cell::Text(text)) => text.clone(),
            _ => "NULL".to_owned(),
        })
        .collect::<Vec<_>>()
        .join("/")
}

// ---------------------------------------------------------------- 目标端执行

/// 一批放进一个事务：整批成功才提交。失败即回滚并返回错误，由调用方降级逐行重放。
async fn insert_page(target: &CatalogSession, statement: &str) -> Result<(), String> {
    match target {
        CatalogSession::Scripted => Err("这个连接没有连着可查询的数据库".to_owned()),
        // Redis 不在迁移目标面（capability 门控）；误入时拒绝写入。
        CatalogSession::Redis(_) => Err("Redis 不是迁移目标；数据经键命令写入。".to_owned()),
        // SQLite 会话的池固定单连接，BEGIN/COMMIT 语句与数据语句落在同一条连接上。
        CatalogSession::Sqlite { pool, .. } => {
            exec_sqlite(pool, "BEGIN").await?;
            match exec_sqlite(pool, statement).await {
                Ok(()) => exec_sqlite(pool, "COMMIT").await,
                Err(error) => {
                    let _ = exec_sqlite(pool, "ROLLBACK").await;
                    Err(error)
                }
            }
        }
        CatalogSession::Mysql(pool) => {
            let mut conn = pool.get_conn().await.map_err(|err| err.to_string())?;
            mysql_async::prelude::Queryable::query_drop(&mut conn, "BEGIN".to_owned())
                .await
                .map_err(|err| err.to_string())?;
            match mysql_async::prelude::Queryable::query_drop(&mut conn, statement.to_owned()).await {
                Ok(()) => mysql_async::prelude::Queryable::query_drop(&mut conn, "COMMIT".to_owned())
                    .await
                    .map_err(|err| err.to_string()),
                Err(error) => {
                    let _ = mysql_async::prelude::Queryable::query_drop(&mut conn, "ROLLBACK".to_owned()).await;
                    Err(error.to_string())
                }
            }
        }
        // Arc<Client> 不支持 transaction()，同一条连接上的显式 BEGIN/COMMIT 等价。
        CatalogSession::Postgres(client) => {
            client.execute("BEGIN", &[]).await.map_err(|err| err.to_string())?;
            match client.execute(statement, &[]).await {
                Ok(_) => client.execute("COMMIT", &[]).await.map(|_| ()).map_err(|err| err.to_string()),
                Err(error) => {
                    let _ = client.execute("ROLLBACK", &[]).await;
                    Err(error.to_string())
                }
            }
        }
        // 同一条 TDS 连接上的显式事务；失败回滚后由调用方降级逐行重放。
        CatalogSession::Sqlserver(client) => {
            let mut locked = client.lock().await;
            locked.execute("BEGIN TRANSACTION", &[]).await.map_err(|err| err.to_string())?;
            match locked.execute(statement.to_owned(), &[]).await {
                Ok(_) => locked.execute("COMMIT", &[]).await.map(|_| ()).map_err(|err| err.to_string()),
                Err(error) => {
                    let _ = locked.execute("ROLLBACK", &[]).await;
                    Err(error.to_string())
                }
            }
        }
    }
}

/// 逐行重放的单条自动提交语句（降级路径专用）。
async fn exec_one(target: &CatalogSession, statement: &str) -> Result<(), String> {
    match target {
        CatalogSession::Scripted => Err("这个连接没有连着可查询的数据库".to_owned()),
        CatalogSession::Redis(_) => Err("Redis 不是迁移目标；数据经键命令写入。".to_owned()),
        CatalogSession::Sqlite { pool, .. } => exec_sqlite(pool, statement).await,
        CatalogSession::Mysql(pool) => {
            let mut conn = pool.get_conn().await.map_err(|err| err.to_string())?;
            mysql_async::prelude::Queryable::query_drop(&mut conn, statement.to_owned())
                .await
                .map_err(|err| err.to_string())
        }
        CatalogSession::Postgres(client) => client.execute(statement, &[]).await.map(|_| ()).map_err(|err| err.to_string()),
        CatalogSession::Sqlserver(client) => {
            let mut locked = client.lock().await;
            locked.execute(statement.to_owned(), &[]).await.map(|_| ()).map_err(|err| err.to_string())
        }
    }
}

async fn exec_sqlite(pool: &sqlx::SqlitePool, statement: &str) -> Result<(), String> {
    sqlx::query(sqlx::AssertSqlSafe(statement.to_owned()))
        .execute(pool)
        .await
        .map(|_| ())
        .map_err(|err| err.to_string())
}

/// 逐行连续失败后的探活：区分"这行数据有问题"和"连接已经断开"。
async fn alive(session: &CatalogSession) -> Result<(), String> {
    fetch_cells(session, "SELECT 1", 1).await.map(|_| ())
}

// ---------------------------------------------------------------- 迁移计划

struct MigrationPair {
    source: Column,
    target: Column,
}

struct Plan {
    source_qualified: String,
    source_projection: String,
    order_clause: String,
    /// 投影里的源列（与 target_columns 一一对应）。
    source_columns: Vec<Column>,
    target_columns: Vec<Column>,
    target_engine: Engine,
    target_qualified: String,
    /// 主键列在投影中的位置（按源表键序）；源表无键时失败行用行号标识。
    key_positions: Vec<usize>,
    warnings: Vec<String>,
    batch_rows: u64,
}

async fn build_plan(
    source: &CatalogSession,
    source_ref: &MigrateTableRef,
    target: &CatalogSession,
    target_ref: &MigrateTableRef,
    mapping: &[ColumnPair],
    options: &MigrateOptions,
) -> Result<Plan, String> {
    let source_engine = Engine::parse(&source_ref.engine)
        .ok()
        .filter(|engine| engine_matches(source, *engine))
        .ok_or_else(|| "源连接的会话类型和引擎不一致".to_owned())?;
    let target_engine = Engine::parse(&target_ref.engine)
        .ok()
        .filter(|engine| engine_matches(target, *engine))
        .ok_or_else(|| "目标连接的会话类型和引擎不一致".to_owned())?;
    let source_schema =
        describe(source, &source_ref.database, &source_ref.schema, &source_ref.table, &source_ref.engine)
            .await
            .map_err(|message| format!("读取源表失败：{message}"))?;
    let target_schema =
        describe(target, &target_ref.database, &target_ref.schema, &target_ref.table, &target_ref.engine)
            .await
            .map_err(|message| format!("读取目标表失败：{message}"))?;
    if target_schema.kind != "table" {
        return Err("目标必须是已经存在的表；视图不能作为迁移目标。".to_owned());
    }

    // 显式映射逐条校验；没给映射时按同名自动映射（精确优先，忽略大小写次之）。
    let mut pairs: Vec<MigrationPair> = Vec::new();
    if mapping.is_empty() {
        for source_column in &source_schema.columns {
            if let Some(target_column) = find_column(&target_schema.columns, &source_column.name) {
                pairs.push(MigrationPair { source: source_column.clone(), target: target_column.clone() });
            }
        }
        if pairs.is_empty() {
            return Err("没有可迁移的列：两边的列名都对不上，请在列映射里手动指定。".to_owned());
        }
    } else {
        for pair in mapping {
            let source_column = find_column(&source_schema.columns, &pair.source)
                .ok_or_else(|| format!("映射里的源列不存在：{}", pair.source))?;
            let target_column = find_column(&target_schema.columns, &pair.target)
                .ok_or_else(|| format!("映射里的目标列不存在：{} → {}", pair.source, pair.target))?;
            if pairs
                .iter()
                .any(|item| item.target.name.eq_ignore_ascii_case(&target_column.name))
            {
                return Err(format!("有两个源列都映射到目标列 {}。", target_column.name));
            }
            pairs.push(MigrationPair { source: source_column.clone(), target: target_column.clone() });
        }
    }

    let mut warnings = Vec::new();
    for pair in &pairs {
        if let Some(note) = pair_note(&pair.source.type_name, &pair.target.type_name) {
            warnings.push(format!("列 {} → {}：{note}", pair.source.name, pair.target.name));
        }
    }
    let mapped_targets: Vec<&str> = pairs.iter().map(|pair| pair.target.name.as_str()).collect();
    for column in &target_schema.columns {
        if column.nullable || column.auto || mapped_targets.iter().any(|name| name.eq_ignore_ascii_case(&column.name)) {
            continue;
        }
        let pk_note = if column.primary { "（它是主键）" } else { "" };
        warnings.push(format!("目标列 {} 没有映射且不可为空；若它没有默认值，这些行会计入失败。{pk_note}", column.name));
    }

    let source_columns: Vec<Column> = pairs.iter().map(|pair| pair.source.clone()).collect();
    let target_columns: Vec<Column> = pairs.iter().map(|pair| pair.target.clone()).collect();
    // 投影只取被映射的源列；full=true 保证二进制全部按十六进制读出，不出现预览信封。
    let source_projection = select_list(
        &TableSchema {
            engine: source_engine,
            schema: source_schema.schema.clone(),
            name: source_schema.name.clone(),
            kind: "table",
            columns: source_columns.clone(),
            key: Vec::new(),
        },
        true,
    );
    let target_qualified = qualified_name(&TableSchema {
        engine: target_engine,
        schema: target_schema.schema.clone(),
        name: target_schema.name.clone(),
        kind: "table",
        columns: target_columns.clone(),
        key: Vec::new(),
    });
    let key_positions = source_schema
        .key
        .iter()
        .filter_map(|key| pairs.iter().position(|pair| pair.source.name.eq_ignore_ascii_case(key)))
        .collect::<Vec<_>>();
    let batch_rows = match options.batch_rows {
        0 => PAGE_ROWS,
        1..=MAX_BATCH_ROWS => options.batch_rows,
        _ => MAX_BATCH_ROWS,
    };
    Ok(Plan {
        source_qualified: qualified_name(&source_schema),
        order_clause: data_order_clause(&source_schema),
        source_projection,
        source_columns,
        target_columns,
        target_engine,
        target_qualified,
        key_positions,
        warnings,
        batch_rows,
    })
}

// ---------------------------------------------------------------- 主流程

#[derive(Default)]
struct Tally {
    inserted: u64,
    skipped: u64,
    failed: u64,
    failed_ids: Vec<String>,
    first_errors: Vec<String>,
    consecutive_failures: u32,
}

impl Tally {
    fn rows(&self) -> u64 {
        self.inserted + self.skipped + self.failed
    }
}

fn blocked(message: String) -> MigrateDto {
    MigrateDto {
        status: "blocked".to_owned(),
        success: false,
        message,
        inserted: 0,
        skipped: 0,
        failed: 0,
        failed_ids: Vec::new(),
        first_errors: Vec::new(),
        warnings: Vec::new(),
    }
}

fn dto_of(status: &str, success: bool, lead: String, tally: &Tally, warnings: Vec<String>) -> MigrateDto {
    let sample = tally.failed_ids.iter().take(5).cloned().collect::<Vec<_>>().join("、");
    let failed_note = if tally.failed > 0 { format!(" 失败样本：{sample}。") } else { String::new() };
    MigrateDto {
        status: status.to_owned(),
        success,
        message: format!(
            "{lead}已插入 {} 行，跳过 {} 行（主键或唯一键冲突），失败 {} 行。{failed_note}",
            tally.inserted, tally.skipped, tally.failed
        ),
        inserted: tally.inserted,
        skipped: tally.skipped,
        failed: tally.failed,
        failed_ids: tally.failed_ids.clone(),
        first_errors: tally.first_errors.clone(),
        warnings,
    }
}

/// 迁移正文。progress 回调在命令层转发成 `migrate_progress` 事件；测试里换成收集器。
pub(crate) async fn run_migrate(
    source: &CatalogSession,
    target: &CatalogSession,
    request: &MigrateRequest,
    progress: &(dyn Fn(MigrateProgress) + Send + Sync),
) -> MigrateDto {
    let flag = begin_job(&request.id);
    let outcome = migrate_body(source, target, request, &flag, progress).await;
    finish_job(&request.id);
    outcome
}

async fn migrate_body(
    source: &CatalogSession,
    target: &CatalogSession,
    request: &MigrateRequest,
    flag: &AtomicBool,
    progress: &(dyn Fn(MigrateProgress) + Send + Sync),
) -> MigrateDto {
    if matches!(source, CatalogSession::Scripted) || matches!(target, CatalogSession::Scripted) {
        return blocked("这个连接没有连着可查询的数据库".to_owned());
    }
    let options = request.effective_options();
    let plan = match build_plan(source, &request.source, target, &request.target, &request.mapping, &options).await {
        Ok(plan) => plan,
        Err(message) => return blocked(message),
    };
    let warnings = plan.warnings.clone();

    let report = |tally: &Tally| {
        progress(MigrateProgress {
            id: request.id.clone(),
            rows: tally.rows(),
            inserted: tally.inserted,
            skipped: tally.skipped,
            failed: tally.failed,
        });
    };

    let cancelled = |tally: &Tally, warnings: Vec<String>| {
        dto_of(
            "cancelled",
            false,
            format!("已取消迁移：在处理 {} 行后停止，已提交的批次保留。", tally.rows()),
            tally,
            warnings,
        )
    };

    let mut tally = Tally::default();
    let mut offset = 0u64;
    loop {
        if flag.load(Ordering::Relaxed) {
            return cancelled(&tally, warnings);
        }
        let sql = format!(
            "SELECT {} FROM {}{} LIMIT {} OFFSET {}",
            plan.source_projection, plan.source_qualified, plan.order_clause, plan.batch_rows, offset
        );
        let rows = match fetch_cells(source, &sql, plan.source_columns.len()).await {
            Ok(rows) => rows,
            Err(message) => {
                let lead = format!("迁移中止：读取源表失败：{message}。");
                return dto_of("failed", false, lead, &tally, warnings);
            }
        };
        if rows.is_empty() {
            break;
        }
        let page_rows = rows.len() as u64;
        let batch = batch_insert(plan.target_engine, &plan.target_qualified, &plan.target_columns, &rows);
        if insert_page(target, &batch).await.is_ok() {
            tally.inserted += page_rows;
        } else {
            // 整批失败（冲突、类型拒绝、约束拒绝都有可能）：降级逐行重放，按行分类并继续。
            let mut since_progress = 0u64;
            for (index, cells) in rows.iter().enumerate() {
                if flag.load(Ordering::Relaxed) {
                    return cancelled(&tally, warnings);
                }
                let ordinal = offset + index as u64 + 1;
                let single =
                    batch_insert(plan.target_engine, &plan.target_qualified, &plan.target_columns, std::slice::from_ref(cells));
                match exec_one(target, &single).await {
                    Ok(()) => {
                        tally.inserted += 1;
                        tally.consecutive_failures = 0;
                    }
                    Err(message) if is_conflict_error(&message) => tally.skipped += 1,
                    Err(message) => {
                        tally.failed += 1;
                        tally.consecutive_failures += 1;
                        let identity = row_identity(&plan.key_positions, ordinal, cells);
                        if tally.failed_ids.len() < FAILED_SAMPLE {
                            tally.failed_ids.push(identity.clone());
                        }
                        if tally.first_errors.len() < FIRST_ERRORS {
                            tally.first_errors.push(format!("{identity}：{message}"));
                        }
                        // 连续失败先探活再继续：连接断了就停，数据问题就记下去。
                        if tally.consecutive_failures % ALIVE_CHECK_EVERY == 0 && alive(target).await.is_err() {
                            let lead = "迁移中止：目标连接疑似已断开，已停止重放。".to_owned();
                            return dto_of("failed", false, lead, &tally, warnings);
                        }
                    }
                }
                since_progress += 1;
                if since_progress % 50 == 0 {
                    report(&tally);
                }
            }
        }
        report(&tally);
        if page_rows < plan.batch_rows {
            break;
        }
        offset += plan.batch_rows;
    }
    let lead = format!("迁移完成：{} → {}。", request.source.table, request.target.table);
    dto_of("done", true, lead, &tally, warnings)
}

// ---------------------------------------------------------------- 测试（SQLite 真库端到端，不依赖 docker）

#[cfg(test)]
mod tests;
