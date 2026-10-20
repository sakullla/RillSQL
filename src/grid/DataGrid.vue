<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, reactive, ref, watch, type ComponentPublicInstance } from "vue";
import ExportDialog from "../export/ExportDialog.vue";
import ImportDialog from "../export/ImportDialog.vue";
import { planExport, type ExportScope } from "../export/plan";
import { runExportTask, runRemoteExport } from "../export/tasks";
import type { CsvOptions } from "../export/csv";
import type { ExportFormat } from "../export/write";
import type { InsertTarget } from "../export/insert";
import { catalogConnections } from "../catalog/store";
import { registerPending } from "../shell/close-guard";
import { loadSettings } from "../shell/settings";
import { describeStructure } from "../structure/api";
import type { StructureForeignKey } from "../structure/types";
import { rectangleSelection, selectionTsv, type CellPoint } from "./clipboard";
import { copyCellText, copySqlValue, needsFullRead, showCell, type NullCopy } from "./display";
import { referenceJump, referenceQuery } from "./reference";
import { isPreviewEnvelope, rejectWrite, typeCaption } from "./typeset";
import "./grid.css";
import { useRowWindow } from "./virtual";
import { LiveTable } from "./live";
import type { TableTarget } from "./target";
import { FILTER_OPS, opLabel, opNeedsValue, type Cell, type Filter, type FilterOp } from "./types";

const props = defineProps<{ tabId: string; draft: string; pageSize?: number; nullCopy?: NullCopy; target?: TableTarget | null }>();
const emit = defineEmits<{
  "jump-reference": [sql: string, ref: string];
}>();
const session = props.target ? reactive(new LiveTable(props.target, props.pageSize ?? loadSettings().pageSize)) : null;
if (session) void session.open();
/** 设置页的预览上限对网格立即生效，避免“改了没反应”。 */
function previewBytes(): number {
  return Math.max(1, loadSettings().previewKB) * 1024;
}
let releasePending: (() => void) | null = null;
function releaseCellPending(): void {
  releasePending?.();
  releasePending = null;
}
/** 只登记未提交的单元格修改；未保存 SQL 不进入关闭确认。 */
watch(
  () => [props.tabId, (session?.pendingCount ?? 0) > 0] as const,
  ([tabId, pending]) => {
    releaseCellPending();
    if (!pending) return;
    releasePending = registerPending(tabId, () => props.tabId === tabId && (session?.pendingCount ?? 0) > 0);
  },
  { immediate: true },
);
onBeforeUnmount(() => {
  releaseCellPending();
  stopDrag();
  resizeUp?.();
  clearTimeout(discardTimer);
});
const rootEl = ref<HTMLElement | null>(null);
const filterColumn = ref("");
watch(
  () => session?.store.schema.columns[0]?.name ?? "",
  (name) => {
    if (name && !filterColumn.value) filterColumn.value = name;
  },
  { immediate: true },
);
const filterOp = ref<FilterOp>("eq");
const filterValue = ref("");
const filterSecond = ref("");
const filterValues = ref("");
const filterError = ref("");
const exportOpen = ref(false);
const importOpen = ref(false);
const exportMessage = ref("");
const importMessage = ref("");
const commitOpen = ref(false);
const commitError = ref("");
const discardArmed = ref(false);
let discardTimer: ReturnType<typeof setTimeout> | undefined;
const editing = ref<{ id: string; column: string; draft: string; isNull: boolean } | null>(null);
const editError = ref("");
const copyMessage = ref("");
const receipt = ref<string | null>(null);
const revealed = reactive(new Set<string>());
const widths = reactive<Record<string, number>>({});
const anchor = ref<CellPoint | null>(null);
const focusCellPoint = ref<CellPoint | null>(null);
const dragging = ref(false);
const menu = ref<{ x: number; y: number; row: string; column: string } | null>(null);
const menuError = ref("");
/** 单元格菜单首次打开时读一次表结构，供外键值跳转；只在成功后缓存，失败允许下次再试。 */
const referenceKeys = ref<StructureForeignKey[] | null>(null);
let referenceLoad: Promise<void> | null = null;
let editToken = 0;
let resizeMove: ((event: PointerEvent) => void) | null = null;
let resizeUp: (() => void) | null = null;
watch(() => props.pageSize, size => { if (session && size) session.pageSize = size; });

const columns = computed(() => session?.store.schema.columns ?? []);
function failureText(id: string, kind: "insert" | "data"): string {
  if (!session) return "";
  if (kind === "insert") return session.failures.get(id) ?? "";
  return [session.failures.get(`u:${id}`), session.failures.get(`d:${id}`)].filter((message): message is string => !!message).join("\n");
}

