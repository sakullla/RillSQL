<script setup lang="ts">
import { computed } from "vue";
import { catalogConnections } from "../catalog/store";
import { resolveTableTarget } from "../grid/target";
import { PRODUCT_TITLE } from "./product";
import Icon from "./Icon.vue";
import type { ShellTab } from "./session";
import { CONNECTION_STATUS_TEXT, OPERATION_STATE_TEXT, type OperationActivity, type OperationState } from "./status-text";

const props = defineProps<{
  tab: ShellTab | null;
  onWorkspace: boolean;
  activity?: OperationActivity | null;
  /** 正在执行的查询标签；有值时状态栏提供取消入口，不必先找回原标签。 */
  runningTabId?: string | null;
  cancelRequested?: boolean;
}>();
const emit = defineEmits<{ cancel: [] }>();

const tableEnvironment = computed(() => {
  const tab = props.tab;
  if (!props.onWorkspace || tab?.kind !== "table") return null;
  const resolved = resolveTableTarget(tab.id, catalogConnections());
  if (!resolved) return null;
  const connection = catalogConnections().find((item) => item.id === resolved.connectionId);
  const label = connection?.envLabel.trim() ?? "";
  if (!connection || !label) return null;
  return { label, tone: connection.envTone };
});

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
          <span v-if="tableEnvironment" class="env xs" :class="tableEnvironment.tone !== 'none' ? `env-${tableEnvironment.tone}` : undefined">{{ tableEnvironment.label }}</span>
        </span>
      </template>
      <span v-else class="sb-item muted">{{ PRODUCT_TITLE }}</span>
    </div>
    <div class="sb-right">
      <span v-if="activity" class="sb-activity" :data-state="activity.state">
        <span class="badge" :class="ACTIVITY_TONE[activity.state]">{{ OPERATION_STATE_TEXT[activity.state] }}</span>
        <span class="sb-activity-text">{{ activity.text }}</span>
      </span>
      <button
        v-if="runningTabId"
        class="btn sm sb-cancel"
        type="button"
        data-action="status-cancel"
        :disabled="cancelRequested"
        :title="cancelRequested ? '已经请求取消，等待数据库确认' : '取消正在执行的查询'"
        @click="emit('cancel')"
      >{{ cancelRequested ? "正在取消…" : "取消查询" }}</button>
    </div>
  </footer>
</template>
