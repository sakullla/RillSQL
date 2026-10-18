<script setup lang="ts">
import { computed, ref } from "vue";
import { Listbox } from "../ui";
import type { ListboxOption } from "../ui";
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

const actionOptions = computed<ListboxOption[]>(() => fkActions(props.engine).map((action) => ({ value: action, label: action })));

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
  <div class="struct-foreign">
    <p v-if="engine === 'sqlite'" class="callout" :class="{ warn: enforced !== true }">
      SQLite 靠 foreign_keys 开关决定外键是否检查数据，当前这个连接是 {{ foreignKeyLabel(enforced) }}。
      <template v-if="enforced === false">新建的外键不会真正检查数据，可以先为这次连接开启。</template>
      <template v-else-if="enforced === true">新外键会正常检查数据。</template>
      <template v-else>没读到这个开关的状态，新外键可能不会检查数据。</template>
      <button v-if="enforced === false" class="btn sm" type="button" @click="emit('enable')">为这次连接开启</button>
    </p>
    <p v-if="myisam" class="callout warn">这张表使用 MyISAM，不支持外键；不会通过关闭检查来假装创建成功。</p>

    <h3>这张表的外键</h3>
    <p v-if="!foreignKeys.length" class="muted">这张表还没有外键。</p>
    <div v-for="key in foreignKeys" :key="key.name" class="obj-row">
      <code>{{ key.name }}</code>
      <span>{{ key.columns.join(", ") }} → {{ key.refTable }}({{ key.refColumns.join(", ") }})</span>
      <span class="badge sm">删除引用行时 {{ key.onDelete }} · 更新引用列时 {{ key.onUpdate }}</span>
      <span class="grow"></span>
      <button class="btn sm danger" type="button" @click="emit('drop', key.name)">删除</button>
    </div>

    <h3>引用这张表的外键</h3>
    <p v-if="!incoming.length" class="muted">没有其他表引用这张表。</p>
    <div v-for="item in incoming" :key="`${item.table}.${item.name}`" class="obj-row">
      <code>{{ item.table }}.{{ item.name }}</code>
      <span class="muted">{{ item.columns.join(", ") }} → {{ item.refColumns.join(", ") }}</span>
    </div>

    <form v-if="!myisam" class="struct-form" @submit.prevent="previewCreate">
      <label>名称<input v-model="name" type="text" aria-label="外键名称" /></label>
      <label>本表字段<input v-model="column" type="text" aria-label="外键列" placeholder="一列，或多列用逗号分隔" /></label>
      <label v-if="engine === 'postgres'">目标 schema<input v-model="refSchema" type="text" aria-label="目标 schema" /></label>
      <label>目标表<input v-model="refTable" type="text" aria-label="目标表" /></label>
      <label>目标字段<input v-model="refColumn" type="text" aria-label="目标列" placeholder="一列，或多列用逗号分隔" /></label>
      <label>删除引用行时
        <Listbox v-model="onDelete" :options="actionOptions" aria-label="删除引用行时" />
      </label>
      <label>更新引用列时
        <Listbox v-model="onUpdate" :options="actionOptions" aria-label="更新引用列时" />
      </label>
      <button class="btn primary" type="submit">查看将要发生什么</button>
    </form>
  </div>
</template>
