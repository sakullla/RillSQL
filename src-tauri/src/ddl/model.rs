use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub use crate::connections::model::Engine;

#[derive(Clone, Debug)]
pub struct Column {
    pub name: String,
    pub type_name: String,
    pub nullable: bool,
    pub default_sql: Option<String>,
    pub primary: bool,
    pub auto: bool,
    pub generated: bool,
    /// 默认值是具名约束的引擎（SQL Server）记下约束名；改/删默认值需要这个名字。其余引擎为 None。
    pub default_constraint: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct IndexColumn {
    pub name: String,
    pub descending: bool,
    /// 前缀长度（MySQL/MariaDB 的 SUB_PART）。整列进索引或引擎没有前缀索引时为 None。
    pub prefix: Option<u32>,
}

/// 索引类型。PostgreSQL、SQLite、SQL Server 只区分主键/唯一/普通；全文与空间只有 MySQL 家族读得到也建得了。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IndexKind {
    Primary,
    Unique,
    #[default]
    Normal,
    Fulltext,
    Spatial,
}

impl IndexKind {
    /// 只区分主键/唯一/普通的引擎按这两个标记推出类型。
    pub fn infer(primary: bool, unique: bool) -> Self {
        if primary {
            IndexKind::Primary
        } else if unique {
            IndexKind::Unique
        } else {
            IndexKind::Normal
        }
    }

    /// 旧的 `unique` 开关与新的类型合在一起：类型是普通而开关为真时按唯一。
    pub fn resolve(unique: bool, kind: IndexKind) -> Self {
        if kind == IndexKind::Normal && unique {
            IndexKind::Unique
        } else {
            kind
        }
    }

    pub fn is_unique(self) -> bool {
        matches!(self, IndexKind::Primary | IndexKind::Unique)
    }

