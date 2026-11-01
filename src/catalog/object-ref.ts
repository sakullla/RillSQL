import type { Engine, ObjectKind } from "./types";

/** 资源管理器里可打开的一张表或视图。 */
export interface CatalogObjectRef {
  connectionId: string;
  connectionName: string;
  engine: Engine;
  database: string;
  schema: string;
  name: string;
  kind: ObjectKind;
  signature?: string;
  relation?: string;
  stale: boolean;
  path: string;
}

export function objectIdentity(ref: Pick<CatalogObjectRef, "connectionId" | "database" | "schema" | "name"> & Partial<Pick<CatalogObjectRef, "kind" | "signature" | "relation">>): string {
  const base = `${ref.connectionId}:${ref.database}:${ref.schema}:${ref.name}`;
  if (!ref.kind || ref.kind === "table" || ref.kind === "view") return base;
  return `${base}:${ref.kind}:${ref.signature ?? ""}:${ref.relation ?? ""}`;
}

export function quoteIdent(engine: Engine, name: string): string {
  // T-SQL 的括号标识符转义是 ]]。
  if (engine === "sqlserver") return `[${name.replaceAll("]", "]]")}]`;
  const quote = engine === "mysql" || engine === "mariadb" ? "`" : '"';
  return `${quote}${name.split(quote).join(quote + quote)}${quote}`;
}

/**
 * SQL 里的限定名层级。SQLite 没有 schema 层，ATTACH 上来的库本身就是限定名：
 * extra 库里的表要写成 "extra"."t" 才查得到，main 保持不限定。
 */
export function queryScope(engine: Engine, database: string, schema: string): string {
  if (engine === "sqlite") return database === "" || database === "main" ? "" : database;
  return schema;
}

/** schema 为空时只引用表名，对应 MySQL、MariaDB 和 SQLite。 */
export function qualifiedObjectName(engine: Engine, schema: string, name: string): string {
  const table = quoteIdent(engine, name);
  if (!schema) return table;
  return `${quoteIdent(engine, schema)}.${table}`;
}

export function browseSql(engine: Engine, schema: string, name: string): string {
  return `SELECT *\nFROM ${qualifiedObjectName(engine, schema, name)}\nLIMIT 100;`;
}
