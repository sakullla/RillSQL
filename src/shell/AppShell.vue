<script setup lang="ts">
import { computed, defineAsyncComponent, nextTick, onBeforeUnmount, onMounted, reactive, ref, watch } from "vue";
import { listPaletteTables, startCatalog } from "../catalog";
import CompareDialog from "../compare/CompareDialog.vue";
import { placeCompareScript, type ScriptTarget } from "../compare/place";
import type { DiagramOpen } from "../diagram/types";
import { objectIdentity, type CatalogObjectRef } from "../catalog/object-ref";
import { isDefinitionKind, objectLabel, objectPath } from "../catalog/tree";
import type { CatalogConnection } from "../catalog/types";
import { catalogConnections, catalogLoaded } from "../catalog/store";
import { listPaletteConnections } from "../connections";
import { engineMeta } from "../engines";
import { setLocale, tr as t } from "../i18n";
import DragImportDialog from "../export/DragImportDialog.vue";
import DragOverlay from "../export/DragOverlay.vue";
import DragPreview from "../export/DragPreview.vue";
import { connectDroppedFiles, createDroppedFileHandler, previewBudgetBytes, type DroppedFileState } from "../export/dropped";
import type { ImportSeed } from "../export/import";
import { exportTasks, taskStatusText } from "../export/tasks";
import type { TableTarget } from "../grid/target";
import type { SavedDraft } from "../history/saved";
import { deletedConnectionNotice, type OpenPlan } from "../history/plan";
import { attachCloser, requestCancel, startTabRun, tabRun } from "../query/runtime";
import { rowLimit } from "../query/scope";
import { parseHash } from "../router";
import { ToastHost } from "../ui";
import { toast } from "../ui/toast";
import { buildInsertText, focusedEditor, insertAtCursor, insertPlan, type InsertReference } from "./query-editor";
import { chooseLeave, chooseStay, closePrompt, installCloseCoordinator, uninstallCloseCoordinator } from "./close-guard";
import ActivityRail from "./ActivityRail.vue";
import CommandPalette from "./CommandPalette.vue";
import { shellCommands, type PaletteItem } from "./commands";
import { syncProductIdentity } from "./identity";
import { loadSettings, saveSettings, type ShellSettings } from "./settings";
import { createSession, restoreSession, SESSION_KEY, snapshotSession, STRUCTURE_PLACEHOLDER, type ShellTab } from "./session";
import type { ConnectionStatus, OperationActivity } from "./status-text";
import ShortcutHelp from "./ShortcutHelp.vue";
import Sidebar from "./Sidebar.vue";
import StatusBar from "./StatusBar.vue";
import { applyDocumentTheme, prefersDarkScheme, type ThemePreference } from "./theme";
import TitleBar from "./TitleBar.vue";
import WorkspaceView from "./WorkspaceView.vue";
import type { AppPage } from "../router";

const ConnectionsPage = defineAsyncComponent(() => import("../connections/ConnectionsPage.vue"));
const HistoryPage = defineAsyncComponent(() => import("../history/HistoryPage.vue"));
const SettingsView = defineAsyncComponent(() => import("./SettingsView.vue"));

function readStoredSession(): string | null {
  try {
    return localStorage.getItem(SESSION_KEY);
  } catch {
    return null;
  }
}

const session = createSession(
  typeof location === "undefined" ? "" : location.hash,
  restoreSession(readStoredSession()),
);
const windowChrome = ref({ custom: false, maximized: false });
const closeDialog = ref<HTMLDialogElement | null>(null);
attachCloser((id) => session.closeTab(id));
installCloseCoordinator(session);

watch(
  () => [session.tabs.value, session.activeId.value],
  () => {
    try {
      localStorage.setItem(SESSION_KEY, JSON.stringify(snapshotSession(session.tabs.value, session.activeId.value)));
    } catch {
      // 配额满或隐私模式下持久化失败：静默放弃，不影响当前会话。
    }
  },
  // flush: sync —— 标签/草稿一变就同步写 localStorage，进程被终止也不留微任务窗口（R2）。
  { deep: true, flush: "sync" },
);

