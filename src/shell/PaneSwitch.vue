<script setup lang="ts">
import { tr as t } from "../i18n";
import Icon from "./Icon.vue";

/** 「数据 / 结构」分段控件：表数据页把它放进网格工具条，结构页让它单独占一行。 */
defineProps<{ pane: "data" | "structure" }>();
const emit = defineEmits<{ select: [pane: "data" | "structure"] }>();
</script>

<template>
  <div class="pane-switch" role="tablist" :aria-label="t('shell.tabs.paneLabel')">
    <button type="button" role="tab" :aria-selected="pane === 'data'" @click="emit('select', 'data')"><Icon name="grid" />{{ t("common.data") }}</button>
    <button type="button" role="tab" :aria-selected="pane === 'structure'" @click="emit('select', 'structure')"><Icon name="columns" />{{ t("common.structure") }}</button>
  </div>
</template>
