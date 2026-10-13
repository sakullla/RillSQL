<script setup lang="ts">
import { ICON_PATHS, type IconName } from "./icons";

defineProps<{ name: IconName; label?: string }>();
</script>

<template>
  <svg class="i" viewBox="0 0 24 24" :aria-hidden="label ? undefined : 'true'" :aria-label="label" :role="label ? 'img' : undefined">
    <path :d="ICON_PATHS[name]" />
  </svg>
</template>
