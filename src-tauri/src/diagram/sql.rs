pub(crate) const POSTGRES_CURRENT_DATABASE: &str = "SELECT current_database()::text";

pub(crate) const POSTGRES_TABLES: &str = "\
SELECT c.relname::text
FROM pg_catalog.pg_class c
JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
WHERE n.nspname = $1
  AND c.relkind IN ('r', 'p')
ORDER BY c.relname";

pub(crate) const POSTGRES_COLUMNS: &str = "\
SELECT a.attname::text,
       pg_catalog.format_type(a.atttypid, a.atttypmod)::text,
       EXISTS (
         SELECT 1
         FROM pg_catalog.pg_index i
         WHERE i.indrelid = c.oid
           AND i.indisprimary
           AND a.attnum = ANY (i.indkey)
       )
FROM pg_catalog.pg_attribute a
JOIN pg_catalog.pg_class c ON c.oid = a.attrelid
JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
WHERE n.nspname = $1
  AND c.relname = $2
  AND c.relkind IN ('r', 'p')
  AND a.attnum > 0
  AND NOT a.attisdropped
ORDER BY a.attnum";

pub(crate) const POSTGRES_FOREIGN_KEYS: &str = "\
SELECT con.conname::text,
       src_attr.attname::text,
       nref.nspname::text,
       tgt.relname::text,
       tgt_attr.attname::text,
       gs.ord::bigint
FROM pg_catalog.pg_constraint con
JOIN pg_catalog.pg_class src ON src.oid = con.conrelid
JOIN pg_catalog.pg_namespace nsrc ON nsrc.oid = src.relnamespace
JOIN pg_catalog.pg_class tgt ON tgt.oid = con.confrelid
JOIN pg_catalog.pg_namespace nref ON nref.oid = tgt.relnamespace
JOIN LATERAL generate_subscripts(con.conkey, 1) AS gs(ord) ON true
JOIN pg_catalog.pg_attribute src_attr
  ON src_attr.attrelid = con.conrelid AND src_attr.attnum = con.conkey[gs.ord]
JOIN pg_catalog.pg_attribute tgt_attr
  ON tgt_attr.attrelid = con.confrelid AND tgt_attr.attnum = con.confkey[gs.ord]
WHERE con.contype = 'f'
  AND nsrc.nspname = $1
  AND src.relname = $2
ORDER BY con.conname, gs.ord";

pub(crate) const MYSQL_TABLES: &str = "\
SELECT TABLE_NAME
FROM information_schema.TABLES
WHERE TABLE_SCHEMA = ? AND TABLE_TYPE = 'BASE TABLE'
ORDER BY TABLE_NAME";

pub(crate) const MYSQL_COLUMNS: &str = "\
SELECT COLUMN_NAME, COLUMN_TYPE, COLUMN_KEY
FROM information_schema.COLUMNS
WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?
ORDER BY ORDINAL_POSITION";

pub(crate) const MYSQL_FOREIGN_KEYS: &str = "\
SELECT CONSTRAINT_NAME, COLUMN_NAME, REFERENCED_TABLE_SCHEMA, REFERENCED_TABLE_NAME, REFERENCED_COLUMN_NAME, ORDINAL_POSITION
FROM information_schema.KEY_COLUMN_USAGE
WHERE TABLE_SCHEMA = ?
  AND TABLE_NAME = ?
  AND REFERENCED_TABLE_NAME IS NOT NULL
ORDER BY CONSTRAINT_NAME, ORDINAL_POSITION";

pub(crate) const SQLITE_TABLES: &str = "\
SELECT name
FROM sqlite_master
WHERE type = 'table' AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\'
ORDER BY name";

pub(crate) const SQLITE_COLUMNS: &str = "SELECT name, type, pk FROM pragma_table_info(?1) ORDER BY cid";

pub(crate) const SQLITE_FOREIGN_KEYS: &str = "\
SELECT id, seq, \"from\", \"table\", \"to\"
FROM pragma_foreign_key_list(?1)
ORDER BY id, seq";

/// SQL Server 只读基表。`{schema}` / `{table}` 在执行前换成已转义的字面量，不拼接标识符。
pub(crate) const SQLSERVER_TABLES: &str = "\
SELECT t.name
FROM sys.tables t
JOIN sys.schemas s ON s.schema_id = t.schema_id
WHERE s.name = N'{schema}'
ORDER BY t.name";

pub(crate) const SQLSERVER_COLUMNS: &str = "\
SELECT c.name,
       TYPE_NAME(c.user_type_id),
       CASE WHEN EXISTS (
         SELECT 1
         FROM sys.index_columns ic
         JOIN sys.indexes i ON i.object_id = ic.object_id AND i.index_id = ic.index_id
         WHERE i.is_primary_key = 1
           AND ic.object_id = c.object_id
           AND ic.column_id = c.column_id
       ) THEN 1 ELSE 0 END
FROM sys.columns c
JOIN sys.tables t ON t.object_id = c.object_id
JOIN sys.schemas s ON s.schema_id = t.schema_id
WHERE s.name = N'{schema}' AND t.name = N'{table}'
ORDER BY c.column_id";

pub(crate) const SQLSERVER_FOREIGN_KEYS: &str = "\
SELECT fk.name, pc.name, rs.name, rt.name, rc.name, fkc.constraint_column_id
FROM sys.foreign_keys fk
JOIN sys.foreign_key_columns fkc ON fkc.constraint_object_id = fk.object_id
JOIN sys.tables pt ON pt.object_id = fk.parent_object_id
JOIN sys.schemas ps ON ps.schema_id = pt.schema_id
JOIN sys.columns pc ON pc.object_id = fkc.parent_object_id AND pc.column_id = fkc.parent_column_id
JOIN sys.tables rt ON rt.object_id = fk.referenced_object_id
JOIN sys.schemas rs ON rs.schema_id = rt.schema_id
JOIN sys.columns rc ON rc.object_id = fkc.referenced_object_id AND rc.column_id = fkc.referenced_column_id
WHERE ps.name = N'{schema}' AND pt.name = N'{table}'
ORDER BY fk.name, fkc.constraint_column_id";

pub(crate) fn diagram_statements() -> &'static [&'static str] {
    &[
        POSTGRES_CURRENT_DATABASE,
        POSTGRES_TABLES,
        POSTGRES_COLUMNS,
        POSTGRES_FOREIGN_KEYS,
        MYSQL_TABLES,
        MYSQL_COLUMNS,
        MYSQL_FOREIGN_KEYS,
        SQLITE_TABLES,
        SQLITE_COLUMNS,
        SQLITE_FOREIGN_KEYS,
        SQLSERVER_TABLES,
        SQLSERVER_COLUMNS,
        SQLSERVER_FOREIGN_KEYS,
    ]
}
