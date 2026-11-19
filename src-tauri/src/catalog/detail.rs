//! 表/视图的按需列与索引读取。只在已经打开的 catalog 会话上发查询，
//! 不新开连接；失败只影响这一个对象，不碰同级的其它节点。
//!
//! 列/索引/主键/注释查询与结果行形状由各适配器的目录方言给出（见 `adapters/<engine>/catalog.rs`）。

use crate::adapters::catalog_of;
use crate::catalog::hierarchy::{ColumnInfo, IndexInfo};
use crate::catalog::model::Engine;
use crate::connections::CatalogSession;
use crate::query::assist::ColumnMeta;

/// 列查询：name, type, nullable, default, comment；注释用左连接读，读不到就是 NULL，不影响列本身。
pub fn columns_sql(engine: Engine, database: &str, schema: &str, table: &str) -> Option<String> {
    catalog_of(engine).columns_sql(database, schema, table)
}

/// 一个库/模式里表和视图的注释：(name, comment)，只列有注释的对象。没有注释来源的引擎为 None。
pub fn table_comments_sql(engine: Engine, database: &str, schema: &str) -> Option<String> {
    catalog_of(engine).table_comments_sql(database, schema)
}

/// SQL 补全只读取当前涉及对象的注释，不预读整个库。
pub fn table_comment_sql(engine: Engine, database: &str, schema: &str, table: &str) -> Option<String> {
    let dialect = catalog_of(engine);
    let sql = dialect.table_comments_sql(database, schema)?;
    let (select, order) = sql.rsplit_once(" ORDER BY ")?;
    let column = dialect.table_comment_name_column();
    Some(format!("{select} AND {column} = {} ORDER BY {order}", crate::adapters::catalog::literal(table)))
}

/// 主键列查询。没有主键就是空集，不是失败。
pub fn primary_key_sql(engine: Engine, database: &str, schema: &str, table: &str) -> Option<String> {
    catalog_of(engine).primary_key_sql(database, schema, table)
}

/// 索引查询。
pub fn indexes_sql(engine: Engine, database: &str, schema: &str, table: &str) -> Option<String> {
    catalog_of(engine).indexes_sql(database, schema, table)
}

fn cell(rows: &[Vec<Option<String>>], row: usize, column: usize) -> String {
    crate::adapters::catalog::cell(rows, row, column)
}

/// 列查询结果收成列（按引擎的行形状）。
pub fn columns_from_rows(engine: Engine, rows: &[Vec<Option<String>>]) -> Vec<ColumnInfo> {
    catalog_of(engine).columns_from_rows(rows)
}

/// 补全和悬停用的列元数据：在列查询的位置约定上再取注释。
/// 主键由行本身给出的引擎直接标上，其它引擎的主键由 apply_primary_to_meta 另查。读不到的类型和注释留空，不编造。
pub fn column_meta_from_rows(engine: Engine, rows: &[Vec<Option<String>>]) -> Vec<ColumnMeta> {
    catalog_of(engine).column_meta_from_rows(rows)
}

/// 主键查询的列名行标到已读列上。对不上的名字不编造列。
pub fn apply_primary_to_meta(columns: &mut [ColumnMeta], rows: &[Vec<Option<String>>]) {
    for index in 0..rows.len() {
        let name = cell(rows, index, 0);
        if name.is_empty() {
            continue;
        }
        for column in columns.iter_mut() {
            if column.name.eq_ignore_ascii_case(&name) {
                column.primary = true;
            }
        }
    }
}

/// (name, comment) 行收成对照表；名字或注释为空的行跳过。
pub fn comments_from_rows(rows: &[Vec<Option<String>>]) -> Vec<(String, String)> {
    (0..rows.len())
        .map(|index| (cell(rows, index, 0), cell(rows, index, 1)))
        .filter(|(name, comment)| !name.is_empty() && !comment.trim().is_empty())
        .collect()
}

pub fn apply_primary_key(columns: &mut [ColumnInfo], rows: &[Vec<Option<String>>]) {
    for index in 0..rows.len() {
        let name = cell(rows, index, 0);
        for column in columns.iter_mut() {
            if column.name == name {
                column.primary = true;
            }
        }
    }
}

/// 索引行收成索引（按引擎的行形状），列按序号排好。
pub fn indexes_from_rows(engine: Engine, rows: &[Vec<Option<String>>]) -> Vec<IndexInfo> {
    catalog_of(engine).indexes_from_rows(rows)
}

pub(crate) fn failure(err: impl Into<crate::error_text::Reason>) -> String {
    let err = err.into();
    format!("读取对象详情失败：{err}")
}

