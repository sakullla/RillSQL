//! 类型规则与 DDL 方言（ADR-0 / R22，A3）：类型/默认值候选与校验、建表/改表/删改/视图/重建语句、
//! 引擎专属的库级动作、结构内省读取与语句执行。
//!
//! 主代码（`ddl/`、`app/views/structure`、`app/views/admin`）只经 [`ddl_of`] / [`types_of`] 取得能力组再调用；
//! 各引擎的实现写在 `adapters/<engine>/ddl.rs`（计划）、`ddl_read.rs`（内省与执行）与 `types.rs`。
//! 没有结构编辑的引擎（Redis/MongoDB）不提供这两组，门面按“没有结构编辑”处理。

use std::collections::BTreeMap;

use crate::connections::model::Engine;
use crate::connections::CatalogSession;
use crate::ddl::model::{Action, ColumnDraft, ForeignKey, Plan, Request, Risk, Snapshot, StepReceipt};
use crate::ddl::plan::{Change, Tok};

use super::{adapter, quote_ident, registry};

/// 完整的外键引用动作（多数引擎）。
pub const FULL_FK_ACTIONS: &[&str] = &["RESTRICT", "CASCADE", "SET NULL", "SET DEFAULT", "NO ACTION"];

/// 管理页分区门控：哪些库级对象与维护动作对这个引擎有意义。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct AdminSections {
    /// 连接级设置：库文件参数与连接上的外键开关。
    pub settings: bool,
    pub attached: bool,
    pub maintenance: bool,
    pub extensions: bool,
    pub sequences: bool,
    pub events: bool,
    pub partitions: bool,
    pub enums: bool,
    pub domains: bool,
}

/// 常用字段模板（时间戳、软删除、状态备注、UUID）使用的类型与默认值。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PresetTypes {
    pub timestamp: &'static str,
    pub timestamp_default: &'static str,
    pub short_text: &'static str,
    pub note: &'static str,
    pub uuid: &'static str,
}

/// 通用写法：没有专属类型系统时按 SQL 的基础类型名（文件型引擎也用这一套）。
pub const GENERIC_PRESET_TYPES: PresetTypes =
    PresetTypes { timestamp: "TEXT", timestamp_default: "CURRENT_TIMESTAMP", short_text: "TEXT", note: "TEXT", uuid: "TEXT" };

/// 类型规则：类型候选与校验、默认值候选与校验、FK 动作、常用字段类型映射。
/// 默认实现是通用写法（未知引擎的结构页候选也按它给出）。
pub trait TypeRules: Sync {
    /// 外键引用动作。
    fn fk_actions(&self) -> &'static [&'static str] {
        FULL_FK_ACTIONS
    }

    /// 手输类型能否按该方言生成。`base` 是小写、去掉括号参数后的类型名（已通过字符与长度检查）。
    fn valid_type_base(&self, _base: &str) -> bool {
        false
    }

    /// 字面量与通用当前时间函数之外，该方言额外接受的默认值。`text` 已去掉首尾空白，`upper` 是它的大写。
    fn extra_default(&self, _text: &str, _upper: &str) -> bool {
        false
    }

    /// 字段类型下拉候选。
    fn type_choices(&self) -> &'static [&'static str] {
        &["INTEGER", "TEXT", "REAL", "NUMERIC", "BLOB"]
    }

    /// 默认值下拉候选（不含 NULL；可空字段由调用方补在最前）。
    fn default_choices(&self) -> &'static [&'static str] {
        &["0", "''", "CURRENT_TIMESTAMP", "CURRENT_DATE"]
    }

    /// 新字段的默认类型。
    fn default_column_type(&self) -> &'static str {
        "TEXT"
    }

    /// 新表默认主键 id 的类型。
    fn id_column_type(&self) -> &'static str {
        "INTEGER"
    }

    /// 常用字段模板的类型。
    fn preset_types(&self) -> PresetTypes {
        GENERIC_PRESET_TYPES
    }
}

/// 未知引擎（结构页拿到的引擎字符串解析不出来）按通用写法给候选。
struct GenericTypes;

impl TypeRules for GenericTypes {}

/// 视图语句头 CREATE 与 VIEW 之间一个方言专属修饰读完后的结果（值是修饰之后的记号位置）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewModifier {
    /// 不影响替换的修饰（例如定义者、算法）。
    Skip(usize),
    /// 物化视图：不能在保留数据的前提下替换。
    Materialized(usize),
}

/// 表与列的注释：(表注释, 列名 → 注释)。
pub type Comments = (Option<String>, BTreeMap<String, String>);

/// DDL 方言。计划生成是纯函数；读取与执行在已打开的会话上进行，不新开连接。
#[async_trait::async_trait]
pub trait DdlDialect: Sync {
    // ---- 命名与执行语义 ----

