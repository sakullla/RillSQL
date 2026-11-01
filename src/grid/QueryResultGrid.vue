<script setup lang="ts">
import { computed, inject, nextTick, onBeforeUnmount, reactive, ref, watch, type ComponentPublicInstance, type Ref } from "vue";
import { tr as t } from "../i18n";
import { catalogConnections } from "../catalog/store";
import { registerPending } from "../shell/close-guard";
import ExportDialog from "../export/ExportDialog.vue";
import type { CsvOptions } from "../export/csv";
import type { InsertTarget } from "../export/insert";
import { planExport, type ExportScope } from "../export/plan";
import { runExportTask, runRemoteExport } from "../export/tasks";
import type { ExportFormat } from "../export/write";
import { continueTabRun, tabRun } from "../query/runtime";
import { gridDescribe } from "./api";
import { rectangleSelection, selectionTsv, type CellPoint } from "./clipboard";
import { viewLoadedRows } from "./data";
import { useRowWindow } from "./virtual";
import { copyCellText, copySqlValue, showCell } from "./display";
import "./grid.css";
import Icon from "../shell/Icon.vue";
import { toast } from "../ui/toast";
import { loadSettings } from "../shell/settings";
import { locateResultRows, provedEditableColumns, readSingleTable, refusalText, resolveEditableTable, ResultEditSession, type TableHit } from "./result-edit";
import { rejectWrite, typeCaption, valueFamily } from "./typeset";
import type { Cell, Column, Engine, Row, SortSpec } from "./types";

const props = defineProps<{
  tabId: string;
  statement: string;
  columns: Column[];
  rows: Row[];
  truncated: boolean;
  connectionId?: string;
  engine?: Engine;
  database?: string;
  schema?: string;
}>();

const rootEl = ref<HTMLElement | null>(null);
/* 错误卡语义图标：圆 + 叉，与 DataGrid/对象树/历史等处的统一 err 线稿一致 */
const ERROR_ICON = "M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18M15 9l-6 6M9 9l6 6";
const find = ref("");
const sort = ref<SortSpec | null>(null);
const exportOpen = ref(false);
const exportMessage = ref("");
const selected = ref<{ row: string; column: string; value: Cell } | null>(null);
const copyMessage = ref("");
/** 查询页把结果标题栏留给查找与导出，省掉一整行工具栏；单独挂载时留在网格内。 */
const toolsHost = inject<Ref<HTMLElement | null> | null>("resultTools", null);
const toolsTarget = computed(() => toolsHost?.value ?? null);
const reason = ref(t("grid.resultEdit.readonly"));
const insertTarget = ref<InsertTarget | null>(null);
const editSession = ref<ResultEditSession | null>(null);
const editing = ref<{ id: string; column: string; draft: string; isNull: boolean } | null>(null);
const editError = ref("");
const commitOpen = ref(false);
const commitError = ref("");
const discardArmed = ref(false);
let discardTimer: ReturnType<typeof setTimeout> | undefined;
const receipt = ref<string | null>(null);
const revealed = reactive(new Set<string>());
const widths = reactive<Record<string, number>>({});
const anchor = ref<CellPoint | null>(null);
const focusPoint = ref<CellPoint | null>(null);
const dragging = ref(false);
const menu = ref<{ x: number; y: number; row: string; column: string } | null>(null);
const menuError = ref("");
let generation = 0;
let continuing = false;
let resizeMove: ((event: PointerEvent) => void) | null = null;
let resizeUp: (() => void) | null = null;

function tableHits(): TableHit[] {
  if (!props.connectionId) return [];
  const connection = catalogConnections().find((item) => item.id === props.connectionId);
  const hits: TableHit[] = [];
  for (const database of connection?.databases ?? []) {
    if (props.database && database.name !== props.database) continue;
      for (const schema of database.schemas) {
      for (const object of schema.objects) {
        if (object.stale || (object.kind !== "table" && object.kind !== "view")) continue;
        hits.push({ schema: schema.name, name: object.name, kind: object.kind });
      }
    }
  }
  return hits;
}

/** 设置页的预览上限对结果网格也立即生效。 */
function previewBytes(): number {
  return Math.max(1, loadSettings().previewKB) * 1024;
}

watch(
  // 不监听 rows 引用：轮询会反复换行对象，旧实现因此每 400ms 重建一次编辑会话。
  () => [props.statement, props.connectionId, props.database, props.schema, props.engine, tabRun(props.tabId)?.id, tabRun(props.tabId)?.status, props.rows.length] as const,
  () => { void resolveEdit(); },
  { immediate: true },
);