    /// 全文与空间索引的键不能带升降序与前缀长度。
    pub fn plain_key_parts(self) -> bool {
        matches!(self, IndexKind::Fulltext | IndexKind::Spatial)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Index {
    pub name: String,
    pub unique: bool,
    pub columns: Vec<IndexColumn>,
    pub protected: bool,
    pub reason: String,
    pub partial: bool,
    pub expression: bool,
    pub sql: Option<String>,
    /// 索引类型。只区分主键/唯一/普通的引擎按 `IndexKind::infer` 填。
    pub kind: IndexKind,
    /// 存储方法（MySQL 的 INDEX_TYPE：BTREE/HASH/FULLTEXT/SPATIAL）。没读到为 None。
    pub method: Option<String>,
    /// 索引注释。没有注释或引擎没有索引注释时为 None。
    pub comment: Option<String>,
    /// 可见性（MySQL 8.0+ 的 IS_VISIBLE、MariaDB 10.6+ 的 IGNORED 取反）。服务器没有这项时为 None。
    pub visible: Option<bool>,
    /// 页面编辑器表达不了、先删后建会丢掉的属性（SQL 关键字形式，如 `USING gin`、`INCLUDE`、`CLUSTERED`）。
    /// 非空时不能用 drop+create 修改这个索引。MySQL 家族在同一条 ALTER TABLE 里改，不填。
    pub rebuild_loses: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct ForeignKey {
    pub name: String,
    pub columns: Vec<String>,
    pub ref_schema: String,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
    pub on_delete: String,
    pub on_update: String,
}

#[derive(Clone, Debug)]
pub struct IncomingForeignKey {
    pub name: String,
    pub table: String,
    pub schema: String,
    pub columns: Vec<String>,
    pub ref_columns: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct DependentSql {
    pub name: String,
    pub sql: String,
}

/// 序列参数。读不到的字段是 None：plan 不会用猜出来的当前值判断“没有变化”。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SequenceState {
    pub start: Option<i64>,
    pub increment: Option<i64>,
    pub min_value: Option<i64>,
    pub max_value: Option<i64>,
    pub cycle: Option<bool>,
}

/// 域的默认值与 NOT NULL。默认值为 None 表示这个域没有默认值。
#[derive(Clone, Debug, Default)]
pub struct DomainState {
    pub default_sql: Option<String>,
    pub not_null: bool,
}

/// SQLite 已附加的库。main 与 temp 也在列表里，它们的 path 为空。
#[derive(Clone, Debug, Default)]
pub struct AttachedDatabase {
    pub name: String,
    pub path: String,
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub engine: Engine,
    pub database: String,
    pub schema: String,
    pub name: String,
    pub kind: String,
    pub storage: Option<String>,
    pub sqlite_version: String,
    pub recorded_foreign_keys: Option<bool>,
    pub live_foreign_keys: Option<bool>,
    pub columns: Vec<Column>,
    pub indexes: Vec<Index>,
    pub foreign_keys: Vec<ForeignKey>,
    pub incoming: Vec<IncomingForeignKey>,
    pub triggers: Vec<DependentSql>,
    pub views: Vec<DependentSql>,
    pub table_sql: String,
    pub has_check: bool,
    pub has_collate: bool,
    pub without_rowid: bool,
    pub strict: bool,
    pub autoincrement: bool,
    pub row_count: Option<i64>,
    pub null_counts: BTreeMap<String, i64>,
    pub object_exists: bool,
    pub reference: Option<Box<Snapshot>>,
    /// PostgreSQL 物化视图。替换会丢掉已经物化的数据，因此不能发送。
    pub materialized: bool,
    /// 视图原文读失败时的服务器错误。空字符串式的占位注释不能当作原文。
    pub definition_error: Option<String>,
    /// 索引查询失败。列已经读到，不能把整表当成没读到。
    pub index_error: Option<String>,
    /// 外键查询失败。列已经读到，不能把整表当成没读到。
    pub foreign_key_error: Option<String>,
    /// PostgreSQL 序列的现有参数。只有序列动作会读。
    pub sequence: Option<SequenceState>,
    /// PostgreSQL 枚举的现有取值，按定义顺序。空表示没读到或不是枚举。
    pub enum_values: Vec<String>,
    /// PostgreSQL 域的现有默认值与 NOT NULL。
    pub domain: Option<DomainState>,
    /// MySQL 事件的启用状态。None 表示没读到或事件不存在。
    pub event_enabled: Option<bool>,
    /// SQLite 的 foreign_keys / journal_mode / user_version 现值。
    pub pragmas: BTreeMap<String, String>,
    /// SQLite 已附加的库（含 main 与 temp）。
    pub attached: Vec<AttachedDatabase>,
    /// 改名的目标名是否已经被同一命名空间里的对象占用。None 表示没查过：改名计划不会当它没被占用。
    pub name_taken: Option<bool>,
    /// 表注释。只有读定义时才去读；None 表示没有注释或没读。
    pub table_comment: Option<String>,
    /// 列注释，按列名。只有读定义时才去读。
    pub column_comments: BTreeMap<String, String>,
    /// 注释查询失败。定义仍可重建，但要说明注释没读到。
    pub comment_error: Option<String>,
    /// 生成这份计划时同连接的用户事务状态。适配器据此把事务内不能执行或要改写边界的步骤
    /// 在计划里说明清楚（例如 SQLite 重建改用保存点）。默认 `Idle`，表示没有活动事务。
    pub transaction: crate::transaction::TransactionState,
    /// 服务器版本原文（连接读到的 VERSION()）。空表示没读到；方言能力要按它判断，不能默认当最新版。
    pub server_version: String,
    /// 会话 sql_mode 原文。字面量转义方式据此选择；空表示没读到。
    pub sql_mode: String,
    /// 版本或 sql_mode 读取失败的原因。表结构仍然可用，但方言判断不能当成已知。
    pub profile_error: Option<String>,
    /// ENUM/SET 字段的候选值，按列名。只收集读回里确实是 enum/set 的列。
    pub enum_sets: BTreeMap<String, EnumSetState>,
    /// 生成列的表达式与存储方式，按列名。普通表达式默认值不在里面。
    pub generation: BTreeMap<String, GenerationState>,
    /// 分区方案。None 表示这张表没有分区或没读到；读失败的原因在 `partition_error`。
    pub partitions: Option<PartitionState>,
    /// 分区查询失败。表结构本身仍然读到，不能因此把整表当成没读到。
    pub partition_error: Option<String>,
    /// 事件快照。只有事件对象会填。
    pub event: Option<EventState>,
    /// 例程（函数/存储过程）快照。只有例程对象会填。
    pub routine: Option<RoutineState>,
    /// 触发器快照。只有触发器对象会填。
    pub trigger: Option<TriggerState>,
    /// 对象元数据读取失败的原因。与表结构读取分开，互不阻断。
    pub object_error: Option<String>,
}

impl Snapshot {
    pub fn bare(engine: Engine, database: &str, schema: &str, name: &str) -> Self {
        Self {
            engine,
            database: database.to_owned(),
            schema: schema.to_owned(),
            name: name.to_owned(),
            kind: "table".to_owned(),
            storage: None,
            sqlite_version: "3.51.3".to_owned(),
            recorded_foreign_keys: Some(false),
            live_foreign_keys: Some(false),
            columns: Vec::new(),
            indexes: Vec::new(),
            foreign_keys: Vec::new(),
            incoming: Vec::new(),
            triggers: Vec::new(),
            views: Vec::new(),
            table_sql: format!("CREATE TABLE {name} (id INTEGER)"),
            has_check: false,
            has_collate: false,
            without_rowid: false,
            strict: false,
            autoincrement: false,
            row_count: Some(0),
            null_counts: BTreeMap::new(),
            object_exists: true,
            reference: None,
            materialized: false,
            definition_error: None,
            index_error: None,
            foreign_key_error: None,
            sequence: None,
            enum_values: Vec::new(),
            domain: None,
            event_enabled: None,
            pragmas: BTreeMap::new(),
            attached: Vec::new(),
            name_taken: None,
            table_comment: None,
            column_comments: BTreeMap::new(),
            comment_error: None,
            transaction: crate::transaction::TransactionState::Idle,
            server_version: String::new(),
            sql_mode: String::new(),
            profile_error: None,
            enum_sets: BTreeMap::new(),
            generation: BTreeMap::new(),
            partitions: None,
            partition_error: None,
            event: None,
            routine: None,
            trigger: None,
            object_error: None,
        }
    }

    /// 字面量转义方式：会话 sql_mode 含 NO_BACKSLASH_ESCAPES 时反斜杠不是转义符。
    /// 没读到 sql_mode 时按服务器默认（反斜杠是转义符），这与连接的实际默认一致。
    pub fn literal_quoting(&self) -> LiteralQuoting {
        LiteralQuoting::from_sql_mode(&self.sql_mode)
    }
}

#[cfg(test)]
impl Snapshot {
    pub(crate) fn sample_index_failure() -> Self {
        let mut snapshot = Self::bare(Engine::Postgres, "app", "public", "orders");
        snapshot.columns.push(Column {
            name: "note".to_owned(),
            type_name: "text".to_owned(),
            nullable: true,
            default_sql: None,
            primary: false,
            auto: false,
            generated: false,
            default_constraint: None,
        });
        snapshot.index_error = Some("permission denied".to_owned());
        snapshot.foreign_key_error = Some("fk denied".to_owned());
        snapshot
    }
}

/// 列默认值的四种状态（ADR-7）。无默认值不写 DEFAULT；NULL 写 `DEFAULT NULL`；字面量由方言按列类型
/// 加引号与转义；表达式只接受白名单函数并原样写出。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", content = "value", rename_all = "camelCase")]
pub enum DefaultSpec {
    #[default]
    None,
    Null,
    Literal(String),
    Expression(String),
}

/// 默认值模式（界面下拉用）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DefaultMode {
    None,
    Null,
    Literal,
    Expression,
}

impl DefaultMode {
    pub const ALL: [DefaultMode; 4] = [DefaultMode::None, DefaultMode::Null, DefaultMode::Literal, DefaultMode::Expression];
}

impl DefaultSpec {
    pub fn mode(&self) -> DefaultMode {
        match self {
            DefaultSpec::None => DefaultMode::None,
            DefaultSpec::Null => DefaultMode::Null,
            DefaultSpec::Literal(_) => DefaultMode::Literal,
            DefaultSpec::Expression(_) => DefaultMode::Expression,
        }
    }

    /// 字面量或表达式的原文；其余为空串。
    pub fn text(&self) -> &str {
        match self {
            DefaultSpec::Literal(text) | DefaultSpec::Expression(text) => text,
            _ => "",
        }
    }

    /// 有非 NULL 的默认值（字面量或非空表达式）。
    pub fn has_value(&self) -> bool {
        match self {
            DefaultSpec::Literal(_) => true,
            DefaultSpec::Expression(text) => !text.trim().is_empty(),
            _ => false,
        }
    }

    /// 切换模式时保留已输入的值（字面量 ↔ 表达式）。
    pub fn with_mode(&self, mode: DefaultMode) -> Self {
        let text = self.text().to_owned();
        match mode {
            DefaultMode::None => DefaultSpec::None,
            DefaultMode::Null => DefaultSpec::Null,
            DefaultMode::Literal => DefaultSpec::Literal(text),
            DefaultMode::Expression => DefaultSpec::Expression(text),
        }
    }

    /// 改写值，模式不变；没有值的模式不受影响。返回是否改变。
    pub fn set_text(&mut self, value: String) -> bool {
        match self {
            DefaultSpec::Literal(text) | DefaultSpec::Expression(text) if *text != value => {
                *text = value;
                true
            }
            _ => false,
        }
    }

    /// 展示用（检索、变更说明）：无默认为空，NULL 为 `NULL`，其余为原文。
    pub fn label(&self) -> String {
        match self {
            DefaultSpec::None => String::new(),
            DefaultSpec::Null => "NULL".to_owned(),
            DefaultSpec::Literal(text) | DefaultSpec::Expression(text) => text.clone(),
        }
    }
}

/// 建表时的表级选项。哪些选项有意义、可选值由方言给出（见 `DdlDialect::table_option_spec`）。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TableOptions {
    pub engine: String,
    pub charset: String,
    pub collation: String,
    pub comment: String,
    pub without_rowid: bool,
    pub strict: bool,
}

/// 字面量转义方式。MySQL 默认把反斜杠当转义符；sql_mode 含 NO_BACKSLASH_ESCAPES 时反斜杠是普通字符，
/// 只能靠把单引号写成两个来转义。两种写法不能混用，所以编码与解析都要带上这个模式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LiteralQuoting {
    pub backslash_escapes: bool,
}

