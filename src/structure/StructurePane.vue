<script setup lang="ts">
import Icon from "../shell/Icon.vue";
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { catalogConnections, refreshCatalog, structureFocus } from "../catalog/store";
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
const section = ref<"columns" | "indexes" | "foreign" | "drop" | "definition">("columns");
const focusedMember = ref("");
const viewSql = ref("");
const previewedViewSql = ref<string | null>(null);
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
const ddlCopyNote = ref("");

const connections = computed(() => catalogConnections().filter((item) => item.status === "connected"));

const isView = computed(() => description.value?.kind === "view");
const viewUnreadable = computed(() => {
  if (!isView.value) return "";
  if (description.value?.definitionError) return description.value.definitionError;
  const sql = description.value?.displaySql.trim() ?? "";
  if (sql === "" || sql.startsWith("--")) return "没有读到可编辑的视图定义，不能把占位说明当作原文。";
  return "";
});

watch(
  () => [props.focusSection, structureFocus.value, props.target?.table, props.target?.connectionId] as const,
  () => {
    const focus = structureFocus.value;
    const target = props.target;
    if (
      focus &&
      target &&
      focus.connectionId === target.connectionId &&
      focus.database === target.database &&
      focus.schema === target.schema &&
      focus.table === target.table
    ) {
      section.value = focus.section;
      focusedMember.value = focus.name;
      return;
    }
    focusedMember.value = "";
    if (props.focusSection) section.value = props.focusSection;
  },
  { immediate: true },
);

function chooseSection(next: "columns" | "indexes" | "foreign" | "drop" | "definition") {
  if (structureFocus.value && structureFocus.value.section !== next) {
    structureFocus.value = null;
    focusedMember.value = "";
  }
  section.value = next;
}
const activeColumns = computed(() => rows.value.filter((row) => !row.dropped).map((row) => row.name).filter(Boolean));

const boundConnection = computed(() => {
  const id = purpose.value === "edit" ? props.target?.connectionId : createConnection.value;
  if (!id) return null;
  return catalogConnections().find((item) => item.id === id) ?? null;
});

const environmentLabel = computed(() => boundConnection.value?.envLabel.trim() ?? "");
const environmentTone = computed(() => boundConnection.value?.envTone ?? "none");
const connectionName = computed(() => boundConnection.value?.name.trim() ?? "");

/* R4：结构视图里随时能读出当前连接与库。环境文字只读目录里的同一条连接。 */
const contextText = computed(() => {
  if (purpose.value !== "edit" || !props.target) return "";
  const name = boundConnection.value?.name ?? props.target.connectionId;
  const place = props.target.schema ? `${props.target.database} / ${props.target.schema}` : props.target.database;
  return `来自连接「${name}」的「${place}」`;
});

const activeEngine = computed(() => {
  if (purpose.value === "edit") return props.target?.engine ?? description.value?.engine ?? "";
  const connection = connections.value.find((item) => item.id === createConnection.value);
  return connection?.engine ?? "";
});

/* 建表 SQL：表的定义按当前结构重建；视图只有读到真实定义才展示，MySQL/PG 视图后端只存占位注释。 */
const ddlVisible = computed(() => {
  const current = description.value;
  if (purpose.value !== "edit" || !current || loading.value) return false;
  const sql = current.displaySql.trim();
  if (sql === "") return false;
  return current.kind === "table" || !sql.startsWith("--");
});

const ddlOriginNote = computed(() => {
  const current = description.value;
  if (!current) return "";
  if (current.kind !== "table" && current.engine === "sqlite") return "这段定义读自 SQLite 的 sqlite_master，是数据库里的原文。";
  return "按当前结构重新生成，便于阅读；不是数据库里的原始定义。";
});

async function copyDdl() {
  const sql = description.value?.displaySql ?? "";
  try {
    await navigator.clipboard.writeText(sql);
    ddlCopyNote.value = "已复制";
  } catch {
    ddlCopyNote.value = "复制失败";
  }
}

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
    const sql = loaded.displaySql.trim();
    viewSql.value = loaded.kind === "view" && sql !== "" && !sql.startsWith("--") ? loaded.displaySql : "";
    previewedViewSql.value = null;
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

function previewView() {
  if (!props.target || !isView.value) return;
  if (viewUnreadable.value) {
    errorText.value = viewUnreadable.value;
    return;
  }
  previewedViewSql.value = viewSql.value;
  openPreview({ kind: "replaceView", table: props.target.table, definition: viewSql.value });
}

watch(viewSql, (value) => {
  if (pending.value?.kind === "replaceView" && previewedViewSql.value !== null && value !== previewedViewSql.value) {
    closePreview();
    previewedViewSql.value = null;
  }
});