/** 恢复的标签指向已删除的连接时给出提示（R2）：不自动连接、不弹错误风暴。文案走字典（R6）。 */
const DELETED_CONNECTION_TAB_NOTICE_KEY = "shell.activity.deletedTabNotice";

watch(
  () => catalogLoaded(),
  (ready) => {
    if (!ready) return;
    const known = new Set(catalogConnections().map((item) => item.id));
    for (const tab of session.tabs.value) {
      if (!tab.connectionId || known.has(tab.connectionId)) continue;
      if (tab.targetNotice === t(DELETED_CONNECTION_TAB_NOTICE_KEY)) continue;
      tab.targetNotice = t(DELETED_CONNECTION_TAB_NOTICE_KEY);
    }
  },
);

const CLOSE_PROMPT_KEYS = {
  window: {
    cells: "shell.closeGuard.windowCells",
    draft: "shell.closeGuard.windowDraft",
    both: "shell.closeGuard.windowBoth",
  },
  tab: {
    cells: "shell.closeGuard.tabCells",
    draft: "shell.closeGuard.tabDraft",
    both: "shell.closeGuard.tabBoth",
  },
} as const;
const closePromptText = computed(() => t(CLOSE_PROMPT_KEYS[closePrompt.scope][closePrompt.reason]));

watch(
  () => closePrompt.open,
  async (open) => {
    if (!open) return;
    await nextTick();
    const dialog = closeDialog.value;
    if (!dialog || dialog.open) return;
    try {
      dialog.showModal();
    } catch {
      dialog.setAttribute("open", "");
    }
  },
);

function asStatus(status: string): ConnectionStatus {
  if (status === "connected" || status === "connecting" || status === "disconnected" || status === "error" || status === "lost") return status;
  return "disconnected";
}

function onHistoryOpen(plan: OpenPlan) {
  const known = plan.connectionId ? catalogConnections().find((item) => item.id === plan.connectionId) ?? null : null;
  if (plan.needsTarget || !known || !plan.connectionId || plan.database == null) {
    session.openQueryDraft({
      sql: plan.sql,
      connectionId: null,
      connectionName: null,
      database: null,
      engine: null,
      status: null,
      needsTarget: true,
      notice: deletedConnectionNotice(),
    });
    return;
  }
  session.openQueryDraft({
    sql: plan.sql,
    connectionId: plan.connectionId,
    connectionName: known.name,
    database: plan.database,
    engine: known.engine,
    status: asStatus(known.status),
    needsTarget: false,
    notice: null,
  });
}
const settings = reactive<ShellSettings>(loadSettings());
const dataDir = ref<string | null>(null);
const sqlitePrompt = ref(0);
const compareOpen = ref(false);

const historySection = ref<"runs" | "saved">("runs");
const saveRequest = ref(0);
const executeRequest = ref(0);

/** 拖放 CSV/JSON 预览（R9）：Tauri 原生 drag-drop 事件 → 扩展名分流 → 预览 → 继续进导入对话框。 */
const dragHovering = ref(false);
const dragState = ref<DroppedFileState | null>(null);
const dragImport = ref<ImportSeed | null>(null);
const onDragDrop = createDroppedFileHandler({
  setState: (state) => {
    dragState.value = state;
  },
  setHovering: (hovering) => {
    dragHovering.value = hovering;
  },
});
let stopDragDrop: (() => void) | null = null;
let dragDropAlive = true;
const currentSavedDraft = computed<SavedDraft | null>(() => {
  const tab = session.activeTab.value;
  if (!tab || tab.kind !== "query") return null;
  return {
    sql: tab.draft,
    connectionId: tab.connectionId ?? "",
    connectionName: tab.connectionName ?? "",
    databaseName: tab.database ?? "",
    engine: tab.engine ?? "",
  };
});

const runningQueries = computed(() => {
  const found: { tab: ShellTab; run: NonNullable<ReturnType<typeof tabRun>> }[] = [];
  for (const tab of session.tabs.value) {
    if (tab.kind !== "query") continue;
    const run = tabRun(tab.id);
    if (run && (run.status === "running" || run.status === "cancel-requested")) found.push({ tab, run });
  }
  return found;
});

