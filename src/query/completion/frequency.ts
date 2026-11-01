import { SQL_KEYWORDS } from "../complete";

/** localStorage 键：只存 标识符 → 次数，不含任何 SQL 文本（R3）。 */
export const FREQUENCY_KEY = "rillsql.completion.frequency";
/** 词频表上限；超出时按次数与名称稳定截断。 */
export const FREQUENCY_LIMIT = 200;

/** 小写标识符 → 执行次数。 */
export type FrequencyMap = Record<string, number>;

const KEYWORDS = new Set<string>(SQL_KEYWORDS.map((keyword) => keyword.toLowerCase()));

function storage(): Storage | null {
  try {
    return globalThis.localStorage ?? null;
  } catch {
    /* 隐私模式或非浏览器环境没有 localStorage：词频只是排序偏好，读不到就当作没有。 */
    return null;
  }
}

/**
 * 抽出可计数的标识符：先去掉注释与字符串字面量，再逐个取名字，SQL 关键字不计入。
 * 点号限定（`o.customer_id`）拆成两段，各自计数。
 */
export function identifiers(sql: string): string[] {
  const stripped = sql.replace(/--[^\n]*/g, " ").replace(/\/\*[\s\S]*?\*\//g, " ");
  const found: string[] = [];
  const pattern = /'(?:[^']|'')*'|"(?:[^"]|"")*"|`(?:[^`]|``)*`|[A-Za-z_][A-Za-z0-9_$]*/g;
  for (const match of stripped.matchAll(pattern)) {
    const token = match[0];
    if (token.startsWith("'")) continue;
    const quoted = token.startsWith('"') || token.startsWith("`");
    const raw = quoted ? token.slice(1, -1) : token;
    const name = quoted ? raw.split(token[0] + token[0]).join(token[0]) : raw;
    if (!name) continue;
    if (KEYWORDS.has(name.toLowerCase())) continue;
    found.push(name);
  }
  return found;
}

/** 只保留正整数计数、键按小写归一，重复键合并。 */
function normalize(raw: unknown): FrequencyMap {
  const next: FrequencyMap = {};
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return next;
  for (const [key, value] of Object.entries(raw as Record<string, unknown>)) {
    if (typeof value !== "number" || !Number.isFinite(value) || value < 1) continue;
    const name = key.trim().toLowerCase();
    if (!name) continue;
    next[name] = (next[name] ?? 0) + Math.floor(value);
  }
  return trim(next);
}

/** 次数降序、同次数按名称升序，取前 FREQUENCY_LIMIT 项。 */
function trim(map: FrequencyMap): FrequencyMap {
  const entries = Object.entries(map);
  if (entries.length <= FREQUENCY_LIMIT) return map;
  const kept: FrequencyMap = {};
  entries
    .sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))
    .slice(0, FREQUENCY_LIMIT)
    .forEach(([key, count]) => { kept[key] = count; });
  return kept;
}

function save(map: FrequencyMap): void {
  try {
    storage()?.setItem(FREQUENCY_KEY, JSON.stringify(map));
  } catch {
    /* 写入失败（配额、隐私模式）不影响补全，只是下次读不到。 */
  }
}

/** 读本地词频；损坏或缺失时返回空表，不含任何 SQL 文本。 */
export function loadFrequency(): FrequencyMap {
  try {
    const raw = storage()?.getItem(FREQUENCY_KEY);
    return raw ? normalize(JSON.parse(raw)) : {};
  } catch {
    return {};
  }
}

/** 把一段已成功执行的 SQL 里的标识符计入本地词频，返回更新后的表。 */
export function bumpIdentifiers(sql: string): FrequencyMap {
  const next = loadFrequency();
  const names = identifiers(sql);
  if (!names.length) return next;
  for (const name of names) {
    const key = name.toLowerCase();
    next[key] = (next[key] ?? 0) + 1;
  }
  const trimmed = trim(next);
  save(trimmed);
  return trimmed;
}

/** 清空本地词频（本地数据重置与测试使用）。 */
export function clearFrequency(): void {
  try {
    storage()?.removeItem(FREQUENCY_KEY);
  } catch {
    /* 清不掉不会让调用方失败。 */
  }
}
