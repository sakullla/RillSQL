import { computed, ref, type Ref } from "vue";
import type { AppPage } from "../router";
import { parseHash, toHash } from "../router";

export type TabKind = "query" | "table" | "structure";

export interface ShellTab {
  id: string;
  kind: TabKind;
  title: string;
  targetText: string | null;
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
    };
    sequence += 1;
    tabs.value = [...tabs.value, tab];
    activeId.value = tab.id;
    showPage("workspace");
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
    activateTab,
    closeTab,
    toggleSidebar,
    openPalette,
    closePalette,
    openHelp,
    closeHelp,
  };
}