const displayRows = computed(() => {
  if (!session) return [];
  const inserts = session.inserts.map((insert) => ({
    id: insert.id,
    kind: "insert" as const,
    located: false,
    deleted: false,
    failed: session.failures.has(insert.id),
    failure: failureText(insert.id, "insert"),
  }));
  const data = session.loaded
    .filter((row) => !session.inserts.some((insert) => insert.id === row.id))
    .map((row) => ({
      id: row.id,
      kind: "data" as const,
      located: row.located,
      deleted: session.deletes.includes(row.id),
      failed: session.failures.has(`u:${row.id}`) || session.failures.has(`d:${row.id}`),
      failure: failureText(row.id, "data"),
    }));
  return [...inserts, ...data];
});
const commitFailures = computed(() => (session ? Array.from(session.failures, ([id, message]) => ({ id, message })) : []));
const pendingVisible = computed(() => (session ? session.pendingCount > 0 || session.failures.size > 0 : false));
const { viewport, range, onScroll, reveal } = useRowWindow(computed(() => displayRows.value.length), false);
function setViewport(el: Element | ComponentPublicInstance | null): void {
  viewport.value = el instanceof HTMLElement ? el : null;
}
watch(
  () => range.value.end,
  (end) => {
    if (!session || !session.truncated || session.error || session.paging) return;
    if (displayRows.value.length > 0 && end >= displayRows.value.length) void session.loadMore();
  },
);
const visibleRows = computed(() => displayRows.value.slice(range.value.start, range.value.end).map(row => ({ ...row, cells: columns.value.map(column => shown(row.id, column.name)) })));
const padTop = computed(() => range.value.top);
const padBottom = computed(() => range.value.bottom);
const loading = computed(() => !!session && !session.ready && !session.error);
const showEmptyTable = computed(() => !!session && session.ready && !session.error && session.filters.length === 0 && displayRows.value.length === 0);
const showNoMatch = computed(() => !!session && session.ready && !session.error && session.filters.length > 0 && displayRows.value.length === 0);
const boundConnection = computed(() => {
  const id = session?.target.connectionId;
  if (!id) return null;
  return catalogConnections().find((item) => item.id === id) ?? null;
});
const connectionName = computed(() => boundConnection.value?.name.trim() || session?.target.connectionId || "");
const environmentLabel = computed(() => boundConnection.value?.envLabel.trim() ?? "");
const environmentTone = computed(() => boundConnection.value?.envTone ?? "none");
const objectName = computed(() => {
  if (!session) return "";
  const schemaName = session.store.schema.schema || session.target.schema;
  const name = session.store.schema.name || session.target.table;
  return schemaName ? `${schemaName}.${name}` : name;
});
const retrying = ref(false);

watch(
  () => session?.pendingCount ?? 0,
  (count) => {
    if (count > 0) receipt.value = null;
  },
);

const selectedColumn = computed(() => columns.value.find((column) => column.name === session?.selectedColumn) ?? null);
const selectedRaw = computed(() => (session?.selectedId && session.selectedColumn ? session.cellValue(session.selectedId, session.selectedColumn) : undefined));
const selectedShown = computed(() => showCell(selectedRaw.value, { column: selectedColumn.value ?? undefined, previewBytes: previewBytes(), opened: isRevealed(), resolved: session?.selectedId ? session.rowById(session.selectedId)?.resolved === true : false }));
const selectedPoints = computed(() => (anchor.value && focusCellPoint.value ? rectangleSelection(displayRows.value, columns.value, anchor.value, focusCellPoint.value) : []));
/** 选中区即时统计（对标 DBeaver 的 Calc 面板）：NULL、空字符串、预览信封和 Number() 解析不了的格子不进数值统计。 */
const calcStats = computed(() => {
  if (!session) return null;
  const points = selectedPoints.value;
  if (!points.length) return null;
  let numeric = 0;
  let sum = 0;
  let min = Number.POSITIVE_INFINITY;
  let max = Number.NEGATIVE_INFINITY;
  for (const point of points) {
    const value = session.cellValue(point.row, point.column);
    if (value === null || value === undefined || value === "") continue;
    const meta = columns.value.find((item) => item.name === point.column);
    if (isPreviewEnvelope(value, meta?.typeName)) continue;
    const parsed = Number(value);
    if (!Number.isFinite(parsed)) continue;
    numeric += 1;
    sum += parsed;
    min = Math.min(min, parsed);
    max = Math.max(max, parsed);
  }
  if (numeric === 0) return { cells: points.length, numeric, sum: null, avg: null, min: null, max: null };
  return { cells: points.length, numeric, sum: statText(sum), avg: statText(sum / numeric), min: statText(min), max: statText(max) };
});
const previewRows = computed(() => session?.loaded.slice(0, 4).map((row) => ({ ...row.cells })) ?? []);
const noKeyHint = computed(() => {
  if (!session) return "";
  if (session.store.schema.kind === "view") return "视图是只读的，不能新增、修改或删除数据。";
  if (!session.store.schema.key.length) return "这张表没有主键，无法唯一确定已有行，所以不能修改或删除它们。";
  return "";
});

async function applyFilter() {
  if (!session) return;
  filterError.value = "";
  const filter: Filter = { column: filterColumn.value, op: filterOp.value, value: filterValue.value };
  if (filterOp.value === "between") filter.second = filterSecond.value;
  if (filterOp.value === "in") filter.values = filterValues.value.split(",").map((item) => item.trim()).filter(Boolean);
  const error = await session.addFilter(filter);
  if (error && !session.error) filterError.value = error;
  else if (!error) {
    filterValue.value = "";
    filterSecond.value = "";
    filterValues.value = "";
  }
}

async function clearFilters() {
  if (!session) return;
  filterError.value = "";
  await session.replaceFilters([]);
}

