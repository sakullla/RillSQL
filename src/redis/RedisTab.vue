<script setup lang="ts">
import { computed, onMounted, reactive, ref } from "vue";
import Icon from "../shell/Icon.vue";
import RedisValueView from "../values/RedisValueView.vue";
import { failureOf, tauriRedisApi, type RedisApi, type RedisKeyDetail, type RedisKeyInfo, type RespReply } from "./api";
import { classifyRedisCommand, dangerPrompt, splitCommandline } from "./danger";
import { describeTtl, keyTypeBadge, mergeScanPage, parseTtlInput } from "./keys";

const props = defineProps<{
  tabId: string;
  connectionId: string;
  connectionName: string;
  database: string;
  status: string;
  initialKey?: string;
  api?: RedisApi;
}>();

const api = computed(() => props.api ?? tauriRedisApi);

const pattern = ref("");
const keys = ref<RedisKeyInfo[]>([]);
const cursor = ref(0);
const done = ref(false);
const loading = ref(false);
const listError = ref("");

const selectedKey = ref("");
const detail = ref<RedisKeyDetail | null>(null);
const detailLoading = ref(false);
const detailError = ref("");

const ttlInput = ref("");
const ttlNote = ref("");
const ttlError = ref("");

const renameTo = ref("");
const renameOpen = ref(false);

const commandInput = ref("");
const commandNote = ref("");
const commandError = ref("");
const commandResult = ref<{ command: string; reply: RespReply } | null>(null);

/** 待确认操作：危险命令 / 删除键 / 重命名。取消不发任何 IPC。 */
const confirmState = reactive<{
  open: boolean;
  title: string;
  prompt: string;
  action: "exec" | "delete" | "rename" | null;
}>({ open: false, title: "", prompt: "", action: null });

function resetScan() {
  keys.value = [];
  cursor.value = 0;
  done.value = false;
}

async function loadPage() {
  if (loading.value || done.value) return;
  loading.value = true;
  listError.value = "";
  try {
    const trimmed = pattern.value.trim();
    const page = await api.value.scanKeys(props.connectionId, cursor.value, trimmed ? trimmed : null);
    const merged = mergeScanPage(keys.value, page);
    keys.value = merged.keys;
    cursor.value = merged.cursor;
    done.value = merged.done;
  } catch (error) {
    listError.value = failureOf(error);
  } finally {
    loading.value = false;
  }
}

/** 模式变化：游标从头开始（MATCH 改变后旧游标无意义）。 */
async function applyPattern() {
  resetScan();
  await loadPage();
}

async function selectKey(name: string) {
  selectedKey.value = name;
  detail.value = null;
  detailError.value = "";
  ttlNote.value = "";
  ttlError.value = "";
  if (!name) return;
  detailLoading.value = true;
  try {
    detail.value = await api.value.readKey(props.connectionId, name);
    ttlInput.value = detail.value.ttlMs != null && detail.value.ttlMs >= 0 ? String(Math.floor(detail.value.ttlMs / 1000)) : "";
  } catch (error) {
    detailError.value = failureOf(error);
  } finally {
    detailLoading.value = false;
  }
}

async function refreshSelected() {
  if (selectedKey.value) await selectKey(selectedKey.value);
}

async function refreshList() {
  resetScan();
  await loadPage();
}

async function saveTtl() {
  if (!selectedKey.value) return;
  const parsed = parseTtlInput(ttlInput.value);
  ttlError.value = "";
  ttlNote.value = "";
  if ("error" in parsed) {
    ttlError.value = parsed.error;
    return;
  }
  try {
    const outcome = await api.value.setTtl(props.connectionId, selectedKey.value, parsed.seconds);
    if (outcome.applied === 0) {
      ttlError.value = "没有作用到键（键可能已不存在）。";
      return;
    }
    ttlNote.value = parsed.seconds == null ? "已移除过期（PERSIST）。" : `已设置 ${parsed.seconds} 秒过期（EXPIRE）。`;
    await refreshSelected();
    await refreshList();
  } catch (error) {
    ttlError.value = failureOf(error);
  }
}

function askDelete() {
  if (!selectedKey.value) return;
  confirmState.open = true;
  confirmState.action = "delete";
  confirmState.title = "删除键";
  confirmState.prompt = `将删除键 ${selectedKey.value}，删除后不可恢复。确认删除？`;
}

function askRename() {
  if (!selectedKey.value) return;
  renameTo.value = "";
  renameOpen.value = true;
}