    /// 结构变更目标的限定名；缺库/模式或不是当前库时说明原因（不另开连接）。
    fn qualified_table(&self, database: &str, schema: &str, table: &str) -> Result<String, String>;

    /// 建表语句里的表名。默认同限定名。
    fn create_table_name(&self, database: &str, schema: &str, table: &str) -> Result<String, String> {
        self.qualified_table(database, schema, table)
    }

    /// 计划目标说明里引擎 id 之后的部分（库、模式与对象名的排列）。
    fn target_scope(&self, database: &str, schema: &str, table: &str) -> String;

    /// 逐条执行的语义说明（失败即停、是否会回滚）。
    fn rule_text(&self) -> &'static str;

    /// 按重建方式执行时的语义说明。只有会重建表的方言需要。
    fn rebuild_rule_text(&self) -> &'static str {
        self.rule_text()
    }

    /// 外键是否由连接上的开关（会话级）控制：计划要说明当前开关，执行后要恢复。
    fn session_foreign_keys(&self) -> bool {
        false
    }

    /// 给已有表新增没有非 NULL 默认值的非空字段时，方言本身就拒绝（与表里有没有数据无关）。返回阻断原因。
    fn add_not_null_without_default(&self, _column: &str) -> Option<String> {
        None
    }

    // ---- 建表与改字段 ----

    /// 单列整数主键自增时写成内联的 `PRIMARY KEY AUTOINCREMENT`（不另写主键约束）。
    fn inline_autoincrement_primary_key(&self) -> bool {
        false
    }

    /// 内联自增主键列的定义末尾补上主键与自增子句（已经写了就不重复）。
    fn finish_inline_autoincrement(&self, _sql: &mut String) {}

    /// 建表语句右括号之后的表选项。
    fn create_table_suffix(&self, _without_rowid: bool, _strict: bool) -> String {
        String::new()
    }

    /// 字段定义里写在 NOT NULL 之前的自增子句。
    fn auto_clause_before_null(&self, _draft: &ColumnDraft) -> &'static str {
        ""
    }

    /// 字段定义里写在 NOT NULL 之后的自增子句。
    fn auto_clause_after_null(&self, _draft: &ColumnDraft) -> &'static str {
        ""
    }

    /// 改字段的计划：直接改写语句，或（方言不支持直接改时）重建表。
    fn alter_plan(&self, snapshot: &Snapshot, table: &str, drafts: &[ColumnDraft], changes: &[Change], target: String, risks: Vec<Risk>) -> Plan;

    // ---- 索引与外键 ----

    /// 删除索引的语句。`index` 已按方言引用。
    fn drop_index_sql(&self, snapshot: &Snapshot, index: &str) -> Result<String, String>;

    /// 删除外键约束用的子句。
    fn drop_foreign_key_clause(&self) -> &'static str {
        "DROP CONSTRAINT"
    }

    /// 外键可以引用另一个模式里的表（结构页显示目标模式）。
    fn references_cross_schema(&self) -> bool {
        false
    }

    /// 读目标表与生成引用时使用的模式：None 表示与本表同一处。
    fn reference_scope<'a>(&self, _ref_schema: &'a str) -> Option<&'a str> {
        None
    }

    /// 外键 REFERENCES 之后的表名。
    fn fk_reference(&self, snapshot: &Snapshot, ref_schema: &str, ref_table: &str) -> Result<String, String> {
        match self.reference_scope(ref_schema) {
            Some(schema) => self.qualified_table(&snapshot.database, schema, ref_table),
            None => Ok(quote_ident(snapshot.engine, ref_table)),
        }
    }

    /// 没有读到目标表时，即使目标表名为空也拦下（为假时空表名交给其它校验报出）。
    fn requires_reference_read(&self) -> bool {
        true
    }

    /// 方言本身不允许给这张表加外键（例如存储引擎不支持）。返回阻断原因。
    fn foreign_key_blocker(&self, _snapshot: &Snapshot, _table: &str) -> Option<String> {
        None
    }

    /// 方言不能直接增删外键时的说明（`adding` 区分新增与删除）。
    fn foreign_key_change_risk(&self, _adding: bool) -> Option<&'static str> {
        None
    }

    /// 方言不能直接增删外键时，按新的外键清单重建表。能直接改的方言返回 None。
    fn rebuild_with_foreign_keys(&self, _snapshot: &Snapshot, _keys: &[ForeignKey], _target: &str, _risks: &[Risk]) -> Option<Plan> {
        None
    }

    // ---- 删除、清空与改名 ----

    /// 删除被外键引用的表时的说明。`live` 是连接上的外键开关（会话级外键的方言才有）。
    fn drop_referenced_note(&self, _live: Option<bool>) -> &'static str {
        "不会添加 CASCADE。被引用时数据库会拒绝删除。"
    }

    /// 清空表的语义说明。
    fn truncate_note(&self) -> &'static str;

    /// 清空被外键引用的表时的说明。
    fn truncate_referenced_note(&self, _live: Option<bool>) -> &'static str {
        "被外键引用的表，数据库可能拒绝 TRUNCATE。不会添加 CASCADE。"
    }

    /// 清空表的语句。
    fn truncate_sql(&self, qualified: &str) -> String {
        format!("TRUNCATE TABLE {qualified};")
    }

    /// 删除视图的关键字。
    fn drop_view_keyword(&self, _materialized: bool) -> &'static str {
        "DROP VIEW"
    }

    /// 方言不允许的改名（例如只改大小写）。返回阻断原因。
    fn rename_blocker(&self, _table: &str, _wanted: &str) -> Option<String> {
        None
    }

    /// 改名语句本身的额外说明。
    fn rename_note(&self) -> Option<&'static str> {
        None
    }

    /// 改表名的语句。
    fn rename_table_sql(&self, snapshot: &Snapshot, table: &str, new_name: &str) -> Result<String, String> {
        let current = self.qualified_table(&snapshot.database, &snapshot.schema, table)?;
        Ok(format!("ALTER TABLE {current} RENAME TO {};", quote_ident(snapshot.engine, new_name)))
    }

    // ---- 视图 ----

    /// 视图被其它视图或触发器依赖时不能替换（替换要先删）。
    fn replace_view_blocked_by_dependents(&self) -> bool {
        false
    }

    /// 支持就地替换视图（CREATE OR REPLACE VIEW）；否则先删后建。
    fn replaces_view_in_place(&self) -> bool {
        false
    }

    /// 视图语句头 CREATE 与 VIEW 之间、通用写法（OR REPLACE、TEMP、RECURSIVE）之外的修饰：从 `index` 读一个。
    /// 不认识或写法不对返回 None（整段定义按不能安全替换处理）。
    fn view_modifier(&self, _tokens: &[Tok], _index: usize) -> Option<ViewModifier> {
        None
    }

    /// 视图名前面的限定段可以是哪些名字。
    fn view_qualifiers<'a>(&self, snapshot: &'a Snapshot) -> Vec<&'a str> {
        vec![snapshot.schema.as_str()]
    }

    // ---- 由元数据重建的定义 ----

    /// 重建定义里没有包含的内容（CHECK 约束与分区之外）。
    fn rebuilt_missing(&self) -> &'static [&'static str] {
        &[]
    }

    /// 有自增列时，重建定义没有包含的自增参数。
    fn rebuilt_identity_missing(&self) -> Option<&'static str> {
        None
    }

    /// 生成列的定义子句；None 表示不写出生成表达式。
    fn generated_column_clause(&self, _expression: &str) -> Option<String> {
        None
    }

    /// 重建定义里自增列的子句。
    fn rebuilt_auto_clause(&self) -> &'static str {
        ""
    }

    /// 外键没有记录目标模式时，重建定义按哪个模式限定目标表（空串表示不限定）。
    fn implied_reference_schema<'a>(&self, _table_schema: &'a str) -> &'a str {
        ""
    }

    /// 重建定义末尾的表/列注释语句。
    fn comment_statements(&self, _snapshot: &Snapshot, _qualified: &str) -> Vec<String> {
        Vec::new()
    }

    /// 结构页表头显示的表选项标签（读自快照里方言专属的表选项）。
    fn table_option_labels(&self, _snapshot: &Snapshot) -> Vec<&'static str> {
        Vec::new()
    }

    // ---- 引擎专属的库级动作（序列、枚举、域、事件、PRAGMA、附加库、维护、外键开关） ----

    /// 这个方言提供该动作时给出计划；不提供返回 None。
    fn plan_object_action(&self, _snapshot: &Snapshot, _action: &Action) -> Option<Plan> {
        None
    }

    /// 该动作属于这个方言、却被别的引擎请求时的拒绝计划；动作不属于它返回 None。
    fn refuse_object_action(&self, _snapshot: &Snapshot, _action: &Action) -> Option<Plan> {
        None
    }

    /// 管理页分区。
    fn admin_sections(&self) -> AdminSections {
        AdminSections::default()
    }

    // ---- 英文 ----

    /// 方言专属计划文字的英文（整句，不含可变部分）。
    fn messages_en(&self) -> &'static [(&'static str, &'static str)] {
        &[]
    }

    /// 方言专属的重建定义“未包含”项的英文。
    fn missing_items_en(&self) -> &'static [(&'static str, &'static str)] {
        &[]
    }

    // ---- 会话：内省与执行 ----

    /// 会话句柄属于这个方言的引擎。
    fn matches_session(&self, session: &CatalogSession) -> bool;

    /// 读表或视图的结构快照（索引/外键失败只记在快照里）。
    async fn load_snapshot(&self, session: &CatalogSession, request: &Request, table: &str) -> Result<Snapshot, String>;

    /// 改名前查新名字是否已被同一命名空间里的对象占用（被改名的表自身不算）。
    async fn name_taken(&self, session: &CatalogSession, snapshot: &Snapshot, name: &str) -> Result<bool, String>;

    /// 执行一条计数语句，取第一行第一列。
    async fn count(&self, session: &CatalogSession, sql: &str) -> Result<i64, String>;

    /// 表与列注释（只给重建定义用）。方言没有可读的注释返回 None。
    async fn load_comments(&self, _session: &CatalogSession, _schema: &str, _table: &str) -> Option<Result<Comments, String>> {
        None
    }

    /// 补上会话级状态（外键开关、版本），没有快照对象时也要读（建表、外键开关）。
    async fn read_session_state(&self, _session: &CatalogSession, _snapshot: &mut Snapshot) -> Result<(), String> {
        Ok(())
    }

    /// 读库级动作自己的状态（序列参数、枚举取值、域、事件、PRAGMA、附加库）。不提供的动作不读。
    async fn read_object_state(&self, _session: &CatalogSession, _request: &Request, _snapshot: &mut Snapshot) -> Result<(), String> {
        Ok(())
    }

    /// 维护动作完成后的结果反馈。
    async fn maintenance_note(&self, _session: &CatalogSession, _action: &Action) -> Option<String> {
        None
    }

    /// 这条已执行成功的语句把连接上的外键开关设成了 `enabled`。
    fn sets_session_foreign_keys(&self, _sql: &str, _enabled: bool) -> bool {
        false
    }
}

