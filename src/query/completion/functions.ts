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

/** 三引擎共用的函数清单来自手写签名表；未收录的方言函数不猜。 */
export function functionCatalog(): string[] {
  return [...KNOWN_FUNCTIONS];
}

export function functionDetail(engine: Engine, name: string): string {
  const signature = signatureFor(name);
  if (signature) return signature;
  return engine === "sqlite" ? tr("query.complete.functionBuiltinSqlite") : tr("query.complete.functionBuiltin");
}
