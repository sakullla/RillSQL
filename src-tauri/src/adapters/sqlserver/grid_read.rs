//! SQL Server 的表网格读写：描述表结构、读一页、计数、逐条执行、外键与导出。
//! tiberius 句柄经互斥锁串行使用同一条连接。

use crate::adapters::grid::{db_err, other_database, ExportSink};
use crate::connections::model::Engine;
use crate::grid::{assemble_foreign_keys, Cell, Column, ForeignKey, TableSchema};

pub(in crate::adapters) async fn describe(client: &TdsHandle, database: &str, schema: &str, table: &str) -> Result<TableSchema, String> {
    if database.is_empty() {
        return Err("需要数据库名，没有另开连接。".to_owned());
    }
    ensure_sqlserver_database(client, database).await?;
    if schema.is_empty() {
        return Err("SQL Server 需要指定 schema（表所在的分组名），没有另开连接。".to_owned());
    }
    describe_sqlserver(client, schema, table).await
}

pub(in crate::adapters) async fn fetch_cells(client: &TdsHandle, sql: &str) -> Result<Vec<Vec<Cell>>, String> {
    let (_names, rows) = sqlserver_rows(client, sql).await?;
    Ok(rows)
}

pub(in crate::adapters) async fn fetch_count(client: &TdsHandle, sql: &str) -> Result<i64, String> {
    let (_names, rows) = sqlserver_rows(client, sql).await?;
    match rows.first().and_then(|row| row.first()) {
        Some(Cell::Text(text)) => text.parse::<i64>().map_err(|_| format!("计数无效 {text}")),
        _ => Err("没有计数".to_owned()),
    }
}

/// execute 走 sp_executesql，返回受影响行数；结果流在下一条语句前会被 tiberius 排空。
pub(in crate::adapters) async fn execute(client: &TdsHandle, sql: &str) -> Result<u64, String> {
    let mut client = client.lock().await;
    client.execute(sql, &[]).await.map(|done| done.total()).map_err(db_err)
}

pub(in crate::adapters) async fn foreign_keys(client: &TdsHandle, database: &str, schema_name: &str, table: &str) -> Result<Vec<ForeignKey>, String> {
    if database.is_empty() || table.is_empty() {
        return Ok(Vec::new());
    }
    ensure_sqlserver_database(client, database).await?;
    // 与查询补全同一条 sys 目录语句，避免网格和外键提示各写一套。
    let sql = foreign_key_sql(schema_name, table);
    let (_names, rows) = sqlserver_rows(client, &sql).await?;
    Ok(assemble_foreign_keys(&rows))
}

/// 导出：TDS 结果流先整批收下（同一把锁上不能边读边写别的语句），再逐行写。
pub(in crate::adapters) async fn stream(client: &TdsHandle, sql: &str, known_names: Option<Vec<String>>, sink: &mut dyn ExportSink) {
    let mut locked = client.lock().await;
    let mut stream = match locked.simple_query(sql).await {
        Ok(stream) => stream,
        Err(err) => return sink.fail(&crate::error_text::chain(&err)),
    };
    let mut names = known_names;
    let mut rows: Vec<tiberius::Row> = Vec::new();
    {
        use futures::TryStreamExt;
        // QueryItem 不公开导出，经 as_metadata/into_row 取内容。
        while let Ok(Some(item)) = stream.try_next().await {
            if let Some(meta) = item.as_metadata() {
                if names.is_none() {
                    names = Some(meta.columns().iter().map(|column| column.name().to_owned()).collect());
                }
                continue;
            }
            if let Some(row) = item.into_row() {
                rows.push(row);
            }
        }
    }
    drop(stream);
    drop(locked);
    let names = names.unwrap_or_default();
    if !sink.start(&names) {
        return;
    }
    for row in rows {
        if !sink.proceed() {
            return;
        }
        let cells = (0..names.len()).map(|index| tds_cell(&row, index)).collect::<Vec<_>>();
        if !sink.row(&cells) {
            return;
        }
    }
    sink.finish()
}

pub(crate) type TdsHandle = std::sync::Arc<tokio::sync::Mutex<crate::connections::drivers::TdsClient>>;

fn tds_text_cell(row: &tiberius::Row, index: usize) -> Option<String> {
    row.try_get::<&str, _>(index).ok().flatten().map(str::to_owned)
}

