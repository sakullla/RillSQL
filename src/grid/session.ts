import { commitChanges, countMatches, queryPage } from "./data";
import { deleteSql, insertSql, updateSql } from "./sql";
import { rejectWrite } from "./typeset";
import type { Cell, Change, CommitReport, CommitStep, Filter, Row, SortSpec, TableStore } from "./types";
import { opNeedsValue } from "./types";

export interface InsertDraft {
  id: string;
  cells: Record<string, Cell | undefined>;
}

let draftSeq = 1;

export class TableSession {
  filters: Filter[] = [];
  sort: SortSpec | null = null;
  loaded: Row[] = [];
  truncated = false;
  matched = 0;
  counted: number | null = null;
  error: string | null = null;
  sql = "";
  edits = new Map<string, Record<string, Cell>>();
  inserts: InsertDraft[] = [];
  deletes: string[] = [];
  failures = new Map<string, string>();
  selectedId: string | null = null;
  selectedColumn: string | null = null;
  revealed = new Set<string>();

  constructor(
    public store: TableStore,
    public pageSize = 100,
  ) {
    this.reload();
  }

  get canInsert(): boolean {
    return this.store.schema.kind === "table";
  }

  get scopeLabel(): string {
    return "筛选和排序作用于整张表";
  }

  get keyLabel(): string {
    if (this.store.schema.kind === "view") return "只读视图";
    if (!this.store.schema.key.length) return "这张表没有主键，只能新增";
    return `按 ${this.store.schema.key.join(" + ")} 定位行`;
  }

  get pendingCount(): number {
    return this.edits.size + this.inserts.length + this.deletes.length;
  }

  reload(): void {
    this.readWindow(0, Math.max(this.pageSize, this.loaded.length || this.pageSize), true);
  }

  readWindow(offset: number, limit: number, replace: boolean): string | null {
    const page = queryPage(this.store, { filters: this.filters, sort: this.sort, offset, limit });
    this.sql = page.sql;
    if (!page.ok) {
      this.error = page.error;
      return page.error;
    }
    this.error = null;
    this.loaded = replace ? page.rows : [...this.loaded, ...page.rows];
    this.matched = page.matched;
    this.truncated = page.truncated;
    if (replace) this.counted = null;
    return null;
  }

  replaceFilters(filters: Filter[]): string | null {
    for (const filter of filters) {
      if (opNeedsValue(filter.op) && !filter.value) return "筛选值不能为空";
    }
    this.filters = filters.map((filter) => ({ ...filter }));
    return this.readWindow(0, this.pageSize, true);
  }

  addFilter(filter: Filter): string | null {
    return this.replaceFilters([...this.filters, filter]);
  }

  removeFilter(index: number): void {
    this.replaceFilters(this.filters.filter((_, item) => item !== index));
  }

  cycleSort(column: string): void {
    if (this.sort?.column !== column) this.sort = { column, direction: "asc" };
    else if (this.sort.direction === "asc") this.sort = { column, direction: "desc" };
    else this.sort = null;
    this.readWindow(0, this.pageSize, true);
  }

  clearSort(): void {
    this.sort = null;
    this.readWindow(0, this.pageSize, true);
  }

  loadMore(): string | null {
    if (!this.truncated) return null;
    return this.readWindow(this.loaded.length, this.pageSize, false);
  }

  count(): string | null {
    const result = countMatches(this.store, this.filters);
    if (typeof result !== "number") {
      this.error = result.error;
      return result.error;
    }
    this.counted = result;
    return null;
  }

  select(id: string, column: string | null): void {
    this.selectedId = id;
    this.selectedColumn = column;
  }

  rowById(id: string): Row | undefined {
    return this.loaded.find((row) => row.id === id) ?? this.store.rows.find((row) => row.id === id);
  }

  canUpdate(row: Row | undefined): boolean {
    return this.store.schema.kind === "table" && !!row?.located;
  }

  canDeleteRow(id: string): boolean {
    if (this.inserts.some((insert) => insert.id === id)) return true;
    const row = this.rowById(id);
    return this.canUpdate(row);
  }

  canDeleteSelection(): boolean {
    return this.selectedId !== null && this.canDeleteRow(this.selectedId);
  }

