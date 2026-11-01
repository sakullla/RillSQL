<script setup lang="ts">
import Icon from "../shell/Icon.vue";
import { computed, ref } from "vue";
import Listbox from "../ui/Listbox.vue";
import { tr as t } from "../i18n";
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
const actionOptions = computed(() => fkActions(props.engine).map((action) => ({ value: action, label: action })));

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
  <div class="struct-foreign struct-sec">
    <p v-if="engine === 'sqlite'" class="callout" :class="{ warn: enforced !== true }">
      {{ t("structure.foreign.sqliteSwitch", { state: foreignKeyLabel(enforced) }) }}
      <template v-if="enforced === false">{{ t("structure.foreign.offNote") }}</template>
      <template v-else-if="enforced === true">{{ t("structure.foreign.onNote") }}</template>
      <template v-else>{{ t("structure.foreign.unknownNote") }}</template>
      <button v-if="enforced === false" class="btn sm" type="button" @click="emit('enable')">{{ t("structure.foreign.enableForConnection") }}</button>
    </p>
    <p v-if="myisam" class="callout warn">{{ t("structure.foreign.myisam") }}</p>

    <h3><Icon name="link" />{{ t("structure.foreign.ownTitle") }}</h3>
    <div v-if="foreignKeys.length" class="obj-list">
      <div v-for="key in foreignKeys" :key="key.name" class="obj-row">
        <code>{{ key.name }}</code>
        <span>{{ key.columns.join(", ") }} → {{ key.refTable }}({{ key.refColumns.join(", ") }})</span>
        <span class="badge sm">{{ t("structure.foreign.ruleLabel", { onDelete: key.onDelete, onUpdate: key.onUpdate }) }}</span>
        <span class="grow"></span>
        <button class="btn sm danger" type="button" @click="emit('drop', key.name)">{{ t("structure.foreign.deleteBtn") }}</button>
      </div>
    </div>
    <p v-else class="muted">{{ t("structure.foreign.ownEmpty") }}</p>

    <h3><Icon name="external" />{{ t("structure.foreign.incomingTitle") }}</h3>
    <div v-if="incoming.length" class="obj-list">
      <div v-for="item in incoming" :key="`${item.table}.${item.name}`" class="obj-row">
        <code>{{ item.table }}.{{ item.name }}</code>
        <span class="muted">{{ item.columns.join(", ") }} → {{ item.refColumns.join(", ") }}</span>
      </div>
    </div>
    <p v-else class="muted">{{ t("structure.foreign.incomingEmpty") }}</p>

    <form v-if="!myisam" class="struct-form" @submit.prevent="previewCreate">
      <label>{{ t("structure.foreign.nameLabel") }}<input v-model="name" type="text" :aria-label="t('structure.foreign.nameAria')" /></label>
      <label>{{ t("structure.foreign.columnLabel") }}<input v-model="column" type="text" :aria-label="t('structure.foreign.columnAria')" :placeholder="t('structure.foreign.columnPlaceholder')" /></label>
      <label v-if="engine === 'postgres'">{{ t("structure.foreign.refSchemaLabel") }}<input v-model="refSchema" type="text" :aria-label="t('structure.foreign.refSchemaAria')" /></label>
      <label>{{ t("structure.foreign.refTableLabel") }}<input v-model="refTable" type="text" :aria-label="t('structure.foreign.refTableAria')" /></label>
      <label>{{ t("structure.foreign.refColumnLabel") }}<input v-model="refColumn" type="text" :aria-label="t('structure.foreign.refColumnAria')" :placeholder="t('structure.foreign.columnPlaceholder')" /></label>
      <label>{{ t("structure.foreign.onDelete") }}
        <Listbox :model-value="onDelete" :options="actionOptions" :aria-label="t('structure.foreign.onDelete')" @update:model-value="onDelete = $event" />
      </label>
      <label>{{ t("structure.foreign.onUpdate") }}
        <Listbox :model-value="onUpdate" :options="actionOptions" :aria-label="t('structure.foreign.onUpdate')" @update:model-value="onUpdate = $event" />
      </label>
      <button class="btn primary" type="submit">{{ t("structure.foreign.preview") }}</button>
    </form>
  </div>
</template>