impl Default for LiteralQuoting {
    fn default() -> Self {
        Self { backslash_escapes: true }
    }
}

impl LiteralQuoting {
    pub fn from_sql_mode(sql_mode: &str) -> Self {
        let backslash_escapes = !sql_mode.to_ascii_uppercase().contains("NO_BACKSLASH_ESCAPES");
        Self { backslash_escapes }
    }
}

/// ENUM / SET 两种专有字段类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum EnumSetKind {
    #[default]
    Enum,
    Set,
}

impl EnumSetKind {
    pub const ALL: [EnumSetKind; 2] = [EnumSetKind::Enum, EnumSetKind::Set];

    /// 写进语句的类型关键字。
    pub fn keyword(&self) -> &'static str {
        match self {
            EnumSetKind::Enum => "enum",
            EnumSetKind::Set => "set",
        }
    }
}

/// 读回的 ENUM/SET 候选值，按定义顺序。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EnumSetState {
    pub kind: EnumSetKind,
    pub values: Vec<String>,
}

/// 草稿里的 ENUM/SET 候选值。顺序就是用户看到的顺序，写进语句时保持原样。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnumSetDraft {
    pub kind: EnumSetKind,
    pub values: Vec<String>,
}

/// 生成列的存储方式。MariaDB 的 PERSISTENT 与 STORED 同义，读回时归一为 Stored。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum GeneratedStorage {
    #[default]
    Virtual,
    Stored,
}

