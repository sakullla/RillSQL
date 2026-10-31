<script setup lang="ts">
import { computed, reactive, ref, watch, type Ref } from "vue";
import { tr as t } from "../i18n";
import { catalogConnections } from "../catalog/store";
import type { CatalogConnection } from "../catalog/types";
import { engineMeta } from "../engines";
import { compareCancel, compareData, compareSchemas, compareStructure } from "./api";
import type { ScriptTarget } from "./place";
import type { CompareSide, DataReport, StructureReport } from "./types";

const emit = defineEmits<{ close: []; place: [sql: string, target: ScriptTarget] }>();

const baselineId = ref("");
const targetId = ref("");
const baselineDatabase = ref("");
const targetDatabase = ref("");
const baselineSchema = ref("");
const targetSchema = ref("");
const busy = ref(false);
const runId = ref("");
const errorText = ref("");
const structure = ref<StructureReport | null>(null);
const dataReport = ref<DataReport | null>(null);
const selected = reactive<Record<string, boolean>>({});
const chosenKey = reactive<Record<string, string>>({});
const loadedSchemas = reactive<Record<string, string[]>>({});
const schemaState = reactive<Record<string, "loading" | "manual" | "ready">>({});
const schemaNote = reactive<Record<string, string>>({});

// 能力门控（ADR-2）：kv/document 引擎没有结构/数据对比，不出现在两侧连接里。
const connections = computed(() => catalogConnections().filter((item) => item.status === "connected" && engineMeta(item.engine).relational));

function connection(id: string): CatalogConnection | null {
  return connections.value.find((item) => item.id === id) ?? null;
}

function databasesOf(id: string): string[] {
  return connection(id)?.databases.map((item) => item.name) ?? [];
}

function schemasOf(id: string, database: string): string[] {
  const node = connection(id)?.databases.find((item) => item.name === database);
  return node?.schemas.map((item) => item.name).filter((name) => name.trim()) ?? [];
}

function schemaKey(id: string, database: string): string {
  return `${id}\u0000${database}`;
}

function schemaChoices(id: string, database: string): string[] {
  const catalogNames = schemasOf(id, database);
  if (catalogNames.length) return catalogNames;
  return loadedSchemas[schemaKey(id, database)] ?? [];
}

function schemaEntry(id: string, database: string): "choices" | "manual" | "loading" | "none" {
  const known = connection(id);
  if (!known || (known.engine !== "postgres" && known.engine !== "sqlserver") || !database.trim()) return "none";
  if (schemaChoices(id, database).length) return "choices";
  if (schemaState[schemaKey(id, database)] === "manual") return "manual";
  return "loading";
}

function schemaHint(id: string, database: string): string {
  if (schemaEntry(id, database) !== "manual") return "";
  const note = schemaNote[schemaKey(id, database)];
  if (note) return `${note}${t("compare.gates.schemaNoteSuffix")}`;
  return t("compare.gates.schemaNotInCatalog");
}

async function ensureSchemas(id: string, database: string, schema: Ref<string>) {
  const known = connection(id);
  const key = schemaKey(id, database);
  if (!known || (known.engine !== "postgres" && known.engine !== "sqlserver") || !database.trim() || schemasOf(id, database).length || schemaState[key]) return;
  schemaState[key] = "loading";
  const still = () => (schema === baselineSchema ? baselineId.value === id && baselineDatabase.value === database : targetId.value === id && targetDatabase.value === database);
  try {
    const names = (await compareSchemas(id, database.trim())).map((name) => name.trim()).filter(Boolean);
    if (schemaState[key] !== "loading") return;
    loadedSchemas[key] = names;
    schemaState[key] = names.length ? "ready" : "manual";
    if (still() && names.length && schema.value && !names.includes(schema.value)) schema.value = "";
  } catch (err) {
    if (schemaState[key] !== "loading") return;
    schemaState[key] = "manual";
    schemaNote[key] = err instanceof Error ? err.message : String(err);
  }
}

function dropUnavailableSchema(id: string, database: string, schema: Ref<string>) {
  const value = schema.value.trim();
  if (value && !schemaChoices(id, database).includes(value)) schema.value = "";
}

const schemaPending = computed(() => schemaEntry(baselineId.value, baselineDatabase.value) === "loading" || schemaEntry(targetId.value, targetDatabase.value) === "loading");

watch([baselineId, baselineDatabase], () => {
  dropUnavailableSchema(baselineId.value, baselineDatabase.value, baselineSchema);
  void ensureSchemas(baselineId.value, baselineDatabase.value, baselineSchema);
});

