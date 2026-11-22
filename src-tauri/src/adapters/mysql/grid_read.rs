//! MySQL / MariaDB 的表网格读写：描述表结构、读一页、计数、逐条执行、外键与流式导出。都在已打开的连接池上进行。

use crate::adapters::grid::{db_err, ExportSink};
use crate::connections::model::Engine;
use crate::ddl::model::LiteralQuoting;
use crate::grid::{push_foreign_key, Cell, Column, ForeignKey, TableSchema};

/// `engine_hint` 区分 MySQL 与 MariaDB（两者共用连接池类型）。
pub(in crate::adapters) async fn describe(pool: &mysql_async::Pool, database: &str, table: &str, engine_hint: &str) -> Result<TableSchema, String> {
    if database.is_empty() {
        return Err("需要数据库名，没有另开连接。".to_owned());
    }
    let engine = if engine_hint == "mariadb" { Engine::Mariadb } else { Engine::Mysql };
    describe_mysql(pool, engine, database, table).await
}

pub(in crate::adapters) async fn fetch_cells(pool: &mysql_async::Pool, sql: &str) -> Result<Vec<Vec<Cell>>, String> {
    let (_names, rows) = mysql_rows(pool, sql).await?;
    Ok(rows)
}

pub(in crate::adapters) async fn fetch_count(pool: &mysql_async::Pool, sql: &str) -> Result<i64, String> {
    let (_names, rows) = mysql_rows(pool, sql).await?;
    match rows.first().and_then(|row| row.first()) {
        Some(Cell::Text(text)) => text.parse::<i64>().map_err(|_| format!("计数无效 {text}")),
        _ => Err("没有计数".to_owned()),
    }
}

pub(in crate::adapters) async fn execute(pool: &mysql_async::Pool, sql: &str) -> Result<u64, String> {
    let mut conn = pool.get_conn().await.map_err(db_err)?;
    let mut result = mysql_async::prelude::Queryable::query_iter(&mut conn, sql).await.map_err(db_err)?;
    let affected = result.affected_rows();
    while result.next().await.map_err(db_err)?.is_some() {}
    Ok(affected)
}

pub(in crate::adapters) async fn foreign_keys(pool: &mysql_async::Pool, database: &str, table: &str) -> Result<Vec<ForeignKey>, String> {
    if database.is_empty() {
        return Ok(Vec::new());
    }
    let mut conn = pool.get_conn().await.map_err(db_err)?;
    let rows: Vec<mysql_async::Row> = mysql_async::prelude::Queryable::exec(
        &mut conn,
        "SELECT CONSTRAINT_NAME, COLUMN_NAME, REFERENCED_TABLE_SCHEMA, REFERENCED_TABLE_NAME, REFERENCED_COLUMN_NAME FROM information_schema.KEY_COLUMN_USAGE WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? AND REFERENCED_TABLE_NAME IS NOT NULL ORDER BY CONSTRAINT_NAME, ORDINAL_POSITION",
        (database.to_owned(), table.to_owned()),
    )
    .await
    .map_err(db_err)?;
    let mut list: Vec<ForeignKey> = Vec::new();
    for row in &rows {
        let name = mysql_text(row, 0)?;
        let column = mysql_text(row, 1)?;
        let ref_schema = mysql_text(row, 2)?;
        let ref_table = mysql_text(row, 3)?;
        let ref_column = mysql_text(row, 4)?;
        let scope = if ref_schema.is_empty() { database.to_owned() } else { ref_schema };
        push_foreign_key(&mut list, &name, &column, &scope, &ref_table, &ref_column);
    }
    Ok(list)
}

/// 流式导出：逐行从服务器取，列名已知时直接用。
pub(in crate::adapters) async fn stream(pool: &mysql_async::Pool, sql: &str, known_names: Option<Vec<String>>, sink: &mut dyn ExportSink) {
    let mut conn = match pool.get_conn().await {
        Ok(conn) => conn,
        Err(err) => return sink.fail(&crate::error_text::chain(&err)),
    };
    let mut result = match mysql_async::prelude::Queryable::query_iter(&mut conn, sql).await {
        Ok(result) => result,
        Err(err) => return sink.fail(&crate::error_text::chain(&err)),
    };
    let names = known_names.unwrap_or_else(|| result.columns_ref().iter().map(|column| column.name_str().into_owned()).collect());
    if !sink.start(&names) {
        return;
    }
    loop {
        if !sink.proceed() {
            return;
        }
        match result.next().await {
            Ok(Some(row)) => {
                let cells = (0..names.len()).map(|index| mysql_cell(row.as_ref(index).unwrap_or(&mysql_async::Value::NULL))).collect::<Vec<_>>();
                if !sink.row(&cells) {
                    return;
                }
            }
            Ok(None) => return sink.finish(),
            Err(err) => return sink.fail(&crate::error_text::chain(&err)),
        }
    }
}

