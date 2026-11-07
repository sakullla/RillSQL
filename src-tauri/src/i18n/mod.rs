//! zh-CN / en 字典。键与 Vue 文案一致；两边都有的句子保持同一写法。

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Locale {
    ZhCn,
    En,
}

pub fn text(locale: Locale, key: &str) -> String {
    let value = match locale {
        Locale::ZhCn => zh(key),
        Locale::En => en(key),
    };
    value.unwrap_or(key).to_owned()
}

macro_rules! dict {
    ($($key:literal => $zh:literal, $en:literal;)*) => {
        fn zh(key: &str) -> Option<&'static str> {
            Some(match key {
                $($key => $zh,)*
                _ => return None,
            })
        }
        fn en(key: &str) -> Option<&'static str> {
            Some(match key {
                $($key => $en,)*
                _ => return None,
            })
        }
        pub fn catalog_keys() -> &'static [&'static str] {
            &[$($key),*]
        }
    };
}

dict! {
    "shell.command.newQuery" => "新建查询", "New query";
    "shell.command.execute" => "执行", "Execute";
    "shell.command.newStructure" => "新建表结构", "New table structure";
    "shell.command.compare" => "对比库", "Compare databases";
    "shell.command.newConnection" => "新建连接", "New connection";
    "shell.command.openSqlite" => "打开 SQLite 文件", "Open SQLite file";
    "shell.command.history" => "查询历史", "Query history";
    "shell.command.saveQuery" => "保存当前查询", "Save current query";
    "shell.command.savedQueries" => "保存的查询", "Saved queries";
    "shell.command.connections" => "连接管理", "Manage connections";
    "shell.command.settings" => "设置", "Settings";
    "shell.command.toggleSidebar" => "显示 / 隐藏资源管理器", "Show / hide the explorer";
    "shell.command.themeLight" => "切换到浅色主题", "Switch to light theme";
    "shell.command.themeDark" => "切换到深色主题", "Switch to dark theme";
    "shell.command.themeSystem" => "主题跟随系统", "Theme follows system";
    "shell.command.help" => "键盘快捷键", "Keyboard shortcuts";
    "shell.command.quit" => "退出", "Quit";
    "shell.menu.file" => "文件", "File";
    "shell.menu.edit" => "编辑", "Edit";
    "shell.menu.view" => "视图", "View";
    "shell.menu.query" => "查询", "Query";
    "shell.menu.tools" => "工具", "Tools";
    "shell.menu.window" => "窗口", "Window";
    "shell.menu.help" => "帮助", "Help";
    "shell.theme.light" => "浅色", "Light";
    "shell.theme.dark" => "深色", "Dark";
    "shell.theme.system" => "跟随系统", "Follow system";
    "shell.settings.title" => "设置", "Settings";
    "shell.settings.density" => "界面密度", "UI density";
    "shell.settings.densityCompact" => "紧凑", "Compact";
    "shell.settings.densityStandard" => "标准", "Standard";
    "shell.settings.densityRelaxed" => "宽松", "Relaxed";
    "shell.settings.editorFont" => "编辑器字号", "Editor font size";
    "shell.settings.editorTheme" => "编辑器配色", "Editor colors";
    "shell.settings.language" => "界面语言", "Language";
    "shell.settings.theme" => "主题", "Theme";
    "shell.settings.editorThemeAuto" => "跟随主题", "Follow theme";
    "shell.settings.editorThemeContrast" => "高对比", "High contrast";
    "shell.settings.editorThemeWarm" => "暖色", "Warm";
    "shell.settings.editorThemeCool" => "冷色", "Cool";
    "shell.settings.editorThemeMono" => "弱化灰阶", "Muted grayscale";
    "shell.settings.rowsPerPage" => "每次读取的行数", "Rows per read";
    "shell.settings.previewLimit" => "大字段预览上限", "Large value preview limit";
    "shell.settings.nullCopy" => "复制 NULL 时写入", "Text written for NULL when copying";
    "shell.settings.timeout" => "语句执行超时", "Statement timeout";
    "shell.shortcuts.groupGrid" => "表格", "Grid";
    "shell.shortcuts.commandPalette" => "命令面板（表、连接、命令）", "Command palette (tables, connections, commands)";
    "shell.shortcuts.quickOpen" => "快速打开表", "Quick open table";
    "shell.shortcuts.newQuery" => "新建查询", "New query";
    "shell.shortcuts.closeTab" => "关闭当前标签", "Close current tab";
    "shell.shortcuts.toggleSidebar" => "显示 / 隐藏资源管理器", "Show / hide the explorer";
    "shell.shortcuts.nextTab" => "切换到下一个标签", "Switch to next tab";
    "shell.shortcuts.nthTab" => "切换到第 N 个标签", "Switch to tab N";
    "shell.shortcuts.openSettings" => "设置", "Settings";
    "shell.shortcuts.shortcutHelp" => "快捷键帮助", "Shortcut help";
    "shell.shortcuts.runSelection" => "执行选中内容；无选中时执行光标所在语句", "Run selection; with nothing selected, run the statement at the cursor";
    "shell.shortcuts.runCurrent" => "只执行光标所在语句", "Run only the statement at the cursor";
    "shell.shortcuts.requestCancel" => "请求取消执行", "Request cancellation";
    "shell.shortcuts.autocomplete" => "补全", "Autocomplete";
    "shell.shortcuts.toggleComment" => "注释 / 取消注释", "Comment / uncomment";
    "shell.shortcuts.toggleResults" => "显示 / 隐藏查询结果区", "Show / hide the query results pane";
    "shell.shortcuts.format" => "格式化选中的 SQL；无选中时格式化全部", "Format selected SQL; with nothing selected, format everything";
    "shell.shortcuts.explain" => "查看当前语句的执行计划", "Show the execution plan of the current statement";
    "shell.shortcuts.leaveEditor" => "离开编辑器（不会插入缩进）", "Leave the editor (no indentation inserted)";
    "shell.shortcuts.moveCells" => "移动单元格；按住 Shift 扩展选择", "Move between cells; hold Shift to extend the selection";
    "shell.shortcuts.selectAllCells" => "选中已读取的全部单元格", "Select every loaded cell";
    "shell.shortcuts.editCell" => "编辑单元格", "Edit cell";
    "shell.shortcuts.cancelEdit" => "取消编辑；未编辑时离开表格", "Cancel editing; leave the grid when not editing";
    "shell.shortcuts.markDelete" => "标记删除选中的行（暂存）", "Mark selected rows for deletion (staged)";
    "shell.shortcuts.commit" => "预览并提交变更", "Preview and commit changes";
    "shell.shortcuts.copyRegion" => "复制选中区域（TSV）", "Copy the selection (TSV)";
    "shell.shortcuts.copySqlValue" => "复制为 SQL 值（区分 NULL）", "Copy as SQL values (NULL-aware)";
    "common.save" => "保存", "Save";
    "common.cancel" => "取消", "Cancel";
    "common.close" => "关闭", "Close";
    "common.copy" => "复制", "Copy";
    "common.localeZh" => "中文（简体）", "中文（简体）";
    "common.localeEn" => "English", "English";
    "common.newConnection" => "新建连接", "New connection";
    "common.openSqlite" => "打开 SQLite 文件", "Open SQLite file";
    "shell.nav.main" => "主导航", "Main navigation";
    "shell.nav.workspace" => "工作台", "Workbench";
    "shell.nav.connections" => "连接", "Connections";
    "shell.nav.history" => "查询历史", "Query history";
    "shell.nav.settings" => "设置", "Settings";
    "shell.sidebar.explorer" => "资源管理器", "Explorer";
    "shell.palette.title" => "命令面板", "Command palette";
    "shell.paletteGroup.connection" => "连接", "Connections";
    "shell.welcome.title" => "RillSQL", "RillSQL";
    "shell.welcome.factEngines" => "支持 PostgreSQL、MySQL、MariaDB、SQL Server、SQLite、Redis、MongoDB", "Supports PostgreSQL, MySQL, MariaDB, SQL Server, SQLite, Redis and MongoDB";
    "shell.welcome.resumeTitle" => "继续工作", "Continue working";
    "shell.welcome.recent" => "最近执行", "Recent runs";
    "shell.welcome.saved" => "保存的查询", "Saved queries";
    "shell.statusbar.noTarget" => "此查询还未选择执行目标——在查询里选一个连接和库", "This query has no execution target yet — pick a connection and database in the query";
    "shell.tabs.tablist" => "打开的标签", "Open tabs";
    "shell.tabs.closeOthers" => "关闭其他", "Close others";
    "shell.tabs.closeRight" => "关闭右侧", "Close to the right";
    "shell.tabs.closeAll" => "关闭全部", "Close all";
    "shell.closeGuard.closeWindow" => "关闭窗口？", "Close the window?";
    "shell.closeGuard.closeTab" => "关闭这个标签？", "Close this tab?";
    "shell.closeGuard.stay" => "留下", "Stay";
    "shell.closeGuard.leave" => "离开", "Leave";
    "shell.closeGuard.windowCells" => "有标签含未提交的单元格修改。留下则窗口和修改都还在；离开会丢掉这些修改，不会写入数据库。", "A tab has uncommitted cell changes. Staying keeps the window and the changes; leaving discards them. Nothing is written to the database.";
    "shell.closeGuard.windowDraft" => "有标签的 SQL 草稿尚未执行。留下则窗口和草稿都还在；离开会丢掉这些草稿。", "A tab has an unexecuted SQL draft. Staying keeps the window and the draft; leaving discards it.";
    "shell.closeGuard.windowBoth" => "有标签含未提交的单元格修改和未执行的 SQL 草稿。留下则窗口和内容都还在；离开会丢掉这些修改与草稿，不会写入数据库。", "A tab has uncommitted cell changes and an unexecuted SQL draft. Staying keeps the window and the content; leaving discards the changes and drafts. Nothing is written to the database.";
    "shell.closeGuard.tabCells" => "这个标签有未提交的单元格修改。留下则标签和修改都还在；离开会丢掉这些修改，不会写入数据库。", "This tab has uncommitted cell changes. Staying keeps the tab and the changes; leaving discards them. Nothing is written to the database.";
    "shell.closeGuard.tabDraft" => "这个标签的 SQL 草稿尚未执行。留下则标签和草稿都还在；离开会丢掉这份草稿。", "This tab has an unexecuted SQL draft. Staying keeps the tab and the draft; leaving discards it.";
    "shell.closeGuard.tabBoth" => "这个标签有未提交的单元格修改和未执行的 SQL 草稿。留下则标签和内容都还在；离开会丢掉这些修改与草稿，不会写入数据库。", "This tab has uncommitted cell changes and an unexecuted SQL draft. Staying keeps the tab and the content; leaving discards the changes and the draft. Nothing is written to the database.";
    "shell.update.busy" => "请先等待查询、导出、迁移等任务完成，并关闭正在操作的对话框。", "Wait for queries, exports and migrations to finish, and close active dialogs first.";
    "shell.update.pending" => "请先提交或放弃表格、结构等未提交修改，再安装更新。", "Commit or discard pending grid and structure changes before installing the update.";
    "shell.update.autoCheck" => "启动时自动检查更新", "Check for updates on startup";
    "shell.state.unknown" => "不支持的应用状态版本，原文件未改动。", "This app-state version is not supported; the file was left unchanged.";
    "shell.state.forbidden" => "应用状态不能保存密码或未提交修改。", "App state cannot store passwords or uncommitted changes.";
    "shell.state.malformed" => "应用状态无法识别，原文件未改动。", "App state could not be read; the file was left unchanged.";
    "connections.page.title" => "连接管理", "Connections";
    "connections.page.emptyFirstHint" => "支持 PostgreSQL、MySQL、MariaDB、SQL Server、SQLite、Redis 和 MongoDB。", "Supports PostgreSQL, MySQL, MariaDB, SQL Server, SQLite, Redis and MongoDB.";
    "connections.page.note" => "连接配置只保存在本机。密码保存在操作系统凭据库；删除连接不会删除数据库或 SQLite 文件。", "Connection settings stay on this machine. Passwords live in the OS credential store; deleting a connection never deletes the database or the SQLite file.";
    "connections.page.importRillSql" => "导入 RillSQL 文件", "Import RillSQL file";
    "connections.page.exportButton" => "导出连接", "Export connections";
    "connections.page.localFile" => "本地文件", "Local file";
    "connections.page.tlsVerify" => "TLS · 核对证书", "TLS · verified";
    "connections.page.tlsRequire" => "TLS · 不核对证书", "TLS · unverified";
    "connections.page.tlsOff" => "不加密", "Unencrypted";
    "connections.page.connect" => "连接", "Connect";
    "connections.page.moreActions" => "{name} 的更多操作", "More actions for {name}";
    "connections.page.menuEdit" => "编辑配置…", "Edit settings…";
    "connections.page.menuDisconnect" => "断开连接", "Disconnect";
    "connections.page.menuDelete" => "删除配置…", "Delete configuration…";
    "connections.editor.newTitle" => "新建连接", "New connection";
    "connections.editor.tls.title" => "TLS 加密", "TLS";
    "connections.editor.ssh.title" => "SSH 跳板机", "SSH jump host";
    "connections.editor.ssh.authLabel" => "登录方式", "Sign-in method";
    "connections.editor.ssh.authKey" => "私钥", "Private key";
    "connections.editor.ssh.authPassword" => "密码", "Password";
    "connections.editor.ssh.keyFile" => "私钥文件", "Private key file";
    "connections.editor.ssh.port" => "SSH 端口", "SSH port";
    "connections.editor.tls.caLabel" => "受信任的 CA", "Trusted CA";
    "connections.editor.tls.caSystem" => "系统信任根", "System trust store";
    "connections.editor.tls.caFile" => "指定 CA 文件", "CA file";
    "connections.editor.tls.clientCert" => "客户端证书（可选）", "Client certificate (optional)";
    "connections.editor.tls.clientKey" => "客户端私钥（可选）", "Client key (optional)";
    "connections.editor.replicaSet" => "副本集（可选）", "Replica set (optional)";
    "connections.exportDialog.passphrase" => "口令（可选）", "Passphrase (optional)";
    "connections.importReport.skippedTitle" => "已跳过", "Skipped";
    "connections.deleteDialog.confirm" => "删除配置", "Delete configuration";
    "connections.deleteDialog.willKeepDb" => "不会删除：数据库本身和其中的任何数据", "Does not delete the database or any of its data";
    "connections.editor.test.button" => "测试连接", "Test connection";
    "connections.editor.test.ok" => "连接成功", "Connection succeeded";
    "connections.editor.test.wait" => "第一次连接这台 SSH 跳板机，需要先确认它的身份", "First connection to this SSH jump host; confirm its identity first";
    "connections.editor.test.failed" => "连接失败", "Connection failed";
    "connections.status.connected" => "已连接", "Connected";
    "connections.status.error" => "连接失败", "Connection failed";
    "connections.deleteDialog.title" => "删除这条连接配置？", "Delete this connection configuration?";
    "connections.validate.chooseFile" => "请选择数据库文件", "Choose a database file";
    "connections.validate.hostRequired" => "请填写主机", "Enter a host";
    "connections.validate.portRange" => "端口应为 1–65535", "Port must be 1–65535";
    "connections.validate.userRequired" => "请填写用户名", "Enter a username";
    "connections.validate.redisDbIndex" => "Redis 库索引应为 0 起的非负整数", "The Redis database index must be a non-negative integer";
    "connections.validate.mongoUriSsh" => "整串 URI 暂不支持 SSH 隧道，请改用 host/port 表单", "A full URI does not support an SSH tunnel yet; use the host/port form";
    "connections.validate.sshHostRequired" => "请填写跳板机主机", "Enter the jump host";
    "connections.validate.sshUserRequired" => "请填写 SSH 用户", "Enter the SSH user";
    "connections.validate.caFileRequired" => "请填写 CA 证书文件", "Enter the CA certificate file";
    "connections.url.unsupported" => "只支持 postgres(ql)://、mysql://、mariadb://、sqlserver://、sqlite:、redis://、mongodb 或带 jdbc: 前缀的连接串。", "Only connection URLs starting with postgres, mysql, mariadb, sqlserver, sqlite, redis, mongodb or a jdbc prefix are supported.";
    "connections.url.parseFailed" => "连接串格式无法解析，请检查主机和端口的写法。", "The connection URL could not be parsed; check the host and port.";
    "connections.url.noHost" => "连接串里没有主机地址或文件路径。", "The connection URL has no host address or file path.";
    "history.page.titleRuns" => "查询历史", "Query history";
    "history.page.runs" => "执行历史", "Run history";
    "history.page.saved" => "保存的查询", "Saved queries";
    "history.page.tabsLabel" => "历史与保存的查询", "History and saved queries";
    "history.page.loadToEditor" => "载入编辑器 ↗", "Load into editor ↗";
    "history.page.loadOnlySql" => "只载入 SQL，不会执行。", "Loads the SQL only; nothing is executed.";
    "history.page.sqlPreview" => "SQL 预览", "SQL preview";
    "history.page.filterConnection" => "按连接筛选", "Filter by connection";
    "history.page.clearConfirm" => "清空本机的全部查询历史？此操作无法撤销。", "Clear the entire local query history? This cannot be undone.";
    "history.savedPanel.openNote" => "打开只载入 SQL，不会执行。", "Opening loads the SQL only; nothing is executed.";
    "history.savedPanel.save" => "保存", "Save";
    "history.savedPanel.needTab" => "先打开一个查询标签，再保存其中的 SQL。", "Open a query tab first, then save its SQL.";
    "history.plan.deletedNotice" => "需要重新选择执行目标。原来的连接已删除，这次只载入 SQL，不会执行。", "An execution target must be chosen again. The original connection was deleted; this loads the SQL only and never executes.";
    "query.editor.area" => "查询", "Query";
    "query.editor.menu.run" => "执行", "Run";
    "query.editor.menu.runCurrent" => "执行当前语句", "Run current statement";
    "query.editor.menu.format" => "格式化", "Format";
    "query.editor.menu.plan" => "执行计划", "Execution plan";
    "query.editor.menu.copy" => "复制", "Copy";
    "query.editor.menu.clear" => "清空编辑器", "Clear editor";
    "catalog.tree.menuLabel" => "对象操作", "Object actions";
    "catalog.tree.openData" => "打开数据", "Open data";
    "catalog.tree.viewStructure" => "查看结构", "View structure";
    "catalog.tree.newQueryHere" => "在此新建查询", "New query here";
    "catalog.tree.copyToQuery" => "复制到新查询", "Copy to a new query";
    "catalog.tree.copyFullName" => "复制完整名称", "Copy full name";
    "grid.menu.copy" => "复制", "Copy";
    "grid.menu.copyRow" => "复制整行", "Copy entire row";
    "grid.menu.copyColumn" => "复制整列", "Copy entire column";
    "grid.menu.filterByValue" => "按此值筛选", "Filter by this value";
    "grid.menu.markDelete" => "标记删除行", "Mark row for deletion";
    "grid.value.copyValue" => "复制值", "Copy value";
    "grid.errors.viewReadonly" => "视图是只读的，不能新增、修改或删除数据。", "Views are read-only; data cannot be inserted, modified or deleted.";
    "query.run.runScript" => "执行脚本", "Run script";
    "query.run.cancel" => "取消", "Cancel";
    "query.outline.heading" => "语句大纲", "Statement outline";
    "query.fileIo.open" => "打开 SQL 文件", "Open SQL file";
    "query.fileIo.save" => "保存到文件", "Save to file";
    "query.complete.expandStar" => "展开为列清单", "Expand to columns";
    "query.variables.title" => "填写变量", "Fill variables";
    "query.variables.proceed" => "继续", "Continue";
    "query.variables.cancel" => "取消", "Cancel";
    "query.errors.needTarget" => "请选择执行目标（连接和数据库）。", "Choose an execution target (connection and database).";
    "query.errors.blankOnly" => "只有空白或注释，没有发送。", "Only whitespace or comments; nothing was sent.";
    "query.errors.unknownDialect" => "未知方言，没有改写 SQL。", "Unknown dialect; the SQL was left unchanged.";
    "query.errors.noProvider" => "没有补全提供者，没有改写 SQL。", "No completion provider; the SQL was left unchanged.";
    "query.format.quote" => "无法格式化：引号未闭合。", "Cannot format: a quote is unclosed.";
    "query.format.comment" => "无法格式化：注释未闭合。", "Cannot format: a comment is unclosed.";
    "query.format.dollar" => "无法格式化：dollar-quote 未闭合。", "Cannot format: a dollar quote is unclosed.";
    "query.format.adjacent" => "无法格式化：会把原本相连的字符拆开。", "Cannot format: that would split characters that were adjacent.";
    "query.format.commentBreak" => "无法格式化：会把行注释断开，后面的文本会变成 SQL。", "Cannot format: that would break a line comment and turn the rest into SQL.";
    "query.format.edgeGlue" => "无法格式化：会去掉选区边缘的空白，使两边的字符粘在一起。", "Cannot format: that would remove whitespace at the selection edge and glue the sides together.";
    "query.plan.empty" => "没有可查看计划的语句。", "There is no statement to explain.";
    "query.plan.multiple" => "一次只能查看一条语句的执行计划。", "Only one statement can be explained at a time.";
    "query.plan.noAnalyze" => "计划不使用 ANALYZE，没有发送这条语句。", "Plans do not use ANALYZE; the statement was not sent.";
    "query.plan.sqliteAnalyze" => "SQLite 不支持实际执行并分析，没有发送会修改数据的语句。", "SQLite cannot analyze by executing; no statement that could change data was sent.";
    "query.plan.sqlserverAnalyze" => "SQL Server 的计划不实际执行语句，没有发送会修改数据的语句。", "SQL Server plans do not execute the statement; no statement that could change data was sent.";
    "query.split.quoteUnclosed" => "引号没有闭合。", "A quote is unclosed.";
    "query.split.blockCommentUnclosed" => "块注释没有闭合。", "A block comment is unclosed.";
    "query.split.dollarUnclosed" => "dollar-quote 没有闭合。", "A dollar quote is unclosed.";
    "query.split.parenUnclosed" => "括号没有闭合。", "Parentheses are unclosed.";
    "query.fileIo.tooLarge" => "文件超过 2 MB 上限，没有载入。", "The file is over the 2 MB limit and was not loaded.";
    "query.fileIo.saved" => "已保存 SQL 文件。", "The SQL file was saved.";
    "query.tree.refresh" => "刷新对象树", "Refresh object tree";
    "query.database.label" => "数据库", "Database";
    "query.status.empty" => "空结果", "Empty result";
    "query.status.running" => "正在执行", "Running";
    "query.status.noObjects" => "没有对象", "No objects";
    "query.results.statement" => "结果", "Result";
    "grid.range.loaded" => "已读取", "Loaded";
    "grid.range.subset" => "已读取子集", "Loaded subset";
    "grid.range.unknownFull" => "全量未知", "Full count unknown";
    "grid.value.unsetLabel" => "未设置", "Unset";
    "grid.value.emptyLabel" => "空字符串", "Empty string";
    "grid.value.null" => "空值", "Null value";
    "grid.value.binaryLabel" => "二进制", "Binary";
    "grid.value.largeLabel" => "大值", "Large value";
    "grid.status.readonly" => "只读", "Read-only";
    "grid.status.writable" => "可编辑", "Editable";
    "grid.status.unlocated" => "无法定位这一行", "This row cannot be located";
    "grid.status.validation" => "校验失败", "Validation failed";
    "grid.status.execution" => "执行失败", "Execution failed";
    "grid.status.rejectedKept" => "被拒绝的修改仍保留", "Rejected edits are kept";
    "grid.preview.title" => "变更预览", "Change preview";
    "grid.preview.confirm" => "确认写入", "Confirm write";
    "grid.preview.cancel" => "取消预览", "Cancel preview";
    "grid.commit.needPreview" => "先打开预览并确认后才会写入。", "Open the preview and confirm before anything is written.";
    "structure.section.columns" => "字段", "Columns";
    "structure.section.indexes" => "索引", "Indexes";
    "structure.section.foreign" => "外键", "Foreign keys";
    "structure.section.view" => "视图", "View";
    "structure.section.danger" => "危险操作", "Dangerous operation";
    "structure.preview.title" => "结构预览", "Structure preview";
    "structure.preview.need" => "先预览 SQL，确认后才执行。", "Preview the SQL first. It runs only after confirmation.";
    "structure.preview.done" => "已按预览执行。", "The preview was executed.";
    "structure.action.preview" => "预览 SQL", "Preview SQL";
    "structure.action.confirm" => "确认执行", "Confirm";
    "structure.action.cancel" => "取消", "Cancel";
    "admin.section.none" => "这个引擎没有数据库级管理操作。", "This engine has no database-level admin actions.";
    "admin.action.vacuum" => "整理数据库", "Vacuum";
    "admin.action.pragma" => "设置 user_version", "Set user_version";
    "admin.action.sequence" => "修改序列", "Alter sequence";
    "admin.action.event" => "启停事件", "Enable or disable event";
    "admin.preview.unconnected" => "没有连接到这个引擎，预览不会执行。", "This engine is not connected, so the preview will not run.";
    "grid.action.search" => "查找", "Search";
    "grid.action.sort" => "排序", "Sort";
    "grid.action.filter" => "筛选", "Filter";
    "grid.action.pageNext" => "下一页", "Next page";
    "grid.action.pagePrev" => "上一页", "Previous page";
    "grid.action.insert" => "插入行", "Insert row";
    "grid.action.preview" => "预览变更", "Preview changes";
    "admin.action.reindex" => "重建索引", "Reindex";
    "admin.action.attach" => "附加数据库", "Attach database";
    "admin.action.foreignKeys" => "外键开关", "Foreign keys";
    "structure.field.column" => "字段名", "Column name";
    "structure.field.type" => "类型", "Type";
    "structure.field.index" => "索引名", "Index name";
    "structure.field.view" => "视图 SQL", "View SQL";
    "export.open" => "导入、导出和任务", "Import, export, and tasks";
    "export.preview.title" => "拖放文件预览", "Dropped file preview";
    "export.preview.truncated" => "预览已按 2 MB 上限截断，确认后也只写入已读取的行。", "The preview stopped at the 2 MB limit. Confirming writes only the rows already read.";
    "export.preview.noWrite" => "预览阶段不写入任何数据", "Nothing is written during preview";
    "export.import.confirm" => "确认导入", "Confirm import";
    "export.import.cancel" => "取消导入", "Cancel import";
    "export.import.unconfirmed" => "未确认，没有写入目标表。", "Not confirmed, so the target table was not written.";
    "export.import.onlyTable" => "只有数据表可以导入，不能写入视图。", "Only tables can be imported. Views are not written.";
    "export.import.gaps" => "必填字段没有对应列，不能开始写入。", "Required columns are unmapped, so nothing will be written.";
    "export.import.partial" => "导入没有全部成功。", "The import was not fully successful.";
    "export.import.done" => "已写入全部已读取的行。", "Every read row was written.";
    "export.import.xls" => ".xls 不能从拖放预览进入，导入对话框只读取 .xlsx。", ".xls is not a drag-preview format. The import dialog only reads .xlsx.";
    "export.import.choose" => "选择文件", "Choose file";
    "export.export.loaded" => "导出已读取的行", "Export loaded rows";
    "export.export.full" => "导出全部结果", "Export the full result";
    "export.export.partialMark" => "这不是完整结果。", "This is not the full result.";
    "export.dump.start" => "开始全库导出", "Start full database export";
    "export.migrate.start" => "开始跨库迁移", "Start cross-database migration";
    "export.task.title" => "后台任务", "Background tasks";
    "export.task.cancel" => "取消任务", "Cancel task";
    "export.task.cancelled" => "已取消", "Cancelled";
    "export.task.running" => "进行中", "Running";
    "export.task.done" => "已完成", "Finished";
    "export.task.failed" => "失败", "Failed";
    "export.task.partial" => "部分完成", "Partly complete";
    "export.task.blocked" => "未开始", "Not started";
    "export.task.empty" => "没有后台任务。", "No background tasks.";
    "export.task.clear" => "清除已结束的", "Clear finished";
    "export.task.range" => "实际完成范围", "Actual completed range";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_locales_translate_every_catalog_key() {
        for key in catalog_keys() {
            for locale in [Locale::ZhCn, Locale::En] {
                let value = text(locale, key);
                assert_ne!(value, *key, "{key:?} {locale:?}");
                assert!(!value.is_empty(), "{key}");
            }
        }
    }
}
