//! 六类右键菜单。条目 id 都进同一个 `WorkbenchModel::dispatch`。

use gpui_kit::assets::IconName;

use crate::catalog::definition::ddl_available;
use crate::catalog::model::{Engine, ObjectKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuPlace {
    ObjectTree,
    Grid,
    TabBar,
    Editor,
    ConnectionRow,
    HistoryEntry,
    /// 资源管理器里的用户分组标题：重命名、折叠、删除。
    ExplorerGroup,
    /// 查询结果区（结果页签头、消息页）：复制、全屏、收起、关闭结果。
    Results,
}

impl MenuPlace {
    pub fn label_key(self) -> &'static str {
        match self {
            Self::ObjectTree => "catalog.tree.menuLabel",
            Self::Grid => "grid.menu.copy",
            Self::TabBar => "shell.tabs.tablist",
            Self::Editor => "query.editor.area",
            Self::ConnectionRow => "connections.page.moreActions",
            Self::HistoryEntry => "history.page.tabsLabel",
            Self::ExplorerGroup => "catalog.tree.menuLabel",
            Self::Results => "ui.query.results",
        }
    }

    pub fn all() -> [MenuPlace; 6] {
        [Self::ObjectTree, Self::Grid, Self::TabBar, Self::Editor, Self::ConnectionRow, Self::HistoryEntry]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuTarget {
    Connection,
    Object,
    /// 例程/触发器/序列等没有行数据的对象：数据页入口禁用并说明原因。
    NonDataObject,
    /// 列或索引行：动作绑定这一列，不代替整张表。
    Column,
    /// 库/模式/对象种类分组等范围行：只有这一行自己支持的动作。
    Scope,
    /// MongoDB 集合：文档语义，没有关系型表/结构/关系图入口。
    DocumentObject,
    QueryTab,
    ReadonlyGrid,
    WritableGrid,
    Run,
    Saved,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuItem {
    /// 稳定的条目身份；分组名、连接名这类可变文本走 text，不进 id。
    pub id: &'static str,
    pub label_key: &'static str,
    /// 动态文案（分组名）；为空时按 label_key 取译文。
    pub text: Option<String>,
    /// 动态命令（带上目标分组/连接的 id）；为空时命令就是 id。
    pub action: Option<String>,
    pub danger: bool,
    pub enabled: bool,
    pub reason_key: Option<&'static str>,
    /// 动态的禁用原因（如会话给出的具体只读原因）；有它时优先于 reason_key。
    pub reason: Option<String>,
    pub icon: Option<IconName>,
    pub shortcut: Option<&'static str>,
    pub separator_before: bool,
    pub children: Vec<MenuItem>,
}

impl MenuItem {
    /// 这一项真正要执行的命令：动态目标优先，否则就是稳定 id。
    pub fn command(&self) -> String {
        self.action.clone().unwrap_or_else(|| self.id.to_owned())
    }
}

fn leaf(id: &'static str, label_key: &'static str, danger: bool, enabled: bool, reason_key: Option<&'static str>) -> MenuItem {
    MenuItem {
        id,
        label_key,
        text: None,
        action: None,
        danger,
        enabled,
        reason_key,
        reason: None,
        icon: None,
        shortcut: chord_for(id),
        separator_before: false,
        children: Vec::new(),
    }
}

/// 目标写在命令里的条目（分组名、连接 id 由模型现算），id 只做稳定身份。
fn dynamic(id: &'static str, label_key: &'static str, text: String, action: String) -> MenuItem {
    MenuItem { text: Some(text), action: Some(action), ..leaf(id, label_key, false, true, None) }
}

/// 已有命令或快捷键表里的按键：都来自快捷键注册表。没有登记的动作不编一个新的。
fn chord_for(id: &'static str) -> Option<&'static str> {
    command_chord(id)
}

/// 快速打开的行内快捷键：与菜单同源（快捷键注册表），不另编一份。
pub(super) fn command_chord(id: &str) -> Option<&'static str> {
    crate::app::keymap::command_chord(id).or_else(|| crate::app::keymap::shortcut(id).map(|shortcut| shortcut.chord()))
}

/// 连接行菜单的第一项跟着连接状态走：已连接时是“断开连接”，否则是“连接”。
/// 已经连上的连接再给一个“连接”既没用，也让人以为它没连上。
pub fn connection_link_item(connected: bool) -> MenuItem {
    if connected {
        with_icon(item("conn-disconnect", "connections.page.menuDisconnect"), IconName::Unplug)
    } else {
        with_icon(item("conn-connect", "connections.page.connect"), IconName::Plug)
    }
}

/// 资源管理器里连接行的右键菜单：在基础连接操作之上，对已打开的连接补齐刷新目录、
/// 建表、关系图、对比与数据库管理（尤其 SQLite 不单独画库节点，连接行就是库级入口）。
pub fn explorer_connection_menu_items(connected: bool, relational: bool) -> Vec<MenuItem> {
    let mut items = vec![
        connection_link_item(connected),
        with_icon(item("conn-new-query", "shell.command.newQuery"), IconName::SquareTerminal),
    ];
    if connected {
        items.push(with_icon(item("tree-refresh-node", "catalog.tree.refreshNode"), IconName::RefreshCw));
        if relational {
            items.push(sep(with_icon(item("tree-create-table", "structure.action.createTable"), IconName::Plus)));
            items.push(with_icon(item("open-diagram", "catalog.tree.diagram"), IconName::ChartNetwork));
            items.push(with_icon(item("compare-object", "shell.command.compare"), IconName::GitCompare));
            items.push(with_icon(item("open-admin", "ui.tool.admin"), IconName::Gauge));
        }
    }
    items.push(sep(with_icon(item("conn-edit", "connections.page.menuEdit"), IconName::Pencil)));
    items.push(with_icon(item("conn-duplicate", "common.copy"), IconName::Copy));
    items.push(with_icon(item("conn-test", "connections.editor.test.button"), IconName::Zap));
    items.push(sep(with_icon(danger("conn-delete", "connections.page.menuDelete"), IconName::Trash)));
    items
}

/// 启动页“最近执行”行的右键菜单：在新查询中打开、复制完整 SQL、到历史页查看这一条、再次执行。
/// 再次执行对写入语句只打开不执行，由用户确认后自己运行。
pub fn welcome_run_items() -> Vec<MenuItem> {
    vec![
        with_icon(item("welcome-run-open", "ui.welcome.runOpen"), IconName::SquarePen),
        with_icon(item("welcome-run-copy", "ui.welcome.runCopy"), IconName::Copy),
        with_icon(item("welcome-run-history", "ui.welcome.runHistory"), IconName::Clock),
        sep(with_icon(item("welcome-run-again", "ui.welcome.runAgain"), IconName::Play)),
    ]
}

fn item(id: &'static str, label_key: &'static str) -> MenuItem {
    leaf(id, label_key, false, true, None)
}

fn danger(id: &'static str, label_key: &'static str) -> MenuItem {
    leaf(id, label_key, true, true, None)
}

fn disabled(id: &'static str, label_key: &'static str, reason_key: &'static str) -> MenuItem {
    leaf(id, label_key, false, false, Some(reason_key))
}

fn with_icon(mut entry: MenuItem, icon: IconName) -> MenuItem {
    entry.icon = Some(icon);
    entry
}

fn branch(id: &'static str, label_key: &'static str, icon: IconName, children: Vec<MenuItem>) -> MenuItem {
    let enabled = children.iter().any(|child| child.enabled);
    MenuItem {
        id,
        label_key,
        text: None,
        action: None,
        danger: false,
        enabled,
        reason_key: None,
        reason: None,
        icon: Some(icon),
        shortcut: None,
        separator_before: false,
        children,
    }
}

/// 叶子命令。子菜单标题本身不是命令，既有测试用它核对动作仍在。
pub fn flatten_items(items: &[MenuItem]) -> Vec<&MenuItem> {
    let mut out = Vec::new();
    fn walk<'a>(items: &'a [MenuItem], out: &mut Vec<&'a MenuItem>) {
        for item in items {
            if item.children.is_empty() {
                out.push(item);
            } else {
                walk(&item.children, out);
            }
        }
    }
    walk(items, &mut out);
    out
}

/// 主菜单靠近右/下边缘时向左、向上翻，避免画出视口。
pub fn place_popup(anchor_x: f32, anchor_y: f32, width: f32, height: f32, view_w: f32, view_h: f32) -> (f32, f32) {
    let margin = 8.;
    let mut x = anchor_x;
    let mut y = anchor_y;
    if x + width > view_w - margin {
        x = (anchor_x - width).max(margin);
        if x + width > view_w - margin {
            x = (view_w - width - margin).max(margin);
        }
    }
    if y + height > view_h - margin {
        y = (anchor_y - height).max(margin);
        if y + height > view_h - margin {
            y = (view_h - height - margin).max(margin);
        }
    }
    (x, y)
}

/// 子菜单默认开在父项右侧；右侧不够则翻到左侧，贴底时上提。
pub fn place_submenu(parent_x: f32, parent_y: f32, parent_w: f32, width: f32, height: f32, view_w: f32, view_h: f32) -> (f32, f32) {
    let margin = 8.;
    let mut x = parent_x + parent_w - 4.;
    if x + width > view_w - margin {
        x = (parent_x - width).max(margin);
    }
    let mut y = parent_y;
    if y + height > view_h - margin {
        y = (view_h - height - margin).max(margin);
    }
    (x, y)
}

/// 页签右键菜单的现场事实：这个标签是否固定，有几个标签可以不经确认直接关掉。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TabMenuFacts {
    pub pinned: bool,
    pub unmodified: usize,
}

/// 页签菜单。“固定 / 取消固定”随标签状态切换文案；“关闭全部未修改”没有可关的标签时禁用并说明原因。
pub fn tab_bar_items(facts: &TabMenuFacts) -> Vec<MenuItem> {
    let pin = if facts.pinned { with_icon(item("tab-pin", "shell.tabs.unpin"), IconName::PinOff) } else { with_icon(item("tab-pin", "shell.tabs.pin"), IconName::Pin) };
    let unmodified = if facts.unmodified > 0 {
        item("tab-close-unmodified", "shell.tabs.closeUnmodified")
    } else {
        disabled("tab-close-unmodified", "shell.tabs.closeUnmodified", "shell.tabs.noneUnmodified")
    };
    vec![
        with_icon(item("close-tab", "common.close"), IconName::X),
        with_icon(item("tab-close-others", "shell.tabs.closeOthers"), IconName::CircleX),
        with_icon(item("tab-close-right", "shell.tabs.closeRight"), IconName::ArrowRight),
        with_icon(unmodified, IconName::Eraser),
        with_icon(item("tab-close-all", "shell.tabs.closeAll"), IconName::Trash),
        sep(pin),
        sep(with_icon(item("history-copy", "query.editor.menu.copy"), IconName::Copy)),
    ]
}

pub fn menu_items(place: MenuPlace, target: MenuTarget) -> Vec<MenuItem> {
    match place {
        // 分组菜单的条目由模型按分组现算（名字与命令都带目标），这里没有通用项。
        MenuPlace::ExplorerGroup => Vec::new(),
        MenuPlace::Results => results_items(&ResultsMenuFacts::default()),
        MenuPlace::ObjectTree => object_tree_items(target, None, 0),
        MenuPlace::Grid => grid_items(&GridMenuFacts { table: true, writable: target == MenuTarget::WritableGrid, ..GridMenuFacts::default() }),
        MenuPlace::TabBar => tab_bar_items(&TabMenuFacts::default()),
        MenuPlace::Editor => vec![
            with_icon(item("execute-query", "query.editor.menu.run"), IconName::Play),
            with_icon(item("run-current", "query.editor.menu.runCurrent"), IconName::StepForward),
            sep(with_icon(item("format", "query.editor.menu.format"), IconName::WandSparkles)),
            with_icon(item("explain", "query.editor.menu.plan"), IconName::ChartNetwork),
            sep(with_icon(item("history-copy", "query.editor.menu.copy"), IconName::Copy)),
            with_icon(item("open-exchange", "shell.command.openExchange"), IconName::ArrowDownUp),
            with_icon(item("command-palette", "shell.shortcuts.commandPalette"), IconName::Search),
            sep(with_icon(danger("editor-clear", "query.editor.menu.clear"), IconName::Eraser)),
        ],
        // 连接页的行菜单：连接与新建查询在上，配置居中，“移动到分组”由模型插在删除之前，删除最后。
        MenuPlace::ConnectionRow => vec![
            with_icon(item("conn-connect", "connections.page.connect"), IconName::Plug),
            // 新建查询到此连接：先按需连接再打开编辑器，和对象树里的同名入口走同一条续接。
            with_icon(item("conn-new-query", "shell.command.newQuery"), IconName::SquareTerminal),
            sep(with_icon(item("conn-edit", "connections.page.menuEdit"), IconName::Pencil)),
            with_icon(item("conn-duplicate", "common.copy"), IconName::Copy),
            with_icon(item("conn-test", "connections.editor.test.button"), IconName::Zap),
            sep(with_icon(danger("conn-delete", "connections.page.menuDelete"), IconName::Trash)),
        ],
        MenuPlace::HistoryEntry => {
            if target == MenuTarget::Saved {
                vec![
                    with_icon(item("history-load", "history.page.loadToEditor"), IconName::SquarePen),
                    with_icon(item("history-copy", "history.preview.copy"), IconName::Copy),
                    with_icon(item("history-rename", "history.page.rename"), IconName::Pencil),
                    sep(with_icon(danger("saved-delete", "ui.history.delete"), IconName::Trash)),
                ]
            } else {
                vec![
                    with_icon(item("history-load", "history.page.loadToEditor"), IconName::SquarePen),
                    with_icon(item("history-copy", "history.preview.copy"), IconName::Copy),
                    with_icon(item("history-detail", "history.page.sqlPreview"), IconName::Eye),
                    sep(with_icon(item("history-filter-connection", "history.page.filterConnection"), IconName::Funnel)),
                    with_icon(item("history-save-preview", "history.preview.saveQuery"), IconName::Bookmark),
                ]
            }
        }
    }
}

pub use crate::app::views::grid::{GridMenuArea, GridMenuFacts};

fn sep(mut entry: MenuItem) -> MenuItem {
    entry.separator_before = true;
    entry
}

fn keyed(mut entry: MenuItem, chord: &'static str) -> MenuItem {
    entry.shortcut = Some(chord);
    entry
}

/// 只读网格里的写入项：留在原位但禁用，并说明为什么不能改。
/// 有会话给出的具体原因（视图、无主键、聚合……）时用它，否则用通用的只读说明。
fn writable_only(entry: MenuItem, facts: &GridMenuFacts) -> MenuItem {
    if facts.writable {
        entry
    } else {
        let reason = Some(facts.readonly_reason.clone()).filter(|reason| !reason.is_empty());
        MenuItem { enabled: false, reason_key: Some("grid.errors.viewReadonly"), reason, ..entry }
    }
}

/// “复制为”各格式要有选区；空选区时禁用并说明。
fn needs_selection(entry: MenuItem, facts: &GridMenuFacts) -> MenuItem {
    if facts.has_selection {
        entry
    } else {
        MenuItem { enabled: false, reason_key: Some("grid.copyAs.empty"), ..entry }
    }
}

/// “复制为”子菜单：行、列、各种文本格式、SQL 字面量与 INSERT。
fn grid_copy_as(facts: &GridMenuFacts, row_only: bool) -> MenuItem {
    let mut copy_as = vec![item("grid-copy-row", "grid.menu.copyRow")];
    if !row_only {
        copy_as.push(item("grid-copy-column", "grid.menu.copyColumn"));
    }
    copy_as.push(sep(needs_selection(item("grid-copy-as-tsv", "grid.copyAs.tsv"), facts)));
    copy_as.push(needs_selection(item("grid-copy-as-csv", "grid.copyAs.csv"), facts));
    copy_as.push(needs_selection(item("grid-copy-as-json", "grid.copyAs.json"), facts));
    copy_as.push(needs_selection(item("grid-copy-as-markdown", "grid.copyAs.markdown"), facts));
    copy_as.push(sep(item("copy-sql-value", "grid.copyAs.sqlLiteral")));
    copy_as.push(needs_selection(item("grid-copy-as-insert", "grid.copyAs.insert"), facts));
    branch("submenu-copy-as", "grid.menu.copyAs", IconName::ClipboardList, copy_as)
}

/// 行操作子菜单：新增、复制为新行、记录视图、撤销此行、撤销全部暂存。
fn grid_row_ops_submenu(facts: &GridMenuFacts, include_record: bool) -> MenuItem {
    let mut children = vec![
        with_icon(writable_only(item("grid-insert", "grid.action.insert"), facts), IconName::Plus),
        with_icon(writable_only(item("grid-clone-row", "grid.write.clone"), facts), IconName::CopyPlus),
    ];
    if include_record {
        children.push(sep(with_icon(item("grid-record-toggle", "grid.record.open"), IconName::PanelRight)));
    }
    if facts.row_dirty {
        children.push(sep(with_icon(item("grid-undo-row", "grid.write.undoRow"), IconName::RotateCcw)));
    }
    if facts.dirty {
        children.push(with_icon(item("grid-undo", "grid.menu.undo"), IconName::Undo2));
    }
    branch("submenu-row-ops", "grid.menu.rowOps", IconName::Rows3, children)
}

/// 设值子菜单：收拢设为 NULL、空字符串、默认值与批量选区设值，避免低频设值动作铺满一级菜单。
fn grid_set_value_submenu(facts: &GridMenuFacts) -> MenuItem {
    let chord = |id: &str| crate::app::keymap::shortcut(id).map(crate::app::keymap::Shortcut::chord);
    let valued = |entry: MenuItem, id: &str| match chord(id) {
        Some(found) => keyed(entry, found),
        None => entry,
    };
    let children = vec![
        valued(with_icon(writable_only(item("grid-set-null", "grid.menu.setNull"), facts), IconName::Eraser), "grid-set-null"),
        valued(with_icon(writable_only(item("grid-set-empty", "grid.value.setEmpty"), facts), IconName::Space), "grid-set-empty"),
        valued(with_icon(writable_only(item("grid-set-default", "grid.value.setDefault"), facts), IconName::RotateCcw), "grid-set-default"),
        sep(with_icon(writable_only(item("grid-selection-set", "grid.write.setValue"), facts), IconName::ListChecks)),
    ];
    branch("submenu-set-value", "grid.menu.setValue", IconName::SlidersHorizontal, children)
}

/// 筛选与排序子菜单：一级保留高频“按此值筛选”，排除与排序收进二级菜单保持精炼。
fn grid_filter_sort_submenu(facts: &GridMenuFacts) -> MenuItem {
    let mut children = vec![
        with_icon(item("grid-filter-exclude", "grid.menu.filterExclude"), IconName::Ban),
        sep(with_icon(item("grid-column:column-sort-asc", "grid.menu.sortAsc"), IconName::ArrowUpNarrowWide)),
        with_icon(item("grid-column:column-sort-desc", "grid.menu.sortDesc"), IconName::ArrowDownWideNarrow),
    ];
    if facts.conditions {
        children.push(sep(with_icon(item("grid-clear-conditions", "grid.menu.clearConditions"), IconName::FunnelX)));
    }
    branch("submenu-filter-sort", "grid.menu.filterSort", IconName::ListFilter, children)
}

/// 删除行条目：可写时标红并带 Delete 快捷键，只读时禁用且说明原因。
fn grid_delete_row_item(facts: &GridMenuFacts) -> MenuItem {
    let mut delete = keyed(with_icon(writable_only(item("mark-delete", "grid.menu.markDelete"), facts), IconName::Trash), "Delete");
    delete.danger = facts.writable;
    delete
}

/// 生成 SQL 子菜单：单元格菜单带按格、按列的 WHERE，行头菜单只有整行。
fn grid_sql(row_only: bool) -> MenuItem {
    let mut children = vec![item("grid-sql-select", "grid.sql.select"), item("grid-sql-update", "grid.sql.update")];
    if !row_only {
        children.push(sep(item("grid-copy-where", "grid.sql.whereCell")));
    }
    children.push(item("grid-copy-row-where", "grid.sql.whereRow"));
    if !row_only {
        children.push(item("grid-copy-column-where", "grid.sql.whereColumn"));
    }
    branch("submenu-grid-sql", "grid.menu.generateSql", IconName::Braces, children)
}

/// 网格（表数据与查询结果共用）唯一的一套右键菜单。按落点分两种：
/// - 单元格：复制 → 编辑/设值（长值/预览才显示“查看完整值”）→ 筛选/排序 → 行操作/删除 → 生成 SQL/刷新；
/// - 行头（行号列、行尾空白）：整行复制 → 记录视图 → 行操作/删除 → 生成 SQL。
///
/// 列头菜单由表头自己处理（排序、筛选、适配列宽、复制列名、冻结与隐藏）。
/// 不可写时写入项保留但禁用，并显示会话给出的具体原因。
pub fn grid_items(facts: &GridMenuFacts) -> Vec<MenuItem> {
    let row_only = facts.area == GridMenuArea::RowHeader;
    let mut items = vec![keyed(with_icon(item("copy-region", "grid.menu.copy"), IconName::Copy), "Mod+C"), grid_copy_as(facts, row_only)];
    if row_only {
        items.push(sep(with_icon(item("grid-record-toggle", "grid.record.open"), IconName::PanelRight)));
        items.push(sep(grid_row_ops_submenu(facts, false)));
        items.push(with_icon(writable_only(item("grid-selection-set", "grid.write.setValue"), facts), IconName::ListChecks));
        items.push(grid_delete_row_item(facts));
        items.push(sep(grid_sql(true)));
    } else {
        let mut first_edit = true;
        if facts.needs_full_viewer {
            items.push(sep(keyed(with_icon(item("grid-value-open", "grid.value.openFull"), IconName::Eye), "Mod+Enter")));
            first_edit = false;
        }
        let edit_cell = keyed(with_icon(writable_only(item("edit-cell", "grid.menu.editCell"), facts), IconName::SquarePen), "Enter");
        items.push(if first_edit { sep(edit_cell) } else { edit_cell });
        items.push(grid_set_value_submenu(facts));
        if facts.cell_dirty {
            items.push(with_icon(item("grid-undo-cell", "grid.write.undoCell"), IconName::Undo2));
        }
        items.push(sep(with_icon(item("grid-filter-value", "grid.menu.filterByValue"), IconName::Funnel)));
        items.push(grid_filter_sort_submenu(facts));
        if facts.foreign_key {
            items.push(sep(with_icon(item("grid-fk-jump", "grid.fk.jump"), IconName::Link)));
            items.push(with_icon(item("grid-fk-select", "grid.fk.select"), IconName::Search));
        }
        items.push(sep(grid_row_ops_submenu(facts, true)));
        items.push(grid_delete_row_item(facts));
        items.push(sep(grid_sql(false)));
    }
    if facts.unresolved {
        items.push(with_icon(item("grid-full-values", "grid.action.readFull"), IconName::Download));
    }
    if facts.table {
        items.push(with_icon(item("grid-refresh", "grid.menu.refresh"), IconName::RefreshCw));
    }
    items
}

pub use crate::app::views::query::ResultsMenuFacts;

/// 结果区菜单：复制 SQL / 消息、全屏或还原、收起或展开、关闭结果。
/// 菜单内容只与结果区有关，不再落到编辑器菜单。
pub fn results_items(facts: &ResultsMenuFacts) -> Vec<MenuItem> {
    use crate::app::views::query::ResultLayout;
    let gate = |entry: MenuItem, enabled: bool, reason: &'static str| if enabled { entry } else { MenuItem { enabled: false, reason_key: Some(reason), ..entry } };
    let maximized = facts.layout == ResultLayout::Maximized;
    let collapsed = facts.layout == ResultLayout::Collapsed;
    vec![
        with_icon(gate(item("result-copy-sql", "query.results.menu.copySql"), facts.has_sql, "query.results.menu.noSql"), IconName::Copy),
        with_icon(gate(item("result-copy-messages", "query.results.menu.copyMessages"), facts.has_messages, "query.log.empty"), IconName::ClipboardList),
        sep(with_icon(
            gate(
                item("result-maximize", if maximized { "query.results.menu.restore" } else { "query.results.menu.maximize" }),
                facts.has_result || maximized,
                "query.results.menu.noResult",
            ),
            if maximized { IconName::Minimize2 } else { IconName::Maximize2 },
        )),
        with_icon(
            item("result-collapse", if collapsed { "query.results.menu.expand" } else { "query.results.menu.collapse" }),
            if collapsed { IconName::ChevronUp } else { IconName::ChevronDown },
        ),
        sep(with_icon(
            gate(
                gate(item("result-close", "query.results.menu.close"), facts.has_result, "query.results.menu.noResult"),
                !collapsed,
                "query.results.menu.closed",
            ),
            IconName::X,
        )),
    ]
}

/// 表菜单里的对比入口：多选了两张及以上时是“对比表”（正好两张才可用，其他数量禁用并说明原因）；
/// 否则是单表的“对比表…”，打开对比并预填基准侧。
fn compare_table_item(selected_tables: usize) -> MenuItem {
    if selected_tables >= 2 {
        let entry = if selected_tables == 2 {
            item("compare-tables", "catalog.tree.compareTables")
        } else {
            disabled("compare-tables", "catalog.tree.compareTables", "catalog.tree.compareNeedsTwo")
        };
        with_icon(entry, IconName::GitCompare)
    } else {
        with_icon(item("compare-table", "catalog.tree.compareTable"), IconName::GitCompare)
    }
}

fn object_tree_items(target: MenuTarget, facts: Option<ObjectFacts>, selected_tables: usize) -> Vec<MenuItem> {
    if target == MenuTarget::Column {
        return vec![
            with_icon(item("tree-copy-column", "catalog.tree.copyColumn"), IconName::Copy),
            with_icon(item("tree-insert-column", "catalog.tree.insertColumn"), IconName::SquarePen),
        ];
    }
    if target == MenuTarget::Scope {
        let mut items = vec![
            with_icon(item("tree-new-query", "catalog.tree.newQueryHere"), IconName::SquareTerminal),
            with_icon(item("tree-copy-name", "catalog.tree.copyFullName"), IconName::Copy),
            with_icon(item("tree-refresh-node", "catalog.tree.refreshNode"), IconName::RefreshCw),
        ];
        // 没带引擎事实时仍给出管理入口；Redis / MongoDB 明确排除。
        let relational = facts.map(|facts| facts.engine.is_relational()).unwrap_or(true);
        if relational {
            items.push(with_icon(item("tree-create-table", "structure.action.createTable"), IconName::Plus));
            // 对比库留在库/范围行上；表和其它对象改用“对比表”。
            items.push(with_icon(item("compare-object", "shell.command.compare"), IconName::GitCompare));
            items.push(with_icon(item("open-admin", "ui.tool.admin"), IconName::Gauge));
        }
        return items;
    }
    if target == MenuTarget::DocumentObject {
        return vec![
            with_icon(item("tree-open-data", "catalog.tree.openData"), IconName::Table),
            disabled("open-structure", "catalog.tree.viewStructure", "catalog.tree.noStructure"),
            with_icon(item("tree-new-query", "catalog.tree.newQueryHere"), IconName::SquareTerminal),
            with_icon(item("tree-copy-name", "catalog.tree.copyFullName"), IconName::Copy),
            with_icon(item("tree-refresh-node", "catalog.tree.refreshNode"), IconName::RefreshCw),
            branch(
                "submenu-tools",
                "catalog.menu.tools",
                IconName::FolderOpen,
                vec![
                    disabled("open-diagram", "catalog.tree.diagram", "diagram.unsupported"),
                    disabled("exchange-object", "shell.command.openExchange", "catalog.tree.relationalOnly"),
                ],
            ),
        ];
    }
    if target == MenuTarget::Connection {
        // 连接节点：连接与查询在上，配置居中，删除在最下。
        return vec![
            with_icon(item("conn-connect", "connections.page.connect"), IconName::Plug),
            with_icon(item("conn-disconnect", "connections.page.menuDisconnect"), IconName::Unplug),
            sep(with_icon(item("conn-new-query", "shell.command.newQuery"), IconName::SquareTerminal)),
            with_icon(item("tree-refresh-node", "catalog.tree.refreshNode"), IconName::RefreshCw),
            with_icon(item("open-admin", "ui.tool.admin"), IconName::Gauge),
            sep(with_icon(item("conn-edit", "connections.page.menuEdit"), IconName::Pencil)),
            with_icon(item("conn-duplicate", "common.copy"), IconName::Copy),
            with_icon(item("conn-test", "connections.editor.test.button"), IconName::Zap),
            sep(with_icon(danger("conn-delete", "connections.page.menuDelete"), IconName::Trash)),
        ];
    }
    // 表/视图（以及例程等无数据对象）的菜单：常用在上、危险在下，不放低频项。
    // 打开 → 复制名称 → 生成 SQL（含建表语句）→ 工具 → 危险操作。
    // “刷新此节点”交给资源管理器的刷新按钮与连接/分组菜单；“插入到编辑器”由“复制名称”与模板覆盖。
    let data_object = target != MenuTarget::NonDataObject;
    let relational = facts.is_some_and(|facts| facts.engine.is_relational());
    let table = facts.is_some_and(|facts| facts.kind == ObjectKind::Table);
    let view = facts.is_some_and(|facts| facts.kind == ObjectKind::View);
    // 事件、例程与触发器没有结构页，但有统一对象编辑器：那一格就换成编辑入口。
    // 用方言自己声明的“事件”能力判断家族（只有 MySQL / MariaDB 声明），主代码不写死引擎变体。
    let editable_object = facts.is_some_and(|facts| {
        crate::app::views::object_editor::editable(facts.kind) && crate::app::views::admin::sections_for(facts.engine.as_str()).events
    });
    let mut items = vec![
        if data_object {
            with_icon(item("tree-open-data", "catalog.tree.openData"), IconName::Table)
        } else {
            disabled("tree-open-data", "catalog.tree.openData", "catalog.tree.noData")
        },
        if editable_object {
            with_icon(item("open-object-editor", "objectEditor.title"), IconName::SquarePen)
        } else if data_object {
            with_icon(item("open-structure", "catalog.tree.viewStructure"), IconName::TableProperties)
        } else {
            disabled("open-structure", "catalog.tree.viewStructure", "catalog.tree.noStructure")
        },
        with_icon(item("tree-new-query", "catalog.tree.newQueryHere"), IconName::SquareTerminal),
        sep(with_icon(item("tree-copy-name", "catalog.tree.copyFullName"), IconName::Copy)),
    ];

    let mut sql = Vec::new();
    if relational && target == MenuTarget::Object && (table || view) {
        sql.extend([
            item("tree-select-sql", "catalog.tree.templateSelect"),
            item("tree-template-insert", "catalog.tree.templateInsert"),
            item("tree-template-update", "catalog.tree.templateUpdate"),
            item("tree-template-delete", "catalog.tree.templateDelete"),
        ]);
    }
    if let Some(facts) = facts.filter(|facts| facts.engine.is_relational()) {
        if ddl_available(facts.engine, facts.kind) {
            let first = sql.is_empty();
            let mut view_ddl = item("tree-view-ddl", "catalog.tree.viewDdl");
            view_ddl.separator_before = !first;
            sql.push(view_ddl);
            sql.push(item("tree-copy-ddl", "catalog.tree.copyDdl"));
        }
    }
    if !sql.is_empty() {
        items.push(branch("submenu-sql", "catalog.menu.generateSql", IconName::Braces, sql));
    }
    // 名称/限定名插入编辑器：放在生成 SQL 之外的一个入口，供写查询时使用。
    items.push(with_icon(item("tree-insert-object", "catalog.menu.insertName"), IconName::SquarePen));

    // 工具：只给关系型对象，不排一行灰色项。
    if relational {
        items.push(sep(with_icon(item("exchange-object", "shell.command.openExchange"), IconName::ArrowDownUp)));
        if data_object {
            items.push(with_icon(item("open-diagram", "catalog.tree.diagram"), IconName::ChartNetwork));
        }
        if target == MenuTarget::Object && table {
            items.push(compare_table_item(selected_tables));
        }
    }

    // 危险操作平铺在最下面：红色、带省略号表示还要确认，不藏进二级菜单。
    if relational && target == MenuTarget::Object && table {
        items.push(sep(with_icon(item("tree-rename-table", "catalog.tree.renameTable"), IconName::Pencil)));
        let mut truncate = with_icon(danger("tree-truncate-table", "catalog.tree.truncateTable"), IconName::Eraser);
        truncate.danger = true;
        items.push(truncate);
        items.push(with_icon(danger("tree-drop-object", "catalog.tree.dropTable"), IconName::Trash));
    } else if relational && target == MenuTarget::Object && view {
        items.push(sep(with_icon(danger("tree-drop-object", "catalog.tree.dropView"), IconName::Trash)));
    }
    items
}

/// 对象树里一个对象行的事实：引擎与种类。DDL、模板与危险项都据此门控。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObjectFacts {
    pub engine: Engine,
    pub kind: ObjectKind,
}

