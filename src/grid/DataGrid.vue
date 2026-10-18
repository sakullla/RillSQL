<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, reactive, ref, watch } from "vue";
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
import { ContextMenu, Listbox, type ListboxOption, type MenuItem } from "../ui";
import { copyPlain, copySqlValue, previewText, showCell, type NullCopy } from "./display";
import { autofitWidth, clampColWidth, MIN_COL_WIDTH } from "./column-width";
import { qualifiedName, updateSql, whereLocator } from "./sql";
import "./grid.css";
import { useRowWindow } from "./virtual";
import { LiveTable } from "./live";
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
import type { TableTarget } from "./target";
import { FILTER_OPS, opLabel, opNeedsValue, type Cell, type FilterOp } from "./types";

const props = defineProps<{ tabId: string; draft: string; pageSize?: number; nullCopy?: NullCopy; target?: TableTarget | null }>();
const session = props.target ? reactive(new LiveTable(props.target, props.pageSize ?? loadSettings().pageSize)) : null;
if (session) void session.open();
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
onBeforeUnmount(releaseCellPending);
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
const filterColumnOptions = computed<ListboxOption[]>(() => columns.value.map((column) => ({ value: column.name, label: column.name })));
const filterOpOptions: ListboxOption[] = FILTER_OPS.map((op) => ({ value: op.id, label: op.label }));
const filterError = ref("");
const exportOpen = ref(false);
const importOpen = ref(false);
const exportMessage = ref("");
const importMessage = ref("");
const commitOpen = ref(false);
const editing = ref<{ id: string; column: string; draft: string; isNull: boolean } | null>(null);
const copyMessage = ref("");
const selection = ref<GridSelection>({ ...emptySelection });
const receipt = ref<string | null>(null);
const revealed = reactive(new Set<string>());
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
/** 行集签名变化（插入行/筛选/排序/重载）后索引选区已失锚：清空选区与焦点，避免高亮和复制漂到未选中的行。 */
watch(
  () => displayRows.value.map((row) => row.id).join("\u0000"),
  () => {
    selection.value = { ...emptySelection };
    if (session) {
      session.selectedId = null;
      session.selectedColumn = null;
    }
  },
);
const commitFailures = computed(() => (session ? Array.from(session.failures, ([id, message]) => ({ id, message })) : []));
const pendingVisible = computed(() => (session ? session.pendingCount > 0 || session.failures.size > 0 : false));
const { viewport, range, onScroll, reveal } = useRowWindow(computed(() => displayRows.value.length), false);
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
const selectedShown = computed(() => showCell(selectedRaw.value));
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
  const error = await session.addFilter({ column: filterColumn.value, op: filterOp.value, value: filterValue.value });
  if (error && !session.error) filterError.value = error;
  else if (!error) filterValue.value = "";
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

function addrOf(rowId: string, colName: string): CellAddr | null {
  const rowIndex = displayRows.value.findIndex((row) => row.id === rowId);
  const colIndex = columns.value.findIndex((column) => column.name === colName);
  if (rowIndex < 0 || colIndex < 0) return null;
  return { rowIndex, colIndex };
}

function rowIdAt(rowIndex: number): string | null {
  return displayRows.value[rowIndex]?.id ?? null;
}

function columnNameAt(colIndex: number): string | null {
  return columns.value[colIndex]?.name ?? null;
}

/** 选区焦点 → 编辑器观察的选中格（LiveTable.selectedId 语义收窄为焦点格）。 */
function syncFocusFromSelection() {
  if (!session) return;
  const focus = selection.value.focus;
  if (!focus) return;
  const rowId = rowIdAt(focus.rowIndex);
  const colName = columnNameAt(focus.colIndex);
  if (!rowId || !colName) return;
  session.select(rowId, colName);
  focusCell(rowId, colName);
}

function cellClasses(rowId: string, colName: string): { on?: boolean; selected?: boolean } {
  const focused = !!session && session.selectedId === rowId && session.selectedColumn === colName;
  if (focused) return { on: true };
  const addr = addrOf(rowId, colName);
  if (!addr) return {};
  return { selected: isCellSelected(selection.value, addr, displayRows.value.length, columns.value.length) };
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
  session.select(row.id, column.name);
  const addr = addrOf(row.id, column.name);
  if (addr) selection.value = { anchor: { ...addr }, focus: { ...addr }, extras: "", allRows: false };
  focusCell(row.id, column.name);
}

