export interface Piece {
  text: string;
  start: number;
  end: number;
}

export interface SplitReport {
  pieces: Piece[];
  notices: string[];
  refusal: string | null;
  /** 拒绝片段的起点。光标落在这里或之后时，不能退回前面已经闭合的语句。 */
  refusalFrom: number | null;
}

const ROUTINE_TAIL = new Set(["IF", "LOOP", "WHILE", "REPEAT", "CASE"]);

export function splitSql(sql: string, engine = "postgres"): Piece[] {
  return inspectSql(sql, engine).pieces;
}

/** 按引擎切语句。拒绝原因非空时，已完成的语句仍留在 pieces 里，但整批脚本不能发送。 */
export function inspectSql(sql: string, engine = "postgres"): SplitReport {
  const hash = hashComments(engine);
  const notices: string[] = [];
  const pieces: Piece[] = [];
  let delimiter = ";";
  let noticed = false;
  let i = 0;
  while (i < sql.length) {
    const begin = skipNoise(sql, i, hash);
    if (begin >= sql.length) break;
    if (isDelimiterCommand(sql, begin)) {
      const lineEnd = eolAt(sql, begin);
      const arg = delimiterArgument(sql.slice(begin, lineEnd));
      if (engine === "postgres" || engine === "sqlite") {
        if (!noticed) {
          notices.push(`${engineLabel(engine)} 不使用 DELIMITER 这条客户端指令，已跳过。其余语句仍按该引擎执行。`);
          noticed = true;
        }
        i = skipBreak(sql, lineEnd);
        continue;
      }
      if (arg == null) return { pieces, notices, refusal: "自定义结束符没有写明，没有发送。", refusalFrom: begin };
      delimiter = arg;
      i = skipBreak(sql, lineEnd);
      continue;
    }
    const scanned = scanStatement(sql, begin, delimiter, hash);
    if (scanned.piece) pieces.push(scanned.piece);
    if (scanned.refusal) return { pieces, notices, refusal: scanned.refusal, refusalFrom: begin };
    if (scanned.next <= begin) break;
    i = scanned.next;
  }
  return { pieces, notices, refusal: null, refusalFrom: null };
}

export function statementAt(sql: string, cursor: number, engine = "postgres"): Piece | null {
  return locateStatement(sql, cursor, engine).piece;
}

/** 光标落在未闭合片段上时 piece 为空，并把 refusal 交回调用方。 */
export function locateStatement(sql: string, cursor: number, engine = "postgres"): { piece: Piece | null; refusal: string | null } {
  const report = inspectSql(sql, engine);
  const found = report.pieces.find((piece) => cursor >= piece.start && cursor <= piece.end);
  if (found) return { piece: found, refusal: null };
  if (report.refusal && report.refusalFrom != null && cursor >= report.refusalFrom) {
    return { piece: null, refusal: report.refusal };
  }
  for (let index = report.pieces.length - 1; index >= 0; index -= 1) {
    const piece = report.pieces[index];
    if (piece && piece.end <= cursor) return { piece, refusal: null };
  }
  if (report.refusal) return { piece: null, refusal: report.refusal };
  return { piece: null, refusal: null };
}

/** 去掉注释、字符串和 dollar-quote 后，代码里还有 ANALYZE 或 ANALYSE。MySQL 可执行注释仍要检查。 */
export function codeHasAnalyze(sql: string, engine = "postgres"): boolean {
  const hash = hashComments(engine);
  let i = 0;
  while (i < sql.length) {
    const ch = sql[i] ?? "";
    if (/\s/.test(ch)) {
      i += 1;
      continue;
    }
    if (sql.startsWith("--", i) || (hash && ch === "#")) {
      i = eolAt(sql, i + (ch === "#" ? 1 : 2));
      continue;
    }
    if (sql.startsWith("/*", i)) {
      if (sql.startsWith("/*!", i)) {
        let j = i + 3;
        while (j < sql.length && /\d/.test(sql[j] ?? "")) j += 1;
        const close = sql.indexOf("*/", j);
        const bodyEnd = close < 0 ? sql.length : close;
        if (codeHasAnalyze(sql.slice(j, bodyEnd), engine)) return true;
        i = close < 0 ? sql.length : close + 2;
        continue;
      }
      const end = sql.indexOf("*/", i + 2);
      i = end < 0 ? sql.length : end + 2;
      continue;
    }
    if (ch === "'" || ch === '"' || ch === "`") {
      i = quotedEnd(sql, i);
      continue;
    }
    if (ch === "$") {
      const end = dollarEnd(sql, i);
      if (end !== null) {
        i = end;
        continue;
      }
    }
    if (isIdentStart(ch)) {
      const word = readWord(sql, i);
      const upper = word.text.toUpperCase();
      if (upper === "ANALYZE" || upper === "ANALYSE") return true;
      i = word.end;
      continue;
    }
    i += 1;
  }
  return false;
}

