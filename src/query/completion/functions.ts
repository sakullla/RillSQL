import { tr } from "../../i18n";
import type { Engine } from "../../engines";

/** 常用内建函数的签名摘要；未收录的函数仍作为候选出现，只是不带签名。 */
const SIGNATURES: Record<string, string> = {
  count: "count(expr) → bigint",
  sum: "sum(expr) → numeric",
  avg: "avg(expr) → numeric",
  min: "min(expr) → same",
  max: "max(expr) → same",
  coalesce: "coalesce(v1, v2, …) → value",
  nullif: "nullif(a, b) → value",
  concat: "concat(s1, s2, …) → text",
  length: "length(text) → int",
  lower: "lower(text) → text",
  upper: "upper(text) → text",
  substring: "substring(text, start, len) → text",
  trim: "trim(text) → text",
  replace: "replace(text, from, to) → text",
  round: "round(value, digits) → numeric",
  floor: "floor(value) → numeric",
  ceil: "ceil(value) → numeric",
  abs: "abs(value) → numeric",
  now: "now() → timestamp",
  current_date: "current_date → date",
  date_trunc: "date_trunc(unit, value) → timestamp",
  extract: "extract(field from value) → numeric",
  cast: "cast(expr as type) → type",
  json_extract: "json_extract(json, path) → value",
  json_extract_path_text: "json_extract_path_text(json, path) → text",
  group_concat: "group_concat(expr, sep) → text",
  string_agg: "string_agg(expr, sep) → text",
  row_number: "row_number() over (…) → bigint",
  rank: "rank() over (…) → bigint",
  lag: "lag(expr, offset) over (…) → value",
  lead: "lead(expr, offset) over (…) → value",
  to_char: "to_char(value, format) → text",
  date_part: "date_part(field, value) → double",
  generate_series: "generate_series(from, to, step) → set",
  array_agg: "array_agg(expr) → array",
  jsonb_build_object: "jsonb_build_object(key, value, …) → jsonb",
  jsonb_agg: "jsonb_agg(expr) → jsonb",
  make_interval: "make_interval(days, hours, …) → interval",
  strftime: "strftime(format, time, modifier) → text",
  datetime: "datetime(time, modifier) → text",
  unixepoch: "unixepoch(time) → integer",
};

export const KNOWN_FUNCTIONS: readonly string[] = Object.keys(SIGNATURES);

export function signatureFor(name: string): string | null {
  return SIGNATURES[name.toLowerCase()] ?? null;
}

const BASE_FUNCTIONS = [
  "count", "sum", "avg", "min", "max", "coalesce", "nullif", "concat", "length", "lower", "upper",
  "substring", "trim", "replace", "round", "floor", "ceil", "abs", "now", "current_date", "cast",
  "row_number", "rank", "lag", "lead",
];

const PG_ONLY = [
  "string_agg", "date_trunc", "extract", "to_char", "date_part", "generate_series", "array_agg",
  "jsonb_build_object", "jsonb_agg", "jsonb_extract_path_text", "make_interval",
];

const MYSQL_ONLY = ["date_format", "ifnull", "group_concat", "json_extract", "date_add", "datediff"];

const SQLITE_ONLY = ["strftime", "datetime", "unixepoch", "json_extract", "json_group_array"];

/** 函数候选按引擎区分：方言 builtin 噪声太大，只用签名表与各引擎补充清单的并集。 */
export function functionCatalog(engine: Engine): string[] {
  const extra = engine === "sqlite" ? SQLITE_ONLY : engine === "mysql" || engine === "mariadb" ? MYSQL_ONLY : PG_ONLY;
  return [...new Set([...BASE_FUNCTIONS, ...extra, ...KNOWN_FUNCTIONS.filter((name) => BASE_FUNCTIONS.includes(name))])];
}

export function functionDetail(engine: Engine, name: string): string {
  const signature = signatureFor(name);
  if (signature) return signature;
  return engine === "sqlite" ? tr("query.complete.functionBuiltinSqlite") : tr("query.complete.functionBuiltin");
}
