import { gridCommit, gridCount, gridDescribe, gridPage, type ChangePayload } from "./api";
import { deleteSql, insertSql, updateSql } from "./sql";
import type { TableTarget } from "./target";
import type { Cell, Change, CommitReport, CommitStep, Filter, Row, SortSpec, TableStore } from "./types";
import { opNeedsValue } from "./types";
import { needsFullRead } from "./display";
import { isPreviewEnvelope, rejectWrite } from "./typeset";

export interface InsertDraft {
  id: string;
  cells: Record<string, Cell | undefined>;
}

let draftSeq = 1;

function errorText(error: unknown): string {
  if (error instanceof Error && error.message) return error.message;
  return String(error);
}

export class LiveTable {
  filters: Filter[] = [];
  sort: SortSpec | null = null;
  loaded: Row[] = [];
  truncated = false;
  counted: number | null = null;
  error: string | null = null;
  sql = "";
  edits = new Map<string, Record<string, Cell>>();
  inserts: InsertDraft[] = [];
  deletes: string[] = [];
  failures = new Map<string, string>();
  selectedId: string | null = null;
  selectedColumn: string | null = null;
  ready = false;
  paging = false;
  store: TableStore;
  private readGeneration = 0;

  constructor(
    public target: TableTarget,
    public pageSize = 100,
  ) {
    this.store = {
      schema: {
        engine: target.engine,
        schema: target.schema,
        name: target.table,
        kind: "table",
        columns: [],
        key: [],
      },
      rows: [],
      readError: null,
    };
  }

  get canInsert(): boolean {
    return this.ready && this.store.schema.kind === "table";
  }

  /** 视图和没有主键的表在页查询里已经是完整值。 */
  private fullValuePage(): boolean {
    return this.store.schema.kind !== "table" || this.store.schema.key.length === 0;
  }

  /** 定位列为空时，页查询的 null 守卫已经返回全文，不再当成未展开的预览。 */
  private rowHasFullValues(row: Row): boolean {
    if (this.fullValuePage()) return true;
    return this.store.schema.key.some((name) => row.cells[name] === null);
  }

  get scopeLabel(): string {
    return "筛选和排序作用于整张表";
  }

  get keyLabel(): string {
    if (!this.ready) return "正在读取表结构";
    if (this.store.schema.kind === "view") return "只读视图";
    if (!this.store.schema.key.length) return "这张表没有主键，只能新增";
    return `按 ${this.store.schema.key.join(" + ")} 定位行`;
  }

  get pendingCount(): number {
    return this.edits.size + this.inserts.length + this.deletes.length;
  }

  async open(): Promise<void> {
    try {
      const schema = await gridDescribe(this.target);
      this.store = { schema, rows: [], readError: null };
      this.ready = true;
      await this.readWindow(0, this.pageSize, true);
    } catch (error) {
      this.ready = false;
      this.error = errorText(error);
    }
  }

  async readWindow(offset: number, limit: number, replace: boolean): Promise<string | null> {
    const generation = ++this.readGeneration;
    try {
      const page = await gridPage(this.target, this.filters, this.sort, offset, limit);
      if (generation !== this.readGeneration) return null;
      this.sql = page.sql;
      this.error = null;
      const rows = page.rows.map((row) => (this.rowHasFullValues(row) ? { ...row, resolved: true } : row));
      this.loaded = replace ? rows : [...this.loaded, ...rows];
      this.truncated = page.truncated;
      if (replace) this.counted = null;
      return null;
    } catch (error) {
      if (generation !== this.readGeneration) return null;
      const message = errorText(error);
      this.error = message;
      return message;
    }
  }

  async replaceFilters(filters: Filter[]): Promise<string | null> {
    for (const filter of filters) {
      if (filter.op === "between") {
        if (!filter.value || !filter.second) return "between 需要两个值";
        continue;
      }
      if (filter.op === "in") {
        if (!filter.values?.length) return "IN 筛选至少要一个值";
        continue;
      }
      if (opNeedsValue(filter.op) && !filter.value) return "筛选值不能为空";
    }
    this.filters = filters.map((filter) => ({ ...filter }));
    return this.readWindow(0, this.pageSize, true);
  }

  addFilter(filter: Filter): Promise<string | null> {
    return this.replaceFilters([...this.filters, filter]);
  }

  removeFilter(index: number): Promise<string | null> {
    return this.replaceFilters(this.filters.filter((_, item) => item !== index));
  }

  async cycleSort(column: string): Promise<void> {
    if (this.sort?.column !== column) this.sort = { column, direction: "asc" };
    else if (this.sort.direction === "asc") this.sort = { column, direction: "desc" };
    else this.sort = null;
    await this.readWindow(0, this.pageSize, true);
  }

  async clearSort(): Promise<void> {
    this.sort = null;
    await this.readWindow(0, this.pageSize, true);
  }

  loadMore(): Promise<string | null> {
    if (!this.truncated || this.paging) return Promise.resolve(null);
    this.paging = true;
    return this.readWindow(this.loaded.length, this.pageSize, false).finally(() => {
      this.paging = false;
    });
  }

