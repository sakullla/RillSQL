<script setup lang="ts">
defineProps<{ active: boolean }>();
</script>

<template>
  <!-- 拖入时的视觉反馈：enter/hover 高亮，pointer-events 不拦截，原生 drop 事件仍到 WebView。 -->
  <div v-if="active" class="drop-overlay" aria-hidden="true">
    <div class="drop-card">
      <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path d="M12 3v12m0 0 4-4m-4 4-4-4M4 17v2a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-2" /></svg>
      <b>松开以预览文件</b>
      <small>拖放预览支持 CSV 与 JSON；其他格式会提示不支持</small>
    </div>
  </div>
</template>

<style scoped>
.drop-overlay { position: fixed; inset: 0; z-index: var(--z-float); pointer-events: none; background: color-mix(in srgb, var(--accent-soft) 30%, var(--overlay)); border: 2px dashed var(--accent); display: grid; place-items: center; }
.drop-card { display: flex; flex-direction: column; align-items: center; gap: var(--sp-1); padding: var(--sp-5) var(--sp-7); border-radius: var(--r-xl); background: var(--surface-overlay); color: var(--text); border: 1px solid var(--stroke-strong); box-shadow: var(--shadow-lg); }
.drop-card .i { width: 22px; height: 22px; color: var(--accent); }
.drop-card b { font-size: var(--fs-md); font-weight: 650; }
.drop-card small { font-size: var(--fs-xs); color: var(--text-2); }
</style>