async function retryRead() {
  if (!session || retrying.value) return;
  retrying.value = true;
  try {
    await session.open();
  } finally {
    retrying.value = false;
  }
}

function cellSelector(id: string, column: string): string {
  return `[data-cell="${CSS.escape(id)}:${CSS.escape(column)}"]`;
}

function focusCell(id: string, column: string) {
  const index = displayRows.value.findIndex(row => row.id === id);
  if (index >= 0) reveal(index);
  void nextTick(() => {
    (rootEl.value?.querySelector(cellSelector(id, column)) as HTMLElement | null)?.focus();
  });
}

function currentCellIndex(): number {
  if (!session?.selectedId || !session.selectedColumn) return -1;
  const rowIndex = displayRows.value.findIndex((row) => row.id === session!.selectedId);
  const columnIndex = columns.value.findIndex((column) => column.name === session!.selectedColumn);
  if (rowIndex < 0 || columnIndex < 0) return -1;
  return rowIndex * columns.value.length + columnIndex;
}

/** Tab / Shift+Tab：按桌面习惯在单元格间移动选中，越过边界时循环。 */
function moveSelection(delta: number) {
  if (!session) return;
  const rows = displayRows.value;
  const cols = columns.value;
  if (!rows.length || !cols.length) return;
  const total = rows.length * cols.length;
  let index = currentCellIndex();
  if (index < 0) index = delta > 0 ? -1 : 0;
  index = (index + delta + total) % total;
  const row = rows[Math.floor(index / cols.length)]!;
  const column = cols[index % cols.length]!;
  anchor.value = { row: row.id, column: column.name };
  focusCellPoint.value = { row: row.id, column: column.name };
  session.select(row.id, column.name);
  focusCell(row.id, column.name);
}

/** Shift+方向键：保留起点，扩展矩形选择。 */
function extendSelection(rowStep: number, columnStep: number) {
  if (!session) return;
  const rows = displayRows.value;
  const cols = columns.value;
  const current = focusCellPoint.value;
  if (!rows.length || !cols.length || !current) return;
  const rowIndex = rows.findIndex((row) => row.id === current.row);
  const columnIndex = cols.findIndex((column) => column.name === current.column);
  if (rowIndex < 0 || columnIndex < 0) return;
  const nextRow = rows[Math.min(rows.length - 1, Math.max(0, rowIndex + rowStep))]!;
  const nextColumn = cols[Math.min(cols.length - 1, Math.max(0, columnIndex + columnStep))]!;
  anchor.value ??= { row: current.row, column: current.column };
  focusCellPoint.value = { row: nextRow.id, column: nextColumn.name };
  session.select(nextRow.id, nextColumn.name);
  focusCell(nextRow.id, nextColumn.name);
}

/** Ctrl+A：选中当前已读取的全部单元格，复制与统计都按矩形走。 */
function selectAllCells() {
  if (!session) return;
  const rows = displayRows.value;
  const cols = columns.value;
  if (!rows.length || !cols.length) return;
  anchor.value = { row: rows[0]!.id, column: cols[0]!.name };
  focusCellPoint.value = { row: rows[rows.length - 1]!.id, column: cols[cols.length - 1]!.name };
  session.select(rows[0]!.id, cols[0]!.name);
}

async function beginEdit(id: string, column: string) {
  const token = ++editToken;
  const row = displayRows.value.find((item) => item.id === id);
  if (!row || !session) return;
  const meta = columns.value.find((item) => item.name === column);
  if (rejectWrite(meta, null)) return;
  if (row.kind === "data" && !session.canUpdate(session.rowById(id))) return;
  if (needsFullRead(session.cellValue(id, column), meta)) {
    const error = await ensureFull(id, column);
    if (token !== editToken) return;
    if (error) {
      editError.value = error;
      if (menu.value?.row === id && menu.value.column === column) menuError.value = error;
      return;
    }
  }
  if (token !== editToken) return;
  const current = session.cellValue(id, column);
  const resolved = session.rowById(id)?.resolved === true;
  if (typeof current === "string" && isPreviewEnvelope(current, meta?.typeName) && !resolved) {
    const message = "预览内容不能写回";
    editError.value = message;
    if (menu.value?.row === id && menu.value.column === column) menuError.value = message;
    return;
  }
  editError.value = "";
  editing.value = { id, column, draft: current ?? "", isNull: current === null };
  session.select(id, column);
}

function editSelected() {
  if (!session?.selectedId || !session.selectedColumn) return;
  beginEdit(session.selectedId, session.selectedColumn);
}

