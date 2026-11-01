<script setup lang="ts">
import { computed, defineAsyncComponent, onBeforeUnmount, onMounted, ref } from "vue";
import DefinitionView from "../catalog/DefinitionView.vue";
import DiagramView from "../diagram/DiagramView.vue";
import { catalogConnections, catalogLoaded } from "../catalog/store";
import type { CatalogObjectRef } from "../catalog/object-ref";
import type { Engine } from "../engines";
import { resolveTableTarget, type TableTarget } from "../grid/target";
import type { OpenPlan } from "../history/plan";
import { tr as t } from "../i18n";
import { targetDetail, targetKey, targetOptions, type TargetOption } from "../query/complete";
import "../structure/structure.css";
import Icon from "./Icon.vue";
import { tabTitle, type QueryTarget, type ShellTab } from "./session";
import { loadSettings } from "./settings";
import WelcomeView from "./WelcomeView.vue";

const DatabasePanel = defineAsyncComponent(() => import("../admin/DatabasePanel.vue"));
const DataGrid = defineAsyncComponent(() => import("../grid/DataGrid.vue"));
const MongoDocsTab = defineAsyncComponent(() => import("../mongo/MongoDocsTab.vue"));
const QueryTab = defineAsyncComponent(() => import("../query/QueryTab.vue"));
const RedisTab = defineAsyncComponent(() => import("../redis/RedisTab.vue"));
const StructurePane = defineAsyncComponent(() => import("../structure/StructurePane.vue"));

const props = defineProps<{ tabs: ShellTab[]; activeId: string | null; pageSize?: number; executeRequest?: number }>();
const emit = defineEmits<{
  activate: [id: string];
  close: [id: string];
  "new-query": [];
  connections: [];
  sqlite: [];
  compare: [];
  "history-open": [plan: OpenPlan];
  "show-history": [section: "runs" | "saved"];
  help: [];
  palette: [];
  structure: [];
  "query-target": [target: QueryTarget];
  draft: [id: string, draft: string];
  executed: [id: string, sql: string];
  target: [id: string, target: QueryTarget];
  "table-view": [id: string, pane: "data" | "structure"];
  "copy-definition": [sql: string];
  "jump-reference": [sql: string, ref: string, target: TableTarget | null];
}>();

const active = computed(() => props.tabs.find((tab) => tab.id === props.activeId) ?? null);
const pageSize = computed(() => props.pageSize ?? loadSettings().pageSize);

/** 标签所在环境色：查询标签带连接 id，表标签从标签 id 解析。 */
function tabConnection(tab: ShellTab) {
  const id = tab.connectionId ?? (tab.kind === "table" ? tableTarget(tab.id)?.connectionId : null);
  return id ? catalogConnections().find((item) => item.id === id) ?? null : null;
}

function tabEnvTone(tab: ShellTab): string {
  const tone = tabConnection(tab)?.envTone;
  return tone && tone !== "none" ? tone : "";
}

const activeProduction = computed(() => (active.value ? tabEnvTone(active.value) === "prod" : false));

function paneOf(tab: { tablePane?: "data" | "structure" }): "data" | "structure" {
  return tab.tablePane ?? "data";
}

function showPane(tabId: string, pane: "data" | "structure") {
  emit("table-view", tabId, pane);
}

function tableTarget(tabId: string) {
  return resolveTableTarget(tabId, catalogConnections());
}

/** 表标签 id 前缀是连接 id：目录里找不到这个前缀就说明连接已删除。 */
function tableConnectionGone(tabId: string): boolean {
  if (!tabId.startsWith("table:")) return false;
  const identity = tabId.slice("table:".length);
  return !catalogConnections().some((item) => identity.startsWith(`${item.id}:`));
}

/**
 * 恢复的标签指向已删除的连接（R2）：显示该标签的空态提示，不自动连接。
 * 只判定快照恢复的标签——会话内打开的标签本来就有活目录。目录还没读完时不判定，避免加载期误报。
 */
function danglingTarget(tab: ShellTab): boolean {
  if (!catalogLoaded() || tab.restored !== true) return false;
  if (tab.kind === "table") return tableConnectionGone(tab.id);
  if (tab.kind === "definition" || tab.kind === "diagram" || tab.kind === "keys" || tab.kind === "docs" || tab.kind === "database") {
    return tab.connectionId != null && !catalogConnections().some((item) => item.id === tab.connectionId);
  }
  return false;
}

/** 数据库级标签（R5）要的引擎：标签里存的是字符串，这里收窄成 Engine，未知回退 postgres（面板按引擎只渲染有内容的分节）。 */
function adminEngine(tab: ShellTab): Engine {
  const value = tab.engine ?? "";
  if (value === "mysql" || value === "mariadb" || value === "sqlite" || value === "sqlserver" || value === "redis" || value === "mongodb") return value;
  return "postgres";
}

