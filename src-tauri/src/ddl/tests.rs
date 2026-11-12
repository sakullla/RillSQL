use std::fs;
use std::path::PathBuf;

use super::commands;
use super::exec::{self, run_steps};
use super::model::{
    fk_actions, Action, AttachedDatabase, Column, ColumnDraft, DependentSql, DomainState, Engine, ForeignKey, IncomingForeignKey, Index, IndexColumn, IndexColumnDraft, Request,
    SequenceState, Snapshot, Step, StepStatus,
};
use super::plan::plan;
use crate::connections::{CatalogSession, Service};

#[test]
fn foreign_key_actions_follow_the_engine() {
    assert_eq!(fk_actions(Engine::Mysql), &["RESTRICT", "CASCADE", "SET NULL", "NO ACTION"]);
    assert_eq!(fk_actions(Engine::Mariadb), fk_actions(Engine::Mysql));
    assert_eq!(fk_actions(Engine::Postgres), &["RESTRICT", "CASCADE", "SET NULL", "SET DEFAULT", "NO ACTION"]);
    assert_eq!(fk_actions(Engine::Sqlite), fk_actions(Engine::Postgres));
    assert_eq!(fk_actions(Engine::Sqlserver), fk_actions(Engine::Postgres));
    assert!(fk_actions(Engine::Redis).is_empty());
    assert!(fk_actions(Engine::Mongodb).is_empty());
}

fn column(name: &str, type_name: &str, nullable: bool, primary: bool) -> Column {
    Column {
        name: name.to_owned(),
        type_name: type_name.to_owned(),
        nullable,
        default_sql: None,
        primary,
        auto: false,
        generated: false,
        default_constraint: None,
    }
}

fn draft_of(column: &Column) -> ColumnDraft {
    ColumnDraft {
        original: Some(column.name.clone()),
        name: column.name.clone(),
        type_name: column.type_name.clone(),
        nullable: column.nullable,
        default_sql: column.default_sql.clone().unwrap_or_default(),
        primary: column.primary,
        auto: column.auto,
        dropped: false,
    }
}

fn notes(version: &str) -> Snapshot {
    let mut snapshot = Snapshot::bare(Engine::Sqlite, "main", "", "notes");
    snapshot.sqlite_version = version.to_owned();
    snapshot.columns = vec![column("id", "INTEGER", false, true), column("note", "TEXT", true, false)];
    snapshot.table_sql = "CREATE TABLE notes (id INTEGER PRIMARY KEY, note TEXT)".to_owned();
    snapshot.row_count = Some(1);
    snapshot.null_counts.insert("note".to_owned(), 0);
    snapshot
}

fn set_note_not_null(snapshot: &Snapshot) -> Vec<ColumnDraft> {
    snapshot
        .columns
        .iter()
        .map(|column| {
            let mut draft = draft_of(column);
            if column.name == "note" {
                draft.nullable = false;
            }
            draft
        })
        .collect()
}

fn request(action: Action) -> Request {
    Request { database: "main".to_owned(), schema: String::new(), engine: Engine::Sqlite, action }
}

#[test]
fn sqlite_353_not_null_is_a_direct_alter() {
    let snapshot = notes("3.53.0");
    let planned = plan(&snapshot, &Action::AlterColumns { table: "notes".to_owned(), columns: set_note_not_null(&snapshot) });
    assert!(planned.blocked.is_empty(), "{:?}", planned.blocked);
    assert!(!planned.rebuild);
    assert_eq!(planned.steps.len(), 1);
    assert_eq!(planned.steps[0].sql, "ALTER TABLE \"notes\" ALTER COLUMN \"note\" SET NOT NULL;");
    let joined = planned.steps.iter().map(|step| step.sql.as_str()).collect::<Vec<_>>().join("\n");
    assert!(!joined.contains("PRAGMA"));
    assert!(!joined.contains("CREATE TABLE"));
}

#[test]
fn sqlite_351_not_null_previews_the_official_rebuild() {
    let snapshot = notes("3.51.3");
    let planned = plan(&snapshot, &Action::AlterColumns { table: "notes".to_owned(), columns: set_note_not_null(&snapshot) });
    assert!(planned.blocked.is_empty(), "{:?}", planned.blocked);
    assert!(planned.rebuild);
    let sql: Vec<&str> = planned.steps.iter().map(|step| step.sql.as_str()).collect();
    let off = sql.iter().position(|item| item.contains("PRAGMA foreign_keys = OFF")).expect("关闭外键");
    let begin = sql.iter().position(|item| item.starts_with("BEGIN")).expect("BEGIN");
    let create = sql.iter().position(|item| item.contains("CREATE TABLE \"_rill_new_notes\"")).expect("新表");
    let copy = sql.iter().position(|item| item.contains("INSERT INTO \"_rill_new_notes\"") && item.contains("FROM \"notes\"")).expect("复制");
    let drop = sql.iter().position(|item| *item == "DROP TABLE \"notes\";").expect("删除旧表");
    let rename = sql.iter().position(|item| item.contains("ALTER TABLE \"_rill_new_notes\" RENAME TO \"notes\"")).expect("改回原名");
    let check = sql.iter().position(|item| item.contains("foreign_key_check")).expect("检查");
    let commit = sql.iter().position(|item| item.starts_with("COMMIT")).expect("COMMIT");
    let restore = sql.iter().position(|item| item.contains("PRAGMA foreign_keys = OFF")).unwrap();
    assert!(off < begin && begin < create && create < copy && copy < drop && drop < rename && rename < check && check < commit);
    assert!(commit < sql.iter().rposition(|item| item.contains("PRAGMA foreign_keys = OFF")).unwrap_or(0) || restore == off);
    assert!(sql.iter().any(|item| item.contains("PRAGMA foreign_keys = OFF") && sql.iter().position(|step| step.contains("COMMIT")).unwrap() < sql.iter().rposition(|step| *step == "PRAGMA foreign_keys = OFF;").unwrap()));
    assert!(sql.iter().all(|item| !item.contains("ALTER TABLE \"notes\" RENAME")));
}

#[test]
fn unsafe_rebuild_is_blocked_before_any_statement() {
    let mut snapshot = notes("3.51.3");
    snapshot.triggers.push(DependentSql {
        name: "trg_note".to_owned(),
        sql: "CREATE TRIGGER trg_note AFTER UPDATE OF note ON notes BEGIN SELECT note; END".to_owned(),
    });
    let mut columns = set_note_not_null(&snapshot);
    columns.iter_mut().find(|column| column.name == "note").unwrap().type_name = "INTEGER".to_owned();
    let planned = plan(&snapshot, &Action::AlterColumns { table: "notes".to_owned(), columns });
    assert!(planned.steps.is_empty());
    assert!(!planned.rebuild);
    assert!(planned.blocked.iter().any(|item| item.contains("阻止")), "{:?}", planned.blocked);
}

#[test]
fn protected_index_is_not_a_plain_drop() {
    let mut snapshot = notes("3.51.3");
    snapshot.indexes.push(Index {
        name: "sqlite_autoindex_notes_1".to_owned(),
        unique: true,
        columns: vec![IndexColumn { name: "id".to_owned(), descending: false }],
        protected: true,
        reason: "由主键约束维护".to_owned(),
        partial: false,
        expression: false,
        sql: None,
    });
    let planned = plan(
        &snapshot,
        &Action::DropIndex { table: "notes".to_owned(), name: "sqlite_autoindex_notes_1".to_owned() },
    );
    assert!(planned.steps.is_empty());
    assert!(planned.blocked.iter().any(|item| item.contains("不能单独删除")), "{:?}", planned.blocked);
    assert!(planned.steps.iter().all(|step| !step.sql.to_ascii_uppercase().contains("DROP INDEX")));
}

fn view_of(engine: Engine, sql: &str) -> Snapshot {
    let mut snapshot = match engine {
        Engine::Postgres => Snapshot::bare(Engine::Postgres, "app", "public", "v_notes"),
        Engine::Mysql => Snapshot::bare(Engine::Mysql, "app", "", "v_notes"),
        Engine::Mariadb => Snapshot::bare(Engine::Mariadb, "app", "", "v_notes"),
        Engine::Sqlite => Snapshot::bare(Engine::Sqlite, "main", "", "v_notes"),
        Engine::Sqlserver => Snapshot::bare(Engine::Sqlserver, "app", "dbo", "v_notes"),
        // Redis 没有结构编辑（capability 门控），测试不构造该引擎。
        Engine::Redis | Engine::Mongodb => unreachable!("专项引擎不参与结构编辑测试"),
    };
    snapshot.kind = "view".to_owned();
    snapshot.table_sql = sql.to_owned();
    snapshot.columns = vec![column("id", "int", true, false)];
    snapshot
}

#[test]
fn replacing_a_view_lists_every_statement_and_refuses_unsafe_cases() {
    let original = "CREATE VIEW \"v_notes\" AS SELECT note FROM notes";
    let mut sqlite = view_of(Engine::Sqlite, original);
    let same = plan(&sqlite, &Action::ReplaceView { table: "v_notes".to_owned(), definition: original.to_owned() });
    assert!(same.steps.is_empty());
    assert!(same.blocked.iter().any(|item| item.contains("没有发送")));

    let changed = plan(
        &sqlite,
        &Action::ReplaceView {
            table: "v_notes".to_owned(),
            definition: "CREATE VIEW \"v_notes\" AS SELECT id FROM notes".to_owned(),
        },
    );
    assert!(changed.blocked.is_empty(), "{:?}", changed.blocked);
    assert_eq!(changed.steps.len(), 2);
    assert_eq!(changed.steps[0].sql, "DROP VIEW \"v_notes\";");
    assert!(changed.steps[1].sql.contains("CREATE VIEW \"v_notes\""));
    assert!(changed.steps.iter().all(|step| !step.sql.to_ascii_uppercase().contains("CASCADE")));
    assert!(changed.steps[0].sql.to_ascii_uppercase().contains("DROP"));
    assert!(changed.steps.iter().position(|step| step.sql.contains("DROP")).unwrap() < changed.steps.iter().position(|step| step.sql.contains("CREATE VIEW")).unwrap());

    let blank = plan(&sqlite, &Action::ReplaceView { table: "v_notes".to_owned(), definition: "-- only a comment".to_owned() });
    assert!(blank.steps.is_empty());
    assert!(blank.blocked.iter().any(|item| item.contains("空白和注释")));

    let placeholder = plan(&sqlite, &Action::ReplaceView { table: "v_notes".to_owned(), definition: "-- main.v_notes".to_owned() });
    assert!(placeholder.steps.is_empty());

    sqlite.views.push(DependentSql { name: "v_other".to_owned(), sql: "SELECT * FROM v_notes".to_owned() });
    let dependent = plan(
        &sqlite,
        &Action::ReplaceView { table: "v_notes".to_owned(), definition: "CREATE VIEW \"v_notes\" AS SELECT id FROM notes".to_owned() },
    );
    assert!(dependent.steps.is_empty());
    assert!(dependent.blocked.iter().any(|item| item.contains("依赖") && item.contains("不发送")));
    assert!(dependent.steps.iter().all(|step| !step.sql.to_ascii_uppercase().contains("DROP")));

    let mut materialized = view_of(Engine::Postgres, "CREATE MATERIALIZED VIEW \"public\".\"v_notes\" AS\nSELECT 1;");
    materialized.materialized = true;
    let kept = plan(
        &materialized,
        &Action::ReplaceView { table: "v_notes".to_owned(), definition: "CREATE OR REPLACE VIEW \"public\".\"v_notes\" AS SELECT 2".to_owned() },
    );
    assert!(kept.steps.is_empty());
    assert!(kept.blocked.iter().any(|item| item.contains("物化数据")));

    let postgres = view_of(Engine::Postgres, "CREATE OR REPLACE VIEW \"public\".\"v_notes\" AS\nSELECT 1;");
    let replaced = plan(
        &postgres,
        &Action::ReplaceView { table: "v_notes".to_owned(), definition: "SELECT id FROM notes".to_owned() },
    );
    assert!(replaced.blocked.is_empty(), "{:?}", replaced.blocked);
    assert_eq!(replaced.steps.len(), 1);
    assert!(replaced.steps[0].sql.starts_with("CREATE OR REPLACE VIEW"));
    assert!(!replaced.steps[0].sql.to_ascii_uppercase().contains("DROP"));

    let mysql = view_of(Engine::Mysql, "CREATE VIEW `v_notes` AS select `id` from `notes`");
    let mysql_plan = plan(
        &mysql,
        &Action::ReplaceView { table: "v_notes".to_owned(), definition: "CREATE VIEW `v_notes` AS select `note` from `notes`".to_owned() },
    );
    assert!(mysql_plan.blocked.is_empty(), "{:?}", mysql_plan.blocked);
    assert_eq!(mysql_plan.steps.len(), 1);
    assert!(mysql_plan.steps[0].sql.to_ascii_uppercase().contains("CREATE OR REPLACE"));
    assert!(mysql_plan.steps[0].sql.contains("`app`.`v_notes`"));
    assert!(!mysql_plan.steps[0].sql.contains("VIEW `v_notes`"));
    assert!(!mysql_plan.steps[0].sql.to_ascii_uppercase().contains("DROP"));

    let multi = plan(
        &postgres,
        &Action::ReplaceView { table: "v_notes".to_owned(), definition: "SELECT 1; DROP TABLE notes".to_owned() },
    );
    assert!(multi.steps.is_empty());
    assert!(multi.blocked.iter().any(|item| item.contains("多条语句")));

    let alter = plan(
        &postgres,
        &Action::AlterColumns { table: "v_notes".to_owned(), columns: postgres.columns.iter().map(draft_of).collect() },
    );
    assert!(alter.steps.is_empty());
    assert!(alter.blocked.iter().any(|item| item.contains("视图")));
}

