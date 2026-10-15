import { sqlWordAt } from "./format";
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

function dollarEnd(sql: string, start: number): number | null {
  const rest = sql.slice(start + 1);
  const end = rest.indexOf("$");
  if (end < 0) return null;
  const tag = rest.slice(0, end);
  if (!/^[A-Za-z0-9_]*$/.test(tag)) return null;
  const marker = `$${tag}$`;
  const close = sql.indexOf(marker, start + marker.length);
  return close < 0 ? sql.length : close + marker.length;
}

function quotedEnd(sql: string, start: number): number {
  const quote = sql[start] ?? "";
  let j = start + 1;
  while (j < sql.length) {
    if (sql[j] === quote && sql[j + 1] === quote) {
      j += 2;
      continue;
    }
    if (sql[j] === quote) return j + 1;
    j += 1;
  }
  return sql.length;
}

/** 去掉字符串、引号标识符、注释和 dollar-quote 后，语句里仍有 ANALYZE 或 ANALYSE。 */
function statementHasAnalyze(sql: string): boolean {
  let i = 0;
  while (i < sql.length) {
    const ch = sql[i] ?? "";
    if (/\s/.test(ch)) {
      i += 1;
      continue;
    }
    if (sql.startsWith("--", i)) {
      i += 2;
      while (i < sql.length && sql[i] !== "\n" && sql[i] !== "\r") i += 1;
      continue;
    }
    if (sql.startsWith("/*", i)) {
      const end = sql.indexOf("*/", i + 2);
      i = end < 0 ? sql.length : end + 2;
      continue;
    }
    if (ch === "'" || ch === '"' || ch === "`") {
      i = quotedEnd(sql, i);
      continue;
    }
    if (ch === "$") {
      const end = dollarEnd(sql, i);
      if (end !== null) {
        i = end;
        continue;
      }
    }
    const word = sqlWordAt(sql, i);
    if (word) {
      const upper = word.text.toUpperCase();
      if (upper === "ANALYZE" || upper === "ANALYSE") return true;
      i = word.end;
      continue;
    }
    i += 1;
  }
  return false;
}

/** PostgreSQL、MySQL、MariaDB 用 EXPLAIN；SQLite 用 EXPLAIN QUERY PLAN。不加 ANALYZE。 */
export function explainSql(engine: string, statement: string): ExplainSql {
  const body = statement.trim().replace(/;\s*$/, "");
  if (statementHasAnalyze(body)) return { ok: false, reason: ANALYZE };
  if (/^\s*explain\b/i.test(body)) {
    if (engine === "sqlite" && !/^\s*explain\s+query\s+plan\b/i.test(body)) {
      return { ok: true, sql: body.replace(/^\s*explain\b/i, "EXPLAIN QUERY PLAN") };
    }
    return { ok: true, sql: body.replace(/^\s*explain\b/i, "EXPLAIN") };
  }
  const prefix = engine === "sqlite" ? "EXPLAIN QUERY PLAN" : "EXPLAIN";
  return { ok: true, sql: `${prefix} ${body}` };
}
