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
  const quote = engine === "mysql" || engine === "mariadb" ? "`" : '"';
  return `${quote}${name.split(quote).join(quote + quote)}${quote}`;
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