/// TDS 行到 Cell：文本优先，数字/浮点/布尔逐类兜底，二进制转十六进制（与前端投影协议一致）。
fn tds_cell(row: &tiberius::Row, index: usize) -> Cell {
    if let Some(text) = tds_text_cell(row, index) {
        return Cell::Text(text);
    }
    if let Ok(Some(value)) = row.try_get::<i64, _>(index) {
        return Cell::Text(value.to_string());
    }
    if let Ok(Some(value)) = row.try_get::<i32, _>(index) {
        return Cell::Text(value.to_string());
    }
    if let Ok(Some(value)) = row.try_get::<i16, _>(index) {
        return Cell::Text(value.to_string());
    }
    if let Ok(Some(value)) = row.try_get::<u8, _>(index) {
        return Cell::Text(value.to_string());
    }
    if let Ok(Some(value)) = row.try_get::<f64, _>(index) {
        return Cell::Text(value.to_string());
    }
    if let Ok(Some(value)) = row.try_get::<f32, _>(index) {
        return Cell::Text(value.to_string());
    }
    if let Ok(Some(value)) = row.try_get::<bool, _>(index) {
        return Cell::Text(if value { "1" } else { "0" }.to_owned());
    }
    if let Ok(Some(value)) = row.try_get::<&[u8], _>(index) {
        return Cell::Text(to_hex_text(value));
    }
    Cell::Null
}

