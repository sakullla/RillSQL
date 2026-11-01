export type ScopeClause = "select" | "from" | "join" | "on" | "where" | "group" | "order" | "having" | "set" | "values" | "use";

export interface ScopeTable {
  /** 语句里使用的名字：显式别名优先，否则是表名。 */
  alias: string;
  schema: string | null;
  name: string;
  /** 引用所在的括号深度，用于隔离子查询作用域。 */
  depth: number;
}

export interface StatementScope {
  clause: ScopeClause | null;
  tables: ScopeTable[];
  ctes: string[];
  /** UPDATE / INSERT 的目标表；没有则为 null。 */
  target: ScopeTable | null;
}

interface Token {
  text: string;
  upper: string;
  word: boolean;
}

const CLAUSE_BY_WORD: Record<string, ScopeClause> = {
  SELECT: "select",
  FROM: "from",
  JOIN: "join",
  ON: "on",
  WHERE: "where",
  GROUP: "group",
  ORDER: "order",
  HAVING: "having",
  SET: "set",
  VALUES: "values",
  USE: "use",
};

/** 子句只认查询结构关键字；DELETE/INSERT 等由最近的关键字决定其后的列上下文。 */
const RESERVED = new Set([
  "SELECT", "FROM", "WHERE", "AND", "OR", "NOT", "NULL", "IS", "IN", "EXISTS", "INSERT", "INTO",
  "VALUES", "UPDATE", "SET", "DELETE", "CREATE", "ALTER", "DROP", "TABLE", "VIEW", "INDEX", "JOIN",
  "LEFT", "RIGHT", "INNER", "OUTER", "FULL", "CROSS", "ON", "GROUP", "BY", "ORDER", "LIMIT",
  "OFFSET", "AS", "DISTINCT", "HAVING", "UNION", "ALL", "CASE", "WHEN", "THEN", "ELSE", "END",
  "WITH", "USING", "RETURNING", "ASC", "DESC", "NULLS", "FIRST", "LAST",
]);

function tokenize(text: string): Token[] {
  const tokens: Token[] = [];
  let i = 0;
  while (i < text.length) {
    const ch = text[i] ?? "";
    if (/\s/.test(ch)) {
      i += 1;
      continue;
    }
    if (ch === "-" && text[i + 1] === "-") {
      const end = text.indexOf("\n", i);
      i = end < 0 ? text.length : end;
      continue;
    }
    if (ch === "/" && text[i + 1] === "*") {
      const end = text.indexOf("*/", i + 2);
      i = end < 0 ? text.length : end + 2;
      continue;
    }
    if (ch === "'" || ch === '"' || ch === "`") {
      const quote = ch;
      let end = i + 1;
      while (end < text.length) {
        if (text[end] === quote && text[end + 1] === quote) {
          end += 2;
          continue;
        }
        if (text[end] === quote) {
          end += 1;
          break;
        }
        end += 1;
      }
      i = end;
      continue;
    }
    if (/[A-Za-z_]/.test(ch)) {
      let end = i + 1;
      while (end < text.length && /[A-Za-z0-9_$]/.test(text[end] ?? "")) end += 1;
      const word = text.slice(i, end);
      tokens.push({ text: word, upper: word.toUpperCase(), word: true });
      i = end;
      continue;
    }
    if (ch === "." || ch === "," || ch === "(" || ch === ")") tokens.push({ text: ch, upper: ch, word: false });
    i += 1;
  }
  return tokens;
}

