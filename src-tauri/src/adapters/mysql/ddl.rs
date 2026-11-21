//! MySQL 家族（MySQL / MariaDB）的 DDL 方言：按库限定、CHANGE COLUMN 改字段、AUTO_INCREMENT、InnoDB 表选项、
//! RENAME TABLE，以及事件启停的库级动作。MariaDB 复用同一实现，按 `engine` 区分。

use crate::adapters::ddl::{AdminSections, DdlDialect, TableOptionSpec, ViewModifier};
use crate::adapters::quote_ident;
use crate::connections::model::Engine;
use crate::connections::CatalogSession;
use crate::ddl::introspect::SESSION_MISMATCH;
use crate::ddl::model::{
    Action, ColumnDraft, EnumSetDraft, GeneratedColumnDraft, LiteralQuoting, Plan, Request, Risk, Snapshot, Step, TableOptions,
};
use crate::ddl::plan::{column_sql_for, finish, is_word, statement_alter_plan, symbol_is, target_label, word_is, Change, Tok, TokKind};
use crate::ddl::sql::{qualified_table, valid_name};

use super::{ddl_read, object_ddl, object_read, profile::MysqlProfile, table_ddl};

pub(in crate::adapters) struct MysqlDdl {
    pub engine: Engine,
}

pub(in crate::adapters) static MYSQL_DDL: MysqlDdl = MysqlDdl { engine: Engine::Mysql };
pub(in crate::adapters) static MARIADB_DDL: MysqlDdl = MysqlDdl { engine: Engine::Mariadb };

#[async_trait::async_trait]
impl DdlDialect for MysqlDdl {
    fn qualified_table(&self, database: &str, _schema: &str, table: &str) -> Result<String, String> {
        if database.is_empty() {
            return Err("需要数据库名，没有另开连接。".to_owned());
        }
        Ok(crate::adapters::qualified(self.engine, &[database, table]))
    }

    fn target_scope(&self, database: &str, _schema: &str, table: &str) -> String {
        format!("{database}.{table}")
    }

