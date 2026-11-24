//! PostgreSQL 的结构内省与执行：pg_catalog 读表结构、序列、枚举与域，逐条执行结构变更。

use std::collections::BTreeMap;

use crate::adapters::ddl::{paren_list, Comments};
use crate::connections::model::Engine;
use crate::ddl::exec::receipt;
use crate::ddl::introspect::keep_rows;
use crate::ddl::model::{Column, DependentSql, DomainState, ForeignKey, IncomingForeignKey, Index, IndexColumn, IndexKind, SequenceState, Snapshot, Step, StepReceipt, StepStatus};
use crate::ddl::sql::{mentions, qualified_table};

/// 改名前查新名字：PostgreSQL 的关系（表、视图、索引、序列）共用 schema 里的名字。
pub(super) async fn name_taken(client: &tokio_postgres::Client, schema: &str, current: &str, name: &str) -> Result<bool, String> {
    let row = client
        .query_one(
            "SELECT COUNT(*) FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 AND c.relname = $2 AND c.relname <> $3",
            &[&schema, &name, &current],
        )
        .await
        .map_err(|err| crate::error_text::chain(&err))?;
    let count: i64 = row.get(0);
    Ok(count > 0)
}

pub(super) async fn count(client: &tokio_postgres::Client, sql: &str) -> Result<i64, String> {
    let row = client.query_one(sql, &[]).await.map_err(|err| crate::error_text::chain(&err))?;
    row.try_get(0).map_err(|err| crate::error_text::chain(&err))
}

pub(super) async fn postgres_comments(client: &tokio_postgres::Client, schema: &str, table: &str) -> Result<Comments, String> {
    let rows = client
        .query(
            "SELECT a.attname::text, col_description(c.oid, a.attnum::int), obj_description(c.oid, 'pg_class') FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace LEFT JOIN pg_attribute a ON a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped WHERE n.nspname = $1 AND c.relname = $2 ORDER BY a.attnum",
            &[&schema, &table],
        )
        .await
        .map_err(|err| crate::error_text::chain(&err))?;
    let mut table_comment = None;
    let mut columns = BTreeMap::new();
    for row in &rows {
        let column: Option<String> = row.get(0);
        let comment: Option<String> = row.get(1);
        let described: Option<String> = row.get(2);
        if table_comment.is_none() {
            table_comment = described.filter(|text| !text.is_empty());
        }
        if let (Some(column), Some(comment)) = (column, comment) {
            if !comment.is_empty() {
                columns.insert(column, comment);
            }
        }
    }
    Ok((table_comment, columns))
}

pub(super) async fn postgres_sequence(client: &tokio_postgres::Client, schema: &str, name: &str) -> Result<SequenceState, String> {
    let row = client
        .query_opt(
            "SELECT start_value, increment_by, min_value, max_value, cycle FROM pg_catalog.pg_sequences WHERE schemaname = $1 AND sequencename = $2",
            &[&schema, &name],
        )
        .await
        .map_err(|err| format!("读不到序列 {name}：{err}"))?;
    let Some(row) = row else {
        return Err(format!("找不到序列 {name}。"));
    };
    // 没有权限时这些列是 NULL：plan 会因此拒绝生成语句，而不是当成 0。
    Ok(SequenceState {
        start: row.get::<_, Option<i64>>(0),
        increment: row.get::<_, Option<i64>>(1),
        min_value: row.get::<_, Option<i64>>(2),
        max_value: row.get::<_, Option<i64>>(3),
        cycle: row.get::<_, Option<bool>>(4),
    })
}

async fn postgres_type_kind(client: &tokio_postgres::Client, schema: &str, name: &str) -> Result<Option<String>, String> {
    let row = client
        .query_opt(
            "SELECT t.typtype::text FROM pg_catalog.pg_type t JOIN pg_catalog.pg_namespace n ON n.oid = t.typnamespace WHERE n.nspname = $1 AND t.typname = $2",
            &[&schema, &name],
        )
        .await
        .map_err(|err| format!("读不到类型 {name}：{err}"))?;
    Ok(row.map(|row| row.get::<_, String>(0)))
}

