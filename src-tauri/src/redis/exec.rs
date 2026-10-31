//! 命令执行通道：任意命令 + 参数，RESP 值结构化返回（status/int/bulk/array/nil 分类）。
//! 写删类命令必须有 confirmed 标记（前端二次确认后传递）；KEYS 全量扫描无条件禁用。

use serde::Serialize;

/// 命令风险分类：Blocked=无条件禁用；Dangerous=写删类需确认；Readonly=直通。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandRisk {
    Blocked,
    Dangerous,
    Readonly,
}

/// 无条件禁用的命令：KEYS 全量扫描会阻塞服务端，键浏览一律走 SCAN 分页。
pub const BLOCKED_COMMANDS: &[&str] = &["KEYS"];

/// 写删类命令（前端二次确认后才发 confirmed=true）。清单与前端 src/redis/danger.ts 保持一致。
pub const DANGEROUS_COMMANDS: &[&str] = &[
    // 键删除/清库
    "DEL", "UNLINK", "FLUSHDB", "FLUSHALL", "FLUSH", "SWAPDB", "SHUTDOWN",
    // 键改名/移动/覆盖
    "RENAME", "RENAMENX", "COPY", "MOVE", "MIGRATE", "RESTORE", "SORT", "GETDEL",
    // 过期
    "EXPIRE", "PEXPIRE", "EXPIREAT", "PEXPIREAT", "EXPIRETIME", "PERSIST",
    // string 写入
    "SET", "SETNX", "SETEX", "PSETEX", "SET_RANGE", "SETRANGE", "MSET", "MSETNX", "GETSET", "APPEND",
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
    "SETBIT", "BITFIELD", "BITFIELD_RO", "GEOADD", "GEOSEARCHSTORE", "PFADD", "PFMERGE", "OBJECT_ENCONDING", "OBJECT_ENCODING",
    // 管理类
    "CONFIG_SET", "ACL_SETUSER", "ACL_DELUSER", "ACL_SAVE", "SCRIPT_FLUSH", "FUNCTION_FLUSH", "FUNCTION_DELETE", "FUNCTION_RESTORE", "CLIENT_KILL", "DEBUG",
    // 事务/订阅会改变会话状态
    "MULTI", "EXEC", "DISCARD", "WATCH", "UNWATCH", "SELECT",
];

pub fn command_risk(word: &str) -> CommandRisk {
    let key = word.trim().to_ascii_uppercase().replace([' ', '-'], "_");
    if key.is_empty() {
        return CommandRisk::Readonly;
    }
    if BLOCKED_COMMANDS.contains(&key.as_str()) {
        return CommandRisk::Blocked;
    }
    if DANGEROUS_COMMANDS.contains(&key.as_str()) {
        return CommandRisk::Dangerous;
    }
    CommandRisk::Readonly
}

/// RESP 值的结构化分类：前端按 kind 分派文本/整数/表格渲染。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RespReply {
    /// status | int | bulk | array | nil
    pub kind: &'static str,
    pub text: Option<String>,
    pub int: Option<i64>,
    pub items: Option<Vec<RespReply>>,
}

pub fn classify_reply(value: &redis::Value) -> RespReply {
    match value {
        redis::Value::Nil => RespReply { kind: "nil", text: None, int: None, items: None },
        redis::Value::Int(number) => RespReply {
            kind: "int",
            text: None,
            int: Some(*number),
            items: None,
        },
        redis::Value::BulkString(bytes) => RespReply {
            kind: "bulk",
            text: Some(String::from_utf8_lossy(bytes).into_owned()),
            int: None,
            items: None,
        },
        redis::Value::SimpleString(text) => RespReply { kind: "status", text: Some(text.clone()), int: None, items: None },
        redis::Value::Okay => RespReply {
            kind: "status",
            text: Some("OK".to_owned()),
            int: None,
            items: None,
        },
        redis::Value::Array(items) => RespReply {
            kind: "array",
            text: None,
            int: None,
            items: Some(items.iter().map(classify_reply).collect()),
        },
        redis::Value::Map(pairs) => RespReply {
            kind: "array",
            text: None,
            int: None,
            items: Some(
                pairs
                    .iter()
                    .flat_map(|(field, value)| [classify_reply(field), classify_reply(value)])
                    .collect(),
            ),
        },
        other => RespReply {
            kind: "bulk",
            text: Some(format!("{other:?}")),
            int: None,
            items: None,
        },
    }
}

/// 执行前置检查：Blocked 无条件拒绝；Dangerous 必须带确认标记。
pub fn guard_command(word: &str, confirmed: bool) -> Result<(), String> {
    match command_risk(word) {
        CommandRisk::Blocked => Err(format!(
            "已禁用 {word}：全量扫描会阻塞 Redis，键空间浏览一律走 SCAN 分页。"
        )),
        CommandRisk::Dangerous if !confirmed => Err(format!("{word} 属于写删类命令，需要先在界面确认。")),
        _ => Ok(()),
    }
}

/// 执行一条命令并结构化返回。调用方必须先通过 guard_command。
pub async fn run_command(
    conn: &mut redis::aio::MultiplexedConnection,
    word: &str,
    args: &[String],
) -> Result<RespReply, redis::RedisError> {
    let mut cmd = redis::cmd(word);
    for arg in args {
        cmd.arg(arg);
    }
    let value: redis::Value = cmd.query_async(conn).await?;
    Ok(classify_reply(&value))
}