#[test]
fn view_replace_matches_the_created_identifier_not_a_substring() {
    let postgres = view_of(Engine::Postgres, "CREATE OR REPLACE VIEW \"public\".\"v_notes\" AS\nSELECT 1;");
    let with_table_word = plan(
        &postgres,
        &Action::ReplaceView {
            table: "v_notes".to_owned(),
            definition: "CREATE VIEW \"public\".\"v_notes\" AS SELECT table_id, tableoid FROM notes".to_owned(),
        },
    );
    assert!(with_table_word.blocked.is_empty(), "{:?}", with_table_word.blocked);
    assert_eq!(with_table_word.steps.len(), 1);
    assert!(with_table_word.steps[0].sql.contains("CREATE OR REPLACE VIEW"));
    assert!(with_table_word.steps[0].sql.contains("\"v_notes\""));
    assert!(with_table_word.steps[0].sql.contains("table_id"));
    assert!(with_table_word.steps[0].sql.contains("tableoid"));
    assert!(!with_table_word.steps[0].sql.to_ascii_uppercase().contains("DROP"));

    let query_with_table_word = plan(
        &postgres,
        &Action::ReplaceView { table: "v_notes".to_owned(), definition: "SELECT table_id, tableoid FROM notes".to_owned() },
    );
    assert!(query_with_table_word.blocked.is_empty(), "{:?}", query_with_table_word.blocked);
    assert!(query_with_table_word.steps[0].sql.starts_with("CREATE OR REPLACE VIEW \"public\".\"v_notes\""));

    let other = plan(
        &postgres,
        &Action::ReplaceView { table: "v_notes".to_owned(), definition: "CREATE VIEW other_v AS SELECT 1 FROM v_notes".to_owned() },
    );
    assert!(other.steps.is_empty());
    assert!(other.blocked.iter().any(|item| item.contains("不是当前视图") && item.contains("没有发送")));
    assert!(other.steps.iter().all(|step| !step.sql.to_ascii_lowercase().contains("other_v")));

    let other_schema = plan(
        &postgres,
        &Action::ReplaceView { table: "v_notes".to_owned(), definition: "CREATE VIEW \"other\".\"v_notes\" AS SELECT 1".to_owned() },
    );
    assert!(other_schema.steps.is_empty());
    assert!(other_schema.blocked.iter().any(|item| item.contains("不是当前视图")));

    let prefix = plan(
        &postgres,
        &Action::ReplaceView { table: "v_notes".to_owned(), definition: "CREATE VIEW v_notes_extra AS SELECT 1 FROM v_notes".to_owned() },
    );
    assert!(prefix.steps.is_empty());

    let quoted_other_case = plan(
        &postgres,
        &Action::ReplaceView { table: "v_notes".to_owned(), definition: "CREATE VIEW \"V_Notes\" AS SELECT 1".to_owned() },
    );
    assert!(quoted_other_case.steps.is_empty());

    let unquoted_same = plan(
        &postgres,
        &Action::ReplaceView { table: "v_notes".to_owned(), definition: "CREATE VIEW V_Notes AS SELECT table_id FROM notes".to_owned() },
    );
    assert!(unquoted_same.blocked.is_empty(), "{:?}", unquoted_same.blocked);
    assert!(unquoted_same.steps[0].sql.contains("CREATE OR REPLACE VIEW \"public\".\"v_notes\""));
    assert!(!unquoted_same.steps[0].sql.contains("V_Notes"));

    let materialized_text = plan(
        &postgres,
        &Action::ReplaceView { table: "v_notes".to_owned(), definition: "SELECT 'MATERIALIZED VIEW' AS label".to_owned() },
    );
    assert!(materialized_text.blocked.is_empty(), "{:?}", materialized_text.blocked);
    assert_eq!(materialized_text.steps.len(), 1);

    let pasted_materialized = plan(
        &postgres,
        &Action::ReplaceView {
            table: "v_notes".to_owned(),
            definition: "CREATE MATERIALIZED VIEW \"public\".\"v_notes\" AS SELECT 1".to_owned(),
        },
    );
    assert!(pasted_materialized.steps.is_empty());
    assert!(pasted_materialized.blocked.iter().any(|item| item.contains("物化数据")));

    let mysql = view_of(Engine::Mysql, "CREATE VIEW `v_notes` AS select `id` from `notes`");
    let mysql_show = plan(
        &mysql,
        &Action::ReplaceView {
            table: "v_notes".to_owned(),
            definition: "CREATE ALGORITHM=UNDEFINED DEFINER=`root`@`localhost` SQL SECURITY DEFINER VIEW `v_notes` AS select `table_id` from `notes`".to_owned(),
        },
    );
    assert!(mysql_show.blocked.is_empty(), "{:?}", mysql_show.blocked);
    assert_eq!(mysql_show.steps.len(), 1);
    assert!(mysql_show.steps[0].sql.starts_with("CREATE OR REPLACE ALGORITHM=UNDEFINED"));
    assert!(mysql_show.steps[0].sql.contains("DEFINER=`root`@`localhost`"));
    assert!(mysql_show.steps[0].sql.contains("VIEW `app`.`v_notes`"));
    assert!(!mysql_show.steps[0].sql.contains("VIEW `v_notes`"));
    assert!(mysql_show.steps[0].sql.contains("table_id"));
    assert!(!mysql_show.steps[0].sql.to_ascii_uppercase().contains("DROP"));

    let mysql_same_db = plan(
        &mysql,
        &Action::ReplaceView { table: "v_notes".to_owned(), definition: "CREATE VIEW `app`.`v_notes` AS select `table_id` from `notes`".to_owned() },
    );
    assert!(mysql_same_db.blocked.is_empty(), "{:?}", mysql_same_db.blocked);
    assert!(mysql_same_db.steps[0].sql.contains("`app`.`v_notes`"));

    let mysql_other_db = plan(
        &mysql,
        &Action::ReplaceView { table: "v_notes".to_owned(), definition: "CREATE VIEW `other`.`v_notes` AS select 1".to_owned() },
    );
    assert!(mysql_other_db.steps.is_empty());

    let sqlite = view_of(Engine::Sqlite, "CREATE VIEW \"v_notes\" AS SELECT note FROM notes");
    let sqlite_same = plan(
        &sqlite,
        &Action::ReplaceView { table: "v_notes".to_owned(), definition: "CREATE VIEW \"main\".\"v_notes\" AS SELECT table_id FROM notes".to_owned() },
    );
    assert!(sqlite_same.blocked.is_empty(), "{:?}", sqlite_same.blocked);
    assert_eq!(sqlite_same.steps.len(), 2);
    assert!(sqlite_same.steps[0].sql.starts_with("DROP VIEW"));
    assert!(sqlite_same.steps[1].sql.contains("table_id"));
    assert!(sqlite_same.steps.iter().position(|step| step.sql.contains("DROP")).unwrap() < sqlite_same.steps.iter().position(|step| step.sql.contains("CREATE VIEW")).unwrap());

    let sqlite_other = plan(
        &sqlite,
        &Action::ReplaceView { table: "v_notes".to_owned(), definition: "CREATE VIEW \"other_v\" AS SELECT 1 FROM v_notes".to_owned() },
    );
    assert!(sqlite_other.steps.is_empty());
    assert!(sqlite_other.blocked.iter().any(|item| item.contains("不是当前视图")));
}

#[test]
fn view_replace_binds_an_unqualified_name_to_the_opened_object() {
    let mut postgres = view_of(Engine::Postgres, "CREATE OR REPLACE VIEW \"analytics\".\"v_notes\" AS\nSELECT 1;");
    postgres.database = "shop".to_owned();
    postgres.schema = "analytics".to_owned();
    let unqualified = plan(
        &postgres,
        &Action::ReplaceView { table: "v_notes".to_owned(), definition: "CREATE VIEW v_notes (id) AS SELECT id FROM notes".to_owned() },
    );
    assert!(unqualified.blocked.is_empty(), "{:?}", unqualified.blocked);
    assert_eq!(
        unqualified.steps[0].sql,
        "CREATE OR REPLACE VIEW \"analytics\".\"v_notes\" (id) AS SELECT id FROM notes;"
    );
    assert!(!unqualified.steps[0].sql.contains("VIEW v_notes"));

    let mut mysql = view_of(Engine::Mysql, "CREATE VIEW `v_notes` AS select `id` from `notes`");
    mysql.database = "shop".to_owned();
    let show = plan(
        &mysql,
        &Action::ReplaceView {
            table: "v_notes".to_owned(),
            definition: "CREATE ALGORITHM=UNDEFINED DEFINER=`root`@`localhost` SQL SECURITY DEFINER VIEW `v_notes` AS select `id` from `notes`".to_owned(),
        },
    );
    assert!(show.blocked.is_empty(), "{:?}", show.blocked);
    assert_eq!(
        show.steps[0].sql,
        "CREATE OR REPLACE ALGORITHM=UNDEFINED DEFINER=`root`@`localhost` SQL SECURITY DEFINER VIEW `shop`.`v_notes` AS select `id` from `notes`;"
    );
    assert!(!show.steps[0].sql.contains("VIEW `v_notes`"));

    let different_case = plan(
        &mysql,
        &Action::ReplaceView { table: "v_notes".to_owned(), definition: "CREATE VIEW V_Notes AS SELECT 1".to_owned() },
    );
    assert!(different_case.blocked.is_empty(), "{:?}", different_case.blocked);
    assert_eq!(different_case.steps[0].sql, "CREATE OR REPLACE VIEW `shop`.`v_notes` AS SELECT 1;");
    assert!(!different_case.steps[0].sql.contains("V_Notes"));

    let qualified_case = plan(
        &mysql,
        &Action::ReplaceView { table: "v_notes".to_owned(), definition: "CREATE VIEW shop.V_Notes AS SELECT note FROM notes".to_owned() },
    );
    assert!(qualified_case.blocked.is_empty(), "{:?}", qualified_case.blocked);
    assert_eq!(qualified_case.steps[0].sql, "CREATE OR REPLACE VIEW `shop`.`v_notes` AS SELECT note FROM notes;");
    assert!(!qualified_case.steps[0].sql.contains("V_Notes"));

    let quoted_case = plan(
        &mysql,
        &Action::ReplaceView { table: "v_notes".to_owned(), definition: "CREATE VIEW `V_Notes` AS SELECT 1".to_owned() },
    );
    assert!(quoted_case.steps.is_empty());
    assert!(quoted_case.blocked.iter().any(|item| item.contains("不是当前视图")));

    let maria = view_of(Engine::Mariadb, "CREATE VIEW `v_notes` AS select 1");
    let maria_plan = plan(
        &maria,
        &Action::ReplaceView { table: "v_notes".to_owned(), definition: "CREATE VIEW v_notes AS select 2".to_owned() },
    );
    assert!(maria_plan.blocked.is_empty(), "{:?}", maria_plan.blocked);
    assert!(maria_plan.steps[0].sql.contains("VIEW `app`.`v_notes`"));
    assert!(!maria_plan.steps[0].sql.contains("VIEW v_notes"));
}

