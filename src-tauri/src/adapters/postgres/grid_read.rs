//! PostgreSQL 的表网格读写：描述表结构、读一页、计数、逐条执行、外键与流式导出。都在已打开的客户端上进行。

use futures::TryStreamExt;

use crate::adapters::grid::{cell_text, db_err, other_database, ExportSink};
use crate::connections::model::Engine;
use crate::grid::{push_foreign_key, Cell, Column, ForeignKey, TableSchema};

pub(in crate::adapters) async fn describe(client: &tokio_postgres::Client, database: &str, schema: &str, table: &str) -> Result<TableSchema, String> {
    ensure_postgres_database(client, database).await?;
    if schema.is_empty() {
        return Err("PostgreSQL 需要指定 schema（表所在的分组名），没有另开连接。".to_owned());
    }
    describe_postgres(client, schema, table).await
}

pub(in crate::adapters) async fn fetch_cells(client: &tokio_postgres::Client, sql: &str, width: usize) -> Result<Vec<Vec<Cell>>, String> {
    let rows = client.query(sql, &[]).await.map_err(db_err)?;
    rows.iter()
        .map(|row| (0..width).map(|index| pg_cell(row, index)).collect::<Result<Vec<_>, _>>())
        .collect()
}

pub(in crate::adapters) async fn fetch_count(client: &tokio_postgres::Client, sql: &str) -> Result<i64, String> {
    let row = client.query_one(sql, &[]).await.map_err(db_err)?;
    row.try_get(0).map_err(db_err)
}

pub(in crate::adapters) async fn execute(client: &tokio_postgres::Client, sql: &str) -> Result<u64, String> {
    client.execute(sql, &[]).await.map_err(db_err)
}

pub(in crate::adapters) async fn foreign_keys(client: &tokio_postgres::Client, database: &str, schema_name: &str, table: &str) -> Result<Vec<ForeignKey>, String> {
    ensure_postgres_database(client, database).await?;
    if schema_name.is_empty() {
        return Ok(Vec::new());
    }
    let rows = client
        .query(
            "SELECT c.conname AS name, a.attname AS column_name, fs.nspname AS ref_schema, ft.relname AS ref_table, af.attname AS ref_column \
             FROM pg_constraint c \
             JOIN pg_class t ON t.oid = c.conrelid \
             JOIN pg_namespace s ON s.oid = t.relnamespace \
             JOIN pg_class ft ON ft.oid = c.confrelid \
             JOIN pg_namespace fs ON fs.oid = ft.relnamespace \
             JOIN LATERAL unnest(c.conkey, c.confkey) WITH ORDINALITY AS k(attnum, fattnum, ord) ON true \
             JOIN pg_attribute a ON a.attrelid = t.oid AND a.attnum = k.attnum \
             JOIN pg_attribute af ON af.attrelid = ft.oid AND af.attnum = k.fattnum \
             WHERE s.nspname = $1 AND t.relname = $2 AND c.contype = 'f' \
             ORDER BY c.conname, k.ord",
            &[&schema_name, &table],
        )
        .await
        .map_err(db_err)?;
    let mut list: Vec<ForeignKey> = Vec::new();
    for row in &rows {
        let name: String = row.get("name");
        let column: String = row.get("column_name");
        let ref_schema: String = row.get("ref_schema");
        let ref_table: String = row.get("ref_table");
        let ref_column: String = row.get("ref_column");
        push_foreign_key(&mut list, &name, &column, &ref_schema, &ref_table, &ref_column);
    }
    Ok(list)
}

/// 流式导出：列名已知时直接用，否则先 prepare 取列名；按 query_raw 逐行读。
pub(in crate::adapters) async fn stream(client: &tokio_postgres::Client, sql: &str, known_names: Option<Vec<String>>, sink: &mut dyn ExportSink) {
    let names = match known_names {
        Some(names) => names,
        None => {
            let statement = match client.prepare(sql).await {
                Ok(statement) => statement,
                Err(err) => return sink.fail(&crate::error_text::chain(&err)),
            };
            statement.columns().iter().map(|column| column.name().to_owned()).collect()
        }
    };
    let statement = match client.prepare(sql).await {
        Ok(statement) => statement,
        Err(err) => return sink.fail(&crate::error_text::chain(&err)),
    };
    if !sink.start(&names) {
        return;
    }
    let params: [&(dyn tokio_postgres::types::ToSql + Sync); 0] = [];
    let stream = match client.query_raw(&statement, params).await {
        Ok(stream) => stream,
        Err(err) => return sink.fail(&crate::error_text::chain(&err)),
    };
    futures::pin_mut!(stream);
    loop {
        if !sink.proceed() {
            return;
        }
        match stream.try_next().await {
            Ok(Some(row)) => {
                let cells = match (0..names.len()).map(|index| pg_cell(&row, index)).collect::<Result<Vec<_>, _>>() {
                    Ok(cells) => cells,
                    Err(message) => return sink.fail(&message),
                };
                if !sink.row(&cells) {
                    return;
                }
            }
            Ok(None) => return sink.finish(),
            Err(err) => return sink.fail(&crate::error_text::chain(&err)),
        }
    }
}