/** 定义标签的引擎：只认已知值，猜错会让枚举/域的编辑入口出现在别的引擎上。 */
function definitionEngine(tab: ShellTab): Engine | undefined {
  const value = tab.engine ?? "";
  if (value === "postgres" || value === "mysql" || value === "mariadb" || value === "sqlite" || value === "sqlserver" || value === "mongodb" || value === "redis") {
    return value;
  }
  return undefined;
}

function definitionObject(tab: ShellTab): Pick<CatalogObjectRef, "connectionId" | "database" | "schema" | "name" | "kind" | "signature" | "relation"> | null {
  if (!tab.definition || !tab.connectionId || tab.database == null) return null;
  return {
    connectionId: tab.connectionId,
    database: tab.database,
    schema: tab.definition.schema,
    name: tab.definition.name,
    kind: tab.definition.kind as CatalogObjectRef["kind"],
    signature: tab.definition.signature,
    relation: tab.definition.relation,
  };
}

function onDraft(tabId: string, draft: string) {
  emit("draft", tabId, draft);
}

function onTarget(target: QueryTarget) {
  if (!active.value) return;
  emit("target", active.value.id, target);
}

/** 数据格子请求跳转引用行时，补上当前表的目标一起交给外壳执行。 */
function onJumpReference(sql: string, ref: string) {
  if (!active.value) return;
  emit("jump-reference", sql, ref, tableTarget(active.value.id));
}

const menu = ref<{ tabId: string; x: number; y: number } | null>(null);
const menuTab = computed(() => props.tabs.find((tab) => tab.id === menu.value?.tabId) ?? null);
const menuTargets = computed(() => (menuTab.value?.kind === "query" ? targetOptions(catalogConnections()) : []));

function currentTargetKey(tab: ShellTab): string {
  if (!tab.connectionId || tab.database == null) return "";
  return targetKey(tab.connectionId, tab.database);
}

function onTabKey(event: KeyboardEvent, id: string) {
  const index = props.tabs.findIndex(tab => tab.id === id);
  const next = event.key === "ArrowRight" ? (index + 1) % props.tabs.length : event.key === "ArrowLeft" ? (index - 1 + props.tabs.length) % props.tabs.length : event.key === "Home" ? 0 : event.key === "End" ? props.tabs.length - 1 : -1;
  if (next < 0) return;
  event.preventDefault();
  emit("activate", props.tabs[next]!.id);
  (event.currentTarget as HTMLElement).parentElement?.querySelectorAll<HTMLElement>('[role="tab"]')[next]?.focus();
}

function onTabBarContext(event: MouseEvent) {
  event.preventDefault();
  const node = event.target;
  if (!(node instanceof Element)) return;
  const tabEl = node.closest(".tab");
  if (!(tabEl instanceof HTMLElement)) {
    menu.value = null;
    return;
  }
  const tab = props.tabs.find((item) => item.id === tabEl.dataset.tabId);
  if (!tab) return;
  emit("activate", tab.id);
  menu.value = {
    tabId: tab.id,
    x: Math.max(8, Math.min(event.clientX, window.innerWidth - 240)),
    y: Math.max(8, Math.min(event.clientY, window.innerHeight - 48)),
  };
}

function closeIds(ids: string[]) {
  menu.value = null;
  for (const id of ids) emit("close", id);
}

function closeOthers(id: string) {
  closeIds(props.tabs.filter((tab) => tab.id !== id).map((tab) => tab.id));
}

function closeToTheRight(id: string) {
  const index = props.tabs.findIndex((tab) => tab.id === id);
  closeIds(props.tabs.slice(index + 1).map((tab) => tab.id));
}

function isLastTab(id: string): boolean {
  return props.tabs.findIndex((tab) => tab.id === id) >= props.tabs.length - 1;
}

function bindTarget(tabId: string, option: TargetOption) {
  emit("target", tabId, {
    connectionId: option.connectionId,
    connectionName: option.name,
    database: option.database,
    engine: option.engine,
    status: option.status,
  });
  menu.value = null;
}

function showTablePane(pane: "data" | "structure") {
  const tab = menuTab.value;
  if (!tab) return;
  emit("table-view", tab.id, pane);
  menu.value = null;
}

function onWindowPointer(event: MouseEvent) {
  const node = event.target;
  if (node instanceof Element && node.closest("[data-menu=tab]")) return;
  menu.value = null;
}

function onWindowKey(event: KeyboardEvent) {
  if (event.key === "Escape") menu.value = null;
}

onMounted(() => {
  window.addEventListener("mousedown", onWindowPointer);
  window.addEventListener("keydown", onWindowKey);
});