/// 门面：引擎的 DDL 方言；没有结构编辑的引擎返回 None。
pub fn ddl_of(engine: Engine) -> Option<&'static dyn DdlDialect> {
    adapter(engine).ddl()
}

/// 门面：引擎的类型规则；没有结构编辑的引擎返回 None。
pub fn types_of(engine: Engine) -> Option<&'static dyn TypeRules> {
    adapter(engine).types()
}

/// 门面：结构页按引擎字符串取类型规则；解析不出或没有类型规则时按通用写法。
pub fn types_named(engine: &str) -> &'static dyn TypeRules {
    registry::parse(engine).and_then(types_of).unwrap_or(&GenericTypes)
}

/// 门面：结构页按引擎字符串取外键动作。专项引擎没有外键动作；解析不出的按通用写法。
pub fn fk_actions_named(engine: &str) -> &'static [&'static str] {
    match registry::parse(engine) {
        Some(engine) => fk_actions(engine),
        None => GenericTypes.fk_actions(),
    }
}

/// 门面：引擎的外键动作；没有结构编辑的引擎为空。
pub fn fk_actions(engine: Engine) -> &'static [&'static str] {
    types_of(engine).map(|types| types.fk_actions()).unwrap_or(&[])
}

/// 门面：结构页按引擎字符串取 DDL 方言。
pub fn ddl_named(engine: &str) -> Option<&'static dyn DdlDialect> {
    registry::parse(engine).and_then(ddl_of)
}

