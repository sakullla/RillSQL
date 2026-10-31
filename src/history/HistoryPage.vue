<script setup lang="ts">
import { computed, nextTick, onMounted, ref, shallowRef, watch } from "vue";
import { catalogConnections } from "../catalog/store";
import { tr as t } from "../i18n";
import Listbox from "../ui/Listbox.vue";
import type { ListboxOption } from "../ui/types";
import { queryHistory, queryHistoryClear, queryHistoryDelete, querySavedDelete, querySavedList, querySavedRename, querySavedSave } from "./api";
import {
  elapsedLabel,
  filterHistory,
  groupHistoryByDay,
  openPlan,
  statusBadge,
  statusLabel,
  timeOfDay,
  type HistoryEntry,
  type OpenPlan,
} from "./plan";
import { filterSaved, openSaved, savedName, savedNameError, savedNeedsTarget, type SavedDraft, type SavedQuery } from "./saved";

const props = withDefaults(
  defineProps<{
    section?: "runs" | "saved";
    currentDraft?: SavedDraft | null;
    saveRequest?: number;
  }>(),
  { section: "runs", currentDraft: null, saveRequest: 0 },
);
const emit = defineEmits<{ open: [plan: OpenPlan]; "update:section": [section: "runs" | "saved"] }>();
const entries = shallowRef<HistoryEntry[]>([]);
const loading = ref(true);
const loadError = ref("");
const text = ref("");
const connectionId = ref("all");
const status = ref("all");

const knownIds = computed(() => new Set(catalogConnections().map((item) => item.id)));
const visible = computed(() => filterHistory(entries.value, { text: text.value, connectionId: connectionId.value, status: status.value }, knownIds.value));
const page = ref(1);
const selectedId = ref<string | null>(null);
const confirmingClear = ref(false);
const section = ref<"runs" | "saved">(props.section);
const savedEntries = shallowRef<SavedQuery[]>([]);
const savedLoading = ref(false);
const savedError = ref("");
const savedText = ref("");
const saveName = ref("");
const saveError = ref("");
const saveMessage = ref("");
const actionError = ref("");
const renamingId = ref<string | null>(null);
const renameValue = ref("");
const renameError = ref("");
const pendingDeleteId = ref<string | null>(null);
const nameInput = ref<HTMLInputElement | null>(null);
const pageSize = 30;
const pageCount = computed(() => Math.max(1, Math.ceil(visible.value.length / pageSize)));
const groups = computed(() => groupHistoryByDay(visible.value.slice((page.value - 1) * pageSize, page.value * pageSize)));
const selected = computed(() => visible.value.find(item => item.id === selectedId.value) ?? visible.value[0] ?? null);
const savedVisible = computed(() => filterSaved(savedEntries.value, savedText.value));
const nameWillUpdate = computed(() => {
  const name = savedName(saveName.value);
  return name !== "" && savedEntries.value.some((item) => item.name === name);
});
watch([text, connectionId, status], () => { page.value = 1; selectedId.value = null; });
watch(() => props.section, (value) => {
  if (section.value !== value) showSection(value);
});
watch(() => props.saveRequest, (value, previous) => {
  if (!value || value === previous) return;
  showSection("saved");
  void nextTick(() => nameInput.value?.focus());
});
const connectionChoices = computed(() => {
  const seen = new Map<string, string>();
  for (const entry of entries.value) seen.set(entry.connectionId, entry.connectionName);
  return [...seen.entries()];
});
const connectionOptions = computed<ListboxOption[]>(() => [
  { value: "all", label: t("history.filters.allConnections") },
  { value: "deleted", label: t("history.filters.deletedConnection") },
  ...connectionChoices.value.map(([id, name]) => ({ value: id, label: name })),
]);
const statusOptions: ListboxOption[] = [
  { value: "all", label: t("history.filters.allResults") },
  { value: "ok", label: t("history.status.ok") },
  { value: "error", label: t("history.status.error") },
  { value: "cancelled", label: t("history.status.cancelled") },
  { value: "unknown", label: t("history.status.unknown") },
  { value: "running", label: t("history.status.running") },
];

function isDeleted(entry: HistoryEntry): boolean {
  return !knownIds.value.has(entry.connectionId);
}

