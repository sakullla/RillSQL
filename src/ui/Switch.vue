<script setup lang="ts">
const props = withDefaults(
  defineProps<{
    modelValue: boolean;
    ariaLabel?: string;
    disabled?: boolean;
  }>(),
  { ariaLabel: undefined, disabled: false },
);

const emit = defineEmits<{ "update:modelValue": [value: boolean] }>();

function toggle() {
  if (props.disabled) return;
  emit("update:modelValue", !props.modelValue);
}
</script>

<template>
  <button
    type="button"
    class="switch"
    role="switch"
    :aria-checked="modelValue"
    :aria-label="ariaLabel"
    :disabled="disabled"
    @click="toggle"
  >
    <span class="switch-thumb" aria-hidden="true" />
  </button>
</template>

<style scoped>
.switch {
  position: relative;
  flex: none;
  width: 36px;
  height: 20px;
  padding: 0;
  border: 1px solid var(--line-2);
  border-radius: 10px;
  background: var(--panel);
  cursor: pointer;
  transition: background var(--dur-fast) var(--ease), border-color var(--dur-fast) var(--ease), box-shadow var(--dur-fast) var(--ease);
}
.switch:hover { background: var(--hover); }
.switch:active { box-shadow: inset 0 1px 2px var(--press); }
.switch:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
.switch[aria-checked="true"] { background: var(--accent); border-color: var(--accent); }
.switch[aria-checked="true"]:hover { background: var(--accent); filter: brightness(1.05); }
.switch:disabled { opacity: .55; cursor: not-allowed; }
.switch-thumb {
  position: absolute;
  top: 2px;
  left: 2px;
  width: 14px;
  height: 14px;
  border-radius: 50%;
  background: var(--text);
  transition: transform var(--dur-fast) var(--ease);
}
.switch[aria-checked="true"] .switch-thumb { background: #fff; transform: translateX(16px); }
</style>
