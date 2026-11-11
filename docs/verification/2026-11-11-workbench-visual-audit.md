# 工作台视觉拉齐 · 逐页核对清单（T7）

- 日期：2026-11-11
- 任务：T7（R9 全局视觉拉齐、R10 不换风格、R13 快捷键），以及用户追加的“去掉自动聚合与常驻说明文字”清理
- 依据：`specs/2026-10-17-desktop-ui-ux-redesign-design.md`（S2.2 / S4.1 / S4.2 / S4.3 / S4.4 / S5.3 / S5.4 / S8.5 / S9），以及本次新增的 S12 GPUI 实现补充
- 基线：`a2f2e5e`；A2 修订基于 `8549eca`（密度对齐 S9、英文界面去中文、窄宽工具条、焦点环、Mongo/Redis 实机截图）
- 证据：Rust 测试（下文 §5）与真实 `rillsql` 原生窗口截图（`workbench-2026-11-11/`，§4）

## 1. 规范条目与落地方式

| 编号 | 规范条目 | 落地 |
| --- | --- | --- |
| A | 字号五档 11/12/13/15/18（S4.1） | `kit::TypeScale::{XS,SM,MD,LG,XL}`；app/ 与 ui/ 全部 `text_size` 与 `kit::title` 字号改用 token（原 12.5×35、11.5×21、10.5、14、16、17、20、22 全部替换）；`style_literals_follow_design_tokens` 源码扫描禁止裸字号 |
| B | 密度：紧凑 行 26 / 树 23 / 控件 26，标准 行 32 / 树 30 / 控件 30（S2.2、S9） | `Metrics::compact()` 26/23/26、`Metrics::standard()` 32/30/30（菜单行 28）、宽松 40/36/32，三档严格递增（`density_metrics_are_distinct_and_ordered`）；默认密度改为紧凑（`Shell::default`、查询页默认）；S9 为唯一来源，S12.2 只记宽松档与派生高度；密度经 `Metrics::publish` 发布为全局，子视图用 `Metrics::of(cx)`（缺省回落紧凑）；分段控件统一 `segmented_sized(.., control_h)` |
| C | 圆角：控件 4、面板/浮层 6（S4.2） | `kit::Radius::{CONTROL,PANEL}`；原 8/9/10/12/7/2 与控件上的 6 全部按角色替换；状态/环境标签 `Radius::TAG = 3`（`kit::pill`，规范 GPUI 节已记录）；组件库主题 `radius=4`、`radius_lg=6`；扫描测试禁止裸圆角，唯一字面例外收窄为活动栏指示条 `px(RAIL_INDICATOR_RADIUS)` |
| D | 发丝线分隔、面板无投影（S4.2） | 沿用 1px `line`/`line_soft` 边框；投影只在浮层（菜单、对话框、签名提示） |
| E | 浅深色独立配色（S4.3/S4.4） | 沿用两套 `Palette`；修复“保存的深色主题重启后仍是浅色”（启动后补 `ApplyTheme`） |
| F | 四态 hover/active/focus-visible(2px)/disabled（S5.3） | `kit::interactive` / `interactive_states` / `row_states`；焦点环 2px 强调色，浅深色对 surface/chrome/raised/rail/hover ≥ 3:1；焦点环改为内描边阴影（`kit::focus_ring`，inset，不填充、不压住内容，`focus_ring_outlines_the_element_without_filling_it`）；鼠标点击 `interactive` 元素不抢走网格/编辑器/树的焦点（`clicking_an_interactive_element_keeps_focus_where_it_was`） |
| G | 空 / 加载 / 错误三态（S8.5） | `kit::empty_state` + 新增 `kit::loading_state`、`kit::error_state`；网格、结构页、关系图改用 |
| H | 图标一致（S5.4） | 全部来自 `gpui_kit::assets::IconName`，统一经 `kit::icon` 取色与尺寸；未发现混用其它图标源 |