async function load() {
  loading.value = true;
  try {
    entries.value = await queryHistory();
    loadError.value = "";
  } catch (err) {
    loadError.value = typeof err === "string" ? err : err instanceof Error ? err.message : t("history.savedPanel.loadFailed");
  } finally {
    loading.value = false;
  }
}

async function clearHistory() {
  try {
    await queryHistoryClear();
    entries.value = [];
    confirmingClear.value = false;
    page.value = 1;
  } catch (err) {
    loadError.value = err instanceof Error ? err.message : String(err);
  }
}

function open(entry: HistoryEntry) {
  emit("open", openPlan(entry, knownIds.value));
}

async function confirmDeleteRun(entry: HistoryEntry) {
  actionError.value = "";
  try {
    await queryHistoryDelete(entry.id);
    pendingDeleteId.value = null;
    await load();
  } catch (err) {
    actionError.value = messageOf(err, t("history.savedPanel.deleteRunFailed"));
  }
}

function messageOf(err: unknown, fallback: string): string {
  if (typeof err === "string" && err) return err;
  if (err instanceof Error && err.message) return err.message;
  return fallback;
}

function showSection(next: "runs" | "saved") {
  section.value = next;
  emit("update:section", next);
  if (next === "saved") void loadSaved();
}

async function loadSaved() {
  savedLoading.value = true;
  try {
    savedEntries.value = await querySavedList();
    savedError.value = "";
    if (pendingDeleteId.value && !savedEntries.value.some((item) => item.id === pendingDeleteId.value)) pendingDeleteId.value = null;
  } catch (err) {
    savedError.value = messageOf(err, t("history.savedPanel.loadFailed"));
  } finally {
    savedLoading.value = false;
  }
}

async function saveCurrent() {
  saveError.value = "";
  saveMessage.value = "";
  const draft = props.currentDraft;
  if (!draft) {
    saveError.value = t("history.savedPanel.saveErrorNeedTab");
    return;
  }
  const nameError = savedNameError(saveName.value);
  if (nameError) {
    saveError.value = nameError;
    return;
  }
  const name = savedName(saveName.value);
  try {
    const written = await querySavedSave({
      name,
      sql: draft.sql,
      connectionId: draft.connectionId,
      connectionName: draft.connectionName,
      databaseName: draft.databaseName,
      engine: draft.engine,
    });
    saveMessage.value = written.created
      ? t("history.savedPanel.savedDone", { name: written.query.name })
      : t("history.savedPanel.savedUpdated", { name: written.query.name });
    saveName.value = "";
    await loadSaved();
  } catch (err) {
    saveError.value = messageOf(err, t("history.savedPanel.saveFailed"));
  }
}

function startRename(entry: SavedQuery) {
  renamingId.value = entry.id;
  renameValue.value = entry.name;
  renameError.value = "";
  pendingDeleteId.value = null;
}

async function confirmRename(entry: SavedQuery) {
  const nameError = savedNameError(renameValue.value);
  if (nameError) {
    renameError.value = nameError;
    return;
  }
  renameError.value = "";
  try {
    await querySavedRename(entry.id, savedName(renameValue.value));
    renamingId.value = null;
    await loadSaved();
  } catch (err) {
    renameError.value = messageOf(err, t("history.savedPanel.renameFailed"));
  }
}

async function confirmDelete(entry: SavedQuery) {
  actionError.value = "";
  try {
    await querySavedDelete(entry.id);
    pendingDeleteId.value = null;
    renamingId.value = null;
    await loadSaved();
  } catch (err) {
    actionError.value = messageOf(err, t("history.savedPanel.deleteFailed"));
  }
}

function openSavedItem(entry: SavedQuery) {
  emit("open", openSaved(entry, knownIds.value));
}

onMounted(() => {
  void load();
  if (section.value === "saved") void loadSaved();
});
</script>

