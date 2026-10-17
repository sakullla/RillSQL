<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, watch } from "vue";
import type { ListboxOption } from "./types";

const props = withDefaults(
  defineProps<{
    modelValue: string | null;
    options: ListboxOption[];
    placeholder?: string;
    ariaLabel?: string;
    filterable?: boolean;
    id?: string;
  }>(),
  { placeholder: "请选择", ariaLabel: undefined, filterable: false, id: undefined },
);

const emit = defineEmits<{ "update:modelValue": [value: string] }>();

const open = ref(false);
const query = ref("");
const active = ref(0);
const trigger = ref<HTMLButtonElement | null>(null);
const list = ref<HTMLUListElement | null>(null);
const filter = ref<HTMLInputElement | null>(null);
const pop = ref<HTMLElement | null>(null);
const dropUp = ref(false);

const selected = computed(() => props.options.find((option) => option.value === props.modelValue) ?? null);

const visible = computed(() => {
  const needle = query.value.trim().toLowerCase();
  if (!props.filterable || !needle) return props.options;
  return props.options.filter(
    (option) => option.label.toLowerCase().includes(needle) || option.value.toLowerCase().includes(needle),
  );
});

const listId = computed(() => (props.id ? `${props.id}-list` : undefined));

function onOutside(event: PointerEvent) {
  const target = event.target;
  if (!(target instanceof Node)) return;
  if (trigger.value?.contains(target)) return;
  if (list.value?.contains(target)) return;
  if (filter.value?.contains(target)) return;
  close();
}

function place() {
  const anchor = trigger.value;
  const panel = pop.value;
  if (!anchor || !panel) return;
  const rect = anchor.getBoundingClientRect();
  // jsdom 下 offsetHeight 为 0，退回估算高度，保证单测也能触发翻转。
  const height = panel.offsetHeight || visible.value.length * 24 + 12;
  dropUp.value = rect.bottom + 4 + height > window.innerHeight && rect.top > height + 4;
}

function openList() {
  if (open.value) return;
  open.value = true;
  query.value = "";
  const picked = props.options.findIndex((option) => option.value === props.modelValue);
  active.value = picked >= 0 ? picked : 0;
  document.addEventListener("pointerdown", onOutside, true);
  window.addEventListener("resize", place);
  void nextTick(() => {
    place();
    (props.filterable ? filter.value : list.value)?.focus();
  });
}

function close() {
  if (!open.value) return;
  open.value = false;
  document.removeEventListener("pointerdown", onOutside, true);
  window.removeEventListener("resize", place);
}

function toggle() {
  if (open.value) close();
  else openList();
}

function move(step: number) {
  const items = visible.value;
  if (!items.length) return;
  let index = active.value;
  for (let count = 0; count < items.length; count += 1) {
    index = (index + step + items.length) % items.length;
    if (!items[index].disabled) break;
  }
  active.value = index;
}

function choose(option: ListboxOption) {
  if (option.disabled) return;
  emit("update:modelValue", option.value);
  close();
  trigger.value?.focus();
}

function onTriggerKey(event: KeyboardEvent) {
  if (open.value) return;
  if (event.key === "ArrowDown" || event.key === "ArrowUp" || event.key === "Enter" || event.key === " ") {
    event.preventDefault();
    openList();
  }
}

function onPopupKey(event: KeyboardEvent) {
  if (event.key === "Escape") {
    event.preventDefault();
    close();
    trigger.value?.focus();
    return;
  }
  if (event.key === "ArrowDown") {
    event.preventDefault();
    move(1);
    return;
  }
  if (event.key === "ArrowUp") {
    event.preventDefault();
    move(-1);
    return;
  }
  if (event.key === "Home") {
    event.preventDefault();
    active.value = 0;
    return;
  }
  if (event.key === "End") {
    event.preventDefault();
    active.value = Math.max(0, visible.value.length - 1);
    return;
  }
  if (event.key === "Enter") {
    event.preventDefault();
    const option = visible.value[active.value];
    if (option) choose(option);
    return;
  }
  if (event.key === "Tab") close();
}

watch(visible, (items) => {
  if (active.value >= items.length) active.value = Math.max(0, items.length - 1);
});

onBeforeUnmount(() => {
  document.removeEventListener("pointerdown", onOutside, true);
  window.removeEventListener("resize", place);
});
</script>