pub(super) async fn postgres_enum_values(client: &tokio_postgres::Client, schema: &str, name: &str) -> Result<Vec<String>, String> {
    match postgres_type_kind(client, schema, name).await?.as_deref() {
        None => return Err(format!("找不到类型 {name}。")),
        Some("e") => {}
        Some(_) => return Err(format!("{name} 不是枚举类型，枚举值只能加在枚举上。")),
    }
    let rows = client
        .query(
            "SELECT e.enumlabel FROM pg_catalog.pg_enum e JOIN pg_catalog.pg_type t ON t.oid = e.enumtypid JOIN pg_catalog.pg_namespace n ON n.oid = t.typnamespace WHERE n.nspname = $1 AND t.typname = $2 ORDER BY e.enumsortorder",
            &[&schema, &name],
        )
        .await
        .map_err(|err| format!("读不到枚举 {name} 的取值：{err}"))?;
    Ok(rows.iter().map(|row| row.get::<_, String>(0)).collect())
}

pub(super) async fn postgres_domain(client: &tokio_postgres::Client, schema: &str, name: &str) -> Result<DomainState, String> {
    match postgres_type_kind(client, schema, name).await?.as_deref() {
        None => return Err(format!("找不到类型 {name}。")),
        Some("d") => {}
        Some(_) => return Err(format!("{name} 不是域（domain）。")),
    }
    let row = client
        .query_opt(
            "SELECT t.typnotnull, pg_catalog.pg_get_expr(t.typdefaultbin, 0) FROM pg_catalog.pg_type t JOIN pg_catalog.pg_namespace n ON n.oid = t.typnamespace WHERE n.nspname = $1 AND t.typname = $2",
            &[&schema, &name],
        )
        .await
        .map_err(|err| format!("读不到域 {name} 的定义：{err}"))?;
    let Some(row) = row else {
        return Err(format!("找不到域 {name}。"));
    };
    Ok(DomainState { not_null: row.get::<_, bool>(0), default_sql: row.get::<_, Option<String>>(1) })
}

/// 从访问方法与 `pg_get_indexdef` 原文里找出页面编辑器表达不了的属性；先删后按普通
/// `CREATE [UNIQUE] INDEX name ON t (col [DESC], ...)` 重建会丢掉它们。只接受 btree、每一项都是列名加可选 DESC、
/// 括号后除 WHERE（部分索引另有标记）外没有其他子句的定义；解析不了的原文也算会丢。
pub(crate) fn postgres_index_losses(method: &str, def: &str) -> Vec<String> {
    let mut losses = Vec::new();
    let method = method.trim();
    if !method.is_empty() && !method.eq_ignore_ascii_case("btree") {
        losses.push(format!("USING {method}"));
    }
    let Some((head, keys, tail)) = split_index_def(def) else {
        losses.push("pg_get_indexdef".to_owned());
        return losses;
    };
    if head.contains(" ON ONLY ") {
        losses.push("ON ONLY".to_owned());
    }
    for item in split_top_level(keys) {
        // 单项顺序：列 [COLLATE 规则] [操作符类] [ASC|DESC] [NULLS FIRST|LAST]
        let mut rest = skip_identifier(item.trim()).trim();
        let mut found: Vec<&str> = Vec::new();
        for (suffix, label) in [("NULLS FIRST", "NULLS FIRST"), ("NULLS LAST", "NULLS LAST")] {
            if let Some(head) = rest.strip_suffix(suffix) {
                found.push(label);
                rest = head.trim_end();
            }
        }
        rest = rest.strip_suffix("DESC").or_else(|| rest.strip_suffix("ASC")).unwrap_or(rest).trim_end();
        if let Some(after) = rest.strip_prefix("COLLATE ") {
            found.push("COLLATE");
            let mut name = skip_identifier(after.trim_start());
            while let Some(next) = name.strip_prefix('.') {
                name = skip_identifier(next);
            }
            rest = name.trim();
        }
        if !rest.is_empty() {
            found.push("OPERATOR CLASS");
        }
        for label in found {
            if !losses.iter().any(|item| item == label) {
                losses.push(label.to_owned());
            }
        }
    }
    for (needle, label) in [("NULLS NOT DISTINCT", "NULLS NOT DISTINCT"), ("INCLUDE (", "INCLUDE"), ("WITH (", "WITH"), ("TABLESPACE ", "TABLESPACE")] {
        if tail.contains(needle) {
            losses.push(label.to_owned());
        }
    }
    losses
}

