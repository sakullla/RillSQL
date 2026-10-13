<script setup lang="ts">
import { computed } from "vue";
import { chordParts, detectMac } from "./shortcuts";

const props = defineProps<{ chord: string; mac?: boolean }>();
const parts = computed(() => chordParts(props.chord, props.mac ?? detectMac()));
</script>

<template>
  <span class="kbd">
    <kbd v-for="part in parts" :key="part">{{ part }}</kbd>
  </span>
</template>
