<script setup lang="ts">
import { computed, reactive, ref, watch } from "vue";
import Listbox from "../ui/Listbox.vue";
import type { ListboxOption } from "../ui/types";
import { dumpConnections, type DumpConnection } from "./dump";
import { chooseExportPath } from "./native";
import { runDatabaseDump } from "./tasks";

const props = defineProps<{ open: boolean }>();

const emit = defineEmits<{ close: [] }>();

const connections = computed(() => (props.open ? dumpConnections() : []));
const connectionOptions = computed<ListboxOption[]>(() =>
  connections.value.map((connection) => ({ value: connection.id, label: connection.name, hint: connection.engine })),
);

const connectionId = ref<string | null>(null);
const database = ref("");
const schemas = reactive({ checked: new Set<string>(), all: new Set<string>() });
const tableScope = reactive({ all: true, checked: new Set<string>() });
const includeData = ref(true);
const path = ref("");
const choosing = ref(false);
const error = ref("");

const connection = computed<DumpConnection | null>(() => connections.value.find((item) => item.id === connectionId.value) ?? connections.value[0] ?? null);
const databaseOptions = computed<ListboxOption[]>(() => {
  const current = connection.value;
  if (!current) return [];
  const preferred = current.defaultDatabase;
  return current.databases.map((name) => ({ value: name, label: name, hint: name === preferred ? "默认库" : undefined }));
});
const schemaList = computed<string[]>(() => connection.value?.schemasOf(database.value) ?? []);
const showsSchemas = computed(() => schemaList.value.length > 0);
const tableList = computed<string[]>(() => {
  const current = connection.value;
  if (!current) return [];
  const chosen = showsSchemas.value ? [...schemas.checked] : [];
  return current.tablesOf(database.value, chosen);
});
const connectionReady = computed(() => Boolean(connection.value));

watch(
  () => props.open,
  (open) => {
    if (!open) return;
    error.value = "";
    includeData.value = true;
    tableScope.all = true;
    tableScope.checked = new Set();
    const first = connections.value[0] ?? null;
    connectionId.value = first?.id ?? null;
    database.value = first ? defaultDatabase(first) : "";
    resetSchemas();
    const stamp = new Date().toISOString().slice(0, 10);
    const stem = database.value || "database";
    path.value = `${stem}-${stamp}.sql`;
  },
  { immediate: true },
);

watch(connection, (next, previous) => {
  if (!props.open || next?.id === previous?.id) return;
  database.value = next ? defaultDatabase(next) : "";
  resetSchemas();
});

watch(database, () => resetSchemas());

function defaultDatabase(connection: DumpConnection): string {
  const preferred = connection.defaultDatabase;
  if (preferred && connection.databases.includes(preferred)) return preferred;
  return connection.databases[0] ?? "";
}

function resetSchemas(): void {
  schemas.all = new Set(schemaList.value);
  schemas.checked = new Set(schemaList.value);
  tableScope.all = true;
  tableScope.checked = new Set();
}

function toggleSchema(name: string, event: Event): void {
  const checked = (event.target as HTMLInputElement).checked;
  if (checked) schemas.checked.add(name);
  else schemas.checked.delete(name);
  tableScope.all = true;
  tableScope.checked = new Set();
}

function toggleTable(name: string, event: Event): void {
  const checked = (event.target as HTMLInputElement).checked;
  if (checked) tableScope.checked.add(name);
  else tableScope.checked.delete(name);
}

const schemaMissing = computed(() => showsSchemas.value && schemas.checked.size === 0);
const pathReady = computed(() => path.value.trim().length > 0);
const canStart = computed(() => connectionReady.value && database.value.length > 0 && !schemaMissing.value && pathReady.value && !choosing.value);
const tableNote = computed(() => {
  if (tableList.value.length === 0) return "目录树里还没有读到表清单：将按全部表导出。";
  if (tableScope.all) return "按全部表导出。";
  const picked = tableScope.checked.size;
  return picked === 0 ? "没有勾选任何表，将按全部表导出。" : `只导出勾选的 ${picked} 张表；视图与例程仍全部导出。`;
});

