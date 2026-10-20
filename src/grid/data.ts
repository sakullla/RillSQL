import { browseSql, compileWhere, deleteSql, insertSql, isNumericType, updateSql } from "./sql";
import {
  COMMIT_RULE,
  type Cell,
  type Change,
  type Column,
  type CommitReport,
  type CommitStep,
  type Filter,
  type Locator,
  type PageOutcome,
  type PageRequest,
  type Row,
  type SortSpec,
  type TableSchema,
  type TableStore,
} from "./types";

export function columnOf(schema: TableSchema, name: string): Column | undefined {
  return schema.columns.find((column) => column.name === name);
}

export function compareValues(column: Column | undefined, left: string, right: string): number {
  if (column && isNumericType(column.typeName) && /^-?\d+(\.\d+)?$/.test(left) && /^-?\d+(\.\d+)?$/.test(right)) {
    return Number(left) - Number(right);
  }
  return left.localeCompare(right, "zh", { numeric: true });
}

export function sameLocator(left: Locator | null, right: Locator | null): boolean {
  if (!left || !right) return false;
  if (left.columns.length !== right.columns.length || left.values.length !== right.values.length) return false;
  return left.columns.every((name, index) => name === right.columns[index] && left.values[index] === right.values[index]);
}

export function locatorFrom(schema: TableSchema, cells: Record<string, Cell | undefined>): Locator | null {
  if (schema.kind !== "table" || schema.key.length === 0) return null;
  const values: Cell[] = [];
  for (const name of schema.key) {
    const value = cells[name];
    if (value === undefined || value === null) return null;
    values.push(value);
  }
  return { columns: [...schema.key], values };
}

export function refreshLocated(schema: TableSchema, rows: Row[]): void {
  const locators = rows.map((row) => locatorFrom(schema, row.cells));
  rows.forEach((row, index) => {
    const locator = locators[index] ?? null;
    if (!locator) {
      row.locator = null;
      row.located = false;
      return;
    }
    const copies = locators.filter((item) => sameLocator(item, locator)).length;
    row.locator = copies === 1 ? locator : null;
    row.located = copies === 1;
  });
}

export function copyRow(row: Row): Row {
  return {
    id: row.id,
    cells: { ...row.cells },
    locator: row.locator ? { columns: [...row.locator.columns], values: [...row.locator.values] } : null,
    located: row.located,
    ...(row.resolved ? { resolved: true } : {}),
  };
}

export function matchesFilter(column: Column, value: Cell, filter: Filter): boolean {
  if (filter.op === "null") return value === null;
  if (filter.op === "notnull") return value !== null;
  if (filter.op === "empty") return value === "";
  if (value === null) return false;
  const raw = filter.value ?? "";
  if (filter.op === "contains") return value.toLowerCase().includes(raw.toLowerCase());
  if (filter.op === "starts") return value.toLowerCase().startsWith(raw.toLowerCase());
  const order = compareValues(column, value, raw);
  if (filter.op === "eq") return order === 0;
  if (filter.op === "ne") return order !== 0;
  if (filter.op === "gt") return order > 0;
  return order < 0;
}

export function compareCells(column: Column | undefined, left: Cell, right: Cell): number {
  if (left === null && right === null) return 0;
  if (left === null) return 1;
  if (right === null) return -1;
  return compareValues(column, left, right);
}

/** 筛选和排序先作用于整表，再按 offset/limit 截一批。 */
export function queryPage(store: TableStore, request: PageRequest): PageOutcome {
  const where = compileWhere(store.schema, request.filters);
  const sql = where.ok ? browseSql(store.schema, request.filters, request.sort, request.limit, request.offset) : "";
  if (store.readError) return { ok: false, error: store.readError, sql, emptyMatch: false };
  if (!where.ok) return { ok: false, error: where.error, sql, emptyMatch: false };
  if (request.sort && !columnOf(store.schema, request.sort.column)) {
    return { ok: false, error: `未知排序列 ${request.sort.column}`, sql: "", emptyMatch: false };
  }
  let rows = store.rows.filter((row) =>
    request.filters.every((filter) => {
      const column = columnOf(store.schema, filter.column);
      return column ? matchesFilter(column, row.cells[filter.column] ?? null, filter) : false;
    }),
  );
  if (request.sort) {
    const sort = request.sort;
    const column = columnOf(store.schema, sort.column);
    rows = [...rows].sort((left, right) => compareCells(column, left.cells[sort.column] ?? null, right.cells[sort.column] ?? null) * (sort.direction === "desc" ? -1 : 1));
  }
  const slice = rows.slice(request.offset, request.offset + request.limit).map(copyRow);
  return {
    ok: true,
    rows: slice,
    matched: rows.length,
    truncated: request.offset + slice.length < rows.length,
    sql,
  };
}

export function countMatches(store: TableStore, filters: readonly Filter[]): number | { error: string } {
  const page = queryPage(store, { filters: [...filters], sort: null, offset: 0, limit: 1 });
  if (!page.ok) return { error: page.error };
  return page.matched;
}

function mergedCells(row: Row, sets: Record<string, Cell>): Record<string, Cell> {
  return { ...row.cells, ...sets };
}