/// 对象树右键菜单：在六类菜单的对象树条目上，按这一行的引擎与种类补上
/// “刷新此节点”“查看/复制 DDL”“SQL 模板”与对象级危险操作。
///
/// - 范围行（连接/库/模式/种类分组/键空间）只多一个“刷新此节点”。
/// - DDL 项只给关系型对象行；读不到定义时禁用并说明原因，不打开空查看器。
/// - 模板与危险项只给关系型引擎的表/视图；清空与重命名只给表。
///   MongoDB、Redis、例程/触发器/序列等、范围行与列行都没有这些项（R11）。
#[cfg_attr(not(test), allow(dead_code))]
pub fn tree_menu_items(target: MenuTarget, facts: Option<ObjectFacts>) -> Vec<MenuItem> {
    object_tree_items(target, facts, 0)
}

/// 同上，并按资源树里 Ctrl/⌘ 多选的表数量给出“对比表”的可用状态。
pub fn tree_menu_items_with_selection(target: MenuTarget, facts: Option<ObjectFacts>, selected_tables: usize) -> Vec<MenuItem> {
    object_tree_items(target, facts, selected_tables)
}


/// 带动态命令、静态文案的条目：折叠/展开、重命名这类目标写在命令里的动作。
fn targeted(id: &'static str, label_key: &'static str, action: String) -> MenuItem {
    MenuItem { action: Some(action), ..leaf(id, label_key, false, true, None) }
}

