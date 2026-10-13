<script setup lang="ts">
defineProps<{
  env: "dev" | "test" | "prod" | "local";
  label: string;
}>();
</script>

<template>
  <span class="env xs" :class="`env-${env}`">{{ label }}</span>
</template>
