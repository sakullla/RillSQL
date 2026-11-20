# 2026-11-08 桌面体验改造：原生验收报告

日期：2026-11-20。候选基线：`85fedee6`，加上本次返修（T10-scroll-layout A3）的改动。
截图都在 [`native/`](native/) 目录下。下文“截图”一栏的文件名都相对这个目录。

## 取证方式

- 本机 macOS 会话处于锁屏状态，而且约定不在用户桌面上做 GUI 自动化，所以没有在 macOS 上截图。
- 原生应用实际运行在 colima（macOS Virtualization.Framework，aarch64）的 Linux 容器 `rill-evidence` 里：
  用 Xvfb `:99`（1360×900×24）+ Mesa Vulkan（软件渲染）运行 `cargo build --bin rillsql` 编译出的同一份
  GPUI 程序（`/target/debug/rillsql`）。界面语言 `zh_CN.UTF-8`，数据目录用 `RILLSQL_DEBUG_DATA_DIR=/data` 与本机隔离。
- 用 `xdotool` 操作鼠标键盘，用 ImageMagick `import -window root` 截取整屏，用 `xclip` 读剪贴板。
- 数据库是 `docker/compose.yaml` 里的夹具：PostgreSQL 16、MySQL 8.4、Redis 7 单机、Redis 7 三主集群（7001–7003）。
  为了让列表出现溢出滚动，取证期间临时往单机 Redis 写了约 5500 个键，往 PostgreSQL 写了宽表 `shop.wide_metrics` 和 60 多张
  `archive_table_*`。这些数据只用于取证；取证结束后已删除临时 Redis 键，之后执行了 `compose down`。
- 截图里修复前和修复后的状态，用的是同一流程分别编译出的二进制，修复后重新编译并重启应用。
- 限制：这是 Linux/X11 上的原生 GPUI 渲染，不是 macOS Metal 渲染。字体是容器里的 Noto CJK，
  快捷键显示为 Linux 写法（Ctrl 而不是 ⌘）。布局、滚动条、浮层层级和交互逻辑在各平台共用同一份代码，
  但 macOS 的字形、抗锯齿和 Metal 合成没有经过原生验证。Windows 也没有原生证据。

## 逐项结论

