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
import { copyPlain, copySqlValue, previewText, showCell, type NullCopy } from "./display";
import "./grid.css";
import { useRowWindow } from "./virtual";
import { LiveTable } from "./live";
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
const filterError = ref("");
const exportOpen = ref(false);
const importOpen = ref(false);
const exportMessage = ref("");
const importMessage = ref("");
const commitOpen = ref(false);
const editing = ref<{ id: string; column: string; draft: string; isNull: boolean } | null>(null);
const copyMessage = ref("");
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
  focusCell(row.id, column.name);
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

/** 未编辑时：Enter 开始编辑选中单元格，Tab 移动选中。 */
function onGridKeydown(event: KeyboardEvent) {
  if (editing.value) return;
  if (["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight"].includes(event.key)) {
    event.preventDefault();
    moveSelection(event.key === "ArrowUp" ? -columns.value.length : event.key === "ArrowDown" ? columns.value.length : event.key === "ArrowLeft" ? -1 : 1);
  } else if (event.key === "Enter" || event.key === "F2") {
    if (session?.selectedId && session.selectedColumn) {
      event.preventDefault();
      beginEdit(session.selectedId, session.selectedColumn);
    }
  } else if (event.key === "Tab") {
    event.preventDefault();
    moveSelection(event.shiftKey ? -1 : 1);
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
        <select v-model="filterColumn" aria-label="筛选列">
          <option v-for="column in columns" :key="column.name" :value="column.name">{{ column.name }}</option>
        </select>
        <select v-model="filterOp" aria-label="筛选条件">
          <option v-for="op in FILTER_OPS" :key="op.id" :value="op.id">{{ op.label }}</option>
        </select>
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
    <div ref="viewport" class="grid-scroll" @scroll.passive="onScroll" @keydown="onGridKeydown">
      <table class="grid-table">
        <thead>
          <tr>
            <th v-for="column in columns" :key="column.name" scope="col" :aria-sort="session.sort?.column === column.name ? (session.sort.direction === 'asc' ? 'ascending' : 'descending') : 'none'">
              <button class="th-btn" type="button" :aria-label="`按 ${column.name} 排序`" @click="session.cycleSort(column.name)">{{ column.name }}</button>
            </th>
          </tr>
        </thead>
        <tbody>
          <tr v-if="padTop" class="virtual-spacer" aria-hidden="true"><td :colspan="columns.length" :style="{ height: `${padTop}px` }"></td></tr>
          <tr v-if="loading">
            <td class="grid-empty" :colspan="columns.length">正在读取表数据…</td>
          </tr>
          <tr v-for="row in visibleRows" :key="row.id" :class="{ 'row-insert': row.kind === 'insert', 'row-delete': row.deleted, 'row-failed': row.failed }" :data-row-id="row.id" :data-located="row.located" :title="row.failure || undefined">
            <td v-for="(column, columnIndex) in columns" :key="column.name">
              <div v-if="editing && editing.id === row.id && editing.column === column.name" class="editor">
                <input :ref="focusEditor" v-model="editing.draft" :aria-label="`编辑 ${column.name}`" :disabled="editing.isNull" @keydown="onEditorKeydown" />
                <button class="btn sm" type="button" :disabled="!column.nullable" @click="editing.isNull = true">设为 NULL</button>
                <button class="btn sm" type="button" @click="editing.isNull = false">空字符串</button>
                <button class="btn sm primary" type="button" @click="saveEdit">确定</button>
                <button class="btn sm ghost" type="button" @click="cancelEdit">取消</button>
              </div>
              <button v-else class="cell-btn" :class="{ on: session.selectedId === row.id && session.selectedColumn === column.name }" :data-cell="`${row.id}:${column.name}`" type="button" @click="session.select(row.id, column.name)" @dblclick="beginEdit(row.id, column.name)">
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
    </template>
  </div>
</template>