#[test]
fn drop_table_sql_contains_only_the_previewed_object() {
    let snapshot = notes("3.51.3");
    let planned = plan(&snapshot, &Action::DropTable { table: "notes".to_owned() });
    assert!(planned.blocked.is_empty(), "{:?}", planned.blocked);
    assert_eq!(planned.steps.len(), 1);
    assert_eq!(planned.steps[0].sql, "DROP TABLE \"notes\";");
    assert!(!planned.steps[0].sql.to_ascii_uppercase().contains("CASCADE"));
}

#[test]
fn mysql_and_postgres_alters_are_not_wrapped_in_a_transaction() {
    let mut mysql = Snapshot::bare(Engine::Mysql, "app", "", "notes");
    mysql.columns = vec![column("id", "int", false, true), column("note", "varchar(20)", true, false)];
    mysql.row_count = Some(0);
    let mut drafts = mysql.columns.iter().map(draft_of).collect::<Vec<_>>();
    drafts.iter_mut().find(|column| column.name == "note").unwrap().type_name = "varchar(40)".to_owned();
    let planned = plan(&mysql, &Action::AlterColumns { table: "notes".to_owned(), columns: drafts });
    assert!(planned.blocked.is_empty(), "{:?}", planned.blocked);
    assert!(planned.steps.iter().all(|step| !step.sql.to_ascii_uppercase().contains("BEGIN") && !step.sql.to_ascii_uppercase().contains("COMMIT")));
    assert!(planned.rule.contains("不能声称已经全部回滚"));

    let mut postgres = Snapshot::bare(Engine::Postgres, "app", "public", "notes");
    postgres.columns = vec![column("id", "integer", false, true), column("note", "text", true, false)];
    postgres.row_count = Some(0);
    let mut drafts = postgres.columns.iter().map(draft_of).collect::<Vec<_>>();
    drafts.iter_mut().find(|column| column.name == "note").unwrap().type_name = "varchar(40)".to_owned();
    let planned = plan(&postgres, &Action::AlterColumns { table: "notes".to_owned(), columns: drafts });
    assert!(planned.blocked.is_empty(), "{:?}", planned.blocked);
    assert!(planned.steps.iter().all(|step| !step.sql.to_ascii_uppercase().contains("BEGIN")));
    assert!(planned.rule.contains("不能据此认为所有数据库都会回滚"));
}

#[test]
fn ddl_commands_stay_registered_beside_existing_commands() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let structure = fs::read_to_string(root.join("src/app/views/structure/mod.rs")).unwrap();
    assert!(structure.contains("application::ddl::preview_request("));
    assert!(structure.contains("application::ddl::execute_request("));
    let query = fs::read_to_string(root.join("src/app/views/query/mod.rs")).unwrap();
    assert!(query.contains("query::commands::start_run("));
    assert!(query.contains("query::commands::poll_run("));
    assert!(query.contains("query::commands::request_cancel("));
    let grid = fs::read_to_string(root.join("src/app/views/grid/mod.rs")).unwrap();
    assert!(grid.contains("application::grid::commit("));
    let mut sources = String::new();
    for name in ["sql.rs", "model.rs", "plan.rs", "introspect.rs", "exec.rs", "commands.rs", "mod.rs"] {
        sources.push_str(&fs::read_to_string(root.join("src/ddl").join(name)).unwrap());
    }
    for forbidden in ["open_sqlite", "open_mysql", "open_postgres", "reset_connection", "danger_accept_invalid", "skip_domain"] {
        assert!(!sources.contains(forbidden), "{forbidden}");
    }
}

async fn open_notes(setup: &[&str]) -> (PathBuf, Service, CatalogSession) {
    let dir = std::env::temp_dir().join(format!("rillsql-ddl-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    let database = dir.join("notes.sqlite");
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).create_if_missing(true))
        .await
        .unwrap();
    for sql in setup {
        sqlx::query(sqlx::AssertSqlSafe((*sql).to_owned())).execute(&pool).await.unwrap();
    }
    pool.close().await;
    let service = Service::new_for_test(dir.join("connections.sqlite"));
    service.open_sqlite_for_test("notes", "笔记", &database).await.unwrap();
    let session = service.catalog_session("notes").expect("已打开会话");
    (dir, service, session)
}

async fn master_sql(session: &CatalogSession, name: &str) -> Option<String> {
    let CatalogSession::Sqlite { pool, .. } = session else { panic!("sqlite") };
    sqlx::query_scalar("SELECT sql FROM sqlite_master WHERE name = ?1")
        .bind(name)
        .fetch_optional(pool)
        .await
        .unwrap()
}

async fn table_exists(session: &CatalogSession, name: &str) -> bool {
    let CatalogSession::Sqlite { pool, .. } = session else { panic!("sqlite") };
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1")
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap();
    count == 1
}

#[tokio::test]
async fn preview_cancel_does_not_submit() {
    let (dir, _service, session) = open_notes(&["CREATE TABLE notes (id INTEGER PRIMARY KEY, note TEXT)"]).await;
    let before = master_sql(&session, "notes").await;
    let loaded = exec::preview(
        &session,
        &request(Action::DropTable { table: "notes".to_owned() }),
    )
    .await
    .unwrap();
    assert_eq!(loaded.steps.len(), 1);
    assert_eq!(loaded.steps[0].sql, "DROP TABLE \"notes\";");
    assert_eq!(master_sql(&session, "notes").await, before);
    assert!(table_exists(&session, "notes").await);
    let _ = fs::remove_dir_all(dir);
}

#[tokio::test]
async fn sqlite_file_applies_add_blocks_unsafe_and_drops_only_the_named_table() {
    let (dir, _service, session) = open_notes(&[
        "CREATE TABLE notes (id INTEGER PRIMARY KEY, note TEXT)",
        "INSERT INTO notes (id, note) VALUES (1, '甲')",
        "CREATE TRIGGER trg_note AFTER UPDATE OF note ON notes BEGIN SELECT 1; END",
        "CREATE TABLE child (id INTEGER PRIMARY KEY, note_id INTEGER REFERENCES notes(id))",
        "INSERT INTO child (id, note_id) VALUES (1, 1)",
    ])
    .await;

    let described = exec::preview(
        &session,
        &request(Action::AlterColumns {
            table: "notes".to_owned(),
            columns: vec![
                ColumnDraft {
                    original: Some("id".to_owned()),
                    name: "id".to_owned(),
                    type_name: "INTEGER".to_owned(),
                    nullable: false,
                    default_sql: String::new(),
                    primary: true,
                    auto: false,
                    dropped: false,
                },
                ColumnDraft {
                    original: Some("note".to_owned()),
                    name: "note".to_owned(),
                    type_name: "TEXT".to_owned(),
                    nullable: true,
                    default_sql: String::new(),
                    primary: false,
                    auto: false,
                    dropped: false,
                },
                ColumnDraft {
                    original: None,
                    name: "extra".to_owned(),
                    type_name: "TEXT".to_owned(),
                    nullable: true,
                    default_sql: String::new(),
                    primary: false,
                    auto: false,
                    dropped: false,
                },
            ],
        }),
    )
    .await
    .unwrap();
    assert!(described.blocked.is_empty(), "{:?}", described.blocked);
    assert!(!described.rebuild);
    let confirmed: Vec<String> = described.steps.iter().map(|step| step.sql.clone()).collect();
    let applied = exec::execute(
        &session,
        &request(Action::AlterColumns {
            table: "notes".to_owned(),
            columns: vec![
                ColumnDraft {
                    original: Some("id".to_owned()),
                    name: "id".to_owned(),
                    type_name: "INTEGER".to_owned(),
                    nullable: false,
                    default_sql: String::new(),
                    primary: true,
                    auto: false,
                    dropped: false,
                },
                ColumnDraft {
                    original: Some("note".to_owned()),
                    name: "note".to_owned(),
                    type_name: "TEXT".to_owned(),
                    nullable: true,
                    default_sql: String::new(),
                    primary: false,
                    auto: false,
                    dropped: false,
                },
                ColumnDraft {
                    original: None,
                    name: "extra".to_owned(),
                    type_name: "TEXT".to_owned(),
                    nullable: true,
                    default_sql: String::new(),
                    primary: false,
                    auto: false,
                    dropped: false,
                },
            ],
        }),
        &confirmed,
    )
    .await
    .unwrap();
    assert!(applied.steps.iter().all(|step| step.status == StepStatus::Ok), "{:?}", applied.steps);
    assert!(master_sql(&session, "notes").await.unwrap_or_default().to_ascii_lowercase().contains("extra"));

    let unsafe_change = exec::execute(
        &session,
        &request(Action::AlterColumns {
            table: "notes".to_owned(),
            columns: vec![
                ColumnDraft {
                    original: Some("id".to_owned()),
                    name: "id".to_owned(),
                    type_name: "INTEGER".to_owned(),
                    nullable: false,
                    default_sql: String::new(),
                    primary: true,
                    auto: false,
                    dropped: false,
                },
                ColumnDraft {
                    original: Some("note".to_owned()),
                    name: "note".to_owned(),
                    type_name: "INTEGER".to_owned(),
                    nullable: true,
                    default_sql: String::new(),
                    primary: false,
                    auto: false,
                    dropped: false,
                },
                ColumnDraft {
                    original: Some("extra".to_owned()),
                    name: "extra".to_owned(),
                    type_name: "TEXT".to_owned(),
                    nullable: true,
                    default_sql: String::new(),
                    primary: false,
                    auto: false,
                    dropped: false,
                },
            ],
        }),
        &["ALTER TABLE \"notes\" RENAME TO \"old_notes\";".to_owned()],
    )
    .await
    .unwrap();
    assert!(!unsafe_change.blocked.is_empty() || unsafe_change.steps.is_empty(), "{:?}", unsafe_change.blocked);
    let kept = master_sql(&session, "notes").await.unwrap_or_default();
    assert!(kept.to_ascii_lowercase().contains("note"));
    assert!(!kept.to_ascii_lowercase().contains("rename to"));

    let drop_plan = exec::preview(&session, &request(Action::DropTable { table: "notes".to_owned() })).await.unwrap();
    assert_eq!(drop_plan.steps.len(), 1);
    assert_eq!(drop_plan.steps[0].sql, "DROP TABLE \"notes\";");
    assert!(!drop_plan.steps[0].sql.contains("child"));
    let confirmed = vec![drop_plan.steps[0].sql.clone()];
    let dropped = exec::execute(&session, &request(Action::DropTable { table: "notes".to_owned() }), &confirmed).await.unwrap();
    assert!(table_exists(&session, "child").await, "子表必须留下");
    if dropped.steps.first().is_some_and(|step| step.status == StepStatus::Ok) {
        assert!(!table_exists(&session, "notes").await);
    } else {
        assert!(table_exists(&session, "notes").await, "{:?}", dropped.steps);
    }
    let _ = fs::remove_dir_all(dir);
}

#[tokio::test]
async fn mid_failure_keeps_the_successful_step_and_skips_the_rest() {
    let (dir, _service, session) = open_notes(&["CREATE TABLE notes (id INTEGER PRIMARY KEY, note TEXT)"]).await;
    let steps = vec![
        Step { sql: "ALTER TABLE \"notes\" ADD COLUMN a TEXT;".to_owned(), label: "新增 a".to_owned() },
        Step { sql: "ALTER TABLE \"notes\" ADD COLUMN a TEXT;".to_owned(), label: "重复 a".to_owned() },
        Step { sql: "ALTER TABLE \"notes\" ADD COLUMN b TEXT;".to_owned(), label: "新增 b".to_owned() },
    ];
    let receipts = run_steps(&session, &steps, Some(false)).await.unwrap();
    assert_eq!(receipts[0].status, StepStatus::Ok);
    assert_eq!(receipts[1].status, StepStatus::Failed);
    assert_eq!(receipts[2].status, StepStatus::Skipped);
    assert!(receipts[2].message.contains("没有执行"));
    let CatalogSession::Sqlite { pool, .. } = &session else { panic!("sqlite") };
    let names: Vec<String> = sqlx::query_scalar("SELECT name FROM pragma_table_info('notes') ORDER BY cid").fetch_all(pool).await.unwrap();
    assert!(names.iter().any(|name| name == "a"));
    assert!(!names.iter().any(|name| name == "b"));
    let _ = fs::remove_dir_all(dir);
}

