/** 键空间分页的纯逻辑：SCAN 游标推进、模式过滤重置、TTL 展示（与 UI 无关，便于单测）。 */
import type { RedisKeyInfo, ScanPage } from "./api";

/** SCAN 游标在遍历中的键空间会随写入变化：同一键可能重复出现，按名字去重保留最新一条。 */
export function mergeScanPage(loaded: readonly RedisKeyInfo[], page: ScanPage): { keys: RedisKeyInfo[]; cursor: number; done: boolean } {
  const byName = new Map(loaded.map((item) => [item.name, item]));
  for (const item of page.keys) {
    byName.set(item.name, item);
  }
  return {
    keys: [...byName.values()],
    cursor: page.cursor,
    done: page.done,
  };
}

/** 过滤条件变化时游标必须从头开始（MATCH 改变后旧游标无意义）。 */
export function scanRestart(pattern: string | null): { cursor: number; pattern: string | null } {
  const trimmed = pattern?.trim() ?? "";
  return { cursor: 0, pattern: trimmed ? trimmed : null };
}

/** PTTL 毫秒的展示：-1 永不过期；-2 键已不存在；其余换算成可读时长。 */
export function describeTtl(ttlMs: number | null): string {
  if (ttlMs == null) return "—";
  if (ttlMs === -1) return tr("redis.keys.neverExpire");
  if (ttlMs === -2) return tr("redis.keys.missing");
  if (ttlMs < 1000) return tr("redis.keys.ms", { n: ttlMs });
  const seconds = Math.floor(ttlMs / 1000);
  if (seconds < 60) return tr("redis.keys.seconds", { n: seconds });
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return tr("redis.keys.minuteSeconds", { m: minutes, s: seconds % 60 });
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return tr("redis.keys.hourMinutes", { h: hours, m: minutes % 60 });
  return tr("redis.keys.dayHours", { d: Math.floor(hours / 24), h: hours % 24 });
}

/** TTL 编辑输入 → 秒数：空串=移除过期（PERSIST）；非负整数=EXPIRE。 */
export function parseTtlInput(value: string): { seconds: number | null } | { error: string } {
  const trimmed = value.trim();
  if (!trimmed) return { seconds: null };
  if (!/^\d+$/.test(trimmed) || Number(trimmed) > 2_147_483_647) {
    return { error: tr("redis.keys.ttlInvalid") };
  }
  return { seconds: Number(trimmed) };
}

/** 键类型徽标：string/hash/list/set/zset/stream 各一档，未知类型降级为首段。 */
export function keyTypeBadge(keyType: string): string {
  const known = ["string", "hash", "list", "set", "zset", "stream"];
  return known.includes(keyType) ? keyType : keyType || "none";
}

import { tr } from "../i18n";