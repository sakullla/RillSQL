import { describe, expect, it } from "vitest";
import type { RedisKeyInfo, ScanPage } from "./api";
import { describeTtl, keyTypeBadge, mergeScanPage, parseTtlInput, scanRestart } from "./keys";

const key = (name: string, keyType = "string", ttlMs: number | null = -1): RedisKeyInfo => ({ name, keyType, ttlMs });
const page = (cursor: number, keys: RedisKeyInfo[]): ScanPage => ({ cursor, done: cursor === 0, keys });

describe("redis scan pagination logic", () => {
  it("merges scan pages and keeps the freshest duplicate", () => {
    const first = mergeScanPage([], page(17, [key("a"), key("b")]));
    expect(first).toEqual({ keys: [key("a"), key("b")], cursor: 17, done: false });

    // SCAN 游标遍历期间键空间可能变化：同一键再次出现时保留最新一条（TTL 已变）。
    const second = mergeScanPage(first.keys, page(0, [key("b", "string", 5000)]));
    expect(second.done).toBe(true);
    expect(second.keys.map((item) => item.name)).toEqual(["a", "b"]);
    expect(second.keys[1]!.ttlMs).toBe(5000);
  });

  it("restarts the cursor from zero when the pattern changes", () => {
    expect(scanRestart("rillsql:*")).toEqual({ cursor: 0, pattern: "rillsql:*" });
    // 空白模式等价于不过滤（后端 MATCH 不下发）。
    expect(scanRestart("   ")).toEqual({ cursor: 0, pattern: null });
    expect(scanRestart(null)).toEqual({ cursor: 0, pattern: null });
  });

  it("describes pttl values for humans", () => {
    expect(describeTtl(-1)).toBe("永不过期");
    expect(describeTtl(-2)).toBe("键已不存在");
    expect(describeTtl(500)).toBe("500 毫秒");
    expect(describeTtl(59_000)).toBe("59 秒");
    expect(describeTtl(125_000)).toBe("2 分 5 秒");
    expect(describeTtl(3_600_000)).toBe("1 时 0 分");
    expect(describeTtl(90_000_000)).toBe("1 天 1 时");
    expect(describeTtl(null)).toBe("—");
  });

  it("parses ttl input with empty meaning persist", () => {
    expect(parseTtlInput("3600")).toEqual({ seconds: 3600 });
    expect(parseTtlInput(" 0 ")).toEqual({ seconds: 0 });
    expect(parseTtlInput("")).toEqual({ seconds: null });
    expect(parseTtlInput("abc")).toEqual({ error: "TTL 秒数应为非负整数（留空表示永不过期）。" });
    expect(parseTtlInput("-5")).toEqual({ error: "TTL 秒数应为非负整数（留空表示永不过期）。" });
  });

  it("maps key types to badges with unknown types degraded", () => {
    for (const kind of ["string", "hash", "list", "set", "zset", "stream"]) {
      expect(keyTypeBadge(kind)).toBe(kind);
    }
    expect(keyTypeBadge("module")).toBe("module");
    expect(keyTypeBadge("")).toBe("none");
  });
});
