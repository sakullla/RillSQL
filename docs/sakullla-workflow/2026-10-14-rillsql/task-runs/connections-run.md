# Task connections

[任务目标与验收](../03-execution-plan.md#tasks/connections)

当前：实现结果标记为完成。

实现：阶段只在驱动完成后标成功：缺密码停在数据库认证且不写已加密；Verify 认证失败仍保留已验证主机名。connections …

验证记录：1 项。

- cargo test --manifest-path src-tauri/Cargo.toml --lib connections: 15 passed

问题与关注项：1 项。

- PostgreSQL 验证模式在系统信任根上同时使用客户端证书时仍会直接失败，并要求改用 CA 文件；按本次范围未改。

[历史记录（3 次）](../evidence/history/sha256-89f7f03e294af0728c90cadd94dd304b2b121e4a638bd5e68f42dd2e0e5a9499.json)

下一步：接收与审查状态以 current packet/回执为准。

## Attempt History

> Runtime 只读事实。
```yaml
format: task_attempt_history
task_id: connections
history_ref: evidence/history/sha256-89f7f03e294af0728c90cadd94dd304b2b121e4a638bd5e68f42dd2e0e5a9499.json
history_count: 3
```

## Execution

> 已接受 Worker 的 status/concerns 由 Runtime 补齐；Owner 只补必要摘要、验证或关注项，保留身份与历史，通过 current apply_command 提交。
> status：completed | completed_with_concerns | needs_context | blocked；summary 为非空单行，最多 64 字符。
> concerns 必填：completed 用 []，其余说明关注或阻断；verification_refs 可省略，仅记录实际验证。
> integration 默认省略，由 Runtime 推导。显式申报时，把 integration 写在 format: task_run 的 YAML 区域，与 execution 同级，不要写进 execution。单仓示例：integration: {paths: ["src/app.py"], subject: "fix: 修正任务实现"}。
> 单仓 integration.paths 必须申报本任务全部精确路径，路径相对仓库根目录；integration.subject 是单行 Conventional Commits。没有改动时用 integration: {paths: [], subject: ""}。
> 多仓按当前 Recipe 的 repository_targets 顺序填写：integration: {repository_targets: [{repository_id: "<Recipe 仓库 ID>", paths: ["src/app.py"]}], subject: "fix: 修正任务实现"}；每组路径相对对应仓库根目录，未改动的仓库仍保留空 paths。
```yaml
format: task_run
task_id: connections
execution:
  status: completed_with_concerns
  summary: 阶段只在驱动完成后标成功：缺密码停在数据库认证且不写已加密；Verify 认证失败仍保留已验证主机名。connections …
  verification_refs: ["cargo test --manifest-path src-tauri/Cargo.toml --lib connections: 15 passed"]
  concerns: [PostgreSQL 验证模式在系统信任根上同时使用客户端证书时仍会直接失败，并要求改用 CA 文件；按本次范围未改。]
```