    fn rule_text(&self) -> &'static str {
        "这些语句逐条执行，失败即停。MySQL 与 MariaDB 会立即提交每条结构变更语句，中途失败不会撤销已经成功的语句，不能声称已经全部回滚。"
    }

    /// 没选的选项按 InnoDB + utf8mb4 写出；排序规则与表注释只有填了才写。
    fn create_table_suffix(&self, options: &TableOptions) -> String {
        let pick = |value: &str, fallback: &str| if value.trim().is_empty() { fallback.to_owned() } else { value.trim().to_owned() };
        let mut suffix = format!(" ENGINE={} DEFAULT CHARSET={}", pick(&options.engine, "InnoDB"), pick(&options.charset, "utf8mb4"));
        if !options.collation.trim().is_empty() {
            suffix.push_str(&format!(" COLLATE={}", options.collation.trim()));
        }
        if !options.comment.trim().is_empty() {
            suffix.push_str(&format!(" COMMENT={}", mysql_text(options.comment.trim())));
        }
        suffix
    }

    fn table_option_spec(&self) -> TableOptionSpec {
        // 排序规则属于字符集：选了字符集后只给它的排序规则，否则服务端报 COLLATION ... is not valid for CHARACTER SET。
        const MARIADB_UTF8MB4: &[&str] = &["utf8mb4_general_ci", "utf8mb4_unicode_ci", "utf8mb4_unicode_520_ci", "utf8mb4_bin"];
        const MYSQL_UTF8MB4: &[&str] = &["utf8mb4_0900_ai_ci", "utf8mb4_general_ci", "utf8mb4_unicode_ci", "utf8mb4_bin"];
        const OTHERS: [(&str, &[&str]); 4] = [
            ("utf8mb3", &["utf8mb3_general_ci", "utf8mb3_unicode_ci", "utf8mb3_bin"]),
            ("latin1", &["latin1_swedish_ci", "latin1_general_ci", "latin1_bin"]),
            ("ascii", &["ascii_general_ci", "ascii_bin"]),
            ("binary", &["binary"]),
        ];
        const MARIADB_BY_CHARSET: &[(&str, &[&str])] = &[("utf8mb4", MARIADB_UTF8MB4), OTHERS[0], OTHERS[1], OTHERS[2], OTHERS[3]];
        const MYSQL_BY_CHARSET: &[(&str, &[&str])] = &[("utf8mb4", MYSQL_UTF8MB4), OTHERS[0], OTHERS[1], OTHERS[2], OTHERS[3]];
        let (collations, charset_collations) = match self.engine {
            Engine::Mariadb => (MARIADB_UTF8MB4, MARIADB_BY_CHARSET),
            _ => (MYSQL_UTF8MB4, MYSQL_BY_CHARSET),
        };
        TableOptionSpec {
            engines: &["InnoDB", "MyISAM", "MEMORY", "ARCHIVE"],
            charsets: &["utf8mb4", "utf8mb3", "latin1", "ascii", "binary"],
            collations,
            charset_collations,
            comment: true,
            column_comments: true,
            ..TableOptionSpec::default()
        }
    }

    fn inline_column_comment(&self, comment: &str) -> Option<String> {
        Some(format!(" COMMENT {}", mysql_text(comment)))
    }

    fn auto_clause_after_null(&self, draft: &ColumnDraft) -> &'static str {
        if draft.auto {
            " AUTO_INCREMENT"
        } else {
            ""
        }
    }

    fn alter_plan(&self, snapshot: &Snapshot, table: &str, drafts: &[ColumnDraft], changes: &[Change], target: String, risks: Vec<Risk>) -> Plan {
        let engine = self.engine;
        // 服务器版本不支持草稿里的专有形态时在预览前阻断，不生成会被服务器拒绝的语句。
        if let Some(problem) = self.shape_capability_problem(snapshot, drafts) {
            return finish(engine, false, target, vec![problem], risks, Vec::new());
        }
        statement_alter_plan(snapshot, table, changes, target, risks, "ADD COLUMN", &|qualified, original, draft| {
            // CHANGE COLUMN 按完整的新定义一次改名、改类型、改可空、默认值与专有形态。
            let definition = column_sql_for(engine, draft, false, snapshot.literal_quoting())?;
            Ok(vec![Step {
                sql: format!("ALTER TABLE {qualified} CHANGE COLUMN {} {definition};", quote_ident(engine, &original.name)),
                label: format!("修改字段 {}", original.name),
            }])
        })
    }

    fn drop_index_sql(&self, snapshot: &Snapshot, index: &str) -> Result<String, String> {
        Ok(format!("DROP INDEX {index} ON {};", self.qualified_table(&snapshot.database, &snapshot.schema, &snapshot.name)?))
    }

    fn drop_foreign_key_clause(&self) -> &'static str {
        "DROP FOREIGN KEY"
    }

    /// 目标表总在同一个库里，按库限定。
    fn fk_reference(&self, snapshot: &Snapshot, _ref_schema: &str, ref_table: &str) -> Result<String, String> {
        self.qualified_table(&snapshot.database, "", ref_table)
    }

    fn foreign_key_blocker(&self, snapshot: &Snapshot, table: &str) -> Option<String> {
        snapshot
            .storage
            .as_deref()
            .is_some_and(|storage| storage.eq_ignore_ascii_case("MyISAM"))
            .then(|| format!("表 {table} 使用 MyISAM，不支持外键。不会通过关闭检查来假装创建成功。"))
    }

    fn truncate_note(&self) -> &'static str {
        "TRUNCATE TABLE 会立即提交并把 AUTO_INCREMENT 重置到初始值，不触发 DELETE 触发器。"
    }

    fn rename_table_sql(&self, snapshot: &Snapshot, table: &str, new_name: &str) -> Result<String, String> {
        let current = self.qualified_table(&snapshot.database, &snapshot.schema, table)?;
        let renamed = self.qualified_table(&snapshot.database, &snapshot.schema, new_name)?;
        Ok(format!("RENAME TABLE {current} TO {renamed};"))
    }

    fn replaces_view_in_place(&self) -> bool {
        true
    }

    /// ALGORITHM=…、DEFINER=…、SQL SECURITY DEFINER|INVOKER（SHOW CREATE VIEW 原样带出）。
    fn view_modifier(&self, tokens: &[Tok], index: usize) -> Option<ViewModifier> {
        let token = tokens.get(index)?;
        let mut index = index + 1;
        if word_is(token, "algorithm") {
            if tokens.get(index).is_some_and(|item| symbol_is(item, '=')) {
                index += 1;
            }
            if !tokens.get(index).is_some_and(is_word) {
                return None;
            }
            return Some(ViewModifier::Skip(index + 1));
        }
        if word_is(token, "definer") {
            if tokens.get(index).is_some_and(|item| symbol_is(item, '=')) {
                index += 1;
            }
            return skip_definer(tokens, index).map(ViewModifier::Skip);
        }
        if word_is(token, "sql") {
            if !tokens.get(index).is_some_and(|item| word_is(item, "security")) {
                return None;
            }
            let security = tokens.get(index + 1)?;
            if !(word_is(security, "definer") || word_is(security, "invoker")) {
                return None;
            }
            return Some(ViewModifier::Skip(index + 2));
        }
        None
    }

    fn view_qualifiers<'a>(&self, snapshot: &'a Snapshot) -> Vec<&'a str> {
        vec![snapshot.database.as_str()]
    }

    fn rebuilt_auto_clause(&self) -> &'static str {
        " AUTO_INCREMENT"
    }

    /// ENUM/SET 的类型写法与字面量编码：候选值的引号与反斜杠都按会话 sql_mode 处理。
    fn enum_set_type_sql(&self, draft: &EnumSetDraft, quoting: LiteralQuoting) -> Result<String, String> {
        table_ddl::enum_set_type(draft, quoting)
    }

    /// 生成列：存储方式关键字按家族写法（MySQL 用 STORED，MariaDB 用 PERSISTENT 并接受 STORED）。
    fn generated_column_sql(&self, draft: &GeneratedColumnDraft) -> Result<String, String> {
        let keyword = match self.engine {
            Engine::Mariadb => "PERSISTENT",
            _ => "STORED",
        };
        table_ddl::generated_clause(draft, keyword)
    }

    fn plan_object_action(&self, snapshot: &Snapshot, action: &Action) -> Option<Plan> {
        object_action(snapshot, action, true)
    }

    fn refuse_object_action(&self, snapshot: &Snapshot, action: &Action) -> Option<Plan> {
        object_action(snapshot, action, false)
    }

    fn admin_sections(&self) -> AdminSections {
        AdminSections { events: true, partitions: true, ..AdminSections::default() }
    }

    fn matches_session(&self, session: &CatalogSession) -> bool {
        matches!(session, CatalogSession::Mysql(_))
    }

    async fn load_snapshot(&self, session: &CatalogSession, request: &Request, table: &str) -> Result<Snapshot, String> {
        let CatalogSession::Mysql(pool) = session else {
            return Err(SESSION_MISMATCH.to_owned());
        };
        ddl_read::load_mysql(pool, self.engine, &request.database, table).await
    }

    async fn name_taken(&self, session: &CatalogSession, snapshot: &Snapshot, name: &str) -> Result<bool, String> {
        let CatalogSession::Mysql(pool) = session else {
            return Err(SESSION_MISMATCH.to_owned());
        };
        ddl_read::name_taken(pool, &snapshot.database, &snapshot.name, name).await
    }

    async fn count(&self, session: &CatalogSession, sql: &str) -> Result<i64, String> {
        let CatalogSession::Mysql(pool) = session else {
            return Err(SESSION_MISMATCH.to_owned());
        };
        ddl_read::count(pool, sql).await
    }

    async fn read_object_state(&self, session: &CatalogSession, request: &Request, snapshot: &mut Snapshot) -> Result<(), String> {
        // 事件、例程与触发器的现状在计划之前读出来：修改与删除都要以读到的实际定义为依据，
        // 读不到时给出原因，不用空值覆盖服务器对象。
        let CatalogSession::Mysql(pool) = session else {
            return Err(SESSION_MISMATCH.to_owned());
        };
        match &request.action {
            Action::SetEventEnabled { event, .. } => {
                snapshot.event_enabled = ddl_read::mysql_event_enabled(pool, &request.database, event).await?;
                if let Ok(state) = object_read::load_event(pool, &request.database, event).await {
                    snapshot.event = Some(state);
                }
            }
            Action::AlterEvent { event } => {
                let name = event.original.clone().unwrap_or_else(|| event.name.clone());
                snapshot.event = Some(object_read::load_event(pool, &request.database, &name).await?);
            }
            Action::DropEvent { event } => {
                snapshot.event = Some(object_read::load_event(pool, &request.database, event).await?);
            }
            Action::ReplaceRoutine { routine } => {
                let name = routine.original.clone().unwrap_or_else(|| routine.name.clone());
                snapshot.routine = Some(object_read::load_routine(pool, &request.database, routine.kind, &name).await?);
            }
            Action::DropRoutine { routine_kind, name } => {
                snapshot.routine = Some(object_read::load_routine(pool, &request.database, *routine_kind, name).await?);
            }
            Action::ReplaceTrigger { trigger } => {
                let name = trigger.original.clone().unwrap_or_else(|| trigger.name.clone());
                snapshot.trigger = Some(object_read::load_trigger(pool, &request.database, &name).await?);
            }
            Action::DropTrigger { trigger } => {
                snapshot.trigger = Some(object_read::load_trigger(pool, &request.database, trigger).await?);
            }
            _ => {}
        }
        Ok(())
    }
}