## 2. 逐页核对

“符合”= 核对时已满足；“已修正”= 本任务改动后满足。每格引用的改动见 §3。

| 页面 | A 字号 | B 密度 | C 圆角 | D 发丝线 | E 浅深 | F 四态 | G 三态 | H 图标 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 顶栏 / 欢迎页 | 已修正（顶栏品牌字 14→13；欢迎页品牌标题 18→15 `TypeScale::LG`，≤16） | 已修正（搜索框高随控件高） | 已修正（搜索框 8→4） | 符合 | 已修正（启动主题） | 已修正（搜索框 `interactive_states`） | 不适用 | 符合 |
| 活动栏 | 符合 | 符合 | 已修正（按钮 9→4；激活条 2px 线条列为例外） | 符合 | 符合 | 已修正（`interactive_states`，可 Tab） | 不适用 | 符合 |
| 对象树 | 已修正（12.5/11.5→12/11） | 已修正（紧凑树行 22→23） | 已修正（行 6→4、错误条 6） | 符合 | 符合 | 已修正（行 `row_states`，键盘焦点在树容器） | 符合（空态、读取失败带重试） | 符合 |
| 查询页 | 已修正 | 已修正（工具栏 `toolbar_h`、结果头 `bar_h`、结果标签与加载更多取控件高） | 已修正（结果标签、大纲条目、加载更多 6→4） | 符合 | 符合 | 已修正（结果标签、加载更多、消息记录 `interactive`） | 符合 | 符合 |
| 网格 | 已修正 | 已修正（行高/表头随密度：紧凑 26、标准 32；工具条 `min_h(tools_h)` 可换行，640 宽“预览变更”完整可见，`narrow_toolbar_wraps_instead_of_clipping_the_preview_button`） | 已修正 | 符合 | 符合 | 符合（组件库按钮） | 已修正（加载态 `loading_state`） | 符合 |
| 记录视图 | 已修正 | 已修正（固定像素改为 `Metrics`：头 `bar_h`、行最小高 `row_h`、宽度随行高） | 符合 | 符合 | 符合 | 符合（组件库按钮） | 符合 | 符合 |
| 结构页 | 已修正（16→15） | 已修正（表头/行高随密度） | 已修正（卡片 12/8→6） | 符合 | 符合 | 符合 | 已修正（加载态） | 符合 |
| 连接页 | 已修正（17→15） | 已修正（引擎选项高随控件） | 已修正（列表行 8→4、引擎选项 7→4） | 符合 | 符合 | 已修正（列表行 `row_states`，引擎选项/SSH 开关 `interactive_states`） | 符合 | 符合 |
| 历史页 | 已修正（17→15） | 符合 | 已修正（卡片 10→6、预览 8→6） | 符合 | 符合 | 已修正（卡片 `row_states`） | 符合 | 符合 |
| 设置页 | 已修正（20→15） | 符合 | 已修正（分段 8/6→4） | 符合 | 符合 | 已修正（分段项 `interactive_states`、更新开关） | 不适用 | 符合 |
| 导入导出 | 已修正 | 符合 | 已修正（卡片 9/8→6、行 6→4） | 符合 | 符合 | 已修正（入口卡片 `interactive_states`） | 符合 | 符合 |
| 对比 | 已修正（统计数字 20→18、标题 14/16→15） | 已修正（选项高随树行） | 已修正 | 符合 | 符合 | 已修正（选项 `interactive_states`） | 符合 | 符合 |
| 关系图 | 符合 | 符合 | 已修正（卡片 10→6） | 符合 | 符合 | 不适用（只读画布） | 已修正（加载态、错误态） | 符合 |
| 管理 | 已修正（16→15） | 符合 | 已修正（12/8→6） | 符合 | 符合 | 符合 | 符合（英文界面说明与阻断文字走 i18n，`english_admin_copy_has_no_chinese`） | 符合 |
| MongoDB | 已修正 | 已修正（顶栏、表头、行、导航行随密度） | 已修正 | 符合 | 符合 | 已修正（导航行 `interactive_states`） | 已修正（对象树不再常驻“读取对象失败”：非关系引擎列元数据为空而非失败，`column_meta_of_non_relational_engines_is_empty_not_a_failure`） | 符合 |
| Redis | 已修正（15 保留为 LG） | 已修正（键列表行随密度） | 已修正（值面板 10→6、行 6→4） | 符合 | 符合 | 已修正（键列表行 `interactive_states`） | 符合 | 符合 |
| 菜单 / 命令面板 / 确认层 / 定义查看器 / 签名提示 | 已修正（帮助标题 14→15；英文界面确认层风险/阻断/错误文字为英文，对象名保留） | 符合（菜单行 `menu_row_h`） | 已修正（菜单 9→6、项 6→4、对话框 12→6） | 符合 | 符合 | 已修正（菜单项、面板项 `row_states`；禁用项保留 not-allowed 与原因） | 符合（确认层读取/执行用加载态） | 符合 |

