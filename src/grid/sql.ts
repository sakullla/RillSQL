import { inspectSql } from "../query/split";
import type { Column, Engine, Filter, SortSpec, TableSchema } from "./types";
import { opNeedsValue } from "./types";
import { binaryLiteral, boolLiteral, numericLiteral, parseHex, valueFamily } from "./typeset";

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

export function sqlCell(column: Column | undefined, value: string | null, engine: Engine = "postgres"): string {
  if (value === null) return "NULL";
  if (!column) return quoteLiteral(value);
  const family = valueFamily(column.typeName);
  if (family === "bool") return boolLiteral(value) ?? quoteLiteral(value);
  if (family === "binary") {
    const hex = parseHex(value);
    return hex == null ? quoteLiteral(value) : binaryLiteral(engine, hex);
  }
  if (isNumericType(column.typeName) && numericLiteral(value)) return value;
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
      return `${quoteIdent(schema.engine, name)} = ${sqlCell(column, values[index] ?? null, schema.engine)}`;
    })
    .join(" AND ");
}

export function updateSql(schema: TableSchema, sets: Record<string, string | null>, locatorColumns: readonly string[], locatorValues: readonly (string | null)[]): string {
  const assignments = Object.entries(sets).map(([name, value]) => {
    const column = schema.columns.find((item) => item.name === name);
    return `${quoteIdent(schema.engine, name)} = ${sqlCell(column, value, schema.engine)}`;
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
  const literals = names.map((name) => sqlCell(schema.columns.find((column) => column.name === name), values[name] ?? null, schema.engine)).join(", ");
  return `INSERT INTO ${qualifiedName(schema)} (${list}) VALUES (${literals})`;
}

export interface SelectGate {
  ok: boolean;
  reason: string | null;
  statement: string | null;
}

/** 去掉注释后只接受单条 SELECT。WITH、多语句和非 SELECT 都不能完整导出。扫描交给 query splitSql。 */
export function selectExportGate(sql: string, engine = "postgres"): SelectGate {
  const report = inspectSql(sql, engine);
  if (report.refusal) return { ok: false, reason: report.refusal, statement: null };
  const statements = report.pieces.map((piece) => piece.text.trim()).filter(Boolean);
  if (!statements.length) return { ok: false, reason: "没有可导出的语句", statement: null };
  if (statements.length > 1) return { ok: false, reason: "完整导出只接受单条 SELECT 语句", statement: null };
  const statement = statements[0]!;
  const keyword = /^[A-Za-z]+/.exec(statement)?.[0]?.toUpperCase() ?? "";
  if (keyword !== "SELECT") return { ok: false, reason: "只有 SELECT 可以重新执行并完整导出", statement };
  return { ok: true, reason: null, statement };
}

/** 语句文本来自 query-window 的扫描，不再在网格里保留一份词法。 */
export function splitSql(sql: string, engine = "postgres"): string[] {
  return inspectSql(sql, engine).pieces.map((piece) => piece.text).filter((text) => text.trim().length > 0);
}
