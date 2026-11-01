import type { Engine } from "../engines";
import { quoteIdentifier } from "../query/completion/dialect";
import { locateForCompletion } from "../query/completion/scan";
import { parseScope, type ScopeTable } from "../query/completion/scope";

/** 目录树单击插入的引用：表（column 为 null）或表里的字段。 */
export interface InsertReference {
  connectionId: string;
  connectionName: string;
  engine: string | null;
  database: string;
  schema: string;
  table: string;
  column: string | null;
}

/** 插入结果：ok 已写入；none 没有可用编辑器；readonly/running 由调用方给提示。 */
export type InsertOutcome = "ok" | "none" | "readonly" | "running";

/** 单击后的处置：插入、只选中、新建标签再写入。 */
export type InsertPlan = "insert" | "select" | "create";

export interface QueryEditorApi {
  /** 在光标处写入引用；返回 false 表示编辑器不可用（未挂载或草稿不可写）。 */
  insert(reference: InsertReference): boolean;
  focus(): void;
  isRunning(): boolean;
}

const editors = new Map<string, QueryEditorApi>();
/** 只读编辑器（如对象定义视图）不接受插入；查询标签默认不是只读。 */
const readOnlyEditors = new Set<string>();
let focusedTabId: string | null = null;
let lastFocusedTabId: string | null = null;
let lastFocusedAt = 0;

/**
 * 焦点宽限期：点目录树时浏览器先把焦点移出编辑器，再派发 click。
 * 在这段窗口内仍把「刚失焦的编辑器」当作插入目标，否则单击插入永远找不到焦点。
 */
export const FOCUS_GRACE_MS = 400;

export function registerEditor(tabId: string, api: QueryEditorApi): void {
  editors.set(tabId, api);
}

export function unregisterEditor(tabId: string): void {
  editors.delete(tabId);
  readOnlyEditors.delete(tabId);
  if (focusedTabId === tabId) focusedTabId = null;
  if (lastFocusedTabId === tabId) {
    lastFocusedTabId = null;
    lastFocusedAt = 0;
  }
}

export function setEditorReadOnly(tabId: string, readOnly: boolean): void {
  if (readOnly) readOnlyEditors.add(tabId);
  else readOnlyEditors.delete(tabId);
}

/** 编辑器焦点变化由 QueryTab 的 updateListener 维护；失焦保留时间戳供宽限判定。 */
export function setEditorFocus(tabId: string, focused: boolean): void {
  if (!editors.has(tabId)) return;
  if (focused) {
    focusedTabId = tabId;
    lastFocusedTabId = tabId;
    lastFocusedAt = Date.now();
    return;
  }
  if (focusedTabId === tabId) focusedTabId = null;
  if (lastFocusedTabId === tabId) lastFocusedAt = Date.now();
}

export function focusedEditor(now: number = Date.now()): { tabId: string; api: QueryEditorApi } | null {
  if (focusedTabId) {
    const api = editors.get(focusedTabId);
    if (api) return { tabId: focusedTabId, api };
  }
  if (!lastFocusedTabId) return null;
  const api = editors.get(lastFocusedTabId);
  if (!api) return null;
  return now - lastFocusedAt <= FOCUS_GRACE_MS ? { tabId: lastFocusedTabId, api } : null;
}

export function insertAtCursor(tabId: string, reference: InsertReference): InsertOutcome {
  const api = editors.get(tabId);
  if (!api) return "none";
  if (readOnlyEditors.has(tabId)) return "readonly";
  if (api.isRunning()) return "running";
  return api.insert(reference) ? "ok" : "none";
}

/** 判定顺序：有聚焦编辑器就插入；否则有查询标签只选中；都没有才新建标签。 */
export function insertPlan(hasFocusedEditor: boolean, hasQueryTab: boolean): InsertPlan {
  if (hasFocusedEditor) return "insert";
  return hasQueryTab ? "select" : "create";
}

/** 测试与热重载用：清空注册表，避免跨用例残留编辑器。 */
export function resetEditors(): void {
  editors.clear();
  readOnlyEditors.clear();
  focusedTabId = null;
  lastFocusedTabId = null;
  lastFocusedAt = 0;
}

const DEFAULT_SCHEMA: Record<string, string> = { postgres: "public", sqlserver: "dbo", sqlite: "main" };

function asEngine(value: string | null): Engine {
  if (value === "mysql" || value === "mariadb" || value === "sqlite" || value === "sqlserver" || value === "postgres") return value;
  return "postgres";
}

function sameName(left: string, right: string): boolean {
  return left.toLocaleLowerCase() === right.toLocaleLowerCase();
}

function statementTables(draft: string, cursor: number): ScopeTable[] {
  const located = locateForCompletion(draft, Math.max(0, Math.min(cursor, draft.length)));
  if (located.refusal) return [];
  const scope = parseScope(draft.slice(located.start, located.end));
  return scope.target ? [...scope.tables, scope.target] : scope.tables;
}

/** 表名限定：非默认 schema（PostgreSQL/SQLite/SQL Server）或非当前库（MySQL）才带前缀。 */
function tablePrefix(reference: InsertReference, engine: Engine, currentDatabase: string | null): string {
  if (engine === "mysql" || engine === "mariadb") {
    if (!reference.database) return "";
    if (currentDatabase && sameName(currentDatabase, reference.database)) return "";
    return `${quoteIdentifier(engine, reference.database)}.`;
  }
  if (!reference.schema) return "";
  const fallback = DEFAULT_SCHEMA[engine];
  if (fallback && sameName(fallback, reference.schema)) return "";
  return `${quoteIdentifier(engine, reference.schema)}.`;
}

/**
 * 插入文本（R4）：字段优先走别名，其次表名限定，语句里没有该表时只写字段；
 * 表名按引擎补限定名。需要引号的标识符按引擎加引号。
 */
export function buildInsertText(
  reference: InsertReference,
  draft: string,
  cursor: number,
  currentDatabase: string | null = null,
): string {
  const engine = asEngine(reference.engine);
  const table = quoteIdentifier(engine, reference.table);
  if (!reference.column) return `${tablePrefix(reference, engine, currentDatabase)}${table}`;
  const column = quoteIdentifier(engine, reference.column);
  const hit = statementTables(draft, cursor).find(
    (item) => sameName(item.name, reference.table) && (!item.schema || !reference.schema || sameName(item.schema, reference.schema)),
  );
  if (!hit) return column;
  if (hit.alias !== hit.name) return `${quoteIdentifier(engine, hit.alias)}.${column}`;
  return `${table}.${column}`;
}