/// 读回的生成列：表达式与存储方式。普通表达式默认值不是生成列，不会出现在这里。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenerationState {
    pub expression: String,
    pub storage: GeneratedStorage,
    /// 服务器读回时的原文写法（MariaDB 可能写 PERSISTENT）。展示用，不影响生成的语句。
    pub reported_storage: String,
}

/// 草稿里的生成列定义。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeneratedColumnDraft {
    pub expression: String,
    pub storage: GeneratedStorage,
}

/// 分区方式。RANGE COLUMNS / LIST COLUMNS 与子分区不在本轮范围内。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum PartitionMethod {
    #[default]
    Range,
    List,
    Hash,
    Key,
}

impl PartitionMethod {
    pub const ALL: [PartitionMethod; 4] = [PartitionMethod::Range, PartitionMethod::List, PartitionMethod::Hash, PartitionMethod::Key];

    pub fn keyword(&self) -> &'static str {
        match self {
            PartitionMethod::Range => "RANGE",
            PartitionMethod::List => "LIST",
            PartitionMethod::Hash => "HASH",
            PartitionMethod::Key => "KEY",
        }
    }

    /// RANGE / LIST 有命名分区与边界；HASH / KEY 只按数量分。
    pub fn named(&self) -> bool {
        matches!(self, PartitionMethod::Range | PartitionMethod::List)
    }
}

