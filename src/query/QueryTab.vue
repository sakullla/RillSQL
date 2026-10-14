<script setup lang="ts">
import { autocompletion, startCompletion, type CompletionContext } from "@codemirror/autocomplete";
import { toggleComment } from "@codemirror/commands";
import { sql } from "@codemirror/lang-sql";
import { Prec } from "@codemirror/state";
import { EditorView, keymap } from "@codemirror/view";
import { EditorState } from "@codemirror/state";
import { basicSetup } from "codemirror";
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { catalogConnections } from "../catalog/store";
import { engineMeta } from "../connections/model";
import QueryResultGrid from "../grid/QueryResultGrid.vue";
import type { Engine } from "../grid/types";
import Icon from "../shell/Icon.vue";
import { loadSettings } from "../shell/settings";
import { CONNECTION_STATUS_TEXT, type ConnectionStatus } from "../shell/status-text";
import type { QueryTarget } from "../shell/session";
import { completionPlan, connectionById, metadataNote, targetDetail, targetKey, targetOptions, type TargetOption } from "./complete";
import { resolveEditor } from "./editor";
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
const targetRoot = ref<HTMLElement | null>(null);
const menuOpen = ref(false);
const activeIndex = ref(0);
const hasSelection = ref(false);
const submitting = ref(false);
const actionError = ref("");
const resultIndex = ref(0);
let view: EditorView | undefined;
let alive = true;

const run = computed(() => tabRun(props.tabId));
const running = computed(() => run.value?.status === "running" || run.value?.status === "cancel-requested");
const options = computed(() => targetOptions(catalogConnections()));
const selectedKey = computed(() => (props.connectionId && props.database != null ? targetKey(props.connectionId, props.database) : ""));
const currentOption = computed(() => options.value.find((item) => item.key === selectedKey.value) ?? null);
const buttonEngine = computed(() => currentOption.value?.engine ?? asEngine(props.engine ?? undefined));
const buttonLabel = computed(() => {
  if (!props.connectionName) return "执行目标";
  const engine = buttonEngine.value ? engineMeta(buttonEngine.value).label : "";
  return `执行目标 ${engine} ${props.connectionName} / ${props.database || "main"}`.replace(/\s+/g, " ").trim();
});
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

function editorExtensions() {
  return [
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
  ];
}

function mountEditor() {
  view = resolveEditor(view, host.value, (parent) => new EditorView({
    parent,
    state: EditorState.create({ doc: props.draft, extensions: editorExtensions() }),
  }));
}

async function execute(scope: "auto" | "current") {
  actionError.value = "";
  if (running.value || submitting.value) return;
  mountEditor();
  const editor = view;
  if (!editor) {
    actionError.value = "编辑器还没有准备好，请再试一次。";
    return;
  }
  const connection = connectionById(catalogConnections(), props.connectionId);
  if (!props.connectionId || props.database == null || !props.connectionName || !props.engine || !connection) {
    actionError.value = props.connectionId && !connection ? "原连接已删除，请重新选择执行目标。" : "请选择执行目标（连接和数据库）。";
    return;
  }
  const range = editor.state.selection.main;
  const picked = sqlToRun(editor.state.doc.toString(), { from: range.from, to: range.to, head: range.head }, scope);
  if (!picked.sql.trim()) {
    actionError.value = "没有可执行的 SQL。";
    return;
  }
  submitting.value = true;
  menuOpen.value = false;
  try {
    await nextTick();
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
  } finally {
    submitting.value = false;
  }
}

function cancel() {
  if (!running.value) return;
  void requestCancel(props.tabId);
}

function toggleMenu() {
  if (running.value || submitting.value) return;
  menuOpen.value = !menuOpen.value;
  if (!menuOpen.value) return;
  const index = options.value.findIndex((item) => item.key === selectedKey.value);
  activeIndex.value = index >= 0 ? index : 0;
}

function choose(option: TargetOption) {
  menuOpen.value = false;
  if (option.key === selectedKey.value) return;
  emit("target", {
    connectionId: option.connectionId,
    connectionName: option.name,
    database: option.database,
    engine: option.engine,
    status: option.status,
  });
}

