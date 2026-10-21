<script setup lang="ts">
import { onBeforeUnmount, ref } from "vue";
import ExplorerTree from "../catalog/ExplorerTree.vue";
import type { DiagramOpen } from "../diagram/types";
import type { CatalogObjectRef } from "../catalog/object-ref";
import Icon from "./Icon.vue";

const props = defineProps<{ width: number }>();
const emit = defineEmits<{
  resize: [width: number];
  "open-object": [object: CatalogObjectRef, pane: "data" | "structure", section?: "columns" | "drop"];
  "open-definition": [object: CatalogObjectRef];
  "query-object": [object: CatalogObjectRef, sql: string];
  "focused-database": [connectionId: string, database: string];
  "open-diagram": [target: DiagramOpen];
  "new-connection": [];
}>();

function forwardOpen(object: CatalogObjectRef, pane: "data" | "structure", section?: "columns" | "drop") {
  emit("open-object", object, pane, section);
}

function forwardDefinition(object: CatalogObjectRef) {
  emit("open-definition", object);
}

function forwardQuery(object: CatalogObjectRef, sql: string) {
  emit("query-object", object, sql);
}

function forwardFocus(connectionId: string, database: string) {
  emit("focused-database", connectionId, database);
}

function forwardDiagram(target: DiagramOpen) {
  emit("open-diagram", target);
}
const filter = ref("");

let stopResize: (() => void) | null = null;
onBeforeUnmount(() => stopResize?.());
function onPointerDown(event: PointerEvent) {
  event.preventDefault();
  stopResize?.();
  const startX = event.clientX;
  const startWidth = (event.currentTarget as HTMLElement).previousElementSibling?.getBoundingClientRect().width ?? 234;
  const move = (ev: PointerEvent) => {
    emit("resize", Math.min(480, Math.max(200, startWidth + ev.clientX - startX)));
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
    <ExplorerTree :filter="filter" @open-object="forwardOpen" @open-definition="forwardDefinition" @query-object="forwardQuery" @focused-database="forwardFocus" @open-diagram="forwardDiagram" @new-connection="emit('new-connection')" />
  </aside>
  <div
    class="sb-resizer"
    role="separator"
    aria-orientation="vertical"
    aria-label="调整资源管理器宽度"
    title="拖动调整宽度，双击恢复默认"
    tabindex="0"
    :aria-valuenow="width" :aria-valuemin="200" :aria-valuemax="480"
    @keydown.left.prevent="emit('resize', Math.max(200, props.width - 20))"
    @keydown.right.prevent="emit('resize', Math.min(480, props.width + 20))"
    @dblclick="emit('resize', 234)"
    @pointerdown="onPointerDown"
  ></div>
</template>
