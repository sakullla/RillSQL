import { shallowRef, reactive } from "vue";
import { tr } from "../i18n";
import { engineMeta, presentEnv, type EnvTag, type SavedConnection } from "../connections/model";
import { loadSettings } from "../shell/settings";
import { fetchCatalog, fetchPage, fetchSessions, listConnections, listEnvTags, type ObjectLimit, type SessionStatusInfo } from "./backend";
import { describeStructure } from "../structure/api";
import { objectPath, refreshConnection, refreshSchema, serverSliceCount, successfulTotal } from "./tree";
import { DEFAULT_OBJECT_PAGE, type CatalogConnection, type Engine, type PageRequest, type PageResult, type PaletteTable, type SchemaNode } from "./types";

export interface ObjectOutline {
  status: "loading" | "ready" | "error";
  error: string;
  columns: string[];
  indexes: string[];
  materialized: boolean;
}

export interface OutlineTarget {
  connectionId: string;
  database: string;
  schema: string;
  name: string;
  engine: Engine;
  key: string;
}

export interface StructureFocus {
  connectionId: string;
  database: string;
  schema: string;
  table: string;
  section: "columns" | "indexes";
  name: string;
}

const connections = shallowRef<CatalogConnection[]>([]);
const loadError = shallowRef<string | null>(null);
/** 目录是否已经成功读过一次：恢复的标签据此判断“连接已删”还是“还在加载”。 */
const loaded = shallowRef(false);
const outlines = shallowRef<Record<string, ObjectOutline>>({});
export const structureFocus = shallowRef<StructureFocus | null>(null);
let outlineLoader: ((target: OutlineTarget) => Promise<ObjectOutline>) | null = null;
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

export function catalogLoaded(): boolean {
  return loaded.value;
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
        // 占位中的 schema 还没读到对象：此时记 0 会把随后到达的对象全部藏起来。
        if (schema.pending) continue;
        const key = schemaKey(connection.id, database.name, schema.name);
        if (shown[key] === undefined) shown[key] = Math.min(page, schema.objects.length);
      }
    }
  }
}

export function replaceCatalog(list: CatalogConnection[]): void {
  connections.value = list;
  loadError.value = null;
  loaded.value = true;
  rememberShown(list);
}