/// 把 `pg_get_indexdef` 拆成「USING 之前」「键列括号内」「括号之后」三段；引号里的内容不参与匹配。
fn split_index_def(def: &str) -> Option<(&str, &str, &str)> {
    let bytes = def.as_bytes();
    let mut quoted = false;
    let mut using = None;
    for (at, byte) in bytes.iter().enumerate() {
        match byte {
            b'"' => quoted = !quoted,
            b' ' if !quoted && def[at..].starts_with(" USING ") => {
                using = Some(at);
                break;
            }
            _ => {}
        }
    }
    let using = using?;
    let open = using + def[using..].find('(')?;
    let mut depth = 0usize;
    let mut quoted = false;
    for (offset, byte) in bytes[open..].iter().enumerate() {
        match byte {
            b'"' => quoted = !quoted,
            b'(' if !quoted => depth += 1,
            b')' if !quoted => {
                depth -= 1;
                if depth == 0 {
                    let close = open + offset;
                    return Some((&def[..using], &def[open + 1..close], &def[close + 1..]));
                }
            }
            _ => {}
        }
    }
    None
}

/// 按顶层逗号拆开键列；引号与括号里的逗号不算。
fn split_top_level(text: &str) -> Vec<&str> {
    let mut items = Vec::new();
    let (mut depth, mut quoted, mut start) = (0usize, false, 0usize);
    for (at, byte) in text.bytes().enumerate() {
        match byte {
            b'"' => quoted = !quoted,
            b'(' if !quoted => depth += 1,
            b')' if !quoted => depth = depth.saturating_sub(1),
            b',' if !quoted && depth == 0 => {
                items.push(&text[start..at]);
                start = at + 1;
            }
            _ => {}
        }
    }
    items.push(&text[start..]);
    items
}

/// 跳过开头的一个标识符（普通名字或双引号名字），返回其后的部分。
fn skip_identifier(item: &str) -> &str {
    if let Some(rest) = item.strip_prefix('"') {
        let bytes = rest.as_bytes();
        let mut at = 0;
        while at < bytes.len() {
            if bytes[at] == b'"' {
                if bytes.get(at + 1) == Some(&b'"') {
                    at += 2;
                    continue;
                }
                return &rest[at + 1..];
            }
            at += 1;
        }
        return "";
    }
    let end = item.find(|ch: char| !(ch.is_alphanumeric() || ch == '_' || ch == '$')).unwrap_or(item.len());
    &item[end..]
}

