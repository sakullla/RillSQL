import { gridCommit, type ChangePayload } from "./api";
import { copyRow, refreshLocated } from "./data";
import { deleteSql, splitSql, updateSql } from "./sql";
import type { TableTarget } from "./target";
import type { Cell, Column, CommitReport, CommitStep, Row, TableSchema } from "./types";

export interface TableHit {
  schema: string;
  name: string;
  kind: "table" | "view";
}

export interface TableRef {
  schema: string | null;
  table: string;
}

export type EditProof = { ok: true; table: TableRef } | { ok: false; reason: string };
export type ResolvedTable = { ok: true; schema: string; table: string } | { ok: false; reason: string };
export type LocatedResult = { ok: true; columns: Column[]; rows: Row[] } | { ok: false; reason: string };

type Token = { kind: "word" | "ident" | "punct" | "string"; text: string };

const CLAUSES = new Set(["WHERE", "ORDER", "LIMIT", "OFFSET", "FETCH", "FOR", "BY", "AND", "OR", "AS", "ASC", "DESC", "NULLS", "FIRST", "LAST", "ONLY", "ROWS", "ROW", "NEXT", "TIES", "SHARE", "NOWAIT", "SKIP", "LOCKED", "OF", "INTO"]);
const FORBIDDEN: Record<string, string> = {
  JOIN: "结果包含 JOIN，不能唯一确定一行。",
  UNION: "结果包含 UNION，不能唯一确定一行。",
  INTERSECT: "结果包含 INTERSECT，不能唯一确定一行。",
  EXCEPT: "结果包含 EXCEPT，不能唯一确定一行。",
  DISTINCT: "结果包含 DISTINCT，不能唯一确定一行。",
  GROUP: "结果包含 GROUP，不能唯一确定一行。",
  HAVING: "结果包含 HAVING，不能唯一确定一行。",
  WINDOW: "结果包含 WINDOW，不能唯一确定一行。",
  INTO: "这条 SELECT 会写入数据，不能修改查询结果。",
};
const AGGREGATES = new Set(["COUNT", "SUM", "AVG", "MIN", "MAX", "GROUP_CONCAT", "STRING_AGG", "ARRAY_AGG", "JSON_AGG", "TOTAL", "EVERY", "BOOL_AND", "BOOL_OR"]);

function isIdentChar(char: string): boolean {
  return /[A-Za-z0-9_\u0080-\uFFFF]/.test(char);
}

function tokenize(sql: string): Token[] {
  const tokens: Token[] = [];
  for (let index = 0; index < sql.length; index += 1) {
    const char = sql[index]!;
    if (/\s/.test(char)) continue;
    if (char === "'" || char === '"') {
      const quote = char;
      let text = "";
      index += 1;
      while (index < sql.length) {
        const next = sql[index]!;
        if (next === quote && sql[index + 1] === quote) {
          text += quote;
          index += 2;
          continue;
        }
        if (next === quote) break;
        text += next;
        index += 1;
      }
      tokens.push(quote === "'" ? { kind: "string", text } : { kind: "ident", text });
      continue;
    }
    if (char === "`") {
      let text = "";
      index += 1;
      while (index < sql.length) {
        const next = sql[index]!;
        if (next === "`" && sql[index + 1] === "`") {
          text += "`";
          index += 2;
          continue;
        }
        if (next === "`") break;
        text += next;
        index += 1;
      }
      tokens.push({ kind: "ident", text });
      continue;
    }
    if (isIdentChar(char)) {
      let text = char;
      while (index + 1 < sql.length && isIdentChar(sql[index + 1]!)) {
        index += 1;
        text += sql[index]!;
      }
      tokens.push({ kind: "word", text: text.toLowerCase() });
      continue;
    }
    tokens.push({ kind: "punct", text: char });
  }
  return tokens;
}

function word(token: Token | undefined): string {
  return token?.kind === "word" ? token.text.toUpperCase() : "";
}

function readName(token: Token | undefined): string | null {
  if (!token) return null;
  if (token.kind === "ident") return token.text;
  if (token.kind === "word") return token.text;
  return null;
}