| 需求 | 结论 | 截图 / 证据 |
|---|---|---|
| R1 对象树搜索：清空后恢复 | 通过 | `R01-tree-search-typed.png`、`R01-tree-search-cleared.png` |
| R2 新建表 | 通过（取证中发现并修复两个缺陷，见下一节） | `R02-create-table-empty-name-blocked.png`（空表名就地阻断）、`R02-mysql-fractional-seconds-default-blocked.png`（datetime(3) 配裸 CURRENT_TIMESTAMP，提交前拦下）、`R02-mysql-current-timestamp-3-accepted.png`、`R02-collation-filtered-by-charset-latin1.png`（选 latin1 后排序规则只剩 latin1_*）、`R02-create-table-preview-sql.png`、`R02-created-table-structure-after-fix-editable-defaults.png`（在 MySQL 8.4 上实际建表成功） |
| R3 连接级和库级概览 | 通过 | `R03-overview-pg-connection-databases.png`、`R03-overview-pg-database-schemas.png`、`R03-overview-pg-schema-tables-scrollbar.png` |
| R4 Redis | 通过；已知限制见“交付限制” | `R04-redis-namespace-tree.png`（键树）、`R04-redis-template-view.png`（模板归并）、`R15-R04-redis-key-list-scrollbar-scan-progress-dark.png`（扫描进度“已读取 5536 / 共 5536 · 已读完”）、`R04-redis-slowlog.png`、`R04-redis-cluster-key-detail-slot-node.png`（集群键详情显示槽 143、节点 127.0.0.1:7001）、`R04-redis-cluster-nodes.png`（3 个主节点的槽范围与键数） |
| R5 打开文件入口 | 通过 | `R05-topbar-open-file-entry.png`（顶栏）、`R05-welcome-single-open-file-card.png`（欢迎页只有一张“打开文件…”卡片，Ctrl+O）、`R05-connections-page-open-file-entry.png`（连接页）、`R05-editor-toolbar-open-file-ctrl-o.png`（编辑器工具栏，同一命令、同一快捷键）。系统文件对话框在 Xvfb 里没有桌面门户，所以没有截图；`.sql`、SQLite 文件和无法识别文件的路由判定由单测覆盖。 |
| R6 快捷键面板 | 通过（前一轮取证中修复：行尾错误标记压在面板上） | `R06-shortcut-search-comment.png`、`R06-R15-shortcut-panel-short-window-scrolled-light.png`、`R15-shortcut-panel-scrollbar-dark.png`；修复前：`R06-defect-error-marker-over-shortcut-panel-before-fix.png` |
| R7 新增行默认值、单元格日历 | 通过 | `R07-new-row-defaults.png`、`R07-cell-calendar.png` |
| R9 错误提示 | 通过（前一轮取证中修复：两个弹层同时出现） | `R09-error-gutter-underline-eol-marker.png`、`R09-squiggle-hover-popover.png`、`R09-single-error-popover-after-fix.png`、`R09-no-flicker-caret-moves-4-frames.png`（光标移动时连续 4 帧，提示位置不变，没有闪烁）；修复前：`R09-defect-two-popovers-before-fix.png` |
| R10 查找栏 | 通过 | `R10-find-bar-collapsed-wide-table.png`、`R10-find-bar-expanded.png` |
| R12 标签面板 | 通过 | `R12-tag-panel-usage-counts.png`（每个标签的使用数）、`R12-tag-panel-linked-connections.png`（展开后看到关联连接，可逐个移除）、`R12-tag-duplicate-name-inline-error.png`（重名时就地报错）、`R12-new-tag-appears-in-filter-bar.png`（新标签“演示”立即出现在筛选栏）、`R12-delete-tag-confirm-shows-count.png`（删除时提示“2 条连接将变为无标签”；截图后点了取消） |
| R13 结构对比和数据对比 | 通过（取证中发现并修复结构脚本的默认值缺陷） | `R13-structure-compare-summary-and-tree.png`、`R13-structure-compare-changed-table-side-by-side.png`、`R13-data-compare-row-level-diff.png`；缺陷：`R13-data-compare-tables-and-script-defects.png`（`DEFAULT new` 没有引号）；修复后：`R13-structure-script-after-fix-quoted-default.png`（`DEFAULT 'new'`） |
| R14 启动页右键菜单 | 通过 | `R14-welcome-connection-context-menu.png`（与连接管理一致：新建查询、编辑配置、复制、测试连接、移动到分组、删除配置）、`R14-welcome-recent-run-context-menu.png`（在新查询中打开、复制 SQL、在历史中查看、再次执行）；`R14-copy-sql-clipboard.txt` 是点“复制 SQL”后从剪贴板读出的完整 4 行 SQL |
| R15 滚动条 | 通过 | 宽表滚到最右：`R15-wide-table-scrolled-right-light.png` / `-dark.png`；对象树：`R15-object-tree-scrollbar-light.png` / `-dark.png`；结果表格：`R15-result-grid-scrollbars-light.png` / `-dark.png`；快捷键面板：`R06-R15-shortcut-panel-short-window-scrolled-light.png` / `R15-shortcut-panel-scrollbar-dark.png`；Redis 键列表溢出：`R15-redis-key-list-scrollbar-light.png` / `R15-R04-redis-key-list-scrollbar-scan-progress-dark.png`。滚动条都可见，不遮挡末列和快捷键键帽，数值右对齐，末列与右边缘之间留有间距。 |
| R16 浅色和深色配色 | 通过 | `R16-syntax-light.png`、`R16-syntax-dark.png` |
| R17 解锁结果编辑 | 通过 | `R17-unlocked-result-editing.png` |
| R19 常用字段模板 | 通过 | `R19-common-field-templates-menu.png`（时间戳、软删除、状态与备注、UUID、管理常用字段…）、`R19-template-inserted-live-sql.png`（插入 created_at/updated_at 后实时建表 SQL 同步更新） |
| R20 空字符串和 NULL | 通过（前一轮取证中修复：新增行里空字符串默认值显示为“默认:”） | `R20-empty-string-vs-null.png` |
| R22 方言边界 | 通过 | 适配器边界扫描测试 `production_code_respects_the_adapter_boundary` 白名单为空，`cargo test` 通过 |

## 取证中发现并已修复的缺陷

前一轮（同一职责，被轮次上限中断）已修复：