async function resolveEdit() {
  // 已经暂存的修改不能被重新解析冲掉；QueryTab 也会在重跑前拦住有暂存的结果。
  if (editSession.value && editSession.value.pendingCount > 0) return;
  const run = tabRun(props.tabId);
  if (run && (run.status === "running" || run.status === "cancel-requested")) {
    editSession.value = null;
    insertTarget.value = null;
    reason.value = t("grid.resultEdit.stillRunning");
    return;
  }
  const current = ++generation;
  editing.value = null;
  commitOpen.value = false;
  editSession.value = null;
  insertTarget.value = null;
  const structural = readSingleTable(props.statement, props.engine ?? "postgres");
  const engine = props.engine ?? "postgres";
  if (!structural.ok) {
    reason.value = refusalText(structural.reason);
    return;
  }
  const resolved = resolveEditableTable(structural.table, tableHits(), props.database ?? "", engine);
  if (!resolved.ok) {
    reason.value = refusalText(resolved.reason);
    return;
  }
  if (!props.connectionId) {
    reason.value = t("grid.resultEdit.noConnection");
    return;
  }
  reason.value = t("grid.resultEdit.checking");
  try {
    const described = await gridDescribe({
      connectionId: props.connectionId,
      database: props.database ?? "",
      schema: resolved.schema,
      table: resolved.table,
      engine,
    });
    if (current !== generation) return;
    const editable = provedEditableColumns(described, props.columns, structural.columns);
    insertTarget.value = described.kind === "table"
      ? {
          engine: described.engine,
          schema: resolved.schema,
          name: resolved.table,
          columns: props.columns.map((column) => {
            const base = editable.includes(column.name) ? described.columns.find((item) => item.name === column.name) : undefined;
            return { name: column.name, typeName: base?.typeName || column.typeName };
          }),
        }
      : null;
    const located = locateResultRows(described, props.columns, props.rows, structural.columns);
    if (!located.ok) {
      editSession.value = null;
      reason.value = refusalText(located.reason);
      return;
    }
    editSession.value = reactive(new ResultEditSession(described, {
      connectionId: props.connectionId,
      database: props.database ?? "",
      schema: resolved.schema,
      table: resolved.table,
      engine: described.engine,
    }, located.rows, located.editable));
    reason.value = t("grid.resultEdit.locating", { key: described.key.join(" + ") });
  } catch (error) {
    if (current !== generation) return;
    editSession.value = null;
    insertTarget.value = null;
    reason.value = error instanceof Error ? error.message : String(error);
  }
}

const sourceRows = computed(() => {
  const session = editSession.value;
  const rows = session?.rows ?? props.rows;
  if (!session) return rows;
  return rows.map((row) => {
    const sets = session.edits.get(row.id);
    if (!sets) return row;
    return { ...row, cells: { ...row.cells, ...sets } };
  });
});
const view = computed(() => viewLoadedRows(sourceRows.value, props.columns, find.value, sort.value));
const { viewport, range, onScroll, reset, reveal } = useRowWindow(computed(() => view.value.rows.length), false);
function setViewport(el: Element | ComponentPublicInstance | null): void {
  viewport.value = el instanceof HTMLElement ? el : null;
}
const visibleRows = computed(() => view.value.rows.slice(range.value.start, range.value.end).map((row, index) => ({
  id: row.id,
  number: range.value.start + index + 1,
  located: row.located,
  deleted: editSession.value?.deletes.includes(row.id) ?? false,
  failed: !!(editSession.value?.failures.get(`u:${row.id}`) || editSession.value?.failures.get(`d:${row.id}`)),
  failure: [editSession.value?.failures.get(`u:${row.id}`), editSession.value?.failures.get(`d:${row.id}`)].filter(Boolean).join("\n"),
  cells: props.columns.map((column) => showCell(row.cells[column.name] ?? null, { column, previewBytes: previewBytes(), opened: revealed.has(`${row.id}:${column.name}`), resolved: true })),
})));
const selectedPoints = computed(() => (anchor.value && focusPoint.value ? rectangleSelection(view.value.rows, props.columns, anchor.value, focusPoint.value) : []));
watch([find, sort], reset);
watch(() => tabRun(props.tabId)?.id, () => reset());
watch(
  () => range.value.end,
  (end) => {
    if (continuing || find.value) return;
    if (!view.value.rows.length || end < view.value.rows.length) return;
    const run = tabRun(props.tabId);
    if (!run || (run.status !== "running" && run.status !== "cancel-requested")) return;
    const statement = run.statements.find((item) => item.sql === props.statement);
    if (!statement || statement.truncated || statement.rows.length === 0) return;
    continuing = true;
    void continueTabRun(props.tabId).finally(() => {
      continuing = false;
    });
  },
);

