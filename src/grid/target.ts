import type { CatalogConnection } from "../catalog/types";
import type { Engine } from "./types";

export interface TableTarget {
  connectionId: string;
  database: string;
  schema: string;
  table: string;
  engine: Engine;
}

/** 表标签 id 是 `table:` 加目录调色板 id。按已打开目录的连接、库、schema 前缀匹配，名称里可以有冒号。 */
export function resolveTableTarget(tabId: string, connections: readonly CatalogConnection[]): TableTarget | null {
  if (!tabId.startsWith("table:")) return null;
  const identity = tabId.slice("table:".length);
  const ordered = [...connections].sort((left, right) => right.id.length - left.id.length);
  for (const connection of ordered) {
    const prefix = `${connection.id}:`;
    if (!identity.startsWith(prefix)) continue;
    const rest = identity.slice(prefix.length);
    const databases = [...connection.databases].sort((left, right) => right.name.length - left.name.length);
    for (const database of databases) {
      const databasePrefix = `${database.name}:`;
      if (!rest.startsWith(databasePrefix)) continue;
      const afterDatabase = rest.slice(databasePrefix.length);
      const schemas = [...database.schemas].sort((left, right) => right.name.length - left.name.length);
      for (const schema of schemas) {
        const schemaPrefix = `${schema.name}:`;
        if (!afterDatabase.startsWith(schemaPrefix)) continue;
        const tableName = afterDatabase.slice(schemaPrefix.length);
        const object = schema.objects.find((item) => item.name === tableName && !item.stale);
        if (!object) continue;
        return {
          connectionId: connection.id,
          database: database.name,
          schema: schema.name,
          table: object.name,
          engine: connection.engine,
        };
      }
    }
  }
  return null;
}