fn pg_cell(row: &tokio_postgres::Row, index: usize) -> Result<Cell, String> {
    if let Ok(value) = row.try_get::<_, Option<String>>(index) {
        return Ok(cell_text(value));
    }
    if let Ok(value) = row.try_get::<_, Option<i64>>(index) {
        return Ok(cell_text(value.map(|number| number.to_string())));
    }
    if let Ok(value) = row.try_get::<_, Option<i32>>(index) {
        return Ok(cell_text(value.map(|number| number.to_string())));
    }
    if let Ok(value) = row.try_get::<_, Option<i16>>(index) {
        return Ok(cell_text(value.map(|number| number.to_string())));
    }
    if let Ok(value) = row.try_get::<_, Option<f64>>(index) {
        return Ok(cell_text(value.map(|number| number.to_string())));
    }
    if let Ok(value) = row.try_get::<_, Option<bool>>(index) {
        return Ok(cell_text(value.map(|number| number.to_string())));
    }
    if let Ok(value) = row.try_get::<_, Option<Vec<u8>>>(index) {
        return Ok(cell_text(value.map(|bytes| String::from_utf8_lossy(&bytes).into_owned())));
    }
    Err(format!("读不到第 {index} 列"))
}

async fn ensure_postgres_database(client: &tokio_postgres::Client, database: &str) -> Result<(), String> {
    if database.is_empty() {
        return Ok(());
    }
    let row = client.query_one("SELECT current_database()", &[]).await.map_err(db_err)?;
    let current: Option<String> = row.try_get(0).map_err(db_err)?;
    let current = current.unwrap_or_default();
    if current != database {
        return Err(other_database(&current, database));
    }
    Ok(())
}

