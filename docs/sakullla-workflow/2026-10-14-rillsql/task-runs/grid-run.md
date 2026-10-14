# Task grid

[任务目标与验收](../03-execution-plan.md#tasks/grid)

当前：实现结果标记为完成。

实现：提交失败时待提交条会显示数据库返回的错误，失败行悬停也能看到同一消息。

[历史记录（3 次）](../evidence/history/sha256-03ba433c53f724fcd0fab7a17020f662d64c9f719de7f1072976c5bb2e114def.json)

下一步：接收与审查状态以 current packet/回执为准。

## Attempt History

> Runtime 只读事实。
```yaml
format: task_attempt_history
task_id: grid
history_ref: evidence/history/sha256-03ba433c53f724fcd0fab7a17020f662d64c9f719de7f1072976c5bb2e114def.json
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
task_id: grid
execution:
  status: completed
  summary: 提交失败时待提交条会显示数据库返回的错误，失败行悬停也能看到同一消息。
  verification_refs: []
  concerns: []
```
