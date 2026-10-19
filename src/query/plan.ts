import { codeHasAnalyze, splitSql, statementAt } from "./split";

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
const SQLITE_ANALYZE = "SQLite 不支持实际执行并分析，没有发送会修改数据的语句。";

/** 无选区时用光标所在语句，有选区时用选区。空语句或多条语句不执行。 */
export function planStatement(sql: string, selection: PlanSelection, engine = "postgres"): PlanPick {
  const selected = selection.from !== selection.to;
  const text = selected
    ? sql.slice(Math.min(selection.from, selection.to), Math.max(selection.from, selection.to))
    : statementAt(sql, selection.head ?? selection.to, engine)?.text ?? "";
  const pieces = splitSql(text, engine);
  if (!text.trim() || pieces.length === 0) return { ok: false, reason: EMPTY };
  if (pieces.length > 1) return { ok: false, reason: MULTIPLE };
  return { ok: true, statement: pieces[0]!.text };
}

/** 语句扫描只走 splitSql。注释里的 ANALYZE / ANALYSE 不算。 */
function statementHasAnalyze(sql: string, engine: string): boolean {
  const pieces = splitSql(sql, engine);
  if (codeHasAnalyze(sql, engine)) return true;
  return pieces.some((piece) => codeHasAnalyze(piece.text, engine));
}

/** PostgreSQL、MySQL、MariaDB 用 EXPLAIN；SQLite 用 EXPLAIN QUERY PLAN。不加 ANALYZE。 */
export function explainSql(engine: string, statement: string): ExplainSql {
  const body = statement.trim().replace(/;\s*$/, "");
  if (statementHasAnalyze(body, engine)) return { ok: false, reason: ANALYZE };
  if (/^\s*explain\b/i.test(body)) {
    if (engine === "sqlite" && !/^\s*explain\s+query\s+plan\b/i.test(body)) {
      return { ok: true, sql: body.replace(/^\s*explain\b/i, "EXPLAIN QUERY PLAN") };
    }
    return { ok: true, sql: body.replace(/^\s*explain\b/i, "EXPLAIN") };
  }
  const prefix = engine === "sqlite" ? "EXPLAIN QUERY PLAN" : "EXPLAIN";
  return { ok: true, sql: `${prefix} ${body}` };
}

/** MySQL / MariaDB 先要层次格式。被拒绝时再退回 explainSql，不执行原语句。 */
export function hierarchicalSql(engine: string, statement: string): ExplainSql {
  const plain = explainSql(engine, statement);
  if (!plain.ok) return plain;
  if (engine !== "mysql" && engine !== "mariadb") return plain;
  const inner = plain.sql.replace(/^\s*EXPLAIN\s+/i, "");
  if (/^\s*FORMAT\s*=/i.test(inner)) return plain;
  const prefix = engine === "mariadb" ? "EXPLAIN FORMAT=JSON" : "EXPLAIN FORMAT=TREE";
  return { ok: true, sql: `${prefix} ${inner}` };
}

/** 确认后才会调用。SQLite 不生成会执行的语句。 */
export function analyzeSql(engine: string, statement: string): ExplainSql {
  const body = statement.trim().replace(/;\s*$/, "");
  if (!body) return { ok: false, reason: EMPTY };
  if (engine === "sqlite") return { ok: false, reason: SQLITE_ANALYZE };
  const inner = body.replace(/^\s*explain(?:\s+query\s+plan|\s+analyze|\s+analyse)?\s+/i, "");
  if (engine === "mariadb") return { ok: true, sql: `ANALYZE ${inner}` };
  return { ok: true, sql: `EXPLAIN ANALYZE ${inner}` };
}

export interface PlanNode {
  depth: number;
  operation: string;
  rows: string | null;
  extra: string;
}

export type PlanPresentation =
  | { kind: "tree"; nodes: PlanNode[] }
  | { kind: "table" };

/** 单列缩进文本按层次展开；多列只当表格。 */
export function presentPlan(columns: string[], rows: (string | null)[][]): PlanPresentation {
  if (columns.length !== 1) return { kind: "table" };
  const lines = rows.flatMap((row) => (row[0] ?? "").split(/\r?\n/));
  const nodes: PlanNode[] = [];
  for (const line of lines) {
    if (!line.trim()) continue;
    nodes.push(parsePlanLine(line));
  }
  return { kind: "tree", nodes };
}

function parsePlanLine(line: string): PlanNode {
  const indent = /^[ \t]*/.exec(line)?.[0] ?? "";
  const depth = Math.floor(indent.replace(/\t/g, "  ").length / 2);
  let text = line.trim().replace(/^->\s*/, "");
  let extra = "";
  const paren = /\(([^()]*)\)\s*$/.exec(text);
  if (paren) {
    extra = paren[1] ?? "";
    text = text.slice(0, paren.index).trim();
  }
  const rows = /rows\s*[=:]\s*([0-9]+(?:\.[0-9]+)?)/i.exec(extra || line) ?? /([0-9]+(?:\.[0-9]+)?)\s*rows?\b/i.exec(text);
  return { depth, operation: text || line.trim(), rows: rows?.[1] ?? null, extra };
}
