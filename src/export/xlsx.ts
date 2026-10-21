import * as XLSX from "@e965/xlsx";
import type { Cell } from "../grid/types";
import type { ImportParse } from "./import";

const NON_TABLE = new Set(["chart", "dialog", "macro"]);

function asBytes(out: unknown): Uint8Array {
  if (out instanceof Uint8Array) return out;
  if (out instanceof ArrayBuffer) return new Uint8Array(out);
  if (Array.isArray(out)) return Uint8Array.from(out as number[]);
  throw new Error("无法生成 xlsx");
}

/** 空单元格不写记号。空字符串也留空，读回时与 NULL 相同。 */
export function renderXlsx(columns: readonly string[], rows: readonly Record<string, string | null>[]): Uint8Array {
  const sheet: XLSX.WorkSheet = {};
  columns.forEach((name, column) => {
    sheet[XLSX.utils.encode_cell({ r: 0, c: column })] = { t: "s", v: name };
  });
  rows.forEach((row, rowIndex) => {
    columns.forEach((name, column) => {
      const value = row[name];
      if (value === null || value === undefined || value === "") return;
      sheet[XLSX.utils.encode_cell({ r: rowIndex + 1, c: column })] = { t: "s", v: value };
    });
  });
  if (columns.length > 0) {
    sheet["!ref"] = XLSX.utils.encode_range({
      s: { r: 0, c: 0 },
      e: { r: Math.max(rows.length, 0), c: columns.length - 1 },
    });
  }
  const book = XLSX.utils.book_new();
  XLSX.utils.book_append_sheet(book, sheet, "Sheet1");
  return asBytes(XLSX.write(book, { type: "array", bookType: "xlsx" }));
}

export function renderXlsxPreview(columns: readonly string[], rows: readonly Record<string, string | null>[]): string {
  const lines = [columns.join("\t")];
  for (const row of rows) {
    lines.push(columns.map((name) => {
      const value = row[name];
      if (value === null || value === undefined || value === "") return "";
      return value.replaceAll("\t", " ").replaceAll("\n", " ");
    }).join("\t"));
  }
  return lines.join("\r\n");
}

function cellText(value: unknown): string | null {
  if (value === null || value === undefined) return null;
  if (typeof value === "string") return value === "" ? null : value;
  if (typeof value === "number" && Number.isFinite(value)) return String(value);
  if (typeof value === "boolean") return value ? "true" : "false";
  return null;
}

function sheetKind(sheet: XLSX.WorkSheet): string | undefined {
  return (sheet as { "!type"?: string })["!type"];
}

function hasCells(sheet: XLSX.WorkSheet): boolean {
  return Object.keys(sheet).some((key) => !key.startsWith("!"));
}

function zipMagic(bytes: Uint8Array): boolean {
  return bytes.length >= 4
    && bytes[0] === 0x50
    && bytes[1] === 0x4b
    && (bytes[2] === 0x03 || bytes[2] === 0x05 || bytes[2] === 0x07)
    && (bytes[3] === 0x04 || bytes[3] === 0x06 || bytes[3] === 0x08);
}

/** 只读第一个工作表。第一行是列名。空单元格是 NULL，不识别额外的空值记号。 */
export function parseImportXlsx(bytes: Uint8Array): ImportParse {
  if (bytes.byteLength === 0) return { ok: false, reason: "xlsx 文件是空的。" };
  if (!zipMagic(bytes)) return { ok: false, reason: "第一个工作表不是表。" };
  let book: XLSX.WorkBook;
  try {
    book = XLSX.read(bytes, { type: "array", cellDates: false });
  } catch {
    return { ok: false, reason: "第一个工作表不是表。" };
  }
  const sheetName = book.SheetNames[0];
  if (!sheetName) return { ok: false, reason: "xlsx 文件是空的。" };
  const sheet = book.Sheets[sheetName];
  if (!sheet) return { ok: false, reason: "xlsx 文件是空的。" };
  const kind = sheetKind(sheet);
  if (kind && NON_TABLE.has(kind)) return { ok: false, reason: "第一个工作表不是表。" };
  if (!hasCells(sheet) || !sheet["!ref"]) return { ok: false, reason: "xlsx 文件是空的。" };
  const range = XLSX.utils.decode_range(sheet["!ref"]);
  if (range.s.r !== 0) return { ok: false, reason: "xlsx 缺少列名。" };
  const matrix = XLSX.utils.sheet_to_json<unknown[]>(sheet, { header: 1, raw: false, defval: null, blankrows: true });
  const header = matrix[0];
  if (!header?.length) return { ok: false, reason: "xlsx 缺少列名。" };
  const columns: string[] = [];
  for (const cell of header) {
    const name = cellText(cell)?.trim() ?? "";
    if (!name) return { ok: false, reason: "xlsx 缺少列名。" };
    if (columns.includes(name)) return { ok: false, reason: `xlsx 列名重复：${name}` };
    columns.push(name);
  }
  const rows = matrix.slice(1).filter((row) => row.some((cell) => cellText(cell) !== null)).map((row) => {
    const cells: Record<string, Cell> = {};
    columns.forEach((name, index) => {
      cells[name] = cellText(row[index]) ?? null;
    });
    return cells;
  });
  return { ok: true, data: { columns, rows } };
}
