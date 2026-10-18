<script setup lang="ts">
withDefaults(
  defineProps<{
    title: string;
    detail?: string;
    icon?: string;
    actionLabel?: string;
    actionId?: string;
  }>(),
  { detail: undefined, icon: undefined, actionLabel: undefined, actionId: undefined },
);

const emit = defineEmits<{ action: [] }>();
</script>

<template>
  <div class="empty-state" data-empty-state role="status">
    <svg v-if="icon" class="i empty-state-icon" viewBox="0 0 24 24" aria-hidden="true">
      <path :d="icon" />
    </svg>
    <p class="empty-state-title">{{ title }}</p>
    <p v-if="detail" class="empty-state-detail">{{ detail }}</p>
    <button v-if="actionLabel" type="button" class="empty-state-action" :data-action="actionId" @click="emit('action')">
      {{ actionLabel }}
    </button>
  </div>
</template>

<style scoped>
.empty-state {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: var(--sp-2);
  padding: var(--sp-6) var(--sp-4);
  text-align: center;
}
.empty-state-icon { width: 16px; height: 16px; color: var(--text-3); }
.empty-state-title { margin: 0; font-size: var(--fs-sm); font-weight: 600; color: var(--text-2); }
.empty-state-detail { margin: 0; font-size: var(--fs-sm); color: var(--text-3); max-width: 480px; }
.empty-state-action {
  display: inline-flex;
  align-items: center;
  height: var(--control-h);
  margin-top: var(--sp-1);
  padding: 0 var(--sp-3);
  border: 1px solid var(--line-2);
  border-radius: var(--r);
  background: var(--panel);
  color: var(--text);
  font-size: var(--fs-sm);
  transition: background var(--dur-fast) var(--ease), border-color var(--dur-fast) var(--ease), box-shadow var(--dur-fast) var(--ease);
}
.empty-state-action:hover { background: var(--hover); }
.empty-state-action:active { background: var(--active); box-shadow: inset 0 1px 2px var(--press); }
.empty-state-action:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
.empty-state-action:disabled { opacity: .55; cursor: not-allowed; }
</style>
