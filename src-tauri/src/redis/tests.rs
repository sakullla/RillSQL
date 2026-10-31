//! Redis 专项模块的纯逻辑测试：SCAN/值读取/TTL 命令组装、RESP 结构化分类、危险命令确认语义。
//! 不依赖 docker；真库端到端（含密码认证/SCAN 实际游标推进）由 docker 夹具人工/集成验证。

use super::exec::{classify_reply, command_risk, guard_command, CommandRisk};
use super::keys::{scan_args, ttl_command, value_read_plan, RedisValue};

#[test]
fn scan_args_builds_cursor_match_count_without_keys() {
    // 游标 0 + 模式 + COUNT：SCAN 的参数顺序固定为 CURSOR [MATCH pattern] [COUNT n]。
    assert_eq!(scan_args(0, Some("user:*"), 100), vec!["0", "MATCH", "user:*", "COUNT", "100"]);
    assert_eq!(scan_args(42, None, 50), vec!["42", "COUNT", "50"]);
    // 空模式等价于不过滤；过大的 COUNT 被钳制。
    assert_eq!(scan_args(7, Some("  "), 100), vec!["7", "COUNT", "100"]);
    assert_eq!(scan_args(7, None, 100_000).last().unwrap(), "1000");
    assert_eq!(scan_args(7, None, 0).last().unwrap(), "1");
}

#[test]
fn value_read_plan_matches_type_commands() {
    assert_eq!(value_read_plan("string"), &["GET"]);
    assert_eq!(value_read_plan("hash"), &["HGETALL"]);
    assert_eq!(value_read_plan("list"), &["LRANGE", "0", "-1"]);
    assert_eq!(value_read_plan("set"), &["SMEMBERS"]);
    assert_eq!(value_read_plan("zset"), &["ZRANGE", "0", "-1", "WITHSCORES"]);
    assert_eq!(value_read_plan("stream"), &["XRANGE", "-", "+"]);
    assert!(value_read_plan("module").is_empty(), "未知类型不做结构化读取");
}

#[test]
fn value_read_plan_never_lists_keys() {
    for key_type in ["string", "hash", "list", "set", "zset", "stream", "none", "module"] {
        let plan = value_read_plan(key_type);
        assert!(
            !plan.iter().any(|word| word.eq_ignore_ascii_case("KEYS")),
            "{key_type} 的读取命令不得使用 KEYS"
        );
    }
}

#[test]
fn ttl_command_maps_seconds_to_expire_and_none_to_persist() {
    assert_eq!(ttl_command(Some(300)), ("EXPIRE", 300));
    assert_eq!(ttl_command(Some(0)), ("EXPIRE", 0));
    assert_eq!(ttl_command(None), ("PERSIST", 0));
}

#[test]
fn reply_classification_covers_resp_kinds() {
    let nil = classify_reply(&redis::Value::Nil);
    assert_eq!(nil.kind, "nil");
    assert_eq!(nil.items, None);

    let int = classify_reply(&redis::Value::Int(-2));
    assert_eq!((int.kind, int.int), ("int", Some(-2)));

    let bulk = classify_reply(&redis::Value::BulkString(b"hello".to_vec()));
    assert_eq!((bulk.kind, bulk.text.as_deref()), ("bulk", Some("hello")));

    let status = classify_reply(&redis::Value::SimpleString("PONG".to_owned()));
    assert_eq!((status.kind, status.text.as_deref()), ("status", Some("PONG")));
    let okay = classify_reply(&redis::Value::Okay);
    assert_eq!((okay.kind, okay.text.as_deref()), ("status", Some("OK")));

    let array = classify_reply(&redis::Value::Array(vec![
        redis::Value::BulkString(b"a".to_vec()),
        redis::Value::BulkString(b"b".to_vec()),
        redis::Value::Nil,
    ]));
    assert_eq!(array.kind, "array");
    assert_eq!(array.items.as_ref().expect("array items").len(), 3);
    assert_eq!(array.items.as_ref().unwrap()[2].kind, "nil");
}

#[test]
fn command_risk_splits_blocked_dangerous_and_readonly() {
    assert_eq!(command_risk("KEYS"), CommandRisk::Blocked);
    assert_eq!(command_risk("keys"), CommandRisk::Blocked);
    for word in [
        "DEL", "UNLINK", "FLUSHDB", "FLUSHALL", "RENAME", "EXPIRE", "PERSIST", "SET", "HSET", "LPOP", "RPOP", "SPOP", "ZREM", "XADD", "SHUTDOWN",
    ] {
        assert_eq!(command_risk(word), CommandRisk::Dangerous, "{word} 应为写删类");
    }
    for word in ["GET", "HGETALL", "LRANGE", "SMEMBERS", "ZRANGE", "XRANGE", "SCAN", "TYPE", "TTL", "PTTL", "DBSIZE", "INFO", "PING"] {
        assert_eq!(command_risk(word), CommandRisk::Readonly, "{word} 应为只读直通");
    }
}

#[test]
fn guard_requires_confirmation_for_dangerous_and_blocks_keys() {
    // KEYS 无论确认与否都拒绝（禁全量扫描）。
    assert!(guard_command("KEYS", false).is_err());
    assert!(guard_command("keys", true).is_err());
    // 写删类：未确认拒绝，确认后放行。
    let denied = guard_command("FLUSHDB", false).unwrap_err();
    assert!(denied.contains("确认"), "拒绝原因要说明需要确认：{denied}");
    assert!(guard_command("FLUSHDB", true).is_ok());
    assert!(guard_command("DEL", false).is_err());
    assert!(guard_command("del", true).is_ok());
    // 只读直通不需要确认。
    assert!(guard_command("GET", false).is_ok());
}

#[test]
fn value_payload_serializes_with_kind_tag() {
    // 前端按 kind 分派查看器：serde tag 必须是 lower-case 的 kind 字段。
    let payload = RedisValue::ZSet {
        entries: vec![super::keys::ScoredMember {
            member: "林夏".to_owned(),
            score: "90".to_owned(),
        }],
    };
    let json = serde_json::to_value(&payload).expect("serialize");
    assert_eq!(json["kind"], "zset");
    assert_eq!(json["entries"][0]["member"], "林夏");
    assert_eq!(json["entries"][0]["score"], "90");
    let none = serde_json::to_value(RedisValue::None {}).expect("serialize");
    assert_eq!(none["kind"], "none");
}