/// 没有结构编辑的引擎的统一说明。
pub fn no_structure_editing(engine: Engine) -> String {
    format!("{} 没有结构编辑。", adapter(engine).meta().label())
}

/// 全部提供结构编辑的方言（英文句型与库级动作归属查找用）。
pub fn all_dialects() -> impl Iterator<Item = &'static dyn DdlDialect> {
    registry::ENGINES.into_iter().filter_map(ddl_of)
}

/// 库级动作：当前引擎提供就给计划，否则由该动作所属的方言给出拒绝计划。
pub fn plan_object_action(snapshot: &Snapshot, action: &Action) -> Option<Plan> {
    if let Some(plan) = ddl_of(snapshot.engine).and_then(|dialect| dialect.plan_object_action(snapshot, action)) {
        return Some(plan);
    }
    all_dialects().find_map(|dialect| dialect.refuse_object_action(snapshot, action))
}

/// 约束定义里括号中的列名清单（去掉各方言的引号）。
pub(crate) fn paren_list(text: &str) -> Option<Vec<String>> {
    let start = text.find('(')?;
    let end = text[start + 1..].find(')')? + start + 1;
    Some(
        text[start + 1..end]
            .split(',')
            .map(|part| part.trim().trim_matches('"').trim_matches('`').to_owned())
            .filter(|part| !part.is_empty())
            .collect(),
    )
}

/// 执行回执里的语句把连接上的外键开关设成了 `enabled`。
pub fn sets_session_foreign_keys(engine: Engine, step: &StepReceipt, enabled: bool) -> bool {
    ddl_of(engine).is_some_and(|dialect| dialect.sets_session_foreign_keys(&step.sql, enabled))
}