/// 读回的一个分区：名字、顺序与边界原文（RANGE 是 VALUES LESS THAN 的值，LIST 是 VALUES IN 的值列表）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PartitionDefState {
    pub name: String,
    pub description: String,
}

/// 读回的分区方案。expression 对 RANGE/LIST 是表达式，对 KEY 是键列清单。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PartitionState {
    pub method: PartitionMethod,
    pub expression: String,
    pub definitions: Vec<PartitionDefState>,
}

/// 草稿里的一个分区定义。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartitionDef {
    pub name: String,
    /// RANGE 写边界值（如 \`100\` 或 \`MAXVALUE\`），LIST 写逗号分隔的取值。
    pub description: String,
}

/// 分区草稿上的操作。哪些操作适用于哪种方式由方言判断，不适用的只给阻断原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum PartitionOp {
    /// 重建整张表的分区方案（ALTER TABLE ... PARTITION BY ...）。会重写数据。
    #[default]
    Replace,
    /// 新增分区：RANGE/LIST 追加命名分区，HASH/KEY 增加数量。
    Add,
    /// 删除分区：RANGE/LIST 删命名分区，HASH/KEY 减少数量。
    Drop,
    /// 重组分区：把若干现有分区的数据重新分到新的分区定义里。
    Reorganize,
    /// 移除分区方案，表恢复成未分区。
    Remove,
}

/// 分区方案草稿。没用到的方式相关字段留空即可，由方言只取适用的部分。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartitionDraft {
    pub op: PartitionOp,
    pub method: PartitionMethod,
    /// RANGE/LIST/HASH 的表达式。KEY 用 \`columns\`。
    pub expression: String,
    /// KEY 分区的列名清单；HASH/KEY 的数量放 \`count\`。
    pub columns: Vec<String>,
    /// HASH/KEY 的分区数量。空表示没改或没填。
    pub count: String,
    /// Replace / Add / Reorganize 用的分区定义。
    pub definitions: Vec<PartitionDef>,
    /// Drop / Reorganize 指定的现有分区名。Reorganize 的目标写在 \`definitions\`。
    pub targets: Vec<String>,
}

/// 事件的调度：单次在某个时间点，周期按间隔重复。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "camelCase")]
pub enum EventSchedule {
    /// \`AT\` 的时间表达式原文（如 \`CURRENT_TIMESTAMP + INTERVAL 1 HOUR\`）。
    OneTime { at: String },
    /// \`EVERY <every> <unit>\`，可带 STARTS / ENDS。
    Recurring { every: String, unit: String, starts: String, ends: String },
}

impl Default for EventSchedule {
    fn default() -> Self {
        EventSchedule::OneTime { at: String::new() }
    }
}

