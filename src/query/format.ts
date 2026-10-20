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
const ADJACENT_REASON = "无法格式化：会把原本相连的字符拆开。";
const COMMENT_BREAK_REASON = "无法格式化：会把行注释断开，后面的文本会变成 SQL。";
const EDGE_GLUE_REASON = "无法格式化：会去掉选区边缘的空白，使两边的字符粘在一起。";

export type FormatResult = { ok: true; sql: string } | { ok: false; reason: string; sql: string };

interface Tok {
  kind: "word" | "string" | "number" | "comment" | "punct" | "dollar";
  text: string;
  /** 这个词法单元前面原本的空白；空字符串表示它和前一个非空白字符相连。 */
  gap: string;
  upper?: string;
}

export function formatSql(sql: string): FormatResult {
  const open = openDelimiter(sql);
  if (open) return { ok: false, reason: OPEN_REASON[open], sql };
  const laid = layout(tokenize(sql));
  if (!laid.ok || !keepsAdjacentNonWhitespace(sql, laid.sql)) {
    return { ok: false, reason: ADJACENT_REASON, sql };
  }
  return { ok: true, sql: laid.sql };
}

export function formatDocument(sql: string, range?: { from: number; to: number }): FormatResult {
  const selected = Boolean(range && range.from !== range.to);
  const from = selected ? Math.max(0, Math.min(range!.from, range!.to)) : 0;
  const to = selected ? Math.min(sql.length, Math.max(range!.from, range!.to)) : sql.length;
  const formatted = formatSql(sql.slice(from, to));
  if (!formatted.ok) return { ok: false, reason: formatted.reason, sql };
  const next = sql.slice(0, from) + formatted.sql + sql.slice(to);
  if (lineCommentBrokenByNewline(sql, next)) return { ok: false, reason: COMMENT_BREAK_REASON, sql };
  if (edgeWhitespaceRemoved(sql, from, to, formatted.sql)) return { ok: false, reason: EDGE_GLUE_REASON, sql };
  return { ok: true, sql: next };
}

function isBlank(ch: string): boolean {
  return /\s/.test(ch);
}

function skipBlank(text: string, index: number): { next: number; newline: boolean } {
  let next = index;
  let newline = false;
  while (next < text.length && isBlank(text[next] ?? "")) {
    const ch = text[next] ?? "";
    if (ch === "\n" || ch === "\r") newline = true;
    next += 1;
  }
  return { next, newline };
}

/** 下一个非空白字符仍落在已经开始的行注释里，说明这个换行把注释拆开了。 */
function commentContinues(inComment: boolean[], index: number): boolean {
  return index > 0 && index < inComment.length && inComment[index] === true && inComment[index - 1] === true;
}

