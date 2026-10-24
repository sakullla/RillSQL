<script setup lang="ts">
import { computed, nextTick, onMounted, ref, shallowRef, watch } from "vue";
import { catalogConnections } from "../catalog/store";
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
  { value: "all", label: "全部连接" },
  { value: "deleted", label: "原连接已删除" },
  ...connectionChoices.value.map(([id, name]) => ({ value: id, label: name })),
]);
const statusOptions: ListboxOption[] = [
  { value: "all", label: "全部结果" },
  { value: "ok", label: "成功" },
  { value: "error", label: "失败" },
  { value: "cancelled", label: "已取消" },
  { value: "unknown", label: "结果未知" },
  { value: "running", label: "执行中" },
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

async function confirmDeleteRun(entry: HistoryEntry) {
  actionError.value = "";
  try {
    await queryHistoryDelete(entry.id);
    pendingDeleteId.value = null;
    await load();
  } catch (err) {
    actionError.value = messageOf(err, "删不了这条查询历史");
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
          <p v-if="section === 'saved'">按名称保存当前 SQL。</p>
          <p v-else>找回执行过的 SQL，检查结果，再回到编辑器继续工作。</p>
          <div class="hist-switch" role="tablist" aria-label="历史与保存的查询">
            <button class="btn sm" type="button" role="tab" :aria-selected="section === 'runs'" data-action="show-history" @click="showSection('runs')">执行历史</button>
            <button class="btn sm" type="button" role="tab" :aria-selected="section === 'saved'" data-action="show-saved" @click="showSection('saved')">保存的查询</button>
          </div>
        </div>
        <button v-if="section === 'runs'" class="btn danger" type="button" data-action="clear-history" :disabled="!entries.length" @click="confirmingClear = true">清空历史</button>
      </div>

      <template v-if="section === 'runs'">
      <div v-if="confirmingClear" class="hist-clear" role="alert"><span>清空本机的全部查询历史？此操作无法撤销。</span><button class="btn sm" @click="confirmingClear = false">取消</button><button class="btn sm danger" data-action="confirm-clear-history" @click="clearHistory">确认清空</button></div>
      <div class="hist-filters">
        <input v-model="text" type="search" aria-label="搜索历史" placeholder="搜索 SQL 或连接名" />
        <Listbox v-model="connectionId" :options="connectionOptions" aria-label="按连接筛选" />
        <Listbox v-model="status" :options="statusOptions" aria-label="按结果筛选" />
        <span class="grow"></span>
      </div>
      <p v-if="actionError" class="saved-alert" role="alert">{{ actionError }}</p>

      <div v-if="loadError" class="hist-error" role="alert">
        <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path d="M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18M15 9l-6 6M9 9l6 6" /></svg>
        <p>历史没有读出来：{{ loadError }}</p>
        <button class="btn sm" type="button" data-action="retry-history" @click="load">重试</button>
      </div>
      <div v-else-if="loading" class="empty"><p>正在读取历史…</p></div>
      <div v-else-if="!visible.length" class="empty">
        <h3>{{ entries.length ? "没有匹配的历史记录" : "历史为空" }}</h3>
        <p>{{ entries.length ? "换个关键词或筛选条件试试。" : "执行过的 SQL 会出现在这里。" }}</p>
      </div>

      <div v-if="visible.length" class="history-layout"><div class="history-records">
      <div class="history-count"><b>{{ visible.length }}</b> 条记录<span>仅保存在本机</span></div>
      <section v-for="group in groups" :key="group.key" class="hist-day">
        <h2>{{ group.label }}</h2>
        <ul class="hist-list">
          <li v-for="entry in group.entries" :key="entry.id" class="hist-card" :class="{ selected: selected?.id === entry.id }">
            <div class="hist-top">
              <span class="badge" :class="statusBadge(entry.status)">{{ statusLabel(entry.status) }}</span>
              <strong class="hist-target">{{ entry.connectionName }} / {{ entry.databaseName || "main" }}</strong>
              <span v-if="isDeleted(entry)" class="badge warn">原连接已删除</span>
              <span class="grow"></span>
              <time v-if="timeOfDay(entry.startedAt)" class="muted sm mono">{{ timeOfDay(entry.startedAt) }}</time>
              <span v-if="elapsedLabel(entry.elapsedMs)" class="muted sm mono">耗时 {{ elapsedLabel(entry.elapsedMs) }}</span>
            </div>
            <button class="hist-preview" :aria-label="`预览 SQL：${entry.sql.slice(0, 80)}`" @click="selectedId = entry.id"><code>{{ entry.sql }}</code></button>
            <div class="hist-foot">
              <span class="muted sm">{{ entry.summary }}</span>
              <span class="grow"></span>
              <span v-if="isDeleted(entry)" class="muted sm">载入后重新选择连接和库</span>
              <button class="btn sm" type="button" data-action="open-history" @click="open(entry)">载入编辑器 ↗</button>
              <button v-if="pendingDeleteId !== entry.id" class="btn sm danger" type="button" data-action="delete-run" @click="pendingDeleteId = entry.id">删除</button>
            </div>
            <div v-if="pendingDeleteId === entry.id" class="hist-clear" role="alert">
              <span>删除这条查询历史？取消后它仍留在列表里。</span>
              <button class="btn sm" type="button" data-action="cancel-delete-run" @click="pendingDeleteId = null">取消</button>
              <button class="btn sm danger" type="button" data-action="confirm-delete-run" @click="confirmDeleteRun(entry)">确认删除</button>
            </div>
          </li>
        </ul>
      </section>
      <footer class="history-pagination"><button class="btn sm" :disabled="page <= 1" @click="page--">上一页</button><span class="mono">{{ page }} / {{ pageCount }}</span><button class="btn sm" :disabled="page >= pageCount" @click="page++">下一页</button></footer>
      </div>
      <aside v-if="selected" class="history-inspector">
        <div class="inspector-head"><span>SQL 预览</span><span class="badge" :class="statusBadge(selected.status)">{{ statusLabel(selected.status) }}</span></div>
        <dl><dt>连接</dt><dd>{{ selected.connectionName }}</dd><dt>数据库</dt><dd>{{ selected.databaseName || 'main' }}</dd><dt>执行耗时</dt><dd class="mono">{{ elapsedLabel(selected.elapsedMs) || '—' }}</dd></dl>
        <pre>{{ selected.sql }}</pre>
        <p>{{ selected.summary }}</p>
        <button class="btn primary" @click="open(selected)">载入编辑器</button>
        <small>只载入 SQL，不会执行。</small>
      </aside>
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
          <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path d="M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18M15 9l-6 6M9 9l6 6" /></svg>
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
              <strong class="hist-name">{{ item.name }}</strong>
              <span class="muted sm">{{ item.connectionName ? `${item.connectionName} / ${item.databaseName || "main"}` : "未记录执行目标" }}</span>
              <span class="grow"></span>
              <span v-if="savedNeedsTarget(item, knownIds)" class="badge warn">需要重新选择执行目标</span>
            </div>
            <pre class="saved-sql">{{ item.sql }}</pre>
            <div class="hist-foot">
              <button class="btn sm" type="button" data-action="open-saved" @click="openSavedItem(item)">打开</button>
              <button v-if="renamingId !== item.id" class="btn sm" type="button" data-action="rename-saved" @click="startRename(item)">重命名</button>
              <button v-if="pendingDeleteId !== item.id" class="btn sm danger" type="button" data-action="delete-saved" @click="pendingDeleteId = item.id">删除</button>
              <span class="grow"></span>
              <span class="muted sm">打开只载入 SQL，不会执行。</span>
            </div>
            <form v-if="renamingId === item.id" class="saved-rename" @submit.prevent="confirmRename(item)">
              <input v-model="renameValue" type="text" :aria-label="`重命名 ${item.name}`" />
              <button class="btn sm" type="button" data-action="cancel-rename-saved" @click="renamingId = null">取消</button>
              <button class="btn sm" type="submit" data-action="confirm-rename-saved">确认重命名</button>
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