/// 在一个已经打开的目录会话上执行一条只读语句，把每格转成文本。
/// 同模块的按需读取和查询补全的外键读取共用它；各引擎的取值规则在适配器里。
pub(crate) async fn rows_for(session: &CatalogSession, sql: &str) -> Result<Vec<Vec<Option<String>>>, String> {
    session.text_rows(sql).await
}

/// 读一个表的列元数据（名字、类型、可空、注释），供补全详情、悬停和对象信息使用。
pub async fn read_column_meta(
    session: &CatalogSession,
    engine: Engine,
    database: &str,
    schema: &str,
    table: &str,
) -> Result<Vec<ColumnMeta>, String> {
    // 没有列清单的引擎：只是没有列元数据，不是读取失败（与注释同一处理）。
    // 当成失败会让对象树顶端常驻“读取对象失败”。
    let Some(sql) = columns_sql(engine, database, schema, table) else {
        return Ok(Vec::new());
    };
    let rows = rows_for(session, &sql).await?;
    let mut meta = column_meta_from_rows(engine, &rows);
    // 主键读不到不算列失败：列已经读到了，只是没有主键标记。行本身带主键的引擎没有另查。
    if let Some(pk_sql) = primary_key_sql(engine, database, schema, table) {
        if let Ok(pk_rows) = rows_for(session, &pk_sql).await {
            apply_primary_to_meta(&mut meta, &pk_rows);
        }
    }
    Ok(meta)
}

/// 读一个库/模式里对象的注释。没有注释来源的引擎返回空表，不是失败。
pub async fn read_table_comments(session: &CatalogSession, engine: Engine, database: &str, schema: &str) -> Result<Vec<(String, String)>, String> {
    let Some(sql) = table_comments_sql(engine, database, schema) else {
        return Ok(Vec::new());
    };
    let rows = rows_for(session, &sql).await?;
    Ok(comments_from_rows(&rows))
}

pub async fn read_table_comment(session: &CatalogSession, engine: Engine, database: &str, schema: &str, table: &str) -> Result<Option<String>, String> {
    let Some(sql) = table_comment_sql(engine, database, schema, table) else { return Ok(None) };
    let rows = rows_for(session, &sql).await?;
    Ok(comments_from_rows(&rows).into_iter().next().map(|(_, comment)| comment))
}

