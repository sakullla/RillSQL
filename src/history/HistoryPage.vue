<script setup lang="ts">
import { computed, onMounted, ref, shallowRef, watch } from "vue";
import { catalogConnections } from "../catalog/store";
import { queryHistory, queryHistoryClear } from "./api";
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

const emit = defineEmits<{ open: [plan: OpenPlan] }>();
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
const pageSize = 30;
const pageCount = computed(() => Math.max(1, Math.ceil(visible.value.length / pageSize)));
const groups = computed(() => groupHistoryByDay(visible.value.slice((page.value - 1) * pageSize, page.value * pageSize)));
const selected = computed(() => visible.value.find(item => item.id === selectedId.value) ?? visible.value[0] ?? null);
watch([text, connectionId, status], () => { page.value = 1; selectedId.value = null; });
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

onMounted(() => {
  void load();
});
</script>

<template>
  <div class="page history-page">
    <div class="page-inner">
      <div class="page-head">
        <div>
          <div class="eyebrow">QUERY HISTORY</div><h1>查询历史</h1>
          <p>找回执行过的 SQL，检查结果，再回到编辑器继续工作。</p>
        </div>
        <button class="btn danger" type="button" data-action="clear-history" :disabled="!entries.length" @click="confirmingClear = true">清空历史</button>
      </div>

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
          <li v-for="entry in group.entries" :key="entry.id" class="hist-card" :class="{ selected: selected?.id === entry.id }">
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
    </div>
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
@media (max-width: 900px) { .history-layout { grid-template-columns: 1fr; } .history-inspector { position: static; grid-row: 1; } .history-inspector pre { max-height: 160px; } }
</style>