const commitFailures = computed(() => (editSession.value ? Array.from(editSession.value.failures, ([id, message]) => ({ id, message })) : []));
const pendingVisible = computed(() => !!editSession.value && (editSession.value.pendingCount > 0 || editSession.value.failures.size > 0));
const boundConnection = computed(() => {
  if (!props.connectionId) return null;
  return catalogConnections().find((item) => item.id === props.connectionId) ?? null;
});
const connectionName = computed(() => boundConnection.value?.name.trim() || props.connectionId || "");
const environmentLabel = computed(() => boundConnection.value?.envLabel.trim() ?? "");
const environmentTone = computed(() => boundConnection.value?.envTone ?? "none");
const objectName = computed(() => {
  const target = editSession.value?.target;
  if (!target) return "";
  return target.schema ? `${target.schema}.${target.table}` : target.table;
});
let releasePending: (() => void) | null = null;
function releaseCellPending(): void {
  releasePending?.();
  releasePending = null;
}
/** 只登记未提交的单元格修改；未执行 SQL 不进入关闭确认。 */
watch(
  () => [props.tabId, (editSession.value?.pendingCount ?? 0) > 0] as const,
  ([tabId, pending]) => {
    releaseCellPending();
    if (!pending) return;
    releasePending = registerPending(tabId, () => props.tabId === tabId && (editSession.value?.pendingCount ?? 0) > 0);
  },
  { immediate: true },
);
onBeforeUnmount(() => {
  releaseCellPending();
  stopDrag();
  resizeUp?.();
  clearTimeout(discardTimer);
});

function inspect(rowId: string, index: number) {
  const row = view.value.rows.find((item) => item.id === rowId);
  const column = props.columns[index];
  if (!row || !column) return;
  selected.value = { row: rowId, column: column.name, value: editSession.value?.cellValue(rowId, column.name) ?? row.cells[column.name] ?? null };
  editSession.value?.select(rowId, column.name);
  copyMessage.value = "";
}

function cellSelector(id: string, column: string): string {
  return `[data-cell="${CSS.escape(id)}:${CSS.escape(column)}"]`;
}

function focusCell(id: string, column: string) {
  const index = view.value.rows.findIndex((row) => row.id === id);
  if (index >= 0) reveal(index);
  void nextTick(() => {
    (rootEl.value?.querySelector(cellSelector(id, column)) as HTMLElement | null)?.focus();
  });
}

function currentCellIndex(): number {
  const current = selected.value;
  if (!current) return -1;
  const rowIndex = view.value.rows.findIndex((row) => row.id === current.row);
  const columnIndex = props.columns.findIndex((column) => column.name === current.column);
  if (rowIndex < 0 || columnIndex < 0) return -1;
  return rowIndex * props.columns.length + columnIndex;
}

/** Tab / Shift+Tab：按桌面习惯在单元格间移动选中，越过边界时循环。 */
function moveSelection(delta: number) {
  const rows = view.value.rows;
  const cols = props.columns;
  if (!rows.length || !cols.length) return;
  const total = rows.length * cols.length;
  let index = currentCellIndex();
  if (index < 0) index = delta > 0 ? -1 : 0;
  index = (index + delta + total) % total;
  const row = rows[Math.floor(index / cols.length)]!;
  const columnIndex = index % cols.length;
  const column = cols[columnIndex]!;
  anchor.value = { row: row.id, column: column.name };
  focusPoint.value = { row: row.id, column: column.name };
  inspect(row.id, columnIndex);
  focusCell(row.id, column.name);
}

/** Shift+方向键：保留起点，扩展矩形选择。 */
function extendSelection(rowStep: number, columnStep: number) {
  const rows = view.value.rows;
  const cols = props.columns;
  const current = focusPoint.value ?? (selected.value ? { row: selected.value.row, column: selected.value.column } : null);
  if (!rows.length || !cols.length || !current) return;
  const rowIndex = rows.findIndex((row) => row.id === current.row);
  const columnIndex = cols.findIndex((column) => column.name === current.column);
  if (rowIndex < 0 || columnIndex < 0) return;
  const nextRow = rows[Math.min(rows.length - 1, Math.max(0, rowIndex + rowStep))]!;
  const nextIndex = Math.min(cols.length - 1, Math.max(0, columnIndex + columnStep));
  anchor.value ??= current;
  focusPoint.value = { row: nextRow.id, column: cols[nextIndex]!.name };
  inspect(nextRow.id, nextIndex);
  focusCell(nextRow.id, cols[nextIndex]!.name);
}

/** Ctrl+A：选中当前已读取的全部单元格。 */
function selectAllCells() {
  const rows = view.value.rows;
  const cols = props.columns;
  if (!rows.length || !cols.length) return;
  const first = rows[0]!;
  anchor.value = { row: first.id, column: cols[0]!.name };
  focusPoint.value = { row: rows[rows.length - 1]!.id, column: cols[cols.length - 1]!.name };
  inspect(first.id, 0);
}

function beginEdit(id: string, column: string) {
  const session = editSession.value;
  const meta = props.columns.find((item) => item.name === column);
  if (rejectWrite(meta, null)) return;
  const row = session?.rowById(id);
  if (!session || !row?.located || session.deletes.includes(id) || !session.canEditColumn(column)) return;
  const current = session.cellValue(id, column);
  editError.value = "";
  editing.value = { id, column, draft: current ?? "", isNull: current === null };
  session.select(id, column);
}

