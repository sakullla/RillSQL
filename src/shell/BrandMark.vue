<script setup lang="ts">
import brandIcon from "../assets/brand.png";

defineProps<{ large?: boolean }>();
</script>

<template>
  <span class="brand-mark" :class="{ lg: large }" aria-hidden="true">
    <img :src="brandIcon" alt="" width="22" height="22" draggable="false" />
  </span>
</template>
