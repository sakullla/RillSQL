import { computed, ref, type Ref } from "vue";
import type { AppPage } from "../router";
import { parseHash, toHash } from "../router";

export type TabKind = "query" | "table" | "structure";

export const NEW_QUERY_DRAFT = "-- 查询模块占位。执行、取消、补全和结果尚未接入。\n";

export interface PlaceholderTarget {
  id: string;
  label: string;
  path?: string;
}

export const STRUCTURE_PLACEHOLDER: PlaceholderTarget = {
  id: "placeholder",
  label: "结构占位",
};

export interface ShellTab {
  id: string;
  kind: TabKind;
  title: string;
  targetText: string | null;
  path: string | null;
  draft: string;
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
  openTable: (target: PlaceholderTarget) => void;
  openStructure: (target: PlaceholderTarget) => void;
  setDraft: (id: string, draft: string) => void;
  activateTab: (id: string) => void;
  closeTab: (id: string) => void;
  toggleSidebar: () => void;
  openPalette: (mode?: "all" | "tables") => void;
  closePalette: () => void;
  openHelp: () => void;
  closeHelp: () => void;
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

  const activeTab = computed(() => tabs.value.find((tab) => tab.id === activeId.value) ?? null);

  function showPage(next: AppPage) {
    page.value = next;
    if (next === "workspace") sidebarHidden.value = false;
    const hash = toHash(next);
    if (typeof location !== "undefined" && location.hash !== hash) location.hash = hash;
  }

  function openQuery() {
    const count = tabs.value.filter((tab) => tab.kind === "query").length + 1;
    const tab: ShellTab = {
      id: `query-${sequence}`,
      kind: "query",
      title: `查询 ${count}`,
      targetText: null,
      path: null,
      draft: NEW_QUERY_DRAFT,
    };
    sequence += 1;
    tabs.value = [...tabs.value, tab];
    activeId.value = tab.id;
    showPage("workspace");
  }

  function openPlaceholder(kind: "table" | "structure", target: PlaceholderTarget) {
    const id = `${kind}:${target.id}`;
    const existing = tabs.value.find((tab) => tab.id === id);
    if (existing) {
      activeId.value = existing.id;
      showPage("workspace");
      return;
    }
    const tab: ShellTab = {
      id,
      kind,
      title: target.label,
      targetText: null,
      path: target.path ?? null,
      draft: "",
    };
    tabs.value = [...tabs.value, tab];
    activeId.value = tab.id;
    showPage("workspace");
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

  function activateTab(id: string) {
    if (!tabs.value.some((tab) => tab.id === id)) return;
    activeId.value = id;
    showPage("workspace");
  }

  function closeTab(id: string) {
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
    openTable,
    openStructure,
    setDraft,
    activateTab,
    closeTab,
    toggleSidebar,
    openPalette,
    closePalette,
    openHelp,
    closeHelp,
  };
}
