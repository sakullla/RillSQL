<script setup lang="ts">
import { onMounted, ref } from "vue";
import { fetchDefinition } from "./backend";
import type { CatalogObjectRef } from "./object-ref";

const props = defineProps<{
  object: Pick<CatalogObjectRef, "connectionId" | "database" | "schema" | "name" | "kind" | "signature" | "relation">;
  label: string;
}>();
const emit = defineEmits<{ "copy-sql": [sql: string] }>();

const sql = ref("");
const error = ref("");
const loading = ref(true);

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
      <h2>{{ label }}</h2>
      <button v-if="sql" class="btn sm" type="button" data-action="copy-definition" @click="copy">复制到新查询</button>
    </header>
    <p v-if="error" class="definition-error" data-definition-error>{{ error }}</p>
    <p v-else-if="loading" data-definition-loading>正在读取定义…</p>
    <pre v-else class="definition-sql" data-definition-sql data-readonly="true">{{ sql }}</pre>
  </section>
</template>

<style scoped>
.definition-view { display: flex; flex-direction: column; gap: var(--sp-3); height: 100%; min-height: 0; padding: var(--sp-4); }
.definition-bar { display: flex; align-items: center; gap: var(--sp-3); }
.definition-bar h2 { margin: 0; font-size: var(--fs-md); font-weight: 600; }
.definition-error { margin: 0; color: var(--err); white-space: pre-wrap; }
.definition-sql { flex: 1; overflow: auto; margin: 0; padding: var(--sp-3); border: 1px solid var(--line); border-radius: var(--r); background: var(--sunken); color: var(--text); font: var(--fs-sm) var(--mono); white-space: pre; }
</style>