<template>
  <div class="page history-page">
    <div class="page-inner">
      <div class="page-head">
        <div>
          <h1>{{ section === "saved" ? t("history.page.titleSaved") : t("history.page.titleRuns") }}</h1>
          <p v-if="section === 'saved'">{{ t("history.page.subtitleSaved") }}</p>
          <p v-else>{{ t("history.page.subtitleRuns") }}</p>
          <div class="hist-switch" role="tablist" :aria-label="t('history.page.tabsLabel')">
            <button class="btn sm" type="button" role="tab" :aria-selected="section === 'runs'" data-action="show-history" @click="showSection('runs')">{{ t("history.page.runs") }}</button>
            <button class="btn sm" type="button" role="tab" :aria-selected="section === 'saved'" data-action="show-saved" @click="showSection('saved')">{{ t("history.page.saved") }}</button>
          </div>
        </div>
        <button v-if="section === 'runs'" class="btn danger" type="button" data-action="clear-history" :disabled="!entries.length" @click="confirmingClear = true">{{ t("history.page.clear") }}</button>
      </div>

      <template v-if="section === 'runs'">
      <div v-if="confirmingClear" class="hist-clear" role="alert"><span>{{ t("history.page.clearConfirm") }}</span><button class="btn sm" @click="confirmingClear = false">{{ t("common.cancel") }}</button><button class="btn sm danger" data-action="confirm-clear-history" @click="clearHistory">{{ t("history.page.confirmClear") }}</button></div>
      <div class="hist-filters">
        <input v-model="text" type="search" :aria-label="t('history.page.searchLabel')" :placeholder="t('history.page.searchPlaceholder')" />
        <Listbox v-model="connectionId" :options="connectionOptions" :aria-label="t('history.page.filterConnection')" />
        <Listbox v-model="status" :options="statusOptions" :aria-label="t('history.page.filterStatus')" />
        <span class="grow"></span>
      </div>
      <p v-if="actionError" class="saved-alert" role="alert">{{ actionError }}</p>

      <div v-if="loadError" class="hist-error" role="alert">
        <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path d="M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18M15 9l-6 6M9 9l6 6" /></svg>
        <p>{{ t("history.page.loadFailed", { error: loadError }) }}</p>
        <button class="btn sm" type="button" data-action="retry-history" @click="load">{{ t("history.page.retry") }}</button>
      </div>
      <div v-else-if="loading" class="empty"><p>{{ t("history.page.loading") }}</p></div>
      <div v-else-if="!visible.length" class="empty">
        <h3>{{ entries.length ? t("history.page.noMatchTitle") : t("history.page.emptyTitle") }}</h3>
        <p>{{ entries.length ? t("history.page.noMatchHint") : t("history.page.emptyHint") }}</p>
      </div>

      <div v-if="visible.length" class="history-layout"><div class="history-records">
      <div class="history-count"><b>{{ visible.length }}</b> {{ t("history.page.records", { n: visible.length }) }}<span>{{ t("history.page.localOnly") }}</span></div>
      <section v-for="group in groups" :key="group.key" class="hist-day">
        <h2>{{ group.label }}</h2>
        <ul class="hist-list">
          <li v-for="entry in group.entries" :key="entry.id" class="hist-card" :class="{ selected: selected?.id === entry.id }">
            <div class="hist-top">
              <span class="badge" :class="statusBadge(entry.status)">{{ statusLabel(entry.status) }}</span>
              <strong class="hist-target">{{ entry.connectionName }} / {{ entry.databaseName || "main" }}</strong>
              <span v-if="isDeleted(entry)" class="badge warn">{{ t("history.page.connectionDeleted") }}</span>
              <span class="grow"></span>
              <time v-if="timeOfDay(entry.startedAt)" class="muted sm mono">{{ timeOfDay(entry.startedAt) }}</time>
              <span v-if="elapsedLabel(entry.elapsedMs)" class="muted sm mono">{{ t("history.page.elapsed", { value: elapsedLabel(entry.elapsedMs) }) }}</span>
            </div>
            <button class="hist-preview" :aria-label="t('history.page.previewSql', { sql: entry.sql.slice(0, 80) })" @click="selectedId = entry.id"><code>{{ entry.sql }}</code></button>
            <div class="hist-foot">
              <span class="muted sm">{{ entry.summary }}</span>
              <span class="grow"></span>
              <span v-if="isDeleted(entry)" class="muted sm">{{ t("history.page.reloadNeeded") }}</span>
              <button class="btn sm" type="button" data-action="open-history" @click="open(entry)">{{ t("history.page.loadToEditor") }}</button>
              <button v-if="pendingDeleteId !== entry.id" class="btn sm danger" type="button" data-action="delete-run" @click="pendingDeleteId = entry.id">{{ t("common.remove") }}</button>
            </div>
            <div v-if="pendingDeleteId === entry.id" class="hist-clear" role="alert">
              <span>{{ t("history.page.deleteConfirm") }}</span>
              <button class="btn sm" type="button" data-action="cancel-delete-run" @click="pendingDeleteId = null">{{ t("common.cancel") }}</button>
              <button class="btn sm danger" type="button" data-action="confirm-delete-run" @click="confirmDeleteRun(entry)">{{ t("history.page.confirmDelete") }}</button>
            </div>
          </li>
        </ul>
      </section>
      <footer class="history-pagination"><button class="btn sm" :disabled="page <= 1" @click="page--">{{ t("history.page.prevPage") }}</button><span class="mono">{{ page }} / {{ pageCount }}</span><button class="btn sm" :disabled="page >= pageCount" @click="page++">{{ t("history.page.nextPage") }}</button></footer>
      </div>
      <aside v-if="selected" class="history-inspector">
        <div class="inspector-head"><span>{{ t("history.page.sqlPreview") }}</span><span class="badge" :class="statusBadge(selected.status)">{{ statusLabel(selected.status) }}</span></div>
        <dl><dt>{{ t("history.page.fieldConnection") }}</dt><dd>{{ selected.connectionName }}</dd><dt>{{ t("history.page.fieldDatabase") }}</dt><dd>{{ selected.databaseName || 'main' }}</dd><dt>{{ t("history.page.fieldElapsed") }}</dt><dd class="mono">{{ elapsedLabel(selected.elapsedMs) || '—' }}</dd></dl>
        <pre>{{ selected.sql }}</pre>
        <p>{{ selected.summary }}</p>
        <button class="btn primary" @click="open(selected)">{{ t("history.page.loadToEditorShort") }}</button>
        <small>{{ t("history.page.loadOnlySql") }}</small>
      </aside>
      </div>
      </template>

      <section v-else class="saved-panel" :aria-label="t('history.savedPanel.label')">
        <form class="saved-form" @submit.prevent="saveCurrent">
          <label>{{ t("history.savedPanel.nameLabel") }}<input ref="nameInput" v-model="saveName" type="text" :aria-label="t('history.savedPanel.nameLabel')" data-action="saved-name" :disabled="!currentDraft" /></label>
          <p v-if="!currentDraft">{{ t("history.savedPanel.needTab") }}</p>
          <template v-else>
            <p>{{ t("history.savedPanel.saveCurrent") }}<span v-if="currentDraft.connectionName">{{ t("history.savedPanel.rememberTarget", { conn: currentDraft.connectionName, db: currentDraft.databaseName || "main" }) }}</span><span v-else>{{ t("history.savedPanel.noTargetNote") }}</span>{{ t("history.savedPanel.saveNoExec") }}</p>
            <pre class="saved-sql">{{ currentDraft.sql }}</pre>
          </template>
          <p v-if="nameWillUpdate">{{ t("history.savedPanel.nameExists") }}</p>
          <p v-if="saveMessage" role="status">{{ saveMessage }}</p>
          <p v-if="saveError" class="saved-alert" role="alert">{{ saveError }}</p>
          <button class="btn primary" type="submit" data-action="save-named-query" :disabled="!currentDraft">{{ t("history.savedPanel.save") }}</button>
        </form>

        <div class="hist-filters">
          <input v-model="savedText" type="search" :aria-label="t('history.savedPanel.searchLabel')" :placeholder="t('history.savedPanel.searchLabel')" />
        </div>
        <p v-if="actionError" class="saved-alert" role="alert">{{ actionError }}</p>
        <div v-if="savedError" class="hist-error" role="alert">
          <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path d="M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18M15 9l-6 6M9 9l6 6" /></svg>
          <p>{{ t("history.savedPanel.loadFailedText", { error: savedError }) }}</p>
          <button class="btn sm" type="button" data-action="retry-saved" @click="loadSaved">{{ t("history.page.retry") }}</button>
        </div>
        <div v-else-if="savedLoading" class="empty"><p>{{ t("history.savedPanel.loading") }}</p></div>
        <div v-else-if="!savedVisible.length" class="empty">
          <h3>{{ savedEntries.length ? t("history.savedPanel.noMatchTitle") : t("history.savedPanel.emptyTitle") }}</h3>
          <p>{{ savedEntries.length ? t("history.savedPanel.noMatchHint") : t("history.savedPanel.emptyHint") }}</p>
        </div>
        <ul v-else class="hist-list saved-list">
          <li v-for="item in savedVisible" :key="item.id" class="hist-card">
            <div class="hist-top">
              <strong class="hist-name">{{ item.name }}</strong>
              <span class="muted sm">{{ item.connectionName ? `${item.connectionName} / ${item.databaseName || "main"}` : t("history.savedPanel.noTarget") }}</span>
              <span class="grow"></span>
              <span v-if="savedNeedsTarget(item, knownIds)" class="badge warn">{{ t("history.savedPanel.needsTarget") }}</span>
            </div>
            <pre class="saved-sql">{{ item.sql }}</pre>
            <div class="hist-foot">
              <button class="btn sm" type="button" data-action="open-saved" @click="openSavedItem(item)">{{ t("history.savedPanel.open") }}</button>
              <button v-if="renamingId !== item.id" class="btn sm" type="button" data-action="rename-saved" @click="startRename(item)">{{ t("history.savedPanel.rename") }}</button>
              <button v-if="pendingDeleteId !== item.id" class="btn sm danger" type="button" data-action="delete-saved" @click="pendingDeleteId = item.id">{{ t("common.remove") }}</button>
              <span class="grow"></span>
              <span class="muted sm">{{ t("history.savedPanel.openNote") }}</span>
            </div>
            <form v-if="renamingId === item.id" class="saved-rename" @submit.prevent="confirmRename(item)">
              <input v-model="renameValue" type="text" :aria-label="t('history.savedPanel.renameLabel', { name: item.name })" />
              <button class="btn sm" type="button" data-action="cancel-rename-saved" @click="renamingId = null">{{ t("common.cancel") }}</button>
              <button class="btn sm" type="submit" data-action="confirm-rename-saved">{{ t("history.savedPanel.confirmRename") }}</button>
              <p v-if="renameError" class="saved-alert" role="alert">{{ renameError }}</p>
            </form>
            <div v-if="pendingDeleteId === item.id" class="hist-clear" role="alert">
              <span>{{ t("history.savedPanel.deleteConfirm", { name: item.name }) }}</span>
              <button class="btn sm" type="button" data-action="cancel-delete-saved" @click="pendingDeleteId = null">{{ t("common.cancel") }}</button>
              <button class="btn sm danger" type="button" data-action="confirm-delete-saved" @click="confirmDelete(item)">{{ t("history.page.confirmDelete") }}</button>
            </div>
          </li>
        </ul>
      </section>
    </div>
  </div>
