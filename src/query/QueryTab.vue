<script setup lang="ts">
import { autocompletion, startCompletion, type CompletionContext } from "@codemirror/autocomplete";
import { toggleComment } from "@codemirror/commands";
import { sql } from "@codemirror/lang-sql";
import { Prec } from "@codemirror/state";
import { EditorView, keymap } from "@codemirror/view";
import { EditorState } from "@codemirror/state";
import { basicSetup } from "codemirror";
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { catalogConnections } from "../catalog/store";
import QueryResultGrid from "../grid/QueryResultGrid.vue";
import type { Engine } from "../grid/types";
import { loadSettings } from "../shell/settings";
import { CONNECTION_STATUS_TEXT, type ConnectionStatus } from "../shell/status-text";
import type { QueryTarget } from "../shell/session";
import { completionPlan, connectionById, metadataNote, targetKey, targetOptions } from "./complete";
import { gridModel } from "./rows";
import { requestCancel, startTabRun, tabRun } from "./runtime";
import { QUERY_EDITOR_KEYS, rowLimit, sqlToRun } from "./scope";

const props = defineProps<{
  tabId: string;
  draft: string;
  connectionId: string | null;
  connectionName: string | null;
  database: string | null;
  engine: string | null;
  connectionStatus: ConnectionStatus | null;
  notice: string | null;
}>();
const emit = defineEmits<{
  "update:draft": [tabId: string, draft: string];
  target: [target: QueryTarget];
}>();

const host = ref<HTMLElement | null>(null);
const hasSelection = ref(false);
const actionError = ref("");
const resultIndex = ref(0);
let view: EditorView | undefined;
let alive = true;

const run = computed(() => tabRun(props.tabId));
const running = computed(() => run.value?.status === "running" || run.value?.status === "cancel-requested");
const options = computed(() => targetOptions(catalogConnections()));
const selectedKey = computed(() => (props.connectionId && props.database != null ? targetKey(props.connectionId, props.database) : ""));
const readout = computed(() => (props.connectionName ? `${props.connectionName} / ${props.database || "main"}` : "未选择"));
const hint = computed(() => {
  const connection = connectionById(catalogConnections(), props.connectionId);
  if (props.connectionId && !connection) {
    return { keywordsOnly: true, note: "原连接已不在目录中：只提供关键字。请重新选择执行目标。", objects: [] };
  }
  return metadataNote(connection, props.connectionId ? props.database : null);
});
const activeStatement = computed(() => run.value?.statements[resultIndex.value] ?? null);
const grid = computed(() => (activeStatement.value && activeStatement.value.columns.length ? gridModel(activeStatement.value) : null));
const gridEngine = computed(() => asEngine(run.value?.engine));

watch(
  () => run.value?.id,
  () => {
    resultIndex.value = 0;
  },
);

const shellTheme = EditorView.theme({
  "&": { backgroundColor: "var(--panel)", color: "var(--text)", height: "100%" },
  ".cm-content": { fontFamily: "var(--mono)", fontSize: "var(--editor-font)", caretColor: "var(--text)" },
  ".cm-gutters": { backgroundColor: "var(--panel)", color: "var(--text-3)", borderRight: "1px solid var(--line)" },
  "&.cm-focused": { outline: "none" },
});

function asEngine(value: string | undefined): Engine | undefined {
  if (value === "postgres" || value === "mysql" || value === "mariadb" || value === "sqlite") return value;
  return undefined;
}

function sqlCompletion(context: CompletionContext) {
  const before = context.state.sliceDoc(0, context.pos);
  if (/--[^\n]*$/.test(before)) return null;
  const word = context.matchBefore(/[A-Za-z0-9_.]*$/);
  if (!word || (word.from === word.to && !context.explicit)) return null;
  const connection = connectionById(catalogConnections(), props.connectionId);
  const plan = completionPlan({
    prefix: word.text,
    connection: props.connectionId && connection ? connection : null,
    database: props.connectionId && connection ? props.database : null,
  });
  return {
    from: word.from,
    filter: false,
    options: plan.items.map((item) => ({ label: item.label, type: item.kind, detail: item.detail })),
  };
}

