<script setup lang="ts">
import { computed, ref } from "vue";
import Listbox from "../ui/Listbox.vue";
import type { ListboxOption } from "../ui/types";
import { gridDescribe } from "../grid/api";
import type { TableTarget } from "../grid/target";
import type { TableSchema } from "../grid/types";
import { dumpConnections, type DumpConnection } from "./dump";
import ImportDialog from "./ImportDialog.vue";
import type { ImportSeed } from "./import";

/**
 * 拖放「继续导入」的目标选择：先选连接/库/schema/表（复用 dump 的已打开连接视图与 grid_describe），
 * 描述成功后把拖放读到的文本交给现有 ImportDialog 走同一条列映射与逐行写入流程。
 */
const props = defineProps<{ seed: ImportSeed }>();

const emit = defineEmits<{ close: [] }>();

const connections = computed(() => dumpConnections());
const connectionOptions = computed<ListboxOption[]>(() =>
  connections.value.map((connection) => ({ value: connection.id, label: connection.name, hint: connection.engine })),
);

const connectionId = ref("");
const database = ref("");
const schema = ref("");
const table = ref("");
const schemaOptions = ref<ListboxOption[]>([]);
const tableOptions = ref<ListboxOption[]>([]);
const described = ref<TableSchema | null>(null);
const loading = ref(false);
const error = ref("");

const connection = computed<DumpConnection | null>(() => connections.value.find((item) => item.id === connectionId.value) ?? null);

function defaultDatabase(item: DumpConnection): string {
  const preferred = item.defaultDatabase;
  if (preferred && item.databases.includes(preferred)) return preferred;
  return item.databases[0] ?? "";
}

/** 连接/库/schema 变化后刷新可选表；表列表来自目录树已读取的对象（与迁移对话框同一来源）。 */
function refreshLists(): void {
  schemaOptions.value = (connection.value?.schemasOf(database.value) ?? []).map((name) => ({ value: name, label: name }));
  const schemas = schemaOptions.value.map((item) => item.value);
  if (schema.value && !schemas.includes(schema.value)) schema.value = schemas[0] ?? "";
  tableOptions.value = (connection.value?.tablesOf(database.value, schemas) ?? []).map((name) => ({ value: name, label: name }));
}

function resetConnection(id: string): void {
  connectionId.value = id;
  const item = connection.value;
  database.value = item ? defaultDatabase(item) : "";
  schema.value = "";
  table.value = "";
  described.value = null;
  error.value = "";
  refreshLists();
  schema.value = schemaOptions.value[0]?.value ?? "";
}

const target = computed<TableTarget | null>(() => {
  const item = connection.value;
  if (!item || !table.value) return null;
  return { connectionId: item.id, engine: item.engine, database: database.value, schema: schema.value, table: table.value };
});

function sameTarget(left: TableTarget | null, right: TableTarget | null): boolean {
  return Boolean(
    left &&
      right &&
      left.connectionId === right.connectionId &&
      left.database === right.database &&
      left.schema === right.schema &&
      left.table === right.table,
  );
}

/** ImportDialog 的表选择列表：当前库当前 schema 下的表（schema 为空表示无 schema 引擎）。 */
const tables = computed(() => tableOptions.value.map((option) => ({ schema: schema.value, table: option.value })));

async function onTable(): Promise<void> {
  described.value = null;
  error.value = "";
  const request = target.value;
  if (!request) return;
  loading.value = true;
  try {
    const schemaOfTable = await gridDescribe(request);
    if (!sameTarget(target.value, request)) return;
    if (schemaOfTable.kind !== "table") {
      error.value = "只有数据表可以导入，不能从文件建表，也不能写入视图。";
      return;
    }
    described.value = schemaOfTable;
  } catch (cause) {
    if (!sameTarget(target.value, request)) return;
    error.value = `读取表结构失败：${cause instanceof Error ? cause.message : String(cause)}`;
  } finally {
    loading.value = false;
  }
}

function onConnection(value: string) {
  resetConnection(value);
}

function onDatabase() {
  table.value = "";
  described.value = null;
  error.value = "";
  refreshLists();
}

function onSchema() {
  table.value = "";
  described.value = null;
  error.value = "";
  refreshLists();
}

// 挂载即默认选第一个已打开的连接；组件由 v-if 控制，每次拖放重新开始。
resetConnection(connections.value[0]?.id ?? "");
</script>