## 3. 改动明细

### 3.1 Token 与扫描

- `ui/kit.rs`：新增 `TypeScale`、`Radius`、`FOCUS_RING_W`、`InteractionStyle`/`interaction_style`、`interactive`、`interactive_states`、`row_states`、`loading_state`、`error_state`、`Metrics::{of, publish, toolbar_h, bar_h, tools_h, strip_h}`；紧凑密度 26/23/26；主题圆角 4/6。
- 源码扫描 `style_literals_follow_design_tokens` 的例外清单（逐条有理由）：引擎徽标字号与圆角随尺寸缩放、品牌标记圆角随尺寸缩放、`kit::title` 实现体、SQL 编辑器字号来自设置、活动栏激活指示条圆角（A2 收窄为表达式 `px(RAIL_INDICATOR_RADIUS)`，`chrome.rs` 常量）。`style_scan_rejects_reverted_literals` 证明回退任何一处（12.5、12.、8.、6.、标题 16.、`TypeScale::SM + 1.`）都会被抓到。
- 仍为固定值的高度：1px 分隔线、开关轨道 20、标签/键帽 18、内容区（定义查看器 320、Mongo 编辑器 220、原型网格 280）——它们是图形或内容区，不是行/控件高度。

### 3.2 前序任务遗留 advisory

- (a) 跳转定义：侧栏收起时打开侧栏，并把选中行滚进可见区（`model.rs` `reveal_node` + `chrome.rs` 资源管理器 `ScrollHandle::scroll_to_item`）；测试 `go_to_definition_locates_in_the_tree_and_opens_the_structure_page` 增补断言。
- (b) 记录视图改用 `Metrics`（见 §2）。
- (c) 加载更多：结果来源目标与标签当前目标不同（换库/换连接/解绑）时隐藏入口，`load_more` 也拒绝执行；测试 `load_more_is_only_offered_on_the_target_the_results_came_from`。

### 3.3 快捷键（R13）

- 既有绑定一项未改；快捷键帮助新增 `paste-cells`（表格组，Mod+V，“从焦点格粘贴 TSV”）。测试 `shortcut_help_keeps_existing_items_and_lists_grid_paste` 固定既有 18 项的分组与按键，并断言 Mod+V 在表格组唯一。

### 3.4 用户追加清理（removed: 位置 + 理由）

