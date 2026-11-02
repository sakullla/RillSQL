/** 0：前缀命中；1：子序列命中；-1：不命中。大小写不敏感。 */
export function matchKind(label: string, prefix: string): number {
  if (!prefix) return 0;
  const haystack = label.toLowerCase();
  const needle = prefix.toLowerCase();
  if (haystack.startsWith(needle)) return 0;
  let index = 0;
  for (const ch of haystack) {
    if (ch === needle[index]) index += 1;
    if (index === needle.length) return 1;
  }
  return -1;
}

export interface Ranked<T> {
  score: number;
  item: T;
}

/** 两种匹配分别收集，保持同分顺序；满额前缀命中后无需再扫描或排序。 */
export function rankBy<T>(items: Iterable<T>, prefix: string, label: (item: T) => string, cap = Infinity): T[] {
  if (cap <= 0) return [];
  const exact: T[] = [];
  const fuzzy: T[] = [];
  for (const item of items) {
    const match = matchKind(label(item), prefix);
    if (match === 0) {
      exact.push(item);
      if (exact.length >= cap) break;
    } else if (match === 1 && fuzzy.length < cap) {
      fuzzy.push(item);
    }
  }
  return exact.concat(fuzzy.slice(0, cap - exact.length));
}
