<script setup lang="ts">
import { computed, onBeforeUnmount, reactive, ref, watch } from "vue";
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
import { copyCellText, showCell } from "./display";
import "./grid.css";
import { loadSettings } from "../shell/settings";
import { locateResultRows, provedEditableColumns, readSingleTable, resolveEditableTable, ResultEditSession, type TableHit } from "./result-edit";
import { rejectWrite, typeCaption } from "./typeset";
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

const find = ref("");
const sort = ref<SortSpec | null>(null);
const exportOpen = ref(false);
const exportMessage = ref("");
const selected = ref<{ row: string; column: string; value: Cell } | null>(null);
const copyMessage = ref("");
const reason = ref("只读");
const insertTarget = ref<InsertTarget | null>(null);
const editSession = ref<ResultEditSession | null>(null);
const editing = ref<{ id: string; column: string; draft: string; isNull: boolean } | null>(null);
const editError = ref("");
const commitOpen = ref(false);
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
  const structural = readSingleTable(props.statement, props.engine ?? "postgres");
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
const { viewport, range, onScroll, reset } = useRowWindow(computed(() => view.value.rows.length), false);
const visibleRows = computed(() => view.value.rows.slice(range.value.start, range.value.end).map((row, index) => ({
  id: row.id,
  number: range.value.start + index + 1,
  located: row.located,
  deleted: editSession.value?.deletes.includes(row.id) ?? false,
  failed: !!(editSession.value?.failures.get(`u:${row.id}`) || editSession.value?.failures.get(`d:${row.id}`)),
  failure: [editSession.value?.failures.get(`u:${row.id}`), editSession.value?.failures.get(`d:${row.id}`)].filter(Boolean).join("\n"),
  cells: props.columns.map((column) => showCell(row.cells[column.name] ?? null, { column, opened: revealed.has(`${row.id}:${column.name}`), resolved: true })),
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
});

function inspect(rowId: string, index: number) {
  const row = view.value.rows.find((item) => item.id === rowId);
  const column = props.columns[index];
  if (!row || !column) return;
  selected.value = { row: rowId, column: column.name, value: editSession.value?.cellValue(rowId, column.name) ?? row.cells[column.name] ?? null };
  editSession.value?.select(rowId, column.name);
  copyMessage.value = "";
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
  editing.value = null;
  receipt.value = null;
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
  try { await navigator.clipboard.writeText(text); copyMessage.value = "已复制"; }
  catch { copyMessage.value = "复制失败，请在值查看器中选中文本复制。"; }
}

function isPicked(row: string, column: string): boolean {
  return selectedPoints.value.some((point) => point.row === row && point.column === column);
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
    copyMessage.value = "已复制";
  } catch {
    copyMessage.value = "复制失败，请在值查看器中选中文本复制。";
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
    menuError.value = "只读，不能修改";
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
    menuError.value = rejected ?? "只读，不能修改";
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

function onGridKeydown(event: KeyboardEvent) {
  if (editing.value) return;
  if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "c") {
    event.preventDefault();
    void copySelection();
  }
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
    engine: props.engine,
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

watch(() => props.rows, (rows) => {
  if (selected.value && !rows.some((row) => row.id === selected.value?.row)) selected.value = null;
});
</script>

<template>
  <div class="data-grid" aria-label="查询结果" :data-tab-id="tabId" data-scope="loaded" :data-editable="editSession ? 'true' : 'false'">
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
    <div ref="viewport" class="grid-scroll" @scroll.passive="onScroll" @keydown="onGridKeydown">
      <table class="grid-table" :aria-rowcount="view.rows.length + 1">
        <thead>
          <tr>
            <th class="row-number" aria-label="行号">#</th>
            <th v-for="column in columns" :key="column.name" :style="columnStyle(column.name)" :data-type="typeCaption(column)" :aria-sort="sort?.column === column.name ? (sort.direction === 'asc' ? 'ascending' : 'descending') : 'none'">
              <button class="th-btn" type="button" :aria-label="`仅对已读取行按 ${column.name} 排序`" @click="cycle(column.name)"><span>{{ column.name }}</span><span class="col-type">{{ typeCaption(column) }}</span><span v-if="sort?.column === column.name" class="sort-arrow">{{ sort.direction === 'asc' ? '↑' : '↓' }}</span></button>
              <span class="col-resize" role="separator" :aria-label="`调整 ${column.name} 列宽`" @pointerdown.stop.prevent="startResize(column.name, $event)"></span>
            </th>
          </tr>
        </thead>
        <tbody>
          <tr v-if="range.top" class="virtual-spacer" aria-hidden="true"><td :colspan="columns.length + 1" :style="{ height: `${range.top}px` }"></td></tr>
          <tr v-for="row in visibleRows" :key="row.id" :data-row-id="row.id" :aria-rowindex="row.number + 1" :class="{ 'row-delete': row.deleted, 'row-failed': row.failed }" :title="row.failure || undefined">
            <td class="row-number">{{ row.number }}<span v-if="row.deleted" class="row-flag del">将删除</span></td>
            <td v-for="(cell, index) in row.cells" :key="index" :style="columnStyle(columns[index]?.name ?? '')">
              <div v-if="editing && editing.id === row.id && editing.column === columns[index]?.name" class="editor">
                <input v-model="editing.draft" :aria-label="`编辑 ${columns[index]?.name}`" :disabled="editing.isNull" @keydown.enter.prevent="saveEdit" />
                <button class="btn sm" type="button" :disabled="columns[index] ? !((editSession?.schema.columns.find(item => item.name === columns[index]?.name)?.nullable) ?? true) : false" @click="editing.isNull = true">设为 NULL</button>
                <button class="btn sm" type="button" @click="editing.isNull = false">空字符串</button>
                <button class="btn sm primary" type="button" @click="saveEdit">确定</button>
                <button class="btn sm ghost" type="button" @click="editing = null">取消</button>
                <p v-if="editError" class="menu-error" role="alert">{{ editError }}</p>
              </div>
              <button v-else class="cell-btn" :class="{ on: selected?.row === row.id && selected?.column === columns[index]?.name, range: isPicked(row.id, columns[index]?.name ?? '') }" :data-cell="`${row.id}:${columns[index]?.name}`" type="button" @pointerdown="onCellDown(row.id, columns[index]?.name ?? '', $event)" @pointerenter="onCellEnter(row.id, columns[index]?.name ?? '')" @contextmenu="onCellMenu(row.id, columns[index]?.name ?? '', $event)" @dblclick="beginEdit(row.id, columns[index]?.name ?? '')"><span :class="{ 'v-null': cell.kind === 'null', 'v-empty': cell.kind === 'empty', 'v-large': cell.kind === 'large' }">{{ cell.label }}</span></button>
            </td>
          </tr>
          <tr v-if="range.bottom" class="virtual-spacer" aria-hidden="true"><td :colspan="columns.length + 1" :style="{ height: `${range.bottom}px` }"></td></tr>
          <tr v-if="!rows.length">
            <td class="grid-empty" :colspan="columns.length + 1">没有行。</td>
          </tr>
          <tr v-else-if="!view.rows.length">
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
    <section v-if="selected" class="inspector" aria-label="查询值查看器"><div class="inspector-meta"><strong>{{ selected.column }}</strong><span class="col-type">{{ typeCaption(columns.find((item) => item.name === selected.column)) }}</span><span>{{ selected.value === null ? 'NULL' : selected.value === '' ? '空字符串（长度 0）' : '值' }}</span><span class="grow"></span><button class="btn sm" @click="selected = null">关闭</button></div><textarea readonly :value="showCell(selected.value, { column: columns.find((item) => item.name === selected.column), opened: revealed.has(`${selected.row}:${selected.column}`), resolved: true }).body" :aria-label="`${selected.column} 的值`"></textarea><div class="inspector-actions"><button v-if="showCell(selected.value, { column: columns.find((item) => item.name === selected.column), resolved: true }).kind === 'large'" class="btn sm" type="button" @click="revealSelected">读取完整值</button><button v-if="editSession" class="btn sm" type="button" :disabled="!editSession.canUpdate(editSession.rowById(selected.row)) || !editSession.canEditColumn(selected.column)" @click="beginEdit(selected.row, selected.column)">编辑</button><button class="btn sm" @click="copyValue">复制值</button><span role="status">{{ copyMessage }}</span></div></section>
    <div v-if="menu" class="cell-menu" role="menu" :style="{ left: `${menu.x}px`, top: `${menu.y}px` }" @contextmenu.prevent>
      <button type="button" role="menuitem" @click="copySelection(); menu = null">复制</button>
      <button type="button" role="menuitem" @click="editFromMenu">编辑</button>
      <button type="button" role="menuitem" @click="setNullFromMenu">设为 NULL</button>
      <p v-if="menuError" class="menu-error" role="alert">{{ menuError }}</p>
    </div>
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
