import { shallowRef, reactive } from "vue";
import { presentEnv, type EnvTag, type SavedConnection } from "../connections/model";
import { loadSettings } from "../shell/settings";
import { fetchCatalog, fetchPage, fetchSessions, listConnections, listEnvTags, type ObjectLimit } from "./backend";
import { objectPath, refreshConnection, refreshSchema } from "./tree";
import { DEFAULT_OBJECT_PAGE, type CatalogConnection, type PageRequest, type PageResult, type PaletteTable, type SchemaNode } from "./types";

const connections = shallowRef<CatalogConnection[]>([]);
const loadError = shallowRef<string | null>(null);
const shown = reactive<Record<string, number>>({});
let pageOverride: number | null = null;
let refreshSource: (() => CatalogConnection[]) | null = null;
let pageSource: ((request: PageRequest) => PageResult) | null = null;
let seenSessions = "";
let pollTimer: ReturnType<typeof setInterval> | null = null;
let refreshGeneration = 0;
const connectionReads = new Map<string, Promise<void>>();

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
  refreshGeneration += 1;
  connectionReads.clear();
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
  if (result.error) return { ...schema, error: result.error, pending: false };
  const objects = [...schema.objects, ...result.objects];
  shown[key] = objects.length;
  return {
    ...schema,
    objects,
    total: result.total,
    error: null,
    kindReports: result.kindReports ?? schema.kindReports,
    pending: false,
  };
}

function applyRefreshPage(schema: SchemaNode, result: PageResult): SchemaNode {
  return refreshSchema(schema, {
    name: schema.name,
    objects: result.objects,
    total: result.total,
    error: result.error,
    kindReports: result.kindReports,
    pending: false,
  });
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
  return "对象列表没读出来，原因未知。";
}

function enqueueConnectionRead(id: string, run: () => Promise<void>): Promise<void> {
  const previous = connectionReads.get(id) ?? Promise.resolve();
  const next = previous.then(run, run);
  connectionReads.set(
    id,
    next.then(
      () => undefined,
      () => undefined,
    ),
  );
  return next;
}

function connectionShell(saved: SavedConnection, connected: boolean, tags: readonly EnvTag[]): CatalogConnection {
  const env = presentEnv(saved.env, tags);
  return {
    id: saved.id,
    name: saved.name,
    engine: saved.engine,
    env: saved.env,
    envLabel: env.label,
    envTone: env.tone,
    status: connected ? "connected" : "disconnected",
    defaultDatabase: saved.database ?? null,
    foreignKeys: null,
    error: null,
    databases: [],
  };
}

function applyShells(shells: CatalogConnection[]): void {
  loadError.value = null;
  const previous = new Map(connections.value.map((connection) => [connection.id, connection]));
  connections.value = shells.map((shell) => {
    const old = previous.get(shell.id);
    if (!old) return shell;
    return {
      ...shell,
      foreignKeys: shell.engine === "sqlite" ? old.foreignKeys : null,
      error: old.error,
      databases: old.databases,
    };
  });
  rememberShown(connections.value);
}

function patchConnection(connectionId: string, update: (connection: CatalogConnection) => CatalogConnection): void {
  connections.value = connections.value.map((connection) => (connection.id === connectionId ? update(connection) : connection));
}

function headerOf(incoming: CatalogConnection): Pick<CatalogConnection, "name" | "engine" | "env" | "envLabel" | "envTone" | "status" | "defaultDatabase"> {
  return {
    name: incoming.name,
    engine: incoming.engine,
    env: incoming.env,
    envLabel: incoming.envLabel,
    envTone: incoming.envTone,
    status: incoming.status,
    defaultDatabase: incoming.defaultDatabase,
  };
}

function applyOutline(incoming: CatalogConnection): void {
  connections.value = connections.value.map((current) => {
    if (current.id !== incoming.id) return current;
    if (incoming.status !== "connected") {
      return { ...current, ...headerOf(incoming), databases: current.databases };
    }
    if (incoming.error && incoming.databases.length === 0) {
      return refreshConnection(current, { ...incoming, databases: [] });
    }
    const previousDatabases = new Map(current.databases.map((database) => [database.name, database]));
    const databases = incoming.databases.map((database) => {
      const oldDatabase = previousDatabases.get(database.name);
      if (database.error) return oldDatabase ? { ...oldDatabase, error: database.error } : database;
      const previousSchemas = new Map((oldDatabase?.schemas ?? []).map((schema) => [schema.name, schema]));
      return {
        ...database,
        error: null,
        schemas: database.schemas.map((schema) => {
          const oldSchema = previousSchemas.get(schema.name);
          if (!oldSchema) return { ...schema, objects: [], total: 0, error: null, pending: true };
          return { ...oldSchema, name: schema.name, error: null, pending: true };
        }),
      };
    });
    return {
      ...current,
      ...headerOf(incoming),
      error: incoming.error,
      foreignKeys: incoming.engine === "sqlite" ? (incoming.foreignKeys ?? current.foreignKeys) : null,
      databases,
    };
  });
  rememberShown(connections.value);
}

