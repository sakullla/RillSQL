<script setup lang="ts">
import { ref } from "vue";
import { objectPageSize, refreshCatalog, revealMore, schemaKey, shownCount } from "./store";
import { KIND_LABEL, objectGroups, objectKey, objectLabel, serverSliceCount, successfulTotal, visibleTotal } from "./tree";
import type { CatalogConnection, CatalogObject, ObjectKind, SchemaNode } from "./types";

const props = defineProps<{
  connection: CatalogConnection;
  database: string;
  schema: SchemaNode;
  depth: number;
  selectedKey?: string;
}>();

const openGroups = ref<Record<string, boolean>>({});

const ICONS = {
  table: "M4 4h16a1 1 0 0 1 1 1v14a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V5a1 1 0 0 1 1-1ZM3 10h18M10 10v10",
  view: "M2 12s3.6-7 10-7 10 7 10 7-3.6 7-10 7S2 12 2 12ZM9 12a3 3 0 1 0 6 0 3 3 0 1 0-6 0",
  function: "M8 7h3M8 12h3M8 17h3M14 7l2 5-2 5M4 4h16v16H4z",
  procedure: "M6 7h12M6 12h12M6 17h8M4 4h16v16H4z",
  trigger: "M12 3v4M12 17v4M4.9 6.5l3.5 2M15.6 15.5l3.5 2M4.9 17.5l3.5-2M15.6 8.5l3.5-2",
  right: "m9 6 6 6-6 6",
  down: "m6 9 6 6 6-6",
  error: "M2 12a10 10 0 1 0 20 0 10 10 0 1 0-20 0M15 9l-6 6M9 9l6 6",
} as const;

function depthStyle(depth: number) {
  return { "--d": String(depth) };
}

function visible(): CatalogObject[] {
  const count = shownCount(schemaKey(props.connection.id, props.database, props.schema.name), props.schema.objects.length);
  return props.schema.objects.slice(0, count);
}

function needsMore(): boolean {
  const count = shownCount(schemaKey(props.connection.id, props.database, props.schema.name), props.schema.objects.length);
  return count < props.schema.objects.length || serverSliceCount(props.schema) < successfulTotal(props.schema);
}

function shownLabel(): string {
  const count = Math.min(
    shownCount(schemaKey(props.connection.id, props.database, props.schema.name), props.schema.objects.length),
    props.schema.objects.length,
  );
  return `已显示 ${count} / ${Math.max(visibleTotal(props.schema), props.schema.objects.length)} 个`;
}

function objectMeta(object: CatalogObject): string {
  if (object.stale) return "";
  if (object.kind === "view") return "视图";
  if (object.kind === "function") return "函数";
  if (object.kind === "procedure") return "存储过程";
  if (object.kind === "trigger") return object.relation ? `触发器 · ${object.relation}` : "触发器";
  return knownEstimate(object.estimatedRows);
}

function knownEstimate(value: number | null): string {
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0) return "";
  return value >= 1000 ? `${(value / 1000).toFixed(value >= 10000 ? 0 : 1)}k` : String(value);
}

function objectPath(): string {
  return [props.connection.name, props.database, props.schema.name].filter(Boolean).join(" / ");
}

function objectTitle(object: CatalogObject): string {
  const path = `${objectPath()} / ${objectLabel(props.connection.engine, object)}`;
  return object.stale ? `${path} · 已在外部被删除，下次刷新后不再显示` : path;
}

function groupOpen(kind: ObjectKind): boolean {
  return openGroups.value[kind] !== false;
}

function toggleGroup(kind: ObjectKind) {
  openGroups.value = { ...openGroups.value, [kind]: !groupOpen(kind) };
}

function objectsOf(kind: ObjectKind): CatalogObject[] {
  return visible().filter((object) => object.kind === kind);
}

function reportOf(kind: ObjectKind) {
  return props.schema.kindReports?.find((item) => item.kind === kind) ?? null;
}

