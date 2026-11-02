import { describe, expect, it, vi } from "vitest";
import { createAppUpdater, type UpdateHandle } from "./app-update";

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}
function fixture() {
  const update: UpdateHandle = {
    version: "0.2.4", body: "Improved queries",
    download: vi.fn(async (event) => {
      event({ event: "Started", data: { contentLength: 100 } });
      event({ event: "Progress", data: { chunkLength: 100 } });
      event({ event: "Finished" });
    }),
    install: vi.fn(async () => {}), close: vi.fn(async () => {}),
  };
  const backend = { available: () => true, check: vi.fn(async (): Promise<UpdateHandle | null> => update), relaunch: vi.fn(async () => {}) };
  return { update, backend, updater: createAppUpdater(backend) };
}

describe("app updater lifecycle", () => {
  it("coalesces concurrent checks and never downloads without an explicit install", async () => {
    const { update, backend, updater } = fixture();
    const waiting = deferred<UpdateHandle | null>();
    backend.check.mockReturnValue(waiting.promise);
    const first = updater.check();
    const second = updater.check();
    expect(second).toBe(first);
    waiting.resolve(update);
    await first;
    expect(backend.check).toHaveBeenCalledTimes(1);
    expect(updater.state.stage).toBe("available");
    expect(update.download).not.toHaveBeenCalled();
    await updater.check();
    expect(update.close).toHaveBeenCalledTimes(1);
    updater.dispose();
    expect(update.close).toHaveBeenCalledTimes(2);
  });

  it("checks work before and after downloading, releases bytes, and restarts after installation", async () => {
    const { updater, update, backend } = fixture();
    const prepare = vi.fn();
    await updater.check();
    const first = updater.install(prepare);
    await Promise.all([first, updater.install(prepare), updater.check()]);
    expect(prepare).toHaveBeenCalledTimes(3); // Before download, install and relaunch.
    expect(update.install).toHaveBeenCalledTimes(1);
    expect(update.close).toHaveBeenCalledTimes(1);
    expect(backend.relaunch).toHaveBeenCalledTimes(1);
    expect(updater.state.downloaded).toBe(100);
    expect(updater.state.stage).toBe("restarting");
  });

  it("blocks installation if work becomes busy during download", async () => {
    const { updater, update, backend } = fixture();
    await updater.check();
    const prepare = vi.fn().mockImplementationOnce(() => {}).mockImplementation(() => { throw new Error("query running"); });
    await updater.install(prepare);
    expect(updater.state.error).toBe("query running");
    expect(update.download).toHaveBeenCalledTimes(1);
    expect(update.install).not.toHaveBeenCalled();
    expect(backend.relaunch).not.toHaveBeenCalled();
    expect(update.close).toHaveBeenCalledTimes(1);
  });

  it("never downloads if checkpointing fails", async () => {
    const { updater, update } = fixture();
    await updater.check();
    await updater.install(() => { throw new Error("storage full"); });
    expect(updater.state.stage).toBe("error");
    expect(update.download).not.toHaveBeenCalled();
    expect(update.install).not.toHaveBeenCalled();
    expect(update.close).toHaveBeenCalledTimes(1);
  });

  it("does not install or restart after signature/download failure", async () => {
    const { updater, update, backend } = fixture();
    vi.mocked(update.download).mockRejectedValue(new Error("invalid signature"));
    await updater.check();
    await updater.install(() => {});
    expect(updater.state.error).toBe("invalid signature");
    expect(update.install).not.toHaveBeenCalled();
    expect(backend.relaunch).not.toHaveBeenCalled();
    expect(update.close).toHaveBeenCalledTimes(1);
  });

  it("keeps installed state when restart fails and allows retry without reinstalling", async () => {
    const { updater, update, backend } = fixture();
    backend.relaunch.mockRejectedValueOnce(new Error("restart unavailable"));
    await updater.check();
    await updater.install(() => {});
    expect(updater.state.stage).toBe("installed");
    await updater.check();
    expect(backend.check).toHaveBeenCalledTimes(1);
    await updater.restart(() => {});
    expect(updater.state.stage).toBe("restarting");
    expect(update.install).toHaveBeenCalledTimes(1);
  });

  it("closes an update returned after disposal without retaining it", async () => {
    const { updater, update, backend } = fixture();
    const waiting = deferred<UpdateHandle | null>();
    backend.check.mockReturnValue(waiting.promise);
    const checking = updater.check();
    updater.dispose();
    waiting.resolve(update);
    await checking;
    expect(update.close).toHaveBeenCalledTimes(1);
    expect(updater.state.version).toBe("");
  });

  it("disposal during download prevents later installation and frees the resource", async () => {
    const { updater, update } = fixture();
    const waiting = deferred<void>();
    vi.mocked(update.download).mockReturnValue(waiting.promise);
    await updater.check();
    const installing = updater.install(() => {});
    updater.dispose();
    waiting.resolve();
    await installing;
    expect(update.install).not.toHaveBeenCalled();
    expect(update.close).toHaveBeenCalledTimes(1);
  });

  it("keeps automatic check errors quiet and reports manual check errors", async () => {
    const { updater, backend } = fixture();
    backend.check.mockRejectedValue(new Error("offline"));
    await updater.check(true);
    expect(updater.state).toMatchObject({ stage: "idle", error: "" });
    await updater.check();
    expect(updater.state).toMatchObject({ stage: "error", error: "offline" });
  });

  it("handles up-to-date and browser-only environments", async () => {
    const { updater, backend } = fixture();
    backend.check.mockResolvedValue(null);
    await updater.check();
    expect(updater.state.stage).toBe("latest");
    const browser = createAppUpdater({ ...backend, available: () => false });
    await browser.check();
    expect(backend.check).toHaveBeenCalledTimes(1);
  });
});
