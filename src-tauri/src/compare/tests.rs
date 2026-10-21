use std::collections::BTreeMap;
use std::fs;

use super::data::diff_data;
use super::model::{
    needs_ephemeral_postgres, Category, ColumnShape, DataColumn, DataInput, DataKey, DataTable, Engine, ForeignKeyShape, IndexShape, Kind, ObjectDef, Side, TableShape,
};
use super::read::absorb_snapshot;
use super::structure::{diff_structure, render_script, table_data_keys};
use crate::grid::{insert_sql, Cell, Column, TableSchema};

fn column(name: &str, type_name: &str, nullable: bool, primary: bool) -> ColumnShape {
    ColumnShape { name: name.to_owned(), type_name: type_name.to_owned(), nullable, default_sql: String::new(), primary }
}

fn shape(columns: Vec<ColumnShape>, indexes: Vec<IndexShape>) -> TableShape {
    TableShape { columns, indexes, foreign_keys: Vec::new(), indexes_read: true, foreign_keys_read: true }
}

fn orders(type_name: &str) -> ObjectDef {
    ObjectDef::table("orders", shape(vec![column("id", "integer", false, true), column("note", type_name, true, false)], Vec::new()))
}

fn index(name: &str, unique: bool, columns: &[(&str, bool)]) -> IndexShape {
    IndexShape {
        name: name.to_owned(),
        unique,
        columns: columns.iter().map(|(name, descending)| ((*name).to_owned(), *descending)).collect(),
        protected: false,
        reason: String::new(),
        partial: false,
        expression: false,
        sql: String::new(),
    }
}

fn row(pairs: &[(&str, Option<&str>)]) -> BTreeMap<String, Cell> {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_owned(), match value { Some(text) => Cell::Text((*text).to_owned()), None => Cell::Null }))
        .collect()
}

fn data_table(name: &str, columns: Vec<DataColumn>, keys: Vec<DataKey>, baseline: Vec<BTreeMap<String, Cell>>, target: Vec<BTreeMap<String, Cell>>) -> DataTable {
    DataTable { name: name.to_owned(), qualify: String::new(), columns, keys, keys_unknown: false, chosen_key: None, baseline, target, read_error: None, only_side: None }
}

fn orders_columns() -> Vec<DataColumn> {
    vec![
        DataColumn { name: "id".to_owned(), type_name: "integer".to_owned(), nullable: false },
        DataColumn { name: "note".to_owned(), type_name: "text".to_owned(), nullable: true },
    ]
}

fn primary() -> Vec<DataKey> {
    vec![DataKey { name: "orders_pkey".to_owned(), columns: vec!["id".to_owned()], primary: true }]
}

#[test]
fn same_engine_structure_script_is_not_a_refusal() {
    let baseline = Side::ready("postgres", "app", "public", vec![orders("text")]);
    let target = Side::ready("postgres", "app", "public", vec![]);
    let report = diff_structure(&baseline, &target);
    assert!(report.refused.is_none());
    assert!(report.only_baseline.iter().any(|item| item.name == "orders"));
    assert!(report.script.contains("CREATE TABLE"));
    assert!(report.script.contains("orders"));
    assert!(report.script.contains("生成时没有执行"));
    assert!(!report.script.contains("没有结构差异"));
}

#[test]
fn column_type_difference_is_an_alter_statement() {
    let report = diff_structure(&Side::ready("postgres", "app", "public", vec![orders("text")]), &Side::ready("postgres", "app", "public", vec![orders("varchar(20)")]));
    assert!(report.changed.iter().any(|item| item.name == "orders"));
    assert!(report.script.contains("ALTER TABLE"));
    assert!(report.script.contains("note"));
    assert!(report.script.to_ascii_lowercase().contains("text"));
}

#[test]
fn identical_structure_has_no_change_statements() {
    let report = diff_structure(&Side::ready("mysql", "app", "", vec![orders("int")]), &Side::ready("mysql", "app", "", vec![orders("int")]));
    assert!(report.script.contains("没有结构差异"));
    let upper = report.script.to_ascii_uppercase();
    assert!(!upper.contains("CREATE"));
    assert!(!upper.contains("ALTER"));
    assert!(!upper.contains("DROP"));
}

#[test]
fn different_engines_do_not_generate_a_script() {
    let report = diff_structure(&Side::ready("mysql", "app", "", vec![orders("int")]), &Side::ready("mariadb", "app", "", vec![orders("int")]));
    assert!(report.refused.unwrap().contains("引擎不同"));
    assert!(report.script.is_empty());
    assert!(report.only_baseline.is_empty());
}

fn foreign_key(name: &str, column: &str, ref_table: &str) -> ForeignKeyShape {
    ForeignKeyShape {
        name: name.to_owned(),
        columns: vec![column.to_owned()],
        ref_schema: String::new(),
        ref_table: ref_table.to_owned(),
        ref_columns: vec!["id".to_owned()],
        on_delete: String::new(),
        on_update: String::new(),
    }
}

fn table_with_fk(name: &str, key: ForeignKeyShape) -> ObjectDef {
    let mut object = ObjectDef::table(name, shape(vec![column("id", "integer", false, true), column(&key.columns[0], "integer", true, false)], vec![]));
    object.table.as_mut().unwrap().foreign_keys.push(key);
    object
}

fn at(script: &str, needle: &str) -> usize {
    script.find(needle).unwrap_or_else(|| panic!("missing {needle} in {script}"))
}