- removed：`app/views/grid/mod.rs` 网格底部选区统计条（`grid-stats`）及 `grid/session.rs` 的 `SelectionStats`、`StatCell`、`selection_stats`、`stats_text`、`stat_cell`、`numeric_or_text`、`format_number`，i18n `grid.stats.text`、`grid.stats.none`，相关测试 `mixed_value_stats_report_numbers_and_count_each_skipped_kind`。理由：选中格子时自动出现的合计/平均/最小/最大/跳过计数干扰视线，用户要求整体去掉；“已选 N 格”也一并去掉（没有布局依赖它，选择由高亮表达）。原用它数选区格数的测试改用 `selection_bounds` 计算。
- removed：网格工具条上常驻的查找范围不可用原因（`grid-find-scope-reason`，`grid.find.scopeQueryOnly/scopeOffline/scopeNoTable`）。理由：常驻占位；改为“查找范围”开关的悬停提示（`grid-find-scope-hint`），开关仍在不可用时禁用。键仍被提示与操作反馈使用，保留。
- removed：`chrome.rs` 历史页标题下的“只载入 SQL，不会执行。”与 SQL 预览面板底部同一句。理由：常驻说明、无状态；空态里仍保留这句作为下一步说明。
- removed：`chrome.rs` 欢迎区品牌字下的标语“轻量、快速的桌面数据库工作台”及 i18n `ui.welcome.subtitle`。理由：装饰性标语（S8.6 / 原 R1 不出现宣传口号）。
- removed：`diagram/mod.rs` 标题下常驻的只读说明（`diagram.readonly`）。理由：常驻说明；改为标题悬停提示。
- removed：`compare/mod.rs` 对比页标题下常驻的说明（`compare.dialog.subtitle`）。理由：常驻说明；改为标题悬停提示。
- removed：`chrome.rs` 连接表单 SSH 开关下的说明行（`ui.conn.sshHint`）。理由：常驻说明；改为开关悬停提示。
- 保留（有状态或错误）：网格只读原因（既有验收要求“必须画出来”，测试 `readonly_reason_is_painted_for_every_specific_denial`）、读取进度/是否读完、执行耗时与行数、消息日志、确认层风险与阻断文字、连接页底部凭据说明（原 R1 文案要求）、各空态说明。
- 同时修正：导入导出面板 6 处写死的中文提示改为 i18n 键（`ui.exchange.importNeedsTable(Hint)`、`noTableData`、`queryResult`、`noResult`、`tableLoading`、`loadedOnly`）。

#### 用户决定（2026-11-11）

用户原话：“表格列选中后的无脑聚合以及ui去掉，现在完全是干扰视线……装饰性无价值的东西也都去掉”。以下移除逐条对应这句话，保持移除、不恢复：

| 移除项 | 对应原话 |
| --- | --- |
| 网格选区自动聚合（合计/平均/最小/最大/跳过计数，`grid-stats`） | “表格列选中后的无脑聚合以及ui去掉” |
| 网格工具条常驻的查找范围不可用说明（`grid-find-scope-reason`） | “现在完全是干扰视线” |
| 装饰性/常驻说明文字：欢迎区标语、历史页“只载入 SQL”说明、关系图只读说明、对比页副标题、SSH 开关说明 | “装饰性无价值的东西也都去掉” |

### 3.5 其它

- 启动时应用保存的主题（`view.rs`），原生截图确认深色重启后即为深色。
- 新增回归测试 `record_toggle_button_opens_and_closes_the_record_view`（真实点击工具条按钮）。
- A2：英文界面不再出现中文——导入导出全部提示与来源标签（`ui.exchange.*`，`english_exchange_copy_has_no_chinese`）、管理页说明（`admin.inventory.unlisted`、`admin.note.*`）、确认层风险与管理计划句子（`ddl/english.rs` 句式表，`every_danger_and_admin_plan_sentence_has_an_english_form`）、未执行语句（`statement_message`，`skipped_statements_read_in_the_ui_language`）；壳层测试断言英文确认层风险无 CJK。
- A2：`compare`/`diagram` 的 `lines()` 与实际渲染一致（不再含已移到悬停提示的副标题/只读说明）。
- A2：连接页与欢迎页的自定义环境标签显示标签本身，不再漏出字典键 `ui.conn.env.<id>`（`custom_env_tags_never_show_a_dictionary_key`）。

## 4. 原生截图