<template>
  <div class="di-overlay" role="presentation">
    <div class="di-sheet" role="dialog" aria-modal="true" aria-label="选择导入目标">
      <header class="di-head">
        <div class="di-titles">
          <h2>导入拖放的文件</h2>
          <p>选择要把 <b class="mono">{{ seed.fileName ?? "文件" }}</b> 写进哪张已有的表。目标选定后进入与导入对话框一致的列映射流程。</p>
        </div>
        <button class="icon-btn" type="button" aria-label="关闭" @click="emit('close')">
          <svg class="i" viewBox="0 0 16 16" aria-hidden="true"><path d="M4 4l8 8M12 4l-8 8" /></svg>
        </button>
      </header>
      <div class="di-body">
        <p v-if="error" class="di-alert" role="alert">{{ error }}</p>
        <p v-if="connections.length === 0" class="di-note">还没有已打开的连接。先到连接页打开要导入的连接，再重新拖放文件。</p>
        <fieldset v-else class="di-picker">
          <legend>导入到哪张表</legend>
          <label class="di-field">连接
            <Listbox id="drag-import-connection" v-model="connectionId" :options="connectionOptions" aria-label="导入的连接" @update:model-value="onConnection" />
          </label>
          <label class="di-field">库
            <Listbox id="drag-import-database" v-model="database" :options="(connection?.databases ?? []).map((name) => ({ value: name, label: name }))" aria-label="导入的库" @update:model-value="onDatabase" />
          </label>
          <label v-if="schemaOptions.length > 0" class="di-field">schema
            <Listbox id="drag-import-schema" v-model="schema" :options="schemaOptions" aria-label="导入的 schema" @update:model-value="onSchema" />
          </label>
          <label class="di-field">表
            <Listbox id="drag-import-table" v-model="table" :options="tableOptions" aria-label="导入的表" @update:model-value="() => void onTable()" />
          </label>
          <p v-if="loading" class="di-note">正在读取表结构…</p>
        </fieldset>
      </div>
      <footer class="di-foot">
        <span class="muted sm">取消不会写入任何数据</span>
        <span class="grow"></span>
        <button class="btn" type="button" @click="emit('close')">取消</button>
      </footer>
    </div>
  </div>
  <!-- 描述成功后：现有 ImportDialog 顶上前台，携带拖放文本走同一条解析与列映射流程。 -->
  <ImportDialog
    v-if="described && target"
    :open="true"
    :target="target"
    :schema="described"
    :tables="tables"
    :seed="seed"
    @close="emit('close')"
    @done="() => {}"
  />
</template>

<style scoped>
/* 与导入/迁移对话框同一套 overlay 表面体系。 */
.di-overlay { position: fixed; inset: 0; background: var(--overlay); backdrop-filter: blur(3px); display: grid; place-items: center; z-index: var(--z-dialog); padding: var(--sp-4); }
.di-sheet { width: min(560px, 100%); max-height: min(88vh, 640px); display: flex; flex-direction: column; background: var(--surface-overlay); color: var(--text); border-radius: var(--r-xl); border: 1px solid var(--stroke-strong); box-shadow: var(--shadow-lg); animation: modal-in var(--dur) var(--ease-out); }
.di-head { display: flex; align-items: flex-start; gap: var(--sp-3); padding: var(--sp-4) var(--sp-5) var(--sp-3); border-bottom: 1px solid var(--line-2); }
.di-titles { flex: 1; min-width: 0; }
.di-titles h2 { margin: 0; font-size: var(--fs-lg); font-weight: 650; letter-spacing: -0.01em; }
.di-titles p { margin: var(--sp-1) 0 0; font-size: var(--fs-sm); color: var(--text-2); line-height: var(--lh); }
.di-titles .mono { font-size: var(--fs-xs); }
.di-body { padding: var(--sp-4) var(--sp-5); overflow: auto; flex: 1; min-height: 0; display: flex; flex-direction: column; gap: var(--sp-3); }
.di-foot { display: flex; align-items: center; gap: var(--sp-2); padding: var(--sp-3) var(--sp-5); border-top: 1px solid var(--line-2); background: color-mix(in srgb, var(--surface-overlay) 94%, var(--text) 6%); border-radius: 0 0 var(--r-xl) var(--r-xl); }
.di-picker { margin: 0; padding: var(--sp-3); border: 1px solid var(--line); border-radius: var(--r); display: flex; flex-direction: column; gap: var(--sp-2); min-width: 0; }
.di-picker legend { padding: 0; margin-bottom: var(--sp-1); font-size: var(--fs-xs); font-weight: 600; color: var(--text-3); }
.di-field { display: flex; flex-direction: column; gap: var(--sp-1); font-size: var(--fs-xs); color: var(--text-2); min-width: 0; }
.di-field :deep(.listbox) { width: 100%; }
.di-field :deep(.listbox-trigger) { width: 100%; justify-content: space-between; }
.di-note { margin: 0; font-size: var(--fs-xs); color: var(--text-2); line-height: var(--lh); }
.di-alert { display: flex; align-items: flex-start; gap: var(--sp-2); margin: 0; padding: var(--sp-2) var(--sp-3); background: var(--err-soft); color: var(--err); border: 1px solid var(--err-line); border-radius: var(--r); box-shadow: inset 3px 0 0 var(--err); font-size: var(--fs-sm); }
.grow { flex: 1; }
</style>