pub(super) async fn load_postgres(client: &tokio_postgres::Client, database: &str, schema: &str, table: &str) -> Result<Snapshot, String> {
    if schema.is_empty() {
        return Err("PostgreSQL 需要指定 schema（表所在的分组名），没有另开连接。".to_owned());
    }
    let current = client.query_one("SELECT current_database()", &[]).await.map_err(|err| crate::error_text::chain(&err))?;
    let current: Option<String> = current.try_get(0).map_err(|err| crate::error_text::chain(&err))?;
    if current.as_deref().unwrap_or("") != database {
        return Err(format!("当前连接打开的是 {}，没有另开连接修改 {database}", current.unwrap_or_default()));
    }
    let kind_rows = client
        .query(
            "SELECT c.relkind::text, c.oid FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 AND c.relname = $2 AND c.relkind IN ('r','p','v','m')",
            &[&schema, &table],
        )
        .await
        .map_err(|err| crate::error_text::chain(&err))?;
    let Some(kind_row) = kind_rows.first() else {
        return Err(format!("找不到表或视图 {schema}.{table}"));
    };
    let kind_text: String = kind_row.get(0);
    let oid: u32 = kind_row.get(1);
    let mut snapshot = Snapshot::bare(Engine::Postgres, database, schema, table);
    snapshot.kind = if kind_text == "r" || kind_text == "p" { "table" } else { "view" }.to_owned();
    snapshot.materialized = kind_text == "m";
    let columns = client
        .query(
            "SELECT a.attname, pg_catalog.format_type(a.atttypid, a.atttypmod), a.attnotnull, pg_get_expr(ad.adbin, ad.adrelid), a.attidentity::text, a.attgenerated::text FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid LEFT JOIN pg_attrdef ad ON ad.adrelid = a.attrelid AND ad.adnum = a.attnum WHERE a.attrelid = $1 AND a.attnum > 0 AND NOT a.attisdropped ORDER BY a.attnum",
            &[&oid],
        )
        .await
        .map_err(|err| crate::error_text::chain(&err))?;
    for row in &columns {
        let identity: String = row.get(4);
        let generated: String = row.get(5);
        let default_sql: Option<String> = row.get(3);
        snapshot.columns.push(Column {
            name: row.get(0),
            type_name: row.get(1),
            nullable: !row.get::<_, bool>(2),
            default_sql,
            primary: false,
            auto: identity == "a" || identity == "d",
            generated: !generated.is_empty(),
            default_constraint: None,
        });
    }
    // indkey 与 indoption 都是从 0 起的 int2vector：array_position 给出的就是 indoption 的下标。
    // 只取前 indnkeyatts 个键列，INCLUDE 列不算键（会丢的属性另记在 rebuild_loses）。
    let indexes = keep_rows(
        &mut snapshot.index_error,
        client
            .query(
                "SELECT i.relname, ix.indisunique, ix.indisprimary, ix.indpred IS NOT NULL, ix.indexprs IS NOT NULL, pg_get_indexdef(ix.indexrelid), a.attname, array_position(ix.indkey, a.attnum), COALESCE((ix.indoption[array_position(ix.indkey, a.attnum)] & 1) = 1, false), am.amname::text FROM pg_index ix JOIN pg_class i ON i.oid = ix.indexrelid LEFT JOIN pg_am am ON am.oid = i.relam LEFT JOIN pg_attribute a ON a.attrelid = ix.indrelid AND a.attnum = ANY (ix.indkey) AND array_position(ix.indkey, a.attnum) < ix.indnkeyatts WHERE ix.indrelid = $1 ORDER BY i.relname, array_position(ix.indkey, a.attnum)",
                &[&oid],
            )
            .await
            .map_err(|err| crate::error_text::chain(&err)),
    );
    for row in &indexes {
        let name: String = row.get(0);
        let column_name: Option<String> = row.get(6);
        let descending: bool = row.get(8);
        let column = column_name.map(|name| IndexColumn { name, descending, prefix: None });
        if let Some(index) = snapshot.indexes.iter_mut().find(|index| index.name == name) {
            if let Some(column) = column {
                index.columns.push(column);
            } else {
                index.expression = true;
            }
        } else {
            let primary: bool = row.get(2);
            let expression = row.get::<_, bool>(4) || column.is_none();
            let method: Option<String> = row.get(9);
            let sql: Option<String> = row.get(5);
            let rebuild_loses = postgres_index_losses(method.as_deref().unwrap_or(""), sql.as_deref().unwrap_or(""));
            snapshot.indexes.push(Index {
                name,
                unique: row.get(1),
                columns: column.into_iter().collect(),
                protected: primary,
                reason: if primary { "由主键约束维护".to_owned() } else { String::new() },
                partial: row.get(3),
                expression,
                sql,
                kind: IndexKind::infer(primary, row.get(1)),
                method,
                rebuild_loses,
                ..Index::default()
            });
        }
    }
    let constraints = keep_rows(
        &mut snapshot.foreign_key_error,
        client
            .query(
                "SELECT con.contype::text, con.conname, pg_get_constraintdef(con.oid), i.relname FROM pg_constraint con LEFT JOIN pg_class i ON i.oid = con.conindid WHERE con.conrelid = $1",
                &[&oid],
            )
            .await
            .map_err(|err| crate::error_text::chain(&err)),
    );
    for row in &constraints {
        let contype: String = row.get(0);
        let name: String = row.get(1);
        let definition: String = row.get(2);
        let index_name: Option<String> = row.get(3);
        if contype == "c" {
            snapshot.has_check = true;
        }
        if matches!(contype.as_str(), "p" | "u" | "f") {
            if let Some(index_name) = index_name {
                if let Some(index) = snapshot.indexes.iter_mut().find(|index| index.name == index_name) {
                    index.protected = true;
                    index.reason = match contype.as_str() {
                        "p" => "由主键约束维护".to_owned(),
                        "f" => format!("外键 {name} 依赖此索引"),
                        _ => format!("由唯一约束 {name} 维护"),
                    };
                }
            }
        }
        if contype == "f" {
            if let Some(parsed) = parse_fk_def(&name, &definition) {
                snapshot.foreign_keys.push(parsed);
            }
        }
    }
    for column in &mut snapshot.columns {
        column.primary = snapshot.indexes.iter().any(|index| index.reason.contains("主键") && index.columns.iter().any(|item| item.name == column.name));
    }
    let incoming = keep_rows(
        &mut snapshot.foreign_key_error,
        client
            .query(
                "SELECT con.conname, n.nspname, c.relname, pg_get_constraintdef(con.oid) FROM pg_constraint con JOIN pg_class c ON c.oid = con.conrelid JOIN pg_namespace n ON n.oid = c.relnamespace WHERE con.contype = 'f' AND con.confrelid = $1",
                &[&oid],
            )
            .await
            .map_err(|err| crate::error_text::chain(&err)),
    );
    for row in &incoming {
        let name: String = row.get(0);
        let schema_name: String = row.get(1);
        let table_name: String = row.get(2);
        let definition: String = row.get(3);
        if let Some(parsed) = parse_fk_def(&name, &definition) {
            snapshot.incoming.push(IncomingForeignKey {
                name,
                table: table_name,
                schema: schema_name,
                columns: parsed.columns,
                ref_columns: parsed.ref_columns,
            });
        }
    }
    let triggers = client
        .query("SELECT tgname, pg_get_triggerdef(oid) FROM pg_trigger WHERE tgrelid = $1 AND NOT tgisinternal", &[&oid])
        .await
        .map_err(|err| crate::error_text::chain(&err))?;
    for row in &triggers {
        snapshot.triggers.push(DependentSql { name: row.get(0), sql: row.get(1) });
    }
    let views = client
        .query("SELECT c.relname, pg_get_viewdef(c.oid) FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 AND c.relkind IN ('v','m')", &[&schema])
        .await
        .map_err(|err| crate::error_text::chain(&err))?;
    for row in &views {
        let name: String = row.get(0);
        let sql: String = row.get(1);
        if mentions(&sql, table) {
            snapshot.views.push(DependentSql { name, sql });
        }
    }
    // 估算行数：先用 LIMIT 1 确认有没有行，再读统计信息；不在大表上跑 COUNT(*)。
    let qualified = qualified_table(Engine::Postgres, database, schema, table)?;
    let any = client
        .query_opt(&format!("SELECT 1 FROM {qualified} LIMIT 1"), &[])
        .await
        .map_err(|err| crate::error_text::chain(&err))?;
    snapshot.row_count = Some(if any.is_none() {
        0
    } else {
        let estimate = client
            .query_opt(
                "SELECT GREATEST(c.reltuples, 0)::bigint FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 AND c.relname = $2",
                &[&schema, &table],
            )
            .await
            .map_err(|err| crate::error_text::chain(&err))?;
        estimate.and_then(|row| row.try_get::<_, i64>(0).ok()).unwrap_or(0).max(1)
    });
    snapshot.object_exists = true;
    if snapshot.kind == "view" {
        match postgres_view_definition(client, oid, database, schema, table, snapshot.materialized).await {
            Ok(sql) => snapshot.table_sql = sql,
            Err(message) => {
                snapshot.table_sql.clear();
                snapshot.definition_error = Some(message);
            }
        }
    } else {
        snapshot.table_sql = format!("-- {schema}.{table}");
    }
    Ok(snapshot)
}