  /** 导出前把可定位行的预览换成完整文本或十六进制。读不到就不要把预览协议写进文件。 */
  async expandPreviews(): Promise<string | null> {
    for (const row of [...this.loaded]) {
      if (row.resolved) continue;
      const needs = this.store.schema.columns.some((column) => needsFullRead(row.cells[column.name], column));
      if (!needs) continue;
      if (!row.located) return "不能按主键读取完整值，预览不能导出";
      const error = await this.readLocated(row.id);
      if (error) return error;
      const fresh = this.rowById(row.id);
      const still = this.store.schema.columns.some((column) => {
        const value = fresh?.cells[column.name];
        return typeof value === "string" && isPreviewEnvelope(value, column.typeName) && fresh?.resolved !== true;
      });
      if (still) return "还没有读到完整值，不能导出";
    }
    return null;
  }

  /** 能定位的行按主键另读完整文本或十六进制。用现有 grid_page 的筛选参数，不新增命令。 */
  async readLocated(rowId: string): Promise<string | null> {
    const row = this.rowById(rowId);
    if (!row?.locator || !row.located) return null;
    const filters: Filter[] = row.locator.columns.map((column, index) => ({
      column,
      op: "eq",
      value: row.locator?.values[index] ?? "",
    }));
    if (filters.some((filter) => !filter.value)) return "主键值不完整，不能另读这一格";
    try {
      const page = await gridPage(this.target, filters, null, 0, 1);
      const fresh = page.rows[0];
      const current = this.rowById(rowId);
      if (!fresh || !current) return "没有读到这一格";
      current.cells = { ...current.cells, ...fresh.cells };
      current.resolved = true;
      return null;
    } catch (error) {
      return errorText(error);
    }
  }

  async count(): Promise<string | null> {
    try {
      this.counted = await gridCount(this.target, this.filters);
      return null;
    } catch (error) {
      const message = errorText(error);
      this.error = message;
      return message;
    }
  }

  select(id: string, column: string | null): void {
    this.selectedId = id;
    this.selectedColumn = column;
  }

  rowById(id: string): Row | undefined {
    return this.loaded.find((row) => row.id === id);
  }

  canUpdate(row: Row | undefined): boolean {
    return this.store.schema.kind === "table" && !!row?.located;
  }

  canDeleteRow(id: string): boolean {
    if (this.inserts.some((insert) => insert.id === id)) return true;
    return this.canUpdate(this.rowById(id));
  }

  canDeleteSelection(): boolean {
    return this.selectedId !== null && this.canDeleteRow(this.selectedId);
  }

  stageUpdate(rowId: string, column: string, value: Cell): string | null {
    const row = this.rowById(rowId);
    if (!row) return "找不到行";
    const found = this.store.schema.columns.find((item) => item.name === column);
    const rejected = rejectWrite(found, value, row.resolved === true);
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

  async commit(): Promise<CommitReport | null> {
    try {
      const report = await gridCommit(this.target, this.payload());
      const appliedIds = new Set(report.applied.map((step) => step.id));
      for (const id of appliedIds) {
        if (id.startsWith("u:")) this.edits.delete(id.slice(2));
        if (id.startsWith("d:")) this.deletes = this.deletes.filter((rowId) => `d:${rowId}` !== id);
        this.inserts = this.inserts.filter((insert) => insert.id !== id);
        this.failures.delete(id);
      }
      if (report.failed) this.failures.set(report.failed.id, report.failed.message);
      await this.readWindow(0, Math.max(this.pageSize, this.loaded.length || this.pageSize), true);
      return report;
    } catch (error) {
      this.error = errorText(error);
      return null;
    }
  }

  changes(): Change[] {
    const changes: Change[] = [];
    for (const [rowId, sets] of this.edits) changes.push({ kind: "update", id: `u:${rowId}`, rowId, sets });
    for (const insert of this.inserts) changes.push({ kind: "insert", id: insert.id, values: { ...insert.cells } });
    for (const rowId of this.deletes) changes.push({ kind: "delete", id: `d:${rowId}`, rowId });
    return changes;
  }

  private payload(): ChangePayload[] {
    return this.changes().map((change) => {
      if (change.kind === "insert") {
        return {
          id: change.id,
          kind: "insert",
          sets: [],
          locator: [],
          values: Object.entries(change.values)
            .filter((entry): entry is [string, Cell] => entry[1] !== undefined)
            .map(([column, value]) => ({ column, value })),
        };
      }
      const row = this.rowById(change.rowId);
      const locator = (row?.locator?.columns ?? []).map((column, index) => ({
        column,
        value: row?.locator?.values[index] ?? null,
      }));
      if (change.kind === "delete") {
        return { id: change.id, kind: "delete", sets: [], locator, values: [] };
      }
      return {
        id: change.id,
        kind: "update",
        sets: Object.entries(change.sets).map(([column, value]) => ({ column, value, resolved: row?.resolved === true })),
        locator,
        values: [],
      };
    });
  }

  private sqlFor(change: Change): string {
    if (change.kind === "insert") return insertSql(this.store.schema, change.values);
    const row = this.rowById(change.rowId);
    if (!row?.locator) return "";
    if (change.kind === "delete") return deleteSql(this.store.schema, row.locator.columns, row.locator.values);
    return updateSql(this.store.schema, change.sets, row.locator.columns, row.locator.values);
  }
}