function onTargetKey(event: KeyboardEvent) {
  if (running.value || submitting.value) return;
  if (!menuOpen.value && (event.key === "ArrowDown" || event.key === "ArrowUp" || event.key === "Enter" || event.key === " ")) {
    event.preventDefault();
    toggleMenu();
    return;
  }
  if (!menuOpen.value) return;
  if (event.key === "Escape") {
    event.preventDefault();
    menuOpen.value = false;
    return;
  }
  if (event.key === "ArrowDown" || event.key === "ArrowUp") {
    event.preventDefault();
    const count = options.value.length;
    if (!count) return;
    const delta = event.key === "ArrowDown" ? 1 : -1;
    activeIndex.value = (activeIndex.value + delta + count) % count;
    return;
  }
  if (event.key === "Enter") {
    event.preventDefault();
    const option = options.value[activeIndex.value];
    if (option) choose(option);
  }
}

function onPointerDown(event: PointerEvent) {
  if (!menuOpen.value) return;
  const root = targetRoot.value;
  if (root && event.target instanceof Node && root.contains(event.target)) return;
  menuOpen.value = false;
}

watch(running, (value) => {
  if (value) menuOpen.value = false;
});

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

watch(host, (element) => {
  if (element) mountEditor();
});

onMounted(() => {
  document.addEventListener("pointerdown", onPointerDown);
  mountEditor();
});

onBeforeUnmount(() => {
  alive = false;
  document.removeEventListener("pointerdown", onPointerDown);
  view?.destroy();
});
</script>

