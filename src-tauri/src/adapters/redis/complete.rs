//! Redis 命令与键名自动补全：提供常用命令签名、中英文说明、读写风险标记及上下文键名补全。

use crate::i18n::Locale;
use super::exec::{command_risk, CommandRisk};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RedisCommandSpec {
    pub name: &'static str,
    pub syntax: &'static str,
    pub group: &'static str,
    pub summary_zh: &'static str,
    pub summary_en: &'static str,
    pub takes_key: bool,
}

impl RedisCommandSpec {
    pub fn summary(&self, locale: Locale) -> &'static str {
        match locale {
            Locale::ZhCn => self.summary_zh,
            Locale::En => self.summary_en,
        }
    }

    pub fn is_write(&self) -> bool {
        matches!(command_risk(self.name), CommandRisk::Dangerous)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RedisCompletion {
    pub label: String,
    pub insert: String,
    /// "command" | "key" | "database"
    pub kind: &'static str,
    pub detail: String,
    pub summary: String,
}

const fn cmd(
    name: &'static str,
    syntax: &'static str,
    group: &'static str,
    summary_zh: &'static str,
    summary_en: &'static str,
    takes_key: bool,
) -> RedisCommandSpec {
    RedisCommandSpec { name, syntax, group, summary_zh, summary_en, takes_key }
}

pub static COMMANDS: &[RedisCommandSpec] = &[
    // String
    cmd("GET", "GET key", "string", "读取字符串键值", "Get the string value of a key", true),
    cmd("SET", "SET key value [EX seconds] [NX|XX]", "string", "设置字符串键值及可选过期时间", "Set the string value of a key", true),
    cmd("MGET", "MGET key [key ...]", "string", "批量读取多个字符串键", "Get values of multiple keys", true),
    cmd("MSET", "MSET key value [key value ...]", "string", "批量设置多个字符串键值", "Set multiple keys to multiple values", true),
    cmd("SETNX", "SETNX key value", "string", "仅在键不存在时写入字符串", "Set key value only if key does not exist", true),
    cmd("SETEX", "SETEX key seconds value", "string", "设置键值并指定秒级过期时间", "Set value and expiration in seconds", true),
    cmd("PSETEX", "PSETEX key milliseconds value", "string", "设置键值并指定毫秒级过期时间", "Set value and expiration in milliseconds", true),
    cmd("GETDEL", "GETDEL key", "string", "读取键值并删除该键", "Get the value of key and delete the key", true),
    cmd("APPEND", "APPEND key value", "string", "向字符串末尾追加内容", "Append a value to a key", true),
    cmd("STRLEN", "STRLEN key", "string", "获取字符串字节长度", "Get the length of the value stored in a key", true),
    cmd("INCR", "INCR key", "string", "将整数键值原子加 1", "Increment the integer value of a key by one", true),
    cmd("INCRBY", "INCRBY key increment", "string", "将整数键值增加指定步长", "Increment the integer value of a key by the given amount", true),
    cmd("INCRBYFLOAT", "INCRBYFLOAT key increment", "string", "将浮点键值增加指定步长", "Increment the float value of a key by the given amount", true),
    cmd("DECR", "DECR key", "string", "将整数键值原子减 1", "Decrement the integer value of a key by one", true),
    cmd("DECRBY", "DECRBY key decrement", "string", "将整数键值减少指定步长", "Decrement the integer value of a key by the given number", true),
    cmd("GETRANGE", "GETRANGE key start end", "string", "截取字符串指定区间的子串", "Get a substring of the string stored at a key", true),
    cmd("SETRANGE", "SETRANGE key offset value", "string", "从指定偏移量覆盖字符串", "Overwrite part of a string at key starting at the specified offset", true),
    // Keys & Expiration
    cmd("SCAN", "SCAN cursor [MATCH pattern] [COUNT count] [TYPE type]", "key", "游标增量遍历键空间（安全替代 KEYS）", "Incrementally iterate the keys space", false),
    cmd("TYPE", "TYPE key", "key", "查看键存储的数据类型", "Determine the type stored at key", true),
    cmd("TTL", "TTL key", "key", "查看键剩余过期秒数", "Get the time to live for a key in seconds", true),
    cmd("PTTL", "PTTL key", "key", "查看键剩余过期毫秒数", "Get the time to live for a key in milliseconds", true),
    cmd("EXISTS", "EXISTS key [key ...]", "key", "检查一个或多个键是否存在", "Determine if a key exists", true),
    cmd("EXPIRE", "EXPIRE key seconds", "key", "为键设置秒级过期时间", "Set a key's time to live in seconds", true),
    cmd("PEXPIRE", "PEXPIRE key milliseconds", "key", "为键设置毫秒级过期时间", "Set a key's time to live in milliseconds", true),
    cmd("PERSIST", "PERSIST key", "key", "移除键的过期时间（转为永不过期）", "Remove the expiration from a key", true),
    cmd("DEL", "DEL key [key ...]", "key", "同步删除一个或多个键", "Delete one or more keys", true),
    cmd("UNLINK", "UNLINK key [key ...]", "key", "异步非阻塞删除一个或多个键", "Delete keys asynchronously in background", true),
    cmd("RENAME", "RENAME key newkey", "key", "重命名键（覆盖同名目标键）", "Rename a key", true),
    cmd("RENAMENX", "RENAMENX key newkey", "key", "仅当新键名不存在时重命名", "Rename a key only if the new key does not exist", true),
    cmd("MEMORY USAGE", "MEMORY USAGE key", "key", "估算键占用的内存字节数", "Estimate the memory usage of a key", true),
    cmd("OBJECT ENCODING", "OBJECT ENCODING key", "key", "查看键底层的内部编码结构", "Inspect the internal encoding of a Redis object", true),
    cmd("RANDOMKEY", "RANDOMKEY", "key", "从当前库随机返回一个键名", "Return a random key from the keyspace", false),
    // Hash
    cmd("HGET", "HGET key field", "hash", "读取哈希表指定字段的值", "Get the value of a hash field", true),
    cmd("HSET", "HSET key field value [field value ...]", "hash", "设置哈希表一个或多个字段值", "Set the string value of a hash field", true),
    cmd("HGETALL", "HGETALL key", "hash", "读取哈希表所有字段与值", "Get all the fields and values in a hash", true),
    cmd("HMGET", "HMGET key field [field ...]", "hash", "批量读取哈希表多个字段的值", "Get the values of all the given hash fields", true),
    cmd("HKEYS", "HKEYS key", "hash", "获取哈希表中的所有字段名", "Get all the fields in a hash", true),
    cmd("HVALS", "HVALS key", "hash", "获取哈希表中的所有字段值", "Get all the values in a hash", true),
    cmd("HLEN", "HLEN key", "hash", "获取哈希表字段数量", "Get the number of fields in a hash", true),
    cmd("HEXISTS", "HEXISTS key field", "hash", "判断哈希表指定字段是否存在", "Determine if a hash field exists", true),
    cmd("HDEL", "HDEL key field [field ...]", "hash", "删除哈希表一个或多个字段", "Delete one or more hash fields", true),
    cmd("HINCRBY", "HINCRBY key field increment", "hash", "将哈希字段整数值增加指定步长", "Increment the integer value of a hash field", true),
    cmd("HINCRBYFLOAT", "HINCRBYFLOAT key field increment", "hash", "将哈希字段浮点值增加指定步长", "Increment the float value of a hash field", true),
    cmd("HSETNX", "HSETNX key field value", "hash", "仅当哈希字段不存在时设置值", "Set the value of a hash field only if it does not exist", true),
    cmd("HSCAN", "HSCAN key cursor [MATCH pattern] [COUNT count]", "hash", "增量遍历哈希表字段", "Incrementally iterate hash fields and associated values", true),
    // List
    cmd("LRANGE", "LRANGE key start stop", "list", "读取列表指定区间内的元素（0 -1 为全部）", "Get a range of elements from a list", true),
    cmd("LLEN", "LLEN key", "list", "获取列表元素个数", "Get the length of a list", true),
    cmd("LINDEX", "LINDEX key index", "list", "按索引读取列表元素", "Get an element from a list by its index", true),
    cmd("LPUSH", "LPUSH key element [element ...]", "list", "将一个或多个值推入列表头部", "Prepend one or multiple elements to a list", true),
    cmd("RPUSH", "RPUSH key element [element ...]", "list", "将一个或多个值推入列表尾部", "Append one or multiple elements to a list", true),
    cmd("LPOP", "LPOP key [count]", "list", "移除并返回列表头部元素", "Remove and get the first elements in a list", true),
    cmd("RPOP", "RPOP key [count]", "list", "移除并返回列表尾部元素", "Remove and get the last elements in a list", true),
    cmd("LSET", "LSET key index element", "list", "按索引更新列表元素值", "Set the value of an element in a list by its index", true),
    cmd("LREM", "LREM key count element", "list", "从列表中移除匹配元素", "Remove elements from a list", true),
    cmd("LTRIM", "LTRIM key start stop", "list", "裁剪列表，仅保留指定区间元素", "Trim a list to the specified range", true),
    cmd("LPOS", "LPOS key element", "list", "查找元素在列表中的索引位置", "Return the index of matching elements inside a Redis list", true),
    // Set
    cmd("SMEMBERS", "SMEMBERS key", "set", "获取集合中的所有成员", "Get all the members in a set", true),
    cmd("SCARD", "SCARD key", "set", "获取集合成员数量", "Get the number of members in a set", true),
    cmd("SISMEMBER", "SISMEMBER key member", "set", "判断成员是否在集合中", "Determine if a given value is a member of a set", true),
    cmd("SADD", "SADD key member [member ...]", "set", "向集合添加一个或多个成员", "Add one or more members to a set", true),
    cmd("SREM", "SREM key member [member ...]", "set", "从集合移除一个或多个成员", "Remove one or more members from a set", true),
    cmd("SPOP", "SPOP key [count]", "set", "随机移除并返回集合成员", "Remove and return one or multiple random members from a set", true),
    cmd("SRANDMEMBER", "SRANDMEMBER key [count]", "set", "随机读取集合成员（不删除）", "Get one or multiple random members from a set", true),
    cmd("SINTER", "SINTER key [key ...]", "set", "计算多个集合的交集", "Intersect multiple sets", true),
    cmd("SUNION", "SUNION key [key ...]", "set", "计算多个集合的并集", "Add multiple sets", true),
    cmd("SDIFF", "SDIFF key [key ...]", "set", "计算多个集合的差集", "Subtract multiple sets", true),
    cmd("SSCAN", "SSCAN key cursor [MATCH pattern] [COUNT count]", "set", "增量遍历集合成员", "Incrementally iterate Set elements", true),
    // Sorted Set
    cmd("ZRANGE", "ZRANGE key start stop [WITHSCORES]", "zset", "按排名区间读取有序集合成员", "Return a range of members in a sorted set", true),
    cmd("ZREVRANGE", "ZREVRANGE key start stop [WITHSCORES]", "zset", "按分值从高到低读取有序集合区间", "Return a range of members in a sorted set, by index, high to low", true),
    cmd("ZRANGEBYSCORE", "ZRANGEBYSCORE key min max [WITHSCORES]", "zset", "按分值区间读取有序集合成员", "Return a range of members in a sorted set, by score", true),
    cmd("ZCARD", "ZCARD key", "zset", "获取有序集合成员总数", "Get the number of members in a sorted set", true),
    cmd("ZSCORE", "ZSCORE key member", "zset", "查询有序集合指定成员的分值", "Get the score associated with the given member in a sorted set", true),
    cmd("ZRANK", "ZRANK key member", "zset", "查询成员按升序排列的排名", "Determine the index of a member in a sorted set", true),
    cmd("ZREVRANK", "ZREVRANK key member", "zset", "查询成员按降序排列的排名", "Determine the index of a member in a sorted set, high to low", true),
    cmd("ZCOUNT", "ZCOUNT key min max", "zset", "统计分值区间内的成员数量", "Count the members in a sorted set with scores within the given values", true),
    cmd("ZADD", "ZADD key [NX|XX] score member [score member ...]", "zset", "添加或更新有序集合成员与分值", "Add one or more members to a sorted set, or update its score", true),
    cmd("ZINCRBY", "ZINCRBY key increment member", "zset", "为有序集合成员分值增加指定增量", "Increment the score of a member in a sorted set", true),
    cmd("ZREM", "ZREM key member [member ...]", "zset", "从有序集合移除一个或多个成员", "Remove one or more members from a sorted set", true),
    cmd("ZSCAN", "ZSCAN key cursor [MATCH pattern] [COUNT count]", "zset", "增量遍历有序集合成员", "Incrementally iterate sorted sets elements and scores", true),
    // Stream
    cmd("XRANGE", "XRANGE key start end [COUNT count]", "stream", "按消息 ID 区间正序读取流消息（- + 为全部）", "Return a range of elements in a stream", true),
    cmd("XREVRANGE", "XREVRANGE key end start [COUNT count]", "stream", "按消息 ID 区间倒序读取流消息", "Return a range of elements in a stream in reverse order", true),
    cmd("XLEN", "XLEN key", "stream", "获取流中的消息数量", "Return the number of entries in a stream", true),
    cmd("XADD", "XADD key * field value [field value ...]", "stream", "向流追加一条新消息", "Appends a new entry to a stream", true),
    cmd("XDEL", "XDEL key id [id ...]", "stream", "从流中删除指定消息", "Removes the specified entries from the stream", true),
    cmd("XTRIM", "XTRIM key MAXLEN [~] threshold", "stream", "裁剪流的长度到阈值以内", "Trims the stream to a given number of items", true),
    cmd("XINFO STREAM", "XINFO STREAM key", "stream", "查看流的元数据摘要", "Get information about a stream", true),
    // Server & Database
    cmd("PING", "PING [message]", "server", "测试连接连通性与响应", "Ping the server", false),
    cmd("SELECT", "SELECT index", "server", "切换到指定逻辑库（0–15）", "Change the selected database for the current connection", false),
    cmd("DBSIZE", "DBSIZE", "server", "查看当前逻辑库的键总数", "Return the number of keys in the selected database", false),
    cmd("INFO", "INFO [section]", "server", "查看服务端运行状态、内存与键空间信息", "Get information and statistics about the server", false),
    cmd("INFO keyspace", "INFO keyspace", "server", "查看各逻辑库（db0–db15）的键数量统计", "Get per-database key counts and expiration stats", false),
    cmd("INFO memory", "INFO memory", "server", "查看服务端内存占用详情", "Get memory consumption metrics", false),
    cmd("INFO clients", "INFO clients", "server", "查看客户端连接统计", "Get client connection statistics", false),
    cmd("INFO stats", "INFO stats", "server", "查看命令吞吐、命中率等统计指标", "Get general server statistics", false),
    cmd("CONFIG GET", "CONFIG GET parameter", "server", "读取服务端运行配置参数", "Get the value of a configuration parameter", false),
    cmd("CONFIG SET", "CONFIG SET parameter value", "server", "动态修改服务端配置参数", "Set a configuration parameter to the given value", false),
    cmd("CLIENT LIST", "CLIENT LIST", "server", "列出当前连接到服务端的客户端", "Get the list of client connections", false),
    cmd("SLOWLOG GET", "SLOWLOG GET [count]", "server", "查看最近的慢查询日志条目", "Get the slow log's entries", false),
    cmd("TIME", "TIME", "server", "返回当前服务端时间戳", "Return the current server time", false),
    cmd("FLUSHDB", "FLUSHDB [ASYNC]", "server", "清空当前逻辑库中的所有键", "Remove all keys from the current database", false),
    cmd("FLUSHALL", "FLUSHALL [ASYNC]", "server", "清空实例所有逻辑库中的全部键", "Remove all keys from all databases", false),
    // Bitmap / HyperLogLog / Geo
    cmd("PFADD", "PFADD key element [element ...]", "hll", "向 HyperLogLog 添加基数统计元素", "Adds the specified elements to the specified HyperLogLog", true),
    cmd("PFCOUNT", "PFCOUNT key [key ...]", "hll", "读取 HyperLogLog 的近似基数估算值", "Return the approximated cardinality of the set(s)", true),
    cmd("GETBIT", "GETBIT key offset", "bitmap", "读取位图指定偏移位的值", "Returns the bit value at offset in the string value stored at key", true),
    cmd("SETBIT", "SETBIT key offset value", "bitmap", "设置位图指定偏移位的值（0 或 1）", "Sets or clears the bit at offset in the string value stored at key", true),
    cmd("BITCOUNT", "BITCOUNT key [start end]", "bitmap", "统计字符串被置为 1 的比特数", "Count set bits in a string", true),
    cmd("GEOADD", "GEOADD key longitude latitude member", "geo", "添加地理空间坐标成员", "Add one or more geospatial items in the geospatial index", true),
    cmd("GEODIST", "GEODIST key member1 member2 [M|KM]", "geo", "计算两个地理空间成员之间的距离", "Returns the distance between two members of a geospatial index", true),
];

/// 按命令名查找签名说明（支持单词命令与 `INFO keyspace` / `CONFIG GET` 双词命令）。
pub fn lookup_command(line: &str) -> Option<&'static RedisCommandSpec> {
    let mut parts = line.split_whitespace();
    let first = parts.next()?;
    if let Some(second) = parts.next() {
        let two = format!("{} {}", first, second);
        if let Some(found) = COMMANDS.iter().find(|spec| spec.name.eq_ignore_ascii_case(&two)) {
            return Some(found);
        }
    }
    COMMANDS.iter().find(|spec| spec.name.eq_ignore_ascii_case(first))
}

