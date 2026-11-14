# 快速打开的行为地图（T5）

## 1. 检索范围与来源

`WorkbenchModel::palette_entries()` 把五类来源合成一份清单，顺序与分组固定：

| 类型（`group`） | 来源 | 行内上下文（`detail`） | 行内类型标注 |
| --- | --- | --- | --- |
| `command` | `crate::app::COMMANDS` | `command_detail`（如当前对象 / 选区） | `shell.paletteGroup.command` |
| `connection` | `connections.rows` | `引擎 · 库`（`connection_detail`） | `shell.paletteGroup.connection` |
| `database` | `nav_source` 里已读到的库（`database_entries`） | 连接名 | `shell.paletteGroup.databases` |
| `table` / `view` | 目录快照 `catalog`（`catalog_entries`） | `库.模式 · 连接`（`object_detail`） | `shell.paletteGroup.{tables,views}` |
| `saved` | `history.saved`（`saved_entries`） | `连接 · 库`（`saved_detail`） | `shell.paletteGroup.savedQueries` |

- 表与视图分成两类：以前两者共用“表 / 视图”一种标注，下一步不同却看不出来；现在类型标注与分组标题读同一份键（`palette_kind_key`），不会一处改了另一处没改。
- 保存的查询在面板打开时补读一次本机历史库（`PendingJob::RefreshHistory`），只读历史库，不连业务库、不执行 SQL。
- **从未加载的对象不会出现**：库只列 `nav_source` 中已读到的库，对象只列 `catalog` 快照里的表 / 视图。连接本身始终可搜（它不依赖目录）。

## 2. 命中与排序

- 名字命中优先，其次是上下文命中：`palette_score` = 名字分 ×2，名字不中才看 `detail`。
- 单条文本的分级沿用既有 `match_score`：前缀 3、包含 2、按序散列（fuzzy 子序列）1、不命中 0。
- 结果按类型先后（命令 → 连接 → 库 → 表 → 视图 → 保存的查询）分块，块内按分数、再按原始序号稳定排序。
- 两个前缀约定不变：`>` 只留命令；`Mod+P`（快速打开）只留能“打开数据”的表与视图（本轮把它从 `group == "table"` 改成收 `table | view`，否则拆分类型后视图会被漏掉）。

## 3. 每类选中之后做什么

| 行 | id | 动作 |
| --- | --- | --- |
| 表 / 视图 | `open-table:{spec}` | 打开数据页，不执行 SQL、不动草稿 |
| 保存的查询 | `palette-saved:{id}` | `history::plan_from_saved` → `apply_load`：写草稿、带原连接与库，**不执行** |
| 连接 | `palette-connection:{id}` | 展开并聚焦对象树，键盘当前行落在连接行；不跳连接管理页 |
| 库 | `palette-database:{id}|{db}` | 展开连接与库、按需读 schema、键盘当前行落在库节点 |
| 命令 | `{command.id}` | 走原命令分发，行为不变 |

定位失败不假装成功：库里没有这个节点时给 `ui.palette.databaseMissing` + 库名；连接在面板打开后被删掉时给 `ui.palette.connectionMissing` + 连接名。

## 4. 按需连接续接（复用 T3）

`on_demand_intent` 现在也覆盖快速打开的三种目标：

- `open-table:` → `ConnectAction::OpenData` + 排队时记下的 `spec`；连上后 `run_connect_intents` 把 `object_spec` 还原成这条 spec 再执行一次 `tree-open-data`，不重新读当前焦点。
- `palette-connection:` / `palette-database:` → `ConnectAction::Run` + `replay`：连上后重放这条定位命令。
- 保存的查询不建连（它只是写草稿）。

在途连接复用同一条 pending 连接，不会因为连点两下排两次；连接失败时意图留在重试入口里（`fail_pending_intents`），不丢用户想打开的对象。

## 5. 空态分流

`palette_empty_key()`：

| 情况 | 键 | 面板表现 |
| --- | --- | --- |
| 有目录 / 树读取失败 | `ui.palette.error` | 失败文案 + 具体原因一行 + 重试按钮 |
| 快速打开（只表 / 视图）且没有已加载对象 | `ui.explorer.noMatch` | 先连接并读取对象 |
| 搜索词为空 | `ui.palette.empty` | 没有匹配项 |
| 有搜索词但没命中 | `ui.palette.noMatch` | 换个更短的关键词 |

“目录失败”与“无命中”是两条不同文案、不同动作：前者带原因和重试，后者只是这一次搜索没有结果，清空搜索词即可恢复。

## 6. 快捷键来源

行内快捷键取 `menus::command_chord(id)`：先查 `COMMANDS` 的 `chord`，再退回 `SHORTCUTS`。这份表同时是菜单项与快捷键帮助（`chrome.rs` 的 `SHORTCUTS` 遍历）的来源，所以面板上显示的键不会和菜单 / 帮助页对不上。非命令条目（连接、库、表、视图、保存查询）一律没有快捷键，不借别人的键。

## 7. 未解决事实

- 原生键盘走查与截图未取得（无原生窗口通道），见 `README.md`。
- 跨引擎（PostgreSQL / MySQL / MariaDB）“先连接再打开”只有模型级与 SQLite 真实会话证据；没有 docker fixture。
- `shell.paletteGroup.table`（旧的“表 / 视图”合并文案）现在没有引用点，保留在字典里未删除，避免与其它任务正在改的字典区块冲突。
