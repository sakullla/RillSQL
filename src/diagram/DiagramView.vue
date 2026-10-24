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
    <p v-if="loading" class="diagram-loading" data-diagram-loading>正在读取关系图…</p>
    <p v-else-if="error" class="diagram-error" data-diagram-error>{{ error }}</p>
    <template v-else-if="diagram">
      <p v-if="diagram.notice" class="diagram-notice" data-diagram-notice>{{ diagram.notice }}</p>
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
/* 只读关系图：标题条 + 下沉视口（基表卡片画布）+ 外键连线清单，全部令牌取值。 */
.diagram-view { display: flex; flex-direction: column; gap: var(--sp-3); height: 100%; min-height: 0; padding: var(--sp-4); overflow: auto; }
.diagram-bar { display: flex; align-items: baseline; gap: var(--sp-3); flex-wrap: wrap; }
.diagram-bar h2 { margin: 0; font-size: var(--fs-lg); font-weight: 650; letter-spacing: -0.01em; }
.diagram-bar p { margin: 0; font-size: var(--fs-xs); color: var(--text-3); }

/* 状态条目：加载 / 错误 / 提示均为软底卡 */
.diagram-loading { margin: 0; padding: var(--sp-2) var(--sp-3); border-radius: var(--r-md); background: var(--info-soft); color: var(--info); font-size: var(--fs-sm); }
.diagram-error { margin: 0; padding: var(--sp-2) var(--sp-3); border: 1px solid var(--err-line); border-radius: var(--r-md); background: var(--err-soft); color: var(--err); font-size: var(--fs-sm); line-height: var(--lh); }
.diagram-notice { margin: 0; padding: var(--sp-2) var(--sp-3); border-radius: var(--r-md); background: var(--surface-sunken); box-shadow: inset 0 0 0 1px var(--stroke); color: var(--text-2); font-size: var(--fs-sm); }
.diagram-omitted { margin: 0; padding: 0; list-style: none; display: flex; flex-direction: column; gap: var(--sp-1); }
.diagram-omitted li { padding: var(--sp-1) var(--sp-2); border-radius: var(--r-sm); background: var(--err-soft); box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--err) 22%, transparent); color: var(--err); font-size: var(--fs-xs); line-height: var(--lh); }

/* 视口：下沉井画布，基表卡片为 panel 表面实体 */
.diagram-nodes { display: flex; flex-wrap: wrap; gap: var(--sp-3); align-items: flex-start; padding: var(--sp-3); border: 1px solid var(--stroke); border-radius: var(--r-lg); background: var(--surface-sunken); box-shadow: inset 0 1px 2px color-mix(in srgb, var(--press) 30%, transparent); }
.diagram-table { width: 220px; border: 1px solid var(--stroke); border-radius: var(--r-md); background: var(--surface-panel); box-shadow: var(--shadow-sm); overflow: hidden; }
.diagram-table h3 { margin: 0; padding: var(--sp-2) var(--sp-3); border-bottom: 1px solid var(--line); background: var(--panel-2); font: 600 var(--fs-sm) var(--mono); letter-spacing: -0.01em; color: var(--text); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.diagram-table ul { list-style: none; margin: 0; padding: var(--sp-1) 0 var(--sp-2); }
.diagram-table li { padding: 2px var(--sp-3); font: var(--fs-xs) var(--mono); color: var(--text-2); }
.diagram-table .diagram-note { padding: var(--sp-1) var(--sp-3) var(--sp-2); font-size: var(--fs-xs); color: var(--err); }

/* 外键连线：两端是 mono 端点芯片，中间强调色连接线带箭头 */
.diagram-links { display: flex; flex-direction: column; gap: var(--sp-1); padding-top: var(--sp-3); border-top: 1px solid var(--line); }
.diagram-edge { display: grid; grid-template-columns: minmax(0, 1fr) 44px minmax(0, 1fr); gap: var(--sp-2); align-items: center; font-size: var(--fs-xs); }
.diagram-end { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-family: var(--mono); color: var(--text-2); padding: 2px var(--sp-2); border-radius: var(--r-sm); background: var(--surface-sunken); box-shadow: inset 0 0 0 1px var(--stroke); }
.diagram-line { height: 2px; border-radius: var(--r-full); background: linear-gradient(90deg, color-mix(in srgb, var(--accent) 30%, transparent), var(--accent)); position: relative; }
.diagram-line::after { content: ""; position: absolute; right: -1px; top: -4px; border-width: 5px 0 5px 6px; border-style: solid; border-color: transparent transparent transparent var(--accent); }
</style>
