import type { CatalogConnection } from "../catalog/types";
import { engineMeta } from "../connections/model";
import { CONNECTION_STATUS_TEXT } from "../shell/status-text";

export const SQL_KEYWORDS = [
  "SELECT", "FROM", "WHERE", "AND", "OR", "NOT", "NULL", "IS", "IN", "EXISTS",
  "INSERT", "INTO", "VALUES", "UPDATE", "SET", "DELETE", "CREATE", "ALTER", "DROP",
  "TABLE", "VIEW", "INDEX", "JOIN", "LEFT", "RIGHT", "INNER", "OUTER", "ON",
  "GROUP", "BY", "ORDER", "LIMIT", "OFFSET", "AS", "DISTINCT", "HAVING", "UNION",
  "ALL", "CASE", "WHEN", "THEN", "ELSE", "END", "WITH", "BEGIN", "COMMIT", "ROLLBACK",
] as const;

export interface CompletionObject {
  label: string;
  name: string;
  schema: string;
  kind: "table" | "view";
}

export interface MetadataNote {
  keywordsOnly: boolean;
  note: string;
  objects: CompletionObject[];
}

export interface CompletionItem {
  label: string;
  kind: "keyword" | "table" | "view";
  detail: string;
}

export function metadataNote(connection: CatalogConnection | null, database: string | null): MetadataNote {
  if (!connection || database == null) {
    return { keywordsOnly: true, note: "未选择执行目标：只提供关键字。", objects: [] };
  }
  if (connection.status !== "connected") {
    return { keywordsOnly: true, note: `${connection.name} 当前是${CONNECTION_STATUS_TEXT[connection.status]}：只提供关键字。`, objects: [] };
  }
  if (connection.error) {
    return { keywordsOnly: true, note: `元数据读取失败（${connection.error}）：只提供关键字。`, objects: [] };
  }
  const db = connection.databases.find((item) => item.name === database);
  if (!db) {
    return { keywordsOnly: true, note: `还没有读到数据库 ${database || "main"} 的元数据：只提供关键字。`, objects: [] };
  }
  if (db.error) {
    return { keywordsOnly: true, note: `元数据读取失败（${db.error}）：只提供关键字。`, objects: [] };
  }
  const failed = db.schemas.filter((schema) => schema.error);
  const ready = db.schemas.filter((schema) => !schema.error);
  if (db.schemas.length > 0 && ready.length === 0) {
    const reason = failed.map((schema) => `${schema.name || "默认"}：${schema.error}`).join("；");
    return { keywordsOnly: true, note: `元数据读取失败（${reason}）：只提供关键字。`, objects: [] };
  }
  const objects: CompletionObject[] = [];
  for (const schema of ready) {
    for (const object of schema.objects) {
      if (object.stale) continue;
      objects.push({
        label: schema.name ? `${schema.name}.${object.name}` : object.name,
        name: object.name,
        schema: schema.name,
        kind: object.kind === "view" ? "view" : "table",
      });
    }
  }
  if (failed.length) {
    const reason = failed.map((schema) => `${schema.name || "默认"}（${schema.error}）`).join("、");
    return { keywordsOnly: false, note: `部分 schema 读取失败，补全不包含这些对象：${reason}。`, objects };
  }
  if (!objects.length) {
    return { keywordsOnly: true, note: "当前目标还没有已读取的表或视图：只提供关键字。", objects: [] };
  }
  return {
    keywordsOnly: false,
    note: `补全来自 ${connection.name} / ${database || "main"} 已读取的表和视图，不含未读取的字段。`,
    objects,
  };
}

export function completionPlan(input: { prefix: string; connection: CatalogConnection | null; database: string | null }): {
  items: CompletionItem[];
  note: string;
  keywordsOnly: boolean;
} {
  const meta = metadataNote(input.connection, input.database);
  const prefix = input.prefix.toLowerCase();
  const items: CompletionItem[] = [];
  if (!meta.keywordsOnly) {
    for (const object of meta.objects) {
      const matched = !prefix || object.label.toLowerCase().startsWith(prefix) || object.name.toLowerCase().startsWith(prefix);
      if (!matched) continue;
      items.push({ label: object.label, kind: object.kind, detail: object.schema || "对象" });
      if (items.length >= 20) break;
    }
  }
  for (const keyword of SQL_KEYWORDS) {
    if (prefix && !keyword.toLowerCase().startsWith(prefix)) continue;
    if (keyword.toLowerCase() === prefix) continue;
    items.push({ label: keyword, kind: "keyword", detail: "关键字" });
    if (items.length >= 40) break;
  }
  return { items, note: meta.note, keywordsOnly: meta.keywordsOnly };
}

export interface TargetOption {
  key: string;
  connectionId: string;
  name: string;
  database: string;
  engine: CatalogConnection["engine"];
  envLabel: string;
  envTone: CatalogConnection["envTone"];
  status: CatalogConnection["status"];
  label: string;
}

/** Engine name, environment, and connection state. Same host and database stay distinguishable. */
export function targetDetail(option: TargetOption): string {
  const parts = [engineMeta(option.engine).label];
  if (option.envTone !== "none" && option.envLabel) parts.push(option.envLabel);
  if (option.status !== "connected") parts.push(CONNECTION_STATUS_TEXT[option.status]);
  return parts.join(" · ");
}

export function targetKey(connectionId: string, database: string): string {
  return `${connectionId}\u0000${database}`;
}

export function targetOptions(connections: CatalogConnection[]): TargetOption[] {
  const options: TargetOption[] = [];
  for (const connection of connections) {
    const databases = connection.databases.length
      ? connection.databases
      : [{ name: connection.defaultDatabase || (connection.engine === "sqlite" ? "main" : ""), schemas: [], error: connection.error }];
    for (const database of databases) {
      options.push({
        key: targetKey(connection.id, database.name),
        connectionId: connection.id,
        name: connection.name,
        database: database.name,
        engine: connection.engine,
        envLabel: connection.envLabel,
        envTone: connection.envTone,
        status: connection.status,
        label: `${connection.name} / ${database.name || "main"}`,
      });
    }
  }
  return options;
}

export function connectionById(connections: CatalogConnection[], id: string | null): CatalogConnection | null {
  if (!id) return null;
  return connections.find((item) => item.id === id) ?? null;
}
