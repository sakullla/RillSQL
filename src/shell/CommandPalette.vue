<script setup lang="ts">
import { computed, nextTick, ref, watch } from "vue";
import { filterPalette, paletteGroupLabel, type PaletteItem } from "./commands";
import { tr as t } from "../i18n";
import ChordKeys from "./ChordKeys.vue";
import Icon from "./Icon.vue";

const props = defineProps<{ open: boolean; mode: "all" | "tables"; items: PaletteItem[] }>();
const emit = defineEmits<{ close: []; run: [item: PaletteItem] }>();

const query = ref("");
const active = ref(0);
const input = ref<HTMLInputElement | null>(null);

const visible = computed(() => filterPalette(props.items, query.value, props.mode));

watch(
  () => props.open,
  async (open) => {
    if (!open) return;
    query.value = "";
    active.value = 0;
    await nextTick();
    input.value?.focus();
  },
);

function move(delta: number) {
  const count = visible.value.length;
  if (!count) return;
  active.value = (active.value + delta + count) % count;
}

function choose(index = active.value) {
  const item = visible.value[index];
  if (!item) return;
  emit("run", item);
}

function onKey(event: KeyboardEvent) {
  if (event.key === "ArrowDown" || event.key === "ArrowUp") {
    event.preventDefault();
    move(event.key === "ArrowDown" ? 1 : -1);
  } else if (event.key === "Enter") {
    event.preventDefault();
    choose();
  } else if (event.key === "Escape") {
    event.preventDefault();
    emit("close");
  }
}
</script>

<template>
  <div v-if="open" id="palette" @mousedown.self="emit('close')">
    <div class="pal-box" role="dialog" :aria-label="t('shell.palette.title')">
      <label class="pal-input">
        <Icon :name="mode === 'tables' ? 'table' : 'search'" />
        <input
          ref="input"
          v-model="query"
          :placeholder="mode === 'tables' ? t('shell.palette.placeholderTables') : t('shell.palette.placeholderAll')"
          autocomplete="off"
          spellcheck="false"
          role="combobox"
          aria-expanded="true"
          aria-controls="pal-list"
          @keydown="onKey"
        />
      </label>
      <div id="pal-list" class="pal-list" role="listbox">
        <p v-if="!visible.length" class="pal-none">
          {{ mode === "tables" ? t("shell.palette.noTables") : t("shell.palette.noMatch") }}
        </p>
        <template v-else>
          <template v-for="(item, index) in visible" :key="item.id">
            <div v-if="index === 0 || visible[index - 1]?.group !== item.group" class="pal-group">{{ paletteGroupLabel(item.group) }}</div>
            <button
              class="pal-item"
              :class="{ active: index === active }"
              type="button"
              role="option"
              :aria-selected="index === active"
              @mouseenter="active = index"
              @click="choose(index)"
            >
              <Icon :name="item.icon" />
              <span class="pal-label">{{ item.label }}</span>
              <span v-if="item.path" class="pal-path">{{ item.path }}</span>
              <ChordKeys v-if="item.chord" :chord="item.chord" />
            </button>
          </template>
        </template>
      </div>
      <div class="pal-foot">
        <span><ChordKeys chord="Up" /><ChordKeys chord="Down" /> {{ t("shell.palette.hintSelect") }}</span>
        <span><ChordKeys chord="Enter" /> {{ t("shell.palette.hintOpen") }}</span>
        <span><ChordKeys chord="Esc" /> {{ t("common.close") }}</span>
        <span class="grow"></span>
        <span>{{ mode === "tables" ? t("shell.palette.hintTables") : t("shell.palette.hintCommands") }}</span>
      </div>
    </div>
  </div>
</template>
