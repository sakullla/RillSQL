<script setup lang="ts">
import { computed } from "vue";
import { catalogConnections } from "../catalog/store";
import DataGrid from "../grid/DataGrid.vue";
import { resolveTableTarget } from "../grid/target";
import QueryTab from "../query/QueryTab.vue";
import StructurePane from "../structure/StructurePane.vue";
import Icon from "./Icon.vue";
import type { ShellTab } from "./session";
import { loadSettings } from "./settings";
import WelcomeView from "./WelcomeView.vue";

const props = defineProps<{ tabs: ShellTab[]; activeId: string | null }>();
const emit = defineEmits<{
  activate: [id: string];
  close: [id: string];
  "new-query": [];
  connections: [];
  sqlite: [];
  palette: [];
  structure: [];
  draft: [id: string, draft: string];
}>();

const active = computed(() => props.tabs.find((tab) => tab.id === props.activeId) ?? null);
const pageSize = computed(() => loadSettings().pageSize);

function tableTarget(tabId: string) {
  return resolveTableTarget(tabId, catalogConnections());
}

function onDraft(tabId: string, draft: string) {
  emit("draft", tabId, draft);
}
</script>

<template>
  <div v-if="tabs.length" class="tabbar" role="tablist" aria-label="打开的标签">
    <div
      v-for="tab in tabs"
      :key="tab.id"
      class="tab"
      :class="{ active: tab.id === activeId }"
      role="tab"
      :aria-selected="tab.id === activeId"
      :tabindex="tab.id === activeId ? 0 : -1"
      @click="emit('activate', tab.id)"
    >
      <Icon :name="tab.kind === 'query' ? 'code' : 'table'" />
      <span class="tab-title">{{ tab.title }}</span>
      <span class="tab-conn none">{{ tab.targetText || tab.path || "未绑定" }}</span>
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
      @update:draft="onDraft"
    />
    <DataGrid
      v-else-if="active && active.kind === 'table'"
      :key="active.id"
      :tab-id="active.id"
      :draft="active.draft"
      :page-size="pageSize"
      :target="tableTarget(active.id)"
    />
    <StructurePane
      v-else-if="active && active.kind === 'structure'"
      :key="active.id"
      :tab-id="active.id"
      :draft="active.draft"
    />
    <WelcomeView
      v-else
      @new-query="emit('new-query')"
      @connections="emit('connections')"
      @sqlite="emit('sqlite')"
      @palette="emit('palette')"
      @structure="emit('structure')"
    />
  </div>
</template>