function saveEdit() {
  const edit = editing.value;
  const session = editSession.value;
  if (!edit || !session) return;
  const error = session.stageUpdate(edit.id, edit.column, edit.isNull ? null : edit.draft);
  if (error) {
    editError.value = error;
    return;
  }
  editError.value = "";
  const { id, column } = edit;
  editing.value = null;
  receipt.value = null;
  focusCell(id, column);
}

function cancelEdit() {
  const edit = editing.value;
  editing.value = null;
  if (edit) focusCell(edit.id, edit.column);
}

function focusEditor(el: Element | ComponentPublicInstance | null) {
  if (!(el instanceof HTMLInputElement)) return;
  el.focus();
  el.select();
}

/** 编辑中：Enter 保存，Esc 取消，Tab 保存并移到下一个单元格。 */
function onEditorKeydown(event: KeyboardEvent) {
  if (event.key === "Enter") {
    event.preventDefault();
    event.stopPropagation();
    saveEdit();
  } else if (event.key === "Escape") {
    event.preventDefault();
    event.stopPropagation();
    cancelEdit();
  } else if (event.key === "Tab") {
    event.preventDefault();
    event.stopPropagation();
    saveEdit();
    moveSelection(event.shiftKey ? -1 : 1);
  }
}

function cycle(column: string) {
  if (sort.value?.column !== column) sort.value = { column, direction: "asc" };
  else if (sort.value.direction === "asc") sort.value = { column, direction: "desc" };
  else sort.value = null;
}

function cells(row: Row): Record<string, Cell> {
  return { ...row.cells };
}

function exportRows(): Record<string, Cell>[] {
  if (editSession.value) return editSession.value.committedCells();
  return props.rows.map(cells);
}

async function copyValue() {
  if (!selected.value) return;
  const column = props.columns.find((item) => item.name === selected.value?.column);
  const text = copyCellText(selected.value.value, loadSettings().nullCopy, column, true);
  try { await navigator.clipboard.writeText(text); copyMessage.value = t("grid.value.copied"); }
  catch { copyMessage.value = t("grid.errors.clipboardFailed"); }
}

/** 结果网格也能复制 SQL 字面量；NULL 写 NULL，文本按引擎转义。 */
async function copySqlValueSelected() {
  if (!selected.value) return;
  const column = props.columns.find((item) => item.name === selected.value?.column);
  const text = copySqlValue(selected.value.value, column, props.engine ?? "postgres");
  try { await navigator.clipboard.writeText(text); copyMessage.value = t("grid.value.copiedSqlValue"); }
  catch { copyMessage.value = t("grid.errors.clipboardFailed"); }
}

function isPicked(row: string, column: string): boolean {
  return selectedPoints.value.some((point) => point.row === row && point.column === column);
}

function columnStyle(name: string): Record<string, string> | undefined {
  const width = widths[name];
  if (!width) return undefined;
  return { width: `${width}px`, minWidth: `${width}px`, maxWidth: `${width}px` };
}

/** 数值列（整数/定点/浮点）右对齐，列头与单元格一致。 */
function numericColumn(column: { typeName: string }): boolean {
  const family = valueFamily(column.typeName);
  return family === "integer" || family === "decimal" || family === "float";
}

function startResize(name: string, event: PointerEvent) {
  const th = ((event.currentTarget as HTMLElement | null) ?? (event.target as HTMLElement | null))?.closest("th");
  const startX = event.clientX;
  const startWidth = widths[name] ?? th?.getBoundingClientRect().width ?? 120;
  resizeMove = (ev) => {
    widths[name] = Math.max(48, Math.round(startWidth + ev.clientX - startX));
  };
  resizeUp = () => {
    if (resizeMove) window.removeEventListener("pointermove", resizeMove);
    if (resizeUp) window.removeEventListener("pointerup", resizeUp);
    resizeMove = null;
    resizeUp = null;
  };
  window.addEventListener("pointermove", resizeMove);
  window.addEventListener("pointerup", resizeUp);
}

function stopDrag() {
  dragging.value = false;
  window.removeEventListener("pointerup", stopDrag);
}

function onCellDown(row: string, column: string, event: PointerEvent) {
  if (event.button !== 0) return;
  anchor.value = { row, column };
  focusPoint.value = { row, column };
  dragging.value = true;
  menu.value = null;
  inspect(row, props.columns.findIndex((item) => item.name === column));
  window.addEventListener("pointerup", stopDrag);
}

function onCellEnter(row: string, column: string) {
  if (!dragging.value) return;
  focusPoint.value = { row, column };
}

function onCellMenu(row: string, column: string, event: MouseEvent) {
  event.preventDefault();
  if (!isPicked(row, column)) {
    anchor.value = { row, column };
    focusPoint.value = { row, column };
  }
  menuError.value = "";
  menu.value = { x: event.clientX, y: event.clientY, row, column };
}