/// 根据当前命令行文本、光标字节偏移与已扫描键清单计算补全列表。
/// 返回 `(replace_start, replace_end, items)`。
pub fn complete_command_line(
    line: &str,
    cursor: usize,
    known_keys: &[String],
    locale: Locale,
) -> (usize, usize, Vec<RedisCompletion>) {
    let end = floor_char_boundary(line, cursor.min(line.len()));
    let line_start = line[..end].rfind('\n').map(|idx| idx + 1).unwrap_or(0);
    let current_line = &line[line_start..end];

    let token_start_rel = current_line
        .rfind(|ch: char| ch.is_whitespace())
        .map(|idx| idx + 1)
        .unwrap_or(0);
    let token_end_rel = line[line_start + token_start_rel..]
        .find(|ch: char| ch.is_whitespace())
        .map(|len| token_start_rel + len)
        .unwrap_or(line.len() - line_start);

    let replace_start = line_start + token_start_rel;
    let replace_end = line_start + token_end_rel;
    let prefix = &line[replace_start..end];
    let before = current_line[..token_start_rel].trim();

    // 1. 首个词（或双词命令第二词）：补全 Redis 命令
    if before.is_empty() {
        let items = match_commands(prefix, locale);
        return (replace_start, replace_end, items);
    }

    let prior_tokens: Vec<&str> = before.split_whitespace().collect();
    if prior_tokens.len() == 1 {
        let head = prior_tokens[0];
        // 双词命令的第二词补全（如 INFO / CONFIG / OBJECT / XINFO / CLIENT / SLOWLOG / MEMORY）
        let compound_prefix = format!("{} {}", head, prefix);
        let mut items = match_compound_subcommands(head, &compound_prefix, prefix, locale);

        // SELECT 补全 0..15 逻辑库
        if head.eq_ignore_ascii_case("SELECT") {
            for db in 0..16u8 {
                let text = db.to_string();
                if prefix.is_empty() || text.starts_with(prefix) {
                    items.push(RedisCompletion {
                        label: format!("db{db}"),
                        insert: text,
                        kind: "database",
                        detail: format!("SELECT {db}"),
                        summary: match locale {
                            Locale::ZhCn => format!("切换到逻辑库 db{db}"),
                            Locale::En => format!("Switch to database db{db}"),
                        },
                    });
                }
            }
            return (replace_start, replace_end, items);
        }

        // 首个参数需要 key 的命令：补全已扫描到的键名
        let takes_key = COMMANDS
            .iter()
            .find(|spec| spec.name.eq_ignore_ascii_case(head))
            .is_some_and(|spec| spec.takes_key)
            || head.eq_ignore_ascii_case("KEYS");
        if takes_key {
            items.extend(match_keys(prefix, known_keys, locale));
        }
        return (replace_start, replace_end, items);
    }

    // MGET / DEL / UNLINK / EXISTS / SINTER / SUNION / SDIFF 支持多键参数继续补全键名
    if let Some(&head) = prior_tokens.first() {
        if ["MGET", "DEL", "UNLINK", "EXISTS", "SINTER", "SUNION", "SDIFF"]
            .iter()
            .any(|cmd_name| head.eq_ignore_ascii_case(cmd_name))
        {
            return (replace_start, replace_end, match_keys(prefix, known_keys, locale));
        }
        // SCAN cursor MATCH ... 提示 MATCH / COUNT / TYPE 关键字
        if head.eq_ignore_ascii_case("SCAN")
            || head.eq_ignore_ascii_case("HSCAN")
            || head.eq_ignore_ascii_case("SSCAN")
            || head.eq_ignore_ascii_case("ZSCAN")
        {
            let mut opts = Vec::new();
            for (kw, desc_zh, desc_en) in [
                ("MATCH", "按通配符模式过滤", "Filter by glob-style pattern"),
                ("COUNT", "每批扫描建议返回条数", "Hint batch count per iteration"),
                ("TYPE", "仅返回指定类型的键", "Filter keys by Redis data type"),
            ] {
                if prefix.is_empty() || kw.starts_with(&prefix.to_ascii_uppercase()) {
                    opts.push(RedisCompletion {
                        label: kw.to_owned(),
                        insert: kw.to_owned(),
                        kind: "command",
                        detail: kw.to_owned(),
                        summary: if locale == Locale::ZhCn { desc_zh.to_owned() } else { desc_en.to_owned() },
                    });
                }
            }
            return (replace_start, replace_end, opts);
        }
    }

    (replace_start, replace_end, Vec::new())
}

