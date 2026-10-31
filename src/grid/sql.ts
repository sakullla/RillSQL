import { inspectSql } from "../query/split";
import type { Column, Engine, Filter, SortSpec, TableSchema } from "./types";
import { opNeedsValue } from "./types";
import { binaryLiteral, boolLiteral, numericLiteral, parseHex, valueFamily, type ValueFamily } from "./typeset";

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
    case "between":
      return `${ident} BETWEEN ${sqlValue(column, value)} AND ${sqlValue(column, filter.second ?? "")}`;
    case "in":
      return `${ident} IN (${(filter.values ?? []).map((item) => sqlValue(column, item)).join(", ")})`;
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

/** 与 Rust plain_cast 镜像：把任意列投影成文本形态，数字/日期等也按显示出来的文本参与匹配。 */
function plainCast(engine: Engine, ident: string): string {
  if (engine === "postgres") return `${ident}::text`;
  if (engine === "mysql" || engine === "mariadb") return `CAST(${ident} AS CHAR)`;
  return `CAST(${ident} AS TEXT)`;
}

/** 可作为文本匹配的列：二进制、几何与类型未知之外的列都参与（R3，Rust 编译层同规则）。 */
function searchableFamily(family: ValueFamily): boolean {
  return family !== "binary" && family !== "geometry" && family !== "unknown";
}

/** 整表关键字搜索子句，与 Rust search_clause 镜像：跨文本可匹配列 OR-LIKE，%/_/! 按 ESCAPE '!' 字面转义；
 * postgres 用 ILIKE、其余引擎 LIKE；没有可参与列时恒假；空关键字返回 null 表示没有搜索。 */
export function searchClause(engine: Engine, columns: readonly Column[], keyword: string): string | null {
  const trimmed = keyword.trim();
  if (!trimmed) return null;
  const op = engine === "postgres" ? "ILIKE" : "LIKE";
  const pattern = likeLiteral(trimmed, "contains");
  const parts = columns
    .filter((column) => searchableFamily(valueFamily(column.typeName)))
    .map((column) => `${plainCast(engine, quoteIdent(engine, column.name))} ${op} ${pattern} ESCAPE '!'`);
  return parts.length ? `(${parts.join(" OR ")})` : "1 = 0";
}

/** compileWhere 的搜索叠加形态：筛选条件与整表关键字 AND 组合，分页与计数共用（与 Rust compile_where_with_search 镜像）。 */
export function compileWhereWithSearch(schema: TableSchema, filters: readonly Filter[], search: string | null): { ok: true; sql: string } | { ok: false; error: string } {
  const where = compileWhere(schema, filters);
  if (!where.ok) return where;
  const clause = search === null ? null : searchClause(schema.engine, schema.columns, search);
  if (!clause) return where;
  return { ok: true, sql: where.sql ? `${where.sql} AND ${clause}` : clause };
}

export function compileWhere(schema: TableSchema, filters: readonly Filter[]): { ok: true; sql: string } | { ok: false; error: string } {
  const parts: string[] = [];
  for (const filter of filters) {
    const column = schema.columns.find((item) => item.name === filter.column);
    if (!column) return { ok: false, error: `未知列 ${filter.column}，不能拼接手写条件` };
    if (filter.op === "between") {
      if (!filter.value || !filter.second) return { ok: false, error: "between 需要两个值" };
      parts.push(filterClause(schema.engine, column, filter));
      continue;
    }
    if (filter.op === "in") {
      if (!filter.values?.length) return { ok: false, error: "IN 筛选至少要一个值" };
      parts.push(filterClause(schema.engine, column, filter));
      continue;
    }
    if (!opNeedsValue(filter.op)) {
      parts.push(filterClause(schema.engine, column, filter));
      continue;
    }
    if (filter.value === undefined || filter.value === "") return { ok: false, error: "筛选值不能为空" };
    parts.push(filterClause(schema.engine, column, filter));
  }
  return { ok: true, sql: parts.join(" AND ") };
}

export function browseSql(schema: TableSchema, filters: readonly Filter[], sort: SortSpec | null, limit: number, offset: number, search: string | null = null): string {
  const where = compileWhereWithSearch(schema, filters, search);
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
