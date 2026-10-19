export interface CellPoint {
  row: string;
  column: string;
}

/** 只保留矩形里两端之间、行和列都真实存在的格子。 */
export function rectangleSelection(rows: readonly { id: string }[], columns: readonly { name: string }[], anchor: CellPoint, focus: CellPoint): CellPoint[] {
  const startRow = rows.findIndex((row) => row.id === anchor.row);
  const endRow = rows.findIndex((row) => row.id === focus.row);
  const startColumn = columns.findIndex((column) => column.name === anchor.column);
  const endColumn = columns.findIndex((column) => column.name === focus.column);
  if (startRow < 0 || endRow < 0 || startColumn < 0 || endColumn < 0) return [];
  const picked: CellPoint[] = [];
  for (let rowIndex = Math.min(startRow, endRow); rowIndex <= Math.max(startRow, endRow); rowIndex += 1) {
    const row = rows[rowIndex];
    if (!row) continue;
    for (let columnIndex = Math.min(startColumn, endColumn); columnIndex <= Math.max(startColumn, endColumn); columnIndex += 1) {
      const column = columns[columnIndex];
      if (!column) continue;
      picked.push({ row: row.id, column: column.name });
    }
  }
  return picked;
}

export function quoteTsvField(value: string): string {
  if (!/[\t\r\n"]/.test(value)) return value;
  return `"${value.replaceAll('"', '""')}"`;
}

export function toTsv(rows: readonly (readonly string[])[]): string {
  return rows.map((row) => row.map((field) => quoteTsvField(field)).join("\t")).join("\n");
}

export function selectionTsv(points: readonly CellPoint[], textAt: (point: CellPoint) => string): string {
  const lines: string[][] = [];
  let current = "";
  let line: string[] = [];
  for (const point of points) {
    if (line.length && point.row !== current) {
      lines.push(line);
      line = [];
    }
    current = point.row;
    line.push(textAt(point));
  }
  if (line.length) lines.push(line);
  return toTsv(lines);
}
