<script setup lang="ts">
import { computed, reactive, ref, watch } from "vue";
import ExportDialog from "../export/ExportDialog.vue";
import { planExport, type ExportScope } from "../export/plan";
import { runRemoteExport } from "../export/tasks";
import type { CsvOptions } from "../export/csv";
import { loadSettings } from "../shell/settings";
import { copyPlain, copySqlValue, previewText, showCell, type NullCopy } from "./display";
import "./grid.css";
import { LiveTable } from "./live";
import type { TableTarget } from "./target";
import { FILTER_OPS, opLabel, opNeedsValue, type Cell, type FilterOp } from "./types";

const props = defineProps<{ tabId: string; draft: string; pageSize?: number; nullCopy?: NullCopy; target?: TableTarget | null }>();
const session = props.target ? reactive(new LiveTable(props.target, props.pageSize ?? loadSettings().pageSize)) : null;
if (session) void session.open();
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
const filterError = ref("");
const exportOpen = ref(false);
const commitOpen = ref(false);
const editing = ref<{ id: string; column: string; draft: string; isNull: boolean } | null>(null);
const copyMessage = ref("");
const revealed = reactive(new Set<string>());
const scrollTop = ref(0);
const viewport = ref(0);
const rowHeight = 28;

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
const windowStart = computed(() => (viewport.value === 0 ? 0 : Math.max(0, Math.floor(scrollTop.value / rowHeight) - 8)));
const windowEnd = computed(() => (viewport.value === 0 ? displayRows.value.length : Math.min(displayRows.value.length, Math.ceil((scrollTop.value + viewport.value) / rowHeight) + 8)));
const visibleRows = computed(() => displayRows.value.slice(windowStart.value, windowEnd.value));
const padTop = computed(() => windowStart.value * rowHeight);
const padBottom = computed(() => Math.max(0, displayRows.value.length - windowEnd.value) * rowHeight);

function onScroll(event: Event) {
  const element = event.target as HTMLElement;
  scrollTop.value = element.scrollTop;
  viewport.value = element.clientHeight;
}

const selectedColumn = computed(() => columns.value.find((column) => column.name === session?.selectedColumn) ?? null);
const selectedRaw = computed(() => (session?.selectedId && session.selectedColumn ? session.cellValue(session.selectedId, session.selectedColumn) : undefined));
const selectedShown = computed(() => showCell(selectedRaw.value));
const previewRows = computed(() => session?.loaded.slice(0, 4).map((row) => ({ ...row.cells })) ?? []);

async function applyFilter() {
  if (!session) return;
  filterError.value = "";
  const error = await session.addFilter({ column: filterColumn.value, op: filterOp.value, value: filterValue.value });
  if (error && !session.error) filterError.value = error;
  else if (!error) filterValue.value = "";
}

function beginEdit(id: string, column: string) {
  const row = displayRows.value.find((item) => item.id === id);
  if (!row) return;
  if (row.kind === "data" && !row.located) return;
  const current = session.cellValue(id, column);
  editing.value = { id, column, draft: current ?? "", isNull: current === null };
  session.select(id, column);
}

