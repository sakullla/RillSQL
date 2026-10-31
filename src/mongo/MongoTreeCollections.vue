<script setup lang="ts">
import { tr as t } from "../i18n";
import type { CatalogConnection } from "../catalog/types";

const props = defineProps<{ connection: CatalogConnection }>();
const emit = defineEmits<{ "open-collection": [connection: CatalogConnection, database: string, collection: string] }>();

/** 目录形状复用 MySQL 先例（databases:true / schemas:false）：库节点来自 connection.databases，
 * 集合是 schema("") 里的 table 对象；行估算就是 estimatedDocumentCount。双击集合开文档标签。 */
function collections(database: string): { name: string; estimated: number | null }[] {
  const node = props.connection.databases.find((item) => item.name === database);
  const schema = node?.schemas.find((item) => item.name === "");
  if (!schema) return [];
  return schema.objects
    .filter((object) => object.kind === "table" && !object.stale)
    .map((object) => ({ name: object.name, estimated: object.estimatedRows }));
}

function estimateText(estimated: number | null): string {
  if (typeof estimated !== "number" || !Number.isFinite(estimated) || estimated < 0) return "";
  return estimated >= 1000 ? `${(estimated / 1000).toFixed(estimated >= 10000 ? 0 : 1)}k` : String(estimated);
}

function openCollection(database: string, collection: string) {
  emit("open-collection", props.connection, database, collection);
}
</script>

<!-- 树里的 MongoDB 集合（R12）：库节点 → 集合行（estimatedDocumentCount 计数）；双击集合开文档标签。
     不提供 SQL/结构编辑/关系图入口（document 引擎能力门控）。 -->
<template>
  <div class="mtc" :data-connection="connection.id" data-mongo-tree>
    <template v-for="database in connection.databases" :key="database.name">
      <div class="tree-row mtc-db" role="treeitem" tabindex="0" data-level="database" :data-database="database.name" :style="{ '--d': 1 }" :title="`${connection.name} / ${database.name}`">
        <span class="tw"></span>
        <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path d="M4 6c0-1.7 3.6-3 8-3s8 1.3 8 3-3.6 3-8 3-8-1.3-8-3ZM4 6v12c0 1.7 3.6 3 8 3s8-1.3 8-3V6M4 12c0 1.7 3.6 3 8 3s8-1.3 8-3" /></svg>
        <span class="tr-name">{{ database.name }}</span>
      </div>
      <div v-if="database.error" class="tree-note err" :style="{ '--d': 2 }" :data-database="database.name" data-database-error>
        <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path d="M2 12a10 10 0 1 0 20 0 10 10 0 1 0-20 0M15 9l-6 6M9 9l6 6" /></svg>
        <div><span class="reason">{{ database.error }}</span></div>
      </div>
      <template v-else>
        <button
          v-for="collection in collections(database.name)"
          :key="`${database.name}:${collection.name}`"
          class="tree-row"
          type="button"
          data-level="collection"
          :data-collection="collection.name"
          :data-database="database.name"
          :style="{ '--d': 2 }"
          :title="`${database.name} / ${collection.name}`"
          @dblclick="openCollection(database.name, collection.name)"
        >
          <span class="tw"></span>
          <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path d="M8 6c-2 0-2 3-2 3s0 3-2 3c2 0 2 3 2 3s0 3 2 3M16 6c2 0 2 3 2 3s0 3 2 3c-2 0-2 3-2 3s0 3 2 3" /></svg>
          <span class="tr-name mono">{{ collection.name }}</span>
          <span class="tr-meta">{{ estimateText(collection.estimated) }}</span>
        </button>
        <div v-if="collections(database.name).length === 0" class="tree-note" :style="{ '--d': 2 }" :data-database="database.name" data-collection-empty>
          <span>{{ t("mongo.tab.noCollections") }}</span>
        </div>
      </template>
    </template>
  </div>
</template>

<style scoped>
.mtc { display: contents; }
.mtc-db { cursor: default; }
.mono { font-family: var(--mono, ui-monospace, monospace); }
</style>