/// ON COMPLETION [NOT] PRESERVE。PRESERVE 是服务器默认。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum EventCompletion {
    #[default]
    Preserve,
    NotPreserve,
}

/// 事件快照：调度、主体与保留属性。\`scheduler_running\` 是服务器 event_scheduler 的实际状态，
/// None 表示没读到；关闭时启用事件不会让它运行，界面必须照实说明。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EventState {
    pub name: String,
    pub enabled: bool,
    pub schedule: EventSchedule,
    pub body: String,
    pub on_completion: EventCompletion,
    pub comment: String,
    pub definer: String,
    pub scheduler_running: Option<bool>,
}

/// 事件草稿。\`original\` 有值表示在改已有事件。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventDraft {
    pub original: Option<String>,
    pub name: String,
    pub schedule: EventSchedule,
    pub body: String,
    pub enabled: bool,
    pub on_completion: EventCompletion,
    pub comment: String,
}

/// 例程类型。函数与存储过程不能互相替换。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum RoutineKind {
    Function,
    #[default]
    Procedure,
}

impl RoutineKind {
    pub fn keyword(&self) -> &'static str {
        match self {
            RoutineKind::Function => "FUNCTION",
            RoutineKind::Procedure => "PROCEDURE",
        }
    }
}

/// 参数模式。函数参数只有输入，存储过程可以 OUT / INOUT。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ParamMode {
    #[default]
    In,
    Out,
    InOut,
}

impl ParamMode {
    pub fn keyword(&self) -> &'static str {
        match self {
            ParamMode::In => "IN",
            ParamMode::Out => "OUT",
            ParamMode::InOut => "INOUT",
        }
    }
}

/// 读回的一个参数。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoutineParamState {
    pub position: u32,
    pub mode: ParamMode,
    pub name: String,
    pub type_name: String,
}

/// 例程快照：按 PARAMETERS 与 SHOW CREATE 组合。未开放编辑的属性也要读出来，修改时才能保留。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoutineState {
    pub kind: RoutineKind,
    pub name: String,
    pub params: Vec<RoutineParamState>,
    /// 函数的返回类型；存储过程为空。
    pub returns: String,
    pub body: String,
    pub definer: String,
    pub comment: String,
    /// SHOW CREATE 原文。读不到时为空，此时不许编辑提交。
    pub create_sql: String,
}

/// 例程草稿里的一个参数。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutineParamDraft {
    pub mode: ParamMode,
    pub name: String,
    pub type_name: String,
}

/// 例程草稿。\`original\` 有值表示在改已有例程。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutineDraft {
    pub kind: RoutineKind,
    pub original: Option<String>,
    pub name: String,
    pub params: Vec<RoutineParamDraft>,
    /// 函数的返回类型；存储过程留空。
    pub returns: String,
    pub body: String,
    pub comment: String,
}

/// 触发时机。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum TriggerTiming {
    #[default]
    Before,
    After,
}

impl TriggerTiming {
    pub fn keyword(&self) -> &'static str {
        match self {
            TriggerTiming::Before => "BEFORE",
            TriggerTiming::After => "AFTER",
        }
    }
}

/// 触发事件。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum TriggerEvent {
    #[default]
    Insert,
    Update,
    Delete,
}

impl TriggerEvent {
    pub fn keyword(&self) -> &'static str {
        match self {
            TriggerEvent::Insert => "INSERT",
            TriggerEvent::Update => "UPDATE",
            TriggerEvent::Delete => "DELETE",
        }
    }
}

/// 触发器快照：目标表、时机、事件与主体。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TriggerState {
    pub name: String,
    pub table: String,
    pub timing: TriggerTiming,
    pub event: TriggerEvent,
    pub body: String,
    pub definer: String,
    /// SHOW CREATE 原文。读不到时为空，此时不许编辑提交。
    pub create_sql: String,
}