async fn postgres_view_definition(
    client: &tokio_postgres::Client,
    oid: u32,
    database: &str,
    schema: &str,
    table: &str,
    materialized: bool,
) -> Result<String, String> {
    let row = client
        .query_one("SELECT pg_get_viewdef($1::oid, true)", &[&oid])
        .await
        .map_err(|err| crate::error_text::chain(&err))?;
    let body: String = row.get(0);
    if body.trim().is_empty() {
        return Err("没有读到视图定义".to_owned());
    }
    let qualified = qualified_table(Engine::Postgres, database, schema, table)?;
    let body = body.trim().trim_end_matches(';').trim();
    if materialized {
        Ok(format!("CREATE MATERIALIZED VIEW {qualified} AS\n{body};"))
    } else {
        Ok(format!("CREATE OR REPLACE VIEW {qualified} AS\n{body};"))
    }
}

fn parse_fk_def(name: &str, definition: &str) -> Option<ForeignKey> {
    let lower = definition.to_ascii_lowercase();
    let key_at = lower.find("foreign key")?;
    let columns = paren_list(&definition[key_at + "foreign key".len()..])?;
    let ref_at = lower.find("references")?;
    let after = definition[ref_at + "references".len()..].trim();
    let (ref_schema, ref_table, ref_columns) = parse_reference(after)?;
    let on_delete = clause_action(definition, "on delete").unwrap_or_else(|| "NO ACTION".to_owned());
    let on_update = clause_action(definition, "on update").unwrap_or_else(|| "NO ACTION".to_owned());
    Some(ForeignKey { name: name.to_owned(), columns, ref_schema, ref_table, ref_columns, on_delete, on_update })
}

