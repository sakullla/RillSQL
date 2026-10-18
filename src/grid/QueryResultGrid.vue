<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, reactive, ref, watch } from "vue";
import { catalogConnections } from "../catalog/store";
import { registerPending } from "../shell/close-guard";
import { ContextMenu, type MenuItem } from "../ui";
import ExportDialog from "../export/ExportDialog.vue";
import type { CsvOptions } from "../export/csv";
import type { InsertTarget } from "../export/insert";
import { planExport, type ExportScope } from "../export/plan";
import { runExportTask, runRemoteExport } from "../export/tasks";
import type { ExportFormat } from "../export/write";
import { gridDescribe } from "./api";
import { viewLoadedRows } from "./data";
import { useRowWindow } from "./virtual";
import { copyPlain, showCell } from "./display";
import { autofitWidth, clampColWidth, MIN_COL_WIDTH } from "./column-width";
import { qualifiedName, updateSql, whereLocator } from "./sql";
import "./grid.css";
import { loadSettings } from "../shell/settings";
import { locateResultRows, provedEditableColumns, readSingleTable, resolveEditableTable, ResultEditSession, type TableHit } from "./result-edit";
import type { Cell, Column, Engine, Row, SortSpec } from "./types";
import {
  applyMove,
  applyPointer,
  cellKey,
  emptySelection,
  isCellSelected,
  selectAllRows,
  selectionToTsv,
  type CellAddr,
  type GridSelection,
  type MoveKey,
} from "./selection";

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

const find = ref("");
const sort = ref<SortSpec | null>(null);
const exportOpen = ref(false);
const exportMessage = ref("");
const selected = ref<{ row: string; column: string; value: Cell } | null>(null);
const selection = ref<GridSelection>({ ...emptySelection });
const rootEl = ref<HTMLElement | null>(null);
const copyMessage = ref("");
const reason = ref("只读");
const insertTarget = ref<InsertTarget | null>(null);
const editSession = ref<ResultEditSession | null>(null);
const editing = ref<{ id: string; column: string; draft: string; isNull: boolean } | null>(null);
const commitOpen = ref(false);
const receipt = ref<string | null>(null);
let generation = 0;

function tableHits(): TableHit[] {
  if (!props.connectionId) return [];
  const connection = catalogConnections().find((item) => item.id === props.connectionId);
  const hits: TableHit[] = [];
  for (const database of connection?.databases ?? []) {
    if (props.database && database.name !== props.database) continue;
    for (const schema of database.schemas) {
      for (const object of schema.objects) {
        if (object.stale) continue;
        hits.push({ schema: schema.name, name: object.name, kind: object.kind });
      }
    }
  }
  return hits;
}

watch(
  () => [props.statement, props.connectionId, props.database, props.schema, props.engine, props.rows, props.columns] as const,
  () => { void resolveEdit(); },
  { immediate: true },
);

