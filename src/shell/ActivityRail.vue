<script setup lang="ts">
import type { AppPage } from "../router";
import { tr as t } from "../i18n";
import ChordKeys from "./ChordKeys.vue";
import Icon from "./Icon.vue";
import type { IconName } from "./icons";

defineProps<{ page: AppPage }>();
const emit = defineEmits<{ navigate: [page: AppPage] }>();

const items: { id: AppPage; icon: IconName; label: string }[] = [
  { id: "workspace", icon: "terminal", label: "shell.nav.workspace" },
  { id: "connections", icon: "database", label: "shell.nav.connections" },
  { id: "history", icon: "history", label: "shell.nav.history" },
];
</script>

<template>
  <nav class="rail" :aria-label="t('shell.nav.main')">
    <button
      v-for="item in items"
      :key="item.id"
      class="rail-btn"
      :class="{ active: page === item.id }"
      type="button"
      :aria-label="t(item.label)"
      :aria-current="page === item.id ? 'page' : 'false'"
      :title="item.id === 'workspace' ? t('shell.nav.workspaceHint') : t(item.label)"
      @click="emit('navigate', item.id)"
    >
      <Icon :name="item.icon" />
      <span class="rail-label" aria-hidden="true">{{ t(item.label) }}</span>
    </button>
    <span class="grow"></span>
    <button
      class="rail-btn"
      :class="{ active: page === 'settings' }"
      type="button"
      :aria-label="t('shell.nav.settings')"
      :aria-current="page === 'settings' ? 'page' : 'false'"
      :title="t('shell.nav.settings')"
      @click="emit('navigate', 'settings')"
    >
      <Icon name="sliders" />
      <span class="rail-label" aria-hidden="true">{{ t("shell.nav.settings") }}</span>
      <ChordKeys class="sr" chord="Mod+," />
    </button>
  </nav>
</template>