function confirmRename() {
  const target = renameTo.value.trim();
  if (!target) return;
  confirmState.open = true;
  confirmState.action = "rename";
  confirmState.title = "重命名键";
  confirmState.prompt = `将把键 ${selectedKey.value} 重命名为 ${target}。目标键已存在时会被覆盖。确认执行？`;
  renameOpen.value = false;
}

function cancelConfirm() {
  confirmState.open = false;
  confirmState.action = null;
}

async function runConfirm() {
  const action = confirmState.action;
  confirmState.open = false;
  confirmState.action = null;
  try {
    if (action === "delete" && selectedKey.value) {
      const outcome = await api.value.deleteKeys(props.connectionId, [selectedKey.value], true);
      commandNote.value = `已删除 ${outcome.deleted} 个键。`;
      const removed = selectedKey.value;
      selectedKey.value = "";
      detail.value = null;
      keys.value = keys.value.filter((item) => item.name !== removed);
      await refreshList();
    } else if (action === "rename" && selectedKey.value) {
      const target = renameTo.value.trim();
      await api.value.renameKey(props.connectionId, selectedKey.value, target, true);
      commandNote.value = `已重命名为 ${target}。`;
      renameTo.value = "";
      await refreshList();
      await selectKey(target);
    } else if (action === "exec") {
      await execConfirmed();
    }
  } catch (error) {
    commandError.value = failureOf(error);
  }
}

async function execConfirmed() {
  const { command, args } = splitCommandline(commandInput.value);
  const outcome = await api.value.exec(props.connectionId, command, args, true);
  commandResult.value = outcome;
  commandNote.value = "已执行。";
  await refreshList();
  if (selectedKey.value) await refreshSelected();
}

/** 命令行入口：只读直通；写删类先确认；KEYS 无条件拒绝。 */
async function runCommand() {
  commandError.value = "";
  commandNote.value = "";
  const line = commandInput.value.trim();
  if (!line) return;
  const risk = classifyRedisCommand(line);
  if (risk === "blocked") {
    commandError.value = "已禁用 KEYS：全量扫描会阻塞 Redis，键空间浏览请用左侧的模式过滤（SCAN）。";
    return;
  }
  if (risk === "dangerous") {
    confirmState.open = true;
    confirmState.action = "exec";
    confirmState.title = "写删类命令确认";
    confirmState.prompt = dangerPrompt(line);
    return;
  }
  try {
    const { command, args } = splitCommandline(line);
    const outcome = await api.value.exec(props.connectionId, command, args, false);
    commandResult.value = outcome;
  } catch (error) {
    commandError.value = failureOf(error);
  }
}

const ttlText = computed(() => (detail.value ? describeTtl(detail.value.ttlMs) : "—"));

onMounted(async () => {
  await loadPage();
  const initial = props.initialKey?.trim();
  if (initial && keys.value.some((item) => item.name === initial)) {
    await selectKey(initial);
  } else if (initial) {
    await selectKey(initial);
  }
});
</script>

