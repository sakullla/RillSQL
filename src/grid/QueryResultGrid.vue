<script setup lang="ts">
import { computed, ref } from "vue";
import ExportDialog from "../export/ExportDialog.vue";
import type { CsvOptions } from "../export/csv";
import { planExport, type ExportScope } from "../export/plan";
import { runExportTask } from "../export/tasks";
import { viewLoadedRows } from "./data";
import { showCell } from "./display";
import "./grid.css";
import type { Cell, Column, Row, SortSpec } from "./types";

const props = defineProps<{
  tabId: string;
  statement: string;
  columns: Column[];
  rows: Row[];
  truncated: boolean;
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
  runExportTask({
    plan,
    acknowledged: payload.acknowledged,
    path: plan.fileName,
    columns: props.columns.map((column) => column.name),
    rows: props.rows.map(cells),
    csv: payload.csv,
    readRows: () => props.rows.map(cells),
  });
  exportOpen.value = false;
}
</script>

<template>
  <div class="data-grid" aria-label="查询结果" :data-tab-id="tabId" data-scope="loaded">
    <div class="result-toolbar">
      <input v-model="find" aria-label="在已读取行中查找" placeholder="在已读取行中查找" />
      <span class="scope-tag local">{{ view.note }}</span>
      <span class="hint">结果只读，不能新增、修改或删除</span>
      <span class="grow"></span>
      <button class="btn" type="button" @click="exportOpen = true">导出</button>
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
        </tbody>
      </table>
    </div>
    <footer class="grid-foot">
      <span>已读取 <b>{{ rows.length }}</b> 行</span>
      <span v-if="truncated">还有未加载数据</span>
      <span>查找和排序{{ view.note }}</span>
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