/// 读一个对象的列与索引。两边分别成败：列失败不影响索引结果；两者都不碰同级的其它节点。
/// database 是节点自己的库：能跨库读目录的引擎用它限定到目标库；会话只连一个库的引擎非当前库要用连到该库的会话。
pub async fn read_object_details(
    session: &CatalogSession,
    engine: Engine,
    database: &str,
    schema: &str,
    table: &str,
) -> (Result<Vec<ColumnInfo>, String>, Result<Vec<IndexInfo>, String>) {
    let dialect = catalog_of(engine);
    let columns = match dialect.columns_sql(database, schema, table) {
        None => Err("这个引擎没有列清单".to_owned()),
        Some(sql) => match rows_for(session, &dialect.scope_sql(database, &sql)).await {
            Ok(rows) => {
                let mut items = columns_from_rows(engine, &rows);
                if let Some(pk_sql) = dialect.primary_key_sql(database, schema, table) {
                    let pk_sql = dialect.scope_sql(database, &pk_sql);
                    // 主键读不到不算列失败：列已经读到了，只是没有主键标记。
                    if let Ok(pk_rows) = rows_for(session, &pk_sql).await {
                        apply_primary_key(&mut items, &pk_rows);
                    }
                }
                Ok(items)
            }
            Err(message) => Err(message),
        },
    };
    let indexes = match indexes_sql(engine, database, schema, table) {
        None => Err("这个引擎没有索引清单".to_owned()),
        Some(sql) => match rows_for(session, &dialect.scope_sql(database, &sql)).await {
            Ok(rows) => {
                let mut items = indexes_from_rows(engine, &rows);
                // 索引清单不带列的引擎：逐个索引读列。
                for item in items.iter_mut() {
                    let Some(sql) = dialect.index_columns_sql(&item.name) else { continue };
                    if let Ok(rows) = rows_for(session, &sql).await {
                        item.columns = (0..rows.len()).map(|index| cell(&rows, index, 2)).filter(|name| !name.is_empty()).collect();
                    }
                }
                Ok(items)
            }
            Err(message) => Err(message),
        },
    };
    (columns, indexes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 没有列清单的引擎（MongoDB / Redis）读列元数据得到空表而不是失败：
    /// 否则连上 MongoDB 后对象树顶端会常驻“读取对象失败”。
    #[tokio::test]
    async fn column_meta_of_non_relational_engines_is_empty_not_a_failure() {
        for engine in [Engine::Mongodb, Engine::Redis] {
            let meta = read_column_meta(&CatalogSession::Scripted, engine, "app", "", "users").await;
            assert_eq!(meta, Ok(Vec::new()), "{engine:?}");
        }
    }

    #[test]
    fn table_comment_is_scoped_and_quotes_untrusted_names() {
        for engine in [Engine::Mysql, Engine::Mariadb, Engine::Postgres, Engine::Sqlserver] {
            let sql = table_comment_sql(engine, "shop", "public", "a'b").unwrap();
            assert!(sql.contains(" = 'a''b' ORDER BY "), "{sql}");
        }
        assert!(table_comment_sql(Engine::Sqlite, "main", "", "item").is_none());
    }

    #[test]
    fn engine_sql_shapes_quote_names_and_skip_non_relational_engines() {
        assert!(columns_sql(Engine::Sqlite, "main", "", "notes").unwrap().contains("PRAGMA table_info"));
        let pg = columns_sql(Engine::Postgres, "shop", "public", "orders").unwrap();
        assert!(pg.contains("table_schema = 'public'"));
        assert!(pg.contains("table_name = 'orders'"));
        let my = columns_sql(Engine::Mysql, "shop", "", "orders").unwrap();
        assert!(my.contains("table_schema = 'shop'"));
        assert!(indexes_sql(Engine::Redis, "0", "", "k").is_none());
        assert!(indexes_sql(Engine::Mongodb, "app", "", "users").is_none());

        // 名字里的引号不能跑出字面量。
        let quoted = columns_sql(Engine::Postgres, "shop", "pub'lic", "o'rders").unwrap();
        assert!(quoted.contains("'pub''lic'"), "{quoted}");
        assert!(quoted.contains("'o''rders'"), "{quoted}");
        let sqlite = columns_sql(Engine::Sqlite, "main", "", "we\"ird").unwrap();
        assert_eq!(sqlite, "PRAGMA table_info(\"we\"\"ird\")");
    }

    #[test]
    fn information_schema_rows_become_columns_and_primary_keys_mark_them() {
        let rows = vec![
            vec![Some("id".to_owned()), Some("integer".to_owned()), Some("NO".to_owned()), None],
            vec![Some("name".to_owned()), Some("text".to_owned()), Some("YES".to_owned()), None],
        ];
        let mut columns = columns_from_rows(Engine::Postgres, &rows);
        assert_eq!(columns.len(), 2);
        assert_eq!(columns[0].data_type, "integer");
        assert!(!columns[0].nullable);
        assert!(columns[1].nullable);
        assert!(columns.iter().all(|column| !column.primary));
        apply_primary_key(&mut columns, &[vec![Some("id".to_owned())]]);
        assert!(columns[0].primary);
        assert!(!columns[1].primary);
    }

    #[test]
    fn column_meta_reads_type_nullability_and_comment_without_inventing_any() {
        let rows = vec![
            vec![Some("id".to_owned()), Some("integer".to_owned()), Some("NO".to_owned()), None, Some("主键".to_owned())],
            vec![Some("note".to_owned()), Some("text".to_owned()), Some("YES".to_owned()), None, None],
            vec![Some("legacy".to_owned()), None, None, None],
        ];
        let mut meta = column_meta_from_rows(Engine::Postgres, &rows);
        assert_eq!(meta.len(), 3);
        assert_eq!(
            meta[0],
            ColumnMeta { name: "id".to_owned(), type_name: "integer".to_owned(), nullable: Some(false), comment: "主键".to_owned(), primary: false, foreign_key: false }
        );
        assert_eq!(meta[1].nullable, Some(true));
        assert!(meta[1].comment.is_empty(), "没有注释就留空");
        assert!(!meta[0].primary, "information_schema 行本身不带主键");
        assert_eq!(meta[2], ColumnMeta::named("legacy"), "读不到的类型和可空不编造");
        apply_primary_to_meta(&mut meta, &[vec![Some("id".to_owned())]]);
        assert!(meta[0].primary);
        assert!(!meta[1].primary);
        assert!(!meta[0].foreign_key, "外键不从列查询编造");

        let sqlite = vec![
            vec![
                Some("0".to_owned()),
                Some("id".to_owned()),
                Some("INTEGER".to_owned()),
                Some("1".to_owned()),
                None,
                Some("1".to_owned()),
            ],
            vec![
                Some("1".to_owned()),
                Some("body".to_owned()),
                Some("TEXT".to_owned()),
                Some("0".to_owned()),
                None,
                Some("0".to_owned()),
            ],
        ];
        let meta = column_meta_from_rows(Engine::Sqlite, &sqlite);
        assert!(meta[0].primary, "PRAGMA pk 非 0 是主键");
        assert_eq!(meta[0].nullable, Some(false));
        assert!(meta[0].comment.is_empty(), "SQLite 没有列注释");
        assert!(!meta[1].primary);
        assert_eq!(meta[1].nullable, Some(true));
    }

    #[test]
    fn column_and_table_comments_come_from_each_engine_catalog() {
        let pg = columns_sql(Engine::Postgres, "shop", "public", "orders").unwrap();
        assert!(pg.contains("pg_description"), "{pg}");
        assert!(pg.contains("LEFT JOIN"), "注释读不到也要有列：{pg}");
        let my = columns_sql(Engine::Mysql, "shop", "", "orders").unwrap();
        assert!(my.contains("column_comment"), "{my}");
        let ms = columns_sql(Engine::Sqlserver, "shop", "dbo", "orders").unwrap();
        assert!(ms.contains("MS_Description") && ms.contains("LEFT JOIN"), "{ms}");
        assert!(table_comments_sql(Engine::Sqlite, "main", "").is_none());
        assert!(table_comments_sql(Engine::Redis, "0", "").is_none());
        let tables = table_comments_sql(Engine::Mysql, "sh'op", "").unwrap();
        assert!(tables.contains("'sh''op'") && tables.contains("table_type <> 'VIEW'"), "{tables}");
        let rows = vec![
            vec![Some("orders".to_owned()), Some("订单".to_owned())],
            vec![Some("blank".to_owned()), Some(" ".to_owned())],
            vec![None, Some("x".to_owned())],
        ];
        assert_eq!(comments_from_rows(&rows), vec![("orders".to_owned(), "订单".to_owned())]);
    }

    #[test]
    fn sqlite_columns_read_notnull_and_pk_flags() {
        // PRAGMA table_info: cid, name, type, notnull, dflt, pk
        let rows = vec![
            vec![
                Some("0".to_owned()),
                Some("id".to_owned()),
                Some("INTEGER".to_owned()),
                Some("0".to_owned()),
                None,
                Some("1".to_owned()),
            ],
            vec![
                Some("1".to_owned()),
                Some("body".to_owned()),
                Some("TEXT".to_owned()),
                Some("1".to_owned()),
                None,
                Some("0".to_owned()),
            ],
        ];
        let columns = columns_from_rows(Engine::Sqlite, &rows);
        assert_eq!(columns[0].name, "id");
        assert!(columns[0].primary);
        assert!(columns[0].nullable, "notnull=0 表示可以为 NULL");
        assert!(!columns[1].primary);
        assert!(!columns[1].nullable, "notnull=1 表示不允许 NULL");
    }

    #[test]
    fn index_rows_group_columns_in_ordinal_order() {
        let rows = vec![
            vec![Some("ix_a".to_owned()), Some("b".to_owned()), Some("t".to_owned()), Some("f".to_owned()), Some("2".to_owned())],
            vec![Some("ix_a".to_owned()), Some("a".to_owned()), Some("t".to_owned()), Some("f".to_owned()), Some("1".to_owned())],
            vec![Some("ix_pk".to_owned()), Some("id".to_owned()), Some("t".to_owned()), Some("t".to_owned()), Some("1".to_owned())],
        ];
        let indexes = indexes_from_rows(Engine::Postgres, &rows);
        assert_eq!(indexes.len(), 2);
        assert_eq!(indexes[0].name, "ix_a");
        assert!(indexes[0].unique);
        assert_eq!(indexes[0].columns, vec!["a", "b"]);
        assert!(indexes[1].primary);
    }

    #[test]
    fn mysql_index_rows_use_show_index_positions() {
        // SHOW INDEX: table, non_unique, key_name, seq, column_name ...
        let rows = vec![
            vec![Some("orders".to_owned()), Some("0".to_owned()), Some("PRIMARY".to_owned()), Some("1".to_owned()), Some("id".to_owned())],
            vec![Some("orders".to_owned()), Some("1".to_owned()), Some("ix_name".to_owned()), Some("1".to_owned()), Some("name".to_owned())],
        ];
        let indexes = indexes_from_rows(Engine::Mysql, &rows);
        assert_eq!(indexes.len(), 2);
        assert!(indexes[0].primary && indexes[0].unique);
        assert_eq!(indexes[0].columns, vec!["id"]);
        assert!(!indexes[1].unique);
        assert_eq!(indexes[1].columns, vec!["name"]);
    }
}