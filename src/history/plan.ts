export interface HistoryEntry {
  id: string;
  sql: string;
  connectionId: string;
  connectionName: string;
  databaseName: string;
  engine: string;
  startedAt: string;
  finishedAt: string | null;
  status: string;
  elapsedMs: number | null;
  summary: string;
}

export interface OpenPlan {
  sql: string;
  connectionId: string | null;
  connectionName: string;
  database: string | null;
  engine: string;
  needsTarget: boolean;
  execute: false;
}

export function openPlan(entry: HistoryEntry, knownIds: ReadonlySet<string>): OpenPlan {
  const exists = knownIds.has(entry.connectionId);
  return {
    sql: entry.sql,
    connectionId: exists ? entry.connectionId : null,
    connectionName: entry.connectionName,
    database: exists ? entry.databaseName : null,
    engine: entry.engine,
    needsTarget: !exists,
    execute: false,
  };
}

export function filterHistory(
  entries: HistoryEntry[],
  query: { text: string; connectionId: string; status: string },
  knownIds: ReadonlySet<string>,
): HistoryEntry[] {
  const text = query.text.trim().toLowerCase();
  return entries.filter((entry) => {
    if (text && !entry.sql.toLowerCase().includes(text) && !entry.connectionName.toLowerCase().includes(text)) return false;
    if (query.connectionId === "deleted") {
      if (knownIds.has(entry.connectionId)) return false;
    } else if (query.connectionId !== "all" && entry.connectionId !== query.connectionId) {
      return false;
    }
    if (query.status !== "all" && entry.status !== query.status) return false;
    return true;
  });
}