#[test]
fn drops_views_and_referencing_tables_before_referenced_tables() {
    let notes = ObjectDef::table("notes", shape(vec![column("id", "integer", false, true)], vec![]));
    let tickets = table_with_fk("tickets", foreign_key("tickets_note", "note_id", "notes"));
    let view = ObjectDef::defined(Kind::View, "v_notes", "CREATE VIEW v_notes AS SELECT id FROM notes");
    let cases = [
        ("postgres", "app", "public", "DROP VIEW \"public\".\"v_notes\"", "DROP TABLE \"public\".\"tickets\"", "DROP TABLE \"public\".\"notes\"", "DROP CONSTRAINT \"tickets_note\""),
        ("mysql", "app", "", "DROP VIEW `app`.`v_notes`", "DROP TABLE `app`.`tickets`", "DROP TABLE `app`.`notes`", "DROP FOREIGN KEY `tickets_note`"),
        ("sqlite", "main", "", "DROP VIEW \"v_notes\"", "DROP TABLE \"tickets\"", "DROP TABLE \"notes\"", ""),
    ];
    for (engine, database, schema, drop_view, drop_child, drop_parent, drop_fk) in cases {
        let report = diff_structure(
            &Side::ready(engine, database, schema, vec![]),
            &Side::ready(engine, database, schema, vec![notes.clone(), tickets.clone(), view.clone()]),
        );
        let script = &report.script;
        assert!(at(script, drop_view) < at(script, drop_parent), "{engine}\n{script}");
        assert!(at(script, drop_child) < at(script, drop_parent), "{engine}\n{script}");
        if drop_fk.is_empty() {
            assert!(!script.to_ascii_uppercase().contains("DROP CONSTRAINT"), "{script}");
        } else {
            assert!(at(script, drop_fk) < at(script, drop_parent), "{engine}\n{script}");
            assert!(at(script, drop_fk) < at(script, drop_child), "{engine}\n{script}");
        }
    }
}

#[test]
fn drops_a_view_before_the_view_it_selects() {
    let base = ObjectDef::defined(Kind::View, "v_base", "CREATE VIEW v_base AS SELECT 1");
    let top = ObjectDef::defined(Kind::View, "v_top", "CREATE VIEW v_top AS SELECT * FROM v_base");
    let report = diff_structure(&Side::ready("postgres", "app", "public", vec![]), &Side::ready("postgres", "app", "public", vec![base, top]));
    assert!(at(&report.script, "DROP VIEW \"public\".\"v_top\"") < at(&report.script, "DROP VIEW \"public\".\"v_base\""), "{}", report.script);
}

#[test]
fn creates_foreign_keys_after_every_new_table() {
    let users = ObjectDef::table("users", shape(vec![column("id", "integer", false, true)], vec![]));
    let orders = table_with_fk("orders", foreign_key("orders_user", "user_id", "users"));
    let cases = [
        ("postgres", "public", "CREATE TABLE \"public\".\"orders\"", "CREATE TABLE \"public\".\"users\""),
        ("mysql", "", "CREATE TABLE `app`.`orders`", "CREATE TABLE `app`.`users`"),
        ("mariadb", "", "CREATE TABLE `app`.`orders`", "CREATE TABLE `app`.`users`"),
    ];
    for (engine, schema, create_orders, create_users) in cases {
        let report = diff_structure(
            &Side::ready(engine, "app", schema, vec![users.clone(), orders.clone()]),
            &Side::ready(engine, "app", schema, vec![]),
        );
        let script = &report.script;
        let orders_at = at(script, create_orders);
        let body_end = orders_at + script[orders_at..].find(';').unwrap_or_else(|| panic!("{engine} {script}"));
        assert!(!script[orders_at..=body_end].contains("FOREIGN KEY"), "{engine}\n{script}");
        let fk_at = at(script, "FOREIGN KEY");
        assert!(orders_at < fk_at && at(script, create_users) < fk_at, "{engine}\n{script}");
        assert!(script.contains("ALTER TABLE"), "{engine}\n{script}");
    }
}

#[test]
fn sqlite_new_table_keeps_the_foreign_key_inside_create_table() {
    let orders = table_with_fk("orders", foreign_key("orders_user", "user_id", "users"));
    let report = diff_structure(&Side::ready("sqlite", "main", "", vec![orders]), &Side::ready("sqlite", "main", "", vec![]));
    let script = &report.script;
    let create_at = at(script, "CREATE TABLE \"orders\"");
    let body_end = create_at + script[create_at..].find(';').expect("statement");
    assert!(script[create_at..=body_end].contains("FOREIGN KEY"), "{script}");
    assert!(!script.to_ascii_uppercase().contains("ALTER TABLE"), "{script}");
}

#[test]
fn cyclic_foreign_keys_are_not_inlined_and_drop_before_either_table() {
    let users = table_with_fk("users", foreign_key("users_order", "order_id", "orders"));
    let orders = table_with_fk("orders", foreign_key("orders_user", "user_id", "users"));
    let created = diff_structure(&Side::ready("postgres", "app", "public", vec![users.clone(), orders.clone()]), &Side::ready("postgres", "app", "public", vec![]));
    let script = &created.script;
    for marker in ["CREATE TABLE \"public\".\"orders\"", "CREATE TABLE \"public\".\"users\""] {
        let start = at(script, marker);
        let end = start + script[start..].find(';').expect("statement");
        assert!(!script[start..=end].contains("FOREIGN KEY"), "{script}");
        assert!(start < at(script, "CONSTRAINT \"orders_user\""), "{script}");
        assert!(start < at(script, "CONSTRAINT \"users_order\""), "{script}");
    }
    let dropped = diff_structure(&Side::ready("postgres", "app", "public", vec![]), &Side::ready("postgres", "app", "public", vec![users, orders]));
    let script = &dropped.script;
    let first_drop = at(script, "DROP TABLE");
    assert!(at(script, "DROP CONSTRAINT \"orders_user\"") < first_drop, "{script}");
    assert!(at(script, "DROP CONSTRAINT \"users_order\"") < first_drop, "{script}");
}

#[test]
fn index_read_failure_omits_index_statements_but_keeps_table_diffs() {
    let mut baseline = Side::ready(
        "postgres",
        "app",
        "public",
        vec![ObjectDef::table(
            "orders",
            shape(vec![column("id", "integer", false, true), column("note", "text", true, false)], vec![index("orders_note", false, &[("note", false)])]),
        )],
    );
    baseline.category_errors.push((Category::Index, "permission denied".to_owned()));
    let target = Side::ready("postgres", "app", "public", vec![orders("varchar(20)")]);
    let report = diff_structure(&baseline, &target);
    assert!(report.changed.iter().any(|item| item.kind == "表" && item.name == "orders"));
    assert!(report.incomplete);
    assert!(report.script.contains("不完整"));
    assert!(report.script.contains("索引读取失败"));
    let upper = report.script.to_ascii_uppercase();
    assert!(!upper.contains("CREATE INDEX"));
    assert!(!upper.contains("DROP INDEX"));
    assert!(upper.contains("ALTER TABLE"));
}

