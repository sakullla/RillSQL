<script setup lang="ts">
import { computed, watch } from "vue";
import type { ThemePreference } from "./theme";
import BrandMark from "./BrandMark.vue";
import ChordKeys from "./ChordKeys.vue";
import Icon from "./Icon.vue";
import MenuBar from "./MenuBar.vue";
import Toolbar from "./Toolbar.vue";
import type { ToolbarAction } from "./toolbar-model";
import TaskCenter from "../export/TaskCenter.vue";
import { useWindowChrome } from "./window-chrome";
import type { MenuDefinition } from "./menu-model";

const props = defineProps<{
  theme: ThemePreference;
  caption?: boolean;
  menus: MenuDefinition[];
  hasQueryTab: boolean;
}>();
const emit = defineEmits<{
  palette: [];
  help: [];
  "toggle-sidebar": [];
  "window-state": [state: { custom: boolean; maximized: boolean }];
  "menu-select": [menuId: string, itemId: string];
  action: [action: ToolbarAction];
}>();
const chrome = useWindowChrome();
const showCaption = computed(() => props.caption === true || chrome.customChrome.value);
watch(
  [showCaption, chrome.maximized],
  () => emit("window-state", { custom: showCaption.value, maximized: chrome.maximized.value }),
  { immediate: true },
);
</script>

<template>
  <header class="titlebar" :class="{ 'custom-chrome': showCaption, 'is-inactive': showCaption && !chrome.focused.value }">
    <div class="tb-row tb-menu-row" :data-tauri-drag-region="showCaption ? 'deep' : undefined">
      <div class="tb-brand" data-tauri-drag-region="false">
        <BrandMark />
        <strong>RillSQL</strong>
        <span class="brand-cn">澄库</span>
      </div>
      <!-- MenuBar 是多根组件，属性不会自动落到 .menubar 上，包一层显式标记不可拖 -->
      <div data-tauri-drag-region="false">
        <MenuBar :menus="menus" @select="(menuId, itemId) => emit('menu-select', menuId, itemId)" />
      </div>
      <span class="grow" :data-tauri-drag-region="showCaption ? 'deep' : undefined"></span>
      <div v-if="showCaption" class="caption" role="group" aria-label="窗口" data-tauri-drag-region="false">
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
    <div class="tb-row tb-tool-row" data-tauri-drag-region="false">
      <Toolbar :has-query-tab="hasQueryTab" :theme="theme" @action="(action) => emit('action', action)">
        <button class="cmd-trigger" type="button" aria-label="打开命令面板" @click="emit('palette')">
          <Icon name="search" />
          <span>搜索表、连接或命令</span>
          <ChordKeys chord="Mod+K" />
        </button>
      </Toolbar>
      <TaskCenter />
      <button class="icon-btn" type="button" title="快捷键 (?)" aria-label="快捷键与帮助" @click="emit('help')">
        <Icon name="keyboard" />
      </button>
      <button class="icon-btn" type="button" title="显示或隐藏资源管理器" aria-label="显示或隐藏资源管理器" @click="emit('toggle-sidebar')">
        <Icon name="sidebar" />
      </button>
    </div>
  </header>
</template>

<style scoped>
.tb-row { display: flex; align-items: center; gap: var(--sp-2); padding: 0 var(--sp-2); min-width: 0; }
.tb-menu-row { height: var(--titlebar-h); }
/* border-box 下固定高度 + 实体 border 会把内容盒压掉 1px，子行被压缩；分隔线用 inset 阴影表达 */
.tb-tool-row { height: var(--toolbar-h); box-shadow: inset 0 1px 0 var(--line); }
.cmd-trigger {
  display: flex; align-items: center; gap: var(--sp-2); width: 300px; height: var(--control-h-sm);
  padding: 0 var(--sp-2); border-radius: var(--r); background: var(--panel-2);
  border: 1px solid var(--line); color: var(--text-3); font-size: var(--fs-sm);
}
.cmd-trigger:hover { border-color: var(--line-2); color: var(--text-2); }
.cmd-trigger:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
</style>
