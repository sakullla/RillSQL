<script setup lang="ts">
import { computed, ref, watch } from "vue";
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
    localError.value = "请填写标签名称";
    return;
  }
  if ([...trimmed].length > 24) {
    localError.value = "标签名称最多 24 个字";
    return;
  }
  if (trimmed === "无标签") {
    localError.value = "无标签表示不使用环境标签";
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
      <span>环境标签</span>
      <button class="btn ghost sm" type="button" data-action="env-manage" :aria-expanded="managing" @click="toggleManage">
        {{ managing ? "完成" : "管理" }}
      </button>
    </legend>
    <div class="env-pick" role="radiogroup" aria-label="环境标签">
      <label v-for="item in tags" :key="item.id" :class="{ on: modelValue === item.id }">
        <input type="radio" name="cf-env" :value="item.id" :checked="modelValue === item.id" @change="pick(item.id)" />
        <span class="env" :class="`env-${item.tone}`">{{ item.label }}</span>
      </label>
      <label :class="{ on: modelValue === ENV_NONE }">
        <input type="radio" name="cf-env" :value="ENV_NONE" :checked="modelValue === ENV_NONE" @change="pick(ENV_NONE)" />
        <span class="env env-none">无标签</span>
      </label>
    </div>
    <div v-if="managing" class="env-manage">
      <div class="env-manage-head">
        <b>{{ editing ? "编辑所选标签" : "添加标签" }}</b>
        <button v-if="editing" class="btn ghost sm" type="button" data-action="env-add" @click="startAdd">添加新标签</button>
        <button v-else-if="selected" class="btn ghost sm" type="button" data-action="env-edit" @click="editSelected">编辑所选</button>
      </div>
      <label class="field">
        <span>{{ editing ? "名称" : "新标签" }}</span>
        <input v-model="label" data-field="env-label" maxlength="24" placeholder="例如 预发" @input="confirmDelete = false" />
      </label>
      <div class="tone-pick" role="radiogroup" aria-label="标签颜色">
        <label v-for="item in TONES" :key="item.id" :class="{ on: tone === item.id }">
          <input type="radio" name="cf-env-tone" :value="item.id" :checked="tone === item.id" @change="tone = item.id" />
          <span class="env" :class="`env-${item.id}`">{{ item.label }}</span>
        </label>
      </div>
      <div class="env-manage-actions">
        <button class="btn sm" type="button" data-action="env-save" @click="submit">{{ editing ? "保存" : "添加" }}</button>
        <button v-if="editing" class="btn sm ghost danger" type="button" data-action="env-delete" @click="askRemove">
          {{ confirmDelete ? "确认删除" : "删除" }}
        </button>
      </div>
      <small v-if="localError || error" class="field-err">{{ localError || error }}</small>
      <small v-else>删除后，使用这个标签的连接会改为无标签。改名会留在原来的连接上。</small>
    </div>
  </fieldset>
</template>