<template>
  <section class="query-pane" aria-label="查询" :data-tab-id="tabId">
    <div class="q-toolbar">
      <div ref="targetRoot" class="q-target">
        <button
          class="target-btn"
          :class="{ none: !connectionName, on: menuOpen }"
          type="button"
          data-action="target"
          :aria-label="buttonLabel"
          aria-haspopup="listbox"
          :aria-expanded="menuOpen"
          :disabled="running || submitting"
          :title="running ? '执行期间不能切换目标' : '选择执行目标（连接与数据库）'"
          @click="toggleMenu"
          @keydown="onTargetKey"
        >
          <template v-if="connectionName">
            <span v-if="buttonEngine" class="eng" :class="`eng-${buttonEngine}`" :title="engineMeta(buttonEngine).label">{{ engineMeta(buttonEngine).short }}</span>
            <span v-if="currentOption && currentOption.envTone !== 'none'" class="env xs" :class="`env-${currentOption.envTone}`">{{ currentOption.envLabel }}</span>
            <span class="tg-name">{{ connectionName }}</span><span class="tg-sep"> / </span><span class="tg-db"><Icon name="database" /><span>{{ database || "main" }}</span></span>
          </template>
          <template v-else>
            <Icon name="database" />
            <span class="tg-name">选择执行目标</span>
          </template>
          <svg class="tg-caret" viewBox="0 0 24 24" aria-hidden="true"><path d="m6 9 6 6 6-6" /></svg>
        </button>
        <div v-if="menuOpen" class="target-menu" role="listbox" aria-label="执行目标">
          <div class="menu-head">选择连接和数据库</div>
          <p v-if="!options.length" class="target-empty">还没有可选的数据库。</p>
          <button
            v-for="(option, index) in options"
            :key="option.key"
            class="target-option"
            :class="{ on: index === activeIndex, picked: option.key === selectedKey }"
            type="button"
            role="option"
            :data-engine="option.engine"
            :aria-selected="option.key === selectedKey"
            @mouseenter="activeIndex = index"
            @click="choose(option)"
          >
            <span class="eng" :class="`eng-${option.engine}`">{{ engineMeta(option.engine).short }}</span>
            <span class="mi-copy">
              <span class="mi-title"><b>{{ option.name }}</b><span> / </span><Icon name="database" /><span>{{ option.database || "main" }}</span></span>
              <small>{{ targetDetail(option) }}</small>
            </span>
            <Icon v-if="option.key === selectedKey" class="check" name="check" />
          </button>
        </div>
      </div>
      <span v-if="connectionStatus" class="conn-state" :class="`st-${connectionStatus}`"><i></i>{{ CONNECTION_STATUS_TEXT[connectionStatus] }}</span>
      <button v-if="!running" class="btn primary" type="button" data-action="run" :disabled="submitting" @click="execute('auto')">{{ submitting ? "正在执行…" : hasSelection ? "执行选中" : "执行" }}</button>
      <button v-if="!running" class="btn" type="button" data-action="run-current" :disabled="submitting" @click="execute('current')">当前语句</button>
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
.q-target { position: relative; min-width: 0; }
.target-btn { display: flex; align-items: center; gap: 6px; height: 30px; max-width: 420px; min-width: 0; padding: 0 8px 0 6px; border: 1px solid var(--line-2); border-radius: 7px; background: var(--panel); font-size: 12.5px; }
.target-btn:hover:not(:disabled) { border-color: var(--text-3); }
.target-btn.on { border-color: var(--accent); box-shadow: 0 0 0 3px var(--sel-strong); }
.target-btn.none { border-style: dashed; color: var(--text-2); }
.target-btn:disabled { opacity: 0.7; cursor: not-allowed; }
.tg-name { font-weight: 600; white-space: nowrap; }
.tg-db { display: inline-flex; align-items: center; gap: 4px; min-width: 0; color: var(--text-2); }
.tg-db span:last-child { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.tg-sep { color: var(--text-3); }
.tg-caret { width: 14px; height: 14px; margin-left: auto; color: var(--text-3); fill: none; stroke: currentColor; stroke-width: 1.8; stroke-linecap: round; stroke-linejoin: round; flex-shrink: 0; }
.target-btn :deep(.i) { width: 14px; height: 14px; color: var(--text-3); }
.eng { --engc: var(--text-2); display: inline-grid; place-items: center; width: 20px; height: 20px; border-radius: 5px; font: 700 9.5px/1 var(--font); color: var(--engc); background: color-mix(in srgb, var(--engc) 14%, transparent); flex-shrink: 0; }
.eng-postgres { --engc: var(--eng-postgres); }
.eng-mysql { --engc: var(--eng-mysql); }
.eng-mariadb { --engc: var(--eng-mariadb); }
.eng-sqlite { --engc: var(--eng-sqlite); }
.target-menu { position: absolute; z-index: 30; top: calc(100% + 4px); left: 0; min-width: 280px; max-width: 420px; max-height: 320px; overflow: auto; padding: 4px; background: var(--panel); border: 1px solid var(--line-2); border-radius: 10px; box-shadow: var(--shadow); }
.target-empty { margin: 0; padding: 8px; font-size: 12.5px; color: var(--text-3); }
.target-option { display: flex; align-items: center; gap: 8px; width: 100%; min-height: 44px; padding: 6px 8px; border-radius: 6px; text-align: left; }
.target-option.on { background: var(--hover); }
.target-option.picked { background: var(--accent-soft); }
.mi-copy { display: flex; flex-direction: column; min-width: 0; flex: 1; }
.mi-title { display: flex; align-items: center; gap: 4px; min-width: 0; font-size: 12.5px; }
.mi-title b { font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.mi-title :deep(.i) { width: 13px; height: 13px; color: var(--text-3); flex-shrink: 0; }
.target-option small { font-size: 11.5px; color: var(--text-3); }
.target-option .check { width: 14px; height: 14px; margin-left: auto; color: var(--accent); flex-shrink: 0; }
.q-note, .q-hint, .q-banner, .q-idle, .q-results p { margin: 0; font-size: 12.5px; color: var(--text-2); }
.q-hint, .q-banner { padding: 4px 8px; }
.q-banner { color: var(--text); background: var(--sunken); }
.q-results { min-height: 120px; overflow: auto; border-top: 1px solid var(--line); padding: 8px; display: flex; flex-direction: column; gap: 8px; }
.q-statements { display: flex; gap: 6px; flex-wrap: wrap; }
.q-sql { margin: 0; white-space: pre-wrap; font-family: var(--mono); font-size: 12px; }
</style>
