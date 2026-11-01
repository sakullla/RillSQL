<script setup lang="ts">
import { toasts } from "./toast";
</script>

<template>
  <div class="toast-host" role="status" aria-live="polite">
    <div v-for="item in toasts" :key="item.id" class="toast" :class="`toast-${item.tone}`">{{ item.text }}</div>
  </div>
</template>
