import type { Cell } from "../grid/types";

/** Markdown 表格。空值写成 nullToken（默认空串），单元格里的 | 转义，换行折叠成空格。 */
export interface MarkdownOptions {
  nullToken?: string;
}

export function markdownCell(cell: Cell | undefined, nullToken: string): string {
  if (cell === null || cell === undefined) return nullToken;
  return cell.replaceAll("|", "\\|").replaceAll("\r", " ").replaceAll("\n", " ");
}

/** 表头两行：列名行和 --- 分隔行。列名与单元格同样转义。 */
export function markdownHeader(columns: readonly string[]): string[] {
  const names = columns.map((name) => markdownCell(name, "")).join(" | ");
  const rule = columns.map(() => "---").join(" | ");
  return [`| ${names} |`, `| ${rule} |`];
}

export function markdownLines(columns: readonly string[], rows: readonly Record<string, Cell>[], options: MarkdownOptions = {}): string[] {
  const nullToken = options.nullToken ?? "";
  return rows.map((row) => `| ${columns.map((name) => markdownCell(row[name], nullToken)).join(" | ")} |`);
}

export function renderMarkdown(columns: readonly string[], rows: readonly Record<string, Cell>[], options: MarkdownOptions = {}): string {
  return [...markdownHeader(columns), ...markdownLines(columns, rows, options)].join("\r\n");
}
