import type { OpenPlan } from "./plan";

export interface SavedQuery {
  id: string;
  name: string;
  sql: string;
  connectionId: string;
  connectionName: string;
  databaseName: string;
  engine: string;
  updatedAt: string;
}

export interface SavedDraft {
  sql: string;
  connectionId: string;
  connectionName: string;
  databaseName: string;
  engine: string;
}

export interface SavedWrite {
  query: SavedQuery;
  created: boolean;
}

export function savedName(name: string): string {
  return name.trim();
}

export function savedNameError(name: string): string | null {
  const trimmed = savedName(name);
  if (!trimmed) return "请填写名称";
  if ([...trimmed].length > 200) return "名称不能超过 200 个字";
  return null;
}

export function savedNeedsTarget(entry: Pick<SavedQuery, "connectionId">, knownIds: ReadonlySet<string>): boolean {
  return entry.connectionId === "" || !knownIds.has(entry.connectionId);
}

/** 打开保存的查询只产生草稿。调用方不能据此执行，也不能把它写进执行历史。 */
export function openSaved(entry: SavedQuery, knownIds: ReadonlySet<string>): OpenPlan {
  const needsTarget = savedNeedsTarget(entry, knownIds);
  return {
    sql: entry.sql,
    connectionId: needsTarget ? null : entry.connectionId,
    connectionName: entry.connectionName,
    database: needsTarget ? null : entry.databaseName,
    engine: entry.engine,
    needsTarget,
    execute: false,
  };
}

export function filterSaved(entries: readonly SavedQuery[], text: string): SavedQuery[] {
  const query = text.trim().toLowerCase();
  if (!query) return [...entries];
  return entries.filter((entry) => entry.name.toLowerCase().includes(query));
}