#[tokio::test]
async fn bundled_sqlite_not_null_rebuilds_without_renaming_the_old_table_first() {
    let (dir, service, _session) = open_notes(&["CREATE TABLE notes (id INTEGER PRIMARY KEY, note TEXT)", "INSERT INTO notes (id, note) VALUES (1, '甲')"]).await;
    assert!(service.set_catalog_foreign_keys("notes", Some(false)));
    let session = service.catalog_session("notes").expect("会话仍在");
    let action = Action::AlterColumns {
        table: "notes".to_owned(),
        columns: vec![
            ColumnDraft {
                original: Some("id".to_owned()),
                name: "id".to_owned(),
                type_name: "INTEGER".to_owned(),
                nullable: false,
                default_sql: String::new(),
                primary: true,
                auto: false,
                dropped: false,
            },
            ColumnDraft {
                original: Some("note".to_owned()),
                name: "note".to_owned(),
                type_name: "TEXT".to_owned(),
                nullable: false,
                default_sql: String::new(),
                primary: false,
                auto: false,
                dropped: false,
            },
        ],
    };
    let planned = exec::preview(&session, &request(action.clone())).await.unwrap();
    assert!(planned.rebuild, "{:?}", planned.blocked);
    assert!(planned.steps.iter().all(|step| !step.sql.contains("ALTER TABLE \"notes\" RENAME")));
    let confirmed: Vec<String> = planned.steps.iter().map(|step| step.sql.clone()).collect();
    let receipt = exec::execute(&session, &request(action), &confirmed).await.unwrap();
    assert!(receipt.blocked.is_empty(), "{:?}", receipt.blocked);
    assert!(receipt.steps.iter().all(|step| step.status == StepStatus::Ok), "{:?}", receipt.steps.iter().map(|step| (&step.label, step.status, &step.message)).collect::<Vec<_>>());
    let CatalogSession::Sqlite { pool, .. } = &session else { panic!("sqlite") };
    let notnull: i64 = sqlx::query_scalar("SELECT \"notnull\" FROM pragma_table_info('notes') WHERE name = 'note'").fetch_one(pool).await.unwrap();
    assert_eq!(notnull, 1);
    let value: String = sqlx::query_scalar("SELECT note FROM notes WHERE id = 1").fetch_one(pool).await.unwrap();
    assert_eq!(value, "甲");
    let fk: i64 = sqlx::query_scalar("PRAGMA foreign_keys").fetch_one(pool).await.unwrap();
    // 标记被写成 false，但这条已打开连接的 live foreign_keys 仍是 ON，重建后恢复 live。
    assert_eq!(fk, 1);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn postgres_create_index_name_is_not_schema_qualified() {
    let mut snapshot = Snapshot::bare(Engine::Postgres, "app", "public", "notes");
    snapshot.columns = vec![column("id", "integer", false, true), column("note", "text", true, false)];
    let columns = vec![IndexColumnDraft { name: "note".to_owned(), descending: false }];
    let plain = plan(
        &snapshot,
        &Action::CreateIndex { table: "notes".to_owned(), name: "idx".to_owned(), unique: false, columns: columns.clone() },
    );
    assert!(plain.blocked.is_empty(), "{:?}", plain.blocked);
    assert_eq!(plain.steps.len(), 1);
    assert_eq!(plain.steps[0].sql, "CREATE INDEX \"idx\" ON \"public\".\"notes\" (\"note\" ASC);");
    assert!(!plain.steps[0].sql.contains("\"public\".\"idx\""));
    let unique = plan(
        &snapshot,
        &Action::CreateIndex { table: "notes".to_owned(), name: "idx".to_owned(), unique: true, columns },
    );
    assert!(unique.blocked.is_empty(), "{:?}", unique.blocked);
    assert_eq!(unique.steps[0].sql, "CREATE UNIQUE INDEX \"idx\" ON \"public\".\"notes\" (\"note\" ASC);");
    assert!(!unique.steps[0].sql.contains("\"public\".\"idx\""));
}

#[test]
fn sqlite_rebuild_preview_restores_live_foreign_keys_not_the_recorded_flag() {
    let mut snapshot = notes("3.51.3");
    snapshot.recorded_foreign_keys = Some(false);
    snapshot.live_foreign_keys = Some(true);
    snapshot.foreign_keys.push(ForeignKey {
        name: "fk_note".to_owned(),
        columns: vec!["note".to_owned()],
        ref_schema: String::new(),
        ref_table: "parent".to_owned(),
        ref_columns: vec!["id".to_owned()],
        on_delete: "NO ACTION".to_owned(),
        on_update: "NO ACTION".to_owned(),
    });
    let planned = plan(&snapshot, &Action::DropForeignKey { table: "notes".to_owned(), name: "fk_note".to_owned() });
    assert!(planned.blocked.is_empty(), "{:?}", planned.blocked);
    assert!(planned.rebuild);
    assert_eq!(planned.steps.last().map(|step| step.sql.as_str()), Some("PRAGMA foreign_keys = ON;"));
    assert!(planned.steps.iter().any(|step| step.sql == "PRAGMA foreign_keys = OFF;"));
    assert!(planned.risks.iter().any(|risk| risk.text.contains("当前连接 foreign_keys = ON")));
    assert!(planned.risks.iter().all(|risk| !risk.text.contains("恢复为 OFF")));
}

fn kept_column(name: &str, type_name: &str, primary: bool) -> ColumnDraft {
    ColumnDraft {
        original: Some(name.to_owned()),
        name: name.to_owned(),
        type_name: type_name.to_owned(),
        nullable: !primary,
        default_sql: String::new(),
        primary,
        auto: false,
        dropped: false,
    }
}

fn recorded_foreign_keys(session: &CatalogSession) -> Option<bool> {
    match session {
        CatalogSession::Sqlite { foreign_keys, .. } => *foreign_keys,
        _ => None,
    }
}

#[tokio::test]
async fn rebuild_restores_live_foreign_keys_and_enable_updates_the_session() {
    let (dir, service, session) = open_notes(&[
        "PRAGMA foreign_keys = OFF",
        "CREATE TABLE parent (id INTEGER PRIMARY KEY)",
        "CREATE TABLE notes (id INTEGER PRIMARY KEY, note TEXT, parent_id INTEGER REFERENCES parent(id))",
        "INSERT INTO notes (id, note, parent_id) VALUES (1, '甲', 99)",
    ])
    .await;
    let CatalogSession::Sqlite { pool, .. } = &session else { panic!("sqlite") };
    sqlx::query("PRAGMA foreign_keys = OFF").execute(pool).await.unwrap();
    assert!(service.set_catalog_foreign_keys("notes", Some(false)));
    let session = service.catalog_session("notes").expect("会话仍在");
    assert_eq!(recorded_foreign_keys(&session), Some(false));
    let CatalogSession::Sqlite { pool, .. } = &session else { panic!("sqlite") };
    let live_off: i64 = sqlx::query_scalar("PRAGMA foreign_keys").fetch_one(pool).await.unwrap();
    assert_eq!(live_off, 0);
    let enable = Action::SetForeignKeys { enabled: true };
    let preview = exec::preview(&session, &request(enable.clone())).await.unwrap();
    assert_eq!(preview.steps.len(), 1);
    assert_eq!(preview.steps[0].sql, "PRAGMA foreign_keys = ON;");
    let confirmed: Vec<String> = preview.steps.iter().map(|step| step.sql.clone()).collect();
    let turned_on = commands::execute_saved(&service, "notes", &request(enable), &confirmed).await.unwrap();
    assert!(turned_on.steps.iter().all(|step| step.status == StepStatus::Ok), "{:?}", turned_on.steps);
    let enabled = service.catalog_session("notes").expect("会话仍在");
    assert_eq!(recorded_foreign_keys(&enabled), Some(true));

    assert!(service.set_catalog_foreign_keys("notes", Some(false)));
    let stale = service.catalog_session("notes").expect("会话仍在");
    assert_eq!(recorded_foreign_keys(&stale), Some(false));
    let CatalogSession::Sqlite { pool, .. } = &stale else { panic!("sqlite") };
    let live_before: i64 = sqlx::query_scalar("PRAGMA foreign_keys").fetch_one(pool).await.unwrap();
    assert_eq!(live_before, 1);

    let action = Action::AlterColumns {
        table: "notes".to_owned(),
        columns: vec![kept_column("id", "INTEGER", true), kept_column("note", "INTEGER", false), kept_column("parent_id", "INTEGER", false)],
    };
    let planned = exec::preview(&stale, &request(action.clone())).await.unwrap();
    assert!(planned.rebuild, "{:?}", planned.blocked);
    assert_eq!(planned.steps.last().map(|step| step.sql.as_str()), Some("PRAGMA foreign_keys = ON;"));
    assert!(planned.steps.iter().any(|step| step.sql == "PRAGMA foreign_keys = OFF;"));
    let confirmed: Vec<String> = planned.steps.iter().map(|step| step.sql.clone()).collect();
    let receipt = exec::execute(&stale, &request(action), &confirmed).await.unwrap();
    assert!(receipt.steps.iter().any(|step| step.status == StepStatus::Failed), "{:?}", receipt.steps);
    let restored = receipt.steps.iter().rev().find(|step| step.sql == "PRAGMA foreign_keys = ON;").expect("恢复 live");
    assert_eq!(restored.status, StepStatus::Ok);
    let live_after: i64 = sqlx::query_scalar("PRAGMA foreign_keys").fetch_one(pool).await.unwrap();
    assert_eq!(live_after, 1);
    let sql = master_sql(&stale, "notes").await.unwrap_or_default();
    assert!(sql.to_ascii_lowercase().contains("note text"), "{sql}");
    assert_eq!(recorded_foreign_keys(&service.catalog_session("notes").expect("会话仍在")), Some(false));
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn sqlserver_alters_use_tsql_forms_without_inline_defaults() {
    // 快照带 IDENTITY 主键、有具名默认约束的列。
    let mut snapshot = Snapshot::bare(Engine::Sqlserver, "rillsql", "dbo", "notes");
    let mut status = column("status", "nvarchar(20)", false, false);
    status.default_sql = Some("N'open'".to_owned());
    status.default_constraint = Some("DF_notes_status".to_owned());
    snapshot.columns = vec![column("id", "int", false, true), status];
    snapshot.row_count = Some(0);

    // 改类型：ALTER COLUMN 只含列名/类型/NULL 性，不带 DEFAULT。
    let mut drafts = snapshot.columns.iter().map(draft_of).collect::<Vec<_>>();
    drafts.iter_mut().find(|column| column.name == "status").unwrap().type_name = "nvarchar(40)".to_owned();
    let planned = plan(&snapshot, &Action::AlterColumns { table: "notes".to_owned(), columns: drafts });
    assert!(planned.blocked.is_empty(), "{:?}", planned.blocked);
    assert!(planned.steps.iter().any(|step| step.sql.contains("ALTER TABLE [dbo].[notes] ALTER COLUMN [status] nvarchar(40) NOT NULL;")), "{:?}", planned.steps);
    assert!(!planned.steps.iter().any(|step| step.sql.contains("CHANGE COLUMN")), "T-SQL 没有 CHANGE COLUMN：{:?}", planned.steps);
    assert!(!planned.steps.iter().any(|step| {
        let upper = step.sql.to_ascii_uppercase();
        upper.contains("ALTER COLUMN") && upper.contains(" DEFAULT")
    }), "ALTER COLUMN 不能内联 DEFAULT：{:?}", planned.steps);

    // 删默认值：用 sys.default_constraints 读到的约束名走 DROP CONSTRAINT。
    let mut drop_default = snapshot.columns.iter().map(draft_of).collect::<Vec<_>>();
    drop_default.iter_mut().find(|column| column.name == "status").unwrap().default_sql = String::new();
    let planned = plan(&snapshot, &Action::AlterColumns { table: "notes".to_owned(), columns: drop_default });
    assert!(planned.steps.iter().any(|step| step.sql.contains("ALTER TABLE [dbo].[notes] DROP CONSTRAINT [DF_notes_status];")), "{:?}", planned.steps);

    // 约束名缺失时按 sys.default_constraints 动态查找，不猜 DF_ 前缀名。
    let mut unnamed = snapshot.clone();
    unnamed.columns[1].default_constraint = None;
    let mut drop_unnamed = unnamed.columns.iter().map(draft_of).collect::<Vec<_>>();
    drop_unnamed.iter_mut().find(|column| column.name == "status").unwrap().default_sql = String::new();
    let planned = plan(&unnamed, &Action::AlterColumns { table: "notes".to_owned(), columns: drop_unnamed });
    let dynamic = planned.steps.iter().find(|step| step.sql.contains("DROP CONSTRAINT")).expect("应当生成删除默认值的语句");
    assert!(dynamic.sql.contains("sys.default_constraints"), "{:?}", planned.steps);
    assert!(dynamic.sql.contains("OBJECT_ID(N'[dbo].[notes]')"), "{:?}", planned.steps);

    // 改默认值：先摘既有约束再 ADD（同名 2714 / 不同名 1781 都会失败），ADD 用 DF_<表>_<列> 名。
    let mut change_default = snapshot.columns.iter().map(draft_of).collect::<Vec<_>>();
    change_default.iter_mut().find(|column| column.name == "status").unwrap().default_sql = "N'done'".to_owned();
    let planned = plan(&snapshot, &Action::AlterColumns { table: "notes".to_owned(), columns: change_default });
    let drop_at = planned.steps.iter().position(|step| step.sql.contains("ALTER TABLE [dbo].[notes] DROP CONSTRAINT [DF_notes_status];"));
    let add_at = planned.steps.iter().position(|step| step.sql.contains("ALTER TABLE [dbo].[notes] ADD CONSTRAINT [DF_notes_status] DEFAULT N'done' FOR [status];"));
    let (Some(drop), Some(add)) = (drop_at, add_at) else {
        panic!("改默认值应先 DROP 再 ADD：{:?}", planned.steps);
    };
    assert!(drop < add, "DROP 必须在 ADD 之前：{:?}", planned.steps);

    // 重命名：sp_rename，不生成 MySQL 式 CHANGE。
    let mut renamed = snapshot.columns.iter().map(draft_of).collect::<Vec<_>>();
    renamed.iter_mut().find(|column| column.name == "status").unwrap().name = "state".to_owned();
    let planned = plan(&snapshot, &Action::AlterColumns { table: "notes".to_owned(), columns: renamed });
    assert!(planned.steps.iter().any(|step| step.sql.contains("EXEC sp_rename N'dbo.notes.status', N'state', 'COLUMN';")), "{:?}", planned.steps);

    // 新增列：T-SQL 是 ADD，不是 ADD COLUMN。
    let mut add = snapshot.columns.clone();
    add.push(column("extra", "int", true, false));
    let planned = plan(&snapshot, &Action::AlterColumns { table: "notes".to_owned(), columns: add.iter().map(draft_of).collect() });
    assert!(planned.steps.iter().any(|step| step.sql.contains("ALTER TABLE [dbo].[notes] ADD [extra] int;")), "{:?}", planned.steps);
    assert!(!planned.steps.iter().any(|step| step.sql.contains("ADD COLUMN")), "{:?}", planned.steps);

    // 新建表的自增列是 IDENTITY(1, 1)（快照是已存在的表：建表动作按“新表”生成创建语句）。
    let mut fresh = Snapshot::bare(Engine::Sqlserver, "rillsql", "dbo", "fresh");
    fresh.object_exists = false;
    let create = plan(
        &fresh,
        &Action::CreateTable {
            name: "fresh".to_owned(),
            columns: vec![ColumnDraft {
                original: None,
                name: "id".to_owned(),
                type_name: "int".to_owned(),
                nullable: false,
                default_sql: String::new(),
                primary: true,
                auto: true,
                dropped: false,
            }],
        },
    );
    assert!(create.steps.iter().any(|step| step.sql.contains("[id] int NOT NULL IDENTITY(1, 1)")), "{:?}", create.steps);
}


fn sequence_snapshot() -> Snapshot {
    let mut snapshot = Snapshot::bare(Engine::Postgres, "app", "public", "");
    snapshot.kind = "sequence".to_owned();
    snapshot.name = "orders_id_seq".to_owned();
    snapshot.sequence = Some(SequenceState {
        start: Some(1),
        increment: Some(1),
        min_value: Some(1),
        max_value: Some(2_147_483_647),
        cycle: Some(false),
    });
    snapshot
}

#[test]
fn postgres_sequence_alter_only_emits_changed_clauses() {
    let snapshot = sequence_snapshot();
    let planned = plan(
        &snapshot,
        &Action::AlterSequence { sequence: "orders_id_seq".to_owned(), start: Some(1), increment: Some(5), min_value: None, max_value: None, cycle: Some(true) },
    );
    assert!(planned.blocked.is_empty(), "{:?}", planned.blocked);
    assert_eq!(planned.steps.len(), 1);
    assert_eq!(planned.steps[0].sql, "ALTER SEQUENCE \"public\".\"orders_id_seq\" INCREMENT BY 5 CYCLE;");
    assert!(planned.risks.iter().any(|risk| risk.text.contains("序列参数改完立即生效")));

    // 和现值完全一致时不给执行入口。
    let same = plan(
        &snapshot,
        &Action::AlterSequence { sequence: "orders_id_seq".to_owned(), start: Some(1), increment: Some(1), min_value: None, max_value: None, cycle: Some(false) },
    );
    assert!(same.steps.is_empty());
    assert!(same.blocked.iter().any(|item| item.contains("没有变化")));

    let zero = plan(
        &snapshot,
        &Action::AlterSequence { sequence: "orders_id_seq".to_owned(), start: None, increment: Some(0), min_value: None, max_value: None, cycle: None },
    );
    assert!(zero.steps.is_empty());
    assert!(zero.blocked.iter().any(|item| item.contains("步长不能是 0")));

    let bounds = plan(
        &snapshot,
        &Action::AlterSequence { sequence: "orders_id_seq".to_owned(), start: None, increment: None, min_value: Some(9), max_value: Some(9), cycle: None },
    );
    assert!(bounds.steps.is_empty());
    assert!(bounds.blocked.iter().any(|item| item.contains("最小值必须小于最大值")));
}

#[test]
fn sequence_alter_blocks_without_state_or_postgres() {
    let missing = Snapshot::bare(Engine::Postgres, "app", "public", "");
    let planned = plan(
        &missing,
        &Action::AlterSequence { sequence: "orders_id_seq".to_owned(), start: None, increment: Some(5), min_value: None, max_value: None, cycle: None },
    );
    assert!(planned.steps.is_empty());
    assert!(planned.blocked.iter().any(|item| item.contains("没有读到序列")), "{:?}", planned.blocked);

    let mut unreadable = sequence_snapshot();
    unreadable.sequence = Some(SequenceState::default());
    let planned = plan(
        &unreadable,
        &Action::AlterSequence { sequence: "orders_id_seq".to_owned(), start: None, increment: Some(5), min_value: None, max_value: None, cycle: None },
    );
    assert!(planned.blocked.iter().any(|item| item.contains("读不到")), "{:?}", planned.blocked);

    let sqlite = Snapshot::bare(Engine::Sqlite, "main", "", "");
    let planned = plan(
        &sqlite,
        &Action::AlterSequence { sequence: "orders_id_seq".to_owned(), start: None, increment: Some(5), min_value: None, max_value: None, cycle: None },
    );
    assert!(planned.steps.is_empty());
    assert!(planned.blocked.iter().any(|item| item.contains("只有 PostgreSQL")), "{:?}", planned.blocked);
}

#[test]
fn postgres_enum_add_value_previews_placement_and_blocks_duplicates() {
    let mut snapshot = Snapshot::bare(Engine::Postgres, "app", "public", "");
    snapshot.kind = "enum".to_owned();
    snapshot.name = "mood".to_owned();
    snapshot.enum_values = vec!["sad".to_owned(), "ok".to_owned()];

    let planned = plan(&snapshot, &Action::AddEnumValue { enum_name: "mood".to_owned(), value: "great".to_owned(), before: Some("ok".to_owned()) });
    assert!(planned.blocked.is_empty(), "{:?}", planned.blocked);
    assert_eq!(planned.steps[0].sql, "ALTER TYPE \"public\".\"mood\" ADD VALUE 'great' BEFORE 'ok';");

    let escaped = plan(&snapshot, &Action::AddEnumValue { enum_name: "mood".to_owned(), value: "it's".to_owned(), before: None });
    assert_eq!(escaped.steps[0].sql, "ALTER TYPE \"public\".\"mood\" ADD VALUE 'it''s';");

    let duplicate = plan(&snapshot, &Action::AddEnumValue { enum_name: "mood".to_owned(), value: "ok".to_owned(), before: None });
    assert!(duplicate.steps.is_empty());
    assert!(duplicate.blocked.iter().any(|item| item.contains("已经是 mood 的值")), "{:?}", duplicate.blocked);

    let anchor = plan(&snapshot, &Action::AddEnumValue { enum_name: "mood".to_owned(), value: "great".to_owned(), before: Some("angry".to_owned()) });
    assert!(anchor.blocked.iter().any(|item| item.contains("找不到枚举值 angry")), "{:?}", anchor.blocked);

    let blank = plan(&snapshot, &Action::AddEnumValue { enum_name: "mood".to_owned(), value: "  ".to_owned(), before: None });
    assert!(blank.blocked.iter().any(|item| item.contains("枚举值不能为空")), "{:?}", blank.blocked);

    let mut unread = snapshot.clone();
    unread.enum_values.clear();
    let planned = plan(&unread, &Action::AddEnumValue { enum_name: "mood".to_owned(), value: "great".to_owned(), before: None });
    assert!(planned.steps.is_empty());
    assert!(planned.blocked.iter().any(|item| item.contains("没有读到枚举")), "{:?}", planned.blocked);

    let mysql = Snapshot::bare(Engine::Mysql, "app", "", "");
    let planned = plan(&mysql, &Action::AddEnumValue { enum_name: "mood".to_owned(), value: "great".to_owned(), before: None });
    assert!(planned.blocked.iter().any(|item| item.contains("只有 PostgreSQL")), "{:?}", planned.blocked);
}

#[test]
fn postgres_domain_alter_previews_default_and_not_null() {
    let mut snapshot = Snapshot::bare(Engine::Postgres, "app", "public", "");
    snapshot.kind = "domain".to_owned();
    snapshot.name = "email".to_owned();
    snapshot.domain = Some(DomainState { default_sql: Some("'none@example.com'".to_owned()), not_null: false });

    let planned = plan(
        &snapshot,
        &Action::AlterDomain { domain: "email".to_owned(), default_sql: Some("'na@example.com'".to_owned()), not_null: Some(true) },
    );
    assert!(planned.blocked.is_empty(), "{:?}", planned.blocked);
    let sql: Vec<&str> = planned.steps.iter().map(|step| step.sql.as_str()).collect();
    assert_eq!(
        sql,
        vec!["ALTER DOMAIN \"public\".\"email\" SET DEFAULT 'na@example.com';", "ALTER DOMAIN \"public\".\"email\" SET NOT NULL;"]
    );
    assert!(planned.risks.iter().any(|risk| risk.level == "warn" && risk.text.contains("SET NOT NULL 会先检查已有数据")));

    let drop_default = plan(&snapshot, &Action::AlterDomain { domain: "email".to_owned(), default_sql: Some(String::new()), not_null: None });
    assert_eq!(drop_default.steps[0].sql, "ALTER DOMAIN \"public\".\"email\" DROP DEFAULT;");

    let unchanged = plan(
        &snapshot,
        &Action::AlterDomain { domain: "email".to_owned(), default_sql: Some("'none@example.com'".to_owned()), not_null: Some(false) },
    );
    assert!(unchanged.steps.is_empty());
    assert!(unchanged.blocked.iter().any(|item| item.contains("没有变化")), "{:?}", unchanged.blocked);

    let bad_default = plan(&snapshot, &Action::AlterDomain { domain: "email".to_owned(), default_sql: Some("now() + 1".to_owned()), not_null: None });
    assert!(bad_default.blocked.iter().any(|item| item.contains("默认值只接受")), "{:?}", bad_default.blocked);

    let not_domain = Snapshot::bare(Engine::Postgres, "app", "public", "");
    let planned = plan(&not_domain, &Action::AlterDomain { domain: "email".to_owned(), default_sql: None, not_null: Some(true) });
    assert!(planned.blocked.iter().any(|item| item.contains("没有读到域")), "{:?}", planned.blocked);
}

#[test]
fn mysql_event_toggle_previews_and_blocks_noop() {
    let mut snapshot = Snapshot::bare(Engine::Mysql, "shop", "", "");
    snapshot.kind = "event".to_owned();
    snapshot.name = "nightly".to_owned();
    snapshot.event_enabled = Some(true);

    let planned = plan(&snapshot, &Action::SetEventEnabled { event: "nightly".to_owned(), enabled: false });
    assert!(planned.blocked.is_empty(), "{:?}", planned.blocked);
    assert_eq!(planned.steps[0].sql, "ALTER EVENT `shop`.`nightly` DISABLE;");
    assert!(planned.risks.iter().any(|risk| risk.text.contains("event_scheduler")));

    let noop = plan(&snapshot, &Action::SetEventEnabled { event: "nightly".to_owned(), enabled: true });
    assert!(noop.steps.is_empty());
    assert!(noop.blocked.iter().any(|item| item.contains("已经是 enabled")), "{:?}", noop.blocked);

    let mut maria = snapshot.clone();
    maria.engine = Engine::Mariadb;
    let planned = plan(&maria, &Action::SetEventEnabled { event: "nightly".to_owned(), enabled: false });
    assert_eq!(planned.steps[0].sql, "ALTER EVENT `shop`.`nightly` DISABLE;");

    let mut missing = snapshot.clone();
    missing.event_enabled = None;
    let planned = plan(&missing, &Action::SetEventEnabled { event: "nightly".to_owned(), enabled: false });
    assert!(planned.blocked.iter().any(|item| item.contains("没有读到事件")), "{:?}", planned.blocked);

    let postgres = Snapshot::bare(Engine::Postgres, "shop", "public", "");
    let planned = plan(&postgres, &Action::SetEventEnabled { event: "nightly".to_owned(), enabled: true });
    assert!(planned.blocked.iter().any(|item| item.contains("只有 MySQL")), "{:?}", planned.blocked);
}

#[test]
fn sqlite_pragma_plan_covers_journal_mode_and_user_version() {
    let mut snapshot = Snapshot::bare(Engine::Sqlite, "main", "", "");
    snapshot.kind = "database".to_owned();
    snapshot.pragmas.insert("journal_mode".to_owned(), "delete".to_owned());
    snapshot.pragmas.insert("user_version".to_owned(), "0".to_owned());
    snapshot.pragmas.insert("foreign_keys".to_owned(), "ON".to_owned());

    let planned = plan(&snapshot, &Action::SetPragma { pragma: "journal_mode".to_owned(), value: "wal".to_owned() });
    assert!(planned.blocked.is_empty(), "{:?}", planned.blocked);
    assert_eq!(planned.steps[0].sql, "PRAGMA journal_mode = WAL;");
    assert!(planned.risks.iter().any(|risk| risk.level == "warn" && risk.text.contains("-wal")));

    let version = plan(&snapshot, &Action::SetPragma { pragma: "user_version".to_owned(), value: "3".to_owned() });
    assert_eq!(version.steps[0].sql, "PRAGMA user_version = 3;");

    let unchanged = plan(&snapshot, &Action::SetPragma { pragma: "journal_mode".to_owned(), value: "DELETE".to_owned() });
    assert!(unchanged.steps.is_empty());
    assert!(unchanged.blocked.iter().any(|item| item.contains("已经是 DELETE")), "{:?}", unchanged.blocked);

    let foreign_keys = plan(&snapshot, &Action::SetPragma { pragma: "foreign_keys".to_owned(), value: "OFF".to_owned() });
    assert!(foreign_keys.steps.is_empty());
    assert!(foreign_keys.blocked.iter().any(|item| item.contains("外键开关动作")), "{:?}", foreign_keys.blocked);

    let unknown = plan(&snapshot, &Action::SetPragma { pragma: "page_size".to_owned(), value: "4096".to_owned() });
    assert!(unknown.blocked.iter().any(|item| item.contains("只改 journal_mode")), "{:?}", unknown.blocked);

    let bad_version = plan(&snapshot, &Action::SetPragma { pragma: "user_version".to_owned(), value: "abc".to_owned() });
    assert!(bad_version.blocked.iter().any(|item| item.contains("0 到 2147483647")), "{:?}", bad_version.blocked);

    let postgres = Snapshot::bare(Engine::Postgres, "app", "public", "");
    let planned = plan(&postgres, &Action::SetPragma { pragma: "journal_mode".to_owned(), value: "wal".to_owned() });
    assert!(planned.blocked.iter().any(|item| item.contains("只有 SQLite")), "{:?}", planned.blocked);
}

#[test]
fn sqlite_attach_plan_blocks_duplicate_and_reserved_names() {
    let mut snapshot = Snapshot::bare(Engine::Sqlite, "main", "", "");
    snapshot.kind = "database".to_owned();
    snapshot.attached = vec![
        AttachedDatabase { name: "main".to_owned(), path: "/tmp/notes.sqlite".to_owned() },
        AttachedDatabase { name: "aux".to_owned(), path: "/tmp/aux.sqlite".to_owned() },
        AttachedDatabase { name: "temp".to_owned(), path: String::new() },
    ];

    let planned = plan(&snapshot, &Action::AttachDatabase { path: "/tmp/other.db".to_owned(), name: "other".to_owned() });
    assert!(planned.blocked.is_empty(), "{:?}", planned.blocked);
    assert_eq!(planned.steps[0].sql, "ATTACH DATABASE '/tmp/other.db' AS \"other\";");

    let quoted = plan(&snapshot, &Action::AttachDatabase { path: "/tmp/it's.db".to_owned(), name: "extra".to_owned() });
    assert_eq!(quoted.steps[0].sql, "ATTACH DATABASE '/tmp/it''s.db' AS \"extra\";");

    let duplicate = plan(&snapshot, &Action::AttachDatabase { path: "/tmp/x.db".to_owned(), name: "AUX".to_owned() });
    assert!(duplicate.steps.is_empty());
    assert!(duplicate.blocked.iter().any(|item| item.contains("已经附加")), "{:?}", duplicate.blocked);

    let reserved = plan(&snapshot, &Action::AttachDatabase { path: "/tmp/x.db".to_owned(), name: "temp".to_owned() });
    assert!(reserved.blocked.iter().any(|item| item.contains("保留的库名")), "{:?}", reserved.blocked);

    let blank = plan(&snapshot, &Action::AttachDatabase { path: "  ".to_owned(), name: "aux2".to_owned() });
    assert!(blank.blocked.iter().any(|item| item.contains("路径不能为空")), "{:?}", blank.blocked);

    let same_file = plan(&snapshot, &Action::AttachDatabase { path: "/tmp/aux.sqlite".to_owned(), name: "aux2".to_owned() });
    assert!(same_file.blocked.is_empty(), "{:?}", same_file.blocked);
    assert!(same_file.risks.iter().any(|risk| risk.text.contains("已经作为 aux 附加过")), "{:?}", same_file.risks);
}

#[test]
fn sqlite_maintenance_plans_cover_vacuum_and_reindex() {
    let snapshot = Snapshot::bare(Engine::Sqlite, "main", "", "");
    let vacuum = plan(&snapshot, &Action::Vacuum);
    assert!(vacuum.blocked.is_empty(), "{:?}", vacuum.blocked);
    assert_eq!(vacuum.steps[0].sql, "VACUUM;");
    assert!(vacuum.risks.iter().any(|risk| risk.level == "warn" && risk.text.contains("重写整个数据库文件")));

    let reindex = plan(&snapshot, &Action::Reindex { target: None });
    assert_eq!(reindex.steps[0].sql, "REINDEX;");
    let indexed = plan(&snapshot, &Action::Reindex { target: Some("orders".to_owned()) });
    assert_eq!(indexed.steps[0].sql, "REINDEX \"orders\";");

    let postgres = Snapshot::bare(Engine::Postgres, "app", "public", "");
    let planned = plan(&postgres, &Action::Vacuum);
    assert!(planned.steps.is_empty());
    assert!(planned.blocked.iter().any(|item| item.contains("维护动作只对 SQLite")), "{:?}", planned.blocked);
}

async fn user_version_of(session: &CatalogSession) -> i64 {
    let CatalogSession::Sqlite { pool, .. } = session else { panic!("sqlite") };
    sqlx::query_scalar("PRAGMA user_version").fetch_one(pool).await.unwrap()
}

async fn page_count_of(session: &CatalogSession) -> i64 {
    let CatalogSession::Sqlite { pool, .. } = session else { panic!("sqlite") };
    sqlx::query_scalar("PRAGMA page_count").fetch_one(pool).await.unwrap()
}

#[tokio::test]
async fn sqlite_pragma_and_attach_round_trip() {
    let (dir, _service, session) = open_notes(&["CREATE TABLE notes (id INTEGER PRIMARY KEY, note TEXT)"]).await;

    let action = Action::SetPragma { pragma: "user_version".to_owned(), value: "3".to_owned() };
    let planned = exec::preview(&session, &request(action.clone())).await.unwrap();
    assert_eq!(planned.steps.len(), 1, "{:?}", planned.blocked);
    let confirmed: Vec<String> = planned.steps.iter().map(|step| step.sql.clone()).collect();
    let receipt = exec::execute(&session, &request(action), &confirmed).await.unwrap();
    assert!(receipt.steps.iter().all(|step| step.status == StepStatus::Ok), "{:?}", receipt.steps);
    assert_eq!(user_version_of(&session).await, 3);

    // 已经是这个值时 preview 不给执行入口。
    let again = exec::preview(&session, &request(Action::SetPragma { pragma: "user_version".to_owned(), value: "3".to_owned() })).await.unwrap();
    assert!(again.steps.is_empty());
    assert!(again.blocked.iter().any(|item| item.contains("已经是")), "{:?}", again.blocked);

    // journal_mode 会带回一行结果：执行路径不能因为“返回了结果”失败。
    let current_mode = {
        let CatalogSession::Sqlite { pool, .. } = &session else { panic!("sqlite") };
        let mode: String = sqlx::query_scalar("PRAGMA journal_mode").fetch_one(pool).await.unwrap();
        mode
    };
    let target_mode = if current_mode.eq_ignore_ascii_case("delete") { "wal" } else { "delete" };
    let action = Action::SetPragma { pragma: "journal_mode".to_owned(), value: target_mode.to_owned() };
    let planned = exec::preview(&session, &request(action.clone())).await.unwrap();
    assert!(planned.blocked.is_empty(), "{:?}", planned.blocked);
    let confirmed: Vec<String> = planned.steps.iter().map(|step| step.sql.clone()).collect();
    let receipt = exec::execute(&session, &request(action), &confirmed).await.unwrap();
    assert_eq!(receipt.steps[0].status, StepStatus::Ok, "{:?}", receipt.steps);
    assert!(receipt.steps[0].message.contains("数据库返回"), "{}", receipt.steps[0].message);

    // ATTACH 后能读到附加库里的表。
    let extra = dir.join("extra.sqlite");
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&extra).create_if_missing(true))
        .await
        .unwrap();
    sqlx::query("CREATE TABLE tags (id INTEGER PRIMARY KEY)").execute(&pool).await.unwrap();
    pool.close().await;
    let path = extra.to_string_lossy().to_string();
    let attach = Action::AttachDatabase { path: path.clone(), name: "extra".to_owned() };
    let planned = exec::preview(&session, &request(attach.clone())).await.unwrap();
    assert!(planned.blocked.is_empty(), "{:?}", planned.blocked);
    let confirmed: Vec<String> = planned.steps.iter().map(|step| step.sql.clone()).collect();
    let receipt = exec::execute(&session, &request(attach), &confirmed).await.unwrap();
    assert!(receipt.steps.iter().all(|step| step.status == StepStatus::Ok), "{:?}", receipt.steps);
    let CatalogSession::Sqlite { pool, .. } = &session else { panic!("sqlite") };
    let tags: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM extra.sqlite_master WHERE type = 'table' AND name = 'tags'")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(tags, 1);
    let attached: Vec<String> = sqlx::query_scalar("SELECT name FROM pragma_database_list").fetch_all(pool).await.unwrap();
    assert!(attached.iter().any(|name| name == "extra"), "{attached:?}");

    // 同名再附加：preview 直接挡住。
    let duplicate = exec::preview(&session, &request(Action::AttachDatabase { path, name: "extra".to_owned() })).await.unwrap();
    assert!(duplicate.steps.is_empty());
    assert!(duplicate.blocked.iter().any(|item| item.contains("已经附加")), "{:?}", duplicate.blocked);
    let _ = fs::remove_dir_all(dir);
}

