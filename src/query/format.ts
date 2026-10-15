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
    if (/\d/.test(ch)) {
      let j = i + 1;
      while (j < sql.length && /\d/.test(sql[j] ?? "")) j += 1;
      if (sql[j] === "." && /\d/.test(sql[j + 1] ?? "")) {
        j += 2;
        while (j < sql.length && /\d/.test(sql[j] ?? "")) j += 1;
      }
      tokens.push({ kind: "number", text: sql.slice(i, j) });
      i = j;
      continue;
    }
    if (/[A-Za-z_]/.test(ch)) {
      let j = i + 1;
      while (j < sql.length && /[A-Za-z0-9_]/.test(sql[j] ?? "")) j += 1;
      const text = sql.slice(i, j);
      tokens.push({ kind: "word", text, upper: text.toUpperCase() });
      i = j;
      continue;
    }
    const two = sql.slice(i, i + 2);
    if (two === "<>" || two === "<=" || two === ">=" || two === "!=" || two === "||") {
      tokens.push({ kind: "punct", text: two });
      i += 2;
      continue;
    }
    tokens.push({ kind: "punct", text: ch });
    i += 1;
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