/// 对象概览里一行的菜单（这一行还没读进对象树时用；读进来了就用对象树同一册菜单）。
/// 表/视图：打开数据、查看结构、新建查询、复制名称；库/模式：打开概览、新建查询、复制名称。
pub fn overview_menu_items(name: &str, object: bool, relational: bool) -> Vec<MenuItem> {
    let mut items = Vec::new();
    if object {
        items.push(with_icon(targeted("overview-open-data", "catalog.tree.openData", format!("overview-open-data:{name}")), IconName::Table));
        items.push(with_icon(
            targeted("overview-open-structure", "catalog.tree.viewStructure", format!("overview-open-structure:{name}")),
            IconName::TableProperties,
        ));
    } else {
        items.push(with_icon(targeted("overview-open-child", "catalog.overview.open", format!("overview-open-child:{name}")), IconName::LayoutDashboard));
    }
    if relational {
        items.push(sep(with_icon(
            targeted("overview-new-query", "catalog.tree.newQueryHere", format!("overview-new-query:{name}")),
            IconName::SquareTerminal,
        )));
    }
    items.push(with_icon(targeted("overview-copy-name", "catalog.tree.copyFullName", format!("overview-copy-name:{name}")), IconName::Copy));
    items
}

/// 只读行列表（库概览、历史）的复制条目：与 `ui::read_table` 的右键菜单同一组方式与文案。
/// 命令为 `{prefix}:{kind}`，由各列表按当前多选与右键所在的列生成文本。第一项前加分隔线。
pub fn row_copy_items(prefix: &str) -> Vec<MenuItem> {
    use crate::ui::read_table::CopyKind;
    CopyKind::MENU
        .iter()
        .enumerate()
        .map(|(index, kind)| {
            let entry = with_icon(targeted(kind.id(), kind.label_key(), format!("{prefix}:{}", row_copy_kind_id(*kind))), IconName::Copy);
            if index == 0 {
                sep(entry)
            } else {
                entry
            }
        })
        .collect()
}