/** 状态栏右侧的进行中操作：导出任务优先，其次正在执行的查询；都没有则留空。 */
const activity = computed<OperationActivity | null>(() => {
  const exporting = exportTasks.find((task) => task.status === "running");
  if (exporting) return { state: "running", text: taskStatusText(exporting) };
  const first = runningQueries.value[0];
  if (first) {
    const more = runningQueries.value.length > 1 ? t("shell.activity.totalMore", { n: runningQueries.value.length }) : "";
    return { state: "running", text: `${t("shell.activity.executingQuery", { conn: first.tab.connectionName ?? "", db: first.tab.database || "main" })}${more}` };
  }
  return null;
});

const activeRun = computed(() => runningQueries.value[0] ?? null);

function onStatusCancel() {
  const current = activeRun.value;
  if (current) void requestCancel(current.tab.id);
}

const commands = computed<PaletteItem[]>(() => [
  ...listPaletteTables().map((table) => ({
    id: table.id,
    group: "table" as const,
    label: table.label,
    icon: "table" as const,
    path: table.path,
    run: () => session.openTable({ id: table.id, label: table.label, path: table.path }),
  })),
  ...listPaletteConnections().map((connection) => ({
    id: connection.id,
    group: "connection" as const,
    label: connection.label,
    icon: "database" as const,
    path: connection.path,
    run: () => onPaletteConnection(connection.id),
  })),
  ...shellCommands({
    openQuery: session.openQuery,
    openStructure: () => session.openStructure(STRUCTURE_PLACEHOLDER),
    showPage: (page) => {
      if (page === "history") historySection.value = "runs";
      session.showPage(page);
    },
    toggleSidebar: session.toggleSidebar,
    setTheme,
    openHelp: session.openHelp,
    requestSqlite,
    saveQuery: () => {
      historySection.value = "saved";
      saveRequest.value += 1;
      session.showPage("history");
    },
    showSavedQueries: () => {
      historySection.value = "saved";
      session.showPage("history");
    },
    openCompare: () => {
      compareOpen.value = true;
      session.showPage("workspace");
    },
    executeQuery: () => {
      const active = session.activeTab.value;
      if (active?.kind !== "query") {
        const existing = session.tabs.value.find((tab) => tab.kind === "query");
        if (existing) session.activateTab(existing.id);
        else session.openQuery();
      }
      executeRequest.value += 1;
    },
  }),
  {
    id: "database-tools",
    group: "command" as const,
    label: t("admin.command.open"),
    icon: "database" as const,
    run: onOpenDatabaseTools,
  },
]);

/** 已连接的关系库直接开查询；其余情况去连接页处理。 */
function onPaletteConnection(id: string) {
  const known = catalogConnections().find((item) => item.id === id);
  const database = known?.defaultDatabase || known?.databases[0]?.name;
  if (known && asStatus(known.status) === "connected" && database != null && engineMeta(known.engine).relational) {
    onTreeNewQuery(known, database);
    return;
  }
  session.showPage("connections");
}

/**
 * 数据库级标签（R5）：优先用当前标签的连接与库，否则取第一条已连接的关系库。
 * schema 留空表示这个引擎不需要 schema（PostgreSQL 时读全部 schema 的序列）。
 */
function onOpenDatabaseTools() {
  const tab = session.activeTab.value;
  const tabConnection = tab?.connectionId ? catalogConnections().find((item) => item.id === tab.connectionId) ?? null : null;
  const connected = catalogConnections().filter((item) => asStatus(item.status) === "connected" && engineMeta(item.engine).relational);
  const known = tabConnection && engineMeta(tabConnection.engine).relational ? tabConnection : connected[0] ?? null;
  if (!known) {
    toast(t("admin.panel.needConnection"), "err");
    return;
  }
  const sameTarget = tab !== null && tab.connectionId === known.id;
  const database = (sameTarget ? tab.database : null) || known.defaultDatabase || known.databases[0]?.name || "";
  const schema = sameTarget && tab.kind === "database" ? tab.admin?.schema ?? "" : "";
  if (window.innerWidth <= 960) session.sidebarHidden.value = true;
  session.openDatabase({
    id: `${known.id}:${database}`,
    connectionId: known.id,
    connectionName: known.name,
    database,
    engine: known.engine,
    status: "connected",
    schema,
  });
}

