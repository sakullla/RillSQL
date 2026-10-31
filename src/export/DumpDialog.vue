<script setup lang="ts">
import { tr as t } from "../i18n";
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
  return current.databases.map((name) => ({ value: name, label: name, hint: name === preferred ? t("export.dump.defaultDb") : undefined }));
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
  if (tableList.value.length === 0) return t("export.dump.noTablesLoaded");
  if (tableScope.all) return t("export.dump.allTables");
  const picked = tableScope.checked.size;
  return picked === 0 ? t("export.dump.nonePicked") : t("export.dump.pickedOnly", { n: picked });
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
    error.value = t("export.common.dialogFailed", { error: cause instanceof Error ? cause.message : String(cause) });
  } finally {
    choosing.value = false;
  }
}
</script>

<template>
  <div v-if="open" class="dp-overlay" role="presentation">
    <div class="dp-sheet" role="dialog" aria-modal="true" :aria-label="t('export.dump.dialogLabel')">
      <header class="dp-head">
        <div class="dp-titles">
          <h2>{{ t("export.dump.title") }}</h2>
          <p>{{ t("export.dump.subtitle") }}</p>
        </div>
        <button class="icon-btn" type="button" :aria-label="t('common.close')" @click="emit('close')">
          <svg class="i" viewBox="0 0 16 16" aria-hidden="true"><path d="M4 4l8 8M12 4l-8 8" /></svg>
        </button>
      </header>

      <div class="dp-body">
        <p v-if="error" class="dp-alert" role="alert">{{ error }}</p>
        <p v-if="!connectionReady" class="dp-note">{{ t("export.common.needOpenConnection", { what: t("export.common.whatExport") }) }}</p>
        <template v-else>
          <div class="dp-grid">
            <div class="dp-field">
              <label for="dump-connection">{{ t("export.dump.connection") }}</label>
              <Listbox id="dump-connection" v-model="connectionId" :options="connectionOptions" :aria-label="t('export.dump.connection')" :disabled="choosing" />
            </div>
            <div class="dp-field">
              <label for="dump-database">{{ t("export.dump.database") }}</label>
              <Listbox id="dump-database" v-model="database" :options="databaseOptions" :aria-label="t('export.dump.database')" :disabled="choosing" />
            </div>
          </div>

          <fieldset v-if="showsSchemas" class="dp-block">
            <legend>{{ t("export.dump.schemaLegend") }}</legend>
            <label v-for="name in schemaList" :key="name" class="dp-check">
              <input type="checkbox" :checked="schemas.checked.has(name)" @change="toggleSchema(name, $event)" />
              {{ name }}
            </label>
          </fieldset>

          <fieldset class="dp-block">
            <legend>{{ t("export.dump.tableLegend") }}</legend>
            <label class="dp-check"><input v-model="tableScope.all" type="checkbox" /> {{ t("export.dump.allTablesCheck") }}</label>
            <details v-if="!tableScope.all && tableList.length > 0" class="dp-tables">
              <summary>{{ t("export.dump.pickTables", { n: tableList.length }) }}</summary>
              <label v-for="name in tableList" :key="name" class="dp-check">
                <input type="checkbox" :checked="tableScope.checked.has(name)" @change="toggleTable(name, $event)" />
                {{ name }}
              </label>
            </details>
            <p class="dp-note">{{ tableNote }}</p>
          </fieldset>

          <label class="dp-check dp-data"><input v-model="includeData" type="checkbox" /> {{ t("export.dump.includeData") }}</label>

          <label class="dp-path">
            {{ t("export.common.saveTo") }}
            <input v-model="path" :aria-label="t('export.common.exportPath')" spellcheck="false" />
          </label>
        </template>
      </div>

      <footer class="dp-foot">
        <span class="muted sm">{{ t("export.dump.taskNote") }}</span>
        <span class="grow"></span>
        <button class="btn" type="button" @click="emit('close')">{{ t("common.cancel") }}</button>
        <button class="btn primary" type="button" :disabled="!canStart" @click="start">{{ choosing ? t("export.common.chooseSaveLocation") : t("export.dump.start") }}</button>
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
