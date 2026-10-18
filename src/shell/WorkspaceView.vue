<script setup lang="ts">
import { computed, defineAsyncComponent, ref } from "vue";
import { catalogConnections } from "../catalog/store";
import { resolveTableTarget } from "../grid/target";
import { targetDetail, targetKey, targetOptions, type TargetOption } from "../query/complete";
import "../structure/structure.css";
import { ContextMenu, type MenuItem } from "../ui";
import Icon from "./Icon.vue";
import type { QueryAction, QueryTarget, ShellTab } from "./session";
import { loadSettings } from "./settings";
import { chordByLabel } from "./shortcuts";
import WelcomeView from "./WelcomeView.vue";

const DataGrid = defineAsyncComponent(() => import("../grid/DataGrid.vue"));
const QueryTab = defineAsyncComponent(() => import("../query/QueryTab.vue"));
const StructurePane = defineAsyncComponent(() => import("../structure/StructurePane.vue"));

const props = defineProps<{ tabs: ShellTab[]; activeId: string | null; pageSize?: number; queryAction?: { action: QueryAction; nonce: number } | null }>();
const emit = defineEmits<{
  activate: [id: string];
  close: [id: string];
  "toggle-pin": [id: string];
  "new-query": [];
  connections: [];
  sqlite: [];
  palette: [];
  structure: [];
  "query-target": [target: QueryTarget];
  draft: [id: string, draft: string];
  target: [id: string, target: QueryTarget];
  "table-view": [id: string, pane: "data" | "structure"];
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

function onDraft(tabId: string, draft: string) {
  emit("draft", tabId, draft);
}

function onTarget(target: QueryTarget) {
  if (!active.value) return;
  emit("target", active.value.id, target);
}

const menu = ref<{ tabId: string; x: number; y: number } | null>(null);
const menuEl = ref<HTMLElement | null>(null);
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

const menuLabel = computed(() => (menuTab.value ? `${menuTab.value.title} 的操作` : "操作"));

const menuItems = computed<MenuItem[]>(() => {
  const tab = menuTab.value;
  if (!tab) return [];
  const items: MenuItem[] = [];
  if (tab.kind === "query") {
    items.push({ kind: "head", text: "选择执行目标" });
    if (!menuTargets.value.length) items.push({ kind: "head", text: "还没有可用的连接——先在「连接」页新建一个。" });
    menuTargets.value.forEach((option, index) => {
      items.push({
        kind: "item",
        id: `bind-target:${index}`,
        label: option.label,
        title: targetDetail(option),
        checked: option.key === currentTargetKey(tab),
      });
    });
    items.push({ kind: "sep" });
  } else if (tab.kind === "table") {
    items.push(
      { kind: "item", id: "show-data", label: "数据" },
      { kind: "item", id: "show-structure", label: "结构" },
      { kind: "sep" },
    );
  }
  const index = props.tabs.findIndex((item) => item.id === tab.id);
  items.push(
    { kind: "item", id: "close", label: "关闭", danger: true, shortcut: chordByLabel("关闭当前标签") },
    { kind: "item", id: "close-others", label: "关闭其他", danger: true, disabled: props.tabs.length < 2 },
    { kind: "item", id: "close-right", label: "关闭右侧", danger: true, disabled: index < 0 || index >= props.tabs.length - 1 },
    { kind: "item", id: "close-all", label: "全部关闭", danger: true },
    { kind: "sep" },
    { kind: "item", id: "copy-sql", label: "复制 SQL", disabled: tab.kind !== "query" || !tab.draft },
    { kind: "item", id: "pin-toggle", label: tab.pinned ? "取消固定" : "固定", checked: tab.pinned },
  );
  return items;
});

function onTabBarContext(event: MouseEvent) {
  event.preventDefault();
  const node = event.target;
  if (!(node instanceof Element)) return;
  const tabEl = node.closest<HTMLElement>(".tab");
  if (!tabEl) {
    closeMenu();
    return;
  }
  const tab = props.tabs.find((item) => item.id === tabEl.dataset.tabId);
  if (!tab) return;
  // 先激活并聚焦触发标签：菜单关闭时 ContextMenu 会把焦点还回来，键盘用户不掉焦点。
  emit("activate", tab.id);
  tabEl.focus();
  menuEl.value = tabEl;
  menu.value = { tabId: tab.id, x: Math.max(8, event.clientX), y: Math.max(8, event.clientY) };
}

function closeMenu() {
  menu.value = null;
}

function closeIds(ids: string[]) {
  for (const id of ids) emit("close", id);
}

/** 关闭其他：保留当前标签，跳过其余已固定标签。 */
function closeOthers(id: string) {
  closeIds(props.tabs.filter((tab) => tab.id !== id && !tab.pinned).map((tab) => tab.id));
}

/** 关闭右侧：已固定标签不在关闭范围内。 */
function closeToTheRight(id: string) {
  const index = props.tabs.findIndex((tab) => tab.id === id);
  closeIds(props.tabs.slice(index + 1).filter((tab) => !tab.pinned).map((tab) => tab.id));
}

function bindTarget(tabId: string, option: TargetOption) {
  emit("target", tabId, {
    connectionId: option.connectionId,
    connectionName: option.name,
    database: option.database,
    engine: option.engine,
    status: option.status,
  });
}

function showTablePane(pane: "data" | "structure") {
  const tab = menuTab.value;
  if (!tab) return;
  emit("table-view", tab.id, pane);
}

function onMenuSelect(id: string) {
  const tab = menuTab.value;
  if (!tab) return;
  if (id.startsWith("bind-target:")) {
    const option = menuTargets.value[Number(id.slice("bind-target:".length))];
    if (option) bindTarget(tab.id, option);
    return;
  }
  if (id === "show-data") return showTablePane("data");
  if (id === "show-structure") return showTablePane("structure");
  if (id === "close") return closeIds([tab.id]);
  if (id === "close-others") return closeOthers(tab.id);
  if (id === "close-right") return closeToTheRight(tab.id);
  if (id === "close-all") return closeIds(props.tabs.map((item) => item.id));
  if (id === "copy-sql") {
    void navigator.clipboard.writeText(tab.draft).catch(() => undefined);
    return;
  }
  if (id === "pin-toggle") emit("toggle-pin", tab.id);
}
</script>

<template>
  <div class="workspace-view">
  <div v-if="tabs.length" class="tabbar" role="tablist" aria-label="打开的标签" @contextmenu="onTabBarContext">
    <div
      v-for="tab in tabs"
      :key="tab.id"
      class="tab"
      :class="{ active: tab.id === activeId, pinned: tab.pinned }"
      role="tab"
      :data-tab-id="tab.id"
      :aria-selected="tab.id === activeId"
      :tabindex="tab.id === activeId ? 0 : -1"
      @click="emit('activate', tab.id)"
      @keydown.enter.prevent="emit('activate', tab.id)"
      @keydown.space.prevent="emit('activate', tab.id)"
      @keydown="onTabKey($event, tab.id)"
    >
      <Icon v-if="tab.pinned" name="pin" label="已固定" />
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
      :query-action="queryAction"
      @update:draft="onDraft"
      @target="onTarget"
      @palette="emit('palette')"
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
    <WelcomeView
      v-else
      @new-query="emit('new-query')"
      @connections="emit('connections')"
      @sqlite="emit('sqlite')"
    />
  </div>
  <ContextMenu
    :open="Boolean(menu)"
    :x="menu?.x ?? 0"
    :y="menu?.y ?? 0"
    :items="menuItems"
    :label="menuLabel"
    :trigger="menuEl"
    @close="closeMenu"
    @select="onMenuSelect"
  />
  </div>
</template>