function lineCommentIndexes(sql: string): boolean[] {
  const marked = Array.from({ length: sql.length }, () => false);
  let i = 0;
  let quote: string | null = null;
  let dollar: string | null = null;
  let block = false;
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
    if (block) {
      if (sql[i] === "*" && sql[i + 1] === "/") {
        block = false;
        i += 2;
        continue;
      }
      i += 1;
      continue;
    }
    if (sql[i] === "#" || sql.startsWith("--", i)) {
      const start = i;
      i += sql[i] === "#" ? 1 : 2;
      while (i < sql.length && sql[i] !== "\n" && sql[i] !== "\r") i += 1;
      for (let k = start; k < i; k += 1) marked[k] = true;
      continue;
    }
    if (sql.startsWith("/*", i)) {
      block = true;
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
  return marked;
}

function lineCommentBrokenByNewline(source: string, formatted: string): boolean {
  const inComment = lineCommentIndexes(source);
  let i = 0;
  let j = 0;
  while (i < source.length && j < formatted.length) {
    const sourceBlank = isBlank(source[i] ?? "");
    const formattedBlank = isBlank(formatted[j] ?? "");
    if (sourceBlank && formattedBlank) {
      const sourceGap = skipBlank(source, i);
      const formattedGap = skipBlank(formatted, j);
      if (formattedGap.newline && commentContinues(inComment, sourceGap.next)) return true;
      i = sourceGap.next;
      j = formattedGap.next;
      continue;
    }
    if (sourceBlank) {
      i = skipBlank(source, i).next;
      continue;
    }
    if (formattedBlank) {
      const formattedGap = skipBlank(formatted, j);
      if (formattedGap.newline && commentContinues(inComment, i)) return true;
      j = formattedGap.next;
      continue;
    }
    i += 1;
    j += 1;
  }
  return false;
}

function outsideTouches(outside: string, inside: string, outsideIsBefore: boolean): boolean {
  if (!outside || !inside) return false;
  const outsideChar = outsideIsBefore ? outside[outside.length - 1] ?? "" : outside[0] ?? "";
  const insideChar = outsideIsBefore ? inside[0] ?? "" : inside[inside.length - 1] ?? "";
  return !isBlank(outsideChar) && !isBlank(insideChar);
}

function edgeWhitespaceRemoved(sql: string, from: number, to: number, formatted: string): boolean {
  const prefix = sql.slice(0, from);
  const selected = sql.slice(from, to);
  const suffix = sql.slice(to);
  if (!selected || (!prefix && !suffix)) return false;
  if (/^\s/.test(selected) && !/^\s/.test(formatted) && outsideTouches(prefix, formatted, true)) return true;
  if (/\s$/.test(selected) && !/\s$/.test(formatted) && outsideTouches(suffix, formatted, false)) return true;
  if (/^\s+$/.test(selected) && formatted.length === 0) {
    const left = prefix[prefix.length - 1] ?? "";
    const right = suffix[0] ?? "";
    return Boolean(left && right && !isBlank(left) && !isBlank(right));
  }
  return false;
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
  return ch === "_" || ch === "$" || /^[\p{L}\p{Nd}\p{M}]$/u.test(ch);
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

function eatDigits(sql: string, index: number): number {
  let j = index;
  while (j < sql.length) {
    if (/\d/.test(sql[j] ?? "")) {
      j += 1;
      continue;
    }
    if (sql[j] === "_" && /\d/.test(sql[j + 1] ?? "")) {
      j += 2;
      continue;
    }
    break;
  }
  return j;
}

function scanExponent(sql: string, index: number): number | null {
  if (sql[index] !== "e" && sql[index] !== "E") return null;
  let k = index + 1;
  if (sql[k] === "+" || sql[k] === "-") k += 1;
  if (!/\d/.test(sql[k] ?? "")) return null;
  while (k < sql.length && /\d/.test(sql[k] ?? "")) k += 1;
  return k;
}

function scanNumber(sql: string, start: number): number | null {
  const head = sql[start] ?? "";
  if (!/\d/.test(head)) return null;
  const mark = sql[start + 1] ?? "";
  const hex = head === "0" && (mark === "x" || mark === "X") && /[0-9A-Fa-f]/.test(sql[start + 2] ?? "");
  if (hex) {
    let j = start + 3;
    while (j < sql.length && /[0-9A-Fa-f]/.test(sql[j] ?? "")) j += 1;
    return j;
  }
  const bin = head === "0" && (mark === "b" || mark === "B") && /[01]/.test(sql[start + 2] ?? "");
  if (bin) {
    let j = start + 3;
    while (j < sql.length && /[01]/.test(sql[j] ?? "")) j += 1;
    return j;
  }
  let j = eatDigits(sql, start);
  if (sql[j] === ".") {
    const dotted = scanExponent(sql, j + 1);
    if (dotted !== null && !/\d/.test(sql[j + 1] ?? "")) return dotted;
    if (/\d/.test(sql[j + 1] ?? "")) j = eatDigits(sql, j + 1);
  }
  return scanExponent(sql, j) ?? j;
}

function tokenize(sql: string): Tok[] {
  const tokens: Tok[] = [];
  let i = 0;
  while (i < sql.length) {
    const gapStart = i;
    while (i < sql.length && /\s/.test(sql[i] ?? "")) i += 1;
    if (i >= sql.length) break;
    const gap = sql.slice(gapStart, i);
    const ch = sql[i] ?? "";
    const push = (kind: Tok["kind"], text: string, next: number, upper?: string) => {
      const tok: Tok = { kind, text, gap };
      if (upper) tok.upper = upper;
      tokens.push(tok);
      i = next;
    };
    if (sql.startsWith("--", i) || ch === "#") {
      let j = ch === "#" ? i + 1 : i + 2;
      while (j < sql.length && sql[j] !== "\n" && sql[j] !== "\r") j += 1;
      push("comment", sql.slice(i, j), j);
      continue;
    }
    if (sql.startsWith("/*", i)) {
      const end = sql.indexOf("*/", i + 2);
      const j = end < 0 ? sql.length : end + 2;
      push("comment", sql.slice(i, j), j);
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
      push("string", sql.slice(i, j), j);
      continue;
    }
    if (ch === "$") {
      const tag = dollarTag(sql, i);
      if (tag !== null) {
        const marker = `$${tag}$`;
        const end = sql.indexOf(marker, i + marker.length);
        const j = end < 0 ? sql.length : end + marker.length;
        push("dollar", sql.slice(i, j), j);
        continue;
      }
      if (/\d/.test(sql[i + 1] ?? "")) {
        let j = i + 2;
        while (j < sql.length && /\d/.test(sql[j] ?? "")) j += 1;
        push("word", sql.slice(i, j), j);
        continue;
      }
    }
    if ((ch === "x" || ch === "X" || ch === "b" || ch === "B") && sql[i + 1] === "'") {
      const j = skipQuoted(sql, i + 1);
      push("number", sql.slice(i, j), j);
      continue;
    }
    const numberEnd = scanNumber(sql, i);
    if (numberEnd !== null) {
      push("number", sql.slice(i, numberEnd), numberEnd);
      continue;
    }
    const word = sqlWordAt(sql, i);
    if (word) {
      push("word", word.text, word.end, word.text.toUpperCase());
      continue;
    }
    const three = sql.slice(i, i + 3);
    if (three === "->>") {
      push("punct", three, i + 3);
      continue;
    }
    const two = sql.slice(i, i + 2);
    if (two === "::" || two === "->" || two === "<>" || two === "<=" || two === ">=" || two === "!=" || two === "||") {
      push("punct", two, i + 2);
      continue;
    }
    if ((ch === ":" || ch === "@") && two !== "::") {
      const named = sqlWordAt(sql, i + 1);
      if (named) {
        push("word", sql.slice(i, named.end), named.end);
        continue;
      }
    }
    const point = codePointAt(sql, i);
    push("punct", point.ch, point.next);
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

function layout(tokens: Tok[]): { ok: true; sql: string } | { ok: false } {
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
    const lineComment = prev?.kind === "comment" && (prev.text.startsWith("--") || prev.text.startsWith("#"));
    let placed: "none" | "space" | "newline" = "none";
    let level = 0;
    let nextBreakBody: boolean = breakBody;
    if (depth === 0 && kind === "clause") {
      if (out.trim().length) {
        placed = "newline";
        level = 0;
      }
      nextBreakBody = true;
    } else if (depth === 0 && kind === "sub") {
      if (out.trim().length) {
        placed = "newline";
        level = 1;
      }
    } else if (lineComment && out.trim().length) {
      placed = "newline";
      level = 1 + depth;
      nextBreakBody = false;
    } else if (breakBody && depth === 0 && !holds(prev, tok)) {
      placed = "newline";
      level = 1;
      nextBreakBody = false;
    } else if (prev?.kind === "punct" && prev.text === "," && depth === 0 && tok.kind !== "comment") {
      placed = "newline";
      level = 1;
    } else if (!tight(tok)) {
      placed = "space";
    }
    if (placed === "newline") {
      if (tok.gap.length === 0) return { ok: false };
      newline(level);
    } else if (placed === "space" && tok.gap.length > 0) {
      space();
    }
    out += render(tok);
    if (tok.kind === "punct" && tok.text === "(") depth += 1;
    if (tok.kind === "punct" && tok.text === ")") depth = Math.max(0, depth - 1);
    breakBody = nextBreakBody;
    prev = tok;
  }
  return { ok: true, sql: out.replace(/[ \t]+$/, "") };
}

/** 成功的格式化只改关键字大小写，并重排原本就有的空白。 */
function keepsAdjacentNonWhitespace(source: string, formatted: string): boolean {
  const left = Array.from(source);
  const right = Array.from(formatted);
  const blank = (ch: string) => /\s/u.test(ch);
  let i = 0;
  let j = 0;
  while (i < left.length && j < right.length) {
    const sourceChar = left[i] ?? "";
    const formattedChar = right[j] ?? "";
    if (blank(sourceChar) && blank(formattedChar)) {
      while (i < left.length && blank(left[i] ?? "")) i += 1;
      while (j < right.length && blank(right[j] ?? "")) j += 1;
      continue;
    }
    if (blank(sourceChar)) {
      while (i < left.length && blank(left[i] ?? "")) i += 1;
      continue;
    }
    if (blank(formattedChar)) return false;
    if (sourceChar.toUpperCase() !== formattedChar.toUpperCase()) return false;
    i += 1;
    j += 1;
  }
  while (i < left.length && blank(left[i] ?? "")) i += 1;
  while (j < right.length && blank(right[j] ?? "")) j += 1;
  return i === left.length && j === right.length;
}
