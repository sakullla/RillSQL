<script setup lang="ts">
import { computed, watch } from "vue";
import type { ThemePreference } from "./theme";
import { tr as t } from "../i18n";
import BrandMark from "./BrandMark.vue";
import ChordKeys from "./ChordKeys.vue";
import Icon from "./Icon.vue";
import TaskCenter from "../export/TaskCenter.vue";
import { useWindowChrome } from "./window-chrome";

const props = defineProps<{ theme: ThemePreference; caption?: boolean }>();
const emit = defineEmits<{
  palette: [];
  help: [];
  "new-query": [];
  "toggle-sidebar": [];
  "set-theme": [theme: ThemePreference];
  "window-state": [state: { custom: boolean; maximized: boolean }];
}>();
const chrome = useWindowChrome();
const showCaption = computed(() => props.caption === true || chrome.customChrome.value);
watch(
  [showCaption, chrome.maximized],
  () => emit("window-state", { custom: showCaption.value, maximized: chrome.maximized.value }),
  { immediate: true },
);

const themeIcon = (theme: ThemePreference) => (theme === "system" ? "monitor" : theme === "dark" ? "moon" : "sun");
const themeLabel = (theme: ThemePreference) => t(theme === "light" ? "shell.theme.light" : theme === "dark" ? "shell.theme.dark" : "shell.theme.system");
const choices: { id: ThemePreference }[] = [
  { id: "light" },
  { id: "dark" },
  { id: "system" },
];
</script>

<template>
  <header
    class="titlebar"
    :class="{ 'custom-chrome': showCaption, 'is-inactive': showCaption && !chrome.focused.value }"
    :data-tauri-drag-region="showCaption ? 'deep' : undefined"
  >
    <div class="tb-brand">
      <BrandMark />
      <strong>RillSQL</strong>
      <button class="icon-btn sidebar-toggle" data-tauri-drag-region="false" type="button" :aria-label="t('shell.titlebar.toggleSidebar')" :title="t('shell.titlebar.sidebarTitle')" @click="emit('toggle-sidebar')"><Icon name="sidebar" /></button>
    </div>
    <button class="cmd-trigger" type="button" :aria-label="t('shell.titlebar.openPalette')" @click="emit('palette')">
      <Icon name="search" />
      <span>{{ t("shell.titlebar.searchPlaceholder") }}</span>
      <ChordKeys chord="Mod+K" />
    </button>
    <div class="tb-actions" :data-tauri-drag-region="showCaption ? 'false' : undefined">
      <button class="btn sm title-query" type="button" @click="emit('new-query')"><Icon name="plus" />{{ t("shell.titlebar.sqlQuery") }}</button>
      <TaskCenter />
      <details class="theme-menu">
        <summary class="icon-btn" :aria-label="t('shell.titlebar.themeMenuLabel', { theme: themeLabel(theme) })" :title="t('shell.titlebar.appearanceMenu')">
          <Icon :name="themeIcon(theme)" />
        </summary>
        <div class="menu theme-menu-pop" role="menu" :aria-label="t('shell.titlebar.appearanceMenu')">
          <div class="menu-head">{{ t("shell.titlebar.appearanceMenu") }}</div>
          <button
            v-for="choice in choices"
            :key="choice.id"
            class="menu-item"
            type="button"
            role="menuitemradio"
            :aria-checked="theme === choice.id"
            @click="emit('set-theme', choice.id)"
          >
            <Icon :name="themeIcon(choice.id)" />
            <span>{{ themeLabel(choice.id) }}</span>
            <Icon v-if="theme === choice.id" class="check" name="check" />
          </button>
        </div>
      </details>
      <button class="icon-btn" type="button" :title="t('shell.titlebar.shortcutsTitle')" :aria-label="t('shell.titlebar.shortcutsHelp')" @click="emit('help')">
        <Icon name="keyboard" />
      </button>
      <div v-if="showCaption" class="caption" role="group" :aria-label="t('shell.titlebar.window')">
        <button type="button" :aria-label="t('shell.titlebar.minimize')" :title="t('shell.titlebar.minimize')" @click="chrome.minimize()">
          <svg viewBox="0 0 10 10" aria-hidden="true"><path fill="currentColor" d="M0 5h10v1H0z" /></svg>
        </button>
        <button type="button" :aria-label="chrome.maximized.value ? t('shell.titlebar.restore') : t('shell.titlebar.maximize')" :title="chrome.maximized.value ? t('shell.titlebar.restore') : t('shell.titlebar.maximize')" @click="chrome.toggleMaximize()">
          <svg v-if="chrome.maximized.value" viewBox="0 0 10 10" aria-hidden="true"><path fill="currentColor" d="M2.6.8h6.6v6.6H8V2H2.6V.8zM.8 2.6h6.6v6.6H.8V2.6zm1 1v4.6h4.6V3.6H1.8z" /></svg>
          <svg v-else viewBox="0 0 10 10" aria-hidden="true"><rect x="0.6" y="0.6" width="8.8" height="8.8" rx="1.2" fill="none" stroke="currentColor" stroke-width="1" /></svg>
        </button>
        <button class="caption-close" type="button" :aria-label="t('common.close')" :title="t('common.close')" @click="chrome.close()">
          <svg viewBox="0 0 10 10" aria-hidden="true"><path fill="currentColor" d="M1.1.4 5 4.3 8.9.4l.7.7L5.7 5l3.9 3.9-.7.7L5 5.7 1.1 9.6l-.7-.7L4.3 5 .4 1.1z" /></svg>
        </button>
      </div>
    </div>
  </header>
</template>

<style scoped>
.theme-menu { position: relative; }
.theme-menu summary { list-style: none; }
.theme-menu summary::-webkit-details-marker { display: none; }
.theme-menu-pop { position: absolute; right: 0; top: calc(100% + 6px); }
.menu-item .check { margin-left: auto; }
</style>
