<script setup lang="ts">
import type { ThemePreference } from "./theme";
import BrandMark from "./BrandMark.vue";
import ChordKeys from "./ChordKeys.vue";
import Icon from "./Icon.vue";
import TaskCenter from "../export/TaskCenter.vue";

defineProps<{ theme: ThemePreference }>();
const emit = defineEmits<{
  palette: [];
  help: [];
  "set-theme": [theme: ThemePreference];
}>();

const themeIcon = (theme: ThemePreference) => (theme === "system" ? "monitor" : theme === "dark" ? "moon" : "sun");
const choices: { id: ThemePreference; label: string }[] = [
  { id: "light", label: "浅色" },
  { id: "dark", label: "深色" },
  { id: "system", label: "跟随系统" },
];
</script>

<template>
  <header class="titlebar">
    <div class="tb-brand">
      <BrandMark />
      <strong>RillSQL</strong>
      <span class="brand-cn">澄库</span>
    </div>
    <button class="cmd-trigger" type="button" aria-label="打开命令面板" @click="emit('palette')">
      <Icon name="search" />
      <span>搜索表、连接或命令</span>
      <ChordKeys chord="Mod+K" />
    </button>
    <div class="tb-actions">
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
    </div>
  </header>
</template>

<style scoped>
.theme-menu { position: relative; }
.theme-menu summary { list-style: none; }
.theme-menu summary::-webkit-details-marker { display: none; }
.theme-menu-pop { position: absolute; right: 0; top: 32px; }
.menu-item .check { margin-left: auto; }
</style>