/// 复制方式在命令里的名字。
pub fn row_copy_kind_id(kind: crate::ui::read_table::CopyKind) -> &'static str {
    use crate::ui::read_table::CopyKind;
    match kind {
        CopyKind::Value => "value",
        CopyKind::Row => "row",
        CopyKind::Column => "column",
        CopyKind::Tsv => "tsv",
        CopyKind::Csv => "csv",
        CopyKind::Json => "json",
    }
}

pub fn parse_row_copy_kind(id: &str) -> Option<crate::ui::read_table::CopyKind> {
    crate::ui::read_table::CopyKind::MENU.into_iter().find(|kind| row_copy_kind_id(*kind) == id)
}

/// 资源管理器分组标题的菜单：新建、折叠/展开、重命名、删除（连接保留）。
pub fn group_menu_items(group_id: &str, collapsed: bool) -> Vec<MenuItem> {
    let toggle_key = if collapsed { "ui.group.expand" } else { "ui.group.collapse" };
    vec![
        with_icon(targeted("conn-new-in-group", "conn.page.newInGroup", format!("conn-new-in-group:{group_id}")), IconName::Plus),
        with_icon(item("group-new", "ui.group.new"), IconName::FolderPlus),
        with_icon(targeted("group-toggle", toggle_key, format!("group-toggle:{group_id}")), IconName::ChevronDown),
        with_icon(targeted("group-rename", "ui.group.rename", format!("group-rename:{group_id}")), IconName::SquarePen),
        MenuItem {
            danger: true,
            separator_before: true,
            ..targeted("group-delete", "ui.group.delete", format!("group-delete:{group_id}"))
        },
    ]
}