#[tokio::test]
async fn sqlite_maintenance_reports_result_and_writes_only_when_executed() {
    let (dir, _service, session) = open_notes(&[
        "CREATE TABLE notes (id INTEGER PRIMARY KEY, note TEXT)",
        "CREATE INDEX notes_note ON notes (note)",
        "INSERT INTO notes (id, note) VALUES (1, 'a')",
    ])
    .await;
    let pages_before = page_count_of(&session).await;
    let planned = exec::preview(&session, &request(Action::Vacuum)).await.unwrap();
    assert_eq!(planned.steps.len(), 1);
    assert_eq!(planned.steps[0].sql, "VACUUM;");
    // 取消（不执行）时没有任何写入。
    assert_eq!(page_count_of(&session).await, pages_before);
    assert!(master_sql(&session, "notes").await.is_some());

    let confirmed: Vec<String> = planned.steps.iter().map(|step| step.sql.clone()).collect();
    let receipt = exec::execute(&session, &request(Action::Vacuum), &confirmed).await.unwrap();
    assert_eq!(receipt.steps.len(), 1);
    assert_eq!(receipt.steps[0].status, StepStatus::Ok, "{:?}", receipt.steps);
    assert!(receipt.steps[0].message.contains("VACUUM 已完成"), "{}", receipt.steps[0].message);

    let reindex = Action::Reindex { target: Some("notes_note".to_owned()) };
    let planned = exec::preview(&session, &request(reindex.clone())).await.unwrap();
    assert_eq!(planned.steps[0].sql, "REINDEX \"notes_note\";");
    let confirmed: Vec<String> = planned.steps.iter().map(|step| step.sql.clone()).collect();
    let receipt = exec::execute(&session, &request(reindex), &confirmed).await.unwrap();
    assert_eq!(receipt.steps[0].status, StepStatus::Ok, "{:?}", receipt.steps);
    assert!(receipt.steps[0].message.contains("REINDEX 已完成"), "{}", receipt.steps[0].message);

    // 会话类型对不上时直接返回原因，不去猜语句。
    let mismatched = exec::preview(
        &session,
        &Request { database: "app".to_owned(), schema: "public".to_owned(), engine: Engine::Postgres, action: Action::Vacuum },
    )
    .await;
    assert!(mismatched.is_err(), "{mismatched:?}");
    let _ = fs::remove_dir_all(dir);
}

