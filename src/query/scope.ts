import { statementAt } from "./split";

export interface SelectionRange {
  from: number;
  to: number;
  head?: number;
}

export function sqlToRun(sql: string, selection: SelectionRange, scope: "auto" | "current"): { sql: string; mode: "selection" | "current" } {
  if (scope === "current" || selection.from === selection.to) {
    const cursor = selection.head ?? selection.to;
    return { sql: statementAt(sql, cursor)?.text ?? "", mode: "current" };
  }
  const from = Math.min(selection.from, selection.to);
  const to = Math.max(selection.from, selection.to);
  return { sql: sql.slice(from, to), mode: "selection" };
}

export function rowLimit(pageSize: number): number {
  if (!Number.isFinite(pageSize)) return 100;
  return Math.min(10000, Math.max(1, Math.trunc(pageSize)));
}

export const QUERY_EDITOR_KEYS = ["Mod-Enter", "Mod-Shift-Enter", "Mod-.", "Ctrl-Space", "Mod-/"] as const;
