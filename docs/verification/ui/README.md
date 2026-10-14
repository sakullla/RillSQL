# UI 捕获

[浏览图片画廊](index.html) · [机器可读清单](capture-ui-manifest.json)

按场景、主题、宽度或关键词浏览界面，支持重置、匹配数量和原图查看。

执行：

```sh
RILLSQL_PREVIEW_URL=http://localhost:1420 node scripts/capture-ui.mjs
```

`--theme light|dark|all`、`--size 1360,900,640`、`--only 'connection-*'` 可缩小范围，`--list` 列出场景。RILLSQL_CAPTURE_OUT 可指定输出目录。预览地址默认 http://localhost:1420。

本次生成 72 张图片，0 项检查失败。合成 IPC 数据仅用于界面检查，不构成真实数据库、SSH 或平台验收。

| 场景 | 图片 |
| --- | --- |
| 欢迎页与资源管理器 | [light 1360](welcome-light-1360.png) · [dark 1360](welcome-dark-1360.png) · [light 900](welcome-light-900.png) · [dark 900](welcome-dark-900.png) · [light 640](welcome-light-640.png) · [dark 640](welcome-dark-640.png) |
| 连接管理 | [light 1360](connections-light-1360.png) · [dark 1360](connections-dark-1360.png) · [light 900](connections-light-900.png) · [dark 900](connections-dark-900.png) · [light 640](connections-light-640.png) · [dark 640](connections-dark-640.png) |
| 编辑连接 | [light 1360](connection-editor-light-1360.png) · [dark 1360](connection-editor-dark-1360.png) · [light 900](connection-editor-light-900.png) · [dark 900](connection-editor-dark-900.png) · [light 640](connection-editor-light-640.png) · [dark 640](connection-editor-dark-640.png) |
| SSH 隧道 | [light 1360](connection-ssh-light-1360.png) · [dark 1360](connection-ssh-dark-1360.png) · [light 900](connection-ssh-light-900.png) · [dark 900](connection-ssh-dark-900.png) · [light 640](connection-ssh-light-640.png) · [dark 640](connection-ssh-dark-640.png) |
| 查询结果 | [light 1360](query-light-1360.png) · [dark 1360](query-dark-1360.png) · [light 900](query-light-900.png) · [dark 900](query-dark-900.png) · [light 640](query-light-640.png) · [dark 640](query-dark-640.png) |
| 数据表格 | [light 1360](data-grid-light-1360.png) · [dark 1360](data-grid-dark-1360.png) · [light 900](data-grid-light-900.png) · [dark 900](data-grid-dark-900.png) · [light 640](data-grid-light-640.png) · [dark 640](data-grid-dark-640.png) |
| 表结构 | [light 1360](structure-light-1360.png) · [dark 1360](structure-dark-1360.png) · [light 900](structure-light-900.png) · [dark 900](structure-dark-900.png) · [light 640](structure-light-640.png) · [dark 640](structure-dark-640.png) |
| 导出 CSV | [light 1360](export-light-1360.png) · [dark 1360](export-dark-1360.png) · [light 900](export-light-900.png) · [dark 900](export-dark-900.png) · [light 640](export-light-640.png) · [dark 640](export-dark-640.png) |
| 命令面板 | [light 1360](command-palette-light-1360.png) · [dark 1360](command-palette-dark-1360.png) · [light 900](command-palette-light-900.png) · [dark 900](command-palette-dark-900.png) · [light 640](command-palette-light-640.png) · [dark 640](command-palette-dark-640.png) |
| 键盘快捷键 | [light 1360](shortcuts-light-1360.png) · [dark 1360](shortcuts-dark-1360.png) · [light 900](shortcuts-light-900.png) · [dark 900](shortcuts-dark-900.png) · [light 640](shortcuts-light-640.png) · [dark 640](shortcuts-dark-640.png) |
| 查询历史 | [light 1360](history-light-1360.png) · [dark 1360](history-dark-1360.png) · [light 900](history-light-900.png) · [dark 900](history-dark-900.png) · [light 640](history-light-640.png) · [dark 640](history-dark-640.png) |
| 设置 | [light 1360](settings-light-1360.png) · [dark 1360](settings-dark-1360.png) · [light 900](settings-light-900.png) · [dark 900](settings-dark-900.png) · [light 640](settings-light-640.png) · [dark 640](settings-dark-640.png) |