async function start(): Promise<void> {
  if (!canStart.value || !connection.value) return;
  choosing.value = true;
  error.value = "";
  try {
    const selected = await chooseExportPath(path.value, "sql");
    if (!selected || !props.open) return;
    path.value = selected;
    const schemasPayload = showsSchemas.value ? [...schemas.checked] : [];
    const tablesPayload = tableScope.all || tableScope.checked.size === 0 ? [] : [...tableScope.checked];
    runDatabaseDump({
      id: `dump-${Date.now()}`,
      connectionId: connection.value.id,
      engine: connection.value.engine,
      database: database.value,
      schemas: schemasPayload,
      tables: tablesPayload,
      includeData: includeData.value,
      path: selected,
    });
    emit("close");
  } catch (cause) {
    error.value = `无法打开保存对话框：${cause instanceof Error ? cause.message : String(cause)}`;
  } finally {
    choosing.value = false;
  }
}
</script>

<template>
  <div v-if="open" class="dp-overlay" role="presentation">
    <div class="dp-sheet" role="dialog" aria-modal="true" aria-label="全库导出">
      <header class="dp-head">
        <div class="dp-titles">
          <h2>全库导出</h2>
          <p>把连接里的结构（可选数据）按依赖顺序写成一个 SQL 文件。这是导出，不是备份；生成过程只读，不执行任何语句。</p>
        </div>
        <button class="icon-btn" type="button" aria-label="关闭" @click="emit('close')">
          <svg class="i" viewBox="0 0 16 16" aria-hidden="true"><path d="M4 4l8 8M12 4l-8 8" /></svg>
        </button>
      </header>

      <div class="dp-body">
        <p v-if="error" class="dp-alert" role="alert">{{ error }}</p>
        <p v-if="!connectionReady" class="dp-note">还没有已打开的连接。先到连接页打开要导出的连接。</p>
        <template v-else>
          <div class="dp-grid">
            <div class="dp-field">
              <label for="dump-connection">连接</label>
              <Listbox id="dump-connection" v-model="connectionId" :options="connectionOptions" aria-label="连接" :disabled="choosing" />
            </div>
            <div class="dp-field">
              <label for="dump-database">库</label>
              <Listbox id="dump-database" v-model="database" :options="databaseOptions" aria-label="库" :disabled="choosing" />
            </div>
          </div>

          <fieldset v-if="showsSchemas" class="dp-block">
            <legend>schema 范围（PostgreSQL）</legend>
            <label v-for="name in schemaList" :key="name" class="dp-check">
              <input type="checkbox" :checked="schemas.checked.has(name)" @change="toggleSchema(name, $event)" />
              {{ name }}
            </label>
          </fieldset>

          <fieldset class="dp-block">
            <legend>表范围</legend>
            <label class="dp-check"><input v-model="tableScope.all" type="checkbox" /> 全部表</label>
            <details v-if="!tableScope.all && tableList.length > 0" class="dp-tables">
              <summary>勾选要导出的表（{{ tableList.length }}）</summary>
              <label v-for="name in tableList" :key="name" class="dp-check">
                <input type="checkbox" :checked="tableScope.checked.has(name)" @change="toggleTable(name, $event)" />
                {{ name }}
              </label>
            </details>
            <p class="dp-note">{{ tableNote }}</p>
          </fieldset>

          <label class="dp-check dp-data"><input v-model="includeData" type="checkbox" /> 包含数据（每张表的 INSERT 跟在各自的建表语句之后；不勾选则只导结构）</label>

          <label class="dp-path">
            保存到
            <input v-model="path" aria-label="导出路径" spellcheck="false" />
          </label>
        </template>
      </div>

      <footer class="dp-foot">
        <span class="muted sm">任务进入后台任务中心，可随时取消；取消后不留下未完成文件。</span>
        <span class="grow"></span>
        <button class="btn" type="button" @click="emit('close')">取消</button>
        <button class="btn primary" type="button" :disabled="!canStart" @click="start">{{ choosing ? "选择保存位置…" : "开始导出" }}</button>
      </footer>
    </div>
  </div>
