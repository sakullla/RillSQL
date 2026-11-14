# 连接分组：创建、重命名、折叠、删除与重启恢复

## 数据路径（唯一一条）

```
资源管理器分组标题 / 连接行  →  WorkbenchModel::group_command / assign_connection_group
   →  PendingJob::GroupOp（分组用自己的任务板 owner="groups"）
   →  jobs::execute_job → Service::{save_group,delete_group,assign_group,set_group_collapsed}
   →  Store（connections.sqlite：connection_groups + connection_group_members）
   →  成功后回读 Service::groups() → JobDone::Groups → WorkbenchModel::apply_groups
```

- 拖拽（`on_drag` / `on_drop`）与右键“移动到分组”产生**同一条**命令 `group-assign:<连接>\u{1}<分组|空>`，二者没有各自的落库分支。
- 分组任务使用独立任务板（`owner = "groups"`）。首版实现曾与连接列表共用 owner，导致启动时 `RefreshGroups` 顶掉在途的 `RefreshConnections`（对象树因此读不到连接）。现在两者互不 supersede。

## 存储与迁移

| 表 | 列 | 说明 |
| --- | --- | --- |
| `connection_groups` | `id, name, position, collapsed` | 分组顺序与折叠状态跟分组一起持久化 |
| `connection_group_members` | `connection_id (PK), group_id` | 没有行 = 未分组，不写哨兵值 |

- `Store::migrate` 建这两张表，然后只清理悬空引用（`connection_id` 不在 `connections` 里、`group_id` 不在 `connection_groups` 里）。**迁移不删任何连接行**，存量连接天然是“未分组”，没有分组时渲染与排序和改动前一致。
- `SavedConnection` 一个字段都没加：分组不进连接定义，也就不会随连接导出/导入走，更不碰 `Secrets`/`SecretInput`。
- `Store::delete`（删连接）顺带删该连接的成员关系；分组本身保留。
- `Store::delete_group` 在一个事务里删成员关系再删分组；失败时两者都不变，连接一条不少。

## 行为断言（实际用例）

| 行为 | 用例 | 断言要点 |
| --- | --- | --- |
| 创建 + 重启恢复 | `connections::tests::groups_persist_across_restart_and_leave_other_connections_ungrouped` | 名称两侧空白去掉；重新 `Store::open` 后分组、成员、折叠状态都在；未归组连接 `members` 里没有条目；连接定义（`saved.name`）不受影响 |
| 重启恢复（经任务路径） | `app::shell::tests::groups_and_collapse_state_survive_a_restart_through_the_job_path` | `GroupOp` 落库 → 新服务/新模型 → `group-refresh` 读回：分组名、折叠状态、成员关系一致；折叠组只剩标题行 |
| 删除分组 | `connections::tests::deleting_a_group_returns_connections_to_the_top_level_without_losing_them` | 成员关系清空、`service.list()` 仍返回该连接；重复删除报“找不到这个分组” |
| 名称校验 | `connections::tests::group_names_are_validated_and_missing_targets_are_rejected` | 空名/超长名拒绝；重命名不换 id；指向不存在分组或连接不落库 |
| 悬空引用 | `connections::tests::unknown_group_ids_read_as_ungrouped_and_deleted_connections_drop_membership` | 分组行被外部删掉后，成员关系在下次打开时清掉，连接按未分组读且数量不变；删连接时成员关系一起走 |
| 平铺外观不变 | `connections::groups::tests::no_groups_means_every_connection_is_ungrouped` | 没有分组时 `layout.ungrouped` 就是全部连接，顺序不变 |
| 空组可见 / 删除后回顶层 | `connections::groups::tests::layout_keeps_empty_groups_and_returns_deleted_groups_to_top_level` | 空组仍出现在 `layout.groups` 里；成员指向已删分组的连接落回顶层 |
| 模型侧归组 | `app::shell::tests::explorer_flat_rows_group_connections_and_keep_empty_groups_visible` | 行序列为 `group:g1, group:g2, conn:c1, conn:c2, conn:c3`；折叠 `g2` 后只剩标题 |
| 命令路由 | `app::shell::tests::group_commands_share_one_pending_job_and_reject_empty_or_missing_targets` | 新建/重命名/折叠/归组/移出/删除各自只产生一条 `PendingJob::GroupOp`；空名当场拒绝；指向不存在目标的命令不产生 |
| 删除两步确认 | 同上 | `group-delete:<id>` 只进确认层（`group_delete`），`group-delete-confirm` 才排队删除；`group-delete-cancel` 清空且不产生命令 |

## 未解决事实

- 删分组当前没有“撤销”；确认层文案说明“连接保留并回到顶层”，但删除后不提供恢复入口。
- 分组不支持拖拽排序（位置由创建顺序决定），需要时用重命名+重建。