fn mysql_cell(value: &mysql_async::Value) -> Cell {
    match value {
        mysql_async::Value::NULL => Cell::Null,
        mysql_async::Value::Bytes(bytes) => Cell::Text(String::from_utf8_lossy(bytes).into_owned()),
        mysql_async::Value::Int(number) => Cell::Text(number.to_string()),
        mysql_async::Value::UInt(number) => Cell::Text(number.to_string()),
        mysql_async::Value::Float(number) => Cell::Text(number.to_string()),
        mysql_async::Value::Double(number) => Cell::Text(number.to_string()),
        mysql_async::Value::Date(year, month, day, hour, minute, second, micros) => {
            Cell::Text(format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}.{micros:06}"))
        }
        mysql_async::Value::Time(negative, days, hour, minute, second, micros) => {
            Cell::Text(format!("{}{days} {hour:02}:{minute:02}:{second:02}.{micros:06}", if *negative { "-" } else { "" }))
        }
    }
}

async fn mysql_rows(pool: &mysql_async::Pool, sql: &str) -> Result<(Vec<String>, Vec<Vec<Cell>>), String> {
    let mut conn = pool.get_conn().await.map_err(db_err)?;
    let mut result = mysql_async::prelude::Queryable::query_iter(&mut conn, sql).await.map_err(db_err)?;
    let names = result.columns_ref().iter().map(|column| column.name_str().into_owned()).collect::<Vec<_>>();
    let mut rows = Vec::new();
    while let Some(row) = result.next().await.map_err(db_err)? {
        let cells = (0..names.len()).map(|index| mysql_cell(row.as_ref(index).unwrap_or(&mysql_async::Value::NULL))).collect();
        rows.push(cells);
    }
    Ok((names, rows))
}

async fn describe_mysql(pool: &mysql_async::Pool, engine: Engine, database: &str, table: &str) -> Result<TableSchema, String> {
    let mut conn = pool.get_conn().await.map_err(db_err)?;
    // 会话 sql_mode 决定 ENUM 候选值里的反斜杠是不是转义符：网格下拉与结构页用同一个解析规则。
    let sql_mode_row: Option<mysql_async::Row> = mysql_async::prelude::Queryable::query_first(&mut conn, "SELECT @@session.sql_mode")
        .await
        .map_err(db_err)?;
    let quoting = LiteralQuoting::from_sql_mode(&sql_mode_row.as_ref().map(|row| mysql_text(row, 0).unwrap_or_default()).unwrap_or_default());
    let kind_row: Option<mysql_async::Row> = mysql_async::prelude::Queryable::exec_first(
        &mut conn,
        "SELECT TABLE_TYPE FROM information_schema.tables WHERE table_schema = ? AND table_name = ?",
        (database.to_owned(), table.to_owned()),
    )
    .await
    .map_err(db_err)?;
    let kind_text = mysql_text(kind_row.as_ref().ok_or_else(|| format!("找不到表或视图 {database}.{table}"))?, 0)?;
    let kind = if kind_text.eq_ignore_ascii_case("VIEW") { "view" } else { "table" };
    let column_rows: Vec<mysql_async::Row> = mysql_async::prelude::Queryable::exec(
        &mut conn,
        "SELECT COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_KEY, EXTRA, COLUMN_DEFAULT FROM information_schema.columns WHERE table_schema = ? AND table_name = ? ORDER BY ORDINAL_POSITION",
        (database.to_owned(), table.to_owned()),
    )
    .await
    .map_err(db_err)?;
    let index_rows: Vec<mysql_async::Row> = mysql_async::prelude::Queryable::exec(
        &mut conn,
        "SELECT INDEX_NAME, NON_UNIQUE, SEQ_IN_INDEX, COLUMN_NAME FROM information_schema.statistics WHERE table_schema = ? AND table_name = ? ORDER BY INDEX_NAME, SEQ_IN_INDEX",
        (database.to_owned(), table.to_owned()),
    )
    .await
    .map_err(db_err)?;
    let mut groups: Vec<(String, bool, Vec<String>)> = Vec::new();
    for row in &index_rows {
        let name = mysql_text(row, 0)?;
        let non_unique = mysql_text(row, 1)?;
        let column = mysql_text(row, 3)?;
        let unique = non_unique == "0";
        if let Some(group) = groups.iter_mut().find(|group| group.0 == name) {
            group.2.push(column);
        } else {
            groups.push((name, unique, vec![column]));
        }
    }
    let pk = groups.iter().find(|group| group.0.eq_ignore_ascii_case("PRIMARY")).map(|group| group.2.clone()).unwrap_or_default();
    let key = if !pk.is_empty() {
        pk.clone()
    } else {
        groups.iter().find(|group| group.1).map(|group| group.2.clone()).unwrap_or_default()
    };
    let mut columns = Vec::new();
    for row in &column_rows {
        let name = mysql_text(row, 0)?;
        let type_name = mysql_text(row, 1)?;
        let nullable = mysql_text(row, 2)?.eq_ignore_ascii_case("YES");
        let extra = mysql_text(row, 4)?;
        let primary = pk.iter().any(|item| item == &name);
        let unique = primary || groups.iter().any(|group| group.1 && group.2.len() == 1 && group.2[0] == name);
        let raw_default = match mysql_cell(row.as_ref(5).unwrap_or(&mysql_async::Value::NULL)) {
            Cell::Text(text) => Some(text),
            Cell::Null => None,
        };
        let traits = column_traits(engine, &type_name, raw_default.as_deref(), &extra, quoting);
        columns.push(Column {
            name,
            type_name,
            nullable,
            primary,
            unique,
            auto: traits.auto,
            default: traits.default,
            generated: traits.generated,
            choices: traits.choices,
        });
    }
    Ok(TableSchema { engine, schema: database.to_owned(), name: table.to_owned(), kind, columns, key })
}