/// 连接菜单里的“移动到分组”子菜单。拖拽与这条菜单落同一个 Store 命令。
pub fn move_to_group_branch(
    connection_id: &str,
    groups: &[crate::connections::groups::ConnectionGroup],
    current: Option<&str>,
) -> MenuItem {
    let mut children = Vec::new();
    for group in groups {
        let mut entry = dynamic(
            "group-assign",
            "ui.group.moveTo",
            group.name.clone(),
            format!("group-assign:{connection_id}\u{1}{}", group.id),
        );
        // 已经在这个分组里：标出来，不给重复动作。
        if current == Some(group.id.as_str()) {
            entry.enabled = false;
            entry.reason_key = Some("ui.group.alreadyHere");
        }
        children.push(with_icon(entry, IconName::Folder));
    }
    if children.is_empty() {
        children.push(disabled("group-assign", "ui.group.none", "ui.group.noneHint"));
    }
    if current.is_some() {
        let out = MenuItem {
            separator_before: true,
            ..targeted("group-assign", "ui.group.moveOut", format!("group-assign:{connection_id}\u{1}"))
        };
        children.push(out);
    }
    let mut branch = branch("submenu-move-group", "ui.group.moveTo", IconName::Folder, children);
    branch.separator_before = true;
    branch
}

#[derive(Clone, Debug)]
pub struct OpenMenu {
    pub place: MenuPlace,
    pub items: Vec<MenuItem>,
    pub index: usize,
    /// 打开的子菜单：(父下标, 子下标)。
    pub submenu: Option<(usize, usize)>,
}

impl OpenMenu {
    pub fn new(place: MenuPlace, target: MenuTarget) -> Self {
        Self::with_items(place, menu_items(place, target))
    }

    pub fn with_items(place: MenuPlace, items: Vec<MenuItem>) -> Self {
        let index = items.iter().position(|item| item.enabled).unwrap_or(0);
        Self { place, items, index, submenu: None }
    }

    pub fn move_by(&mut self, step: isize) {
        if let Some((parent, child)) = self.submenu {
            let Some(children) = self.items.get(parent).map(|item| &item.children) else {
                self.submenu = None;
                return;
            };
            let enabled: Vec<usize> = children.iter().enumerate().filter(|(_, item)| item.enabled).map(|(index, _)| index).collect();
            if enabled.is_empty() {
                return;
            }
            let current = enabled.iter().position(|index| *index == child).unwrap_or(0);
            let next = if step >= 0 {
                enabled[(current + 1) % enabled.len()]
            } else {
                enabled[(current + enabled.len() - 1) % enabled.len()]
            };
            self.submenu = Some((parent, next));
            return;
        }
        if self.items.is_empty() {
            return;
        }
        let enabled: Vec<usize> = self.items.iter().enumerate().filter(|(_, item)| item.enabled).map(|(index, _)| index).collect();
        if enabled.is_empty() {
            return;
        }
        let current = enabled.iter().position(|index| *index == self.index).unwrap_or(0);
        let next = if step >= 0 {
            enabled[(current + 1) % enabled.len()]
        } else {
            enabled[(current + enabled.len() - 1) % enabled.len()]
        };
        self.index = next;
    }

    pub fn home(&mut self) {
        if let Some((parent, _)) = self.submenu {
            if let Some(child) = self.items.get(parent).and_then(|item| item.children.iter().position(|child| child.enabled)) {
                self.submenu = Some((parent, child));
            }
            return;
        }
        if let Some(index) = self.items.iter().position(|item| item.enabled) {
            self.index = index;
        }
    }

