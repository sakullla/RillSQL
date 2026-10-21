<script setup lang="ts">
import { computed } from "vue";
import { columnImpact, columnMark, impactLevel, pendingCount } from "./logic";
import type { ColumnDraft, StructureColumn } from "./types";

const props = defineProps<{ rows: ColumnDraft[]; baseline: StructureColumn[]; mode: "create" | "edit"; focusName?: string }>();
const emit = defineEmits<{ preview: [] }>();

const changed = computed(() => pendingCount(props.rows, props.baseline));
const colspan = computed(() => (props.mode === "create" ? 7 : 5));

function baselineOf(draft: ColumnDraft): StructureColumn | undefined {
  return props.baseline.find((column) => column.name === draft.original);
}

function markText(draft: ColumnDraft): "" | "新增" | "已修改" | "将删除" {
  const mark = columnMark(draft, baselineOf(draft));
  return mark === "新" ? "新增" : mark === "改" ? "已修改" : mark === "删" ? "将删除" : "";
}

function markBadge(draft: ColumnDraft): "accent" | "warn" | "err" | "" {
  const mark = columnMark(draft, baselineOf(draft));
  return mark === "新" ? "accent" : mark === "改" ? "warn" : mark === "删" ? "err" : "";
}

function impactOf(draft: ColumnDraft): string {
  return columnImpact(draft, baselineOf(draft));
}

function addColumn() {
  props.rows.push({
    original: null,
    name: "",
    typeName: props.mode === "create" ? "INTEGER" : "TEXT",
    nullable: true,
    defaultSql: "",
    primary: false,
    auto: false,
    dropped: false,
  });
}

function toggleDrop(draft: ColumnDraft) {
  if (!draft.original) {
    const index = props.rows.indexOf(draft);
    if (index >= 0) props.rows.splice(index, 1);
    return;
  }
  draft.dropped = !draft.dropped;
}
</script>

<template>
  <div class="struct-editor">
    <div class="struct-toolbar">
      <span v-if="mode === 'edit' && changed" class="badge warn">{{ changed }} 处更改未执行</span>
      <span v-else-if="mode === 'edit'" class="muted">直接改下面的字段；改动先记下来，不会立即写入数据库。</span>
      <span v-else class="muted">这张新表将包含下面这些字段。</span>
      <span class="grow"></span>
      <button class="btn" type="button" @click="addColumn">新增字段</button>
      <button class="btn primary" type="button" :disabled="!changed" @click="emit('preview')">查看将要发生什么</button>
    </div>
    <table class="struct-table">
      <thead>
        <tr>
          <th>字段</th>
          <th>类型</th>
          <th>可空</th>
          <th>默认值</th>
          <th v-if="mode === 'create'">主键</th>
          <th v-if="mode === 'create'">自增</th>
          <th></th>
        </tr>
      </thead>
      <tbody>
        <template v-for="(draft, index) in rows" :key="draft.original ?? `new-${index}`">
          <tr :class="{ 'row-drop': draft.dropped, 'row-focus': props.focusName === draft.name }" :data-column="draft.name" :data-focused="props.focusName === draft.name ? 'true' : 'false'">
            <td>
              <div class="cell-name">
                <span v-if="markText(draft)" class="badge sm" :class="markBadge(draft)">{{ markText(draft) }}</span>
                <input v-model="draft.name" type="text" :aria-label="`字段名 ${index + 1}`" :disabled="draft.dropped" />
                <span v-if="mode === 'edit' && draft.primary" class="badge sm">主键</span>
              </div>
            </td>
            <td><input v-model="draft.typeName" type="text" :aria-label="`类型 ${index + 1}`" :disabled="draft.dropped" /></td>
            <td class="cell-check"><input v-model="draft.nullable" type="checkbox" :aria-label="`可空 ${index + 1}`" :disabled="draft.dropped" /></td>
            <td><input v-model="draft.defaultSql" type="text" :aria-label="`默认值 ${index + 1}`" :disabled="draft.dropped" /></td>
            <td v-if="mode === 'create'" class="cell-check"><input v-model="draft.primary" type="checkbox" :aria-label="`主键 ${index + 1}`" :disabled="draft.dropped" /></td>
            <td v-if="mode === 'create'" class="cell-check">
              <input v-model="draft.auto" type="checkbox" :aria-label="`自增 ${index + 1}`" :disabled="draft.dropped || !draft.primary || !/int/i.test(draft.typeName)" />
            </td>
            <td class="cell-act">
              <button class="btn sm" :class="{ danger: !draft.dropped }" type="button" @click="toggleDrop(draft)">
                {{ draft.dropped ? "撤销删除" : "删除" }}
              </button>
            </td>
          </tr>
          <tr v-if="impactOf(draft)" class="row-impact">
            <td :colspan="colspan">
              <p class="impact" :class="impactLevel(draft, baselineOf(draft))">{{ impactOf(draft) }}</p>
            </td>
          </tr>
        </template>
      </tbody>
    </table>
  </div>
</template>
