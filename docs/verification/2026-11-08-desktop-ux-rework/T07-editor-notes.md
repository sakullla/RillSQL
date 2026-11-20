# T07 编辑器：开发验证记录

证据来源：Rust 单测与 gpui 测试窗口（`cargo test`）。原生应用截图、录屏未取得：本机会话处于锁屏状态
（`ioreg` 报 `CGSSessionScreenIsLocked=true`），`screencapture` 无法截取应用窗口。

## R9 错误提示闪烁：原因（代码分析，未经原生复现）

改动前，同一位置的错误有两个弹层：

1. 静态 lint 写进 gpui-base 的诊断集合，鼠标移动时由 `on_mouse_move` 显示诊断弹层，滚动时 `on_scroll_wheel` 清除它；
2. 同一条 lint 和执行错误的原文又由 `SqlHover`（LSP 悬停提供者）作为 Markdown 悬停弹层返回。

两个弹层锚定在同一范围，一个由鼠标移动同步出现，一个由悬停任务异步出现。滚动时诊断弹层先被清掉，
下一次鼠标移动又重新出现，两者交替覆盖，这就是闪烁。签名提示用的是上一帧的光标几何再加一次滚动偏移，
没有几何时会跳到编辑器右上角，这也会让弹层跳位。

## 改动

- 执行错误与 lint 都只写进编辑器诊断集合（波浪线 + 一个诊断弹层）。`SqlHover` 在诊断范围内返回 None，
  不再出现第二个弹层（`hover_markdown`，单测 `hover_shows_every_diagnostic...` 断言 `lsp_hover_text` 为 None）。
- 错误呈现：行号侧错误标记（gutter 装饰，组件只支持色条，不支持图标）、出错语句浅色底色、
  错误位置深色底色、行尾简短标记（错误码 + 原文首行）。鼠标移到行尾标记上会展开弹层，里面有错误码、完整信息和复制按钮。
  弹层贴着编辑器右缘向下展开（A1；A2 已改，见下）。
- 行尾标记和签名提示都用 `Floating` 元素定位：它作为 deferred 子元素，在编辑器本帧 prepaint 之后读取本帧的光标几何，
  不再叠加滚动偏移。几何缺失或目标行不在可见区时不画。
- 消息面板：点失败记录（`log:N`）会跳到错误位置；原有行为，这次没有改。

## A2 修正（针对 A1 评审）

- 错误弹层不再挂在行尾标记下方，而是独立浮层：放在出错语句整段（首个显示行到最后一个显示行，含软换行）
  下方；编辑器里放不下时放上方；都放不下时放在语句下方并越过编辑器下缘（压在结果区上，不压语句）。
  规则在 `popover_top`，单测 `the_error_popover_is_placed_clear_of_the_failing_statement`；GPUI 测试
  `the_error_popover_stays_clear_of_the_statement_and_survives_moving_onto_it` 断言多行 INSERT 的弹层边界不与语句行重叠。
- 标记与弹层之间隔着语句：离开其中一个后延迟 400ms 再判断，鼠标移到弹层上时不收起；点击标记固定展开，再点收起。
- PostgreSQL 错误码：`pg_failure` 从 `DbError::code()` 取 SQLSTATE（如 23505），经 `Failure.code` →
  `StatementStop::Error.code` → `StatementReport/StatementDto/StatementView.error_code` 带到编辑器；
  SQL Server 取服务器错误号。界面先用结构化错误码，没有再从文本识别（`exec_errors`）。
- 选区格式化被安全检查拒绝（`commentBreak`、`edgeGlue`）时也带位置：断开的注释处或粘连的选区边缘，界面显示行列并可定位。
  草稿在失败后改过时收起定位按钮（`drop_stale_format_failure`）。
- 新增面板级测试：⌘⇧/ 生产键位走 `QueryPane::command("toggle-block-comment")` 的挂起路径并一次撤销；
  格式化失败的行列说明、`query-format-jump` 按钮与 `jump-format-error` 命令。

## 仍需原生验证

- 光标移动和滚动时错误提示不闪烁、不跳位（需要录屏或连续截图）。A2 时（2026-11-20）macOS 会话仍处于锁屏
  （`ioreg` CGSSessionScreenIsLocked=true），无法截图，原生证据仍未取得。
- 错误弹层在原生窗口里与多行语句不重叠（GPUI 测试已覆盖几何，原生截图未取得）。
- 浅色/深色 SQL 配色截图对比。
- ⌘⇧/ 在 macOS 键盘布局下是被识别为 `cmd-shift-/` 还是 `cmd-?`。两种按键串都已绑定。
