<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, reactive, ref } from "vue";
import { listPaletteTables, startCatalog } from "../catalog";
import { objectIdentity, type CatalogObjectRef } from "../catalog/object-ref";
import { catalogConnections } from "../catalog/store";
import ConnectionsPage from "../connections/ConnectionsPage.vue";
import { listPaletteConnections } from "../connections";
import HistoryPage from "../history/HistoryPage.vue";
import type { OpenPlan } from "../history/plan";
import { attachCloser, guardClose } from "../query/runtime";
import { parseHash } from "../router";
import ActivityRail from "./ActivityRail.vue";
import CommandPalette from "./CommandPalette.vue";
import { shellCommands, type PaletteItem } from "./commands";
import { syncProductIdentity } from "./identity";
import SettingsView from "./SettingsView.vue";
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

const session = createSession(typeof location === "undefined" ? "" : location.hash);
const windowChrome = ref({ custom: false, maximized: false });
attachCloser((id) => session.closeTab(id));
session.setCloseGuard((id) => guardClose(id));

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
      notice: "原来的连接已删除——在查询里重新选择一个连接和库后再执行。",
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
    showPage: session.showPage,
    toggleSidebar: session.toggleSidebar,
    setTheme,
    openHelp: session.openHelp,
    requestSqlite,
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
  session.setCloseGuard(null);
});
</script>

<template>
  <div class="app-root" :class="{ 'sidebar-hidden': session.sidebarHidden.value || session.page.value !== 'workspace', 'custom-chrome': windowChrome.custom, 'is-maximized': windowChrome.maximized }">
    <a class="skip" href="#main">跳到主区域</a>
    <TitleBar :theme="settings.theme" @palette="session.openPalette('all')" @help="session.openHelp()" @set-theme="setTheme" @window-state="windowChrome = $event" />
    <div class="body">
      <ActivityRail :page="session.page.value" @navigate="onNavigate" />
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
          v-if="session.page.value === 'workspace'"
          :tabs="session.tabs.value"
          :active-id="session.activeId.value"
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
        />
        <ConnectionsPage v-else-if="session.page.value === 'connections'" :sqlite-prompt="sqlitePrompt" />
        <HistoryPage v-else-if="session.page.value === 'history'" @open="onHistoryOpen" />
        <SettingsView v-else :settings="settings" :data-dir="dataDir" @change="onSettings" @help="session.openHelp()" />
      </main>
    </div>
    <StatusBar :tab="session.activeTab.value" :on-workspace="session.page.value === 'workspace'" />
    <CommandPalette :open="session.paletteOpen.value" :mode="session.paletteMode.value" :items="commands" @close="session.closePalette()" @run="runCommand" />
    <ShortcutHelp :open="session.helpOpen.value" @close="session.closeHelp()" />
  </div>
</template>
