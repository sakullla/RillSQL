pub(crate) const POSTGRES_CURRENT_DATABASE: &str = "SELECT current_database()::text";

pub(crate) const POSTGRES_TABLES: &str = "\
SELECT c.relname::text
FROM pg_catalog.pg_class c
JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
WHERE n.nspname = $1
  AND c.relkind IN ('r', 'p')
ORDER BY c.relname";

pub(crate) const POSTGRES_COLUMNS: &str = "\
SELECT a.attname::text
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
SELECT COLUMN_NAME
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

pub(crate) const SQLITE_COLUMNS: &str = "SELECT name FROM pragma_table_info(?1) ORDER BY cid";

pub(crate) const SQLITE_FOREIGN_KEYS: &str = "\
SELECT id, seq, \"from\", \"table\", \"to\"
FROM pragma_foreign_key_list(?1)
ORDER BY id, seq";

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
    ]
}