watch([targetId, targetDatabase], () => {
  dropUnavailableSchema(targetId.value, targetDatabase.value, targetSchema);
  void ensureSchemas(targetId.value, targetDatabase.value, targetSchema);
});

function side(id: string, database: string, schema: string): CompareSide | null {
  const known = connection(id);
  if (!known) return null;
  const resolvedDatabase = known.engine === "sqlite" ? "main" : database.trim();
  const resolvedSchema = known.engine === "postgres" || known.engine === "sqlserver" ? schema.trim() : "";
  return { connectionId: id, database: resolvedDatabase, schema: resolvedSchema, engine: known.engine };
}

function targetOf(): ScriptTarget {
  const known = connection(targetId.value);
  return {
    connectionId: known?.id ?? null,
    connectionName: known?.name ?? null,
    database: known?.engine === "sqlite" ? "main" : targetDatabase.value || null,
    engine: known?.engine ?? null,
    status: known?.status ?? null,
  };
}

function schemaRejected(id: string, database: string, schema: string): string | null {
  const known = connection(id);
  if (!known || (known.engine !== "postgres" && known.engine !== "sqlserver") || !database.trim()) return null;
  if (schemaEntry(id, database) === "loading") return t("compare.gates.schemaStillLoading");
  const choices = schemaChoices(id, database);
  const value = schema.trim();
  if (choices.length && value && !choices.includes(value)) return t("compare.gates.schemaBothNeeded");
  return null;
}

function refuseLocal(left: CompareSide, right: CompareSide): string | null {
  if (left.engine !== right.engine) return t("compare.gates.engineMismatch");
  if (!left.connectionId || !right.connectionId) return t("compare.gates.sideNotOpen");
  if (left.engine !== "sqlite" && (!left.database || !right.database)) return t("compare.gates.databasesNeeded");
  const pending = schemaRejected(baselineId.value, baselineDatabase.value, baselineSchema.value) ?? schemaRejected(targetId.value, targetDatabase.value, targetSchema.value);
  if (pending) return pending;
  if (left.engine === "postgres" && (!left.schema || !right.schema)) return t("compare.gates.schemaBothNeeded");
  if (left.engine === "sqlserver" && (!left.schema || !right.schema)) return t("compare.gates.sqlserverSchemaNeeded");
  return null;
}

async function runStructure() {
  const left = side(baselineId.value, baselineDatabase.value, baselineSchema.value);
  const right = side(targetId.value, targetDatabase.value, targetSchema.value);
  structure.value = null;
  dataReport.value = null;
  errorText.value = "";
  resetSelection();
  if (!left || !right) {
    errorText.value = t("compare.gates.chooseBoth");
    return;
  }
  const refused = refuseLocal(left, right);
  if (refused) {
    structure.value = { refused, script: "", incomplete: false, notes: [], onlyBaseline: [], onlyTarget: [], changed: [], tables: [] };
    return;
  }
  const id = `compare-${Date.now()}`;
  runId.value = id;
  busy.value = true;
  try {
    structure.value = await compareStructure({ id, baseline: left, target: right });
    for (const table of structure.value.tables) selected[table.name] = table.onBoth;
  } catch (err) {
    errorText.value = err instanceof Error ? err.message : String(err);
  } finally {
    busy.value = false;
  }
}

async function runData() {
  const left = side(baselineId.value, baselineDatabase.value, baselineSchema.value);
  const right = side(targetId.value, targetDatabase.value, targetSchema.value);
  dataReport.value = null;
  errorText.value = "";
  if (!left || !right) {
    errorText.value = t("compare.gates.chooseBoth");
    return;
  }
  const refused = refuseLocal(left, right);
  if (refused) {
    dataReport.value = { refused, script: "", incomplete: false, notes: [], diffs: [] };
    return;
  }
  const tables = (structure.value?.tables ?? [])
    .filter((table) => table.onBoth && selected[table.name])
    .map((table) => ({ name: table.name, key: chosenKey[table.name] || null }));
  if (tables.length === 0) {
    dataReport.value = { refused: t("compare.gates.noTablesPicked"), script: "", incomplete: false, notes: [], diffs: [] };
    return;
  }
  const id = `compare-data-${Date.now()}`;
  runId.value = id;
  busy.value = true;
  try {
    dataReport.value = await compareData({ id, baseline: left, target: right, tables });
  } catch (err) {
    errorText.value = err instanceof Error ? err.message : String(err);
  } finally {
    busy.value = false;
  }
}

function resetSelection() {
  for (const name of Object.keys(selected)) delete selected[name];
  for (const name of Object.keys(chosenKey)) delete chosenKey[name];
}

