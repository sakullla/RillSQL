# MySQL / MariaDB 版本基线（T1）

日期：2026-11-21 · 任务：T1 · 覆盖需求：R1、R9

本文件固定本轮的兼容目标版本，并记录实施时最新稳定版的具体版本号与镜像摘要。后续 Task 的
真实验收（T2/T3/T4 的 live 报告）以此为基础建立“实现路径 → 适用版本 → 实际运行版本 → 被覆盖版本”的映射。

## 1. 取数方式（可复现）

- 时间：2026-11-21（本机 `date` 为 2026-11-21）。
- 来源：Docker Hub 官方镜像仓库 API
  `https://hub.docker.com/v2/repositories/library/mysql/tags?page_size=100&name=<前缀>&ordering=last_updated`
  与同一接口的 `library/mariadb`。每个 tag 的 `digest` 字段为 registry 侧的 manifest（多架构 manifest list）摘要。
- 判定“实施时最新稳定版”的规则：在该 tag 列表中取**纯数字三段的正式 tag**（形如 `9.7.4`，排除 `-rc`
  `-ubi` `-jammy` 等后缀）中主次修订号最大者。取数成功，没有使用记忆或推测补全。

## 2. 基线版本与镜像摘要

### MySQL

| 目标 | 固定版本 | 镜像 tag | manifest 摘要 | tag 最后推送 |
| --- | --- | --- | --- | --- |
| 5.7 | 5.7.44 | `mysql:5.7.44` | `sha256:4bc6bc963e6d8443453676cae56536f4b8156d78bae03c0145cbe47c2aad73bb` | 2023-12-15 |
| 8.0 | 8.0.46 | `mysql:8.0.46` | `sha256:7dcddc01f13bab2f15cde676d44d01f61fc9f99fe7785e86196dfc07d358ae2b` | 2026-05-05 |
| 8.4 | 8.4.13 | `mysql:8.4.13` | `sha256:a81d043930e51745522bd5c430961cf8286f0a23f0a9a2a6e0108f7ea71b0902` | 2026-11-03 |
| **9.x（实施时最新稳定版）** | **9.7.4** | `mysql:9.7.4` | `sha256:df90258da46f6e7ff8574ff84f17618aba0765cac749928299c69fd4e7cb8ad8` | 2026-11-03 |

### MariaDB

| 目标 | 固定版本 | 镜像 tag | manifest 摘要 | tag 最后推送 |
| --- | --- | --- | --- | --- |
| 10.6 | 10.6.28 | `mariadb:10.6.28` | `sha256:75079c0fb96894284d00d035bc4bef94234814f6f6c28b79877bc69d3285d760` | 2026-11-20 |
| 10.11 | 10.11.19 | `mariadb:10.11.19` | `sha256:e5fe75b2b9de0eb9f6f9a4d9c97272fb3a7480e389ccd487a786bab2a12cc0d7` | 2026-11-20 |
| 11.4 | 11.4.13 | `mariadb:11.4.13` | `sha256:a8992b5e5404d11e03c54a31d0c1bdea3fd116d283aa6d1a627551f29045309b` | 2026-11-20 |
| 11.8 | 11.8.9 | `mariadb:11.8.9` | `sha256:e1d5495f445654134f226f55a41ce0da227116f6f27f283101f405c794d841fc` | 2026-11-20 |
| **12.x（实施时最新稳定版）** | **12.3.3** | `mariadb:12.3.3` | `sha256:4aaf88b3106382c8c18e49f0cec60267be618fc22f858c4503afb2a2555afed3` | 2026-11-20 |

说明与限制：

- 摘要是**该 tag 在取数时刻**的 registry manifest 摘要；同一 tag 被重新推送会得到新摘要，届时以本文件的
  “取数时间”为准，不把摘要当成永久标识。
- MySQL 5.7 已在 2023-10 结束官方支持，但 R1 明确把它列为兼容目标，因此仍在基线内。它是否覆盖更高版本
  所用行为，要按 R9 用实现路径判断，不能因为“版本更老”就默认行为相同或不同。
- 实施后新发布的 9.x / 12.x 小版本不自动进入本轮基线（R11）。

## 3. 验收分组（初始假设，最终以 live 报告的实际路径为准）

分组只是起点。T2/T3/T4 的 live 报告必须按**实际代码路径**填写，出现下列任一差异就要单独验证：

| 差异点 | MySQL 家族内的表现 | 需要注意的版本 |
| --- | --- | --- |
| 版本串形态 | MariaDB 在旧客户端握手里报 `5.5.5-` 前缀；MySQL 只有数字 | 全部 MariaDB 目标 |
| 生成列持久化关键字 | MySQL 写 `STORED`；MariaDB 写 `PERSISTENT`（并接受 `STORED`） | MySQL 5.7+/MariaDB 10.2+ |
| 表达式默认值标记 | MySQL 8 在 EXTRA 里标 `DEFAULT_GENERATED`（不是生成列） | 8.0/8.4/9.x |
| COLUMN_DEFAULT 写法 | MySQL 报不带引号的原文；MariaDB 报带引号的字面量或 `NULL` | 全部 |
| 自身连接认证 | MySQL 8.0+ 默认 `caching_sha2_password`；8.4 起未装 `mysql_native_password` 插件；9.x 已移除该插件 | 8.4、9.x |
| 事件与调度器 | 两个家族都有 EVENT 与 `event_scheduler`，但默认关闭 | 全部 |
| 分区 | RANGE/LIST/HASH/KEY 与 `REMOVE PARTITIONING` 在两个家族都有 | 全部；RANGE/LIST COLUMNS 与子分区不在本轮（R11） |
| 元数据表 | `information_schema.PARAMETERS`、`TRIGGERS`、`ROUTINES` 两边都有 | 全部 |

下面这些**尚未验证**，只作为待验证假设，不能在 live 报告里当成结论：

- MySQL 9.x 是否在本轮用到的语法上有移除或默认值变化（除上面列出的认证插件移除）。
- MariaDB 12.x 相对 11.8 在本轮用到的语法、元数据列或读回写法上是否有差异。
- `information_schema.PARTITIONS` 的 `PARTITION_DESCRIPTION` 在两个家族各版本上的确切写法。
- 触发器与例程的 `SHOW CREATE` 原文在各版本的空格与引号差异。

## 4. 本地夹具

`docker/compose.mysql-versions.yaml` 按上面九个版本各提供一个服务，全部挂在 compose profile 上
（`mysql-5.7` … `mariadb-12`），主机端口 3406–3414。日常开发夹具仍是 `docker/compose.yaml`
里的 MySQL 8.4 与 MariaDB 11.4（3306/3307）。种子在 `docker/init/mysql-proprietary.sql`，
只用全部目标版本都支持的语法，事件调度器保持默认关闭，便于验证“启用事件也不会运行”。

启动单个版本（示例）：

```
docker compose -f docker/compose.mysql-versions.yaml --profile mysql-9 up -d
docker compose -f docker/compose.mysql-versions.yaml --profile mysql-9 down -v
```

## 5. 本轮没有取得或没有做的事

- 没有在本机启动上述容器实测；版本号与摘要是从 registry 取到的元数据，不是运行结果。
- 没有把 `docs/verification/` 下历史报告里的版本当作本轮证据。
- T2/T3/T4 的 live 报告必须记录**实际运行**的服务器版本，离线跳过不计通过。