    pub fn end(&mut self) {
        if let Some((parent, _)) = self.submenu {
            if let Some(child) = self.items.get(parent).and_then(|item| item.children.iter().rposition(|child| child.enabled)) {
                self.submenu = Some((parent, child));
            }
            return;
        }
        if let Some(index) = self.items.iter().rposition(|item| item.enabled) {
            self.index = index;
        }
    }

    /// 有子菜单的当前项展开，并落在第一个可用子项。
    pub fn open_submenu(&mut self) -> bool {
        let Some(parent) = self.items.get(self.index) else { return false };
        if parent.children.is_empty() || !parent.enabled {
            return false;
        }
        let child = parent.children.iter().position(|item| item.enabled).unwrap_or(0);
        self.submenu = Some((self.index, child));
        true
    }

    pub fn close_submenu(&mut self) -> bool {
        self.submenu.take().is_some()
    }

    /// 悬停父项：切到这一项；有子菜单就换开它，没有就收起。
    pub fn hover(&mut self, index: usize) {
        if self.items.get(index).is_none() {
            return;
        }
        self.index = index;
        if self.items[index].children.is_empty() || !self.items[index].enabled {
            self.submenu = None;
            return;
        }
        let child = self.items[index].children.iter().position(|item| item.enabled).unwrap_or(0);
        self.submenu = Some((index, child));
    }

    pub fn current(&self) -> Option<&MenuItem> {
        if let Some((parent, child)) = self.submenu {
            return self.items.get(parent).and_then(|item| item.children.get(child)).filter(|item| item.enabled);
        }
        self.items.get(self.index).filter(|item| item.enabled && item.children.is_empty())
    }

    pub fn active_parent(&self) -> Option<&MenuItem> {
        self.items.get(self.index)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavKey {
    Escape,
    ArrowDown,
    ArrowUp,
    ArrowLeft,
    ArrowRight,
    Enter,
    Home,
    End,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn six_menus_skip_disabled_items_and_keep_command_ids() {
        for place in MenuPlace::all() {
            let target = match place {
                MenuPlace::Grid => MenuTarget::ReadonlyGrid,
                MenuPlace::ObjectTree => MenuTarget::Object,
                _ => MenuTarget::QueryTab,
            };
            let mut menu = OpenMenu::new(place, target);
            assert!(!menu.items.is_empty(), "{place:?}");
            assert!(menu.items.iter().all(|item| !item.id.is_empty() && !item.label_key.is_empty()));
            menu.home();
            let first = menu.index;
            menu.move_by(1);
            assert_ne!(menu.index, first);
            assert!(menu.items[menu.index].enabled);
            menu.end();
            assert!(menu.items[menu.index].enabled);
        }
        let grid = OpenMenu::new(MenuPlace::Grid, MenuTarget::ReadonlyGrid);
        assert!(grid.items.iter().any(|item| item.id == "edit-cell" && !item.enabled));
        assert!(grid.items.iter().any(|item| item.id == "mark-delete" && item.reason_key == Some("grid.errors.viewReadonly")));
        let writable = OpenMenu::new(MenuPlace::Grid, MenuTarget::WritableGrid);
        assert!(writable.items.iter().any(|item| item.id == "edit-cell" && item.enabled));

        // 例程等没有行数据的对象：数据页入口禁用并说明原因。
        let routine = OpenMenu::new(MenuPlace::ObjectTree, MenuTarget::NonDataObject);
        let open_data = routine.items.iter().find(|item| item.id == "tree-open-data").expect("open data");
        assert!(!open_data.enabled);
        assert_eq!(open_data.reason_key, Some("catalog.tree.noData"));
        assert!(flatten_items(&routine.items).iter().any(|item| item.id == "tree-insert-object"));

        // 列行只提供列自己的动作。
        let column = OpenMenu::new(MenuPlace::ObjectTree, MenuTarget::Column);
        let ids: Vec<&str> = column.items.iter().map(|item| item.id).collect();
        assert_eq!(ids, vec!["tree-copy-column", "tree-insert-column"]);
    }

    const DANGER: [&str; 3] = ["tree-drop-object", "tree-truncate-table", "tree-rename-table"];
    const TEMPLATES: [&str; 3] = ["tree-template-insert", "tree-template-update", "tree-template-delete"];

    fn ids(items: &[MenuItem]) -> Vec<&'static str> {
        flatten_items(items).into_iter().map(|item| item.id).collect()
    }

    fn top_ids(items: &[MenuItem]) -> Vec<&'static str> {
        items.iter().map(|item| item.id).collect()
    }

    fn facts(engine: Engine, kind: ObjectKind) -> Option<ObjectFacts> {
        Some(ObjectFacts { engine, kind })
    }

    #[test]
    fn danger_items_only_on_relational_tables_and_views() {
        for engine in [Engine::Sqlite, Engine::Mysql, Engine::Mariadb, Engine::Postgres, Engine::Sqlserver] {
            // 表：清空/删除是危险项（红色、一级、在最下面）；重命名在旁边但不标红。
            let table = tree_menu_items(MenuTarget::Object, facts(engine, ObjectKind::Table));
            for id in ["tree-truncate-table", "tree-drop-object"] {
                let item = table.iter().find(|item| item.id == id).unwrap_or_else(|| panic!("{engine:?} {id}"));
                assert!(item.danger && item.enabled, "{engine:?} {id}");
            }
            let rename = table.iter().find(|item| item.id == "tree-rename-table").expect("rename");
            assert!(rename.enabled && !rename.danger);
            assert_eq!(table.last().map(|item| item.id), Some("tree-drop-object"), "删除表在最下面");
            assert!(TEMPLATES.iter().all(|id| ids(&table).contains(id)), "{engine:?}");
            // 视图：只有删除，清空与重命名只给表。
            let view = tree_menu_items(MenuTarget::Object, facts(engine, ObjectKind::View));
            let drop = flatten_items(&view).into_iter().find(|item| item.id == "tree-drop-object").expect("drop view");
            assert_eq!(drop.label_key, "catalog.tree.dropView");
            assert!(!ids(&view).contains(&"tree-truncate-table") && !ids(&view).contains(&"tree-rename-table"));
            assert!(TEMPLATES.iter().all(|id| ids(&view).contains(id)));
        }
        // MongoDB 集合、Redis、非数据对象、范围行与列行都没有危险项和模板。
        let cases = [
            (MenuTarget::DocumentObject, facts(Engine::Mongodb, ObjectKind::Table)),
            (MenuTarget::Object, facts(Engine::Mongodb, ObjectKind::Table)),
            (MenuTarget::Object, facts(Engine::Redis, ObjectKind::Table)),
            (MenuTarget::Scope, None),
            (MenuTarget::Column, None),
            (MenuTarget::NonDataObject, facts(Engine::Postgres, ObjectKind::Sequence)),
            (MenuTarget::NonDataObject, facts(Engine::Postgres, ObjectKind::Function)),
            (MenuTarget::NonDataObject, facts(Engine::Mysql, ObjectKind::Procedure)),
            (MenuTarget::NonDataObject, facts(Engine::Sqlite, ObjectKind::Trigger)),
            (MenuTarget::NonDataObject, facts(Engine::Postgres, ObjectKind::Extension)),
            (MenuTarget::NonDataObject, facts(Engine::Mysql, ObjectKind::Event)),
            (MenuTarget::NonDataObject, facts(Engine::Postgres, ObjectKind::Enum)),
            (MenuTarget::NonDataObject, facts(Engine::Postgres, ObjectKind::Domain)),
        ];
        for (target, facts) in cases {
            let items = tree_menu_items(target, facts);
            for id in DANGER.iter().chain(TEMPLATES.iter()) {
                assert!(!ids(&items).contains(id), "{target:?} {facts:?} {id}");
            }
            assert!(items.iter().all(|item| !item.danger), "{target:?} {facts:?}");
        }
        // 没有对象事实时（通用对象菜单）不凭空给出危险项。
        assert!(tree_menu_items(MenuTarget::Object, None).iter().all(|item| !item.danger));
    }

