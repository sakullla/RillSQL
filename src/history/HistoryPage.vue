<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { catalogConnections } from "../catalog/store";
import { queryHistory, queryHistoryClear } from "./api";
import { filterHistory, openPlan, type HistoryEntry, type OpenPlan } from "./plan";

const emit = defineEmits<{ open: [plan: OpenPlan] }>();
const entries = ref<HistoryEntry[]>([]);
const loadError = ref("");
const text = ref("");
const connectionId = ref("all");
const status = ref("all");

const knownIds = computed(() => new Set(catalogConnections().map((item) => item.id)));
const visible = computed(() => filterHistory(entries.value, { text: text.value, connectionId: connectionId.value, status: status.value }, knownIds.value));
const connectionChoices = computed(() => {
  const seen = new Map<string, string>();
  for (const entry of entries.value) seen.set(entry.connectionId, entry.connectionName);
  return [...seen.entries()];
});

async function load() {
  try {
    entries.value = await queryHistory();
    loadError.value = "";
  } catch (err) {
    loadError.value = typeof err === "string" ? err : err instanceof Error ? err.message : "读不了查询历史";
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
          <p>历史只保存在本机。载入草稿不会执行，数据库要等你再次执行才会变化。重启不会恢复未执行的草稿。SQL 可能含敏感内容，可以清空。</p>
        </div>
        <button class="btn" type="button" data-action="clear-history" :disabled="!entries.length" @click="clearHistory">清空历史</button>
      </div>
      <div class="hist-filters">
        <input v-model="text" aria-label="搜索历史" placeholder="搜索 SQL 或连接名" />
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
          <option value="unknown">未知</option>
          <option value="running">执行中</option>
        </select>
      </div>
      <p v-if="loadError" role="alert">{{ loadError }}</p>
      <p v-else-if="!visible.length">{{ entries.length ? "没有匹配的历史记录。" : "历史为空。执行过的 SQL 会出现在这里。" }}</p>
      <ul v-else class="hist-list">
        <li v-for="entry in visible" :key="entry.id">
          <div class="hist-meta">
            <strong>{{ entry.connectionName }} / {{ entry.databaseName || "main" }}</strong>
            <span>{{ entry.status }}</span>
            <span>{{ entry.summary }}</span>
            <span v-if="!knownIds.has(entry.connectionId)">原连接已删除</span>
          </div>
          <pre>{{ entry.sql }}</pre>
          <button class="btn" type="button" data-action="open-history" @click="open(entry)">载入草稿</button>
        </li>
      </ul>
    </div>
  </div>
</template>

<style scoped>
.hist-filters { display: flex; gap: 8px; flex-wrap: wrap; margin-bottom: 16px; }
.hist-list { list-style: none; padding: 0; margin: 0; display: flex; flex-direction: column; gap: 12px; }
.hist-list li { border: 1px solid var(--line); border-radius: 8px; padding: 10px 12px; }
.hist-meta { display: flex; gap: 8px; flex-wrap: wrap; font-size: 12.5px; color: var(--text-2); }
.hist-list pre { white-space: pre-wrap; font-family: var(--mono); font-size: 12.5px; margin: 8px 0; }
</style>
