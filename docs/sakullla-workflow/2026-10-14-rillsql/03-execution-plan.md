---
format: execution_plan
tasks:
  - id: shell
    goal: 桌面窗口以澄库 · RillSQL 启动，并呈现原型中的外壳、主题、快捷键帮助和关于信息。
    depends_on: []
    covers: [R1, R18]
    scope:
      - .gitignore
      - package.json
      - package-lock.json
      - vite.config.ts
      - tsconfig.json
      - tsconfig.app.json
      - tsconfig.node.json
      - index.html
      - src/main.ts
      - src/env.d.ts
      - src/router.ts
      - src/shell
      - src/styles
      - src/connections
      - src/catalog
      - src/query
      - src/history
      - src/grid
      - src/export
      - src/structure
      - src-tauri/Cargo.toml
      - src-tauri/Cargo.lock
      - src-tauri/build.rs
      - src-tauri/tauri.conf.json
      - src-tauri/capabilities
      - src-tauri/icons
      - src-tauri/gen
      - src-tauri/src
    outcomes:
      - 启动后窗口标题和关于页能识别澄库与 RillSQL，且无需云登录即可进入欢迎页。
      - 浅色、深色和跟随系统主题使用原型中的色彩令牌；状态和环境不靠颜色单独区分。
      - 导航、标题栏、状态栏、命令面板和快捷键帮助与原型的布局一致，窄窗口时导航可只留图标。
      - 关于页写明首版没有 AI、云同步、NoSQL、关系图、备份恢复和运维监控入口。
      - 后续模块的界面和 Rust 入口以可编译占位接在外壳上，依赖和锁文件由本任务固定。
    test: new
  - id: connections
    goal: 用户能保存、测试、打开和删除 MySQL、MariaDB、PostgreSQL 与 SQLite 连接，TLS 校验名与 SSH 传输地址分离。
    depends_on: [shell]
    covers: [R2, R3, R4, R5]
    scope:
      - src/connections
      - src-tauri/src/connections
    outcomes:
      - 保存的远程连接和 SQLite 文件在重启后仍可打开；删除配置不删除数据库或 SQLite 文件。
      - 选择保存的密码进入系统凭据库，普通配置和日志中没有明文密码；凭据库不可用时仍可用本次输入的密码。
      - 开启证书验证时按数据库主机名校验；经 SSH 时 TCP 进入转发器，只签给 localhost 的证书会失败，且不会改成不验证后重试。
      - 一次 SSH 会话可接受多条本地连接，每条连接单独一条到数据库的通道；主机密钥首次需确认，变化时阻止连接。
      - 测试按阶段失败并停止后续阶段。MySQL 与 MariaDB 是两种连接，SQLite 不出现 TLS 设置。
    test: new
  - id: catalog
    goal: 用户能按引擎查看、搜索和刷新库表结构，大量对象分批出现，读取失败不显示成空库。
    depends_on: [connections]
    covers: [R6]
    scope:
      - src/catalog
      - src-tauri/src/catalog
    outcomes:
      - PostgreSQL 显示数据库和 schema，MySQL 与 MariaDB 显示数据库，SQLite 直接列出对象并显示外键开关状态。
      - 筛选结果带连接、库和 schema 路径，同名表可以区分；未连接的连接不混入结果。
      - 对象超过默认批量时先显示一批并可以继续读取；读取失败显示错误和重试，不显示成没有表。
      - 外部删除的对象标为失效，再次刷新后从树中消失。
    test: new
  - id: query
    goal: 用户能在多个连接上编辑、执行、取消 SQL，并在本机历史中找回语句而不自动执行。
    depends_on: [catalog]
    covers: [R7, R8, R9, R19]
    scope:
      - src/query
      - src/history
      - src-tauri/src/query
    outcomes:
      - 执行目标始终显示连接和数据库；有选区时只执行选区，当前语句只执行光标所在语句。
      - 补全使用当前目标已读到的元数据；元数据失败时只提供关键字并说明原因。
      - 取消区分已请求、已确认和失败；经 SSH 时取消另开一条通道。多语句在失败处停止并保留已完成结果。
      - 断连后写入结果不明时标记未知且不重放。执行期间可以编辑其他标签，关闭正在执行的标签前先请求取消。
      - 历史只在用户再次执行时才提交；原连接已删除时必须重新选择目标。重启不恢复未执行草稿。
    test: new
  - id: grid
    goal: 用户能分批浏览和编辑表数据，无主键的表仍可新增，并按声明的范围导出 CSV。
    depends_on: [connections]
    covers: [R10, R11, R17]
    scope:
      - src/grid
      - src/export
      - src-tauri/src/grid
    outcomes:
      - 表数据和查询结果默认只加载一批，界面标明已加载数量和截断；NULL 与空字符串在显示、复制和导出中可区分。
      - 表浏览的筛选和排序在服务器端按结构化条件执行；查询结果里的查找和排序只作用于已读取行。
      - 基础表始终可以新增。没有可靠唯一标识时不能修改或删除，插入后仍不能定位的新行保持不可改、不可删。
      - 提交逐条执行，失败即停，已成功的语句不撤销，失败行留在暂存区。
      - 部分导出标明不是完整结果；查询结果的全量导出只接受 SELECT 并在执行前说明会重新执行。失败保留已写部分，取消删除未完成文件。
    test: new
  - id: structure
    goal: 用户能在预览 SQL 和影响之后创建或修改表、字段、索引和外键，取消时数据库不变。
    depends_on: [catalog]
    covers: [R12, R13, R14, R15, R16]
    scope:
      - src/structure
      - src-tauri/src/ddl
    outcomes:
      - 建表、改字段、索引、外键和删表在执行前展示目标、风险和逐条 SQL；取消不向数据库提交。
      - 执行后逐条给出成功、失败、未执行或结果未知。MySQL 中途失败不声称已全部回滚。
      - SQLite 在不能可靠保留数据或约束时阻止执行；需要重建时展示官方重建步骤。能直接修改的 NOT NULL 不套重建。
      - 删除表只删除预览中的对象。受约束保护的索引不能当作普通索引单独删除。
    test: new
delivery_verification:
  full:
    command: npm test
---
# Execution Plan

R20–R25 由需求中的“不做事项”覆盖，不设实现任务。仓库里原先没有测试，各任务在自己的目录中新增测试。

`shell` 固定依赖、锁文件和模块入口。其后的任务只改自己的目录，不新增依赖，也不改清单和锁文件。`grid` 与 `catalog` 可以并行；`query` 与 `structure` 可以并行。

交互和视觉以本机 `docs/prototype/` 为准。该目录被忽略，不进入版本库。
