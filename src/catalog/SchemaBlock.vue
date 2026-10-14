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
  layers: "M12 3 2 8l10 5 10-5ZM2 13l10 5 10-5",
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
  return `已显示 ${count} / ${Math.max(props.schema.total, props.schema.objects.length)} 个对象`;
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

function emptyLabel(): string {
  if (props.connection.engine === "postgres") return "此 schema 中没有表";
  if (props.connection.engine === "sqlite") return "没有表";
  return "此数据库中没有表";
}
</script>

<template>
  <div v-if="schema.error" class="tree-note err" :style="depthStyle(depth)">
    <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="ICONS.error" /></svg>
    <div>
      <strong>无法读取对象列表</strong>
      <span>{{ schema.error }}</span>
      <button class="link" type="button" data-action="tree-retry" :data-schema="schema.name" @click="refreshCatalog()">重试</button>
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
      :title="[connection.name, database, schema.name, object.name].filter(Boolean).join(' / ')"
    >
      <span class="tw"></span>
      <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="object.kind === 'view' ? ICONS.view : ICONS.table" /></svg>
      <span class="tr-name">{{ object.name }}</span>
      <span v-if="object.stale" class="pill pill-err">已失效</span>
      <span v-else-if="objectMeta(object)" class="tr-meta">{{ objectMeta(object) }}</span>
    </div>
    <div v-if="needsMore() && !schema.error" class="tree-note more" :style="depthStyle(depth)">
      <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="ICONS.layers" /></svg>
      <div>
        <span>{{ shownLabel() }}</span>
        <span class="tree-more-acts">
          <button
            class="link"
            type="button"
            data-action="tree-more"
            :data-key="schemaKey(connection.id, database, schema.name)"
            @click="revealMore(connection.id, database, schema.name)"
          >
            再显示 {{ objectPageSize() }} 个
          </button>
        </span>
      </div>
    </div>
    <div v-if="schemaIsEmpty(schema)" class="tree-note" :style="depthStyle(depth)">
      <span>{{ emptyLabel() }}</span>
    </div>
  </template>
</template>