#[test]
fn routines_are_definition_text_only() {
    let baseline = Side::ready("postgres", "app", "public", vec![ObjectDef::defined(Kind::Function, "add_customer", "CREATE FUNCTION add_customer() RETURNS integer LANGUAGE sql AS $$ SELECT 1 $$")]);
    let target = Side::ready("postgres", "app", "public", vec![ObjectDef::defined(Kind::Function, "add_customer", "CREATE FUNCTION add_customer() RETURNS integer LANGUAGE sql AS $$ SELECT 2 $$")]);
    let report = diff_structure(&baseline, &target);
    assert!(report.changed.iter().any(|item| item.kind == "函数" && item.detail.contains("定义文本")));
    assert!(report.script.contains("SELECT 1"));
    assert!(!report.script.contains("例程表单"));
}

#[test]
fn cancelled_listing_is_incomplete_and_does_not_drop_or_create() {
    let baseline = Side::ready("postgres", "app", "public", vec![orders("integer")]);
    let mut target = Side::ready("postgres", "app", "public", vec![]);
    target.cancelled = true;
    target.listing_complete = false;
    let report = diff_structure(&baseline, &target);
    assert!(report.only_baseline.iter().any(|item| item.name == "orders"));
    assert!(report.incomplete);
    assert!(report.script.contains("不完整"));
    assert!(!report.script.contains("没有结构差异"));
    assert!(!report.script.to_ascii_uppercase().contains("CREATE"));
    assert!(!report.script.to_ascii_uppercase().contains("DROP"));
}

#[test]
fn data_script_inserts_updates_and_deletes_without_executing_a_connection() {
    let table = data_table(
        "orders",
        orders_columns(),
        primary(),
        vec![row(&[("id", Some("1")), ("note", Some("a"))]), row(&[("id", Some("3")), ("note", None)]), row(&[("id", Some("4")), ("note", None)])],
        vec![row(&[("id", Some("1")), ("note", Some("b"))]), row(&[("id", Some("2")), ("note", Some("x"))]), row(&[("id", Some("4")), ("note", Some(""))])],
    );
    let report = diff_data(&DataInput { baseline_engine: "sqlite".to_owned(), target_engine: "sqlite".to_owned(), tables: vec![table], cancelled: false });
    assert!(report.refused.is_none());
    assert!(report.script.contains("INSERT"));
    assert!(report.script.contains("DELETE"));
    assert!(report.script.contains("UPDATE"));
    assert!(report.script.contains("NULL"));
    assert!(report.diffs.iter().any(|item| item.action == "update" && item.key == "4"));
    let schema = TableSchema {
        engine: crate::grid::Engine::Sqlite,
        schema: String::new(),
        name: "orders".to_owned(),
        kind: "table",
        columns: vec![
            Column { name: "id".to_owned(), type_name: "integer".to_owned(), nullable: false, primary: true, unique: false, auto: false },
            Column { name: "note".to_owned(), type_name: "text".to_owned(), nullable: true, primary: false, unique: false, auto: false },
        ],
        key: vec!["id".to_owned()],
    };
    let expected = insert_sql(&schema, &[("id".to_owned(), Cell::Text("3".to_owned())), ("note".to_owned(), Cell::Null)]);
    assert!(report.script.contains(&expected));
    assert!(report.script.contains("生成时没有执行"));
}

#[test]
fn tables_without_a_key_do_not_invent_rows() {
    let table = data_table("notes", vec![DataColumn { name: "label".to_owned(), type_name: "text".to_owned(), nullable: true }], Vec::new(), vec![row(&[("label", Some("a"))])], Vec::new());
    let report = diff_data(&DataInput { baseline_engine: "sqlite".to_owned(), target_engine: "sqlite".to_owned(), tables: vec![table], cancelled: false });
    assert!(report.script.contains("无法对应行"));
    assert!(!report.script.to_ascii_uppercase().contains("INSERT"));
    assert!(!report.script.to_ascii_uppercase().contains("DELETE"));
}

#[test]
fn multiple_unique_keys_wait_for_a_choice() {
    let mut waiting = data_table(
        "twin",
        vec![
            DataColumn { name: "email".to_owned(), type_name: "text".to_owned(), nullable: true },
            DataColumn { name: "code".to_owned(), type_name: "text".to_owned(), nullable: true },
        ],
        vec![DataKey { name: "email".to_owned(), columns: vec!["email".to_owned()], primary: false }, DataKey { name: "code".to_owned(), columns: vec!["code".to_owned()], primary: false }],
        vec![row(&[("email", Some("a@b.c")), ("code", Some("a"))])],
        Vec::new(),
    );
    let held = diff_data(&DataInput { baseline_engine: "mysql".to_owned(), target_engine: "mysql".to_owned(), tables: vec![waiting.clone()], cancelled: false });
    assert!(held.script.contains("选定前"));
    assert!(!held.script.to_ascii_uppercase().contains("INSERT"));
    waiting.chosen_key = Some("email".to_owned());
    let chosen = diff_data(&DataInput { baseline_engine: "mysql".to_owned(), target_engine: "mysql".to_owned(), tables: vec![waiting], cancelled: false });
    assert!(chosen.script.contains("INSERT"));
    assert!(chosen.script.contains("email"));
    assert!(chosen.notes.iter().any(|note| note.contains("email")));
}

#[test]
fn unsafe_literals_skip_the_row() {
    let table = data_table(
        "geo",
        vec![DataColumn { name: "id".to_owned(), type_name: "integer".to_owned(), nullable: false }, DataColumn { name: "g".to_owned(), type_name: "geometry".to_owned(), nullable: true }],
        vec![DataKey { name: "PRIMARY".to_owned(), columns: vec!["id".to_owned()], primary: true }],
        vec![row(&[("id", Some("1")), ("g", Some("POINT(1 2)"))])],
        Vec::new(),
    );
    let report = diff_data(&DataInput { baseline_engine: "postgres".to_owned(), target_engine: "postgres".to_owned(), tables: vec![table], cancelled: false });
    assert!(report.diffs.iter().any(|item| item.action == "rejected" && item.key == "1"));
    assert!(report.script.contains("未能生成语句"));
    assert!(!report.script.to_ascii_uppercase().contains("INSERT"));
    assert!(!report.script.contains("POINT(1 2)"));
}