fn parse_reference(text: &str) -> Option<(String, String, Vec<String>)> {
    let paren = text.find('(')?;
    let head = text[..paren].trim().trim_matches('"');
    let mut parts = head.split('.').map(|part| part.trim().trim_matches('"').to_owned()).collect::<Vec<_>>();
    let ref_table = parts.pop()?;
    let ref_schema = parts.pop().unwrap_or_default();
    let ref_columns = paren_list(&text[paren..])?;
    Some((ref_schema, ref_table, ref_columns))
}

fn clause_action(definition: &str, clause: &str) -> Option<String> {
    let lower = definition.to_ascii_lowercase();
    let pos = lower.find(clause)?;
    let rest = definition[pos + clause.len()..].trim();
    let words = rest.split_whitespace().take(2).collect::<Vec<_>>();
    if words.first().map(|word| word.eq_ignore_ascii_case("SET")).unwrap_or(false) {
        Some(format!("SET {}", words.get(1).unwrap_or(&"").trim_matches(',').to_ascii_uppercase()))
    } else {
        Some(words.first()?.trim_matches(',').to_ascii_uppercase())
    }
}

pub(in crate::adapters) async fn run_steps(client: &tokio_postgres::Client, steps: &[Step]) -> Result<Vec<StepReceipt>, String> {
    let mut results = Vec::new();
    let mut stop = false;
    for step in steps {
        if stop {
            results.push(receipt(step, StepStatus::Skipped, "前面的语句没有成功，这条没有执行。"));
            continue;
        }
        match client.batch_execute(&step.sql).await {
            Ok(()) => results.push(receipt(step, StepStatus::Ok, "")),
            Err(err) if err.as_db_error().is_some() => {
                stop = true;
                results.push(receipt(step, StepStatus::Failed, &crate::error_text::chain(&err)));
            }
            Err(err) => {
                stop = true;
                results.push(receipt(step, StepStatus::Unknown, &crate::error_text::chain(&err)));
            }
        }
    }
    Ok(results)
}
