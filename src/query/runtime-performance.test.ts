import { afterEach, describe, expect, it, vi } from "vitest";
import { isReactive } from "vue";
import type { RunDto } from "./api";
import { adoptRun, continueTabRun, guardClose, requestCancel, resetQueryRuntime, startTabRun, tabRun, whenSettled } from "./runtime";
const api = vi.hoisted(() => ({ queryStart: vi.fn(), queryPoll: vi.fn(), queryCancel: vi.fn(), queryRelease: vi.fn().mockResolvedValue(true) }));
vi.mock("./api", () => api);
const dto = (status = "running"): RunDto => ({ id: "run", connectionId: "db", connectionName: "DB", database: "main", engine: "sqlite", status, cancel: "none", cancelMessage: "", viaSsh: false, freshChannel: false, statements: [], elapsedMs: 0, startedAt: "" });
afterEach(() => { resetQueryRuntime(); vi.useRealTimers(); vi.clearAllMocks(); });
it("does not proxy nested result rows and releases completed results when their tab closes", () => {
  const result = dto("done");
  adoptRun("q", result);
  expect(isReactive(tabRun("q"))).toBe(true);
  expect(isReactive(tabRun("q")!.statements)).toBe(false);
  expect(api.queryRelease).toHaveBeenCalledWith("run");
  expect(guardClose("q")).toBe(true);
  expect(tabRun("q")).toBeUndefined();
});
describe("slow query status requests", () => {
  it("resolves run waiters without timers and releases them on replacement or reset", async () => {
    vi.useFakeTimers();
    adoptRun("q", dto());
    const done = vi.fn();
    const first = whenSettled("q").then(done);
    expect(vi.getTimerCount()).toBe(0);
    adoptRun("q", { ...dto(), id: "replacement" });
    await first;
    expect(done).toHaveBeenCalledTimes(1);
    const second = whenSettled("q").then(done);
    resetQueryRuntime();
    await second;
    expect(done).toHaveBeenCalledTimes(2);
    expect(vi.getTimerCount()).toBe(0);
  });
  it("releases final snapshots once and ignores older concurrent responses", async () => {
    vi.useFakeTimers();
    let resolvePoll!: (value: RunDto) => void;
    api.queryStart.mockResolvedValue(dto());
    api.queryPoll.mockImplementation(() => new Promise<RunDto>((resolve) => { resolvePoll = resolve; }));
    api.queryCancel.mockResolvedValue({ ...dto("cancelled"), cancel: "confirmed" });
    await startTabRun({ tabId: "q", connectionId: "db", connectionName: "DB", database: "main", engine: "sqlite", sql: "SELECT 1", limit: 100 });
    expect(api.queryRelease).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(400);
    await requestCancel("q");
    expect(api.queryRelease).toHaveBeenCalledExactlyOnceWith("run");
    resolvePoll(dto());
    await vi.advanceTimersByTimeAsync(1000);
    expect(tabRun("q")?.status).toBe("cancelled");
    expect(api.queryRelease).toHaveBeenCalledTimes(1);
    expect(api.queryPoll).toHaveBeenCalledTimes(1);
  });

  it("does not let an old cancellation overwrite a replacement run", async () => {
    let resolveCancel!: (value: RunDto) => void;
    api.queryCancel.mockImplementation(() => new Promise<RunDto>((resolve) => { resolveCancel = resolve; }));
    adoptRun("q", dto());
    const cancelled = requestCancel("q");
    adoptRun("q", { ...dto("done"), id: "new" });
    resolveCancel({ ...dto("cancelled"), cancel: "confirmed" });
    await cancelled;
    expect(tabRun("q")?.id).toBe("new");
  });
  it("does not poll a result that already completed during start", async () => {
    vi.useFakeTimers();
    api.queryStart.mockResolvedValue(dto("done"));
    await startTabRun({ tabId: "q", connectionId: "db", connectionName: "DB", database: "main", engine: "sqlite", sql: "SELECT 1", limit: 100 });
    await vi.advanceTimersByTimeAsync(2000);
    expect(api.queryPoll).not.toHaveBeenCalled();
  });
  it("keeps at most one poll in flight and ignores a response for a replaced run", async () => {
    vi.useFakeTimers();
    let resolvePoll!: (run: RunDto) => void;
    api.queryStart.mockResolvedValue(dto());
    api.queryPoll.mockImplementation(() => new Promise<RunDto>(resolve => { resolvePoll = resolve; }));
    await startTabRun({ tabId: "q", connectionId: "db", connectionName: "DB", database: "main", engine: "sqlite", sql: "SELECT 1", limit: 100 });
    await vi.advanceTimersByTimeAsync(2000);
    expect(api.queryPoll).toHaveBeenCalledTimes(1);
    adoptRun("q", { ...dto("done"), id: "replacement" });
    resolvePoll(dto("done"));
    await Promise.resolve();
    expect(tabRun("q")?.id).toBe("replacement");
  });
  it("continues the same run through query_poll and does not start another", async () => {
    api.queryStart.mockResolvedValue(dto());
    api.queryPoll.mockResolvedValue(dto());
    await startTabRun({ tabId: "q", connectionId: "db", connectionName: "DB", database: "main", engine: "sqlite", sql: "SELECT 1", limit: 100 });
    await continueTabRun("q");
    expect(api.queryStart).toHaveBeenCalledTimes(1);
    expect(api.queryPoll).toHaveBeenCalledWith("run", true);
    await continueTabRun("missing");
    expect(api.queryStart).toHaveBeenCalledTimes(1);
  });
});

describe("completion latency", () => {
  it("keeps statement scanning and scope parsing within one frame on a 10k-line script", async () => {
    const { locateForCompletion } = await import("./completion/scan");
    const { parseScope } = await import("./completion/scope");
    const statement = "SELECT o.id, c.name FROM orders o JOIN customers c ON c.id = o.customer_id WHERE o.status = 'paid'";
    const doc = Array.from({ length: 10_000 }, () => statement + ";").join("\n");
    const at = doc.length - 5;
    const runs: number[] = [];
    for (let index = 0; index < 5; index += 1) {
      const started = performance.now();
      const located = locateForCompletion(doc, at);
      parseScope(doc.slice(located.start, at));
      runs.push(performance.now() - started);
    }
    runs.sort((a, b) => a - b);
    expect(runs[Math.floor(runs.length / 2)]).toBeLessThanOrEqual(16);
  });
});
