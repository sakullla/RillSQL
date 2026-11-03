<script setup lang="ts">
import { tr as t } from "../i18n";
import { computed, reactive, ref, watch } from "vue";
import Listbox from "../ui/Listbox.vue";
import type { ListboxOption } from "../ui/types";
import { gridDescribe } from "../grid/api";
import type { TableSchema } from "../grid/types";
import { dumpConnections, type DumpConnection } from "./dump";
import { migrationHint, suggestMigrationMapping } from "./migrate";
import { runMigrateTask } from "./tasks";

const props = defineProps<{ open: boolean }>();

const emit = defineEmits<{ close: [] }>();

interface Side {
  connection: DumpConnection | null;
  connectionId: string | null;
  database: string;
  schema: string;
  table: string;
  schemaOptions: ListboxOption[];
  tableOptions: ListboxOption[];
  columns: TableSchema | null;
  loading: boolean;
}

const connections = computed(() => (props.open ? dumpConnections() : []));
const connectionOptions = computed<ListboxOption[]>(() =>
  connections.value.map((connection) => ({ value: connection.id, label: connection.name, hint: connection.engine })),
);

const source = reactive<Side>(blankSide());
const target = reactive<Side>(blankSide());
/** 源列名 → 目标列名；空字符串表示不迁移这一列。 */
const mapping = ref<Record<string, string>>({});
const acknowledged = ref(false);
const warned = ref(false);
const starting = ref(false);
const error = ref("");

function blankSide(): Side {
  return {
    connection: null,
    connectionId: null,
    database: "",
    schema: "",
    table: "",
    schemaOptions: [],
    tableOptions: [],
    columns: null,
    loading: false,
  };
}

function defaultDatabase(connection: DumpConnection): string {
  const preferred = connection.defaultDatabase;
  if (preferred && connection.databases.includes(preferred)) return preferred;
  return connection.databases[0] ?? "";
}

function sideOf(connectionId: string | null): DumpConnection | null {
  return connections.value.find((item) => item.id === connectionId) ?? null;
}

function resetSide(side: Side, connectionId: string | null): void {
  const connection = sideOf(connectionId);
  side.connection = connection;
  side.connectionId = connection?.id ?? null;
  side.database = connection ? defaultDatabase(connection) : "";
  side.table = "";
  side.columns = null;
  refreshLists(side);
  side.schema = side.schemaOptions[0]?.value ?? "";
}

function refreshLists(side: Side): void {
  const connection = side.connection;
  side.schemaOptions = (connection?.schemasOf(side.database) ?? []).map((name) => ({ value: name, label: name }));
  const schemas = side.schemaOptions.map((item) => item.value);
  side.tableOptions = (connection?.tablesOf(side.database, schemas) ?? []).map((name) => ({ value: name, label: name }));
}

function onConnection(side: Side, value: string | null): void {
  resetSide(side, value);
  resetMapping();
}

function onDatabase(side: Side): void {
  side.table = "";
  side.columns = null;
  refreshLists(side);
  side.schema = side.schemaOptions[0]?.value ?? "";
  resetMapping();
}

function onSchema(side: Side): void {
  side.table = "";
  side.columns = null;
  refreshLists(side);
  resetMapping();
}

function onTable(side: Side): void {
  side.columns = null;
  resetMapping();
  void loadColumns();
}

function sideTarget(side: Side) {
  const connection = side.connection;
  if (!connection || !side.table) return null;
  return {
    connectionId: connection.id,
    engine: connection.engine,
    database: side.database,
    schema: side.schema,
    table: side.table,
  };
}

async function loadColumns(): Promise<void> {
  const sourceTarget = sideTarget(source);
  const targetTarget = sideTarget(target);
  if (!sourceTarget || !targetTarget) return;
  source.loading = true;
  target.loading = true;
  try {
    const [sourceSchema, targetSchema] = await Promise.all([gridDescribe(sourceTarget), gridDescribe(targetTarget)]);
    if (!props.open) return;
    if (sideTarget(source)?.table !== sourceTarget.table || sideTarget(target)?.table !== targetTarget.table) return;
    source.columns = sourceSchema;
    target.columns = targetSchema;
    mapping.value = sourceSchema.columns.length > 0 ? suggestMigrationMapping(sourceSchema.columns, targetSchema.columns) : {};
  } catch (cause) {
    if (!props.open) return;
    source.columns = null;
    target.columns = null;
    error.value = t("export.common.schemaReadFailed", { error: cause instanceof Error ? cause.message : String(cause) });
  } finally {
    source.loading = false;
    target.loading = false;
  }
}

