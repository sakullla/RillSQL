import { computed, ref, type Ref } from "vue";
import type { AppPage } from "../router";
import { parseHash, toHash } from "../router";
import type { ConnectionStatus } from "./status-text";

export type TabKind = "query" | "table" | "structure";

export const NEW_QUERY_DRAFT = "";

export interface PlaceholderTarget {
  id: string;
  label: string;
  path?: string;
  pane?: "data" | "structure";
  section?: "columns" | "drop";
}

export const STRUCTURE_PLACEHOLDER: PlaceholderTarget = {
  id: "placeholder",
  label: "结构占位",
};

export interface QueryTarget {
  connectionId: string;
  connectionName: string;
  database: string;
  engine: string;
  status: ConnectionStatus;
}

export interface QueryDraftInput {
  sql: string;
  connectionId: string | null;
  connectionName: string | null;
  database: string | null;
  engine: string | null;
  status: ConnectionStatus | null;
  needsTarget: boolean;
  notice: string | null;
}

export interface ShellTab {
  id: string;
  kind: TabKind;
  title: string;
  targetText: string | null;
  path: string | null;
  draft: string;
  connectionId: string | null;
  connectionName: string | null;
  database: string | null;
  engine: string | null;
  connectionStatus: ConnectionStatus | null;
  targetNotice: string | null;
  tablePane: "data" | "structure";
  structureSection: "columns" | "indexes" | "foreign" | "drop";
}

export interface ShellSession {
  page: Ref<AppPage>;
  tabs: Ref<ShellTab[]>;
  activeId: Ref<string | null>;
  sidebarHidden: Ref<boolean>;
  sidebarWidth: Ref<number>;
  paletteOpen: Ref<boolean>;
  paletteMode: Ref<"all" | "tables">;
  helpOpen: Ref<boolean>;
  activeTab: Ref<ShellTab | null>;
  showPage: (page: AppPage) => void;
  openQuery: () => void;
  openQueryDraft: (input: QueryDraftInput) => string;
  openTable: (target: PlaceholderTarget) => void;
  openStructure: (target: PlaceholderTarget) => void;
  setTableView: (id: string, pane: "data" | "structure", section?: "columns" | "drop") => void;
  setDraft: (id: string, draft: string) => void;
  setQueryTarget: (id: string, target: QueryTarget | null) => void;
  setCloseGuard: (guard: ((id: string) => boolean) | null) => void;
  activateTab: (id: string) => void;
  closeTab: (id: string) => void;
  toggleSidebar: () => void;
  openPalette: (mode?: "all" | "tables") => void;
  closePalette: () => void;
  openHelp: () => void;
  closeHelp: () => void;
}

function blankTab(kind: TabKind, id: string, title: string, path: string | null, draft: string): ShellTab {
  return {
    id,
    kind,
    title,
    targetText: null,
    path,
    draft,
    connectionId: null,
    connectionName: null,
    database: null,
    engine: null,
    connectionStatus: null,
    targetNotice: null,
    tablePane: "data",
    structureSection: "columns",
  };
}

function applyTarget(tab: ShellTab, target: QueryTarget | null) {
  if (!target) {
    tab.connectionId = null;
    tab.connectionName = null;
    tab.database = null;
    tab.engine = null;
    tab.connectionStatus = null;
    tab.targetText = null;
    return;
  }
  tab.connectionId = target.connectionId;
  tab.connectionName = target.connectionName;
  tab.database = target.database;
  tab.engine = target.engine;
  tab.connectionStatus = target.status;
  tab.targetText = `${target.connectionName} / ${target.database || "main"}`;
  tab.targetNotice = null;
}