function saveEdit() {
  const edit = editing.value;
  if (!edit) return;
  const value: Cell | undefined = edit.isNull ? null : edit.draft;
  if (session.inserts.some((insert) => insert.id === edit.id)) session.stageInsert(edit.id, edit.column, value);
  else session.stageUpdate(edit.id, edit.column, value);
  editing.value = null;
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

function startExport(payload: { scope: ExportScope; acknowledged: boolean; path: string; csv: CsvOptions }) {
  if (!session?.target) return;
  const plan = planExport({
    origin: "table",
    scope: payload.scope,
    truncated: session.truncated,
    fileName: payload.path,
  });
  void runRemoteExport({
    plan,
    acknowledged: payload.acknowledged,
    path: plan.fileName,
    csv: payload.csv,
    mode: payload.scope === "full" ? "table-full" : "table-loaded",
    connectionId: session.target.connectionId,
    database: session.target.database,
    schema: session.target.schema,
    table: session.target.table,
    engine: session.target.engine,
    filters: session.filters,
    sort: session.sort,
    limit: payload.scope === "loaded" ? session.loaded.length : 0,
    truncated: session.truncated,
    statement: "",
  });
  exportOpen.value = false;
}
</script>

<template>
  <div class="data-grid" aria-label="数据表格" :data-tab-id="tabId" :data-draft="draft" :data-loaded="session ? session.loaded.length : 0" :data-truncated="session ? session.truncated : false">
    <p v-if="!session" class="grid-alert" role="alert">没有绑定已打开的表，不能用示例数据代替。</p>
    <template v-else>
    <div class="grid-toolbar">
      <form class="filter-form" @submit.prevent="applyFilter">
        <select v-model="filterColumn" aria-label="筛选列">
          <option v-for="column in columns" :key="column.name" :value="column.name">{{ column.name }}</option>
        </select>
        <select v-model="filterOp" aria-label="筛选条件">
          <option v-for="op in FILTER_OPS" :key="op.id" :value="op.id">{{ op.label }}</option>
        </select>
        <input v-if="opNeedsValue(filterOp)" v-model="filterValue" aria-label="筛选值" />
        <button class="btn" type="submit">筛选</button>
      </form>
      <span v-for="(filter, index) in session.filters" :key="`${filter.column}-${index}`" class="chip">
        <b>{{ filter.column }}</b> {{ opLabel(filter.op) }} <code v-if="opNeedsValue(filter.op)">{{ filter.value }}</code>
        <button class="chip-x" type="button" :aria-label="`移除条件 ${index + 1}`" @click="session.removeFilter(index)">×</button>
      </span>
      <span v-if="session.sort" class="chip">{{ session.sort.column }} {{ session.sort.direction === "asc" ? "升序" : "降序" }}
        <button class="chip-x" type="button" aria-label="清除排序" @click="session.clearSort()">×</button>
      </span>
      <span class="scope-tag server">{{ session.scopeLabel }}</span>
      <span class="grow"></span>
      <span class="key-chip" :class="session.store.schema.key.length ? 'ok' : 'warn'">{{ session.keyLabel }}</span>
      <button class="btn" type="button" :disabled="!session.canInsert" @click="session.addRow()">新增行</button>
      <button class="btn" type="button" :disabled="!session.canDeleteSelection()" :title="session.store.schema.key.length ? '删除选中的行' : '无法唯一定位已有行，删除已禁用'" @click="session.deleteSelected()">删除行</button>
      <button class="btn" type="button" @click="exportOpen = true">导出</button>
    </div>
    <p v-if="session.filters.length || session.sort" class="sql-strip"><code>{{ session.sql }}</code></p>
    <p v-if="filterError" class="grid-alert" role="alert">{{ filterError }}</p>
    <p v-if="session.error" class="grid-alert" role="alert">读取失败：{{ session.error }}。这不是无匹配结果。筛选条件仍保留。</p>
    <div class="grid-scroll" @scroll="onScroll">
      <div :style="{ height: `${padTop}px` }"></div>
      <table class="grid-table">
        <thead>
          <tr>
            <th v-for="column in columns" :key="column.name" scope="col" :aria-sort="session.sort?.column === column.name ? (session.sort.direction === 'asc' ? 'ascending' : 'descending') : 'none'">
              <button class="th-btn" type="button" :aria-label="`按 ${column.name} 排序`" @click="session.cycleSort(column.name)">{{ column.name }}</button>
            </th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="row in visibleRows" :key="row.id" :class="{ 'row-insert': row.kind === 'insert', 'row-delete': row.deleted, 'row-failed': row.failed }" :data-row-id="row.id" :data-located="row.located" :title="row.failure || undefined">
            <td v-for="column in columns" :key="column.name">
              <div v-if="editing && editing.id === row.id && editing.column === column.name" class="editor">
                <input v-model="editing.draft" :aria-label="`编辑 ${column.name}`" :disabled="editing.isNull" @keydown.enter.prevent="saveEdit" />
                <button class="btn" type="button" :disabled="!column.nullable" @click="editing.isNull = true">设为 NULL</button>
                <button class="btn" type="button" @click="editing.isNull = false">空字符串</button>
                <button class="btn primary" type="button" @click="saveEdit">暂存</button>
              </div>
              <button v-else class="cell-btn" :class="{ on: session.selectedId === row.id && session.selectedColumn === column.name }" type="button" @click="session.select(row.id, column.name)" @dblclick="beginEdit(row.id, column.name)">
                <span v-if="shown(row.id, column.name).kind === 'null'" class="v-null">NULL</span>
                <span v-else-if="shown(row.id, column.name).kind === 'empty'" class="v-empty">空字符串</span>
                <span v-else-if="shown(row.id, column.name).kind === 'large'" class="v-large">{{ shown(row.id, column.name).label }}</span>
                <span v-else>{{ shown(row.id, column.name).label }}</span>
              </button>
            </td>
          </tr>
          <tr v-if="!displayRows.length && !session.error">
            <td :colspan="columns.length">没有匹配行</td>
          </tr>
        </tbody>
      </table>
      <div :style="{ height: `${padBottom}px` }"></div>
    </div>
    <footer class="grid-foot">
      <span>已读取 <b>{{ session.loaded.length }}</b> 行</span>
      <span v-if="session.truncated">还有未加载数据</span>
      <span v-else-if="session.loaded.length">已读取全部匹配行</span>
      <button v-if="session.truncated" class="btn" type="button" aria-label="继续读取" @click="session.loadMore()">继续读取</button>
      <button class="btn" type="button" @click="session.count()">统计</button>
      <span v-if="session.counted !== null">共 {{ session.counted }} 行匹配</span>
      <span v-else>匹配行数未统计</span>
      <span class="grow"></span>
      <span class="v-null">NULL</span>
      <span>空值</span>
      <span class="v-empty">空字符串</span>
      <span>长度为 0</span>
    </footer>
    <div v-if="session.pendingCount || session.failures.size" class="pending-bar" role="region" aria-label="未提交的变更">
      <strong>{{ session.pendingCount }} 项变更未提交</strong>
      <span>修改 {{ session.edits.size }} 行 · 新增 {{ session.inserts.length }} 行 · 删除 {{ session.deletes.length }} 行</span>
      <span v-if="session.failures.size" class="err-text">{{ session.failures.size }} 行提交失败，仍留在暂存区</span>
      <span class="grow"></span>
      <button class="btn" type="button" @click="session.discard()">全部放弃</button>
      <button class="btn primary" type="button" :disabled="!session.pendingCount" @click="commitOpen = true">预览并提交</button>
      <p v-for="failure in commitFailures" :key="failure.id" class="commit-failure" role="alert">{{ failure.message }}</p>
    </div>
    <section v-if="session.selectedId" class="inspector" aria-label="值查看器">
      <div class="inspector-meta">
        <span>列 <strong>{{ session.selectedColumn }}</strong></span>
        <span>值 <em :class="selectedShown.kind === 'null' ? 'v-null' : selectedShown.kind === 'empty' ? 'v-empty' : ''">{{ selectedShown.kind === "null" ? "NULL" : selectedShown.kind === "empty" ? "空字符串（长度 0）" : selectedShown.kind === "large" ? "大字段，当前不是完整值" : selectedShown.title }}</em></span>
      </div>
      <textarea readonly :value="typeof selectedRaw === 'string' && selectedShown.kind === 'large' && !isRevealed() ? previewText(selectedRaw) : (selectedRaw ?? '')" :aria-label="`${session.selectedColumn} 的值`"></textarea>
      <div class="filters">
        <button v-if="selectedShown.kind === 'large'" class="btn" type="button" @click="fullValue">读取完整值</button>
        <button class="btn" type="button" :disabled="editDisabled()" @click="session.selectedId && session.selectedColumn && beginEdit(session.selectedId, session.selectedColumn)">编辑</button>
        <button class="btn" type="button" @click="copySelected(false)">复制值</button>
        <button class="btn" type="button" @click="copySelected(true)">复制为 SQL 值</button>
        <span v-if="copyMessage" data-copy>{{ copyMessage }}</span>
      </div>
    </section>
    <div v-if="commitOpen" class="overlay" role="presentation">
      <div class="sheet" role="dialog" aria-label="提交数据变更">
        <header>
          <div>
            <h2>提交数据变更</h2>
            <p class="hint">{{ session.store.schema.name }} · 失败即停，已成功的语句不撤销。</p>
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
          <button class="btn primary" type="button" @click="session.commit(); commitOpen = false">提交变更</button>
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
      @close="exportOpen = false"
      @start="startExport"
    />
    </template>
  </div>
</template>
