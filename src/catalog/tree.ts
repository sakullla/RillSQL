import type { CatalogConnection, CatalogObject, ConnStatus, Engine, EngineShape, SchemaNode } from "./types";

export function engineShape(engine: Engine): EngineShape {
  if (engine === "postgres") return { databases: true, schemas: true, foreignKeys: false };
  if (engine === "sqlite") return { databases: false, schemas: false, foreignKeys: true };
  return { databases: true, schemas: false, foreignKeys: false };
}

export function isConnected(status: ConnStatus): boolean {
  return status === "connected";
}

/** 空的 schema 名不占路径一段。MySQL / MariaDB / SQLite 没有独立 schema 层。 */
export function objectPath(connection: string, database: string, schema: string): string {
  return [connection, database, schema].filter((part) => part.length > 0).join(" / ");
}

export function reconcileObjects(previous: CatalogObject[], incoming: CatalogObject[]): CatalogObject[] {
  const nextByName = new Map(incoming.map((item) => [item.name, item]));
  const seen = new Set<string>();
  const objects: CatalogObject[] = [];
  for (const old of previous) {
    seen.add(old.name);
    if (old.stale) continue;
    const fresh = nextByName.get(old.name);
    if (!fresh) {
      objects.push({ ...old, stale: true });
      continue;
    }
    objects.push({ ...fresh, stale: false });
  }
  for (const item of incoming) {
    if (!seen.has(item.name)) objects.push({ ...item, stale: false });
  }
  return objects;
}

export function refreshSchema(previous: SchemaNode, incoming: SchemaNode): SchemaNode {
  if (incoming.error) {
    return {
      name: incoming.name || previous.name,
      objects: previous.objects.map((item) => ({ ...item })),
      total: previous.objects.length,
      error: incoming.error,
    };
  }
  const objects = reconcileObjects(previous.objects, incoming.objects);
  return {
    name: incoming.name,
    objects,
    total: Math.max(incoming.total, objects.length),
    error: null,
  };
}

export function refreshConnection(previous: CatalogConnection, incoming: CatalogConnection): CatalogConnection {
  if (incoming.error && incoming.databases.length === 0) {
    return {
      ...incoming,
      foreignKeys: incoming.engine === "sqlite" ? (incoming.foreignKeys ?? previous.foreignKeys) : null,
      databases: previous.databases.map((database) => ({
        ...database,
        schemas: database.schemas.map((schema) => ({
          ...schema,
          objects: schema.objects.map((object) => ({ ...object })),
        })),
      })),
    };
  }
  const previousDatabases = new Map(previous.databases.map((database) => [database.name, database]));
  return {
    ...incoming,
    foreignKeys: incoming.engine === "sqlite" ? incoming.foreignKeys : null,
    databases: incoming.databases.map((database) => {
      const oldDatabase = previousDatabases.get(database.name);
      if (!oldDatabase) return database;
      if (database.error) {
        return {
          ...oldDatabase,
          error: database.error,
        };
      }
      const previousSchemas = new Map(oldDatabase.schemas.map((schema) => [schema.name, schema]));
      return {
        ...database,
        schemas: database.schemas.map((schema) => {
          const oldSchema = previousSchemas.get(schema.name);
          return oldSchema ? refreshSchema(oldSchema, schema) : schema;
        }),
      };
    }),
  };
}

export interface SearchHit {
  id: string;
  name: string;
  path: string;
  env: CatalogConnection["env"];
  envLabel: string;
  stale: boolean;
  connectionId: string;
  database: string;
  schema: string;
}

export function searchObjects(connections: CatalogConnection[], query: string): { hits: SearchHit[]; skipped: string[] } {
  const needle = query.trim().toLowerCase();
  const hits: SearchHit[] = [];
  const skipped: string[] = [];
  for (const connection of connections) {
    if (!isConnected(connection.status)) {
      skipped.push(connection.name);
      continue;
    }
    if (!needle) continue;
    for (const database of connection.databases) {
      if (database.error) continue;
      for (const schema of database.schemas) {
        if (schema.error && schema.objects.length === 0) continue;
        for (const object of schema.objects) {
          if (!object.name.toLowerCase().includes(needle)) continue;
          hits.push({
            id: `${connection.id}:${database.name}:${schema.name}:${object.name}`,
            name: object.name,
            path: objectPath(connection.name, database.name, schema.name),
            env: connection.env,
            envLabel: connection.envLabel,
            stale: object.stale,
            connectionId: connection.id,
            database: database.name,
            schema: schema.name,
          });
        }
      }
    }
  }
  return { hits, skipped };
}

export function splitMatch(text: string, query: string): { text: string; hit: boolean }[] {
  const needle = query.trim().toLowerCase();
  if (!needle) return [{ text, hit: false }];
  const index = text.toLowerCase().indexOf(needle);
  if (index < 0) return [{ text, hit: false }];
  return [
    { text: text.slice(0, index), hit: false },
    { text: text.slice(index, index + needle.length), hit: true },
    { text: text.slice(index + needle.length), hit: false },
  ].filter((part) => part.text.length > 0);
}

export function schemaFailed(schema: SchemaNode): boolean {
  return schema.error !== null;
}

export function schemaIsEmpty(schema: SchemaNode): boolean {
  return schema.error === null && schema.objects.length === 0;
}
