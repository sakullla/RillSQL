export interface Piece {
  text: string;
  start: number;
  end: number;
}

export function splitSql(sql: string): Piece[] {
  const out: Piece[] = [];
  let start = 0;
  let i = 0;
  let quote: string | null = null;
  let dollar: string | null = null;
  while (i < sql.length) {
    if (dollar !== null) {
      const marker = `$${dollar}$`;
      if (sql.startsWith(marker, i)) {
        i += marker.length;
        dollar = null;
        continue;
      }
      i += 1;
      continue;
    }
    if (quote !== null) {
      if (sql[i] === quote) {
        if (quote === "'" && sql[i + 1] === "'") {
          i += 2;
          continue;
        }
        quote = null;
      }
      i += 1;
      continue;
    }
    if (sql.startsWith("--", i)) {
      i += 2;
      while (i < sql.length && sql[i] !== "\n") i += 1;
      continue;
    }
    if (sql.startsWith("/*", i)) {
      i += 2;
      while (i + 1 < sql.length && !(sql[i] === "*" && sql[i + 1] === "/")) i += 1;
      i = Math.min(i + 2, sql.length);
      continue;
    }
    if (sql[i] === "'" || sql[i] === '"' || sql[i] === "`") {
      quote = sql[i] ?? null;
      i += 1;
      continue;
    }
    if (sql[i] === "$") {
      const tag = dollarTag(sql, i);
      if (tag !== null) {
        dollar = tag;
        i += tag.length + 2;
        continue;
      }
    }
    if (sql[i] === ";") {
      pushPiece(sql, start, i + 1, out);
      start = i + 1;
    }
    i += 1;
  }
  pushPiece(sql, start, sql.length, out);
  return out;
}

export function statementAt(sql: string, cursor: number): Piece | null {
  const pieces = splitSql(sql);
  const found = pieces.find((piece) => cursor >= piece.start && cursor <= piece.end);
  if (found) return found;
  for (let index = pieces.length - 1; index >= 0; index -= 1) {
    const piece = pieces[index];
    if (piece && piece.end <= cursor) return piece;
  }
  return null;
}

function pushPiece(sql: string, start: number, end: number, out: Piece[]) {
  const raw = sql.slice(start, end);
  const lead = leadingNoise(raw);
  const body = raw.slice(lead).trimEnd();
  if (!meaningful(body)) return;
  const textStart = start + lead;
  out.push({ text: body, start: textStart, end: textStart + body.length });
}

function leadingNoise(raw: string): number {
  let i = 0;
  while (i < raw.length) {
    if (/\s/.test(raw[i] ?? "")) {
      i += 1;
      continue;
    }
    if (raw.startsWith("--", i)) {
      i += 2;
      while (i < raw.length && raw[i] !== "\n") i += 1;
      continue;
    }
    if (raw.startsWith("/*", i)) {
      i += 2;
      while (i + 1 < raw.length && !(raw[i] === "*" && raw[i + 1] === "/")) i += 1;
      i = Math.min(i + 2, raw.length);
      continue;
    }
    break;
  }
  return i;
}

function meaningful(text: string): boolean {
  return stripComments(text).trim().length > 0;
}

function stripComments(sql: string): string {
  let out = "";
  let i = 0;
  let quote: string | null = null;
  while (i < sql.length) {
    if (quote !== null) {
      out += sql[i];
      if (sql[i] === quote) quote = null;
      i += 1;
      continue;
    }
    if (sql.startsWith("--", i)) {
      out += " ";
      i += 2;
      while (i < sql.length && sql[i] !== "\n") i += 1;
      continue;
    }
    if (sql.startsWith("/*", i)) {
      out += " ";
      i += 2;
      while (i + 1 < sql.length && !(sql[i] === "*" && sql[i + 1] === "/")) i += 1;
      i = Math.min(i + 2, sql.length);
      continue;
    }
    if (sql[i] === "'" || sql[i] === '"' || sql[i] === "`") quote = sql[i] ?? null;
    out += sql[i];
    i += 1;
  }
  return out;
}

function dollarTag(sql: string, start: number): string | null {
  const rest = sql.slice(start + 1);
  const end = rest.indexOf("$");
  if (end < 0) return null;
  const tag = rest.slice(0, end);
  if (/^[A-Za-z0-9_]*$/.test(tag)) return tag;
  return null;
}

export type OpenDelimiter = "quote" | "comment" | "dollar";

/** 引号、块注释或 dollar-quote 走到末尾仍未闭合。行注释在文末视为已结束。 */
export function openDelimiter(sql: string): OpenDelimiter | null {
  let i = 0;
  let quote: string | null = null;
  let dollar: string | null = null;
  let comment = false;
  while (i < sql.length) {
    if (dollar !== null) {
      const marker = `$${dollar}$`;
      if (sql.startsWith(marker, i)) {
        i += marker.length;
        dollar = null;
        continue;
      }
      i += 1;
      continue;
    }
    if (quote !== null) {
      if (sql[i] === quote && sql[i + 1] === quote) {
        i += 2;
        continue;
      }
      if (sql[i] === quote) quote = null;
      i += 1;
      continue;
    }
    if (comment) {
      if (sql[i] === "*" && sql[i + 1] === "/") {
        comment = false;
        i += 2;
        continue;
      }
      i += 1;
      continue;
    }
    if (sql.startsWith("--", i)) {
      i += 2;
      while (i < sql.length && sql[i] !== "\n") i += 1;
      continue;
    }
    if (sql.startsWith("/*", i)) {
      comment = true;
      i += 2;
      continue;
    }
    if (sql[i] === "'" || sql[i] === '"' || sql[i] === "`") {
      quote = sql[i] ?? null;
      i += 1;
      continue;
    }
    if (sql[i] === "$") {
      const tag = dollarTag(sql, i);
      if (tag !== null) {
        dollar = tag;
        i += tag.length + 2;
        continue;
      }
    }
    i += 1;
  }
  if (quote) return "quote";
  if (comment) return "comment";
  if (dollar !== null) return "dollar";
  return null;
}