function applyTheme() {
  applyDocumentTheme(settings.theme, settings.density, prefersDarkScheme(), settings.editorFont);
}

function setTheme(theme: ThemePreference) {
  settings.theme = theme;
  persist();
}

function persist() {
  saveSettings(settings);
  applyTheme();
}

function onOpenObject(object: CatalogObjectRef, pane: "data" | "structure", section?: "columns" | "drop") {
  if (object.kind !== "table" && object.kind !== "view") return;
  if (window.innerWidth <= 960) session.sidebarHidden.value = true;
  session.openTable({
    id: objectIdentity(object),
    label: object.name,
    path: object.path,
    pane,
    section,
  });
}

function onOpenDefinition(object: CatalogObjectRef) {
  if (!isDefinitionKind(object.kind)) return;
  if (window.innerWidth <= 960) session.sidebarHidden.value = true;
  session.openDefinition({
    id: objectIdentity(object),
    label: objectLabel(object.engine, object),
    path: object.path,
    connectionId: object.connectionId,
    connectionName: object.connectionName,
    database: object.database,
    engine: object.engine,
    status: "connected",
    schema: object.schema,
    name: object.name,
    kind: object.kind,
    signature: object.signature ?? "",
    relation: object.relation ?? "",
  });
}

function onOpenKeys(connection: CatalogConnection, key: string) {
  if (window.innerWidth <= 960) session.sidebarHidden.value = true;
  session.openKeys({
    id: connection.id,
    label: t("shell.tabs.keyspaceTab", { name: connection.name }),
    titleKey: { key: "shell.tabs.keyspaceTab", params: { name: connection.name } },
    path: objectPath(connection.name, "", ""),
    connectionId: connection.id,
    connectionName: connection.name,
    database: connection.defaultDatabase || "0",
    engine: connection.engine,
    status: "connected",
    key: key || undefined,
  });
}

function onOpenDocs(connection: CatalogConnection, database: string, collection: string) {
  if (window.innerWidth <= 960) session.sidebarHidden.value = true;
  session.openDocs({
    id: `${connection.id}:${database}:${collection}`,
    label: t("shell.tabs.docsTab", { collection }),
    titleKey: { key: "shell.tabs.docsTab", params: { collection } },
    path: objectPath(connection.name, database, ""),
    connectionId: connection.id,
    connectionName: connection.name,
    database,
    engine: connection.engine,
    status: "connected",
    collection,
  });
}

function onOpenDiagram(target: DiagramOpen) {
  if (window.innerWidth <= 960) session.sidebarHidden.value = true;
  session.openDiagram({
    id: `${target.connectionId}:${target.database}:${target.schema}`,
    label: target.label,
    path: target.path,
    connectionId: target.connectionId,
    connectionName: target.connectionName,
    database: target.database,
    engine: target.engine,
    status: "connected",
    schema: target.schema,
  });
}

function onCopyDefinition(sql: string) {
  const tab = session.activeTab.value;
  const ready = tab?.connectionId && tab.connectionName && tab.database != null && tab.engine && tab.connectionStatus;
  session.openQueryDraft({
    sql,
    connectionId: ready ? tab.connectionId : null,
    connectionName: ready ? tab.connectionName : null,
    database: ready ? tab.database : null,
    engine: ready ? tab.engine : null,
    status: ready ? tab.connectionStatus : null,
    needsTarget: !ready,
    notice: null,
  });
}

/** 树上的「在此库新建查询」：连接已开，目标直接绑定，光标落进空编辑器。 */
function onTreeNewQuery(connection: CatalogConnection, database: string) {
  if (window.innerWidth <= 960) session.sidebarHidden.value = true;
  session.openQueryDraft({
    sql: "",
    connectionId: connection.id,
    connectionName: connection.name,
    database,
    engine: connection.engine,
    status: "connected",
    needsTarget: false,
    notice: null,
  });
}

/**
 * 目录树单击（R4）：有聚焦编辑器就插入到光标处；否则有查询标签只选中；
 * 一个查询标签都没有时新建标签并写入标识符。只读与执行中的标签给提示、不改草稿。
 */
