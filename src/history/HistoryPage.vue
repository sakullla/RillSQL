<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
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
const entries = ref<HistoryEntry[]>([]);
const loading = ref(true);
const loadError = ref("");
const text = ref("");
const connectionId = ref("all");
const status = ref("all");

const knownIds = computed(() => new Set(catalogConnections().map((item) => item.id)));
const visible = computed(() => filterHistory(entries.value, { text: text.value, connectionId: connectionId.value, status: status.value }, knownIds.value));
const groups = computed(() => groupHistoryByDay(visible.value));
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
  await queryHistoryClear();
  entries.value = [];
}

function open(entry: HistoryEntry) {
  emit("open", openPlan(entry, knownIds.value));
}

onMounted(() => {
  void load();
});
</script>

<template>
  <div class="page">
    <div class="page-inner">
      <div class="page-head">
        <div>
          <h1>查询历史</h1>
          <p>执行过的 SQL 只记在这台电脑上。点「载入编辑器」只是把 SQL 放回编辑器，不会执行——数据库要等你再次执行才会变化。SQL 可能含有敏感内容，不需要时可以清空。</p>
        </div>
        <button class="btn danger" type="button" data-action="clear-history" :disabled="!entries.length" @click="clearHistory">清空历史</button>
      </div>

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

      <section v-for="group in groups" :key="group.key" class="hist-day">
        <h2>{{ group.label }}</h2>
        <ul class="hist-list">
          <li v-for="entry in group.entries" :key="entry.id" class="hist-card">
            <div class="hist-top">
              <span class="badge" :class="statusBadge(entry.status)">{{ statusLabel(entry.status) }}</span>
              <strong class="hist-target">{{ entry.connectionName }} / {{ entry.databaseName || "main" }}</strong>
              <span v-if="isDeleted(entry)" class="badge warn">原连接已删除</span>
              <span class="grow"></span>
              <time v-if="timeOfDay(entry.startedAt)" class="muted sm">{{ timeOfDay(entry.startedAt) }}</time>
              <span v-if="elapsedLabel(entry.elapsedMs)" class="muted sm">耗时 {{ elapsedLabel(entry.elapsedMs) }}</span>
            </div>
            <pre class="hist-sql">{{ entry.sql }}</pre>
            <div class="hist-foot">
              <span class="muted sm">{{ entry.summary }}</span>
              <span class="grow"></span>
              <span v-if="isDeleted(entry)" class="muted sm">载入后重新选择连接和库，再手动执行</span>
              <button class="btn sm" type="button" data-action="open-history" @click="open(entry)">载入编辑器（不会执行）</button>
            </div>
          </li>
        </ul>
      </section>
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
</style>