<!-- Redis 键浏览器工作区（R11）：左键列表（SCAN 分页/模式过滤）· 右值查看 + TTL/删除/重命名 · 底部命令行。 -->
<template>
  <div class="redis-tab" :data-connection="connectionId">
    <aside class="rk-list">
      <form class="rk-filter" @submit.prevent="applyPattern">
        <input
          v-model="pattern"
          type="search"
          class="mono"
          data-field="redis-pattern"
          placeholder="模式过滤（如 rillsql:*）"
          aria-label="键模式过滤"
        />
        <button class="btn sm" type="submit" data-action="redis-apply-pattern" :disabled="loading">过滤</button>
        <button class="btn sm ghost" type="button" data-action="redis-refresh" :disabled="loading" @click="refreshList">刷新</button>
      </form>
      <p v-if="listError" class="rk-error" role="alert" data-redis-list-error>{{ listError }}</p>
      <ul class="rk-keys" data-redis-keys>
        <li v-for="item in keys" :key="item.name">
          <button
            type="button"
            class="rk-key"
            :class="{ active: item.name === selectedKey }"
            :data-key="item.name"
            :data-type="keyTypeBadge(item.keyType)"
            @dblclick="selectKey(item.name)"
            @click="selectKey(item.name)"
          >
            <span class="rk-badge" :data-type="keyTypeBadge(item.keyType)">{{ keyTypeBadge(item.keyType) }}</span>
            <span class="rk-name mono">{{ item.name }}</span>
            <span class="rk-ttl">{{ describeTtl(item.ttlMs) }}</span>
          </button>
        </li>
      </ul>
      <p v-if="!loading && keys.length === 0 && !listError" class="rk-empty">没有读到键。调整模式过滤或先写入一些键。</p>
      <div class="rk-more">
        <button v-if="!done" class="btn sm" type="button" data-action="redis-more" :disabled="loading" @click="loadPage">
          {{ loading ? "读取中…" : "再读取一批（SCAN）" }}
        </button>
        <span v-else class="rk-done">本轮流读完成（键空间可能有变化，刷新重新开始）</span>
      </div>
    </aside>

    <section class="rk-detail">
      <template v-if="selectedKey">
        <header class="rk-head">
          <h3 class="mono" data-redis-key-title>{{ selectedKey }}</h3>
          <span v-if="detail" class="rk-badge lg" :data-type="keyTypeBadge(detail.keyType)">{{ keyTypeBadge(detail.keyType) }}</span>
          <span class="rk-ttl-now">TTL：{{ ttlText }}</span>
        </header>
        <p v-if="detailError" class="rk-error" role="alert">{{ detailError }}</p>
        <p v-else-if="detailLoading" class="rk-empty">读取中…</p>
        <RedisValueView v-else-if="detail" :value="detail.value" />
        <div class="rk-actions">
          <form class="rk-ttl-form" @submit.prevent="saveTtl">
            <input v-model="ttlInput" class="mono" data-field="redis-ttl" placeholder="秒（留空=永不过期）" aria-label="TTL 秒数" />
            <button class="btn sm" type="submit" data-action="redis-set-ttl">设置 TTL</button>
            <span v-if="ttlNote" class="rk-note ok" data-redis-ttl-note>{{ ttlNote }}</span>
            <span v-if="ttlError" class="rk-note err" data-redis-ttl-error>{{ ttlError }}</span>
          </form>
          <span class="grow"></span>
          <button class="btn sm" type="button" data-action="redis-rename" @click="askRename">重命名…</button>
          <button class="btn sm danger" type="button" data-action="redis-delete" @click="askDelete">删除…</button>
        </div>
      </template>
      <p v-else class="rk-empty">在左侧选择一个键查看内容。双击同样打开。</p>
    </section>

    <form class="rk-cmd" @submit.prevent="runCommand">
      <Icon name="terminal" />
      <input v-model="commandInput" class="mono" data-field="redis-command" placeholder="命令（如 GET key；写删类会先确认；KEYS 已禁用）" aria-label="Redis 命令" />
      <button class="btn sm primary" type="submit" data-action="redis-exec">执行</button>
    </form>
    <div class="rk-result" aria-live="polite">
      <p v-if="commandError" class="rk-error" role="alert" data-redis-cmd-error>{{ commandError }}</p>
      <p v-if="commandNote" class="rk-note ok" data-redis-cmd-note>{{ commandNote }}</p>
      <template v-if="commandResult">
        <p class="rk-reply-kind" :data-reply-kind="commandResult.reply.kind">
          <span class="rk-badge">{{ commandResult.command }}</span>
          <span class="rk-kind">{{ commandResult.reply.kind }}</span>
          <span v-if="commandResult.reply.kind === 'int'" class="mono">{{ commandResult.reply.int }}</span>
          <span v-else-if="commandResult.reply.text" class="mono">{{ commandResult.reply.text }}</span>
          <span v-else-if="commandResult.reply.kind === 'nil'">（nil）</span>
        </p>
        <ul v-if="commandResult.reply.items?.length" class="rk-reply-items">
          <li v-for="(item, index) in commandResult.reply.items" :key="index" class="mono">
            <span class="rk-index">{{ index }}</span>{{ item.text ?? item.int ?? (item.kind === "nil" ? "(nil)" : item.kind) }}
          </li>
        </ul>
      </template>
    </div>

    <div v-if="renameOpen" class="rk-overlay" data-menu="rename" role="dialog" aria-label="重命名键" @click.self="renameOpen = false">
      <form class="rk-card" @submit.prevent="confirmRename">
        <h4>重命名 {{ selectedKey }}</h4>
        <input v-model="renameTo" class="mono" data-field="redis-rename-to" placeholder="新键名" aria-label="新键名" />
        <div class="rk-card-actions">
          <button class="btn sm" type="button" data-action="redis-rename-cancel" @click="renameOpen = false">取消</button>
          <button class="btn sm primary" type="submit" data-action="redis-rename-confirm" :disabled="!renameTo.trim()">继续</button>
        </div>
      </form>
    </div>

    <div v-if="confirmState.open" class="rk-overlay" data-menu="confirm" role="dialog" aria-label="写操作确认" @click.self="cancelConfirm">
      <div class="rk-card">
        <h4>{{ confirmState.title }}</h4>
        <p class="rk-prompt" data-redis-confirm-prompt>{{ confirmState.prompt }}</p>
        <div class="rk-card-actions">
          <button class="btn sm" type="button" data-action="redis-confirm-cancel" @click="cancelConfirm">取消</button>
          <button class="btn sm danger" type="button" data-action="redis-confirm-ok" @click="runConfirm">确认执行</button>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.redis-tab { display: grid; grid-template-columns: minmax(220px, 320px) 1fr; grid-template-rows: 1fr auto auto; gap: var(--sp-2); height: 100%; min-height: 0; padding: var(--sp-2); }
