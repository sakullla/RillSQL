//! MySQL 家族（MySQL / MariaDB）的 DDL 方言：按库限定、CHANGE COLUMN 改字段、AUTO_INCREMENT、InnoDB 表选项、
//! RENAME TABLE，以及事件启停的库级动作。MariaDB 复用同一实现，按 `engine` 区分。

use crate::adapters::ddl::{AdminSections, DdlDialect, ViewModifier};
use crate::adapters::quote_ident;
use crate::connections::model::Engine;
use crate::connections::CatalogSession;
use crate::ddl::introspect::SESSION_MISMATCH;
use crate::ddl::model::{Action, ColumnDraft, Plan, Request, Risk, Snapshot, Step};
use crate::ddl::plan::{column_sql, finish, is_word, statement_alter_plan, symbol_is, target_label, word_is, Change, Tok, TokKind};
use crate::ddl::sql::{qualified_table, valid_name};

use super::ddl_read;

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

    fn create_table_suffix(&self, _without_rowid: bool, _strict: bool) -> String {
        " ENGINE=InnoDB DEFAULT CHARSET=utf8mb4".to_owned()
    }

    fn auto_clause_after_null(&self, draft: &ColumnDraft) -> &'static str {
        if draft.auto {
            " AUTO_INCREMENT"
        } else {
            ""
        }
    }

    fn alter_plan(&self, snapshot: &Snapshot, table: &str, _drafts: &[ColumnDraft], changes: &[Change], target: String, risks: Vec<Risk>) -> Plan {
        let engine = self.engine;
        statement_alter_plan(snapshot, table, changes, target, risks, "ADD COLUMN", &|qualified, original, draft| {
            // CHANGE COLUMN 按完整的新定义一次改名、改类型、改可空与默认值。
            let definition = column_sql(engine, draft, false)?;
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
        let Action::SetEventEnabled { event, .. } = &request.action else {
            return Ok(());
        };
        let CatalogSession::Mysql(pool) = session else {
            return Err(SESSION_MISMATCH.to_owned());
        };
        snapshot.event_enabled = ddl_read::mysql_event_enabled(pool, &request.database, event).await?;
        Ok(())
    }
}

/// MySQL 家族的库级动作。`supported` 为假时是别的引擎请求了它：给出同一个动作的拒绝计划。
fn object_action(snapshot: &Snapshot, action: &Action, supported: bool) -> Option<Plan> {
    match action {
        Action::SetEventEnabled { event, enabled } => Some(plan_set_event_enabled(snapshot, event, *enabled, supported)),
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
