<script setup lang="ts">
import { autocompletion, startCompletion, type CompletionContext } from "@codemirror/autocomplete";
import { toggleComment } from "@codemirror/commands";
import { sql } from "@codemirror/lang-sql";
import { HighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { tags } from "@lezer/highlight";
import { EditorState, Prec, StateEffect, StateField } from "@codemirror/state";
import { Decoration, EditorView, keymap } from "@codemirror/view";
import { basicSetup } from "codemirror";
import ChordKeys from "../shell/ChordKeys.vue";
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { catalogConnections } from "../catalog/store";
import { engineMeta } from "../connections/model";
import QueryResultGrid from "../grid/QueryResultGrid.vue";
import type { Engine } from "../grid/types";
import Icon from "../shell/Icon.vue";
import { loadSettings } from "../shell/settings";
import { CONNECTION_STATUS_TEXT, type ConnectionStatus } from "../shell/status-text";
import type { QueryTarget } from "../shell/session";
import { ambiguousFieldNote, cachedColumns, fieldFailureNote, loadFields, qualifierFor, unknownFieldNote } from "./columns";
import { completionPlan, connectionById, metadataNote, targetDetail, targetKey, targetOptions, type TargetOption } from "./complete";
import { resolveEditor } from "./editor";
import { formatDocument } from "./format";
import { analyzeSql, explainSql, hierarchicalSql, planStatement, presentPlan } from "./plan";
import { gridModel } from "./rows";
import { continueTabRun, requestCancel, startTabRun, tabRun, whenSettled } from "./runtime";
import { QUERY_EDITOR_KEYS, rowLimit, sqlToRun } from "./scope";
import { inspectSql, openDelimiter } from "./split";

const props = defineProps<{
  tabId: string;
  draft: string;
  connectionId: string | null;
  connectionName: string | null;
  database: string | null;
  engine: string | null;
  connectionStatus: ConnectionStatus | null;
  notice: string | null;
  executeRequest?: number;
}>();
const emit = defineEmits<{
  "update:draft": [tabId: string, draft: string];
  target: [target: QueryTarget];
}>();

const editorPercent = ref(42);
const cursor = ref({ line: 1, column: 1 });
let stopResize: (() => void) | null = null;
function resizeEditor(event: PointerEvent) {
  event.preventDefault();
  stopResize?.();
  const pane = (event.currentTarget as HTMLElement).closest(".query-pane")!;
  const height = pane.getBoundingClientRect().height;
  const startY = event.clientY;
  const start = editorPercent.value;
  const move = (next: PointerEvent) => { editorPercent.value = Math.max(20, Math.min(70, start + (next.clientY - startY) / height * 100)); };
  stopResize = () => { window.removeEventListener("pointermove", move); window.removeEventListener("pointerup", stopResize!); window.removeEventListener("pointercancel", stopResize!); stopResize = null; };
  window.addEventListener("pointermove", move);
  window.addEventListener("pointerup", stopResize);
  window.addEventListener("pointercancel", stopResize);
}
const host = ref<HTMLElement | null>(null);
const targetRoot = ref<HTMLElement | null>(null);
const runRoot = ref<HTMLElement | null>(null);
const menuOpen = ref(false);
const runMenuOpen = ref(false);
const activeIndex = ref(0);
const hasSelection = ref(false);
const submitting = ref(false);
const actionError = ref("");
const fieldNote = ref("");
const planMode = ref(false);
const planKind = ref<"none" | "estimate" | "analyze">("none");
const analyzePrompt = ref(false);
const analyzeStatement = ref("");
const resultIndex = ref(0);
const detailsOpen = ref(false);
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
  const base = props.connectionId && !connection
    ? { keywordsOnly: true, note: "原连接已不在目录中：补全只提供关键字。字段不可用。请重新选择执行目标。", objects: [] }
    : metadataNote(connection, props.connectionId ? props.database : null);
  if (!fieldNote.value) return base;
  return { ...base, note: fieldNote.value };
});
const activeStatement = computed(() => run.value?.statements[resultIndex.value] ?? null);
const grid = computed(() => (activeStatement.value && activeStatement.value.columns.length ? gridModel(activeStatement.value) : null));
const gridEngine = computed(() => asEngine(run.value?.engine));

/* 一句话摘要（R4）：默认结果区只读这一句话，细节收进「详情」。 */
const summary = computed(() => {
  const current = run.value;
  if (!current) return "";
  const elapsed = `${current.elapsedMs} ms`;
  if (current.status === "running") return "执行中…";
  if (current.status === "cancel-requested") return "取消中…";
  if (current.status === "cancelled") return `已取消。已读到的结果保留 · ${elapsed}`;
  if (current.status === "unknown") return `连接中断，结果未知 · ${elapsed}`;
  const statements = current.statements;
  if (current.status === "error") {
    const failedIndex = statements.findIndex((statement) => statement.status === "error");
    const doneCount = statements.filter((statement) => statement.status === "ok").length;
    const skippedCount = statements.filter((statement) => statement.status === "skipped").length;
    const where = statements.length > 1 && failedIndex >= 0 ? `第 ${failedIndex + 1} 条语句失败` : "执行失败";
    const kept = doneCount ? `已完成 ${doneCount} 条的结果保留` : "";
    const skipped = skippedCount ? `其余 ${skippedCount} 条未执行` : "";
    const tail = [kept, skipped].filter(Boolean).join("，");
    return `${where}${tail ? `。${tail}` : ""} · ${elapsed}`;
  }
  if (statements.length === 1 && statements[0]) {
    const statement = statements[0];
    if (statement.status === "ok" && statement.columns.length) {
      return `${statement.rows.length} 行${statement.truncated ? "（还有未读行）" : ""} · ${elapsed}`;
    }
    if (statement.status === "ok") return `${statement.affected ?? 0} 行受影响 · ${elapsed}`;
  }
  const rowStatements = statements.filter((statement) => statement.status === "ok" && statement.columns.length).length;
  return `${statements.length} 条语句完成${rowStatements ? `，${rowStatements} 条返回了行` : ""} · ${elapsed}`;
});

