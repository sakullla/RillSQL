<script setup lang="ts">
import { computed } from "vue";
import Icon from "./Icon.vue";
import Tooltip from "../ui/Tooltip.vue";
import type { ThemePreference } from "./theme";
import type { ToolbarAction } from "./toolbar-model";

const props = defineProps<{ hasQueryTab: boolean; theme: ThemePreference }>();
const emit = defineEmits<{ action: [action: ToolbarAction] }>();

const themeIcon = computed(() => (props.theme === "system" ? "monitor" : props.theme === "dark" ? "moon" : "sun"));
const themeLabel = computed(() => (props.theme === "system" ? "跟随系统" : props.theme === "dark" ? "深色" : "浅色"));
</script>

<template>
  <div class="toolbar" role="toolbar" aria-label="主工具栏">
    <div class="tb-group">
      <Tooltip text="新建查询 (Ctrl+T)">
        <button class="icon-btn" type="button" aria-label="新建查询" data-action="new-query" @click="emit('action', 'new-query')"><Icon name="plus" /></button>
      </Tooltip>
      <Tooltip text="打开连接">
        <button class="icon-btn" type="button" aria-label="打开连接" data-action="open-connections" @click="emit('action', 'open-connections')"><Icon name="database" /></button>
      </Tooltip>
      <Tooltip text="保存当前查询">
        <button class="icon-btn" type="button" aria-label="保存当前查询" data-action="save-draft" @click="emit('action', 'save-draft')"><Icon name="save" /></button>
      </Tooltip>
    </div>
    <span class="tb-sep" role="separator"></span>
    <div class="tb-group">
      <Tooltip text="格式化 (Ctrl+Shift+F)">
        <button class="icon-btn" type="button" aria-label="格式化" data-action="format" :disabled="!hasQueryTab" @click="emit('action', 'format')"><Icon name="format" /></button>
      </Tooltip>
      <Tooltip text="执行计划 (Ctrl+Shift+E)">
        <button class="icon-btn" type="button" aria-label="执行计划" data-action="plan" :disabled="!hasQueryTab" @click="emit('action', 'plan')"><Icon name="plan" /></button>
      </Tooltip>
      <Tooltip text="查询历史">
        <button class="icon-btn" type="button" aria-label="查询历史" data-action="history" @click="emit('action', 'history')"><Icon name="history" /></button>
      </Tooltip>
    </div>
    <span class="tb-sep" role="separator"></span>
    <div class="tb-center"><slot /></div>
    <div class="tb-group">
      <Tooltip :text="`外观主题：${themeLabel}`">
        <button class="icon-btn" type="button" :aria-label="`外观主题：${themeLabel}`" data-action="toggle-theme" @click="emit('action', 'toggle-theme')"><Icon :name="themeIcon" /></button>
      </Tooltip>
      <Tooltip text="任务中心">
        <button class="icon-btn" type="button" aria-label="任务中心" data-action="tasks" @click="emit('action', 'tasks')"><Icon name="list" /></button>
      </Tooltip>
      <Tooltip text="命令面板 (Ctrl+K)">
        <button class="icon-btn" type="button" aria-label="命令面板" data-action="palette" @click="emit('action', 'palette')"><Icon name="command" /></button>
      </Tooltip>
      <button class="btn sm primary toolbar-run" type="button" aria-label="执行" data-action="run" :disabled="!hasQueryTab" @click="emit('action', 'run')">
        <Icon name="play" /> 执行 <kbd>Ctrl Enter</kbd>
      </button>
    </div>
  </div>
</template>

<style scoped>
.toolbar { display: flex; align-items: center; gap: var(--sp-1); }
.tb-group { display: flex; align-items: center; gap: 2px; }
.tb-sep { width: 1px; height: 18px; background: var(--line); margin: 0 var(--sp-2); }
.tb-center { flex: 1; display: flex; justify-content: center; min-width: 0; padding: 0 var(--sp-3); }
.toolbar-run { margin-left: var(--sp-2); }
.toolbar-run kbd { font: 500 var(--fs-xs)/1 var(--font); opacity: .85; border-color: color-mix(in srgb, var(--on-accent) 45%, transparent); }
</style>