function saveEdit() {
  const edit = editing.value;
  if (!edit || !session) return;
  const value: Cell | undefined = edit.isNull ? null : edit.draft;
  const error = session.inserts.some((insert) => insert.id === edit.id)
    ? session.stageInsert(edit.id, edit.column, value)
    : session.stageUpdate(edit.id, edit.column, value);
  if (error) {
    editError.value = error;
    return;
  }
  editError.value = "";
  const { id, column } = edit;
  editing.value = null;
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

/** 未编辑时：Enter 开始编辑，Tab/方向键移动，快捷键与帮助里写的一致。 */
function onGridKeydown(event: KeyboardEvent) {
  if (editing.value) return;
  const mod = event.ctrlKey || event.metaKey;
  const key = event.key.toLowerCase();
  if (mod && event.shiftKey && key === "c") {
    event.preventDefault();
    void copySelected(true);
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
    if (session?.pendingCount) {
      commitError.value = "";
      commitOpen.value = true;
    }
    return;
  }
  if (event.key === "Delete") {
    event.preventDefault();
    const error = session?.deleteSelected();
    if (error) {
      editError.value = error;
    } else {
      editError.value = "";
      receipt.value = null;
    }
    return;
  }
  if (["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight"].includes(event.key)) {
    event.preventDefault();
    const rowStep = event.key === "ArrowUp" ? -1 : event.key === "ArrowDown" ? 1 : 0;
    const columnStep = event.key === "ArrowLeft" ? -1 : event.key === "ArrowRight" ? 1 : 0;
    if (event.shiftKey) extendSelection(rowStep, columnStep);
    else moveSelection(rowStep * columns.value.length + columnStep);
  } else if (event.key === "Enter" || event.key === "F2") {
    if (session?.selectedId && session.selectedColumn) {
      event.preventDefault();
      beginEdit(session.selectedId, session.selectedColumn);
    }
  } else if (event.key === "Tab") {
    event.preventDefault();
    moveSelection(event.shiftKey ? -1 : 1);
  } else if (event.key === "Escape") {
    (document.activeElement as HTMLElement | null)?.blur();
  }
}

function discardAll() {
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

async function commitChangesNow() {
  if (!session) return;
  commitError.value = "";
  const report = await session.commit();
  if (!report) {
    // 提交没有返回报告时，面板不能关掉：否则看起来像成功了。
    commitError.value = session.commitError || "提交没有完成。未成功的更改仍保留在待提交列表中。";
    return;
  }
  const saved = report.applied.length;
  if (report.failed) {
    commitError.value = `已保存 ${saved} 处，1 处失败。失败的更改仍保留在待提交列表中，原因已标出。`;
    return;
  }
  commitOpen.value = false;
  receipt.value = `${saved} 处已保存。`;
}

function openCommit() {
  commitError.value = "";
  commitOpen.value = true;
}

function shown(id: string, column: string) {
  const value = session?.cellValue(id, column);
  const meta = columns.value.find((item) => item.name === column);
  return showCell(value, { column: meta, previewBytes: previewBytes(), opened: revealed.has(`${id}:${column}`), resolved: session?.rowById(id)?.resolved === true });
}

function isPicked(row: string, column: string): boolean {
  return selectedPoints.value.some((point) => point.row === row && point.column === column);
}

/** 统计数值最多留 4 位小数并去掉尾零，避免浮点噪音。 */
function statText(value: number): string {
  return String(Number(value.toFixed(4)));
}

function columnStyle(name: string): Record<string, string> | undefined {
  const width = widths[name];
  if (!width) return undefined;
  return { width: `${width}px`, minWidth: `${width}px`, maxWidth: `${width}px` };
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
  if (event.button !== 0 || !session) return;
  anchor.value = { row, column };
  focusCellPoint.value = { row, column };
  dragging.value = true;
  menu.value = null;
  session.select(row, column);
  window.addEventListener("pointerup", stopDrag);
}

function onCellEnter(row: string, column: string) {
  if (!dragging.value) return;
  focusCellPoint.value = { row, column };
  session?.select(row, column);
}

function onCellMenu(row: string, column: string, event: MouseEvent) {
  event.preventDefault();
  if (!isPicked(row, column)) {
    anchor.value = { row, column };
    focusCellPoint.value = { row, column };
  }
  session?.select(row, column);
  menuError.value = "";
  menu.value = { x: event.clientX, y: event.clientY, row, column };
  loadReferenceKeys();
}

function loadReferenceKeys(): void {
  if (referenceKeys.value || referenceLoad || !props.target) return;
  referenceLoad = describeStructure(props.target).then(
    (described) => {
      referenceKeys.value = described.foreignKeyList;
    },
    () => {
      referenceLoad = null;
      if (menu.value) menuError.value = "读取表结构失败，暂时不能跳转引用行。";
    },
  );
}

/** 右键的格子命中可跳转的外键时给出菜单项：点击后在引用表上执行等值查询。 */
const referenceMenuItem = computed<{ sql: string; label: string } | null>(() => {
  const target = menu.value;
  if (!target || !session || !referenceKeys.value) return null;
  const rowCells: Record<string, Cell | undefined> = {};
  for (const column of columns.value) rowCells[column.name] = session.cellValue(target.row, column.name);
  const jump = referenceJump(referenceKeys.value, target.column, rowCells);
  if (!jump) return null;
  const label = jump.foreignKey.refSchema ? `${jump.foreignKey.refSchema}.${jump.foreignKey.refTable}` : jump.foreignKey.refTable;
  return { sql: referenceQuery(session.store.schema.engine, jump.foreignKey, jump.values), label };
});

function jumpReferenceFromMenu(): void {
  const item = referenceMenuItem.value;
  if (!item) return;
  menu.value = null;
  emit("jump-reference", item.sql, item.label);
}

async function ensureFull(rowId: string, column: string): Promise<string | null> {
  if (!session) return null;
  const meta = columns.value.find((item) => item.name === column);
  if (!needsFullRead(session.cellValue(rowId, column), meta)) return null;
  if (!session.rowById(rowId)?.located) return null;
  return session.readLocated(rowId);
}

async function copySelection() {
  if (!session) return;
  const points = selectedPoints.value.length
    ? selectedPoints.value
    : session.selectedId && session.selectedColumn
      ? [{ row: session.selectedId, column: session.selectedColumn }]
      : [];
  if (!points.length) return;
  for (const point of points) {
    const error = await ensureFull(point.row, point.column);
    if (error) {
      copyMessage.value = error;
      return;
    }
  }
  const text = selectionTsv(points, (point) => copyCellText(session.cellValue(point.row, point.column), props.nullCopy ?? loadSettings().nullCopy, columns.value.find((item) => item.name === point.column), session.rowById(point.row)?.resolved === true));
  try {
    await navigator.clipboard.writeText(text);
    copyMessage.value = "已复制";
  } catch {
    copyMessage.value = text;
  }
}

function editFromMenu() {
  const target = menu.value;
  if (!target || !session) return;
  const meta = columns.value.find((item) => item.name === target.column);
  const rejected = rejectWrite(meta, null);
  if (rejected) {
    menuError.value = rejected;
    return;
  }
  const row = displayRows.value.find((item) => item.id === target.row);
  if (row?.kind === "data" && !session.canUpdate(session.rowById(target.row))) {
    menuError.value = "只读，不能修改";
    return;
  }
  menu.value = null;
  beginEdit(target.row, target.column);
}

function setNullFromMenu() {
  const target = menu.value;
  if (!target || !session) return;
  const meta = columns.value.find((item) => item.name === target.column);
  const rejected = rejectWrite(meta, null);
  if (rejected) {
    menuError.value = rejected;
    return;
  }
  if (meta && !meta.nullable && !session.inserts.some((insert) => insert.id === target.row)) {
    menuError.value = "这一列不能是 NULL";
    return;
  }
  const error = session.inserts.some((insert) => insert.id === target.row)
    ? session.stageInsert(target.row, target.column, null)
    : session.stageUpdate(target.row, target.column, null);
  if (error) menuError.value = error;
  else {
    menu.value = null;
    receipt.value = null;
  }
}

async function copySelected(asSql: boolean) {
  if (!session?.selectedColumn || !session.selectedId) return;
  const error = await ensureFull(session.selectedId, session.selectedColumn);
  if (error) {
    copyMessage.value = error;
    return;
  }
  const column = selectedColumn.value ?? undefined;
  const resolved = session.rowById(session.selectedId)?.resolved === true;
  const text = asSql ? copySqlValue(selectedRaw.value, column, session.store.schema.engine) : copyCellText(selectedRaw.value, props.nullCopy ?? loadSettings().nullCopy, column, resolved);
  try {
    await navigator.clipboard.writeText(text);
    copyMessage.value = "已复制";
  } catch {
    copyMessage.value = text || "复制失败，请在值查看器中选中文本复制。";
  }
}

function revealKey(): string {
  if (!session) return "";
  return `${session.selectedId}:${session.selectedColumn}`;
}

function isRevealed(): boolean {
  return revealed.has(revealKey());
}

async function fullValue() {
  if (!session || !session.selectedId || !session.selectedColumn) return;
  const error = await ensureFull(session.selectedId, session.selectedColumn);
  if (error) {
    copyMessage.value = error;
    return;
  }
  revealed.add(revealKey());
}

function editDisabled(): boolean {
  if (!session || !session.selectedId || !session.selectedColumn) return true;
  return !session.canDeleteRow(session.selectedId);
}

const importTables = computed(() => {
  if (!session?.target) return [];
  const target = session.target;
  const found: { schema: string; table: string }[] = [];
  const connection = catalogConnections().find((item) => item.id === target.connectionId);
  for (const database of connection?.databases ?? []) {
    if (target.database && database.name !== target.database) continue;
    for (const schema of database.schemas) {
      for (const object of schema.objects) {
        if (object.stale || object.kind !== "table") continue;
        found.push({ schema: schema.name, table: object.name });
      }
    }
  }
  return found;
});

const insertTarget = computed<InsertTarget | null>(() => {
  if (!session) return null;
  return {
    engine: session.store.schema.engine,
    schema: session.store.schema.schema,
    name: session.store.schema.name,
    columns: session.store.schema.columns,
  };
});

function onImportDone(message: string): void {
  importMessage.value = message;
  importOpen.value = false;
  if (!session) return;
  void session.readWindow(0, Math.max(session.pageSize, session.loaded.length || session.pageSize), true);
}

async function startExport(payload: { scope: ExportScope; acknowledged: boolean; path: string; csv: CsvOptions; format: ExportFormat }) {
  if (!session?.target) return;
  const plan = planExport({
    origin: "table",
    scope: payload.scope,
    truncated: session.truncated,
    fileName: payload.path,
    engine: session.target.engine,
  });
  exportOpen.value = false;
  if (payload.scope === "loaded") {
    exportMessage.value = payload.format === "csv" ? "正在保存 CSV…" : "正在保存已读取的行…";
    const blocked = await session.expandPreviews();
    if (blocked) {
      exportMessage.value = blocked;
      return;
    }
    const columns = session.store.schema.columns.map((column) => column.name);
    void runExportTask({
      plan,
      acknowledged: payload.acknowledged,
      path: plan.fileName,
      columns,
      rows: session.loaded.map((row) => ({ ...row.cells })),
      csv: payload.csv,
      format: payload.format,
      insert: insertTarget.value ?? undefined,
    }).then(({ result }) => {
      exportMessage.value = result.message;
    });
    return;
  }
  exportMessage.value = "正在从已打开的连接导出全部匹配行。";
  const { done } = runRemoteExport({
    plan,
    acknowledged: payload.acknowledged,
    path: plan.fileName,
    csv: payload.csv,
    mode: "table-full",
    connectionId: session.target.connectionId,
    database: session.target.database,
    schema: session.target.schema,
    table: session.target.table,
    engine: session.target.engine,
    filters: session.filters,
    sort: session.sort,
    limit: 0,
    truncated: session.truncated,
    statement: "",
    format: payload.format,
  });
  void done.then((task) => {
    exportMessage.value = task.message;
  });
}
</script>

<template>
  <div ref="rootEl" class="data-grid" aria-label="数据表格" :data-tab-id="tabId" :data-draft="draft" :data-loaded="session ? session.loaded.length : 0" :data-truncated="session ? session.truncated : false">
    <p v-if="!session" class="grid-alert" role="alert">没有绑定已打开的表，不能用示例数据代替。</p>
    <template v-else>
    <div class="grid-toolbar">
      <span v-if="environmentLabel" class="env xs" data-env :class="environmentTone !== 'none' ? `env-${environmentTone}` : undefined">{{ environmentLabel }}</span>
      <form class="filter-form" @submit.prevent="applyFilter">
        <select v-model="filterColumn" aria-label="筛选列">
          <option v-for="column in columns" :key="column.name" :value="column.name">{{ column.name }}</option>
        </select>
        <select v-model="filterOp" aria-label="筛选条件">
          <option v-for="op in FILTER_OPS" :key="op.id" :value="op.id">{{ op.label }}</option>
        </select>
        <input v-if="opNeedsValue(filterOp)" v-model="filterValue" aria-label="筛选值" :placeholder="filterOp === 'between' ? '下界' : undefined" />
        <input v-if="filterOp === 'between'" v-model="filterSecond" aria-label="筛选第二个值" placeholder="上界" />
        <input v-if="filterOp === 'in'" v-model="filterValues" aria-label="筛选值列表" placeholder="多个值用逗号分隔，空段忽略" />
        <button class="btn sm" type="submit">筛选</button>
      </form>
      <span v-for="(filter, index) in session.filters" :key="`${filter.column}-${index}`" class="chip">
        <b>{{ filter.column }}</b> {{ opLabel(filter.op) }}
        <code v-if="filter.op === 'between'">{{ filter.value }} ~ {{ filter.second }}</code>
        <code v-else-if="filter.op === 'in'">{{ (filter.values ?? []).join(", ") }}</code>
        <code v-else-if="opNeedsValue(filter.op)">{{ filter.value }}</code>
        <button class="chip-x" type="button" :aria-label="`移除条件 ${index + 1}`" @click="session.removeFilter(index)">×</button>
      </span>
      <span v-if="session.sort" class="chip">{{ session.sort.column }} {{ session.sort.direction === "asc" ? "升序" : "降序" }}
        <button class="chip-x" type="button" aria-label="清除排序" @click="session.clearSort()">×</button>
      </span>
      <p class="toolbar-hint">{{ session.scopeLabel }}，不限于已读取的行。</p>
      <span class="grow"></span>
      <p v-if="noKeyHint" class="toolbar-hint key-hint">{{ noKeyHint }}</p>
      <button class="btn sm" type="button" :disabled="!session.canInsert" @click="session.addRow()">新增行</button>
      <button
        class="btn sm"
        type="button"
        :disabled="!session.canDeleteSelection()"
        :title="session.store.schema.key.length ? '删除选中的行' : '这张表没有主键，无法唯一确定已有行，不能删除'"
        @click="session.deleteSelected()"
      >删除行</button>
      <button class="btn sm" type="button" :disabled="session.store.schema.kind !== 'table'" @click="importOpen = true">导入</button>
      <button class="btn sm" type="button" @click="exportOpen = true">导出</button>
    </div>
    <details v-if="(session.filters.length || session.sort) && session.sql" class="disclosure grid-sql">
      <summary>查看这次读取在数据库里执行的 SQL</summary>
      <div class="disclosure-body"><pre class="sql-code">{{ session.sql }}</pre></div>
    </details>
    <p v-if="filterError" class="grid-alert" role="alert">{{ filterError }}</p>
    <p v-if="editError && !editing" class="grid-alert" role="alert">{{ editError }}</p>
    <p v-if="importMessage" class="hint export-message" role="status">{{ importMessage }}</p>
    <p v-if="exportMessage" class="hint export-message" role="status">{{ exportMessage }}</p>
    <p v-if="session.error" class="grid-alert" role="alert">读取失败：{{ session.error }}。这不是无匹配结果。筛选条件仍保留。 <button class="btn sm" type="button" :disabled="retrying" @click="retryRead">重试</button></p>
    <div :ref="setViewport" class="grid-scroll" @scroll.passive="onScroll" @keydown="onGridKeydown">
      <table class="grid-table">
        <thead>
          <tr>
            <th v-for="column in columns" :key="column.name" scope="col" :style="columnStyle(column.name)" :data-type="typeCaption(column)" :aria-sort="session.sort?.column === column.name ? (session.sort.direction === 'asc' ? 'ascending' : 'descending') : 'none'">
              <button class="th-btn" type="button" :aria-label="`按 ${column.name} 排序`" @click="session.cycleSort(column.name)"><span>{{ column.name }}</span><span class="col-type">{{ typeCaption(column) }}</span></button>
              <span class="col-resize" role="separator" :aria-label="`调整 ${column.name} 列宽`" @pointerdown.stop.prevent="startResize(column.name, $event)"></span>
            </th>
          </tr>
        </thead>
        <tbody>
          <tr v-if="padTop" class="virtual-spacer" aria-hidden="true"><td :colspan="columns.length" :style="{ height: `${padTop}px` }"></td></tr>
          <tr v-if="loading">
            <td class="grid-empty" :colspan="columns.length">正在读取表数据…</td>
          </tr>
          <tr v-for="row in visibleRows" :key="row.id" :class="{ 'row-insert': row.kind === 'insert', 'row-delete': row.deleted, 'row-failed': row.failed }" :data-row-id="row.id" :data-located="row.located" :title="row.failure || undefined">
            <td v-for="(column, columnIndex) in columns" :key="column.name" :style="columnStyle(column.name)">
              <div v-if="editing && editing.id === row.id && editing.column === column.name" class="editor">
                <input :ref="focusEditor" v-model="editing.draft" :aria-label="`编辑 ${column.name}`" :disabled="editing.isNull" @keydown="onEditorKeydown" />
                <button class="btn sm" type="button" :disabled="!column.nullable" @click="editing.isNull = true">设为 NULL</button>
                <button class="btn sm" type="button" @click="editing.isNull = false">空字符串</button>
                <button class="btn sm primary" type="button" @click="saveEdit">确定</button>
                <button class="btn sm ghost" type="button" @click="cancelEdit">取消</button>
                <p v-if="editError" class="menu-error" role="alert">{{ editError }}</p>
              </div>
              <button v-else class="cell-btn" :class="{ on: session.selectedId === row.id && session.selectedColumn === column.name, range: isPicked(row.id, column.name) }" :data-cell="`${row.id}:${column.name}`" type="button" @pointerdown="onCellDown(row.id, column.name, $event)" @pointerenter="onCellEnter(row.id, column.name)" @click="session.select(row.id, column.name)" @contextmenu="onCellMenu(row.id, column.name, $event)" @dblclick="beginEdit(row.id, column.name)">
                <span v-if="columnIndex === 0 && row.kind === 'insert'" class="row-flag">新增</span>
                <span v-if="columnIndex === 0 && row.deleted" class="row-flag del">将删除</span>
                <span :class="{ 'v-null': row.cells[columnIndex]?.kind === 'null', 'v-empty': row.cells[columnIndex]?.kind === 'empty', 'v-large': row.cells[columnIndex]?.kind === 'large' }">{{ row.cells[columnIndex]?.label }}</span>
              </button>
            </td>
          </tr>
          <tr v-if="padBottom" class="virtual-spacer" aria-hidden="true"><td :colspan="columns.length" :style="{ height: `${padBottom}px` }"></td></tr>
          <tr v-if="showEmptyTable" data-empty="table">
            <td class="grid-empty" :colspan="Math.max(columns.length, 1)">{{ session.store.schema.kind === "view" ? "这个视图没有行。" : "这张表没有行。" }}<button v-if="session.canInsert" class="btn sm" type="button" @click="session.addRow()">新增行</button></td>
          </tr>
          <tr v-else-if="showNoMatch" data-empty="filter">
            <td class="grid-empty" :colspan="Math.max(columns.length, 1)">没有匹配行。 <button class="btn sm" type="button" @click="clearFilters">清除筛选</button></td>
          </tr>
        </tbody>
      </table>
    </div>
    <footer class="grid-foot">
      <template v-if="session.truncated">
        <span>已读取前 <b>{{ session.loaded.length }}</b> 行，还有匹配的行没有读取。</span>
        <button class="btn sm" type="button" aria-label="继续读取" @click="session.loadMore()">继续读取</button>
      </template>
      <span v-else-if="session.loaded.length">已读取全部匹配行（{{ session.loaded.length }} 行）。</span>
      <span class="grow"></span>
      <button class="btn sm" type="button" @click="session.count()">统计匹配行数</button>
      <span v-if="session.counted !== null" role="status">共 {{ session.counted }} 行匹配</span>
      <span v-else class="muted">匹配行数未统计</span>
      <span v-if="calcStats" class="calc-stat" role="status">已选 {{ calcStats.cells }} 格 · 数值 {{ calcStats.numeric }} 个<template v-if="calcStats.numeric > 0"> · 总和 {{ calcStats.sum }} · 平均 {{ calcStats.avg }} · 最小 {{ calcStats.min }} · 最大 {{ calcStats.max }}</template></span>
    </footer>
    <div v-if="pendingVisible" class="pending-bar" role="region" aria-label="未保存的更改">
      <span class="badge warn">{{ session.pendingCount }} 处更改未保存</span>
      <button class="btn sm" type="button" @click="openCommit">查看</button>
      <button
        class="btn sm danger"
        type="button"
        :data-armed="discardArmed ? 'true' : undefined"
        :title="discardArmed ? '再次点击，确认放弃全部未保存的更改' : '放弃全部未保存的更改'"
        @click="discardAll"
      >{{ discardArmed ? "确认放弃" : "放弃" }}</button>
      <button class="btn sm primary" type="button" :disabled="!session.pendingCount" @click="openCommit">提交</button>
      <span v-if="receipt" class="receipt">{{ receipt }}</span>
      <p v-for="failure in commitFailures" :key="failure.id" class="commit-failure" role="alert">{{ failure.message }}</p>
    </div>
    <p v-else-if="receipt" class="commit-receipt" role="status">{{ receipt }}</p>
    <section v-if="session.selectedId" class="inspector" aria-label="值查看器">
      <div class="inspector-meta">
        <span>列 <strong>{{ session.selectedColumn }}</strong></span>
        <span>值 <em :class="selectedShown.kind === 'null' ? 'v-null' : selectedShown.kind === 'empty' ? 'v-empty' : ''">{{ selectedShown.kind === "null" ? "NULL" : selectedShown.kind === "empty" ? "空字符串（长度 0）" : selectedShown.kind === "large" ? "大字段，当前不是完整值" : selectedShown.title }}</em></span>
      </div>
      <textarea readonly :value="selectedShown.body" :aria-label="`${session.selectedColumn} 的值`"></textarea>
      <div class="inspector-actions">
        <button v-if="selectedShown.kind === 'large'" class="btn sm" type="button" @click="fullValue">读取完整值</button>
        <button class="btn sm" type="button" :disabled="editDisabled()" @click="editSelected">编辑</button>
        <button class="btn sm" type="button" @click="copySelected(false)">复制值</button>
        <button class="btn sm" type="button" @click="copySelected(true)">复制为 SQL 值</button>
        <span v-if="copyMessage" class="copy-message" data-copy>{{ copyMessage }}</span>
      </div>
      <p class="legend">
        <span><span class="v-null">NULL</span> 空值</span>
        <span><span class="v-empty">空字符串</span> 长度为 0 的文本</span>
        <span>灰字「大字段 · N 字节」只预览了开头，读取完整值可看全部</span>
      </p>
    </section>
    <div v-if="commitOpen" class="overlay" role="presentation" @keydown.esc="commitOpen = false">
      <div class="sheet" role="dialog" aria-modal="true" aria-label="待提交的更改">
        <header>
          <div>
            <h2>提交这些更改？</h2>
            <p class="hint" data-confirm-target>目标：{{ connectionName }} / {{ objectName }} <span v-if="environmentLabel" class="env xs" :class="environmentTone !== 'none' ? `env-${environmentTone}` : undefined">{{ environmentLabel }}</span></p>
            <p class="hint">将按顺序逐条写入数据库，遇到错误即停止；已写入的不会自动撤销。</p>
          </div>
        </header>
        <div class="body">
          <p v-if="commitError" class="sheet-error" role="alert" data-commit-error>{{ commitError }}</p>
          <ol class="sql-list">
            <li v-for="step in session.preview()" :key="step.id">
              <strong>{{ step.label }}</strong>
              <pre>{{ step.sql }}</pre>
            </li>
          </ol>
        </div>
        <footer>
          <button class="btn" type="button" @click="commitOpen = false">取消</button>
          <span class="grow"></span>
          <button class="btn primary" type="button" :disabled="!session.pendingCount" @click="commitChangesNow()">提交 {{ session.pendingCount }} 处更改</button>
        </footer>
      </div>
    </div>
    <ExportDialog
      :open="exportOpen"
      origin="table"
      :truncated="session.truncated"
      :file-stem="session.store.schema.name"
      :loaded-count="session.loaded.length"
      :pending="session.pendingCount"
      :columns="columns.map((column) => column.name)"
      :preview-rows="previewRows"
      :insert-target="insertTarget"
      @close="exportOpen = false"
      @start="startExport"
    />
    <ImportDialog
      :open="importOpen"
      :target="session.target"
      :schema="session.store.schema"
      :tables="importTables"
      @close="importOpen = false"
      @done="onImportDone"
    />
    <div v-if="menu" class="cell-menu" role="menu" :style="{ left: `${menu.x}px`, top: `${menu.y}px` }" @contextmenu.prevent>
      <button type="button" role="menuitem" @click="copySelection(); menu = null">复制</button>
      <button type="button" role="menuitem" @click="editFromMenu">编辑</button>
      <button type="button" role="menuitem" @click="setNullFromMenu">设为 NULL</button>
      <button v-if="referenceMenuItem" type="button" role="menuitem" data-action="jump-reference" @click="jumpReferenceFromMenu">查看引用行 → {{ referenceMenuItem.label }}</button>
      <p v-if="menuError" class="menu-error" role="alert">{{ menuError }}</p>
    </div>
    </template>
  </div>
</template>