#[test]
fn cancelled_data_keeps_rows_already_read() {
    let table = data_table("orders", orders_columns(), primary(), vec![row(&[("id", Some("1")), ("note", Some("a"))])], Vec::new());
    let report = diff_data(&DataInput { baseline_engine: "sqlite".to_owned(), target_engine: "sqlite".to_owned(), tables: vec![table], cancelled: true });
    assert!(report.incomplete);
    assert!(report.script.contains("不完整"));
    assert!(report.script.contains("INSERT"));
    assert!(!report.script.contains("没有数据差异"));
}

#[test]
fn one_sided_columns_are_not_written_as_null() {
    let columns = vec![
        DataColumn { name: "id".to_owned(), type_name: "integer".to_owned(), nullable: false },
        DataColumn { name: "note".to_owned(), type_name: "text".to_owned(), nullable: true },
        DataColumn { name: "extra".to_owned(), type_name: "text".to_owned(), nullable: true },
    ];
    let differing = data_table(
        "orders",
        columns.clone(),
        primary(),
        vec![row(&[("id", Some("1")), ("note", Some("a")), ("only_base", Some("keep"))])],
        vec![row(&[("id", Some("1")), ("note", Some("b")), ("extra", Some("t"))])],
    );
    let updated = diff_data(&DataInput { baseline_engine: "sqlite".to_owned(), target_engine: "sqlite".to_owned(), tables: vec![differing], cancelled: false });
    assert!(updated.script.contains("列集合"));
    assert!(updated.script.contains("only_base"));
    assert!(updated.script.contains("extra"));
    let updated_sql = statements_only(&updated.script);
    assert!(updated_sql.contains("UPDATE"));
    assert!(updated_sql.contains("note"));
    assert!(!updated_sql.contains("extra"));
    assert!(!updated_sql.contains("only_base"));
    assert!(!updated_sql.to_ascii_uppercase().contains("NULL"));

    let inserted = data_table(
        "orders",
        columns,
        primary(),
        vec![row(&[("id", Some("2")), ("note", Some("b")), ("only_base", Some("keep"))])],
        Vec::new(),
    );
    let report = diff_data(&DataInput { baseline_engine: "sqlite".to_owned(), target_engine: "sqlite".to_owned(), tables: vec![inserted], cancelled: false });
    assert!(report.script.contains("只在对照有 extra"));
    assert!(report.script.contains("只在基准有 only_base"));
    let sql = statements_only(&report.script);
    assert!(sql.contains("INSERT"));
    assert!(sql.contains("b"));
    assert!(!sql.contains("extra"));
    assert!(!sql.contains("only_base"));
    assert!(!sql.to_ascii_uppercase().contains("NULL"));
    assert!(!report.script.contains("没有数据差异"));
}

fn statements_only(script: &str) -> String {
    script.lines().filter(|line| !line.trim_start().starts_with("--")).collect::<Vec<_>>().join("\n")
}

#[test]
fn one_sided_table_is_structural() {
    let mut table = data_table("orders", orders_columns(), primary(), Vec::new(), Vec::new());
    table.only_side = Some("基准".to_owned());
    let report = diff_data(&DataInput { baseline_engine: "sqlite".to_owned(), target_engine: "sqlite".to_owned(), tables: vec![table], cancelled: false });
    assert!(report.script.contains("结构差异"));
    assert!(!report.script.to_ascii_uppercase().contains("INSERT"));
}

fn pk_index(columns: &[(&str, bool)]) -> IndexShape {
    let mut index = index("orders_pkey", true, columns);
    index.reason = "由主键约束维护".to_owned();
    index
}

fn orders_shape(primary: bool) -> ObjectDef {
    let columns = vec![column("id", "integer", false, primary), column("note", "text", true, false)];
    let indexes = if primary { vec![pk_index(&[("id", false)])] } else { Vec::new() };
    ObjectDef::table("orders", shape(columns, indexes))
}

#[test]
fn primary_key_script_drops_the_target_key_before_adding_the_baseline_key() {
    let dropped = diff_structure(&Side::ready("postgres", "app", "public", vec![orders_shape(false)]), &Side::ready("postgres", "app", "public", vec![orders_shape(true)]));
    assert!(dropped.changed.iter().any(|item| item.detail.contains("主键")));
    assert!(dropped.script.contains("DROP CONSTRAINT \"orders_pkey\""));
    assert!(!dropped.script.contains("ADD PRIMARY KEY"));
    assert!(!dropped.script.contains("没有结构差异"));

    let columns = vec![column("id", "integer", false, true), column("note", "text", true, false)];
    let baseline = ObjectDef::table("orders", shape(columns.clone(), vec![pk_index(&[("id", false), ("note", false)])]));
    let target = ObjectDef::table("orders", shape(columns, vec![pk_index(&[("id", false)])]));
    let changed = diff_structure(&Side::ready("postgres", "app", "public", vec![baseline]), &Side::ready("postgres", "app", "public", vec![target]));
    let drop_at = changed.script.find("DROP CONSTRAINT").expect("drop");
    let add_at = changed.script.find("ADD PRIMARY KEY (\"id\", \"note\")").expect("add");
    assert!(drop_at < add_at);

    let with_pk = orders_shape(true);
    let without_pk = orders_shape(false);
    let add_only = diff_structure(&Side::ready("mysql", "shop", "", vec![with_pk.clone()]), &Side::ready("mysql", "shop", "", vec![without_pk.clone()]));
    assert!(add_only.script.contains("ADD PRIMARY KEY"));
    assert!(!add_only.script.to_ascii_uppercase().contains("DROP PRIMARY KEY"));
    let drop_only = diff_structure(&Side::ready("mysql", "shop", "", vec![without_pk]), &Side::ready("mysql", "shop", "", vec![with_pk]));
    assert!(drop_only.script.contains("DROP PRIMARY KEY"));
    assert!(!drop_only.script.contains("ADD PRIMARY KEY"));
}

#[test]
fn sqlite_foreign_key_only_difference_rebuilds_the_target_table() {
    let columns = vec![column("id", "integer", false, true), column("customer_id", "integer", true, false)];
    let mut with_fk = shape(columns.clone(), vec![pk_index(&[("id", false)])]);
    with_fk.foreign_keys.push(ForeignKeyShape {
        name: "orders_customer".to_owned(),
        columns: vec!["customer_id".to_owned()],
        ref_schema: String::new(),
        ref_table: "customers".to_owned(),
        ref_columns: vec!["id".to_owned()],
        on_delete: String::new(),
        on_update: String::new(),
    });
    let without_fk = shape(columns, vec![pk_index(&[("id", false)])]);
    let report = diff_structure(
        &Side::ready("sqlite", "main", "", vec![ObjectDef::table("orders", with_fk)]),
        &Side::ready("sqlite", "main", "", vec![ObjectDef::table("orders", without_fk)]),
    );
    assert!(report.changed.iter().any(|item| item.kind == "外键"));
    assert!(report.script.contains("CREATE TABLE"));
    assert!(report.script.contains("FOREIGN KEY"));
    assert!(report.script.contains("orders_customer"));
    assert!(!report.script.contains("没有结构差异"));
}

