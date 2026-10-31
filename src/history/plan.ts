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

/** 原连接已不在目录中时，编辑器要写出这句话，并且这次打开不执行。 */
export function deletedConnectionNotice(): string {
  return tr("history.plan.deletedNotice");
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

/** 原始状态值翻译成直白中文；颜色不作唯一信息来源，文字始终在。 */
export function statusLabel(status: string): string {
  if (status === "ok") return tr("history.status.ok");
  if (status === "error") return tr("history.status.error");
  if (status === "cancelled") return tr("history.status.cancelled");
  if (status === "running") return tr("history.status.running");
  return tr("history.status.unknown");
}

export function statusBadge(status: string): string {
  if (status === "ok") return "ok";
  if (status === "error") return "err";
  if (status === "cancelled") return "warn";
  if (status === "running") return "info";
  return "";
}

export interface HistoryDayGroup {
  key: string;
  label: string;
  entries: HistoryEntry[];
}

function dayKey(date: Date): string {
  return `${date.getFullYear()}-${date.getMonth()}-${date.getDate()}`;
}

/** 按开始时间的自然日分组（保持原有顺序，新的在前）；今天/昨天用直白叫法。 */
export function groupHistoryByDay(entries: readonly HistoryEntry[], now: Date = new Date()): HistoryDayGroup[] {
  const yesterday = new Date(now);
  yesterday.setDate(now.getDate() - 1);
  const todayKey = dayKey(now);
  const yesterdayKey = dayKey(yesterday);
  const groups: HistoryDayGroup[] = [];
  for (const entry of entries) {
    const started = new Date(entry.startedAt);
    if (Number.isNaN(started.getTime())) {
      const fallback = groups.find((group) => group.key === "unknown") ?? groups[groups.push({ key: "unknown", label: tr("history.plan.groupUnknown"), entries: [] }) - 1]!;
      fallback.entries.push(entry);
      continue;
    }
    const key = dayKey(started);
    const label = key === todayKey ? tr("history.plan.today") : key === yesterdayKey ? tr("history.plan.yesterday") : tr("history.plan.dateLabel", { y: started.getFullYear(), m: started.getMonth() + 1, d: started.getDate() });
    const group = groups.find((item) => item.key === key) ?? groups[groups.push({ key, label, entries: [] }) - 1]!;
    group.entries.push(entry);
  }
  return groups;
}

export function timeOfDay(iso: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return "";
  return `${String(date.getHours()).padStart(2, "0")}:${String(date.getMinutes()).padStart(2, "0")}`;
}

export function elapsedLabel(ms: number | null): string {
  if (ms == null) return "";
  if (ms < 1000) return tr("history.plan.ms", { n: ms });
  return tr("history.plan.seconds", { n: (ms / 1000).toFixed(1) });
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

import { tr } from "../i18n";