async function resolveEdit() {
  const current = ++generation;
  editing.value = null;
  commitOpen.value = false;
  editSession.value = null;
  insertTarget.value = null;
  const structural = readSingleTable(props.statement);
  const engine = props.engine ?? "postgres";
  if (!structural.ok) {
    reason.value = structural.reason;
    return;
  }
  const resolved = resolveEditableTable(structural.table, tableHits(), props.database ?? "", engine);
  if (!resolved.ok) {
    reason.value = resolved.reason;
    return;
  }
  if (!props.connectionId) {
    reason.value = "没有绑定已打开的连接，不能修改查询结果。";
    return;
  }
  reason.value = "正在确认这张表能否修改…";
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
      reason.value = located.reason;
      return;
    }
    editSession.value = reactive(new ResultEditSession(described, {
      connectionId: props.connectionId,
      database: props.database ?? "",
      schema: resolved.schema,
      table: resolved.table,
      engine: described.engine,
    }, located.rows, located.editable));
    reason.value = `按 ${described.key.join(" + ")} 定位行。查询结果不能新增行。`;
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
const { viewport, range, onScroll, reset, reveal } = useRowWindow(computed(() => view.value.rows.length));
const visibleRows = computed(() => view.value.rows.slice(range.value.start, range.value.end).map((row, index) => ({
  id: row.id,
  number: range.value.start + index + 1,
  located: row.located,
  deleted: editSession.value?.deletes.includes(row.id) ?? false,
  failed: !!(editSession.value?.failures.get(`u:${row.id}`) || editSession.value?.failures.get(`d:${row.id}`)),
  failure: [editSession.value?.failures.get(`u:${row.id}`), editSession.value?.failures.get(`d:${row.id}`)].filter(Boolean).join("\n"),
  cells: props.columns.map((column) => showCell(row.cells[column.name] ?? null)),
})));
watch([find, sort, () => props.rows, () => props.columns], () => {
  reset();
  // 行列或过滤变化后行下标失锚，清空选区防止幽灵选中。
  selection.value = { ...emptySelection };
  selected.value = null;
});
// 暂存编辑在 find/sort 下会改变可见行成员或顺序，而上面的 watch 只盯 props——按过滤结果签名补清。
watch(
  () => view.value.rows.map((row) => row.id).join("\u0000"),
  () => {
    selection.value = { ...emptySelection };
    selected.value = null;
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
onBeforeUnmount(releaseCellPending);

function addrOf(rowId: string, colName: string): CellAddr | null {
  const rowIndex = view.value.rows.findIndex((row) => row.id === rowId);
  const colIndex = props.columns.findIndex((column) => column.name === colName);
  if (rowIndex < 0 || colIndex < 0) return null;
  return { rowIndex, colIndex };
}

function rowIdAt(rowIndex: number): string | null {
  return view.value.rows[rowIndex]?.id ?? null;
}

function columnNameAt(colIndex: number): string | null {
  return props.columns[colIndex]?.name ?? null;
}

function rawCellValue(rowId: string, colName: string): Cell {
  const row = view.value.rows.find((item) => item.id === rowId);
  return row?.cells[colName] ?? null;
}

function cellSelector(rowId: string, colName: string): string {
  return `[data-cell="${CSS.escape(rowId)}:${CSS.escape(colName)}"]`;
}

function focusCell(rowId: string, colName: string) {
  const index = view.value.rows.findIndex((row) => row.id === rowId);
  if (index >= 0) reveal(index);
  void nextTick(() => {
    (viewport.value?.querySelector(cellSelector(rowId, colName)) as HTMLElement | null)?.focus();
  });
}

/** 选区焦点 → 值查看器观察的选中格；没有焦点时收起查看器。 */
function syncSelectionToState() {
  const focus = selection.value.focus;
  const rowId = focus ? rowIdAt(focus.rowIndex) : null;
  const colName = focus ? columnNameAt(focus.colIndex) : null;
  if (!rowId || !colName) {
    selected.value = null;
    return;
  }
  selected.value = { row: rowId, column: colName, value: rawCellValue(rowId, colName) };
  editSession.value?.select(rowId, colName);
  copyMessage.value = "";
}

function syncFocusFromSelection() {
  syncSelectionToState();
  const focus = selection.value.focus;
  const rowId = focus ? rowIdAt(focus.rowIndex) : null;
  const colName = focus ? columnNameAt(focus.colIndex) : null;
  if (rowId && colName) focusCell(rowId, colName);
}

function cellClasses(rowId: string, colName: string): { on?: boolean; selected?: boolean } {
  const addr = addrOf(rowId, colName);
  if (!addr) return {};
  const focus = selection.value.focus;
  if (focus && focus.rowIndex === addr.rowIndex && focus.colIndex === addr.colIndex) return { on: true };
  return { selected: isCellSelected(selection.value, addr, view.value.rows.length, props.columns.length) };
}

function currentCellIndex(): number {
  const focus = selection.value.focus;
  const colCount = props.columns.length;
  if (!focus || !colCount) return -1;
  return focus.rowIndex * colCount + focus.colIndex;
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
  const rowIndex = Math.floor(index / cols.length);
  const colIndex = index % cols.length;
  selection.value = { anchor: { rowIndex, colIndex }, focus: { rowIndex, colIndex }, extras: "", allRows: false };
  syncFocusFromSelection();
}

function pointerModifiers(event: PointerEvent): { shift: boolean; ctrl: boolean } {
  return { shift: event.shiftKey, ctrl: event.ctrlKey || event.metaKey };
}

/** 当前选区展开后的格集合（矩形/allRows/extras 都算）。 */
function selectionKeySet(): Set<string> {
  const rowCount = view.value.rows.length;
  const colCount = props.columns.length;
  const keys = new Set<string>();
  for (let rowIndex = 0; rowIndex < rowCount; rowIndex += 1) {
    for (let colIndex = 0; colIndex < colCount; colIndex += 1) {
      if (isCellSelected(selection.value, { rowIndex, colIndex }, rowCount, colCount)) {
        keys.add(cellKey({ rowIndex, colIndex }));
      }
    }
  }
  return keys;
}

/** 把选区塌缩为 extras 集合（去掉矩形/allRows），行列 Ctrl 减选依赖它。 */
function applyExtrasSet(keys: Set<string>) {
  const extras = [...keys]
    .sort((a, b) => {
      const [aRow, aCol] = a.split(":").map(Number);
      const [bRow, bCol] = b.split(":").map(Number);
      return aRow - bRow || aCol - bCol;
    })
    .join(",");
  const focusKey = selection.value.focus ? cellKey(selection.value.focus) : null;
  let focus: CellAddr | null = null;
  if (focusKey && keys.has(focusKey)) {
    focus = { ...selection.value.focus! };
  } else if (extras) {
    const [rowIndex, colIndex] = extras.split(",")[0]!.split(":").map(Number);
    focus = { rowIndex, colIndex };
  }
  selection.value = { anchor: null, focus, extras, allRows: false };
  syncFocusFromSelection();
}

function toggleKey(keys: Set<string>, cell: CellAddr): void {
  const key = cellKey(cell);
  if (keys.has(key)) keys.delete(key);
  else keys.add(key);
}

function onCellPointer(event: PointerEvent, rowId: string, colName: string) {
  const addr = addrOf(rowId, colName);
  if (!addr) return;
  selection.value = applyPointer(selection.value, addr, pointerModifiers(event));
  syncFocusFromSelection();
}

/** pointerdown 被 prevent 后部分环境只剩 click；无修饰的纯单击按单选补齐，有修饰时以选区焦点为准。 */
function onCellClick(rowId: string, colName: string) {
  if (!selection.value.focus) {
    const addr = addrOf(rowId, colName);
    if (addr) selection.value = applyPointer({ ...emptySelection }, addr, { shift: false, ctrl: false });
  }
  syncFocusFromSelection();
}

/** 行号：无修饰选中整行；Shift 从锚点连到行尾；Ctrl 把整行从当前选区加/减掉。 */
function onRowNumberPointer(event: PointerEvent, rowId: string) {
  const rowIndex = view.value.rows.findIndex((row) => row.id === rowId);
  const lastCol = props.columns.length - 1;
  if (rowIndex < 0 || lastCol < 0) return;
  const mods = pointerModifiers(event);
  if (mods.ctrl) {
    const keys = selectionKeySet();
    for (let colIndex = 0; colIndex <= lastCol; colIndex += 1) {
      toggleKey(keys, { rowIndex, colIndex });
    }
    applyExtrasSet(keys);
  } else if (mods.shift) {
    selection.value = applyPointer(selection.value, { rowIndex, colIndex: lastCol }, { shift: true, ctrl: false });
    syncFocusFromSelection();
  } else {
    selection.value = applyPointer(selection.value, { rowIndex, colIndex: 0 }, { shift: false, ctrl: false });
    selection.value = applyPointer(selection.value, { rowIndex, colIndex: lastCol }, { shift: true, ctrl: false });
    syncFocusFromSelection();
  }
}

/** 表头：无修饰选中整列；Shift 从锚点连到列尾；Ctrl 把整列从当前选区加/减掉。 */
function onHeaderPointer(event: PointerEvent, colName: string) {
  const colIndex = props.columns.findIndex((column) => column.name === colName);
  const lastRow = view.value.rows.length - 1;
  if (colIndex < 0 || lastRow < 0) return;
  const mods = pointerModifiers(event);
  if (mods.ctrl) {
    const keys = selectionKeySet();
    for (let rowIndex = 0; rowIndex <= lastRow; rowIndex += 1) {
      toggleKey(keys, { rowIndex, colIndex });
    }
    applyExtrasSet(keys);
  } else if (mods.shift) {
    selection.value = applyPointer(selection.value, { rowIndex: lastRow, colIndex }, { shift: true, ctrl: false });
    syncFocusFromSelection();
  } else {
    selection.value = applyPointer(selection.value, { rowIndex: 0, colIndex }, { shift: false, ctrl: false });
    selection.value = applyPointer(selection.value, { rowIndex: lastRow, colIndex }, { shift: true, ctrl: false });
    syncFocusFromSelection();
  }
}

/** 列宽：th/td 都按 colWidths 写显式宽度，避免依赖 table-layout 猜测。 */
const colWidths = ref<Record<string, number>>({});
const resizing = ref<{ column: string; width: number; startX: number; startWidth: number } | null>(null);

function colStyle(name: string | undefined): { width: string } | undefined {
  const width = name ? colWidths.value[name] : undefined;
  return width ? { width: `${width}px` } : undefined;
}

function startColResize(event: PointerEvent, column: string) {
  const th = (event.currentTarget as HTMLElement | null)?.closest("th") as HTMLElement | null;
  const stored = colWidths.value[column];
  const inline = Number.parseFloat(th?.style.width ?? "");
  const measured = th?.getBoundingClientRect().width || th?.offsetWidth || 0;
  const startWidth = stored ?? (Number.isFinite(inline) && inline > 0 ? inline : (measured > 0 ? measured : MIN_COL_WIDTH));
  resizing.value = { column, width: startWidth, startX: event.clientX, startWidth };
  window.addEventListener("pointermove", moveColResize);
  window.addEventListener("pointerup", stopColResize);
  window.addEventListener("pointercancel", stopColResize);
}

function moveColResize(event: PointerEvent) {
  const state = resizing.value;
  if (!state) return;
  const width = clampColWidth(state.startWidth + (event.clientX - state.startX));
  state.width = width;
  colWidths.value[state.column] = width;
}

function stopColResize() {
  resizing.value = null;
  window.removeEventListener("pointermove", moveColResize);
  window.removeEventListener("pointerup", stopColResize);
  window.removeEventListener("pointercancel", stopColResize);
}
onBeforeUnmount(stopColResize);

function autofitCol(column: string) {
  const samples: string[] = [column];
  for (const row of view.value.rows.slice(0, 200)) {
    samples.push(row.cells[column] ?? "");
  }
  colWidths.value[column] = autofitWidth(samples);
}

async function copySelectionTsv() {
  if (!view.value.rows.length || !props.columns.length) return;
  const settings = loadSettings();
  const selectedKeys = selectionKeySet();
  // 单格复制纯值（不带表头/TSV），多格才按 TSV 导出。
  if (selectedKeys.size === 1) {
    const [key] = selectedKeys;
    const [rowIndex, colIndex] = key!.split(":").map(Number);
    const row = view.value.rows[rowIndex];
    const column = props.columns[colIndex];
    if (!row || !column) return;
    try {
      await navigator.clipboard.writeText(copyPlain(rawCellValue(row.id, column.name), settings.nullCopy));
      copyMessage.value = "已复制";
    } catch {
      copyMessage.value = "复制失败，请在值查看器中选中文本复制。";
    }
    return;
  }
  if (!selectedKeys.size) return;
  const tsv = selectionToTsv(selection.value, {
    headers: settings.tsvHeaders,
    columnNames: props.columns.map((column) => column.name),
    cellAt: (rowIndex, colIndex) => {
      const row = view.value.rows[rowIndex];
      const column = props.columns[colIndex];
      if (!row || !column) return null;
      return rawCellValue(row.id, column.name);
    },
    nullText: settings.nullCopy === "empty" ? "" : settings.nullCopy === "N" ? "\\N" : "NULL",
  });
  if (!tsv) return;
  try {
    await navigator.clipboard.writeText(tsv);
    copyMessage.value = "已复制为 TSV";
  } catch {
    copyMessage.value = "复制失败，请在值查看器中选中文本复制。";
  }
}

/** 未编辑时：方向键移动/Shift 扩展，Enter/F2 编辑焦点格，Tab 移动，Ctrl+A 全选，Ctrl+C 复制，Ctrl+F 查找，Esc 清除选择。 */
function onGridKeydown(event: KeyboardEvent) {
  if (editing.value) return;
  const rows = view.value.rows;
  const cols = props.columns;
  const moveKeys: MoveKey[] = ["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight"];
  if (moveKeys.includes(event.key as MoveKey)) {
    event.preventDefault();
    selection.value = applyMove(selection.value, event.key as MoveKey, event.shiftKey, rows.length, cols.length);
    syncFocusFromSelection();
  } else if (event.key === "Enter" || event.key === "F2") {
    const focus = selection.value.focus;
    const rowId = focus ? rowIdAt(focus.rowIndex) : null;
    const colName = focus ? columnNameAt(focus.colIndex) : null;
    if (editSession.value && rowId && colName) {
      event.preventDefault();
      beginEdit(rowId, colName);
    }
  } else if (event.key === "Tab") {
    event.preventDefault();
    moveSelection(event.shiftKey ? -1 : 1);
  } else if ((event.ctrlKey || event.metaKey) && (event.key === "a" || event.key === "A")) {
    event.preventDefault();
    selection.value = selectAllRows(rows.length, cols.length);
    syncFocusFromSelection();
  } else if ((event.ctrlKey || event.metaKey) && (event.key === "c" || event.key === "C")) {
    event.preventDefault();
    void copySelectionTsv();
  } else if ((event.ctrlKey || event.metaKey) && (event.key === "f" || event.key === "F")) {
    event.preventDefault();
    const input = rootEl.value?.querySelector('input[aria-label="在已读取行中查找"]') as HTMLInputElement | null;
    input?.focus();
    input?.select();
  } else if (event.key === "Escape") {
    event.preventDefault();
    selection.value = { ...emptySelection };
    selected.value = null;
    copyMessage.value = "";
    if (editSession.value) {
      editSession.value.selectedId = null;
      editSession.value.selectedColumn = null;
    }
  }
}

type ContextMenuKind = "cell" | "row" | "col";

const menu = ref<{ x: number; y: number; kind: ContextMenuKind; rowId?: string; column?: string } | null>(null);
const menuEl = ref<HTMLElement | null>(null);

function closeMenu() {
  menu.value = null;
}

function cellFromKey(key: string | null): { rowId: string; column: string } | null {
  if (!key) return null;
  const sep = key.indexOf(":");
  if (sep <= 0 || sep === key.length - 1) return null;
  return { rowId: key.slice(0, sep), column: key.slice(sep + 1) };
}

/** 网格内右键：单元格/行号/表头分发三套菜单；非焦点格先设为焦点格。 */
function onGridContextMenu(event: MouseEvent) {
  const target = event.target;
  if (!(target instanceof Element)) return;
  const cellBtn = target.closest<HTMLElement>("button.cell-btn");
  if (cellBtn) {
    event.preventDefault();
    const addr = cellFromKey(cellBtn.getAttribute("data-cell"));
    if (!addr) return;
    const focus = selection.value.focus;
    const focused = focus && rowIdAt(focus.rowIndex) === addr.rowId && columnNameAt(focus.colIndex) === addr.column;
    if (!focused) {
      const cell = addrOf(addr.rowId, addr.column);
      if (cell) {
        selection.value = applyPointer(selection.value, cell, { shift: false, ctrl: false });
        syncFocusFromSelection();
      }
    } else {
      focusCell(addr.rowId, addr.column);
    }
    menuEl.value = cellBtn;
    menu.value = { x: Math.max(8, event.clientX), y: Math.max(8, event.clientY), kind: "cell", ...addr };
    return;
  }
  const rowNumber = target.closest<HTMLElement>("td.row-number");
  if (rowNumber) {
    event.preventDefault();
    const rowId = rowNumber.closest("tr[data-row-id]")?.getAttribute("data-row-id") || "";
    if (!rowId) return;
    menuEl.value = rowNumber;
    menu.value = { x: Math.max(8, event.clientX), y: Math.max(8, event.clientY), kind: "row", rowId };
    return;
  }
  const header = target.closest<HTMLElement>("thead th[data-col]");
  if (header) {
    event.preventDefault();
    const column = header.getAttribute("data-col") || "";
    if (!column) return;
    menuEl.value = header;
    menu.value = { x: Math.max(8, event.clientX), y: Math.max(8, event.clientY), kind: "col", column };
  }
}

const menuLabel = computed(() => {
  const current = menu.value;
  if (!current) return "单元格操作";
  if (current.kind === "row") return "行操作";
  if (current.kind === "col") return `${current.column ?? ""} 列的操作`;
  return `${current.column ?? ""} 单元格的操作`;
});

function tsvNullText(): string {
  const nullCopy = loadSettings().nullCopy;
  return nullCopy === "empty" ? "" : nullCopy === "N" ? "\\N" : "NULL";
}

function rowTsv(rowId: string): string {
  const rows = view.value.rows;
  const names = props.columns.map((column) => column.name);
  const rowIndex = rows.findIndex((row) => row.id === rowId);
  if (rowIndex < 0 || !names.length) return "";
  return selectionToTsv(
    { anchor: { rowIndex, colIndex: 0 }, focus: { rowIndex, colIndex: names.length - 1 }, extras: "", allRows: false },
    {
      headers: false,
      columnNames: names,
      cellAt: (row, col) => rows[row]?.cells[names[col]!] ?? null,
      nullText: tsvNullText(),
    },
  );
}

function colTsv(column: string): string {
  const rows = view.value.rows;
  const colIndex = props.columns.findIndex((item) => item.name === column);
  const lastRow = rows.length - 1;
  if (colIndex < 0 || lastRow < 0) return "";
  return selectionToTsv(
    { anchor: { rowIndex: 0, colIndex }, focus: { rowIndex: lastRow, colIndex }, extras: "", allRows: false },
    {
      headers: false,
      columnNames: props.columns.map((item) => item.name),
      cellAt: (row) => rows[row]?.cells[column] ?? null,
      nullText: tsvNullText(),
    },
  );
}

/** 有主键且能定位到基表行时才生成 SELECT，否则给出具体原因。 */
function selectSqlText(rowId: string): string | null {
  const session = editSession.value;
  const row = session?.rowById(rowId);
  if (!session || !session.schema.key.length || !row?.located || !row.locator) return null;
  return `SELECT * FROM ${qualifiedName(session.schema)} WHERE ${whereLocator(session.schema, row.locator.columns, row.locator.values)}`;
}

function updateSqlText(rowId: string, column: string): string | null {
  const session = editSession.value;
  const row = session?.rowById(rowId);
  if (!session || !session.schema.key.length || !row?.located || !row.locator) return null;
  if (!session.canEditColumn(column)) return null;
  const value = session.cellValue(rowId, column);
  return updateSql(session.schema, { [column]: value ?? null }, row.locator.columns, row.locator.values);
}

function cellBlockTitle(rowId: string, column: string): string {
  const session = editSession.value;
  if (!session) return reason.value || "查询结果是只读的。";
  if (!session.canUpdate(session.rowById(rowId))) return "无法唯一确定这一行，不能修改或删除。";
  if (!session.canEditColumn(column)) return "这一列不是基表列，不能修改";
  return "";
}

function rowBlockTitle(rowId: string): string {
  const session = editSession.value;
  if (!session) return reason.value || "查询结果是只读的。";
  if (!session.canDeleteRow(rowId)) return "无法唯一确定这一行，不能删除。";
  return "";
}

const menuItems = computed<MenuItem[]>(() => {
  const current = menu.value;
  if (!current) return [];
  if (current.kind === "row") {
    const rowId = current.rowId!;
    const block = rowBlockTitle(rowId);
    return [
      { kind: "item", id: "copy-row", label: "复制行（TSV）" },
      { kind: "sep" },
      { kind: "item", id: "delete-item", label: "标记删除行", danger: true, disabled: !!block, title: block || undefined },
    ];
  }
  if (current.kind === "col") {
    return [
      { kind: "item", id: "copy-col", label: "复制列（TSV）" },
      { kind: "item", id: "filter-eq", label: "按此值筛选", disabled: true, title: "表头没有单元格值；右键单元格可按该值查找" },
    ];
  }
  const rowId = current.rowId!;
  const column = current.column!;
  const cellBlock = cellBlockTitle(rowId, column);
  const rowBlock = rowBlockTitle(rowId);
  const selectSql = selectSqlText(rowId);
  const updateText = updateSqlText(rowId, column);
  const noLocator = !editSession.value || !editSession.value.schema.key.length;
  return [
    { kind: "item", id: "copy-value", label: "复制值" },
    { kind: "item", id: "copy-row", label: "复制行（TSV）" },
    { kind: "item", id: "copy-col", label: "复制列（TSV）" },
    { kind: "item", id: "filter-eq", label: "按此值筛选" },
    { kind: "sep" },
    {
      kind: "item",
      id: "select-sql",
      label: "生成 SELECT",
      disabled: !selectSql,
      title: selectSql ? "复制 SELECT 到剪贴板" : reason.value || "无法唯一确定这一行，不能生成 SELECT",
    },
    {
      kind: "item",
      id: "update-sql",
      label: "生成 UPDATE",
      disabled: !updateText,
      title: updateText ? "已复制到剪贴板，不会执行" : cellBlock || (noLocator ? "查询结果没有可定位的主键，不能生成 UPDATE" : "无法唯一确定这一行，不能生成 UPDATE"),
    },
    { kind: "item", id: "edit-cell", label: "编辑单元格", disabled: !!cellBlock, title: cellBlock || undefined },
    { kind: "item", id: "delete-item", label: "标记删除行", danger: true, disabled: !!rowBlock, title: rowBlock || "标记后进入待提交，提交前会再次确认" },
  ];
});

async function writeClipboard(text: string, message: string) {
  try {
    await navigator.clipboard.writeText(text);
    copyMessage.value = message;
  } catch {
    copyMessage.value = "复制失败，请在值查看器中选中文本复制。";
  }
}

async function onMenuSelect(id: string) {
  const current = menu.value;
  if (!current) return;
  const settings = loadSettings();
  if (id === "copy-value" && current.kind === "cell") {
    await writeClipboard(copyPlain(rawCellValue(current.rowId!, current.column!), settings.nullCopy), "已复制");
    return;
  }
  if (id === "copy-row" && current.rowId) {
    const tsv = rowTsv(current.rowId);
    if (tsv) await writeClipboard(tsv, "已复制为 TSV");
    return;
  }
  if (id === "copy-col" && current.column) {
    const tsv = colTsv(current.column);
    if (tsv) await writeClipboard(tsv, "已复制为 TSV");
    return;
  }
  if (id === "filter-eq" && current.kind === "cell" && current.column) {
    find.value = String(rawCellValue(current.rowId!, current.column) ?? "");
    return;
  }
  if (id === "select-sql" && current.rowId) {
    const sql = selectSqlText(current.rowId);
    if (sql) await writeClipboard(sql, "已复制");
    return;
  }
  if (id === "update-sql" && current.kind === "cell" && current.rowId && current.column) {
    const sql = updateSqlText(current.rowId, current.column);
    if (sql) await writeClipboard(sql, "已复制，未执行");
    return;
  }
  if (id === "edit-cell" && current.kind === "cell" && current.rowId && current.column) {
    beginEdit(current.rowId, current.column);
    return;
  }
  if (id === "delete-item" && current.rowId) {
    // 只标记删除进入待提交，不直接写库；提交对话框是二次确认入口。
    editSession.value?.deleteRow(current.rowId);
    receipt.value = null;
  }
}

function beginEdit(id: string, column: string) {
  const session = editSession.value;
  const row = session?.rowById(id);
  if (!session || !row?.located || session.deletes.includes(id) || !session.canEditColumn(column)) return;
  const current = session.cellValue(id, column);
  editing.value = { id, column, draft: current ?? "", isNull: current === null };
  const addr = addrOf(id, column);
  if (addr) {
    selection.value = { anchor: { ...addr }, focus: { ...addr }, extras: "", allRows: false };
    syncSelectionToState();
  } else {
    session.select(id, column);
  }
}

function saveEdit() {
  const edit = editing.value;
  const session = editSession.value;
  if (!edit || !session) return;
  session.stageUpdate(edit.id, edit.column, edit.isNull ? null : edit.draft);
  editing.value = null;
  receipt.value = null;
  // stage 后刷新查看器快照，避免 inspector 停留在编辑前的值。
  syncSelectionToState();
  focusCell(edit.id, edit.column);
}

function cancelEdit() {
  const edit = editing.value;
  editing.value = null;
  if (edit) focusCell(edit.id, edit.column);
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
  try { await navigator.clipboard.writeText(copyPlain(selected.value.value, loadSettings().nullCopy)); copyMessage.value = "已复制"; }
  catch { copyMessage.value = "复制失败，请在值查看器中选中文本复制。"; }
}

function deleteSelected() {
  if (!selected.value || !editSession.value) return;
  editSession.value.deleteRow(selected.value.row);
  receipt.value = null;
}

async function commitChangesNow() {
  const session = editSession.value;
  if (!session) return;
  const pendingBefore = session.pendingCount;
  const report = await session.commit();
  commitOpen.value = false;
  if (!report) {
    receipt.value = "提交没有完成。尚未成功的暂存没有记成已成功。";
    return;
  }
  const saved = report.applied.length;
  if (report.failed) {
    const waiting = report.pendingIds.filter((id) => id !== report.failed?.id).length;
    receipt.value = `${saved} 处已保存，1 处失败。失败的更改仍保留。还有 ${waiting} 处没有执行，没有记成已成功。`;
    return;
  }
  receipt.value = `${saved} 处已保存。`;
  if (saved !== pendingBefore && session.pendingCount > 0) receipt.value += "尚未成功的暂存没有记成已成功。";
}

function startExport(payload: { scope: ExportScope; acknowledged: boolean; path: string; csv: CsvOptions; format: ExportFormat }) {
  const plan = planExport({
    origin: "query",
    scope: payload.scope,
    statement: props.statement,
    truncated: props.truncated,
    fileName: payload.path,
  });
  if (payload.format === "insert" && !insertTarget.value?.name) {
    exportMessage.value = `${reason.value} INSERT 没有开始。`;
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
            rejectReason: "查询结果没有绑定已打开的连接，不能把已加载行当成全量导出。",
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
      exportMessage.value = "正在从已打开的连接导出全部结果。";
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
  exportMessage.value = "正在保存已读取的行…";
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
</script>

<template>
  <div ref="rootEl" class="data-grid" aria-label="查询结果" :data-tab-id="tabId" data-scope="loaded" :data-editable="editSession ? 'true' : 'false'">
    <div class="result-toolbar">
      <input v-model="find" aria-label="在已读取行中查找" placeholder="在已读取行中查找" />
      <span class="toolbar-hint" :title="`查找和排序${view.note}，不会查询数据库。`">在已读取的 {{ rows.length }} 行中查找和排序</span>
      <span class="grow"></span>
      <span class="hint key-hint">{{ reason }}</span>
      <button v-if="editSession" class="btn sm" type="button" :disabled="!selected || !editSession.canDeleteRow(selected.row)" @click="deleteSelected">删除行</button>
      <button class="btn sm" type="button" @click="exportOpen = true">导出</button>
    </div>
    <p v-if="exportMessage" class="grid-hint" role="status">{{ exportMessage }}</p>
    <p v-if="receipt" class="grid-hint" role="status">{{ receipt }}</p>
    <div ref="viewport" class="grid-scroll" @scroll.passive="onScroll" @keydown="onGridKeydown" @contextmenu="onGridContextMenu">
      <table class="grid-table" :aria-rowcount="view.rows.length + 1">
        <thead>
          <tr>
            <th class="row-number" aria-label="行号">#</th>
            <th v-for="column in columns" :key="column.name" :data-col="column.name" :style="colStyle(column.name)" :aria-sort="sort?.column === column.name ? (sort.direction === 'asc' ? 'ascending' : 'descending') : 'none'" @pointerdown.prevent="onHeaderPointer($event, column.name)">
              <button class="th-btn" type="button" :aria-label="`仅对已读取行按 ${column.name} 排序`" @click="cycle(column.name)">{{ column.name }}<span v-if="sort?.column === column.name" class="sort-arrow">{{ sort.direction === 'asc' ? '↑' : '↓' }}</span></button>
              <span
                class="col-grip"
                role="separator"
                aria-orientation="vertical"
                :aria-label="`调整 ${column.name} 列宽`"
                :data-col="column.name"
                @pointerdown.stop.prevent="startColResize($event, column.name)"
                @dblclick.prevent="autofitCol(column.name)"
              ></span>
              <span v-if="resizing && resizing.column === column.name" class="col-width-tip">{{ resizing.width }}px</span>
            </th>
          </tr>
        </thead>
        <tbody>
          <tr v-if="range.top" class="virtual-spacer" aria-hidden="true"><td :colspan="columns.length + 1" :style="{ height: `${range.top}px` }"></td></tr>
          <tr v-for="row in visibleRows" :key="row.id" :data-row-id="row.id" :aria-rowindex="row.number + 1" :class="{ 'row-delete': row.deleted, 'row-failed': row.failed }" :title="row.failure || undefined">
            <td class="row-number" @pointerdown.prevent="onRowNumberPointer($event, row.id)">{{ row.number }}<span v-if="row.deleted" class="row-flag del">将删除</span></td>
            <td v-for="(cell, index) in row.cells" :key="index" :style="colStyle(columns[index]?.name)">
              <div v-if="editing && editing.id === row.id && editing.column === columns[index]?.name" class="editor">
                <input v-model="editing.draft" :aria-label="`编辑 ${columns[index]?.name}`" :disabled="editing.isNull" @keydown="onEditorKeydown" />
                <button class="btn sm" type="button" :disabled="columns[index] ? !((editSession?.schema.columns.find(item => item.name === columns[index]?.name)?.nullable) ?? true) : false" @click="editing.isNull = true">设为 NULL</button>
                <button class="btn sm" type="button" @click="editing.isNull = false">空字符串</button>
                <button class="btn sm primary" type="button" @click="saveEdit">确定</button>
                <button class="btn sm ghost" type="button" @click="cancelEdit">取消</button>
              </div>
              <button v-else class="cell-btn" :class="cellClasses(row.id, columns[index]?.name ?? '')" :data-cell="`${row.id}:${columns[index]?.name}`" type="button" @pointerdown.prevent="onCellPointer($event, row.id, columns[index]?.name ?? '')" @click="onCellClick(row.id, columns[index]?.name ?? '')" @dblclick="beginEdit(row.id, columns[index]?.name ?? '')"><span :class="{ 'v-null': cell.kind === 'null', 'v-empty': cell.kind === 'empty', 'v-large': cell.kind === 'large' }">{{ cell.label }}</span></button>
            </td>
          </tr>
          <tr v-if="range.bottom" class="virtual-spacer" aria-hidden="true"><td :colspan="columns.length + 1" :style="{ height: `${range.bottom}px` }"></td></tr>
          <tr v-if="!view.rows.length">
            <td class="grid-empty" :colspan="columns.length + 1">已读取的行里没有匹配「{{ find }}」的行。</td>
          </tr>
        </tbody>
      </table>
    </div>
    <footer class="grid-foot">
      <span v-if="find">匹配 {{ view.rows.length }} 行 · </span>
      <span v-if="truncated">已读取前 <b>{{ rows.length }}</b> 行，结果没有读完。</span>
      <span v-else>已读取全部结果行（{{ rows.length }} 行）。</span>
      <span v-if="truncated" class="muted">需要完整结果时，可导出「完整结果」，会重新执行这条查询。</span>
    </footer>
    <div v-if="pendingVisible && editSession" class="pending-bar" role="region" aria-label="未保存的更改">
      <span class="badge warn">{{ editSession.pendingCount }} 处更改未保存</span>
      <button class="btn sm" type="button" @click="commitOpen = true">查看</button>
      <button class="btn sm danger" type="button" @click="editSession.discard(); receipt = null">放弃</button>
      <button class="btn sm primary" type="button" :disabled="!editSession.pendingCount" @click="commitOpen = true">提交</button>
      <p v-for="failure in commitFailures" :key="failure.id" class="commit-failure" role="alert">{{ failure.message }}</p>
    </div>
    <section v-if="selected" class="inspector" aria-label="查询值查看器"><div class="inspector-meta"><strong>{{ selected.column }}</strong><span>{{ selected.value === null ? 'NULL' : selected.value === '' ? '空字符串（长度 0）' : '完整值' }}</span><span class="grow"></span><button class="btn sm" @click="selected = null">关闭</button></div><textarea readonly :value="selected.value ?? ''" :aria-label="`${selected.column} 的值`"></textarea><div class="inspector-actions"><button v-if="editSession" class="btn sm" type="button" :disabled="!editSession.canUpdate(editSession.rowById(selected.row)) || !editSession.canEditColumn(selected.column)" @click="beginEdit(selected.row, selected.column)">编辑</button><button class="btn sm" @click="copyValue">复制值</button><span role="status">{{ copyMessage }}</span></div></section>
    <div v-if="commitOpen && editSession" class="overlay" role="presentation">
      <div class="sheet" role="dialog" aria-modal="true" aria-label="待提交的更改">
        <header><div><h2>提交这些更改？</h2><p class="hint" data-confirm-target>目标：{{ connectionName }} / {{ objectName }} <span v-if="environmentLabel" class="env xs" :class="environmentTone !== 'none' ? `env-${environmentTone}` : undefined">{{ environmentLabel }}</span></p><p class="hint">将按顺序逐条写入数据库，遇到错误即停止；已写入的不会自动撤销。查询结果不能新增行。</p></div></header>
        <div class="body"><ol class="sql-list"><li v-for="step in editSession.preview()" :key="step.id"><strong>{{ step.label }}</strong><pre>{{ step.sql }}</pre></li></ol></div>
        <footer>
          <button class="btn" type="button" @click="commitOpen = false">取消</button>
          <span class="grow"></span>
          <button class="btn primary" type="button" @click="commitChangesNow()">提交 {{ editSession.pendingCount }} 处更改</button>
        </footer>
      </div>
    </div>
    <ExportDialog
      :open="exportOpen"
      origin="query"
      :statement="statement"
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
    <ContextMenu
      :open="Boolean(menu)"
      :x="menu?.x ?? 0"
      :y="menu?.y ?? 0"
      :items="menuItems"
      :label="menuLabel"
      :trigger="menuEl"
      @close="closeMenu"
      @select="onMenuSelect"
    />
  </div>
</template>
