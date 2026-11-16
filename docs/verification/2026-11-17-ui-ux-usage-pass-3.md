# 2026-11-17 UI/UX 实用性打磨（第三轮）

基线：`ca64f6c`（v0.2.39）。方法同第二轮：原生应用（Linux / Xvfb，1360×900）连真实 PostgreSQL（`shop` 模式，5000 位客户、20000 笔订单），
按日常流程操作——改格子并提交、筛选、写查询、导出、改表结构、看历史、测试连接——记录卡点再修。
每项行为改动都有对应 Rust 测试；截图来自原生应用（`ui-ux-2026-11-17/`）。

## 修复与改进

| # | 问题 | 处理 |
|---|---|---|
| 1 | **PostgreSQL 带自增序列的表改不了结构**：主键默认值是数据库报回的 `nextval('…'::regclass)`，校验把它当成“不是允许的字面量”，于是这张表任何字段改动（哪怕只改列名）都被拦下；什么都没改时预览也报这条红色错误（`03`） | 修改已有表时，只校验用户改过的类型和默认值；没动过的值是数据库自己报回来的，不写进语句。手动改成同样写法仍按规则拦下。改名现在生成一条 `RENAME COLUMN`（`05`），已在真实库执行确认 |
| 2 | **结构改动执行成功后仍显示“1 处修改未应用”**，行上的修改色条也不消失（`06`） | 修改已有表全部成功后，按数据库的新结构重读，草稿从新结构重建；部分失败时保留草稿 |
| 3 | 结构预览在没有改动时画一个空的 SQL 框、红色报错、执行规则说明，“确认执行”仍可点 | 没有语句时不画 SQL 框和执行规则；“没有字段变更”用中性色说明；确认不可点（`04`） |
| 4 | **改完格子后的第一次 Enter 被吞掉**：编辑框提交时挂起的“回声”标记是个永久开关，中间的 Ctrl/⌘+S 等快捷键不经过网格、清不掉它，结果变更预览里按 Enter 确认写入没有反应 | 回声标记改为 150 ms 时间窗，只吞同一轮按键的回声 |
| 5 | 变更预览只能点鼠标确认 | 回车确认写入、Esc 取消，页脚写明（`02`）。**生产环境**（按环境标签判定，与删表确认同一规则）回车不写入，页脚提示“请点按确认写入”；外壳尚未告知环境的网格按生产处理 |
| 6 | 结构页在连接未打开时显示红色“读取结构失败” + “重试”，而表数据页同一情况是中性“连接未打开” + “连接并读取”（`09`） | 结构页与表数据页一致（`10`），点按直接请外壳连接 |
| 7 | 字段表把余宽全给字段名（约 600 px），默认值列 150 px，`nextval(…)` 被截断；主键行的名称框因钥匙图标右移，与其它行不齐（`07`） | 宽屏余量分四成给默认值列（最多加 260 px）；钥匙占固定槽，名称框与表头左缘对齐（`08`） |
| 8 | 测试连接缺密码时，表单的密码占位写“留空则沿用已保存的密码”——其实没有保存；键盘停在顶上的连接串框（`11`） | 没存密码时占位为“未保存密码；留空则连接时询问”；测试失败落到某个字段时，键盘移到那个字段（`12`） |
| 9 | 导出面板：来源分区标题写“当前表”但来源是查询结果；查询已全部读取，仍说“全量导出不可用”，并列出一条不可用的“导出全部结果”（`13`） | 分区标题改为“来源”；完整的查询结果入口叫“导出结果 · 导出全部 N 行”，不再列出不存在的限制（`14`）；结果被截断时仍如实说明 |
| 10 | 网格每个非空文本格悬停都弹出和格子里一样的字（`15`），扫过表格时是噪音 | 只在可能显示不全时提示：多行、超过 80 字，或估算宽度放不进当前列宽 |

## 验证

- `cargo test --manifest-path src-tauri/Cargo.toml --locked`（PostgreSQL / MySQL / MariaDB / SQL Server 夹具在线）：1214 passed，1 failed，9 ignored。
  唯一失败的是 CPU 密集的 `long_select_keeps_polling_past_ten_seconds`（6 核机器上与整套测试并行时超过 60 s 上限）；
  单独运行通过（57.8 s）。上一轮未改动的基线在这台机器上同样超时。
- `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked`：0 告警。
- 新增测试：`untouched_postgres_defaults_do_not_block_other_column_changes`、`previewing_an_untouched_table_is_a_neutral_no_op`、
  `structure_reloads_after_a_successful_column_change`（真实 SQLite）、`an_unopened_connection_offers_connect_instead_of_an_error`、
  `enter_confirms_the_change_preview_except_on_production`（含过时回声标记）、`a_complete_query_result_exports_as_the_whole_result`、
  `a_failed_test_points_the_keyboard_at_the_blamed_field`、`query_columns_with_number_values_align_right`；
  扩展 `compact_field_widths_fit_the_narrowest_content_column`（宽屏默认值列）与单元格悬停提示用例。
- 原生应用：上述每项都在应用内复现并确认修复。结构改名在 PostgreSQL 上真实执行（`information_schema` 确认列名已变）；
  改格子 → Enter → Ctrl+S → Enter 后 `shop.orders` 里的值确认写入（`16`）。

## 检查过、未改动

- 查询结果的数字列已按值右对齐（列窄时看起来像左对齐），补了回归测试。
- 筛选无结果时不保留表头：代码注明是需求 R4 的明确决定（空表格交给三态说明），未改。
- 树里的对象种类分组仍全部列出：v0.2.39 为高延迟目录取消了逐类计数，展开前不知道是否为空。
