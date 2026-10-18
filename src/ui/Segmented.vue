<script setup lang="ts">
import type { SegmentedOption } from "./types";

withDefaults(
  defineProps<{
    modelValue: string;
    options: SegmentedOption[];
    ariaLabel?: string;
    disabled?: boolean;
  }>(),
  { ariaLabel: undefined, disabled: false },
);

const emit = defineEmits<{ "update:modelValue": [value: string] }>();

function choose(option: SegmentedOption) {
  if (option.disabled) return;
  emit("update:modelValue", option.value);
}
</script>

<template>
  <div class="segmented" role="radiogroup" :aria-label="ariaLabel">
    <button
      v-for="option in options"
      :key="option.value"
      type="button"
      class="segmented-item"
      :class="{ picked: option.value === modelValue, disabled: option.disabled || disabled }"
      role="radio"
      :aria-checked="option.value === modelValue"
      :disabled="option.disabled || disabled"
      @click="choose(option)"
    >
      {{ option.label }}
    </button>
  </div>
</template>

<style scoped>
.segmented {
  display: inline-flex;
  align-items: center;
  gap: 2px;
  padding: 2px;
  border: 1px solid var(--line-2);
  border-radius: var(--r);
  background: var(--panel);
}
.segmented-item {
  display: inline-flex;
  align-items: center;
  height: var(--control-h-sm);
  padding: 0 var(--sp-3);
  border: none;
  border-radius: calc(var(--r) - 2px);
  background: none;
  color: var(--text-2);
  font-size: var(--fs-sm);
  cursor: pointer;
  white-space: nowrap;
  transition: background var(--dur-fast) var(--ease), color var(--dur-fast) var(--ease), box-shadow var(--dur-fast) var(--ease);
}
.segmented-item:hover:not(:disabled) { background: var(--hover); }
.segmented-item:active:not(:disabled) { background: var(--active); box-shadow: inset 0 1px 2px var(--press); }
.segmented-item:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
.segmented-item.picked { background: var(--sel); color: var(--text); font-weight: 600; }
.segmented-item:disabled { opacity: .55; cursor: not-allowed; }
</style>
