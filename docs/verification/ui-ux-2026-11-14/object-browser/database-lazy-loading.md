# 非当前库的按库懒加载（评审修复 A2）

## 问题（评审事实）

此前 `catalog::read::assemble_postgres` / `assemble_sqlserver` 把会话当前库以外的每个库都造成
**错误节点**（`error = other_database_message(...)`）。`hierarchy::push_databases` 因此把库节点设成
`expandable = false`：展开没有反应、没有加载指示、也没有任何命令能换库，数百个库的场景（R7）里只有一个库可浏览。
当时留下的“聚焦该库再读”的退路并不存在：`nav_activation_command` 没有库节点分支，也没有调用
`Service::focus_database` / `open_ephemeral_postgres` 的地方。

## 现在的数据路径

```
展开库行 / 双击库行 / 库行重试
  → NavAction::Load(库节点)                        （hierarchy::activate）
  → WorkbenchModel::request_lazy_read → request_database
  → NavTree::needs_database / begin_database       （先落加载态，行上出现 Spinner）
  → NavRequest::Database(库节点)
  → view::spawn_nav_database
      ├─ PostgreSQL 非当前库：Service::open_ephemeral_postgres（比较页同一条路）
      ├─ SQL Server：原会话 + 三段名限定（不发 USE、不改当前库）
      → catalog::read_database_outline → 库的 schema 清单（不预读对象）
  → NavTree::set_database
      ├─ 成功：补上 schema 子节点；对象种类分组仍是 unloaded，展开时按组读
      └─ 失败：原因只挂在这个库节点上（连接行与别的库不动），行上有重试按钮
```

- 库层级以下的读取沿用同一条“按库”会话：`spawn_nav_kind` / `spawn_nav_more` / `spawn_nav_reload` /
  `spawn_nav_details` 都经 `view::nav_read_session` 取会话——PostgreSQL 的非当前库另开一个连到那个库的
  临时会话（`EphemeralRead`，读完随返回值一起断开，不替换当前会话、不写已保存配置），SQL Server 在原会话上用
  `[db].sys.*` 三段名读。
- 当前库的路径完全不变：连接建立时一次读出当前库的 schema 清单，库节点 `unloaded = false`，展开不发任何请求
  （`a_database_outside_the_current_session_reads_on_expand_and_keeps_its_own_error` 里第一条断言即此）。
- 原因文案不再进错误态：非当前库的 `DatabaseNode` 是 `{name, schemas: [], error: None}`。

## SQL Server 的三段名

`catalog::read::sqlserver_qualified(sql, database)` 只做三件事（空库名原样返回，当前库读取因此零改动）：

| 原写法 | 限定后 |
| --- | --- |
| `sys.schemas` / `sys.objects` / `sys.tables` … | `[db].sys.schemas` … |
| `information_schema.columns` | `[db].information_schema.columns` |
| `OBJECT_NAME(t.parent_id)`（只认当前库） | `(SELECT name FROM [db].sys.objects WHERE object_id = t.parent_id)` |

库名里的 `]` 按 SQL 规则翻倍（`au]dit` → `[au]]dit]`），由
`catalog::read::database_scope_tests::sqlserver_sql_is_scoped_with_three_part_names` 断言。

## 用例

| 用例 | 断言 |
| --- | --- |
| `catalog::hierarchy::tests::a_non_current_database_loads_layer_by_layer_with_a_loading_state` | 非当前库无 error、expandable、`unloaded`；`begin_database` 后行上 `loading = true`；`set_database(Ok)` 后出现 schema 与 unloaded 的对象种类分组；分组读回对象；当前库的模式不受影响 |
| `catalog::hierarchy::tests::a_failed_database_read_keeps_the_reason_on_that_database_only` | 失败只在这个库上留原因、`unloaded` 保留（可重试）、连接行无 error；重试清原因；成功清 `unloaded` |
| `app::shell::tests::a_database_outside_the_current_session_reads_on_expand_and_keeps_its_own_error` | 展开库行只排队一次 `NavRequest::Database` 且行上 loading；失败→`nav-retry` 再排队；成功后出现 schema/分组，分组读回对象；当前库展开不发请求 |
| `catalog::tests::refresh_keeps_the_loaded_window_and_a_failed_list_is_not_empty` | 非当前库现在是 `error: None` + 空 schemas（原断言“带错误”已改） |
| `catalog::read::database_scope_tests::*` | 三段名限定与转义；按库读取要求库名；不分库的引擎不走这条路径 |

## 仍未解决（不在本任务授权范围内的相邻边界）

1. **非当前库的数据页**：`catalog::read::read_postgres_page` / `read_sqlserver_page` 之外的网格读取
   （`src-tauri/src/app/views/grid`、`src-tauri/src/grid`）仍按“会话当前库”取数，打开另一个库的表数据会得到
   “当前连接打开的是 X，没有另开连接读取 Y”一类原因。数据页不在 T4 授权路径内，本轮没有改。
2. **非当前库的 DDL**：`catalog::read::read_definition` 现在**显式拒绝**非当前库（PostgreSQL 的 pg_catalog
   只覆盖自己的库，静默读会读到同名但不同库的对象）。宁可报原因也不给错 DDL；真正的跨库 DDL 需要把临时会话接进
   `application::ddl`，属于结构页范围。
3. **每次按库读取各开一个临时会话**：没有做会话缓存（`EphemeralRead` 读完即断开），代价是每展开/续读一次多一次
   连接建立；数百个库全部展开时的连接开销未实测。
4. **未取得原生证据**：本轮没有 PostgreSQL/SQL Server fixture 与原生窗口，以上全部是模型级与源码级断言，
   真机上的“展开→加载→出现对象”观感由 Delivery Owner 在集成 candidate 上补。