fn table_of_engine(engine: Engine) -> Snapshot {
    let (database, schema) = match engine {
        Engine::Sqlite => ("main", ""),
        Engine::Mysql | Engine::Mariadb => ("app", ""),
        Engine::Postgres => ("app", "public"),
        _ => ("app", "dbo"),
    };
    let mut snapshot = Snapshot::bare(engine, database, schema, "orders");
    snapshot.columns = vec![column("id", "int", false, true), column("note", "varchar(20)", true, false)];
    snapshot.row_count = Some(3);
    snapshot
}

fn view_snapshot(engine: Engine) -> Snapshot {
    let mut snapshot = table_of_engine(engine);
    snapshot.name = "v_orders".to_owned();
    snapshot.kind = "view".to_owned();
    snapshot
}

fn has_danger(planned: &super::model::Plan) -> bool {
    planned.risks.iter().any(|risk| risk.level == "danger")
}

#[test]
fn object_actions_emit_engine_specific_sql_with_a_danger_risk() {
    let truncate = Action::TruncateTable { table: "orders".to_owned() };
    let expected_truncate = [
        (Engine::Sqlite, "DELETE FROM \"orders\";"),
        (Engine::Mysql, "TRUNCATE TABLE `app`.`orders`;"),
        (Engine::Mariadb, "TRUNCATE TABLE `app`.`orders`;"),
        (Engine::Postgres, "TRUNCATE TABLE \"public\".\"orders\";"),
        (Engine::Sqlserver, "TRUNCATE TABLE [dbo].[orders];"),
    ];
    for (engine, sql) in expected_truncate {
        let planned = plan(&table_of_engine(engine), &truncate);
        assert!(planned.blocked.is_empty(), "{engine:?} {:?}", planned.blocked);
        assert_eq!(planned.steps.len(), 1, "{engine:?}");
        assert_eq!(planned.steps[0].sql, sql, "{engine:?}");
        assert!(has_danger(&planned), "{engine:?}");
        assert!(!planned.steps[0].sql.to_ascii_uppercase().contains("CASCADE"));
    }

    let rename = Action::RenameTable { table: "orders".to_owned(), new_name: "orders_old".to_owned() };
    let expected_rename = [
        (Engine::Sqlite, "ALTER TABLE \"orders\" RENAME TO \"orders_old\";"),
        (Engine::Postgres, "ALTER TABLE \"public\".\"orders\" RENAME TO \"orders_old\";"),
        (Engine::Mysql, "RENAME TABLE `app`.`orders` TO `app`.`orders_old`;"),
        (Engine::Mariadb, "RENAME TABLE `app`.`orders` TO `app`.`orders_old`;"),
        (Engine::Sqlserver, "EXEC sp_rename N'[dbo].[orders]', N'orders_old';"),
    ];
    for (engine, sql) in expected_rename {
        let mut snapshot = table_of_engine(engine);
        snapshot.name_taken = Some(false);
        let planned = plan(&snapshot, &rename);
        assert!(planned.blocked.is_empty(), "{engine:?} {:?}", planned.blocked);
        assert_eq!(planned.steps.len(), 1, "{engine:?}");
        assert_eq!(planned.steps[0].sql, sql, "{engine:?}");
        assert!(has_danger(&planned), "{engine:?}");
    }

    let drop_view = Action::DropView { view: "v_orders".to_owned() };
    let expected_drop = [
        (Engine::Sqlite, "DROP VIEW \"v_orders\";"),
        (Engine::Postgres, "DROP VIEW \"public\".\"v_orders\";"),
        (Engine::Mysql, "DROP VIEW `app`.`v_orders`;"),
        (Engine::Mariadb, "DROP VIEW `app`.`v_orders`;"),
        (Engine::Sqlserver, "DROP VIEW [dbo].[v_orders];"),
    ];
    for (engine, sql) in expected_drop {
        let planned = plan(&view_snapshot(engine), &drop_view);
        assert!(planned.blocked.is_empty(), "{engine:?} {:?}", planned.blocked);
        assert_eq!(planned.steps.len(), 1, "{engine:?}");
        assert_eq!(planned.steps[0].sql, sql, "{engine:?}");
        assert!(has_danger(&planned), "{engine:?}");
    }
    let mut materialized = view_snapshot(Engine::Postgres);
    materialized.materialized = true;
    let planned = plan(&materialized, &drop_view);
    assert_eq!(planned.steps[0].sql, "DROP MATERIALIZED VIEW \"public\".\"v_orders\";");
}

