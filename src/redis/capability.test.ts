import { describe, expect, it } from "vitest";
import { ENGINES, engineMeta } from "../engines";
import { targetOptions } from "../query/complete";
import type { CatalogConnection } from "../catalog/types";

function redisConnection(): CatalogConnection {
  return {
    id: "cache",
    name: "缓存",
    engine: "redis",
    env: "none",
    envLabel: "无标签",
    envTone: "none",
    status: "connected",
    defaultDatabase: "0",
    foreignKeys: null,
    error: null,
    // 后端对 Redis 会话返回空 databases（键空间走 SCAN 专项通路）。
    databases: [],
  };
}

function postgresConnection(): CatalogConnection {
  return {
    id: "shop",
    name: "书店",
    engine: "postgres",
    env: "none",
    envLabel: "无标签",
    envTone: "none",
    status: "connected",
    defaultDatabase: "bookstore",
    foreignKeys: null,
    error: null,
    databases: [{ name: "bookstore", error: null, schemas: [{ name: "public", objects: [], total: 0, error: null }] }],
  };
}

describe("engine capability gating (ADR-2 / R11)", () => {
  it("keeps ENGINES at six entries with redis as the only kv engine", () => {
    expect(ENGINES).toHaveLength(6);
    expect(ENGINES.map((item) => item.id)).toEqual(["postgres", "mysql", "mariadb", "sqlserver", "sqlite", "redis"]);
    expect(engineMeta("redis")).toMatchObject({ relational: false, document: false, kv: true });
    expect(ENGINES.filter((item) => item.kv).map((item) => item.id)).toEqual(["redis"]);
  });

  it("keeps the SQL executor target list free of kv engines", () => {
    const options = targetOptions([redisConnection(), postgresConnection()]);
    // SQL 执行器的目标下拉只列关系引擎；Redis 连接不出现在查询目标里。
    expect(options.map((option) => option.connectionId)).toEqual(["shop"]);
    expect(options[0]!.engine).toBe("postgres");
  });

  it("does not treat an empty-database connection as a query target for redis", () => {
    // targetOptions 对空 databases 有“默认库兜底”逻辑：redis 连接即使有默认库也被能力位排除。
    const withDefault = { ...redisConnection(), defaultDatabase: "2" };
    expect(targetOptions([withDefault])).toEqual([]);
  });
});
