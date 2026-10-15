import { SQL_KEYWORDS } from "./complete";
import { openDelimiter, type OpenDelimiter } from "./split";

const KEYWORDS = new Set<string>(SQL_KEYWORDS);
const CLAUSE = new Set(["SELECT", "FROM", "WHERE", "GROUP", "ORDER", "HAVING", "LIMIT", "OFFSET", "UNION", "INSERT", "UPDATE", "DELETE", "VALUES", "SET", "WITH", "RETURNING"]);
const SUB = new Set(["AND", "OR", "JOIN", "LEFT", "RIGHT", "INNER", "FULL", "CROSS"]);
const HOLDS: Record<string, readonly string[]> = {
  SELECT: ["DISTINCT"],
  GROUP: ["BY"],
  ORDER: ["BY"],
  INSERT: ["INTO"],
  UNION: ["ALL"],
  LEFT: ["OUTER", "JOIN"],
  RIGHT: ["OUTER", "JOIN"],
  INNER: ["JOIN"],
  FULL: ["OUTER", "JOIN"],
  CROSS: ["JOIN"],
  OUTER: ["JOIN"],
  IS: ["NOT", "NULL"],
  NOT: ["NULL", "IN", "EXISTS"],
};

const OPEN_REASON: Record<OpenDelimiter, string> = {
  quote: "无法格式化：引号未闭合。",
  comment: "无法格式化：注释未闭合。",
  dollar: "无法格式化：dollar-quote 未闭合。",
};

export type FormatResult = { ok: true; sql: string } | { ok: false; reason: string; sql: string };

interface Tok {
  kind: "word" | "string" | "number" | "comment" | "punct" | "dollar";
  text: string;
  upper?: string;
}

export function formatSql(sql: string): FormatResult {
  const open = openDelimiter(sql);
  if (open) return { ok: false, reason: OPEN_REASON[open], sql };
  return { ok: true, sql: layout(tokenize(sql)) };
}

export function formatDocument(sql: string, range?: { from: number; to: number }): FormatResult {
  const selected = Boolean(range && range.from !== range.to);
  const from = selected ? Math.max(0, Math.min(range!.from, range!.to)) : 0;
  const to = selected ? Math.min(sql.length, Math.max(range!.from, range!.to)) : sql.length;
  const formatted = formatSql(sql.slice(from, to));
  if (!formatted.ok) return { ok: false, reason: formatted.reason, sql };
  return { ok: true, sql: sql.slice(0, from) + formatted.sql + sql.slice(to) };
}

function dollarTag(sql: string, start: number): string | null {
  const rest = sql.slice(start + 1);
  const end = rest.indexOf("$");
  if (end < 0) return null;
  const tag = rest.slice(0, end);
  if (/^[A-Za-z0-9_]*$/.test(tag)) return tag;
  return null;
}

function codePointAt(sql: string, index: number): { ch: string; next: number } {
  const cp = sql.codePointAt(index);
  if (cp === undefined) return { ch: "", next: index + 1 };
  const ch = String.fromCodePoint(cp);
  return { ch, next: index + ch.length };
}

function isIdentStart(ch: string): boolean {
  return ch === "_" || /^\p{L}$/u.test(ch);
}

function isIdentCont(ch: string): boolean {
  return ch === "_" || /^[\p{L}\p{Nd}\p{M}]$/u.test(ch);
}

/** 从 start 读取一个 SQL 标识符；引号、注释和数值不在这里处理。 */
export function sqlWordAt(sql: string, start: number): { text: string; end: number } | null {
  const first = codePointAt(sql, start);
  if (!isIdentStart(first.ch)) return null;
  let end = first.next;
  while (end < sql.length) {
    const cont = codePointAt(sql, end);
    if (!isIdentCont(cont.ch)) break;
    end = cont.next;
  }
  return { text: sql.slice(start, end), end };
}

function skipQuoted(sql: string, start: number): number {
  const quote = sql[start] ?? "";
  let j = start + 1;
  while (j < sql.length) {
    if (sql[j] === quote && sql[j + 1] === quote) {
      j += 2;
      continue;
    }
    if (sql[j] === quote) return j + 1;
    j += 1;
  }
  return sql.length;
}

function scanNumber(sql: string, start: number): number | null {
  const head = sql[start] ?? "";
  if (!/\d/.test(head)) return null;
  const hex = head === "0" && (sql[start + 1] === "x" || sql[start + 1] === "X") && /[0-9A-Fa-f]/.test(sql[start + 2] ?? "");
  if (hex) {
    let j = start + 3;
    while (j < sql.length && /[0-9A-Fa-f]/.test(sql[j] ?? "")) j += 1;
    return j;
  }
  let j = start + 1;
  while (j < sql.length && /\d/.test(sql[j] ?? "")) j += 1;
  if (sql[j] === "." && /\d/.test(sql[j + 1] ?? "")) {
    j += 2;
    while (j < sql.length && /\d/.test(sql[j] ?? "")) j += 1;
  }
  if (sql[j] === "e" || sql[j] === "E") {
    let k = j + 1;
    if (sql[k] === "+" || sql[k] === "-") k += 1;
    if (/\d/.test(sql[k] ?? "")) {
      k += 1;
      while (k < sql.length && /\d/.test(sql[k] ?? "")) k += 1;
      j = k;
    }
  }
  return j;
}

