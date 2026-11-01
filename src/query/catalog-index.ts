import type { CatalogConnection } from "../catalog/types";
import { objectLabel } from "../catalog/tree";
import type { CompletionObject, CompletionSource } from "./complete";
import type { ScopeTable } from "./completion/scope";

export interface CatalogIndex {
  databases: string[];
  schemas: Map<string, string[]>;
  objects: CompletionObject[];
  byQualified: Map<string, CompletionObject>;
  byName: Map<string, CompletionObject[]>;
}

const CACHE = new WeakMap<CatalogConnection, CatalogIndex>();

/** 目录对象索引：连接对象一变（刷新换新引用）就整体重建，避免按键时重复拼装。 */
export function catalogIndex(connection: CatalogConnection | null): CatalogIndex {
  if (!connection) return emptyIndex();
  const cached = CACHE.get(connection);
  if (cached) return cached;
  const index = emptyIndex();
  for (const database of connection.databases) {
    if (database.name) index.databases.push(database.name);
    const names: string[] = [];
    for (const schema of database.schemas) {
      if (schema.name && !names.includes(schema.name)) names.push(schema.name);
      /* MySQL/SQLite 的目录里 schema 为空串，库名才是限定前缀；统一收敛成"有效限定符"。 */
      const qualifier = schema.name || database.name;
      for (const object of schema.objects) {
        if (object.stale) continue;
        const label = qualifier ? `${qualifier}.${objectLabel(connection.engine, object)}` : objectLabel(connection.engine, object);
        const item: CompletionObject = { label, name: object.name, schema: qualifier, kind: object.kind };
        index.objects.push(item);
        index.byQualified.set(label.toLowerCase(), item);
        const bucket = index.byName.get(object.name.toLowerCase());
        if (bucket) bucket.push(item);
        else index.byName.set(object.name.toLowerCase(), [item]);
      }
    }
    index.schemas.set(database.name, names);
  }
  CACHE.set(connection, index);
  return index;
}

function emptyIndex(): CatalogIndex {
  return { databases: [], schemas: new Map(), objects: [], byQualified: new Map(), byName: new Map() };
}

/** 解析语句里的表引用：先按限定名，再按裸名；裸名有歧义时不猜。 */
export function resolveTable(index: CatalogIndex, table: ScopeTable, preferred: string | null = null): CompletionObject | null {
  const qualified = table.schema ? `${table.schema}.${table.name}`.toLowerCase() : null;
  if (qualified) {
    const exact = index.byQualified.get(qualified);
    if (exact) return exact;
    const bySchema = index.objects.find((object) => object.schema.toLowerCase() === table.schema!.toLowerCase() && object.name.toLowerCase() === table.name.toLowerCase());
    if (bySchema) return bySchema;
  }
  const candidates = index.byName.get(table.name.toLowerCase()) ?? [];
  if (candidates.length === 1) return candidates[0]!;
  if (preferred) {
    const scoped = candidates.filter((object) => object.schema.toLowerCase() === preferred.toLowerCase());
    return scoped.length === 1 ? scoped[0]! : null;
  }
  return null;
}

/** 把 FROM/JOIN 的表转成候选来源；列由调用方按缓存注入，未加载时为空数组。 */
export function sourcesFor(
  index: CatalogIndex,
  tables: readonly ScopeTable[],
  columnsOf: (object: CompletionObject) => string[] = () => [],
  preferred: string | null = null,
): CompletionSource[] {
  const sources: CompletionSource[] = [];
  for (const table of tables) {
    const object = resolveTable(index, table, preferred);
    if (!object) continue;
    sources.push({ alias: table.alias, label: object.label, columns: columnsOf(object) });
  }
  return sources;
}

/** SELECT * 展开：按来源顺序拼接列名。 */
export function expandStar(sources: readonly CompletionSource[]): string[] {
  const seen = new Set<string>();
  const columns: string[] = [];
  for (const source of sources) {
    for (const column of source.columns) {
      const key = column.toLowerCase();
      if (seen.has(key)) continue;
      seen.add(key);
      columns.push(column);
    }
  }
  return columns;
}
