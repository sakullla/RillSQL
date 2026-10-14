<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { catalogConnections, refreshCatalog } from "../catalog/store";
import type { TableTarget } from "../grid/target";
import { describeStructure, executeStructure, previewStructure, type StructureRequest } from "./api";
import ChangePreviewDialog from "./ChangePreviewDialog.vue";
import ForeignKeyEditor from "./ForeignKeyEditor.vue";
import IndexEditor from "./IndexEditor.vue";
import { draftsFromColumns } from "./logic";
import StructureEditor from "./StructureEditor.vue";
import "./structure.css";
import type { ColumnDraft, StructureAction, StructureDescription, StructureForeignKey, StructurePlan, StructureReceipt } from "./types";

const props = defineProps<{
  tabId: string;
  draft: string;
  target?: TableTarget | null;
  purpose?: "create" | "edit";
  focusSection?: "columns" | "indexes" | "foreign" | "drop";
}>();

const purpose = computed(() => props.purpose ?? "create");
const section = ref<"columns" | "indexes" | "foreign" | "drop">("columns");
const description = ref<StructureDescription | null>(null);
const rows = ref<ColumnDraft[]>([]);
const errorText = ref("");
const loading = ref(false);
const plan = ref<StructurePlan | null>(null);
const receipt = ref<StructureReceipt | null>(null);
const pending = ref<StructureAction | null>(null);
const busy = ref(false);
const createName = ref("");
const createConnection = ref("");
const createDatabase = ref("");
const createSchema = ref("");

const connections = computed(() => catalogConnections().filter((item) => item.status === "connected"));

watch(
  () => props.focusSection,
  (value) => {
    if (value) section.value = value;
  },
  { immediate: true },
);
const activeColumns = computed(() => rows.value.filter((row) => !row.dropped).map((row) => row.name).filter(Boolean));

/* R4：结构视图里随时能读出当前连接与库。 */
const contextText = computed(() => {
  if (purpose.value !== "edit" || !props.target) return "";
  const name = connections.value.find((item) => item.id === props.target!.connectionId)?.name ?? props.target.connectionId;
  const place = props.target.schema ? `${props.target.database} / ${props.target.schema}` : props.target.database;
  return `来自连接「${name}」的「${place}」`;
});

const activeEngine = computed(() => {
  if (purpose.value === "edit") return props.target?.engine ?? description.value?.engine ?? "";
  const connection = connections.value.find((item) => item.id === createConnection.value);
  return connection?.engine ?? "";
});

function requestOf(): StructureRequest | null {
  if (purpose.value === "edit") {
    if (!props.target) return null;
    return {
      connectionId: props.target.connectionId,
      database: props.target.database,
      schema: props.target.schema,
      engine: props.target.engine,
    };
  }
  const connection = connections.value.find((item) => item.id === createConnection.value);
  if (!connection || !createDatabase.value) return null;
  return {
    connectionId: connection.id,
    database: createDatabase.value,
    schema: createSchema.value,
    engine: connection.engine,
  };
}

async function loadTable() {
  if (purpose.value !== "edit" || !props.target) return;
  loading.value = true;
  errorText.value = "";
  try {
    const loaded = await describeStructure(props.target);
    description.value = loaded;
    rows.value = draftsFromColumns(loaded.columns);
  } catch (error) {
    errorText.value = error instanceof Error ? error.message : String(error);
  } finally {
    loading.value = false;
  }
}

watch(
  () => props.target?.table,
  () => {
    void loadTable();
  },
  { immediate: true },
);

function seedColumns(engine: string) {
  if (purpose.value !== "create" || rows.value.length > 0) return;
  const sqlite = engine === "sqlite";
  const postgres = engine === "postgres";
  rows.value = [
    { original: null, name: "id", typeName: sqlite ? "INTEGER" : "bigint", nullable: false, defaultSql: "", primary: true, auto: true, dropped: false },
    {
      original: null,
      name: "created_at",
      typeName: sqlite ? "TEXT" : postgres ? "timestamptz" : "datetime",
      nullable: false,
      defaultSql: sqlite ? "CURRENT_TIMESTAMP" : postgres ? "now()" : "CURRENT_TIMESTAMP",
      primary: false,
      auto: false,
      dropped: false,
    },
  ];
}

