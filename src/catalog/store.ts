import { shallowRef, reactive } from "vue";
import { loadSettings } from "../shell/settings";
import { objectPath, refreshConnection } from "./tree";
import { DEFAULT_OBJECT_PAGE, type CatalogConnection, type PageRequest, type PageResult, type PaletteTable } from "./types";

const connections = shallowRef<CatalogConnection[]>([]);
const shown = reactive<Record<string, number>>({});
let pageOverride: number | null = null;
let refreshSource: (() => CatalogConnection[]) | null = null;
let pageSource: ((request: PageRequest) => PageResult) | null = null;

export function objectPageSize(): number {
  if (pageOverride && pageOverride > 0) return pageOverride;
  const size = loadSettings().objectPage;
  return Number.isFinite(size) && size > 0 ? size : DEFAULT_OBJECT_PAGE;
}

export function setObjectPage(size: number | null): void {
  pageOverride = size && size > 0 ? size : null;
}

export function setRefreshSource(source: (() => CatalogConnection[]) | null): void {
  refreshSource = source;
}

export function setPageSource(source: ((request: PageRequest) => PageResult) | null): void {
  pageSource = source;
}

export function schemaKey(connectionId: string, database: string, schema: string): string {
  return `s:${connectionId}:${database}:${schema}`;
}

export function catalogConnections(): CatalogConnection[] {
  return connections.value;
}

export function shownCount(key: string, loaded: number): number {
  return shown[key] ?? Math.min(objectPageSize(), loaded);
}

function rememberShown(list: CatalogConnection[]): void {
  const page = objectPageSize();
  for (const connection of list) {
    for (const database of connection.databases) {
      for (const schema of database.schemas) {
        const key = schemaKey(connection.id, database.name, schema.name);
        if (shown[key] === undefined) shown[key] = Math.min(page, schema.objects.length);
      }
    }
  }
}

export function replaceCatalog(list: CatalogConnection[]): void {
  connections.value = list;
  rememberShown(list);
}

export function resetCatalog(): void {
  connections.value = [];
  refreshSource = null;
  pageSource = null;
  pageOverride = null;
  for (const key of Object.keys(shown)) delete shown[key];
}

export function revealMore(connectionId: string, database: string, schemaName: string): void {
  const key = schemaKey(connectionId, database, schemaName);
  const page = objectPageSize();
  connections.value = connections.value.map((connection) => {
    if (connection.id !== connectionId) return connection;
    return {
      ...connection,
      databases: connection.databases.map((item) => {
        if (item.name !== database) return item;
        return {
          ...item,
          schemas: item.schemas.map((schema) => {
            if (schema.name !== schemaName) return schema;
            const current = shown[key] ?? Math.min(page, schema.objects.length);
            if (current < schema.objects.length) {
              shown[key] = Math.min(schema.objects.length, current + page);
              return schema;
            }
            if (schema.objects.length >= schema.total || !pageSource) return schema;
            const result = pageSource({
              connectionId,
              database,
              schema: schemaName,
              offset: schema.objects.length,
              limit: page,
            });
            if (result.error) {
              return { ...schema, error: result.error };
            }
            const objects = [...schema.objects, ...result.objects];
            shown[key] = objects.length;
            return { ...schema, objects, total: result.total, error: null };
          }),
        };
      }),
    };
  });
}

export function refreshCatalog(): void {
  if (!refreshSource) return;
  const incoming = refreshSource();
  const previous = new Map(connections.value.map((connection) => [connection.id, connection]));
  const merged = incoming.map((connection) => {
    const old = previous.get(connection.id);
    return old ? refreshConnection(old, connection) : connection;
  });
  connections.value = merged;
  rememberShown(merged);
}

export function listPaletteTables(): PaletteTable[] {
  const tables: PaletteTable[] = [];
  for (const connection of connections.value) {
    if (connection.status !== "connected") continue;
    for (const database of connection.databases) {
      for (const schema of database.schemas) {
        if (schema.error && schema.objects.length === 0) continue;
        for (const object of schema.objects) {
          if (object.stale) continue;
          tables.push({
            id: `${connection.id}:${database.name}:${schema.name}:${object.name}`,
            label: object.name,
            path: objectPath(connection.name, database.name, schema.name),
          });
        }
      }
    }
  }
  return tables;
}