function previewCurrent() {
  if (section.value === "definition") previewView();
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
  if (!request || !plan.value || !pending.value || plan.value.blocked.length || plan.value.steps.length === 0) return;
  if (pending.value.kind === "replaceView" && pending.value.definition !== viewSql.value) {
    errorText.value = "定义在预览之后又改过，需要重新预览。没有发送。";
    closePreview();
    return;
  }
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
      <header class="structure-heading"><span class="action-icon"><Icon name="table" /></span><div><h2>{{ purpose === 'create' ? '设计新表' : target?.table }}</h2><p><span v-if="environmentLabel" class="env xs" :class="environmentTone !== 'none' ? `env-${environmentTone}` : undefined">{{ environmentLabel }}</span> {{ purpose === 'create' ? '定义字段、类型与约束，预览 SQL 后再创建。' : contextText }}</p></div><span class="grow"></span><div v-if="description" class="structure-stats"><span><b>{{ description.columns.length }}</b> 字段</span><span><b>{{ description.indexes.length }}</b> 索引</span></div></header>
      <nav class="struct-nav" aria-label="结构分区">
        <button type="button" :class="{ on: section === 'columns' }" @click="chooseSection('columns')">字段</button>
        <button v-if="purpose === 'edit'" type="button" :class="{ on: section === 'indexes' }" @click="chooseSection('indexes')">索引</button>
        <button v-if="purpose === 'edit'" type="button" :class="{ on: section === 'foreign' }" @click="chooseSection('foreign')">外键</button>
        <button v-if="isView" type="button" :class="{ on: section === 'definition' }" data-section="definition" @click="chooseSection('definition')">定义</button>
        <span class="grow"></span>
        <button v-if="purpose === 'edit' && !isView" type="button" class="danger" :class="{ on: section === 'drop' }" @click="chooseSection('drop')">删除表</button>
      </nav>
      <details v-if="ddlVisible" class="disclosure struct-ddl">
        <summary>建表 SQL</summary>
        <div class="disclosure-body">
          <div class="struct-ddl-bar">
            <button class="btn sm" type="button" @click="copyDdl">复制</button>
            <span v-if="ddlCopyNote" class="muted" role="status">{{ ddlCopyNote }}</span>
          </div>
          <pre>{{ description?.displaySql }}</pre>
          <p class="hint">{{ ddlOriginNote }}</p>
        </div>
      </details>
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
        <StructureEditor v-if="section === 'columns'" :rows="rows" :baseline="description?.columns ?? []" :mode="purpose === 'create' ? 'create' : 'edit'" :focus-name="focusedMember" @preview="previewColumns" />
        <IndexEditor
          v-else-if="section === 'indexes' && description"
          :indexes="description.indexes"
          :columns="activeColumns"
          :focus-name="focusedMember"
          @drop="openPreview({ kind: 'dropIndex', table: target!.table, name: $event })"
          @create="openPreview({ kind: 'createIndex', table: target!.table, name: $event.name, unique: $event.unique, columns: $event.columns })"
        />
        <section v-else-if="section === 'definition' && isView" class="struct-view-def" data-view-definition>
          <p v-if="viewUnreadable" class="struct-alert" role="alert" data-view-error>{{ viewUnreadable }}</p>
          <template v-else>
            <p class="muted">修改下面的定义。先列出将发送的全部语句；取消不发送，确认后才按这个顺序发送。函数、存储过程和触发器不能从这里保存。</p>
            <textarea v-model="viewSql" data-view-sql aria-label="视图定义" spellcheck="false"></textarea>
            <div>
              <button class="btn primary" type="button" data-action="preview-view" @click="previewView">查看将要发生什么</button>
            </div>
          </template>
        </section>
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
      :connection-name="connectionName"
      :env-label="environmentLabel"
      :env-tone="environmentTone"
      :expect-name="pending?.kind === 'dropTable' ? pending.table : undefined"
      @cancel="closePreview"
      @confirm="confirm"
    />
  </div>
</template>

<style scoped>
/* 建表 SQL 区块：折叠壳复用 base.css 的 disclosure，正文 pre 与 structure.css 的 pv-sql pre 同款。 */
.struct-ddl { border-bottom: 1px solid var(--line); background: var(--panel-2); }
.struct-ddl-bar { display: flex; align-items: center; gap: var(--sp-2); margin-bottom: var(--sp-2); }
.struct-view-def { display: flex; flex-direction: column; gap: var(--sp-2); padding: var(--sp-3); min-height: 0; }
.struct-view-def textarea {
  min-height: 180px;
  padding: var(--sp-2) var(--sp-3);
  border: 1px solid var(--line);
  border-radius: var(--r);
  background: var(--sunken);
  color: var(--text);
  font: var(--fs-sm) var(--mono);
  resize: vertical;
}
.struct-ddl pre {
  margin: 0 0 var(--sp-2);
  padding: var(--sp-2) var(--sp-3);
  background: var(--sunken);
  border-radius: var(--r);
  overflow: auto;
  font-family: var(--mono);
  font-size: var(--fs-xs);
  white-space: pre-wrap;
  word-break: break-all;
}
</style>