async function copySelection() {
  const points = selectedPoints.value.length
    ? selectedPoints.value
    : selected.value
      ? [{ row: selected.value.row, column: selected.value.column }]
      : [];
  if (!points.length) return;
  const text = selectionTsv(points, (point) => {
    const column = props.columns.find((item) => item.name === point.column);
    const row = sourceRows.value.find((item) => item.id === point.row);
    const value = editSession.value?.cellValue(point.row, point.column) ?? row?.cells[point.column] ?? null;
    return copyCellText(value, loadSettings().nullCopy, column, true);
  });
  try {
    await navigator.clipboard.writeText(text);
    copyMessage.value = t("grid.value.copied");
    toast(t("grid.menu.copiedCells", { n: points.length }));
  } catch {
    copyMessage.value = t("grid.errors.clipboardFailed");
  }
}

function editFromMenu() {
  const target = menu.value;
  if (!target) return;
  const meta = props.columns.find((item) => item.name === target.column);
  const rejected = rejectWrite(meta, null);
  if (rejected) {
    menuError.value = rejected;
    return;
  }
  const session = editSession.value;
  const row = session?.rowById(target.row);
  if (!session || !row?.located || session.deletes.includes(target.row) || !session.canEditColumn(target.column)) {
    menuError.value = t("grid.errors.readonlyCell");
    return;
  }
  menu.value = null;
  beginEdit(target.row, target.column);
}

function setNullFromMenu() {
  const target = menu.value;
  const session = editSession.value;
  if (!target) return;
  const meta = props.columns.find((item) => item.name === target.column);
  const rejected = rejectWrite(meta, null);
  if (rejected || !session) {
    menuError.value = rejected ?? t("grid.errors.readonlyCell");
    return;
  }
  const error = session.stageUpdate(target.row, target.column, null);
  if (error) menuError.value = error;
  else {
    menu.value = null;
    receipt.value = null;
  }
}

function revealSelected() {
  if (!selected.value) return;
  revealed.add(`${selected.value.row}:${selected.value.column}`);
}

const inspectorColumn = computed(() => {
  const current = selected.value;
  return current ? props.columns.find((item) => item.name === current.column) : undefined;
});

/** 未编辑时：Enter 开始编辑，方向键和 Tab 移动选中，快捷键与帮助里写的一致。 */
function onGridKeydown(event: KeyboardEvent) {
  if (editing.value) return;
  const mod = event.ctrlKey || event.metaKey;
  const key = event.key.toLowerCase();
  if (mod && event.shiftKey && key === "c") {
    event.preventDefault();
    void copySqlValueSelected();
    return;
  }
  if (mod && key === "c") {
    event.preventDefault();
    void copySelection();
    return;
  }
  if (mod && key === "a") {
    event.preventDefault();
    selectAllCells();
    return;
  }
  if (mod && key === "s") {
    event.preventDefault();
    if (editSession.value?.pendingCount) openCommit();
    return;
  }
  if (event.key === "Delete") {
    event.preventDefault();
    deleteSelected();
    return;
  }
  if (["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight"].includes(event.key)) {
    event.preventDefault();
    const rowStep = event.key === "ArrowUp" ? -1 : event.key === "ArrowDown" ? 1 : 0;
    const columnStep = event.key === "ArrowLeft" ? -1 : event.key === "ArrowRight" ? 1 : 0;
    if (event.shiftKey) extendSelection(rowStep, columnStep);
    else moveSelection(rowStep * props.columns.length + columnStep);
  } else if (event.key === "Enter" || event.key === "F2") {
    if (selected.value) {
      event.preventDefault();
      beginEdit(selected.value.row, selected.value.column);
    }
  } else if (event.key === "Tab") {
    event.preventDefault();
    moveSelection(event.shiftKey ? -1 : 1);
  } else if (event.key === "Escape") {
    (document.activeElement as HTMLElement | null)?.blur();
  }
}

function deleteSelected() {
  if (!selected.value || !editSession.value) return;
  const error = editSession.value.deleteRow(selected.value.row);
  if (error) {
    editError.value = error;
    return;
  }
  editError.value = "";
  receipt.value = null;
}

async function commitChangesNow() {
  const session = editSession.value;
  if (!session) return;
  commitError.value = "";
  const pendingBefore = session.pendingCount;
  const report = await session.commit();
  if (!report) {
    commitError.value = commitFailures.value[0]?.message || t("grid.commit.incomplete");
    return;
  }
  const saved = report.applied.length;
  if (report.failed) {
    const waiting = report.pendingIds.filter((id) => id !== report.failed?.id).length;
    commitError.value = t("grid.commit.savedOneFailedRest", { n: saved, waiting });
    return;
  }
  commitOpen.value = false;
  receipt.value = t("grid.commit.savedAll", { n: saved });
  if (saved !== pendingBefore && session.pendingCount > 0) receipt.value += t("grid.commit.pendingLeft");
}

