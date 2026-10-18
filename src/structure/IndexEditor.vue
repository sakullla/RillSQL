<script setup lang="ts">
import { computed, ref } from "vue";
import { Listbox } from "../ui";
import type { ListboxOption } from "../ui";
import { canDropIndex } from "./logic";
import type { StructureIndex } from "./types";

const props = defineProps<{ indexes: StructureIndex[]; columns: string[] }>();
const emit = defineEmits<{
  drop: [name: string];
  create: [payload: { name: string; unique: boolean; columns: { name: string; descending: boolean }[] }];
}>();

const name = ref("");
const unique = ref(false);
const parts = ref<{ name: string; descending: boolean }[]>([{ name: "", descending: false }]);

const columnOptions = computed<ListboxOption[]>(() => props.columns.map((item) => ({ value: item, label: item })));

function addPart() {
  parts.value.push({ name: "", descending: false });
}

function previewCreate(columns: string[]) {
  const chosen = parts.value
    .map((part) => ({ name: part.name || columns[0] || "", descending: part.descending }))
    .filter((part) => part.name);
  if (!name.value.trim() || !chosen.length) return;
  emit("create", { name: name.value.trim(), unique: unique.value, columns: chosen });
}
</script>

<template>
  <div class="struct-indexes">
    <p class="hint">由主键、唯一约束或外键自动维护的索引不能单独删除；要移除它，需要改对应的约束。</p>
    <p v-if="!indexes.length" class="muted">这张表还没有索引。</p>
    <div v-for="index in indexes" :key="index.name" class="obj-row">
      <code>{{ index.name }}</code>
      <span class="badge sm">{{ index.unique ? "唯一" : "普通" }}</span>
      <span class="muted">{{ index.columns.map((item) => `${item.name}${item.descending ? "（降序）" : ""}`).join(" → ") }}</span>
      <span class="grow"></span>
      <span v-if="!canDropIndex(index)" class="lock">{{ index.reason || "由约束维护" }}，不能单独删除</span>
      <button v-else class="btn sm danger" type="button" @click="emit('drop', index.name)">删除</button>
    </div>
    <form class="struct-form" @submit.prevent="previewCreate(columns)">
      <label>索引名<input v-model="name" type="text" aria-label="索引名" /></label>
      <div v-for="(part, index) in parts" :key="index" class="part-row">
        <label>第 {{ index + 1 }} 列
          <Listbox v-model="part.name" :options="columnOptions" :aria-label="`索引列 ${index + 1}`" placeholder="选择字段" />
        </label>
        <label class="check"><input v-model="part.descending" type="checkbox" />降序</label>
      </div>
      <button class="btn" type="button" @click="addPart">再加一列</button>
      <label class="check"><input v-model="unique" type="checkbox" />唯一索引：不允许重复值</label>
      <button class="btn primary" type="submit">查看将要发生什么</button>
    </form>
  </div>
</template>