function resetMapping(): void {
  mapping.value = {};
  acknowledged.value = false;
  warned.value = false;
  error.value = "";
}

const mappingRows = computed(() => {
  const sourceColumns = source.columns?.columns ?? [];
  const targetColumns = target.columns?.columns ?? [];
  return sourceColumns.map((column) => {
    const name = mapping.value[column.name] ?? "";
    const to = targetColumns.find((item) => item.name === name);
    return {
      column,
      targetName: name,
      hint: migrationHint(column, to),
      label: to ? to.typeName : "",
    };
  });
});

const targetColumnOptions = computed<ListboxOption[]>(() => [
  { value: "", label: t("export.migrate.skip") },
  ...(target.columns?.columns ?? []).map((column) => ({ value: column.name, label: column.name, hint: column.typeName })),
]);

const warningCount = computed(() => mappingRows.value.filter((row) => row.targetName && row.hint).length);
const mappedCount = computed(() => mappingRows.value.filter((row) => row.targetName).length);
const sameTable = computed(() => {
  const sourceTarget = sideTarget(source);
  const targetTarget = sideTarget(target);
  return Boolean(sourceTarget && targetTarget && sourceTarget.connectionId === targetTarget.connectionId && sourceTarget.table === targetTarget.table);
});
const ready = computed(() =>
  Boolean(
    source.connection &&
      target.connection &&
      source.table &&
      target.table &&
      source.columns &&
      target.columns &&
      mappedCount.value > 0 &&
      acknowledged.value &&
      (warningCount.value === 0 || warned.value) &&
      !source.loading &&
      !target.loading &&
      !starting.value,
  ),
);
const startNote = computed(() => {
  if (sameTable.value) return t("export.migrate.sameTable");
  if (warningCount.value > 0) return t("export.migrate.warningCount", { n: warningCount.value });
  return t("export.migrate.taskNote");
});

function start(): void {
  if (!ready.value || !source.connection || !target.connection) return;
  starting.value = true;
  try {
    const pairs = Object.entries(mapping.value)
      .filter(([, targetName]) => targetName)
      .map(([sourceName, targetName]) => ({ source: sourceName, target: targetName }));
    runMigrateTask({
      id: `migrate-${Date.now()}`,
      source: {
        connectionId: source.connection.id,
        engine: source.connection.engine,
        database: source.database,
        schema: source.schema,
        table: source.table,
      },
      target: {
        connectionId: target.connection.id,
        engine: target.connection.engine,
        database: target.database,
        schema: target.schema,
        table: target.table,
      },
      mapping: pairs,
      options: null,
    });
    emit("close");
  } finally {
    starting.value = false;
  }
}

watch(
  () => props.open,
  (open) => {
    if (!open) return;
    Object.assign(source, blankSide());
    Object.assign(target, blankSide());
    mapping.value = {};
    acknowledged.value = false;
    warned.value = false;
    error.value = "";
    const first = connections.value[0] ?? null;
    resetSide(source, first?.id ?? null);
    const second = connections.value.find((item) => item.id !== first?.id) ?? first;
    resetSide(target, second?.id ?? null);
  },
  { immediate: true },
);
</script>