function cancelRun() {
  if (runId.value) void compareCancel(runId.value);
}

function place(sql: string) {
  if (!sql.trim()) return;
  emit("place", sql, targetOf());
}
</script>

<template>
  <div class="cmp-overlay" role="presentation" @click.self="emit('close')">
    <div class="cmp-sheet" role="dialog" aria-modal="true" :aria-label="t('compare.dialog.title')" data-compare>
      <header class="cmp-head">
        <div class="cmp-titles">
          <h2>{{ t("compare.dialog.title") }}</h2>
          <p class="cmp-sub">{{ t("compare.dialog.subtitle") }}</p>
        </div>
        <button class="icon-btn" type="button" :aria-label="t('common.close')" @click="emit('close')">
          <svg class="i" viewBox="0 0 16 16" aria-hidden="true"><path d="M4 4l8 8M12 4l-8 8" /></svg>
        </button>
      </header>
      <div class="cmp-body">
        <div class="compare-sides">
          <fieldset>
            <legend>{{ t("compare.dialog.baseline") }}</legend>
            <label>{{ t("compare.dialog.connection") }}<select v-model="baselineId" data-side="baseline" data-field="connection"><option value="">{{ t("compare.dialog.chooseConnection") }}</option><option v-for="item in connections" :key="item.id" :value="item.id">{{ item.name }} · {{ item.engine }}</option></select></label>
            <label v-if="connection(baselineId)?.engine !== 'sqlite'">{{ t("compare.dialog.database") }}<select v-model="baselineDatabase" data-side="baseline" data-field="database"><option value="">{{ t("compare.dialog.chooseDatabase") }}</option><option v-for="name in databasesOf(baselineId)" :key="name" :value="name">{{ name }}</option></select></label>
            <label v-if="connection(baselineId)?.engine === 'postgres'">schema
              <select v-if="schemaEntry(baselineId, baselineDatabase) === 'choices'" v-model="baselineSchema" data-side="baseline" data-field="schema"><option value="">{{ t("compare.dialog.chooseSchema") }}</option><option v-for="name in schemaChoices(baselineId, baselineDatabase)" :key="name" :value="name">{{ name }}</option></select>
              <input v-else-if="schemaEntry(baselineId, baselineDatabase) === 'manual'" v-model="baselineSchema" type="text" data-side="baseline" data-field="schema" :placeholder="t('compare.dialog.schemaManualPlaceholder')" autocomplete="off" />
              <span v-else-if="schemaEntry(baselineId, baselineDatabase) === 'loading'" class="muted" data-side="baseline" data-field="schema" data-schema-state="loading">{{ t("compare.dialog.schemaLoading") }}</span>
              <select v-else v-model="baselineSchema" data-side="baseline" data-field="schema"><option value="">{{ t("compare.dialog.chooseSchema") }}</option></select>
            </label>
            <p v-if="schemaHint(baselineId, baselineDatabase)" class="muted" data-side="baseline" data-schema-hint>{{ schemaHint(baselineId, baselineDatabase) }}</p>
          </fieldset>
          <fieldset>
            <legend>{{ t("compare.dialog.target") }}</legend>
            <label>{{ t("compare.dialog.connection") }}<select v-model="targetId" data-side="target" data-field="connection"><option value="">{{ t("compare.dialog.chooseConnection") }}</option><option v-for="item in connections" :key="item.id" :value="item.id">{{ item.name }} · {{ item.engine }}</option></select></label>
            <label v-if="connection(targetId)?.engine !== 'sqlite'">{{ t("compare.dialog.database") }}<select v-model="targetDatabase" data-side="target" data-field="database"><option value="">{{ t("compare.dialog.chooseDatabase") }}</option><option v-for="name in databasesOf(targetId)" :key="name" :value="name">{{ name }}</option></select></label>
            <label v-if="connection(targetId)?.engine === 'postgres'">schema
              <select v-if="schemaEntry(targetId, targetDatabase) === 'choices'" v-model="targetSchema" data-side="target" data-field="schema"><option value="">{{ t("compare.dialog.chooseSchema") }}</option><option v-for="name in schemaChoices(targetId, targetDatabase)" :key="name" :value="name">{{ name }}</option></select>
              <input v-else-if="schemaEntry(targetId, targetDatabase) === 'manual'" v-model="targetSchema" type="text" data-side="target" data-field="schema" :placeholder="t('compare.dialog.schemaManualPlaceholder')" autocomplete="off" />
              <span v-else-if="schemaEntry(targetId, targetDatabase) === 'loading'" class="muted" data-side="target" data-field="schema" data-schema-state="loading">{{ t("compare.dialog.schemaLoading") }}</span>
              <select v-else v-model="targetSchema" data-side="target" data-field="schema"><option value="">{{ t("compare.dialog.chooseSchema") }}</option></select>
            </label>
            <p v-if="schemaHint(targetId, targetDatabase)" class="muted" data-side="target" data-schema-hint>{{ schemaHint(targetId, targetDatabase) }}</p>
          </fieldset>
        </div>
        <div class="compare-actions">
          <button class="btn primary" type="button" data-action="compare-structure" :disabled="busy || schemaPending" @click="runStructure">{{ t("compare.dialog.compareStructure") }}</button>
          <button class="btn" type="button" data-action="compare-cancel" :disabled="!busy" @click="cancelRun">{{ t("common.cancel") }}</button>
          <span v-if="busy" class="cmp-hint">{{ t("compare.dialog.comparing") }}</span>
        </div>
        <p v-if="errorText" class="cmp-alert" role="alert">{{ errorText }}</p>
        <p v-if="structure?.refused" class="cmp-alert" role="alert" data-compare-refused>{{ structure.refused }}</p>
        <p v-if="structure?.incomplete" class="badge warn" data-compare-incomplete>{{ t("compare.dialog.scriptIncomplete") }}</p>
        <ul v-if="structure && !structure.refused" class="compare-list" data-compare-diffs>
          <li v-for="item in structure.onlyBaseline" :key="`b-${item.kind}-${item.name}`">{{ t("compare.dialog.onlyBaseline", { kind: item.kind, name: item.name }) }}</li>
          <li v-for="item in structure.onlyTarget" :key="`t-${item.kind}-${item.name}`">{{ t("compare.dialog.onlyTarget", { kind: item.kind, name: item.name }) }}</li>
          <li v-for="item in structure.changed" :key="`c-${item.kind}-${item.name}`">{{ item.kind }} {{ item.name }}：{{ item.detail }}</li>
        </ul>
        <template v-if="structure && !structure.refused">
          <label class="compare-script">{{ t("compare.dialog.structureScript") }}<textarea readonly :value="structure.script" data-compare-script></textarea></label>
          <button class="btn" type="button" data-action="place-structure" :disabled="!structure.script.trim()" @click="place(structure.script)">{{ t("compare.dialog.placeToQuery") }}</button>
        </template>
        <section v-if="structure && !structure.refused" class="compare-data">
          <h3>{{ t("compare.dialog.dataTitle") }}</h3>
          <p v-if="!structure.tables.length" class="muted">{{ t("compare.dialog.noTables") }}</p>
          <label v-for="table in structure.tables" :key="table.name" class="compare-table">
            <input v-model="selected[table.name]" type="checkbox" :disabled="!table.onBoth" :data-table="table.name" />
            <span class="mono">{{ table.name }}</span>
            <span v-if="!table.onBoth" class="muted">{{ t("compare.dialog.oneSideOnly") }}</span>
            <select v-if="table.needsKeyChoice" v-model="chosenKey[table.name]" :data-key="table.name">
              <option value="">{{ t("compare.dialog.chooseKey") }}</option>
              <option v-for="key in table.keys" :key="key.name" :value="key.name">{{ key.name }}（{{ key.columns.join(", ") }}）</option>
            </select>
          </label>
          <button class="btn" type="button" data-action="compare-data" :disabled="busy || schemaPending" @click="runData">{{ t("compare.dialog.compareData") }}</button>
          <p v-if="dataReport?.refused" class="cmp-alert" role="alert">{{ dataReport.refused }}</p>
          <p v-if="dataReport?.incomplete" class="badge warn">{{ t("compare.dialog.dataScriptIncomplete") }}</p>
          <ul v-if="dataReport" class="compare-list">
            <li v-for="(item, index) in dataReport.diffs" :key="`${item.table}-${item.action}-${index}`"><span class="mono">{{ item.table }}</span> {{ item.detail }} <template v-if="item.key">{{ t("compare.dialog.key") }} {{ item.key }}</template></li>
            <li v-for="(note, index) in dataReport.notes" :key="`n-${index}`">{{ note }}</li>
          </ul>
          <template v-if="dataReport && !dataReport.refused">
            <label class="compare-script">{{ t("compare.dialog.dataScript") }}<textarea readonly :value="dataReport.script" data-data-script></textarea></label>
            <button class="btn" type="button" data-action="place-data" :disabled="!dataReport.script.trim()" @click="place(dataReport.script)">{{ t("compare.dialog.placeToQuery") }}</button>
          </template>
        </section>
      </div>
    </div>
  </div>
