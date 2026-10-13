<script setup lang="ts">
import { ref } from "vue";
import ExplorerTree from "../catalog/ExplorerTree.vue";
import Icon from "./Icon.vue";

defineProps<{ width: number }>();
const emit = defineEmits<{ resize: [width: number] }>();
const filter = ref("");

function onPointerDown(event: PointerEvent) {
  const startX = event.clientX;
  const startWidth = (event.currentTarget as HTMLElement).previousElementSibling?.getBoundingClientRect().width ?? 268;
  const move = (ev: PointerEvent) => {
    emit("resize", Math.min(460, Math.max(200, startWidth + ev.clientX - startX)));
  };
  const stop = () => {
    window.removeEventListener("pointermove", move);
    window.removeEventListener("pointerup", stop);
  };
  window.addEventListener("pointermove", move);
  window.addEventListener("pointerup", stop);
}
</script>

<template>
  <aside class="sidebar" :style="{ width: `${width}px` }" aria-label="资源管理器">
    <div class="sb-head">
      <span class="sb-title">资源管理器</span>
    </div>
    <label class="sb-search">
      <Icon name="search" />
      <input v-model="filter" type="search" placeholder="筛选表" aria-label="筛选表" />
    </label>
    <ExplorerTree :filter="filter" />
  </aside>
  <div
    class="sb-resizer"
    role="separator"
    aria-orientation="vertical"
    aria-label="调整资源管理器宽度"
    tabindex="0"
    @pointerdown="onPointerDown"
  ></div>
</template>
