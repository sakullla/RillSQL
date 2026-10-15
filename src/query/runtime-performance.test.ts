import { afterEach, describe, expect, it, vi } from "vitest";
import { isReactive } from "vue";
import type { RunDto } from "./api";
import { adoptRun, guardClose, resetQueryRuntime, startTabRun, tabRun } from "./runtime";
const api = vi.hoisted(() => ({ queryStart: vi.fn(), queryPoll: vi.fn(), queryCancel: vi.fn() }));
vi.mock("./api", () => api);
const dto = (status = "running"): RunDto => ({ id: "run", connectionId: "db", connectionName: "DB", database: "main", engine: "sqlite", status, cancel: "none", cancelMessage: "", viaSsh: false, freshChannel: false, statements: [], elapsedMs: 0, startedAt: "" });
afterEach(() => { resetQueryRuntime(); vi.useRealTimers(); vi.clearAllMocks(); });
it("does not proxy nested result rows and releases completed results when their tab closes", () => {
  const result = dto("done");
  adoptRun("q", result);
  expect(isReactive(tabRun("q"))).toBe(true);
  expect(isReactive(tabRun("q")!.statements)).toBe(false);
  expect(guardClose("q")).toBe(true);
  expect(tabRun("q")).toBeUndefined();
});
describe("slow query status requests", () => {
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
});
