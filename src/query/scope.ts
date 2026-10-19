import { statementAt } from "./split";

export interface SelectionRange {
  from: number;
  to: number;
  head?: number;
}

export function sqlToRun(
  sql: string,
  selection: SelectionRange,
  scope: "auto" | "current" | "script",
  engine = "postgres",
): { sql: string; mode: "selection" | "current" | "script"; from: number; to: number } {
  if (scope === "script") return { sql, mode: "script", from: 0, to: sql.length };
  if (scope === "current" || selection.from === selection.to) {
    const cursor = selection.head ?? selection.to;
    const piece = statementAt(sql, cursor, engine);
    return { sql: piece?.text ?? "", mode: "current", from: piece?.start ?? cursor, to: piece?.end ?? cursor };
  }
  const from = Math.min(selection.from, selection.to);
  const to = Math.max(selection.from, selection.to);
  return { sql: sql.slice(from, to), mode: "selection", from, to };
}

export function rowLimit(pageSize: number): number {
  if (!Number.isFinite(pageSize)) return 100;
  return Math.min(10000, Math.max(1, Math.trunc(pageSize)));
}

export const QUERY_EDITOR_KEYS = ["Mod-Enter", "Mod-Shift-Enter", "Mod-.", "Ctrl-Space", "Mod-/"] as const;
