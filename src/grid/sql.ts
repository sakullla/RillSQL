import type { Column, Engine, Filter, SortSpec, TableSchema } from "./types";
import { opNeedsValue } from "./types";

export function isNumericType(typeName: string): boolean {
  return /int|numeric|decimal|real|double|float|serial|number/i.test(typeName);
}

export function quoteIdent(engine: Engine, name: string): string {
  const quote = engine === "mysql" || engine === "mariadb" ? "`" : '"';
  return `${quote}${name.split(quote).join(quote + quote)}${quote}`;
}

export function quoteLiteral(value: string): string {
  return `'${value.replaceAll("'", "''")}'`;
}

export function qualifiedName(schema: TableSchema): string {
  const table = quoteIdent(schema.engine, schema.name);
  if (!schema.schema) return table;
  return `${quoteIdent(schema.engine, schema.schema)}.${table}`;
}

export function sqlValue(column: Column, value: string): string {
  if (isNumericType(column.typeName) && /^-?\d+(\.\d+)?$/.test(value)) return value;
  return quoteLiteral(value);
}

export function sqlCell(column: Column | undefined, value: string | null): string {
  if (value === null) return "NULL";
  if (column && isNumericType(column.typeName) && /^-?\d+(\.\d+)?$/.test(value)) return value;
  return quoteLiteral(value);
}

function likeLiteral(value: string, mode: "contains" | "starts"): string {
  const escaped = value.replaceAll("!", "!!").replaceAll("%", "!%").replaceAll("_", "!_").replaceAll("'", "''");
  const pattern = mode === "contains" ? `%${escaped}%` : `${escaped}%`;
  return `'${pattern}'`;
}

export function filterClause(engine: Engine, column: Column, filter: Filter): string {
  const ident = quoteIdent(engine, column.name);
  const value = filter.value ?? "";
  switch (filter.op) {
    case "eq":
      return `${ident} = ${sqlValue(column, value)}`;
    case "ne":
      return `${ident} <> ${sqlValue(column, value)}`;
    case "contains":
      return `${ident} ${engine === "postgres" ? "ILIKE" : "LIKE"} ${likeLiteral(value, "contains")} ESCAPE '!'`;
    case "starts":
      return `${ident} LIKE ${likeLiteral(value, "starts")} ESCAPE '!'`;
    case "gt":
      return `${ident} > ${sqlValue(column, value)}`;
    case "lt":
      return `${ident} < ${sqlValue(column, value)}`;
    case "null":
      return `${ident} IS NULL`;
    case "notnull":
      return `${ident} IS NOT NULL`;
    case "empty":
      return `${ident} = ''`;
    default: {
      const unreachable: never = filter.op;
      return unreachable;
    }
  }
}

export function compileWhere(schema: TableSchema, filters: readonly Filter[]): { ok: true; sql: string } | { ok: false; error: string } {
  const parts: string[] = [];
  for (const filter of filters) {
    const column = schema.columns.find((item) => item.name === filter.column);
    if (!column) return { ok: false, error: `未知列 ${filter.column}，不能拼接手写条件` };
    if (!opNeedsValue(filter.op)) {
      parts.push(filterClause(schema.engine, column, filter));
      continue;
    }
    if (filter.value === undefined || filter.value === "") return { ok: false, error: "筛选值不能为空" };
    parts.push(filterClause(schema.engine, column, filter));
  }
  return { ok: true, sql: parts.join(" AND ") };
}

export function browseSql(schema: TableSchema, filters: readonly Filter[], sort: SortSpec | null, limit: number, offset: number): string {
  const where = compileWhere(schema, filters);
  const clause = where.ok && where.sql ? ` WHERE ${where.sql}` : "";
  const order = sort ? ` ORDER BY ${quoteIdent(schema.engine, sort.column)} ${sort.direction === "desc" ? "DESC" : "ASC"}` : "";
  const tail = offset > 0 ? ` LIMIT ${limit} OFFSET ${offset}` : ` LIMIT ${limit}`;
  return `SELECT * FROM ${qualifiedName(schema)}${clause}${order}${tail}`;
}

export function whereLocator(schema: TableSchema, columns: readonly string[], values: readonly (string | null)[]): string {
  return columns
    .map((name, index) => {
      const column = schema.columns.find((item) => item.name === name);
      return `${quoteIdent(schema.engine, name)} = ${sqlCell(column, values[index] ?? null)}`;
    })
    .join(" AND ");
}

