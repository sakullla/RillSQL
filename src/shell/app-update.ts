import { readonly, shallowReactive } from "vue";
import type { DownloadEvent } from "@tauri-apps/plugin-updater";
import { desktopShell } from "./window-chrome";

export interface UpdateHandle {
  version: string;
  body?: string;
  download: (onEvent: (event: DownloadEvent) => void, options: { timeout: number }) => Promise<void>;
  install: () => Promise<void>;
  close: () => Promise<void>;
}

export type UpdateStage = "idle" | "checking" | "latest" | "available" | "downloading" | "installing" | "installed" | "restarting" | "error";

export function createAppUpdater(backend: {
  available: () => boolean;
  check: () => Promise<UpdateHandle | null>;
  relaunch: () => Promise<void>;
}) {
  const state = shallowReactive({
    stage: "idle" as UpdateStage, version: "", notes: "", error: "",
    downloaded: 0, total: null as number | null, checkedAt: null as number | null,
  });
  let handle: UpdateHandle | null = null;
  let operation: Promise<void> | null = null;
  let generation = 0;
  const errorText = (error: unknown) => error instanceof Error ? error.message : String(error);
  const close = async (update: UpdateHandle | null) => { await update?.close().catch(() => undefined); };
  async function release() {
    const previous = handle;
    handle = null;
    await close(previous);
  }
  function once(run: () => Promise<void>): Promise<void> {
    if (operation) return operation;
    const pending = run().finally(() => { if (operation === pending) operation = null; });
    operation = pending;
    return pending;
  }
  function check(background = false): Promise<void> {
    if (!backend.available() || state.stage === "installed") return Promise.resolve();
    return once(async () => {
      const token = generation;
      state.stage = "checking";
      state.error = "";
      await release();
      try {
        const update = await backend.check();
        if (token !== generation) { await close(update); return; }
        handle = update;
        state.version = update?.version ?? "";
        state.notes = update?.body ?? "";
        state.checkedAt = Date.now();
        state.stage = update ? "available" : "latest";
      } catch (error) {
        if (token !== generation) return;
        state.stage = background ? "idle" : "error";
        state.error = background ? "" : errorText(error);
      }
    });
  }
  function install(prepare: () => void): Promise<void> {
    if (!backend.available() || !handle || state.stage !== "available") return Promise.resolve();
    return once(async () => {
      const token = generation;
      const update = handle!;
      state.error = "";
      try {
        prepare();
        state.stage = "downloading";
        state.downloaded = 0;
        state.total = null;
        await update.download((event) => {
          if (token !== generation) return;
          if (event.event === "Started") {
            state.downloaded = 0;
            state.total = event.data.contentLength && event.data.contentLength > 0 ? event.data.contentLength : null;
          } else if (event.event === "Progress") state.downloaded += event.data.chunkLength;
        }, { timeout: 300_000 });
        if (token !== generation) return;
        // Downloads can take minutes. Recheck work and persist the newest drafts just before Windows exits.
        prepare();
        state.stage = "installing";
        await update.install();
        if (token !== generation) return;
        state.stage = "installed";
      } catch (error) {
        if (token === generation) { state.error = errorText(error); state.stage = "error"; }
      } finally {
        if (handle === update) await release();
        else await close(update);
      }
      if (token === generation && state.stage === "installed") await restart(prepare);
    });
  }
  async function restart(prepare: () => void): Promise<void> {
    if (state.stage !== "installed") return;
    const token = generation;
    try {
      prepare();
      state.error = "";
      state.stage = "restarting";
      await backend.relaunch();
    } catch (error) {
      if (token === generation) { state.error = errorText(error); state.stage = "installed"; }
    }
  }
  function dispose() {
    generation += 1;
    // An in-flight operation owns its resource until its completion; then its finally closes it.
    if (!operation) void release();
  }
  return { state: readonly(state), available: backend.available, check, install, restart, dispose };
}

export const appUpdater = createAppUpdater({
  available: desktopShell,
  check: async () => (await import("@tauri-apps/plugin-updater")).check({ timeout: 15_000 }),
  relaunch: async () => (await import("@tauri-apps/plugin-process")).relaunch(),
});