</template>

<style scoped>
/* 与 ExportDialog 同一套 overlay 表面体系（对话框样式是 scoped，各自携带）。 */
.dp-overlay { position: fixed; inset: 0; background: var(--overlay); backdrop-filter: blur(3px); display: grid; place-items: center; z-index: var(--z-dialog); padding: var(--sp-4); }
.dp-sheet { width: min(560px, 100%); max-height: min(88vh, 720px); display: flex; flex-direction: column; background: var(--surface-overlay); color: var(--text); border-radius: var(--r-xl); border: 1px solid var(--stroke-strong); box-shadow: var(--shadow-lg); animation: modal-in var(--dur) var(--ease-out); }
.dp-head { display: flex; align-items: flex-start; gap: var(--sp-3); padding: var(--sp-4) var(--sp-5) var(--sp-3); border-bottom: 1px solid var(--line-2); }
.dp-titles { flex: 1; min-width: 0; }
.dp-titles h2 { margin: 0; font-size: var(--fs-lg); font-weight: 650; letter-spacing: -0.01em; }
.dp-titles p { margin: var(--sp-1) 0 0; font-size: var(--fs-sm); color: var(--text-2); line-height: var(--lh); }
.dp-body { padding: var(--sp-4) var(--sp-5); overflow: auto; flex: 1; min-height: 0; display: flex; flex-direction: column; gap: var(--sp-3); }
.dp-foot { display: flex; align-items: center; gap: var(--sp-2); padding: var(--sp-3) var(--sp-5); border-top: 1px solid var(--line-2); background: color-mix(in srgb, var(--surface-overlay) 94%, var(--text) 6%); border-radius: 0 0 var(--r-xl) var(--r-xl); }
.dp-grid { display: grid; grid-template-columns: 1fr 1fr; gap: var(--sp-3); }
.dp-field { display: flex; flex-direction: column; gap: var(--sp-1); font-size: var(--fs-xs); color: var(--text-2); min-width: 0; }
.dp-field :deep(.listbox) { width: 100%; }
.dp-field :deep(.listbox-trigger) { width: 100%; justify-content: space-between; }
.dp-block { margin: 0; padding: 0; border: 0; display: flex; flex-direction: column; gap: var(--sp-1); min-width: 0; }
.dp-block legend { padding: 0; margin-bottom: var(--sp-1); font-size: var(--fs-sm); font-weight: 600; color: var(--text-3); }
.dp-check { display: flex; align-items: center; gap: var(--sp-2); font-size: var(--fs-sm); color: var(--text); }
.dp-data { padding: var(--sp-2) var(--sp-3); border: 1px solid var(--line); border-radius: var(--r); background: var(--surface-panel); }
.dp-tables { max-height: 180px; overflow: auto; border: 1px solid var(--line); border-radius: var(--r); padding: var(--sp-2) var(--sp-3); display: flex; flex-direction: column; gap: var(--sp-1); }
.dp-tables summary { cursor: pointer; font-size: var(--fs-xs); color: var(--text-2); }
.dp-note { margin: 0; font-size: var(--fs-xs); color: var(--text-2); line-height: var(--lh); }
.dp-alert { display: flex; align-items: flex-start; gap: var(--sp-2); margin: 0; padding: var(--sp-2) var(--sp-3); background: var(--err-soft); color: var(--err); border: 1px solid var(--err-line); border-radius: var(--r); box-shadow: inset 3px 0 0 var(--err); font-size: var(--fs-sm); }
.dp-path { display: flex; flex-direction: column; gap: var(--sp-1); font-size: var(--fs-sm); color: var(--text-2); }
.dp-path input { width: 100%; font-family: var(--mono); font-size: var(--fs-xs); }
</style>
