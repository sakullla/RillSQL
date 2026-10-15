import { gridCommit, type ChangePayload } from "./api";
import { copyRow, refreshLocated } from "./data";
import { deleteSql, splitSql, updateSql } from "./sql";
import type { TableTarget } from "./target";
import type { Cell, Column, CommitReport, CommitStep, Engine, Row, TableSchema } from "./types";

export interface TableHit {
  schema: string;
  name: string;
  kind: "table" | "view";
}

export interface TableRef {
  schema: string | null;
  table: string;
  /** 点号左侧限定名的原文。没有限定名时为 null。库名比较只用这份拼写。 */
  qualifierSource: string | null;
}

/** 选择列表里能证明是基表列的结果列。别名列不在其中。 */
export interface ColumnProof {
  star: boolean;
  baseColumns: string[];
  aliasColumns: string[];
}

export type EditProof = { ok: true; table: TableRef; columns: ColumnProof } | { ok: false; reason: string };
export type ResolvedTable = { ok: true; schema: string; table: string } | { ok: false; reason: string };
export type LocatedResult = { ok: true; columns: Column[]; rows: Row[]; editable: string[] } | { ok: false; reason: string };

type SelectItem =
  | { kind: "star"; qualifier: string | null }
  | { kind: "column"; qualifier: string | null; name: string; output: string; aliased: boolean };

const EMPTY_COLUMNS: ColumnProof = { star: false, baseColumns: [], aliasColumns: [] };

type Token = { kind: "word" | "ident" | "punct" | "string"; text: string; source?: string };

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
      tokens.push({ kind: "word", text: text.toLowerCase(), source: text });
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

/** 未加引号的标识符仍按小写参与关键字和表名折叠，库名比较要留下原文。 */
function sourceName(token: Token | undefined): string | null {
  if (!token) return null;
  if (token.kind === "ident") return token.text;
  if (token.kind === "word") return token.source ?? token.text;
  return null;
}

function sameIdent(left: string, right: string): boolean {
  return left === right || left.toLowerCase() === right.toLowerCase();
}

function qualifierMatches(qualifier: string, table: string, alias: string | null): boolean {
  return sameIdent(qualifier, table) || (alias !== null && sameIdent(qualifier, alias));
}

/** MySQL、MariaDB 的限定名是库，必须与连接库名原文相同。SQLite 的 main 不区分大小写，其它附加库同样按原文比较。 */
function qualifierIsCurrentDatabase(engine: Engine, qualifier: string, database: string): boolean {
  if (engine === "sqlite" && qualifier.toLowerCase() === "main" && (database === "" || database === "main")) return true;
  return database !== "" && qualifier === database;
}

function usesDatabaseQualifier(engine: Engine): boolean {
  return engine === "mysql" || engine === "mariadb" || engine === "sqlite";
}

function readAlias(tokens: Token[], index: number, end: number): { ok: true; aliased: boolean; name: string; next: number } | { ok: false } {
  if (index >= end) return { ok: true, aliased: false, name: "", next: index };
  if (word(tokens[index]) === "AS") {
    const alias = readName(tokens[index + 1]);
    if (!alias || index + 1 >= end) return { ok: false };
    return { ok: true, aliased: true, name: alias, next: index + 2 };
  }
  const alias = readName(tokens[index]);
  if (!alias) return { ok: true, aliased: false, name: "", next: index };
  return { ok: true, aliased: true, name: alias, next: index + 1 };
}

function parseSelectItem(tokens: Token[], index: number, end: number): { item: SelectItem; next: number } | null {
  const token = tokens[index];
  if (!token || index >= end) return null;
  if (token.kind === "punct" && token.text === "*") return { item: { kind: "star", qualifier: null }, next: index + 1 };
  const name = readName(token);
  if (!name) return null;
  if (tokens[index + 1]?.text === ".") {
    const right = tokens[index + 2];
    if (right?.kind === "punct" && right.text === "*") return { item: { kind: "star", qualifier: name }, next: index + 3 };
    const column = readName(right);
    if (!column) return null;
    const alias = readAlias(tokens, index + 3, end);
    if (!alias.ok) return null;
    return {
      item: { kind: "column", qualifier: name, name: column, output: alias.aliased ? alias.name : column, aliased: alias.aliased },
      next: alias.next,
    };
  }
  const alias = readAlias(tokens, index + 1, end);
  if (!alias.ok) return null;
  return {
    item: { kind: "column", qualifier: null, name, output: alias.aliased ? alias.name : name, aliased: alias.aliased },
    next: alias.next,
  };
}