function onInsertObject(reference: InsertReference) {
  const target = focusedEditor();
  const hasQueryTab = session.tabs.value.some((tab) => tab.kind === "query");
  const plan = insertPlan(target !== null, hasQueryTab);
  if (plan === "insert" && target) {
    const outcome = insertAtCursor(target.tabId, reference);
    if (outcome === "ok") return;
    if (outcome === "running") toast(t("catalog.tree.insertBlockedRunning"), "err");
    else if (outcome === "readonly") toast(t("catalog.tree.insertBlockedReadonly"), "err");
    /* none：编辑器刚好卸载，下面按「有无查询标签」继续判定。 */
    if (outcome === "running" || outcome === "readonly") return;
  }
  if (hasQueryTab) return;
  if (window.innerWidth <= 960) session.sidebarHidden.value = true;
  session.openQueryDraft({
    sql: buildInsertText(reference, "", 0, reference.database),
    connectionId: reference.connectionId,
    connectionName: reference.connectionName,
    database: reference.database,
    engine: reference.engine,
    status: "connected",
    needsTarget: false,
    notice: null,
  });
}

function onQueryObject(object: CatalogObjectRef, sql: string) {
  session.openQueryDraft({
    sql,
    connectionId: object.connectionId,
    connectionName: object.connectionName,
    database: object.database,
    engine: object.engine,
    status: "connected",
    needsTarget: false,
    notice: null,
  });
}

/** 外键值跳转：新开查询标签并立即执行这条只读 SELECT；连接忙时保留草稿并提示。 */
async function onJumpReference(sql: string, ref: string, target: TableTarget | null): Promise<void> {
  const known = target ? catalogConnections().find((item) => item.id === target.connectionId) ?? null : null;
  if (!target || !known) {
    session.openQueryDraft({
      sql,
      connectionId: null,
      connectionName: null,
      database: null,
      engine: null,
      status: null,
      needsTarget: true,
      notice: t("shell.activity.jumpNeedTarget", { ref }),
    });
    return;
  }
  const tabId = session.openQueryDraft({
    sql,
    connectionId: known.id,
    connectionName: known.name,
    database: target.database,
    engine: known.engine,
    status: asStatus(known.status),
    needsTarget: false,
    notice: null,
  });
  try {
    await startTabRun({
      tabId,
      connectionId: known.id,
      connectionName: known.name,
      database: target.database,
      engine: known.engine,
      sql,
      limit: rowLimit(loadSettings().pageSize),
    });
  } catch (err) {
    const tab = session.tabs.value.find((item) => item.id === tabId);
    if (tab) {
      const reason = typeof err === "string" ? err : err instanceof Error ? err.message : t("shell.activity.autoRunFailed");
      tab.targetNotice = t("shell.activity.draftKept", { reason });
    }
  }
}

function onPlaceCompare(sql: string, target: ScriptTarget) {
  placeCompareScript(session.openQueryDraft, sql, target);
  compareOpen.value = false;
}

function onFocusedDatabase(connectionId: string, database: string) {
  const known = catalogConnections().find((item) => item.id === connectionId);
  for (const tab of session.tabs.value) {
    if (tab.kind !== "query" || tab.connectionId !== connectionId || !tab.connectionName || !tab.engine) continue;
    session.setQueryTarget(tab.id, {
      connectionId,
      connectionName: known?.name ?? tab.connectionName,
      database,
      engine: known?.engine ?? tab.engine,
      status: "connected",
    });
  }
}

function onSettings(patch: Partial<ShellSettings>) {
  Object.assign(settings, patch);
  if (patch.locale) setLocale(patch.locale);
  persist();
}

function onNavigate(page: AppPage) {
  if (page === "workspace" && session.page.value === "workspace") {
    session.toggleSidebar();
    return;
  }
  if (page === "history") historySection.value = "runs";
  session.showPage(page);
}

function requestSqlite() {
  session.showPage("connections");
  sqlitePrompt.value += 1;
}

function runCommand(item: PaletteItem) {
  session.closePalette();
  item.run();
}

