# UI 捕获

[浏览图片画廊](index.html) · [机器可读清单](capture-ui-manifest.json)

按场景、主题、宽度或关键词浏览界面，支持重置、匹配数量和原图查看。

执行：

```sh
RILLSQL_PREVIEW_URL=http://localhost:1420 node scripts/capture-ui.mjs --theme all
```

`--theme light|dark|all`、`--size 1360,900,640`、`--only 'connection-*'` 可缩小范围，`--list` 列出场景。RILLSQL_CAPTURE_OUT 可指定输出目录。预览地址默认 http://localhost:1420。

本次生成 114 张图片，0 项检查失败。合成 IPC 数据仅用于界面检查，不构成真实数据库、SSH 或平台验收。

| 场景 | 图片 |
| --- | --- |
| 欢迎页与资源管理器（已有连接时内容区不列连接卡片，对象树仍是连接名单） | [light 1360](welcome-light-1360.png) · [dark 1360](welcome-dark-1360.png) · [light 900](welcome-light-900.png) · [dark 900](welcome-dark-900.png) · [light 640](welcome-light-640.png) · [dark 640](welcome-dark-640.png) |
| 连接管理（已连接、每次询问密码、SQLite 文件三种卡片） | [light 1360](connections-light-1360.png) · [dark 1360](connections-dark-1360.png) · [light 900](connections-light-900.png) · [dark 900](connections-dark-900.png) · [light 640](connections-light-640.png) · [dark 640](connections-dark-640.png) |
| 编辑连接（直连必需项，高级选项按此连接展开） | [light 1360](connection-editor-light-1360.png) · [dark 1360](connection-editor-dark-1360.png) · [light 900](connection-editor-light-900.png) · [dark 900](connection-editor-dark-900.png) · [light 640](connection-editor-light-640.png) · [dark 640](connection-editor-dark-640.png) |
| SSH 隧道（单跳隧道和已信任的主机密钥） | [light 1360](connection-ssh-light-1360.png) · [dark 1360](connection-ssh-dark-1360.png) · [light 900](connection-ssh-light-900.png) · [dark 900](connection-ssh-dark-900.png) · [light 640](connection-ssh-light-640.png) · [dark 640](connection-ssh-dark-640.png) |
| 查询结果（选择执行目标后的查询结果） | [light 1360](query-light-1360.png) · [dark 1360](query-dark-1360.png) · [light 900](query-light-900.png) · [dark 900](query-dark-900.png) · [light 640](query-light-640.png) · [dark 640](query-dark-640.png) |
| 数据表格（orders 已读取的三行，含 NULL） | [light 1360](data-grid-light-1360.png) · [dark 1360](data-grid-dark-1360.png) · [light 900](data-grid-light-900.png) · [dark 900](data-grid-dark-900.png) · [light 640](data-grid-light-640.png) · [dark 640](data-grid-dark-640.png) |
| 表结构（orders 的字段、类型和默认值） | [light 1360](structure-light-1360.png) · [dark 1360](structure-dark-1360.png) · [light 900](structure-light-900.png) · [dark 900](structure-dark-900.png) · [light 640](structure-light-640.png) · [dark 640](structure-dark-640.png) |
| 导出为 CSV（只导出已读取的行和全表两种范围） | [light 1360](export-light-1360.png) · [dark 1360](export-dark-1360.png) · [light 900](export-light-900.png) · [dark 900](export-dark-900.png) · [light 640](export-light-640.png) · [dark 640](export-dark-640.png) |
| 导入 CSV（CSV 列对应到 orders 字段，确认后才写入） | [light 1360](import-light-1360.png) · [dark 1360](import-dark-1360.png) · [light 900](import-light-900.png) · [dark 900](import-dark-900.png) · [light 640](import-light-640.png) · [dark 640](import-dark-640.png) |
| 函数定义（函数的只读定义与“复制到新查询”） | [light 1360](definition-light-1360.png) · [dark 1360](definition-dark-1360.png) · [light 900](definition-light-900.png) · [dark 900](definition-dark-900.png) · [light 640](definition-light-640.png) · [dark 640](definition-dark-640.png) |
| 关系图（基表卡片与外键连线，只读） | [light 1360](diagram-light-1360.png) · [dark 1360](diagram-dark-1360.png) · [light 900](diagram-light-900.png) · [dark 900](diagram-dark-900.png) · [light 640](diagram-light-640.png) · [dark 640](diagram-dark-640.png) |
| 命令面板（表、连接和命令） | [light 1360](command-palette-light-1360.png) · [dark 1360](command-palette-dark-1360.png) · [light 900](command-palette-light-900.png) · [dark 900](command-palette-dark-900.png) · [light 640](command-palette-light-640.png) · [dark 640](command-palette-dark-640.png) |
| 键盘快捷键（全局、SQL 编辑器和表格快捷键） | [light 1360](shortcuts-light-1360.png) · [dark 1360](shortcuts-dark-1360.png) · [light 900](shortcuts-light-900.png) · [dark 900](shortcuts-dark-900.png) · [light 640](shortcuts-light-640.png) · [dark 640](shortcuts-dark-640.png) |
| 对比库（仅一侧拥有的表、列差异与两侧脚本） | [light 1360](compare-light-1360.png) · [dark 1360](compare-dark-1360.png) · [light 900](compare-light-900.png) · [dark 900](compare-dark-900.png) · [light 640](compare-light-640.png) · [dark 640](compare-dark-640.png) |
| 关闭守卫（未执行草稿的标签关闭确认） | [light 1360](close-guard-light-1360.png) · [dark 1360](close-guard-dark-1360.png) · [light 900](close-guard-light-900.png) · [dark 900](close-guard-dark-900.png) · [light 640](close-guard-light-640.png) · [dark 640](close-guard-dark-640.png) |
| 标题栏主题菜单（标题栏外观菜单：浅色 / 深色 / 跟随系统） | [light 1360](title-theme-light-1360.png) · [dark 1360](title-theme-dark-1360.png) · [light 900](title-theme-light-900.png) · [dark 900](title-theme-dark-900.png) · [light 640](title-theme-light-640.png) · [dark 640](title-theme-dark-640.png) |
| 查询历史（成功、失败，以及原连接已删除） | [light 1360](history-light-1360.png) · [dark 1360](history-dark-1360.png) · [light 900](history-light-900.png) · [dark 900](history-dark-900.png) · [light 640](history-light-640.png) · [dark 640](history-dark-640.png) |
| 保存的查询（保存的名称与 SQL，旧连接需要重选执行目标） | [light 1360](history-saved-light-1360.png) · [dark 1360](history-saved-dark-1360.png) · [light 900](history-saved-light-900.png) · [dark 900](history-saved-dark-900.png) · [light 640](history-saved-light-640.png) · [dark 640](history-saved-dark-640.png) |
| 设置（外观、数据读取、执行与复制、本机数据和关于） | [light 1360](settings-light-1360.png) · [dark 1360](settings-dark-1360.png) · [light 900](settings-light-900.png) · [dark 900](settings-dark-900.png) · [light 640](settings-light-640.png) · [dark 640](settings-dark-640.png) |
