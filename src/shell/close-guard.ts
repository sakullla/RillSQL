import { reactive } from "vue";
import { guardClose, tabRun } from "../query/runtime";

const checks = new Map<string, Set<() => boolean>>();
const discardOnce = new Set<string>();

/** In-app close prompt. Registration is only uncommitted cell edits, never SQL drafts. */
export const closePrompt = reactive({
  open: false,
  scope: "tab" as "tab" | "window",
  tabId: null as string | null,
});

let tabCloser: (id: string) => void = () => undefined;
let openTabs: () => readonly string[] = () => [];
let windowDecision: Promise<boolean> | null = null;
let resolveWindow: ((leave: boolean) => void) | null = null;
/** Lets a title-bar close call the native window close without the system hook asking again. */
let releaseSystemClose = false;
let titleBarFlight: Promise<void> | null = null;

export function registerPending(tabId: string, isPending: () => boolean): () => void {
  let group = checks.get(tabId);
  if (!group) {
    group = new Set();
    checks.set(tabId, group);
  }
  group.add(isPending);
  return () => {
    const current = checks.get(tabId);
    if (!current) return;
    current.delete(isPending);
    if (current.size === 0) checks.delete(tabId);
  };
}

export function tabHasPending(tabId: string): boolean {
  const group = checks.get(tabId);
  if (!group) return false;
  for (const isPending of group) {
    if (isPending()) return true;
  }
  return false;
}

export function anyPending(): boolean {
  for (const tabId of checks.keys()) {
    if (tabHasPending(tabId)) return true;
  }
  return false;
}

export function installCloseCoordinator(session: {
  tabs: { value: readonly { id: string }[] };
  closeTab: (id: string) => void;
  setCloseGuard: (guard: ((id: string) => boolean) | null) => void;
}): void {
  tabCloser = (id) => session.closeTab(id);
  openTabs = () => session.tabs.value.map((tab) => tab.id);
  session.setCloseGuard(coordinateTabClose);
}

export function uninstallCloseCoordinator(session: { setCloseGuard: (guard: null) => void }): void {
  session.setCloseGuard(null);
  tabCloser = () => undefined;
  openTabs = () => [];
  if (closePrompt.open) chooseStay();
}

function live(tabId: string): boolean {
  const status = tabRun(tabId)?.status;
  return status === "running" || status === "cancel-requested";
}

/** Session close slot. Running queries go through guardClose before any pending prompt. */
export function coordinateTabClose(tabId: string): boolean {
  if (live(tabId)) {
    guardClose(tabId);
    return false;
  }
  if (discardOnce.delete(tabId)) {
    checks.delete(tabId);
    guardClose(tabId);
    return true;
  }
  if (tabHasPending(tabId)) {
    if (!closePrompt.open) {
      closePrompt.scope = "tab";
      closePrompt.tabId = tabId;
      closePrompt.open = true;
    }
    return false;
  }
  guardClose(tabId);
  return true;
}

function blockLiveQueries(): boolean {
  let blocked = false;
  for (const id of openTabs()) {
    if (!live(id)) continue;
    guardClose(id);
    blocked = true;
  }
  return blocked;
}

function askWindow(): Promise<boolean> {
  if (windowDecision) return windowDecision;
  closePrompt.scope = "window";
  closePrompt.tabId = null;
  closePrompt.open = true;
  windowDecision = new Promise((resolve) => {
    resolveWindow = resolve;
  });
  return windowDecision;
}

/** Whether the window may close. Title bar and onCloseRequested both use this. */
export function evaluateWindowClose(): Promise<boolean> {
  if (releaseSystemClose) return Promise.resolve(true);
  if (blockLiveQueries()) return Promise.resolve(false);
  if (!anyPending()) return Promise.resolve(true);
  if (closePrompt.open && closePrompt.scope === "tab") return Promise.resolve(false);
  return askWindow();
}

export function chooseStay(): void {
  if (!closePrompt.open) return;
  if (closePrompt.scope === "window") {
    finishWindow(false);
    return;
  }
  closePrompt.open = false;
  closePrompt.tabId = null;
}

export function chooseLeave(): void {
  if (!closePrompt.open) return;
  if (closePrompt.scope === "window") {
    finishWindow(true);
    return;
  }
  const tabId = closePrompt.tabId;
  closePrompt.open = false;
  closePrompt.tabId = null;
  if (!tabId) return;
  discardOnce.add(tabId);
  tabCloser(tabId);
}

function finishWindow(leave: boolean): void {
  closePrompt.open = false;
  closePrompt.tabId = null;
  const resolve = resolveWindow;
  resolveWindow = null;
  windowDecision = null;
  resolve?.(leave);
}

export function handleCloseRequested(event: { preventDefault(): void }): Promise<void> {
  if (releaseSystemClose) return Promise.resolve();
  return evaluateWindowClose().then(
    (leave) => {
      if (!leave) event.preventDefault();
    },
    () => {
      event.preventDefault();
    },
  );
}

export function closeFromTitleBar(nativeClose: () => Promise<void>): Promise<void> {
  if (titleBarFlight) return titleBarFlight;
  titleBarFlight = (async () => {
    const leave = await evaluateWindowClose();
    if (!leave) return;
    releaseSystemClose = true;
    try {
      await nativeClose();
    } finally {
      releaseSystemClose = false;
    }
  })().finally(() => {
    titleBarFlight = null;
  });
  return titleBarFlight;
}

export function resetCloseGuard(): void {
  checks.clear();
  discardOnce.clear();
  releaseSystemClose = false;
  titleBarFlight = null;
  tabCloser = () => undefined;
  openTabs = () => [];
  const resolve = resolveWindow;
  resolveWindow = null;
  windowDecision = null;
  closePrompt.open = false;
  closePrompt.scope = "tab";
  closePrompt.tabId = null;
  resolve?.(false);
}