fn match_commands(prefix: &str, locale: Locale) -> Vec<RedisCompletion> {
    let needle = prefix.trim().to_ascii_uppercase();
    let mut ranked: Vec<(u8, &'static RedisCommandSpec)> = Vec::new();
    for spec in COMMANDS {
        let upper = spec.name.to_ascii_uppercase();
        let score = if needle.is_empty() {
            1
        } else if upper == needle {
            0
        } else if upper.starts_with(&needle) {
            1
        } else if upper.contains(&needle) {
            2
        } else {
            continue;
        };
        ranked.push((score, spec));
    }
    ranked.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.name.len().cmp(&b.1.name.len())).then_with(|| a.1.name.cmp(b.1.name)));
    ranked
        .into_iter()
        .take(36)
        .map(|(_, spec)| RedisCompletion {
            label: spec.name.to_owned(),
            insert: spec.name.to_owned(),
            kind: "command",
            detail: spec.syntax.to_owned(),
            summary: spec.summary(locale).to_owned(),
        })
        .collect()
}

fn match_compound_subcommands(head: &str, full_prefix: &str, sub_prefix: &str, locale: Locale) -> Vec<RedisCompletion> {
    let head_upper = head.to_ascii_uppercase();
    let full_upper = full_prefix.to_ascii_uppercase();
    let sub_upper = sub_prefix.to_ascii_uppercase();
    let mut items = Vec::new();
    for spec in COMMANDS {
        let Some((first, second)) = spec.name.split_once(' ') else { continue };
        if !first.eq_ignore_ascii_case(&head_upper) {
            continue;
        }
        if sub_upper.is_empty() || second.to_ascii_uppercase().starts_with(&sub_upper) || spec.name.to_ascii_uppercase().starts_with(&full_upper) {
            items.push(RedisCompletion {
                label: spec.name.to_owned(),
                insert: second.to_owned(),
                kind: "command",
                detail: spec.syntax.to_owned(),
                summary: spec.summary(locale).to_owned(),
            });
        }
    }
    items
}

