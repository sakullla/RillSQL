<script setup lang="ts">
import { computed, reactive, ref, watch } from "vue";
import { catalogConnections } from "../catalog/store";
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
import "./grid.css";
import { loadSettings } from "../shell/settings";
import { locateResultRows, provedEditableColumns, readSingleTable, resolveEditableTable, ResultEditSession, type TableHit } from "./result-edit";
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
  const loadedColumns = () => props.columns.map((column) => ({ name: column.name, typeName: column.typeName }));
  insertTarget.value = {
    engine,
    schema: resolved.schema,
    name: resolved.table,
    columns: loadedColumns(),
  };
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
    insertTarget.value = {
      engine: described.engine,
      schema: resolved.schema,
      name: resolved.table,
      columns: props.columns.map((column) => {
        const base = editable.includes(column.name) ? described.columns.find((item) => item.name === column.name) : undefined;
        return { name: column.name, typeName: base?.typeName || column.typeName };
      }),
    };
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
const { viewport, range, onScroll, reset } = useRowWindow(computed(() => view.value.rows.length));
const visibleRows = computed(() => view.value.rows.slice(range.value.start, range.value.end).map((row, index) => ({
  id: row.id,
  number: range.value.start + index + 1,
  located: row.located,
  deleted: editSession.value?.deletes.includes(row.id) ?? false,
  failed: !!(editSession.value?.failures.get(`u:${row.id}`) || editSession.value?.failures.get(`d:${row.id}`)),
  failure: [editSession.value?.failures.get(`u:${row.id}`), editSession.value?.failures.get(`d:${row.id}`)].filter(Boolean).join("\n"),
  cells: props.columns.map((column) => showCell(row.cells[column.name] ?? null)),
})));
watch([find, sort, () => props.rows], reset);

const commitFailures = computed(() => (editSession.value ? Array.from(editSession.value.failures, ([id, message]) => ({ id, message })) : []));
const pendingVisible = computed(() => !!editSession.value && (editSession.value.pendingCount > 0 || editSession.value.failures.size > 0));

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
  const row = session?.rowById(id);
  if (!session || !row?.located || session.deletes.includes(id) || !session.canEditColumn(column)) return;
  const current = session.cellValue(id, column);
  editing.value = { id, column, draft: current ?? "", isNull: current === null };
  session.select(id, column);
}

function saveEdit() {
  const edit = editing.value;
  const session = editSession.value;
  if (!edit || !session) return;
  session.stageUpdate(edit.id, edit.column, edit.isNull ? null : edit.draft);
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

watch(() => props.rows, () => { selected.value = null; });
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
    <div ref="viewport" class="grid-scroll" @scroll.passive="onScroll">
      <table class="grid-table" :aria-rowcount="view.rows.length + 1">
        <thead>
          <tr>
            <th class="row-number" aria-label="行号">#</th>
            <th v-for="column in columns" :key="column.name" :aria-sort="sort?.column === column.name ? (sort.direction === 'asc' ? 'ascending' : 'descending') : 'none'">
              <button class="th-btn" type="button" :aria-label="`仅对已读取行按 ${column.name} 排序`" @click="cycle(column.name)">{{ column.name }}<span v-if="sort?.column === column.name" class="sort-arrow">{{ sort.direction === 'asc' ? '↑' : '↓' }}</span></button>
            </th>
          </tr>
        </thead>
        <tbody>
          <tr v-if="range.top" class="virtual-spacer" aria-hidden="true"><td :colspan="columns.length + 1" :style="{ height: `${range.top}px` }"></td></tr>
          <tr v-for="row in visibleRows" :key="row.id" :data-row-id="row.id" :aria-rowindex="row.number + 1" :class="{ 'row-delete': row.deleted, 'row-failed': row.failed }" :title="row.failure || undefined">
            <td class="row-number">{{ row.number }}<span v-if="row.deleted" class="row-flag del">将删除</span></td>
            <td v-for="(cell, index) in row.cells" :key="index">
              <div v-if="editing && editing.id === row.id && editing.column === columns[index]?.name" class="editor">
                <input v-model="editing.draft" :aria-label="`编辑 ${columns[index]?.name}`" :disabled="editing.isNull" @keydown.enter.prevent="saveEdit" />
                <button class="btn sm" type="button" :disabled="columns[index] ? !((editSession?.schema.columns.find(item => item.name === columns[index]?.name)?.nullable) ?? true) : false" @click="editing.isNull = true">设为 NULL</button>
                <button class="btn sm" type="button" @click="editing.isNull = false">空字符串</button>
                <button class="btn sm primary" type="button" @click="saveEdit">确定</button>
                <button class="btn sm ghost" type="button" @click="editing = null">取消</button>
              </div>
              <button v-else class="cell-btn" :class="{ on: selected?.row === row.id && selected?.column === columns[index]?.name }" :data-cell="`${row.id}:${columns[index]?.name}`" @click="inspect(row.id, index)" @dblclick="beginEdit(row.id, columns[index]?.name ?? '')"><span :class="{ 'v-null': cell.kind === 'null', 'v-empty': cell.kind === 'empty', 'v-large': cell.kind === 'large' }">{{ cell.label }}</span></button>
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
        <header><div><h2>提交这些更改？</h2><p class="hint">将按顺序逐条写入数据库，遇到错误即停止；已写入的不会自动撤销。查询结果不能新增行。</p></div></header>
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
  </div>
</template>
