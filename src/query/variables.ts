/** 执行前变量面板：扫描待发送 SQL 里的占位符，并把用户输入代换成转义字面量。纯前端，不触库。 */

export interface VariableInput {
  /** 表单键：命名占位符为名称本身，位置占位符为 "?" 加 1 起的序号。 */
  key: string;
  /** 展示标签：命名占位符为 ":name"，位置占位符为 "?1" 这类序号。 */
  label: string;
}

interface VariableSpot {
  key: string;
  start: number;
  end: number;
}

const IDENT_CHAR = /[A-Za-z0-9_]/;

/**
 * 收集 SQL 里的占位符：`:name` 命名占位符（同名只报一次，按首次出现排序）与 `?` 位置占位符（按出现顺序编号）。
 * `'...'` 字符串字面量、`--` 与 `/* ... *\/` 注释、反引号/双引号标识符、dollar-quote 里的内容不识别；`::` 强制转换不算。
 */
export function findVariables(sql: string): VariableInput[] {
  const inputs: VariableInput[] = [];
  const seen = new Set<string>();
  for (const spot of scanSpots(sql)) {
    if (seen.has(spot.key)) continue;
    seen.add(spot.key);
    inputs.push({ key: spot.key, label: spot.key.startsWith("?") ? spot.key : `:${spot.key}` });
  }
  return inputs;
}

/**
 * 把 `:name` 的全部出现处与第 i 个 `?` 代换成转义字面量：输入留空代换为 NULL，其余按字符串字面量发送（' 转成 ''）。
 * 与 findVariables 用同一套扫描，跳过的区域不会代换。
 */
export function substituteVariables(sql: string, values: Record<string, string>): string {
  const spots = scanSpots(sql);
  if (!spots.length) return sql;
  let out = "";
  let last = 0;
  for (const spot of spots) {
    out += sql.slice(last, spot.start);
    out += literal(values[spot.key]);
    last = spot.end;
  }
  out += sql.slice(last);
  return out;
}

function literal(value: string | undefined): string {
  if (value == null || value === "") return "NULL";
  return `'${value.replace(/'/g, "''")}'`;
}

function scanSpots(sql: string): VariableSpot[] {
  const spots: VariableSpot[] = [];
  let questionIndex = 0;
  let i = 0;
  let quote: string | null = null;
  let dollar: string | null = null;
  let comment = false;
  while (i < sql.length) {
    if (dollar !== null) {
      if (sql.startsWith(`$${dollar}$`, i)) {
        i += dollar.length + 2;
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
      i = eolAt(sql, i + 2);
      continue;
    }
    if (sql.startsWith("/*", i)) {
      comment = true;
      i += 2;
      continue;
    }
    const ch = sql[i] ?? "";
    if (ch === "'" || ch === '"' || ch === "`") {
      quote = ch;
      i += 1;
      continue;
    }
    if (ch === "$") {
      const tag = dollarTag(sql, i);
      if (tag !== null) {
        dollar = tag;
        i += tag.length + 2;
        continue;
      }
    }
    if (ch === ":") {
      if (sql[i + 1] === ":") {
        i += 2;
        continue;
      }
      let end = i + 1;
      while (end < sql.length && IDENT_CHAR.test(sql[end] ?? "")) end += 1;
      if (end > i + 1) {
        spots.push({ key: sql.slice(i + 1, end), start: i, end });
        i = end;
        continue;
      }
      i += 1;
      continue;
    }
    if (ch === "?") {
      questionIndex += 1;
      spots.push({ key: `?${questionIndex}`, start: i, end: i + 1 });
      i += 1;
      continue;
    }
    i += 1;
  }
  return spots;
}

function dollarTag(sql: string, start: number): string | null {
  const end = sql.indexOf("$", start + 1);
  if (end < 0) return null;
  const tag = sql.slice(start + 1, end);
  return /^[A-Za-z0-9_]*$/.test(tag) ? tag : null;
}

function eolAt(sql: string, index: number): number {
  let i = index;
  while (i < sql.length && sql[i] !== "\n" && sql[i] !== "\r") i += 1;
  return i;
}