<template>
  <div v-if="open" class="mg-overlay" role="presentation">
    <div class="mg-sheet" role="dialog" aria-modal="true" :aria-label="t('export.migrate.dialogLabel')">
      <header class="mg-head">
        <div class="mg-titles">
          <h2>{{ t("export.migrate.title") }}</h2>
          <p>{{ t("export.migrate.subtitle") }}</p>
        </div>
        <button class="icon-btn" type="button" :aria-label="t('common.close')" @click="emit('close')">
          <svg class="i" viewBox="0 0 16 16" aria-hidden="true"><path d="M4 4l8 8M12 4l-8 8" /></svg>
        </button>
      </header>

      <div class="mg-body">
        <p v-if="error" class="mg-alert" role="alert">{{ error }}</p>
        <p v-if="connections.length === 0" class="mg-note">{{ t("export.common.needOpenConnection", { what: t("export.common.whatBoth") }) }}</p>
        <template v-else>
          <div class="mg-sides">
            <fieldset class="mg-side">
              <legend>{{ t("export.migrate.sourceLegend") }}</legend>
              <label class="mg-field">{{ t("export.migrate.connection") }}
                <Listbox :id="'migrate-source-connection'" v-model="source.connectionId" :options="connectionOptions" :aria-label="t('export.migrate.sourceConnection')" @update:model-value="onConnection(source, $event)" />
              </label>
              <label class="mg-field">{{ t("export.migrate.database") }}
                <Listbox :id="'migrate-source-database'" v-model="source.database" :options="(source.connection?.databases ?? []).map((name) => ({ value: name, label: name }))" :aria-label="t('export.migrate.sourceDatabase')" @update:model-value="onDatabase(source)" />
              </label>
              <label v-if="source.schemaOptions.length > 0" class="mg-field">schema
                <Listbox :id="'migrate-source-schema'" v-model="source.schema" :options="source.schemaOptions" :aria-label="t('export.migrate.sourceSchema')" @update:model-value="onSchema(source)" />
              </label>
              <label class="mg-field">{{ t("export.migrate.table") }}
                <Listbox :id="'migrate-source-table'" v-model="source.table" :options="source.tableOptions" :aria-label="t('export.migrate.sourceTable')" @update:model-value="onTable(source)" />
              </label>
            </fieldset>
            <fieldset class="mg-side">
              <legend>{{ t("export.migrate.targetLegend") }}</legend>
              <label class="mg-field">{{ t("export.migrate.connection") }}
                <Listbox :id="'migrate-target-connection'" v-model="target.connectionId" :options="connectionOptions" :aria-label="t('export.migrate.targetConnection')" @update:model-value="onConnection(target, $event)" />
              </label>
              <label class="mg-field">{{ t("export.migrate.database") }}
                <Listbox :id="'migrate-target-database'" v-model="target.database" :options="(target.connection?.databases ?? []).map((name) => ({ value: name, label: name }))" :aria-label="t('export.migrate.targetDatabase')" @update:model-value="onDatabase(target)" />
              </label>
              <label v-if="target.schemaOptions.length > 0" class="mg-field">schema
                <Listbox :id="'migrate-target-schema'" v-model="target.schema" :options="target.schemaOptions" :aria-label="t('export.migrate.targetSchema')" @update:model-value="onSchema(target)" />
              </label>
              <label class="mg-field">{{ t("export.migrate.table") }}
                <Listbox :id="'migrate-target-table'" v-model="target.table" :options="target.tableOptions" :aria-label="t('export.migrate.targetTable')" @update:model-value="onTable(target)" />
              </label>
            </fieldset>
          </div>

          <p v-if="source.table && target.table && source.loading" class="mg-note">{{ t("export.migrate.loadingBoth") }}</p>
          <template v-if="mappingRows.length > 0">
            <fieldset class="mg-mapping">
              <legend>{{ t("export.migrate.mappingLegend", { mapped: mappedCount, total: mappingRows.length }) }}</legend>
              <div v-for="row in mappingRows" :key="row.column.name" class="mg-map-row">
                <span class="mg-map-src" :title="row.column.typeName">{{ row.column.name }}<em>{{ row.column.typeName }}</em></span>
                <span class="mg-map-arrow" aria-hidden="true">→</span>
                <Listbox
                  :id="`migrate-map-${row.column.name}`"
                  :model-value="row.targetName"
                  :options="targetColumnOptions"
                  :aria-label="t('export.migrate.mapAria', { column: row.column.name })"
                  @update:model-value="mapping[row.column.name] = String($event ?? '')"
                />
                <span v-if="row.targetName && row.hint" class="mg-hint" role="note">{{ row.hint }}</span>
                <span v-else-if="row.targetName && row.label" class="mg-hint ok">{{ row.label }}</span>
              </div>
            </fieldset>

            <label class="mg-check"><input v-model="acknowledged" type="checkbox" /> {{ t("export.migrate.ackWrite") }}</label>
            <label v-if="warningCount > 0" class="mg-check"><input v-model="warned" type="checkbox" /> {{ t("export.migrate.ackTypes") }}</label>
            <p class="mg-note">{{ startNote }}</p>
          </template>
          <p v-else-if="source.table && target.table && !source.loading && !error" class="mg-note">{{ t("export.migrate.sourceNoColumns") }}</p>
        </template>
      </div>

      <footer class="mg-foot">
        <span class="muted sm">{{ t("export.migrate.taskDoneNote") }}</span>
        <span class="grow"></span>
        <button class="btn" type="button" @click="emit('close')">{{ t("common.cancel") }}</button>
        <button class="btn primary" type="button" :disabled="!ready" @click="start">{{ starting ? t("export.migrate.starting") : t("export.migrate.start") }}</button>
      </footer>
    </div>
  </div>
