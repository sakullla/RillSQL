import { gridDescribe } from "../grid/api";
import type { Engine, ForeignKey } from "../grid/types";
import type { CompletionObject } from "./complete";

export interface CachedFields {
  connectionId: string;
  database: string;
  schema: string;
  table: string;
  label: string;
  columns: string[];
  error: string | null;
}

const MAX_TABLES = 200;
const cache = new Map<string, CachedFields>();
const inFlight = new Map<string, Promise<CachedFields>>();
const foreignKeys = new Map<string, ForeignKey[]>();
const foreignKeysInFlight = new Map<string, Promise<ForeignKey[]>>();

export function clearColumnCache(): void {
  cache.clear();
  inFlight.clear();
  foreignKeys.clear();
  foreignKeysInFlight.clear();
}

/** 供测试与诊断读取当前缓存规模。 */
export function columnCacheSize(): number {
  return cache.size;
}

/** 结构变更或断开后由调用方按前缀失效；不传参数即清空。 */
export function invalidateColumns(prefix?: string): void {
  if (!prefix) {
    clearColumnCache();
    return;
  }
  for (const entries of [cache, inFlight, foreignKeys, foreignKeysInFlight]) {
    for (const key of entries.keys()) {
      if (key.startsWith(prefix)) entries.delete(key);
    }
  }
}

function remember(key: string, entry: CachedFields): CachedFields {
  cache.delete(key);
  cache.set(key, entry);
  while (cache.size > MAX_TABLES) {
    const oldest = cache.keys().next().value;
    if (oldest === undefined) break;
    cache.delete(oldest);
  }
  return entry;
}

function recall(key: string): CachedFields | undefined {
  const entry = cache.get(key);
  if (!entry) return undefined;
  cache.delete(key);
  cache.set(key, entry);
  return entry;
}

export function columnKey(connectionId: string, database: string, schema: string, table: string): string {
  return [connectionId, database, schema, table].join("\u0000");
}

export type Qualifier =
  | { kind: "plain" }
  | { kind: "unknown"; name: string }
  | { kind: "ambiguous"; name: string }
  | { kind: "object"; object: CompletionObject; columnPrefix: string };

/** 点号后只认已经读到的那一个对象。还在输入对象名时保持原来的对象补全。别名表存语句里 FROM/JOIN 起的别名。 */
export function qualifierFor(
  prefix: string,
  objects: readonly CompletionObject[],
  aliases?: ReadonlyMap<string, CompletionObject>,
): Qualifier {
  const dot = prefix.lastIndexOf(".");
  if (dot < 0) return { kind: "plain" };
  const qualifier = prefix.slice(0, dot);
  const columnPrefix = prefix.slice(dot + 1);
  if (!qualifier) return { kind: "unknown", name: qualifier };
  const wanted = qualifier.toLowerCase();
  const typed = prefix.toLowerCase();
  const continuesObject = objects.some((object) => {
    const label = object.label.toLowerCase();
    const longerLabel = label.startsWith(typed) && label.length > typed.length;
    const longerName = !typed.includes(".") && object.name.toLowerCase().startsWith(typed) && object.name.length > typed.length;
    return longerLabel || longerName;
  });
  if (continuesObject) return { kind: "plain" };
  const byLabel = objects.filter((object) => object.label.toLowerCase() === wanted);
  if (byLabel.length > 1) return { kind: "ambiguous", name: qualifier };
  if (byLabel.length === 1) return { kind: "object", object: byLabel[0]!, columnPrefix };
  if (!wanted.includes(".")) {
    const byName = objects.filter((object) => object.name.toLowerCase() === wanted);
    if (byName.length > 1) return { kind: "ambiguous", name: qualifier };
    if (byName.length === 1) return { kind: "object", object: byName[0]!, columnPrefix };
    const aliased = aliases?.get(wanted);
    if (aliased) return { kind: "object", object: aliased, columnPrefix };
  }
  return { kind: "unknown", name: qualifier };
}

