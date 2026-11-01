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

/** 分数越小越靠前；同分保持传入顺序，再由调用方按字典序收尾。 */
export function rankBy<T>(items: readonly T[], prefix: string, label: (item: T) => string): T[] {
  const scored: Ranked<T>[] = [];
  items.forEach((item, order) => {
    const match = matchKind(label(item), prefix);
    if (match < 0) return;
    scored.push({ score: match * 1000 + order, item });
  });
  return scored.sort((a, b) => a.score - b.score).map((entry) => entry.item);
}
