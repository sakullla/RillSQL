# Task query

[任务目标与验收](../03-execution-plan.md#tasks/query)

当前：实现结果标记为完成。

实现：MySQL整批复用同一连接且不重置，超限在服务器停住，失败会放开忙槽。

[历史记录（2 次）](../evidence/history/sha256-a9ddfef254475edbd26ba631e01d066f742b6d1c1ca738ec8f0a02eaac0756ff.json)

下一步：接收与审查状态以 current packet/回执为准。

## Attempt History

> Runtime 只读事实。
```yaml
format: task_attempt_history
task_id: query
history_ref: evidence/history/sha256-a9ddfef254475edbd26ba631e01d066f742b6d1c1ca738ec8f0a02eaac0756ff.json
history_count: 2
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
task_id: query
execution:
  status: completed
  summary: MySQL整批复用同一连接且不重置，超限在服务器停住，失败会放开忙槽。
  verification_refs: []
  concerns: []
```
