<script setup lang="ts">
import { tr as t } from "../i18n";
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
      ttlError.value = t("redis.tab.ttlMissed");
      return;
    }
    ttlNote.value = parsed.seconds == null ? t("redis.tab.ttlRemoved") : t("redis.tab.ttlSet", { n: parsed.seconds });
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
  confirmState.title = t("redis.tab.deleteTitle");
  confirmState.prompt = t("redis.tab.deletePrompt", { key: selectedKey.value });
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
  confirmState.title = t("redis.tab.renameTitle");
  confirmState.prompt = t("redis.tab.renamePrompt", { key: selectedKey.value, target });
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
      commandNote.value = t("redis.tab.deletedCount", { n: outcome.deleted });
      const removed = selectedKey.value;
      selectedKey.value = "";
      detail.value = null;
      keys.value = keys.value.filter((item) => item.name !== removed);
      await refreshList();
    } else if (action === "rename" && selectedKey.value) {
      const target = renameTo.value.trim();
      await api.value.renameKey(props.connectionId, selectedKey.value, target, true);
      commandNote.value = t("redis.tab.renamedNote", { target });
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
  commandNote.value = t("redis.tab.executed");
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
    commandError.value = t("redis.tab.keysRefused");
    return;
  }
  if (risk === "dangerous") {
    confirmState.open = true;
    confirmState.action = "exec";
    confirmState.title = t("redis.tab.confirmTitle");
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
          :placeholder="t('redis.tab.patternPlaceholder')"
          :aria-label="t('redis.tab.patternLabel')"
        />
        <button class="btn sm" type="submit" data-action="redis-apply-pattern" :disabled="loading">{{ t("redis.tab.filter") }}</button>
        <button class="btn sm ghost" type="button" data-action="redis-refresh" :disabled="loading" @click="refreshList">{{ t("redis.tab.refresh") }}</button>
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
      <p v-if="!loading && keys.length === 0 && !listError" class="rk-empty">{{ t("redis.tab.emptyList") }}</p>
      <div class="rk-more">
        <button v-if="!done" class="btn sm" type="button" data-action="redis-more" :disabled="loading" @click="loadPage">
          {{ loading ? t("redis.tab.loading") : t("redis.tab.loadMore") }}
        </button>
        <span v-else class="rk-done">{{ t("redis.tab.scanDone") }}</span>
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
        <p v-else-if="detailLoading" class="rk-empty">{{ t("redis.tab.loading") }}</p>
        <RedisValueView v-else-if="detail" :value="detail.value" />
        <div class="rk-actions">
          <form class="rk-ttl-form" @submit.prevent="saveTtl">
            <input v-model="ttlInput" class="mono" data-field="redis-ttl" :placeholder="t('redis.tab.ttlPlaceholder')" :aria-label="t('redis.tab.ttlLabel')" />
            <button class="btn sm" type="submit" data-action="redis-set-ttl">{{ t("redis.tab.setTtl") }}</button>
            <span v-if="ttlNote" class="rk-note ok" data-redis-ttl-note>{{ ttlNote }}</span>
            <span v-if="ttlError" class="rk-note err" data-redis-ttl-error>{{ ttlError }}</span>
          </form>
          <span class="grow"></span>
          <button class="btn sm" type="button" data-action="redis-rename" @click="askRename">{{ t("redis.tab.rename") }}</button>
          <button class="btn sm danger" type="button" data-action="redis-delete" @click="askDelete">{{ t("redis.tab.remove") }}</button>
        </div>
      </template>
      <p v-else class="rk-empty">{{ t("redis.tab.pickKey") }}</p>
    </section>

    <form class="rk-cmd" @submit.prevent="runCommand">
      <Icon name="terminal" />
      <input v-model="commandInput" class="mono" data-field="redis-command" :placeholder="t('redis.tab.commandPlaceholder')" :aria-label="t('redis.tab.commandLabel')" />
      <button class="btn sm primary" type="submit" data-action="redis-exec">{{ t("redis.tab.execute") }}</button>
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

    <div v-if="renameOpen" class="rk-overlay" data-menu="rename" role="dialog" :aria-label="t('redis.tab.renameKeyTitle')" @click.self="renameOpen = false">
      <form class="rk-card" @submit.prevent="confirmRename">
        <h4>{{ t("redis.tab.renameKeyHeading", { key: selectedKey }) }}</h4>
        <input v-model="renameTo" class="mono" data-field="redis-rename-to" :placeholder="t('redis.tab.renameToPlaceholder')" :aria-label="t('redis.tab.renameToLabel')" />
        <div class="rk-card-actions">
          <button class="btn sm" type="button" data-action="redis-rename-cancel" @click="renameOpen = false">{{ t("common.cancel") }}</button>
          <button class="btn sm primary" type="submit" data-action="redis-rename-confirm" :disabled="!renameTo.trim()">{{ t("redis.tab.proceed") }}</button>
        </div>
      </form>
    </div>

    <div v-if="confirmState.open" class="rk-overlay" data-menu="confirm" role="dialog" :aria-label="t('redis.tab.confirmAria')" @click.self="cancelConfirm">
      <div class="rk-card">
        <h4>{{ confirmState.title }}</h4>
        <p class="rk-prompt" data-redis-confirm-prompt>{{ confirmState.prompt }}</p>
        <div class="rk-card-actions">
          <button class="btn sm" type="button" data-action="redis-confirm-cancel" @click="cancelConfirm">{{ t("common.cancel") }}</button>
          <button class="btn sm danger" type="button" data-action="redis-confirm-ok" @click="runConfirm">{{ t("redis.tab.confirmExecute") }}</button>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.redis-tab { display: grid; grid-template-columns: minmax(240px, 320px) minmax(0, 1fr); grid-template-rows: minmax(0, 1fr) auto auto; height: 100%; min-height: 0; }
.rk-list { display: flex; flex-direction: column; gap: var(--sp-2); min-height: 0; padding: var(--sp-2); border-right: 1px solid var(--line); background: var(--panel-2); overflow: hidden; }
.rk-filter { display: flex; gap: var(--sp-1); }
.rk-filter input { flex: 1; min-width: 0; }
.rk-keys { margin: 0; padding: 0; list-style: none; overflow: auto; display: grid; gap: 2px; align-content: start; }
.rk-key { display: grid; grid-template-columns: auto minmax(0, 1fr) auto; gap: var(--sp-2); align-items: center; width: 100%; height: var(--tree-h); padding: 0 var(--sp-2); border-radius: var(--r-sm); text-align: left; font-size: var(--fs-sm); }
.rk-key:hover { background: var(--hover); }
.rk-key.active { background: var(--sel-strong); }
.rk-name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.rk-badge { font-size: 10.5px; line-height: 16px; padding: 0 5px; border-radius: var(--r-sm); background: var(--sunken); color: var(--text-2); }
.rk-badge.lg { font-size: 12px; }
.rk-ttl { font-size: 11px; color: var(--text-3); white-space: nowrap; }
.rk-empty { color: var(--text-3); font-size: 12px; margin: var(--sp-2) 0; }
.rk-error { color: var(--err); font-size: 12px; margin: 0; }
.rk-more { display: flex; align-items: center; gap: var(--sp-2); }
.rk-done { font-size: 11px; color: var(--text-3); }
.rk-detail { display: flex; flex-direction: column; gap: var(--sp-2); min-height: 0; min-width: 0; padding: var(--sp-3); overflow: hidden; }
.rk-head { display: flex; align-items: center; gap: var(--sp-2); }
.rk-head h3 { margin: 0; font-size: var(--fs-md); font-weight: 600; overflow-wrap: anywhere; }
.rk-ttl-now { font-size: 12px; color: var(--text-3); }
.rk-detail > :not(header, .rk-empty, .rk-error) { flex: 1; min-height: 0; }
.rk-detail > .rk-actions { display: flex; align-items: center; gap: var(--sp-2); flex: 0 0 auto; flex-wrap: wrap; padding-top: var(--sp-2); border-top: 1px solid var(--line); }
.rk-ttl-form { display: flex; align-items: center; gap: var(--sp-1); }
.rk-ttl-form input { width: 150px; }
.rk-note { font-size: 12px; }
.rk-note.ok { color: var(--ok); }
.rk-cmd { grid-column: 1 / -1; display: flex; align-items: center; gap: var(--sp-2); padding: var(--sp-2) var(--sp-3); border-top: 1px solid var(--line); background: var(--panel-2); }
.rk-cmd input { flex: 1; min-width: 0; }
.rk-result { grid-column: 1 / -1; max-height: 160px; overflow: auto; display: grid; gap: var(--sp-1); align-content: start; padding: var(--sp-2) var(--sp-3); border-top: 1px solid var(--line); font-family: var(--mono); }
.rk-result:not(:has(*)) { display: none; }
.rk-reply-kind { display: flex; gap: var(--sp-2); align-items: center; margin: 0; font-size: 13px; }
.rk-kind { font-size: 11px; color: var(--text-3); }
.rk-reply-items { margin: 0; padding: 0; list-style: none; display: grid; gap: 2px; font-size: 12px; }
.rk-index { color: var(--text-3); margin-right: var(--sp-2); }
.rk-overlay { position: fixed; inset: 0; background: var(--overlay); display: grid; place-items: center; z-index: var(--z-dialog); animation: fade-in var(--dur-fast) var(--ease); }
.rk-card { background: var(--surface-overlay); color: var(--text, inherit); border-radius: var(--r-md); padding: var(--sp-4); min-width: 320px; display: grid; gap: var(--sp-2); box-shadow: 0 12px 32px rgba(0, 0, 0, 0.18);  box-shadow: var(--shadow-lg); border-radius: var(--r-xl); }
.rk-card h4 { margin: 0; font-size: 14px; }
.rk-prompt { margin: 0; font-size: 13px; color: var(--text-2); overflow-wrap: anywhere; }
.rk-card-actions { display: flex; justify-content: flex-end; gap: var(--sp-2); }
.grow { flex: 1; }
.mono { font-family: var(--mono, ui-monospace, monospace); }
</style>
