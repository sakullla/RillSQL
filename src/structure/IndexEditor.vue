<script setup lang="ts">
import { ref } from "vue";
import { canDropIndex } from "./logic";
import type { StructureIndex } from "./types";

defineProps<{ indexes: StructureIndex[]; columns: string[] }>();
const emit = defineEmits<{
  drop: [name: string];
  create: [payload: { name: string; unique: boolean; columns: { name: string; descending: boolean }[] }];
}>();

const name = ref("");
const unique = ref(false);
const parts = ref<{ name: string; descending: boolean }[]>([{ name: "", descending: false }]);

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
  <div>
    <p class="muted">由主键、唯一约束或外键维护的索引带锁，不能当作普通索引单独删除。</p>
    <div v-for="index in indexes" :key="index.name" class="obj-row">
      <code>{{ index.name }}</code>
      <span class="pill">{{ index.unique ? "唯一" : "普通" }}</span>
      <span>{{ index.columns.map((item) => `${item.name} ${item.descending ? "DESC" : "ASC"}`).join(" → ") }}</span>
      <span class="grow"></span>
      <span v-if="!canDropIndex(index)" class="muted">{{ index.reason || "由约束维护" }}，不能单独删除</span>
      <button v-else class="btn" type="button" @click="emit('drop', index.name)">删除</button>
    </div>
    <form class="struct-form" @submit.prevent="previewCreate(columns)">
      <label>索引名<input v-model="name" type="text" aria-label="索引名" /></label>
      <label v-for="(part, index) in parts" :key="index">列 {{ index + 1 }}
        <select v-model="part.name" :aria-label="`索引列 ${index + 1}`">
          <option v-for="item in columns" :key="item" :value="item">{{ item }}</option>
        </select>
        <label><input v-model="part.descending" type="checkbox" />降序</label>
      </label>
      <button class="btn" type="button" @click="addPart">增加列</button>
      <label><input v-model="unique" type="checkbox" />唯一</label>
      <button class="btn primary" type="submit">预览 SQL</button>
    </form>
  </div>
</template>
