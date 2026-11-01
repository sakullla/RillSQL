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
  jsonb_extract_path_text: "jsonb_extract_path_text(json, …) → text",
  group_concat: "group_concat(expr, sep) → text",
  string_agg: "string_agg(expr, sep) → text",
  row_number: "row_number() over (…) → bigint",
  rank: "rank() over (…) → bigint",
  lag: "lag(expr, offset) over (…) → value",
  lead: "lead(expr, offset) over (…) → value",
};

/** 方言 builtin 之外的常用函数名，用于补齐未出现在 builtin 列表里的引擎（如 PostgreSQL）。 */
export const KNOWN_FUNCTIONS: readonly string[] = Object.keys(SIGNATURES);

export function signatureFor(name: string): string | null {
  return SIGNATURES[name.toLowerCase()] ?? null;
}

/** 三引擎共用的函数清单来自方言 builtin；这里只补签名与去重。 */
export function functionDetail(engine: Engine, name: string): string {
  const signature = signatureFor(name);
  if (signature) return signature;
  return engine === "sqlite" ? tr("query.complete.functionBuiltinSqlite") : tr("query.complete.functionBuiltin");
}
