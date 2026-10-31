import { invoke } from "@tauri-apps/api/core";

/** SCAN 一页键：nextCursor 为 0 表示本轮遍历完成。 */
export interface RedisKeyInfo {
  name: string;
  keyType: string;
  /** PTTL 毫秒：-1=永不过期，-2=键已不存在。 */
  ttlMs: number | null;
}

export interface ScanPage {
  cursor: number;
  done: boolean;
  keys: RedisKeyInfo[];
}

export type RedisValuePayload =
  | { kind: "string"; text: string }
  | { kind: "hash"; entries: { field: string; value: string }[] }
  | { kind: "list"; items: string[] }
  | { kind: "set"; items: string[] }
  | { kind: "zset"; entries: { member: string; score: string }[] }
  | { kind: "stream"; messages: { id: string; fields: { field: string; value: string }[] }[] }
  | { kind: "none" };

export interface RedisKeyDetail {
  keyType: string;
  ttlMs: number | null;
  value: RedisValuePayload;
}

export interface RespReply {
  kind: "status" | "int" | "bulk" | "array" | "nil";
  text: string | null;
  int: number | null;
  items: RespReply[] | null;
}

export interface ExecOutcome {
  command: string;
  reply: RespReply;
}

function failureText(error: unknown): string {
  if (typeof error === "string" && error.trim()) return error;
  if (error instanceof Error && error.message) return error.message;
  if (error && typeof error === "object" && "message" in error) {
    const message = (error as { message?: unknown }).message;
    if (typeof message === "string" && message.trim()) return message;
  }
  return "操作失败，原因未知。";
}

export interface RedisApi {
  scanKeys(connectionId: string, cursor: number, pattern: string | null, count?: number): Promise<ScanPage>;
  readKey(connectionId: string, key: string): Promise<RedisKeyDetail>;
  setTtl(connectionId: string, key: string, seconds: number | null): Promise<{ applied: number }>;
  deleteKeys(connectionId: string, keys: string[], confirmed: boolean): Promise<{ deleted: number }>;
  renameKey(connectionId: string, from: string, to: string, confirmed: boolean): Promise<{ renamed: boolean }>;
  exec(connectionId: string, command: string, args: string[], confirmed: boolean): Promise<ExecOutcome>;
}

export function failureOf(error: unknown): string {
  return failureText(error);
}

export const tauriRedisApi: RedisApi = {
  scanKeys: (connectionId, cursor, pattern, count) =>
    invoke<ScanPage>("redis_scan_keys", { connectionId, cursor, pattern, count: count ?? null }),
  readKey: (connectionId, key) => invoke<RedisKeyDetail>("redis_read_key", { connectionId, key }),
  setTtl: (connectionId, key, seconds) => invoke<{ applied: number }>("redis_set_ttl", { connectionId, key, seconds }),
  deleteKeys: (connectionId, keys, confirmed) => invoke<{ deleted: number }>("redis_delete_keys", { connectionId, keys, confirmed }),
  renameKey: (connectionId, from, to, confirmed) => invoke<{ renamed: boolean }>("redis_rename_key", { connectionId, from, to, confirmed }),
  exec: (connectionId, command, args, confirmed) => invoke<ExecOutcome>("redis_exec", { connectionId, command, args, confirmed }),
};
