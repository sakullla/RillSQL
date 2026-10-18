export interface CellAddr {
  rowIndex: number;
  colIndex: number;
}

export interface GridSelection {
  anchor: CellAddr | null;
  focus: CellAddr | null;
  /** Sorted, comma-joined "rowIndex:colIndex" of extra ctrl-toggled cells. */
  extras: string;
  allRows: boolean;
}

export interface PointerModifier {
  shift: boolean;
  ctrl: boolean;
}

export type MoveKey = "ArrowUp" | "ArrowDown" | "ArrowLeft" | "ArrowRight";

export interface NormalizedRect {
  r0: number;
  r1: number;
  c0: number;
  c1: number;
}

export const emptySelection: GridSelection = { anchor: null, focus: null, extras: "", allRows: false };

export function cellKey(cell: CellAddr): string {
  return `${cell.rowIndex}:${cell.colIndex}`;
}

export function parseExtras(extras: string): CellAddr[] {
  if (!extras) {
    return [];
  }
  return extras.split(",").map((part) => {
    const [rowIndex, colIndex] = part.split(":");
    return { rowIndex: Number(rowIndex), colIndex: Number(colIndex) };
  });
}

function compareKeys(a: string, b: string): number {
  const [aRow, aCol] = a.split(":").map(Number);
  const [bRow, bCol] = b.split(":").map(Number);
  if (aRow !== bRow) {
    return aRow - bRow;
  }
  return aCol - bCol;
}

function extrasFromSet(keys: Set<string>): string {
  return [...keys].sort(compareKeys).join(",");
}

export function normalizeRect(anchor: CellAddr, focus: CellAddr): NormalizedRect {
  return {
    r0: Math.min(anchor.rowIndex, focus.rowIndex),
    r1: Math.max(anchor.rowIndex, focus.rowIndex),
    c0: Math.min(anchor.colIndex, focus.colIndex),
    c1: Math.max(anchor.colIndex, focus.colIndex),
  };
}

export function isCellSelected(selection: GridSelection, cell: CellAddr, rowCount: number, colCount: number): boolean {
  if (selection.allRows) {
    return cell.rowIndex >= 0 && cell.rowIndex < rowCount && cell.colIndex >= 0 && cell.colIndex < colCount;
  }
  if (parseExtras(selection.extras).some((item) => item.rowIndex === cell.rowIndex && item.colIndex === cell.colIndex)) {
    return true;
  }
  if (selection.anchor && selection.focus) {
    const rect = normalizeRect(selection.anchor, selection.focus);
    return cell.rowIndex >= rect.r0 && cell.rowIndex <= rect.r1 && cell.colIndex >= rect.c0 && cell.colIndex <= rect.c1;
  }
  return false;
}

export function applyPointer(selection: GridSelection, cell: CellAddr, modifiers: PointerModifier): GridSelection {
  if (modifiers.shift) {
    return {
      anchor: selection.anchor ?? { ...cell },
      focus: { ...cell },
      extras: "",
      allRows: false,
    };
  }
  if (modifiers.ctrl) {
    const keys = new Set(parseExtras(selection.extras).map(cellKey));
    const key = cellKey(cell);
    if (keys.has(key)) {
      keys.delete(key);
    } else {
      keys.add(key);
    }
    return {
      anchor: selection.anchor ?? { ...cell },
      focus: selection.focus ?? { ...cell },
      extras: extrasFromSet(keys),
      allRows: false,
    };
  }
  return { anchor: { ...cell }, focus: { ...cell }, extras: "", allRows: false };
}

export function applyMove(selection: GridSelection, key: MoveKey, extend: boolean, rowCount: number, colCount: number): GridSelection {
  const maxRow = Math.max(rowCount - 1, 0);
  const maxCol = Math.max(colCount - 1, 0);
  const current = selection.focus ?? { rowIndex: 0, colIndex: 0 };
  const next: CellAddr = {
    rowIndex: Math.min(Math.max(current.rowIndex + (key === "ArrowDown" ? 1 : key === "ArrowUp" ? -1 : 0), 0), maxRow),
    colIndex: Math.min(Math.max(current.colIndex + (key === "ArrowRight" ? 1 : key === "ArrowLeft" ? -1 : 0), 0), maxCol),
  };
  if (extend) {
    return { anchor: selection.anchor ?? { ...current }, focus: next, extras: "", allRows: false };
  }
  return { anchor: next, focus: next, extras: "", allRows: false };
}