</template>

<style scoped>
/* 对话框：与 base.css 的 dialog.modal 同一套 overlay 表面体系
   （--surface-overlay + --stroke-strong + --shadow-lg + --r-xl）。 */
.cmp-overlay { position: fixed; inset: 0; background: var(--overlay); backdrop-filter: blur(3px); display: grid; place-items: center; z-index: var(--z-dialog); padding: var(--sp-4); }
.cmp-sheet { width: min(880px, calc(100vw - 48px)); max-height: calc(100vh - 48px); display: flex; flex-direction: column; background: var(--surface-overlay); color: var(--text); border: 1px solid var(--stroke-strong); border-radius: var(--r-xl); box-shadow: var(--shadow-lg); animation: modal-in var(--dur) var(--ease-out); }
.cmp-head { display: flex; align-items: flex-start; gap: var(--sp-3); padding: var(--sp-4) var(--sp-5) var(--sp-3); border-bottom: 1px solid var(--line-2); }
.cmp-titles { flex: 1; min-width: 0; }
.cmp-titles h2 { margin: 0; font-size: var(--fs-lg); font-weight: 650; letter-spacing: -0.01em; }
.cmp-sub { margin: var(--sp-1) 0 0; font-size: var(--fs-sm); color: var(--text-2); }
.cmp-body { flex: 1; min-height: 0; overflow: auto; padding: var(--sp-4) var(--sp-5); display: flex; flex-direction: column; gap: var(--sp-3); }