function groupTotal(kind: ObjectKind, loaded: number): number {
  const report = reportOf(kind);
  if (report && !report.error) return report.total;
  return loaded;
}

function showGroups(): boolean {
  return !props.schema.error || props.schema.objects.length > 0;
}
</script>

<template>
  <div v-if="schema.error" class="tree-note err" :style="depthStyle(depth)" data-schema-error>
    <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="ICONS.error" /></svg>
    <div>
      <strong>这部分对象列表没读出来</strong>
      <span class="reason">{{ schema.error }}</span>
      <span class="tree-note-acts">
        <button class="btn sm" type="button" data-action="tree-retry" :data-schema="schema.name" @click="refreshCatalog()">重试</button>
      </span>
    </div>
  </div>
  <template v-if="showGroups()">
    <template v-for="kind in objectGroups(connection.engine)" :key="kind">
      <div
        class="tree-row tr-group"
        role="treeitem"
        tabindex="0"
        data-level="group"
        :data-kind-group="kind"
        :data-schema="schema.name"
        :data-database="database"
        :style="depthStyle(depth)"
        :aria-expanded="groupOpen(kind)"
        @click.stop="toggleGroup(kind)"
        @dblclick.stop
      >
        <span class="tw">
          <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="groupOpen(kind) ? ICONS.down : ICONS.right" /></svg>
        </span>
        <span class="tr-name">{{ KIND_LABEL[kind] }}</span>
        <span v-if="!reportOf(kind)?.error" class="tr-meta">{{ groupTotal(kind, objectsOf(kind).length) }}</span>
      </div>
      <template v-if="groupOpen(kind)">
        <div v-if="reportOf(kind)?.error" class="tree-note err" :style="depthStyle(depth + 1)" data-kind-error :data-kind-group="kind">
          <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="ICONS.error" /></svg>
          <div>
            <strong>这类对象没读出来</strong>
            <span class="reason">{{ reportOf(kind)?.error }}</span>
          </div>
        </div>
        <div
          v-else-if="schema.pending && !schema.kindReports && objectsOf(kind).length === 0"
          class="tree-note"
          :style="depthStyle(depth + 1)"
          data-kind-pending
          :data-kind-group="kind"
        >
          <span>正在读取…</span>
        </div>
        <div
          v-else-if="objectsOf(kind).length === 0 && groupTotal(kind, 0) === 0"
          class="tree-note"
          :style="depthStyle(depth + 1)"
          data-kind-empty
          :data-kind-group="kind"
        >
          <span>没有{{ KIND_LABEL[kind] }}</span>
        </div>
        <div
          v-for="object in objectsOf(kind)"
          :key="objectKey(object)"
          class="tree-row"
          :class="{ dropped: object.stale, active: selectedKey === objectKey(object) }"
          role="treeitem"
          tabindex="0"
          data-level="object"
          :data-object="object.name"
          :data-key="objectKey(object)"
          :data-kind="object.kind"
          :data-schema="schema.name"
          :data-database="database"
          :data-selected="selectedKey === objectKey(object) ? 'true' : 'false'"
          :data-stale="object.stale ? 'true' : 'false'"
          :style="depthStyle(depth + 1)"
          :title="objectTitle(object)"
        >
          <span class="tw"></span>
          <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="ICONS[object.kind]" /></svg>
          <span class="tr-name">{{ objectLabel(connection.engine, object) }}</span>
          <span v-if="object.stale" class="badge err">已失效</span>
          <span v-else-if="objectMeta(object)" class="tr-meta">{{ objectMeta(object) }}</span>
        </div>
      </template>
    </template>
    <div v-if="needsMore() && !schema.error" class="tree-note more" :style="depthStyle(depth)" data-more>
      <span>{{ shownLabel() }}</span>
      <button
        class="btn ghost sm"
        type="button"
        data-action="tree-more"
        :data-key="schemaKey(connection.id, database, schema.name)"
        @click="revealMore(connection.id, database, schema.name)"
      >
        再显示 {{ objectPageSize() }} 个
      </button>
    </div>
  </template>
</template>