</template>

<style scoped>
/* 历史页视觉：颜色 / 间距 / 字号 / 动效一律引用 tokens.css 令牌，控件基样式来自 base.css，
   页面骨架直接消费 workbench.css 的 .page / .page-inner / .page-head 公共类。
   表面语言与已完成页对齐：surface-panel + --stroke 描边 + --shadow-sm，行内分隔用 --line，
   SQL 一律下沉井 + mono，状态色走 badge 的 --ok/--err/--warn/--info 语义。 */

/* ---------- 区块切换：下沉井轨道上的分段控件 ---------- */
.hist-switch {
  display: inline-flex;
  padding: 2px;
  gap: 2px;
  margin-top: var(--sp-4);
  background: var(--surface-sunken);
  border: 1px solid var(--stroke);
  border-radius: var(--r-md);
  box-shadow: inset 0 1px 2px color-mix(in srgb, var(--press) 30%, transparent);
}
.hist-switch .btn { background: transparent; border-color: transparent; box-shadow: none; color: var(--text-2); }
.hist-switch .btn:hover:not(:disabled) { background: var(--hover); border-color: transparent; color: var(--text); }
.hist-switch button.btn[aria-selected="true"] { background: var(--sel-strong); color: var(--text); font-weight: 600; }

/* ---------- 筛选工具条 ---------- */
.hist-filters { display: flex; align-items: center; gap: var(--sp-2); flex-wrap: wrap; margin-bottom: var(--sp-4); }
.hist-filters input { flex: 1; min-width: 220px; max-width: 420px; }