/** 只接受一条没有联接、聚合或子查询的 SELECT，并读出唯一的基表名。 */
export function readSingleTable(sql: string): EditProof {
  const statements = splitSql(sql).map((item) => item.trim()).filter(Boolean);
  if (!statements.length) return { ok: false, reason: "没有可修改的语句。" };
  if (statements.length > 1) return { ok: false, reason: "不是单条 SELECT，不能修改查询结果。" };
  const tokens = tokenize(statements[0]!);
  if (word(tokens[0]) !== "SELECT") return { ok: false, reason: "只有单条 SELECT 可以修改查询结果。" };
  for (let index = 0; index < tokens.length; index += 1) {
    const token = tokens[index]!;
    const upper = word(token);
    if (upper && FORBIDDEN[upper]) return { ok: false, reason: FORBIDDEN[upper]! };
    if (upper && AGGREGATES.has(upper) && tokens[index + 1]?.text === "(") return { ok: false, reason: "结果包含聚合，不能唯一确定一行。" };
    if (token.kind === "punct" && token.text === "(") return { ok: false, reason: "结果包含子查询或函数，不能证明只是一张基表。" };
  }
  const fromAt = tokens.findIndex((token) => word(token) === "FROM");
  if (fromAt < 0 || tokens.findIndex((token, index) => index > fromAt && word(token) === "FROM") >= 0) {
    return { ok: false, reason: "不能确定这条 SELECT 只读取一张基表。" };
  }
  const left = readName(tokens[fromAt + 1]);
  if (!left) return { ok: false, reason: "不能确定这条 SELECT 只读取一张基表。" };
  let cursor = fromAt + 2;
  let table: TableRef = { schema: null, table: left };
  if (tokens[cursor]?.text === ".") {
    const right = readName(tokens[cursor + 1]);
    if (!right) return { ok: false, reason: "不能确定这条 SELECT 只读取一张基表。" };
    table = { schema: left, table: right };
    cursor += 2;
  }
  if (word(tokens[cursor]) === "AS") cursor += 2;
  else if (tokens[cursor]?.kind === "word" && !CLAUSES.has(word(tokens[cursor])) && tokens[cursor]?.text !== ".") cursor += 1;
  let clause = "FROM";
  for (let index = cursor; index < tokens.length; index += 1) {
    const token = tokens[index]!;
    const upper = word(token);
    if (token.kind === "punct" && token.text === ",") {
      if (clause !== "ORDER") return { ok: false, reason: "结果包含多张表，不能唯一确定一行。" };
      continue;
    }
    if (upper === "ORDER") clause = "ORDER";
    else if (upper && CLAUSES.has(upper) && upper !== "BY") clause = upper;
  }
  return { ok: true, table };
}

/** 没有写出模式时，同名表落在多个模式就保持只读。 */
export function resolveEditableTable(table: TableRef, hits: readonly TableHit[], database: string): ResolvedTable {
  if (table.schema) {
    const schema = table.schema === database ? "" : table.schema;
    return { ok: true, schema, table: table.table };
  }
  const matches = hits.filter((hit) => hit.name === table.table || hit.name.toLowerCase() === table.table.toLowerCase());
  const schemas = new Set(matches.map((hit) => hit.schema));
  if (schemas.size > 1) return { ok: false, reason: "同名表出现在多个模式，SQL 没有写出模式，不能唯一确定行。" };
  if (matches.length === 0) return { ok: false, reason: "SQL 没有写出模式，还不能证明只对应一张基表。" };
  return { ok: true, schema: matches[0]!.schema, table: matches[0]!.name };
}

export function locateResultRows(schema: TableSchema, columns: readonly Column[], rows: readonly Row[]): LocatedResult {
  if (schema.kind !== "table") return { ok: false, reason: "视图是只读的，不能修改或删除数据。" };
  if (!schema.key.length) return { ok: false, reason: "这张表没有主键，无法唯一确定已有行，所以不能修改或删除。" };
  const names = new Set(columns.map((column) => column.name));
  const missing = schema.key.filter((name) => !names.has(name));
  if (missing.length) return { ok: false, reason: `主键列 ${missing.join("、")} 不在结果中，无法唯一确定已有行。` };
  const merged = columns.map((column) => schema.columns.find((item) => item.name === column.name) ?? column);
  const next = rows.map(copyRow);
  refreshLocated(schema, next);
  if (next.length > 0 && next.every((row) => !row.located)) return { ok: false, reason: "无法唯一确定已有行，不能修改或删除。" };
  return { ok: true, columns: merged, rows: next };
}

export class ResultEditSession {
  edits = new Map<string, Record<string, Cell>>();
  deletes: string[] = [];
  failures = new Map<string, string>();
  rows: Row[];
  selectedId: string | null = null;
  selectedColumn: string | null = null;

  constructor(
    public schema: TableSchema,
    public target: TableTarget,
    rows: readonly Row[],
  ) {
    this.rows = rows.map(copyRow);
  }

  get canInsert(): boolean {
    return false;
  }

  get pendingCount(): number {
    return this.edits.size + this.deletes.length;
  }

  addRow(): string {
    return "查询结果不能新增行。";
  }