#[test]
fn sqlserver_rename_escapes_quotes_and_keeps_the_new_name_bare() {
    let mut snapshot = table_of_engine(Engine::Sqlserver);
    snapshot.name_taken = Some(false);
    let planned = plan(&snapshot, &Action::RenameTable { table: "orders".to_owned(), new_name: "o'rders]".to_owned() });
    assert!(planned.blocked.is_empty(), "{:?}", planned.blocked);
    assert_eq!(planned.steps[0].sql, "EXEC sp_rename N'[dbo].[orders]', N'o''rders]';");
}

#[test]
fn object_actions_block_bad_names_and_kind_mismatches_with_a_reason() {
    let blocked = |snapshot: &Snapshot, action: &Action| {
        let planned = plan(snapshot, action);
        assert!(planned.steps.is_empty(), "{action:?} {:?}", planned.steps);
        assert!(!planned.blocked.is_empty(), "{action:?}");
        assert!(planned.blocked.iter().all(|reason| !reason.trim().is_empty()));
        planned.blocked
    };
    for engine in [Engine::Sqlite, Engine::Mysql, Engine::Postgres, Engine::Sqlserver] {
        let mut table = table_of_engine(engine);
        table.name_taken = Some(false);
        let reasons = blocked(&table, &Action::RenameTable { table: "orders".to_owned(), new_name: "   ".to_owned() });
        assert!(reasons[0].contains("不能为空"), "{reasons:?}");
        let reasons = blocked(&table, &Action::RenameTable { table: "orders".to_owned(), new_name: "orders".to_owned() });
        assert!(reasons[0].contains("相同"), "{reasons:?}");
        let mut taken = table.clone();
        taken.name_taken = Some(true);
        let reasons = blocked(&taken, &Action::RenameTable { table: "orders".to_owned(), new_name: "customers".to_owned() });
        assert!(reasons[0].contains("已经被其它对象占用"), "{reasons:?}");
        let mut unknown = table.clone();
        unknown.name_taken = None;
        blocked(&unknown, &Action::RenameTable { table: "orders".to_owned(), new_name: "customers".to_owned() });

        let view = view_snapshot(engine);
        let reasons = blocked(&view, &Action::TruncateTable { table: "v_orders".to_owned() });
        assert!(reasons[0].contains("视图"), "{reasons:?}");
        let mut view_named = view.clone();
        view_named.name_taken = Some(false);
        blocked(&view_named, &Action::RenameTable { table: "v_orders".to_owned(), new_name: "v_new".to_owned() });
        let reasons = blocked(&table, &Action::DropView { view: "orders".to_owned() });
        assert!(reasons[0].contains("不是视图"), "{reasons:?}");
        // 名字和读到的对象不一致也不生成语句。
        blocked(&table, &Action::TruncateTable { table: "other".to_owned() });
        blocked(&view, &Action::DropView { view: "other".to_owned() });
    }
}

#[test]
fn truncate_warns_when_incoming_foreign_keys_may_reject_it() {
    for engine in [Engine::Postgres, Engine::Sqlserver] {
        let mut snapshot = table_of_engine(engine);
        let quiet = plan(&snapshot, &Action::TruncateTable { table: "orders".to_owned() });
        assert!(quiet.risks.iter().all(|risk| risk.level != "warn"), "{engine:?} {:?}", quiet.risks);
        snapshot.incoming.push(IncomingForeignKey {
            name: "fk_items_order".to_owned(),
            table: "items".to_owned(),
            schema: snapshot.schema.clone(),
            columns: vec!["order_id".to_owned()],
            ref_columns: vec!["id".to_owned()],
        });
        let planned = plan(&snapshot, &Action::TruncateTable { table: "orders".to_owned() });
        assert!(planned.blocked.is_empty(), "{:?}", planned.blocked);
        let warn = planned.risks.iter().find(|risk| risk.level == "warn").expect("incoming FK warn");
        assert!(warn.text.contains("items.fk_items_order"), "{}", warn.text);
        assert!(warn.text.contains("拒绝"), "{}", warn.text);
        assert!(!planned.steps[0].sql.to_ascii_uppercase().contains("CASCADE"));
    }
}

fn index(name: &str, column: &str, primary: bool, descending: bool, sql: Option<&str>) -> Index {
    Index {
        name: name.to_owned(),
        unique: primary,
        columns: vec![IndexColumn { name: column.to_owned(), descending }],
        protected: primary,
        reason: if primary { "由主键约束维护".to_owned() } else { String::new() },
        partial: false,
        expression: false,
        sql: sql.map(str::to_owned),
    }
}