fn mysql_text(row: &mysql_async::Row, index: usize) -> Result<String, String> {
    match mysql_cell(row.as_ref(index).unwrap_or(&mysql_async::Value::NULL)) {
        Cell::Text(text) => Ok(text),
        Cell::Null => Err("元数据列是空值".to_owned()),
    }
}

/// 由 information_schema.columns 的一行推出的网格列特性（R7）：自增、默认值、生成列与枚举值。
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ColumnTraits {
    pub auto: bool,
    pub default: Option<String>,
    pub generated: bool,
    pub choices: Vec<String>,
}

/// EXTRA 是否标明生成列（VIRTUAL/STORED/PERSISTENT GENERATED）。MySQL 8 给表达式默认值标的
/// DEFAULT_GENERATED 不是生成列，列仍可写。
pub(crate) fn extra_marks_generated(extra: &str) -> bool {
    let extra = extra.to_ascii_uppercase();
    ["VIRTUAL GENERATED", "STORED GENERATED", "PERSISTENT GENERATED"].iter().any(|flag| extra.contains(flag))
}

/// MySQL 8 的 COLUMN_DEFAULT 字面量不带引号、表达式默认在 EXTRA 里标 DEFAULT_GENERATED；
/// MariaDB 的字面量带引号、没有默认值的可空列报回文本 NULL。两者都按类型规则解析成四态，
/// 无默认值与默认 NULL 一律记为 None（新增行本来就是 NULL）。生成列（VIRTUAL/STORED/PERSISTENT GENERATED）
/// 不能写；表达式默认（DEFAULT_GENERATED）不是生成列。
pub(crate) fn column_traits(engine: Engine, type_name: &str, raw_default: Option<&str>, extra: &str, quoting: LiteralQuoting) -> ColumnTraits {
    let extra = extra.to_ascii_uppercase();
    let auto = extra.contains("AUTO_INCREMENT");
    let generated = extra_marks_generated(&extra);
    let default = if auto || generated {
        None
    } else {
        raw_default.filter(|raw| {
            !matches!(crate::ddl::sql::existing_default(engine, Some(raw)), crate::ddl::DefaultSpec::None | crate::ddl::DefaultSpec::Null)
        })
        .map(str::to_owned)
    };
    ColumnTraits { auto, default, generated, choices: enum_choices(type_name, quoting) }
}

/// `enum('a','b''c')` 的可选值（按定义顺序，'' 还原成 '）。SET 允许多选，不给单选下拉。
///
/// 解析统一交给 `literal::parse_enum_set`：结构页与网格对同一份 COLUMN_TYPE 用同一个编码器，
/// 候选值里的空串、单引号、反斜杠、逗号与中文不会因为两处规则不同而读出不同结果。
/// 服务器读回的反斜杠写法取决于会话 sql_mode，所以先按档案给的转义方式解析，解析不出再用另一种写法。
pub(crate) fn enum_choices(type_name: &str, quoting: LiteralQuoting) -> Vec<String> {
    let Some(state) = super::literal::parse_enum_set(type_name, quoting)
        .or_else(|| super::literal::parse_enum_set(type_name, LiteralQuoting { backslash_escapes: !quoting.backslash_escapes }))
    else {
        return Vec::new();
    };
    // SET 的候选值是多选，网格不给单选下拉（与既有行为一致）。
    if state.kind != crate::ddl::model::EnumSetKind::Enum {
        return Vec::new();
    }
    state.values
}

