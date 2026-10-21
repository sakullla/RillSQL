<script setup lang="ts">
import { computed, reactive, ref, watch, type Ref } from "vue";
import "../structure/structure.css";
import { catalogConnections } from "../catalog/store";
import type { CatalogConnection } from "../catalog/types";
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

const connections = computed(() => catalogConnections().filter((item) => item.status === "connected"));

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
  if (!known || known.engine !== "postgres" || !database.trim()) return "none";
  if (schemaChoices(id, database).length) return "choices";
  if (schemaState[schemaKey(id, database)] === "manual") return "manual";
  return "loading";
}

function schemaHint(id: string, database: string): string {
  if (schemaEntry(id, database) !== "manual") return "";
  const note = schemaNote[schemaKey(id, database)];
  if (note) return `${note} 仍可填写已有 schema，不会改连当前会话。`;
  return "目录里没有这个库的 schema，填写已有名称。不会改连当前会话。";
}

async function ensureSchemas(id: string, database: string, schema: Ref<string>) {
  const known = connection(id);
  const key = schemaKey(id, database);
  if (!known || known.engine !== "postgres" || !database.trim() || schemasOf(id, database).length || schemaState[key]) return;
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

watch([baselineId, baselineDatabase], () => {
  const names = schemasOf(baselineId.value, baselineDatabase.value);
  if (names.length && baselineSchema.value && !names.includes(baselineSchema.value)) baselineSchema.value = "";
  void ensureSchemas(baselineId.value, baselineDatabase.value, baselineSchema);
});

watch([targetId, targetDatabase], () => {
  const names = schemasOf(targetId.value, targetDatabase.value);
  if (names.length && targetSchema.value && !names.includes(targetSchema.value)) targetSchema.value = "";
  void ensureSchemas(targetId.value, targetDatabase.value, targetSchema);
});

function side(id: string, database: string, schema: string): CompareSide | null {
  const known = connection(id);
  if (!known) return null;
  const resolvedDatabase = known.engine === "sqlite" ? "main" : database.trim();
  const resolvedSchema = known.engine === "postgres" ? schema.trim() : "";
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

function refuseLocal(left: CompareSide, right: CompareSide): string | null {
  if (left.engine !== right.engine) return "两侧引擎不同。MySQL 与 MariaDB 是不同引擎，不生成脚本。";
  if (!left.connectionId || !right.connectionId) return "有一侧还没有打开，不生成脚本。";
  if (left.engine !== "sqlite" && (!left.database || !right.database)) return "需要为两侧选择数据库，不生成脚本。";
  if (left.engine === "postgres" && (!left.schema || !right.schema)) return "PostgreSQL 需要为基准和对照各选一个 schema，不生成脚本。";
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
    errorText.value = "请选择已打开的基准和对照。";
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
    errorText.value = "请选择已打开的基准和对照。";
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
    dataReport.value = { refused: "没有选择要对比的表，不生成脚本。", script: "", incomplete: false, notes: [], diffs: [] };
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
  <div class="overlay" role="presentation" @click.self="emit('close')">
    <div class="sheet compare-sheet" role="dialog" aria-modal="true" aria-label="对比库" data-compare>
      <header>
        <h2>对比库</h2>
        <span class="grow"></span>
        <button class="btn" type="button" @click="emit('close')">关闭</button>
      </header>
      <div class="body">
        <p class="muted">基准是期望的一侧。脚本只描述如何让对照变成基准，生成时不执行。</p>
        <div class="compare-sides">
          <fieldset>
            <legend>基准</legend>
            <label>连接<select v-model="baselineId" data-side="baseline" data-field="connection"><option value="">选择连接</option><option v-for="item in connections" :key="item.id" :value="item.id">{{ item.name }} · {{ item.engine }}</option></select></label>
            <label v-if="connection(baselineId)?.engine !== 'sqlite'">数据库<select v-model="baselineDatabase" data-side="baseline" data-field="database"><option value="">选择数据库</option><option v-for="name in databasesOf(baselineId)" :key="name" :value="name">{{ name }}</option></select></label>
            <label v-if="connection(baselineId)?.engine === 'postgres'">schema
              <select v-if="schemaEntry(baselineId, baselineDatabase) === 'choices'" v-model="baselineSchema" data-side="baseline" data-field="schema"><option value="">选择 schema</option><option v-for="name in schemaChoices(baselineId, baselineDatabase)" :key="name" :value="name">{{ name }}</option></select>
              <input v-else-if="schemaEntry(baselineId, baselineDatabase) === 'manual'" v-model="baselineSchema" type="text" data-side="baseline" data-field="schema" placeholder="填写已有 schema" autocomplete="off" />
              <span v-else-if="schemaEntry(baselineId, baselineDatabase) === 'loading'" class="muted" data-side="baseline" data-field="schema" data-schema-state="loading">正在读取 schema</span>
              <select v-else v-model="baselineSchema" data-side="baseline" data-field="schema"><option value="">选择 schema</option></select>
            </label>
            <p v-if="schemaHint(baselineId, baselineDatabase)" class="muted" data-side="baseline" data-schema-hint>{{ schemaHint(baselineId, baselineDatabase) }}</p>
          </fieldset>
          <fieldset>
            <legend>对照</legend>
            <label>连接<select v-model="targetId" data-side="target" data-field="connection"><option value="">选择连接</option><option v-for="item in connections" :key="item.id" :value="item.id">{{ item.name }} · {{ item.engine }}</option></select></label>
            <label v-if="connection(targetId)?.engine !== 'sqlite'">数据库<select v-model="targetDatabase" data-side="target" data-field="database"><option value="">选择数据库</option><option v-for="name in databasesOf(targetId)" :key="name" :value="name">{{ name }}</option></select></label>
            <label v-if="connection(targetId)?.engine === 'postgres'">schema
              <select v-if="schemaEntry(targetId, targetDatabase) === 'choices'" v-model="targetSchema" data-side="target" data-field="schema"><option value="">选择 schema</option><option v-for="name in schemaChoices(targetId, targetDatabase)" :key="name" :value="name">{{ name }}</option></select>
              <input v-else-if="schemaEntry(targetId, targetDatabase) === 'manual'" v-model="targetSchema" type="text" data-side="target" data-field="schema" placeholder="填写已有 schema" autocomplete="off" />
              <span v-else-if="schemaEntry(targetId, targetDatabase) === 'loading'" class="muted" data-side="target" data-field="schema" data-schema-state="loading">正在读取 schema</span>
              <select v-else v-model="targetSchema" data-side="target" data-field="schema"><option value="">选择 schema</option></select>
            </label>
            <p v-if="schemaHint(targetId, targetDatabase)" class="muted" data-side="target" data-schema-hint>{{ schemaHint(targetId, targetDatabase) }}</p>
          </fieldset>
        </div>
        <div class="compare-actions">
          <button class="btn primary" type="button" data-action="compare-structure" :disabled="busy" @click="runStructure">对比结构</button>
          <button class="btn" type="button" data-action="compare-cancel" :disabled="!busy" @click="cancelRun">取消</button>
        </div>
        <p v-if="errorText" class="struct-alert" role="alert">{{ errorText }}</p>
        <p v-if="structure?.refused" class="struct-alert" role="alert" data-compare-refused>{{ structure.refused }}</p>
        <p v-if="structure?.incomplete" class="badge warn" data-compare-incomplete>脚本不完整</p>
        <ul v-if="structure && !structure.refused" class="compare-list" data-compare-diffs>
          <li v-for="item in structure.onlyBaseline" :key="`b-${item.kind}-${item.name}`">仅基准拥有 {{ item.kind }} {{ item.name }}</li>
          <li v-for="item in structure.onlyTarget" :key="`t-${item.kind}-${item.name}`">仅对照拥有 {{ item.kind }} {{ item.name }}</li>
          <li v-for="item in structure.changed" :key="`c-${item.kind}-${item.name}`">{{ item.kind }} {{ item.name }}：{{ item.detail }}</li>
        </ul>
        <template v-if="structure && !structure.refused">
          <label class="compare-script">结构脚本<textarea readonly :value="structure.script" data-compare-script></textarea></label>
          <button class="btn" type="button" data-action="place-structure" :disabled="!structure.script.trim()" @click="place(structure.script)">放入新查询</button>
        </template>
        <section v-if="structure && !structure.refused" class="compare-data">
          <h3>数据差异</h3>
          <p v-if="!structure.tables.length" class="muted">没有可对比的表。</p>
          <label v-for="table in structure.tables" :key="table.name" class="compare-table">
            <input v-model="selected[table.name]" type="checkbox" :disabled="!table.onBoth" :data-table="table.name" />
            {{ table.name }}
            <span v-if="!table.onBoth" class="muted">只在一侧，属于结构差异</span>
            <select v-if="table.needsKeyChoice" v-model="chosenKey[table.name]" :data-key="table.name">
              <option value="">选择唯一键</option>
              <option v-for="key in table.keys" :key="key.name" :value="key.name">{{ key.name }}（{{ key.columns.join(", ") }}）</option>
            </select>
          </label>
          <button class="btn" type="button" data-action="compare-data" :disabled="busy" @click="runData">对比数据</button>
          <p v-if="dataReport?.refused" class="struct-alert" role="alert">{{ dataReport.refused }}</p>
          <p v-if="dataReport?.incomplete" class="badge warn">数据脚本不完整</p>
          <ul v-if="dataReport" class="compare-list">
            <li v-for="(item, index) in dataReport.diffs" :key="`${item.table}-${item.action}-${index}`">{{ item.table }} {{ item.detail }} <template v-if="item.key">键 {{ item.key }}</template></li>
            <li v-for="(note, index) in dataReport.notes" :key="`n-${index}`">{{ note }}</li>
          </ul>
          <template v-if="dataReport && !dataReport.refused">
            <label class="compare-script">数据脚本<textarea readonly :value="dataReport.script" data-data-script></textarea></label>
            <button class="btn" type="button" data-action="place-data" :disabled="!dataReport.script.trim()" @click="place(dataReport.script)">放入新查询</button>
          </template>
        </section>
      </div>
    </div>
  </div>
</template>

<style scoped>
.compare-sheet { width: min(880px, calc(100vw - 48px)); max-height: calc(100vh - 48px); display: flex; flex-direction: column; }
.compare-sheet header, .compare-actions, .compare-data { display: flex; align-items: center; gap: var(--sp-2); }
.compare-sheet header h2 { margin: 0; font-size: var(--fs-lg); }
.compare-sheet .body { overflow: auto; display: flex; flex-direction: column; gap: var(--sp-3); padding: var(--sp-4); }
.compare-sides { display: grid; grid-template-columns: 1fr 1fr; gap: var(--sp-3); }
.compare-sides fieldset { border: 1px solid var(--line); border-radius: var(--r-md); padding: var(--sp-3); display: flex; flex-direction: column; gap: var(--sp-2); }
.compare-sides label, .compare-table, .compare-script { display: flex; flex-direction: column; gap: var(--sp-1); font-size: var(--fs-sm); }
.compare-sides input, .compare-sides select { width: 100%; box-sizing: border-box; }
.compare-script textarea { min-height: 140px; font-family: var(--font-mono, ui-monospace, monospace); }
.compare-list { margin: 0; padding-left: 1.2em; }
.compare-data { flex-wrap: wrap; align-items: flex-start; }
.compare-table { flex-direction: row; align-items: center; }
</style>