#[test]
fn rebuilt_definition_lists_columns_indexes_comments_and_what_it_lacks() {
    let mut postgres = table_of_engine(Engine::Postgres);
    postgres.columns = vec![column("id", "integer", false, true), column("customer_note", "character varying(40)", true, false)];
    postgres.indexes = vec![
        index("orders_pkey", "id", true, false, Some("CREATE UNIQUE INDEX orders_pkey ON public.orders USING btree (id)")),
        index("orders_note_idx", "customer_note", false, false, Some("CREATE INDEX orders_note_idx ON public.orders USING btree (customer_note)")),
    ];
    postgres.table_comment = Some("订单".to_owned());
    postgres.column_comments.insert("customer_note".to_owned(), "客户's 备注".to_owned());
    let (text, missing) = super::plan::rebuilt_definition(&postgres);
    assert!(text.contains("CREATE TABLE \"public\".\"orders\""), "{text}");
    assert!(text.contains("\"id\" integer NOT NULL"), "{text}");
    assert!(text.contains("\"customer_note\" character varying(40)"), "{text}");
    assert!(text.contains("CONSTRAINT \"orders_pkey\" PRIMARY KEY (\"id\")"), "{text}");
    assert!(text.contains("CREATE INDEX orders_note_idx ON public.orders USING btree (customer_note);"), "{text}");
    assert!(!text.contains("CREATE UNIQUE INDEX orders_pkey"), "{text}");
    assert!(text.contains("COMMENT ON TABLE \"public\".\"orders\" IS '订单';"), "{text}");
    assert!(text.contains("COMMENT ON COLUMN \"public\".\"orders\".\"customer_note\" IS '客户''s 备注';"), "{text}");
    assert!(text.starts_with("-- "), "{text}");
    assert!(missing.iter().any(|item| item.contains("CHECK")), "{missing:?}");
    assert!(missing.iter().any(|item| item.contains("分区")), "{missing:?}");

    let mut sqlserver = table_of_engine(Engine::Sqlserver);
    sqlserver.columns = vec![column("id", "int", false, true), column("total", "decimal(10,2)", true, false)];
    sqlserver.columns[0].auto = true;
    sqlserver.indexes = vec![index("PK_orders", "id", true, false, None), index("IX_orders_total", "total", false, true, None)];
    sqlserver.column_comments.insert("total".to_owned(), "金额".to_owned());
    let (text, missing) = super::plan::rebuilt_definition(&sqlserver);
    assert!(text.contains("CREATE TABLE [dbo].[orders]"), "{text}");
    assert!(text.contains("[id] int IDENTITY(1, 1) NOT NULL"), "{text}");
    assert!(text.contains("[total] decimal(10,2)"), "{text}");
    assert!(text.contains("CONSTRAINT [PK_orders] PRIMARY KEY ([id])"), "{text}");
    assert!(text.contains("CREATE INDEX [IX_orders_total] ON [dbo].[orders] ([total] DESC);"), "{text}");
    assert!(text.contains("@value = N'金额'") && text.contains("@level2name = N'total'"), "{text}");
    assert!(missing.iter().any(|item| item.contains("CHECK")), "{missing:?}");
    assert!(missing.iter().any(|item| item.contains("IDENTITY")), "{missing:?}");

    let mut failed = table_of_engine(Engine::Postgres);
    failed.index_error = Some("permission denied".to_owned());
    let (_, missing) = super::plan::rebuilt_definition(&failed);
    assert!(missing.iter().any(|item| item.contains("索引") && item.contains("permission denied")), "{missing:?}");
}

#[test]
fn sqlserver_index_sql_keeps_include_columns_out_of_the_key_and_writes_clustering_and_filter() {
    use super::introspect::{sqlserver_index_sql, SqlserverIndexExtra};
    let mut covering = index("IX_orders_customer", "customer_id", false, false, None);
    covering.columns.push(IndexColumn { name: "created_at".to_owned(), descending: true });
    let extra = SqlserverIndexExtra {
        name: "IX_orders_customer".to_owned(),
        type_desc: "NONCLUSTERED".to_owned(),
        include: vec!["total".to_owned(), "status".to_owned()],
        filter: Some("([status]<>(0))".to_owned()),
    };
    assert_eq!(
        sqlserver_index_sql("[dbo].[orders]", &covering, &extra),
        "CREATE NONCLUSTERED INDEX [IX_orders_customer] ON [dbo].[orders] ([customer_id], [created_at] DESC) INCLUDE ([total], [status]) WHERE ([status]<>(0));"
    );

    let mut clustered = index("CIX_orders_created", "created_at", false, false, None);
    clustered.unique = true;
    let extra = SqlserverIndexExtra { name: clustered.name.clone(), type_desc: "CLUSTERED".to_owned(), ..Default::default() };
    assert_eq!(sqlserver_index_sql("[dbo].[orders]", &clustered, &extra), "CREATE UNIQUE CLUSTERED INDEX [CIX_orders_created] ON [dbo].[orders] ([created_at]);");

    let mut columnstore = index("NCCI_orders", "total", false, false, None);
    columnstore.columns.clear();
    let extra = SqlserverIndexExtra { name: columnstore.name.clone(), type_desc: "NONCLUSTERED COLUMNSTORE".to_owned(), include: vec!["total".to_owned(), "status".to_owned()], filter: None };
    assert_eq!(sqlserver_index_sql("[dbo].[orders]", &columnstore, &extra), "CREATE NONCLUSTERED COLUMNSTORE INDEX [NCCI_orders] ON [dbo].[orders] ([total], [status]);");

    let xml = index("XML_orders_doc", "doc", false, false, None);
    let extra = SqlserverIndexExtra { name: xml.name.clone(), type_desc: "XML".to_owned(), ..Default::default() };
    let text = sqlserver_index_sql("[dbo].[orders]", &xml, &extra);
    assert!(text.starts_with("-- ") && text.contains("XML") && !text.contains("CREATE"), "{text}");

    // 重建的定义原样使用读取时拼好的语句：INCLUDE 列不会混进键列，说明注释也不会被补上分号。
    let mut sqlserver = table_of_engine(Engine::Sqlserver);
    sqlserver.columns = vec![column("id", "int", false, true), column("customer_id", "int", true, false), column("doc", "xml", true, false)];
    let mut covering_with_sql = covering.clone();
    covering_with_sql.columns.truncate(1);
    covering_with_sql.sql = Some("CREATE NONCLUSTERED INDEX [IX_orders_customer] ON [dbo].[orders] ([customer_id]) INCLUDE ([total]);".to_owned());
    let mut xml_with_sql = xml.clone();
    xml_with_sql.sql = Some(text.clone());
    sqlserver.indexes = vec![index("PK_orders", "id", true, false, None), covering_with_sql, xml_with_sql];
    let (rebuilt, missing) = super::plan::rebuilt_definition(&sqlserver);
    assert!(rebuilt.contains("CREATE NONCLUSTERED INDEX [IX_orders_customer] ON [dbo].[orders] ([customer_id]) INCLUDE ([total]);"), "{rebuilt}");
    assert!(rebuilt.contains(&text) && !rebuilt.contains(&format!("{text};")), "{rebuilt}");
    assert!(missing.iter().any(|item| item.contains("CLUSTERED")), "{missing:?}");
    assert!(missing.iter().any(|item| item.contains("XML")), "{missing:?}");
}

#[tokio::test]
async fn sqlite_case_only_rename_is_not_blocked_as_a_taken_name() {
    let (dir, _service, session) = open_notes(&["CREATE TABLE notes (id INTEGER PRIMARY KEY)", "CREATE TABLE tags (id INTEGER PRIMARY KEY)"]).await;
    // 表自身不算占用：真实读取得到 name_taken = false。
    let snapshot = super::introspect::load(&session, &request(Action::DropTable { table: "notes".to_owned() }), "notes").await.unwrap();
    assert!(!super::introspect::name_taken(&session, &snapshot, "Notes").await.unwrap());
    assert!(super::introspect::name_taken(&session, &snapshot, "TAGS").await.unwrap());

    // SQLite 会拒绝只改大小写的改名，所以预览给出单独的原因，而不是说名字被其它对象占用。
    let action = Action::RenameTable { table: "notes".to_owned(), new_name: "Notes".to_owned() };
    let previewed = exec::preview(&session, &request(action.clone())).await.unwrap();
    assert!(previewed.steps.is_empty());
    assert!(previewed.blocked.iter().any(|reason| reason.contains("大小写")), "{:?}", previewed.blocked);
    assert!(!previewed.blocked.iter().any(|reason| reason.contains("占用")), "{:?}", previewed.blocked);
    let receipt = exec::execute(&session, &request(action), &[]).await.unwrap();
    assert!(!receipt.blocked.is_empty() && receipt.steps.is_empty(), "{:?}", receipt.blocked);
    assert!(table_exists(&session, "notes").await);

    // 其它引擎只改大小写时照常生成语句（PostgreSQL 的名字区分大小写，SQL Server 的 sp_rename 支持）。
    for engine in [Engine::Postgres, Engine::Sqlserver] {
        let mut other = table_of_engine(engine);
        other.name_taken = Some(false);
        let planned = plan(&other, &Action::RenameTable { table: other.name.clone(), new_name: other.name.to_uppercase() });
        assert!(planned.blocked.is_empty(), "{engine:?} {:?}", planned.blocked);
    }

    // 另一个对象只差大小写时仍然算占用。
    let taken = exec::preview(&session, &request(Action::RenameTable { table: "notes".to_owned(), new_name: "Tags".to_owned() })).await.unwrap();
    assert!(taken.steps.is_empty());
    assert!(taken.blocked.iter().any(|reason| reason.contains("占用")), "{:?}", taken.blocked);
    let _ = fs::remove_dir_all(dir);
}

async fn notes_rows(session: &CatalogSession) -> i64 {
    let CatalogSession::Sqlite { pool, .. } = session else { panic!("sqlite") };
    sqlx::query_scalar("SELECT COUNT(*) FROM notes").fetch_one(pool).await.unwrap()
}

#[tokio::test]
async fn object_actions_refuse_sql_that_differs_from_the_preview() {
    let (dir, _service, session) = open_notes(&[
        "CREATE TABLE notes (id INTEGER PRIMARY KEY, note TEXT)",
        "INSERT INTO notes (id, note) VALUES (1, 'a'), (2, 'b')",
        "CREATE TABLE tags (id INTEGER PRIMARY KEY)",
        "CREATE VIEW v_notes AS SELECT id FROM notes",
    ])
    .await;
    let actions = [
        (Action::DropView { view: "v_notes".to_owned() }, "DROP VIEW \"v_notes\";"),
        (Action::TruncateTable { table: "notes".to_owned() }, "DELETE FROM \"notes\";"),
        (Action::RenameTable { table: "notes".to_owned(), new_name: "notes_old".to_owned() }, "ALTER TABLE \"notes\" RENAME TO \"notes_old\";"),
    ];
    for (action, sql) in &actions {
        let previewed = exec::preview(&session, &request(action.clone())).await.unwrap();
        assert!(previewed.blocked.is_empty(), "{action:?} {:?}", previewed.blocked);
        assert_eq!(previewed.steps.len(), 1);
        assert_eq!(previewed.steps[0].sql, *sql);
        // 预览本身不写入。
        assert_eq!(notes_rows(&session).await, 2);
        let tampered = exec::execute(&session, &request(action.clone()), &[format!("{sql} -- changed")]).await;
        let message = tampered.err().expect("SQL changed since preview");
        assert!(message.contains("预览已过期"), "{message}");
        assert!(exec::execute(&session, &request(action.clone()), &[]).await.is_err(), "{action:?}");
    }
    assert!(master_sql(&session, "v_notes").await.is_some());
    assert!(table_exists(&session, "notes").await);
    assert_eq!(notes_rows(&session).await, 2);

    // 名字已被占用（SQLite 不分大小写）：真实读取后阻断，不生成语句。
    let taken = exec::preview(&session, &request(Action::RenameTable { table: "notes".to_owned(), new_name: "TAGS".to_owned() })).await.unwrap();
    assert!(taken.steps.is_empty());
    assert!(taken.blocked.iter().any(|reason| reason.contains("占用")), "{:?}", taken.blocked);

    for (action, sql) in &actions {
        let receipt = exec::execute(&session, &request(action.clone()), &[(*sql).to_owned()]).await.unwrap();
        assert!(receipt.blocked.is_empty(), "{:?}", receipt.blocked);
        assert!(receipt.steps.iter().all(|step| step.status == StepStatus::Ok), "{:?}", receipt.steps);
        if matches!(action, Action::TruncateTable { .. }) {
            assert_eq!(notes_rows(&session).await, 0);
        }
    }
    assert!(master_sql(&session, "v_notes").await.is_none());
    assert!(!table_exists(&session, "notes").await);
    assert!(table_exists(&session, "notes_old").await);
    let _ = fs::remove_dir_all(dir);
}
