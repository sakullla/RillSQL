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
import type { ConnectionStatus } from "./status-text";
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
const executeRequest = ref(0);
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
    executeQuery: () => {
      executeRequest.value += 1;
    },
  }),
]);

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
    <TitleBar :theme="settings.theme" @palette="session.openPalette('all')" @help="session.openHelp()" @set-theme="setTheme" @window-state="windowChrome = $event" @new-query="session.openQuery()" @toggle-sidebar="session.toggleSidebar()" />
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
          @palette="session.openPalette('all')"
          @structure="session.openStructure(STRUCTURE_PLACEHOLDER)"
          @draft="session.setDraft"
          @target="session.setQueryTarget"
          @table-view="session.setTableView"
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
    <StatusBar :tab="session.activeTab.value" :on-workspace="session.page.value === 'workspace'" />
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
