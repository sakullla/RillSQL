<script setup lang="ts">
import { autocompletion, startCompletion, type CompletionContext } from "@codemirror/autocomplete";
import { toggleComment } from "@codemirror/commands";
import { sql } from "@codemirror/lang-sql";
import { HighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { linter } from "@codemirror/lint";
import { tags } from "@lezer/highlight";
import { EditorState, Prec, StateEffect, StateField } from "@codemirror/state";
import { Decoration, type DecorationSet, EditorView, keymap } from "@codemirror/view";
import { basicSetup } from "codemirror";
import ChordKeys from "../shell/ChordKeys.vue";
import { computed, nextTick, onBeforeUnmount, onMounted, ref, shallowRef, watch } from "vue";
import { catalogConnections } from "../catalog/store";
import { engineMeta } from "../connections/model";
import { queryHistory, querySavedList, querySavedSave } from "../history/api";
import { deletedConnectionNotice, elapsedLabel, openPlan, statusBadge, statusLabel as historyStatusLabel, type HistoryEntry, type OpenPlan } from "../history/plan";
import { openSaved, savedName, savedNameError, savedNeedsTarget, type SavedQuery } from "../history/saved";
import QueryResultGrid from "../grid/QueryResultGrid.vue";
import type { Engine } from "../grid/types";
import Icon from "../shell/Icon.vue";
import { tabHasPending } from "../shell/close-guard";
import { loadSettings } from "../shell/settings";
import { CONNECTION_STATUS_TEXT, type ConnectionStatus } from "../shell/status-text";
import type { QueryTarget } from "../shell/session";
import { ambiguousFieldNote, cachedColumns, fieldFailureNote, loadFields, qualifierFor, unknownFieldNote } from "./columns";
import { completionPlan, connectionById, metadataNote, statementContext, targetDetail, targetKey, targetOptions, type TargetOption } from "./complete";
import { resolveEditor } from "./editor";
import { consumeExecuteRequest } from "./execute-request";
import { fileOpenPlan, readSqlFile, saveSqlFile, sqlFileProblem, sqlSaveName, SQL_FILE_ACCEPT } from "./file-io";
import { formatDocument } from "./format";
import { sqlLintDiagnostics } from "./lint";
import { analyzeSql, explainSql, hierarchicalSql, planStatement, presentPlan } from "./plan";
import { gridModel } from "./rows";
import { continueTabRun, requestCancel, startTabRun, tabRun, whenSettled } from "./runtime";
import { QUERY_EDITOR_KEYS, rowLimit, sqlToRun } from "./scope";
import { inspectSql, locateStatement, openDelimiter } from "./split";
import { findVariables, substituteVariables, type VariableInput } from "./variables";

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
  executed: [tabId: string, sql: string];
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
const analyzeAt = ref<{ from: number; to: number } | null>(null);
/* 执行前变量面板：发现占位符时暂停执行，等用户填值或取消。 */
const variablePrompt = ref(false);
const variableFields = ref<VariableInput[]>([]);
const variableValues = ref<Record<string, string>>({});
const pendingExecute = ref<{ sql: string; from: number; to: number } | null>(null);
const resultIndex = ref(0);
const detailsOpen = ref(false);
const historyRoot = ref<HTMLElement | null>(null);
const savedRoot = ref<HTMLElement | null>(null);
const historyOpen = ref(false);
const savedOpen = ref(false);
const historyEntries = shallowRef<HistoryEntry[]>([]);
const historyLoading = ref(false);
const historyError = ref("");
const savedEntries = shallowRef<SavedQuery[]>([]);
const savedLoading = ref(false);
const savedError = ref("");
const savedNameValue = ref("");
const savedWriteError = ref("");
const savedWriteMessage = ref("");
const savedBusy = ref(false);
const openFileInput = ref<HTMLInputElement | null>(null);
const saveFileBusy = ref(false);
const saveFileMessage = ref("");
const saveFileError = ref("");
let view: EditorView | undefined;
let alive = true;

const run = computed(() => tabRun(props.tabId));
const running = computed(() => run.value?.status === "running" || run.value?.status === "cancel-requested");
const options = computed(() => targetOptions(catalogConnections()));
const knownIds = computed(() => new Set(catalogConnections().map((item) => item.id)));
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
  if (current.pollError) return "状态刷新失败";
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

/** jsdom 的 Range 没有 getClientRects，选区测量不能变成未处理异常。 */
function ensureRangeClientRects(): void {
  if (typeof Range === "undefined") return;
  const proto = Range.prototype as Range & { getClientRects?: () => DOMRectList };
  if (typeof proto.getClientRects === "function") return;
  proto.getClientRects = () => [] as unknown as DOMRectList;
}

const setSendRange = StateEffect.define<{ from: number; to: number } | null>();
const sendRangeMark = Decoration.mark({ class: "cm-send-range" });
const sendRangeField = StateField.define<DecorationSet>({
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

/* 编辑器走 sunken 下沉井表面（tokens：编辑器/井下沉），与结果区的 panel 面板拉开层级。 */
const shellTheme = EditorView.theme({
  "&": { backgroundColor: "var(--surface-sunken)", color: "var(--text)", height: "100%" },
  ".cm-content": { fontFamily: "var(--mono)", fontSize: "var(--editor-font)", caretColor: "var(--text)" },
  ".cm-gutters": { backgroundColor: "var(--surface-sunken)", color: "var(--text-3)", borderRight: "1px solid var(--line)" },
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
  /* 语句上下文：取当前语句起点到光标的前缀，解析子句与 FROM/JOIN 别名；定位不到语句时回退原有行为。 */
  const located = locateStatement(context.state.doc.toString(), context.pos, props.engine ?? undefined);
  const statementPiece = located.piece && !located.refusal ? located.piece : null;
  const parsed = statementPiece ? statementContext(before.slice(statementPiece.start), meta.objects) : null;
  const qualified = qualifierFor(word.text, meta.objects, parsed?.aliases);
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
      context: parsed ? { clause: parsed.clause } : undefined,
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
    /* 结构 lint：只画内联波浪线（悬停显示消息），不加 gutter、不开诊断面板；每次调用读当前引擎。 */
    linter((editor) => sqlLintDiagnostics(editor.state.doc.toString(), props.engine ?? "postgres")),
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
  ensureRangeClientRects();
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
  markSendRange(editor, picked.from, picked.to);
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
  analyzeAt.value = { from: picked.from, to: picked.to };
  analyzePrompt.value = true;
}

function cancelAnalyze() {
  analyzePrompt.value = false;
  analyzeStatement.value = "";
  analyzeAt.value = null;
}

async function confirmAnalyze() {
  const statement = analyzeStatement.value;
  const at = analyzeAt.value;
  analyzePrompt.value = false;
  analyzeStatement.value = "";
  analyzeAt.value = null;
  const engine = props.engine ?? "postgres";
  const explained = analyzeSql(engine, statement);
  if (!explained.ok) {
    actionError.value = explained.reason;
    return;
  }
  const editor = view;
  if (editor && at) markSendRange(editor, at.from, at.to);
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
  if (tabHasPending(props.tabId)) {
    actionError.value = "结果里还有未提交的更改。先提交或放弃这些更改，再重新执行，避免它们被新结果冲掉。";
    return;
  }
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
  if (picked.mode === "current") {
    const located = locateStatement(doc, range.head, engine);
    if (located.refusal) {
      actionError.value = located.refusal;
      return;
    }
    if (!located.piece) {
      const document = inspectSql(doc, engine);
      actionError.value = document.refusal ?? (document.notices.join("") || "只有空白或注释，没有发送。");
      return;
    }
  }
  const inspected = inspectSql(picked.mode === "script" ? doc : picked.sql, engine);
  if (inspected.refusal) {
    actionError.value = inspected.refusal;
    return;
  }
  if (inspected.pieces.length === 0) {
    actionError.value = inspected.notices.join("") || "只有空白或注释，没有发送。";
    return;
  }
  const variables = findVariables(picked.sql);
  if (variables.length) {
    openVariablePrompt(variables, picked.sql, picked.from, picked.to);
    return;
  }
  await sendExecute(picked.sql, picked.from, picked.to);
}

/** 标记原文的 send-range、发送 sqlText 并等待结束；变量代换后的发送与直发共用这一段。 */
async function sendExecute(sqlText: string, from: number, to: number) {
  if (tabHasPending(props.tabId)) {
    actionError.value = "结果里还有未提交的更改。先提交或放弃这些更改，再重新执行，避免它们被新结果冲掉。";
    return;
  }
  mountEditor();
  const editor = view;
  if (!editor) {
    actionError.value = "编辑器还没有准备好，请再试一次。";
    return;
  }
  markSendRange(editor, from, to);
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
      engine: props.engine ?? "postgres",
      sql: sqlText,
      limit: rowLimit(loadSettings().pageSize),
    });
    // 成功发出才算“执行过”；之后关闭标签不会再提示这份草稿会丢失。
    emit("executed", props.tabId, sqlText);
  } catch (err) {
    actionError.value = typeof err === "string" ? err : err instanceof Error ? err.message : "执行失败";
  } finally {
    submitting.value = false;
  }
}

/** 发现占位符时暂停执行并弹面板：submitting 维持已占用的状态，面板打开期间沿用既有阻塞，避免重入。 */
function openVariablePrompt(variables: VariableInput[], sqlText: string, from: number, to: number) {
  variablePrompt.value = true;
  variableFields.value = variables;
  variableValues.value = Object.fromEntries(variables.map((item) => [item.key, ""]));
  pendingExecute.value = { sql: sqlText, from, to };
  submitting.value = true;
}

/** 取消：不执行、不报错，只关面板并释放阻塞。 */
function cancelVariables() {
  variablePrompt.value = false;
  variableFields.value = [];
  variableValues.value = {};
  pendingExecute.value = null;
  submitting.value = false;
}

/** 继续：用代换后的 SQL 走原有执行流程。编辑器草稿与 send-range 标记保持原文范围，历史记录记代换后的 SQL。 */
async function confirmVariables() {
  const pending = pendingExecute.value;
  if (!pending) return;
  variablePrompt.value = false;
  const sqlText = substituteVariables(pending.sql, variableValues.value);
  variableFields.value = [];
  variableValues.value = {};
  pendingExecute.value = null;
  await sendExecute(sqlText, pending.from, pending.to);
}

function continueRead() {
  void continueTabRun(props.tabId);
}

watch(
  () => props.executeRequest,
  (value) => {
    // immediate：命令面板请求可能先于本标签挂载发出；水位保证每个请求只消费一次。
    if (!consumeExecuteRequest(value)) return;
    void execute("auto");
  },
  { immediate: true },
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

function failureMessage(err: unknown, fallback: string): string {
  if (typeof err === "string" && err) return err;
  if (err instanceof Error && err.message) return err.message;
  return fallback;
}

function toggleHistoryPanel() {
  if (running.value || submitting.value) return;
  historyOpen.value = !historyOpen.value;
  if (!historyOpen.value) return;
  savedOpen.value = false;
  void loadHistory();
}

async function loadHistory() {
  historyLoading.value = true;
  try {
    historyEntries.value = await queryHistory();
    historyError.value = "";
  } catch (err) {
    historyError.value = failureMessage(err, "读不了查询历史");
  } finally {
    historyLoading.value = false;
  }
}

function toggleSavedPanel() {
  if (running.value || submitting.value) return;
  savedOpen.value = !savedOpen.value;
  if (!savedOpen.value) return;
  historyOpen.value = false;
  void loadSaved();
}

async function loadSaved() {
  savedLoading.value = true;
  try {
    savedEntries.value = await querySavedList();
    savedError.value = "";
  } catch (err) {
    savedError.value = failureMessage(err, "读不了保存的查询");
  } finally {
    savedLoading.value = false;
  }
}

/** 历史或保存查询的载入计划：全量替换编辑器文本成为草稿（updateListener 会发出 update:draft），绝不执行。 */
function applyOpenPlan(plan: OpenPlan) {
  actionError.value = "";
  mountEditor();
  const editor = view;
  if (!editor) {
    actionError.value = "编辑器还没有准备好，请再试一次。";
    return;
  }
  if (editor.state.doc.toString() !== plan.sql) {
    editor.dispatch({ changes: { from: 0, to: editor.state.doc.length, insert: plan.sql } });
  }
  if (plan.needsTarget) {
    actionError.value = deletedConnectionNotice();
    return;
  }
  if (plan.connectionId == null || plan.connectionId === props.connectionId) return;
  const connection = connectionById(catalogConnections(), plan.connectionId);
  if (!connection) return;
  emit("target", {
    connectionId: connection.id,
    connectionName: plan.connectionName,
    database: plan.database ?? connection.defaultDatabase ?? "",
    engine: plan.engine,
    status: connection.status,
  });
}

function chooseHistory(entry: HistoryEntry) {
  historyOpen.value = false;
  applyOpenPlan(openPlan(entry, knownIds.value));
}

function chooseSaved(entry: SavedQuery) {
  savedOpen.value = false;
  applyOpenPlan(openSaved(entry, knownIds.value));
}

/** 从磁盘打开 .sql 文件：只把文件内容写进当前标签的草稿，不会执行，也不新开标签。 */
function pickSqlFile() {
  actionError.value = "";
  openFileInput.value?.click();
}

async function onSqlFile(event: Event) {
  const input = event.target as HTMLInputElement;
  const file = input.files?.[0];
  input.value = "";
  if (!file) return;
  const problem = sqlFileProblem(file);
  if (problem) {
    actionError.value = problem;
    return;
  }
  try {
    applyOpenPlan(fileOpenPlan(await readSqlFile(file)));
  } catch (err) {
    actionError.value = `文件没有读出来：${failureMessage(err, "读取失败")}`;
  }
}

/** 把草稿保存成磁盘 .sql 文件：原生先弹保存对话框再写文件，浏览器直接下载。 */
async function saveSqlToFile() {
  if (saveFileBusy.value) return;
  saveFileMessage.value = "";
  saveFileError.value = "";
  mountEditor();
  const editor = view;
  if (!editor) {
    saveFileError.value = "编辑器还没有准备好，请再试一次。";
    return;
  }
  saveFileBusy.value = true;
  try {
    const outcome = await saveSqlFile({ defaultPath: sqlSaveName(), contents: editor.state.doc.toString() });
    if (outcome.status === "cancelled") return;
    if (outcome.status === "saved") saveFileMessage.value = outcome.message;
    else saveFileError.value = outcome.message;
  } finally {
    saveFileBusy.value = false;
  }
}

async function saveNamedQuery() {
  savedWriteError.value = "";
  savedWriteMessage.value = "";
  const nameError = savedNameError(savedNameValue.value);
  if (nameError) {
    savedWriteError.value = nameError;
    return;
  }
  mountEditor();
  const editor = view;
  if (!editor) {
    savedWriteError.value = "编辑器还没有准备好，请再试一次。";
    return;
  }
  savedBusy.value = true;
  try {
    const written = await querySavedSave({
      sql: editor.state.doc.toString(),
      connectionId: props.connectionId ?? "",
      connectionName: props.connectionName ?? "",
      databaseName: props.database ?? "",
      engine: props.engine ?? "postgres",
      name: savedName(savedNameValue.value),
    });
    savedWriteMessage.value = written.created
      ? `已保存「${written.query.name}」。只保存草稿，不会执行。`
      : `已覆盖更新「${written.query.name}」。只保存草稿，不会执行。`;
    savedNameValue.value = "";
    await loadSaved();
  } catch (err) {
    savedWriteError.value = failureMessage(err, "存不了这条查询");
  } finally {
    savedBusy.value = false;
  }
}

/** 弹出列表里的一行 SQL 摘要：压平空白后截断，完整内容放在 title。 */
function sqlSummary(sql: string): string {
  const flat = sql.replace(/\s+/g, " ").trim();
  return flat.length > 100 ? `${flat.slice(0, 100)}…` : flat;
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

function onHistoryKey(event: KeyboardEvent) {
  if (event.key !== "Escape") return;
  event.preventDefault();
  historyOpen.value = false;
}

function onSavedKey(event: KeyboardEvent) {
  if (event.key !== "Escape") return;
  event.preventDefault();
  savedOpen.value = false;
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
  if (historyOpen.value) {
    const root = historyRoot.value;
    if (!root || !(event.target instanceof Node) || !root.contains(event.target)) historyOpen.value = false;
  }
  if (savedOpen.value) {
    const root = savedRoot.value;
    if (!root || !(event.target instanceof Node) || !root.contains(event.target)) savedOpen.value = false;
  }
}

watch(running, (value) => {
  if (!value) return;
  menuOpen.value = false;
  runMenuOpen.value = false;
  historyOpen.value = false;
  savedOpen.value = false;
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
      <button class="btn" type="button" data-action="format" title="格式化选中的 SQL；没有选中时格式化全部" @click="formatEditor"><Icon name="format" />格式化</button>
      <button class="btn" type="button" data-action="open-file" title="从 .sql 或 .txt 文件载入 SQL；只写进当前标签的草稿，不会执行" :disabled="running || submitting" @click="pickSqlFile"><Icon name="file" />打开</button>
      <input ref="openFileInput" class="hidden-file-input" type="file" data-action="open-file-input" :accept="SQL_FILE_ACCEPT" aria-label="打开 SQL 文件" @change="onSqlFile" />
      <div ref="historyRoot" class="q-pop">
        <button class="btn" type="button" data-action="history" title="从查询历史载入 SQL；只写进草稿，不会执行" :disabled="running || submitting" @click="toggleHistoryPanel"><Icon name="history" />历史</button>
        <div v-if="historyOpen" class="pop-menu history-menu" role="menu" aria-label="查询历史" @keydown="onHistoryKey">
          <div class="menu-head">查询历史 · 载入只写进草稿，不会执行</div>
          <p v-if="historyError" class="pop-error" role="alert">历史没有读出来：{{ historyError }}<button class="btn sm" type="button" data-action="retry-history" @click="loadHistory">重试</button></p>
          <p v-else-if="historyLoading" class="pop-empty">正在读取历史…</p>
          <p v-else-if="!historyEntries.length" class="pop-empty">还没有执行过查询。</p>
          <template v-else>
            <button v-for="entry in historyEntries" :key="entry.id" class="pop-item" type="button" role="menuitem" data-action="open-history" :title="entry.sql" @click="chooseHistory(entry)">
              <span class="badge" :class="statusBadge(entry.status)">{{ historyStatusLabel(entry.status) }}</span>
              <span class="pop-copy">
                <span class="pop-title"><b>{{ entry.connectionName }} / {{ entry.databaseName || "main" }}</b><span v-if="!knownIds.has(entry.connectionId)" class="pop-warn">原连接已删除</span></span>
                <code>{{ sqlSummary(entry.sql) }}</code>
                <small>耗时 {{ elapsedLabel(entry.elapsedMs) || "未知" }}</small>
              </span>
            </button>
          </template>
        </div>
      </div>
      <div ref="savedRoot" class="q-pop">
        <button class="btn" type="button" data-action="saved" title="按名称保存当前 SQL，或打开已保存的查询" :disabled="running || submitting" @click="toggleSavedPanel"><Icon name="save" />保存</button>
        <div v-if="savedOpen" class="pop-menu saved-menu" role="menu" aria-label="保存的查询" @keydown="onSavedKey">
          <form class="pop-form" @submit.prevent="saveNamedQuery">
            <label>保存当前 SQL 为<input v-model="savedNameValue" type="text" aria-label="保存查询的名称" data-action="saved-name" placeholder="名称" /></label>
            <p v-if="savedWriteMessage" role="status">{{ savedWriteMessage }}</p>
            <p v-if="savedWriteError" class="pop-error" role="alert">{{ savedWriteError }}</p>
            <button class="btn sm primary" type="submit" data-action="save-named-query" :disabled="savedBusy">保存</button>
          </form>
          <div class="pop-form">
            <button class="btn sm" type="button" data-action="save-file" :disabled="saveFileBusy" @click="saveSqlToFile">{{ saveFileBusy ? "正在保存…" : "保存到文件" }}</button>
            <p v-if="saveFileMessage" role="status">{{ saveFileMessage }}</p>
            <p v-if="saveFileError" class="pop-error" role="alert">{{ saveFileError }}</p>
          </div>
          <div class="menu-head">已保存的查询 · 打开只写进草稿，不会执行</div>
          <p v-if="savedError" class="pop-error" role="alert">保存的查询没有读出来：{{ savedError }}<button class="btn sm" type="button" data-action="retry-saved" @click="loadSaved">重试</button></p>
          <p v-else-if="savedLoading" class="pop-empty">正在读取保存的查询…</p>
          <p v-else-if="!savedEntries.length" class="pop-empty">还没有保存的查询。</p>
          <template v-else>
            <button v-for="item in savedEntries" :key="item.id" class="pop-item" type="button" role="menuitem" data-action="open-saved" :title="item.sql" @click="chooseSaved(item)">
              <span class="pop-copy">
                <span class="pop-title"><b>{{ item.name }}</b><span v-if="savedNeedsTarget(item, knownIds)" class="pop-warn">需要重新选择执行目标</span></span>
                <small>{{ item.connectionName ? `${item.connectionName} / ${item.databaseName || "main"}` : "未记录执行目标" }}</small>
                <code>{{ sqlSummary(item.sql) }}</code>
              </span>
            </button>
          </template>
        </div>
      </div>
      <button class="btn" type="button" data-action="plan" title="查看一条语句的执行计划，不使用 ANALYZE" :disabled="running || submitting" @click="requestPlan"><Icon name="plan" />执行计划</button>
      <button class="btn" type="button" data-action="analyze" title="在服务器上实际执行并分析这一条语句" :disabled="running || submitting" @click="requestAnalyze">实际执行并分析</button>
      <div v-if="!running" ref="runRoot" class="run-group">
        <button class="btn primary run-main" type="button" data-action="run" :disabled="submitting" @click="execute('auto')"><Icon name="play" class="run-glyph" />{{ submitting ? "正在执行…" : hasSelection ? "执行选中" : "执行" }} <ChordKeys chord="Mod+Enter" /></button>
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
    <div
      v-if="variablePrompt"
      class="analyze-dialog variable-dialog"
      role="dialog"
      aria-modal="true"
      aria-label="填写执行变量"
      data-action="variables-dialog"
      @keydown.escape.prevent="cancelVariables"
    >
      <p>发送前把占位符代换成字面量：留空按 NULL 发送，其余按字符串字面量发送（单引号会转义）。只改本次发送的文本，编辑器草稿保持原文。仅「执行」支持变量；「执行计划」与「实际执行并分析」不做代换。</p>
      <div class="variable-fields">
        <label v-for="field in variableFields" :key="field.key">
          <span class="mono">{{ field.label }}</span>
          <input v-model="variableValues[field.key]" type="text" :data-variable-key="field.key" :aria-label="`变量 ${field.label} 的值`" placeholder="留空发送 NULL" />
        </label>
      </div>
      <div class="analyze-actions">
        <button class="btn" type="button" data-action="variables-cancel" @click="cancelVariables">取消</button>
        <button class="btn primary" type="button" data-action="variables-confirm" @click="confirmVariables">继续</button>
      </div>
    </div>
    <p v-if="notice" class="q-banner is-notice">{{ notice }}</p>
    <p v-if="actionError" class="q-banner is-error" role="alert">{{ actionError }}</p>
    <p v-if="hintVisible" class="q-hint" :data-keywords-only="hint.keywordsOnly" :title="hint.note">{{ hint.note }}</p>
    <div ref="host" class="cm-host"></div>
    <div class="editor-position"><span class="q-guide">{{ operationGuide }}</span><span class="mono">Ln {{ cursor.line }}, Col {{ cursor.column }}</span></div>
    <div class="query-splitter" role="separator" tabindex="0" aria-label="调整编辑器与结果区域高度" aria-orientation="horizontal" :aria-valuenow="editorPercent" :aria-valuemin="20" :aria-valuemax="70" @pointerdown="resizeEditor" @keydown.up.prevent="editorPercent = Math.max(20, editorPercent - 5)" @keydown.down.prevent="editorPercent = Math.min(70, editorPercent + 5)"><span></span></div>
    <section class="q-results" aria-label="执行结果">
      <div class="results-heading"><span><Icon name="table" />结果</span><span v-if="run" class="q-summary" :data-status="run.status">{{ summary }}</span><span v-else class="muted">等待执行</span><span class="grow"></span><span class="badge">只读结果</span></div>
      <template v-if="run">
        <div v-if="run.statements.length > 1" class="result-tabs" role="tablist" aria-label="语句结果"><button v-for="(statement, index) in run.statements" :key="index" role="tab" :aria-selected="resultIndex === index" @click="resultIndex = index">结果 {{ index + 1 }} <span>{{ statement.columns.length ? `${statement.rows.length} 行` : statusLabel(statement.status) }}</span></button></div>
        <p v-if="run.pollError" class="q-note" role="alert">状态刷新失败：{{ run.pollError }}。已读取的结果保留，可以稍后重试读取。</p>
        <p v-else-if="run.cancel === 'requested'" class="q-note" role="status">取消中…数据库确认后立即停止。</p>
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
/* 查询页 chrome（R1）：四层空间层级——工具条 chrome / 编辑器 sunken 井 / 结果区 panel / 浮层 overlay。
   一律引用 tokens.css 令牌；控件描边走 --stroke/--stroke-strong，按压走 --press-*。 */
.query-pane { display: flex; flex-direction: column; flex: 1; min-height: 0; }

/* ---------- 工具条：chrome 层实体控件带 ---------- */
.q-toolbar { display: flex; gap: var(--sp-2); align-items: center; flex-wrap: wrap; padding: var(--sp-2) var(--sp-3); min-height: calc(var(--control-h) + var(--sp-3)); border-bottom: 1px solid var(--line); background: var(--surface-chrome); }
.q-toolbar .btn :deep(.i) { width: 14px; height: 14px; color: var(--text-3); }
.q-target { position: relative; min-width: 0; }
.target-btn { display: flex; align-items: center; gap: var(--sp-2); height: var(--control-h); max-width: 420px; min-width: 0; padding: 0 var(--sp-2); border: 1px solid var(--stroke); border-radius: var(--r); background: var(--surface-panel); font-size: var(--fs-sm); box-shadow: var(--shadow-sm); transition: border-color var(--dur-fast) var(--ease), box-shadow var(--dur-fast) var(--ease), background var(--dur-fast) var(--ease); }
.target-btn:hover:not(:disabled) { border-color: var(--stroke-strong); background: var(--hover); }
.target-btn:active:not(:disabled) { background: var(--active); box-shadow: inset 0 1px 2px var(--press); }
.target-btn.on { border-color: var(--accent); box-shadow: 0 0 0 2px var(--ring); }
.target-btn.none { border-style: dashed; color: var(--text-2); }
.target-btn:disabled { opacity: 0.7; cursor: not-allowed; }
.tg-name { font-weight: 600; white-space: nowrap; }
.tg-db { display: inline-flex; align-items: center; gap: var(--sp-1); min-width: 0; color: var(--text-2); }
.tg-db span:last-child { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.tg-sep { color: var(--text-3); }
.tg-caret { width: 14px; height: 14px; margin-left: auto; color: var(--text-3); fill: none; stroke: currentColor; stroke-width: 1.8; stroke-linecap: round; stroke-linejoin: round; flex-shrink: 0; }
.target-btn :deep(.i) { width: 14px; height: 14px; color: var(--text-3); }
.eng { --engc: var(--text-2); display: inline-grid; place-items: center; width: 20px; height: 20px; border-radius: var(--r-sm); font: 700 var(--fs-xs)/1 var(--font); color: var(--engc); background: color-mix(in srgb, var(--engc) 14%, transparent); box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--engc) 24%, transparent); flex-shrink: 0; }
.eng-postgres { --engc: var(--eng-postgres); }
.eng-mysql { --engc: var(--eng-mysql); }
.eng-mariadb { --engc: var(--eng-mariadb); }
.eng-sqlite { --engc: var(--eng-sqlite); }

/* ---------- 浮层：overlay 表面 + md 圆角 + 单档浮层阴影 ---------- */
.target-menu, .run-menu { position: absolute; z-index: var(--z-menu); top: calc(100% + var(--sp-1)); left: 0; min-width: 280px; max-width: 420px; max-height: 320px; overflow: auto; padding: var(--sp-1); background: var(--surface-overlay); border: 1px solid var(--stroke-strong); border-radius: var(--r-md); box-shadow: var(--shadow); animation: pop-in var(--dur-fast) var(--ease-out); }
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

/* ---------- 执行拆分按钮：主段 primary + 更多段同色系延伸（不带 primary 类） ---------- */
.run-group { position: relative; display: flex; }
.run-group .run-main { border-radius: var(--r) 0 0 var(--r); }
.run-group .run-more { border-radius: 0 var(--r) var(--r) 0; margin-left: -1px; padding: 0 var(--sp-2); border-color: transparent; background: var(--accent); color: var(--on-accent); box-shadow: var(--shadow-sm), inset 0 1px 0 color-mix(in srgb, var(--on-accent) 24%, transparent), inset 1px 0 0 color-mix(in srgb, var(--on-accent) 22%, transparent); }
.run-group .run-more:hover:not(:disabled) { background: var(--accent-hover); border-color: transparent; }
.run-group .run-more:active:not(:disabled) { background: var(--accent-active); box-shadow: inset 0 1px 2px var(--press-accent); }
.run-group .run-more:disabled { background: var(--disabled-bg); color: var(--disabled-fg); border-color: var(--line); box-shadow: none; }
.run-group .run-more .tg-caret { margin-left: 0; color: var(--on-accent); }
.run-glyph { width: 13px; height: 13px; }
.run-menu { min-width: 220px; }
.run-item { display: flex; flex-direction: column; align-items: flex-start; gap: 2px; width: 100%; padding: var(--sp-2); border-radius: var(--r); text-align: left; font-size: var(--fs-sm); }
.run-item:hover { background: var(--hover); }
.run-item:active { box-shadow: inset 0 1px 2px var(--press); }
.run-item small { font-size: var(--fs-xs); color: var(--text-3); }

/* ---------- 历史 / 保存浮出：overlay 表面 ---------- */
.q-pop { position: relative; display: inline-flex; }
.hidden-file-input { display: none; }
.pop-menu { position: absolute; z-index: var(--z-menu); top: calc(100% + var(--sp-1)); right: 0; min-width: 300px; max-width: 440px; max-height: 360px; overflow: auto; padding: var(--sp-1); background: var(--surface-overlay); border: 1px solid var(--stroke-strong); border-radius: var(--r-md); box-shadow: var(--shadow); animation: pop-in var(--dur-fast) var(--ease-out); }
.pop-item { display: flex; align-items: flex-start; gap: var(--sp-2); width: 100%; padding: var(--sp-2); border-radius: var(--r); text-align: left; }
.pop-item:hover { background: var(--hover); }
.pop-item:active { box-shadow: inset 0 1px 2px var(--press); }
.pop-item .badge { flex-shrink: 0; }
.pop-copy { display: flex; flex-direction: column; gap: 2px; min-width: 0; flex: 1; }
.pop-title { display: flex; align-items: center; gap: var(--sp-2); min-width: 0; font-size: var(--fs-sm); }
.pop-title b { font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.pop-copy code { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-family: var(--mono); font-size: var(--fs-xs); color: var(--text-2); }
.pop-copy small { font-size: var(--fs-xs); color: var(--text-3); }
.pop-warn { font-size: var(--fs-xs); color: var(--warn); flex-shrink: 0; }
.pop-empty { margin: 0; padding: var(--sp-2); font-size: var(--fs-sm); color: var(--text-3); }
.pop-error { display: flex; align-items: center; gap: var(--sp-2); margin: 0; padding: var(--sp-2); font-size: var(--fs-sm); color: var(--err); }
.pop-error .btn { margin-left: auto; flex-shrink: 0; }
.pop-form { display: flex; flex-direction: column; align-items: stretch; gap: var(--sp-2); padding: var(--sp-2); margin: 0 0 var(--sp-1); border-bottom: 1px solid var(--line); }
.pop-form label { display: flex; flex-direction: column; gap: var(--sp-1); font-size: var(--fs-xs); color: var(--text-2); }
.pop-form p { margin: 0; font-size: var(--fs-xs); }
.pop-form .btn { align-self: flex-start; }

/* ---------- 提示条 / 横幅 ---------- */
.q-note, .q-hint, .q-banner { margin: 0; font-size: var(--fs-sm); color: var(--text-2); }
.q-hint, .q-banner { padding: var(--sp-1) var(--sp-4); }
.q-banner { color: var(--text); background: var(--surface-sunken); box-shadow: inset 2px 0 0 var(--line-2); }
.q-banner.is-error { color: var(--err); background: var(--err-soft); box-shadow: inset 2px 0 0 var(--err); }

/* ---------- 编辑器：sunken 下沉井 ---------- */
.query-pane .cm-host { flex: 0 1 var(--editor-share); min-height: 90px; margin: var(--sp-2) var(--sp-3) 0; border: 1px solid var(--line); border-radius: var(--r-md); overflow: hidden; background: var(--surface-sunken); box-shadow: inset 0 1px 2px color-mix(in srgb, var(--press) 24%, transparent); }
.query-pane :deep(.cm-content) { padding-top: var(--sp-3); line-height: 1.8; }
.query-pane :deep(.cm-line) { padding-left: var(--sp-4); }
.query-pane :deep(.cm-gutters) { background: var(--surface-sunken); color: var(--text-3); border-right: 1px solid var(--line); min-width: 44px; }
.query-pane :deep(.cm-activeLine), .query-pane :deep(.cm-activeLineGutter) { background: var(--sel); }
.query-pane :deep(.cm-activeLineGutter) { color: var(--text); }
.query-pane :deep(.cm-cursor) { border-left-color: var(--accent); }
.query-pane :deep(.cm-selectionBackground), .query-pane :deep(.cm-focused .cm-selectionBackground) { background: var(--sel-strong) !important; }
.query-pane :deep(.cm-send-range) { background: color-mix(in srgb, var(--accent) 22%, transparent); border-radius: 2px; }
.editor-position { display: flex; justify-content: space-between; align-items: center; gap: var(--sp-2); padding: var(--sp-1) var(--sp-4); color: var(--text-3); font-size: var(--fs-xs); }
.q-guide { min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }

/* ---------- 分隔条：chrome 层抓手 ---------- */
.query-splitter { height: 9px; flex-shrink: 0; display: grid; place-items: center; background: var(--surface-chrome); border-block: 1px solid var(--line); cursor: row-resize; touch-action: none; transition: background var(--dur-fast) var(--ease); }
.query-splitter span { height: 3px; width: 34px; background: var(--line-2); border-radius: var(--r-full); transition: background var(--dur-fast) var(--ease); }
.query-splitter:hover, .query-splitter:focus-visible { background: var(--accent-soft); }
.query-splitter:hover span, .query-splitter:focus-visible span { background: var(--accent); }

/* ---------- 结果区：panel 层 ---------- */
.query-pane .q-results { flex: 1; min-height: 130px; padding: 0; gap: 0; border: 0; background: var(--surface-panel); }
.q-results { min-height: 120px; overflow: auto; border-top: 1px solid var(--line); padding: var(--sp-3); display: flex; flex-direction: column; gap: var(--sp-2); }
.results-heading { display: flex; align-items: center; gap: var(--sp-4); padding: var(--sp-2) var(--sp-4); border-bottom: 1px solid var(--line); font-size: var(--fs-xs); flex-shrink: 0; background: var(--surface-chrome); color: var(--text-2); }
.results-heading > span:first-child { display: inline-flex; align-items: center; gap: var(--sp-2); font-size: var(--fs-sm); font-weight: 600; color: var(--text); }
.results-heading > span:first-child :deep(.i) { width: 14px; height: 14px; color: var(--text-3); }
.q-summary { margin: 0; font-size: var(--fs-md); font-weight: 600; color: var(--text); }
.q-summary[data-status="error"] { color: var(--err); }
.q-summary[data-status="unknown"] { color: var(--warn); }
.results-heading .q-summary { font-size: var(--fs-xs); font-weight: 500; color: var(--text-2); }
.results-heading .q-summary[data-status="error"] { color: var(--err); }
.results-heading .q-summary[data-status="unknown"] { color: var(--warn); }

/* 语句结果页签：下缘指示条与结果区上缘连成一体 */
.result-tabs { display: flex; flex-shrink: 0; gap: 2px; padding: 0 var(--sp-3); border-bottom: 1px solid var(--line); overflow: auto; background: var(--surface-chrome); }
.result-tabs button { padding: var(--sp-2) var(--sp-3); font-size: var(--fs-xs); color: var(--text-2); white-space: nowrap; border-bottom: 2px solid transparent; border-radius: var(--r-sm) var(--r-sm) 0 0; transition: color var(--dur-fast) var(--ease), border-color var(--dur-fast) var(--ease), background var(--dur-fast) var(--ease); }
.result-tabs button:hover { color: var(--text); border-bottom-color: var(--line-2); background: var(--hover); }
.result-tabs button[aria-selected="true"] { color: var(--text); border-bottom-color: var(--accent); background: var(--surface-panel); box-shadow: 0 1px 0 0 var(--surface-panel); }
.result-tabs span { font-size: var(--fs-xs); color: var(--text-3); margin-left: var(--sp-1); }

/* 执行计划视图 */
.plan-view { padding: var(--sp-3) var(--sp-4); overflow: auto; }
.plan-caption { margin: 0 0 var(--sp-2); font-size: var(--fs-sm); color: var(--text-2); }
.plan-tree { list-style: none; margin: 0; padding: 0; font-family: var(--mono); font-size: var(--fs-xs); }
.plan-tree li { display: flex; gap: var(--sp-2); padding-block: 2px; }
.plan-view table { border-collapse: collapse; font-family: var(--mono); font-size: var(--fs-xs); }
.plan-view th, .plan-view td { border: 1px solid var(--line); padding: var(--sp-1) var(--sp-2); text-align: left; vertical-align: top; }

/* 详情披露与逐条语句时间线 */
.q-details { flex-shrink: 0; font-size: var(--fs-sm); border: 0; border-top: 1px solid var(--line); border-radius: 0; background: var(--surface-panel); }
.q-details .disclosure-body { max-height: 180px; overflow: auto; }
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

/* ---------- 行内确认面板：overlay 表面卡片 ---------- */
.analyze-dialog { margin: var(--sp-2) var(--sp-3); padding: var(--sp-4); max-width: 560px; border: 1px solid var(--stroke-strong); border-radius: var(--r-lg); background: var(--surface-overlay); box-shadow: var(--shadow); }
.analyze-dialog p { margin: 0 0 var(--sp-2); font-size: var(--fs-sm); color: var(--text-2); line-height: var(--lh); }
.analyze-actions { display: flex; gap: var(--sp-2); }
.variable-fields { display: flex; flex-direction: column; gap: var(--sp-2); margin: 0 0 var(--sp-2); }
.variable-fields label { display: flex; flex-direction: column; gap: var(--sp-1); font-size: var(--fs-xs); color: var(--text-2); }
.variable-fields input { font-family: var(--mono); }

.run-main :deep(kbd) { background: transparent; color: inherit; opacity: .75; border-color: currentColor; box-shadow: none; }
@media (max-width: 800px) { .run-main :deep(.kbd) { display: none; } .target-btn { max-width: 290px; } .q-toolbar .conn-state { display: none; } .editor-position > span:first-child { overflow: hidden; white-space: nowrap; text-overflow: ellipsis; } }
</style>
