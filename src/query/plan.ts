import { codeHasAnalyze, inspectSql, locateStatement, splitSql } from "./split";

export interface PlanSelection {
  from: number;
  to: number;
  head?: number;
}

export type PlanPick = { ok: true; statement: string; from: number; to: number } | { ok: false; reason: string };
export type ExplainSql = { ok: true; sql: string } | { ok: false; reason: string };

const EMPTY = "没有可查看计划的语句。";
const MULTIPLE = "一次只能查看一条语句的执行计划。";
const ANALYZE = "计划不使用 ANALYZE，没有发送这条语句。";
const SQLITE_ANALYZE = "SQLite 不支持实际执行并分析，没有发送会修改数据的语句。";

/** 无选区时用光标所在语句，有选区时用选区。空语句、多条或未闭合时不执行。 */
export function planStatement(sql: string, selection: PlanSelection, engine = "postgres"): PlanPick {
  const from = Math.min(selection.from, selection.to);
  const to = Math.max(selection.from, selection.to);
  if (from === to) {
    const located = locateStatement(sql, selection.head ?? to, engine);
    if (located.refusal) return { ok: false, reason: located.refusal };
    if (!located.piece) {
      const document = inspectSql(sql, engine);
      if (document.refusal) return { ok: false, reason: document.refusal };
      if (document.notices.length > 0 && document.pieces.length === 0) return { ok: false, reason: document.notices.join("") };
    }
    const piece = located.piece;
    return onePlan(piece?.text ?? "", engine, piece?.start ?? from, piece?.end ?? to);
  }
  return onePlan(sql.slice(from, to), engine, from, to);
}

function onePlan(text: string, engine: string, from: number, to: number): PlanPick {
  const report = inspectSql(text, engine);
  if (report.refusal) return { ok: false, reason: report.refusal };
  if (report.pieces.length === 0) {
    if (report.notices.length) return { ok: false, reason: report.notices.join("") };
    return { ok: false, reason: EMPTY };
  }
  if (report.pieces.length > 1) return { ok: false, reason: MULTIPLE };
  return { ok: true, statement: report.pieces[0]!.text, from, to };
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

/** 单列缩进文本或 MariaDB 的 JSON 计划按层次展开；认不出时只当表格。 */
export function presentPlan(columns: string[], rows: (string | null)[][]): PlanPresentation {
  if (columns.length !== 1) return { kind: "table" };
  const text = rows.map((row) => row[0] ?? "").join("\n").trim();
  const json = parseJsonPlan(text);
  if (json) return { kind: "tree", nodes: json };
  if (!isIndentedPlan(text)) return { kind: "table" };
  const nodes: PlanNode[] = [];
  for (const line of text.split(/\r?\n/)) {
    if (!line.trim()) continue;
    nodes.push(parsePlanLine(line));
  }
  return nodes.length ? { kind: "tree", nodes } : { kind: "table" };
}

function isIndentedPlan(text: string): boolean {
  return text.split(/\r?\n/).some((line) => {
    const trimmed = line.trim();
    if (!trimmed) return false;
    return /^\s*->/.test(line)
      || /\brows\s*[=:]/i.test(line)
      || /\bcost\s*=/i.test(line)
      || /^(?:Seq Scan|Index(?: Only)? Scan|Bitmap|Nested Loop|Hash|Sort|Limit|Aggregate|Gather|Materialize|Result|Group|WindowAgg|CTE|Subquery|SCAN\b|SEARCH\b)/.test(trimmed);
  });
}

function parseJsonPlan(text: string): PlanNode[] | null {
  const start = text.indexOf("{");
  if (start < 0) return null;
  let value: unknown;
  try {
    value = JSON.parse(text.slice(start));
  } catch {
    return null;
  }
  if (!containsQueryBlock(value)) return null;
  const nodes: PlanNode[] = [];
  walkPlan(value, 0, nodes);
  return nodes.length ? nodes : null;
}

function containsQueryBlock(value: unknown): boolean {
  if (Array.isArray(value)) return value.some((item) => containsQueryBlock(item));
  if (!isRecord(value)) return false;
  if ("query_block" in value) return true;
  return Object.values(value).some((item) => containsQueryBlock(item));
}

const PLAN_SKIP = new Set(["select_id", "cost_info", "possible_keys", "ref", "used_columns", "used_key_parts", "attached_condition"]);
const ROW_KEYS = ["rows", "r_rows", "rows_examined", "rows_produced_per_join"];

function walkPlan(value: unknown, depth: number, nodes: PlanNode[]): void {
  if (Array.isArray(value)) {
    for (const item of value) walkPlan(item, depth, nodes);
    return;
  }
  if (!isRecord(value)) return;
  const table = value.table;
  if (isRecord(table)) {
    nodes.push(tableNode(table, depth));
    for (const child of Object.values(table)) {
      if (child && typeof child === "object") walkPlan(child, depth + 1, nodes);
    }
  }
  for (const [key, child] of Object.entries(value)) {
    if (key === "query_block") {
      walkPlan(child, depth, nodes);
      continue;
    }
    if (key === "table" || PLAN_SKIP.has(key)) continue;
    if (!Array.isArray(child) && !isRecord(child)) continue;
    nodes.push({
      depth,
      operation: key.replaceAll("_", " "),
      rows: isRecord(child) ? firstScalar(child, ROW_KEYS) : null,
      extra: scalarExtra(child),
    });
    walkPlan(child, depth + 1, nodes);
  }
}

function tableNode(table: Record<string, unknown>, depth: number): PlanNode {
  const access = scalar(table.access_type);
  const name = scalar(table.table_name);
  const skip = new Set(["access_type", "table_name", ...ROW_KEYS]);
  const extra = Object.entries(table)
    .filter(([key, value]) => !skip.has(key) && value != null && typeof value !== "object")
    .map(([key, value]) => `${key}=${String(value)}`)
    .join(" ");
  return {
    depth,
    operation: [access, name].filter(Boolean).join(" ") || "table",
    rows: firstScalar(table, ROW_KEYS),
    extra,
  };
}

function scalarExtra(value: unknown): string {
  if (!isRecord(value)) return "";
  const skip = new Set(ROW_KEYS);
  return Object.entries(value)
    .filter(([key, item]) => !skip.has(key) && item != null && typeof item !== "object")
    .map(([key, item]) => `${key}=${String(item)}`)
    .join(" ");
}

function firstScalar(record: Record<string, unknown>, keys: string[]): string | null {
  for (const key of keys) {
    const value = record[key];
    if (typeof value === "number" || (typeof value === "string" && value.length > 0)) return String(value);
  }
  return null;
}

function scalar(value: unknown): string {
  return typeof value === "string" || typeof value === "number" ? String(value) : "";
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value != null && typeof value === "object" && !Array.isArray(value);
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
