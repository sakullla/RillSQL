<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { tr as t } from "../i18n";
import { ENV_NONE, TONES, type EnvTag, type EnvTone } from "./model";

const props = defineProps<{
  modelValue: string;
  tags: EnvTag[];
  error?: string;
}>();

const emit = defineEmits<{
  "update:modelValue": [value: string];
  save: [payload: { id?: string; label: string; tone: EnvTone }];
  remove: [id: string];
}>();

const managing = ref(false);
const adding = ref(false);
const label = ref("");
const tone = ref<EnvTone>("dev");
const confirmDelete = ref(false);
const localError = ref("");

const selected = computed(() => props.tags.find((tag) => tag.id === props.modelValue) ?? null);
const editing = computed(() => (adding.value ? null : selected.value));

function fill(tag: EnvTag | null) {
  confirmDelete.value = false;
  localError.value = "";
  if (tag) {
    label.value = tag.label;
    tone.value = tag.tone;
  } else {
    label.value = "";
    tone.value = "dev";
  }
}

function toggleManage() {
  managing.value = !managing.value;
  if (!managing.value) return;
  adding.value = !selected.value;
  fill(adding.value ? null : selected.value);
}

function pick(id: string) {
  adding.value = false;
  emit("update:modelValue", id);
  fill(props.tags.find((tag) => tag.id === id) ?? null);
}

function startAdd() {
  adding.value = true;
  fill(null);
}

function editSelected() {
  if (!selected.value) return;
  adding.value = false;
  fill(selected.value);
}

function submit() {
  const trimmed = label.value.trim();
  if (!trimmed) {
    localError.value = t("connections.tags.errRequired");
    return;
  }
  if ([...trimmed].length > 24) {
    localError.value = t("connections.tags.errTooLong");
    return;
  }
  if (trimmed === t("connections.tags.none")) {
    localError.value = t("connections.tags.errReserved");
    return;
  }
  localError.value = "";
  emit("save", { id: editing.value?.id, label: trimmed, tone: tone.value });
}

function askRemove() {
  if (!editing.value) return;
  if (!confirmDelete.value) {
    confirmDelete.value = true;
    return;
  }
  const id = editing.value.id;
  confirmDelete.value = false;
  emit("remove", id);
}

watch(
  () => [props.modelValue, props.tags] as const,
  () => {
    if (!managing.value) return;
    const tag = props.tags.find((item) => item.id === props.modelValue) ?? null;
    if (adding.value) {
      if (tag && tag.label === label.value.trim()) {
        adding.value = false;
        fill(tag);
      }
      return;
    }
    fill(tag);
  },
);
</script>

<template>
  <fieldset class="field env-field">
    <legend class="env-legend">
      <span>{{ t("connections.tags.legend") }}</span>
      <button class="btn ghost sm" type="button" data-action="env-manage" :aria-expanded="managing" @click="toggleManage">
        {{ managing ? t("connections.tags.done") : t("connections.tags.manage") }}
      </button>
    </legend>
    <div class="env-pick" role="radiogroup" :aria-label="t('connections.tags.legend')">
      <label v-for="item in tags" :key="item.id" :class="{ on: modelValue === item.id }">
        <input type="radio" name="cf-env" :value="item.id" :checked="modelValue === item.id" @change="pick(item.id)" />
        <span class="env" :class="`env-${item.tone}`">{{ item.label }}</span>
      </label>
      <label :class="{ on: modelValue === ENV_NONE }">
        <input type="radio" name="cf-env" :value="ENV_NONE" :checked="modelValue === ENV_NONE" @change="pick(ENV_NONE)" />
        <span class="env env-none">{{ t("connections.tags.none") }}</span>
      </label>
    </div>
    <div v-if="managing" class="env-manage">
      <div class="env-manage-head">
        <b>{{ editing ? t("connections.tags.editTitle") : t("connections.tags.addTitle") }}</b>
        <button v-if="editing" class="btn ghost sm" type="button" data-action="env-add" @click="startAdd">{{ t("connections.tags.addNew") }}</button>
        <button v-else-if="selected" class="btn ghost sm" type="button" data-action="env-edit" @click="editSelected">{{ t("connections.tags.editSelected") }}</button>
      </div>
      <label class="field">
        <span>{{ editing ? t("connections.tags.nameLabel") : t("connections.tags.newLabel") }}</span>
        <input v-model="label" data-field="env-label" maxlength="24" :placeholder="t('connections.tags.placeholder')" @input="confirmDelete = false" />
      </label>
      <div class="tone-pick" role="radiogroup" :aria-label="t('connections.tags.color')">
        <label v-for="item in TONES" :key="item.id" :class="{ on: tone === item.id }">
          <input type="radio" name="cf-env-tone" :value="item.id" :checked="tone === item.id" @change="tone = item.id" />
          <span class="env" :class="`env-${item.id}`">{{ t(item.label) }}</span>
        </label>
      </div>
      <div class="env-manage-actions">
        <button class="btn sm" type="button" data-action="env-save" @click="submit">{{ editing ? t("common.save") : t("connections.tags.add") }}</button>
        <button v-if="editing" class="btn sm ghost danger" type="button" data-action="env-delete" @click="askRemove">
          {{ confirmDelete ? t("connections.tags.confirmDelete") : t("common.remove") }}
        </button>
      </div>
      <small v-if="localError || error" class="field-err">{{ localError || error }}</small>
      <small v-else>{{ t("connections.tags.hint") }}</small>
    </div>
  </fieldset>
</template>
