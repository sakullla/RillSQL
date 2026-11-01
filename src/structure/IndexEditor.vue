<script setup lang="ts">
import Icon from "../shell/Icon.vue";
import { computed, ref } from "vue";
import Listbox from "../ui/Listbox.vue";
import { tr as t } from "../i18n";
import { canDropIndex } from "./logic";
import type { StructureIndex } from "./types";

const props = defineProps<{ indexes: StructureIndex[]; columns: string[]; focusName?: string }>();
const emit = defineEmits<{
  drop: [name: string];
  create: [payload: { name: string; unique: boolean; columns: { name: string; descending: boolean }[] }];
}>();

const name = ref("");
const unique = ref(false);
const parts = ref<{ name: string; descending: boolean }[]>([{ name: "", descending: false }]);
const columnOptions = computed(() => props.columns.map((item) => ({ value: item, label: item })));

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
  <div class="struct-indexes struct-sec">
    <p class="hint">{{ t("structure.index.autoNote") }}</p>
    <h3 v-if="indexes.length"><Icon name="sort" />{{ t("structure.index.existingTitle") }}</h3>
    <div v-if="indexes.length" class="obj-list">
      <div v-for="index in indexes" :key="index.name" class="obj-row" :class="{ 'row-focus': props.focusName === index.name }" :data-index="index.name" :data-focused="props.focusName === index.name ? 'true' : 'false'">
        <code>{{ index.name }}</code>
        <span class="badge sm" :class="index.unique ? 'accent' : undefined">{{ index.unique ? t("structure.index.unique") : t("structure.index.plain") }}</span>
        <span class="muted">{{ index.columns.map((item) => `${item.name}${item.descending ? t("structure.index.descending") : ""}`).join(" → ") }}</span>
        <span class="grow"></span>
        <span v-if="!canDropIndex(index)" class="lock">{{ t("structure.index.cannotDrop", { reason: index.reason || t("structure.index.constraintKept") }) }}</span>
        <button v-else class="btn sm danger" type="button" @click="emit('drop', index.name)">{{ t("structure.index.delete") }}</button>
      </div>
    </div>
    <p v-else class="muted">{{ t("structure.index.empty") }}</p>
    <form class="struct-form" @submit.prevent="previewCreate(columns)">
      <label>{{ t("structure.index.nameLabel") }}<input v-model="name" type="text" :aria-label="t('structure.index.nameAria')" /></label>
      <div v-for="(part, index) in parts" :key="index" class="part-row">
        <label>{{ t("structure.index.partLabel", { n: index + 1 }) }}
          <Listbox :model-value="part.name" :options="columnOptions" :aria-label="t('structure.index.partAria', { n: index + 1 })" @update:model-value="part.name = $event" />
        </label>
        <label class="check"><input v-model="part.descending" type="checkbox" />{{ t("structure.index.descendingCheck") }}</label>
      </div>
      <button class="btn" type="button" @click="addPart">{{ t("structure.index.addPart") }}</button>
      <label class="check"><input v-model="unique" type="checkbox" />{{ t("structure.index.uniqueCheck") }}</label>
      <button class="btn primary" type="submit">{{ t("structure.index.preview") }}</button>
    </form>
  </div>
</template>