export function selectAllRows(rowCount: number, colCount: number): GridSelection {
  return {
    anchor: { rowIndex: 0, colIndex: 0 },
    focus: { rowIndex: Math.max(rowCount - 1, 0), colIndex: Math.max(colCount - 1, 0) },
    extras: "",
    allRows: true,
  };
}

export interface TsvOptions {
  headers: boolean;
  columnNames?: string[];
  cellAt: (rowIndex: number, colIndex: number) => unknown;
  nullText?: string;
  sanitize?: (value: string) => string;
}

const defaultSanitize = (value: string): string => value.replace(/[\t\r\n]+/g, " ");

function renderCell(value: unknown, nullText: string, sanitize: (value: string) => string): string {
  if (value === null || value === undefined) {
    return nullText;
  }
  return sanitize(String(value));
}

export function selectionToTsv(selection: GridSelection, options: TsvOptions): string {
  const nullText = options.nullText ?? "NULL";
  const sanitize = options.sanitize ?? defaultSanitize;

  if (selection.allRows) {
    const focus = selection.focus ?? { rowIndex: 0, colIndex: 0 };
    return renderRegion(0, focus.rowIndex, 0, focus.colIndex, () => true, options, nullText, sanitize);
  }

  const extraCells = parseExtras(selection.extras);
  if (!selection.anchor && !selection.focus && extraCells.length === 0) {
    return "";
  }

  const rect: NormalizedRect | null = selection.anchor && selection.focus ? normalizeRect(selection.anchor, selection.focus) : null;
  let r0 = rect?.r0 ?? Number.POSITIVE_INFINITY;
  let r1 = rect?.r1 ?? Number.NEGATIVE_INFINITY;
  let c0 = rect?.c0 ?? Number.POSITIVE_INFINITY;
  let c1 = rect?.c1 ?? Number.NEGATIVE_INFINITY;
  for (const cell of extraCells) {
    r0 = Math.min(r0, cell.rowIndex);
    r1 = Math.max(r1, cell.rowIndex);
    c0 = Math.min(c0, cell.colIndex);
    c1 = Math.max(c1, cell.colIndex);
  }

  const isSelected = (rowIndex: number, colIndex: number): boolean => {
    if (rect && rowIndex >= rect.r0 && rowIndex <= rect.r1 && colIndex >= rect.c0 && colIndex <= rect.c1) {
      return true;
    }
    return extraCells.some((item) => item.rowIndex === rowIndex && item.colIndex === colIndex);
  };

  return renderRegion(r0, r1, c0, c1, isSelected, options, nullText, sanitize);
}

function renderRegion(
  r0: number,
  r1: number,
  c0: number,
  c1: number,
  isSelected: (rowIndex: number, colIndex: number) => boolean,
  options: TsvOptions,
  nullText: string,
  sanitize: (value: string) => string,
): string {
  if (!Number.isFinite(r0) || !Number.isFinite(r1) || !Number.isFinite(c0) || !Number.isFinite(c1) || r1 < r0 || c1 < c0) {
    return "";
  }
  const lines: string[] = [];
  if (options.headers) {
    const names: string[] = [];
    for (let col = c0; col <= c1; col += 1) {
      names.push(sanitize(String(options.columnNames?.[col] ?? "")));
    }
    lines.push(names.join("\t"));
  }
  let hasSelection = false;
  for (let row = r0; row <= r1; row += 1) {
    const values: string[] = [];
    let rowHasSelection = false;
    for (let col = c0; col <= c1; col += 1) {
      if (isSelected(row, col)) {
        values.push(renderCell(options.cellAt(row, col), nullText, sanitize));
        rowHasSelection = true;
        hasSelection = true;
      } else {
        values.push("");
      }
    }
    if (rowHasSelection) {
      lines.push(values.join("\t"));
    }
  }
  if (!hasSelection) {
    return "";
  }
  return lines.join("\n");
}
