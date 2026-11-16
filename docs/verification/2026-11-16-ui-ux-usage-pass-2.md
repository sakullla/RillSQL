# 2026-11-16 UI/UX 实用性打磨（第二轮）

方法：在原生应用（Linux / Xvfb，1360×900）里连真实的 PostgreSQL 库（另建 `shop` 模式，5000 位客户、20000 笔订单），
照日常流程操作：新建连接 → 重启后再连 → 展开对象 → 开表、排序 → 写查询、看报错。记录卡点再修。
每项行为改动都有对应 Rust 测试；截图来自原生应用（`ui-ux-2026-11-16b/`）。

## 修复与改进

| # | 问题 | 处理 |
|---|---|---|
| 1 | **PostgreSQL 表默认按文本排序**：整数主键显示成 1、10、100、1000、1001…；点数值列排序同样按文本排 | 网格把每列转成文本读取（`"id"::text` 的输出列名仍是 `id`），而 `ORDER BY "id"` 的裸名字优先匹配输出列。排序列改为带表名限定（`"shop"."customers"."id"`），指向原始列、按真实类型排序；MySQL / MariaDB / SQL Server / SQLite 同样适用 |
| 2 | **密码错误被报成“网络连接失败”**：PostgreSQL 明文连接的任何失败都标成网络错误，提示“网络连接失败，没有改走加密连接重试” | 服务端回了 SQLSTATE 就按它分类：28xxx → 认证失败（“请检查数据库用户名和密码”），3D000 → 库不存在；只有没有 SQLSTATE 的才算网络/加密失败 |
| 3 | **没保存密码的连接只能“重试”，而重试必然再失败**（选择不保存密码，或系统钥匙串不可用） | 连接因缺密码或密码被拒失败时，直接弹出密码框：标题点名连接、显示 用户@地址/库；回车连接、Esc 取消、可改去编辑连接。被拒时标明“服务器拒绝了这个密码”。连上后重放失败时挂起的动作（展开、开表、运行查询） |
| 4 | **手输的密码只够连一次**：切换到同一连接的另一个库（PostgreSQL）会报“请填写数据库密码” | 认证通过的手输密码在本次运行内存里记住，切换数据库、手动重连不再要；删除或重新保存连接即作废；不写配置、不写日志。自动重连仍只用钥匙串里的密码（原有约定不变） |
| 5 | 连接详情看不出密码存没存 | 详情新增“密码：已保存在系统钥匙串 / 未保存，连接时询问” |
| 6 | **输入框比同排控件矮一截**：组件库 small 档是 1.5rem，工作台 rem 为 13px，输入框只有 19.5px，旁边按钮与分段控件是 26/30/32 | 有框输入框高度跟随当前密度的 `control_h`；嵌在格子/条件块里的无外观输入框（原位编辑、筛选值）不受影响 |
| 7 | 排序只能从表头右键菜单找到 | 表头右侧加排序开关：悬停表头出现，排着序时常亮并标方向；点按 升序 → 降序 → 不排序 轮换，不会顺手选中整列。开关浮在右缘不占排版，列窄时先让出类型缩写，列名最后省略 |
| 8 | 模式文件夹下的表名带重复前缀，窄侧栏里成了 `shop.custo…` | 树里对象只写对象名（它总在自己的模式节点下面）；完整路径在悬停提示里 |
| 9 | 树节点悬停提示重复名字：“127.0.0.1 / rillsql / rillsql” | 路径已以节点名结尾时不再拼一次 |
| 10 | 报错面板里语句出现两遍、都是无格式的散行，改写说明用警示色和错误抢视线 | 只给一个等宽代码块：被自动改写时是实际执行的语句，前面一行信息色说明改写原因；原句就在编辑器里，不再重复 |

## 验证

- `cargo test --manifest-path src-tauri/Cargo.toml --locked --lib`：1196 passed，1 failed，7 ignored（PostgreSQL / MySQL / MariaDB / SQL Server 夹具在线）。
  唯一失败的是 `long_select_keeps_polling_past_ten_seconds`：在这台机器上 60.2 s 超过 60 s 上限；未改动的基线提交同样 60.27 s 失败，与本次改动无关（CPU 密集的 SQLite 递归查询）。
- `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked`：0 告警。
- 新增测试：`order_by_targets_the_table_column_not_the_text_cast`、`a_rejected_postgres_password_is_an_auth_failure_not_a_network_one`、
  `a_typed_password_is_remembered_for_this_run_only`、`a_missing_password_prompts_and_reconnects_with_the_typed_one`、
  `credential_failures_are_recognised_by_stage_not_by_driver_code`、`tree_tooltips_do_not_repeat_the_node_name`、
  `bordered_inputs_match_the_density_control_height`、`the_header_sort_toggle_cycles_without_selecting_the_column`。
- 原生应用：上述每项都在应用内复现并确认修复（PostgreSQL 语句日志确认实际发出的 `ORDER BY "shop"."orders"."amount" DESC`），截图见 `ui-ux-2026-11-16b/`。

## 未处理

- 树里的对象种类分组（视图、函数、存储过程、触发器、序列、枚举、域、扩展）在展开前不知道是否为空，仍全部列出。