export function createSession(initialHash = ""): ShellSession {
  const page = ref<AppPage>(parseHash(initialHash));
  const tabs = ref<ShellTab[]>([]);
  const activeId = ref<string | null>(null);
  const sidebarHidden = ref(false);
  const sidebarWidth = ref(268);
  const paletteOpen = ref(false);
  const paletteMode = ref<"all" | "tables">("all");
  const helpOpen = ref(false);
  let sequence = 1;
  let closeGuard: ((id: string) => boolean) | null = null;

  const activeTab = computed(() => tabs.value.find((tab) => tab.id === activeId.value) ?? null);

  function showPage(next: AppPage) {
    page.value = next;
    if (next === "workspace") sidebarHidden.value = false;
    const hash = toHash(next);
    if (typeof location !== "undefined" && location.hash !== hash) location.hash = hash;
  }

  function openQuery() {
    const count = tabs.value.filter((tab) => tab.kind === "query").length + 1;
    const tab = blankTab("query", `query-${sequence}`, `查询 ${count}`, null, NEW_QUERY_DRAFT);
    sequence += 1;
    tabs.value = [...tabs.value, tab];
    activeId.value = tab.id;
    showPage("workspace");
  }

  function openQueryDraft(input: QueryDraftInput): string {
    const count = tabs.value.filter((tab) => tab.kind === "query").length + 1;
    const tab = blankTab("query", `query-${sequence}`, `查询 ${count}`, null, input.sql);
    sequence += 1;
    tab.targetNotice = input.notice;
    if (!input.needsTarget && input.connectionId && input.connectionName && input.database != null && input.engine && input.status) {
      applyTarget(tab, {
        connectionId: input.connectionId,
        connectionName: input.connectionName,
        database: input.database,
        engine: input.engine,
        status: input.status,
      });
    }
    tabs.value = [...tabs.value, tab];
    activeId.value = tab.id;
    showPage("workspace");
    return tab.id;
  }

  function openPlaceholder(kind: "table" | "structure", target: PlaceholderTarget) {
    const id = `${kind}:${target.id}`;
    const existing = tabs.value.find((tab) => tab.id === id);
    if (existing) {
      if (target.pane) existing.tablePane = target.pane;
      if (target.section) existing.structureSection = target.section;
      activeId.value = existing.id;
      showPage("workspace");
      return;
    }
    const tab = blankTab(kind, id, target.label, target.path ?? null, "");
    if (target.pane) tab.tablePane = target.pane;
    if (target.section) tab.structureSection = target.section;
    tabs.value = [...tabs.value, tab];
    activeId.value = tab.id;
    showPage("workspace");
  }

  function setTableView(id: string, pane: "data" | "structure", section?: "columns" | "drop") {
    const tab = tabs.value.find((item) => item.id === id);
    if (!tab || tab.kind !== "table") return;
    tab.tablePane = pane;
    if (section) tab.structureSection = section;
  }

  function openTable(target: PlaceholderTarget) {
    openPlaceholder("table", target);
  }

  function openStructure(target: PlaceholderTarget) {
    openPlaceholder("structure", target);
  }

  function setDraft(id: string, draft: string) {
    const tab = tabs.value.find((item) => item.id === id);
    if (!tab || tab.draft === draft) return;
    tab.draft = draft;
  }

  function setQueryTarget(id: string, target: QueryTarget | null) {
    const tab = tabs.value.find((item) => item.id === id);
    if (!tab || tab.kind !== "query") return;
    applyTarget(tab, target);
  }

  function setCloseGuard(guard: ((id: string) => boolean) | null) {
    closeGuard = guard;
  }

  function activateTab(id: string) {
    if (!tabs.value.some((tab) => tab.id === id)) return;
    activeId.value = id;
    showPage("workspace");
  }

  function closeTab(id: string) {
    if (closeGuard && closeGuard(id) === false) return;
    const index = tabs.value.findIndex((tab) => tab.id === id);
    if (index < 0) return;
    const next = tabs.value.filter((tab) => tab.id !== id);
    tabs.value = next;
    if (activeId.value === id) {
      activeId.value = next[Math.min(index, next.length - 1)]?.id ?? null;
    }
  }

  function toggleSidebar() {
    if (page.value !== "workspace") {
      showPage("workspace");
      sidebarHidden.value = false;
      return;
    }
    sidebarHidden.value = !sidebarHidden.value;
  }

  function openPalette(mode: "all" | "tables" = "all") {
    paletteMode.value = mode;
    paletteOpen.value = true;
    helpOpen.value = false;
  }

  function closePalette() {
    paletteOpen.value = false;
  }

  function openHelp() {
    helpOpen.value = true;
    paletteOpen.value = false;
  }

  function closeHelp() {
    helpOpen.value = false;
  }

  return {
    page,
    tabs,
    activeId,
    sidebarHidden,
    sidebarWidth,
    paletteOpen,
    paletteMode,
    helpOpen,
    activeTab,
    showPage,
    openQuery,
    openQueryDraft,
    openTable,
    openStructure,
    setTableView,
    setDraft,
    setQueryTarget,
    setCloseGuard,
    activateTab,
    closeTab,
    toggleSidebar,
    openPalette,
    closePalette,
    openHelp,
    closeHelp,
  };
}
