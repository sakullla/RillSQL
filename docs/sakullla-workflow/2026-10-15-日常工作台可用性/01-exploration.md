---
# Runtime 只读取这一处文件头机器区；不要在正文重复机器字段。
format: exploration
# AI 在此填写非空摘要；正文由 owner 按阶段契约组织。
summary: "四种引擎上的查询、表数据暂存提交、CSV 导出和执行历史已经可用。字段补全、格式化、独立执行计划、查询结果修改、CSV/JSON 导入、JSON/INSERT 导出和命名保存查询都没有产品入口。查询结果行没有基表定位，目录对象不含字段；默认执行发送全文，当前语句是单独动作。"
---
# Exploration

## 已有能力

PostgreSQL、MySQL、MariaDB、SQLite 可以新建连接。查询能按选区、当前语句或全文执行，失败时保留已完成语句的结果并显示数据库错误。表数据可以暂存修改、删除和新增，预览 SQL 后按序提交。查询结果可以查看、在已读行中查找排序，并导出 CSV。执行历史可以载入且不执行。

直接证据：`src/connections/model.ts` 的 `ENGINES`；`src/query/scope.ts` 的 `sqlToRun`；`src/grid/live.ts` 的暂存与提交；`src/grid/QueryResultGrid.vue`；`src/export/plan.ts`；`src/history/plan.ts` 的 `openPlan`。

## 相对需求的缺口

- R1：补全种类只有关键字、表和视图。已连接且对象已读到时，说明文字写明不含未读取的字段。目录里的表和视图没有字段列表。
- R2：查询编辑器的命令和菜单里没有格式化。在 `src/query`、`src-tauri/src/query` 和 `src` 的标识符搜索中没有格式化入口。
- R3：没有单独的执行计划动作。使用者自己写下的 `EXPLAIN` 会按普通 SQL 发给数据库；它不计入写入分类，也不会被结果行数上限改写。客户端记录的是本次运行的墙钟时间。
- R4：查询结果单元格不能编辑。结果列不标主键，每一行的定位信息为空。表数据侧已经能按主键定位，视图只读，没有主键时不能改、不能删已有行。
- R5：`src/export`、`src-tauri/src/native_export.rs` 和 `src-tauri/src` 的导入标识符搜索没有把 CSV 或 JSON 写入已有表的入口。`parseCsvDistinct` 只在内存里解析 CSV。
- R6：导出只有 CSV。JSON 数组和 `INSERT` 语句都没有写入器。
- R7：没有按名称保存的查询库。历史是执行记录，打开时不执行。

## 会约束后续方案的事实

默认执行和“当前语句”是两条路。没有选区时，普通执行发送整篇编辑器文本；有选区时发送选区原文，不自动对齐到语句边界。光标所在语句要另选“执行当前语句”。需求 R3 写的是计划与执行使用同一种语句选择，且无选区时用光标所在语句。这是现状和需求文字的差异，不是已实现行为。证据：`src/query/scope.ts`，`src/query/QueryTab.vue` 的执行快捷键。

表数据提交是逐条 SQL。前面的语句已经写入后，失败的那条和更后面的暂存仍留在界面上，并显示数据库错误。证据：`src/grid/live.ts` 的提交循环，以及数据表探索对 `src/grid/grid.test.ts` 的核对。

查询结果目前无法复用这套定位。`src/query/rows.ts` 的 `gridModel` 把每列主键标成否，并把 `locator` 设为空、`located` 设为否。表侧只有在对象是基表、键非空、键值非空且当前已读行里该键唯一时才标成可定位。证据：`src/grid/data.ts` 的 `locatorFrom`。

全量查询导出只接受去掉注释后的一条 `SELECT`。`WITH`、多语句和非 `SELECT` 会被拒绝，导出不开始；重新执行前还要确认。已读的截断结果会标成部分结果。表上未提交的修改不会进入文件，对话框只警告，不因此禁止导出。取消系统保存对话框不写入，对话框仍开着；原生写入失败时不替换已有目标文件。导出说明写明 CSV 不是数据库备份。证据：`src/grid/sql.ts` 的 `selectExportGate`，`src/export/ExportDialog.vue`，`src-tauri/src/native_export.rs`。

字段如果要补全，不能从当前目录对象直接取得。列信息出现在打开表数据时的表结构描述里，不在 `CatalogObject` 上。证据：`src/catalog/types.ts`，`src/query/complete.ts` 的 `metadataNote`。

## 排除项的现状

在已查的连接模型、壳层命令、历史和结构页面里，没有第五种引擎，没有 ER 图、结构对比、生成式 AI、整库备份或还原，也没有执行前语义检查和重构入口。结构页仍是单表的字段、索引、外键和带预览的删除。这些观察限于上述入口，不是全仓证明。

## 未知

CodeMirror `basicSetup` 的依赖源码没有打开，所以不能从包内部证明它完全没有格式化快捷键。应用自己的查询命令表里没有格式化。

使用者手写并执行的 `EXPLAIN` 在数据库内部是否改数据，取决于那条 SQL 和数据库，客户端没有单独的“只看计划”路径。