impl MysqlDdl {
    /// 服务器版本不支持草稿里的专有形态时给出可识别的原因（R1）。
    ///
    /// 能力取自方言档案（版本 + sql_mode），不按"版本号较新"推断。没读到版本时档案是保守的，
    /// 这里只在草稿确实用到该能力时才阻断；普通字段的修改不受影响。
    fn shape_capability_problem(&self, snapshot: &Snapshot, drafts: &[ColumnDraft]) -> Option<String> {
        let profile = MysqlProfile::new(self.engine, &snapshot.server_version, &snapshot.sql_mode);
        let features = profile.features();
        if features.generated_columns && features.enum_set {
            return None;
        }
        let version = profile.version.map(|version| version.short()).unwrap_or_else(|| "未知版本".to_owned());
        let banner = if profile.raw_version.is_empty() { version } else { profile.raw_version.clone() };
        if !features.generated_columns && drafts.iter().any(|draft| draft.generated.is_some()) {
            return Some(format!("服务器 {banner} 不支持生成列，这里不生成语句。"));
        }
        if !features.enum_set && drafts.iter().any(|draft| draft.enum_set.is_some()) {
            return Some(format!("服务器 {banner} 不支持 ENUM/SET 字段类型，这里不生成语句。"));
        }
        None
    }
}

