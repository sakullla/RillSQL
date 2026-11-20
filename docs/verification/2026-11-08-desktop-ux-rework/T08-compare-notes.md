# T08 表/库对比：开发验证记录

证据来源：Rust 单测、gpui 测试窗口（`cargo test`），以及 docker MySQL 夹具上的 live 测试。
原生应用截图未取得：本机会话处于锁屏状态（2026-11-20 `ioreg` 报 `CGSSessionScreenIsLocked=true`），
`screencapture` 无法截取应用窗口。库结构对比结果页与数据对比结果页的原生截图仍待补。

## 卡在“正在对比…”的修复

- 每次读取都有超时（`compare/read.rs` 的 `Run::bounded`）：取设置里的查询超时；设置为 0（不限）时用目录读取的默认 30 秒。
  覆盖：临时会话、范围校验、对象分页、批量表结构、逐个对象定义、`describe`、每页行读取。
  超时后这次对比停止发起新的读取，报告带 `timed_out`，界面显示明确的失败说明（“对比读取超过 N 秒没有完成…”）与重试按钮。
- 取消立即生效（`app/views/compare/mod.rs` 的 `cancel`）：中止后台 tokio 任务（`AbortHandle`）、作废 token、
  `running=false`，可以立刻再次发起；迟到结果不会写回。
- 两侧并行读取（`tokio::join!`）；数据对比两侧的行也并行读取。
- 表结构批量读取：MySQL/MariaDB 适配器的 `compare_table_snapshots` 用三条 information_schema 查询读完整库的列、索引、外键
  （逐表读取每张表要 7–8 次往返，含入向外键与建表原文，这是大库上长时间无响应的主要来源）。
  批量读不到或方言没有实现时，退回逐个对象读取，每侧最多 4 个并发。
- 数据对比只读取选中表的定义（名单仍读全）。
- 差异匹配用 HashMap 按对象身份索引（原先每个身份线性查找两侧，O(n²)）。
- 进度：后台按请求 id 登记阶段与计数（列出对象 / 读取定义 n/m / 读取行 n/m 张表 / 比较），界面运行中每 250ms 轮询。
- SQL Server 行分页：A5 起已经走 `sql_syntax(engine).page_tail`，SQL Server 输出 `OFFSET … ROWS FETCH NEXT … ROWS ONLY`
  （既有测试 `sqlserver_paging_uses_offset_fetch_and_defaults_to_a_stable_order`）。

## 结果页

- 摘要：仅基准（将新建）/ 仅对照（将删除）/ 有变更 / 相同 四个计数，下面按对象种类细分（如“表 +1 −0 ~2”）；两侧一致时明确提示“两侧结构一致，没有差异”。
- 差异树：按种类分组（表、视图、函数…，索引/外键归在后面），可按 全部/仅基准/仅对照/有变更 筛选、“仅差异”开关（关掉后列出相同对象）、搜索（匹配名称或所属对象）。
- 选中项：基准/对照并排定义（表按对照侧限定名重建，只出现在一侧的行高亮），以及只针对这个对象的同步 SQL（复制、在查询中打开，放入前同样要确认）。
- 整体同步脚本：预览、复制、放入新查询（不执行，沿用既有确认）。
- 数据对比：按表列出 插入/更新/删除 计数、未能生成的行数、用来对应行的键列；无主键/唯一键、只在一侧、键未选定等原因写在表行上。
  点表下钻到行级差异：按键对齐，字段级显示基准值 → 对照值并高亮，附该行的同步语句（最多显示 200 行，完整语句在数据脚本里）。

## 实际运行的验证

- `cargo test --manifest-path src-tauri/Cargo.toml --locked`：1416 通过，11 忽略（需要夹具的手动测试）。含边界扫描 `production_code_respects_the_adapter_boundary`。
- `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked`：无警告。
- 新增单测：
  - `compare::read::run_tests::a_read_past_the_timeout_stops_the_run_with_a_clear_message`（超时说明、停止后续读取、进度登记清除）
  - `compare::read::run_tests::progress_counts_up_within_a_stage_and_resets_on_the_next`
  - `compare::tests::identical_sides_report_no_difference_and_list_the_same_objects`（空差异）
  - `compare::tests::each_difference_carries_side_by_side_definitions_and_its_own_sql`
  - `compare::tests::many_objects_diff_by_identity_without_mixing_them_up`（两侧各 3000+ 对象、倒序）
  - `compare::tests::data_report_summarizes_each_table_and_keeps_field_level_differences`
  - `app::views::compare::tests::cancel_ends_the_run_at_once_and_a_new_compare_can_start`（闸门挡住后台任务，取消后立即不再运行并可再次发起）
  - `app::views::compare::tests::a_timed_out_compare_is_a_failure_with_retry`
  - `app::views::compare::tests::the_result_tree_groups_filters_and_shows_the_picked_item`
  - `app::views::compare::tests::data_results_list_tables_and_drill_into_field_differences`
- Live（docker MySQL 夹具，`docker-compose -f docker/compose.yaml up -d mysql`）：
  - `live_mysql_two_databases_on_one_connection_finish_with_no_difference`：同一连接下两个不同库，60 秒限时内返回完成状态并给出“无差异”（本次约 35ms）；
    同时核对 MySQL 批量读取的列/索引/外键与逐表读取完全一致（种子库 rillsql）。
  - 既有 `live_engines_compare_a_made_difference_and_read_the_diagram` 的 MySQL 部分通过（差异列表含 t7_marker.note）；PostgreSQL/MariaDB/SQL Server 夹具未启动，按测试约定跳过。

## 仍需原生验证 / 限制

- 原生截图：库结构对比结果页、数据对比结果页（锁屏，未取得）。
- 批量表结构读取只实现了 MySQL/MariaDB；PostgreSQL、SQL Server、SQLite 仍逐个对象读取（有限并发 + 每次读取超时）。
- 卡住的具体 await 没有在原生环境复现追踪；修复按“每次读取有超时 + 取消立即中止 + 减少往返”处理。
- 源/目标选择器没有改成“连接 → 库 →（可选）表”的下拉式紧凑选择器，仍是列表卡片（交换、从树/标签带入沿用既有实现）。
