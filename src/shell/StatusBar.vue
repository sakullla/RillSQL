<script setup lang="ts">
import { computed } from "vue";
import { PRODUCT_TITLE } from "./product";
import Icon from "./Icon.vue";
import type { ShellTab } from "./session";
import { CONNECTION_STATUS_TEXT, OPERATION_STATE_TEXT, type OperationActivity, type OperationState } from "./status-text";

const props = defineProps<{ tab: ShellTab | null; onWorkspace: boolean; activity?: OperationActivity | null }>();

const statusLabel = computed(() => {
  const status = props.tab?.connectionStatus;
  return status ? CONNECTION_STATUS_TEXT[status] : null;
});

const ACTIVITY_TONE: Record<OperationState, "info" | "ok" | "err" | "warn"> = {
  running: "info",
  ok: "ok",
  error: "err",
  unknown: "warn",
};
</script>

<template>
  <footer class="statusbar">
    <div class="sb-left">
      <template v-if="onWorkspace && tab">
        <span v-if="tab.kind === 'query' && !tab.targetText" class="sb-item sb-warn">
          <Icon name="database" />此查询还未选择执行目标——在查询里选一个连接和库
        </span>
        <span v-else-if="tab.targetText" class="sb-item sb-target">
          <Icon name="database" />
          <strong>{{ tab.targetText }}</strong>
          <span v-if="statusLabel" class="conn-state" :class="`st-${tab.connectionStatus}`"><i></i>{{ statusLabel }}</span>
        </span>
        <span v-else class="sb-item sb-target">
          <Icon name="table" />
          <strong>{{ tab.title }}</strong>
          <span v-if="tab.path" class="muted">{{ tab.path }}</span>
        </span>
      </template>
      <span v-else class="sb-item muted">{{ PRODUCT_TITLE }}</span>
    </div>
    <div class="sb-right">
      <span v-if="activity" class="sb-activity" :data-state="activity.state">
        <span class="badge" :class="ACTIVITY_TONE[activity.state]">{{ OPERATION_STATE_TEXT[activity.state] }}</span>
        <span class="sb-activity-text">{{ activity.text }}</span>
      </span>
    </div>
  </footer>
</template>
