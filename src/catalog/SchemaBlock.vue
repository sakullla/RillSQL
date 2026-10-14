<script setup lang="ts">
import { objectPageSize, refreshCatalog, revealMore, schemaKey, shownCount } from "./store";
import { schemaIsEmpty } from "./tree";
import type { CatalogConnection, CatalogObject, SchemaNode } from "./types";

const props = defineProps<{
  connection: CatalogConnection;
  database: string;
  schema: SchemaNode;
  depth: number;
}>();

const ICONS = {
  table: "M4 4h16a1 1 0 0 1 1 1v14a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V5a1 1 0 0 1 1-1ZM3 10h18M10 10v10",
  view: "M2 12s3.6-7 10-7 10 7 10 7-3.6 7-10 7S2 12 2 12ZM9 12a3 3 0 1 0 6 0 3 3 0 1 0-6 0",
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
  return count < props.schema.objects.length || props.schema.objects.length < props.schema.total;
}

function shownLabel(): string {
  const count = Math.min(
    shownCount(schemaKey(props.connection.id, props.database, props.schema.name), props.schema.objects.length),
    props.schema.objects.length,
  );
  return `已显示 ${count} / ${Math.max(props.schema.total, props.schema.objects.length)} 个`;
}

function objectMeta(object: CatalogObject): string {
  if (object.stale) return "";
  if (object.kind === "view") return "视图";
  return knownEstimate(object.estimatedRows);
}

/** 负数是未知估算。PostgreSQL 的 reltuples 在从未分析时为 -1，不是行数。 */
function knownEstimate(value: number | null): string {
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0) return "";
  return value >= 1000 ? `${(value / 1000).toFixed(value >= 10000 ? 0 : 1)}k` : String(value);
}

function objectPath(): string {
  return [props.connection.name, props.database, props.schema.name].filter(Boolean).join(" / ");
}

function objectTitle(object: CatalogObject): string {
  const path = `${objectPath()} / ${object.name}`;
  return object.stale ? `${path} · 已在外部被删除，下次刷新后不再显示` : path;
}

function emptyLabel(): string {
  if (props.connection.engine === "postgres") return "这个 schema 里没有表或视图";
  if (props.connection.engine === "sqlite") return "这个文件里没有表或视图";
  return "这个数据库里没有表或视图";
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
  <template v-if="!schema.error || schema.objects.length">
    <div
      v-for="object in visible()"
      :key="object.name"
      class="tree-row"
      :class="{ dropped: object.stale }"
      role="treeitem"
      data-level="object"
      :data-object="object.name"
      :data-kind="object.kind"
      :data-schema="schema.name"
      :data-database="database"
      :data-stale="object.stale ? 'true' : 'false'"
      :style="depthStyle(depth)"
      :title="objectTitle(object)"
    >
      <span class="tw"></span>
      <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="object.kind === 'view' ? ICONS.view : ICONS.table" /></svg>
      <span class="tr-name">{{ object.name }}</span>
      <span v-if="object.stale" class="badge err">已失效</span>
      <span v-else-if="objectMeta(object)" class="tr-meta">{{ objectMeta(object) }}</span>
    </div>
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
    <div v-if="schemaIsEmpty(schema)" class="tree-note" :style="depthStyle(depth)">
      <span>{{ emptyLabel() }}</span>
    </div>
  </template>
</template>