function uniqueConflict(schema: TableSchema, rows: readonly Row[], cells: Record<string, Cell | undefined>, ignoreId: string | null): string | null {
  const uniqueColumns = schema.columns.filter((column) => column.unique || column.primary);
  for (const column of uniqueColumns) {
    const value = cells[column.name];
    if (value === undefined || value === null) continue;
    const hit = rows.find((row) => row.id !== ignoreId && (row.cells[column.name] ?? null) === value);
    if (hit) return `duplicate key value violates unique constraint "${schema.name}_${column.name}_key"`;
  }
  if (schema.key.length > 1) {
    const tuple = schema.key.map((name) => cells[name]);
    if (tuple.every((value) => value !== undefined && value !== null)) {
      const hit = rows.find((row) => row.id !== ignoreId && schema.key.every((name, index) => (row.cells[name] ?? null) === tuple[index]));
      if (hit) return `duplicate key value violates unique constraint "${schema.name}_pkey"`;
    }
  }
  return null;
}

function nextAuto(rows: readonly Row[], column: Column): string {
  const max = rows.reduce((peak, row) => {
    const value = row.cells[column.name];
    if (value === null || value === undefined || !/^-?\d+$/.test(value)) return peak;
    return Math.max(peak, Number(value));
  }, 0);
  return String(max + 1);
}

function fail(steps: CommitStep[], changeId: string, sql: string, message: string, pendingIds: string[]): CommitReport {
  return {
    applied: steps,
    failed: { id: changeId, sql, label: changeId, message },
    pendingIds,
    rolledBack: false,
    rule: COMMIT_RULE,
  };
}

export function commitChanges(store: TableStore, changes: readonly Change[]): CommitReport {
  const applied: CommitStep[] = [];
  for (let index = 0; index < changes.length; index += 1) {
    const change = changes[index]!;
    const pendingIds = changes.slice(index).map((item) => item.id);
    if (store.schema.kind !== "table") {
      return fail(applied, change.id, "", "只有数据表可以新增、修改或删除，视图不行", pendingIds);
    }
    if (change.kind === "update") {
      const row = store.rows.find((item) => item.id === change.rowId);
      if (!row?.located || !row.locator) return fail(applied, change.id, "", "无法唯一确定这一行，不能修改", pendingIds);
      const hits = store.rows.filter((item) => sameLocator(item.locator, row.locator));
      const sql = updateSql(store.schema, change.sets, row.locator.columns, row.locator.values);
      if (hits.length !== 1) return fail(applied, change.id, sql, `影响 ${hits.length} 行，已停止。已成功的语句不撤销。`, pendingIds);
      const next = mergedCells(row, change.sets);
      const conflict = uniqueConflict(store.schema, store.rows, next, row.id);
      if (conflict) return fail(applied, change.id, sql, conflict, pendingIds);
      row.cells = next;
      refreshLocated(store.schema, store.rows);
      applied.push({ id: change.id, sql, label: `修改 ${row.id}` });
      continue;
    }
    if (change.kind === "delete") {
      const row = store.rows.find((item) => item.id === change.rowId);
      if (!row?.located || !row.locator) return fail(applied, change.id, "", "无法唯一确定这一行，不能删除", pendingIds);
      const hits = store.rows.filter((item) => sameLocator(item.locator, row.locator));
      const sql = deleteSql(store.schema, row.locator.columns, row.locator.values);
      if (hits.length !== 1) return fail(applied, change.id, sql, `影响 ${hits.length} 行，已停止。已成功的语句不撤销。`, pendingIds);
      store.rows = store.rows.filter((item) => item.id !== row.id);
      refreshLocated(store.schema, store.rows);
      applied.push({ id: change.id, sql, label: `删除 ${row.id}` });
      continue;
    }
    const draft: Record<string, Cell | undefined> = { ...change.values };
    for (const column of store.schema.columns) {
      if (draft[column.name] === undefined && column.auto) draft[column.name] = nextAuto(store.rows, column);
    }
    for (const column of store.schema.columns) {
      if (!column.nullable && !column.auto && draft[column.name] === undefined) {
        return fail(applied, change.id, insertSql(store.schema, draft), `新增行缺少必填字段 ${column.name}`, pendingIds);
      }
    }
    const conflict = uniqueConflict(store.schema, store.rows, draft, null);
    const sql = insertSql(store.schema, draft);
    if (conflict) return fail(applied, change.id, sql, conflict, pendingIds);
    const cells: Record<string, Cell> = {};
    for (const column of store.schema.columns) {
      const value = draft[column.name];
      cells[column.name] = value === undefined ? null : value;
    }
    const row: Row = { id: change.id, cells, locator: null, located: false };
    store.rows.push(row);
    refreshLocated(store.schema, store.rows);
    applied.push({ id: change.id, sql, label: "新增 1 行" });
  }
  return { applied, failed: null, pendingIds: [], rolledBack: false, rule: COMMIT_RULE };
}

/** 查询结果的查找和排序只接收已经读入的行，不会回表。 */
export function viewLoadedRows(rows: readonly Row[], columns: readonly Column[], find: string, sort: SortSpec | null): { rows: Row[]; note: string } {
  const needle = find.trim().toLowerCase();
  let next = rows.map(copyRow);
  if (needle) {
    next = next.filter((row) =>
      columns.some((column) => {
        const value = row.cells[column.name];
        return value !== null && value !== undefined && value.toLowerCase().includes(needle);
      }),
    );
  }
  if (sort) {
    const column = columns.find((item) => item.name === sort.column);
    next.sort((left, right) => compareCells(column, left.cells[sort.column] ?? null, right.cells[sort.column] ?? null) * (sort.direction === "desc" ? -1 : 1));
  }
  return { rows: next, note: `仅作用于已读取的 ${rows.length} 行` };
}