/* 工具栏状态行：取消只呈现直白三态，机制名词不出现在这里。 */
const runStatusLine = computed(() => {
  const current = run.value;
  if (!current) return "";
  if (current.cancel === "requested") return "取消中…";
  if (current.cancel === "confirmed" || current.status === "cancelled") return "已取消";
  if (current.cancel === "failed") return "取消失败，仍在执行";
  const labels: Record<string, string> = { running: "执行中…", done: "完成", error: "执行失败", unknown: "结果未知" };
  return labels[current.status] ?? statusLabel(current.status);
});
const cancelButtonLabel = computed(() => {
  if (run.value?.cancel === "requested") return "取消中…";
  if (run.value?.cancel === "failed") return "再次取消";
  return "取消";
});
const detailsCaption = computed(() => {
  const current = run.value;
  return current ? `共 ${current.statements.length} 条语句` : "";
});
const operationGuide = "选择执行目标，编写 SQL，然后执行。有选中时只运行选中部分，无选中时执行光标所在语句。";
const planCaption = computed(() => (planKind.value === "analyze" ? "该语句已在服务器上执行" : "执行计划 · 只读 · 未使用 ANALYZE"));
const planView = computed(() => {
  const statement = activeStatement.value;
  if (!planMode.value || !statement?.columns.length) return null;
  return presentPlan(statement.columns, statement.rows);
});
const canContinue = computed(() => Boolean(running.value && activeStatement.value && activeStatement.value.rows.length > 0 && !activeStatement.value.truncated));
/* 没有目标、未连接、尚未读到对象时不另起说明；字段说明和含「失败」的原因仍显示。 */
const hintVisible = computed(() => Boolean(fieldNote.value) || hint.value.note.includes("失败"));

watch(
  () => run.value?.id,
  () => {
    resultIndex.value = preferredIndex(run.value?.statements ?? []);
  },
);

/* 失败时自动展开详情，让错误与「在编辑器中定位」入口直接可见（R3）。 */
watch(
  () => [run.value?.id, run.value?.status, run.value?.cancel] as const,
  ([, status, cancel]) => {
    if (status === "error" || cancel === "failed") detailsOpen.value = true;
  },
  { immediate: true },
);

/** 默认结果区展示的语句：优先最后一条有结果集的，否则最后一条。 */
function preferredIndex(statements: { columns: string[] }[]): number {
  if (!statements.length) return 0;
  for (let index = statements.length - 1; index >= 0; index -= 1) {
    if (statements[index]!.columns.length) return index;
  }
  return statements.length - 1;
}

const setSendRange = StateEffect.define<{ from: number; to: number } | null>();
const sendRangeMark = Decoration.mark({ class: "cm-send-range" });
const sendRangeField = StateField.define<Decoration.DecorationSet>({
  create() {
    return Decoration.none;
  },
  update(marks, transaction) {
    if (transaction.docChanged) return Decoration.none;
    for (const effect of transaction.effects) {
      if (!effect.is(setSendRange)) continue;
      const range = effect.value;
      if (!range || range.from >= range.to || range.to > transaction.state.doc.length) return Decoration.none;
      return Decoration.set([sendRangeMark.range(range.from, range.to)]);
    }
    return marks;
  },
  provide: (field) => EditorView.decorations.from(field),
});

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

async function sqlCompletion(context: CompletionContext) {
  const before = context.state.sliceDoc(0, context.pos);
  if (/--[^\n]*$/.test(before) || openDelimiter(before, props.engine ?? undefined)) return null;
  const word = context.matchBefore(/[A-Za-z0-9_.]*$/);
  if (!word || (word.from === word.to && !context.explicit)) return null;
  const connection = connectionById(catalogConnections(), props.connectionId);
  const ready = Boolean(props.connectionId && connection);
  const database = ready ? props.database : null;
  const meta = metadataNote(ready ? connection : null, database);
  const qualified = qualifierFor(word.text, meta.objects);
  if (qualified.kind === "plain") {
    fieldNote.value = "";
    const columns = ready && connection && database != null && connection.status === "connected" && !meta.keywordsOnly
      ? cachedColumns(connection.id, database)
      : [];
    const plan = completionPlan({
      prefix: word.text,
      connection: ready ? connection : null,
      database,
      columns,
    });
    return {
      from: word.from,
      filter: false,
      options: plan.items.map((item) => ({ label: item.label, type: item.kind, detail: item.detail })),
    };
  }
  const columnFrom = word.to - (qualified.kind === "object" ? qualified.columnPrefix.length : 0);
  if (!connection || database == null || connection.status !== "connected") {
    fieldNote.value = meta.note;
    return { from: columnFrom, filter: false, options: [] };
  }
  if (qualified.kind === "unknown") {
    fieldNote.value = unknownFieldNote(qualified.name);
    return { from: columnFrom, filter: false, options: [] };
  }
  if (qualified.kind === "ambiguous") {
    fieldNote.value = ambiguousFieldNote(qualified.name);
    return { from: columnFrom, filter: false, options: [] };
  }
  const engine = asEngine(props.engine ?? connection.engine);
  if (!engine) {
    fieldNote.value = "当前目标的引擎未知：字段不可用，不会猜测字段名。";
    return { from: columnFrom, filter: false, options: [] };
  }
  const loaded = await loadFields({
    connectionId: connection.id,
    database,
    engine,
    object: qualified.object,
    refresh: context.explicit,
  });
  if (loaded.error) {
    fieldNote.value = fieldFailureNote(qualified.object.label, loaded.error);
    return { from: columnFrom, filter: false, options: [] };
  }
  fieldNote.value = "";
  const columnPrefix = qualified.columnPrefix.toLowerCase();
  const options = loaded.columns
    .filter((name) => !columnPrefix || name.toLowerCase().startsWith(columnPrefix))
    .slice(0, 40)
    .map((name) => ({ label: name, type: "property", detail: qualified.object.label }));
  return { from: columnFrom, filter: false, options };
}

