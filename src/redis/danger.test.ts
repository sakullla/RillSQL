import { describe, expect, it } from "vitest";
import { BLOCKED_COMMANDS, DANGEROUS_COMMANDS, classifyRedisCommand, dangerPrompt, splitCommandline } from "./danger";

describe("redis danger classification (C2)", () => {
  it("blocks KEYS outright regardless of arguments", () => {
    expect(classifyRedisCommand("KEYS *")).toBe("blocked");
    expect(classifyRedisCommand("keys rillsql:*")).toBe("blocked");
  });

  it("marks destructive and write commands as dangerous", () => {
    for (const line of [
      "FLUSHDB",
      "FLUSHALL",
      "DEL rillsql:string:welcome",
      "UNLINK a b c",
      "RENAME a b",
      "EXPIRE key 60",
      "PERSIST key",
      "SET key value",
      "HSET hash field value",
      "LPOP queue",
      "RPOP queue",
      "SPOP tags",
      "ZREM rank member",
      "SHUTDOWN",
    ]) {
      expect(classifyRedisCommand(line), line).toBe("dangerous");
    }
  });

  it("lets read-only commands straight through", () => {
    for (const line of ["GET key", "HGETALL hash", "LRANGE q 0 -1", "SMEMBERS tags", "ZRANGE rank 0 -1 WITHSCORES", "XRANGE s - +", "SCAN 0 MATCH user:* COUNT 100", "TYPE key", "TTL key", "PTTL key", "DBSIZE", "INFO server", "PING"]) {
      expect(classifyRedisCommand(line), line).toBe("readonly");
    }
  });

  it("keeps the frontend list aligned with the required minimum from R11", () => {
    // R11 验收点名的那批写删命令必须出现在常量清单里（后端 src-tauri/src/redis/exec.rs 同清单）。
    for (const word of ["DEL", "UNLINK", "FLUSHDB", "FLUSHALL", "RENAME", "EXPIRE", "PERSIST", "SET", "HSET", "LPOP", "RPOP", "SPOP", "ZREM"]) {
      expect(DANGEROUS_COMMANDS).toContain(word);
    }
    expect(BLOCKED_COMMANDS).toEqual(["KEYS"]);
  });

  it("splits a command line respecting quoted arguments", () => {
    expect(splitCommandline("SET greeting 你好，世界")).toEqual({ command: "SET", args: ["greeting", "你好，世界"] });
    expect(splitCommandline('SET k "值 里 有 空 格"')).toEqual({ command: "SET", args: ["k", "值 里 有 空 格"] });
    expect(splitCommandline("  GET   key  ")).toEqual({ command: "GET", args: ["key"] });
    expect(splitCommandline("")).toEqual({ command: "", args: [] });
  });

  it("writes explicit prompts for the most destructive commands", () => {
    expect(dangerPrompt("FLUSHDB")).toContain("清空当前库");
    expect(dangerPrompt("FLUSHALL SYNC")).toContain("全部库");
    expect(dangerPrompt("SHUTDOWN")).toContain("关闭 Redis 服务器");
    expect(dangerPrompt("DEL a b")).toContain("写删类命令");
  });
});
