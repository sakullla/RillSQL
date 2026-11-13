# 连接测试与服务基础（T2）验证目录

- 日期：2026-11-14
- 任务：T2「让连接测试针对当前未保存草稿，并为最近连接和后续连接流程提供可靠的服务与持久化基础」
- 覆盖需求：R2（截图缺陷中的测试/字段错误部分）、R6（连接弹窗的测试失败保留、保存不连接、按需连接前提）
- 覆盖规范：S8.1（连接列表与编辑器）、S8.5（连接状态），以及 ADR-2/ADR-3/ADR-4 的服务侧结论
- 改动范围：`src-tauri/src/connections/{service,store,tests}.rs`、`src-tauri/src/app/views/connections/mod.rs`、
  `src-tauri/src/app/shell/{jobs,model,tests}.rs`

## 文件

| 文件 | 内容 |
| --- | --- |
| `behavior-map.md` | T2 四条 outcomes 逐条对应实现入口、行为取舍与回归用例 |
| `test-receipts.md` | 本轮实际运行的命令、结果、环境与时序 |

## 证据类型（请勿混读）

- **源码级 + Rust 测试**：本目录全部结论来自 `cargo test` 与本轮源码。连接测试使用真实的 SQLite 文件、
  脚本化 SSH/数据库探针（`ScriptSsh`/`ScriptDb`）与内存凭据库，不依赖外部 fixture 与网络。
- **未取得：原生窗口与截图**。本任务在无原生窗口/截屏通道的环境中完成，未生成连接弹窗或启动台的 PNG。
  按计划这类证据由 Delivery Owner 在最终集成 candidate 上取得；本目录不用历史截图替代。
- **未在本任务内完成的 UI 呈现**：字段旁的错误排版、弹窗三区结构与启动台的“最近连接”列表属于 T3
  （`src-tauri/src/app/shell/chrome.rs` 不在 T2 写入范围）。T2 提供并测试的是模型与服务层：
  字段级 `FieldIssue`（含原文 `detail`）、按持久化时间排序的 `Service::recent_ids` 与
  `ConnectionPage::recent`。当前 `chrome.rs` 仍把 `issues` 渲染成编辑器下方的提示条，
  这是 T3 要替换的既有呈现，不是本任务声称已完成的视觉结果。
