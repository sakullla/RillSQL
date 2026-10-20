import { reactive } from "vue";
import { guardClose, tabRun } from "../query/runtime";

const checks = new Map<string, Set<() => boolean>>();
const discardOnce = new Set<string>();

/** Close prompt shown for a tab or the whole window. */
export const closePrompt = reactive({
  open: false,
  scope: "tab" as "tab" | "window",
  tabId: null as string | null,
  /** What would be lost: uncommitted cell edits, a non-empty SQL draft, or both. */
  reason: "cells" as "cells" | "draft" | "both",
});

let tabCloser: (id: string) => void = () => undefined;
let openTabs: () => readonly { id: string; kind: string; draft: string }[] = () => [];
let windowDecision: Promise<boolean> | null = null;
let resolveWindow: ((leave: boolean) => void) | null = null;
/**
 * Title bar already chose to leave. Window.close only posts Close and returns;
 * tauri://close-requested runs later, so this stays set until that hook consumes it.
 */
let releaseSystemClose = false;
let releaseHook: (() => void) | null = null;
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

/** Unexecuted SQL drafts are pending too: the text would be lost on close. */
function draftPending(tabId: string): boolean {
  const tab = openTabs().find((item) => item.id === tabId);
  return tab?.kind === "query" && tab.draft.trim() !== "";
}

function anyCellsPending(): boolean {
  for (const tabId of checks.keys()) {
    if (tabHasPending(tabId)) return true;
  }
  return false;
}

function anyDraftPending(): boolean {
  for (const tab of openTabs()) {
    if (tab.kind === "query" && tab.draft.trim() !== "") return true;
  }
  return false;
}

function promptReason(cells: boolean, draft: boolean): "cells" | "draft" | "both" {
  if (cells && draft) return "both";
  if (draft) return "draft";
  return "cells";
}

export function anyPending(): boolean {
  return anyCellsPending() || anyDraftPending();
}

export function installCloseCoordinator(session: {
  tabs: { value: readonly { id: string; kind: string; draft: string }[] };
  closeTab: (id: string) => void;
  setCloseGuard: (guard: ((id: string) => boolean) | null) => void;
}): void {
  tabCloser = (id) => session.closeTab(id);
  openTabs = () => session.tabs.value;
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
  const cells = tabHasPending(tabId);
  const draft = draftPending(tabId);
  if (cells || draft) {
    if (!closePrompt.open) {
      closePrompt.scope = "tab";
      closePrompt.tabId = tabId;
      closePrompt.reason = promptReason(cells, draft);
      closePrompt.open = true;
    }
    return false;
  }
  guardClose(tabId);
  return true;
}

function blockLiveQueries(): boolean {
  let blocked = false;
  for (const tab of openTabs()) {
    if (!live(tab.id)) continue;
    guardClose(tab.id);
    blocked = true;
  }
  return blocked;
}

function askWindow(): Promise<boolean> {
  if (windowDecision) return windowDecision;
  closePrompt.scope = "window";
  closePrompt.tabId = null;
  closePrompt.reason = promptReason(anyCellsPending(), anyDraftPending());
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

function consumeRelease(): void {
  releaseSystemClose = false;
  const done = releaseHook;
  releaseHook = null;
  done?.();
}

/** Window.close will not deliver tauri://close-requested (refused, or not the desktop shell). */
export function releaseCloseWithoutHook(): void {
  consumeRelease();
}

export function handleCloseRequested(event: { preventDefault(): void }): Promise<void> {
  if (releaseSystemClose) {
    // Do not preventDefault: the listener already made Tauri prevent_close, and
    // @tauri-apps/api finishes that close with destroy() only when this is unset.
    consumeRelease();
    return Promise.resolve();
  }
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
    const hookDone = new Promise<void>((resolve) => {
      releaseHook = resolve;
    });
    releaseSystemClose = true;
    try {
      await nativeClose();
      if (releaseSystemClose) await hookDone;
    } finally {
      releaseSystemClose = false;
      releaseHook = null;
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
  const done = releaseHook;
  releaseHook = null;
  done?.();
  titleBarFlight = null;
  tabCloser = () => undefined;
  openTabs = () => [];
  const resolve = resolveWindow;
  resolveWindow = null;
  windowDecision = null;
  closePrompt.open = false;
  closePrompt.scope = "tab";
  closePrompt.tabId = null;
  closePrompt.reason = "cells";
  resolve?.(false);
}