onBeforeUnmount(() => {
  window.removeEventListener("mousedown", onWindowPointer);
  window.removeEventListener("keydown", onWindowKey);
});
</script>

<template>
  <div class="workspace-view">
  <div v-if="tabs.length" class="tabbar" role="tablist" :aria-label="t('shell.tabs.tablist')" @contextmenu="onTabBarContext" @dblclick.self="emit('new-query')">
    <div
      v-for="tab in tabs"
      :key="tab.id"
      class="tab"
      :class="{ active: tab.id === activeId }"
      role="tab"
      :data-tab-id="tab.id"
      :aria-selected="tab.id === activeId"
      :tabindex="tab.id === activeId ? 0 : -1"
      @click="emit('activate', tab.id)"
      @keydown.enter.prevent="emit('activate', tab.id)"
      @keydown.space.prevent="emit('activate', tab.id)"
      @keydown="onTabKey($event, tab.id)"
    >
      <Icon :name="tab.kind === 'query' ? 'code' : tab.kind === 'diagram' ? 'link' : tab.kind === 'keys' ? 'key' : tab.kind === 'docs' ? 'columns' : tab.kind === 'database' ? 'database' : 'table'" />
      <span v-if="tabEnvTone(tab)" class="tab-env" :class="`env-${tabEnvTone(tab)}`" aria-hidden="true"></span>
      <span class="tab-title">{{ tabTitle(tab) }}</span>
      <span
        v-if="tab.targetText || tab.path || tab.kind === 'query'"
        class="tab-target"
        :class="{ none: !tab.targetText && !tab.path }"
      >{{ tab.targetText || tab.path || t("shell.tabs.noConnection") }}</span>
      <button class="tab-x" type="button" :aria-label="t('shell.tabs.closeAria', { title: tabTitle(tab) })" :title="t('common.close')" @click.stop="emit('close', tab.id)">
        <Icon name="x" />
      </button>
    </div>
    <button class="icon-btn sm tab-new" type="button" :aria-label="t('common.newQuery')" :title="t('common.newQuery')" @click="emit('new-query')">
      <Icon name="plus" />
    </button>
  </div>
  <div v-if="activeProduction" class="env-stripe" data-env-stripe aria-hidden="true"></div>
  <div class="tab-body" id="tab-body">
    <QueryTab
      v-if="active && active.kind === 'query'"
      :key="active.id"
      :tab-id="active.id"
      :draft="active.draft"
      :connection-id="active.connectionId"
      :connection-name="active.connectionName"
      :database="active.database"
      :engine="active.engine"
      :connection-status="active.connectionStatus"
      :notice="active.targetNotice"
      :execute-request="executeRequest"
      @update:draft="onDraft"
      @executed="(id, sql) => emit('executed', id, sql)"
      @target="onTarget"
    />
    <div v-else-if="active && danglingTarget(active)" class="tab-dangling" data-dangling-target>
      <div class="tab-dangling-card">
        <h3>{{ tabTitle(active) }}</h3>
        <p>{{ t("shell.tabs.danglingNotice") }}</p>
        <p class="tab-dangling-path">{{ active.targetText || active.path || active.id }}</p>
      </div>
    </div>
    <div v-else-if="active && active.kind === 'table'" class="table-workspace">
      <div class="pane-switch" role="tablist" :aria-label="t('shell.tabs.paneLabel')">
        <button type="button" role="tab" :aria-selected="paneOf(active) === 'data'" @click="showPane(active.id, 'data')"><Icon name="grid" />{{ t("common.data") }}</button>
        <button type="button" role="tab" :aria-selected="paneOf(active) === 'structure'" @click="showPane(active.id, 'structure')"><Icon name="columns" />{{ t("common.structure") }}</button>
      </div>
      <DataGrid
        v-if="paneOf(active) === 'data'"
        :key="active.id"
        :tab-id="active.id"
        :draft="active.draft"
        :page-size="pageSize"
        :target="tableTarget(active.id)"
        @jump-reference="onJumpReference"
      />
      <StructurePane
        v-else
        :key="`${active.id}:structure`"
        :tab-id="active.id"
        :draft="active.draft"
        purpose="edit"
        :target="tableTarget(active.id)"
        :focus-section="active.structureSection"
      />
    </div>
    <DatabasePanel
      v-else-if="active && active.kind === 'database'"
      :key="active.id"
      :connection-id="active.connectionId ?? ''"
      :connection-name="active.connectionName ?? ''"
      :database="active.database ?? ''"
      :engine="adminEngine(active)"
      :schema="active.admin?.schema ?? ''"
      :env-label="tabConnection(active)?.envLabel"
      :env-tone="tabEnvTone(active)"
    />
    <RedisTab
      v-else-if="active && active.kind === 'keys' && active.connectionId"
      :key="active.id"
      :tab-id="active.id"
      :connection-id="active.connectionId"
      :connection-name="active.connectionName ?? ''"
      :database="active.database ?? '0'"
      :status="active.connectionStatus ?? 'connected'"
      :initial-key="active.redis?.key"
    />
    <MongoDocsTab
      v-else-if="active && active.kind === 'docs' && active.connectionId && active.doc"
      :key="active.id"
      :tab-id="active.id"
      :connection-id="active.connectionId"
      :connection-name="active.connectionName ?? ''"
      :database="active.doc.database"
      :collection="active.doc.collection"
      :status="active.connectionStatus ?? 'connected'"
    />
    <StructurePane
      v-else-if="active && active.kind === 'structure'"
      :key="active.id"
      :tab-id="active.id"
      :draft="active.draft"
      purpose="create"
    />
    <DefinitionView
      v-else-if="active && active.kind === 'definition' && definitionObject(active)"
      :key="active.id"
      :label="tabTitle(active)"
      :object="definitionObject(active)!"
      :engine="definitionEngine(active!)"
      @copy-sql="emit('copy-definition', $event)"
    />
    <DiagramView
      v-else-if="active && active.kind === 'diagram' && active.connectionId && active.database != null"
      :key="active.id"
      :label="tabTitle(active)"
      :connection-id="active.connectionId"
      :database="active.database"
      :schema="active.diagram?.schema ?? ''"
    />
    <WelcomeView
      v-else
      @new-query="emit('new-query')"
      @connections="emit('connections')"
      @sqlite="emit('sqlite')"
      @compare="emit('compare')"
      @history-open="emit('history-open', $event)"
      @show-history="emit('show-history', $event)"
      @help="emit('help')"
    />
  </div>
  <Teleport to="body">
    <div
      v-if="menu && menuTab"
      class="menu tab-menu"
      data-menu="tab"
      role="menu"
      :aria-label="t('shell.tabs.targetMenuLabel', { title: tabTitle(menuTab) })"
      :style="{ left: `${menu.x}px`, top: `${menu.y}px` }"
      @mousedown.stop
      @contextmenu.prevent
    >
      <template v-if="menuTab.kind === 'query'">
        <div class="menu-head">{{ t("shell.tabs.pickTarget") }}</div>
        <p v-if="!menuTargets.length" class="target-empty">{{ t("shell.tabs.noConnections") }}</p>
        <button
          v-for="option in menuTargets"
          :key="option.key"
          class="menu-item"
          type="button"
          data-action="bind-target"
          :data-key="option.key"
          :title="targetDetail(option)"
          @click="bindTarget(menuTab.id, option)"
        >
          <span>{{ option.label }}</span>
          <Icon v-if="option.key === currentTargetKey(menuTab)" class="check" name="check" />
        </button>
        <div class="menu-sep" role="separator"></div>
      </template>
      <template v-else-if="menuTab.kind === 'table'">
        <button class="menu-item" type="button" data-action="show-data" @click="showTablePane('data')">{{ t("common.data") }}</button>
        <button class="menu-item" type="button" data-action="show-structure" @click="showTablePane('structure')">{{ t("common.structure") }}</button>
        <div class="menu-sep" role="separator"></div>
      </template>
      <button class="menu-item" type="button" data-action="close-tab" @click="closeIds([menuTab.id])">{{ t("common.close") }}</button>
      <button class="menu-item" type="button" data-action="close-others" :disabled="tabs.length < 2" @click="closeOthers(menuTab.id)">{{ t("shell.tabs.closeOthers") }}</button>
      <button class="menu-item" type="button" data-action="close-right" :disabled="isLastTab(menuTab.id)" @click="closeToTheRight(menuTab.id)">{{ t("shell.tabs.closeRight") }}</button>
      <button class="menu-item" type="button" data-action="close-all" @click="closeIds(tabs.map((tab) => tab.id))">{{ t("shell.tabs.closeAll") }}</button>
    </div>
  </Teleport>
  </div>
</template>

<style scoped>
.tab-dangling { display: grid; place-items: center; height: 100%; min-height: 0; }
.tab-dangling-card {
  max-width: 420px;
  padding: var(--sp-4);
  border: 1px dashed var(--line);
  border-radius: var(--r-md);
  color: var(--text-2);
  text-align: center;
  display: grid;
  gap: var(--sp-2);
}
.tab-dangling-card h3 { margin: 0; color: var(--text); font-size: var(--fs-md, 15px); }
.tab-dangling-path { margin: 0; font-size: 12px; color: var(--text-3); word-break: break-all; }
</style>