  stageUpdate(rowId: string, column: string, value: Cell): string | null {
    const row = this.rowById(rowId);
    if (!row) return "找不到行";
    const found = this.store.schema.columns.find((item) => item.name === column);
    const rejected = rejectWrite(found, value);
    if (rejected) return rejected;
    if (!this.canUpdate(row)) return "无法唯一确定这一行，不能修改";
    const sets = { ...(this.edits.get(rowId) ?? {}), [column]: value };
    this.edits.set(rowId, sets);
    this.failures.delete(`u:${rowId}`);
    return null;
  }

  addRow(): string | null {
    if (!this.canInsert) return "只有数据表可以新增，视图不行";
    const id = `new-${draftSeq}`;
    draftSeq += 1;
    const cells: Record<string, Cell | undefined> = {};
    for (const column of this.store.schema.columns) cells[column.name] = undefined;
    this.inserts.unshift({ id, cells });
    this.selectedId = id;
    this.selectedColumn = this.store.schema.columns.find((column) => !column.auto)?.name ?? null;
    return null;
  }

  stageInsert(id: string, column: string, value: Cell | undefined): string | null {
    const insert = this.inserts.find((item) => item.id === id);
    if (!insert) return "找不到新增行";
    const found = this.store.schema.columns.find((item) => item.name === column);
    const rejected = rejectWrite(found, value);
    if (rejected) return rejected;
    insert.cells[column] = value;
    this.failures.delete(id);
    return null;
  }

  deleteSelected(): string | null {
    if (!this.selectedId) return "先选择一行";
    return this.deleteRow(this.selectedId);
  }

  deleteRow(id: string): string | null {
    const insertIndex = this.inserts.findIndex((item) => item.id === id);
    if (insertIndex >= 0) {
      this.inserts.splice(insertIndex, 1);
      if (this.selectedId === id) this.selectedId = null;
      return null;
    }
    if (!this.canDeleteRow(id)) return "无法唯一确定这一行，不能删除";
    if (!this.deletes.includes(id)) this.deletes.push(id);
    this.edits.delete(id);
    return null;
  }

  discard(): void {
    this.edits.clear();
    this.inserts = [];
    this.deletes = [];
    this.failures.clear();
  }

  cellValue(id: string, column: string): Cell | undefined {
    const insert = this.inserts.find((item) => item.id === id);
    if (insert) return insert.cells[column];
    const edit = this.edits.get(id);
    if (edit && column in edit) return edit[column];
    return this.rowById(id)?.cells[column];
  }

  preview(): CommitStep[] {
    return this.changes().map((change) => ({
      id: change.id,
      sql: this.sqlFor(change),
      label: change.kind === "insert" ? "新增 1 行" : change.kind === "delete" ? `删除 ${change.rowId}` : `修改 ${change.rowId}`,
    }));
  }

  commit(): CommitReport {
    const changes = this.changes();
    const report = commitChanges(this.store, changes);
    const appliedIds = new Set(report.applied.map((step) => step.id));
    for (const id of appliedIds) {
      if (id.startsWith("u:")) this.edits.delete(id.slice(2));
      if (id.startsWith("d:")) this.deletes = this.deletes.filter((rowId) => `d:${rowId}` !== id);
      this.inserts = this.inserts.filter((insert) => insert.id !== id);
      this.failures.delete(id);
    }
    if (report.failed) this.failures.set(report.failed.id, report.failed.message);
    this.readWindow(0, Math.max(this.pageSize, this.loaded.length || this.pageSize), true);
    return report;
  }

  changes(): Change[] {
    const changes: Change[] = [];
    for (const [rowId, sets] of this.edits) changes.push({ kind: "update", id: `u:${rowId}`, rowId, sets });
    for (const insert of this.inserts) changes.push({ kind: "insert", id: insert.id, values: { ...insert.cells } });
    for (const rowId of this.deletes) changes.push({ kind: "delete", id: `d:${rowId}`, rowId });
    return changes;
  }

  sqlFor(change: Change): string {
    if (change.kind === "insert") return insertSql(this.store.schema, change.values);
    const row = this.store.rows.find((item) => item.id === change.rowId);
    if (!row?.locator) return "";
    if (change.kind === "delete") return deleteSql(this.store.schema, row.locator.columns, row.locator.values);
    return updateSql(this.store.schema, change.sets, row.locator.columns, row.locator.values);
  }
}