fn customer_fk() -> ForeignKeyShape {
    ForeignKeyShape {
        name: "orders_customer".to_owned(),
        columns: vec!["customer_id".to_owned()],
        ref_schema: String::new(),
        ref_table: "customers".to_owned(),
        ref_columns: vec!["id".to_owned()],
        on_delete: String::new(),
        on_update: String::new(),
    }
}

#[test]
fn sqlite_rebuild_is_withheld_when_foreign_keys_are_not_fully_known() {
    let columns = |note: &str| vec![column("id", "integer", false, true), column("customer_id", "integer", true, false), column("note", note, true, false)];
    let mut baseline = shape(columns("text"), vec![pk_index(&[("id", false)])]);
    baseline.foreign_keys.push(customer_fk());
    let mut target = shape(columns("varchar(20)"), vec![pk_index(&[("id", false)])]);
    target.foreign_keys.push(customer_fk());
    let kept = diff_structure(
        &Side::ready("sqlite", "main", "", vec![ObjectDef::table("orders", baseline.clone())]),
        &Side::ready("sqlite", "main", "", vec![ObjectDef::table("orders", target.clone())]),
    );
    assert!(kept.script.contains("DROP TABLE"));
    assert!(kept.script.contains("FOREIGN KEY"));
    assert!(kept.script.contains("orders_customer"));

    baseline.foreign_keys_read = false;
    target.foreign_keys_read = false;
    let mut unread = Side::ready("sqlite", "main", "", vec![ObjectDef::table("orders", baseline.clone())]);
    unread.category_errors.push((Category::ForeignKey, "fk denied".to_owned()));
    let withheld = diff_structure(&unread, &Side::ready("sqlite", "main", "", vec![ObjectDef::table("orders", target.clone())]));
    assert!(withheld.changed.iter().any(|item| item.kind == "表" && item.name == "orders"));
    assert!(withheld.incomplete);
    assert!(withheld.script.contains("不完整"));
    assert!(withheld.script.contains("外键没有读全"));
    let upper = withheld.script.to_ascii_uppercase();
    assert!(!upper.contains("DROP TABLE"));
    assert!(!upper.contains("CREATE TABLE"));
    assert!(!upper.contains("FOREIGN KEY"));
    assert!(!withheld.script.contains("没有结构差异"));

    let mut blocked = Side::ready("sqlite", "main", "", vec![ObjectDef::table("orders", {
        let mut known = baseline.clone();
        known.foreign_keys_read = true;
        known
    })]);
    blocked.category_errors.push((Category::ForeignKey, "fk denied".to_owned()));
    let mut blocked_target = target.clone();
    blocked_target.foreign_keys_read = true;
    let category_blocked = diff_structure(&blocked, &Side::ready("sqlite", "main", "", vec![ObjectDef::table("orders", blocked_target)]));
    assert!(category_blocked.changed.iter().any(|item| item.kind == "表"));
    assert!(category_blocked.incomplete);
    assert!(!category_blocked.script.to_ascii_uppercase().contains("DROP TABLE"));
    assert!(!category_blocked.script.to_ascii_uppercase().contains("FOREIGN KEY"));
}

#[test]
fn create_statements_are_qualified_on_the_target_side() {
    let baseline_view = ObjectDef::defined(Kind::View, "v_orders", "CREATE OR REPLACE VIEW \"public\".\"v_orders\" AS SELECT 1");
    let target_view = ObjectDef::defined(Kind::View, "v_orders", "CREATE OR REPLACE VIEW \"sales\".\"v_orders\" AS SELECT 2");
    let views = diff_structure(&Side::ready("postgres", "app", "public", vec![baseline_view]), &Side::ready("postgres", "app", "sales", vec![target_view]));
    assert!(views.script.contains("DROP VIEW \"sales\".\"v_orders\""));
    assert!(views.script.contains("CREATE OR REPLACE VIEW \"sales\".\"v_orders\" AS SELECT 1"));
    assert!(!views.script.contains("\"public\".\"v_orders\""));

    let baseline_fn = ObjectDef::defined(Kind::Function, "add_customer", "CREATE FUNCTION public.add_customer() RETURNS integer LANGUAGE sql AS $$ SELECT 1 $$");
    let target_fn = ObjectDef::defined(Kind::Function, "add_customer", "CREATE FUNCTION sales.add_customer() RETURNS integer LANGUAGE sql AS $$ SELECT 2 $$");
    let functions = diff_structure(&Side::ready("postgres", "app", "public", vec![baseline_fn]), &Side::ready("postgres", "app", "sales", vec![target_fn]));
    assert!(functions.script.contains("DROP FUNCTION \"sales\".\"add_customer\""));
    assert!(functions.script.contains("CREATE FUNCTION \"sales\".\"add_customer\"()"));
    assert!(functions.script.contains("SELECT 1"));
    assert!(!functions.script.contains("public.add_customer"));

    let mut baseline_trigger = ObjectDef::defined(Kind::Trigger, "touch", "CREATE TRIGGER touch BEFORE UPDATE ON public.orders FOR EACH ROW EXECUTE FUNCTION public.touch()");
    baseline_trigger.relation = "orders".to_owned();
    let mut target_trigger = ObjectDef::defined(Kind::Trigger, "touch", "CREATE TRIGGER touch BEFORE UPDATE ON sales.orders FOR EACH ROW EXECUTE FUNCTION sales.touch()");
    target_trigger.relation = "orders".to_owned();
    let triggers = diff_structure(&Side::ready("postgres", "app", "public", vec![baseline_trigger]), &Side::ready("postgres", "app", "sales", vec![target_trigger]));
    assert!(triggers.script.contains("DROP TRIGGER \"touch\" ON \"sales\".\"orders\""));
    assert!(triggers.script.contains("CREATE TRIGGER \"touch\" BEFORE UPDATE ON \"sales\".\"orders\""));
    assert!(!triggers.script.contains("CREATE TRIGGER \"sales\".\"touch\""));
    assert!(!triggers.script.contains("\"sales\".\"touch\""));
    assert!(!triggers.script.contains("ON public.orders"));

    let baseline_mysql = ObjectDef::defined(
        Kind::View,
        "v_orders",
        "CREATE ALGORITHM=UNDEFINED DEFINER=`root`@`localhost` SQL SECURITY DEFINER VIEW `shop`.`v_orders` AS select 1 AS `id`",
    );
    let target_mysql = ObjectDef::defined(Kind::View, "v_orders", "CREATE VIEW `other`.`v_orders` AS select 2");
    let mysql = diff_structure(&Side::ready("mysql", "shop", "", vec![baseline_mysql]), &Side::ready("mysql", "other", "", vec![target_mysql]));
    assert!(mysql.script.contains("DROP VIEW `other`.`v_orders`"));
    assert!(mysql.script.contains("VIEW `other`.`v_orders` AS select 1"));
    assert!(!mysql.script.contains("`shop`.`v_orders`"));

    let mut partial = index("orders_note", false, &[("note", false)]);
    partial.partial = true;
    partial.sql = "CREATE INDEX orders_note ON public.orders USING btree (note) WHERE (note IS NOT NULL)".to_owned();
    let indexed = ObjectDef::table("orders", shape(vec![column("id", "integer", false, true), column("note", "text", true, false)], vec![partial]));
    let plain = orders("integer");
    let indexes = diff_structure(&Side::ready("postgres", "app", "public", vec![indexed]), &Side::ready("postgres", "app", "sales", vec![plain]));
    assert!(indexes.script.contains("ON \"sales\".\"orders\""));
    assert!(!indexes.script.contains("ON public.orders"));
}

