import { gridDescribe } from "../grid/api";
import type { Engine } from "../grid/types";
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

const cache = new Map<string, CachedFields>();

export function clearColumnCache(): void {
  cache.clear();
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
  const prefix = `${connectionId}\u0000${database}\u0000`;
  const items: { label: string; detail: string }[] = [];
  for (const [key, entry] of cache) {
    if (!key.startsWith(prefix) || entry.error) continue;
    for (const column of entry.columns) items.push({ label: column, detail: entry.label });
  }
  return items;
}

export function fieldFailureNote(label: string, error: string): string {
  return `读取 ${label} 的字段失败（${error}）：字段不可用，不会猜测字段名。`;
}

export function unknownFieldNote(name: string): string {
  const shown = name || "点号前的名字";
  return `“${shown}”不是当前目标已读取的表或视图：字段不可用，不会猜测字段名。`;
}

export function ambiguousFieldNote(name: string): string {
  return `“${name}”匹配到多张已读取的表或视图：请写成“模式.名称.”后再补全字段。不会猜测字段名。`;
}

function errorText(err: unknown): string {
  if (typeof err === "string" && err.trim()) return err;
  if (err instanceof Error && err.message.trim()) return err.message;
  return "读取字段失败";
}

export async function loadFields(input: {
  connectionId: string;
  database: string;
  engine: Engine;
  object: CompletionObject;
  refresh?: boolean;
}): Promise<CachedFields> {
  const key = columnKey(input.connectionId, input.database, input.object.schema, input.object.name);
  const existing = cache.get(key);
  if (existing && !input.refresh) return existing;
  const base = {
    connectionId: input.connectionId,
    database: input.database,
    schema: input.object.schema,
    table: input.object.name,
    label: input.object.label,
  };
  try {
    const schema = await gridDescribe({
      connectionId: input.connectionId,
      database: input.database,
      schema: input.object.schema,
      table: input.object.name,
      engine: input.engine,
    });
    const entry: CachedFields = { ...base, columns: schema.columns.map((column) => column.name), error: null };
    cache.set(key, entry);
    return entry;
  } catch (err) {
    const entry: CachedFields = { ...base, columns: [], error: errorText(err) };
    cache.set(key, entry);
    return entry;
  }
}
