import { splitSql, statementAt } from "./split";

export interface PlanSelection {
  from: number;
  to: number;
  head?: number;
}

export type PlanPick = { ok: true; statement: string } | { ok: false; reason: string };
export type ExplainSql = { ok: true; sql: string } | { ok: false; reason: string };

const EMPTY = "没有可查看计划的语句。";
const MULTIPLE = "一次只能查看一条语句的执行计划。";
const ANALYZE = "计划不使用 ANALYZE，没有发送这条语句。";

/** 无选区时用光标所在语句，有选区时用选区。空语句或多条语句不执行。 */
export function planStatement(sql: string, selection: PlanSelection): PlanPick {
  const selected = selection.from !== selection.to;
  const text = selected
    ? sql.slice(Math.min(selection.from, selection.to), Math.max(selection.from, selection.to))
    : statementAt(sql, selection.head ?? selection.to)?.text ?? "";
  const pieces = splitSql(text);
  if (!text.trim() || pieces.length === 0) return { ok: false, reason: EMPTY };
  if (pieces.length > 1) return { ok: false, reason: MULTIPLE };
  return { ok: true, statement: pieces[0]!.text };
}

/** PostgreSQL、MySQL、MariaDB 用 EXPLAIN；SQLite 用 EXPLAIN QUERY PLAN。不加 ANALYZE。 */
export function explainSql(engine: string, statement: string): ExplainSql {
  const body = statement.trim().replace(/;\s*$/, "");
  if (/^\s*explain\s+(analyze\b|\(\s*analyze\b)/i.test(body)) return { ok: false, reason: ANALYZE };
  if (/^\s*explain\b/i.test(body)) {
    if (engine === "sqlite" && !/^\s*explain\s+query\s+plan\b/i.test(body)) {
      return { ok: true, sql: body.replace(/^\s*explain\b/i, "EXPLAIN QUERY PLAN") };
    }
    return { ok: true, sql: body.replace(/^\s*explain\b/i, "EXPLAIN") };
  }
  const prefix = engine === "sqlite" ? "EXPLAIN QUERY PLAN" : "EXPLAIN";
  return { ok: true, sql: `${prefix} ${body}` };
}
