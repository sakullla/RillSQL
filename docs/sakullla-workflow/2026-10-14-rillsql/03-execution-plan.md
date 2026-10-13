---
# 计划字段仅在此 YAML 中填写。
format: execution_plan
tasks:
  # 复制条目并使用唯一 id；依赖仅引用本列表，不能成环。
  - id: T1
    # goal：行为变化；outcomes：可观察的完成结果。
    goal: ""
    depends_on: []
    # 以下三项非空；covers 填当前 R-id。
    covers: []
    # 仓库相对写入范围，含生成文件；并行任务不重叠。
    scope: []
    outcomes: []
    # 检索测试后选择：reuse|extend|new|not_applicable。
    test: reuse
delivery_verification:
  # 稳定 id，command 必填且唯一；默认运行一次。
  full:
    command: ""
    # sla_seconds > 0：预热一次，计时三次。
---
# Execution Plan

<!-- 正文仅供阅读。 -->