function loadedCount(connectionId: string, database: string, schemaName: string): number {
  return (
    connections.value
      .find((item) => item.id === connectionId)
      ?.databases.find((item) => item.name === database)
      ?.schemas.find((item) => item.name === schemaName)?.objects.length ?? 0
  );
}

function pageTargets(connection: CatalogConnection): { database: string; schema: string }[] {
  const targets: { database: string; schema: string }[] = [];
  for (const database of connection.databases) {
    if (database.error) continue;
    for (const schema of database.schemas) {
      if (schema.error) continue;
      targets.push({ database: database.name, schema: schema.name });
    }
  }
  return targets;
}

async function readConnectionTargets(connectionId: string, page: number, generation: number): Promise<void> {
  const current = () => generation === refreshGeneration;
  let outline: CatalogConnection[];
  try {
    outline = await fetchCatalog([], page, connectionId, true);
  } catch (error) {
    if (current()) patchConnection(connectionId, (connection) => ({ ...connection, error: failureText(error) }));
    return;
  }
  if (!current()) return;
  const incoming = outline.find((item) => item.id === connectionId);
  if (!incoming) return;
  applyOutline(incoming);
  if (incoming.status !== "connected" || (incoming.error && incoming.databases.length === 0)) return;
  for (const target of pageTargets(incoming)) {
    if (!current()) return;
    const limit = refreshLimit(loadedCount(connectionId, target.database, target.schema), page);
    try {
      const result = await fetchPage({
        connectionId,
        database: target.database,
        schema: target.schema,
        offset: 0,
        limit,
      });
      if (!current()) return;
      patchSchema(connectionId, target.database, target.schema, (schema) => applyRefreshPage(schema, result));
    } catch (error) {
      if (!current()) return;
      patchSchema(connectionId, target.database, target.schema, (schema) =>
        refreshSchema(schema, { ...schema, objects: [], total: 0, error: failureText(error), pending: false }),
      );
    }
  }
}

export async function refreshFromBackend(): Promise<void> {
  const generation = refreshGeneration;
  const page = objectPageSize();
  let sessions: string[] | null;
  let saved: SavedConnection[];
  let tags: EnvTag[];
  try {
    [sessions, saved, tags] = await Promise.all([fetchSessions(), listConnections(), listEnvTags().catch(() => [] as EnvTag[])]);
  } catch (error) {
    if (generation === refreshGeneration) loadError.value = failureText(error);
    return;
  }
  if (generation !== refreshGeneration) return;
  if (sessions === null) {
    loadError.value = "读不到已打开的连接。到「连接」页确认连接还能打开。";
    return;
  }
  const open = new Set(sessions);
  applyShells(saved.map((item) => connectionShell(item, open.has(item.id), tags)));
  await Promise.all(
    saved
      .filter((item) => open.has(item.id))
      .map((item) => enqueueConnectionRead(item.id, () => readConnectionTargets(item.id, page, generation))),
  );
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
    if (connections.value.length === 0) loadError.value = "读不到已打开的连接。到「连接」页确认连接还能打开。";
    return;
  }
  await refreshFromBackend();
  seenSessions = ids.join("\n");
  armPoll();
}

export function refreshCatalog(): Promise<void> {
  if (refreshSource) {
    applySnapshot(refreshSource());
    return Promise.resolve();
  }
  return refreshFromBackend();
}

export function listPaletteTables(): PaletteTable[] {
  const tables: PaletteTable[] = [];
  for (const connection of connections.value) {
    if (connection.status !== "connected") continue;
    for (const database of connection.databases) {
      for (const schema of database.schemas) {
        if (schema.error && schema.objects.length === 0) continue;
        for (const object of schema.objects) {
          if (object.stale || (object.kind !== "table" && object.kind !== "view")) continue;
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