    #[test]
    fn ddl_items_follow_definition_availability_and_scope_rows_refresh() {
        let table = tree_menu_items(MenuTarget::Object, facts(Engine::Postgres, ObjectKind::Table));
        // 查看/复制 DDL 归在“生成 SQL”里，与模板同组。
        for id in ["tree-view-ddl", "tree-copy-ddl"] {
            assert!(flatten_items(&table).iter().any(|item| item.id == id && item.enabled), "{id}");
        }
        // 扩展读不到定义：不出现 DDL 项（不摆一排灰色项）。
        let extension = tree_menu_items(MenuTarget::NonDataObject, facts(Engine::Postgres, ObjectKind::Extension));
        assert!(!ids(&extension).contains(&"tree-view-ddl") && !ids(&extension).contains(&"tree-copy-ddl"));
        // 例程有定义：DDL 可用，但仍没有危险项。
        let routine = tree_menu_items(MenuTarget::NonDataObject, facts(Engine::Postgres, ObjectKind::Function));
        assert!(flatten_items(&routine).iter().any(|item| item.id == "tree-view-ddl" && item.enabled));
        // SQLite 序列之类没有定义来源。
        let sqlite = tree_menu_items(MenuTarget::NonDataObject, facts(Engine::Sqlite, ObjectKind::Sequence));
        assert!(!ids(&sqlite).contains(&"tree-view-ddl"));
        // MongoDB 集合没有 DDL 项。
        let mongo = tree_menu_items(MenuTarget::DocumentObject, facts(Engine::Mongodb, ObjectKind::Table));
        assert!(!ids(&mongo).contains(&"tree-view-ddl"));
        // 范围行：自己的动作，加上关系型的新建表和管理页；刷新留在范围行上。
        assert_eq!(
            top_ids(&tree_menu_items(MenuTarget::Scope, None)),
            vec!["tree-new-query", "tree-copy-name", "tree-refresh-node", "tree-create-table", "compare-object", "open-admin"]
        );
        let redis_scope = tree_menu_items(MenuTarget::Scope, facts(Engine::Redis, ObjectKind::Table));
        assert!(!ids(&redis_scope).contains(&"open-admin"));
        // 表菜单：打开数据/结构/新建查询在最前，一级没有低频的刷新与管理页。
        let top = top_ids(&table);
        assert_eq!(&top[..3], &["tree-open-data", "open-structure", "tree-new-query"]);
        assert!(top.contains(&"submenu-sql") && top.contains(&"tree-copy-name"));
        assert!(!ids(&table).contains(&"tree-refresh-node") && !ids(&table).contains(&"open-admin"));
        assert!(ids(&table).contains(&"tree-select-sql"));
        assert!(ids(&table).contains(&"open-diagram"));
        assert!(ids(&table).contains(&"exchange-object"));
    }

    /// 网格菜单按现场事实生成：只读（含只读查询结果）的写入项保留但禁用，悬停说明会话给出的具体原因；
    /// 查看完整值仅在长值/预览/多行/JSON 时出现，低频设值/排序/行操作收拢到二级菜单保持精炼。
    #[test]
    fn grid_menu_follows_the_cell_it_was_opened_on() {
        let reason = "结果来自聚合查询，不能写回。".to_owned();
        let query = grid_items(&GridMenuFacts { readonly_reason: reason.clone(), has_selection: true, ..GridMenuFacts::default() });
        let query_ids = ids(&query);
        assert!(query_ids.contains(&"copy-region") && query_ids.contains(&"grid-filter-value"));
        // 短值单元格不展示“查看完整值”，避免无意义的菜单项。
        assert!(!query_ids.contains(&"grid-value-open"), "短值单元格不显示查看完整值");
        let query_leaves = flatten_items(&query);
        for id in ["edit-cell", "grid-set-null", "grid-set-empty", "grid-set-default", "grid-insert", "grid-clone-row", "grid-selection-set", "mark-delete"] {
            let entry = query_leaves.iter().find(|item| item.id == id).unwrap_or_else(|| panic!("只读查询结果也列出 {id}"));
            assert!(!entry.enabled, "{id}");
            assert_eq!(entry.reason.as_deref(), Some(reason.as_str()), "{id} 说明具体原因");
        }
        for id in ["grid-refresh", "grid-undo-cell", "grid-undo-row", "grid-undo", "grid-clear-conditions", "grid-fk-jump", "grid-full-values"] {
            assert!(!query_ids.contains(&id), "查询结果不该有 {id}");
        }
        let all: Vec<&str> = query_leaves.iter().map(|item| item.id).collect();
        for id in [
            "grid-copy-row",
            "grid-copy-column",
            "grid-copy-as-tsv",
            "grid-copy-as-csv",
            "grid-copy-as-json",
            "grid-copy-as-markdown",
            "copy-sql-value",
            "grid-copy-as-insert",
            "grid-copy-where",
            "grid-copy-row-where",
            "grid-copy-column-where",
            "grid-sql-select",
            "grid-sql-update",
            "grid-record-toggle",
        ] {
            assert!(all.contains(&id), "{id}");
        }

        let readonly = grid_items(&GridMenuFacts { table: true, ..GridMenuFacts::default() });
        let readonly_leaves = flatten_items(&readonly);
        for id in ["edit-cell", "grid-set-null", "grid-set-empty", "grid-set-default", "grid-insert", "grid-clone-row", "mark-delete"] {
            let item = readonly_leaves.iter().find(|item| item.id == id).unwrap_or_else(|| panic!("{id}"));
            assert!(!item.enabled && item.reason_key == Some("grid.errors.viewReadonly") && item.reason.is_none(), "{id}");
        }
        assert!(!readonly.iter().find(|item| item.id == "mark-delete").unwrap().danger, "禁用的删除不标红");
        // 空选区：“复制为”各格式禁用并说明。
        let copy_as = readonly.iter().find(|item| item.id == "submenu-copy-as").unwrap();
        let tsv = copy_as.children.iter().find(|item| item.id == "grid-copy-as-tsv").unwrap();
        assert!(!tsv.enabled && tsv.reason_key == Some("grid.copyAs.empty"));

        let busy = grid_items(&GridMenuFacts {
            table: true,
            writable: true,
            cell_dirty: true,
            row_dirty: true,
            foreign_key: true,
            conditions: true,
            dirty: true,
            unresolved: true,
            has_selection: true,
            needs_full_viewer: true,
            ..GridMenuFacts::default()
        });
        let busy_ids = ids(&busy);
        for id in ["grid-value-open", "grid-undo-cell", "grid-undo-row", "grid-undo", "grid-clear-conditions", "grid-fk-jump", "grid-fk-select", "grid-full-values", "grid-refresh"] {
            assert!(busy_ids.contains(&id), "{id}");
        }
        let delete = busy.iter().find(|item| item.id == "mark-delete").unwrap();
        assert!(delete.enabled && delete.danger && delete.shortcut == Some("Delete"));
        assert_eq!(busy.first().map(|item| (item.id, item.shortcut)), Some(("copy-region", Some("Mod+C"))));
        // 一级菜单保持精简，不铺开 20 项杂乱条目。
        let clean_cell = grid_items(&GridMenuFacts { table: true, writable: true, has_selection: true, ..GridMenuFacts::default() });
        assert!(clean_cell.len() <= 11, "普通单元格一级菜单应精炼（实际 {} 项）", clean_cell.len());

        // 行头：整行的操作；没有按格编辑、按值筛选和排序，生成 SQL 只有整行。
        let row = grid_items(&GridMenuFacts { area: GridMenuArea::RowHeader, table: true, writable: true, has_selection: true, ..GridMenuFacts::default() });
        let row_ids = ids(&row);
        for id in ["copy-region", "grid-record-toggle", "grid-insert", "grid-clone-row", "mark-delete"] {
            assert!(row_ids.contains(&id), "{id}");
        }
        for id in ["edit-cell", "grid-set-null", "grid-filter-value", "grid-column:column-sort-asc", "grid-value-open"] {
            assert!(!row_ids.contains(&id), "行头菜单不该有 {id}");
        }
        let row_sql: Vec<&str> = row.iter().find(|item| item.id == "submenu-grid-sql").unwrap().children.iter().map(|item| item.id).collect();
        assert_eq!(row_sql, vec!["grid-sql-select", "grid-sql-update", "grid-copy-row-where"]);
        assert_ne!(ids(&busy), row_ids, "单元格与行头的菜单不同");
    }

