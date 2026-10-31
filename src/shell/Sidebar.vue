<script setup lang="ts">
import { onBeforeUnmount, ref } from "vue";
import ExplorerTree from "../catalog/ExplorerTree.vue";
import type { DiagramOpen } from "../diagram/types";
import type { CatalogConnection } from "../catalog/types";
import type { CatalogObjectRef } from "../catalog/object-ref";
import { tr as t } from "../i18n";
import Icon from "./Icon.vue";

const props = defineProps<{ width: number }>();
const emit = defineEmits<{
  resize: [width: number];
  "open-object": [object: CatalogObjectRef, pane: "data" | "structure", section?: "columns" | "drop"];
  "open-definition": [object: CatalogObjectRef];
  "query-object": [object: CatalogObjectRef, sql: string];
  "focused-database": [connectionId: string, database: string];
  "open-diagram": [target: DiagramOpen];
  "open-keys": [connection: CatalogConnection, key: string];
  "open-docs": [connection: CatalogConnection, database: string, collection: string];
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

function forwardKeys(connection: CatalogConnection, key: string) {
  emit("open-keys", connection, key);
}

function forwardDocs(connection: CatalogConnection, database: string, collection: string) {
  emit("open-docs", connection, database, collection);
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
  <aside class="sidebar" :style="{ width: `${width}px` }" :aria-label="t('shell.sidebar.explorer')">
    <div class="sb-head">
      <span class="sb-title">{{ t("shell.sidebar.explorer") }}</span>
    </div>
    <label class="sb-search">
      <Icon name="search" />
      <input v-model="filter" type="search" :placeholder="t('shell.sidebar.filterTables')" :aria-label="t('shell.sidebar.filterTables')" />
    </label>
    <ExplorerTree :filter="filter" @open-object="forwardOpen" @open-definition="forwardDefinition" @query-object="forwardQuery" @focused-database="forwardFocus" @open-diagram="forwardDiagram" @open-keys="forwardKeys" @open-docs="forwardDocs" @new-connection="emit('new-connection')" />
  </aside>
  <div
    class="sb-resizer"
    role="separator"
    aria-orientation="vertical"
    :aria-label="t('shell.sidebar.resize')"
    :title="t('shell.sidebar.resizeHint')"
    tabindex="0"
    :aria-valuenow="width" :aria-valuemin="200" :aria-valuemax="480"
    @keydown.left.prevent="emit('resize', Math.max(200, props.width - 20))"
    @keydown.right.prevent="emit('resize', Math.min(480, props.width + 20))"
    @dblclick="emit('resize', 234)"
    @pointerdown="onPointerDown"
  ></div>
</template>
