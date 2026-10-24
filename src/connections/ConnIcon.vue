<script setup lang="ts">
import { ICON_PATHS } from "../shell/icons";

defineProps<{ name: string }>();

/* 连接页统一消费 Icon.vue 线稿体系（src/shell/icons.ts，ADR-4）。
   只有三个连接语义图标尚未进入全局图标集，在这里按同一套 24×24 单 path 线稿规范补齐，
   描边规格（圆端点、1.7 线宽）仍由 base.css 的 .i 提供。 */
const CONNECTION_PATHS: Record<string, string> = {
  /* 开锁：锁体与全局 lock 一致，锁闩右侧抬起断开 */
  unlock: "M7.5 11h9a2 2 0 0 1 2 2v5.5a2 2 0 0 1-2 2h-9a2 2 0 0 1-2-2V13a2 2 0 0 1 2-2ZM8.5 11V7.5a3.5 3.5 0 0 1 6.7-1.4",
  /* 跳板机通道：两条平行轨道 + 两侧竖井 */
  tunnel: "M4 7h16M4 17h16M7 7v10M17 7v10",
  /* 已拔下：上半插齿与插头体、下半座体纵向分离出断口 */
  unplug: "M8 3.5V8M16 3.5V8M6 10.5V9.5a1.5 1.5 0 0 1 1.5-1.5h9A1.5 1.5 0 0 1 18 9.5v1M6 13v1.5a6 6 0 0 0 12 0V13",
};

const paths: Record<string, string> = { ...ICON_PATHS, ...CONNECTION_PATHS };
</script>

<template>
  <svg class="i" viewBox="0 0 24 24" aria-hidden="true">
    <path :d="paths[name] ?? ''" />
  </svg>
</template>