</template>

<style scoped>
/* 与 DumpDialog 同一套 overlay 表面体系。 */
.mg-overlay { position: fixed; inset: 0; background: var(--overlay); backdrop-filter: blur(3px); display: grid; place-items: center; z-index: var(--z-dialog); padding: var(--sp-4); }
.mg-sheet { width: min(680px, 100%); max-height: min(88vh, 760px); display: flex; flex-direction: column; background: var(--surface-overlay); color: var(--text); border-radius: var(--r-xl); border: 1px solid var(--stroke-strong); box-shadow: var(--shadow-lg); animation: modal-in var(--dur) var(--ease-out); }
.mg-head { display: flex; align-items: flex-start; gap: var(--sp-3); padding: var(--sp-4) var(--sp-5) var(--sp-3); border-bottom: 1px solid var(--line-2); }
.mg-titles { flex: 1; min-width: 0; }
.mg-titles h2 { margin: 0; font-size: var(--fs-lg); font-weight: 650; letter-spacing: -0.01em; }
.mg-titles p { margin: var(--sp-1) 0 0; font-size: var(--fs-sm); color: var(--text-2); line-height: var(--lh); }
.mg-body { padding: var(--sp-4) var(--sp-5); overflow: auto; flex: 1; min-height: 0; display: flex; flex-direction: column; gap: var(--sp-3); }
.mg-foot { display: flex; align-items: center; gap: var(--sp-2); padding: var(--sp-3) var(--sp-5); border-top: 1px solid var(--line-2); background: color-mix(in srgb, var(--surface-overlay) 94%, var(--text) 6%); border-radius: 0 0 var(--r-xl) var(--r-xl); }
.mg-sides { display: grid; grid-template-columns: 1fr 1fr; gap: var(--sp-3); }
.mg-side { margin: 0; padding: var(--sp-3); border: 1px solid var(--line); border-radius: var(--r); display: flex; flex-direction: column; gap: var(--sp-2); min-width: 0; }
.mg-side legend { padding: 0 var(--sp-1); font-size: var(--fs-xs); font-weight: 600; color: var(--text-3); }
.mg-field { display: flex; flex-direction: column; gap: var(--sp-1); font-size: var(--fs-xs); color: var(--text-2); min-width: 0; }
.mg-field :deep(.listbox) { width: 100%; }
.mg-field :deep(.listbox-trigger) { width: 100%; justify-content: space-between; }
.mg-mapping { margin: 0; padding: var(--sp-3); border: 1px solid var(--line); border-radius: var(--r); display: flex; flex-direction: column; gap: var(--sp-2); max-height: 260px; overflow: auto; }
.mg-mapping legend { padding: 0 var(--sp-1); font-size: var(--fs-xs); font-weight: 600; color: var(--text-3); }
.mg-map-row { display: grid; grid-template-columns: minmax(120px, 1fr) auto minmax(140px, 1fr); gap: var(--sp-2); align-items: center; font-size: var(--fs-sm); }
.mg-map-row .mg-hint { grid-column: 1 / -1; }
.mg-map-src { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-family: var(--mono); font-size: var(--fs-xs); }
.mg-map-src em { font-style: normal; color: var(--text-3); margin-left: var(--sp-1); }
.mg-map-arrow { color: var(--text-3); }
.mg-map-row :deep(.listbox) { width: 100%; }
.mg-map-row :deep(.listbox-trigger) { width: 100%; justify-content: space-between; }
.mg-hint { margin: 0; font-size: var(--fs-xs); color: var(--warn); line-height: var(--lh); }
.mg-hint.ok { color: var(--text-3); }
.mg-check { display: flex; align-items: center; gap: var(--sp-2); font-size: var(--fs-sm); color: var(--text); }
.mg-note { margin: 0; font-size: var(--fs-xs); color: var(--text-2); line-height: var(--lh); }
.mg-alert { display: flex; align-items: flex-start; gap: var(--sp-2); margin: 0; padding: var(--sp-2) var(--sp-3); background: var(--err-soft); color: var(--err); border: 1px solid var(--err-line); border-radius: var(--r); box-shadow: inset 3px 0 0 var(--err); font-size: var(--fs-sm); }
</style>
