<script setup lang="ts">
import { ref } from "vue";
import { fkActions, foreignKeyLabel } from "./logic";
import type { StructureForeignKey, StructureIncoming } from "./types";

const props = defineProps<{
  foreignKeys: StructureForeignKey[];
  incoming: StructureIncoming[];
  columns: string[];
  engine: string;
  storage: string | null;
  enforced: boolean | null;
}>();
const emit = defineEmits<{
  drop: [name: string];
  create: [foreignKey: StructureForeignKey];
  enable: [];
}>();

const name = ref("");
const column = ref("");
const refSchema = ref("");
const refTable = ref("");
const refColumn = ref("");
const onDelete = ref("NO ACTION");
const onUpdate = ref("NO ACTION");
const myisam = props.storage?.toLowerCase() === "myisam";

function splitColumns(value: string): string[] {
  return value.split(",").map((item) => item.trim()).filter(Boolean);
}

function previewCreate() {
  const locals = splitColumns(column.value || props.columns[0] || "");
  const remotes = splitColumns(refColumn.value);
  if (!name.value.trim() || !locals.length || !refTable.value.trim() || !remotes.length) return;
  emit("create", {
    name: name.value.trim(),
    columns: locals,
    refSchema: refSchema.value.trim(),
    refTable: refTable.value.trim(),
    refColumns: remotes,
    onDelete: onDelete.value,
    onUpdate: onUpdate.value,
  });
}
</script>

<template>
  <div>
    <p v-if="engine === 'sqlite'" class="callout" :class="{ warn: enforced !== true }">
      当前连接 foreign_keys = {{ foreignKeyLabel(enforced) }}。不会为了让创建看起来成功而静默关闭检查。
      <button v-if="enforced === false" class="btn" type="button" @click="emit('enable')">为本次连接开启</button>
    </p>
    <p v-if="myisam" class="callout warn">这张表使用 MyISAM，不支持外键。不会通过关闭检查来假装创建成功。</p>
    <div v-for="key in foreignKeys" :key="key.name" class="obj-row">
      <code>{{ key.name }}</code>
      <span>{{ key.columns.join(", ") }} → {{ key.refTable }}({{ key.refColumns.join(", ") }})</span>
      <span class="pill">{{ key.onDelete }} / {{ key.onUpdate }}</span>
      <span class="grow"></span>
      <button class="btn" type="button" @click="emit('drop', key.name)">删除</button>
    </div>
    <h3>引用此表的外键</h3>
    <p v-if="!incoming.length" class="muted">没有其他表引用这张表。</p>
    <div v-for="item in incoming" :key="`${item.table}.${item.name}`" class="obj-row">
      <code>{{ item.table }}.{{ item.name }}</code>
      <span>{{ item.columns.join(", ") }} → {{ item.refColumns.join(", ") }}</span>
    </div>
    <form v-if="!myisam" class="struct-form" @submit.prevent="previewCreate">
      <label>名称<input v-model="name" type="text" aria-label="外键名称" /></label>
      <label>本表列<input v-model="column" type="text" aria-label="外键列" placeholder="一列，或多列用逗号分隔" /></label>
      <label v-if="engine === 'postgres'">目标 schema<input v-model="refSchema" type="text" aria-label="目标 schema" /></label>
      <label>目标表<input v-model="refTable" type="text" aria-label="目标表" /></label>
      <label>目标列<input v-model="refColumn" type="text" aria-label="目标列" /></label>
      <label>ON DELETE
        <select v-model="onDelete" aria-label="ON DELETE">
          <option v-for="action in fkActions(engine)" :key="action">{{ action }}</option>
        </select>
      </label>
      <label>ON UPDATE
        <select v-model="onUpdate" aria-label="ON UPDATE">
          <option v-for="action in fkActions(engine)" :key="action">{{ action }}</option>
        </select>
      </label>
      <button class="btn primary" type="submit">预览 SQL</button>
    </form>
  </div>
</template>