<template>
  <div class="listbox" :class="{ open }">
    <button
      ref="trigger"
      :id="id"
      type="button"
      class="listbox-trigger"
      role="combobox"
      :aria-expanded="open"
      aria-haspopup="listbox"
      :aria-controls="listId"
      :aria-label="ariaLabel"
      @click="toggle"
      @keydown="onTriggerKey"
    >
      <span class="listbox-value" :class="{ placeholder: !selected }">{{ selected ? selected.label : placeholder }}</span>
      <svg class="i listbox-caret" viewBox="0 0 24 24" aria-hidden="true"><path d="M6 9l6 6 6-6" /></svg>
    </button>
    <div v-if="open" ref="pop" class="listbox-pop" :class="{ up: dropUp }">
      <input
        v-if="filterable"
        ref="filter"
        v-model="query"
        class="listbox-filter"
        type="search"
        placeholder="筛选"
        aria-label="筛选选项"
        @keydown="onPopupKey"
      />
      <ul :id="listId" ref="list" class="listbox-list" role="listbox" tabindex="-1" :aria-label="ariaLabel" @keydown="onPopupKey">
        <li
          v-for="(option, index) in visible"
          :key="option.value"
          class="listbox-option"
          :class="{ on: index === active, picked: option.value === modelValue, disabled: option.disabled }"
          role="option"
          :aria-selected="option.value === modelValue"
          :aria-disabled="option.disabled"
          @mouseenter="active = index"
          @click="choose(option)"
        >
          <span>{{ option.label }}</span>
          <span v-if="option.hint" class="listbox-hint">{{ option.hint }}</span>
        </li>
        <li v-if="!visible.length" class="listbox-empty" role="presentation">没有匹配项</li>
      </ul>
    </div>
  </div>
</template>

<style scoped>
.listbox { position: relative; display: inline-flex; min-width: 0; }
.listbox-trigger {
  display: inline-flex;
  align-items: center;
  gap: var(--sp-2);
  height: var(--control-h);
  max-width: 100%;
  padding: 0 var(--sp-2) 0 var(--sp-3);
  border: 1px solid var(--line-2);
  border-radius: var(--r);
  background: var(--panel);
  color: var(--text);
  font-size: var(--fs-sm);
  transition: background var(--dur-fast) var(--ease), border-color var(--dur-fast) var(--ease), box-shadow var(--dur-fast) var(--ease);
}
.listbox-trigger:hover { background: var(--hover); }
.listbox-trigger:active { background: var(--active); box-shadow: inset 0 1px 2px var(--press); transform: translateY(.5px); }
.listbox-trigger:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
.listbox.open .listbox-trigger { border-color: var(--accent); box-shadow: 0 0 0 2px var(--ring); }
.listbox-value { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.listbox-value.placeholder { color: var(--text-3); }
.listbox-caret { width: 14px; height: 14px; color: var(--text-3); transition: transform var(--dur-fast) var(--ease); }
.listbox.open .listbox-caret { transform: rotate(180deg); }
.listbox-pop {
  position: absolute;
  top: calc(100% + 4px);
  left: 0;
  z-index: var(--z-menu);
  min-width: 100%;
  max-width: 320px;
  padding: var(--sp-1);
  border: 1px solid var(--line-2);
  border-radius: var(--r-md);
  background: var(--panel);
  box-shadow: var(--shadow);
}
.listbox-pop.up { top: auto; bottom: calc(100% + 4px); }
.listbox-filter {
  width: 100%;
  height: var(--control-h-sm);
  margin-bottom: var(--sp-1);
  padding: 0 var(--sp-2);
  border: 1px solid var(--line-2);
  border-radius: var(--r);
  background: var(--panel);
  font-size: var(--fs-sm);
}
.listbox-filter:focus { outline: none; border-color: var(--accent); box-shadow: 0 0 0 2px var(--ring); }
.listbox-list { list-style: none; margin: 0; padding: 0; max-height: 260px; overflow: auto; }
.listbox-option {
  display: flex;
  align-items: center;
  gap: var(--sp-2);
  height: var(--control-h-sm);
  padding: 0 var(--sp-2);
  border-radius: var(--r);
  font-size: var(--fs-sm);
  color: var(--text);
  cursor: default;
  white-space: nowrap;
}
.listbox-option.on { background: var(--hover); }
.listbox-option.picked { background: var(--sel-strong); color: var(--accent-text); font-weight: 600; }
.listbox-option.disabled { color: var(--disabled-fg); }
.listbox-option.disabled.on { background: none; }
.listbox-hint { margin-left: auto; font-size: var(--fs-xs); color: var(--text-3); }
.listbox-empty { padding: var(--sp-2); font-size: var(--fs-sm); color: var(--text-3); }
</style>