function tokenize(sql: string): Tok[] {
  const tokens: Tok[] = [];
  let i = 0;
  while (i < sql.length) {
    const ch = sql[i] ?? "";
    if (/\s/.test(ch)) {
      i += 1;
      continue;
    }
    if (sql.startsWith("--", i)) {
      let j = i + 2;
      while (j < sql.length && sql[j] !== "\n" && sql[j] !== "\r") j += 1;
      tokens.push({ kind: "comment", text: sql.slice(i, j) });
      i = j;
      continue;
    }
    if (sql.startsWith("/*", i)) {
      const end = sql.indexOf("*/", i + 2);
      const j = end < 0 ? sql.length : end + 2;
      tokens.push({ kind: "comment", text: sql.slice(i, j) });
      i = j;
      continue;
    }
    if (ch === "'" || ch === '"' || ch === "`") {
      let j = i + 1;
      while (j < sql.length) {
        if (sql[j] === ch && sql[j + 1] === ch) {
          j += 2;
          continue;
        }
        if (sql[j] === ch) {
          j += 1;
          break;
        }
        j += 1;
      }
      tokens.push({ kind: "string", text: sql.slice(i, j) });
      i = j;
      continue;
    }
    if (ch === "$") {
      const tag = dollarTag(sql, i);
      if (tag !== null) {
        const marker = `$${tag}$`;
        const end = sql.indexOf(marker, i + marker.length);
        const j = end < 0 ? sql.length : end + marker.length;
        tokens.push({ kind: "dollar", text: sql.slice(i, j) });
        i = j;
        continue;
      }
    }
    if ((ch === "x" || ch === "X") && sql[i + 1] === "'") {
      const j = skipQuoted(sql, i + 1);
      tokens.push({ kind: "number", text: sql.slice(i, j) });
      i = j;
      continue;
    }
    const numberEnd = scanNumber(sql, i);
    if (numberEnd !== null) {
      tokens.push({ kind: "number", text: sql.slice(i, numberEnd) });
      i = numberEnd;
      continue;
    }
    const word = sqlWordAt(sql, i);
    if (word) {
      tokens.push({ kind: "word", text: word.text, upper: word.text.toUpperCase() });
      i = word.end;
      continue;
    }
    const three = sql.slice(i, i + 3);
    if (three === "->>") {
      tokens.push({ kind: "punct", text: three });
      i += 3;
      continue;
    }
    const two = sql.slice(i, i + 2);
    if (two === "::" || two === "->" || two === "<>" || two === "<=" || two === ">=" || two === "!=" || two === "||") {
      tokens.push({ kind: "punct", text: two });
      i += 2;
      continue;
    }
    const point = codePointAt(sql, i);
    tokens.push({ kind: "punct", text: point.ch });
    i = point.next;
  }
  return tokens;
}

function holds(prev: Tok | undefined, tok: Tok): boolean {
  if (!prev || prev.kind !== "word" || tok.kind !== "word" || !prev.upper || !tok.upper) return false;
  return HOLDS[prev.upper]?.includes(tok.upper) ?? false;
}

function breakKind(tok: Tok, prev: Tok | undefined): "clause" | "sub" | null {
  if (tok.kind !== "word" || !tok.upper || holds(prev, tok)) return null;
  if (CLAUSE.has(tok.upper)) return "clause";
  if (SUB.has(tok.upper)) return "sub";
  return null;
}

function render(tok: Tok): string {
  if (tok.kind === "word" && tok.upper && KEYWORDS.has(tok.upper)) return tok.upper;
  return tok.text;
}

function layout(tokens: Tok[]): string {
  let out = "";
  let depth = 0;
  let breakBody = false;
  let prev: Tok | undefined;
  const newline = (level: number) => {
    out = out.replace(/[ \t]+$/, "");
    if (out.length) out += "\n";
    out += "  ".repeat(level);
  };
  const space = () => {
    if (!out.length || /[\s.(]$/.test(out)) return;
    out += " ";
  };
  const tight = (tok: Tok): boolean => {
    if (tok.kind === "punct" && (tok.text === "," || tok.text === ";" || tok.text === "." || tok.text === ")")) return true;
    if (tok.kind === "punct" && tok.text === "(" && prev?.kind === "word" && prev.upper && !CLAUSE.has(prev.upper) && !SUB.has(prev.upper)) return true;
    if (prev?.kind === "punct" && (prev.text === "(" || prev.text === ".")) return true;
    return false;
  };

  for (const tok of tokens) {
    const kind = breakKind(tok, prev);
    const lineComment = prev?.kind === "comment" && prev.text.startsWith("--");
    if (depth === 0 && kind === "clause") {
      if (out.trim().length) newline(0);
      breakBody = true;
    } else if (depth === 0 && kind === "sub") {
      if (out.trim().length) newline(1);
    } else if (lineComment && out.trim().length) {
      newline(1 + depth);
      breakBody = false;
    } else if (breakBody && depth === 0 && !holds(prev, tok)) {
      newline(1);
      breakBody = false;
    } else if (prev?.kind === "punct" && prev.text === "," && depth === 0 && tok.kind !== "comment") {
      newline(1);
    } else if (!tight(tok)) {
      space();
    }
    out += render(tok);
    if (tok.kind === "punct" && tok.text === "(") depth += 1;
    if (tok.kind === "punct" && tok.text === ")") depth = Math.max(0, depth - 1);
    prev = tok;
  }
  return out.replace(/[ \t]+$/, "");
}