async fn describe_postgres(client: &tokio_postgres::Client, schema: &str, table: &str) -> Result<TableSchema, String> {
    let kind_rows = client
        .query(
            "SELECT c.relkind::text FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 AND c.relname = $2 AND c.relkind IN ('r','p','v','m')",
            &[&schema, &table],
        )
        .await
        .map_err(db_err)?;
    let kind_text: String = kind_rows.first().ok_or_else(|| format!("找不到表或视图 {schema}.{table}"))?.get(0);
    let kind = if kind_text == "r" || kind_text == "p" { "table" } else { "view" };
    let columns_rows = client
        .query(
            "SELECT a.attname, pg_catalog.format_type(a.atttypid, a.atttypmod), a.attnotnull, a.attidentity::text, pg_get_expr(ad.adbin, ad.adrelid) FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid JOIN pg_namespace n ON n.oid = c.relnamespace LEFT JOIN pg_attrdef ad ON ad.adrelid = a.attrelid AND ad.adnum = a.attnum WHERE n.nspname = $1 AND c.relname = $2 AND a.attnum > 0 AND NOT a.attisdropped ORDER BY a.attnum",
            &[&schema, &table],
        )
        .await
        .map_err(db_err)?;
    let pk_rows = client
        .query(
            "SELECT a.attname FROM pg_index i JOIN pg_class c ON c.oid = i.indrelid JOIN pg_namespace n ON n.oid = c.relnamespace JOIN pg_attribute a ON a.attrelid = c.oid AND a.attnum = ANY (i.indkey) WHERE n.nspname = $1 AND c.relname = $2 AND i.indisprimary ORDER BY array_position(i.indkey, a.attnum)",
            &[&schema, &table],
        )
        .await
        .map_err(db_err)?;
    let pk = pk_rows.iter().map(|row| row.get::<_, String>(0)).collect::<Vec<_>>();
    let unique_rows = client
        .query(
            "SELECT i.indexrelid::text, a.attname FROM pg_index i JOIN pg_class c ON c.oid = i.indrelid JOIN pg_namespace n ON n.oid = c.relnamespace JOIN pg_attribute a ON a.attrelid = c.oid AND a.attnum = ANY (i.indkey) WHERE n.nspname = $1 AND c.relname = $2 AND i.indisunique AND NOT i.indisprimary AND i.indpred IS NULL AND i.indexprs IS NULL AND NOT (0 = ANY (i.indkey)) ORDER BY i.indexrelid, array_position(i.indkey, a.attnum)",
            &[&schema, &table],
        )
        .await
        .map_err(db_err)?;
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for row in &unique_rows {
        let id: String = row.get(0);
        let name: String = row.get(1);
        if let Some(group) = groups.iter_mut().find(|group| group.0 == id) {
            group.1.push(name);
        } else {
            groups.push((id, vec![name]));
        }
    }
    let key = if !pk.is_empty() { pk.clone() } else { groups.first().map(|group| group.1.clone()).unwrap_or_default() };
    // 生成列与枚举值各读一次：information_schema.columns.is_generated 在没有生成列的旧版本里恒为 NEVER，
    // 不需要按服务器版本分支；枚举值来自 pg_enum，按定义顺序。
    let generated_rows = client
        .query(
            "SELECT column_name::text FROM information_schema.columns WHERE table_schema = $1 AND table_name = $2 AND is_generated = 'ALWAYS'",
            &[&schema, &table],
        )
        .await
        .map_err(db_err)?;
    let generated_names = generated_rows.iter().map(|row| row.get::<_, String>(0)).collect::<Vec<_>>();
    let enum_rows = client
        .query(
            "SELECT a.attname, e.enumlabel::text FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid JOIN pg_namespace n ON n.oid = c.relnamespace JOIN pg_enum e ON e.enumtypid = a.atttypid WHERE n.nspname = $1 AND c.relname = $2 AND a.attnum > 0 AND NOT a.attisdropped ORDER BY a.attnum, e.enumsortorder",
            &[&schema, &table],
        )
        .await
        .map_err(db_err)?;
    let mut columns = Vec::new();
    for row in &columns_rows {
        let name: String = row.get(0);
        let type_name: String = row.get(1);
        let not_null: bool = row.get(2);
        let identity: String = row.get(3);
        let default: Option<String> = row.get(4);
        let generated = generated_names.iter().any(|item| item == &name);
        let (auto, default) = column_traits(&identity, default.as_deref(), generated);
        let choices = enum_rows.iter().filter(|item| item.get::<_, String>(0) == name).map(|item| item.get::<_, String>(1)).collect();
        let primary = pk.iter().any(|item| item == &name);
        let unique = primary || groups.iter().any(|group| group.1.len() == 1 && group.1[0] == name);
        columns.push(Column { name, type_name, nullable: !not_null, primary, unique, auto, default, generated, choices });
    }
    Ok(TableSchema { engine: Engine::Postgres, schema: schema.to_owned(), name: table.to_owned(), kind, columns, key })
}

/// 网格列的自增与默认值（R7）：标识列和 nextval 默认是自增，不再另记默认值；生成列的表达式不是默认值；
/// 默认 NULL（含 `NULL::type`）与没有默认值一样记为 None。
pub(crate) fn column_traits(identity: &str, raw_default: Option<&str>, generated: bool) -> (bool, Option<String>) {
    let auto = identity == "a" || identity == "d" || raw_default.unwrap_or("").starts_with("nextval(");
    if auto || generated {
        return (auto, None);
    }
    let default = raw_default
        .filter(|raw| {
            !matches!(
                crate::ddl::sql::existing_default(Engine::Postgres, Some(raw)),
                crate::ddl::DefaultSpec::None | crate::ddl::DefaultSpec::Null
            )
        })
        .map(str::to_owned);
    (false, default)
}

#[cfg(test)]
mod column_trait_tests {
    use super::column_traits;

    #[test]
    fn describe_keeps_defaults_and_marks_identity_and_generated_columns() {
        assert_eq!(column_traits("", Some("CURRENT_TIMESTAMP"), false), (false, Some("CURRENT_TIMESTAMP".to_owned())));
        assert_eq!(column_traits("", Some("'active'::character varying"), false), (false, Some("'active'::character varying".to_owned())));
        assert_eq!(column_traits("", Some("nextval('orders_id_seq'::regclass)"), false), (true, None));
        assert_eq!(column_traits("a", None, false), (true, None));
        assert_eq!(column_traits("d", None, false), (true, None));
        assert_eq!(column_traits("", Some("NULL::text"), false), (false, None));
        assert_eq!(column_traits("", None, false), (false, None));
        // 生成列：pg_attrdef 里存的是生成表达式，不是默认值。
        assert_eq!(column_traits("", Some("(price * qty)"), true), (false, None));
    }
}
