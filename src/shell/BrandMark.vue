<script setup lang="ts">
defineProps<{ large?: boolean }>();
</script>

<template>
  <span class="brand-mark" :class="{ lg: large }" aria-hidden="true">
    <svg viewBox="0 0 32 32">
      <rect x="1" y="1" width="30" height="30" rx="8" />
      <path d="M8 11.5c2.6-1.8 5.4 1.8 8 0s5.4 1.8 8 0M8 16.5c2.6-1.8 5.4 1.8 8 0s5.4 1.8 8 0M8 21.5c2.6-1.8 5.4 1.8 8 0" />
    </svg>
  </span>
</template>
