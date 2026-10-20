import { computed, ref, type Ref } from "vue";
import type { AppPage } from "../router";
import { parseHash, toHash } from "../router";
import { CONNECTION_STATUS_TEXT, type ConnectionStatus } from "./status-text";

export type TabKind = "query" | "table" | "structure" | "definition";

export interface DefinitionRequest {
  schema: string;
  name: string;
  kind: string;
  signature: string;
  relation: string;
}

export interface DefinitionTarget {
  id: string;
  label: string;
  path?: string;
  connectionId: string;
  connectionName: string;
  database: string;
  engine: string;
  status: ConnectionStatus;
  schema: string;
  name: string;
  kind: string;
  signature: string;
  relation: string;
}

export const NEW_QUERY_DRAFT = "";

export const SESSION_KEY = "rillsql.session";

/**
 * Persisted query tab. Table/structure/definition tabs depend on catalog state
 * that is not restored on startup, so only query tabs are snapshotted.
 */
export interface SessionSnapshotTab {
  id: string;
  title: string;
  draft: string;
  connectionId: string | null;
  connectionName: string | null;
  database: string | null;
  engine: string | null;
  connectionStatus: ConnectionStatus | null;
  targetText: string | null;
  targetNotice: string | null;
}

export interface SessionSnapshot {
  tabs: SessionSnapshotTab[];
  activeId: string | null;
}

export interface PlaceholderTarget {
  id: string;
  label: string;
  path?: string;
  pane?: "data" | "structure";
  section?: "columns" | "drop";
}

export const STRUCTURE_PLACEHOLDER: PlaceholderTarget = {
  id: "placeholder",
  label: "新建表结构",
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
  definition?: DefinitionRequest | null;
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
  openDefinition: (target: DefinitionTarget) => void;
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
    definition: null,
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

export function snapshotSession(tabs: readonly ShellTab[], activeId: string | null): SessionSnapshot {
  const kept = tabs.filter((tab) => tab.kind === "query");
  return {
    tabs: kept.map((tab) => ({
      id: tab.id,
      title: tab.title,
      draft: tab.draft,
      connectionId: tab.connectionId,
      connectionName: tab.connectionName,
      database: tab.database,
      engine: tab.engine,
      connectionStatus: tab.connectionStatus,
      targetText: tab.targetText,
      targetNotice: tab.targetNotice,
    })),
    // 活动标签若不是查询标签（快照里被剔除），回落到最后一个查询标签。
    activeId: kept.some((tab) => tab.id === activeId) ? activeId : kept[kept.length - 1]?.id ?? null,
  };
}

function optionalText(value: unknown): boolean {
  return value === null || typeof value === "string";
}

/** Parse persisted session JSON. Any malformed shape or field returns null so the app starts fresh. */
export function restoreSession(raw: string | null): SessionSnapshot | null {
  if (!raw) return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return null;
  }
  if (typeof parsed !== "object" || parsed === null) return null;
  const source = parsed as { tabs?: unknown; activeId?: unknown };
  if (!Array.isArray(source.tabs)) return null;
  const activeId = source.activeId ?? null;
  if (activeId !== null && typeof activeId !== "string") return null;
  const tabs: SessionSnapshotTab[] = [];
  for (const item of source.tabs) {
    if (typeof item !== "object" || item === null) return null;
    const tab = item as Record<string, unknown>;
    if (typeof tab.id !== "string" || tab.id === "") return null;
    if (typeof tab.title !== "string") return null;
    if (typeof tab.draft !== "string") return null;
    const status = tab.connectionStatus ?? null;
    if (status !== null && !(typeof status === "string" && status in CONNECTION_STATUS_TEXT)) return null;
    if (!optionalText(tab.connectionId)) return null;
    if (!optionalText(tab.connectionName)) return null;
    if (!optionalText(tab.database)) return null;
    if (!optionalText(tab.engine)) return null;
    if (!optionalText(tab.targetText)) return null;
    if (!optionalText(tab.targetNotice)) return null;
    tabs.push({
      id: tab.id,
      title: tab.title,
      draft: tab.draft,
      connectionId: tab.connectionId as string | null,
      connectionName: tab.connectionName as string | null,
      database: tab.database as string | null,
      engine: tab.engine as string | null,
      connectionStatus: status as ConnectionStatus | null,
      targetText: tab.targetText as string | null,
      targetNotice: tab.targetNotice as string | null,
    });
  }
  const resolved = tabs.some((tab) => tab.id === activeId) ? activeId : tabs[tabs.length - 1]?.id ?? null;
  return { tabs, activeId: resolved };
}

function fromSnapshot(item: SessionSnapshotTab): ShellTab {
  const tab = blankTab("query", item.id, item.title, null, item.draft);
  tab.connectionId = item.connectionId;
  tab.connectionName = item.connectionName;
  tab.database = item.database;
  tab.engine = item.engine;
  tab.connectionStatus = item.connectionStatus;
  tab.targetText = item.targetText;
  tab.targetNotice = item.targetNotice;
  return tab;
}

export function createSession(initialHash = "", restored: SessionSnapshot | null = null): ShellSession {
  const page = ref<AppPage>(parseHash(initialHash));
  const tabs = ref<ShellTab[]>([]);
  const activeId = ref<string | null>(null);
  const sidebarHidden = ref(typeof window !== "undefined" && window.innerWidth <= 960);
  const sidebarWidth = ref(268);
  const paletteOpen = ref(false);
  const paletteMode = ref<"all" | "tables">("all");
  const helpOpen = ref(false);
  let sequence = 1;
  let closeGuard: ((id: string) => boolean) | null = null;

  if (restored && restored.tabs.length > 0) {
    tabs.value = restored.tabs.map(fromSnapshot);
    activeId.value = restored.tabs.some((tab) => tab.id === restored.activeId)
      ? restored.activeId
      : restored.tabs[restored.tabs.length - 1]!.id;
    // 续号到恢复的 query-N 最大 N 之后，避免新标签与恢复标签撞 id。
    sequence =
      restored.tabs.reduce((max, tab) => {
        const match = /^query-(\d+)$/.exec(tab.id);
        return match ? Math.max(max, Number(match[1])) : max;
      }, 0) + 1;
  }

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

  function openDefinition(target: DefinitionTarget) {
    const id = `definition:${target.id}`;
    const existing = tabs.value.find((tab) => tab.id === id);
    if (existing) {
      activeId.value = existing.id;
      showPage("workspace");
      return;
    }
    const tab = blankTab("definition", id, target.label, target.path ?? null, "");
    applyTarget(tab, {
      connectionId: target.connectionId,
      connectionName: target.connectionName,
      database: target.database,
      engine: target.engine,
      status: target.status,
    });
    tab.definition = {
      schema: target.schema,
      name: target.name,
      kind: target.kind,
      signature: target.signature,
      relation: target.relation,
    };
    tabs.value = [...tabs.value, tab];
    activeId.value = tab.id;
    showPage("workspace");
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
    openDefinition,
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
