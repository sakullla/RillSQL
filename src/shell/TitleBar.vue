<script setup lang="ts">
import { computed, watch } from "vue";
import type { ThemePreference } from "./theme";
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
const choices: { id: ThemePreference; label: string }[] = [
  { id: "light", label: "浅色" },
  { id: "dark", label: "深色" },
  { id: "system", label: "跟随系统" },
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
      <span class="brand-cn">澄库</span>
      <button class="icon-btn sidebar-toggle" data-tauri-drag-region="false" type="button" aria-label="显示或隐藏资源管理器" title="资源管理器 (Ctrl+B)" @click="emit('toggle-sidebar')"><Icon name="sidebar" /></button>
    </div>
    <button class="cmd-trigger" type="button" aria-label="打开命令面板" @click="emit('palette')">
      <Icon name="search" />
      <span>搜索表、连接或命令</span>
      <ChordKeys chord="Mod+K" />
    </button>
    <div class="tb-actions" :data-tauri-drag-region="showCaption ? 'false' : undefined">
      <button class="btn sm title-query" type="button" @click="emit('new-query')"><Icon name="plus" /> SQL 查询</button>
      <TaskCenter />
      <details class="theme-menu">
        <summary class="icon-btn" :aria-label="`外观主题：${choices.find((item) => item.id === theme)?.label}`" title="外观主题">
          <Icon :name="themeIcon(theme)" />
        </summary>
        <div class="menu theme-menu-pop" role="menu" aria-label="外观">
          <div class="menu-head">外观</div>
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
            <span>{{ choice.label }}</span>
            <Icon v-if="theme === choice.id" class="check" name="check" />
          </button>
        </div>
      </details>
      <button class="icon-btn" type="button" title="快捷键 (?)" aria-label="快捷键与帮助" @click="emit('help')">
        <Icon name="keyboard" />
      </button>
      <div v-if="showCaption" class="caption" role="group" aria-label="窗口">
        <button type="button" aria-label="最小化" title="最小化" @click="chrome.minimize()">
          <svg viewBox="0 0 10 10" aria-hidden="true"><path fill="currentColor" d="M0 5h10v1H0z" /></svg>
        </button>
        <button type="button" :aria-label="chrome.maximized.value ? '还原' : '最大化'" :title="chrome.maximized.value ? '还原' : '最大化'" @click="chrome.toggleMaximize()">
          <svg v-if="chrome.maximized.value" viewBox="0 0 10 10" aria-hidden="true"><path fill="currentColor" d="M2.6.8h6.6v6.6H8V2H2.6V.8zM.8 2.6h6.6v6.6H.8V2.6zm1 1v4.6h4.6V3.6H1.8z" /></svg>
          <svg v-else viewBox="0 0 10 10" aria-hidden="true"><rect x="0.6" y="0.6" width="8.8" height="8.8" rx="1.2" fill="none" stroke="currentColor" stroke-width="1" /></svg>
        </button>
        <button class="caption-close" type="button" aria-label="关闭" title="关闭" @click="chrome.close()">
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