/// 触发器草稿。\`original\` 有值表示在改已有触发器。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TriggerDraft {
    pub original: Option<String>,
    pub name: String,
    pub table: String,
    pub timing: TriggerTiming,
    pub event: TriggerEvent,
    pub body: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnDraft {
    pub original: Option<String>,
    pub name: String,
    pub type_name: String,
    pub nullable: bool,
    #[serde(default)]
    pub default: DefaultSpec,
    /// 字段注释。只有建表时写出（修改表的注释不在这里改）。
    #[serde(default)]
    pub comment: String,
    pub primary: bool,
    pub auto: bool,
    pub dropped: bool,
    /// ENUM/SET 字段的候选值。None 表示这列不是 ENUM/SET，类型仍按 `type_name` 原文。
    #[serde(default)]
    pub enum_set: Option<EnumSetDraft>,
    /// 生成列的表达式与存储方式。None 表示普通列；表达式默认值不算生成列。
    #[serde(default)]
    pub generated: Option<GeneratedColumnDraft>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexColumnDraft {
    pub name: String,
    pub descending: bool,
    /// 前缀长度。只有 MySQL 家族的字符串列可以填。
    #[serde(default)]
    pub prefix: Option<u32>,
}

/// 新建或修改后的索引。
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexDraft {
    pub name: String,
    #[serde(default)]
    pub kind: IndexKind,
    pub columns: Vec<IndexColumnDraft>,
    /// 索引注释。空串表示不写注释。
    #[serde(default)]
    pub comment: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForeignKeyDraft {
    pub name: String,
    pub columns: Vec<String>,
    pub ref_schema: String,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
    pub on_delete: String,
    pub on_update: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Action {
    CreateTable {
        name: String,
        columns: Vec<ColumnDraft>,
        #[serde(default)]
        options: TableOptions,
    },
    AlterColumns { table: String, columns: Vec<ColumnDraft> },
    /// 新建索引。`kind` 为普通而 `unique` 为真时按唯一索引（兼容只有唯一开关的调用方）。
    CreateIndex {
        table: String,
        name: String,
        unique: bool,
        columns: Vec<IndexColumnDraft>,
        /// 动作本身的标签叫 kind（见 serde tag），索引类型换名字。
        #[serde(default)]
        index_kind: IndexKind,
        #[serde(default)]
        comment: String,
    },
    /// 修改已有索引：MySQL 家族一条 ALTER TABLE 先删后加；其他引擎删除再新建两条语句，中途失败时旧索引已经不在。
    AlterIndex { table: String, old: String, new: IndexDraft },
    DropIndex { table: String, name: String },
    CreateForeignKey { table: String, foreign_key: ForeignKeyDraft },
    DropForeignKey { table: String, name: String },
    DropTable { table: String },
    /// 删除一个视图（PostgreSQL 物化视图走 DROP MATERIALIZED VIEW）。不删除底层表的数据。
    DropView { view: String },
    /// 清空一张表的数据，保留结构。SQLite 没有 TRUNCATE，用 DELETE FROM。
    TruncateTable { table: String },
    /// 给一张表改名。新名字为空或已被占用时只给出阻断原因。
    RenameTable { table: String, new_name: String },
    /// 只改变当前 SQLite 连接的 foreign_keys。不清理数据，也不表示约束已经验证通过。
    SetForeignKeys { enabled: bool },
    /// 替换已有视图的定义。确认前只生成语句，不发送。
    ReplaceView { table: String, definition: String },
    /// 改 PostgreSQL 序列的参数。只有填了值的子句会进语句。
    AlterSequence { sequence: String, start: Option<i64>, increment: Option<i64>, min_value: Option<i64>, max_value: Option<i64>, cycle: Option<bool> },
    /// 给 PostgreSQL 枚举追加一个值。枚举值只能追加，不能删除或重排。
    AddEnumValue { enum_name: String, value: String, before: Option<String> },
    /// 改 PostgreSQL 域的默认值与 NOT NULL。默认值为空串表示删掉默认值。
    AlterDomain { domain: String, default_sql: Option<String>, not_null: Option<bool> },
    /// 启停 MySQL/MariaDB 事件。事件的事件体仍然只读。
    SetEventEnabled { event: String, enabled: bool },
    /// 改 SQLite 的 PRAGMA 值。foreign_keys 走 SetForeignKeys，这里不重复。
    SetPragma { pragma: String, value: String },
    /// 附加一个 SQLite 数据库文件。只对当前连接生效。
    AttachDatabase { path: String, name: String },
    /// SQLite 维护：整理数据库文件。确认前不写入。
    Vacuum,
    /// SQLite 维护：重建索引。不带名字时重建整个库的索引。
    Reindex { target: Option<String> },
    /// 改 MySQL/MariaDB 表的分区方案。RANGE/LIST 有命名分区，HASH/KEY 按数量；不适用时只给阻断原因。
    AlterPartitions { table: String, partition: PartitionDraft },
    /// 创建事件。调度器关闭时启用事件只说明它仍然不会运行，不改服务器级状态。
    CreateEvent { event: EventDraft },
    /// 改已有事件（ALTER EVENT）。启用状态另走 `SetEventEnabled`。
    AlterEvent { event: EventDraft },
    /// 删除事件。
    DropEvent { event: String },
    /// 创建函数或存储过程。
    CreateRoutine { routine: RoutineDraft },
    /// 替换函数或存储过程：DROP + CREATE，中途失败时对象可能已经不存在。
    ReplaceRoutine { routine: RoutineDraft },
    /// 删除函数或存储过程。函数与存储过程不能互相替换，所以要带类型。
    /// 动作本身的标签已经叫 kind（见 Action 的 serde tag），这里的字段换名字避免冲突。
    DropRoutine { routine_kind: RoutineKind, name: String },
    /// 创建触发器。
    CreateTrigger { trigger: TriggerDraft },
    /// 替换触发器：DROP + CREATE，中途失败时触发器可能已经不存在。
    ReplaceTrigger { trigger: TriggerDraft },
    /// 删除触发器。
    DropTrigger { trigger: String },
}

#[derive(Clone, Debug)]
pub struct Request {
    pub database: String,
    pub schema: String,
    pub engine: Engine,
    pub action: Action,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub sql: String,
    pub label: String,
}

#[derive(Clone, Debug)]
pub struct Risk {
    pub level: String,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct Plan {
    pub blocked: Vec<String>,
    pub risks: Vec<Risk>,
    pub steps: Vec<Step>,
    pub rule: String,
    pub rebuild: bool,
    pub target: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Ok,
    Failed,
    Skipped,
    Unknown,
    RolledBack,
}

/// 活动外层事务里 SQLite 重建的保存点边界（ADR-4）。
///
/// 有外层事务时不能另开内部事务：执行层把重建计划里的 `BEGIN`/`COMMIT` 换成这两条语句，
/// 适配器的逐条执行据此只回滚到保存点，不撤销此前已经应用的修改。无外层事务时仍用原有重建事务。
pub(crate) const SQLITE_REBUILD_SAVEPOINT: &str = "rillsql_rebuild";

/// 重建保存点的开始语句（执行层与适配器共用同一份文本）。
pub(crate) const SQLITE_REBUILD_SAVEPOINT_SQL: &str = "SAVEPOINT rillsql_rebuild;";

/// 重建保存点的释放语句。
pub(crate) const SQLITE_REBUILD_RELEASE_SQL: &str = "RELEASE SAVEPOINT rillsql_rebuild;";

#[derive(Clone, Debug)]
pub struct StepReceipt {
    pub sql: String,
    pub label: String,
    pub status: StepStatus,
    pub message: String,
}

/// 引擎的外键引用动作（由适配器的类型规则给出；没有结构编辑的引擎为空）。
pub use crate::adapters::ddl::fk_actions;

/// 执行语义说明。`rebuild` 为真时按重建方式说明；没有结构编辑的引擎说明这一点。
pub fn rule_text(engine: Engine, rebuild: bool) -> String {
    match crate::adapters::ddl::ddl_of(engine) {
        Some(dialect) if rebuild => dialect.rebuild_rule_text().to_owned(),
        Some(dialect) => dialect.rule_text().to_owned(),
        None => crate::adapters::ddl::no_structure_editing(engine),
    }
}