#[test]
fn marked_differences_are_not_called_absent_when_no_statement_was_emitted() {
    let marked = render_script(&[], &[], false, false, "没有结构差异。");
    assert!(!marked.contains("没有结构差异"));
    let clear = render_script(&[], &[], false, true, "没有结构差异。");
    assert!(clear.contains("没有结构差异"));
}

#[test]
fn index_or_foreign_key_failure_keeps_columns_for_the_table_diff() {
    let snapshot = crate::ddl::Snapshot::sample_index_failure();
    let mut object = ObjectDef::table("orders", shape(vec![], vec![]));
    let errors = absorb_snapshot(&mut object, &snapshot);
    assert!(object.read_error.is_none());
    assert_eq!(object.table.as_ref().unwrap().columns.len(), 1);
    assert!(!object.table.as_ref().unwrap().indexes_read);
    assert!(!object.table.as_ref().unwrap().foreign_keys_read);
    assert!(errors.iter().any(|(category, _)| *category == Category::Index));
    assert!(errors.iter().any(|(category, _)| *category == Category::ForeignKey));
    let mut baseline = Side::ready("postgres", "app", "public", vec![object]);
    baseline.category_errors = errors;
    let report = diff_structure(&baseline, &Side::ready("postgres", "app", "public", vec![orders("varchar(20)")]));
    assert!(report.changed.iter().any(|item| item.kind == "表" && item.name == "orders"));
    assert!(report.incomplete);
    assert!(report.script.contains("不完整"));
    assert!(report.script.to_ascii_uppercase().contains("ALTER TABLE"));
    assert!(!report.script.to_ascii_uppercase().contains("CREATE INDEX"));
    assert!(!report.script.to_ascii_uppercase().contains("DROP INDEX"));
    assert!(!report.script.to_ascii_uppercase().contains("FOREIGN KEY"));
    assert!(!report.script.contains("PRIMARY KEY"));
    assert!(!report.script.contains("DROP CONSTRAINT"));
}

#[test]
fn unread_index_metadata_is_not_a_missing_primary_key() {
    let mut unread = shape(vec![column("id", "integer", false, false), column("note", "text", true, false)], Vec::new());
    unread.indexes_read = false;
    assert!(table_data_keys(Engine::Postgres, Some(&unread), Some(&unread)).is_err());
    assert!(table_data_keys(Engine::Mysql, Some(&unread), Some(&unread)).is_err());
    let known = shape(vec![column("id", "integer", false, true)], vec![pk_index(&[("id", false)])]);
    assert!(table_data_keys(Engine::Postgres, Some(&known), Some(&unread)).is_err());

    let mut sqlite_unread = shape(vec![column("id", "integer", false, true), column("note", "text", true, false)], Vec::new());
    sqlite_unread.indexes_read = false;
    let sqlite_keys = table_data_keys(Engine::Sqlite, Some(&sqlite_unread), Some(&sqlite_unread)).expect("sqlite primary key comes from columns");
    assert!(sqlite_keys.iter().any(|key| key.primary && key.columns == ["id".to_owned()]));

    let mut table = data_table("orders", orders_columns(), Vec::new(), vec![row(&[("id", Some("1")), ("note", Some("a"))])], Vec::new());
    table.keys_unknown = true;
    let report = diff_data(&DataInput { baseline_engine: "postgres".to_owned(), target_engine: "postgres".to_owned(), tables: vec![table], cancelled: false });
    assert!(report.script.contains("索引或主键没有读到"));
    assert!(!report.script.contains("没有主键或唯一键"));
    assert!(!report.script.contains("没有数据差异"));
    assert!(!report.script.to_ascii_uppercase().contains("INSERT"));
    assert!(!report.script.to_ascii_uppercase().contains("UPDATE"));
    assert!(!report.script.to_ascii_uppercase().contains("DELETE"));

    let mut left = Side::ready("postgres", "app", "public", vec![ObjectDef::table("orders", unread.clone())]);
    left.category_errors.push((Category::Index, "permission denied".to_owned()));
    let listed = diff_structure(&left, &Side::ready("postgres", "app", "public", vec![ObjectDef::table("orders", unread)]));
    let choice = listed.tables.iter().find(|item| item.name == "orders").expect("orders");
    assert!(choice.keys.is_empty());
    assert!(!choice.needs_key_choice);

    let sqlite_report = diff_structure(
        &Side::ready("sqlite", "main", "", vec![ObjectDef::table("orders", sqlite_unread.clone())]),
        &Side::ready("sqlite", "main", "", vec![ObjectDef::table("orders", sqlite_unread.clone())]),
    );
    let sqlite_choice = sqlite_report.tables.iter().find(|item| item.name == "orders").expect("orders");
    assert!(sqlite_choice.keys.iter().any(|key| key.primary));
    assert!(!sqlite_choice.needs_key_choice);

    let mut sqlite_no_pk = shape(vec![column("code", "text", false, false)], vec![index("code_key", true, &[("code", false)])]);
    sqlite_no_pk.indexes_read = false;
    assert!(table_data_keys(Engine::Sqlite, Some(&sqlite_no_pk), Some(&sqlite_no_pk)).is_err());
    assert!(table_data_keys(Engine::Sqlite, Some(&sqlite_unread), Some(&sqlite_no_pk)).is_err());
    let mut pg_column_flag = shape(vec![column("id", "integer", false, true)], Vec::new());
    pg_column_flag.indexes_read = false;
    assert!(table_data_keys(Engine::Postgres, Some(&pg_column_flag), Some(&pg_column_flag)).is_err());
    let readable_unique = shape(vec![column("code", "text", false, false)], vec![index("code_key", true, &[("code", false)])]);
    let unique_keys = table_data_keys(Engine::Sqlite, Some(&readable_unique), Some(&readable_unique)).expect("unique key was read");
    assert!(unique_keys.iter().any(|key| !key.primary && key.columns == ["code".to_owned()]));

    let codes = diff_structure(
        &Side::ready("sqlite", "main", "", vec![ObjectDef::table("codes", sqlite_no_pk.clone())]),
        &Side::ready("sqlite", "main", "", vec![ObjectDef::table("codes", sqlite_no_pk)]),
    );
    let codes_choice = codes.tables.iter().find(|item| item.name == "codes").expect("codes");
    assert!(codes_choice.keys.is_empty());
    assert!(!codes_choice.needs_key_choice);
}

