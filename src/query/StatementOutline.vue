<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { tr as t } from "../i18n";
import Icon from "../shell/Icon.vue";
import ContextMenu from "../ui/ContextMenu.vue";
import type { MenuItem } from "../ui/types";
import type { OutlineStatement } from "./outline";

const props = defineProps<{
  statements: OutlineStatement[];
  activeIndex: number;
}>();
const emit = defineEmits<{
  jump: [statement: OutlineStatement];
  run: [statement: OutlineStatement];
  copy: [statement: OutlineStatement];
}>();

const listEl = ref<HTMLElement | null>(null);
/* 右键菜单锚点与目标语句；null 表示关闭。 */
const outlineMenu = ref<{ x: number; y: number; statement: OutlineStatement } | null>(null);

/* 光标移动/滚动时让当前语句保持可见，但不强制滚到列表中间。 */
watch(
  () => props.activeIndex,
  (index) => {
    const root = listEl.value;
    if (!root || index < 0) return;
    const item = root.querySelectorAll(".outline-item")[index];
    if (item instanceof HTMLElement && typeof item.scrollIntoView === "function") item.scrollIntoView({ block: "nearest" });
  },
);

const outlineMenuItems = computed<MenuItem[]>(() => [
  { kind: "item", id: "run", label: t("query.outline.menu.runStatement") },
  { kind: "item", id: "copy", label: t("query.outline.menu.copyStatement") },
  { kind: "sep" },
  { kind: "item", id: "jump", label: t("query.outline.menu.jump") },
]);

function openMenu(statement: OutlineStatement, event: MouseEvent): void {
  event.preventDefault();
  outlineMenu.value = { x: event.clientX, y: event.clientY, statement };
}

function onMenuSelect(id: string): void {
  const target = outlineMenu.value?.statement;
  outlineMenu.value = null;
  if (!target) return;
  if (id === "run") emit("run", target);
  else if (id === "copy") emit("copy", target);
  else emit("jump", target);
}

function statusLabel(status: OutlineStatement["status"]): string {
  const labels: Record<OutlineStatement["status"], string> = {
    none: "",
    pending: t("query.status.running"),
    running: t("query.status.running"),
    ok: t("query.status.ok"),
    error: t("query.status.error"),
    skipped: t("query.status.skipped"),
    cancelled: t("query.status.cancelled"),
    unknown: t("query.status.unknown"),
  };
  return labels[status];
}
</script>

<template>
  <aside class="stmt-outline" :aria-label="t('query.outline.heading')">
    <header class="outline-head">
      <span class="outline-title">{{ t("query.outline.heading") }}</span>
      <span class="outline-count">{{ t("query.outline.count", { n: statements.length }) }}</span>
    </header>
    <div v-if="!statements.length" class="outline-empty">{{ t("query.outline.empty") }}</div>
    <ol v-else ref="listEl" class="outline-list">
      <li v-for="(statement, index) in statements" :key="`${statement.start}-${index}`">
        <button
          class="outline-item"
          :class="[`is-${statement.status}`]"
          :data-outline-index="index"
          :aria-current="index === activeIndex ? 'true' : undefined"
          :aria-label="t('query.outline.itemTitle', { n: statement.index, line: statement.line })"
          :title="statement.preview"
          type="button"
          @click="emit('jump', statement)"
          @contextmenu.prevent="openMenu(statement, $event)"
        >
          <span class="outline-status" :data-status="statement.status" :title="statusLabel(statement.status)">
            <Icon v-if="statement.status === 'ok'" name="check" class="i" />
            <Icon v-else-if="statement.status === 'error'" name="x" class="i" />
            <span v-else-if="statement.status === 'skipped'" class="outline-dash" aria-hidden="true">–</span>
            <Icon v-else-if="statement.status === 'cancelled' || statement.status === 'unknown'" name="alert" class="i" />
            <span v-else-if="statement.status === 'pending' || statement.status === 'running'" class="outline-spin" aria-hidden="true"></span>
            <span v-else class="outline-no">{{ statement.index }}</span>
          </span>
          <span class="outline-preview">{{ statement.preview }}</span>
          <span class="outline-line">Ln {{ statement.line }}</span>
        </button>
      </li>
    </ol>
    <ContextMenu
      :open="outlineMenu !== null"
      :x="outlineMenu?.x ?? 0"
      :y="outlineMenu?.y ?? 0"
      :items="outlineMenuItems"
      :label="t('query.outline.heading')"
      @close="outlineMenu = null"
      @select="onMenuSelect"
    />
  </aside>
</template>

<style scoped>
.stmt-outline {
  display: flex;
  flex-direction: column;
  width: 232px;
  flex-shrink: 0;
  min-height: 0;
  border-left: 1px solid var(--line);
  background: var(--surface-chrome);
}
.outline-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--sp-2);
  padding: var(--sp-1) var(--sp-2);
  border-bottom: 1px solid var(--line);
  flex-shrink: 0;
}
.outline-title { font-size: var(--fs-xs); font-weight: 600; color: var(--text-2); letter-spacing: 0.04em; }
.outline-count { font-size: var(--fs-xs); color: var(--text-3); font-variant-numeric: tabular-nums; }
.outline-empty { padding: var(--sp-3); font-size: var(--fs-xs); line-height: var(--lh); color: var(--text-3); }
.outline-list {
  list-style: none;
  margin: 0;
  padding: var(--sp-1);
  overflow: auto;
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
  gap: 1px;
}
.outline-item {
  display: flex;
  align-items: center;
  gap: var(--sp-2);
  width: 100%;
  min-height: 28px;
  padding: 3px var(--sp-2);
  border-radius: var(--r-sm);
  text-align: left;
  font-size: var(--fs-xs);
  color: var(--text-2);
  transition: background var(--dur-fast) var(--ease), color var(--dur-fast) var(--ease);
}
.outline-item:hover { background: var(--hover); color: var(--text); }
.outline-item:active { box-shadow: inset 0 1px 2px var(--press); }
.outline-item[aria-current="true"] { background: var(--sel); color: var(--text); box-shadow: inset 2px 0 0 var(--accent); }
.outline-status {
  display: inline-grid;
  place-items: center;
  width: 16px;
  height: 16px;
  flex-shrink: 0;
  border-radius: var(--r-full);
  font-size: var(--fs-2xs);
  font-weight: 600;
  color: var(--text-3);
  font-variant-numeric: tabular-nums;
}
.outline-status .i { width: 13px; height: 13px; }
.outline-status[data-status="ok"] { color: var(--ok); }
.outline-status[data-status="error"] { color: var(--err); }
.outline-status[data-status="skipped"] { color: var(--text-3); }
.outline-status[data-status="cancelled"], .outline-status[data-status="unknown"] { color: var(--warn); }
.outline-spin {
  width: 10px;
  height: 10px;
  border-radius: var(--r-full);
  border: 1.5px solid var(--accent);
  border-top-color: transparent;
  animation: outline-rot 0.8s linear infinite;
}
@keyframes outline-rot { to { transform: rotate(360deg); } }
.outline-preview {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  white-space: nowrap;
  text-overflow: ellipsis;
  font-family: var(--mono);
  font-size: var(--fs-xs);
}
.outline-line {
  flex-shrink: 0;
  font-size: var(--fs-2xs);
  color: var(--text-3);
  font-variant-numeric: tabular-nums;
}
</style>
