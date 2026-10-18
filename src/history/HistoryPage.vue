<script setup lang="ts">
import { computed, nextTick, onMounted, ref, shallowRef, watch } from "vue";
import { catalogConnections } from "../catalog/store";
import { queryHistory, queryHistoryClear, querySavedDelete, querySavedList, querySavedRename, querySavedSave } from "./api";
import { ContextMenu, type MenuItem } from "../ui";
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

function isDeleted(entry: HistoryEntry): boolean {
  return !knownIds.value.has(entry.connectionId);
}

async function load() {
  loading.value = true;
  try {
    entries.value = await queryHistory();
    loadError.value = "";
  } catch (err) {
    loadError.value = typeof err === "string" ? err : err instanceof Error ? err.message : "读不了查询历史";
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

const rowMenu = ref<{ x: number; y: number; entry: HistoryEntry } | null>(null);
const rowMenuEl = ref<HTMLElement | null>(null);

const rowMenuItems = computed<MenuItem[]>(() => {
  const entry = rowMenu.value?.entry;
  if (!entry) return [];
  return [
    { kind: "item", id: "load", label: "载入编辑器" },
    { kind: "item", id: "copy-sql", label: "复制 SQL" },
    { kind: "item", id: "details", label: "查看执行详情" },
    { kind: "item", id: "filter-conn", label: `只看「${entry.connectionName}」` },
  ];
});

function onCardContextMenu(event: MouseEvent, entry: HistoryEntry) {
  const node = event.target;
  if (!(node instanceof Element)) return;
  const card = node.closest<HTMLElement>(".hist-card");
  if (!card) return;
  // 先聚焦触发行：菜单关闭时 ContextMenu 会把焦点还回来，键盘用户不掉焦点。
  card.setAttribute("tabindex", "-1");
  card.focus();
  rowMenuEl.value = card;
  rowMenu.value = { x: Math.max(8, event.clientX), y: Math.max(8, event.clientY), entry };
}

function onRowMenuSelect(id: string) {
  const entry = rowMenu.value?.entry;
  if (!entry) return;
  if (id === "load") return open(entry);
  if (id === "copy-sql") {
    void navigator.clipboard.writeText(entry.sql).catch(() => undefined);
    return;
  }
  if (id === "details") {
    selectedId.value = entry.id;
    return;
  }
  if (id === "filter-conn") {
    // 下拉选项由历史里出现过的连接生成，当前条目的 connectionId 一定合法。
    connectionId.value = entry.connectionId;
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
    savedError.value = messageOf(err, "读不了保存的查询");
  } finally {
    savedLoading.value = false;
  }
}

async function saveCurrent() {
  saveError.value = "";
  saveMessage.value = "";
  const draft = props.currentDraft;
  if (!draft) {
    saveError.value = "先打开一个查询标签，再保存其中的 SQL。";
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
      ? `已保存「${written.query.name}」。没有执行，也没有写入执行历史。`
      : `已更新「${written.query.name}」。没有执行，也没有写入执行历史。`;
    saveName.value = "";
    await loadSaved();
  } catch (err) {
    saveError.value = messageOf(err, "存不了这条查询");
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
    renameError.value = messageOf(err, "改不了这个名称");
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
    actionError.value = messageOf(err, "删不了保存的查询");
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
          <h1>{{ section === "saved" ? "保存的查询" : "查询历史" }}</h1>
          <p v-if="section === 'saved'">按名称保存当前 SQL。打开只载入 SQL 和还在的执行目标，不会执行，也不会写入执行历史。</p>
          <p v-else>找回执行过的 SQL，检查结果，再回到编辑器继续工作。</p>
          <div class="hist-switch" role="tablist" aria-label="历史与保存的查询">
            <button class="btn sm" :class="{ primary: section === 'runs' }" type="button" role="tab" :aria-selected="section === 'runs'" data-action="show-history" @click="showSection('runs')">执行历史</button>
            <button class="btn sm" :class="{ primary: section === 'saved' }" type="button" role="tab" :aria-selected="section === 'saved'" data-action="show-saved" @click="showSection('saved')">保存的查询</button>
          </div>
        </div>
        <button v-if="section === 'runs'" class="btn danger" type="button" data-action="clear-history" :disabled="!entries.length" @click="confirmingClear = true">清空历史</button>
      </div>

      <template v-if="section === 'runs'">
      <div v-if="confirmingClear" class="hist-clear" role="alert"><span>清空本机的全部查询历史？此操作无法撤销。</span><button class="btn sm" @click="confirmingClear = false">取消</button><button class="btn sm danger" data-action="confirm-clear-history" @click="clearHistory">确认清空</button></div>
      <div class="hist-filters">
        <input v-model="text" type="search" aria-label="搜索历史" placeholder="搜索 SQL 或连接名" />
        <select v-model="connectionId" aria-label="按连接筛选">
          <option value="all">全部连接</option>
          <option value="deleted">原连接已删除</option>
          <option v-for="[id, name] in connectionChoices" :key="id" :value="id">{{ name }}</option>
        </select>
        <select v-model="status" aria-label="按结果筛选">
          <option value="all">全部结果</option>
          <option value="ok">成功</option>
          <option value="error">失败</option>
          <option value="cancelled">已取消</option>
          <option value="unknown">结果未知</option>
          <option value="running">执行中</option>
        </select>
      </div>

      <div v-if="loadError" class="hist-error" role="alert">
        <p>历史没有读出来：{{ loadError }}</p>
        <button class="btn sm" type="button" data-action="retry-history" @click="load">重试</button>
      </div>
      <div v-else-if="loading" class="empty"><p>正在读取历史…</p></div>
      <div v-else-if="!visible.length" class="empty">
        <h3>{{ entries.length ? "没有匹配的历史记录" : "历史为空" }}</h3>
        <p>{{ entries.length ? "换个关键词或筛选条件试试。" : "执行过的 SQL 会出现在这里。" }}</p>
      </div>

      <div v-if="visible.length" class="history-layout"><div class="history-records">
      <div class="history-count">{{ visible.length }} 条记录 <span>仅保存在本机</span></div>
      <section v-for="group in groups" :key="group.key" class="hist-day">
        <h2>{{ group.label }}</h2>
        <ul class="hist-list">
          <li v-for="entry in group.entries" :key="entry.id" class="hist-card" :class="{ selected: selected?.id === entry.id }" @contextmenu.prevent="onCardContextMenu($event, entry)">
            <div class="hist-top">
              <span class="badge" :class="statusBadge(entry.status)">{{ statusLabel(entry.status) }}</span>
              <strong class="hist-target">{{ entry.connectionName }} / {{ entry.databaseName || "main" }}</strong>
              <span v-if="isDeleted(entry)" class="badge warn">原连接已删除</span>
              <span class="grow"></span>
              <time v-if="timeOfDay(entry.startedAt)" class="muted sm">{{ timeOfDay(entry.startedAt) }}</time>
              <span v-if="elapsedLabel(entry.elapsedMs)" class="muted sm">耗时 {{ elapsedLabel(entry.elapsedMs) }}</span>
            </div>
            <button class="hist-preview" :aria-label="`预览 SQL：${entry.sql.slice(0, 80)}`" @click="selectedId = entry.id"><code>{{ entry.sql }}</code></button>
            <div class="hist-foot">
              <span class="muted sm">{{ entry.summary }}</span>
              <span class="grow"></span>
              <span v-if="isDeleted(entry)" class="muted sm">载入后重新选择连接和库</span>
              <button class="btn sm" type="button" data-action="open-history" @click="open(entry)">载入编辑器 ↗</button>
            </div>
          </li>
        </ul>
      </section>
      <footer class="history-pagination"><button class="btn sm" :disabled="page <= 1" @click="page--">上一页</button><span>{{ page }} / {{ pageCount }}</span><button class="btn sm" :disabled="page >= pageCount" @click="page++">下一页</button></footer>
      </div>
      <aside v-if="selected" class="history-inspector"><div class="inspector-head"><span>SQL 预览</span><span class="badge" :class="statusBadge(selected.status)">{{ statusLabel(selected.status) }}</span></div><dl><dt>连接</dt><dd>{{ selected.connectionName }}</dd><dt>数据库</dt><dd>{{ selected.databaseName || 'main' }}</dd><dt>执行耗时</dt><dd>{{ elapsedLabel(selected.elapsedMs) || '—' }}</dd></dl><pre>{{ selected.sql }}</pre><p>{{ selected.summary }}</p><button class="btn primary" @click="open(selected)">载入编辑器</button><small>只载入 SQL，不会执行。</small></aside>
      </div>
      </template>

      <section v-else class="saved-panel" aria-label="保存的查询">
        <form class="saved-form" @submit.prevent="saveCurrent">
          <label>名称<input ref="nameInput" v-model="saveName" type="text" aria-label="保存查询的名称" data-action="saved-name" :disabled="!currentDraft" /></label>
          <p v-if="!currentDraft">先打开一个查询标签，再保存其中的 SQL。</p>
          <template v-else>
            <p>保存当前编辑器中的 SQL<span v-if="currentDraft.connectionName">，并记住 {{ currentDraft.connectionName }} / {{ currentDraft.databaseName || "main" }}</span><span v-else>。还没有执行目标，打开时需要重新选择</span>。保存不会执行。</p>
            <pre class="saved-sql">{{ currentDraft.sql }}</pre>
          </template>
          <p v-if="nameWillUpdate">这个名称已存在，保存会更新它的 SQL，不会执行。</p>
          <p v-if="saveMessage" role="status">{{ saveMessage }}</p>
          <p v-if="saveError" class="saved-alert" role="alert">{{ saveError }}</p>
          <button class="btn primary" type="submit" data-action="save-named-query" :disabled="!currentDraft">保存</button>
        </form>

        <div class="hist-filters">
          <input v-model="savedText" type="search" aria-label="按名称查找" placeholder="按名称查找" />
        </div>
        <p v-if="actionError" class="saved-alert" role="alert">{{ actionError }}</p>
        <div v-if="savedError" class="hist-error" role="alert">
          <p>保存的查询没有读出来：{{ savedError }}</p>
          <button class="btn sm" type="button" data-action="retry-saved" @click="loadSaved">重试</button>
        </div>
        <div v-else-if="savedLoading" class="empty"><p>正在读取保存的查询…</p></div>
        <div v-else-if="!savedVisible.length" class="empty">
          <h3>{{ savedEntries.length ? "没有匹配的名称" : "还没有保存的查询" }}</h3>
          <p>{{ savedEntries.length ? "换个名称试试。" : "为当前 SQL 起一个名称后，它会出现在这里。" }}</p>
        </div>
        <ul v-else class="hist-list saved-list">
          <li v-for="item in savedVisible" :key="item.id" class="hist-card">
            <div class="hist-top">
              <strong>{{ item.name }}</strong>
              <span class="muted sm">{{ item.connectionName ? `${item.connectionName} / ${item.databaseName || "main"}` : "未记录执行目标" }}</span>
              <span v-if="savedNeedsTarget(item, knownIds)" class="badge warn">需要重新选择执行目标</span>
            </div>
            <pre class="saved-sql">{{ item.sql }}</pre>
            <div class="hist-foot">
              <button class="btn sm" type="button" data-action="open-saved" @click="openSavedItem(item)">打开</button>
              <button v-if="renamingId !== item.id" class="btn sm" type="button" data-action="rename-saved" @click="startRename(item)">重命名</button>
              <button v-if="pendingDeleteId !== item.id" class="btn sm" type="button" data-action="delete-saved" @click="pendingDeleteId = item.id">删除</button>
              <span class="muted sm">打开只载入 SQL，不会执行。</span>
            </div>
            <form v-if="renamingId === item.id" class="saved-rename" @submit.prevent="confirmRename(item)">
              <input v-model="renameValue" type="text" :aria-label="`重命名 ${item.name}`" />
              <button class="btn sm" type="button" data-action="cancel-rename-saved" @click="renamingId = null">取消</button>
              <button class="btn sm primary" type="submit" data-action="confirm-rename-saved">确认重命名</button>
              <p v-if="renameError" class="saved-alert" role="alert">{{ renameError }}</p>
            </form>
            <div v-if="pendingDeleteId === item.id" class="hist-clear" role="alert">
              <span>删除「{{ item.name }}」？取消后它仍留在列表里。</span>
              <button class="btn sm" type="button" data-action="cancel-delete-saved" @click="pendingDeleteId = null">取消</button>
              <button class="btn sm danger" type="button" data-action="confirm-delete-saved" @click="confirmDelete(item)">确认删除</button>
            </div>
          </li>
        </ul>
      </section>
    </div>
    <ContextMenu
      :open="Boolean(rowMenu)"
      :x="rowMenu?.x ?? 0"
      :y="rowMenu?.y ?? 0"
      :items="rowMenuItems"
      :label="rowMenu ? `${rowMenu.entry.connectionName} 历史记录的操作` : '操作'"
      :trigger="rowMenuEl"
      @close="rowMenu = null"
      @select="onRowMenuSelect"
    />
  </div>
</template>

<style scoped>
.hist-filters { display: flex; gap: var(--sp-2); flex-wrap: wrap; margin-bottom: var(--sp-5); }
.hist-filters input { flex: 1; min-width: 220px; }
.hist-error { display: flex; align-items: center; gap: var(--sp-3); padding: var(--sp-3) var(--sp-4); border: 1px solid var(--err-line); border-radius: var(--r-md); background: var(--err-soft); color: var(--err); }
.hist-error p { margin: 0; flex: 1; }
.hist-day { margin-bottom: var(--sp-6); }
.hist-day h2 { font-size: var(--fs-sm); color: var(--text-3); font-weight: 600; margin: 0 0 var(--sp-2); }
.hist-list { list-style: none; padding: 0; margin: 0; display: flex; flex-direction: column; gap: var(--sp-3); }
.hist-card { border: 1px solid var(--line); border-radius: var(--r-lg); background: var(--panel); padding: var(--sp-3) var(--sp-4); }
.hist-top { display: flex; align-items: center; gap: var(--sp-2); flex-wrap: wrap; }
.hist-target { font-size: var(--fs-sm); font-weight: 600; }
.hist-sql { white-space: pre-wrap; word-break: break-word; font-family: var(--mono); font-size: var(--fs-sm); line-height: var(--lh); background: var(--sunken); border-radius: var(--r); padding: var(--sp-2) var(--sp-3); margin: var(--sp-2) 0; }
.hist-foot { display: flex; align-items: center; gap: var(--sp-3); flex-wrap: wrap; }
.hist-clear { display: flex; gap: 10px; align-items: center; padding: 12px; margin-bottom: 16px; background: var(--warn-soft); border: 1px solid var(--warn-line); border-radius: var(--r); }
.hist-clear > span { flex: 1; }
.history-layout { display: grid; grid-template-columns: minmax(0, 1fr) minmax(280px, .7fr); gap: 24px; align-items: start; }
.history-count { display: flex; justify-content: space-between; color: var(--text-2); font-size: 11px; padding-bottom: 20px; }
.history-count span { color: var(--text-3); }
.hist-list { gap: 0; }
.hist-card { border-radius: 0; border-bottom: 0; padding: 14px 16px; }
.hist-card:first-child { border-radius: var(--r-lg) var(--r-lg) 0 0; }
.hist-card:last-child { border-bottom: 1px solid var(--line); border-radius: 0 0 var(--r-lg) var(--r-lg); }
.hist-card:only-child { border-radius: var(--r-lg); }
.hist-card.selected { background: var(--accent-soft); box-shadow: inset 3px 0 var(--accent); }
.hist-preview { display: block; text-align: left; width: 100%; margin: 12px 0; color: var(--text); }
.hist-preview code { display: block; font-size: 12px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.hist-foot { gap: 8px; }
.hist-foot .btn { background: transparent; border: 0; color: var(--accent-text); padding: 0; }
.history-inspector { position: sticky; top: 0; background: var(--panel); border: 1px solid var(--line); border-radius: var(--r-lg); padding: 20px; }
.inspector-head { display: flex; justify-content: space-between; font-size: 12px; font-weight: 600; }
.history-inspector dl { display: grid; grid-template-columns: 75px 1fr; gap: 10px; margin: 20px 0; font-size: 11px; }
.history-inspector dt { color: var(--text-3); }
.history-inspector dd { margin: 0; overflow-wrap: anywhere; }
.history-inspector pre { padding: 16px; margin: 0 -20px; background: var(--sunken); color: var(--text); font: 12px/1.9 var(--mono); white-space: pre-wrap; overflow-wrap: anywhere; max-height: 350px; overflow: auto; }
.history-inspector p { font-size: 12px; color: var(--text-2); }
.history-inspector small { display: block; margin-top: 12px; font-size: 11px; color: var(--text-3); }
.history-pagination { display: flex; justify-content: space-between; align-items: center; font-size: 11px; color: var(--text-2); }
.hist-switch { display: flex; gap: var(--sp-2); margin-top: var(--sp-3); }
.saved-form { display: flex; flex-direction: column; align-items: flex-start; gap: var(--sp-3); margin: var(--sp-4) 0; }
.saved-form label { display: flex; flex-direction: column; gap: var(--sp-2); font-size: var(--fs-sm); }
.saved-sql { white-space: pre-wrap; word-break: break-word; font-family: var(--mono); font-size: var(--fs-sm); line-height: var(--lh); background: var(--sunken); border-radius: var(--r); padding: var(--sp-2) var(--sp-3); margin: var(--sp-2) 0; max-width: 100%; }
.saved-alert { margin: 0; color: var(--err); }
.saved-rename { display: flex; gap: var(--sp-2); align-items: center; flex-wrap: wrap; margin-top: var(--sp-2); }
.saved-list { margin-top: var(--sp-3); }
@media (max-width: 900px) { .history-layout { grid-template-columns: 1fr; } .history-inspector { position: static; grid-row: 1; } .history-inspector pre { max-height: 160px; } }
</style>