export function resetCatalog(): void {
  refreshGeneration += 1;
  connectionReads.clear();
  connections.value = [];
  loadError.value = null;
  loaded.value = false;
  refreshSource = null;
  pageSource = null;
  pageOverride = null;
  seenSessions = "";
  if (pollTimer !== null) {
    clearInterval(pollTimer);
    pollTimer = null;
  }
  for (const key of Object.keys(shown)) delete shown[key];
  outlines.value = {};
  structureFocus.value = null;
  outlineLoader = null;
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
            if (serverSliceCount(schema) >= successfulTotal(schema)) return schema;
            const request: PageRequest = {
              connectionId,
              database,
              schema: schemaName,
              offset: serverSliceCount(schema),
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
  loaded.value = true;
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
  return tr("catalog.store.loadFailedUnknown");
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

function connectionShell(saved: SavedConnection, status: CatalogConnection["status"], reason: string | null, tags: readonly EnvTag[]): CatalogConnection {
  const env = presentEnv(saved.env, tags);
  return {
    id: saved.id,
    name: saved.name,
    engine: saved.engine,
    env: saved.env,
    envLabel: env.label,
    envTone: env.tone,
    status,
    defaultDatabase: saved.database ?? null,
    foreignKeys: null,
    // lost 状态下没有目录读取，error 字段用来带出断开原因（重连失败的分阶段原因）。
    error: status === "lost" ? reason : null,
    databases: [],
  };
}

function applyShells(shells: CatalogConnection[]): void {
  loadError.value = null;
  loaded.value = true;
  const previous = new Map(connections.value.map((connection) => [connection.id, connection]));
  connections.value = shells.map((shell) => {
    const old = previous.get(shell.id);
    if (!old) return shell;
    return {
      ...shell,
      foreignKeys: shell.engine === "sqlite" ? old.foreignKeys : null,
      // lost 的原因随本次快照更新；其余状态沿用已读到的目录错误。
      error: shell.status === "lost" ? shell.error : old.error,
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

/** 轮询键包含状态与原因：断线/重连/原因变化都会触发一次目录刷新。 */
function sessionsKey(sessions: readonly SessionStatusInfo[]): string {
  return sessions.map((item) => `${item.id}:${item.status}:${item.reason ?? ""}`).join("\n");
}

function sessionStatus(info: SessionStatusInfo | undefined): CatalogConnection["status"] {
  if (!info) return "disconnected";
  if (info.status === "reconnecting") return "connecting";
  return info.status;
}

export async function refreshFromBackend(): Promise<void> {
  const generation = refreshGeneration;
  const page = objectPageSize();
  let sessions: SessionStatusInfo[] | null;
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
    loadError.value = tr("catalog.store.cannotReadOpen");
    return;
  }
  const statusById = new Map(sessions.map((item) => [item.id, item]));
  applyShells(saved.map((item) => connectionShell(item, sessionStatus(statusById.get(item.id)), statusById.get(item.id)?.reason ?? null, tags)));
  await Promise.all(
    saved
      .filter((item) => statusById.get(item.id)?.status === "connected")
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
  const sessions = await fetchSessions();
  if (sessions === null) return;
  const key = sessionsKey(sessions);
  if (key === seenSessions) return;
  seenSessions = key;
  await refreshFromBackend();
}

export async function startCatalog(): Promise<void> {
  if (refreshSource) return;
  const sessions = await fetchSessions();
  if (sessions === null) {
    if (connections.value.length === 0) loadError.value = tr("catalog.store.cannotReadOpen");
    return;
  }
  seenSessions = sessionsKey(sessions);
  await refreshFromBackend();
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
    // 能力门控（ADR-2）：kv/document 引擎没有表网格，“打开表”快捷面板只列关系引擎。
    if (!engineMeta(connection.engine).relational) continue;
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

export function outlineStorageKey(target: Pick<OutlineTarget, "connectionId" | "database" | "schema" | "key">): string {
  return [target.connectionId, target.database, target.schema, target.key].join("\u0001");
}

export function objectOutlines(): Record<string, ObjectOutline> {
  return outlines.value;
}

export function setOutlineLoader(loader: ((target: OutlineTarget) => Promise<ObjectOutline>) | null): void {
  outlineLoader = loader;
}

export function setStructureFocus(focus: StructureFocus | null): void {
  structureFocus.value = focus;
}

function outlineFailure(error: unknown): string {
  if (typeof error === "string" && error) return error;
  if (error instanceof Error && error.message) return error.message;
  return tr("catalog.store.membersFailed");
}

async function defaultOutline(target: OutlineTarget): Promise<ObjectOutline> {
  const description = await describeStructure({
    connectionId: target.connectionId,
    database: target.database,
    schema: target.schema,
    table: target.name,
    engine: target.engine,
  });
  return {
    status: "ready",
    error: "",
    columns: description.columns.map((column) => column.name),
    indexes: description.indexes.map((index) => index.name),
    materialized: description.materialized === true,
  };
}

export function loadObjectOutline(target: OutlineTarget): void {
  const key = outlineStorageKey(target);
  const current = outlines.value[key];
  if (current?.status === "loading" || current?.status === "ready") return;
  outlines.value = {
    ...outlines.value,
    [key]: { status: "loading", error: "", columns: [], indexes: [], materialized: false },
  };
  void (outlineLoader ?? defaultOutline)(target)
    .then((outline) => {
      outlines.value = { ...outlines.value, [key]: { ...outline, status: outline.status === "error" ? "error" : "ready" } };
    })
    .catch((error: unknown) => {
      outlines.value = {
        ...outlines.value,
        [key]: { status: "error", error: outlineFailure(error), columns: [], indexes: [], materialized: false },
      };
    });
}

export function retryObjectOutline(target: OutlineTarget): void {
  const key = outlineStorageKey(target);
  const next = { ...outlines.value };
  delete next[key];
  outlines.value = next;
  loadObjectOutline(target);
}
