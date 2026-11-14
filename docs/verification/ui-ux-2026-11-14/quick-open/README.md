# 命令面板 / 快速打开统一搜索（T5）验证目录

- 日期：2026-11-14
- 任务：T5「在现有命令面板/快速打开中统一搜索连接、库、表、视图、保存查询和命令，并复用按需连接续接」
- 覆盖需求：R3（键盘与密集列表）、R6（对象浏览与快速打开端到端）、R8（命令与快捷键一致）
- 覆盖规范：S8.5（无数据 / 无命中 / 错误语义分开）、S5.2（listbox 行与选中底）、S12.4（三态）、S13（命令与快捷键单一来源）
- 改动范围（全部在 `src-tauri/src/app/shell/`，i18n 只追加键）：
  - `model.rs`：`PaletteEntry` 增加 `shortcut` 与类型分组；`palette_kind_key`、`database_entries`、`saved_entries`、`connection_detail`、`saved_detail`、`palette_error`、`palette_empty_key` 的“无命中 / 目录失败”分流；`palette_focus_connection`、`palette_focus_database`、`palette_load_saved` 三条动作；`on_demand_intent` 增加 `palette-connection:` / `palette-database:` / `open-table:` 的按需连接意图
  - `menus.rs`：`command_chord`（COMMANDS → SHORTCUTS 同源取键，不另编一份）
  - `chrome.rs`：每行画出上下文与类型标注，空态按原因分流，图标按类型
  - `tests.rs`：7 个新用例 + 2 个既有快速打开用例的类型断言
  - `src-tauri/src/i18n/mod.rs`：追加 `shell.paletteGroup.{tables,views,databases,savedQueries}`、`ui.palette.noMatch`、`ui.palette.databaseMissing`、`ui.palette.connectionMissing`

## 文件

| 文件 | 内容 |
| --- | --- |
| `behavior-map.md` | 条目来源、排序与命中规则、每类的动作、按需连接续接、空态分流、快捷键来源 |
| `test-receipts.md` | 本轮实际运行的命令与结果、用例清单、lint 对照 |

## 证据类型（请勿混读）

- **源码级 + 模型断言**：条目集合、排序、动作分发、按需连接排队与重放、空态文案键，均为 `WorkbenchModel` 上的断言，没有经过 UI。
- **headless 渲染级**：GPUI 测试窗口 640×800 下读出 `pal-kind-*` / `pal-detail-*` 元素的无障碍名称与占位尺寸，以及面板不越界。对象索引来自真实 SQLite 会话，保存的查询来自真实历史库。
- **未取得：原生窗口与截图**。本任务运行环境没有原生窗口和截屏通道，也没有可用的“纯键盘原生”录屏，所以**没有**原生像素或原生键盘证据；本目录不用历史截图替代。最终 candidate 上的一次原生键盘走查（Mod+K → 输入 → ↑↓ → Enter 打开数据 / 载入查询 / 聚焦树）由 Delivery Owner 取得。
- **未取得：PostgreSQL/MySQL 端到端**。跨引擎的“先连接再打开”只在模型层与 SQLite 真实会话上验证，未起 docker fixture，原因见 `behavior-map.md` 的「未解决事实」。
