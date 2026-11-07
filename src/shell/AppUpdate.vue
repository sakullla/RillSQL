<script setup lang="ts">
import { computed } from "vue";
import { currentLocale, tr as t } from "../i18n";
import { appUpdater } from "./app-update";
import { PRODUCT_VERSION } from "./product";
import Icon from "./Icon.vue";

const props = defineProps<{ autoCheck: boolean; prepare?: () => void }>();
const emit = defineEmits<{ autoCheck: [enabled: boolean] }>();
const state = appUpdater.state;
const busy = computed(() => ["checking", "downloading", "installing", "restarting"].includes(state.stage));
const percent = computed(() => state.total ? Math.min(100, Math.round(state.downloaded / state.total * 100)) : undefined);
const status = computed(() => !appUpdater.available() ? t("shell.update.desktopOnly")
  : state.stage === "available" ? t("shell.update.available", { version: state.version })
  : t(`shell.update.${state.stage}`));
const mb = (bytes: number) => (bytes / 1048576).toFixed(1);
function prepare() {
  if (!props.prepare) throw new Error(t("shell.update.checkpointFailed"));
  props.prepare();
}
</script>

<template>
  <section id="app-update" class="set-sec app-update">
    <h2><Icon name="refresh" />{{ t("shell.update.title") }}</h2>
    <div class="set-row">
      <div class="set-text">
        <h4>{{ t("shell.update.current", { version: PRODUCT_VERSION }) }}</h4>
        <p role="status" aria-live="polite">{{ status }}</p>
        <p v-if="state.checkedAt" class="update-meta">{{ t("shell.update.checkedAt", { time: new Date(state.checkedAt).toLocaleString(currentLocale()) }) }}</p>
      </div>
      <div class="update-actions">
        <button v-if="state.stage === 'available'" class="btn primary" type="button" @click="appUpdater.install(prepare)">{{ t("shell.update.install") }}</button>
        <button v-else-if="state.stage === 'installed'" class="btn primary" type="button" @click="appUpdater.restart(prepare)">{{ t("shell.update.restart") }}</button>
        <button v-else class="btn" type="button" :disabled="busy || !appUpdater.available()" @click="appUpdater.check()">{{ t("shell.update.check") }}</button>
      </div>
    </div>
    <p v-if="state.error" class="update-error" role="alert">{{ state.error }}</p>
    <div v-if="state.stage === 'downloading'" class="update-progress">
      <progress :value="percent" max="100" :aria-label="t('shell.update.downloading')"></progress>
      <span>{{ state.total ? t("shell.update.progress", { done: mb(state.downloaded), total: mb(state.total) }) : t("shell.update.progressUnknown", { done: mb(state.downloaded) }) }}</span>
    </div>
    <details v-if="state.version && state.notes" class="update-notes">
      <summary>{{ t("shell.update.notes", { version: state.version }) }}</summary>
      <pre>{{ state.notes }}</pre>
    </details>
    <div class="set-row">
      <div class="set-text"><h4>{{ t("shell.update.autoCheck") }}</h4><p>{{ t("shell.update.autoCheckHint") }}</p></div>
      <input type="checkbox" :checked="props.autoCheck" :aria-label="t('shell.update.autoCheck')" @change="emit('autoCheck', ($event.target as HTMLInputElement).checked)" />
    </div>
    <p class="muted sm">{{ t("shell.update.installHint") }}</p>
  </section>
</template>

<style scoped>
.update-actions { display: flex; flex-shrink: 0; gap: var(--sp-2); }
.update-error { color: var(--err); overflow-wrap: anywhere; }
.update-progress { display: flex; align-items: center; gap: var(--sp-3); margin-block: var(--sp-3); }
.update-progress progress { flex: 1; min-width: 0; accent-color: var(--accent); }
.update-notes { margin-block: var(--sp-3); }
.update-notes summary { cursor: pointer; }
.update-notes pre { white-space: pre-wrap; overflow-wrap: anywhere; max-height: 240px; overflow: auto; font: inherit; font-size: var(--fs-sm); }
.update-meta { font-size: var(--fs-xs); }
</style>