export function cachedColumns(connectionId: string, database: string): { label: string; detail: string }[] {
  return [...iterateCachedColumns(connectionId, database)];
}

/** 候选生成可以达到上限即停止，不必展开所有已缓存表的字段。 */
export function* iterateCachedColumns(connectionId: string, database: string): Iterable<{ label: string; detail: string }> {
  const prefix = `${connectionId}\u0000${database}\u0000`;
  for (const [key, entry] of cache) {
    if (!key.startsWith(prefix) || entry.error) continue;
    for (const column of entry.columns) yield { label: column, detail: entry.label };
  }
}

export function fieldFailureNote(label: string, error: string): string {
  return tr("query.columns.readFailed", { label, error });
}

export function unknownFieldNote(name: string): string {
  const shown = name || tr("query.columns.dotName");
  return tr("query.columns.unknownTable", { name: shown });
}

export function ambiguousFieldNote(name: string): string {
  return tr("query.columns.ambiguousTable", { name });
}

function errorText(err: unknown): string {
  if (typeof err === "string" && err.trim()) return err;
  if (err instanceof Error && err.message.trim()) return err.message;
  return tr("query.columns.failed");
}

interface FieldRequest {
  connectionId: string;
  database: string;
  engine: Engine;
  object: CompletionObject;
  refresh?: boolean;
}

/** 外键独立按需读取，保留精简结果；与字段共享失效边界和 200 表 LRU 上限。 */
export async function loadForeignKeys(input: FieldRequest): Promise<ForeignKey[]> {
  const key = columnKey(input.connectionId, input.database, input.object.schema, input.object.name);
  const running = foreignKeysInFlight.get(key);
  if (running) return running;
  const cached = foreignKeys.get(key);
  if (cached && !input.refresh) {
    foreignKeys.delete(key);
    foreignKeys.set(key, cached);
    return cached;
  }
  const request = gridDescribe({
    connectionId: input.connectionId, database: input.database, engine: input.engine,
    schema: input.object.schema, table: input.object.name,
  }, { includeForeignKeys: true })
    .then((schema) => schema.foreignKeys ?? [], () => [])
    .then((keys) => {
      // 断开/刷新期间返回的旧请求不能把已失效的数据重新写入。
      if (foreignKeysInFlight.get(key) === request) {
        foreignKeysInFlight.delete(key);
        foreignKeys.delete(key);
        foreignKeys.set(key, keys);
        if (foreignKeys.size > MAX_TABLES) foreignKeys.delete(foreignKeys.keys().next().value!);
      }
      return keys;
    });
  foreignKeysInFlight.set(key, request);
  return request;
}

export async function loadFields(input: FieldRequest): Promise<CachedFields> {
  const key = columnKey(input.connectionId, input.database, input.object.schema, input.object.name);
  const running = inFlight.get(key);
  if (running) return running;
  if (!input.refresh) {
    const cached = recall(key);
    if (cached) return cached;
  }
  const base = {
    connectionId: input.connectionId,
    database: input.database,
    schema: input.object.schema,
    table: input.object.name,
    label: input.object.label,
  };
  let request: Promise<CachedFields> | null = null;
  const save = (entry: CachedFields) => inFlight.get(key) === request ? remember(key, entry) : entry;
  request = (async (): Promise<CachedFields> => {
    try {
      const schema = await gridDescribe({
        connectionId: input.connectionId,
        database: input.database,
        schema: input.object.schema,
        table: input.object.name,
        engine: input.engine,
      });
      return save({ ...base, columns: schema.columns.map((column) => column.name), error: null });
    } catch (err) {
      return save({ ...base, columns: [], error: errorText(err) });
    } finally {
      if (request && inFlight.get(key) === request) inFlight.delete(key);
    }
  })();
  inFlight.set(key, request);
  return request;
}

import { tr } from "../i18n";
