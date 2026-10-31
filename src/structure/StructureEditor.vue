<script setup lang="ts">
import Icon from "../shell/Icon.vue";
import { computed } from "vue";
import { tr as t } from "../i18n";
import { columnImpact, columnMark, impactLevel, pendingCount } from "./logic";
import type { ColumnDraft, StructureColumn } from "./types";

const props = defineProps<{ rows: ColumnDraft[]; baseline: StructureColumn[]; mode: "create" | "edit"; focusName?: string }>();
const emit = defineEmits<{ preview: [] }>();

const changed = computed(() => pendingCount(props.rows, props.baseline));
const colspan = computed(() => (props.mode === "create" ? 7 : 5));

function baselineOf(draft: ColumnDraft): StructureColumn | undefined {
  return props.baseline.find((column) => column.name === draft.original);
}

function markText(draft: ColumnDraft): string {
  const mark = columnMark(draft, baselineOf(draft));
  return mark === "new" ? t("structure.logic.textNew") : mark === "changed" ? t("structure.logic.textChanged") : mark === "dropped" ? t("structure.logic.textDropped") : "";
}

function markBadge(draft: ColumnDraft): "accent" | "warn" | "err" | "" {
  const mark = columnMark(draft, baselineOf(draft));
  return mark === "new" ? "accent" : mark === "changed" ? "warn" : mark === "dropped" ? "err" : "";
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
      <span v-if="mode === 'edit' && changed" class="badge warn">{{ t("structure.editor.changedCount", { n: changed }) }}</span>
      <span v-else-if="mode === 'edit'" class="muted">{{ t("structure.editor.editHint") }}</span>
      <span v-else class="muted">{{ t("structure.editor.createHint") }}</span>
      <span class="grow"></span>
      <button class="btn" type="button" @click="addColumn"><Icon name="plus" />{{ t("structure.editor.addColumn") }}</button>
      <button class="btn primary" type="button" :disabled="!changed" @click="emit('preview')">{{ t("structure.pane.preview") }}</button>
    </div>
    <table class="struct-table">
      <thead>
        <tr>
          <th>{{ t("structure.editor.thField") }}</th>
          <th>{{ t("structure.editor.thType") }}</th>
          <th>{{ t("structure.editor.thNullable") }}</th>
          <th>{{ t("structure.editor.thDefault") }}</th>
          <th v-if="mode === 'create'">{{ t("structure.editor.thPrimary") }}</th>
          <th v-if="mode === 'create'">{{ t("structure.editor.thAuto") }}</th>
          <th></th>
        </tr>
      </thead>
      <tbody>
        <template v-for="(draft, index) in rows" :key="draft.original ?? `new-${index}`">
          <tr :class="{ 'row-drop': draft.dropped, 'row-focus': props.focusName === draft.name }" :data-column="draft.name" :data-focused="props.focusName === draft.name ? 'true' : 'false'">
            <td>
              <div class="cell-name">
                <span v-if="markText(draft)" class="badge sm" :class="markBadge(draft)">{{ markText(draft) }}</span>
                <input v-model="draft.name" type="text" :aria-label="t('structure.editor.fieldName', { n: index + 1 })" :disabled="draft.dropped" />
                <span v-if="mode === 'edit' && draft.primary" class="badge sm">{{ t("structure.editor.primaryBadge") }}</span>
              </div>
            </td>
            <td><input v-model="draft.typeName" type="text" :aria-label="t('structure.editor.typeAria', { n: index + 1 })" :disabled="draft.dropped" /></td>
            <td class="cell-check"><input v-model="draft.nullable" type="checkbox" :aria-label="t('structure.editor.nullableAria', { n: index + 1 })" :disabled="draft.dropped" /></td>
            <td><input v-model="draft.defaultSql" type="text" :aria-label="t('structure.editor.defaultAria', { n: index + 1 })" :disabled="draft.dropped" /></td>
            <td v-if="mode === 'create'" class="cell-check"><input v-model="draft.primary" type="checkbox" :aria-label="t('structure.editor.primaryAria', { n: index + 1 })" :disabled="draft.dropped" /></td>
            <td v-if="mode === 'create'" class="cell-check">
              <input v-model="draft.auto" type="checkbox" :aria-label="t('structure.editor.autoAria', { n: index + 1 })" :disabled="draft.dropped || !draft.primary || !/int/i.test(draft.typeName)" />
            </td>
            <td class="cell-act">
              <button class="btn sm" :class="{ danger: !draft.dropped }" type="button" @click="toggleDrop(draft)">
                {{ draft.dropped ? t("structure.editor.undoDelete") : t("structure.editor.delete") }}
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
