//! 连接分组：只保存组织元数据（名称、顺序、折叠），连接定义与机密都不进这张模型。
//!
//! 分组是用户自己建的组织层，和环境标签互不复用：同一个连接可以既在“生产”标签下、
//! 又放在“夜间巡检”分组里，两者都不参与凭据与连接身份。

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// 分组名上限，和环境标签一致：侧栏一行放得下，又不至于被截断到认不出。
pub const MAX_GROUP_NAME: usize = 24;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionGroup {
    pub id: String,
    pub name: String,
    pub position: i64,
    /// 折叠状态跟着分组一起持久化：重启后回到用户离开时的样子。
    pub collapsed: bool,
}

impl ConnectionGroup {
    pub fn new(id: impl Into<String>, name: impl Into<String>, position: i64) -> Self {
        Self { id: id.into(), name: name.into(), position, collapsed: false }
    }
}

/// 分组名只做长度与空白校验：重名允许，靠 id 区分，界面要能指出是哪一个。
pub fn validate_group_name(name: &str) -> Result<String, String> {
    let trimmed = name.trim();
    let count = trimmed.chars().count();
    if count == 0 {
        return Err("请填写分组名称".to_owned());
    }
    if count > MAX_GROUP_NAME {
        return Err(format!("分组名称最多 {MAX_GROUP_NAME} 个字"));
    }
    Ok(trimmed.to_owned())
}

/// 位置优先、名称其次、id 兜底：排序结果不依赖数据库返回顺序。
pub fn sorted_groups(mut groups: Vec<ConnectionGroup>) -> Vec<ConnectionGroup> {
    groups.sort_by(|left, right| {
        left.position
            .cmp(&right.position)
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.id.cmp(&right.id))
    });
    groups
}

/// 资源管理器要渲染的分组结构：每个分组的成员连接（按传入顺序）和未分组连接。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GroupLayout {
    /// 每项是（分组, 成员连接 id）；空分组也在里面，界面必须看得见。
    pub groups: Vec<(ConnectionGroup, Vec<String>)>,
    /// 未分组连接，保持传入顺序（配置库按名称排序）。
    pub ungrouped: Vec<String>,
}

/// 成员关系里指向已删除分组的那些连接按未分组处理，不隐藏、不报错。
pub fn layout_groups(groups: &[ConnectionGroup], members: &BTreeMap<String, String>, connection_ids: &[String]) -> GroupLayout {
    let known: BTreeSet<&str> = groups.iter().map(|group| group.id.as_str()).collect();
    let mut layout = GroupLayout {
        groups: sorted_groups(groups.to_vec()).into_iter().map(|group| (group, Vec::new())).collect(),
        ungrouped: Vec::new(),
    };
    let index: BTreeMap<String, usize> = layout
        .groups
        .iter()
        .enumerate()
        .map(|(index, (group, _))| (group.id.clone(), index))
        .collect();
    for id in connection_ids {
        match members.get(id).filter(|group| known.contains(group.as_str())) {
            Some(group) => {
                if let Some(slot) = index.get(group).and_then(|slot| layout.groups.get_mut(*slot)) {
                    slot.1.push(id.clone());
                }
            }
            None => layout.ungrouped.push(id.clone()),
        }
    }
    layout
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(id: &str, name: &str, position: i64) -> ConnectionGroup {
        ConnectionGroup::new(id, name, position)
    }

    fn members(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(connection, group)| ((*connection).to_owned(), (*group).to_owned())).collect()
    }

    #[test]
    fn group_names_are_trimmed_and_bounded() {
        assert_eq!(validate_group_name("  夜间巡检 "), Ok("夜间巡检".to_owned()));
        assert!(validate_group_name("   ").is_err());
        assert!(validate_group_name(&"组".repeat(MAX_GROUP_NAME + 1)).is_err());
        assert!(validate_group_name(&"组".repeat(MAX_GROUP_NAME)).is_ok());
    }

    #[test]
    fn sort_is_position_then_name_then_id() {
        let sorted = sorted_groups(vec![group("b", "乙", 1), group("a", "甲", 1), group("c", "丙", 0)]);
        let ids: Vec<&str> = sorted.iter().map(|group| group.id.as_str()).collect();
        // 位置 0 的“丙”在最前；同位置按名称（这里按 UTF-8 字节序，乙在甲前），再按 id 兜底。
        assert_eq!(ids, vec!["c", "b", "a"]);
    }

    #[test]
    fn layout_keeps_empty_groups_and_returns_deleted_groups_to_top_level() {
        let groups = vec![group("g1", "开发", 0), group("g2", "空组", 1)];
        let connections = vec!["c1".to_owned(), "c2".to_owned(), "c3".to_owned()];
        let layout = layout_groups(&groups, &members(&[("c1", "g1"), ("c2", "ghost")]), &connections);
        assert_eq!(layout.groups[0].1, vec!["c1".to_owned()]);
        assert_eq!(layout.groups[1].0.name, "空组");
        assert!(layout.groups[1].1.is_empty(), "空分组仍然可见");
        assert_eq!(layout.ungrouped, vec!["c2".to_owned(), "c3".to_owned()], "已删除分组的成员回到顶层");
    }

    #[test]
    fn no_groups_means_every_connection_is_ungrouped() {
        let connections = vec!["c1".to_owned(), "c2".to_owned()];
        let layout = layout_groups(&[], &BTreeMap::new(), &connections);
        assert!(layout.groups.is_empty());
        assert_eq!(layout.ungrouped, connections);
    }
}