function editorExtensions() {
  return [
    basicSetup,
    sendRangeField,
    sql(),
    syntaxHighlighting(HighlightStyle.define([
      { tag: tags.keyword, color: "var(--tk-kw)" },
      { tag: tags.string, color: "var(--tk-str)" },
      { tag: tags.number, color: "var(--tk-num)" },
      { tag: tags.comment, color: "var(--tk-com)", fontStyle: "italic" },
      { tag: tags.operator, color: "var(--text-2)" },
      { tag: tags.name, color: "var(--tk-id)" },
    ])),
    shellTheme,
    Prec.high(
      keymap.of([
        { key: QUERY_EDITOR_KEYS[0], run: () => { void execute("auto"); return true; } },
        { key: QUERY_EDITOR_KEYS[1], run: () => { void execute("current"); return true; } },
        { key: QUERY_EDITOR_KEYS[2], run: () => { cancel(); return true; } },
        { key: QUERY_EDITOR_KEYS[3], run: startCompletion },
        { key: QUERY_EDITOR_KEYS[4], run: toggleComment },
        { key: "Mod-Shift-f", run: () => { formatEditor(); return true; } },
        { key: "Mod-Shift-e", run: () => { void requestPlan(); return true; } },
      ]),
    ),
    autocompletion({ override: [sqlCompletion] }),
    EditorView.updateListener.of((update) => {
      if (!alive) return;
      if (update.selectionSet || update.docChanged) {
        const range = update.state.selection.main;
        hasSelection.value = range.from !== range.to;
        const line = update.state.doc.lineAt(range.head);
        cursor.value = { line: line.number, column: range.head - line.from + 1 };
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

function formatEditor() {
  actionError.value = "";
  mountEditor();
  const editor = view;
  if (!editor) {
    actionError.value = "编辑器还没有准备好，请再试一次。";
    return;
  }
  const doc = editor.state.doc.toString();
  const range = editor.state.selection.main;
  const from = Math.min(range.from, range.to);
  const to = Math.max(range.from, range.to);
  const selected = from !== to;
  const formatted = formatDocument(doc, selected ? { from, to } : undefined);
  if (!formatted.ok) {
    actionError.value = formatted.reason;
    return;
  }
  if (formatted.sql === doc) return;
  const insertFrom = selected ? from : 0;
  const insertTo = selected ? to : doc.length;
  const suffixLength = doc.length - insertTo;
  const inserted = formatted.sql.slice(insertFrom, formatted.sql.length - suffixLength);
  editor.dispatch({
    changes: { from: insertFrom, to: insertTo, insert: inserted },
    selection: selected ? { anchor: insertFrom, head: insertFrom + inserted.length } : { anchor: formatted.sql.length },
  });
}

function targetProblem(purpose: "执行" | "查看执行计划"): string | null {
  const connection = connectionById(catalogConnections(), props.connectionId);
  if (!props.connectionId || props.database == null || !props.connectionName || !props.engine || !connection) {
    return props.connectionId && !connection ? "原连接已删除，请重新选择执行目标。" : "请选择执行目标（连接和数据库）。";
  }
  if (connection.status !== "connected") {
    return `${props.connectionName} 当前${CONNECTION_STATUS_TEXT[connection.status]}，无法${purpose}。`;
  }
  return null;
}

function markSendRange(editor: EditorView, from: number, to: number) {
  editor.dispatch({ effects: setSendRange.of(from < to ? { from, to } : null) });
}

async function requestPlan() {
  actionError.value = "";
  analyzePrompt.value = false;
  if (running.value || submitting.value) return;
  mountEditor();
  const editor = view;
  if (!editor) {
    actionError.value = "编辑器还没有准备好，请再试一次。";
    return;
  }
  const blocked = targetProblem("查看执行计划");
  if (blocked) {
    actionError.value = blocked;
    return;
  }
  const engine = props.engine ?? "postgres";
  const range = editor.state.selection.main;
  const doc = editor.state.doc.toString();
  const picked = planStatement(doc, { from: range.from, to: range.to, head: range.head }, engine);
  if (!picked.ok) {
    actionError.value = picked.reason;
    return;
  }
  const explained = hierarchicalSql(engine, picked.statement);
  if (!explained.ok) {
    actionError.value = explained.reason;
    return;
  }
  const from = range.from === range.to ? doc.indexOf(picked.statement) : Math.min(range.from, range.to);
  const to = from < 0 ? range.to : from + (range.from === range.to ? picked.statement.length : Math.abs(range.to - range.from));
  markSendRange(editor, Math.max(0, from), to);
  submitting.value = true;
  menuOpen.value = false;
  runMenuOpen.value = false;
  try {
    planMode.value = true;
    planKind.value = "estimate";
    await nextTick();
    await startTabRun({
      tabId: props.tabId,
      connectionId: props.connectionId!,
      connectionName: props.connectionName!,
      database: props.database!,
      engine,
      sql: explained.sql,
      limit: rowLimit(loadSettings().pageSize),
    });
    if (engine === "mysql" || engine === "mariadb") {
      await whenSettled(props.tabId);
      const current = tabRun(props.tabId);
      const failed = current?.status === "error" || current?.statements.some((statement) => statement.status === "error");
      const plain = explainSql(engine, picked.statement);
      if (failed && plain.ok && plain.sql !== explained.sql && planKind.value === "estimate") {
        await startTabRun({
          tabId: props.tabId,
          connectionId: props.connectionId!,
          connectionName: props.connectionName!,
          database: props.database!,
          engine,
          sql: plain.sql,
          limit: rowLimit(loadSettings().pageSize),
        });
      }
    }
  } catch (err) {
    planMode.value = false;
    planKind.value = "none";
    actionError.value = typeof err === "string" ? err : err instanceof Error ? err.message : "执行计划失败";
  } finally {
    submitting.value = false;
  }
}

function requestAnalyze() {
  actionError.value = "";
  if (running.value || submitting.value) return;
  mountEditor();
  const editor = view;
  if (!editor) {
    actionError.value = "编辑器还没有准备好，请再试一次。";
    return;
  }
  const blocked = targetProblem("查看执行计划");
  if (blocked) {
    actionError.value = blocked;
    return;
  }
  const engine = props.engine ?? "postgres";
  const range = editor.state.selection.main;
  const picked = planStatement(editor.state.doc.toString(), { from: range.from, to: range.to, head: range.head }, engine);
  if (!picked.ok) {
    actionError.value = picked.reason;
    return;
  }
  analyzeStatement.value = picked.statement;
  analyzePrompt.value = true;
}

function cancelAnalyze() {
  analyzePrompt.value = false;
  analyzeStatement.value = "";
}

async function confirmAnalyze() {
  const statement = analyzeStatement.value;
  analyzePrompt.value = false;
  analyzeStatement.value = "";
  const engine = props.engine ?? "postgres";
  const explained = analyzeSql(engine, statement);
  if (!explained.ok) {
    actionError.value = explained.reason;
    return;
  }
  const editor = view;
  if (editor) {
    const doc = editor.state.doc.toString();
    const at = doc.indexOf(statement);
    if (at >= 0) markSendRange(editor, at, at + statement.length);
  }
  submitting.value = true;
  menuOpen.value = false;
  runMenuOpen.value = false;
  try {
    planMode.value = true;
    planKind.value = "analyze";
    await nextTick();
    await startTabRun({
      tabId: props.tabId,
      connectionId: props.connectionId!,
      connectionName: props.connectionName!,
      database: props.database!,
      engine,
      sql: explained.sql,
      limit: rowLimit(loadSettings().pageSize),
    });
    await whenSettled(props.tabId);
    const failed = tabRun(props.tabId)?.statements.find((item) => item.status === "error");
    if (failed && planKind.value === "analyze") {
      actionError.value = `服务器不支持实际执行并分析，没有再发送会修改数据的语句。${failed.message}`;
    }
  } catch (err) {
    planMode.value = false;
    planKind.value = "none";
    actionError.value = typeof err === "string" ? err : err instanceof Error ? err.message : "执行计划失败";
  } finally {
    submitting.value = false;
  }
}

async function execute(scope: "auto" | "current" | "script") {
  actionError.value = "";
  analyzePrompt.value = false;
  if (running.value || submitting.value) return;
  mountEditor();
  const editor = view;
  if (!editor) {
    actionError.value = "编辑器还没有准备好，请再试一次。";
    return;
  }
  const blocked = targetProblem("执行");
  if (blocked) {
    actionError.value = blocked;
    return;
  }
  const engine = props.engine ?? "postgres";
  const doc = editor.state.doc.toString();
  const range = editor.state.selection.main;
  const picked = sqlToRun(doc, { from: range.from, to: range.to, head: range.head }, scope, engine);
  const inspected = inspectSql(scope === "selection" ? picked.sql : doc, engine);
  if (scope === "current") {
    if (!picked.sql.trim()) {
      actionError.value = inspected.refusal ?? "只有空白或注释，没有发送。";
      return;
    }
  } else if (inspected.refusal) {
    actionError.value = inspected.refusal;
    return;
  } else if (inspected.pieces.length === 0) {
    actionError.value = "只有空白或注释，没有发送。";
    return;
  }
  markSendRange(editor, picked.from, picked.to);
  submitting.value = true;
  menuOpen.value = false;
  runMenuOpen.value = false;
  planMode.value = false;
  planKind.value = "none";
  try {
    await nextTick();
    await startTabRun({
      tabId: props.tabId,
      connectionId: props.connectionId!,
      connectionName: props.connectionName!,
      database: props.database!,
      engine,
      sql: picked.sql,
      limit: rowLimit(loadSettings().pageSize),
    });
  } catch (err) {
    actionError.value = typeof err === "string" ? err : err instanceof Error ? err.message : "执行失败";
  } finally {
    submitting.value = false;
  }
}

function continueRead() {
  void continueTabRun(props.tabId);
}

watch(
  () => props.executeRequest,
  (value, previous) => {
    if (value == null || value === previous) return;
    void execute("auto");
  },
);

function cancel() {
  if (!running.value) return;
  void requestCancel(props.tabId);
}

/** 在编辑器中定位失败语句：选中对应文本并滚动到可见位置。 */
function locateInEditor(sqlText: string) {
  mountEditor();
  const editor = view;
  if (!editor) return;
  const needle = sqlText.trim();
  if (!needle) return;
  const doc = editor.state.doc.toString();
  const from = doc.indexOf(needle);
  if (from < 0) return;
  editor.dispatch({ selection: { anchor: from, head: from + needle.length }, scrollIntoView: true });
  editor.focus();
}

function toggleMenu() {
  if (running.value || submitting.value) return;
  menuOpen.value = !menuOpen.value;
  if (!menuOpen.value) return;
  const index = options.value.findIndex((item) => item.key === selectedKey.value);
  activeIndex.value = index >= 0 ? index : 0;
}

function toggleRunMenu() {
  if (running.value || submitting.value) return;
  runMenuOpen.value = !runMenuOpen.value;
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

function onRunMenuKey(event: KeyboardEvent) {
  if (event.key !== "Escape") return;
  event.preventDefault();
  runMenuOpen.value = false;
}

function onPointerDown(event: PointerEvent) {
  if (menuOpen.value) {
    const root = targetRoot.value;
    if (!root || !(event.target instanceof Node) || !root.contains(event.target)) menuOpen.value = false;
  }
  if (runMenuOpen.value) {
    const root = runRoot.value;
    if (!root || !(event.target instanceof Node) || !root.contains(event.target)) runMenuOpen.value = false;
  }
}

watch(running, (value) => {
  if (!value) return;
  menuOpen.value = false;
  runMenuOpen.value = false;
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

watch(
  () => [props.connectionId, props.database, props.connectionStatus] as const,
  () => {
    fieldNote.value = "";
  },
);

onMounted(() => {
  document.addEventListener("pointerdown", onPointerDown);
  mountEditor();
});

onBeforeUnmount(() => {
  alive = false;
  stopResize?.();
  document.removeEventListener("pointerdown", onPointerDown);
  view?.destroy();
});
</script>

<template>
  <section class="query-pane" aria-label="查询" :data-tab-id="tabId" :style="{ '--editor-share': `${editorPercent}%` }">
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
      <span class="grow"></span>
      <button class="btn" type="button" data-action="format" title="格式化选中的 SQL；没有选中时格式化全部" @click="formatEditor">格式化</button>
      <button class="btn" type="button" data-action="plan" title="查看一条语句的执行计划，不使用 ANALYZE" :disabled="running || submitting" @click="requestPlan">执行计划</button>
      <button class="btn" type="button" data-action="analyze" title="在服务器上实际执行并分析这一条语句" :disabled="running || submitting" @click="requestAnalyze">实际执行并分析</button>
      <div v-if="!running" ref="runRoot" class="run-group">
        <button class="btn primary run-main" type="button" data-action="run" :disabled="submitting" @click="execute('auto')"><span aria-hidden="true">▶</span>{{ submitting ? "正在执行…" : hasSelection ? "执行选中" : "执行" }} <ChordKeys chord="Mod+Enter" /></button>
        <button
          class="btn run-more"
          type="button"
          data-action="run-menu"
          aria-label="更多执行方式"
          aria-haspopup="menu"
          :aria-expanded="runMenuOpen"
          :disabled="submitting"
          @click="toggleRunMenu"
        >
          <svg class="tg-caret" viewBox="0 0 24 24" aria-hidden="true"><path d="m6 9 6 6 6-6" /></svg>
        </button>
        <div v-if="runMenuOpen" class="run-menu" role="menu" aria-label="执行方式" @keydown="onRunMenuKey">
          <button class="run-item" type="button" role="menuitem" data-action="run-current" @click="execute('current')">
            <span>执行当前语句</span>
            <small>只执行光标所在的语句</small>
          </button>
          <button class="run-item" type="button" role="menuitem" data-action="run-script" @click="execute('script')">
            <span>执行脚本</span>
            <small>按顺序执行编辑器里的全部语句</small>
          </button>
        </div>
      </div>
      <button v-else class="btn danger" type="button" data-action="cancel" :disabled="run?.cancel === 'requested'" @click="cancel">{{ cancelButtonLabel }}</button>
      <span v-if="run" class="q-note" role="status">{{ runStatusLine }} · {{ run.elapsedMs }} ms</span>
    </div>
    <div v-if="analyzePrompt" class="analyze-dialog" role="dialog" aria-modal="true" aria-label="实际执行并分析">
      <p>这条语句会在服务器上执行。</p>
      <div class="analyze-actions">
        <button class="btn" type="button" data-action="analyze-cancel" @click="cancelAnalyze">取消</button>
        <button class="btn" type="button" data-action="analyze-confirm" @click="confirmAnalyze">继续</button>
      </div>
    </div>
    <p v-if="notice" class="q-banner">{{ notice }}</p>
    <p v-if="actionError" class="q-banner" role="alert">{{ actionError }}</p>
    <p v-if="hintVisible" class="q-hint" :data-keywords-only="hint.keywordsOnly" :title="hint.note">{{ hint.note }}</p>
    <div ref="host" class="cm-host"></div>
    <div class="editor-position"><span class="q-guide">{{ operationGuide }}</span><span class="mono">Ln {{ cursor.line }}, Col {{ cursor.column }}</span></div>
    <div class="query-splitter" role="separator" tabindex="0" aria-label="调整编辑器与结果区域高度" aria-orientation="horizontal" :aria-valuenow="editorPercent" :aria-valuemin="20" :aria-valuemax="70" @pointerdown="resizeEditor" @keydown.up.prevent="editorPercent = Math.max(20, editorPercent - 5)" @keydown.down.prevent="editorPercent = Math.min(70, editorPercent + 5)"><span></span></div>
    <section class="q-results" aria-label="执行结果">
      <div class="results-heading"><span><Icon name="table" />结果</span><span v-if="run" class="q-summary" :data-status="run.status">{{ summary }}</span><span v-else class="muted">等待执行</span><span class="grow"></span><span class="muted">只读结果</span></div>
      <template v-if="run">
        <div v-if="run.statements.length > 1" class="result-tabs" role="tablist" aria-label="语句结果"><button v-for="(statement, index) in run.statements" :key="index" role="tab" :aria-selected="resultIndex === index" @click="resultIndex = index">结果 {{ index + 1 }} <span>{{ statement.columns.length ? `${statement.rows.length} 行` : statusLabel(statement.status) }}</span></button></div>
        <p v-if="run.cancel === 'requested'" class="q-note" role="status">取消中…数据库确认后立即停止。</p>
        <p v-else-if="run.cancel === 'confirmed' || run.status === 'cancelled'" class="q-note" role="status">已取消。已读到的结果保留。</p>
        <p v-else-if="run.cancel === 'failed'" class="q-note" role="alert">取消失败，查询仍在执行。可以再次取消。</p>
        <template v-if="activeStatement">
          <p v-if="activeStatement.status === 'unknown'" class="q-note">写入结果未知。不会自动重放，草稿保留。</p>
          <p v-else-if="activeStatement.status === 'skipped'" class="q-note">这条语句没有执行。</p>
          <p v-else-if="activeStatement.status === 'ok' && activeStatement.message" class="q-note">{{ activeStatement.message }}</p>
          <p v-if="canContinue" class="q-note"><button class="btn sm" type="button" data-action="continue-read" @click="continueRead">继续读取</button></p>
          <div v-if="planMode && planView" class="plan-view" data-result="plan">
            <p class="plan-caption">{{ planCaption }}</p>
            <ol v-if="planView.kind === 'tree'" class="plan-tree">
              <li v-for="(node, nodeIndex) in planView.nodes" :key="nodeIndex" :style="{ paddingLeft: `${node.depth * 16}px` }">
                <span>{{ node.operation }}</span>
                <span v-if="node.rows">估计 {{ node.rows }} 行</span>
                <span v-if="node.extra">{{ node.extra }}</span>
              </li>
            </ol>
            <template v-else>
              <table>
                <thead><tr><th v-for="(column, columnIndex) in activeStatement.columns" :key="`${column}-${columnIndex}`">{{ column }}</th></tr></thead>
                <tbody>
                  <tr v-for="(row, rowIndex) in activeStatement.rows" :key="rowIndex">
                    <td v-for="(cell, cellIndex) in row" :key="cellIndex">{{ cell === null ? "NULL" : cell }}</td>
                  </tr>
                </tbody>
              </table>
              <p>没有层次结果</p>
            </template>
          </div>
          <QueryResultGrid
            v-else-if="grid"
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
        <details class="disclosure q-details" :open="detailsOpen">
          <summary @click.prevent="detailsOpen = !detailsOpen">详情 <span class="muted">{{ detailsCaption }}</span></summary>
          <div v-if="detailsOpen" class="disclosure-body">
            <ol class="q-timeline">
              <li v-for="(statement, index) in run.statements" :key="`${run.id}-${index}`" class="q-step" :class="`st-${statement.status}`">
                <div class="q-step-head">
                  <button class="q-step-pick" type="button" :data-statement-index="index" :aria-current="index === resultIndex" @click="resultIndex = index">
                    第 {{ index + 1 }} 条 · {{ statusLabel(statement.status) }}
                  </button>
                  <button v-if="statement.status === 'error'" class="btn sm" type="button" data-action="locate" @click="locateInEditor(statement.sql)">在编辑器中定位</button>
                </div>
                <pre class="q-sql">{{ statement.sql }}</pre>
                <p v-if="statement.status === 'error'" class="q-step-msg" role="alert">{{ statement.message }}</p>
                <p v-else-if="statement.status === 'unknown'" class="q-step-msg">{{ statement.message ? `${statement.message}。` : "" }}写入结果未知，不会自动重放。</p>
                <p v-else-if="statement.status === 'skipped'" class="q-step-msg">前面的语句失败后，这条语句没有发送到数据库。</p>
                <p v-else-if="statement.status === 'cancelled'" class="q-step-msg">数据库已确认取消这条语句。</p>
                <p v-else-if="statement.message" class="q-step-msg">{{ statement.message }}</p>
              </li>
            </ol>
            <div v-if="run.cancelMessage" class="q-cancel-detail">
              <p>{{ run.cancelMessage }}</p>
              <p v-if="run.viaSsh && run.freshChannel" class="muted">这个连接经 SSH 转发，取消是另开一条通道送达数据库的。</p>
            </div>
            <p v-if="run.pollError" class="q-step-msg" role="alert">状态刷新失败：{{ run.pollError }}</p>
          </div>
        </details>
      </template>
    </section>
  </section>
</template>

<style scoped>
.query-pane { display: flex; flex-direction: column; flex: 1; min-height: 0; }
.q-toolbar { display: flex; gap: var(--sp-2); align-items: center; flex-wrap: wrap; padding: var(--sp-2) var(--sp-3); border-bottom: 1px solid var(--line); }
.q-target { position: relative; min-width: 0; }
.target-btn { display: flex; align-items: center; gap: var(--sp-2); height: var(--control-h); max-width: 420px; min-width: 0; padding: 0 var(--sp-2); border: 1px solid var(--line-2); border-radius: var(--r); background: var(--panel); font-size: var(--fs-sm); transition: border-color var(--dur-fast) var(--ease), box-shadow var(--dur-fast) var(--ease), background var(--dur-fast) var(--ease); }
.target-btn:hover:not(:disabled) { border-color: var(--text-3); background: var(--hover); }
.target-btn.on { border-color: var(--accent); box-shadow: 0 0 0 2px var(--ring); }
.target-btn.none { border-style: dashed; color: var(--text-2); }
.target-btn:disabled { opacity: 0.7; cursor: not-allowed; }
.tg-name { font-weight: 600; white-space: nowrap; }
.tg-db { display: inline-flex; align-items: center; gap: var(--sp-1); min-width: 0; color: var(--text-2); }
.tg-db span:last-child { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.tg-sep { color: var(--text-3); }
.tg-caret { width: 14px; height: 14px; margin-left: auto; color: var(--text-3); fill: none; stroke: currentColor; stroke-width: 1.8; stroke-linecap: round; stroke-linejoin: round; flex-shrink: 0; }
.target-btn :deep(.i) { width: 14px; height: 14px; color: var(--text-3); }
.eng { --engc: var(--text-2); display: inline-grid; place-items: center; width: 20px; height: 20px; border-radius: var(--r-sm); font: 700 var(--fs-xs)/1 var(--font); color: var(--engc); background: color-mix(in srgb, var(--engc) 14%, transparent); flex-shrink: 0; }
.eng-postgres { --engc: var(--eng-postgres); }
.eng-mysql { --engc: var(--eng-mysql); }
.eng-mariadb { --engc: var(--eng-mariadb); }
.eng-sqlite { --engc: var(--eng-sqlite); }
.target-menu, .run-menu { position: absolute; z-index: 30; top: calc(100% + var(--sp-1)); left: 0; min-width: 280px; max-width: 420px; max-height: 320px; overflow: auto; padding: var(--sp-1); background: var(--panel); border: 1px solid var(--line-2); border-radius: var(--r-md); box-shadow: var(--shadow); animation: pop-in var(--dur-fast) var(--ease-out); }
.target-empty { margin: 0; padding: var(--sp-2); font-size: var(--fs-sm); color: var(--text-3); }
.target-option { display: flex; align-items: center; gap: var(--sp-2); width: 100%; min-height: 44px; padding: var(--sp-2); border-radius: var(--r); text-align: left; }
.target-option.on { background: var(--hover); }
.target-option.picked { background: var(--accent-soft); }
.mi-copy { display: flex; flex-direction: column; min-width: 0; flex: 1; }
.mi-title { display: flex; align-items: center; gap: var(--sp-1); min-width: 0; font-size: var(--fs-sm); }
.mi-title b { font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.mi-title :deep(.i) { width: 13px; height: 13px; color: var(--text-3); flex-shrink: 0; }
.target-option small { font-size: var(--fs-xs); color: var(--text-3); }
.target-option .check { width: 14px; height: 14px; margin-left: auto; color: var(--accent); flex-shrink: 0; }
.run-group { position: relative; display: flex; }
.run-group .run-main { border-radius: var(--r) 0 0 var(--r); }
.run-group .run-more { border-radius: 0 var(--r) var(--r) 0; margin-left: -1px; padding: 0 var(--sp-2); }
.run-group .run-more .tg-caret { margin-left: 0; color: var(--text-2); }
.run-menu { min-width: 220px; }
.run-item { display: flex; flex-direction: column; align-items: flex-start; gap: 2px; width: 100%; padding: var(--sp-2); border-radius: var(--r); text-align: left; font-size: var(--fs-sm); }
.run-item:hover { background: var(--hover); }
.run-item small { font-size: var(--fs-xs); color: var(--text-3); }
.q-note, .q-hint, .q-banner { margin: 0; font-size: var(--fs-sm); color: var(--text-2); }
.q-hint, .q-banner { padding: var(--sp-1) var(--sp-3); }
.q-banner { color: var(--text); background: var(--sunken); }
.q-results { min-height: 120px; overflow: auto; border-top: 1px solid var(--line); padding: var(--sp-3); display: flex; flex-direction: column; gap: var(--sp-2); }
.q-summary { margin: 0; font-size: var(--fs-md); font-weight: 600; color: var(--text); }
.q-summary[data-status="error"] { color: var(--err); }
.q-summary[data-status="unknown"] { color: var(--warn); }
.q-details { font-size: var(--fs-sm); }
.q-timeline { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: var(--sp-3); }
.q-step { display: flex; flex-direction: column; gap: var(--sp-1); }
.q-step-head { display: flex; align-items: center; gap: var(--sp-2); }
.q-step-pick { font-size: var(--fs-sm); font-weight: 600; color: var(--text-2); border-radius: var(--r-sm); padding: 0; }
.q-step-pick:hover { color: var(--accent-text); }
.q-step-pick[aria-current="true"] { color: var(--accent-text); }
.q-step.st-error .q-step-pick { color: var(--err); }
.q-step-msg { margin: 0; font-size: var(--fs-sm); color: var(--text-2); }
.q-step.st-error .q-step-msg { color: var(--err); }
.q-sql { margin: 0; white-space: pre-wrap; font-family: var(--mono); font-size: var(--fs-xs); color: var(--text-2); }
.q-cancel-detail { margin-top: var(--sp-3); padding-top: var(--sp-2); border-top: 1px solid var(--line); font-size: var(--fs-sm); color: var(--text-2); }
.q-cancel-detail p { margin: 0 0 var(--sp-1); }
.query-pane .q-toolbar { background: var(--panel); min-height: 46px; }
.query-pane .cm-host { flex: 0 1 var(--editor-share); min-height: 90px; }
.query-pane :deep(.cm-content) { padding-top: 14px; line-height: 1.8; }
.query-pane :deep(.cm-line) { padding-left: 16px; }
.query-pane :deep(.cm-gutters) { background: var(--panel-2); color: var(--text-3); border-right: 1px solid var(--line); min-width: 44px; }
.query-pane :deep(.cm-activeLine), .query-pane :deep(.cm-activeLineGutter) { background: var(--sel); }
.query-pane :deep(.cm-activeLineGutter) { color: var(--text); }
.query-pane :deep(.cm-cursor) { border-left-color: var(--accent); }
.query-pane :deep(.cm-selectionBackground), .query-pane :deep(.cm-focused .cm-selectionBackground) { background: var(--sel-strong) !important; }
.editor-position { display: flex; justify-content: space-between; align-items: center; gap: 10px; padding: 4px 16px; color: var(--text-3); font-size: 10px; }
.q-guide { min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
.query-splitter { height: 9px; flex-shrink: 0; display: grid; place-items: center; background: var(--panel-2); border-block: 1px solid var(--line); cursor: row-resize; touch-action: none; transition: background var(--dur-fast) var(--ease); }
.query-splitter span { height: 3px; width: 34px; background: var(--line-2); border-radius: var(--r-full); transition: background var(--dur-fast) var(--ease); }
.query-splitter:hover, .query-splitter:focus-visible { background: var(--accent-soft); }
.query-splitter:hover span, .query-splitter:focus-visible span { background: var(--accent); }
.query-pane .q-results { flex: 1; min-height: 130px; padding: 0; gap: 0; border: 0; }
.results-heading { display: flex; align-items: center; gap: 20px; padding: 8px 16px; border-bottom: 1px solid var(--line); font-size: 11px; flex-shrink: 0; background: var(--panel-2); }
.plan-view { padding: 12px 16px; overflow: auto; }
.plan-caption { margin: 0 0 8px; font-size: var(--fs-sm); color: var(--text-2); }
.plan-tree { list-style: none; margin: 0; padding: 0; font-family: var(--mono); font-size: var(--fs-xs); }
.plan-tree li { display: flex; gap: 8px; padding-block: 2px; }
.analyze-dialog { margin: var(--sp-2) var(--sp-3); padding: var(--sp-3); border: 1px solid var(--line-2); border-radius: var(--r-md); background: var(--panel); }
.analyze-dialog p { margin: 0 0 var(--sp-2); }
.analyze-actions { display: flex; gap: var(--sp-2); }
.query-pane :deep(.cm-send-range) { background: color-mix(in srgb, var(--accent) 22%, transparent); border-radius: 2px; }
.plan-view table { border-collapse: collapse; font-family: var(--mono); font-size: var(--fs-xs); }
.plan-view th, .plan-view td { border: 1px solid var(--line); padding: 4px 8px; text-align: left; vertical-align: top; }
.results-heading > span:first-child { display: inline-flex; align-items: center; gap: 8px; font-weight: 600; }
.results-heading .q-summary { font-size: 11px; font-weight: 400; }
.result-tabs { display: flex; flex-shrink: 0; gap: 2px; padding: 0 12px; border-bottom: 1px solid var(--line); overflow: auto; background: var(--panel-2); }
.result-tabs button { padding: 8px 12px; font-size: 11px; white-space: nowrap; border-bottom: 2px solid transparent; border-radius: var(--r) var(--r) 0 0; transition: color var(--dur-fast) var(--ease), border-color var(--dur-fast) var(--ease); }
.result-tabs button:hover { color: var(--text); border-bottom-color: var(--line-2); }
.result-tabs button[aria-selected="true"] { border-color: var(--accent); color: var(--accent-text); }
.result-tabs span { font-size: 10px; color: var(--text-3); margin-left: 6px; }
.q-details { flex-shrink: 0; border: 0; border-top: 1px solid var(--line); border-radius: 0; }
.q-details .disclosure-body { max-height: 180px; overflow: auto; }
.run-main :deep(kbd) { background: transparent; color: inherit; opacity: .75; border-color: currentColor; }
@media (max-width: 800px) { .run-main :deep(.kbd) { display: none; } .target-btn { max-width: 290px; } .q-toolbar .conn-state { display: none; } .editor-position > span:first-child { overflow: hidden; white-space: nowrap; text-overflow: ellipsis; } }
</style>
