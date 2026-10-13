export type CsvCell = string | null;
export type CsvDelimiter = "," | ";" | "\t";
export type NullToken = "" | "\\N" | "NULL";

export interface CsvOptions {
  delimiter: CsvDelimiter;
  nullToken: NullToken;
  bom: boolean;
  header: boolean;
}

export const DEFAULT_CSV: CsvOptions = {
  delimiter: ",",
  nullToken: "",
  bom: true,
  header: true,
};

export function encodeField(value: CsvCell, nullToken: NullToken): string {
  if (value === null) return nullToken;
  return `"${value.replaceAll('"', '""')}"`;
}

export function renderCsv(columns: readonly string[], rows: readonly Record<string, CsvCell>[], options: CsvOptions): string {
  const lines: string[] = [];
  if (options.header) lines.push(columns.map((name) => encodeField(name, options.nullToken)).join(options.delimiter));
  for (const row of rows) {
    lines.push(columns.map((name) => encodeField(row[name] ?? null, options.nullToken)).join(options.delimiter));
  }
  const body = lines.join("\r\n");
  return options.bom ? `\uFEFF${body}` : body;
}

export interface ParsedField {
  value: string | null;
  quoted: boolean;
}

/** 空字段且无引号是 NULL；"" 是空字符串。 */
export function parseCsvDistinct(text: string, delimiter: CsvDelimiter = ","): ParsedField[][] {
  const source = text.charCodeAt(0) === 0xfeff ? text.slice(1) : text;
  const rows: ParsedField[][] = [];
  let row: ParsedField[] = [];
  let field = "";
  let quoted = false;
  let sawQuote = false;
  const pushField = () => {
    row.push({ value: !sawQuote && field === "" ? null : field, quoted: sawQuote });
    field = "";
    sawQuote = false;
  };
  for (let index = 0; index < source.length; index += 1) {
    const char = source[index]!;
    if (quoted) {
      if (char === '"') {
        if (source[index + 1] === '"') {
          field += '"';
          index += 1;
        } else quoted = false;
      } else field += char;
      continue;
    }
    if (char === '"') {
      quoted = true;
      sawQuote = true;
      continue;
    }
    if (char === delimiter) {
      pushField();
      continue;
    }
    if (char === "\n") {
      if (field.endsWith("\r")) field = field.slice(0, -1);
      pushField();
      rows.push(row);
      row = [];
      continue;
    }
    field += char;
  }
  if (field.length > 0 || sawQuote || row.length > 0) {
    if (field.endsWith("\r")) field = field.slice(0, -1);
    pushField();
    rows.push(row);
  }
  return rows;
}
