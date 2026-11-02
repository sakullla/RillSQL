/** 文本预览无需加载 Excel 编解码库。 */
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
