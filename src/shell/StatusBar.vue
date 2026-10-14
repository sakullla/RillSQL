<script setup lang="ts">
import { computed } from "vue";
import { PRODUCT_TITLE } from "./product";
import ChordKeys from "./ChordKeys.vue";
import Icon from "./Icon.vue";
import type { ShellTab } from "./session";
import { CONNECTION_STATUS_TEXT } from "./status-text";

const props = defineProps<{ tab: ShellTab | null; onWorkspace: boolean }>();
const emit = defineEmits<{ help: [] }>();
const statusLabel = computed(() => {
  const status = props.tab?.connectionStatus;
  return status ? CONNECTION_STATUS_TEXT[status] : "状态未知";
});
</script>

<template>
  <footer class="statusbar">
    <div class="sb-left">
      <span v-if="onWorkspace && tab && !tab.targetText" class="sb-item warn">未绑定连接 · 请选择执行目标</span>
      <span v-else-if="onWorkspace && tab?.targetText" class="sb-item sb-conn">
        <strong>{{ tab.targetText }}</strong>
        <span class="conn-state" :class="tab.connectionStatus ? `st-${tab.connectionStatus}` : ''"><i></i>{{ statusLabel }}</span>
      </span>
      <span v-else class="sb-item muted">{{ PRODUCT_TITLE }}</span>
    </div>
    <div class="sb-right">
      <span class="sb-item" title="无需账号、不联网同步；配置与历史只保存在本机">
        <Icon name="lock" />仅存本机
      </span>
      <button class="sb-item" type="button" @click="emit('help')">
        <ChordKeys chord="?" />快捷键
      </button>
    </div>
  </footer>
</template>
