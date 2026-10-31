/** Redis 命令风险分类（C2/R11）：写删类执行前前端二次确认；只读类直通；KEYS 无条件禁用。
 * 清单与后端 src-tauri/src/redis/exec.rs 的 BLOCKED/DANGEROUS 保持一致（后端同样强制）。 */

export type CommandRisk = "blocked" | "dangerous" | "readonly";

/** 无条件禁用：KEYS 全量扫描会阻塞服务端，键空间一律走 SCAN 分页。 */
export const BLOCKED_COMMANDS: readonly string[] = ["KEYS"];

/** 写删类命令：执行前弹确认对话框，取消则不发任何命令。 */
export const DANGEROUS_COMMANDS: readonly string[] = [
  // 键删除/清库
  "DEL", "UNLINK", "FLUSHDB", "FLUSHALL", "FLUSH", "SWAPDB", "SHUTDOWN",
  // 键改名/移动/覆盖
  "RENAME", "RENAMENX", "COPY", "MOVE", "MIGRATE", "RESTORE", "SORT", "GETDEL",
  // 过期
  "EXPIRE", "PEXPIRE", "EXPIREAT", "PEXPIREAT", "EXPIRETIME", "PERSIST",
  // string 写入
  "SET", "SETNX", "SETEX", "PSETEX", "SETRANGE", "MSET", "MSETNX", "GETSET", "APPEND",
  "INCR", "INCRBY", "INCRBYFLOAT", "DECR", "DECRBY",
  // hash 写入
  "HSET", "HMSET", "HSETNX", "HDEL", "HINCRBY", "HINCRBYFLOAT",
  // list 写入
  "LPUSH", "RPUSH", "LPUSHX", "RPUSHX", "LPOP", "RPOP", "LREM", "LSET", "LINSERT", "LTRIM", "RPOPLPUSH", "LMOVE", "LMPOP", "BLPOP", "BRPOP",
  // set 写入
  "SADD", "SPOP", "SREM", "SMOVE", "SINTERSTORE", "SUNIONSTORE", "SDIFFSTORE", "SINTERCARD",
  // zset 写入
  "ZADD", "ZINCRBY", "ZREM", "ZPOPMIN", "ZPOPMAX", "ZREMRANGEBYRANK", "ZREMRANGEBYSCORE", "ZREMRANGEBYLEX", "ZMPOP", "BZPOPMIN", "BZPOPMAX",
  // stream 写入
  "XADD", "XDEL", "XTRIM", "XSETID",
  // 位图/几何/其它写入
  "SETBIT", "BITFIELD", "GEOADD", "GEOSEARCHSTORE", "PFADD", "PFMERGE",
  // 管理类
  "CONFIG_SET", "ACL_SETUSER", "ACL_DELUSER", "ACL_SAVE", "SCRIPT_FLUSH", "FUNCTION_FLUSH", "FUNCTION_DELETE", "FUNCTION_RESTORE", "CLIENT_KILL", "DEBUG",
  // 事务/订阅会改变会话状态
  "MULTI", "EXEC", "DISCARD", "WATCH", "UNWATCH", "SELECT",
];

/** 命令行输入（如 `SET key value`）的首词风险分类。 */
export function classifyRedisCommand(line: string): CommandRisk {
  const word = line.trim().split(/\s+/)[0] ?? "";
  const key = word.toUpperCase().replace(/[\s-]/g, "_");
  if (!key) return "readonly";
  if (BLOCKED_COMMANDS.includes(key)) return "blocked";
  if (DANGEROUS_COMMANDS.includes(key)) return "dangerous";
  return "readonly";
}

/** 把命令行拆成首词 + 参数（引号段保留为单参数；shell 风格的最小实现）。 */
export function splitCommandline(line: string): { command: string; args: string[] } {
  const tokens: string[] = [];
  const pattern = /"([^"]*)"|'([^']*)'|(\S+)/g;
  for (const match of line.trim().matchAll(pattern)) {
    tokens.push(match[1] ?? match[2] ?? match[3] ?? "");
  }
  const [command = "", ...args] = tokens;
  return { command, args };
}

/** 危险命令确认对话框的提示文案。 */
export function dangerPrompt(line: string): string {
  const word = splitCommandline(line).command.toUpperCase();
  if (word === "FLUSHDB") return "FLUSHDB 会清空当前库的全部键，且不可撤销。确认执行？";
  if (word === "FLUSHALL") return "FLUSHALL 会清空这台服务器的全部库，且不可撤销。确认执行？";
  if (word === "SHUTDOWN") return "SHUTDOWN 会直接关闭 Redis 服务器。确认执行？";
  return `${word} 属于写删类命令，可能修改或删除数据。确认执行？`;
}
