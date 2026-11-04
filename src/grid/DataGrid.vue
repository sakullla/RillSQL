<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, reactive, ref, watch, type ComponentPublicInstance } from "vue";
import { tr as t } from "../i18n";
import ExportDialog from "../export/ExportDialog.vue";
import ImportDialog from "../export/ImportDialog.vue";
import { planExport, type ExportScope } from "../export/plan";
import { runExportTask, runRemoteExport } from "../export/tasks";
import type { CsvOptions } from "../export/csv";
import type { ExportFormat } from "../export/write";
import type { InsertTarget } from "../export/insert";
import { catalogConnections } from "../catalog/store";
import { registerPending } from "../shell/close-guard";
import { useGridSettings } from "./settings";
import { describeStructure } from "../structure/api";
import type { StructureForeignKey } from "../structure/types";
import { rectangleSelection, selectionTsv, type CellPoint } from "./clipboard";
import { copyCellText, copySqlValue, needsFullRead, showCell, type NullCopy } from "./display";
import { referenceJump, referenceQuery } from "./reference";
import { columnTypeSuffix, isPreviewEnvelope, rejectWrite, typeCaption, valueFamily } from "./typeset";
import "./grid.css";
import Icon from "../shell/Icon.vue";
import { Listbox } from "../ui";
import { toast } from "../ui/toast";
import { useRowWindow } from "./virtual";
import { LiveTable } from "./live";
import type { TableTarget } from "./target";
import { FILTER_OPS, opLabel, opNeedsValue, type Cell, type Filter, type FilterOp } from "./types";

