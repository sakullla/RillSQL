use super::sql::{browse_sql, browse_sql_with_search, compile_where, compile_where_with_search, quote_ident, search_clause};
use super::store::{customers_fixture, matches, query_page, Cell, Column, Engine, Filter, FilterOp, Sort, TableSchema};

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

/// 覆盖各类型家族的表：文本/数字/日期参与整表搜索，二进制/几何/未知类型不参与。
fn rich_schema(engine: Engine) -> TableSchema {
    TableSchema {
        engine,
        schema: "public".to_owned(),
        name: "customers".to_owned(),
        kind: "table",
        key: vec!["id".to_owned()],
        columns: vec![
            col("id", "integer"),
            col("name", "text"),
            col("email", "varchar(64)"),
            col("joined", "date"),
            col("avatar", "bytea"),
            col("shape", "geometry"),
            col("mystery", ""),
        ],
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

#[test]
fn search_clause_compiles_or_like_in_each_engine_dialect() {
    let columns = rich_schema(Engine::Postgres).columns;
    for engine in [Engine::Postgres, Engine::Mysql, Engine::Mariadb, Engine::Sqlite] {
        let clause = search_clause(engine, &columns, " 甲'x ").expect("非空关键字生成子句");
        let op = if engine == Engine::Postgres { "ILIKE" } else { "LIKE" };
        let (id, name, email, joined) = match engine {
            Engine::Postgres => (r#""id"::text"#.to_owned(), r#""name"::text"#.to_owned(), r#""email"::text"#.to_owned(), r#""joined"::text"#.to_owned()),
            Engine::Mysql | Engine::Mariadb => ("CAST(`id` AS CHAR)".to_owned(), "CAST(`name` AS CHAR)".to_owned(), "CAST(`email` AS CHAR)".to_owned(), "CAST(`joined` AS CHAR)".to_owned()),
            Engine::Sqlite => (r#"CAST("id" AS TEXT)"#.to_owned(), r#"CAST("name" AS TEXT)"#.to_owned(), r#"CAST("email" AS TEXT)"#.to_owned(), r#"CAST("joined" AS TEXT)"#.to_owned()),
        };
        let pattern = r"'%甲''x%'";
        assert_eq!(clause, format!("({id} {op} {pattern} ESCAPE '!' OR {name} {op} {pattern} ESCAPE '!' OR {email} {op} {pattern} ESCAPE '!' OR {joined} {op} {pattern} ESCAPE '!')"), "engine {engine:?}");
    }
    assert_eq!(search_clause(Engine::Postgres, &columns, ""), None);
    assert_eq!(search_clause(Engine::Postgres, &columns, "   "), None);
}

#[test]
fn search_clause_matches_pattern_characters_literally() {
    let columns = schema_of(Engine::Sqlite).columns;
    let percent = search_clause(Engine::Sqlite, &columns, "50%").unwrap();
    assert!(percent.contains(r"'%50!%%' ESCAPE '!'"));
    assert!(!percent.contains("50%% ESCAPE"));
    let underscore = search_clause(Engine::Sqlite, &columns, "a_b!c").unwrap();
    assert!(underscore.contains(r"'%a!_b!!c%' ESCAPE '!'"));
}

#[test]
fn search_clause_skips_binary_geometry_and_unknown_columns() {
    let columns = rich_schema(Engine::Mysql).columns;
    let clause = search_clause(Engine::Mysql, &columns, "x").unwrap();
    assert!(clause.contains("`id`"));
    assert!(clause.contains("`name`"));
    assert!(!clause.contains("avatar"));
    assert!(!clause.contains("shape"));
    assert!(!clause.contains("mystery"));
    let unsearchable = vec![col("avatar", "blob"), col("shape", "geography")];
    assert_eq!(search_clause(Engine::Mysql, &unsearchable, "x"), Some("1 = 0".to_owned()));
}

#[test]
fn search_clause_combines_with_filters_sort_and_paging() {
    let schema = schema_of(Engine::Mysql);
    let clause = search_clause(Engine::Mysql, &schema.columns, "小河").unwrap();
    let filter = Filter { column: "id".to_owned(), op: FilterOp::Gt, value: Some("10".to_owned()), second: None, values: Vec::new() };
    assert_eq!(compile_where_with_search(&schema, &[filter.clone()], Some("小河")).unwrap(), format!("`id` > 10 AND {clause}"));
    assert_eq!(compile_where_with_search(&schema, &[], Some("  ")).unwrap(), "");
    assert_eq!(compile_where_with_search(&schema, &[], None).unwrap(), "");
    assert_eq!(compile_where_with_search(&schema, &[one_of("id", &[])], Some("小河")).unwrap_err(), "IN 筛选至少要一个值");

    let sql = browse_sql_with_search(&schema, &[filter], Some("小河"), None, 20, 40).unwrap();
    assert_eq!(sql, format!("SELECT * FROM `public`.`customers` WHERE `id` > 10 AND {clause} LIMIT 20 OFFSET 40"));
    let sort = Sort { column: "id".to_owned(), descending: true };
    let sorted = browse_sql_with_search(&schema, &[], Some("小河"), Some(&sort), 10, 0).unwrap();
    assert_eq!(sorted, format!("SELECT * FROM `public`.`customers` WHERE {clause} ORDER BY `id` DESC LIMIT 10"));
    // 没有关键字时与既有 browse_sql 完全一致（存量行为不变）。
    assert_eq!(browse_sql_with_search(&schema, &[], None, Some(&sort), 10, 0).unwrap(), browse_sql(&schema, &[], Some(&sort), 10, 0).unwrap());
}