fn to_hex_text(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

pub(crate) async fn sqlserver_rows(client: &TdsHandle, sql: &str) -> Result<(Vec<String>, Vec<Vec<Cell>>), String> {
    let mut locked = client.lock().await;
    let mut stream = locked.simple_query(sql).await.map_err(db_err)?;
    let mut names: Vec<String> = Vec::new();
    let mut rows = Vec::new();
    {
        use futures::TryStreamExt;
        // QueryItem 不公开导出，经 as_metadata/into_row 取内容。
        while let Some(item) = stream.try_next().await.map_err(|err| db_err(crate::error_text::chain(&err)))? {
            if let Some(meta) = item.as_metadata() {
                if names.is_empty() {
                    names = meta.columns().iter().map(|column| column.name().to_owned()).collect();
                }
                continue;
            }
            if let Some(row) = item.into_row() {
                if names.is_empty() {
                    names = (0..row.len()).map(|index| format!("col{}", index + 1)).collect();
                }
                let cells = (0..names.len()).map(|index| tds_cell(&row, index)).collect();
                rows.push(cells);
            }
        }
    }
    Ok((names, rows))
}

async fn ensure_sqlserver_database(client: &TdsHandle, database: &str) -> Result<(), String> {
    let (_names, rows) = sqlserver_rows(client, "SELECT DB_NAME()").await?;
    let current = match rows.first().and_then(|row| row.first()) {
        Some(Cell::Text(text)) => text.clone(),
        _ => String::new(),
    };
    if current != database {
        return Err(other_database(&current, database));
    }
    Ok(())
}

async fn describe_sqlserver(client: &TdsHandle, schema: &str, table: &str) -> Result<TableSchema, String> {
    let kind_rows = sqlserver_rows(
        client,
        &format!(
            "SELECT CASE WHEN o.type = 'V' THEN 'VIEW' ELSE 'BASE TABLE' END FROM sys.objects o JOIN sys.schemas s ON s.schema_id = o.schema_id WHERE s.name = N'{}' AND o.name = N'{}' AND o.type IN ('U', 'V')",
            escape_tds_text(schema),
            escape_tds_text(table)
        ),
    )
    .await?;
    let kind_text = match kind_rows.1.first().and_then(|row| row.first()) {
        Some(Cell::Text(text)) => text.clone(),
        _ => return Err(format!("找不到表或视图 {schema}.{table}")),
    };
    let kind = if kind_text.eq_ignore_ascii_case("VIEW") { "view" } else { "table" };
    let column_rows = sqlserver_rows(
        client,
        &format!(
            "SELECT c.name, TYPE_NAME(c.user_type_id), c.is_nullable, c.is_identity, dc.definition FROM sys.columns c JOIN sys.objects o ON o.object_id = c.object_id JOIN sys.schemas s ON s.schema_id = o.schema_id LEFT JOIN sys.default_constraints dc ON dc.parent_object_id = c.object_id AND dc.parent_column_id = c.column_id WHERE s.name = N'{}' AND o.name = N'{}' ORDER BY c.column_id",
            escape_tds_text(schema),
            escape_tds_text(table)
        ),
    )
    .await?;
    let pk_rows = sqlserver_rows(
        client,
        &format!(
            "SELECT c.name FROM sys.index_columns ic JOIN sys.indexes i ON i.object_id = ic.object_id AND i.index_id = ic.index_id JOIN sys.columns c ON c.object_id = ic.object_id AND c.column_id = ic.column_id JOIN sys.objects o ON o.object_id = i.object_id JOIN sys.schemas s ON s.schema_id = o.schema_id WHERE i.is_primary_key = 1 AND s.name = N'{}' AND o.name = N'{}' ORDER BY ic.key_ordinal",
            escape_tds_text(schema),
            escape_tds_text(table)
        ),
    )
    .await?;
    let unique_rows = sqlserver_rows(
        client,
        &format!(
            "SELECT i.name, i.is_unique, ic.key_ordinal, c.name FROM sys.indexes i JOIN sys.index_columns ic ON ic.object_id = i.object_id AND ic.index_id = i.index_id JOIN sys.columns c ON c.object_id = ic.object_id AND c.column_id = ic.column_id JOIN sys.objects o ON o.object_id = i.object_id JOIN sys.schemas s ON s.schema_id = o.schema_id WHERE i.is_unique = 1 AND i.is_primary_key = 0 AND s.name = N'{}' AND o.name = N'{}' ORDER BY i.name, ic.key_ordinal",
            escape_tds_text(schema),
            escape_tds_text(table)
        ),
    )
    .await?;
    let pk: Vec<String> = pk_rows
        .1
        .iter()
        .filter_map(|row| match row.first() {
            Some(Cell::Text(text)) => Some(text.clone()),
            _ => None,
        })
        .collect();
    let mut groups: Vec<(String, bool, Vec<String>)> = Vec::new();
    for row in &unique_rows.1 {
        let cells: Vec<&Cell> = row.iter().collect();
        let name = match cells.first().and_then(|cell| match cell {
            Cell::Text(text) => Some(text.as_str()),
            _ => None,
        }) {
            Some(name) => name.to_owned(),
            None => continue,
        };
        let unique = cells.get(1).map(|cell| matches!(cell, Cell::Text(text) if text == "1")).unwrap_or(false);
        let column = cells
            .get(3)
            .and_then(|cell| match cell {
                Cell::Text(text) => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default();
        if let Some(group) = groups.iter_mut().find(|group| group.0 == name) {
            group.2.push(column);
        } else {
            groups.push((name, unique, vec![column]));
        }
    }
    let key = if !pk.is_empty() {
        pk.clone()
    } else {
        groups.iter().find(|group| group.1).map(|group| group.2.clone()).unwrap_or_default()
    };
    let mut columns = Vec::new();
    for row in &column_rows.1 {
        let cells: Vec<&Cell> = row.iter().collect();
        let name = match cells.first().and_then(|cell| match cell {
            Cell::Text(text) => Some(text.clone()),
            _ => None,
        }) {
            Some(name) => name,
            None => return Err("列名是空的".to_owned()),
        };
        let type_name = cells.get(1).and_then(|cell| match cell {
            Cell::Text(text) => Some(text.clone()),
            _ => None,
        }).unwrap_or_default();
        let nullable = cells.get(2).map(|cell| matches!(cell, Cell::Text(text) if text == "1")).unwrap_or(true);
        let identity = cells.get(3).map(|cell| matches!(cell, Cell::Text(text) if text == "1")).unwrap_or(false);
        let primary = pk.iter().any(|item| item == &name);
        let unique = primary || groups.iter().any(|group| group.1 && group.2.len() == 1 && group.2[0] == name);
        // IDENTITY 列即网格的自增列：插入时省略、UPDATE 不进 SET。
        columns.push(Column { name, type_name, nullable, primary, unique, auto: identity });
    }
    Ok(TableSchema { engine: Engine::Sqlserver, schema: schema.to_owned(), name: table.to_owned(), kind, columns, key })
}

/// 元数据查询里内联的 N'...' 文本转义。标识符与表名来自用户输入。
fn escape_tds_text(value: &str) -> String {
    value.replace('\'', "''")
}

/// SQL Server 出向外键查询。只读 sys 目录，标识符按 N'...' 转义。
pub(crate) fn foreign_key_sql(schema: &str, table: &str) -> String {
    let mut sql = String::from(
        "SELECT fk.name, pc.name, rs.name, rt.name, rc.name \
         FROM sys.foreign_key_columns fkc \
         JOIN sys.foreign_keys fk ON fk.object_id = fkc.constraint_object_id \
         JOIN sys.tables pt ON pt.object_id = fkc.parent_object_id \
         JOIN sys.schemas ps ON ps.schema_id = pt.schema_id \
         JOIN sys.columns pc ON pc.object_id = fkc.parent_object_id AND pc.column_id = fkc.parent_column_id \
         JOIN sys.tables rt ON rt.object_id = fkc.referenced_object_id \
         JOIN sys.schemas rs ON rs.schema_id = rt.schema_id \
         JOIN sys.columns rc ON rc.object_id = fkc.referenced_object_id AND rc.column_id = fkc.referenced_column_id \
         WHERE pt.name = ",
    );
    sql.push_str(&format!("N'{}'", escape_tds_text(table)));
    if !schema.is_empty() {
        sql.push_str(" AND ps.name = ");
        sql.push_str(&format!("N'{}'", escape_tds_text(schema)));
    }
    sql.push_str(" ORDER BY fk.name, fkc.constraint_column_id");
    sql
}