fn match_keys(prefix: &str, known_keys: &[String], locale: Locale) -> Vec<RedisCompletion> {
    let needle = prefix.to_ascii_lowercase();
    let mut ranked: Vec<(u8, &String)> = Vec::new();
    for key in known_keys {
        let lower = key.to_ascii_lowercase();
        let score = if needle.is_empty() {
            1
        } else if lower.starts_with(&needle) {
            0
        } else if lower.contains(&needle) {
            1
        } else {
            continue;
        };
        ranked.push((score, key));
    }
    ranked.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.len().cmp(&b.1.len())).then_with(|| a.1.cmp(b.1)));
    let summary = match locale {
        Locale::ZhCn => "当前库已加载键",
        Locale::En => "Scanned key in current DB",
    };
    ranked
        .into_iter()
        .take(40)
        .map(|(_, key)| {
            let insert = if key.chars().any(char::is_whitespace) { format!("\"{key}\"") } else { key.clone() };
            RedisCompletion {
                label: key.clone(),
                insert,
                kind: "key",
                detail: "key".to_owned(),
                summary: summary.to_owned(),
            }
        })
        .collect()
}

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    if index >= text.len() {
        return text.len();
    }
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_and_key_completion_rank_prefixes_and_context() {
        let keys = vec!["user:1001".to_owned(), "user:1002".to_owned(), "order:9".to_owned()];
        let (start, end, cmds) = complete_command_line("hge", 3, &keys, Locale::ZhCn);
        assert_eq!((start, end), (0, 3));
        assert!(cmds.iter().any(|c| c.label == "HGET"));
        assert!(cmds.iter().any(|c| c.label == "HGETALL"));

        let (kstart, kend, key_items) = complete_command_line("HGETALL us", 10, &keys, Locale::ZhCn);
        assert_eq!((kstart, kend), (8, 10));
        assert_eq!(key_items.len(), 2);
        assert_eq!(key_items[0].label, "user:1001");
        assert_eq!(key_items[0].kind, "key");

        let (_, _, dbs) = complete_command_line("SELECT 1", 8, &keys, Locale::En);
        assert!(dbs.iter().any(|d| d.insert == "1" && d.kind == "database"));
        assert!(dbs.iter().any(|d| d.insert == "15"));
    }
}