    /// 结果区菜单：复制、全屏 / 退出全屏、收起 / 展开、关闭结果；没有结果时全屏与关闭禁用；关闭只隐藏结果区、不删结果，已收起时禁用。
    #[test]
    fn results_menu_offers_layout_and_copy_for_the_result_area() {
        use crate::app::views::query::ResultLayout;
        let empty = results_items(&ResultsMenuFacts::default());
        assert_eq!(ids(&empty), vec!["result-copy-sql", "result-copy-messages", "result-maximize", "result-collapse", "result-close"]);
        let state = |items: &[MenuItem], id: &str| items.iter().find(|item| item.id == id).map(|item| (item.enabled, item.reason_key, item.label_key)).unwrap();
        assert_eq!(state(&empty, "result-maximize"), (false, Some("query.results.menu.noResult"), "query.results.menu.maximize"));
        assert!(!state(&empty, "result-close").0);
        assert!(state(&empty, "result-collapse").0, "没有结果也可以收起");
        let full = results_items(&ResultsMenuFacts { has_result: true, has_sql: true, has_messages: true, layout: ResultLayout::Maximized });
        assert!(full.iter().all(|item| item.enabled));
        assert_eq!(state(&full, "result-maximize").2, "query.results.menu.restore");
        let collapsed = results_items(&ResultsMenuFacts { has_result: true, layout: ResultLayout::Collapsed, ..ResultsMenuFacts::default() });
        assert_eq!(state(&collapsed, "result-collapse").2, "query.results.menu.expand");
        assert_eq!(state(&collapsed, "result-close"), (false, Some("query.results.menu.closed"), "query.results.menu.close"));
        assert_eq!(MenuPlace::Results.label_key(), "ui.query.results");
    }

    #[test]
    fn compare_tables_follows_the_multi_selection_and_compare_databases_stays_on_scopes() {
        let entry = |items: &[MenuItem], id: &str| flatten_items(items).into_iter().find(|item| item.id == id).cloned();
        for engine in [Engine::Sqlite, Engine::Mysql, Engine::Postgres, Engine::Sqlserver] {
            // 单表：“对比表…”可用；表菜单里不再有“对比库”。
            let single = tree_menu_items_with_selection(MenuTarget::Object, facts(engine, ObjectKind::Table), 0);
            assert!(entry(&single, "compare-table").is_some_and(|item| item.enabled), "{engine:?}");
            assert!(entry(&single, "compare-object").is_none(), "{engine:?} 表菜单没有对比库");
            assert!(entry(&single, "compare-tables").is_none());
            // 正好两张：“对比表”可用。
            let two = tree_menu_items_with_selection(MenuTarget::Object, facts(engine, ObjectKind::Table), 2);
            let pair = entry(&two, "compare-tables").expect("对比表");
            assert!(pair.enabled && pair.label_key == "catalog.tree.compareTables", "{engine:?}");
            assert!(entry(&two, "compare-object").is_none());
            // 三张：禁用并说明原因。
            let three = tree_menu_items_with_selection(MenuTarget::Object, facts(engine, ObjectKind::Table), 3);
            let pair = entry(&three, "compare-tables").expect("对比表");
            assert!(!pair.enabled && pair.reason_key == Some("catalog.tree.compareNeedsTwo"), "{engine:?}");
            // 视图与其它对象：没有对比库，也没有对比表。
            for (target, kind) in [(MenuTarget::Object, ObjectKind::View), (MenuTarget::NonDataObject, ObjectKind::Function)] {
                let items = tree_menu_items_with_selection(target, facts(engine, kind), 2);
                for id in ["compare-object", "compare-table", "compare-tables"] {
                    assert!(entry(&items, id).is_none(), "{engine:?} {kind:?} {id}");
                }
            }
        }
        // 库/范围行仍有“对比库”，连接行也保留。
        let scope = tree_menu_items(MenuTarget::Scope, facts(Engine::Postgres, ObjectKind::Table));
        assert!(entry(&scope, "compare-object").is_some_and(|item| item.enabled && item.label_key == "shell.command.compare"));
        assert!(entry(&explorer_connection_menu_items(true, true), "compare-object").is_some());
        // Redis、MongoDB 节点不显示对比表，也不显示对比库。
        for (target, engine) in [
            (MenuTarget::DocumentObject, Engine::Mongodb),
            (MenuTarget::Object, Engine::Mongodb),
            (MenuTarget::Object, Engine::Redis),
            (MenuTarget::Scope, Engine::Redis),
            (MenuTarget::Scope, Engine::Mongodb),
        ] {
            let items = tree_menu_items_with_selection(target, facts(engine, ObjectKind::Table), 2);
            for id in ["compare-table", "compare-tables", "compare-object"] {
                assert!(entry(&items, id).is_none(), "{target:?} {engine:?} {id}");
            }
        }
    }

    #[test]
    fn menu_items_keep_chords_that_already_exist() {
        let editor = menu_items(MenuPlace::Editor, MenuTarget::QueryTab);
        assert_eq!(editor.iter().find(|item| item.id == "execute-query").and_then(|item| item.shortcut), Some("Mod+Enter"));
        assert_eq!(editor.iter().find(|item| item.id == "command-palette").and_then(|item| item.shortcut), Some("Mod+K"));
        let tabs = menu_items(MenuPlace::TabBar, MenuTarget::QueryTab);
        assert_eq!(tabs.iter().find(|item| item.id == "close-tab").and_then(|item| item.shortcut), Some("Mod+W"));
    }

    #[test]
    fn submenu_arrows_open_and_escape_closes_the_child_first() {
        let mut menu = OpenMenu::with_items(MenuPlace::ObjectTree, tree_menu_items(MenuTarget::Object, facts(Engine::Postgres, ObjectKind::Table)));
        let sql = menu.items.iter().position(|item| item.id == "submenu-sql").expect("sql");
        menu.index = sql;
        assert!(menu.open_submenu());
        assert_eq!(menu.submenu.map(|pair| pair.0), Some(sql));
        assert_eq!(menu.current().map(|item| item.id), Some("tree-select-sql"));
        menu.move_by(1);
        assert_eq!(menu.current().map(|item| item.id), Some("tree-template-insert"));
        assert!(menu.close_submenu());
        assert!(menu.submenu.is_none());
        menu.hover(sql);
        assert!(menu.submenu.is_some());
        let plain = menu.items.iter().position(|item| item.id == "tree-open-data").unwrap();
        menu.hover(plain);
        assert!(menu.submenu.is_none());
    }

    #[test]
    fn popup_flips_on_both_axes_and_submenu_flips_left() {
        let (x, y) = place_popup(900., 700., 232., 280., 1000., 800.);
        assert!(x + 232. <= 1000. - 8. + 0.5, "{x}");
        assert!(y + 280. <= 800. - 8. + 0.5, "{y}");
        assert!(x < 900.);
        assert!(y < 700.);
        let (right, _) = place_submenu(100., 40., 232., 220., 160., 1360., 900.);
        assert!(right >= 100. + 232. - 8., "默认开在右侧 {right}");
        let (left, _) = place_submenu(1200., 40., 232., 220., 160., 1360., 900.);
        assert!(left + 220. <= 1200. + 0.5, "右侧不够时翻到左侧 {left}");
        let (_, raised) = place_submenu(100., 800., 232., 220., 200., 1360., 900.);
        assert!(raised + 200. <= 900. - 8. + 0.5, "{raised}");
    }

    /// MySQL / MariaDB 的事件、例程与触发器把“查看结构”那一格换成统一对象编辑器。
    #[test]
    fn object_editor_entry_replaces_structure_for_mysql_objects() {
        for kind in [ObjectKind::Event, ObjectKind::Function, ObjectKind::Procedure, ObjectKind::Trigger] {
            for engine in [Engine::Mysql, Engine::Mariadb] {
                let items = tree_menu_items(MenuTarget::NonDataObject, facts(engine, kind));
                let entry = items.iter().find(|item| item.id == "open-object-editor").unwrap_or_else(|| panic!("{engine:?} {kind:?}"));
                assert!(entry.enabled && !entry.danger, "{engine:?} {kind:?}");
                assert!(!ids(&items).contains(&"open-structure"), "结构页不处理这些对象：{engine:?} {kind:?}");
            }
        }
        // 其它引擎的例程仍走定义页，不凭空给出 MySQL 专属入口。
        let pg = tree_menu_items(MenuTarget::NonDataObject, facts(Engine::Postgres, ObjectKind::Function));
        assert!(!ids(&pg).contains(&"open-object-editor"));
        assert!(ids(&pg).contains(&"open-structure"));
        // 表与视图不是对象编辑器的范围。
        for kind in [ObjectKind::Table, ObjectKind::View] {
            let items = tree_menu_items(MenuTarget::Object, facts(Engine::Mysql, kind));
            assert!(!ids(&items).contains(&"open-object-editor"), "{kind:?}");
            assert!(ids(&items).contains(&"open-structure"), "{kind:?}");
        }
    }
}