export function updateSql(schema: TableSchema, sets: Record<string, string | null>, locatorColumns: readonly string[], locatorValues: readonly (string | null)[]): string {
  const assignments = Object.entries(sets).map(([name, value]) => {
    const column = schema.columns.find((item) => item.name === name);
    return `${quoteIdent(schema.engine, name)} = ${sqlCell(column, value)}`;
  });
  return `UPDATE ${qualifiedName(schema)} SET ${assignments.join(", ")} WHERE ${whereLocator(schema, locatorColumns, locatorValues)}`;
}

export function deleteSql(schema: TableSchema, locatorColumns: readonly string[], locatorValues: readonly (string | null)[]): string {
  return `DELETE FROM ${qualifiedName(schema)} WHERE ${whereLocator(schema, locatorColumns, locatorValues)}`;
}

export function insertSql(schema: TableSchema, values: Record<string, string | null | undefined>): string {
  const names = schema.columns.filter((column) => values[column.name] !== undefined).map((column) => column.name);
  if (!names.length) {
    if (schema.engine === "mysql" || schema.engine === "mariadb") return `INSERT INTO ${qualifiedName(schema)} () VALUES ()`;
    return `INSERT INTO ${qualifiedName(schema)} DEFAULT VALUES`;
  }
  const list = names.map((name) => quoteIdent(schema.engine, name)).join(", ");
  const literals = names.map((name) => sqlCell(schema.columns.find((column) => column.name === name), values[name] ?? null)).join(", ");
  return `INSERT INTO ${qualifiedName(schema)} (${list}) VALUES (${literals})`;
}

export interface SelectGate {
  ok: boolean;
  reason: string | null;
  statement: string | null;
}

/** 去掉注释后只接受单条 SELECT。WITH、多语句和非 SELECT 都不能全量导出。 */
export function selectExportGate(sql: string): SelectGate {
  const statements = splitSql(sql).map((item) => item.trim()).filter(Boolean);
  if (!statements.length) return { ok: false, reason: "没有可导出的语句", statement: null };
  if (statements.length > 1) return { ok: false, reason: "全量导出只接受单条 SELECT", statement: null };
  const statement = statements[0]!;
  const keyword = /^[A-Za-z]+/.exec(statement)?.[0]?.toUpperCase() ?? "";
  if (keyword !== "SELECT") return { ok: false, reason: "只有 SELECT 可以重新执行并全量导出", statement };
  return { ok: true, reason: null, statement };
}

export function splitSql(sql: string): string[] {
  const statements: string[] = [];
  let current = "";
  let mode: "normal" | "line" | "block" | "single" | "double" | "tick" = "normal";
  for (let index = 0; index < sql.length; index += 1) {
    const char = sql[index]!;
    const next = sql[index + 1] ?? "";
    if (mode === "line") {
      if (char === "\n") mode = "normal";
      continue;
    }
    if (mode === "block") {
      if (char === "*" && next === "/") {
        mode = "normal";
        index += 1;
      }
      continue;
    }
    if (mode === "single") {
      current += char;
      if (char === "'" && next === "'") {
        current += next;
        index += 1;
      } else if (char === "'") mode = "normal";
      continue;
    }
    if (mode === "double") {
      current += char;
      if (char === '"' && next === '"') {
        current += next;
        index += 1;
      } else if (char === '"') mode = "normal";
      continue;
    }
    if (mode === "tick") {
      current += char;
      if (char === "`" && next === "`") {
        current += next;
        index += 1;
      } else if (char === "`") mode = "normal";
      continue;
    }
    if (char === "-" && next === "-") {
      mode = "line";
      index += 1;
      continue;
    }
    if (char === "/" && next === "*") {
      mode = "block";
      index += 1;
      continue;
    }
    if (char === "'") {
      mode = "single";
      current += char;
      continue;
    }
    if (char === '"') {
      mode = "double";
      current += char;
      continue;
    }
    if (char === "`") {
      mode = "tick";
      current += char;
      continue;
    }
    if (char === ";") {
      statements.push(current);
      current = "";
      continue;
    }
    current += char;
  }
  if (current.trim()) statements.push(current);
  return statements;
}