1. R6：行尾错误标记和错误弹层是 deferred 绘制，会压在快捷键面板、命令面板、模态框上。外壳有浮层时，
   设置全局 `ShellOverlayOpen`，编辑器的浮动元素在这一帧不绘制（`ui/editor_proto.rs`、`app/shell/chrome.rs`）。
   GPUI 测试：`error_lenses_stay_under_shell_overlays`。
2. R9：鼠标从波浪线移到行尾标记时，编辑器自己的诊断弹层收不到离开事件，和错误弹层同时显示。
   现在打开错误弹层时会先收起诊断弹层，同一时刻只显示一个。
3. R20：新增行里空字符串默认值写成“默认: 空”（`app/views/grid/session.rs`，测试 `an_empty_string_default_reads_as_empty_in_new_rows`）。
4. 概览：按需连接失败后，经密码框连上时自动重读；虚拟列表行改为满宽，与列头对齐（`app/shell/overview.rs`、`app/views/overview/mod.rs`）。
5. 对象树：没有当前行时按回车会无限递归、撑爆主线程栈，现在不做任何事（`app/shell/model.rs`）。
6. 快捷键面板搜索框的示例按键改为按平台显示（macOS ⌘/，其他平台 Ctrl+/）。

本轮（A3）在取证中发现并修复：

7. MySQL 8 会给表达式默认值（`CURRENT_TIMESTAMP` 等）在 `EXTRA` 里标 `DEFAULT_GENERATED`。结构页读列时只检查是否包含 `generated`，
   结果把 `created_at`/`updated_at` 误判为生成列：显示“生成”徽标、锁定，默认值也不能编辑（`R02-created-table-structure-before-fix.png`）。
   改为与网格共用 `extra_marks_generated`，只认 VIRTUAL/STORED/PERSISTENT GENERATED（`adapters/mysql/ddl_read.rs`、`grid_read.rs`）。
   修复后见 `R02-created-table-structure-after-fix-editable-defaults.png`。
8. MySQL 的 `information_schema.COLUMN_DEFAULT` 返回不带引号的文本字面量（例如 `new`），结构对比和转储把它原样拼成 `DEFAULT new`，
   这是无效的 SQL。新增 `ddl::sql::existing_default_sql`：把方言判定为文本字面量的值按列类型补上引号；表达式、数字、NULL、
   已带引号或括号的写法原样保留。对比（`compare/read.rs`）和转储（`dump/dump.rs`）都改为经过这个函数。
   测试：`existing_defaults_become_valid_default_clauses`。
9. 新增的阻断原因补了英文（`ddl/english.rs`、`adapters/mysql/english.rs`）。英文覆盖扫描测试 `every_chinese_literal_on_ddl_paths_has_an_english_form` 原先失败，现已通过。
   新测试断言嵌套的方言说明也会翻成英文。MySQL 小数秒的说明文字改为以“列类型 …”开头，避免和通用句型串配。
   因此 `R02-mysql-fractional-seconds-default-blocked.png` 里的提示是改动前的措辞，其余内容一致。

## 三个非阻断评审项（Delivery P2/P3）的处理

- P2(a) MySQL 小数秒默认值：接受 `CURRENT_TIMESTAMP(n)`（n 为 0–6）和 `(CURRENT_DATE)`。
  datetime(n)/timestamp(n) 的当前时间默认值，小数位数必须和列一致，否则提交前就拦下。候选里不再给出裸写的 `CURRENT_DATE`。
  测试：`adapters::mysql::types::default_expression_tests::fractional_seconds_and_current_date_follow_mysql_rules`、
  `ddl::tests::mysql_create_checks_fractional_seconds_and_charset_collation_before_submit`。
  原生取证：`R02-mysql-fractional-seconds-default-blocked.png` → `R02-mysql-current-timestamp-3-accepted.png`，
  在 MySQL 8.4 上实际建表成功（`R02-created-table-structure-after-fix-editable-defaults.png`）。
- P2(b) 排序规则随字符集过滤：`TableOptionSpec::charset_collations`、`collations_for` 和 `collation_problem` 已实现，
  建表计划会阻断不匹配的组合；界面换字符集时，会清掉不适用的排序规则。测试同上。原生截图：`R02-collation-filtered-by-charset-latin1.png`。
