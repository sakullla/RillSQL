# UI 捕获

[浏览图片画廊](index.html) · [机器可读清单](capture-ui-manifest.json)

按场景、主题、宽度或关键词浏览界面，支持重置、匹配数量和原图查看。

执行：

```sh
RILLSQL_PREVIEW_URL=http://localhost:1420 node scripts/capture-ui.mjs --theme light --size 1360 --only welcome,connections,query,data-grid,command-palette,settings
```

`--theme light|dark|all`、`--size 1360,900,640`、`--only 'connection-*'` 可缩小范围，`--list` 列出场景。RILLSQL_CAPTURE_OUT 可指定输出目录。预览地址默认 http://localhost:1420。

本次生成 6 张图片，0 项检查失败。合成 IPC 数据仅用于界面检查，不构成真实数据库、SSH 或平台验收。

| 场景 | 图片 |
| --- | --- |
| 欢迎页与资源管理器 | [light 1360](welcome-light-1360.png) |
| 连接管理 | [light 1360](connections-light-1360.png) |
| 查询结果 | [light 1360](query-light-1360.png) |
| 数据表格 | [light 1360](data-grid-light-1360.png) |
| 命令面板 | [light 1360](command-palette-light-1360.png) |
| 设置 | [light 1360](settings-light-1360.png) |
