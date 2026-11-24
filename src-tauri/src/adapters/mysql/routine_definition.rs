//! MySQL 家族的函数/存储过程定义读取（R6）。
//!
//! 依次尝试三个来源：`SHOW CREATE`（服务器原文）→ `information_schema.ROUTINES` + `PARAMETERS`
//! 重建 → `mysql.proc`（MySQL 5.7 / MariaDB，读不到忽略）。重建的文本第一行写明来源，查看器据此
//! 标注“按元数据重建”。全部读不到时给出具体原因：当前账号、定义者、缺少的权限与 GRANT 示例；
//! 对象不存在（或当前账号对它没有任何权限）时说找不到例程，不笼统地说“没有读到定义”。

use mysql_async::prelude::Queryable;

use crate::catalog::read::definition_failure;
use crate::connections::model::Engine;
use crate::ddl::model::{RoutineKind, RoutineParamState};

use super::ddl_read::mysql_string;
use super::object_read::{routine_params, routine_type_keyword, PARAMETERS_SQL};

/// 重建文本第一行的来源标记。`catalog::definition` 用 [`crate::catalog::definition::REBUILT_SOURCE_TAG`] 识别它。
const SOURCE_ROUTINES: &str = "information_schema.ROUTINES + PARAMETERS (SHOW CREATE returned no definition)";
const SOURCE_PROC: &str = "mysql.proc (SHOW CREATE and information_schema returned no definition)";

/// 例程元数据中重建 CREATE 语句需要的部分。两个回退来源都映射到它，重建只有一份。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct RoutineSource {
    pub kind: RoutineKind,
    pub name: String,
    pub definer: String,
    /// 已渲染好的参数列表（括号内的部分）。
    pub params: String,
    /// 函数的返回类型；存储过程为空。
    pub returns: String,
    pub deterministic: bool,
    /// CONTAINS SQL / NO SQL / READS SQL DATA / MODIFIES SQL DATA。
    pub data_access: String,
    /// DEFINER / INVOKER。
    pub security: String,
    pub comment: String,
    pub body: String,
}

/// ROUTINES 回退读取的列：定义、定义者、返回类型、数据访问、安全类型、注释、确定性。
const ROUTINE_SOURCE_SQL: &str = "SELECT ROUTINE_DEFINITION, DEFINER, DTD_IDENTIFIER, SQL_DATA_ACCESS, SECURITY_TYPE, ROUTINE_COMMENT, IS_DETERMINISTIC FROM information_schema.ROUTINES WHERE ROUTINE_SCHEMA = ? AND ROUTINE_NAME = ? AND ROUTINE_TYPE = ?";

/// MySQL 5.7 与 MariaDB 的 mysql.proc；8.0 起不存在，查询失败直接忽略。
const PROC_SQL: &str = "SELECT param_list, `returns`, body, definer, is_deterministic, sql_data_access, security_type, comment FROM mysql.proc WHERE db = ? AND name = ? AND type = ?";

/// `user@host` → `` `user`@`host` ``（DEFINER 子句写法）。没有 @ 时整体加引号。
fn quote_account(account: &str) -> String {
    match account.rsplit_once('@') {
        Some((user, host)) => format!("{}@{}", quote_ident(user), quote_ident(host)),
        None => quote_ident(account),
    }
}

/// `user@host` → `'user'@'host'`（GRANT 语句写法）。
fn grant_account(account: &str) -> String {
    let literal = |text: &str| format!("'{}'", text.replace('\'', "''"));
    match account.rsplit_once('@') {
        Some((user, host)) => format!("{}@{}", literal(user), literal(host)),
        None => literal(account),
    }
}

