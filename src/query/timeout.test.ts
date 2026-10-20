import { afterEach, describe, expect, it, vi } from "vitest";
import type { RunDto } from "./api";
import { resetQueryRuntime, startTabRun } from "./runtime";

const api = vi.hoisted(() => ({ queryStart: vi.fn(), queryPoll: vi.fn(), queryCancel: vi.fn() }));
vi.mock("./api", () => api);

const dto = (): RunDto => ({ id: "run", connectionId: "db", connectionName: "DB", database: "main", engine: "postgres", status: "done", cancel: "none", cancelMessage: "", viaSsh: false, freshChannel: false, statements: [], elapsedMs: 0, startedAt: "" });

function runInput() {
  return { tabId: "q", connectionId: "db", connectionName: "DB", database: "main", engine: "postgres", sql: "SELECT 1", limit: 100 };
}

afterEach(() => {
  resetQueryRuntime();
  localStorage.clear();
  vi.clearAllMocks();
});

describe("statement timeout", () => {
  it("omits timeoutMs entirely when the setting is off", async () => {
    localStorage.setItem("rillsql.shell", JSON.stringify({ queryTimeoutSec: 0 }));
    api.queryStart.mockResolvedValue(dto());
    await startTabRun(runInput());
    const request = api.queryStart.mock.calls[0]![0];
    expect(request).toEqual({ connectionId: "db", connectionName: "DB", database: "main", engine: "postgres", sql: "SELECT 1", limit: 100 });
    expect("timeoutMs" in request).toBe(false);
  });

  it("omits timeoutMs when no stored settings exist", async () => {
    api.queryStart.mockResolvedValue(dto());
    await startTabRun(runInput());
    expect("timeoutMs" in api.queryStart.mock.calls[0]![0]).toBe(false);
  });

  it("injects the configured timeout converted to milliseconds", async () => {
    localStorage.setItem("rillsql.shell", JSON.stringify({ queryTimeoutSec: 30 }));
    api.queryStart.mockResolvedValue(dto());
    await startTabRun(runInput());
    expect(api.queryStart.mock.calls[0]![0]).toEqual({ connectionId: "db", connectionName: "DB", database: "main", engine: "postgres", sql: "SELECT 1", limit: 100, timeoutMs: 30000 });
  });
});