function pointerModifiers(event: PointerEvent): { shift: boolean; ctrl: boolean } {
  return { shift: event.shiftKey, ctrl: event.ctrlKey || event.metaKey };
}

/** 当前选中格集合（矩形/allRows/extras 展开后的真实格）。 */
function selectionKeySet(): Set<string> {
  const rowCount = displayRows.value.length;
  const colCount = columns.value.length;
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
  if (session) {
    if (focus) syncFocusFromSelection();
    else {
      session.selectedId = null;
      session.selectedColumn = null;
    }
  }
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
  if (!session) return;
  if (!selection.value.focus) {
    const addr = addrOf(rowId, colName);
    if (addr) selection.value = applyPointer({ ...emptySelection }, addr, { shift: false, ctrl: false });
  }
  syncFocusFromSelection();
}

/** 行号：无修饰选中整行；Shift 从锚点连到行尾；Ctrl 把整行从当前选区加/减掉。 */
function onRowNumberPointer(event: PointerEvent, rowId: string) {
  const rowIndex = displayRows.value.findIndex((row) => row.id === rowId);
  const lastCol = columns.value.length - 1;
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
  const colIndex = columns.value.findIndex((column) => column.name === colName);
  const lastRow = displayRows.value.length - 1;
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
  for (const row of displayRows.value.slice(0, 200)) {
    const value = session?.cellValue(row.id, column);
    samples.push(value ?? "");
  }
  colWidths.value[column] = autofitWidth(samples);
}

