<script setup lang="ts">
import { columnImpact, columnMark } from "./logic";
import type { ColumnDraft, StructureColumn } from "./types";

const props = defineProps<{ rows: ColumnDraft[]; baseline: StructureColumn[]; mode: "create" | "edit" }>();
const emit = defineEmits<{ preview: [] }>();

function baselineOf(draft: ColumnDraft): StructureColumn | undefined {
  return props.baseline.find((column) => column.name === draft.original);
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
    <div class="struct-nav">
      <strong>字段</strong>
      <span class="grow"></span>
      <button class="btn" type="button" @click="addColumn">新增字段</button>
      <button class="btn primary" type="button" @click="emit('preview')">预览 SQL</button>
    </div>
    <table class="struct-table">
      <thead>
        <tr>
          <th>标记</th>
          <th>名称</th>
          <th>类型</th>
          <th>可空</th>
          <th>默认值</th>
          <th>主键</th>
          <th v-if="mode === 'create'">自增</th>
          <th>影响</th>
          <th></th>
        </tr>
      </thead>
      <tbody>
        <tr v-for="(draft, index) in rows" :key="draft.original ?? `new-${index}`" :class="{ 'row-drop': draft.dropped }">
          <td><span class="mark" :class="columnMark(draft, baselineOf(draft)) === '新' ? 'new' : columnMark(draft, baselineOf(draft)) === '删' ? 'drop' : columnMark(draft, baselineOf(draft)) === '改' ? 'edit' : ''">{{ columnMark(draft, baselineOf(draft)) }}</span></td>
          <td><input v-model="draft.name" type="text" :aria-label="`字段名 ${index + 1}`" :disabled="draft.dropped" /></td>
          <td><input v-model="draft.typeName" type="text" :aria-label="`类型 ${index + 1}`" :disabled="draft.dropped" /></td>
          <td><input v-model="draft.nullable" type="checkbox" :aria-label="`可空 ${index + 1}`" :disabled="draft.dropped" /></td>
          <td><input v-model="draft.defaultSql" type="text" :aria-label="`默认值 ${index + 1}`" :disabled="draft.dropped" /></td>
          <td><input v-model="draft.primary" type="checkbox" :aria-label="`主键 ${index + 1}`" :disabled="draft.dropped || mode === 'edit'" /></td>
          <td v-if="mode === 'create'"><input v-model="draft.auto" type="checkbox" :aria-label="`自增 ${index + 1}`" :disabled="draft.dropped || !draft.primary || !/int/i.test(draft.typeName)" /></td>
          <td>{{ columnImpact(draft, baselineOf(draft)) }}</td>
          <td>
            <button class="btn" type="button" :aria-label="draft.dropped ? '撤销删除' : '删除字段'" @click="toggleDrop(draft)">{{ draft.dropped ? "撤销" : "删除" }}</button>
          </td>
        </tr>
      </tbody>
    </table>
  </div>
</template>