/// MySQL 字符串字面量：单引号翻倍，反斜杠也要翻倍（默认 sql_mode 下反斜杠是转义符）。
pub(in crate::adapters) fn mysql_text(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "''"))
}

/// MySQL 家族的库级动作。`supported` 为假时是别的引擎请求了它：给出同一个动作的拒绝计划。
fn object_action(snapshot: &Snapshot, action: &Action, supported: bool) -> Option<Plan> {
    match action {
        Action::SetEventEnabled { event, enabled } => Some(plan_set_event_enabled(snapshot, event, *enabled, supported)),
        // 表的专有结构继续在 table_ddl 里判定；对象生命周期在 object_ddl 里。
        Action::AlterPartitions { table, partition } => Some(table_ddl::plan_partition(snapshot, table, partition, supported)),
        action @ (Action::CreateEvent { .. }
        | Action::AlterEvent { .. }
        | Action::DropEvent { .. }
        | Action::CreateRoutine { .. }
        | Action::ReplaceRoutine { .. }
        | Action::DropRoutine { .. }
        | Action::CreateTrigger { .. }
        | Action::ReplaceTrigger { .. }
        | Action::DropTrigger { .. }) => object_ddl::plan(snapshot, action, supported),
        _ => None,
    }
}

fn plan_set_event_enabled(snapshot: &Snapshot, event: &str, enabled: bool, supported: bool) -> Plan {
    let target = target_label(snapshot.engine, &snapshot.database, &snapshot.schema, event);
    let mut risks = vec![Risk { level: "info".to_owned(), text: "只改事件的启停状态，不改它的事件体。".to_owned() }];
    if !supported {
        return finish(snapshot.engine, false, target, vec!["只有 MySQL 与 MariaDB 有事件（event）对象，这里不生成语句。".to_owned()], risks, Vec::new());
    }
    let mut blocked = Vec::new();
    if !valid_name(event) {
        blocked.push("事件名无效。".to_owned());
    }
    match snapshot.event_enabled {
        None => blocked.push(format!("没有读到事件 {event}，没有生成语句。")),
        Some(current) if current == enabled => {
            blocked.push(format!("事件 {event} 已经是 {}。", if enabled { "enabled" } else { "disabled" }));
        }
        Some(_) => {}
    }
    if !blocked.is_empty() {
        return finish(snapshot.engine, false, target, blocked, risks, Vec::new());
    }
    risks.push(Risk {
        level: "warn".to_owned(),
        text: "event_scheduler 关闭时事件仍然不会执行；这里不改调度器状态。".to_owned(),
    });
    match qualified_table(snapshot.engine, &snapshot.database, "", event) {
        Ok(name) => {
            let keyword = if enabled { "ENABLE" } else { "DISABLE" };
            let sql = format!("ALTER EVENT {name} {keyword};");
            finish(
                snapshot.engine,
                false,
                target,
                Vec::new(),
                risks,
                vec![Step { label: if enabled { format!("启用事件 {event}") } else { format!("停用事件 {event}") }, sql }],
            )
        }
        Err(message) => finish(snapshot.engine, false, target, vec![message], risks, Vec::new()),
    }
}

/// DEFINER 的值：CURRENT_USER[()] 或 user[@host]（各段可带引号）。
fn skip_definer(tokens: &[Tok], mut index: usize) -> Option<usize> {
    if tokens.get(index).is_some_and(|token| word_is(token, "current_user")) {
        index += 1;
        if tokens.get(index).is_some_and(|token| symbol_is(token, '(')) {
            index += 1;
            if !tokens.get(index).is_some_and(|token| symbol_is(token, ')')) {
                return None;
            }
            index += 1;
        }
        return Some(index);
    }
    if !tokens.get(index).is_some_and(is_definer_atom) {
        return None;
    }
    index += 1;
    if tokens.get(index).is_some_and(|token| symbol_is(token, '@')) {
        index += 1;
        if !tokens.get(index).is_some_and(is_definer_atom) {
            return None;
        }
        index += 1;
    }
    Some(index)
}

fn is_definer_atom(token: &Tok) -> bool {
    matches!(token.kind, TokKind::Word(_) | TokKind::Ident(_) | TokKind::Lit)
}