async function execute(scope: "auto" | "current") {
  actionError.value = "";
  if (!view || running.value) return;
  const connection = connectionById(catalogConnections(), props.connectionId);
  if (!props.connectionId || props.database == null || !props.connectionName || !props.engine || !connection) {
    actionError.value = props.connectionId && !connection ? "原连接已删除，请重新选择执行目标。" : "请选择执行目标（连接和数据库）。";
    return;
  }
  const range = view.state.selection.main;
  const picked = sqlToRun(view.state.doc.toString(), { from: range.from, to: range.to, head: range.head }, scope);
  if (!picked.sql.trim()) {
    actionError.value = "没有可执行的 SQL。";
    return;
  }
  try {
    await startTabRun({
      tabId: props.tabId,
      connectionId: props.connectionId,
      connectionName: props.connectionName,
      database: props.database,
      engine: props.engine,
      sql: picked.sql,
      limit: rowLimit(loadSettings().pageSize),
    });
  } catch (err) {
    actionError.value = typeof err === "string" ? err : err instanceof Error ? err.message : "执行失败";
  }
}

function cancel() {
  if (!running.value) return;
  void requestCancel(props.tabId);
}

function onTarget(event: Event) {
  const value = (event.target as HTMLSelectElement).value;
  const found = options.value.find((item) => item.key === value);
  if (!found) return;
  emit("target", {
    connectionId: found.connectionId,
    connectionName: found.name,
    database: found.database,
    engine: found.engine,
    status: found.status,
  });
}

function statusLabel(status: string): string {
  const labels: Record<string, string> = {
    pending: "等待",
    running: "执行中",
    ok: "完成",
    error: "失败",
    skipped: "未执行",
    cancelled: "已取消",
    unknown: "未知",
  };
  return labels[status] ?? status;
}

onMounted(() => {
  if (!host.value) return;
  view = new EditorView({
    parent: host.value,
    state: EditorState.create({
      doc: props.draft,
      extensions: [
        basicSetup,
        sql(),
        shellTheme,
        Prec.high(
          keymap.of([
            { key: QUERY_EDITOR_KEYS[0], run: () => { void execute("auto"); return true; } },
            { key: QUERY_EDITOR_KEYS[1], run: () => { void execute("current"); return true; } },
            { key: QUERY_EDITOR_KEYS[2], run: () => { cancel(); return true; } },
            { key: QUERY_EDITOR_KEYS[3], run: startCompletion },
            { key: QUERY_EDITOR_KEYS[4], run: toggleComment },
          ]),
        ),
        autocompletion({ override: [sqlCompletion] }),
        EditorView.updateListener.of((update) => {
          if (!alive) return;
          if (update.selectionSet || update.docChanged) {
            const range = update.state.selection.main;
            hasSelection.value = range.from !== range.to;
          }
          if (!update.docChanged) return;
          const draft = update.state.doc.toString();
          if (draft !== props.draft) emit("update:draft", props.tabId, draft);
        }),
      ],
    }),
  });
});

onBeforeUnmount(() => {
  alive = false;
  view?.destroy();
});
</script>

