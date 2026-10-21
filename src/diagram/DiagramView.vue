<script setup lang="ts">
import { onMounted, ref } from "vue";
import { fetchDiagram } from "./api";
import type { Diagram, DiagramEdge } from "./types";

const props = defineProps<{
  connectionId: string;
  database: string;
  schema: string;
  label: string;
}>();

const diagram = ref<Diagram | null>(null);
const error = ref("");
const loading = ref(true);

onMounted(async () => {
  try {
    diagram.value = await fetchDiagram(props.connectionId, props.database, props.schema);
  } catch (caught) {
    diagram.value = null;
    error.value = caught instanceof Error && caught.message ? caught.message : typeof caught === "string" && caught ? caught : "没有读到关系图";
  } finally {
    loading.value = false;
  }
});

function edgeKey(edge: DiagramEdge): string {
  return [edge.fromTable, edge.fromColumns.join(","), edge.toTable, edge.toColumns.join(","), edge.name].join("\u0001");
}
</script>

<template>
  <section class="diagram-view" data-diagram data-readonly="true">
    <header class="diagram-bar">
      <h2>{{ label }}</h2>
      <p data-diagram-readonly>只读。打开和查看这张图不会发送建表、改表、删表或数据写入。</p>
    </header>
    <p v-if="loading" data-diagram-loading>正在读取关系图…</p>
    <p v-else-if="error" class="diagram-error" data-diagram-error>{{ error }}</p>
    <template v-else-if="diagram">
      <p v-if="diagram.notice" data-diagram-notice>{{ diagram.notice }}</p>
      <ul v-if="diagram.omitted.length" class="diagram-omitted" data-diagram-omitted>
        <li v-for="item in diagram.omitted" :key="item.name" data-omitted :data-omitted-table="item.name">{{ item.message }}</li>
      </ul>
      <div v-if="diagram.tables.length" class="diagram-nodes">
        <article v-for="table in diagram.tables" :key="table.name" class="diagram-table" data-table :data-table-name="table.name">
          <h3>{{ table.name }}</h3>
          <ul>
            <li v-for="(column, index) in table.columns" :key="`${table.name}:${index}:${column}`" data-column>{{ column }}</li>
          </ul>
          <p v-for="(note, index) in table.outside" :key="`${table.name}:outside:${index}`" class="diagram-note" data-outside>{{ note }}</p>
        </article>
      </div>
      <div v-if="diagram.edges.length" class="diagram-links" aria-label="外键">
        <div
          v-for="edge in diagram.edges"
          :key="edgeKey(edge)"
          class="diagram-edge"
          data-edge
          :data-from="edge.fromTable"
          :data-to="edge.toTable"
          :data-from-columns="edge.fromColumns.join(',')"
          :data-to-columns="edge.toColumns.join(',')"
        >
          <span class="diagram-end">{{ edge.fromTable }}.{{ edge.fromColumns.join(", ") }}</span>
          <span class="diagram-line" aria-hidden="true"></span>
          <span class="diagram-end">{{ edge.toTable }}.{{ edge.toColumns.join(", ") }}</span>
        </div>
      </div>
    </template>
  </section>
</template>

<style scoped>
.diagram-view { display: flex; flex-direction: column; gap: var(--sp-3); height: 100%; min-height: 0; padding: var(--sp-4); overflow: auto; }
.diagram-bar h2 { margin: 0; font-size: var(--fs-md); font-weight: 600; }
.diagram-bar p { margin: var(--sp-1) 0 0; color: var(--text-2); }
.diagram-error, .diagram-note { margin: 0; color: var(--err); }
.diagram-omitted { margin: 0; padding-left: 1.2em; color: var(--err); }
.diagram-nodes { display: flex; flex-wrap: wrap; gap: var(--sp-3); align-items: flex-start; }
.diagram-table { width: 220px; border: 1px solid var(--line); border-radius: var(--r); background: var(--sunken); }
.diagram-table h3 { margin: 0; padding: var(--sp-2) var(--sp-3); border-bottom: 1px solid var(--line); font-size: var(--fs-sm); }
.diagram-table ul { list-style: none; margin: 0; padding: var(--sp-1) 0 var(--sp-2); }
.diagram-table li { padding: 2px var(--sp-3); font: var(--fs-xs) var(--mono); }
.diagram-table .diagram-note { padding: 0 var(--sp-3) var(--sp-2); font-size: var(--fs-xs); }
.diagram-links { display: flex; flex-direction: column; gap: var(--sp-2); }
.diagram-edge { display: grid; grid-template-columns: minmax(0, 1fr) 40px minmax(0, 1fr); gap: var(--sp-2); align-items: center; font-size: var(--fs-sm); }
.diagram-end { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-family: var(--mono); }
.diagram-line { height: 2px; background: var(--accent); position: relative; }
.diagram-line::after { content: ""; position: absolute; right: -1px; top: -4px; border-width: 5px 0 5px 6px; border-style: solid; border-color: transparent transparent transparent var(--accent); }
</style>