#[test]
fn missing_key_column_does_not_drop_rows_or_claim_no_data_difference() {
    let missing_on_baseline = data_table(
        "orders",
        orders_columns(),
        primary(),
        vec![row(&[("note", Some("a"))])],
        vec![row(&[("id", Some("2")), ("note", Some("x"))])],
    );
    let dropped = diff_data(&DataInput { baseline_engine: "postgres".to_owned(), target_engine: "postgres".to_owned(), tables: vec![missing_on_baseline], cancelled: false });
    assert!(dropped.script.contains("键列"));
    assert!(dropped.script.contains("无法对应"));
    assert!(!dropped.script.contains("没有数据差异"));
    let upper = dropped.script.to_ascii_uppercase();
    assert!(!upper.contains("INSERT"));
    assert!(!upper.contains("UPDATE"));
    assert!(!upper.contains("DELETE"));

    let empty_target = data_table("orders", orders_columns(), primary(), vec![row(&[("note", Some("a"))])], Vec::new());
    let quiet = diff_data(&DataInput { baseline_engine: "postgres".to_owned(), target_engine: "postgres".to_owned(), tables: vec![empty_target], cancelled: false });
    assert!(quiet.script.contains("键列"));
    assert!(!quiet.script.contains("没有数据差异"));
    assert!(!quiet.script.to_ascii_uppercase().contains("INSERT"));

    let mut noted = data_table("orders", orders_columns(), primary(), vec![row(&[("note", Some("a"))])], vec![row(&[("id", Some("2")), ("note", Some("x"))])]);
    noted.read_error = Some("page failed".to_owned());
    let kept = diff_data(&DataInput { baseline_engine: "postgres".to_owned(), target_engine: "postgres".to_owned(), tables: vec![noted], cancelled: false });
    assert!(kept.script.contains("读取失败"));
    assert!(kept.script.contains("page failed"));
    assert!(kept.script.contains("键列"));
    assert!(!kept.script.to_ascii_uppercase().contains("DELETE"));
    assert!(!kept.script.contains("没有数据差异"));
}

#[test]
fn postgres_other_database_needs_an_ephemeral_session() {
    assert!(needs_ephemeral_postgres("postgres", "app", "other"));
    assert!(!needs_ephemeral_postgres("postgres", "app", "app"));
    assert!(!needs_ephemeral_postgres("mysql", "app", "other"));
    assert_eq!(Engine::Mysql.as_str(), "mysql");
}

#[test]
fn compare_commands_are_registered_and_do_not_focus_or_execute() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let capability: serde_json::Value = serde_json::from_str(&fs::read_to_string(root.join("capabilities/default.json")).unwrap()).unwrap();
    let permissions = capability["permissions"].as_array().unwrap();
    let acl = fs::read_to_string(root.join("permissions/app-commands.toml")).unwrap();
    let registration = fs::read_to_string(root.join("src/lib.rs")).unwrap();
    let wire = registration.split("fn wire").nth(1).unwrap_or("");
    let wire = wire.split("pub fn run").next().unwrap_or("");
    for command in ["compare_structure", "compare_data", "compare_cancel", "compare_schemas"] {
        let allow = format!("allow-{}", command.replace('_', "-"));
        assert!(permissions.iter().any(|item| item.as_str() == Some(allow.as_str())), "{allow}");
        assert!(acl.contains(&format!("commands.allow = [\"{command}\"]")), "{command}");
        assert!(wire.contains(&format!("compare::commands::{command}")), "{command}");
    }
    let reader = include_str!("read.rs");
    let commands = include_str!("commands.rs");
    let schemas = reader.split("pub async fn list_schemas").nth(1).unwrap();
    assert!(schemas.contains("hold_session"));
    assert!(schemas.contains("metadata_sql"));
    assert!(!schemas.contains("focus_database"));
    assert!(!reader.contains("focus_database"));
    assert!(!commands.contains("focus_database"));
    assert!(!commands.contains("query_start"));
    assert!(!commands.contains("ddl_execute"));
    let service = include_str!("../connections/service.rs");
    let start = service.find("fn open_ephemeral_postgres").unwrap();
    let body = &service[start..];
    let end = body.find("pub fn disconnect").unwrap();
    let body = &body[..end];
    assert!(!body.contains("focus_database"));
    assert!(!body.contains(".insert("));
    assert!(!body.contains(".save("));
}

