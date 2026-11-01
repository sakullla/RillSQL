<script setup lang="ts">
import { tr } from "../i18n";
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { fetchDiagram } from "./api";
import { edgePath, layoutColumns, type Point } from "./layout";
import type { Diagram, DiagramEdge, DiagramTable } from "./types";

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
    error.value = caught instanceof Error && caught.message ? caught.message : typeof caught === "string" && caught ? caught : tr("diagram.failed");
  } finally {
    loading.value = false;
  }
});

const canvas = ref<HTMLElement | null>(null);
const hovered = ref<string | null>(null);
const paths = ref<{ key: string; edge: DiagramEdge; d: string }[]>([]);
const size = ref({ width: 0, height: 0 });
const columns = computed<DiagramTable[][]>(() => (diagram.value ? layoutColumns(diagram.value.tables, diagram.value.edges) : []));
const foreignColumns = computed(() => new Set((diagram.value?.edges ?? []).flatMap((edge) => edge.fromColumns.map((column) => `${edge.fromTable}.${column}`))));

function anchor(table: string, column: string | undefined): DOMRect | null {
  const card = canvas.value?.querySelector<HTMLElement>(`[data-table-name="${CSS.escape(table)}"]`);
  if (!card) return null;
  const row = column ? card.querySelector<HTMLElement>(`[data-column-name="${CSS.escape(column)}"]`) : null;
  const box = card.getBoundingClientRect();
  const rowBox = (row ?? card.querySelector<HTMLElement>("h3"))?.getBoundingClientRect();
  return new DOMRect(box.left, rowBox?.top ?? box.top, box.width, rowBox?.height ?? box.height);
}

/** 连线画在卡片之上的 SVG 里，坐标以画布左上角为原点（含滚动偏移）。 */
function measure() {
  const host = canvas.value;
  if (!host || !diagram.value) return;
  const origin = host.getBoundingClientRect();
  const local = (x: number, y: number): Point => ({ x: x - origin.left + host.scrollLeft, y: y - origin.top + host.scrollTop });
  const next: { key: string; edge: DiagramEdge; d: string }[] = [];
  for (const edge of diagram.value.edges) {
    const from = anchor(edge.fromTable, edge.fromColumns[0]);
    const to = anchor(edge.toTable, edge.toColumns[0]);
    if (!from || !to) continue;
    const leftward = to.left + to.width / 2 <= from.left + from.width / 2;
    const start = local(leftward ? from.left : from.right, from.top + from.height / 2);
    const end = local(leftward ? to.right : to.left, to.top + to.height / 2);
    next.push({ key: edgeKey(edge), edge, d: edgePath(start, end) });
  }
  paths.value = next;
  size.value = { width: host.scrollWidth, height: host.scrollHeight };
}

function edgeHot(edge: DiagramEdge): boolean {
  return hovered.value === edge.fromTable || hovered.value === edge.toTable;
}

function tableLinked(name: string): boolean {
  if (!hovered.value || hovered.value === name) return true;
  return (diagram.value?.edges ?? []).some((edge) => (edge.fromTable === hovered.value && edge.toTable === name) || (edge.toTable === hovered.value && edge.fromTable === name));
}

let observer: ResizeObserver | null = null;
watch(canvas, (host) => {
  observer?.disconnect();
  if (!host) return;
  if (typeof ResizeObserver !== "undefined") {
    observer = new ResizeObserver(() => measure());
    observer.observe(host);
  }
  void nextTick(measure);
});
onBeforeUnmount(() => observer?.disconnect());

function edgeKey(edge: DiagramEdge): string {
  return [edge.fromTable, edge.fromColumns.join(","), edge.toTable, edge.toColumns.join(","), edge.name].join("\u0001");
}
</script>