function typingTarget(event: KeyboardEvent): boolean {
  const target = event.target;
  if (!(target instanceof HTMLElement)) return false;
  return target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.tagName === "SELECT" || target.isContentEditable;
}

function onKey(event: KeyboardEvent) {
  if (session.paletteOpen.value) return;
  const mod = event.ctrlKey || event.metaKey;
  const key = event.key.toLowerCase();
  if (mod && key === "k") {
    event.preventDefault();
    session.openPalette("all");
    return;
  }
  if (mod && key === "p") {
    event.preventDefault();
    session.openPalette("tables");
    return;
  }
  if (mod && !event.altKey && key === "t") {
    event.preventDefault();
    session.openQuery();
    return;
  }
  if (mod && key === "w") {
    event.preventDefault();
    if (session.activeId.value) session.closeTab(session.activeId.value);
    return;
  }
  if (mod && key === "b") {
    event.preventDefault();
    session.toggleSidebar();
    return;
  }
  if (mod && event.key === ",") {
    event.preventDefault();
    session.showPage("settings");
    return;
  }
  if (event.ctrlKey && event.key === "Tab") {
    event.preventDefault();
    const tabs = session.tabs.value;
    if (tabs.length < 2) return;
    const index = tabs.findIndex((tab) => tab.id === session.activeId.value);
    const step = event.shiftKey ? -1 : 1;
    session.activateTab(tabs[(index + step + tabs.length) % tabs.length]!.id);
    return;
  }
  if (!mod && event.altKey && /^[1-9]$/.test(event.key)) {
    const tab = session.tabs.value[Number(event.key) - 1];
    if (tab) {
      event.preventDefault();
      session.activateTab(tab.id);
    }
    return;
  }
  if (!mod && !event.altKey && event.key === "?" && !typingTarget(event)) {
    event.preventDefault();
    session.openHelp();
  }
}

function onHashChange() {
  session.page.value = parseHash(location.hash);
}

let media: MediaQueryList | null = null;
function onSchemeChange() {
  if (settings.theme === "system") applyTheme();
}

onMounted(() => {
  applyTheme();
  window.addEventListener("keydown", onKey);
  window.addEventListener("hashchange", onHashChange);
  if (typeof window.matchMedia === "function") {
    media = window.matchMedia("(prefers-color-scheme: dark)");
    media.addEventListener("change", onSchemeChange);
  }
  void syncProductIdentity().then((info) => {
    dataDir.value = info?.data_dir ?? null;
  });
  void startCatalog();
  void connectDroppedFiles(onDragDrop).then((stop) => {
    if (dragDropAlive) stopDragDrop = stop;
    else stop?.();
  });
});

onBeforeUnmount(() => {
  dragDropAlive = false;
  stopDragDrop?.();
  stopDragDrop = null;
  window.removeEventListener("keydown", onKey);
  window.removeEventListener("hashchange", onHashChange);
  media?.removeEventListener("change", onSchemeChange);
  attachCloser(() => undefined);
  uninstallCloseCoordinator(session);
});
</script>

