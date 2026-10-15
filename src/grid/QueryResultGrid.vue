<script setup lang="ts">
import { computed, ref, watch } from "vue";
import ExportDialog from "../export/ExportDialog.vue";
import type { CsvOptions } from "../export/csv";
import { planExport, type ExportScope } from "../export/plan";
import { runExportTask, runRemoteExport } from "../export/tasks";
import { viewLoadedRows } from "./data";
import { useRowWindow } from "./virtual";
import { copyPlain, showCell } from "./display";
import "./grid.css";
import { loadSettings } from "../shell/settings";
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
function inspect(rowId: string, index: number) {
  const row = view.value.rows.find(row => row.id === rowId);
  const column = props.columns[index];
  if (!row || !column) return;
  selected.value = { row: rowId, column: column.name, value: row.cells[column.name] ?? null };
  copyMessage.value = "";
}
async function copyValue() {
  if (!selected.value) return;
  try { await navigator.clipboard.writeText(copyPlain(selected.value.value, loadSettings().nullCopy)); copyMessage.value = "已复制"; }
  catch { copyMessage.value = "复制失败，请在值查看器中选中文本复制。"; }
}
watch(() => props.rows, () => { selected.value = null; });
const view = computed(() => viewLoadedRows(props.rows, props.columns, find.value, sort.value));
const { viewport, range, onScroll, reset } = useRowWindow(computed(() => view.value.rows.length));
const visibleRows = computed(() => view.value.rows.slice(range.value.start, range.value.end).map((row, index) => ({
  id: row.id, number: range.value.start + index + 1,
  cells: props.columns.map(column => showCell(row.cells[column.name] ?? null)),
})));
watch([find, sort, () => props.rows], reset);

function cycle(column: string) {
  if (sort.value?.column !== column) sort.value = { column, direction: "asc" };
  else if (sort.value.direction === "asc") sort.value = { column, direction: "desc" };
  else sort.value = null;
}

function cells(row: Row): Record<string, Cell> {
  return { ...row.cells };
}

function startExport(payload: { scope: ExportScope; acknowledged: boolean; path: string; csv: CsvOptions }) {
  const plan = planExport({
    origin: "query",
    scope: payload.scope,
    statement: props.statement,
    truncated: props.truncated,
    fileName: payload.path,
  });
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
      }).then(({ result }) => { exportMessage.value = result.message; });
    } else {
      exportMessage.value = "正在从已打开的连接导出全部结果。";
      const { done } = runRemoteExport({
        plan,
        acknowledged: payload.acknowledged,
        path: plan.fileName,
        csv: payload.csv,
        mode: "query",
        connectionId: props.connectionId,
        database: props.database ?? "",
        schema: props.schema ?? "",
        table: "",
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
  exportMessage.value = "正在保存 CSV…";
  void runExportTask({
    plan,
    acknowledged: payload.acknowledged,
    path: plan.fileName,
    columns: props.columns.map((column) => column.name),
    rows: props.rows.map(cells),
    csv: payload.csv,
  }).then(({ result }) => { exportMessage.value = result.message; });
  exportOpen.value = false;
}
</script>

<template>
  <div class="data-grid" aria-label="查询结果" :data-tab-id="tabId" data-scope="loaded">
    <div class="result-toolbar">
      <input v-model="find" aria-label="在已读取行中查找" placeholder="在已读取行中查找" />
      <span class="toolbar-hint" :title="`查找和排序${view.note}，不会查询数据库。`">在已读取的 {{ rows.length }} 行中查找和排序</span>
      <span class="grow"></span>
      <span class="hint">只读</span>
      <button class="btn sm" type="button" @click="exportOpen = true">导出</button>
    </div>
    <p v-if="exportMessage" class="grid-hint" role="status">{{ exportMessage }}</p>
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
          <tr v-for="row in visibleRows" :key="row.id" :data-row-id="row.id" :aria-rowindex="row.number + 1">
            <td class="row-number">{{ row.number }}</td>
            <td v-for="(cell, index) in row.cells" :key="index" :title="cell.title"><button class="cell-btn" :class="{ on: selected?.row === row.id && selected?.column === columns[index]?.name }" @click="inspect(row.id, index)"><span :class="{ 'v-null': cell.kind === 'null', 'v-empty': cell.kind === 'empty', 'v-large': cell.kind === 'large' }">{{ cell.label }}</span></button></td>
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
    <section v-if="selected" class="inspector" aria-label="查询值查看器"><div class="inspector-meta"><strong>{{ selected.column }}</strong><span>{{ selected.value === null ? 'NULL' : selected.value === '' ? '空字符串（长度 0）' : '完整值 · 只读' }}</span><span class="grow"></span><button class="btn sm" @click="selected = null">关闭</button></div><textarea readonly :value="selected.value ?? ''" :aria-label="`${selected.column} 的值`"></textarea><div class="inspector-actions"><button class="btn sm" @click="copyValue">复制值</button><span role="status">{{ copyMessage }}</span></div></section>
    <ExportDialog
      :open="exportOpen"
      origin="query"
      :statement="statement"
      :truncated="truncated"
      file-stem="query"
      :loaded-count="rows.length"
      :pending="0"
      :columns="columns.map((column) => column.name)"
      :preview-rows="rows.slice(0, 4).map(cells)"
      @close="exportOpen = false"
      @start="startExport"
    />
  </div>
</template>