watch(connections, (list) => {
  if (!createConnection.value && list[0]) {
    createConnection.value = list[0].id;
    createDatabase.value = list[0].defaultDatabase ?? list[0].databases[0]?.name ?? "";
    createSchema.value = list[0].databases[0]?.schemas[0]?.name ?? "";
    seedColumns(list[0].engine);
  }
}, { immediate: true });

function openPreview(action: StructureAction) {
  const request = requestOf();
  if (!request) {
    errorText.value = purpose.value === "create" ? "需要至少一个已打开的连接。" : "这张表还没有绑定已打开的连接。";
    return;
  }
  busy.value = true;
  errorText.value = "";
  receipt.value = null;
  pending.value = action;
  void previewStructure(request, action)
    .then((next) => {
      plan.value = next;
    })
    .catch((error: unknown) => {
      plan.value = null;
      pending.value = null;
      errorText.value = error instanceof Error ? error.message : String(error);
    })
    .finally(() => {
      busy.value = false;
    });
}

function previewColumns() {
  const request = requestOf();
  if (!request) {
    errorText.value = purpose.value === "create" ? "需要至少一个已打开的连接。" : "这张表还没有绑定已打开的连接。";
    return;
  }
  if (purpose.value === "create") {
    if (!createName.value.trim()) {
      errorText.value = "先给新表起个名字。";
      return;
    }
    openPreview({ kind: "createTable", name: createName.value.trim(), columns: rows.value.map((row) => ({ ...row })) });
    return;
  }
  if (!props.target) return;
  openPreview({ kind: "alterColumns", table: props.target.table, columns: rows.value.map((row) => ({ ...row })) });
}

function previewCurrent() {
  if (section.value === "columns") previewColumns();
  if (section.value === "drop" && props.target) openPreview({ kind: "dropTable", table: props.target.table });
}

function onKeydown(event: KeyboardEvent) {
  if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "s" && !event.altKey) {
    event.preventDefault();
    previewCurrent();
  }
}

onMounted(() => window.addEventListener("keydown", onKeydown));
onBeforeUnmount(() => window.removeEventListener("keydown", onKeydown));

async function confirm() {
  const request = requestOf();
  if (!request || !plan.value || !pending.value || plan.value.blocked.length) return;
  busy.value = true;
  try {
    const done = await executeStructure(request, pending.value, plan.value.steps.map((step) => step.sql));
    receipt.value = done;
    if (done.steps.some((step) => step.status === "ok")) {
      refreshCatalog();
      await loadTable();
    }
  } catch (error) {
    receipt.value = {
      blocked: [],
      rule: plan.value.rule,
      target: plan.value.target,
      rebuild: plan.value.rebuild,
      steps: [{ sql: "", label: "执行", status: "unknown", message: error instanceof Error ? error.message : String(error) }],
    };
  } finally {
    busy.value = false;
  }
}

function closePreview() {
  plan.value = null;
  receipt.value = null;
  pending.value = null;
}

function createForeignKey(foreignKey: StructureForeignKey) {
  if (!props.target) return;
  openPreview({ kind: "createForeignKey", table: props.target.table, foreignKey });
}
</script>

