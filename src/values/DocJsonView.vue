<script setup lang="ts">
import { computed } from "vue";

const props = defineProps<{ json: string; label?: string }>();

const pretty = computed(() => {
  try {
    return JSON.stringify(JSON.parse(props.json), null, 2);
  } catch {
    return props.json;
  }
});

const valid = computed(() => {
  try {
    const parsed: unknown = JSON.parse(props.json);
    return typeof parsed === "object" && parsed !== null;
  } catch {
    return false;
  }
});
</script>

<!-- 文档 JSON 查看器（R12/ADR-4）：MongoDB 整档文档以 pretty JSON 展示，值查看器家族的一员。 -->
<template>
  <div class="doc-json" data-value-kind="json">
    <p v-if="!valid" class="dj-invalid">这份文档不是合法 JSON：{{ json }}</p>
    <pre v-else class="dj-body mono" :aria-label="label ?? '文档 JSON'">{{ pretty }}</pre>
  </div>
</template>

<style scoped>
.doc-json { min-height: 0; overflow: auto; }
.dj-invalid { color: var(--danger, #c0392b); font-size: 12px; }
.dj-body {
  margin: 0;
  padding: var(--sp-2);
  border: 1px solid var(--line);
  border-radius: var(--r-sm, 6px);
  white-space: pre-wrap;
  word-break: break-all;
  font-size: 12px;
  line-height: 1.6;
}
.mono { font-family: var(--mono, ui-monospace, monospace); }
</style>