/* 基准 / 对照两侧：panel 表面实体卡 */
.compare-sides { display: grid; grid-template-columns: 1fr 1fr; gap: var(--sp-3); }
.compare-sides fieldset { margin: 0; padding: var(--sp-3); border: 1px solid var(--stroke); border-radius: var(--r-md); background: var(--surface-panel); box-shadow: var(--shadow-sm); display: flex; flex-direction: column; gap: var(--sp-2); min-width: 0; }
.compare-sides legend { padding: 0 var(--sp-1); font-size: var(--fs-xs); font-weight: 600; letter-spacing: 0.06em; color: var(--text-3); }
.compare-sides label { display: flex; flex-direction: column; gap: var(--sp-1); font-size: var(--fs-sm); color: var(--text-2); }
.compare-sides input, .compare-sides select { width: 100%; }
.compare-sides .muted[data-schema-hint] { margin: 0; font-size: var(--fs-xs); line-height: var(--lh); }

/* 操作行与提示 */
.compare-actions { display: flex; align-items: center; gap: var(--sp-2); }
.cmp-hint { font-size: var(--fs-sm); color: var(--text-3); }
.cmp-alert { margin: 0; padding: var(--sp-2) var(--sp-3); border: 1px solid var(--err-line); border-radius: var(--r); background: var(--err-soft); color: var(--err); font-size: var(--fs-sm); line-height: var(--lh); }

/* 差异清单与脚本 */
.compare-list { margin: 0; padding: 0 var(--sp-1) 0 var(--sp-5); display: flex; flex-direction: column; gap: var(--sp-1); font-size: var(--fs-sm); color: var(--text-2); line-height: var(--lh); }
.compare-list .mono { color: var(--text); }
.compare-script { display: flex; flex-direction: column; gap: var(--sp-1); font-size: var(--fs-sm); color: var(--text-2); }
.compare-script textarea { min-height: 140px; padding: var(--sp-2) var(--sp-3); border: 1px solid var(--stroke-strong); border-radius: var(--r); background: var(--surface-sunken); font-family: var(--mono); font-size: var(--fs-xs); line-height: var(--lh); resize: vertical; }

/* 数据差异分区 */
.compare-data { display: flex; flex-direction: column; gap: var(--sp-2); padding-top: var(--sp-3); border-top: 1px solid var(--line-2); }
.compare-data h3 { margin: 0; font-size: var(--fs-sm); font-weight: 600; color: var(--text-3); }
.compare-table { display: flex; align-items: center; gap: var(--sp-2); padding: var(--sp-1) var(--sp-2); border-radius: var(--r-sm); font-size: var(--fs-sm); }
.compare-table:hover { background: var(--hover); }
.compare-table .mono { font-size: var(--fs-xs); }
.compare-table .muted { font-size: var(--fs-xs); }
.compare-table select { width: auto; min-width: 140px; }
@media (max-width: 720px) {
  .compare-sides { grid-template-columns: 1fr; }
}
</style>
