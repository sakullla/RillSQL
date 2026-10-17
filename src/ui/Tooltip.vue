<script setup lang="ts">
import { onBeforeUnmount, ref } from "vue";

const props = withDefaults(
  defineProps<{ text: string; delay?: number; placement?: "top" | "bottom" }>(),
  { delay: 400, placement: "top" },
);

const shown = ref(false);
let timer: ReturnType<typeof setTimeout> | undefined;

function show() {
  if (!props.text) return;
  clearTimeout(timer);
  timer = setTimeout(() => {
    shown.value = true;
  }, props.delay);
}

function hide() {
  clearTimeout(timer);
  shown.value = false;
}

onBeforeUnmount(() => clearTimeout(timer));
</script>

<template>
  <span class="tip-host" @mouseenter="show" @mouseleave="hide" @focusin="show" @focusout="hide">
    <slot />
    <span v-if="shown" class="tooltip" role="tooltip" :class="`tip-${placement}`">{{ text }}</span>
  </span>
</template>

<style scoped>
.tip-host { position: relative; display: inline-flex; }
.tooltip { position: absolute; left: 50%; transform: translateX(-50%); animation: tip-in var(--dur-fast) var(--ease); }
.tip-top { bottom: calc(100% + 6px); }
.tip-bottom { top: calc(100% + 6px); }
@keyframes tip-in {
  from { opacity: 0; transform: translateX(-50%) translateY(3px); }
  to { opacity: 1; transform: translateX(-50%) translateY(0); }
}
@media (prefers-reduced-motion: reduce) {
  .tooltip { animation: none; }
}
</style>
