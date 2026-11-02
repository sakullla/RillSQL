import { describe, expect, it, vi } from "vitest";
import { effectScope, isReactive } from "vue";
import { refreshLocated, viewLoadedRows } from "./data";
import { customersFixture } from "./fixture";
import { useGridSettings } from "./settings";
import { DEFAULT_SETTINGS, saveSettings } from "../shell/settings";
import type { Row } from "./types";

describe("large result resource usage", () => {
  it("shares row data during filtering and sorting without changing the source order", () => {
    const { schema, rows } = customersFixture(1000);
    const original = rows.map((row) => row.id);
    const viewed = viewLoadedRows(rows, schema.columns, "", { column: "id", direction: "desc" });
    expect(viewed.rows[0]).toBe(rows[rows.length - 1]);
    expect(viewed.rows[0]!.cells).toBe(rows[rows.length - 1]!.cells);
    expect(rows.map((row) => row.id)).toEqual(original);
    const found = viewLoadedRows(rows, schema.columns, rows[50]!.cells.email!, null);
    expect(found.rows[0]).toBe(rows[50]);
  });

  it("locates 10,000 rows with one read of each key and rejects duplicate composite keys", () => {
    const { schema } = customersFixture(0);
    schema.key = ["id", "name"];
    let keyReads = 0;
    const rows: Row[] = Array.from({ length: 10_000 }, (_, index) => ({
      id: `r${index}`, cells: { get id() { keyReads += 1; return String(index); }, name: "a" }, locator: null, located: false,
    }));
    const timings: number[] = [];
    for (let run = 0; run < 3; run += 1) {
      const started = performance.now();
      refreshLocated(schema, rows);
      timings.push(performance.now() - started);
    }
    expect(keyReads).toBe(30_000);
    expect(timings.sort((a, b) => a - b)[1]).toBeLessThan(100);
    expect(rows.every((row) => row.located)).toBe(true);
    const pairs = [["a,b", "c"], ["a", "b,c"], ["same", "key"], ["same", "key"], [null, "key"]];
    const duplicateRows = pairs.map(([id, name], index) => ({ id: String(index), cells: { id: id!, name: name! }, locator: null, located: false }));
    refreshLocated(schema, duplicateRows);
    expect(duplicateRows.map((row) => row.located)).toEqual([true, true, false, false, false]);
  });

  it("reads settings once per grid, updates on save, and detaches on disposal", () => {
    const read = vi.spyOn(Storage.prototype, "getItem");
    const scope = effectScope();
    try {
      const settings = scope.run(useGridSettings)!;
      const reads = read.mock.calls.length;
      for (let index = 0; index < 1000; index += 1) void settings.value.previewKB;
      expect(read.mock.calls.length).toBe(reads);
      expect(isReactive(settings.value)).toBe(false);
      saveSettings({ ...DEFAULT_SETTINGS, previewKB: 5 }, null);
      expect(settings.value.previewKB).toBe(5);
      scope.stop();
      saveSettings({ ...DEFAULT_SETTINGS, previewKB: 8 }, null);
      expect(settings.value.previewKB).toBe(5);
    } finally {
      scope.stop();
      read.mockRestore();
    }
  });
});
