import { describe, expect, it } from "vitest";
import type { RunDto } from "./api";
import { mergeRunStatus, outlineStatements, statementIndexAt } from "./outline";

function runDto(statements: RunDto["statements"]): RunDto {
  return {
    id: "run-1",
    connectionId: "shop",
    connectionName: "Shop",
    database: "shop",
    engine: "postgres",
    status: "done",
    cancel: "none",
    cancelMessage: "",
    viaSsh: false,
    freshChannel: false,
    statements,
    startedAt: "2026-10-20T00:00:00Z",
    elapsedMs: 3,
  };
}

describe("outlineStatements", () => {
  it("lists every statement with line and first-line preview", () => {
    const sql = "-- header\nSELECT id, name\nFROM orders;\n\nUPDATE stock SET n = 1;\nSELECT 'a;b';";
    const list = outlineStatements(sql);
    expect(list.map((item) => item.index)).toEqual([1, 2, 3]);
    expect(list.map((item) => item.line)).toEqual([2, 5, 6]);
    expect(list[0]!.preview).toBe("SELECT id, name");
    expect(list[1]!.preview).toBe("UPDATE stock SET n = 1;");
    expect(list[2]!.preview).toBe("SELECT 'a;b';");
    expect(list[0]!.start).toBe(sql.indexOf("SELECT id"));
    expect(list[0]!.text.startsWith("SELECT id")).toBe(true);
  });

  it("flattens long first lines and marks the truncation", () => {
    const long = `SELECT ${Array.from({ length: 40 }, (_, i) => `column_${i}`).join(", ")} FROM wide_table;`;
    const list = outlineStatements(long);
    expect(list).toHaveLength(1);
    expect(list[0]!.preview.endsWith("…")).toBe(true);
    expect(list[0]!.preview.length).toBeLessThanOrEqual(73);
  });

  it("returns an empty list for blanks and comments only", () => {
    expect(outlineStatements("")).toEqual([]);
    expect(outlineStatements("-- only a comment\n  ")).toEqual([]);
  });
});

describe("mergeRunStatus", () => {
  it("matches statements by text and keeps unmatched rows as none", () => {
    const sql = "SELECT 1;\nSELECT 2;\nSELECT 3;";
    const list = outlineStatements(sql);
    const run = runDto([
      { sql: "SELECT 1;", status: "ok", message: "", columns: ["?column?"], rows: [["1"]], truncated: false, affected: null },
      { sql: "SELECT 3;", status: "error", message: "boom", columns: [], rows: [], truncated: false, affected: null },
    ]);
    const merged = mergeRunStatus(list, { ...run, tabId: "t1", pollError: "" });
    expect(merged.map((item) => item.status)).toEqual(["ok", "none", "error"]);
    expect(merged[0]!.rows).toBe(1);
    expect(merged[2]!.rows).toBe(0);
  });

  it("pairs duplicate statement texts in order", () => {
    const sql = "SELECT 1;\nSELECT 1;";
    const list = outlineStatements(sql);
    const run = runDto([
      { sql: "SELECT 1;", status: "ok", message: "", columns: ["v"], rows: [["a"]], truncated: false, affected: null },
      { sql: "SELECT 1;", status: "error", message: "boom", columns: [], rows: [], truncated: false, affected: null },
    ]);
    const merged = mergeRunStatus(list, { ...run, tabId: "t1", pollError: "" });
    expect(merged.map((item) => item.status)).toEqual(["ok", "error"]);
  });

  it("keeps everything none without a run and maps unknown raw statuses", () => {
    const list = outlineStatements("SELECT 1;");
    expect(mergeRunStatus(list, undefined)[0]!.status).toBe("none");
    const run = runDto([{ sql: "SELECT 1;", status: "mystery", message: "", columns: [], rows: [], truncated: false, affected: 5 }]);
    const merged = mergeRunStatus(list, { ...run, tabId: "t1", pollError: "" });
    expect(merged[0]!.status).toBe("unknown");
    expect(merged[0]!.affected).toBe(5);
  });
});

describe("statementIndexAt", () => {
  it("finds the statement containing an offset, else the nearest earlier one", () => {
    const sql = "SELECT 1;\n\nSELECT 2;\nSELECT 3;";
    const list = outlineStatements(sql);
    const secondStart = list[1]!.start;
    expect(statementIndexAt(list, secondStart + 2)).toBe(1);
    // 语句之间的空白：退回前一条。
    expect(statementIndexAt(list, secondStart - 1)).toBe(0);
    // 文档末尾之后：最后一条。
    expect(statementIndexAt(list, sql.length + 10)).toBe(2);
    expect(statementIndexAt(list, -1)).toBe(0);
    expect(statementIndexAt([], 0)).toBe(-1);
  });
});