<template>
  <section class="query-pane" aria-label="查询" :data-tab-id="tabId">
    <div class="q-toolbar">
      <label class="q-target">
        执行目标
        <strong>{{ readout }}</strong>
        <select aria-label="执行目标" :value="selectedKey" :disabled="running" @change="onTarget">
          <option value="">选择连接和数据库</option>
          <option v-for="option in options" :key="option.key" :value="option.key">{{ option.label }}</option>
        </select>
      </label>
      <span v-if="connectionStatus" class="conn-state" :class="`st-${connectionStatus}`"><i></i>{{ CONNECTION_STATUS_TEXT[connectionStatus] }}</span>
      <button v-if="!running" class="btn primary" type="button" data-action="run" @click="execute('auto')">{{ hasSelection ? "执行选中" : "执行" }}</button>
      <button v-if="!running" class="btn" type="button" data-action="run-current" @click="execute('current')">当前语句</button>
      <button v-else class="btn" type="button" data-action="cancel" :disabled="run?.cancel === 'requested'" @click="cancel">
        {{ run?.cancel === "failed" ? "再次请求取消" : run?.cancel === "requested" ? "等待确认" : "请求取消" }}
      </button>
      <span v-if="hasSelection && !running" class="q-note">只执行选中的内容</span>
      <span v-if="run" class="q-note" role="status">{{ statusLabel(run.status) }} · {{ run.elapsedMs }} ms</span>
    </div>
    <p v-if="notice" class="q-banner">{{ notice }}</p>
    <p v-if="actionError" class="q-banner" role="alert">{{ actionError }}</p>
    <p class="q-hint" :data-keywords-only="hint.keywordsOnly">{{ hint.note }}</p>
    <div ref="host" class="cm-host"></div>
    <section class="q-results" aria-label="执行结果">
      <p v-if="!run" class="q-idle">执行 SQL 后，结果显示在这里。有选区时只执行选区；当前语句只执行光标所在语句。</p>
      <template v-else>
        <div class="q-statements" role="tablist" aria-label="语句结果">
          <button
            v-for="(statement, index) in run.statements"
            :key="`${run.id}-${index}`"
            class="btn"
            :class="{ primary: index === resultIndex }"
            type="button"
            role="tab"
            :aria-selected="index === resultIndex"
            @click="resultIndex = index"
          >
            {{ index + 1 }} {{ statusLabel(statement.status) }}
          </button>
        </div>
        <p v-if="run.cancel === 'requested'" role="status">已请求取消，等待数据库确认。{{ run.viaSsh ? "经 SSH 时取消另开一条通道。" : "" }}{{ run.cancelMessage }}</p>
        <p v-else-if="run.cancel === 'confirmed' || run.status === 'cancelled'" role="status">数据库已确认取消。{{ run.cancelMessage }}</p>
        <p v-else-if="run.cancel === 'failed'" role="alert">取消失败，查询仍在执行。{{ run.cancelMessage }}</p>
        <p v-if="run.status === 'error'">已完成的结果保留，后续语句未执行。</p>
        <template v-if="activeStatement">
          <pre class="q-sql">{{ activeStatement.sql }}</pre>
          <p v-if="activeStatement.status === 'unknown'">写入结果未知。不会自动重放。</p>
          <p v-else-if="activeStatement.status === 'skipped'">这条语句没有发送到数据库。</p>
          <p v-else-if="activeStatement.status === 'cancelled'">数据库已确认取消这条语句。</p>
          <p v-else-if="activeStatement.status === 'error'">{{ activeStatement.message }}</p>
          <p v-else-if="activeStatement.status === 'ok' && !grid">影响 {{ activeStatement.affected ?? 0 }} 行</p>
          <QueryResultGrid
            v-if="grid"
            :tab-id="tabId"
            :statement="activeStatement.sql"
            :columns="grid.columns"
            :rows="grid.rows"
            :truncated="activeStatement.truncated"
            :connection-id="run.connectionId"
            :engine="gridEngine"
            :database="run.database"
          />
        </template>
      </template>
    </section>
  </section>
</template>

<style scoped>
.query-pane { display: flex; flex-direction: column; flex: 1; min-height: 0; }
.q-toolbar { display: flex; gap: 8px; align-items: center; flex-wrap: wrap; padding: 6px 8px; border-bottom: 1px solid var(--line); }
.q-target { display: inline-flex; gap: 6px; align-items: center; font-size: 12.5px; }
.q-target select { max-width: 280px; }
.q-note, .q-hint, .q-banner, .q-idle, .q-results p { margin: 0; font-size: 12.5px; color: var(--text-2); }
.q-hint, .q-banner { padding: 4px 8px; }
.q-banner { color: var(--text); background: var(--sunken); }
.q-results { min-height: 120px; overflow: auto; border-top: 1px solid var(--line); padding: 8px; display: flex; flex-direction: column; gap: 8px; }
.q-statements { display: flex; gap: 6px; flex-wrap: wrap; }
.q-sql { margin: 0; white-space: pre-wrap; font-family: var(--mono); font-size: 12px; }
</style>
