import { invoke } from "@tauri-apps/api/core";
import { catalogConnections } from "../catalog/store";
import { engineMeta } from "../engines";
import type { CatalogConnection, Engine } from "../catalog/types";

/** 全库导出（R7）的请求载荷：结构与 grid_export 的 TableRef 同源，engine 由目录树给出。 */
export interface DumpPayload {
  id: string;
  connectionId: string;
  engine: string;
  database: string;
  /** PostgreSQL 的 schema 范围；空数组表示全部 schema。 */
  schemas: string[];
  /** 表范围；空数组表示全部表。 */
  tables: string[];
  includeData: boolean;
  path: string;
}

export interface DumpResult {
  status: string;
  success: boolean;
  path: string | null;
  residualPath: string | null;
  message: string;
  objects: number;
  rows: number;
}

/** dump_progress 事件的载荷：phase 为 ddl 或 data，rows 是该表已写入的行数。 */
export interface DumpProgressEvent {
  id: string;
  object: string;
  phase: "ddl" | "data";
  rows: number;
}

export function databaseDump(request: DumpPayload): Promise<DumpResult> {
  return invoke<DumpResult>("database_dump", { request });
}

export function databaseDumpCancel(id: string): Promise<void> {
  return invoke<void>("database_dump_cancel", { id });
}

/** 对话框里的一个可选连接：只列已打开的会话，dump 复用已打开的连接，不另开新连接。 */
export interface DumpConnection {
  id: string;
  name: string;
  engine: Engine;
  databases: string[];
  defaultDatabase: string | null;
  /** PostgreSQL 的 schema 名；其余引擎为空（单范围）。 */
  schemasOf: (database: string) => string[];
  /** 目录树已读取的表名（可能不全，仅用于勾选范围）。 */
  tablesOf: (database: string, schemas: readonly string[]) => string[];
}

function schemaNames(connection: CatalogConnection, database: string): string[] {
  const node = connection.databases.find((item) => item.name === database);
  if (!node) return [];
  return node.schemas.map((schema) => schema.name).filter((name) => name.trim().length > 0);
}

function tableNames(connection: CatalogConnection, database: string, schemas: readonly string[]): string[] {
  const node = connection.databases.find((item) => item.name === database);
  if (!node) return [];
  const wanted = schemas.length > 0 ? schemas : node.schemas.map((schema) => schema.name);
  const names = new Set<string>();
  for (const schema of node.schemas) {
    if (!wanted.includes(schema.name)) continue;
    for (const object of schema.objects) {
      if (object.kind === "table") names.add(object.name);
    }
  }
  return [...names].sort((left, right) => left.localeCompare(right));
}

/** 已打开的连接转成对话框可选项；未打开的连接没有会话可复用，不进列表。
 *  能力门控（ADR-2）：kv/document 引擎没有整库导出/迁移，不进列表。 */
export function dumpConnections(): DumpConnection[] {
  return catalogConnections()
    .filter((connection) => connection.status === "connected")
    .filter((connection) => engineMeta(connection.engine).relational)
    .map((connection) => ({
      id: connection.id,
      name: connection.name,
      engine: connection.engine,
      databases: connection.databases.map((item) => item.name),
      defaultDatabase: connection.defaultDatabase,
      schemasOf: (database: string) => schemaNames(connection, database),
      tablesOf: (database: string, schemas: readonly string[]) => tableNames(connection, database, schemas),
    }));
}
