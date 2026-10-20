use super::sql::{compile_where, quote_ident};
use super::store::{customers_fixture, matches, query_page, Cell, Column, Engine, Filter, FilterOp, TableSchema};

fn col(name: &str, type_name: &str) -> Column {
    Column { name: name.to_owned(), type_name: type_name.to_owned(), nullable: true, primary: false, unique: false, auto: false }
}

fn schema_of(engine: Engine) -> TableSchema {
    TableSchema {
        engine,
        schema: "public".to_owned(),
        name: "customers".to_owned(),
        kind: "table",
        key: vec!["id".to_owned()],
        columns: vec![col("id", "integer"), col("name", "text")],
    }
}

fn between(column: &str, low: &str, high: &str) -> Filter {
    Filter { column: column.to_owned(), op: FilterOp::Between, value: Some(low.to_owned()), second: Some(high.to_owned()), values: Vec::new() }
}

fn one_of(column: &str, values: &[&str]) -> Filter {
    Filter { column: column.to_owned(), op: FilterOp::In, value: None, second: None, values: values.iter().map(|item| (*item).to_owned()).collect() }
}

#[test]
fn between_and_in_compile_on_all_four_engines() {
    for engine in [Engine::Postgres, Engine::Mysql, Engine::Mariadb, Engine::Sqlite] {
        let schema = schema_of(engine);
        let range = compile_where(&schema, &[between("id", "5", "10")]).unwrap();
        assert_eq!(range, format!("{} BETWEEN 5 AND 10", quote_ident(engine, "id")));
        let list = compile_where(&schema, &[one_of("name", &["甲", "乙'"])]).unwrap();
        assert_eq!(list, format!("{} IN ('甲', '乙''')", quote_ident(engine, "name")));
    }
}

#[test]
fn between_and_in_match_cells_locally() {
    let id = col("id", "integer");
    let note = col("note", "text");
    assert!(matches(&id, &Cell::Text("5".to_owned()), &between("id", "3", "7")));
    assert!(matches(&id, &Cell::Text("7".to_owned()), &between("id", "3", "7")));
    assert!(!matches(&id, &Cell::Text("2".to_owned()), &between("id", "3", "7")));
    assert!(!matches(&id, &Cell::Text("8".to_owned()), &between("id", "3", "7")));
    assert!(matches(&id, &Cell::Text("50".to_owned()), &between("id", "3", "100")));
    assert!(!matches(&id, &Cell::Text("5".to_owned()), &Filter { column: "id".to_owned(), op: FilterOp::Between, value: None, second: Some("7".to_owned()), values: Vec::new() }));
    assert!(!matches(&note, &Cell::Null, &between("note", "a", "c")));
    assert!(matches(&note, &Cell::Text("b".to_owned()), &between("note", "a", "c")));
    assert!(!matches(&note, &Cell::Text("a".to_owned()), &between("note", "b", "c")));
    assert!(matches(&note, &Cell::Text("乙".to_owned()), &one_of("note", &["甲", "乙"])));
    assert!(!matches(&note, &Cell::Text("丙".to_owned()), &one_of("note", &["甲", "乙"])));
    assert!(!matches(&note, &Cell::Null, &one_of("note", &["甲"])));
}

#[test]
fn between_and_in_filter_fixture_rows() {
    let store = customers_fixture(20);
    let page = query_page(&store, &[between("id", "5", "8")], None, 0, 100).unwrap();
    assert_eq!(page.matched, 4);
    assert!(page.sql.contains("BETWEEN 5 AND 8"));
    let names = query_page(&store, &[one_of("name", &["小河", "000-first"])], None, 0, 100).unwrap();
    assert_eq!(names.matched, 3);
    assert!(names.sql.contains("\"name\" IN ('小河', '000-first')"));
}

#[test]
fn between_and_in_reject_missing_values() {
    let schema = schema_of(Engine::Postgres);
    assert_eq!(compile_where(&schema, &[between("id", "", "7")]).unwrap_err(), "between 需要两个值");
    let missing_second = Filter { column: "id".to_owned(), op: FilterOp::Between, value: Some("5".to_owned()), second: None, values: Vec::new() };
    assert_eq!(compile_where(&schema, &[missing_second.clone()]).unwrap_err(), "between 需要两个值");
    assert_eq!(compile_where(&schema, &[one_of("id", &[])]).unwrap_err(), "IN 筛选至少要一个值");
    let store = customers_fixture(4);
    let error = query_page(&store, &[missing_second], None, 0, 10).unwrap_err();
    assert_eq!(error.0, "between 需要两个值");
    let error = query_page(&store, &[one_of("id", &[])], None, 0, 10).unwrap_err();
    assert_eq!(error.0, "IN 筛选至少要一个值");
}