方法：`cargo build --release --bin rillsql`，Python `ctypes` 调整真实窗口（150% DPI，客户区逻辑约 1345/885/640 × 893）、模拟点击与按键，Pillow `ImageGrab` 截取窗口。数据：临时 SQLite 演示库（customers 40 行、orders 180 行、products 20 行、1 个视图、1 个触发器）、一个 `prod` 色调自定义标签 `t7-live` 的副本库、Docker MongoDB 8.3（127.0.0.1:27017，`rillsql` 库 customers/orders）与 Redis 7.4（127.0.0.1:6379，已灌种子键）；密码临时写入系统凭据库。截图前备份本机 RillSQL 数据目录，结束后删除临时凭据并原样恢复（md5 与备份一致）。

| 组合 | 文件（`workbench-2026-11-11/`） |
| --- | --- |
| 1360 · 浅 · 标准 · 中 | query、query-object-panel、results-tabs、query-messages、grid-record、query-pinned、menu-tree、ddl-viewer、structure、diagram、export、compare、connections、history、settings、shortcut-help、danger-confirm-prod、admin、mongo（集合浏览器 orders）、redis（键浏览器 hash 键）、welcome、focus-ring（Tab 到“查询 1”标签，内描边焦点环）（后缀 `-1360-light-standard-zh`） |
| 1360 · 深 · 紧凑 · 英 | 同上 22 张（后缀 `-1360-dark-compact-en`） |
| 900 · 浅 · 紧凑 · 英 / 900 · 深 · 标准 · 中 | tree-open、query |
| 640 · 浅 · 标准 · 中 / 640 · 深 · 紧凑 · 英 | tree-open（侧栏覆盖）、query（侧栏收起；网格工具条换行，“预览变更”完整可见） |

## 5. 遗留与未覆盖（如实记录）

A1 记录的偏离（Mongo/Redis 未截到、英文界面残留中文、640 工具条裁切、快捷键帮助未滚到表格组、紧凑控件高大于标准）在 A2 均已修正并重截，不再列为遗留。仍存在的事实：

1. 确认层与管理计划的英文是显示时按句式表翻译（`ddl/english.rs`），后端计划仍生成中文；句式表外的文本（如数据库原始错误）原样透出。
2. 管理页：恢复的管理标签在连接建立前探测一次目录会话，之后不再重探，因此截图中即使连接已绿点，仍显示“没有连接到这个引擎，预览不会执行”；界面上也没找到打开管理页的入口（只有恢复的标签与内部 `open-admin`）。非视觉问题，本任务未改。
3. 键盘 Tab：焦点停在工作台未挂到元素树的根句柄上时（例如 Ctrl+, 进入设置页后），Tab 不移动焦点；从对象树等已挂载的焦点出发正常。焦点环截图从对象树 Tab 出发。
4. 自定义环境标签在连接页/欢迎页显示标签 id（壳层没有读取 `env_tags` 的显示名），颜色按内置色调规则。
5. 英文网格工具条里搜索框占位与查找范围开关同为 “Find in loaded rows”（`ui.grid.searchPlaceholder` 与 `grid.find.loaded`），措辞重复，非中文残留。
6. 四态覆盖：自定义可点元素按 `interactive`/`interactive_states`/`row_states` 收敛，测试断言焦点环 2px、浅深色对比 ≥ 3:1、可用元素是 Tab 停靠点、禁用元素不可聚焦、点击不抢焦点；不声称每个元素都逐一有测试。
7. 调试版 `target/debug/rillsql.exe` 在本机启动即主线程栈溢出（`thread 'main' has overflowed its stack`）；发布版正常，截图全部来自发布版。未验证该问题是否在基线 `a2f2e5e` 已存在。

## 6. 复现

```bash
cargo test --manifest-path src-tauri/Cargo.toml --locked -- --test-threads=1
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked
cargo build --manifest-path src-tauri/Cargo.toml --bin rillsql --locked
```