.rk-list { display: flex; flex-direction: column; gap: var(--sp-2); min-height: 0; border: 1px solid var(--line); border-radius: var(--r-md, 8px); padding: var(--sp-2); overflow: hidden; }
.rk-filter { display: flex; gap: var(--sp-1); }
.rk-filter input { flex: 1; min-width: 0; }
.rk-keys { margin: 0; padding: 0; list-style: none; overflow: auto; display: grid; gap: 2px; align-content: start; }
.rk-key { display: grid; grid-template-columns: auto 1fr auto; gap: var(--sp-2); align-items: center; width: 100%; text-align: left; padding: var(--sp-1) var(--sp-2); border: 0; border-radius: var(--r-sm, 6px); background: transparent; color: inherit; cursor: pointer; }
.rk-key:hover { background: var(--bg-2, rgba(127, 127, 127, 0.12)); }
.rk-key.active { background: var(--accent-soft, rgba(127, 127, 127, 0.2)); }
.rk-name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.rk-badge { font-size: 11px; padding: 1px 6px; border-radius: 999px; border: 1px solid var(--line); color: var(--text-2); }
.rk-badge.lg { font-size: 12px; }
.rk-ttl { font-size: 11px; color: var(--text-3); white-space: nowrap; }
.rk-empty { color: var(--text-3); font-size: 12px; margin: var(--sp-2) 0; }
.rk-error { color: var(--danger, #c0392b); font-size: 12px; margin: 0; }
.rk-more { display: flex; align-items: center; gap: var(--sp-2); }
.rk-done { font-size: 11px; color: var(--text-3); }
.rk-detail { display: flex; flex-direction: column; gap: var(--sp-2); min-height: 0; border: 1px solid var(--line); border-radius: var(--r-md, 8px); padding: var(--sp-2); overflow: hidden; }
.rk-head { display: flex; align-items: center; gap: var(--sp-2); }
.rk-head h3 { margin: 0; font-size: 14px; overflow-wrap: anywhere; }
.rk-ttl-now { font-size: 12px; color: var(--text-3); }
.rk-detail > :not(header, .rk-empty, .rk-error) { flex: 1; min-height: 0; }
.rk-actions { display: flex; align-items: center; gap: var(--sp-2); flex: initial; flex-wrap: wrap; }
.rk-ttl-form { display: flex; align-items: center; gap: var(--sp-1); }
.rk-ttl-form input { width: 150px; }
.rk-note { font-size: 12px; }
.rk-note.ok { color: var(--ok, #2e7d32); }
.rk-cmd { grid-column: 1 / -1; display: flex; align-items: center; gap: var(--sp-2); }
.rk-cmd input { flex: 1; min-width: 0; }
.rk-result { grid-column: 1 / -1; max-height: 160px; overflow: auto; display: grid; gap: var(--sp-1); align-content: start; }
.rk-reply-kind { display: flex; gap: var(--sp-2); align-items: center; margin: 0; font-size: 13px; }
.rk-kind { font-size: 11px; color: var(--text-3); }
.rk-reply-items { margin: 0; padding: 0; list-style: none; display: grid; gap: 2px; font-size: 12px; }
.rk-index { color: var(--text-3); margin-right: var(--sp-2); }
.rk-overlay { position: fixed; inset: 0; background: rgba(0, 0, 0, 0.35); display: grid; place-items: center; z-index: 60; }
.rk-card { background: var(--bg, #fff); color: var(--text, inherit); border-radius: var(--r-md, 8px); padding: var(--sp-3); min-width: 320px; display: grid; gap: var(--sp-2); box-shadow: 0 12px 32px rgba(0, 0, 0, 0.18); }
.rk-card h4 { margin: 0; font-size: 14px; }
.rk-prompt { margin: 0; font-size: 13px; color: var(--text-2); overflow-wrap: anywhere; }
.rk-card-actions { display: flex; justify-content: flex-end; gap: var(--sp-2); }
.grow { flex: 1; }
.mono { font-family: var(--mono, ui-monospace, monospace); }
</style>