fn quote_ident(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

/// 参数行 → 括号内的参数列表。函数参数没有 IN/OUT 模式，存储过程每个参数都写模式。
pub(super) fn render_params(kind: RoutineKind, params: &[RoutineParamState]) -> String {
    params
        .iter()
        .map(|param| {
            let head = match kind {
                RoutineKind::Procedure => format!("{} ", param.mode.keyword()),
                RoutineKind::Function => String::new(),
            };
            format!("{head}{} {}", quote_ident(&param.name), param.type_name)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// 按 SHOW CREATE 的版式重建 CREATE 语句，第一行注释写明来源。
pub(super) fn rebuild_create(source: &RoutineSource, origin: &str) -> String {
    let keyword = routine_type_keyword(source.kind);
    let definer = if source.definer.trim().is_empty() { String::new() } else { format!("DEFINER={} ", quote_account(source.definer.trim())) };
    let mut out = format!("{}{origin}\nCREATE {definer}{keyword} {}({})", crate::catalog::definition::REBUILT_SOURCE_TAG, quote_ident(&source.name), source.params);
    if source.kind == RoutineKind::Function && !source.returns.trim().is_empty() {
        out.push_str(&format!(" RETURNS {}", source.returns.trim()));
    }
    out.push('\n');
    let mut characteristic = |text: String| {
        out.push_str("    ");
        out.push_str(&text);
        out.push('\n');
    };
    if source.deterministic {
        characteristic("DETERMINISTIC".to_owned());
    }
    let access = source.data_access.trim().replace('_', " ").to_ascii_uppercase();
    if !access.is_empty() && access != "CONTAINS SQL" {
        characteristic(access);
    }
    if source.security.trim().eq_ignore_ascii_case("INVOKER") {
        characteristic("SQL SECURITY INVOKER".to_owned());
    }
    if !source.comment.is_empty() {
        characteristic(format!("COMMENT '{}'", source.comment.replace('\\', "\\\\").replace('\'', "''")));
    }
    out.push_str(&source.body);
    out
}

/// 定义被服务器隐藏时的原因：当前账号、定义者、缺少的权限与 GRANT 示例。
/// MySQL 8.0.20+ 有 SHOW_ROUTINE；MariaDB 读 mysql.proc。只读定义查看器与对象编辑器共用这一句。
pub(super) fn permission_error(mariadb: bool, database: &str, name: &str, current_user: &str, definer: &str) -> String {
    let routine = format!("{database}.{name}");
    let user = if current_user.trim().is_empty() { "?" } else { current_user.trim() };
    let grantee = grant_account(user);
    let definer = definer.trim();
    match (mariadb, definer.is_empty()) {
        (false, false) => format!(
            "当前账号 {user} 不是例程 {routine} 的定义者 {definer}，且缺少 SHOW_ROUTINE 权限（MySQL 8.0.20+）或全局 SELECT 权限，服务器不返回定义。可以请管理员执行：GRANT SHOW_ROUTINE ON *.* TO {grantee};"
        ),
        (false, true) => format!(
            "当前账号 {user} 不是例程 {routine} 的定义者，且缺少 SHOW_ROUTINE 权限（MySQL 8.0.20+）或全局 SELECT 权限，服务器不返回定义。可以请管理员执行：GRANT SHOW_ROUTINE ON *.* TO {grantee};"
        ),
        (true, false) => format!(
            "当前账号 {user} 不是例程 {routine} 的定义者 {definer}，且没有 mysql.proc 的 SELECT 权限，服务器不返回定义。可以请管理员执行：GRANT SELECT ON mysql.proc TO {grantee};"
        ),
        (true, true) => format!(
            "当前账号 {user} 不是例程 {routine} 的定义者，且没有 mysql.proc 的 SELECT 权限，服务器不返回定义。可以请管理员执行：GRANT SELECT ON mysql.proc TO {grantee};"
        ),
    }
}

/// 例程不存在，或当前账号对它没有任何权限（此时服务器把它当作不存在）。
pub(super) fn not_found_error(database: &str, name: &str, current_user: &str) -> String {
    let user = if current_user.trim().is_empty() { "?" } else { current_user.trim() };
    format!("找不到例程 {database}.{name}（当前账号 {user} 也可能对它没有任何权限）。")
}

/// 当前账号与服务器是否为 MariaDB。读不到时账号为空、按 MySQL 处理：只影响提示措辞。
pub(super) async fn account_facts(conn: &mut mysql_async::Conn) -> (String, bool) {
    match conn.query_first::<mysql_async::Row, _>("SELECT CURRENT_USER(), VERSION()").await {
        Ok(Some(row)) => (mysql_string(&row, 0), mysql_string(&row, 1).to_ascii_lowercase().contains("mariadb")),
        _ => (String::new(), false),
    }
}

/// 对象编辑器：ROUTINE_DEFINITION 被隐藏（NULL）时的同一权限原因。
pub(super) async fn hidden_definition_error(conn: &mut mysql_async::Conn, database: &str, name: &str, definer: &str) -> String {
    let (user, mariadb) = account_facts(conn).await;
    permission_error(mariadb, database, name, &user, definer)
}

fn value_is_null(row: &mysql_async::Row, index: usize) -> bool {
    matches!(row.as_ref(index), None | Some(mysql_async::Value::NULL))
}

/// SHOW CREATE 的语句列：非空文本才算读到；NULL/空表示被权限隐藏。
fn show_create_text(row: &mysql_async::Row) -> Option<String> {
    if value_is_null(row, 2) {
        return None;
    }
    let text = mysql_string(row, 2);
    (!text.trim().is_empty()).then_some(text)
}

/// 只读定义查看器读函数/存储过程：SHOW CREATE → ROUTINES 重建 → mysql.proc → 具体原因。
pub(super) async fn read_routine_definition(pool: &mysql_async::Pool, engine: Engine, database: &str, name: &str, kind: RoutineKind) -> Result<String, String> {
    let keyword = routine_type_keyword(kind);
    let mut conn = pool.get_conn().await.map_err(definition_failure)?;
    let show_sql = format!("SHOW CREATE {keyword} {}.{}", quote_ident(database), quote_ident(name));
    let (show_row_seen, show_error) = match conn.query_first::<mysql_async::Row, _>(show_sql).await {
        Ok(Some(row)) => match show_create_text(&row) {
            Some(text) => return Ok(text),
            None => (true, None),
        },
        Ok(None) => (false, None),
        Err(err) => (false, Some(err)),
    };
    let args = (database.to_owned(), name.to_owned(), keyword.to_owned());
    let routine: Option<mysql_async::Row> = conn.exec_first(ROUTINE_SOURCE_SQL, args.clone()).await.ok().flatten();
    let mut definer = String::new();
    if let Some(row) = &routine {
        definer = mysql_string(row, 1);
        if !value_is_null(row, 0) && !mysql_string(row, 0).trim().is_empty() {
            let rows: Vec<mysql_async::Row> = conn.exec(PARAMETERS_SQL, args.clone()).await.unwrap_or_default();
            let tuples: Vec<(String, String, String, String, String)> =
                rows.iter().map(|row| (mysql_string(row, 0), mysql_string(row, 1), mysql_string(row, 2), mysql_string(row, 3), mysql_string(row, 4))).collect();
            let (params, returns) = routine_params(&tuples, kind);
            let source = RoutineSource {
                kind,
                name: name.to_owned(),
                definer: definer.clone(),
                params: render_params(kind, &params),
                returns: if returns.is_empty() { mysql_string(row, 2) } else { returns },
                deterministic: mysql_string(row, 6).eq_ignore_ascii_case("YES"),
                data_access: mysql_string(row, 3),
                security: mysql_string(row, 4),
                comment: mysql_string(row, 5),
                body: mysql_string(row, 0),
            };
            return Ok(rebuild_create(&source, SOURCE_ROUTINES));
        }
    }
    if let Ok(Some(row)) = conn.exec_first::<mysql_async::Row, _, _>(PROC_SQL, args).await {
        let body = mysql_string(&row, 2);
        if !body.trim().is_empty() {
            let source = RoutineSource {
                kind,
                name: name.to_owned(),
                definer: mysql_string(&row, 3),
                params: mysql_string(&row, 0).trim().to_owned(),
                returns: mysql_string(&row, 1),
                deterministic: mysql_string(&row, 4).eq_ignore_ascii_case("YES"),
                data_access: mysql_string(&row, 5),
                security: mysql_string(&row, 6),
                comment: mysql_string(&row, 7),
                body,
            };
            return Ok(rebuild_create(&source, SOURCE_PROC));
        }
    }
    let (user, mariadb) = account_facts(&mut conn).await;
    let mariadb = mariadb || engine == Engine::Mariadb;
    if routine.is_some() || show_row_seen {
        return Err(definition_failure(permission_error(mariadb, database, name, &user, &definer)));
    }
    match show_error {
        // 1305 ER_SP_DOES_NOT_EXIST：不存在，或当前账号没有任何权限。
        Some(mysql_async::Error::Server(err)) if err.code != 1305 => Err(definition_failure(err.message)),
        Some(err @ (mysql_async::Error::Driver(_) | mysql_async::Error::Io(_))) => Err(definition_failure(err)),
        _ => Err(definition_failure(not_found_error(database, name, &user))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ddl::model::ParamMode;

    fn param(mode: ParamMode, name: &str, type_name: &str) -> RoutineParamState {
        RoutineParamState { position: 1, mode, name: name.to_owned(), type_name: type_name.to_owned() }
    }

    #[test]
    fn procedure_rebuild_keeps_in_out_params_and_source() {
        let params = [param(ParamMode::In, "seed", "int"), param(ParamMode::Out, "total", "int")];
        let source = RoutineSource {
            kind: RoutineKind::Procedure,
            name: "add_one".into(),
            definer: "app@%".into(),
            params: render_params(RoutineKind::Procedure, &params),
            data_access: "READS SQL DATA".into(),
            security: "INVOKER".into(),
            comment: "it's here".into(),
            body: "SET total = seed + 1".into(),
            ..Default::default()
        };
        let sql = rebuild_create(&source, SOURCE_ROUTINES);
        assert!(sql.starts_with(crate::catalog::definition::REBUILT_SOURCE_TAG), "{sql}");
        assert!(sql.contains("information_schema.ROUTINES"), "{sql}");
        assert!(sql.contains("CREATE DEFINER=`app`@`%` PROCEDURE `add_one`(IN `seed` int, OUT `total` int)\n"), "{sql}");
        assert!(sql.contains("    READS SQL DATA\n"), "{sql}");
        assert!(sql.contains("    SQL SECURITY INVOKER\n"), "{sql}");
        assert!(sql.contains("    COMMENT 'it''s here'\n"), "{sql}");
        assert!(sql.ends_with("SET total = seed + 1"), "{sql}");
        assert!(!sql.contains("RETURNS"));
        assert!(!sql.contains("DETERMINISTIC"));
    }

    #[test]
    fn function_rebuild_has_returns_and_deterministic_without_param_modes() {
        let rows = vec![
            ("FUNCTION".to_owned(), "0".to_owned(), String::new(), String::new(), "int".to_owned()),
            ("FUNCTION".to_owned(), "1".to_owned(), String::new(), "seed".to_owned(), "int".to_owned()),
        ];
        let (params, returns) = routine_params(&rows, RoutineKind::Function);
        let source = RoutineSource {
            kind: RoutineKind::Function,
            name: "twice".into(),
            definer: "root@localhost".into(),
            params: render_params(RoutineKind::Function, &params),
            returns,
            deterministic: true,
            data_access: "NO_SQL".into(),
            security: "DEFINER".into(),
            body: "RETURN seed * 2".into(),
            ..Default::default()
        };
        let sql = rebuild_create(&source, SOURCE_PROC);
        assert!(sql.contains("mysql.proc"), "{sql}");
        assert!(sql.contains("CREATE DEFINER=`root`@`localhost` FUNCTION `twice`(`seed` int) RETURNS int\n"), "{sql}");
        assert!(sql.contains("    DETERMINISTIC\n    NO SQL\n"), "{sql}");
        assert!(!sql.contains("IN `seed`"));
        assert!(!sql.contains("SQL SECURITY"));
        assert!(sql.ends_with("RETURN seed * 2"));
    }

    #[test]
    fn permission_error_names_account_definer_privilege_and_grant() {
        let mysql = permission_error(false, "shop", "add_one", "viewer@%", "app@%");
        assert!(mysql.contains("viewer@%"), "{mysql}");
        assert!(mysql.contains("app@%"), "{mysql}");
        assert!(mysql.contains("SHOW_ROUTINE"), "{mysql}");
        assert!(mysql.contains("全局 SELECT"), "{mysql}");
        assert!(mysql.contains("GRANT SHOW_ROUTINE ON *.* TO 'viewer'@'%';"), "{mysql}");
        assert!(!mysql.contains("没有读到定义"));

        let maria = permission_error(true, "shop", "add_one", "o'neil@localhost", "");
        assert!(maria.contains("mysql.proc"), "{maria}");
        assert!(maria.contains("GRANT SELECT ON mysql.proc TO 'o''neil'@'localhost';"), "{maria}");

        let missing = not_found_error("shop", "gone", "viewer@%");
        assert!(missing.contains("找不到例程 shop.gone"), "{missing}");
    }

    #[test]
    fn permission_messages_have_english_forms() {
        for text in [
            permission_error(false, "shop", "p", "viewer@%", "app@%"),
            permission_error(false, "shop", "p", "viewer@%", ""),
            permission_error(true, "shop", "p", "viewer@%", "app@%"),
            permission_error(true, "shop", "p", "viewer@%", ""),
            not_found_error("shop", "p", "viewer@%"),
            definition_failure(permission_error(false, "shop", "p", "viewer@%", "app@%")),
        ] {
            let english = crate::catalog::definition::error_text(&text, crate::i18n::Locale::En);
            assert!(!crate::ddl::english::has_cjk(&english), "{text} -> {english}");
            assert!(english.contains("viewer@%"), "{english}");
        }
    }
}

/// 真实服务器验收：`docker compose -f docker/compose.yaml up -d mysql` 后运行
/// `cargo test --manifest-path src-tauri/Cargo.toml --locked -- --ignored routine_definition_live`。
/// 端口取 RILLSQL_ROUTINE_MYSQL_PORT（默认 3306，MariaDB 夹具用 3307）。夹具由用例以 root 幂等补齐。
#[cfg(test)]
mod live_tests {
    use super::*;

    const SEED: &str = include_str!("../../../../docker/init/mysql-routines.sql");

    fn pool(user: &str, password: &str) -> mysql_async::Pool {
        let port = std::env::var("RILLSQL_ROUTINE_MYSQL_PORT").ok().and_then(|port| port.parse().ok()).unwrap_or(3306);
        mysql_async::Pool::new(
            mysql_async::OptsBuilder::default()
                .ip_or_hostname("127.0.0.1")
                .tcp_port(port)
                .user(Some(user))
                .pass(Some(password))
                .db_name(Some("rillsql")),
        )
    }

    async fn seed() {
        let root = pool("root", "root");
        let mut conn = root.get_conn().await.expect("root 连接");
        let script: String = SEED.lines().filter(|line| !line.trim_start().starts_with("--")).collect::<Vec<_>>().join("\n");
        for statement in script.split(';').map(str::trim).filter(|statement| !statement.is_empty()) {
            conn.query_drop(statement).await.unwrap_or_else(|err| panic!("{statement}: {err}"));
        }
        drop(conn);
        root.disconnect().await.expect("断开");
    }

    #[tokio::test]
    #[ignore = "需要 docker/compose.yaml 的 MySQL（或 RILLSQL_ROUTINE_MYSQL_PORT 指向的 MariaDB）"]
    async fn routine_definition_live() {
        seed().await;
        let engine = Engine::Mysql;

        // 定义者：SHOW CREATE 原文。
        let owner = pool("rillsql", "rillsql");
        let proc_sql = read_routine_definition(&owner, engine, "rillsql", "rillsql_routine_proc", RoutineKind::Procedure).await.expect("定义者读过程");
        assert!(proc_sql.contains("PROCEDURE `rillsql_routine_proc`"), "{proc_sql}");
        assert!(proc_sql.contains("IN seed") && proc_sql.contains("OUT total"), "{proc_sql}");
        let func_sql = read_routine_definition(&owner, engine, "rillsql", "rillsql_routine_func", RoutineKind::Function).await.expect("定义者读函数");
        assert!(func_sql.contains("FUNCTION `rillsql_routine_func`") && func_sql.contains("RETURNS int") && func_sql.contains("DETERMINISTIC"), "{func_sql}");
        let missing = read_routine_definition(&owner, engine, "rillsql", "rillsql_no_such_routine", RoutineKind::Procedure).await.expect_err("不存在");
        assert!(missing.contains("找不到例程 rillsql.rillsql_no_such_routine"), "{missing}");
        owner.disconnect().await.expect("断开");

        // 非定义者、无 SHOW_ROUTINE：得到回退结果或带权限说明的错误，不是“没有读到定义”。
        let viewer = pool("rillsql_viewer", "rillsql");
        for (name, kind) in [("rillsql_routine_proc", RoutineKind::Procedure), ("rillsql_routine_func", RoutineKind::Function)] {
            match read_routine_definition(&viewer, engine, "rillsql", name, kind).await {
                Ok(text) => assert!(text.starts_with(crate::catalog::definition::REBUILT_SOURCE_TAG) || text.contains(name), "{text}"),
                Err(message) => {
                    eprintln!("{name}: {message}");
                    assert!(!message.contains("没有读到定义"), "{message}");
                    assert!(message.contains("rillsql_viewer@"), "当前账号：{message}");
                    assert!(message.contains("rillsql@%"), "定义者：{message}");
                    assert!(message.contains("SHOW_ROUTINE") || message.contains("mysql.proc"), "所需权限：{message}");
                    assert!(message.contains("GRANT "), "GRANT 建议：{message}");
                }
            }
        }
        // 对象编辑器：定义被隐藏时返回同一权限错误，不是空主体。
        let editor = super::super::object_read::load_routine(&viewer, "rillsql", RoutineKind::Procedure, "rillsql_routine_proc").await;
        match editor {
            Ok(state) => assert!(!state.body.trim().is_empty(), "编辑器不能拿到空主体：{state:?}"),
            Err(message) => {
                eprintln!("load_routine: {message}");
                assert!(message.contains("rillsql_viewer@") && message.contains("rillsql@%") && message.contains("GRANT "), "{message}");
            }
        }
        viewer.disconnect().await.expect("断开");
    }
}