function openCommit() {
  commitError.value = "";
  commitOpen.value = true;
}

function discardAll() {
  const session = editSession.value;
  if (!session) return;
  if (!discardArmed.value) {
    discardArmed.value = true;
    clearTimeout(discardTimer);
    discardTimer = setTimeout(() => {
      discardArmed.value = false;
    }, 4000);
    return;
  }
  clearTimeout(discardTimer);
  discardArmed.value = false;
  commitError.value = "";
  session.discard();
  receipt.value = null;
}

function startExport(payload: { scope: ExportScope; acknowledged: boolean; path: string; csv: CsvOptions; format: ExportFormat }) {
  const plan = planExport({
    origin: "query",
    scope: payload.scope,
    statement: props.statement,
    truncated: props.truncated,
    fileName: payload.path,
    engine: props.engine,
  });
  if (payload.format === "insert" && !insertTarget.value?.name) {
    exportMessage.value = t("grid.resultEdit.insertNotStarted", { reason: reason.value });
    exportOpen.value = false;
    return;
  }
  if (payload.scope === "full") {
    if (!plan.allowed || !props.connectionId) {
      const blocked = plan.allowed
        ? {
            ...plan,
            allowed: false,
            serverRead: false,
            reexecutes: false,
            rejectReason: t("grid.resultEdit.fullExportNeedsConnection"),
          }
        : plan;
      void runExportTask({
        plan: blocked,
        acknowledged: payload.acknowledged,
        path: plan.fileName,
        columns: props.columns.map((column) => column.name),
        rows: [],
        csv: payload.csv,
        format: payload.format,
        insert: insertTarget.value ?? undefined,
      }).then(({ result }) => { exportMessage.value = result.message; });
    } else {
      exportMessage.value = t("grid.export.exportingAllResults");
      const { done } = runRemoteExport({
        plan,
        acknowledged: payload.acknowledged,
        path: plan.fileName,
        csv: payload.csv,
        format: payload.format,
        insert: insertTarget.value ?? undefined,
        mode: "query",
        connectionId: props.connectionId,
        database: props.database ?? "",
        schema: insertTarget.value?.schema || props.schema || "",
        table: insertTarget.value?.name || "",
        engine: props.engine ?? "postgres",
        filters: [],
        sort: null,
        limit: 0,
        truncated: props.truncated,
        statement: props.statement,
      });
      void done.then(task => { exportMessage.value = task.message; });
    }
    exportOpen.value = false;
    return;
  }
  exportMessage.value = t("grid.export.savingLoaded");
  void runExportTask({
    plan,
    acknowledged: payload.acknowledged,
    path: plan.fileName,
    columns: props.columns.map((column) => column.name),
    rows: exportRows(),
    csv: payload.csv,
    format: payload.format,
    insert: insertTarget.value ?? undefined,
  }).then(({ result }) => { exportMessage.value = result.message; });
  exportOpen.value = false;
}

watch(() => props.rows, (rows) => {
  if (selected.value && !rows.some((row) => row.id === selected.value?.row)) selected.value = null;
});
</script>

