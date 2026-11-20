# T09 Redis 重做：验证记录

## 原生截图（未取得，延至 Delivery）

键页（树/模板视图、扫描进度）、慢日志页、集群键详情的原生截图本轮未取得。
A1 时会话锁屏；A2（2026-11-20）会话已解锁、`screencapture` 可用，但用户正在使用桌面，
应用没有直接打开 Redis 页签/诊断页的启动参数，截图需要 osascript 驱动鼠标键盘、抢占焦点。
按 Owner 指示不抢占用户桌面，截图延至 Delivery 在最终 candidate 上统一补取。

A3（2026-11-20）按 Owner 授权尝试补取：约定空闲不少于 5 分钟才开始，每步前复查，空闲时间回落到 60 秒以下立即停止。
准备工作：隔离数据目录 `RILLSQL_DEBUG_DATA_DIR`；本地无密码 Redis 容器（2500 键）与 3 主集群容器，只连这两个 fixture。
首次启动应用时空闲约 530 秒；第二次启动后复查只剩 2 秒，关闭应用 50 秒后再查是 0 秒，前台也已切到别的应用，判断用户已回到桌面。
因此按约定停止，一张截图都没有拍：应用已关闭，原前台应用已恢复，临时容器已删除，colima 已停止，docker context 已恢复为 default。
截图仍延至 Delivery。

## A2 修复与证据（Rust 测试，docker 夹具 live 运行）

夹具：colima + `docker-compose -f docker/compose.yaml up -d redis redis-init redis-cluster`。

- 批量操作绑定发起时的逻辑库（审查 P1）
  - `RedisHandle::pinned(db)`：单机另开一条连接并显式 `SELECT db`，批量 UNLINK/EXPIRE/PERSIST 只在这条连接上执行，
    不再共用主连接（多路复用同一 socket，SELECT 作用于整条 socket）。集群只有 0 号库，沿用集群连接。
  - 界面：确认框记下发起时的库；批量进行中拒绝切库与控制台 `SELECT`；确认时库已变则不执行。
  - `live_batch_stays_on_its_db_after_the_shared_connection_selects_another`：10、11 号库写入同名 1200 键，
    主连接 `SELECT 11` 后对 10 号库批量删除——10 号库删完、11 号库 1200 键保留。
    把实现临时改回共用主连接时该测试失败（“发起时的 10 号库被删完” 断言），恢复后通过。
  - `running_batch_pins_its_db_and_blocks_db_switches`（GPUI 无头）：切库、控制台 SELECT 被拒，库变后确认不执行。
- 集群建连失败退回单节点（审查 P2）
  - `settle_cluster`：集群连接建不起来时退回种子节点上的单节点会话并记录原因（`Topology.fallback`），
    经 SSH 隧道同样标记；顶栏显示“集群 · 单节点浏览”，说明带原因。集群建连单节点超时 15s → 8s。
  - `cluster_setup_failure_falls_back_to_the_seed_node`（单测）、`cluster_fallback_is_explained`（GPUI 无头）。
  - `live_cluster_with_unreachable_announced_nodes_falls_back_to_the_seed`：测试钩子把集群夹具公告的节点地址
    换成不可达端口（模拟 NAT/容器内地址），连接照常打开为单节点会话，`PING` 可用，`fallback` 带原因。
- 夹具：`redis-cluster` 容器重启时节点从 nodes.conf 恢复，原先 `cluster_state:ok` 尚未就绪就去 `--cluster create`
  报 not empty 退出；改为按 `cluster_known_nodes:3` 判断已建集群。`docker restart` 后验证仍 healthy、`cluster_state:ok`。

`cargo test --manifest-path src-tauri/Cargo.toml --locked`：1442 passed, 0 failed, 11 ignored（Redis 与集群夹具在线）。
`cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked`：无警告。

## A3 修复（A2 审查 P3）

- 集群退回单节点浏览时（`Topology.fallback` 有值）同样只允许 0 号库：库选择器切到 n 号库、控制台执行 `SELECT n`（n≠0）都在发送前被拒，
  库号和键索引保持不变。测试：`cluster_fallback_keeps_db_zero_only`（GPUI 无头）。
- 退回时版本串去掉硬编码的中文后缀，改为与语言无关的 `· Cluster`；退回状态与原因只由顶栏的 i18n 标记和说明显示。
- `cargo test --locked redis`：67 passed，1 ignored。
