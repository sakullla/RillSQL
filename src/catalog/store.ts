import { shallowRef, reactive } from "vue";
import { loadSettings } from "../shell/settings";
import { fetchCatalog, fetchPage, fetchSessions, type ObjectLimit } from "./backend";
import { objectPath, refreshConnection } from "./tree";
import { DEFAULT_OBJECT_PAGE, type CatalogConnection, type PageRequest, type PageResult, type PaletteTable, type SchemaNode } from "./types";

const connections = shallowRef<CatalogConnection[]>([]);
const loadError = shallowRef<string | null>(null);
const shown = reactive<Record<string, number>>({});
let pageOverride: number | null = null;
let refreshSource: (() => CatalogConnection[]) | null = null;
let pageSource: ((request: PageRequest) => PageResult) | null = null;
let seenSessions = "";
let pollTimer: ReturnType<typeof setInterval> | null = null;

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

export function catalogLoadError(): string | null {
  return loadError.value;
}

export function refreshLimit(loaded: number, page: number): number {
  const size = page > 0 ? page : DEFAULT_OBJECT_PAGE;
  return Math.max(loaded, size);
}

export function limitsForRefresh(list: CatalogConnection[], page: number): ObjectLimit[] {
  const limits: ObjectLimit[] = [];
  for (const connection of list) {
    for (const database of connection.databases) {
      for (const schema of database.schemas) {
        limits.push({
          connectionId: connection.id,
          database: database.name,
          schema: schema.name,
          limit: refreshLimit(schema.objects.length, page),
        });
      }
    }
  }
  return limits;
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
  loadError.value = null;
  rememberShown(list);
}

export function resetCatalog(): void {
  connections.value = [];
  loadError.value = null;
  refreshSource = null;
  pageSource = null;
  pageOverride = null;
  seenSessions = "";
  if (pollTimer !== null) {
    clearInterval(pollTimer);
    pollTimer = null;
  }
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
            if (schema.objects.length >= schema.total) return schema;
            const request: PageRequest = {
              connectionId,
              database,
              schema: schemaName,
              offset: schema.objects.length,
              limit: page,
            };
            if (pageSource) {
              return applyPage(schema, key, pageSource(request));
            }
            void fetchPage(request)
              .then((result) => {
                patchSchema(connectionId, database, schemaName, (current) => applyPage(current, key, result));
              })
              .catch((error: unknown) => {
                patchSchema(connectionId, database, schemaName, (current) => applyPage(current, key, { objects: [], total: current.total, error: failureText(error) }));
              });
            return schema;
          }),
        };
      }),
    };
  });
}

function applyPage(schema: SchemaNode, key: string, result: PageResult): SchemaNode {
  if (result.error) return { ...schema, error: result.error };
  const objects = [...schema.objects, ...result.objects];
  shown[key] = objects.length;
  return { ...schema, objects, total: result.total, error: null };
}

function patchSchema(connectionId: string, database: string, schemaName: string, update: (schema: SchemaNode) => SchemaNode): void {
  connections.value = connections.value.map((connection) => {
    if (connection.id !== connectionId) return connection;
    return {
      ...connection,
      databases: connection.databases.map((item) => {
        if (item.name !== database) return item;
        return {
          ...item,
          schemas: item.schemas.map((schema) => (schema.name === schemaName ? update(schema) : schema)),
        };
      }),
    };
  });
}

function applySnapshot(incoming: CatalogConnection[]): void {
  loadError.value = null;
  const previous = new Map(connections.value.map((connection) => [connection.id, connection]));
  const merged = incoming.map((connection) => {
    const old = previous.get(connection.id);
    return old ? refreshConnection(old, connection) : connection;
  });
  connections.value = merged;
  rememberShown(merged);
}

function failureText(error: unknown): string {
  if (error instanceof Error && error.message) return error.message;
  if (typeof error === "string" && error) return error;
  return "无法读取对象列表";
}

export async function refreshFromBackend(): Promise<void> {
  const page = objectPageSize();
  const limits = limitsForRefresh(connections.value, page);
  try {
    applySnapshot(await fetchCatalog(limits, page));
  } catch (error) {
    loadError.value = failureText(error);
  }
}

function armPoll(): void {
  if (pollTimer !== null || typeof window === "undefined") return;
  pollTimer = setInterval(() => {
    void pollSessions();
  }, 1000);
}

async function pollSessions(): Promise<void> {
  if (refreshSource) return;
  const ids = await fetchSessions();
  if (ids === null) return;
  const key = ids.join("\n");
  if (key === seenSessions) return;
  seenSessions = key;
  await refreshFromBackend();
}

export async function startCatalog(): Promise<void> {
  if (refreshSource) return;
  const ids = await fetchSessions();
  if (ids === null) {
    if (connections.value.length === 0) loadError.value = "无法读取已打开的连接";
    return;
  }
  await refreshFromBackend();
  seenSessions = ids.join("\n");
  armPoll();
}

export function refreshCatalog(): void {
  if (refreshSource) {
    applySnapshot(refreshSource());
    return;
  }
  void refreshFromBackend();
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
