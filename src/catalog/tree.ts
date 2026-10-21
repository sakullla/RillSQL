import type { CatalogConnection, CatalogObject, ConnStatus, Engine, EngineShape, ObjectKind, SchemaNode } from "./types";

export const KIND_LABEL: Record<ObjectKind, string> = {
  table: "表",
  view: "视图",
  function: "函数",
  procedure: "存储过程",
  trigger: "触发器",
  sequence: "序列",
  enum: "枚举",
  domain: "域",
};

export function objectGroups(engine: Engine): ObjectKind[] {
  if (engine === "sqlite" || engine === "mysql") return engine === "sqlite" ? ["table", "view", "trigger"] : ["table", "view", "function", "procedure", "trigger"];
  if (engine === "mariadb") return ["table", "view", "function", "procedure", "trigger", "sequence"];
  return ["table", "view", "function", "procedure", "trigger", "sequence", "enum", "domain"];
}

/** 普通视图不显示索引分组。物化视图只有快照确实返回索引时才显示。表没有索引时仍显示空分组。 */
export function showsIndexGroup(kind: ObjectKind, materialized: boolean, indexCount: number | null): boolean {
  if (kind === "table") return true;
  if (kind === "view" && materialized) return (indexCount ?? 0) > 0;
  return false;
}

export function isDefinitionKind(kind: ObjectKind): boolean {
  return kind === "function" || kind === "procedure" || kind === "trigger" || kind === "sequence" || kind === "enum" || kind === "domain";
}

export function objectKey(object: Pick<CatalogObject, "kind" | "name" | "signature" | "relation">): string {
  return [object.kind, object.name, object.signature ?? "", object.relation ?? ""].join("\u0001");
}

export function objectLabel(engine: Engine, object: Pick<CatalogObject, "name" | "kind" | "signature">): string {
  if (engine === "postgres" && (object.kind === "function" || object.kind === "procedure")) {
    return `${object.name}(${object.signature ?? ""})`;
  }
  return object.name;
}

export function engineShape(engine: Engine): EngineShape {
  if (engine === "postgres") return { databases: true, schemas: true, foreignKeys: false };
  if (engine === "sqlite") return { databases: false, schemas: false, foreignKeys: true };
  return { databases: true, schemas: false, foreignKeys: false };
}

export function isConnected(status: ConnStatus): boolean {
  return status === "connected";
}

/** PostgreSQL 把其他库标成这条说明时，并不是对象列表读失败。 */
export function postgresSessionGap(error: string | null | undefined): { current: string; database: string } | null {
  if (typeof error !== "string") return null;
  const match = error.match(/当前会话连在\s*(.+?)\s*[,，]\s*没有另开连接读取\s*(\S+)/);
  if (!match?.[1] || !match[2]) return null;
  return { current: match[1], database: match[2] };
}

/** 空的 schema 名不占路径一段。MySQL / MariaDB / SQLite 没有独立 schema 层。 */
export function objectPath(connection: string, database: string, schema: string): string {
  return [connection, database, schema].filter((part) => part.length > 0).join(" / ");
}

export function reconcileObjects(previous: CatalogObject[], incoming: CatalogObject[]): CatalogObject[] {
  const nextByKey = new Map(incoming.map((item) => [objectKey(item), item]));
  const seen = new Set<string>();
  const objects: CatalogObject[] = [];
  for (const old of previous) {
    const key = objectKey(old);
    seen.add(key);
    if (old.stale) continue;
    const fresh = nextByKey.get(key);
    if (!fresh) {
      objects.push({ ...old, stale: true });
      continue;
    }
    objects.push({ ...fresh, stale: false });
  }
  for (const item of incoming) {
    if (!seen.has(objectKey(item))) objects.push({ ...item, stale: false });
  }
  return objects;
}

export function refreshSchema(previous: SchemaNode, incoming: SchemaNode): SchemaNode {
  if (incoming.error) {
    return {
      name: incoming.name || previous.name,
      objects: previous.objects.map((item) => ({ ...item })),
      total: Math.max(previous.total, previous.objects.length),
      error: incoming.error,
      kindReports: incoming.kindReports ?? previous.kindReports,
      pending: false,
    };
  }
  const failed = new Set((incoming.kindReports ?? []).filter((item) => item.error).map((item) => item.kind));
  const kept = previous.objects.filter((item) => failed.has(item.kind)).map((item) => ({ ...item }));
  const objects = [
    ...reconcileObjects(
      previous.objects.filter((item) => !failed.has(item.kind)),
      incoming.objects.filter((item) => !failed.has(item.kind)),
    ),
    ...kept,
  ];
  return {
    name: incoming.name,
    objects,
    total: Math.max(incoming.total + kept.length, objects.length),
    error: null,
    kindReports: incoming.kindReports ?? previous.kindReports,
    pending: false,
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
  key: string;
  name: string;
  kind: CatalogObject["kind"];
  path: string;
  env: CatalogConnection["env"];
  envLabel: string;
  envTone: CatalogConnection["envTone"];
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
            id: `${connection.id}:${database.name}:${schema.name}:${objectKey(object)}`,
            key: objectKey(object),
            name: object.name,
            kind: object.kind,
            path: objectPath(connection.name, database.name, schema.name),
            env: connection.env,
            envLabel: connection.envLabel,
            envTone: connection.envTone,
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

function kindFailed(schema: Pick<SchemaNode, "kindReports">, kind: ObjectKind): boolean {
  return schema.kindReports?.some((item) => item.kind === kind && item.error !== null) ?? false;
}

/** 仍在服务端按名称切片里的行。失败类的保留行和 stale 行不占 offset。 */
export function serverSliceCount(schema: Pick<SchemaNode, "objects" | "kindReports">): number {
  return schema.objects.filter((object) => !object.stale && !kindFailed(schema, object.kind)).length;
}

/** 成功类的总数。没有分类报告时，schema.total 就是这个数。 */
export function successfulTotal(schema: Pick<SchemaNode, "total" | "kindReports">): number {
  const reports = schema.kindReports;
  if (!reports || reports.length === 0) return schema.total;
  return reports.reduce((sum, item) => (item.error ? sum : sum + item.total), 0);
}

export function visibleTotal(schema: Pick<SchemaNode, "objects" | "total" | "kindReports">): number {
  return successfulTotal(schema) + schema.objects.length - serverSliceCount(schema);
}

export function schemaFailed(schema: SchemaNode): boolean {
  return schema.error !== null;
}

export function schemaIsEmpty(schema: SchemaNode): boolean {
  return schema.error === null && schema.objects.length === 0;
}
