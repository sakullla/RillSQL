//! Redis 专项模块的纯逻辑测试：SCAN/值读取/TTL 命令组装、RESP 结构化分类、危险命令确认语义。
//! 不依赖 docker；真库端到端（含密码认证/SCAN 实际游标推进）由 docker 夹具人工/集成验证。

use super::exec::{classify_reply, command_risk, expand_command, guard_command, guard_invocation, invocation_word, CommandRisk};
use super::keys::{
    page_command, prepare_mutation, scan_args, ttl_command, RedisInitialValue, RedisMutation, RedisValue,
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
fn value_pages_use_cursor_or_offset_commands() {
    let plan = |kind: &str, token: Option<&str>| page_command(kind, "k", token, 200).unwrap_or_default().join(" ");
    assert_eq!(plan("string", None), "GET k");
    assert_eq!(plan("hash", None), "HSCAN k 0 COUNT 200");
    assert_eq!(plan("hash", Some("17")), "HSCAN k 17 COUNT 200");
    assert_eq!(plan("set", Some("9")), "SSCAN k 9 COUNT 200");
    assert_eq!(plan("list", None), "LRANGE k 0 199");
    assert_eq!(plan("list", Some("200")), "LRANGE k 200 399");
    assert_eq!(plan("zset", Some("400")), "ZRANGE k 400 599 WITHSCORES");
    assert_eq!(plan("stream", None), "XRANGE k - + COUNT 200");
    assert_eq!(plan("stream", Some("(5-0")), "XRANGE k (5-0 + COUNT 200");
    assert_eq!(plan("ReJSON-RL", None), "JSON.GET k");
    assert!(page_command("module", "k", None, 200).is_none(), "未知类型不做结构化读取");
}

#[test]
fn value_pages_never_list_keys() {
    for key_type in ["string", "hash", "list", "set", "zset", "stream", "ReJSON-RL", "none", "module"] {
        let plan = page_command(key_type, "k", None, 50).unwrap_or_default();
        assert!(!plan.iter().any(|word| word.eq_ignore_ascii_case("KEYS")), "{key_type} 的读取命令不得使用 KEYS");
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

// ------------------------------------------------------------------ R4：Cluster、模板、分组、诊断、批量

use super::batch::{self, BatchOp};
use super::cluster::{self, cross_slot, hash_tag, key_slot, parse_cluster_nodes};
use super::diag::{self, BigKey, SlowSort, TopN};
use super::keyspace::{self, KeyspacePrefs, TemplateRule, TreeRow};

#[test]
fn key_slot_follows_crc16_and_hash_tags() {
    // Redis Cluster 规范里的 CRC16 校验值与常见键的槽。
    assert_eq!(cluster::crc16(b"123456789"), 0x31C3);
    assert_eq!(key_slot("123456789"), 12739);
    assert_eq!(key_slot("foo"), 12182);
    assert_eq!(key_slot("bar"), 5061);
    assert_eq!(hash_tag("{user1000}.following"), Some("user1000"));
    assert_eq!(key_slot("{user1000}.following"), key_slot("{user1000}.followers"));
    assert_eq!(key_slot("{user1000}.following"), key_slot("user1000"));
    // 空 tag 不算：整个键参与计算。
    assert_eq!(hash_tag("foo{}{bar}"), None);
    assert_eq!(key_slot("foo{}{bar}"), cluster::crc16(b"foo{}{bar}") % 16384);
    // 只取第一个 { 到其后第一个 }。
    assert_eq!(hash_tag("foo{{bar}}zap"), Some("{bar"));
    assert_eq!(hash_tag("foo{bar}{zap}"), Some("bar"));
    assert!((0..16384).contains(&key_slot("任意中文键")));
}

#[test]
fn cross_slot_detects_multi_key_commands_across_slots() {
    let args = |items: &[&str]| items.iter().map(|item| (*item).to_owned()).collect::<Vec<_>>();
    let found = cross_slot("MGET", &args(&["foo", "bar"])).expect("foo 与 bar 不同槽");
    assert_eq!(found.slot_count(), 2);
    assert_eq!(found.keys[0], ("foo".to_owned(), 12182));
    assert!(cross_slot("MGET", &args(&["{u:1}:a", "{u:1}:b"])).is_none(), "同一 hash tag 同槽");
    assert!(cross_slot("GET", &args(&["foo"])).is_none(), "单键命令不检测");
    // MSET 只看键位（隔一个取一个），值不参与。
    assert!(cross_slot("MSET", &args(&["{t}a", "1", "{t}b", "2"])).is_none());
    assert!(cross_slot("MSET", &args(&["foo", "1", "bar", "2"])).is_some());
    // numkeys 形态与 BLPOP 的超时参数。
    assert!(cross_slot("EVAL", &args(&["return 1", "2", "foo", "bar", "argv"])).is_some());
    assert!(cross_slot("BLPOP", &args(&["{q}a", "{q}b", "0"])).is_none());
    assert!(cross_slot("SINTERCARD", &args(&["2", "foo", "bar"])).is_some());
    let message = super::exec::crossslot_message("mget", &args(&["foo", "bar"])).expect("提示");
    assert!(message.contains("CROSSSLOT") && message.contains("hash tag") && message.contains("12182"), "{message}");
}

#[test]
fn cluster_nodes_parse_roles_status_and_slot_ranges() {
    let text = "\
07c37dfeb235213a872192d90877d0cd55635b91 127.0.0.1:30004@31004,host-a slave e7d1eecce10fd6bb5eb35b9f99a514335d9ba9ca 0 1426238317239 4 connected
67ed2db8d677e59ec4a4cefb06858cf2a1a89fa1 127.0.0.1:30002@31002 master - 0 1426238316232 2 connected 5461-10922
292f8b365bb7edb5e285caf0b7e6ddc7265d2f4f 127.0.0.1:30003@31003 master,fail? - 0 1426238318243 3 connected 10923-16383 [93-<-292f8b]
e7d1eecce10fd6bb5eb35b9f99a514335d9ba9ca 127.0.0.1:30001@31001 myself,master - 0 0 1 connected 0-5460
6ec23923021cf3ffec47632106199cb7f496ce01 127.0.0.1:30005@31005 slave 67ed2db8d677e59ec4a4cefb06858cf2a1a89fa1 0 1426238316232 5 disconnected
";
    let nodes = parse_cluster_nodes(text);
    assert_eq!(nodes.len(), 5);
    let masters: Vec<_> = nodes.iter().filter(|node| node.master).collect();
    assert_eq!(masters.len(), 3);
    assert_eq!(masters[0].addr(), "127.0.0.1:30001", "主节点按首槽排序");
    assert!(masters[0].myself);
    assert_eq!(masters[0].slot_text(), "0-5460");
    assert_eq!(masters[0].slot_total(), 5461);
    assert_eq!(masters[2].status(), "pfail");
    assert_eq!(masters[2].slots, vec![(10923, 16383)], "迁移标记不算槽");
    let replica = nodes.iter().find(|node| node.port == 30005).unwrap();
    assert!(!replica.master);
    assert_eq!(replica.status(), "disconnected");
    assert_eq!(replica.master_id, "67ed2db8d677e59ec4a4cefb06858cf2a1a89fa1");
    assert_eq!(nodes.iter().find(|node| node.port == 30004).unwrap().host, "127.0.0.1", "地址里的 hostname 后缀去掉");
    let place = cluster::placement(&nodes, "foo");
    assert_eq!(place.slot, 12182);
    assert_eq!(place.node.as_deref(), Some("127.0.0.1:30003"));
    assert_eq!(cluster::placement(&nodes, "{user}:1").hash_tag.as_deref(), Some("user"));
    assert!(super::connect::cluster_enabled("# Cluster\r\ncluster_enabled:1\r\n"));
    assert!(!super::connect::cluster_enabled("# Cluster\r\ncluster_enabled:0\r\n"));
}

fn seps(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_owned()).collect()
}

#[test]
fn tree_groups_by_configured_separators_with_counts() {
    let names = ["a:b:1", "a:b:2", "a:c", "solo"];
    let indexed: Vec<(usize, &str)> = names.iter().copied().enumerate().collect();
    // 全部收起：只看到顶层 a（计数 3）与 solo。
    let rows = keyspace::build_tree(&indexed, &seps(&[":"]), &Default::default());
    assert!(rows.contains(&TreeRow::Folder { prefix: "a".into(), segment: "a".into(), depth: 0, count: 3, open: false }), "{rows:?}");
    assert_eq!(rows.len(), 2);
    // 展开 a 与 a:b：a→b 分组计数 2，叶子 1、2 在深度 2。
    let expanded = ["a".to_owned(), "a:b".to_owned()].into_iter().collect();
    let rows = keyspace::build_tree(&indexed, &seps(&[":"]), &expanded);
    assert!(rows.contains(&TreeRow::Folder { prefix: "a:b".into(), segment: "b".into(), depth: 1, count: 2, open: true }), "{rows:?}");
    assert!(rows.contains(&TreeRow::Key { index: 0, display: "1".into(), depth: 2 }));
    assert!(rows.contains(&TreeRow::Key { index: 1, display: "2".into(), depth: 2 }));
    assert!(rows.contains(&TreeRow::Key { index: 2, display: "c".into(), depth: 1 }));
    // 多个分隔符：| 与 : 都切段；更长的分隔符优先。
    let segments: Vec<&str> = keyspace::split_segments("app|user:7", &seps(&[":", "|"])).into_iter().map(|segment| segment.text).collect();
    assert_eq!(segments, vec!["app", "user", "7"]);
    let segments: Vec<&str> = keyspace::split_segments("a::b:c", &seps(&[":", "::"])).into_iter().map(|segment| segment.text).collect();
    assert_eq!(segments, vec!["a", "b", "c"]);
    let indexed = vec![(0, "app|user:7"), (1, "app|user:8")];
    let open = ["app".to_owned()].into_iter().collect();
    let rows = keyspace::build_tree(&indexed, &seps(&[":", "|"]), &open);
    assert!(rows.contains(&TreeRow::Folder { prefix: "app|user".into(), segment: "user".into(), depth: 1, count: 2, open: false }), "{rows:?}");
    assert_eq!(keyspace::folder_pattern("a:b", &seps(&[":"])), "a:b:*");
    assert_eq!(keyspace::folder_pattern("x*y", &seps(&[":"])), "x\\*y:*", "前缀里的 glob 字符要转义");
}

#[test]
fn templates_merge_variable_segments_and_respect_rules() {
    let uuid = "3f2504e0-4f89-11d3-9a0c-0305e82c3301";
    let hex = "a3f9c0d1e2b4f6a8c9d0";
    let names = ["order:1001".to_owned(), "order:1002".to_owned(), format!("order:{uuid}"), format!("order:{hex}"), "config".to_owned(), "user:alice:profile".to_owned()];
    let names = &names[..];
    let keys: Vec<(usize, &str, &str)> = names.iter().enumerate().map(|(index, name)| (index, name.as_str(), "string")).collect();
    let mut keys3 = keys.clone();
    keys3.retain(|(_, name, _)| *name != names[3]);
    let groups = keyspace::group_templates(&keys3, &seps(&[":"]), &[]);
    let order = groups.iter().find(|group| group.template == "order:*").expect("order:*");
    assert_eq!(order.count, 3, "order:1001、order:1002、order:<uuid> 归并为 order:* 计数 3");
    assert_eq!(groups[0].template, "order:*", "按键数降序");
    let groups = keyspace::group_templates(&keys, &seps(&[":"]), &[]);
    assert_eq!(groups.iter().find(|group| group.template == "order:*").unwrap().count, 4, "≥16 位十六进制也是变量");
    assert!(groups.iter().any(|group| group.template == "config"), "单段键不归并");
    assert!(groups.iter().any(|group| group.template == "user:alice:profile"), "普通单词不是变量");
    // 变量识别：数字、日期、时间戳、含数字的长随机串；普通单词不是。
    for segment in ["42", "2024-01-02", "20240102", "2024-01-02T10:00:00Z", "k8x2m9q4w7z1", uuid, hex] {
        assert!(keyspace::is_variable_segment(segment), "{segment}");
    }
    for segment in ["user", "profile", "v2", "abcdef", "session", ""] {
        assert!(!keyspace::is_variable_segment(segment), "{segment}");
    }
    // 手动标记：把 alice 段标为变量后，bob 也归入 user:*:profile；把 order:* 的变量段标为固定后拆回原样。
    let rules = vec![keyspace::rule_for("user:alice:profile", &seps(&[":"]), &[], 1, true).unwrap()];
    assert_eq!(rules[0], TemplateRule { pattern: "user:*:profile".into(), index: 1, variable: true });
    assert_eq!(keyspace::template_of("user:bob:profile", &seps(&[":"]), &rules), "user:*:profile");
    let fixed = vec![keyspace::rule_for("order:1001", &seps(&[":"]), &[], 1, false).unwrap()];
    assert_eq!(keyspace::template_of("order:1001", &seps(&[":"]), &fixed), "order:1001");
    assert_eq!(keyspace::template_of("order:1002", &seps(&[":"]), &fixed), "order:1002");
    let mut all = rules.clone();
    keyspace::upsert_rule(&mut all, TemplateRule { pattern: "user:*:profile".into(), index: 1, variable: false });
    assert_eq!(all.len(), 1, "同形态同段的规则被替换");
    assert_eq!(keyspace::template_pattern("order:*", &seps(&[":"])), "order:*");
    assert_eq!(keyspace::template_pattern("a[1]:*", &seps(&[":"])), "a\\[1\\]:*");
    assert_eq!(keyspace::template_segments("order:*", &seps(&[":"])), vec![("order".to_owned(), false), ("*".to_owned(), true)]);
    // 内存估算：抽样平均 × 键数，读不到的样本不计。
    assert_eq!(keyspace::estimate_memory(&[100, 300, -1], 10), Some(2000));
    assert_eq!(keyspace::estimate_memory(&[-1], 10), None);
    assert_eq!(keyspace::sample(&(0..100).collect::<Vec<_>>(), 4), vec![0, 25, 50, 75]);
    assert_eq!(keyspace::human_bytes(1536), "1.5 KB");
}

#[test]
fn keyspace_prefs_roundtrip_and_normalize() {
    let mut prefs = KeyspacePrefs::default();
    assert_eq!(prefs.separators, vec![":"]);
    prefs.set_separators_text("  :  |  : ");
    assert_eq!(prefs.separators, vec![":", "|"], "去空去重");
    prefs.set_separators_text("   ");
    assert_eq!(prefs.separators, vec![":"], "全空回到默认");
    prefs.rules.push(TemplateRule { pattern: "a:*".into(), index: 1, variable: false });
    prefs.scan_cap = 5;
    let restored = KeyspacePrefs::from_json(Some(&prefs.to_json()));
    assert_eq!(restored.rules, prefs.rules);
    assert_eq!(restored.scan_cap, 1_000, "上限钳到合理范围");
    assert_eq!(KeyspacePrefs::from_json(Some("not json")), KeyspacePrefs::default());
    assert_eq!(KeyspacePrefs::from_json(Some("{\"separators\":[\"/\"]}")).separators, vec!["/"], "缺省字段补默认");
}

#[test]
fn value_pages_decode_and_append_without_duplicates() {
    use super::keys::{append_page, decode_page, next_offset, RedisValue};
    let bulk = |text: &str| redis::Value::BulkString(text.as_bytes().to_vec());
    let page = decode_page("hash", None, 2, redis::Value::Array(vec![bulk("7"), redis::Value::Array(vec![bulk("f1"), bulk("v1"), bulk("f2"), bulk("")])]));
    assert_eq!(page.next.as_deref(), Some("7"));
    let mut value = page.value;
    let more = decode_page("hash", Some("7"), 2, redis::Value::Array(vec![bulk("0"), redis::Value::Array(vec![bulk("f2"), bulk(""), bulk("f3"), bulk("v3")])]));
    assert_eq!(more.next, None, "游标回到 0 即读完");
    append_page(&mut value, more.value);
    match &value {
        RedisValue::Hash { entries } => {
            assert_eq!(entries.iter().map(|entry| entry.field.as_str()).collect::<Vec<_>>(), vec!["f1", "f2", "f3"], "重复字段去掉");
            assert_eq!(entries[1].value, "", "空字符串照常保留");
        }
        other => panic!("{other:?}"),
    }
    let list = decode_page("list", Some("200"), 2, redis::Value::Array(vec![bulk("a"), bulk("b")]));
    assert_eq!(list.next.as_deref(), Some("202"));
    assert_eq!(next_offset(None, 1, 2), None, "不满一页就是最后一页");
    let stream = decode_page(
        "stream",
        None,
        1,
        redis::Value::Array(vec![redis::Value::Array(vec![bulk("5-0"), redis::Value::Array(vec![bulk("k"), bulk("v")])])]),
    );
    assert_eq!(stream.next.as_deref(), Some("(5-0"), "下一页从这条之后开始");
    let json = decode_page("ReJSON-RL", None, 200, bulk("{\"a\":1}"));
    assert_eq!(json.value, RedisValue::Json { text: "{\"a\":1}".into() });
    let plan = prepare_mutation(&RedisMutation::JsonSet { key: "doc".into(), value: "{\"a\":2}".into() }, true).unwrap();
    assert_eq!(plan.steps[0].word, "JSON.SET");
    assert_eq!(plan.steps[0].args, vec!["doc", "$", "{\"a\":2}"]);
    assert!(prepare_mutation(&RedisMutation::JsonSet { key: "doc".into(), value: "{bad".into() }, true).is_err());
    assert!(prepare_mutation(&RedisMutation::JsonSet { key: "doc".into(), value: "{}".into() }, false).is_err());
}

#[test]
fn danger_table_covers_diagnostic_and_cluster_writes() {
    for word in ["SLOWLOG RESET", "CONFIG RESETSTAT", "CONFIG REWRITE", "CLIENT KILL", "FLUSHALL", "FLUSHDB", "CLUSTER RESET", "CLUSTER FAILOVER", "SCRIPT KILL", "JSON.SET", "JSON.DEL"] {
        assert_eq!(command_risk(word), CommandRisk::Dangerous, "{word}");
    }
    for word in ["SLOWLOG GET", "CLIENT LIST", "CLUSTER NODES", "INFO", "MEMORY USAGE", "JSON.GET", "PUBLISH"] {
        assert_eq!(command_risk(word), CommandRisk::Readonly, "{word}");
    }
    // 拆开的两个词也按危险命令处理。
    assert!(guard_invocation("SLOWLOG", &["RESET".into()], false).is_err());
    assert!(guard_invocation("CONFIG", &["RESETSTAT".into()], false).is_err());
    assert!(guard_invocation("CLIENT", &["KILL".into(), "ID".into(), "7".into()], false).is_err());
    assert!(guard_invocation("CLIENT", &["KILL".into(), "ID".into(), "7".into()], true).is_ok());
    assert!(guard_invocation("SLOWLOG", &["GET".into()], false).is_ok());
}

#[test]
fn diagnostics_parse_slowlog_info_clients_and_rank_big_keys() {
    let bulk = |text: &str| redis::Value::BulkString(text.as_bytes().to_vec());
    let entry = |id: i64, at: i64, micros: i64| {
        redis::Value::Array(vec![
            redis::Value::Int(id),
            redis::Value::Int(at),
            redis::Value::Int(micros),
            redis::Value::Array(vec![bulk("HGETALL"), bulk("big")]),
            bulk("10.0.0.1:5000"),
            bulk("worker"),
        ])
    };
    let mut entries = diag::parse_slowlog(&redis::Value::Array(vec![entry(1, 100, 500), entry(2, 50, 90_000), entry(3, 300, 20)]), "n1");
    assert_eq!(entries[1].command, "HGETALL big");
    assert_eq!(entries[1].client_name, "worker");
    diag::sort_slowlog(&mut entries, SlowSort::Duration, true);
    assert_eq!(entries.iter().map(|entry| entry.id).collect::<Vec<_>>(), vec![2, 1, 3], "按耗时降序");
    diag::sort_slowlog(&mut entries, SlowSort::Time, false);
    assert_eq!(entries.iter().map(|entry| entry.id).collect::<Vec<_>>(), vec![2, 1, 3], "按时间升序");
    diag::sort_slowlog(&mut entries, SlowSort::Time, true);
    assert_eq!(entries[0].id, 3);

    let info = diag::parse_info("# Server\r\nredis_version:7.2.4\r\nrole:master\r\n\r\n# Memory\r\nused_memory_human:1.50M\r\n# Stats\r\nkeyspace_hits:90\r\nkeyspace_misses:10\r\n");
    assert_eq!(info.len(), 3);
    assert_eq!(info[1].name, "Memory");
    assert_eq!(diag::info_value(&info, "role"), Some("master"));
    let metrics = diag::key_metrics(&info);
    assert!(metrics.contains(&("redis.info.memory", "1.50M".to_owned())));
    assert!(metrics.contains(&("redis.info.hitRate", "90.0%".to_owned())));
    let stats = redis::Value::Array(vec![bulk("peak.allocated"), redis::Value::Int(10), bulk("db.0"), redis::Value::Array(vec![bulk("overhead.hashtable.main"), redis::Value::Int(72)])]);
    assert_eq!(diag::flatten_stats(&stats, ""), vec![("peak.allocated".into(), "10".into()), ("db.0.overhead.hashtable.main".into(), "72".into())]);

    let clients = diag::parse_clients("id=3 addr=127.0.0.1:5000 laddr=127.0.0.1:6379 fd=8 name=app age=12 idle=0 flags=N db=0 cmd=client|list\nid=4 addr=10.0.0.2:41 name= age=3 idle=3 db=2 cmd=get\n", "n1");
    assert_eq!(clients.len(), 2);
    assert_eq!(clients[0].addr, "127.0.0.1:5000", "addr 不和 laddr 混淆");
    assert_eq!(clients[0].cmd, "client|list");
    assert_eq!(clients[1].db, "2");

    let mut top = TopN::new(2);
    for (key, size, memory) in [("a", 5, None), ("b", 50, None), ("c", 7, None), ("d", 1, Some(4096))] {
        top.push(BigKey { key: key.into(), key_type: "list".into(), size, memory, node: String::new() });
    }
    assert_eq!(top.sorted().iter().map(|item| item.key.as_str()).collect::<Vec<_>>(), vec!["d", "b"], "有内存按内存，只保留前 N");
    assert!(diag::is_pattern("news.*") && !diag::is_pattern("alerts"));
}

#[test]
fn batch_plans_preview_and_require_confirmation() {
    let keys: Vec<String> = (0..1203).map(|index| format!("order:{index}")).collect();
    let preview = batch::preview(&keys, BatchOp::Unlink);
    assert_eq!(preview.count, 1203);
    assert_eq!(preview.samples.len(), batch::PREVIEW_SAMPLES);
    assert!(preview.command.starts_with("UNLINK"));
    assert!(batch::preview(&keys, BatchOp::Ttl(Some(60))).command.starts_with("EXPIRE … 60"));
    assert!(batch::preview(&keys, BatchOp::Ttl(None)).command.starts_with("PERSIST"));
    assert!(batch::require_batch(&keys, BatchOp::Unlink, false).unwrap_err().contains("1203"));
    assert!(batch::require_batch(&[], BatchOp::Unlink, true).is_err());
    assert!(batch::require_batch(&keys, BatchOp::Ttl(Some(-1)), true).is_err());
    assert!(batch::require_batch(&keys, BatchOp::Unlink, true).is_ok());
    let chunks = batch::plan_chunks(&keys, false, 500);
    assert_eq!(chunks.iter().map(Vec::len).collect::<Vec<_>>(), vec![500, 500, 203]);
    // 集群：每批只含同一个槽的键。
    let chunks = batch::plan_chunks(&keys, true, 500);
    assert_eq!(chunks.iter().map(Vec::len).sum::<usize>(), 1203);
    for chunk in &chunks {
        let slot = key_slot(&chunk[0]);
        assert!(chunk.iter().all(|key| key_slot(key) == slot));
    }
    let tagged: Vec<String> = (0..10).map(|index| format!("{{cart}}:{index}")).collect();
    assert_eq!(batch::plan_chunks(&tagged, true, 500).len(), 1, "同 hash tag 一批发完");
}
