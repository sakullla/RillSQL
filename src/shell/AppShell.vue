<script setup lang="ts">
import { computed, defineAsyncComponent, nextTick, onBeforeUnmount, onMounted, reactive, ref, watch } from "vue";
import { listPaletteTables, startCatalog } from "../catalog";
import { objectIdentity, type CatalogObjectRef } from "../catalog/object-ref";
import { catalogConnections } from "../catalog/store";
import { listPaletteConnections } from "../connections";
import type { SavedDraft } from "../history/saved";
import { deletedConnectionNotice, type OpenPlan } from "../history/plan";
import { attachCloser } from "../query/runtime";
import { parseHash } from "../router";
import { chooseLeave, chooseStay, closePrompt, installCloseCoordinator, uninstallCloseCoordinator } from "./close-guard";
import ActivityRail from "./ActivityRail.vue";
import CommandPalette from "./CommandPalette.vue";
import { shellCommands, type PaletteItem } from "./commands";
import { syncProductIdentity } from "./identity";
import { loadSettings, saveSettings, type ShellSettings } from "./settings";
import { createSession, STRUCTURE_PLACEHOLDER } from "./session";
import { shellMenus } from "./menus";
import type { MenuDefinition } from "./menu-model";
import type { ConnectionStatus } from "./status-text";
import ShortcutHelp from "./ShortcutHelp.vue";
import Sidebar from "./Sidebar.vue";
import StatusBar from "./StatusBar.vue";
import { applyDocumentTheme, prefersDarkScheme, type ThemePreference } from "./theme";
import TitleBar from "./TitleBar.vue";
import type { ToolbarAction } from "./toolbar-model";
import WorkspaceView from "./WorkspaceView.vue";
import type { AppPage } from "../router";

const ConnectionsPage = defineAsyncComponent(() => import("../connections/ConnectionsPage.vue"));
const HistoryPage = defineAsyncComponent(() => import("../history/HistoryPage.vue"));
const SettingsView = defineAsyncComponent(() => import("./SettingsView.vue"));

const session = createSession(typeof location === "undefined" ? "" : location.hash);
const windowChrome = ref({ custom: false, maximized: false });
const closeDialog = ref<HTMLDialogElement | null>(null);
attachCloser((id) => session.closeTab(id));
installCloseCoordinator(session);

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

const historySection = ref<"runs" | "saved">("runs");
const saveRequest = ref(0);
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