const props = defineProps<{ tabId: string; draft: string; pageSize?: number; nullCopy?: NullCopy; target?: TableTarget | null }>();
const emit = defineEmits<{
  "jump-reference": [sql: string, ref: string];
}>();
/* 错误卡语义图标：圆 + 叉，与对象树/历史/导出等处的统一 err 线稿一致 */
const ERROR_ICON = "M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18M15 9l-6 6M9 9l6 6";
const settings = useGridSettings();
const session = props.target ? reactive(new LiveTable(props.target, props.pageSize ?? settings.value.pageSize)) : null;
if (session) void session.open();
/** 设置页的预览上限对网格立即生效，避免“改了没反应”。 */
function previewBytes(): number {
  return Math.max(1, settings.value.previewKB) * 1024;
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
  clearTimeout(searchTimer);
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
const columnOptions = computed(() => columns.value.map((column) => ({ value: column.name, label: column.name })));
const opOptions = computed(() => FILTER_OPS.map((op) => ({ value: op.id, label: opLabel(op.id) })));
/** 值查看器只在右键「查看值」时打开：普通单击、键盘导航和拖选都不展开它。 */
const viewer = ref<CellPoint | null>(null);
/** 普通选择换了单元格就关掉已打开的查看器，而不是立即展示新值。 */
watch(
  () => [session?.selectedId, session?.selectedColumn] as const,
  ([id, column]) => {
    const target = viewer.value;
    if (!target) return;
    if (target.row !== id || target.column !== column) viewer.value = null;
  },
);
const filterValue = ref("");
const filterSecond = ref("");
const filterValues = ref("");
const filterError = ref("");
/** 整表关键字搜索（R3）：输入停顿或回车后重新读取，清空即恢复；筛选与排序可叠加。 */
const searchDraft = ref("");
let searchTimer: ReturnType<typeof setTimeout> | undefined;
watch(searchDraft, () => {
  clearTimeout(searchTimer);
  searchTimer = setTimeout(() => void applySearch(), 300);
});
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
/** 指针捕获目标：捕获期间 pointermove 的 target 会固定在起始格，命中只能按坐标算。 */
let dragCapture: { pointerId: number; el: Element } | null = null;
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
const showEmptyTable = computed(() => !!session && session.ready && !session.error && session.filters.length === 0 && !session.search && displayRows.value.length === 0);
const showNoMatch = computed(() => !!session && session.ready && !session.error && (session.filters.length > 0 || !!session.search) && displayRows.value.length === 0);
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

/** 查看器内容只跟显式的查看目标走，不随普通选择漂移。 */
const viewerKey = computed(() => (viewer.value ? `${viewer.value.row}:${viewer.value.column}` : ""));
const viewerColumn = computed(() => columns.value.find((column) => column.name === viewer.value?.column) ?? null);
const viewerRaw = computed(() => (viewer.value && session ? session.cellValue(viewer.value.row, viewer.value.column) : undefined));
const viewerShown = computed(() => showCell(viewerRaw.value, { column: viewerColumn.value ?? undefined, previewBytes: previewBytes(), opened: revealed.has(viewerKey.value), resolved: viewer.value && session ? session.rowById(viewer.value.row)?.resolved === true : false }));
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
  if (session.store.schema.kind === "view") return t("grid.errors.viewReadonly");
  if (!session.store.schema.key.length) return t("grid.errors.noKeyEdit");
  return "";
});
const noKeyBadge = computed(() => {
  if (!session || !noKeyHint.value) return "";
  return session.store.schema.kind === "view" ? t("grid.scope.readonlyBadge") : t("grid.scope.noKeyBadge");
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

/** 关键字与已生效值一致时不重读；中文输入法回车确认候选时不触发。 */
async function applySearch(): Promise<void> {
  if (!session) return;
  clearTimeout(searchTimer);
  const keyword = searchDraft.value.trim();
  if (keyword === (session.search ?? "")) return;
  await session.replaceSearch(keyword);
}

function onSearchEnter(event: KeyboardEvent): void {
  if (event.isComposing) return;
  event.preventDefault();
  void applySearch();
}

/** 空态里的“清除关键字”立即恢复，不等输入停顿。 */
function clearSearch(): void {
  searchDraft.value = "";
  void applySearch();
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
    const message = t("grid.errors.previewWriteback");
    editError.value = message;
    if (menu.value?.row === id && menu.value.column === column) menuError.value = message;
    return;
  }
  editError.value = "";
  editing.value = { id, column, draft: current ?? "", isNull: current === null };
  session.select(id, column);
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
    commitError.value = session.commitError || t("grid.commit.incomplete");
    return;
  }
  const saved = report.applied.length;
  if (report.failed) {
    commitError.value = t("grid.commit.savedOneFailed", { n: saved });
    return;
  }
  commitOpen.value = false;
  receipt.value = t("grid.commit.savedAll", { n: saved });
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

/** 从单元格元素读出坐标；行 id 可能含冒号，所以按 data-row/data-column 取。 */
function pointFromElement(el: Element | null): CellPoint | null {
  const hit = el?.closest(".cell-btn") ?? null;
  if (!hit || !rootEl.value?.contains(hit)) return null;
  const row = hit.getAttribute("data-row");
  const column = hit.getAttribute("data-column");
  if (!row || !column) return null;
  return { row, column };
}

/** 捕获期间只能用坐标命中：事件目标一直是起始格，按目标算会永远停在锚点。 */
function pointAt(x: number, y: number): CellPoint | null {
  if (typeof document.elementFromPoint !== "function") return null;
  return pointFromElement(document.elementFromPoint(x, y));
}

/** 指针落点命中：有捕获时按坐标，没有捕获（合成事件、旧实现）时退回事件目标。 */
function cellFromEvent(event: PointerEvent): CellPoint | null {
  if (dragCapture) {
    if (!Number.isFinite(event.clientX) || !Number.isFinite(event.clientY)) return null;
    return pointAt(event.clientX, event.clientY);
  }
  return pointFromElement(event.target instanceof Element ? event.target : null);
}

function samePoint(a: CellPoint | null, b: CellPoint | null): boolean {
  return !!a && !!b && a.row === b.row && a.column === b.column;
}

/** 拖动期间只移动可见终点：不重开查看器，也不逐格提交选中。 */
function onDragMove(event: PointerEvent) {
  if (!dragging.value) return;
  const hit = cellFromEvent(event);
  if (!hit || samePoint(hit, focusCellPoint.value)) return;
  focusCellPoint.value = hit;
}

/** 结束拖动并摘下全部捕获与监听；pointerup、pointercancel、window blur 共用。 */
function stopDrag() {
  dragging.value = false;
  releaseCapture();
  window.removeEventListener("pointermove", onDragMove);
  window.removeEventListener("pointerup", onDragEnd);
  window.removeEventListener("pointercancel", onDragCancel);
  window.removeEventListener("blur", onDragCancel);
}

/** 捕获指针，保证拖出窗口后仍能收到 move/up；合成事件拿不到捕获就退回事件目标。 */
function capturePointer(el: Element | null, pointerId: number | undefined): void {
  dragCapture = null;
  if (!el || pointerId === undefined || pointerId < 0) return;
  const target = el as Element & { setPointerCapture?: (id: number) => void };
  if (typeof target.setPointerCapture !== "function") return;
  try {
    target.setPointerCapture(pointerId);
    dragCapture = { pointerId, el };
  } catch {
    dragCapture = null;
  }
}

/** 释放捕获；指针已经结束时浏览器会自行收尾，这里的失败可以忽略。 */
function releaseCapture(): void {
  const current = dragCapture;
  dragCapture = null;
  if (!current) return;
  const target = current.el as Element & { releasePointerCapture?: (id: number) => void };
  if (typeof target.releasePointerCapture !== "function") return;
  try {
    target.releasePointerCapture(current.pointerId);
  } catch {
    // 指针已结束或元素已移除。
  }
}

function onDragEnd(event: PointerEvent) {
  if (!dragging.value) return;
  // 网格外释放：保留最后一个有效区域，不额外扩展，也不残留拖动状态。
  const hit = cellFromEvent(event);
  if (hit) focusCellPoint.value = hit;
  const last = focusCellPoint.value;
  stopDrag();
  if (last) session?.select(last.row, last.column);
}

function onDragCancel() {
  // 指针取消不编辑、不复制，只清理监听。
  stopDrag();
}

function onCellDown(row: string, column: string, event: PointerEvent) {
  if (event.button !== 0 || !session) return;
  // 上一次拖动没收到 up（例如在窗口外松开）时先收尾，避免残留监听叠加。
  stopDrag();
  anchor.value = { row, column };
  focusCellPoint.value = { row, column };
  dragging.value = true;
  menu.value = null;
  viewer.value = null;
  session.select(row, column);
  capturePointer((event.currentTarget as Element | null) ?? (event.target instanceof Element ? event.target : null), event.pointerId);
  window.addEventListener("pointermove", onDragMove);
  window.addEventListener("pointerup", onDragEnd);
  window.addEventListener("pointercancel", onDragCancel);
  // 窗口失焦收不到 pointerup：作为兜底结束拖动，不留下卡住的 dragging。
  window.addEventListener("blur", onDragCancel);
}

/** 捕获指针后浏览器会把 click 发到按下时的单元格：不能让它把当前单元格拉回锚点。 */
function onCellClick(row: string, column: string) {
  if (dragging.value) return;
  if (focusCellPoint.value && !samePoint(focusCellPoint.value, { row, column })) return;
  session?.select(row, column);
}

/** pointerenter 只是快速移动的兜底：同样只移动可见终点。 */
function onCellEnter(row: string, column: string) {
  if (!dragging.value) return;
  if (samePoint(focusCellPoint.value, { row, column })) return;
  focusCellPoint.value = { row, column };
}

/** 右键「按此值筛选」：走与工具栏相同的整表筛选；大字段预览不是完整值，不提供。 */
const menuValue = computed(() => (menu.value && session ? session.cellValue(menu.value.row, menu.value.column) : undefined));
const canFilterByValue = computed(() => {
  const value = menuValue.value;
  return value !== undefined && !(typeof value === "string" && isPreviewEnvelope(value));
});

async function filterByValue() {
  const current = menu.value;
  if (!session || !current) return;
  const value = menuValue.value;
  menu.value = null;
  const error = await session.addFilter(value === null ? { column: current.column, op: "null", value: "" } : { column: current.column, op: "eq", value: String(value) });
  filterError.value = error ?? "";
}

/** 右键「查看值」：以右键单元格为目标建立选择并显式打开查看器。 */
function viewFromMenu() {
  const target = menu.value;
  if (!target || !session) return;
  if (!isPicked(target.row, target.column)) {
    anchor.value = { row: target.row, column: target.column };
    focusCellPoint.value = { row: target.row, column: target.column };
  }
  session.select(target.row, target.column);
  viewer.value = { row: target.row, column: target.column };
  copyMessage.value = "";
  menu.value = null;
}

/** 键盘触发的 contextmenu 坐标是 0：用目标单元格的矩形定位，菜单不会跑到左上角。 */
function menuPosition(event: MouseEvent): { x: number; y: number } {
  const x = event.clientX;
  const y = event.clientY;
  if (Number.isFinite(x) && Number.isFinite(y) && (x !== 0 || y !== 0)) return { x, y };
  const el = event.currentTarget instanceof HTMLElement ? event.currentTarget : null;
  const rect = el?.getBoundingClientRect();
  if (!rect || (!rect.width && !rect.height)) return { x: Number.isFinite(x) ? x : 0, y: Number.isFinite(y) ? y : 0 };
  return { x: Math.round(rect.left + rect.width / 2), y: Math.round(rect.top + rect.height / 2) };
}

function onCellMenu(row: string, column: string, event: MouseEvent) {
  event.preventDefault();
  if (!isPicked(row, column)) {
    anchor.value = { row, column };
    focusCellPoint.value = { row, column };
  }
  session?.select(row, column);
  menuError.value = "";
  menu.value = { ...menuPosition(event), row, column };
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
      if (menu.value) menuError.value = t("grid.errors.schemaReadFailed");
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
  const text = selectionTsv(points, (point) => copyCellText(session.cellValue(point.row, point.column), props.nullCopy ?? settings.value.nullCopy, columns.value.find((item) => item.name === point.column), session.rowById(point.row)?.resolved === true));
  try {
    await navigator.clipboard.writeText(text);
    copyMessage.value = t("grid.value.copied");
    toast(t("grid.menu.copiedCells", { n: points.length }));
  } catch {
    copyMessage.value = text;
    toast(t("grid.errors.clipboardFailed"), "err");
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
    menuError.value = t("grid.errors.readonlyCell");
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
    menuError.value = t("grid.errors.columnNotNull");
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

/** 复制一格的原值或 SQL 字面量；查看器与快捷键只是目标来源不同。 */
async function copyCell(rowId: string, columnName: string, asSql: boolean) {
  if (!session) return;
  const error = await ensureFull(rowId, columnName);
  if (error) {
    copyMessage.value = error;
    toast(error, "err");
    return;
  }
  const column = columns.value.find((item) => item.name === columnName);
  const resolved = session.rowById(rowId)?.resolved === true;
  const raw = session.cellValue(rowId, columnName);
  const text = asSql ? copySqlValue(raw, column, session.store.schema.engine) : copyCellText(raw, props.nullCopy ?? settings.value.nullCopy, column, resolved);
  try {
    await navigator.clipboard.writeText(text);
    copyMessage.value = t("grid.value.copied");
    // 查看器默认关闭：单格复制（含复制 SQL 值）的成败都要有全局可见反馈。
    toast(asSql ? t("grid.value.copiedSqlValue") : t("grid.value.copied"));
  } catch {
    copyMessage.value = text || t("grid.errors.clipboardFailed");
    toast(t("grid.errors.clipboardFailed"), "err");
  }
}

function copySelected(asSql: boolean) {
  if (!session?.selectedId || !session.selectedColumn) return;
  void copyCell(session.selectedId, session.selectedColumn, asSql);
}

/** 查看器按钮始终复制本次明确查看的单元格。 */
function copyViewed(asSql: boolean) {
  const target = viewer.value;
  if (!target) return;
  void copyCell(target.row, target.column, asSql);
}

async function fullValue() {
  const target = viewer.value;
  if (!session || !target) return;
  const error = await ensureFull(target.row, target.column);
  if (error) {
    copyMessage.value = error;
    return;
  }
  revealed.add(viewerKey.value);
}

function editDisabled(): boolean {
  const target = viewer.value;
  if (!session || !target) return true;
  return !session.canDeleteRow(target.row);
}

/** 查看器里的“编辑”只作用于本次明确查看的单元格。 */
function editViewed() {
  const target = viewer.value;
  if (!target) return;
  void beginEdit(target.row, target.column);
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
    exportMessage.value = payload.format === "csv" ? t("grid.export.savingCsv") : t("grid.export.savingLoaded");
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
  exportMessage.value = t("grid.export.exportingAll");
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
    search: session.search ?? null,
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
  <div ref="rootEl" class="data-grid" :aria-label="t('grid.tableArea')" :data-tab-id="tabId" :data-draft="draft" :data-loaded="session ? session.loaded.length : 0" :data-truncated="session ? session.truncated : false">
    <p v-if="!session" class="grid-alert" role="alert"><svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="ERROR_ICON" /></svg>{{ t("grid.unbound") }}</p>
    <template v-else>
    <div class="grid-main">
    <div class="grid-toolbar">
      <slot name="leading" />
      <span v-if="environmentLabel" class="env xs" data-env :class="environmentTone !== 'none' ? `env-${environmentTone}` : undefined">{{ environmentLabel }}</span>
      <form class="filter-form" @submit.prevent="applyFilter">
        <div class="filter-group">
          <Listbox v-model="filterColumn" :options="columnOptions" filterable :aria-label="t('grid.filter.column')" />
          <Listbox :model-value="filterOp" :options="opOptions" :aria-label="t('grid.filter.condition')" @update:model-value="filterOp = $event as FilterOp" />
          <input v-if="opNeedsValue(filterOp)" v-model="filterValue" :aria-label="t('grid.filter.value')" :placeholder="filterOp === 'between' ? t('grid.filter.lowerBound') : undefined" />
          <input v-if="filterOp === 'between'" v-model="filterSecond" :aria-label="t('grid.filter.secondValue')" :placeholder="t('grid.filter.upperBound')" />
          <input v-if="filterOp === 'in'" v-model="filterValues" :aria-label="t('grid.filter.valueList')" :placeholder="t('grid.filter.valueListHint')" />
          <button class="btn sm" type="submit">{{ t("grid.filter.apply") }}</button>
        </div>
        <span v-for="(filter, index) in session.filters" :key="`${filter.column}-${index}`" class="chip">
          <b>{{ filter.column }}</b> {{ opLabel(filter.op) }}
          <code v-if="filter.op === 'between'">{{ filter.value }} ~ {{ filter.second }}</code>
          <code v-else-if="filter.op === 'in'">{{ (filter.values ?? []).join(", ") }}</code>
          <code v-else-if="opNeedsValue(filter.op)">{{ filter.value }}</code>
          <button class="chip-x" type="button" :aria-label="t('grid.filter.removeCondition', { n: index + 1 })" @click="session.removeFilter(index)">×</button>
        </span>
        <span v-if="session.sort" class="chip">{{ session.sort.column }} {{ session.sort.direction === "asc" ? t("grid.filter.asc") : t("grid.filter.desc") }}
          <button class="chip-x" type="button" :aria-label="t('grid.filter.clearSort')" @click="session.clearSort()">×</button>
        </span>
        <p class="sr">{{ session.scopeLabel }}{{ t("grid.filter.hintWholeTable") }}</p>
        <div class="filter-group filter-search">
          <input v-model="searchDraft" type="search" :aria-label="t('grid.filter.searchKeyword')" :placeholder="t('grid.filter.searchPlaceholder')" :title="`${session.scopeLabel}${t('grid.filter.hintWholeTable')}`" @keydown.enter="onSearchEnter" />
          <button class="btn sm" type="button" @click="applySearch">{{ t("grid.filter.search") }}</button>
        </div>
      </form>
      <div class="gt-actions">
        <span v-if="noKeyHint" class="badge warn key-hint" :title="noKeyHint">{{ noKeyBadge }}</span>
        <button class="btn sm" type="button" :disabled="!session.canInsert" @click="session.addRow()"><Icon name="plus" />{{ t("grid.toolbar.addRow") }}</button>
        <button
          class="btn sm"
          type="button"
          :disabled="!session.canDeleteSelection()"
          :title="session.store.schema.key.length ? t('grid.toolbar.deleteRowTitle') : t('grid.toolbar.noKeyDeleteTitle')"
          @click="session.deleteSelected()"
        ><Icon name="trash" />{{ t("grid.toolbar.deleteRow") }}</button>
        <span class="toolbar-sep" aria-hidden="true"></span>
        <button class="icon-btn" type="button" :aria-label="t('grid.toolbar.import')" :title="t('grid.toolbar.import')" :disabled="session.store.schema.kind !== 'table'" @click="importOpen = true"><Icon name="upload" /></button>
        <button class="icon-btn" type="button" :aria-label="t('grid.toolbar.export')" :title="t('grid.toolbar.export')" @click="exportOpen = true"><Icon name="download" /></button>
      </div>
    </div>
    <details v-if="(session.filters.length || session.sort || session.search) && session.sql" class="disclosure grid-sql">
      <summary>{{ t("grid.toolbar.viewSql") }}</summary>
      <div class="disclosure-body"><pre class="sql-code">{{ session.sql }}</pre></div>
    </details>
    <p v-if="filterError" class="grid-alert" role="alert"><svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="ERROR_ICON" /></svg>{{ filterError }}</p>
    <p v-if="editError && !editing" class="grid-alert" role="alert"><svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="ERROR_ICON" /></svg>{{ editError }}</p>
    <p v-if="importMessage" class="hint export-message" role="status">{{ importMessage }}</p>
    <p v-if="exportMessage" class="hint export-message" role="status">{{ exportMessage }}</p>
    <p v-if="session.error" class="grid-alert" role="alert"><svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="ERROR_ICON" /></svg>{{ t("grid.read.failed", { error: session.error }) }} <button class="btn sm" type="button" :disabled="retrying" @click="retryRead">{{ t("grid.read.retry") }}</button></p>
    <div :ref="setViewport" class="grid-scroll" @scroll.passive="onScroll" @keydown="onGridKeydown">
      <table class="grid-table">
        <thead>
          <tr>
            <th v-for="column in columns" :key="column.name" scope="col" :class="{ num: numericColumn(column) }" :style="columnStyle(column.name)" :data-type="typeCaption(column)" :aria-sort="session.sort?.column === column.name ? (session.sort.direction === 'asc' ? 'ascending' : 'descending') : 'none'">
              <button class="th-btn" type="button" :aria-label="t('grid.read.sortLabel', { column: column.name })" @click="session.cycleSort(column.name)"><span class="th-name">{{ column.name }}</span><span v-if="columnTypeSuffix(column)" class="col-type">{{ columnTypeSuffix(column) }}</span><span v-if="session.sort?.column === column.name" class="sort-arrow" aria-hidden="true">{{ session.sort.direction === "asc" ? "↑" : "↓" }}</span></button>
              <span class="col-resize" role="separator" :aria-label="t('grid.read.resizeColumn', { column: column.name })" @pointerdown.stop.prevent="startResize(column.name, $event)"></span>
            </th>
          </tr>
        </thead>
        <tbody>
          <tr v-if="padTop" class="virtual-spacer" aria-hidden="true"><td :colspan="columns.length" :style="{ height: `${padTop}px` }"></td></tr>
          <tr v-if="loading">
            <td class="grid-empty" :colspan="columns.length">{{ t("grid.read.loading") }}</td>
          </tr>
          <tr v-for="row in visibleRows" :key="row.id" :class="{ 'row-insert': row.kind === 'insert', 'row-delete': row.deleted, 'row-failed': row.failed }" :data-row-id="row.id" :data-located="row.located" :title="row.failure || undefined">
            <td v-for="(column, columnIndex) in columns" :key="column.name" :class="{ num: numericColumn(column) }" :style="columnStyle(column.name)">
              <div v-if="editing && editing.id === row.id && editing.column === column.name" class="editor">
                <input :ref="focusEditor" v-model="editing.draft" :aria-label="t('grid.editor.editColumn', { column: column.name })" :disabled="editing.isNull" @keydown="onEditorKeydown" />
                <button class="btn sm" type="button" :disabled="!column.nullable" @click="editing.isNull = true">{{ t("grid.editor.setNull") }}</button>
                <button class="btn sm" type="button" @click="editing.isNull = false">{{ t("grid.editor.emptyString") }}</button>
                <button class="btn sm primary" type="button" @click="saveEdit">{{ t("common.ok") }}</button>
                <button class="btn sm ghost" type="button" @click="cancelEdit">{{ t("common.cancel") }}</button>
                <p v-if="editError" class="menu-error" role="alert">{{ editError }}</p>
              </div>
              <button v-else class="cell-btn" :class="{ on: session.selectedId === row.id && session.selectedColumn === column.name, range: isPicked(row.id, column.name) }" :data-cell="`${row.id}:${column.name}`" :data-row="row.id" :data-column="column.name" type="button" @pointerdown="onCellDown(row.id, column.name, $event)" @pointerenter="onCellEnter(row.id, column.name)" @click="onCellClick(row.id, column.name)" @contextmenu="onCellMenu(row.id, column.name, $event)" @dblclick="beginEdit(row.id, column.name)">
                <span v-if="columnIndex === 0 && row.kind === 'insert'" class="row-flag">{{ t("grid.editor.newFlag") }}</span>
                <span v-if="columnIndex === 0 && row.deleted" class="row-flag del">{{ t("grid.editor.deleteFlag") }}</span>
                <span :class="{ 'v-null': row.cells[columnIndex]?.kind === 'null', 'v-empty': row.cells[columnIndex]?.kind === 'empty', 'v-large': row.cells[columnIndex]?.kind === 'large' }">{{ row.cells[columnIndex]?.label }}</span>
              </button>
            </td>
          </tr>
          <tr v-if="padBottom" class="virtual-spacer" aria-hidden="true"><td :colspan="columns.length" :style="{ height: `${padBottom}px` }"></td></tr>
          <tr v-if="showEmptyTable" data-empty="table">
            <td class="grid-empty" :colspan="Math.max(columns.length, 1)">{{ session.store.schema.kind === "view" ? t("grid.read.viewEmpty") : t("grid.read.tableEmpty") }}<button v-if="session.canInsert" class="btn sm" type="button" @click="session.addRow()">{{ t("grid.toolbar.addRow") }}</button></td>
          </tr>
          <tr v-else-if="showNoMatch" data-empty="filter">
            <td class="grid-empty" :colspan="Math.max(columns.length, 1)">{{ t("grid.read.noMatch") }} <button v-if="session.filters.length" class="btn sm" type="button" @click="clearFilters">{{ t("grid.read.clearFilters") }}</button> <button v-if="session.search" class="btn sm" type="button" @click="clearSearch">{{ t("grid.read.clearSearch") }}</button></td>
          </tr>
        </tbody>
      </table>
    </div>
    <footer class="grid-foot">
      <template v-if="session.truncated">
        <span>{{ t("grid.read.loadedPrefixA") }}<b>{{ session.loaded.length }}</b>{{ t("grid.read.loadedPrefixB", { n: session.loaded.length }) }}</span>
        <button class="btn sm" type="button" :aria-label="t('grid.read.continueRead')" @click="session.loadMore()">{{ t("grid.read.continueRead") }}</button>
      </template>
      <span v-else-if="session.loaded.length">{{ t("grid.read.allMatched", { n: session.loaded.length }) }}</span>
      <span class="grow"></span>
      <button class="btn sm" type="button" @click="session.count()">{{ t("grid.read.countMatches") }}</button>
      <span v-if="session.counted !== null" role="status">{{ t("grid.read.matchedCount", { n: session.counted }) }}</span>
      <span v-else class="muted">{{ t("grid.read.countPending") }}</span>
      <span v-if="calcStats" class="calc-stat" role="status">{{ t("grid.read.stats", { cells: calcStats.cells, numeric: calcStats.numeric }) }}<template v-if="calcStats.numeric > 0">{{ t("grid.read.statsNumeric", { sum: calcStats.sum, avg: calcStats.avg, min: calcStats.min, max: calcStats.max }) }}</template></span>
    </footer>
    <div v-if="pendingVisible" class="pending-bar" role="region" :aria-label="t('grid.commit.region')">
      <span class="badge warn">{{ t("grid.commit.pendingCount", { n: session.pendingCount }) }}</span>
      <button class="btn sm" type="button" @click="openCommit">{{ t("grid.commit.view") }}</button>
      <button
        class="btn sm danger"
        type="button"
        :data-armed="discardArmed ? 'true' : undefined"
        :title="discardArmed ? t('grid.commit.discardConfirmTitle') : t('grid.commit.discardTitle')"
        @click="discardAll"
      >{{ discardArmed ? t("grid.commit.discardConfirm") : t("grid.commit.discard") }}</button>
      <button class="btn sm primary" type="button" :disabled="!session.pendingCount" @click="openCommit">{{ t("grid.commit.submit") }}</button>
      <span v-if="receipt" class="receipt">{{ receipt }}</span>
      <p v-for="failure in commitFailures" :key="failure.id" class="commit-failure" role="alert">{{ failure.message }}</p>
    </div>
    <p v-else-if="receipt" class="commit-receipt" role="status">{{ receipt }}</p>
    </div>
    <section v-if="viewer" class="inspector" :aria-label="t('grid.value.viewer')">
      <div class="inspector-meta">
        <button class="icon-btn sm inspector-close" type="button" :aria-label="t('common.close')" :title="t('common.close')" @click="viewer = null"><Icon name="x" /></button>
        <span>{{ t("grid.value.column") }} <strong>{{ viewer.column }}</strong></span>
        <span>{{ t("grid.value.value") }} <em :class="viewerShown.kind === 'null' ? 'v-null' : viewerShown.kind === 'empty' ? 'v-empty' : ''">{{ viewerShown.kind === "null" ? "NULL" : viewerShown.kind === "empty" ? t("grid.value.emptyTitle") : viewerShown.kind === "large" ? t("grid.value.largeCurrent") : viewerShown.title }}</em></span>
      </div>
      <textarea readonly :value="viewerShown.body" :aria-label="t('grid.value.columnValue', { column: viewer.column })"></textarea>
      <div class="inspector-actions">
        <button v-if="viewerShown.kind === 'large'" class="btn sm" type="button" @click="fullValue">{{ t("grid.value.readFull") }}</button>
        <button class="btn sm" type="button" :disabled="editDisabled()" @click="editViewed">{{ t("common.edit") }}</button>
        <button class="btn sm" type="button" @click="copyViewed(false)">{{ t("grid.value.copyValue") }}</button>
        <button class="btn sm" type="button" @click="copyViewed(true)">{{ t("grid.value.copySqlValue") }}</button>
        <span v-if="copyMessage" class="copy-message" data-copy>{{ copyMessage }}</span>
      </div>
      <p class="legend">
        <span><span class="v-null">NULL</span> {{ t("grid.value.nullLabel") }}</span>
        <span><span class="v-empty">{{ t("grid.value.emptyLabel") }}</span> {{ t("grid.value.emptyZeroLength") }}</span>
        <span>{{ t("grid.value.legendLarge") }}</span>
      </p>
    </section>
    <div v-if="commitOpen" class="overlay" role="presentation" @keydown.esc="commitOpen = false">
      <div class="sheet" role="dialog" aria-modal="true" :aria-label="t('grid.commit.title')">
        <header>
          <div>
            <h2>{{ t("grid.commit.heading") }}</h2>
            <p class="hint" data-confirm-target>{{ t("grid.commit.target", { conn: connectionName, object: objectName }) }} <span v-if="environmentLabel" class="env xs" :class="environmentTone !== 'none' ? `env-${environmentTone}` : undefined">{{ environmentLabel }}</span></p>
            <p class="hint">{{ t("grid.commit.writeRule") }}</p>
          </div>
        </header>
        <div class="body">
          <p v-if="commitError" class="sheet-error" role="alert" data-commit-error><svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="ERROR_ICON" /></svg>{{ commitError }}</p>
          <ol class="sql-list">
            <li v-for="step in session.preview()" :key="step.id">
              <strong>{{ step.label }}</strong>
              <pre>{{ step.sql }}</pre>
            </li>
          </ol>
        </div>
        <footer>
          <button class="btn" type="button" @click="commitOpen = false">{{ t("common.cancel") }}</button>
          <span class="grow"></span>
          <button class="btn primary" type="button" :disabled="!session.pendingCount" @click="commitChangesNow()">{{ t("grid.commit.submitCount", { n: session.pendingCount }) }}</button>
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
      <button type="button" role="menuitem" data-action="view-value" @click="viewFromMenu"><Icon name="eye" />{{ t("grid.menu.viewValue") }}</button>
      <button type="button" role="menuitem" @click="copySelection(); menu = null"><Icon name="copy" />{{ t("grid.menu.copy") }}</button>
      <button v-if="canFilterByValue" type="button" role="menuitem" data-action="filter-by-value" @click="filterByValue"><Icon name="filter" />{{ t("grid.menu.filterByValue") }}</button>
      <button type="button" role="menuitem" @click="editFromMenu"><Icon name="pencil" />{{ t("common.edit") }}</button>
      <button type="button" role="menuitem" @click="setNullFromMenu"><Icon name="x" />{{ t("grid.editor.setNull") }}</button>
      <button v-if="referenceMenuItem" type="button" role="menuitem" data-action="jump-reference" @click="jumpReferenceFromMenu">{{ t("grid.menu.jumpReference", { label: referenceMenuItem.label }) }}</button>
      <p v-if="menuError" class="menu-error" role="alert">{{ menuError }}</p>
    </div>
    </template>
  </div>
</template>
