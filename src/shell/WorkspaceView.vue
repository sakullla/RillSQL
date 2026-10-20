<script setup lang="ts">
import { computed, defineAsyncComponent, onBeforeUnmount, onMounted, ref } from "vue";
import DefinitionView from "../catalog/DefinitionView.vue";
import { catalogConnections } from "../catalog/store";
import type { CatalogObjectRef } from "../catalog/object-ref";
import { resolveTableTarget, type TableTarget } from "../grid/target";
import { targetDetail, targetKey, targetOptions, type TargetOption } from "../query/complete";
import "../structure/structure.css";
import Icon from "./Icon.vue";
import type { QueryTarget, ShellTab } from "./session";
import { loadSettings } from "./settings";
import WelcomeView from "./WelcomeView.vue";

const DataGrid = defineAsyncComponent(() => import("../grid/DataGrid.vue"));
const QueryTab = defineAsyncComponent(() => import("../query/QueryTab.vue"));
const StructurePane = defineAsyncComponent(() => import("../structure/StructurePane.vue"));

const props = defineProps<{ tabs: ShellTab[]; activeId: string | null; pageSize?: number; executeRequest?: number }>();
const emit = defineEmits<{
  activate: [id: string];
  close: [id: string];
  "new-query": [];
  connections: [];
  sqlite: [];
  palette: [];
  structure: [];
  "query-target": [target: QueryTarget];
  draft: [id: string, draft: string];
  target: [id: string, target: QueryTarget];
  "table-view": [id: string, pane: "data" | "structure"];
  "copy-definition": [sql: string];
  "jump-reference": [sql: string, ref: string, target: TableTarget | null];
}>();

const active = computed(() => props.tabs.find((tab) => tab.id === props.activeId) ?? null);
const pageSize = computed(() => props.pageSize ?? loadSettings().pageSize);

function paneOf(tab: { tablePane?: "data" | "structure" }): "data" | "structure" {
  return tab.tablePane ?? "data";
}

function showPane(tabId: string, pane: "data" | "structure") {
  emit("table-view", tabId, pane);
}

function tableTarget(tabId: string) {
  return resolveTableTarget(tabId, catalogConnections());
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
  <div v-if="tabs.length" class="tabbar" role="tablist" aria-label="打开的标签" @contextmenu="onTabBarContext">
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
      <Icon :name="tab.kind === 'query' ? 'code' : 'table'" />
      <span class="tab-title">{{ tab.title }}</span>
      <span
        v-if="tab.targetText || tab.path || tab.kind === 'query'"
        class="tab-target"
        :class="{ none: !tab.targetText && !tab.path }"
      >{{ tab.targetText || tab.path || "未选择连接" }}</span>
      <button class="tab-x" type="button" :aria-label="`关闭 ${tab.title}`" title="关闭" @click.stop="emit('close', tab.id)">
        <Icon name="x" />
      </button>
    </div>
    <button class="icon-btn sm tab-new" type="button" aria-label="新建查询" title="新建查询" @click="emit('new-query')">
      <Icon name="plus" />
    </button>
  </div>
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
      @target="onTarget"
    />
    <div v-else-if="active && active.kind === 'table'" class="table-workspace">
      <div class="pane-switch" role="tablist" aria-label="表内容">
        <button type="button" role="tab" :aria-selected="paneOf(active) === 'data'" @click="showPane(active.id, 'data')">数据</button>
        <button type="button" role="tab" :aria-selected="paneOf(active) === 'structure'" @click="showPane(active.id, 'structure')">结构</button>
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
      :label="active.title"
      :object="definitionObject(active)!"
      @copy-sql="emit('copy-definition', $event)"
    />
    <WelcomeView
      v-else
      @new-query="emit('new-query')"
      @connections="emit('connections')"
      @sqlite="emit('sqlite')"
    />
  </div>
  <Teleport to="body">
    <div
      v-if="menu && menuTab"
      class="menu tab-menu"
      data-menu="tab"
      role="menu"
      :aria-label="`${menuTab.title} 的操作`"
      :style="{ left: `${menu.x}px`, top: `${menu.y}px` }"
      @mousedown.stop
      @contextmenu.prevent
    >
      <template v-if="menuTab.kind === 'query'">
        <div class="menu-head">选择执行目标</div>
        <p v-if="!menuTargets.length" class="target-empty">还没有可用的连接——先在「连接」页新建一个。</p>
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
        <button class="menu-item" type="button" data-action="show-data" @click="showTablePane('data')">数据</button>
        <button class="menu-item" type="button" data-action="show-structure" @click="showTablePane('structure')">结构</button>
        <div class="menu-sep" role="separator"></div>
      </template>
      <button class="menu-item" type="button" data-action="close-tab" @click="closeIds([menuTab.id])">关闭</button>
      <button class="menu-item" type="button" data-action="close-others" :disabled="tabs.length < 2" @click="closeOthers(menuTab.id)">关闭其他</button>
      <button class="menu-item" type="button" data-action="close-right" :disabled="isLastTab(menuTab.id)" @click="closeToTheRight(menuTab.id)">关闭右侧</button>
      <button class="menu-item" type="button" data-action="close-all" @click="closeIds(tabs.map((tab) => tab.id))">关闭全部</button>
    </div>
  </Teleport>
  </div>
</template>