#[cfg(test)]
mod column_trait_tests {
    use super::*;
    use crate::ddl::model::LiteralQuoting;

    /// 反斜杠是转义符的会话（默认 sql_mode）。
    fn quoting() -> LiteralQuoting {
        LiteralQuoting::default()
    }

    fn traits(engine: Engine, type_name: &str, raw_default: Option<&str>, extra: &str) -> ColumnTraits {
        column_traits(engine, type_name, raw_default, extra, quoting())
    }

    #[test]
    fn describe_keeps_defaults_generated_columns_and_enum_values() {
        let created = traits(Engine::Mysql, "datetime", Some("CURRENT_TIMESTAMP"), "DEFAULT_GENERATED");
        assert_eq!(created, ColumnTraits { auto: false, default: Some("CURRENT_TIMESTAMP".to_owned()), generated: false, choices: Vec::new() });
        let id = traits(Engine::Mysql, "bigint", None, "auto_increment");
        assert!(id.auto && id.default.is_none() && !id.generated);
        assert_eq!(traits(Engine::Mysql, "varchar(20)", Some("active"), "").default.as_deref(), Some("active"));
        // 没有默认值与默认 NULL 都不算默认值：MySQL 报 SQL NULL，MariaDB 报文本 NULL。
        assert_eq!(traits(Engine::Mysql, "varchar(20)", None, "").default, None);
        assert_eq!(traits(Engine::Mariadb, "varchar(20)", Some("NULL"), "").default, None);
        assert_eq!(traits(Engine::Mariadb, "varchar(20)", Some("'active'"), "").default.as_deref(), Some("'active'"));
        let total = traits(Engine::Mysql, "int", None, "STORED GENERATED");
        assert!(total.generated && total.default.is_none());
        assert!(traits(Engine::Mariadb, "int", None, "VIRTUAL GENERATED").generated);
        // 结构页读列也用同一判定：DEFAULT_GENERATED（表达式默认值）不是生成列。
        assert!(!super::extra_marks_generated("DEFAULT_GENERATED"));
        assert!(!super::extra_marks_generated("DEFAULT_GENERATED on update CURRENT_TIMESTAMP"));
        assert!(super::extra_marks_generated("stored generated"));
        assert_eq!(
            traits(Engine::Mysql, "enum('draft','it''s','done')", Some("draft"), "").choices,
            vec!["draft".to_owned(), "it's".to_owned(), "done".to_owned()]
        );
        assert!(enum_choices("set('a','b')", quoting()).is_empty());
        assert!(enum_choices("varchar(10)", quoting()).is_empty());
    }

    /// 网格下拉与结构页共用 literal 的解析：空串、引号、逗号、中文与反斜杠都读成原值，
    /// 反斜杠是否为转义符按会话 sql_mode 判断。
    #[test]
    fn enum_choices_follow_the_shared_literal_parser() {
        let backslash = LiteralQuoting { backslash_escapes: true };
        let no_backslash = LiteralQuoting { backslash_escapes: false };
        assert_eq!(
            enum_choices("enum('','it''s','a,b','中文')", backslash),
            vec![String::new(), "it's".to_owned(), "a,b".to_owned(), "中文".to_owned()]
        );
        // 服务器读回的含反斜杠写法：转义模式下两个反斜杠是一个；NO_BACKSLASH_ESCAPES 下一个就是字面量。
        let escaped = "enum('a\u{5c}\u{5c}b')".to_string();
        let literal = "enum('a\u{5c}b')".to_string();
        let one = "a\u{5c}b".to_string();
        assert_eq!(enum_choices(&escaped, backslash), vec![one.clone()]);
        assert_eq!(enum_choices(&literal, no_backslash), vec![one]);
        // 会话模式给错时用另一种写法兜底，不让下拉悄悄变空。
        assert_eq!(enum_choices("enum('x''y')", backslash), vec!["x'y".to_owned()]);
        assert_eq!(enum_choices("enum('x''y')", no_backslash), vec!["x'y".to_owned()]);
    }
}
