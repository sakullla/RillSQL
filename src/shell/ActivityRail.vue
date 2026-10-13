<script setup lang="ts">
import type { AppPage } from "../router";
import ChordKeys from "./ChordKeys.vue";
import Icon from "./Icon.vue";
import type { IconName } from "./icons";

defineProps<{ page: AppPage }>();
const emit = defineEmits<{ navigate: [page: AppPage] }>();

const items: { id: AppPage; icon: IconName; label: string }[] = [
  { id: "workspace", icon: "terminal", label: "工作台" },
  { id: "connections", icon: "database", label: "连接" },
  { id: "history", icon: "history", label: "查询历史" },
];
</script>

<template>
  <nav class="rail" aria-label="主导航">
    <button
      v-for="item in items"
      :key="item.id"
      class="rail-btn"
      :class="{ active: page === item.id }"
      type="button"
      :aria-label="item.label"
      :aria-current="page === item.id ? 'page' : 'false'"
      :title="item.id === 'workspace' ? '工作台（再次点击切换资源管理器）' : item.label"
      @click="emit('navigate', item.id)"
    >
      <Icon :name="item.icon" />
      <span>{{ item.label }}</span>
    </button>
    <span class="grow"></span>
    <button
      class="rail-btn"
      :class="{ active: page === 'settings' }"
      type="button"
      aria-label="设置"
      :aria-current="page === 'settings' ? 'page' : 'false'"
      title="设置"
      @click="emit('navigate', 'settings')"
    >
      <Icon name="sliders" />
      <span>设置</span>
      <ChordKeys class="sr" chord="Mod+," />
    </button>
  </nav>
</template>
