//! Redis 专项模块的纯逻辑测试：SCAN/值读取/TTL 命令组装、RESP 结构化分类、危险命令确认语义。
//! 不依赖 docker；真库端到端（含密码认证/SCAN 实际游标推进）由 docker 夹具人工/集成验证。

use super::exec::{classify_reply, command_risk, expand_command, guard_command, guard_invocation, invocation_word, CommandRisk};
use super::keys::{
    prepare_mutation, scan_args, ttl_command, value_read_plan, RedisInitialValue, RedisMutation, RedisValue,
};

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
    for word in ["GET", "HGETALL", "LRANGE", "SMEMBERS", "ZRANGE", "XRANGE", "SCAN", "TYPE", "TTL", "PTTL", "DBSIZE", "INFO", "PING", "CONFIG"] {
        assert_eq!(command_risk(word), CommandRisk::Readonly, "{word} 应为只读直通");
    }
    assert_eq!(command_risk("CONFIG SET"), CommandRisk::Dangerous);
    assert_eq!(command_risk("ACL SETUSER"), CommandRisk::Dangerous);
    assert_eq!(command_risk("SCRIPT FLUSH"), CommandRisk::Dangerous);
    assert_eq!(command_risk("CLIENT KILL"), CommandRisk::Dangerous);
    assert_eq!(invocation_word("CONFIG", &["SET".into(), "save".into()]), "CONFIG SET");
    assert_eq!(expand_command("CONFIG SET", &["save".into()]).0, "CONFIG");
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
    // 只读直通不需要确认。CONFIG 单独出现仍是只读；CONFIG SET 即使拆成两个词也要确认。
    assert!(guard_command("GET", false).is_ok());
    assert!(guard_command("CONFIG", false).is_ok());
    assert!(guard_invocation("CONFIG", &["SET".into(), "save".into()], false).is_err());
    assert!(guard_invocation("CONFIG", &["SET".into(), "save".into()], true).is_ok());
    assert!(guard_command("CONFIG SET", false).is_err());
    assert!(guard_command("KEYS *", true).is_err());
}

#[test]
fn mutation_refuses_unconfirmed_writes_and_preserves_ttl() {
    let string_set = RedisMutation::SetString { key: "user:1".into(), value: "林夏".into() };
    let denied = prepare_mutation(&string_set, false).unwrap_err();
    assert!(denied.contains("确认"), "{denied}");
    let plan = prepare_mutation(&string_set, true).unwrap();
    assert!(plan.exists_key.is_none());
    assert_eq!(plan.steps[0].word, "SET");
    assert_eq!(plan.steps[0].args, vec!["user:1", "林夏", "KEEPTTL"]);
    assert!(plan.steps.iter().all(|step| !step.word.eq_ignore_ascii_case("KEYS")));

    let ttl = RedisMutation::SetTtl { key: "user:1".into(), seconds: Some(30) };
    assert!(prepare_mutation(&ttl, false).is_err());
    let ttl_plan = prepare_mutation(&ttl, true).unwrap();
    assert_eq!(ttl_plan.steps[0].word, "EXPIRE");
    let persist = prepare_mutation(&RedisMutation::SetTtl { key: "user:1".into(), seconds: None }, true).unwrap();
    assert_eq!(persist.steps[0].word, "PERSIST");

    let created = RedisMutation::CreateKey {
        key: "rank".into(),
        ttl_secs: Some(15),
        overwrite: false,
        initial: RedisInitialValue::ZSet { score: 1.5, member: "张伟".into() },
    };
    let created = prepare_mutation(&created, true).unwrap();
    assert_eq!(created.exists_key.as_deref(), Some("rank"));
    assert!(!created.overwrite);
    assert_eq!(created.steps[0].word, "ZADD");
    assert_eq!(created.steps[1].word, "EXPIRE");
    assert!(created.steps.iter().all(|step| !step.word.eq_ignore_ascii_case("KEYS")));

    let bad_score = RedisMutation::ZSetAdd { key: "rank".into(), score: f64::NAN, member: "张伟".into() };
    assert!(prepare_mutation(&bad_score, true).is_err(), "非法分值即使确认也不能生成写计划");
    let stream = RedisMutation::StreamAdd { key: "events".into(), id: String::new(), fields: Vec::new() };
    assert!(prepare_mutation(&stream, true).is_err());
    for mutation in [
        RedisMutation::HashSet { key: "h".into(), field: "f".into(), value: "v".into() },
        RedisMutation::HashDel { key: "h".into(), fields: vec!["f".into()] },
        RedisMutation::ListPush { key: "l".into(), head: true, value: "a".into() },
        RedisMutation::ListSet { key: "l".into(), index: 0, value: "a".into() },
        RedisMutation::ListRem { key: "l".into(), count: 1, value: "a".into() },
        RedisMutation::SetAdd { key: "s".into(), member: "a".into() },
        RedisMutation::SetRem { key: "s".into(), member: "a".into() },
        RedisMutation::ZSetRem { key: "z".into(), member: "a".into() },
        RedisMutation::StreamDel { key: "e".into(), id: "1-0".into() },
    ] {
        assert!(prepare_mutation(&mutation, false).is_err(), "{mutation:?}");
        let plan = prepare_mutation(&mutation, true).unwrap();
        assert!(plan.steps.iter().all(|step| !step.word.eq_ignore_ascii_case("KEYS")));
    }
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
