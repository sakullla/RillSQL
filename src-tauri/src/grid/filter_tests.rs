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
    assert_eq!(compile_where(&schema, std::slice::from_ref(&missing_second)).unwrap_err(), "between 需要两个值");
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
    for engine in [Engine::Postgres, Engine::Mysql, Engine::Mariadb, Engine::Sqlite, Engine::Sqlserver] {
        let clause = search_clause(engine, &columns, " 甲'x ").expect("非空关键字生成子句");
        let op = if engine == Engine::Postgres { "ILIKE" } else { "LIKE" };
        let (id, name, email, joined) = match engine {
            Engine::Postgres => (r#""id"::text"#.to_owned(), r#""name"::text"#.to_owned(), r#""email"::text"#.to_owned(), r#""joined"::text"#.to_owned()),
            Engine::Mysql | Engine::Mariadb => ("CAST(`id` AS CHAR)".to_owned(), "CAST(`name` AS CHAR)".to_owned(), "CAST(`email` AS CHAR)".to_owned(), "CAST(`joined` AS CHAR)".to_owned()),
            Engine::Sqlite => (r#"CAST("id" AS TEXT)"#.to_owned(), r#"CAST("name" AS TEXT)"#.to_owned(), r#"CAST("email" AS TEXT)"#.to_owned(), r#"CAST("joined" AS TEXT)"#.to_owned()),
            // T-SQL 用括号标识符、NVARCHAR 投影与 N'' 模式串。
            Engine::Sqlserver => ("CAST([id] AS NVARCHAR(MAX))".to_owned(), "CAST([name] AS NVARCHAR(MAX))".to_owned(), "CAST([email] AS NVARCHAR(MAX))".to_owned(), "CAST([joined] AS NVARCHAR(MAX))".to_owned()),
            // 循环只遍历关系型引擎；Redis 没有 SQL 编译。
            Engine::Redis | Engine::Mongodb => unreachable!("专项引擎不参与全文搜索方言测试"),
        };
        let pattern = if engine == Engine::Sqlserver { r#"N'%甲''x%'"# } else { r"'%甲''x%'" };
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
    assert_eq!(compile_where_with_search(&schema, std::slice::from_ref(&filter), Some("小河")).unwrap(), format!("`id` > 10 AND {clause}"));
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

#[test]
fn sqlserver_dialect_quotes_identifiers_and_literals() {
    use super::sql::{quote_ident, text_literal};
    assert_eq!(quote_ident(Engine::Sqlserver, "order"), "[order]");
    assert_eq!(quote_ident(Engine::Sqlserver, "br]acket"), "[br]]acket]");
    // Unicode 字符串用 N''，普通转义仍是双单引号。
    assert_eq!(text_literal(Engine::Sqlserver, "a'b"), "N'a''b'");
    assert_eq!(text_literal(Engine::Mysql, "a'b"), "'a''b'");
}

#[test]
fn sqlserver_cell_literals_cover_binary_and_bool() {
    use super::sql::sql_cell;
    use super::store::Column;
    fn col(type_name: &str) -> Column {
        Column { name: "c".to_owned(), type_name: type_name.to_owned(), nullable: true, primary: false, unique: false, auto: false }
    }
    // 0x 前缀的二进制字面量；空二进制是 0x。
    assert_eq!(sql_cell(Engine::Sqlserver, Some(&col("varbinary")), &Cell::Text("0xAB10".to_owned())), "0xab10".to_owned());
    assert_eq!(sql_cell(Engine::Sqlserver, Some(&col("varbinary")), &Cell::Text("".to_owned())), "0x".to_owned());
    // bit 列走 TRUE/FALSE 字面量。
    assert_eq!(sql_cell(Engine::Sqlserver, Some(&col("bit")), &Cell::Text("1".to_owned())), "TRUE".to_owned());
    assert_eq!(sql_cell(Engine::Sqlserver, Some(&col("bit")), &Cell::Text("f".to_owned())), "FALSE".to_owned());
    // 文本列 N''，数字列保持数字。
    assert_eq!(sql_cell(Engine::Sqlserver, Some(&col("nvarchar")), &Cell::Text("值'x".to_owned())), "N'值''x'".to_owned());
    assert_eq!(sql_cell(Engine::Sqlserver, Some(&col("int")), &Cell::Text("42".to_owned())), "42".to_owned());
}

#[test]
fn sqlserver_paging_uses_offset_fetch_and_defaults_to_a_stable_order() {
    let schema = schema_of(Engine::Sqlserver);
    let sort = Sort { column: "id".to_owned(), descending: false };
    // 有排序时直接 OFFSET/FETCH。
    let sorted = browse_sql_with_search(&schema, &[], None, Some(&sort), 20, 40).unwrap();
    assert_eq!(sorted, "SELECT * FROM [public].[customers] ORDER BY [id] ASC OFFSET 40 ROWS FETCH NEXT 20 ROWS ONLY");
    // 无排序时补恒定序（OFFSET/FETCH 必须跟在 ORDER BY 后面）。
    let unsorted = browse_sql_with_search(&schema, &[], None, None, 20, 40).unwrap();
    assert_eq!(unsorted, "SELECT * FROM [public].[customers] ORDER BY (SELECT NULL) OFFSET 40 ROWS FETCH NEXT 20 ROWS ONLY");
    // 首页同样显式 OFFSET 0。
    let first = browse_sql_with_search(&schema, &[], None, Some(&sort), 20, 0).unwrap();
    assert!(first.ends_with("ORDER BY [id] ASC OFFSET 0 ROWS FETCH NEXT 20 ROWS ONLY"));
}

#[test]
fn sqlserver_empty_insert_uses_default_values() {
    use super::sql::insert_sql;
    let schema = schema_of(Engine::Sqlserver);
    assert_eq!(insert_sql(&schema, &[]), "INSERT INTO [public].[customers] DEFAULT VALUES");
}

#[test]
fn sqlserver_value_family_maps_tsql_type_names() {
    use super::sql::value_family_test::test_names;
    let families = test_names();
    assert_eq!(families.get("datetime2"), Some(&super::sql::ValueFamily::Datetime));
    assert_eq!(families.get("datetimeoffset"), Some(&super::sql::ValueFamily::Datetime));
    assert_eq!(families.get("smalldatetime"), Some(&super::sql::ValueFamily::Datetime));
    assert_eq!(families.get("money"), Some(&super::sql::ValueFamily::Decimal));
    assert_eq!(families.get("smallmoney"), Some(&super::sql::ValueFamily::Decimal));
    assert_eq!(families.get("bit"), Some(&super::sql::ValueFamily::Bool));
    assert_eq!(families.get("uniqueidentifier"), Some(&super::sql::ValueFamily::Text));
    assert_eq!(families.get("nvarchar"), Some(&super::sql::ValueFamily::Text));
    assert_eq!(families.get("varbinary"), Some(&super::sql::ValueFamily::Binary));
}
