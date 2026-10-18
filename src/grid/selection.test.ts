import { describe, expect, it } from "vitest";
import {
  applyMove,
  applyPointer,
  cellKey,
  emptySelection,
  isCellSelected,
  normalizeRect,
  parseExtras,
  selectAllRows,
  selectionToTsv,
} from "./selection";

describe("grid selection model", () => {
  it("plain click selects and anchors a single cell", () => {
    const next = applyPointer(emptySelection, { rowIndex: 2, colIndex: 1 }, { shift: false, ctrl: false });
    expect(next).toEqual({ anchor: { rowIndex: 2, colIndex: 1 }, focus: { rowIndex: 2, colIndex: 1 }, extras: "", allRows: false });
  });

  it("shift click extends a rectangular range from the anchor", () => {
    let selection = applyPointer(emptySelection, { rowIndex: 1, colIndex: 1 }, { shift: false, ctrl: false });
    selection = applyPointer(selection, { rowIndex: 3, colIndex: 2 }, { shift: true, ctrl: false });
    expect(selection.allRows).toBe(false);
    expect(selection.anchor).toEqual({ rowIndex: 1, colIndex: 1 });
    expect(selection.focus).toEqual({ rowIndex: 3, colIndex: 2 });
    expect(isCellSelected(selection, { rowIndex: 2, colIndex: 2 }, 10, 4)).toBe(true);
    expect(isCellSelected(selection, { rowIndex: 0, colIndex: 0 }, 10, 4)).toBe(false);
  });

  it("ctrl click toggles extra cells while keeping the existing anchor", () => {
    let selection = applyPointer(emptySelection, { rowIndex: 0, colIndex: 0 }, { shift: false, ctrl: false });
    selection = applyPointer(selection, { rowIndex: 5, colIndex: 2 }, { shift: false, ctrl: true });
    expect(selection.anchor).toEqual({ rowIndex: 0, colIndex: 0 });
    expect(selection.focus).toEqual({ rowIndex: 0, colIndex: 0 });
    expect(parseExtras(selection.extras)).toEqual([{ rowIndex: 5, colIndex: 2 }]);
    expect(cellKey({ rowIndex: 5, colIndex: 2 })).toBe("5:2");
    expect(isCellSelected(selection, { rowIndex: 0, colIndex: 0 }, 10, 4)).toBe(true);
    expect(isCellSelected(selection, { rowIndex: 5, colIndex: 2 }, 10, 4)).toBe(true);
    selection = applyPointer(selection, { rowIndex: 5, colIndex: 2 }, { shift: false, ctrl: true });
    expect(selection.extras).toBe("");
    expect(isCellSelected(selection, { rowIndex: 0, colIndex: 0 }, 10, 4)).toBe(true);
    expect(isCellSelected(selection, { rowIndex: 5, colIndex: 2 }, 10, 4)).toBe(false);
  });

  it("ctrl-clicking the same cell twice on an empty selection deselects it", () => {
    let selection = applyPointer(emptySelection, { rowIndex: 4, colIndex: 3 }, { shift: false, ctrl: true });
    expect(selection.anchor).toBeNull();
    expect(selection.focus).toBeNull();
    expect(isCellSelected(selection, { rowIndex: 4, colIndex: 3 }, 10, 4)).toBe(true);
    selection = applyPointer(selection, { rowIndex: 4, colIndex: 3 }, { shift: false, ctrl: true });
    expect(selection).toEqual({ anchor: null, focus: null, extras: "", allRows: false });
    expect(isCellSelected(selection, { rowIndex: 4, colIndex: 3 }, 10, 4)).toBe(false);
  });

  it("selectAllRows marks every loaded cell and serializes full TSV with headers", () => {
    const selection = selectAllRows(3, 2);
    expect(selection.allRows).toBe(true);
    expect(selection.anchor).toEqual({ rowIndex: 0, colIndex: 0 });
    expect(selection.focus).toEqual({ rowIndex: 2, colIndex: 1 });
    const tsv = selectionToTsv(selection, {
      headers: true,
      columnNames: ["a", "b"],
      cellAt: (row, col) => `${row}${col}`,
    });
    expect(tsv).toBe("a\tb\n00\t01\n10\t11\n20\t21");
  });

  it("applyMove moves the focus, following the anchor unless extending", () => {
    const base = { anchor: { rowIndex: 1, colIndex: 1 }, focus: { rowIndex: 1, colIndex: 1 }, extras: "", allRows: false };
    const moved = applyMove(base, "ArrowRight", false, 10, 4);
    expect(moved).toEqual({ anchor: { rowIndex: 1, colIndex: 2 }, focus: { rowIndex: 1, colIndex: 2 }, extras: "", allRows: false });
    const extended = applyMove(base, "ArrowDown", true, 10, 4);
    expect(extended).toEqual({ anchor: { rowIndex: 1, colIndex: 1 }, focus: { rowIndex: 2, colIndex: 1 }, extras: "", allRows: false });
  });

  it("clamps move focus into the grid bounds", () => {
    const base = { anchor: { rowIndex: 0, colIndex: 1 }, focus: { rowIndex: 0, colIndex: 1 }, extras: "", allRows: false };
    expect(applyMove(base, "ArrowUp", false, 10, 4).focus).toEqual({ rowIndex: 0, colIndex: 1 });
    expect(applyMove(base, "ArrowRight", false, 10, 4).focus).toEqual({ rowIndex: 0, colIndex: 2 });
    const rightEdge = { anchor: { rowIndex: 0, colIndex: 3 }, focus: { rowIndex: 0, colIndex: 3 }, extras: "", allRows: false };
    expect(applyMove(rightEdge, "ArrowRight", false, 10, 4).focus).toEqual({ rowIndex: 0, colIndex: 3 });
    expect(applyMove(base, "ArrowDown", false, 0, 0).focus).toEqual({ rowIndex: 0, colIndex: 0 });
  });

  it("serializes selected cells to TSV, replacing null with NULL and sanitizing separators", () => {
    const selection = applyPointer(applyPointer(emptySelection, { rowIndex: 0, colIndex: 0 }, { shift: false, ctrl: false }), { rowIndex: 0, colIndex: 1 }, { shift: true, ctrl: false });
    const tsv = selectionToTsv(selection, {
      headers: false,
      cellAt: (row, col) => (col === 0 ? "a\tb" : null),
    });
    expect(tsv).toBe("a b\tNULL");
  });

  it("skips rows with no selected cell and returns empty string when nothing is selected", () => {
    const selection = applyPointer(emptySelection, { rowIndex: 1, colIndex: 0 }, { shift: false, ctrl: false });
    const tsv = selectionToTsv(selection, {
      headers: true,
      columnNames: ["a", "b"],
      cellAt: (row, col) => `${row}${col}`,
    });
    expect(tsv).toBe("a\n10");
    expect(selectionToTsv(emptySelection, {
      headers: true,
      columnNames: ["a"],
      cellAt: () => "x",
    })).toBe("");
  });

  it("keeps extras sorted and rejects out-of-bounds cells under select all", () => {
    let selection = applyPointer(emptySelection, { rowIndex: 5, colIndex: 2 }, { shift: false, ctrl: false });
    selection = applyPointer(selection, { rowIndex: 0, colIndex: 0 }, { shift: false, ctrl: true });
    selection = applyPointer(selection, { rowIndex: 2, colIndex: 1 }, { shift: false, ctrl: true });
    expect(selection.extras).toBe("0:0,2:1");
    const all = selectAllRows(3, 2);
    expect(isCellSelected(all, { rowIndex: 0, colIndex: 0 }, 3, 2)).toBe(true);
    expect(isCellSelected(all, { rowIndex: 2, colIndex: 1 }, 3, 2)).toBe(true);
    expect(isCellSelected(all, { rowIndex: 3, colIndex: 0 }, 3, 2)).toBe(false);
    expect(isCellSelected(all, { rowIndex: 0, colIndex: 2 }, 3, 2)).toBe(false);
  });

  it("selectAllRows on an empty result produces no phantom rows in TSV", () => {
    const all = selectAllRows(0, 2);
    expect(all).toEqual({ anchor: null, focus: null, extras: "", allRows: true });
    expect(selectionToTsv(all, {
      headers: true,
      columnNames: ["a", "b"],
      cellAt: () => "x",
    })).toBe("");
  });

  it("sanitizes header names too", () => {
    const selection = selectAllRows(1, 2);
    const tsv = selectionToTsv(selection, {
      headers: true,
      columnNames: ["a\tb", "c\nd"],
      cellAt: () => "",
    });
    expect(tsv).toBe("a b\tc d\n\t");
  });

  it("normalizes a reversed rectangle and builds stable cell keys", () => {
    expect(normalizeRect({ rowIndex: 3, colIndex: 2 }, { rowIndex: 1, colIndex: 0 })).toEqual({ r0: 1, r1: 3, c0: 0, c1: 2 });
    expect(cellKey({ rowIndex: 4, colIndex: 7 })).toBe("4:7");
  });
});
