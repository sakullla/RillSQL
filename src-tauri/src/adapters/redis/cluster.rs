//! Redis Cluster 的纯逻辑：键槽计算（CRC16 + hash tag）、多键命令的跨槽检测与 `CLUSTER NODES` 解析。
//! 不发网络请求；连接与路由在 `conn.rs`。

use serde::Serialize;

/// 集群槽总数。
pub const SLOT_COUNT: u16 = 16384;

/// CRC16/XMODEM（多项式 0x1021、初值 0），与 Redis Cluster 规范一致。
pub fn crc16(bytes: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &byte in bytes {
        crc ^= u16::from(byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
        }
    }
    crc
}

/// 键里的 hash tag：第一个 `{` 与其后第一个 `}` 之间的非空内容。没有或为空时整个键参与计算。
pub fn hash_tag(key: &str) -> Option<&str> {
    let open = key.find('{')?;
    let rest = &key[open + 1..];
    let close = rest.find('}')?;
    if close == 0 {
        return None;
    }
    Some(&rest[..close])
}

/// 键所在的槽。
pub fn key_slot(key: &str) -> u16 {
    let part = hash_tag(key).unwrap_or(key);
    crc16(part.as_bytes()) % SLOT_COUNT
}

/// 多键命令里参与路由的键（按 Redis 命令表的键位置）。单键或无键命令返回空。
pub fn command_keys<'a>(command: &str, args: &'a [String]) -> Vec<&'a str> {
    let word = command.trim().to_ascii_uppercase();
    let all = || args.iter().map(String::as_str).collect::<Vec<_>>();
    match word.as_str() {
        "DEL" | "UNLINK" | "EXISTS" | "MGET" | "TOUCH" | "WATCH" | "SINTER" | "SUNION" | "SDIFF" | "SINTERSTORE" | "SUNIONSTORE"
        | "SDIFFSTORE" | "PFCOUNT" | "PFMERGE" => all(),
        "MSET" | "MSETNX" => args.iter().step_by(2).map(String::as_str).collect(),
        "RENAME" | "RENAMENX" | "COPY" | "SMOVE" | "LMOVE" | "RPOPLPUSH" | "BLMOVE" | "BRPOPLPUSH" => {
            args.iter().take(2).map(String::as_str).collect()
        }
        // 最后一个参数是超时。
        "BLPOP" | "BRPOP" | "BZPOPMIN" | "BZPOPMAX" => {
            let n = args.len().saturating_sub(1);
            args.iter().take(n).map(String::as_str).collect()
        }
        // numkeys 在第一个参数：SINTERCARD / ZUNION / ZINTER / ZDIFF / LMPOP / ZMPOP。
        "SINTERCARD" | "ZUNION" | "ZINTER" | "ZDIFF" | "LMPOP" | "ZMPOP" => numkeys_at(args, 0),
        // 目标键 + numkeys：ZUNIONSTORE dest numkeys key...
        "ZUNIONSTORE" | "ZINTERSTORE" | "ZDIFFSTORE" => {
            let mut keys: Vec<&str> = args.first().map(String::as_str).into_iter().collect();
            keys.extend(numkeys_at(args, 1));
            keys
        }
        // EVAL script numkeys key...
        "EVAL" | "EVALSHA" | "EVAL_RO" | "EVALSHA_RO" | "FCALL" | "FCALL_RO" => numkeys_at(args, 1),
        _ => Vec::new(),
    }
}

fn numkeys_at(args: &[String], at: usize) -> Vec<&str> {
    let Some(count) = args.get(at).and_then(|value| value.parse::<usize>().ok()) else { return Vec::new() };
    args.iter().skip(at + 1).take(count).map(String::as_str).collect()
}

/// 跨槽检测结果：参与路由的键与各自的槽。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrossSlot {
    pub keys: Vec<(String, u16)>,
}

impl CrossSlot {
    /// 去重后的槽数。
    pub fn slot_count(&self) -> usize {
        let mut slots: Vec<u16> = self.keys.iter().map(|(_, slot)| *slot).collect();
        slots.sort_unstable();
        slots.dedup();
        slots.len()
    }
}

/// 多键命令的键分布在不止一个槽时返回明细（集群会以 CROSSSLOT 拒绝）。
pub fn cross_slot(command: &str, args: &[String]) -> Option<CrossSlot> {
    let keys = command_keys(command, args);
    if keys.len() < 2 {
        return None;
    }
    let found = CrossSlot { keys: keys.iter().map(|key| ((*key).to_owned(), key_slot(key))).collect() };
    (found.slot_count() > 1).then_some(found)
}

