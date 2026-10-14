<script setup lang="ts">
import { computed, ref } from "vue";
import ExportDialog from "../export/ExportDialog.vue";
import type { CsvOptions } from "../export/csv";
import { planExport, type ExportScope } from "../export/plan";
import { runExportTask, runRemoteExport } from "../export/tasks";
import { viewLoadedRows } from "./data";
import { showCell } from "./display";
import "./grid.css";
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
const view = computed(() => viewLoadedRows(props.rows, props.columns, find.value, sort.value));

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
      });
    } else {
      void runRemoteExport({
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
    }
    exportOpen.value = false;
    return;
  }
  void runExportTask({
    plan,
    acknowledged: payload.acknowledged,
    path: plan.fileName,
    columns: props.columns.map((column) => column.name),
    rows: props.rows.map(cells),
    csv: payload.csv,
  });
  exportOpen.value = false;
}
</script>

<template>
  <div class="data-grid" aria-label="查询结果" :data-tab-id="tabId" data-scope="loaded">
    <div class="result-toolbar">
      <input v-model="find" aria-label="在已读取行中查找" placeholder="在已读取行中查找" />
      <p class="toolbar-hint">查找和排序{{ view.note }}，不会查询数据库。</p>
      <span class="grow"></span>
      <span class="hint">结果只读，不能新增、修改或删除</span>
      <button class="btn sm" type="button" @click="exportOpen = true">导出</button>
    </div>
    <div class="grid-scroll">
      <table class="grid-table">
        <thead>
          <tr>
            <th v-for="column in columns" :key="column.name">
              <button class="th-btn" type="button" :aria-label="`仅对已读取行按 ${column.name} 排序`" @click="cycle(column.name)">{{ column.name }}</button>
            </th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="row in view.rows" :key="row.id" :data-row-id="row.id">
            <td v-for="column in columns" :key="column.name">
              <span v-if="showCell(row.cells[column.name] ?? null).kind === 'null'" class="v-null">NULL</span>
              <span v-else-if="showCell(row.cells[column.name] ?? null).kind === 'empty'" class="v-empty">空字符串</span>
              <span v-else>{{ showCell(row.cells[column.name] ?? null).label }}</span>
            </td>
          </tr>
          <tr v-if="!view.rows.length">
            <td class="grid-empty" :colspan="columns.length">已读取的行里没有匹配「{{ find }}」的行。</td>
          </tr>
        </tbody>
      </table>
    </div>
    <footer class="grid-foot">
      <span v-if="truncated">已读取前 <b>{{ rows.length }}</b> 行，结果没有读完。</span>
      <span v-else>已读取全部结果行（{{ rows.length }} 行）。</span>
      <span v-if="truncated" class="muted">需要完整结果时，可导出「完整结果」，会重新执行这条查询。</span>
    </footer>
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