- P3 `select_sample_sql`：主代码里不再出现 `LIMIT` 兜底。引擎识别不出来时返回 None，菜单不提供“在查询中打开”。
  边界扫描新增 `} LIMIT `、`LIMIT {}`、` RETURNING `、`ON UPDATE CURRENT_TIMESTAMP` 等片段，自测里也注入了对应的违例。

## A4 返修（针对 A3 评审）

- P1：`.gitignore` 增加本目录的重新包含规则，报告与 `native/` 截图随检查点一起提交（此前被 `docs/verification/*` 忽略）。
- P2：MySQL 8 的表达式默认值读回不带括号（`(CURRENT_DATE)` 读回 `curdate()`，EXTRA 标 `DEFAULT_GENERATED`），
  对比和转储曾拼出非法的 `DEFAULT curdate()`。结构快照读列时改经 `adapters::mysql::types::reported_default` 规整：
  DEFAULT_GENERATED 的表达式补外层括号（当前时间写法保持原样）；非 NULL 的空串保留为 `''`，不再与“无默认值”混同；
  含单引号的文本字面量（如 `it's`）补引号并翻倍。测试：`reported_defaults_become_executable_default_clauses`、
  `reported_defaults_parse_into_the_structure_states`。
- P3：`ShellOverlayOpen` 改为按窗口记录，一个窗口的浮层不再藏掉其他窗口的错误标记。
  GPUI 测试：`a_shell_overlay_hides_error_lenses_only_in_its_own_window`。
- A4 尝试在 Linux/Xvfb 中用改动前（`c5f764f`）与改动后的二进制录制 R9 连续帧，但本轮容器里应用窗口始终渲染为全黑
  （同一环境 vkcube 正常），未能取得新证据。R9 原始闪烁仍未原生复现，原因仍是代码分析结论。

## 观察到但未修改的问题（不阻断）

- 数据对比截图里出现乱码（`å¼ ä¼Ÿ`），这是夹具数据本身被双重编码导致的：
  `docker/init` 的 MySQL 种子以错误的客户端字符集写入，`HEX(name)` 是 `C3A5C2BC…`，不是应用的问题。
- 本地 Redis 连接没有保存密码时，应用会直接报 NOAUTH，不像 MySQL/PostgreSQL 那样弹出密码框。
  连接表单写的是“留空则连接时询问”，两者不一致。这次没有修改，建议另开任务处理。
- 新建表页抬头写成 `rillsql.`（库名后面多一个点，schema 为空时的拼接问题），只影响显示。
- MySQL 文本默认值里含单引号时（例如 `it's`），`COLUMN_DEFAULT` 返回的原文会被方言判定为表达式，不会补引号。这个问题以前就存在，属于边缘情况。

## 交付限制

- macOS（Metal）和 Windows 的原生截图没有取得；以上证据都来自 Linux/Xvfb 上的原生 GPUI 渲染。
  A4 时（2026-11-20）本机会话仍锁屏（`IOConsoleLocked=true`）。因此 R6 的 ⌘⌥⇧ 键帽、R15/R16 在 Metal 下的观感未经原生验证。
- R9：原始闪烁没有在原生应用中复现，`R09-no-flicker-caret-moves-4-frames.png` 只是静态连续帧，不能证明滚动时不闪烁。
- R4 的已知限制（T09 已披露）：集群模式下控制台的 KEYS→SCAN 只覆盖一个节点；经 SSH 隧道连接集群时只能单节点浏览；
  模板批量 TTL/删除只作用于已读取的键，预览里的数量可能小于服务端总数；TYPE/PTTL 是预读的；短十六进制段不归入模板。
- 系统文件对话框（R5 Ctrl+O 之后）在 Xvfb 里没有桌面门户，没有截图。

## 开发验证

- `cargo test --manifest-path src-tauri/Cargo.toml --locked`（清理容器之后、夹具不在线时的最终运行）：1457 个通过，0 个失败，11 个忽略。
  Redis live 测试 `live_redis_scans_filters_reads_ttl_rename_delete_and_confirms` 在临时写入 5500 个键时失败过：
  种子键排到虚拟列表可见区之外，无法点击。删除临时键、恢复成仓库种子数据后通过，说明这是环境问题，不是代码问题。
- `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked`：无警告。
- A4 返修后：`cargo test --locked` 1460 个通过，0 个失败，11 个忽略；clippy 无警告。
