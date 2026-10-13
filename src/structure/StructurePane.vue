<script setup lang="ts">
defineProps<{ tabId: string; draft: string }>();
</script>

<template>
  <div class="empty" aria-label="表结构" :data-tab-id="tabId" :data-draft="draft">
    <h3>结构占位</h3>
    <p>字段、索引、外键和删表会先给出 SQL 预览。取消时不向数据库提交。</p>
  </div>
</template>
