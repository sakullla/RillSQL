<script setup lang="ts">
import { computed } from "vue";
import DataGrid from "../grid/DataGrid.vue";
import QueryTab from "../query/QueryTab.vue";
import StructurePane from "../structure/StructurePane.vue";
import Icon from "./Icon.vue";
import type { ShellTab } from "./session";
import WelcomeView from "./WelcomeView.vue";

const props = defineProps<{ tabs: ShellTab[]; activeId: string | null }>();
const emit = defineEmits<{
  activate: [id: string];
  close: [id: string];
  "new-query": [];
  connections: [];
  sqlite: [];
  palette: [];
}>();

const active = computed(() => props.tabs.find((tab) => tab.id === props.activeId) ?? null);
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
      <span class="tab-conn none">{{ tab.targetText ?? "未绑定" }}</span>
      <button class="tab-x" type="button" :aria-label="`关闭 ${tab.title}`" title="关闭" @click.stop="emit('close', tab.id)">
        <Icon name="x" />
      </button>
    </div>
    <button class="icon-btn sm tab-new" type="button" aria-label="新建查询" title="新建查询" @click="emit('new-query')">
      <Icon name="plus" />
    </button>
  </div>
  <div class="tab-body" id="tab-body">
    <QueryTab v-if="active?.kind === 'query'" />
    <DataGrid v-else-if="active?.kind === 'table'" />
    <StructurePane v-else-if="active?.kind === 'structure'" />
    <WelcomeView
      v-else
      @new-query="emit('new-query')"
      @connections="emit('connections')"
      @sqlite="emit('sqlite')"
      @palette="emit('palette')"
    />
  </div>
</template>