export type OpenDelimiter = "quote" | "comment" | "dollar";

/** 引号、块注释或 dollar-quote 走到末尾仍未闭合。行注释在文末视为已结束。 */
export function openDelimiter(sql: string, engine?: string): OpenDelimiter | null {
  const hash = engine == null ? true : hashComments(engine);
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
    if (hash && sql[i] === "#") {
      i = eolAt(sql, i + 1);
      continue;
    }
    if (sql.startsWith("--", i)) {
      i = eolAt(sql, i + 2);
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

function scanStatement(sql: string, start: number, delimiter: string, hash: boolean): { piece: Piece | null; next: number; refusal: string | null } {
  let i = start;
  let quote: string | null = null;
  let dollar: string | null = null;
  let block = false;
  let paren = 0;
  let beginDepth = 0;
  let routine = false;
  let decided = false;
  const words: string[] = [];
  let closed = false;
  let closeAt = start;
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
    if (block) {
      if (sql[i] === "*" && sql[i + 1] === "/") {
        block = false;
        i += 2;
        continue;
      }
      i += 1;
      continue;
    }
    if (sql.startsWith("--", i)) {
      i = eolAt(sql, i + 2);
      continue;
    }
    if (hash && sql[i] === "#") {
      i = eolAt(sql, i + 1);
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
      if (paren === 0 && beginDepth === 0 && delimiter.startsWith("$") && sql.startsWith(delimiter, i)) {
        closed = true;
        closeAt = i + delimiter.length;
        i = closeAt;
        break;
      }
      const tag = dollarTag(sql, i);
      if (tag !== null) {
        dollar = tag;
        i += tag.length + 2;
        continue;
      }
    }
    if (sql[i] === "(") {
      paren += 1;
      i += 1;
      continue;
    }
    if (sql[i] === ")") {
      paren = Math.max(0, paren - 1);
      i += 1;
      continue;
    }
    if (paren === 0 && isIdentStart(sql[i] ?? "")) {
      const word = readWord(sql, i);
      const upper = word.text.toUpperCase();
      if (!decided) {
        words.push(upper);
        const look = routineLook(words);
        if (look === "yes") {
          routine = true;
          decided = true;
        } else if (look === "no") decided = true;
      }
      if (upper === "BEGIN" && routine) beginDepth += 1;
      else if (upper === "END" && routine && beginDepth > 0) {
        const next = peekWord(sql, word.end, hash);
        if (!next || !ROUTINE_TAIL.has(next)) beginDepth -= 1;
      }
      i = word.end;
      continue;
    }
    if (paren === 0 && beginDepth === 0 && delimiter.length > 0 && sql.startsWith(delimiter, i)) {
      closed = true;
      closeAt = i + delimiter.length;
      i = closeAt;
      break;
    }
    i += 1;
  }
  if (quote) return { piece: null, next: sql.length, refusal: "引号没有闭合，没有发送。" };
  if (block) return { piece: null, next: sql.length, refusal: "块注释没有闭合，没有发送。" };
  if (dollar !== null) return { piece: null, next: sql.length, refusal: "dollar-quote 没有闭合，没有发送。" };
  if (beginDepth > 0) return { piece: null, next: sql.length, refusal: "例程体未闭合，没有发送。" };
  if (!closed && delimiter !== ";" && meaningful(sql.slice(start, i), hash)) {
    return { piece: null, next: sql.length, refusal: "自定义结束符没有闭合，没有发送。" };
  }
  const end = closed ? closeAt : i;
  return { piece: makePiece(sql, start, end, delimiter, closed, hash), next: end, refusal: null };
}

function makePiece(sql: string, start: number, end: number, delimiter: string, closed: boolean, hash: boolean): Piece | null {
  const bodyEnd = closed && delimiter !== ";" ? end - delimiter.length : end;
  const raw = sql.slice(start, Math.max(start, bodyEnd));
  const lead = leadingNoise(raw, hash);
  const body = raw.slice(lead).trimEnd();
  if (!meaningful(body, hash)) return null;
  const textStart = start + lead;
  return { text: body, start: textStart, end: textStart + body.length };
}

const ROUTINE_KIND = new Set(["PROCEDURE", "FUNCTION", "TRIGGER"]);
const NOT_ROUTINE = new Set([
  "TABLE", "VIEW", "INDEX", "UNIQUE", "FULLTEXT", "SPATIAL", "DATABASE", "SCHEMA", "USER", "ROLE",
  "EVENT", "SERVER", "TABLESPACE", "LOGFILE", "SEQUENCE", "TYPE", "EXTENSION", "TEMPORARY", "TEMP",
  "UNLOGGED", "MATERIALIZED", "RECURSIVE", "POLICY", "RULE", "DOMAIN", "CAST", "OPERATOR", "AGGREGATE",
  "COLLATION", "CONVERSION", "LANGUAGE",
]);

function routineLook(words: string[]): "yes" | "no" | "maybe" {
  if (words[0] !== "CREATE") return "no";
  let index = 1;
  if (words[index] === "OR") {
    if (words[index + 1] == null) return "maybe";
    if (words[index + 1] !== "REPLACE") return "no";
    index += 2;
  }
  if (words[index] === "DEFINER") {
    index += 1;
    while (words[index] && !ROUTINE_KIND.has(words[index]!)) {
      if (NOT_ROUTINE.has(words[index]!)) return "no";
      index += 1;
    }
  }
  const kind = words[index];
  if (kind == null) return "maybe";
  if (ROUTINE_KIND.has(kind)) return "yes";
  return "no";
}

function isDelimiterCommand(sql: string, index: number): boolean {
  const word = readWord(sql, index);
  return word.text.toUpperCase() === "DELIMITER";
}

function delimiterArgument(command: string): string | null {
  const match = /^\s*delimiter\b([\s\S]*)$/i.exec(command);
  if (!match) return null;
  const arg = (match[1] ?? "").trim();
  return arg.length ? arg : null;
}

function engineLabel(engine: string): string {
  if (engine === "sqlite") return "SQLite";
  if (engine === "mysql") return "MySQL";
  if (engine === "mariadb") return "MariaDB";
  return "PostgreSQL";
}

function hashComments(engine: string): boolean {
  return engine === "mysql" || engine === "mariadb";
}

function skipNoise(sql: string, index: number, hash: boolean): number {
  let i = index;
  while (i < sql.length) {
    if (/\s/.test(sql[i] ?? "")) {
      i += 1;
      continue;
    }
    if (sql.startsWith("--", i)) {
      i = eolAt(sql, i + 2);
      continue;
    }
    if (hash && sql[i] === "#") {
      i = eolAt(sql, i + 1);
      continue;
    }
    if (sql.startsWith("/*", i)) {
      i += 2;
      while (i + 1 < sql.length && !(sql[i] === "*" && sql[i + 1] === "/")) i += 1;
      i = Math.min(i + 2, sql.length);
      continue;
    }
    break;
  }
  return i;
}

function leadingNoise(raw: string, hash: boolean): number {
  return skipNoise(raw, 0, hash);
}

function meaningful(text: string, hash: boolean): boolean {
  return stripComments(text, hash).trim().length > 0;
}

function stripComments(sql: string, hash: boolean): string {
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
    if (sql.startsWith("--", i) || (hash && sql[i] === "#")) {
      out += " ";
      i = eolAt(sql, i + (sql[i] === "#" ? 1 : 2));
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

function peekWord(sql: string, index: number, hash: boolean): string | null {
  const at = skipNoise(sql, index, hash);
  if (!isIdentStart(sql[at] ?? "")) return null;
  return readWord(sql, at).text.toUpperCase();
}

function readWord(sql: string, index: number): { text: string; end: number } {
  let end = index + 1;
  while (end < sql.length && isIdentCont(sql[end] ?? "")) end += 1;
  return { text: sql.slice(index, end), end };
}

function isIdentStart(ch: string): boolean {
  return /[A-Za-z_]/.test(ch);
}

function isIdentCont(ch: string): boolean {
  return /[A-Za-z0-9_]/.test(ch);
}

function eolAt(sql: string, index: number): number {
  let i = index;
  while (i < sql.length && sql[i] !== "\n" && sql[i] !== "\r") i += 1;
  return i;
}

function skipBreak(sql: string, index: number): number {
  if (sql[index] === "\r" && sql[index + 1] === "\n") return index + 2;
  if (sql[index] === "\n" || sql[index] === "\r") return index + 1;
  return index;
}

function quotedEnd(sql: string, start: number): number {
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

function dollarEnd(sql: string, start: number): number | null {
  const tag = dollarTag(sql, start);
  if (tag === null) return null;
  const marker = `$${tag}$`;
  const close = sql.indexOf(marker, start + marker.length);
  return close < 0 ? sql.length : close + marker.length;
}

function dollarTag(sql: string, start: number): string | null {
  const rest = sql.slice(start + 1);
  const end = rest.indexOf("$");
  if (end < 0) return null;
  const tag = rest.slice(0, end);
  if (/^[A-Za-z0-9_]*$/.test(tag)) return tag;
  return null;
}