/// 集群节点（`CLUSTER NODES` 的一行）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClusterNode {
    pub id: String,
    pub host: String,
    pub port: u16,
    pub master: bool,
    /// 副本所跟随的主节点 id；主节点为空。
    pub master_id: String,
    pub flags: Vec<String>,
    /// connected / disconnected。
    pub link: String,
    pub slots: Vec<(u16, u16)>,
    pub myself: bool,
}

impl ClusterNode {
    pub fn addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// 节点状态：fail / pfail（疑似下线）/ disconnected / ok。
    pub fn status(&self) -> &'static str {
        if self.flags.iter().any(|flag| flag == "fail") {
            "fail"
        } else if self.flags.iter().any(|flag| flag == "fail?") {
            "pfail"
        } else if self.link != "connected" {
            "disconnected"
        } else {
            "ok"
        }
    }

    pub fn owns(&self, slot: u16) -> bool {
        self.slots.iter().any(|(start, end)| (*start..=*end).contains(&slot))
    }

    pub fn slot_total(&self) -> u32 {
        self.slots.iter().map(|(start, end)| u32::from(*end) - u32::from(*start) + 1).sum()
    }

    /// 槽范围的紧凑文本：`0-5460, 7000`。
    pub fn slot_text(&self) -> String {
        self.slots
            .iter()
            .map(|(start, end)| if start == end { start.to_string() } else { format!("{start}-{end}") })
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// 解析 `CLUSTER NODES` 输出。地址形如 `ip:port@cport[,hostname]`；`[slot->-node]` 迁移标记忽略。
pub fn parse_cluster_nodes(text: &str) -> Vec<ClusterNode> {
    let mut nodes = Vec::new();
    for line in text.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 8 {
            continue;
        }
        let addr = parts[1].split(',').next().unwrap_or("");
        let addr = addr.split('@').next().unwrap_or("");
        let Some((host, port)) = addr.rsplit_once(':') else { continue };
        let Ok(port) = port.parse::<u16>() else { continue };
        let flags: Vec<String> = parts[2].split(',').map(str::to_owned).collect();
        let master = flags.iter().any(|flag| flag == "master");
        let master_id = if parts[3] == "-" { String::new() } else { parts[3].to_owned() };
        let mut slots = Vec::new();
        for item in &parts[8..] {
            if item.starts_with('[') {
                continue;
            }
            let range = match item.split_once('-') {
                Some((start, end)) => start.parse::<u16>().ok().zip(end.parse::<u16>().ok()),
                None => item.parse::<u16>().ok().map(|slot| (slot, slot)),
            };
            if let Some(range) = range {
                slots.push(range);
            }
        }
        slots.sort_unstable();
        nodes.push(ClusterNode {
            id: parts[0].to_owned(),
            host: host.to_owned(),
            port,
            master,
            master_id,
            myself: flags.iter().any(|flag| flag == "myself"),
            flags,
            link: parts[7].to_owned(),
            slots,
        });
    }
    // 主节点按首个槽排序，副本跟在后面。
    nodes.sort_by_key(|node| (!node.master, node.slots.first().map(|(start, _)| *start).unwrap_or(u16::MAX), node.addr()));
    nodes
}

/// 某个槽归哪个主节点。
pub fn node_for_slot(nodes: &[ClusterNode], slot: u16) -> Option<&ClusterNode> {
    nodes.iter().find(|node| node.master && node.owns(slot))
}

/// 键在集群里的位置说明：槽、hash tag、所在主节点。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyPlacement {
    pub slot: u16,
    pub hash_tag: Option<String>,
    pub node: Option<String>,
}

pub fn placement(nodes: &[ClusterNode], key: &str) -> KeyPlacement {
    let slot = key_slot(key);
    KeyPlacement { slot, hash_tag: hash_tag(key).map(str::to_owned), node: node_for_slot(nodes, slot).map(ClusterNode::addr) }
}

/// 按槽分组（批量删除/过期在集群里只能同槽多键一起发）。保持各组内的原有顺序。
pub fn group_by_slot(keys: &[String]) -> Vec<(u16, Vec<String>)> {
    let mut groups: std::collections::BTreeMap<u16, Vec<String>> = std::collections::BTreeMap::new();
    for key in keys {
        groups.entry(key_slot(key)).or_default().push(key.clone());
    }
    groups.into_iter().collect()
}