/** 解析语句作用域：当前子句、FROM/JOIN 表与别名、WITH 定义的 CTE 名。 */
export function parseScope(statement: string): StatementScope {
  const tokens = tokenize(statement);
  const tables: ScopeTable[] = [];
  const ctes: string[] = [];
  let clause: ScopeClause | null = null;
  let depth = 0;
  let here = 0;
  let target: ScopeTable | null = null;

  for (let index = 0; index < tokens.length; index += 1) {
    const token = tokens[index]!;
    if (!token.word) {
      if (token.text === "(") depth += 1;
      if (token.text === ")") depth = Math.max(0, depth - 1);
      here = depth;
      continue;
    }
    if (token.upper === "WITH") {
      index = readCtes(tokens, index + 1, ctes) - 1;
      continue;
    }
    if (token.upper === "UPDATE" || (token.upper === "INTO" && !target)) {
      const read = readTarget(tokens, index + 1, here);
      if (read.table) target = read.table;
      index = read.next - 1;
      continue;
    }
    if (token.upper === "FROM" || token.upper === "JOIN") {
      clause = token.upper === "FROM" ? "from" : "join";
      index = readTables(tokens, index + 1, tables, here) - 1;
      continue;
    }
    const found = CLAUSE_BY_WORD[token.upper];
    if (found) clause = found;
  }

  return { clause, tables: tables.filter((table) => table.depth <= depth), ctes, target };
}

function readTarget(tokens: Token[], start: number, depth: number): { table: ScopeTable | null; next: number } {
  const first = tokens[start];
  if (!first?.word || RESERVED.has(first.upper)) return { table: null, next: start };
  let schema: string | null = null;
  let name = first.text;
  let next = start + 1;
  if (tokens[next]?.text === ".") {
    const second = tokens[next + 1];
    if (second?.word) {
      schema = first.text;
      name = second.text;
      next += 2;
    }
  }
  return { table: { alias: name, schema, name, depth }, next };
}

function readCtes(tokens: Token[], start: number, ctes: string[]): number {
  let index = start;
  for (;;) {
    const name = tokens[index];
    if (!name || !name.word || RESERVED.has(name.upper)) return index;
    const next = tokens[index + 1];
    if (!next || next.upper !== "AS") return index;
    ctes.push(name.text);
    index += 2;
    // 跳过 AS 后的括号体
    if (tokens[index]?.text === "(") {
      let depth = 0;
      for (; index < tokens.length; index += 1) {
        if (tokens[index]!.text === "(") depth += 1;
        if (tokens[index]!.text === ")") {
          depth -= 1;
          if (depth === 0) {
            index += 1;
            break;
          }
        }
      }
    }
    if (tokens[index]?.text === ",") {
      index += 1;
      continue;
    }
    return index;
  }
}

function readTables(tokens: Token[], start: number, tables: ScopeTable[], depth: number): number {
  let index = start;
  for (;;) {
    const first = tokens[index];
    if (first && !first.word && first.text === "(") {
      let depth = 0;
      for (; index < tokens.length; index += 1) {
        if (tokens[index]!.text === "(") depth += 1;
        if (tokens[index]!.text === ")") {
          depth -= 1;
          if (depth === 0) {
            index += 1;
            break;
          }
        }
      }
      const derived = tokens[index];
      if (derived?.word) {
        tables.push({ alias: derived.text, schema: null, name: derived.text, depth });
        index += 1;
      }
      const comma = tokens[index];
      if (comma && !comma.word && comma.text === ",") {
        index += 1;
        continue;
      }
      return index;
    }
    if (!first || !first.word) return index;
    let schema: string | null = null;
    let name = first.text;
    let next = index + 1;
    if (tokens[next]?.text === ".") {
      const second = tokens[next + 1];
      if (second?.word) {
        schema = first.text;
        name = second.text;
        next += 2;
      }
    }
    let alias: string | null = null;
    const after = tokens[next];
    if (after?.word) {
      if (after.upper === "AS") {
        const named = tokens[next + 1];
        if (named?.word) {
          alias = named.text;
          next += 2;
        }
      } else if (!RESERVED.has(after.upper)) {
        alias = after.text;
        next += 1;
      }
    }
    tables.push({ alias: alias ?? name, schema, name, depth });
    const separator = tokens[next];
    if (separator && !separator.word && separator.text === ",") {
      index = next + 1;
      continue;
    }
    return next;
  }
}
