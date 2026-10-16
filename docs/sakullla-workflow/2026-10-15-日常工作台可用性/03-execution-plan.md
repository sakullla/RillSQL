---
# 计划字段仅在此 YAML 中填写。
format: execution_plan
tasks:
  - id: editor-assist
    goal: "查询编辑器按当前目标补全已读字段，能安全格式化 SQL，并能对一条语句显示不改数据的执行计划。"
    depends_on: []
    covers: [R1, R2, R3]
    scope:
      - src/query/complete.ts
      - src/query/columns.ts
      - src/query/format.ts
      - src/query/plan.ts
      - src/query/split.ts
      - src/query/QueryTab.vue
      - src/query/query.test.ts
      - src/query/format.test.ts
      - src/query/plan.test.ts
      - src/shell/shortcuts.ts
    outcomes:
      - "当前目标已读到表 orders 的字段 status 时，在 orders. 后补全出现 status。"
      - "未选择目标、未连接或字段读取失败时，补全不出现猜测字段名，并可见原因。"
      - "格式化整理关键字、缩进和换行，字符串与注释文本不变；引号或注释未闭合时原文不变并可见原因；只格式化选区时选区外不变。"
      - "无选区时对光标所在语句请求计划，有选区时对选区请求计划；空语句或多条语句不发起执行。"
      - "计划使用不带 ANALYZE 的 EXPLAIN 或 SQLite 的 EXPLAIN QUERY PLAN，结果可读，请求前后所读表的数据不变。"
    test: extend
  - id: grid-exchange
    goal: "可定位的单表查询结果能暂存修改，CSV 或 JSON 能导入已有表，当前行范围能导出 JSON 或 INSERT。"
    depends_on: []
    covers: [R4, R5, R6]
    scope:
      - src/grid/DataGrid.vue
      - src/grid/QueryResultGrid.vue
      - src/grid/data.ts
      - src/grid/api.ts
      - src/grid/result-edit.ts
      - src/grid/grid.test.ts
      - src/query/rows.ts
      - src/export/ExportDialog.vue
      - src/export/ImportDialog.vue
      - src/export/plan.ts
      - src/export/tasks.ts
      - src/export/native.ts
      - src/export/write.ts
      - src/export/csv.ts
      - src/export/json.ts
      - src/export/insert.ts
      - src/export/import.ts
      - src/export/export.test.ts
      - src/export/import.test.ts
      - src/export/native.test.ts
      - src-tauri/src/grid/sql.rs
      - src-tauri/src/grid/live.rs
      - src-tauri/src/grid/commands.rs
      - src-tauri/src/grid/tests.rs
      - src-tauri/src/native_export.rs
    outcomes:
      - "单表 SELECT 且结果含主键时，修改单元格、预览并确认后，重新查询能看到新值；查询结果不能新增行。"
      - "联接、聚合、视图、缺少主键或无法证明单表时，结果保持只读并可见原因，数据库原值不变。"
      - "提交中某一行被拒绝时，该行可见失败原因，尚未成功的暂存不显示为已成功。"
      - "向已有表确认导入对应好的 CSV 或对象数组后可见写入行数；取消、文件不合法或缺必填列时不写入。"
      - "导入中某一行失败时可见该行原因，结果不显示为全部成功。"
      - "已读行可导出 JSON 数组或这些行的 INSERT；空值仍为空值，未提交修改不进入文件，说明中可见这不是数据库备份。"
      - "非 SELECT 不能全量导出；取消保存不产生任务；写入失败不替换已有目标文件。"
    test: extend
  - id: saved-queries
    goal: "使用者能按名称保存查询，打开时不执行；连接消失时只载入 SQL 并要求重选目标。"
    depends_on: []
    covers: [R7]
    scope:
      - src/history/api.ts
      - src/history/plan.ts
      - src/history/HistoryPage.vue
      - src/history/saved.ts
      - src/history/saved.test.ts
      - src/history/history.test.ts
      - src/shell/commands.ts
      - src/shell/AppShell.vue
      - src/shell/session.ts
      - src/shell/shell.test.ts
      - src/shell/appshell.test.ts
      - src-tauri/src/query/mod.rs
      - src-tauri/src/query/commands.rs
      - src-tauri/src/query/saved.rs
      - src-tauri/src/query/history.rs
      - src-tauri/src/query/tests.rs
      - src-tauri/src/lib.rs
      - src-tauri/permissions/app-commands.toml
      - src-tauri/capabilities/default.json
      - src-tauri/gen/schemas/acl-manifests.json
      - src-tauri/gen/schemas/capabilities.json
      - src-tauri/gen/schemas/desktop-schema.json
      - src-tauri/gen/schemas/windows-schema.json
    outcomes:
      - "保存为日报后再次打开，编辑器中是同一 SQL，且这次打开没有新增成功的执行历史。"
      - "原连接已删除时打开，SQL 仍在，并可见需要重选目标；此时不执行。"
      - "删除前取消则查询仍在；确认删除后列表中不再有该查询。"
      - "执行历史的载入仍不执行，也不被保存查询替换。"
    test: new
delivery_verification:
  frontend-test:
    command: "npm test"
  rust-test:
    command: "cargo test --manifest-path src-tauri/Cargo.toml --lib"
  typecheck:
    command: "npm run typecheck"
---
# Execution Plan

R8–R12 不设任务。三个任务可以并行：`editor-assist` 只写查询编辑器，`grid-exchange` 只写表格、导入导出和现有导出命令，`saved-queries` 只写保存查询及其新命令注册。`saved_queries` 的建表和连接池访问写在 `src-tauri/src/query/history.rs` 的 `HistoryStore::open`，该文件只属于 `saved-queries`。约束分别来自技术方案的 ADR 1–3、ADR 4–6 和 ADR 7。