<template>
  <section class="diagram-view" data-diagram data-readonly="true">
    <header class="diagram-bar">
      <h2>{{ label }}</h2>
      <p data-diagram-readonly>{{ tr("diagram.readonly") }}</p>
    </header>
    <p v-if="loading" class="diagram-loading" data-diagram-loading>{{ tr("diagram.loading") }}</p>
    <p v-else-if="error" class="diagram-error" data-diagram-error>{{ error }}</p>
    <template v-else-if="diagram">
      <p v-if="diagram.notice" class="diagram-notice" data-diagram-notice>{{ diagram.notice }}</p>
      <ul v-if="diagram.omitted.length" class="diagram-omitted" data-diagram-omitted>
        <li v-for="item in diagram.omitted" :key="item.name" data-omitted :data-omitted-table="item.name">{{ item.message }}</li>
      </ul>
      <div v-if="diagram.tables.length" ref="canvas" class="diagram-nodes">
        <div v-for="(group, depth) in columns" :key="depth" class="diagram-column">
          <article
            v-for="table in group"
            :key="table.name"
            class="diagram-table"
            :class="{ dim: !tableLinked(table.name), hot: hovered === table.name }"
            data-table
            :data-table-name="table.name"
            @mouseenter="hovered = table.name"
            @mouseleave="hovered = null"
          >
            <h3>{{ table.name }}</h3>
            <ul>
              <li v-for="(column, index) in table.columns" :key="`${table.name}:${index}:${column}`" data-column :data-column-name="column" :class="{ fk: foreignColumns.has(`${table.name}.${column}`) }">{{ column }}</li>
            </ul>
            <p v-for="(note, index) in table.outside" :key="`${table.name}:outside:${index}`" class="diagram-note" data-outside>{{ note }}</p>
          </article>
        </div>
        <svg class="diagram-wires" :width="size.width" :height="size.height" aria-hidden="true">
          <path v-for="item in paths" :key="item.key" :d="item.d" class="diagram-wire" :class="{ hot: edgeHot(item.edge), dim: hovered && !edgeHot(item.edge) }" />
        </svg>
      </div>
      <details v-if="diagram.edges.length" class="disclosure diagram-list">
        <summary>{{ tr("diagram.foreignKeys") }} <span class="muted">{{ diagram.edges.length }}</span></summary>
      <div class="diagram-links" :aria-label="tr('diagram.foreignKeys')">
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
      </details>
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
.diagram-omitted li { padding: var(--sp-1) var(--sp-2); border-radius: var(--r-sm); background: var(--surface-sunken); box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--stroke) 22%, transparent); color: var(--text-2); font-size: var(--fs-xs); line-height: var(--lh); }

/* 画布：点阵底 + 按依赖分列的表卡片，被引用的表在左 */
.diagram-nodes {
  position: relative;
  flex: 1 1 auto;
  min-height: 240px;
  display: flex;
  gap: 88px;
  align-items: flex-start;
  padding: var(--sp-6);
  overflow: auto;
  border-radius: var(--r-lg);
  background-color: var(--surface-sunken);
  background-image: radial-gradient(color-mix(in srgb, var(--text-3) 26%, transparent) 1px, transparent 1px);
  background-size: 18px 18px;
}
.diagram-column { display: flex; flex-direction: column; gap: var(--sp-5); flex-shrink: 0; }
.diagram-table { position: relative; z-index: 2; width: 220px; border-radius: var(--r-md); background: var(--surface-panel); box-shadow: var(--shadow-sm), 0 0 0 1px var(--stroke); overflow: hidden; transition: box-shadow var(--dur-fast) var(--ease), opacity var(--dur-fast) var(--ease); }
.diagram-table.hot { box-shadow: var(--shadow), 0 0 0 1.5px var(--accent); }
.diagram-table.dim { opacity: .45; }
.diagram-table h3 { margin: 0; padding: var(--sp-2) var(--sp-3); border-bottom: 1px solid var(--line); background: var(--panel-2); font: 600 var(--fs-sm) var(--mono); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.diagram-table ul { list-style: none; margin: 0; padding: var(--sp-1) 0; }
.diagram-table li { padding: 2px var(--sp-3); font: var(--fs-xs) var(--mono); color: var(--text-2); }
.diagram-table li.fk { color: var(--accent-text); font-weight: 600; }
.diagram-table .diagram-note { padding: var(--sp-1) var(--sp-3) var(--sp-2); margin: 0; font-size: var(--fs-xs); color: var(--text-3); }
.diagram-wires { position: absolute; left: 0; top: 0; z-index: 1; pointer-events: none; overflow: visible; }
.diagram-wire { fill: none; stroke: color-mix(in srgb, var(--accent) 62%, var(--text-3)); stroke-width: 1.5; transition: stroke-opacity var(--dur-fast) var(--ease), stroke-width var(--dur-fast) var(--ease); }
.diagram-wire.hot { stroke: var(--accent); stroke-width: 2.25; }
.diagram-wire.dim { stroke-opacity: .2; }
.diagram-list.disclosure { flex-shrink: 0; }
.diagram-list > summary { padding: var(--sp-2) var(--sp-3); font-size: var(--fs-sm); }

/* 外键连线：两端是 mono 端点芯片，中间强调色连接线带箭头 */
.diagram-links { display: flex; flex-direction: column; gap: var(--sp-1); padding: var(--sp-1) var(--sp-3) var(--sp-3); }
.diagram-edge { display: grid; grid-template-columns: minmax(0, 1fr) 44px minmax(0, 1fr); gap: var(--sp-2); align-items: center; font-size: var(--fs-xs); }
.diagram-end { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-family: var(--mono); color: var(--text-2); padding: 2px var(--sp-2); border-radius: var(--r-sm); background: var(--surface-sunken); box-shadow: inset 0 0 0 1px var(--stroke); }
.diagram-line { height: 2px; border-radius: var(--r-full); background: linear-gradient(90deg, color-mix(in srgb, var(--accent) 30%, transparent), var(--accent)); position: relative; }
.diagram-line::after { content: ""; position: absolute; right: -1px; top: -4px; border-width: 5px 0 5px 6px; border-style: solid; border-color: transparent transparent transparent var(--accent); }
</style>
