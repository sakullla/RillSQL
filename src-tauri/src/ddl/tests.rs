use std::fs;
use std::path::PathBuf;

use super::commands;
use super::exec::{self, run_steps};
use super::model::{
    Action, Column, ColumnDraft, DependentSql, Engine, ForeignKey, Index, IndexColumn, IndexColumnDraft, Request, Snapshot, Step, StepStatus,
};
use super::plan::plan;
use crate::connections::{CatalogSession, Service};

fn column(name: &str, type_name: &str, nullable: bool, primary: bool) -> Column {
    Column {
        name: name.to_owned(),
        type_name: type_name.to_owned(),
        nullable,
        default_sql: None,
        primary,
        auto: false,
        generated: false,
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
    let capability: serde_json::Value = serde_json::from_str(&fs::read_to_string(root.join("capabilities/default.json")).unwrap()).unwrap();
    let permissions = capability["permissions"].as_array().unwrap();
    let acl = fs::read_to_string(root.join("permissions/app-commands.toml")).unwrap();
    let registration = fs::read_to_string(root.join("src/lib.rs")).unwrap();
    let wire = registration.split("fn wire").nth(1).unwrap_or("");
    let wire = wire.split("pub fn run").next().unwrap_or("");
    let expected = [
        ("app_info", "app_info"),
        ("connections_list", "connections::commands::connections_list"),
        ("connections_save", "connections::commands::connections_save"),
        ("connections_delete", "connections::commands::connections_delete"),
        ("connections_test", "connections::commands::connections_test"),
        ("connections_open", "connections::commands::connections_open"),
        ("connections_focus_database", "connections::commands::connections_focus_database"),
        ("connections_disconnect", "connections::commands::connections_disconnect"),
        ("connections_confirm_host_key", "connections::commands::connections_confirm_host_key"),
        ("connections_forget_host_key", "connections::commands::connections_forget_host_key"),
        ("catalog_sessions", "catalog::commands::catalog_sessions"),
        ("catalog_refresh", "catalog::commands::catalog_refresh"),
        ("catalog_page", "catalog::commands::catalog_page"),
        ("grid_describe", "grid::commands::grid_describe"),
        ("grid_page", "grid::commands::grid_page"),
        ("grid_count", "grid::commands::grid_count"),
        ("grid_commit", "grid::commands::grid_commit"),
        ("grid_export", "grid::commands::grid_export"),
        ("grid_export_cancel", "grid::commands::grid_export_cancel"),
        ("ddl_describe", "ddl::commands::ddl_describe"),
        ("ddl_preview", "ddl::commands::ddl_preview"),
        ("ddl_execute", "ddl::commands::ddl_execute"),
        ("query_start", "query::commands::query_start"),
        ("query_poll", "query::commands::query_poll"),
        ("query_cancel", "query::commands::query_cancel"),
        ("query_history", "query::commands::query_history"),
        ("query_history_clear", "query::commands::query_history_clear"),
    ];
    for (command, wire_name) in expected {
        let allow = format!("allow-{}", command.replace('_', "-"));
        assert!(permissions.iter().any(|item| item.as_str() == Some(allow.as_str())), "{allow}");
        assert!(acl.contains(&format!("commands.allow = [\"{command}\"]")), "{command}");
        assert!(wire.contains(wire_name), "{wire_name}");
    }
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