function parseSelectList(tokens: Token[], fromAt: number): SelectItem[] | null {
  let index = 1;
  if (word(tokens[index]) === "ALL") index += 1;
  if (index > fromAt) return null;
  if (index === fromAt) return [];
  const items: SelectItem[] = [];
  while (index < fromAt) {
    const parsed = parseSelectItem(tokens, index, fromAt);
    if (!parsed || parsed.next <= index) return null;
    items.push(parsed.item);
    index = parsed.next;
    if (index < fromAt) {
      if (tokens[index]?.text !== ",") return null;
      index += 1;
      if (index >= fromAt) return null;
    }
  }
  return items;
}

function columnProof(items: readonly SelectItem[], table: string, alias: string | null): ColumnProof {
  const bases: string[] = [];
  const aliases: string[] = [];
  let star = false;
  for (const item of items) {
    if (item.kind === "star") {
      if (item.qualifier === null || qualifierMatches(item.qualifier, table, alias)) star = true;
      continue;
    }
    if (item.aliased) {
      aliases.push(item.output);
      continue;
    }
    if (item.qualifier !== null && !qualifierMatches(item.qualifier, table, alias)) continue;
    bases.push(item.name);
  }
  const aliasFold = new Set(aliases.map((name) => name.toLowerCase()));
  return {
    star,
    baseColumns: bases.filter((name) => !aliasFold.has(name.toLowerCase())),
    aliasColumns: aliases,
  };
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
  if (word(tokens[fromAt + 1]) === "ONLY") return { ok: false, reason: "不能确定这条 SELECT 只读取一张基表。" };
  const left = readName(tokens[fromAt + 1]);
  if (!left) return { ok: false, reason: "不能确定这条 SELECT 只读取一张基表。" };
  let cursor = fromAt + 2;
  let table: TableRef = { schema: null, table: left, qualifierSource: null };
  if (tokens[cursor]?.text === ".") {
    const right = readName(tokens[cursor + 1]);
    if (!right) return { ok: false, reason: "不能确定这条 SELECT 只读取一张基表。" };
    cursor += 2;
    if (tokens[cursor]?.text === ".") return { ok: false, reason: "限定名超过两部分，不能证明只是一张基表。" };
    table = { schema: left, table: right, qualifierSource: sourceName(tokens[fromAt + 1]) };
  }
  let alias: string | null = null;
  if (word(tokens[cursor]) === "AS") {
    const name = readName(tokens[cursor + 1]);
    if (!name) return { ok: false, reason: "不能确定这条 SELECT 只读取一张基表。" };
    alias = name;
    cursor += 2;
  } else if ((tokens[cursor]?.kind === "word" || tokens[cursor]?.kind === "ident") && !CLAUSES.has(word(tokens[cursor])) && tokens[cursor]?.text !== ".") {
    alias = readName(tokens[cursor]);
    cursor += 1;
  }
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
  const items = parseSelectList(tokens, fromAt);
  return { ok: true, table, columns: items ? columnProof(items, table.table, alias) : EMPTY_COLUMNS };
}

function nameMatches(tableName: string, hits: readonly TableHit[], schema: string | null): TableHit[] {
  return hits.filter((hit) => sameIdent(hit.name, tableName) && (schema === null || sameIdent(hit.schema, schema)));
}

function uniqueTable(matches: readonly TableHit[], unqualified: boolean): ResolvedTable {
  if (matches.length === 0) {
    return unqualified
      ? { ok: false, reason: "SQL 没有写出模式，还不能证明只对应一张基表。" }
      : { ok: false, reason: "不能唯一确定这一张基表。" };
  }
  const schemas = new Set(matches.map((hit) => hit.schema));
  if (schemas.size > 1) return { ok: false, reason: "同名表出现在多个模式，SQL 没有写出模式，不能唯一确定行。" };
  if (matches.length > 1) return { ok: false, reason: "同一模式里有只靠大小写区分的表，不能唯一确定行。" };
  return { ok: true, schema: matches[0]!.schema, table: matches[0]!.name };
}