/* ---------- 错误与确认条 ---------- */
/* 读取失败卡：与全局 err 模式一致（软底卡 + 同色 3px 左缘 + 语义图标） */
.hist-error { display: flex; align-items: center; gap: var(--sp-3); padding: var(--sp-3) var(--sp-4); border: 1px solid var(--err-line); border-radius: var(--r-md); background: var(--err-soft); color: var(--err); box-shadow: inset 3px 0 0 var(--err); font-size: var(--fs-sm); }
.hist-error .i { width: 14px; height: 14px; flex-shrink: 0; }
.hist-error p { margin: 0; flex: 1; }
.hist-clear { display: flex; gap: var(--sp-2); align-items: center; flex-wrap: wrap; padding: var(--sp-2) var(--sp-3); margin-top: var(--sp-2); background: var(--warn-soft); border: 1px solid var(--warn-line); border-radius: var(--r-md); font-size: var(--fs-sm); }
.hist-clear > span { flex: 1; min-width: 200px; }
.page-inner > .hist-clear { margin: 0 0 var(--sp-4); }
.saved-alert { margin: 0 0 var(--sp-2); font-size: var(--fs-sm); color: var(--err); }

/* ---------- 执行历史：列表 + 检查器双栏 ---------- */
.history-layout { display: grid; grid-template-columns: minmax(0, 1fr) minmax(280px, 340px); gap: var(--sp-5); align-items: start; }
.history-records { min-width: 0; border: 1px solid var(--stroke); border-radius: var(--r-lg); background: var(--surface-panel); box-shadow: var(--shadow-sm); }
.history-count { display: flex; align-items: baseline; gap: var(--sp-1); padding: var(--sp-2) var(--sp-4); font-size: var(--fs-xs); color: var(--text-2); background: var(--panel-2); border-bottom: 1px solid var(--line); border-radius: var(--r-lg) var(--r-lg) 0 0; }
.history-count b { font: 600 var(--fs-sm) var(--mono); font-variant-numeric: tabular-nums; color: var(--text); }
.history-count span { margin-left: auto; color: var(--text-3); }
.hist-day h2 { margin: 0; padding: var(--sp-2) var(--sp-4); font-size: var(--fs-xs); font-weight: 600; letter-spacing: .04em; color: var(--text-3); border-bottom: 1px solid var(--line); background: color-mix(in srgb, var(--panel-2) 55%, var(--surface-panel)); }
.hist-list { list-style: none; padding: 0; margin: 0; }
.hist-card { position: relative; padding: var(--sp-3) var(--sp-4); border-bottom: 1px solid var(--line); transition: background var(--dur-fast) var(--ease), box-shadow var(--dur-fast) var(--ease); }
.hist-list > li:last-child { border-bottom: 0; }
.hist-card:hover { background: var(--hover); }
.hist-card.selected, .hist-card.selected:hover { background: var(--sel); }
.hist-card.selected, .hist-card.selected:hover { box-shadow: inset 2px 0 0 var(--accent); }
.hist-top { display: flex; align-items: center; gap: var(--sp-2); flex-wrap: wrap; }
.hist-target { font-size: var(--fs-sm); font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.hist-name { font-size: var(--fs-md); font-weight: 600; }
.hist-top .mono { font-variant-numeric: tabular-nums; }
.hist-preview { display: block; text-align: left; width: 100%; margin: var(--sp-2) 0; padding: var(--sp-2) var(--sp-3); border-radius: var(--r); background: var(--surface-sunken); box-shadow: inset 0 0 0 1px var(--stroke); color: var(--text); transition: box-shadow var(--dur-fast) var(--ease); }
.hist-preview:hover { box-shadow: inset 0 0 0 1px var(--accent); }
.hist-preview code { display: block; font-size: var(--fs-sm); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.hist-foot { display: flex; align-items: center; gap: var(--sp-1); flex-wrap: wrap; }
.hist-foot .btn { height: auto; padding: 0 var(--sp-1); background: transparent; border-color: transparent; box-shadow: none; color: var(--text-2); }
.hist-foot .btn:hover:not(:disabled) { background: transparent; border-color: transparent; color: var(--accent-text); }
.hist-foot .btn.danger { color: var(--err); }
.hist-foot .btn.danger:hover:not(:disabled) { background: transparent; border-color: transparent; color: var(--err); }
.history-pagination { display: flex; justify-content: space-between; align-items: center; gap: var(--sp-3); padding: var(--sp-2) var(--sp-4); font-size: var(--fs-xs); color: var(--text-2); background: var(--panel-2); border-top: 1px solid var(--line); border-radius: 0 0 var(--r-lg) var(--r-lg); }
.history-pagination .mono { font-variant-numeric: tabular-nums; }

/* ---------- 检查器 ---------- */
.history-inspector { position: sticky; top: 0; display: flex; flex-direction: column; min-width: 0; padding: var(--sp-4); background: var(--surface-panel); border: 1px solid var(--stroke); border-radius: var(--r-lg); box-shadow: var(--shadow-sm); }
.inspector-head { display: flex; justify-content: space-between; align-items: center; gap: var(--sp-2); font-size: var(--fs-sm); font-weight: 600; padding-bottom: var(--sp-3); border-bottom: 1px solid var(--line); }
.history-inspector dl { display: grid; grid-template-columns: 64px 1fr; gap: var(--sp-1) var(--sp-3); margin: var(--sp-3) 0; font-size: var(--fs-xs); }
.history-inspector dt { color: var(--text-3); }
.history-inspector dd { margin: 0; overflow-wrap: anywhere; }
.history-inspector dd.mono { font-variant-numeric: tabular-nums; }
.history-inspector pre { margin: 0; padding: var(--sp-3); background: var(--surface-sunken); box-shadow: inset 0 0 0 1px var(--stroke); border-radius: var(--r-md); color: var(--text); font: var(--fs-sm)/var(--lh) var(--mono); white-space: pre-wrap; overflow-wrap: anywhere; max-height: 320px; overflow: auto; }
.history-inspector p { margin: var(--sp-3) 0 0; font-size: var(--fs-sm); color: var(--text-2); line-height: var(--lh); }
.history-inspector .btn.primary { margin-top: var(--sp-4); width: 100%; }
.history-inspector small { display: block; margin-top: var(--sp-2); font-size: var(--fs-xs); color: var(--text-3); text-align: center; }

/* ---------- 保存的查询 ---------- */
.saved-form { display: flex; flex-direction: column; align-items: flex-start; gap: var(--sp-3); margin: 0 0 var(--sp-5); padding: var(--sp-4); border: 1px solid var(--stroke); border-radius: var(--r-lg); background: var(--surface-panel); box-shadow: var(--shadow-sm); }
.saved-form > label { display: flex; flex-direction: column; gap: var(--sp-1); width: min(420px, 100%); font-size: var(--fs-sm); font-weight: 500; }
.saved-form > label input { width: 100%; }
.saved-form p { margin: 0; font-size: var(--fs-xs); color: var(--text-2); line-height: var(--lh); }
.saved-form > p[role="status"] { color: var(--ok); }
.saved-sql { width: 100%; max-width: 100%; max-height: 200px; overflow: auto; white-space: pre-wrap; word-break: break-word; font: var(--fs-sm)/var(--lh) var(--mono); background: var(--surface-sunken); box-shadow: inset 0 0 0 1px var(--stroke); border-radius: var(--r-md); padding: var(--sp-3); margin: 0; color: var(--text); }
.saved-rename { display: flex; gap: var(--sp-2); align-items: center; flex-wrap: wrap; margin-top: var(--sp-2); }
.saved-rename input { flex: 1; min-width: 200px; }
.saved-rename .saved-alert { width: 100%; margin: 0; }
.saved-list { margin-top: var(--sp-3); border: 1px solid var(--stroke); border-radius: var(--r-lg); background: var(--surface-panel); box-shadow: var(--shadow-sm); overflow: hidden; }
.saved-list .hist-card { border-bottom: 0; }
.saved-list .hist-card + .hist-card { border-top: 1px solid var(--line); }

@media (max-width: 900px) {
  .history-layout { grid-template-columns: 1fr; }
  .history-inspector { position: static; grid-row: 1; }
  .history-inspector pre { max-height: 160px; }
}
</style>