async function copySelectionTsv() {
  if (!session || !displayRows.value.length || !columns.value.length) return;
  const settings = loadSettings();
  const selected = selectionKeySet();
  // 单格复制纯值（不带表头/TSV），多格才按 TSV 导出。
  if (selected.size === 1) {
    const [key] = selected;
    const [rowIndex, colIndex] = key!.split(":").map(Number);
    const row = displayRows.value[rowIndex];
    const column = columns.value[colIndex];
    if (!row || !column) return;
    const text = copyPlain(session.cellValue(row.id, column.name), settings.nullCopy);
    try {
      await navigator.clipboard.writeText(text);
      copyMessage.value = "已复制";
    } catch {
      copyMessage.value = "复制失败";
    }
    return;
  }
  if (!selected.size) return;
  const tsv = selectionToTsv(selection.value, {
    headers: settings.tsvHeaders,
    columnNames: columns.value.map((column) => column.name),
    cellAt: (rowIndex, colIndex) => {
      const row = displayRows.value[rowIndex];
      const column = columns.value[colIndex];
      if (!row || !column) return null;
      return session.cellValue(row.id, column.name);
    },
    nullText: settings.nullCopy === "empty" ? "" : settings.nullCopy === "N" ? "\\N" : "NULL",
  });
  if (!tsv) return;
  try {
    await navigator.clipboard.writeText(tsv);
    copyMessage.value = "已复制为 TSV";
  } catch {
    copyMessage.value = "复制失败";
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
    if (!addr || !session) return;
    if (session.selectedId !== addr.rowId || session.selectedColumn !== addr.column) {
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
  if (!session || !displayRows.value.length || !columns.value.length) return "";
  const names = columns.value.map((column) => column.name);
  const rowIndex = displayRows.value.findIndex((row) => row.id === rowId);
  if (rowIndex < 0) return "";
  return selectionToTsv(
    { anchor: { rowIndex, colIndex: 0 }, focus: { rowIndex, colIndex: names.length - 1 }, extras: "", allRows: false },
    {
      headers: false,
      columnNames: names,
      cellAt: (row, col) => {
        const item = displayRows.value[row];
        return item ? session!.cellValue(item.id, names[col]!) : null;
      },
      nullText: tsvNullText(),
    },
  );
}

function colTsv(column: string): string {
  if (!session || !displayRows.value.length || !columns.value.length) return "";
  const colIndex = columns.value.findIndex((item) => item.name === column);
  const lastRow = displayRows.value.length - 1;
  if (colIndex < 0 || lastRow < 0) return "";
  return selectionToTsv(
    { anchor: { rowIndex: 0, colIndex }, focus: { rowIndex: lastRow, colIndex }, extras: "", allRows: false },
    {
      headers: false,
      columnNames: columns.value.map((item) => item.name),
      cellAt: (row) => {
        const item = displayRows.value[row];
        return item ? session!.cellValue(item.id, column) : null;
      },
      nullText: tsvNullText(),
    },
  );
}

function selectSqlText(rowId: string): string | null {
  if (!session) return null;
  const schema = session.store.schema;
  if (session.inserts.some((insert) => insert.id === rowId)) return null;
  const row = session.rowById(rowId);
  if (!row) return null;
  let locatorColumns: string[];
  let locatorValues: (Cell | undefined)[];
  if (schema.key.length) {
    if (!row.located || !row.locator) return null;
    locatorColumns = row.locator.columns;
    locatorValues = row.locator.values;
  } else {
    // 没有主键时用全部非空列拼 WHERE，仍只生成 SELECT 不执行。
    locatorColumns = columns.value.map((column) => column.name).filter((name) => {
      const value = session!.cellValue(rowId, name);
      return value !== null && value !== undefined;
    });
    locatorValues = locatorColumns.map((name) => session!.cellValue(rowId, name));
  }
  if (!locatorColumns.length) return null;
  return `SELECT * FROM ${qualifiedName(schema)} WHERE ${whereLocator(schema, locatorColumns, locatorValues.map((value) => value ?? null))}`;
}

function updateSqlText(rowId: string, column: string): string | null {
  if (!session) return null;
  const schema = session.store.schema;
  if (!schema.key.length) return null;
  const row = session.rowById(rowId);
  if (!row?.located || !row.locator) return null;
  const value = session.cellValue(rowId, column);
  return updateSql(schema, { [column]: value ?? null }, row.locator.columns, row.locator.values);
}

/** 只读/无主键时给出具体原因，不能只写「不可用」。 */
function writeBlockTitle(rowId: string): string {
  if (!session) return "没有绑定已打开的表。";
  if (session.store.schema.kind === "view") return "视图是只读的，不能修改或删除数据。";
  const draft = session.inserts.find((insert) => insert.id === rowId);
  if (draft) return "";
  if (!session.store.schema.key.length) return "这张表没有主键，无法唯一确定已有行，所以不能修改或删除它们。";
  const row = session.rowById(rowId);
  if (!row?.located) return "无法唯一确定这一行，不能修改或删除。";
  return "";
}

const menuItems = computed<MenuItem[]>(() => {
  const current = menu.value;
  if (!current || !session) return [];
  if (current.kind === "row") {
    const rowId = current.rowId!;
    const block = writeBlockTitle(rowId);
    return [
      { kind: "item", id: "copy-row", label: "复制行（TSV）" },
      { kind: "sep" },
      { kind: "item", id: "delete-item", label: "标记删除行", danger: true, disabled: !!block, title: block || undefined },
    ];
  }
  if (current.kind === "col") {
    return [
      { kind: "item", id: "copy-col", label: "复制列（TSV）" },
      { kind: "item", id: "filter-eq", label: "按此值筛选", disabled: true, title: "表头没有单元格值；右键单元格可按该值筛选" },
    ];
  }
  const rowId = current.rowId!;
  const column = current.column!;
  const block = writeBlockTitle(rowId);
  const selectSql = selectSqlText(rowId);
  const updateSqlTextValue = updateSqlText(rowId, column);
  return [
    { kind: "item", id: "copy-value", label: "复制值" },
    { kind: "item", id: "copy-row", label: "复制行（TSV）" },
    { kind: "item", id: "copy-col", label: "复制列（TSV）" },
    { kind: "item", id: "filter-eq", label: "按此值筛选" },
    { kind: "sep" },
    { kind: "item", id: "select-sql", label: "生成 SELECT", disabled: !selectSql, title: selectSql ? "复制 SELECT 到剪贴板" : "无法唯一确定这一行，不能生成 SELECT" },
    {
      kind: "item",
      id: "update-sql",
      label: "生成 UPDATE",
      disabled: !updateSqlTextValue,
      title: updateSqlTextValue ? "已复制到剪贴板，不会执行" : block || "这张表没有主键，无法唯一确定已有行，不能生成 UPDATE",
    },
    { kind: "item", id: "edit-cell", label: "编辑单元格", disabled: !!block, title: block || undefined },
    { kind: "item", id: "delete-item", label: "标记删除行", danger: true, disabled: !!block, title: block || "标记后进入待提交，提交前会再次确认" },
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
  if (!current || !session) return;
  if (id === "copy-value" && current.kind === "cell") {
    const raw = session.cellValue(current.rowId!, current.column!);
    await writeClipboard(copyPlain(raw, props.nullCopy ?? loadSettings().nullCopy), "已复制");
    return;
  }
  if (id === "copy-row" && current.rowId) {
    await writeClipboard(rowTsv(current.rowId), "已复制为 TSV");
    return;
  }
  if (id === "copy-col" && current.column) {
    await writeClipboard(colTsv(current.column), "已复制为 TSV");
    return;
  }
  if (id === "filter-eq" && current.kind === "cell" && current.column) {
    filterError.value = "";
    const raw = session.cellValue(current.rowId!, current.column);
    filterColumn.value = current.column;
    const error = await session.addFilter({ column: current.column, op: "eq", value: String(raw ?? "") });
    if (error && !session.error) filterError.value = error;
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
    session.deleteRow(current.rowId);
    receipt.value = null;
  }
}

function beginEdit(id: string, column: string) {
  const row = displayRows.value.find((item) => item.id === id);
  if (!row) return;
  if (row.kind === "data" && !row.located) return;
  const current = session.cellValue(id, column);
  editing.value = { id, column, draft: current ?? "", isNull: current === null };
  session.select(id, column);
}

function editSelected() {
  if (!session?.selectedId || !session.selectedColumn) return;
  beginEdit(session.selectedId, session.selectedColumn);
}

function saveEdit() {
  const edit = editing.value;
  if (!edit) return;
  const value: Cell | undefined = edit.isNull ? null : edit.draft;
  if (session.inserts.some((insert) => insert.id === edit.id)) session.stageInsert(edit.id, edit.column, value);
  else session.stageUpdate(edit.id, edit.column, value);
  const { id, column } = edit;
  editing.value = null;
  focusCell(id, column);
}

function cancelEdit() {
  const edit = editing.value;
  editing.value = null;
  if (edit) focusCell(edit.id, edit.column);
}

function focusEditor(el: HTMLInputElement | null) {
  el?.focus();
  el?.select();
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

/** Shift+滚轮转为横向滚动；普通滚轮保持原生纵向滚动，不拦截。 */
function onScrollWheel(event: WheelEvent) {
  if (!event.shiftKey) return;
  event.preventDefault();
  const el = viewport.value;
  if (!el) return;
  el.scrollLeft += event.deltaY || event.deltaX;
}

/** 未编辑时：方向键移动/Shift 扩展，Enter/F2 编辑焦点格，Tab 移动，Ctrl+A 全选，Ctrl+C 复制 TSV，Esc 清除选择。 */
function onGridKeydown(event: KeyboardEvent) {
  if (editing.value) return;
  const rows = displayRows.value;
  const cols = columns.value;
  const moveKeys: MoveKey[] = ["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight"];
  if (moveKeys.includes(event.key as MoveKey)) {
    event.preventDefault();
    selection.value = applyMove(selection.value, event.key as MoveKey, event.shiftKey, rows.length, cols.length);
    syncFocusFromSelection();
  } else if (event.key === "Enter" || event.key === "F2") {
    const focus = selection.value.focus;
    const rowId = focus ? rowIdAt(focus.rowIndex) : session?.selectedId;
    const colName = focus ? columnNameAt(focus.colIndex) : session?.selectedColumn;
    if (session && rowId && colName) {
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
  } else if (event.key === "Escape") {
    event.preventDefault();
    selection.value = { ...emptySelection };
    if (session) {
      session.selectedId = null;
      session.selectedColumn = null;
    }
  }
}

function discardAll() {
  session?.discard();
  receipt.value = null;
}

async function commitChangesNow() {
  if (!session) return;
  const report = await session.commit();
  commitOpen.value = false;
  if (!report) return;
  const saved = report.applied.length;
  const failed = report.failed ? 1 : 0;
  receipt.value = failed ? `${saved} 处已保存，${failed} 处失败。失败的更改仍保留在待提交列表中，原因已标出。` : `${saved} 处已保存。`;
}

function shown(id: string, column: string) {
  const value = session.cellValue(id, column);
  const key = `${id}:${column}`;
  if (value && revealed.has(key)) return { kind: "text" as const, label: value, title: value };
  return showCell(value);
}

function copySelected(asSql: boolean) {
  if (!session.selectedColumn) return;
  const column = selectedColumn.value ?? undefined;
  copyMessage.value = asSql ? copySqlValue(selectedRaw.value, column) : copyPlain(selectedRaw.value, props.nullCopy ?? "NULL");
}

function revealKey(): string {
  return `${session.selectedId}:${session.selectedColumn}`;
}

function isRevealed(): boolean {
  return revealed.has(revealKey());
}

function fullValue() {
  if (!session.selectedId || !session.selectedColumn || typeof selectedRaw.value !== "string") return;
  revealed.add(revealKey());
}

function editDisabled(): boolean {
  if (!session.selectedId || !session.selectedColumn) return true;
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

function startExport(payload: { scope: ExportScope; acknowledged: boolean; path: string; csv: CsvOptions; format: ExportFormat }) {
  if (!session?.target) return;
  const plan = planExport({
    origin: "table",
    scope: payload.scope,
    truncated: session.truncated,
    fileName: payload.path,
  });
  exportOpen.value = false;
  if (payload.scope === "loaded") {
    exportMessage.value = payload.format === "csv" ? "正在保存 CSV…" : "正在保存已读取的行…";
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
        <Listbox v-model="filterColumn" :options="filterColumnOptions" aria-label="筛选列" />
        <Listbox v-model="filterOp" :options="filterOpOptions" aria-label="筛选条件" />
        <input v-if="opNeedsValue(filterOp)" v-model="filterValue" aria-label="筛选值" />
        <button class="btn sm" type="submit">筛选</button>
      </form>
      <span v-for="(filter, index) in session.filters" :key="`${filter.column}-${index}`" class="chip">
        <b>{{ filter.column }}</b> {{ opLabel(filter.op) }} <code v-if="opNeedsValue(filter.op)">{{ filter.value }}</code>
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
    <p v-if="importMessage" class="hint export-message" role="status">{{ importMessage }}</p>
    <p v-if="exportMessage" class="hint export-message" role="status">{{ exportMessage }}</p>
    <p v-if="session.error" class="grid-alert" role="alert">读取失败：{{ session.error }}。这不是无匹配结果。筛选条件仍保留。 <button class="btn sm" type="button" :disabled="retrying" @click="retryRead">重试</button></p>
    <div ref="viewport" class="grid-scroll" @scroll.passive="onScroll" @wheel="onScrollWheel" @keydown="onGridKeydown" @contextmenu="onGridContextMenu">
      <table class="grid-table">
        <thead>
          <tr>
            <th class="row-number" scope="col" aria-label="行号">#</th>
            <th v-for="column in columns" :key="column.name" scope="col" :data-col="column.name" :style="colStyle(column.name)" :aria-sort="session.sort?.column === column.name ? (session.sort.direction === 'asc' ? 'ascending' : 'descending') : 'none'" @pointerdown.prevent="onHeaderPointer($event, column.name)">
              <button class="th-btn" type="button" :aria-label="`按 ${column.name} 排序`" @click="session.cycleSort(column.name)">{{ column.name }}</button>
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
          <tr v-if="padTop" class="virtual-spacer" aria-hidden="true"><td :colspan="columns.length + 1" :style="{ height: `${padTop}px` }"></td></tr>
          <tr v-if="loading">
            <td class="grid-empty" :colspan="columns.length + 1">正在读取表数据…</td>
          </tr>
          <tr v-for="(row, visibleIndex) in visibleRows" :key="row.id" :class="{ 'row-insert': row.kind === 'insert', 'row-delete': row.deleted, 'row-failed': row.failed }" :data-row-id="row.id" :data-located="row.located" :title="row.failure || undefined">
            <td class="row-number" @pointerdown.prevent="onRowNumberPointer($event, row.id)">{{ range.start + visibleIndex + 1 }}</td>
            <td v-for="(column, columnIndex) in columns" :key="column.name" :style="colStyle(column.name)">
              <div v-if="editing && editing.id === row.id && editing.column === column.name" class="editor">
                <input :ref="focusEditor" v-model="editing.draft" :aria-label="`编辑 ${column.name}`" :disabled="editing.isNull" @keydown="onEditorKeydown" />
                <button class="btn sm" type="button" :disabled="!column.nullable" @click="editing.isNull = true">设为 NULL</button>
                <button class="btn sm" type="button" @click="editing.isNull = false">空字符串</button>
                <button class="btn sm primary" type="button" @click="saveEdit">确定</button>
                <button class="btn sm ghost" type="button" @click="cancelEdit">取消</button>
              </div>
              <button v-else class="cell-btn" :class="cellClasses(row.id, column.name)" :data-cell="`${row.id}:${column.name}`" type="button" @pointerdown.prevent="onCellPointer($event, row.id, column.name)" @click="onCellClick(row.id, column.name)" @dblclick="beginEdit(row.id, column.name)">
                <span v-if="columnIndex === 0 && row.kind === 'insert'" class="row-flag">新增</span>
                <span v-if="columnIndex === 0 && row.deleted" class="row-flag del">将删除</span>
                <span :class="{ 'v-null': row.cells[columnIndex]?.kind === 'null', 'v-empty': row.cells[columnIndex]?.kind === 'empty', 'v-large': row.cells[columnIndex]?.kind === 'large' }">{{ row.cells[columnIndex]?.label }}</span>
              </button>
            </td>
          </tr>
          <tr v-if="padBottom" class="virtual-spacer" aria-hidden="true"><td :colspan="columns.length + 1" :style="{ height: `${padBottom}px` }"></td></tr>
          <tr v-if="showEmptyTable" data-empty="table">
            <td class="grid-empty" :colspan="Math.max(columns.length, 1) + 1">{{ session.store.schema.kind === "view" ? "这个视图没有行。" : "这张表没有行。" }}<button v-if="session.canInsert" class="btn sm" type="button" @click="session.addRow()">新增行</button></td>
          </tr>
          <tr v-else-if="showNoMatch" data-empty="filter">
            <td class="grid-empty" :colspan="Math.max(columns.length, 1) + 1">没有匹配行。 <button class="btn sm" type="button" @click="clearFilters">清除筛选</button></td>
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
    </footer>
    <div v-if="pendingVisible" class="pending-bar" role="region" aria-label="未保存的更改">
      <span class="badge warn">{{ session.pendingCount }} 处更改未保存</span>
      <button class="btn sm" type="button" @click="commitOpen = true">查看</button>
      <button class="btn sm danger" type="button" @click="discardAll">放弃</button>
      <button class="btn sm primary" type="button" :disabled="!session.pendingCount" @click="commitOpen = true">提交</button>
      <span v-if="receipt" class="receipt">{{ receipt }}</span>
      <p v-for="failure in commitFailures" :key="failure.id" class="commit-failure" role="alert">{{ failure.message }}</p>
    </div>
    <p v-else-if="receipt" class="commit-receipt" role="status">{{ receipt }}</p>
    <section v-if="session.selectedId" class="inspector" aria-label="值查看器">
      <div class="inspector-meta">
        <span>列 <strong>{{ session.selectedColumn }}</strong></span>
        <span>值 <em :class="selectedShown.kind === 'null' ? 'v-null' : selectedShown.kind === 'empty' ? 'v-empty' : ''">{{ selectedShown.kind === "null" ? "NULL" : selectedShown.kind === "empty" ? "空字符串（长度 0）" : selectedShown.kind === "large" ? "大字段，当前不是完整值" : selectedShown.title }}</em></span>
      </div>
      <textarea readonly :value="typeof selectedRaw === 'string' && selectedShown.kind === 'large' && !isRevealed() ? previewText(selectedRaw) : (selectedRaw ?? '')" :aria-label="`${session.selectedColumn} 的值`"></textarea>
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
      @done="(message) => { importMessage.value = message; importOpen = false; void session.readWindow(0, Math.max(session.pageSize, session.loaded.length || session.pageSize), true); }"
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
    </template>
  </div>
</template>