const commands = computed<PaletteItem[]>(() => [
  ...listPaletteTables().map((table) => ({
    id: table.id,
    group: "表与视图" as const,
    label: table.label,
    icon: "table" as const,
    path: table.path,
    run: () => session.openTable({ id: table.id, label: table.label, path: table.path }),
  })),
  ...listPaletteConnections().map((connection) => ({
    id: connection.id,
    group: "连接" as const,
    label: connection.label,
    icon: "database" as const,
    path: connection.path,
    run: () => session.showPage("connections"),
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
  }),
]);

const hasQueryTab = computed(() => session.activeTab.value?.kind === "query");
const menus = computed<MenuDefinition[]>(() => shellMenus({ hasQueryTab: hasQueryTab.value }, settings.theme));

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
  if (window.innerWidth <= 960) session.sidebarHidden.value = true;
  session.openTable({
    id: objectIdentity(object),
    label: object.name,
    path: object.path,
    pane,
    section,
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

/** 对象树连接节点的编辑/测试/复制/删除：透传到连接页，由连接页打开对应弹窗。 */
const connActionSeq = ref(0);
const connectionActionRequest = ref<{ id: number; connectionId: string; action: "edit" | "test" | "copy" | "delete" } | null>(null);
function onConnectionAction(connectionId: string, action: "edit" | "test" | "copy" | "delete") {
  connActionSeq.value += 1;
  connectionActionRequest.value = { id: connActionSeq.value, connectionId, action };
  session.showPage("connections");
}

// 离开连接页后清掉请求，避免下次进入连接页时弹窗重开。
watch(
  () => session.page.value,
  (page) => {
    if (page !== "connections") connectionActionRequest.value = null;
  },
);

function onSettings(patch: Partial<ShellSettings>) {
  Object.assign(settings, patch);
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

function onMenuSelect(menuId: string, itemId: string) {
  const key = `${menuId}:${itemId}`;
  switch (key) {
    case "file:new-query":
      return session.openQuery();
    case "file:open-sqlite":
      return requestSqlite();
    case "file:save-draft":
      historySection.value = "saved";
      saveRequest.value += 1;
      return session.showPage("history");
    case "file:settings":
    case "tools:settings":
    case "help:about":
      return session.showPage("settings");
    case "edit:palette":
      return session.openPalette("all");
    case "edit:quick-open":
      return session.openPalette("tables");
    case "view:toggle-sidebar":
      return session.toggleSidebar();
    case "view:theme-light":
      return setTheme("light");
    case "view:theme-dark":
      return setTheme("dark");
    case "view:theme-system":
      return setTheme("system");
    case "view:shortcuts":
    case "help:shortcuts":
      return session.openHelp();
    case "window:close-tab":
      if (session.activeId.value) session.closeTab(session.activeId.value);
      return;
    case "window:close-others": {
      const active = session.activeId.value;
      for (const tab of [...session.tabs.value]) if (tab.id !== active) session.closeTab(tab.id);
      return;
    }
    case "window:close-right": {
      const index = session.tabs.value.findIndex((tab) => tab.id === session.activeId.value);
      if (index < 0) return;
      for (const tab of session.tabs.value.slice(index + 1)) session.closeTab(tab.id);
      return;
    }
    case "query:run":
      return session.requestQueryAction("run");
    case "query:run-statement":
      return session.requestQueryAction("run-statement");
    case "query:cancel":
      return session.requestQueryAction("cancel");
    case "query:format":
      return session.requestQueryAction("format");
    case "query:plan":
      return session.requestQueryAction("plan");
    case "tools:tasks":
      return; // TaskCenter 自管开关
    default:
      return;
  }
}

function onToolbarAction(action: ToolbarAction) {
  switch (action) {
    case "new-query":
      return session.openQuery();
    case "open-connections":
      return session.showPage("connections");
    case "save-draft":
      historySection.value = "saved";
      saveRequest.value += 1;
      return session.showPage("history");
    case "history":
      return session.showPage("history");
    case "palette":
      return session.openPalette("all");
    case "tasks":
      return; // TaskCenter 自管开关
    case "toggle-theme":
      return setTheme(settings.theme === "light" ? "dark" : settings.theme === "dark" ? "system" : "light");
    case "format":
      return session.requestQueryAction("format");
    case "plan":
      return session.requestQueryAction("plan");
    case "run":
      return session.requestQueryAction("run");
  }
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
});

onBeforeUnmount(() => {
  window.removeEventListener("keydown", onKey);
  window.removeEventListener("hashchange", onHashChange);
  media?.removeEventListener("change", onSchemeChange);
  attachCloser(() => undefined);
  uninstallCloseCoordinator(session);
});
</script>

<template>
  <div class="app-root" :class="{ 'sidebar-hidden': session.sidebarHidden.value || session.page.value !== 'workspace', 'custom-chrome': windowChrome.custom, 'is-maximized': windowChrome.maximized }">
    <a class="skip" href="#main">跳到主区域</a>
    <TitleBar
      :theme="settings.theme"
      :menus="menus"
      :has-query-tab="hasQueryTab"
      @palette="session.openPalette('all')"
      @help="session.openHelp()"
      @window-state="windowChrome = $event"
      @toggle-sidebar="session.toggleSidebar()"
      @menu-select="onMenuSelect"
      @action="onToolbarAction"
    />
    <div class="body">
      <ActivityRail :page="session.page.value" @navigate="onNavigate" />
      <button v-if="session.page.value === 'workspace' && !session.sidebarHidden.value" class="sidebar-dismiss" aria-label="关闭资源管理器" @click="session.toggleSidebar()"></button>
      <Sidebar
        v-if="session.page.value === 'workspace' && !session.sidebarHidden.value"
        :width="session.sidebarWidth.value"
        @resize="session.sidebarWidth.value = $event"
        @open-object="onOpenObject"
        @query-object="onQueryObject"
        @focused-database="onFocusedDatabase"
        @connection-action="onConnectionAction"
      />
      <main id="main" tabindex="-1">
        <WorkspaceView
          v-show="session.page.value === 'workspace'"
          :tabs="session.tabs.value"
          :active-id="session.activeId.value"
          :page-size="settings.pageSize"
          :query-action="session.pendingQueryAction.value"
          @activate="session.activateTab"
          @close="session.closeTab"
          @toggle-pin="session.toggleTabPin"
          @new-query="session.openQuery()"
          @connections="session.showPage('connections')"
          @sqlite="requestSqlite()"
          @palette="session.openPalette('all')"
          @structure="session.openStructure(STRUCTURE_PLACEHOLDER)"
          @draft="session.setDraft"
          @target="session.setQueryTarget"
          @table-view="session.setTableView"
          @query-target="target => session.openQueryDraft({ ...target, sql: '', needsTarget: false, notice: null })"
        />
        <ConnectionsPage v-if="session.page.value === 'connections'" :sqlite-prompt="sqlitePrompt" :action-request="connectionActionRequest" @sqlite-consumed="sqlitePrompt = 0" @workspace="session.showPage('workspace')" @query-target="target => session.openQueryDraft({ ...target, sql: '', needsTarget: false, notice: null })" />
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
    <StatusBar :tab="session.activeTab.value" :on-workspace="session.page.value === 'workspace'" @navigate="(target) => session.showPage(target)" />
    <CommandPalette :open="session.paletteOpen.value" :mode="session.paletteMode.value" :items="commands" @close="session.closePalette()" @run="runCommand" />
    <ShortcutHelp :open="session.helpOpen.value" @close="session.closeHelp()" />
    <dialog v-if="closePrompt.open" ref="closeDialog" class="modal" aria-labelledby="close-guard-title" @cancel.prevent="chooseStay">
      <div class="modal-head">
        <div class="modal-titles">
          <h2 id="close-guard-title">{{ closePrompt.scope === "window" ? "关闭窗口？" : "关闭这个标签？" }}</h2>
          <p class="modal-sub">
            {{ closePrompt.scope === "window"
              ? "有标签含未提交的单元格修改。留下则窗口和修改都还在；离开会丢掉这些修改，不会写入数据库。"
              : "这个标签有未提交的单元格修改。留下则标签和修改都还在；离开会丢掉这些修改，不会写入数据库。" }}
          </p>
        </div>
      </div>
      <div class="modal-foot">
        <span class="grow"></span>
        <button class="btn" type="button" data-action="close-stay" @click="chooseStay">留下</button>
        <button class="btn danger" type="button" data-action="close-leave" @click="chooseLeave">离开</button>
      </div>
    </dialog>
  </div>
</template>