/** 限定名对不上当前库、同名表跨模式，或同一模式里只靠大小写区分时，保持只读。 */
export function resolveEditableTable(table: TableRef, hits: readonly TableHit[], database: string, engine: Engine = "postgres"): ResolvedTable {
  if (table.schema && usesDatabaseQualifier(engine)) {
    const qualifier = table.qualifierSource ?? table.schema;
    if (!qualifierIsCurrentDatabase(engine, qualifier, database)) {
      return { ok: false, reason: "限定名不能证明是当前数据库中的对象，结果保持只读。" };
    }
    const matches = nameMatches(table.table, hits, null);
    if (matches.length === 0) return { ok: false, reason: "限定名是当前数据库，但还不能证明只对应一张基表。" };
    return uniqueTable(matches, true);
  }
  if (table.schema) {
    const scoped = nameMatches(table.table, hits, table.schema);
    if (scoped.length > 1) return { ok: false, reason: "同一模式里有只靠大小写区分的表，不能唯一确定行。" };
    if (scoped.length === 1) return { ok: true, schema: scoped[0]!.schema, table: scoped[0]!.name };
    return { ok: true, schema: table.schema, table: table.table };
  }
  return uniqueTable(nameMatches(table.table, hits, null), true);
}

/** 只有证明是基表列、且没有同名别名的结果列可以改。 */
export function provedEditableColumns(schema: TableSchema, columns: readonly Column[], proof: ColumnProof | null): string[] {
  const resultNames = columns.map((column) => column.name);
  if (!proof) return resultNames.filter((name) => schema.columns.some((item) => item.name === name));
  const aliases = new Set(proof.aliasColumns.map((name) => name.toLowerCase()));
  const editable: string[] = [];
  const add = (name: string) => {
    if (aliases.has(name.toLowerCase()) || !resultNames.includes(name) || editable.includes(name)) return;
    editable.push(name);
  };
  if (proof.star) {
    for (const name of resultNames) {
      if (schema.columns.some((item) => item.name === name)) add(name);
    }
  }
  for (const sqlName of proof.baseColumns) {
    if (aliases.has(sqlName.toLowerCase())) continue;
    const matches = schema.columns.filter((item) => item.name === sqlName || item.name.toLowerCase() === sqlName.toLowerCase());
    if (matches.length !== 1) continue;
    add(matches[0]!.name);
  }
  return editable;
}

export function locateResultRows(schema: TableSchema, columns: readonly Column[], rows: readonly Row[], proof: ColumnProof | null = null): LocatedResult {
  if (schema.kind !== "table") return { ok: false, reason: "视图是只读的，不能修改或删除数据。" };
  if (!schema.key.length) return { ok: false, reason: "这张表没有主键，无法唯一确定已有行，所以不能修改或删除。" };
  const editable = provedEditableColumns(schema, columns, proof);
  const names = proof ? new Set(editable) : new Set(columns.map((column) => column.name));
  const missing = schema.key.filter((name) => !names.has(name));
  if (missing.length) {
    const visible = missing.filter((name) => columns.some((column) => column.name === name));
    if (proof && visible.length) return { ok: false, reason: `主键列 ${visible.join("、")} 不能证明就是基表列，无法唯一确定已有行。` };
    return { ok: false, reason: `主键列 ${missing.join("、")} 不在结果中，无法唯一确定已有行。` };
  }
  const merged = columns.map((column) => schema.columns.find((item) => item.name === column.name) ?? column);
  const next = rows.map(copyRow);
  refreshLocated(schema, next);
  if (next.length > 0 && next.every((row) => !row.located)) return { ok: false, reason: "无法唯一确定已有行，不能修改或删除。" };
  return { ok: true, columns: merged, rows: next, editable };
}

export class ResultEditSession {
  edits = new Map<string, Record<string, Cell>>();
  deletes: string[] = [];
  failures = new Map<string, string>();
  rows: Row[];
  selectedId: string | null = null;
  selectedColumn: string | null = null;
  readonly editableColumns: ReadonlySet<string>;

  constructor(
    public schema: TableSchema,
    public target: TableTarget,
    rows: readonly Row[],
    editableColumns?: readonly string[],
  ) {
    this.rows = rows.map(copyRow);
    this.editableColumns = new Set(editableColumns ?? schema.columns.map((column) => column.name));
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

  canEditColumn(column: string): boolean {
    return this.editableColumns.has(column);
  }

  stageUpdate(rowId: string, column: string, value: Cell): string | null {
    const row = this.rowById(rowId);
    if (!row) return "找不到行";
    if (!this.canEditColumn(column)) return "这一列不是基表列，不能修改";
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