  select(id: string, column: string | null): void {
    this.selectedId = id;
    this.selectedColumn = column;
  }

  rowById(id: string): Row | undefined {
    return this.rows.find((row) => row.id === id);
  }

  canUpdate(row: Row | undefined): boolean {
    return this.schema.kind === "table" && !!row?.located && !this.deletes.includes(row.id);
  }

  canDeleteRow(id: string): boolean {
    return this.canUpdate(this.rowById(id));
  }

  stageUpdate(rowId: string, column: string, value: Cell): string | null {
    const row = this.rowById(rowId);
    if (!row) return "找不到行";
    if (!this.schema.columns.some((item) => item.name === column)) return "这一列不属于基表，不能修改";
    if (!this.canUpdate(row)) return "无法唯一确定这一行，不能修改";
    this.edits.set(rowId, { ...(this.edits.get(rowId) ?? {}), [column]: value });
    this.failures.delete(`u:${rowId}`);
    return null;
  }

  deleteRow(id: string): string | null {
    if (!this.canDeleteRow(id)) return "无法唯一确定这一行，不能删除";
    if (!this.deletes.includes(id)) this.deletes.push(id);
    this.edits.delete(id);
    this.failures.delete(`u:${id}`);
    return null;
  }

  discard(): void {
    this.edits.clear();
    this.deletes = [];
    this.failures.clear();
  }

  cellValue(id: string, column: string): Cell | undefined {
    const edit = this.edits.get(id);
    if (edit && column in edit) return edit[column];
    return this.rowById(id)?.cells[column];
  }

  /** 导出只读已提交的单元格，暂存修改留在界面上。 */
  committedCells(): Record<string, Cell>[] {
    return this.rows.map((row) => ({ ...row.cells }));
  }

  preview(): CommitStep[] {
    return this.changes().map((change) => ({
      id: change.id,
      sql: this.sqlFor(change),
      label: change.kind === "delete" ? `删除 ${change.rowId}` : `修改 ${change.rowId}`,
    }));
  }

  async commit(): Promise<CommitReport | null> {
    const changes = this.changes();
    if (!changes.length) return null;
    try {
      const report = await gridCommit(this.target, this.payload(changes));
      const appliedIds = new Set(report.applied.map((step) => step.id));
      for (const id of appliedIds) {
        if (id.startsWith("u:")) {
          const rowId = id.slice(2);
          const sets = this.edits.get(rowId);
          const row = this.rowById(rowId);
          if (row && sets) row.cells = { ...row.cells, ...sets };
          this.edits.delete(rowId);
        }
        if (id.startsWith("d:")) {
          const rowId = id.slice(2);
          this.deletes = this.deletes.filter((item) => item !== rowId);
          this.rows = this.rows.filter((row) => row.id !== rowId);
        }
        this.failures.delete(id);
      }
      refreshLocated(this.schema, this.rows);
      if (report.failed) this.failures.set(report.failed.id, report.failed.message);
      return { ...report, rolledBack: false };
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      this.failures.set(changes[0]!.id, message);
      return null;
    }
  }

  changes(): Array<{ kind: "update"; id: string; rowId: string; sets: Record<string, Cell> } | { kind: "delete"; id: string; rowId: string }> {
    const changes: Array<{ kind: "update"; id: string; rowId: string; sets: Record<string, Cell> } | { kind: "delete"; id: string; rowId: string }> = [];
    for (const [rowId, sets] of this.edits) changes.push({ kind: "update", id: `u:${rowId}`, rowId, sets });
    for (const rowId of this.deletes) changes.push({ kind: "delete", id: `d:${rowId}`, rowId });
    return changes;
  }

  private payload(changes: ReturnType<ResultEditSession["changes"]>): ChangePayload[] {
    return changes.map((change) => {
      const row = this.rowById(change.rowId);
      const locator = (row?.locator?.columns ?? []).map((column, index) => ({
        column,
        value: row?.locator?.values[index] ?? null,
      }));
      if (change.kind === "delete") return { id: change.id, kind: "delete", sets: [], locator, values: [] };
      return {
        id: change.id,
        kind: "update",
        sets: Object.entries(change.sets).map(([column, value]) => ({ column, value })),
        locator,
        values: [],
      };
    });
  }

  private sqlFor(change: { kind: "update" | "delete"; rowId: string; sets?: Record<string, Cell> }): string {
    const row = this.rowById(change.rowId);
    if (!row?.locator) return "";
    if (change.kind === "delete") return deleteSql(this.schema, row.locator.columns, row.locator.values);
    return updateSql(this.schema, change.sets ?? {}, row.locator.columns, row.locator.values);
  }
}
