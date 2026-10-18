<script setup lang="ts">
import { onBeforeUnmount, ref } from "vue";
import ExplorerTree from "../catalog/ExplorerTree.vue";
import type { CatalogObjectRef } from "../catalog/object-ref";
import Icon from "./Icon.vue";

const props = defineProps<{ width: number }>();
const emit = defineEmits<{
  resize: [width: number];
  "open-object": [object: CatalogObjectRef, pane: "data" | "structure", section?: "columns" | "drop"];
  "query-object": [object: CatalogObjectRef, sql: string];
  "focused-database": [connectionId: string, database: string];
  "connection-action": [connectionId: string, action: "edit" | "test" | "copy" | "delete"];
}>();

function forwardOpen(object: CatalogObjectRef, pane: "data" | "structure", section?: "columns" | "drop") {
  emit("open-object", object, pane, section);
}

function forwardQuery(object: CatalogObjectRef, sql: string) {
  emit("query-object", object, sql);
}

function forwardFocus(connectionId: string, database: string) {
  emit("focused-database", connectionId, database);
}

function forwardConnectionAction(connectionId: string, action: "edit" | "test" | "copy" | "delete") {
  emit("connection-action", connectionId, action);
}
const filter = ref("");

let stopResize: (() => void) | null = null;
onBeforeUnmount(() => stopResize?.());
const MIN_WIDTH = 200;
const MAX_WIDTH = 480;
const DEFAULT_WIDTH = 234;
function onPointerDown(event: PointerEvent) {
  event.preventDefault();
  stopResize?.();
  const startX = event.clientX;
  const startWidth = (event.currentTarget as HTMLElement).previousElementSibling?.getBoundingClientRect().width ?? DEFAULT_WIDTH;
  const move = (ev: PointerEvent) => {
    emit("resize", Math.min(MAX_WIDTH, Math.max(MIN_WIDTH, startWidth + ev.clientX - startX)));
  };
  const stop = () => {
    window.removeEventListener("pointermove", move);
    window.removeEventListener("pointerup", stop);
    window.removeEventListener("pointercancel", stop);
    stopResize = null;
  };
  stopResize = stop;
  window.addEventListener("pointercancel", stop);
  window.addEventListener("pointermove", move);
  window.addEventListener("pointerup", stop);
}
</script>

<template>
  <aside class="sidebar" :style="{ width: `${width}px` }" aria-label="资源管理器">
    <div class="sb-head">
      <span class="sb-title">资源管理器</span>
    </div>
    <label class="sb-search">
      <Icon name="search" />
      <input v-model="filter" type="search" placeholder="筛选表" aria-label="筛选表" />
    </label>
    <ExplorerTree
      :filter="filter"
      @open-object="forwardOpen"
      @query-object="forwardQuery"
      @focused-database="forwardFocus"
      @connection-action="forwardConnectionAction"
    />
  </aside>
  <div
    class="sb-resizer"
    role="separator"
    aria-orientation="vertical"
    aria-label="调整资源管理器宽度"
    tabindex="0"
    :aria-valuenow="width" :aria-valuemin="MIN_WIDTH" :aria-valuemax="MAX_WIDTH"
    @keydown.left.prevent="emit('resize', Math.max(MIN_WIDTH, props.width - 20))"
    @keydown.right.prevent="emit('resize', Math.min(MAX_WIDTH, props.width + 20))"
    @dblclick="emit('resize', DEFAULT_WIDTH)"
    @pointerdown="onPointerDown"
  ></div>
</template>
