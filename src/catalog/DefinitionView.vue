<script setup lang="ts">
import { onMounted, ref } from "vue";
import { fetchDefinition } from "./backend";
import type { CatalogObjectRef } from "./object-ref";
import { KIND_LABEL } from "./tree";
import type { ObjectKind } from "./types";

const props = defineProps<{
  object: Pick<CatalogObjectRef, "connectionId" | "database" | "schema" | "name" | "kind" | "signature" | "relation">;
  label: string;
}>();
const emit = defineEmits<{ "copy-sql": [sql: string] }>();

const sql = ref("");
const error = ref("");
const loading = ref(true);

const ICONS: Record<ObjectKind, string> = {
  table: "M4 4h16a1 1 0 0 1 1 1v14a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V5a1 1 0 0 1 1-1ZM3 10h18M10 10v10",
  view: "M2 12s3.6-7 10-7 10 7 10 7-3.6 7-10 7S2 12 2 12ZM9 12a3 3 0 1 0 6 0 3 3 0 1 0-6 0",
  function: "M8 7h3M8 12h3M8 17h3M14 7l2 5-2 5M4 4h16v16H4z",
  procedure: "M6 7h12M6 12h12M6 17h8M4 4h16v16H4z",
  trigger: "M12 3v4M12 17v4M4.9 6.5l3.5 2M15.6 15.5l3.5 2M4.9 17.5l3.5-2M15.6 8.5l3.5-2",
  sequence: "M4 7h16M4 12h10M4 17h16",
  enum: "M5 5h6v6H5zM13 5h6v6h-6zM5 13h6v6H5zM13 13h6v6h-6z",
  domain: "M4 6h16v12H4zM8 10h8M8 14h5",
};

/** 错误态图标独立于按 ObjectKind 索引的映射，不进 ICONS。 */
const ERROR_ICON = "M2 12a10 10 0 1 0 20 0 10 10 0 1 0-20 0M15 9l-6 6M9 9l6 6";

onMounted(async () => {
  try {
    sql.value = await fetchDefinition(props.object);
  } catch (caught) {
    error.value = caught instanceof Error && caught.message ? caught.message : typeof caught === "string" && caught ? caught : "没有读到定义";
    sql.value = "";
  } finally {
    loading.value = false;
  }
});

function copy() {
  if (!sql.value.trim()) return;
  emit("copy-sql", sql.value);
}
</script>

<template>
  <section class="definition-view" data-definition :data-kind="object.kind">
    <header class="definition-bar">
      <span class="definition-kind" aria-hidden="true">
        <svg class="i" viewBox="0 0 24 24"><path :d="ICONS[object.kind]" /></svg>
      </span>
      <div class="definition-titles">
        <h2>{{ label }}</h2>
        <p>{{ KIND_LABEL[object.kind] }} · 只读定义</p>
      </div>
      <button v-if="sql" class="btn sm" type="button" data-action="copy-definition" @click="copy">复制到新查询</button>
    </header>
    <p v-if="error" class="definition-error" data-definition-error>
      <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path :d="ERROR_ICON" /></svg>
      <span>{{ error }}</span>
    </p>
    <div v-else-if="loading" class="definition-loading" data-definition-loading>
      <span class="definition-spin" aria-hidden="true"></span>
      正在读取定义…
    </div>
    <pre v-else class="definition-sql" data-definition-sql data-readonly="true">{{ sql }}</pre>
  </section>
</template>

<style scoped>
.definition-view { display: flex; flex-direction: column; gap: var(--sp-3); height: 100%; min-height: 0; padding: var(--sp-4); }
.definition-bar { display: flex; align-items: center; gap: var(--sp-3); }
.definition-kind {
  display: grid;
  place-items: center;
  width: 28px;
  height: 28px;
  border-radius: var(--r-sm);
  background: var(--accent-soft);
  color: var(--accent-text);
  box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--accent) 20%, transparent);
  flex-shrink: 0;
}
.definition-kind .i { width: 15px; height: 15px; }
.definition-titles { flex: 1; min-width: 0; }
.definition-bar h2 { margin: 0; font-size: var(--fs-md); font-weight: 600; letter-spacing: -0.01em; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.definition-titles p { margin: 2px 0 0; font-size: var(--fs-xs); color: var(--text-3); }
.definition-error {
  display: flex;
  align-items: flex-start;
  gap: var(--sp-2);
  margin: 0;
  padding: var(--sp-3);
  border-radius: var(--r-md);
  background: var(--err-soft);
  box-shadow: inset 0 0 0 1px var(--err-line);
  color: var(--err);
  font-size: var(--fs-sm);
  line-height: var(--lh);
  white-space: pre-wrap;
}
.definition-error .i { width: 14px; height: 14px; flex-shrink: 0; margin-top: 2px; }
.definition-loading { display: flex; align-items: center; gap: var(--sp-2); color: var(--text-2); font-size: var(--fs-sm); }
.definition-spin { width: 12px; height: 12px; border-radius: var(--r-full); border: 1.5px solid var(--line-2); border-top-color: var(--accent); animation: definition-spin 0.8s linear infinite; }
.definition-sql {
  flex: 1;
  overflow: auto;
  margin: 0;
  padding: var(--sp-4);
  border: 1px solid var(--stroke);
  border-radius: var(--r-md);
  background: var(--surface-sunken);
  color: var(--text);
  font: var(--fs-sm)/1.6 var(--mono);
  white-space: pre;
  box-shadow: inset 0 1px 2px color-mix(in srgb, var(--press) 25%, transparent);
}
@keyframes definition-spin { to { transform: rotate(1turn); } }
</style>
