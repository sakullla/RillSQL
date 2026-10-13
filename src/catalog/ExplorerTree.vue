<script setup lang="ts">
defineProps<{ filter: string }>();
</script>

<template>
  <div class="tree" role="tree" aria-label="对象">
    <p class="tree-note subtle">
      目录模块占位。连接后按引擎显示数据库、schema 和表。
      <template v-if="filter">当前筛选“{{ filter }}”不会把读取失败显示成空库。</template>
    </p>
  </div>
</template>