#[tokio::test]
async fn ephemeral_open_keeps_the_current_session_and_saved_config() {
    let dir = std::env::temp_dir().join(format!("rillsql-compare-ephemeral-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    let database = dir.join("notes.sqlite");
    let setup = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).create_if_missing(true))
        .await
        .unwrap();
    setup.close().await;
    let service = crate::connections::Service::new_for_test(dir.join("connections.sqlite"));
    service.open_sqlite_for_test("notes", "笔记", &database).await.unwrap();
    let before = service.list().await.unwrap();
    let error = match service.open_ephemeral_postgres("notes", "other").await {
        Err(error) => error,
        Ok(_) => panic!("SQLite 不应打开临时读取会话"),
    };
    assert!(error.contains("只有 PostgreSQL"));
    assert!(service.catalog_session("notes").is_some());
    let after = service.list().await.unwrap();
    assert_eq!(before[0].saved.database, after[0].saved.database);
    assert_eq!(before[0].saved.file_path, after[0].saved.file_path);
    let _ = fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn schema_list_rejects_a_closed_or_non_postgres_session_without_focusing() {
    let dir = std::env::temp_dir().join(format!("rillsql-compare-schemas-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    let database = dir.join("notes.sqlite");
    let setup = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).create_if_missing(true))
        .await
        .unwrap();
    setup.close().await;
    let service = crate::connections::Service::new_for_test(dir.join("connections.sqlite"));
    let missing = super::read::list_schemas(&service, "pg", "other").await.unwrap_err();
    assert!(missing.contains("还没有打开"));
    service.open_sqlite_for_test("notes", "笔记", &database).await.unwrap();
    let before = service.list().await.unwrap();
    let error = super::read::list_schemas(&service, "notes", "other").await.unwrap_err();
    assert!(error.contains("只有 PostgreSQL"));
    assert!(service.catalog_session("notes").is_some());
    let after = service.list().await.unwrap();
    assert_eq!(before[0].saved.database, after[0].saved.database);
    assert_eq!(before[0].saved.file_path, after[0].saved.file_path);
    let _ = fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn sqlite_compare_reads_both_connections_and_does_not_execute_the_script() {
    let dir = std::env::temp_dir().join(format!("rillsql-compare-sqlite-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    let baseline_file = dir.join("baseline.sqlite");
    let target_file = dir.join("target.sqlite");
    let baseline = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&baseline_file).create_if_missing(true))
        .await
        .unwrap();
    sqlx::query("CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT)")
        .execute(&baseline)
        .await
        .unwrap();
    sqlx::query("CREATE TABLE orders (id INTEGER PRIMARY KEY, note TEXT)")
        .execute(&baseline)
        .await
        .unwrap();
    sqlx::query("INSERT INTO orders (id, note) VALUES (1, 'a'), (3, NULL)")
        .execute(&baseline)
        .await
        .unwrap();
    baseline.close().await;
    let target = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&target_file).create_if_missing(true))
        .await
        .unwrap();
    sqlx::query("CREATE TABLE orders (id INTEGER PRIMARY KEY, note INTEGER)")
        .execute(&target)
        .await
        .unwrap();
    sqlx::query("INSERT INTO orders (id, note) VALUES (1, 2), (2, 9)")
        .execute(&target)
        .await
        .unwrap();
    target.close().await;

    let service = crate::connections::Service::new_for_test(dir.join("connections.sqlite"));
    service.open_sqlite_for_test("base", "基准", &baseline_file).await.unwrap();
    service.open_sqlite_for_test("other", "对照", &target_file).await.unwrap();
    let structure = super::read::read_structure(
        &service,
        &super::model::StructureRequest {
            id: "structure".to_owned(),
            baseline: super::model::SideRequest { connection_id: "base".to_owned(), database: "main".to_owned(), schema: String::new(), engine: "sqlite".to_owned() },
            target: super::model::SideRequest { connection_id: "other".to_owned(), database: "main".to_owned(), schema: String::new(), engine: "sqlite".to_owned() },
        },
    )
    .await;
    assert!(structure.refused.is_none(), "{:?}", structure.refused);
    assert!(structure.only_baseline.iter().any(|item| item.name == "customers"), "{:?}", structure.only_baseline);
    assert!(structure.script.contains("customers"));
    assert!(structure.script.to_ascii_uppercase().contains("CREATE TABLE"));
    assert!(structure.changed.iter().any(|item| item.name == "orders"), "{:?}", structure.changed);
    assert!(structure.script.to_ascii_uppercase().contains("TEXT"));

    let session = service.catalog_session("other").unwrap();
    let missing = crate::grid::describe(&session, "main", "", "customers", "sqlite").await.unwrap_err();
    assert!(missing.contains("找不到"));
    let described = crate::grid::describe(&session, "main", "", "orders", "sqlite").await.unwrap();
    let note = described.columns.iter().find(|column| column.name == "note").unwrap();
    assert!(note.type_name.eq_ignore_ascii_case("INTEGER"));

    let data = super::read::read_data(
        &service,
        &super::model::DataRequest {
            id: "data".to_owned(),
            baseline: super::model::SideRequest { connection_id: "base".to_owned(), database: "main".to_owned(), schema: String::new(), engine: "sqlite".to_owned() },
            target: super::model::SideRequest { connection_id: "other".to_owned(), database: "main".to_owned(), schema: String::new(), engine: "sqlite".to_owned() },
            tables: vec![super::model::DataTableRequest { name: "orders".to_owned(), key: None }, super::model::DataTableRequest { name: "customers".to_owned(), key: None }],
        },
    )
    .await;
    assert!(data.refused.is_none(), "{:?}", data.refused);
    assert!(data.script.contains("INSERT"), "{}", data.script);
    assert!(data.script.contains("DELETE"), "{}", data.script);
    assert!(data.script.contains("UPDATE"), "{}", data.script);
    assert!(data.script.contains("结构差异"), "{}", data.script);
    let crate::connections::CatalogSession::Sqlite { pool, .. } = service.catalog_session("other").unwrap() else {
        panic!("对照会话还在");
    };
    let note: i64 = sqlx::query_scalar("SELECT note FROM orders WHERE id = 1").fetch_one(&pool).await.unwrap();
    assert_eq!(note, 2);
    let extra: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM orders WHERE id = 2").fetch_one(&pool).await.unwrap();
    assert_eq!(extra, 1);
    let _ = fs::remove_dir_all(&dir);
}
