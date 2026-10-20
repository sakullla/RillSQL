import { parseCsvDistinct, type CsvDelimiter } from "./csv";
import type { ChangePayload } from "../grid/api";
import type { Cell, Column, CommitReport } from "../grid/types";

export interface ParsedImport {
  columns: string[];
  rows: Record<string, Cell>[];
}

export type ImportParse = { ok: true; data: ParsedImport } | { ok: false; reason: string };

function blankRow(row: { value: string | null; quoted: boolean }[]): boolean {
  return row.every((field) => field.value === null && !field.quoted);
}

export function parseImportCsv(text: string, delimiter: CsvDelimiter = ","): ImportParse {
  const source = text.trim();
  if (!source) return { ok: false, reason: "文件不是可用的 CSV。" };
  const table = parseCsvDistinct(text, delimiter);
  if (!table.length || !table[0]?.length) return { ok: false, reason: "CSV 缺少列名。" };
  const columns: string[] = [];
  for (const field of table[0]) {
    const name = field.value?.trim() ?? "";
    if (!name) return { ok: false, reason: "CSV 缺少列名。" };
    if (columns.includes(name)) return { ok: false, reason: `CSV 列名重复：${name}` };
    columns.push(name);
  }
  const rows = table.slice(1).filter((row) => !blankRow(row)).map((row) => {
    const cells: Record<string, Cell> = {};
    columns.forEach((name, index) => {
      cells[name] = row[index]?.value ?? null;
    });
    return cells;
  });
  return { ok: true, data: { columns, rows } };
}

export function parseImportJson(text: string): ImportParse {
  const source = text.charCodeAt(0) === 0xfeff ? text.slice(1) : text;
  if (!source.trim()) return { ok: false, reason: "JSON 必须是由对象组成的数组。" };
  let parsed: unknown;
  try {
    parsed = JSON.parse(source);
  } catch {
    return { ok: false, reason: "JSON 必须是由对象组成的数组。" };
  }
  if (!Array.isArray(parsed)) return { ok: false, reason: "JSON 必须是由对象组成的数组。" };
  const columns: string[] = [];
  const rows: Record<string, Cell>[] = [];
  for (const item of parsed) {
    if (!item || typeof item !== "object" || Array.isArray(item)) return { ok: false, reason: "JSON 必须是由对象组成的数组。" };
    const cells: Record<string, Cell> = {};
    for (const [key, value] of Object.entries(item as Record<string, unknown>)) {
      if (!columns.includes(key)) columns.push(key);
      if (value === null) cells[key] = null;
      else if (typeof value === "string" || typeof value === "number" || typeof value === "boolean") cells[key] = String(value);
      else return { ok: false, reason: "JSON 字段只能是字符串、数字、布尔或空值。" };
    }
    rows.push(cells);
  }
  for (const row of rows) {
    for (const name of columns) if (!(name in row)) row[name] = null;
  }
  return { ok: true, data: { columns, rows } };
}

export function suggestMapping(tableColumns: readonly Column[], fileColumns: readonly string[]): Record<string, string> {
  const mapping: Record<string, string> = {};
  for (const column of tableColumns) {
    const exact = fileColumns.find((name) => name === column.name);
    const loose = fileColumns.find((name) => name.toLowerCase() === column.name.toLowerCase());
    mapping[column.name] = exact ?? loose ?? "";
  }
  return mapping;
}

export function requiredGaps(columns: readonly Column[], mapping: Record<string, string>): string[] {
  return columns.filter((column) => !column.nullable && !column.auto && !mapping[column.name]).map((column) => column.name);
}

/** 每一行都是一次插入。值只来自文件，不读取表上尚未提交的修改。 */
export function importChanges(rows: readonly Record<string, Cell>[], mapping: Record<string, string>, columns: readonly Column[]): ChangePayload[] {
  return rows.map((row, index) => ({
    id: `import-${index + 1}`,
    kind: "insert" as const,
    sets: [],
    locator: [],
    values: columns
      .filter((column) => mapping[column.name])
      .map((column) => ({ column: column.name, value: row[mapping[column.name]!] ?? null })),
  }));
}

export function mappedPreview(rows: readonly Record<string, Cell>[], mapping: Record<string, string>, columns: readonly Column[]): Record<string, Cell>[] {
  return rows.map((row) => {
    const cells: Record<string, Cell> = {};
    for (const column of columns) {
      const source = mapping[column.name];
      if (!source) continue;
      cells[column.name] = row[source] ?? null;
    }
    return cells;
  });
}

export function importResultText(report: CommitReport, total: number): { allSucceeded: boolean; text: string } {
  const written = report.applied.length;
  if (!report.failed) return { allSucceeded: written === total, text: `已写入 ${written} 行。` };
  const row = /^import-(\d+)$/.exec(report.failed.id)?.[1] ?? report.failed.id;
  const waiting = report.pendingIds.filter((id) => id !== report.failed?.id).length;
  return {
    allSucceeded: false,
    text: `第 ${row} 行失败：${report.failed.message}。已写入 ${written} 行，还有 ${waiting} 行没有写入。导入不是全部成功。`,
  };
}