<template>
  <div class="app-root" :class="{ 'sidebar-hidden': session.sidebarHidden.value || session.page.value !== 'workspace', 'custom-chrome': windowChrome.custom, 'is-maximized': windowChrome.maximized }">
    <a class="skip" href="#main">{{ t("shell.skipToMain") }}</a>
    <TitleBar :theme="settings.theme" @palette="session.openPalette('all')" @help="session.openHelp()" @set-theme="setTheme" @window-state="windowChrome = $event" @new-query="session.openQuery()" @toggle-sidebar="session.toggleSidebar()" />
    <div class="body">
      <ActivityRail :page="session.page.value" @navigate="onNavigate" />
      <button v-if="session.page.value === 'workspace' && !session.sidebarHidden.value" class="sidebar-dismiss" :aria-label="t('shell.closeExplorer')" @click="session.toggleSidebar()"></button>
      <Sidebar
        v-if="session.page.value === 'workspace' && !session.sidebarHidden.value"
        :width="session.sidebarWidth.value"
        @resize="session.sidebarWidth.value = $event"
        @open-object="onOpenObject"
        @open-definition="onOpenDefinition"
        @open-diagram="onOpenDiagram"
        @open-keys="onOpenKeys"
        @open-docs="onOpenDocs"
        @query-object="onQueryObject"
        @insert-object="onInsertObject"
        @focused-database="onFocusedDatabase"
        @new-connection="session.showPage('connections')"
        @new-query="onTreeNewQuery"
      />
      <main id="main" tabindex="-1">
        <WorkspaceView
          v-show="session.page.value === 'workspace'"
          :tabs="session.tabs.value"
          :active-id="session.activeId.value"
          :page-size="settings.pageSize"
          :execute-request="executeRequest"
          @activate="session.activateTab"
          @close="session.closeTab"
          @new-query="session.openQuery()"
          @connections="session.showPage('connections')"
          @sqlite="requestSqlite()"
          @compare="compareOpen = true"
          @history-open="onHistoryOpen"
          @show-history="section => { historySection = section; session.showPage('history'); }"
          @help="session.openHelp()"
          @palette="session.openPalette('all')"
          @structure="session.openStructure(STRUCTURE_PLACEHOLDER)"
          @draft="session.setDraft"
          @executed="session.setExecuted"
          @target="session.setQueryTarget"
          @table-view="session.setTableView"
          @copy-definition="onCopyDefinition"
          @jump-reference="onJumpReference"
          @query-target="target => session.openQueryDraft({ ...target, sql: '', needsTarget: false, notice: null })"
        />
        <ConnectionsPage v-if="session.page.value === 'connections'" :sqlite-prompt="sqlitePrompt" @sqlite-consumed="sqlitePrompt = 0" @workspace="session.showPage('workspace')" @query-target="target => session.openQueryDraft({ ...target, sql: '', needsTarget: false, notice: null })" />
        <HistoryPage
          v-else-if="session.page.value === 'history'"
          :section="historySection"
          :current-draft="currentSavedDraft"
          :save-request="saveRequest"
          @open="onHistoryOpen"
          @update:section="historySection = $event"
        />
        <SettingsView v-else-if="session.page.value === 'settings'" :settings="settings" :data-dir="dataDir" @change="onSettings" @help="session.openHelp()" />
      </main>
    </div>
    <StatusBar
      :tab="session.activeTab.value"
      :on-workspace="session.page.value === 'workspace'"
      :activity="activity"
      :running-tab-id="activeRun?.tab.id ?? null"
      :cancel-requested="activeRun?.run.status === 'cancel-requested'"
      @cancel="onStatusCancel"
    />
    <CommandPalette :open="session.paletteOpen.value" :mode="session.paletteMode.value" :items="commands" @close="session.closePalette()" @run="runCommand" />
    <CompareDialog v-if="compareOpen" @close="compareOpen = false" @place="onPlaceCompare" />
    <DragOverlay :active="dragHovering" />
    <DragPreview
      :open="dragState !== null"
      :state="dragState"
      :budget-bytes="previewBudgetBytes(settings.previewKB)"
      @cancel="dragState = null"
      @proceed="seed => { dragImport = seed; dragState = null; }"
    />
    <DragImportDialog v-if="dragImport" :seed="dragImport" @close="dragImport = null" />
    <ToastHost />
    <ShortcutHelp :open="session.helpOpen.value" @close="session.closeHelp()" />
    <dialog v-if="closePrompt.open" ref="closeDialog" class="modal" aria-labelledby="close-guard-title" @cancel.prevent="chooseStay">
      <div class="modal-head">
        <div class="modal-titles">
          <h2 id="close-guard-title">{{ closePrompt.scope === "window" ? t("shell.closeGuard.closeWindow") : t("shell.closeGuard.closeTab") }}</h2>
          <p class="modal-sub">{{ closePromptText }}</p>
        </div>
      </div>
      <div class="modal-foot">
        <span class="grow"></span>
        <button class="btn" type="button" data-action="close-stay" @click="chooseStay">{{ t("shell.closeGuard.stay") }}</button>
        <button class="btn danger" type="button" data-action="close-leave" @click="chooseLeave">{{ t("shell.closeGuard.leave") }}</button>
      </div>
    </dialog>
  </div>
</template>