<template>
  <div ref="rootEl" class="data-grid" :aria-label="t('grid.queryArea')" :data-tab-id="tabId" data-scope="loaded" :data-editable="editSession ? 'true' : 'false'">
    <div class="grid-main">
    <Teleport :to="toolsTarget" :disabled="!toolsTarget">
    <div class="result-toolbar">
      <input v-model="find" :aria-label="t('grid.find.label')" :placeholder="t('grid.find.label')" :title="t('grid.find.hintTitle', { note: view.note })" />
      <span class="sr">{{ t("grid.find.hint", { n: rows.length }) }}</span>
      <span class="grow"></span>
      <span class="toolbar-hint key-hint" :title="reason">{{ reason }}</span>
      <button v-if="editSession" class="btn sm" type="button" :disabled="!selected || !editSession.canDeleteRow(selected.row)" @click="deleteSelected">{{ t("grid.toolbar.deleteRow") }}</button>
      <button class="btn sm" type="button" @click="exportOpen = true"><Icon name="download" />{{ t("grid.toolbar.export") }}</button>
    </div>
    </Teleport>
    <p v-if="exportMessage" class="grid-hint" role="status">{{ exportMessage }}</p>
    <p v-if="receipt" class="grid-hint" role="status">{{ receipt }}</p>
    <div :ref="setViewport" class="grid-scroll" @scroll.passive="onScroll" @keydown="onGridKeydown">
      <table class="grid-table" :aria-rowcount="view.rows.length + 1">
        <thead>
          <tr>
            <th class="row-number" :aria-label="t('grid.cell.rowNumber')">#</th>
            <th v-for="column in columns" :key="column.name" :class="{ num: numericColumn(column) }" :style="columnStyle(column.name)" :data-type="typeCaption(column)" :aria-sort="sort?.column === column.name ? (sort.direction === 'asc' ? 'ascending' : 'descending') : 'none'">
              <button class="th-btn" type="button" :aria-label="t('grid.cell.sortLoaded', { column: column.name })" @click="cycle(column.name)"><span class="th-name">{{ column.name }}</span><span class="col-type">{{ typeCaption(column) }}</span><span v-if="sort?.column === column.name" class="sort-arrow" aria-hidden="true">{{ sort.direction === 'asc' ? '↑' : '↓' }}</span></button>
              <span class="col-resize" role="separator" :aria-label="t('grid.read.resizeColumn', { column: column.name })" @pointerdown.stop.prevent="startResize(column.name, $event)"></span>
            </th>
          </tr>
        </thead>
        <tbody>
          <tr v-if="range.top" class="virtual-spacer" aria-hidden="true"><td :colspan="columns.length + 1" :style="{ height: `${range.top}px` }"></td></tr>
          <tr v-for="row in visibleRows" :key="row.id" :data-row-id="row.id" :aria-rowindex="row.number + 1" :class="{ 'row-delete': row.deleted, 'row-failed': row.failed }" :title="row.failure || undefined">
            <td class="row-number">{{ row.number }}<span v-if="row.deleted" class="row-flag del">{{ t("grid.editor.deleteFlag") }}</span></td>
            <td v-for="(cell, index) in row.cells" :key="index" :class="{ num: columns[index] ? numericColumn(columns[index]) : false }" :style="columnStyle(columns[index]?.name ?? '')">
              <div v-if="editing && editing.id === row.id && editing.column === columns[index]?.name" class="editor">
                <input :ref="focusEditor" v-model="editing.draft" :aria-label="t('grid.editor.editColumn', { column: columns[index]?.name })" :disabled="editing.isNull" @keydown="onEditorKeydown" />
                <button class="btn sm" type="button" :disabled="columns[index] ? !((editSession?.schema.columns.find(item => item.name === columns[index]?.name)?.nullable) ?? true) : false" @click="editing.isNull = true">{{ t("grid.editor.setNull") }}</button>
                <button class="btn sm" type="button" @click="editing.isNull = false">{{ t("grid.editor.emptyString") }}</button>
                <button class="btn sm primary" type="button" @click="saveEdit">{{ t("common.ok") }}</button>
                <button class="btn sm ghost" type="button" @click="cancelEdit">{{ t("common.cancel") }}</button>
                <p v-if="editError" class="menu-error" role="alert">{{ editError }}</p>
              </div>
              <button v-else class="cell-btn" :class="{ on: selected?.row === row.id && selected?.column === columns[index]?.name, range: isPicked(row.id, columns[index]?.name ?? '') }" :data-cell="`${row.id}:${columns[index]?.name}`" type="button" @pointerdown="onCellDown(row.id, columns[index]?.name ?? '', $event)" @pointerenter="onCellEnter(row.id, columns[index]?.name ?? '')" @contextmenu="onCellMenu(row.id, columns[index]?.name ?? '', $event)" @dblclick="beginEdit(row.id, columns[index]?.name ?? '')"><span :class="{ 'v-null': cell.kind === 'null', 'v-empty': cell.kind === 'empty', 'v-large': cell.kind === 'large' }">{{ cell.label }}</span></button>
            </td>
          </tr>
          <tr v-if="range.bottom" class="virtual-spacer" aria-hidden="true"><td :colspan="columns.length + 1" :style="{ height: `${range.bottom}px` }"></td></tr>
          <tr v-if="!rows.length">
            <td class="grid-empty" :colspan="columns.length + 1">{{ t("grid.find.noRows") }}</td>
          </tr>
          <tr v-else-if="!view.rows.length">
            <td class="grid-empty" :colspan="columns.length + 1">{{ t("grid.find.noMatch", { find }) }}</td>
          </tr>
        </tbody>
      </table>
    </div>
    <footer class="grid-foot" :class="{ quiet: !find && !truncated }">
      <span v-if="find">{{ t("grid.find.matched", { n: view.rows.length }) }}</span>
      <span v-if="truncated">{{ t("grid.find.truncatedNoteA") }}<b>{{ rows.length }}</b>{{ t("grid.find.truncatedNoteB", { n: rows.length }) }}</span>
      <span v-else>{{ t("grid.find.allRows", { n: rows.length }) }}</span>
      <span v-if="truncated" class="muted">{{ t("grid.find.fullExportNote") }}</span>
    </footer>
    <div v-if="pendingVisible && editSession" class="pending-bar" role="region" :aria-label="t('grid.commit.region')">
      <span class="badge warn">{{ t("grid.commit.pendingCount", { n: editSession.pendingCount }) }}</span>
      <button class="btn sm" type="button" @click="openCommit">{{ t("grid.commit.view") }}</button>
      <button
        class="btn sm danger"
        type="button"
        :data-armed="discardArmed ? 'true' : undefined"
        :title="discardArmed ? t('grid.commit.discardConfirmTitle') : t('grid.commit.discardTitle')"
        @click="discardAll"
      >{{ discardArmed ? t("grid.commit.discardConfirm") : t("grid.commit.discard") }}</button>
      <button class="btn sm primary" type="button" :disabled="!editSession.pendingCount" @click="openCommit">{{ t("grid.commit.submit") }}</button>
      <p v-for="failure in commitFailures" :key="failure.id" class="commit-failure" role="alert">{{ failure.message }}</p>
    </div>
    </div>
    <section v-if="selected" class="inspector" :aria-label="t('grid.value.queryViewer')">
      <div class="inspector-meta">
        <strong>{{ selected.column }}</strong>
        <span class="col-type">{{ typeCaption(inspectorColumn) }}</span>
        <span>{{ selected.value === null ? 'NULL' : selected.value === '' ? t('grid.value.emptyTitle') : t('grid.value.value') }}</span>
        <span class="grow"></span>
        <button class="btn sm" type="button" @click="selected = null">{{ t("common.close") }}</button>
      </div>
      <textarea readonly :value="showCell(selected.value, { column: inspectorColumn, previewBytes: previewBytes(), opened: revealed.has(`${selected.row}:${selected.column}`), resolved: true }).body" :aria-label="t('grid.value.columnValue', { column: selected.column })"></textarea>
      <div class="inspector-actions">
        <button v-if="showCell(selected.value, { column: inspectorColumn, previewBytes: previewBytes(), resolved: true }).kind === 'large'" class="btn sm" type="button" @click="revealSelected">{{ t("grid.value.readFull") }}</button>
        <button v-if="editSession" class="btn sm" type="button" :disabled="!editSession.canUpdate(editSession.rowById(selected.row)) || !editSession.canEditColumn(selected.column)" @click="beginEdit(selected.row, selected.column)">{{ t("common.edit") }}</button>
        <button class="btn sm" type="button" @click="copyValue">{{ t("grid.value.copyValue") }}</button>
        <button class="btn sm" type="button" @click="copySqlValueSelected">{{ t("grid.value.copySqlValue") }}</button>
        <span role="status">{{ copyMessage }}</span>
      </div>
    </section>
    <div v-if="menu" class="cell-menu" role="menu" :style="{ left: `${menu.x}px`, top: `${menu.y}px` }" @contextmenu.prevent>
      <button type="button" role="menuitem" @click="copySelection(); menu = null"><Icon name="copy" />{{ t("grid.menu.copy") }}</button>
      <button type="button" role="menuitem" @click="copySqlValueSelected(); menu = null"><Icon name="code" />{{ t("grid.value.copySqlValue") }}</button>
      <button type="button" role="menuitem" @click="editFromMenu"><Icon name="pencil" />{{ t("common.edit") }}</button>
      <button type="button" role="menuitem" @click="setNullFromMenu"><Icon name="x" />{{ t("grid.editor.setNull") }}</button>
      <p v-if="menuError" class="menu-error" role="alert">{{ menuError }}</p>
    </div>
    <div v-if="commitOpen && editSession" class="overlay" role="presentation">
      <div class="sheet" role="dialog" aria-modal="true" :aria-label="t('grid.commit.title')">
        <header><div><h2>{{ t("grid.commit.heading") }}</h2><p class="hint" data-confirm-target>{{ t("grid.commit.target", { conn: connectionName, object: objectName }) }} <span v-if="environmentLabel" class="env xs" :class="environmentTone !== 'none' ? `env-${environmentTone}` : undefined">{{ environmentLabel }}</span></p><p class="hint">{{ t("grid.commit.writeRule") }} {{ t("grid.resultEdit.insertBlocked") }}</p></div></header>
        <div class="body">
          <p v-if="commitError" class="sheet-error" role="alert" data-commit-error><svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="ERROR_ICON" /></svg>{{ commitError }}</p>
          <ol class="sql-list"><li v-for="step in editSession.preview()" :key="step.id"><strong>{{ step.label }}</strong><pre>{{ step.sql }}</pre></li></ol>
        </div>
        <footer>
          <button class="btn" type="button" @click="commitOpen = false">{{ t("common.cancel") }}</button>
          <span class="grow"></span>
          <button class="btn primary" type="button" @click="commitChangesNow()">{{ t("grid.commit.submitCount", { n: editSession.pendingCount }) }}</button>
        </footer>
      </div>
    </div>
    <ExportDialog
      :open="exportOpen"
      origin="query"
      :statement="statement"
      :engine="engine"
      :truncated="truncated"
      file-stem="query"
      :loaded-count="rows.length"
      :pending="editSession?.pendingCount ?? 0"
      :columns="columns.map((column) => column.name)"
      :preview-rows="exportRows().slice(0, 4)"
      :insert-target="insertTarget"
      :insert-reason="insertTarget ? '' : reason"
      @close="exportOpen = false"
      @start="startExport"
    />
  </div>
</template>