<template>
  <div class="structure-pane" aria-label="表结构" :data-tab-id="tabId" :data-draft="draft" tabindex="-1">
    <p v-if="purpose === 'edit' && !target" class="empty" role="alert">这张表还没有绑定已打开的连接。</p>
    <template v-else>
      <nav class="struct-nav" aria-label="结构分区">
        <button type="button" :class="{ on: section === 'columns' }" @click="section = 'columns'">字段</button>
        <button v-if="purpose === 'edit'" type="button" :class="{ on: section === 'indexes' }" @click="section = 'indexes'">索引</button>
        <button v-if="purpose === 'edit'" type="button" :class="{ on: section === 'foreign' }" @click="section = 'foreign'">外键</button>
        <span class="grow"></span>
        <button v-if="purpose === 'edit'" type="button" class="danger" :class="{ on: section === 'drop' }" @click="section = 'drop'">删除表</button>
      </nav>
      <p v-if="purpose === 'edit' && target" class="struct-context">正在修改表 <strong>{{ target.table }}</strong> · {{ contextText }}</p>
      <p v-if="errorText" class="struct-alert" role="alert">{{ errorText }}</p>
      <p v-if="loading" class="struct-loading muted">正在读取这张表的结构……</p>
      <div v-else class="struct-body">
        <form v-if="purpose === 'create'" class="struct-form" @submit.prevent="previewColumns">
          <label>连接
            <select v-model="createConnection" aria-label="目标连接">
              <option v-for="item in connections" :key="item.id" :value="item.id">{{ item.name }}</option>
            </select>
          </label>
          <label>数据库<input v-model="createDatabase" type="text" aria-label="目标数据库" /></label>
          <label>Schema<input v-model="createSchema" type="text" aria-label="目标 schema" /></label>
          <label>新表名<input v-model="createName" type="text" aria-label="新表名" /></label>
          <p v-if="!connections.length" class="muted">需要至少一个已打开的连接。</p>
        </form>
        <StructureEditor v-if="section === 'columns'" :rows="rows" :baseline="description?.columns ?? []" :mode="purpose === 'create' ? 'create' : 'edit'" @preview="previewColumns" />
        <IndexEditor
          v-else-if="section === 'indexes' && description"
          :indexes="description.indexes"
          :columns="activeColumns"
          @drop="openPreview({ kind: 'dropIndex', table: target!.table, name: $event })"
          @create="openPreview({ kind: 'createIndex', table: target!.table, name: $event.name, unique: $event.unique, columns: $event.columns })"
        />
        <ForeignKeyEditor
          v-else-if="section === 'foreign' && description"
          :foreign-keys="description.foreignKeyList"
          :incoming="description.incoming"
          :columns="activeColumns"
          :engine="description.engine"
          :storage="description.storage"
          :enforced="description.foreignKeys"
          @drop="openPreview({ kind: 'dropForeignKey', table: target!.table, name: $event })"
          @create="createForeignKey"
          @enable="openPreview({ kind: 'setForeignKeys', enabled: true })"
        />
        <section v-else-if="section === 'drop' && target" class="struct-drop">
          <h3>删除表 {{ target.table }}</h3>
          <p class="impact danger">
            将永久删除这张表<template v-if="description?.rowCount != null">和里面约 {{ description.rowCount }} 行数据</template>
            <template v-else>和里面所有数据（暂时不知道有多少行）</template>，无法恢复。
          </p>
          <p class="muted">只删除这一张表，不会连带删除其他表或对象。</p>
          <template v-if="description?.incoming.length">
            <p class="impact warn">有 {{ description.incoming.length }} 个外键引用这张表：删除可能被数据库拒绝，也可能留下无效的引用。</p>
            <details class="disclosure">
              <summary>查看引用它的外键</summary>
              <div class="disclosure-body">
                <ul class="plain-list">
                  <li v-for="item in description.incoming" :key="`${item.table}.${item.name}`">
                    <code>{{ item.table }}.{{ item.name }}</code>（{{ item.columns.join(", ") }} → {{ item.refColumns.join(", ") }}）
                  </li>
                </ul>
              </div>
            </details>
          </template>
          <div>
            <button class="btn danger" type="button" @click="openPreview({ kind: 'dropTable', table: target.table })">查看将要发生什么</button>
          </div>
        </section>
      </div>
    </template>
    <ChangePreviewDialog
      v-if="plan"
      :plan="plan"
      :receipt="receipt"
      :busy="busy"
      :engine="activeEngine"
      :expect-name="pending?.kind === 'dropTable' ? pending.table : undefined"
      @cancel="closePreview"
      @confirm="confirm"
    />
  </div>
</template>
