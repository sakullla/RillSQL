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

/// 已有命令或快捷键表里的按键。没有登记的动作不编一个新的。
fn chord_for(id: &'static str) -> Option<&'static str> {
    if let Some(chord) = crate::app::COMMANDS.iter().find(|command| command.id == id).and_then(|command| command.chord) {
        return Some(chord);
    }
    crate::app::model::SHORTCUTS.iter().find(|shortcut| shortcut.id == id).map(|shortcut| shortcut.chord)
}

/// 快速打开的行内快捷键：与菜单同源（COMMANDS → SHORTCUTS），不另编一份。
pub(super) fn command_chord(id: &str) -> Option<&'static str> {
    match crate::app::COMMANDS.iter().find(|command| command.id == id) {
        Some(command) => command.chord,
        None => crate::app::model::SHORTCUTS.iter().find(|shortcut| shortcut.id == id).map(|shortcut| shortcut.chord),
    }
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

pub fn menu_items(place: MenuPlace, target: MenuTarget) -> Vec<MenuItem> {
    match place {
        // 分组菜单的条目由模型按分组现算（名字与命令都带目标），这里没有通用项。
        MenuPlace::ExplorerGroup => Vec::new(),
        MenuPlace::ObjectTree => object_tree_items(target, None),
        MenuPlace::Grid => {
            let write = target == MenuTarget::WritableGrid;
            vec![
                item("copy-region", "grid.value.copyValue"),
                item("grid-copy-row", "grid.menu.copyRow"),
                item("grid-copy-column", "grid.menu.copyColumn"),
                item("grid-filter-value", "grid.menu.filterByValue"),
                item("tree-select-sql", "catalog.tree.copyToQuery"),
                item("open-exchange", "shell.command.openExchange"),
                // 网格里的对比以正在显示的表格为目标，不复用对象树选择（compare-object）。
                item("compare-grid", "shell.command.compare"),
                if write {
                    item("edit-cell", "shell.shortcuts.editCell")
                } else {
                    disabled("edit-cell", "shell.shortcuts.editCell", "grid.errors.viewReadonly")
                },
                if write {
                    danger("mark-delete", "grid.menu.markDelete")
                } else {
                    disabled("mark-delete", "grid.menu.markDelete", "grid.errors.viewReadonly")
                },
            ]
        }
        MenuPlace::TabBar => vec![
            item("close-tab", "common.close"),
            item("tab-close-others", "shell.tabs.closeOthers"),
            item("tab-close-right", "shell.tabs.closeRight"),
            item("tab-close-all", "shell.tabs.closeAll"),
            item("history-copy", "query.editor.menu.copy"),
        ],
        MenuPlace::Editor => vec![
            item("execute-query", "query.editor.menu.run"),
            item("run-current", "query.editor.menu.runCurrent"),
            item("format", "query.editor.menu.format"),
            item("explain", "query.editor.menu.plan"),
            item("history-copy", "query.editor.menu.copy"),
            item("open-exchange", "shell.command.openExchange"),
            item("command-palette", "shell.shortcuts.commandPalette"),
            item("editor-clear", "query.editor.menu.clear"),
        ],
        MenuPlace::ConnectionRow => vec![
            item("conn-connect", "connections.page.connect"),
            item("conn-edit", "connections.page.menuEdit"),
            item("conn-test", "connections.editor.test.button"),
            item("conn-duplicate", "common.copy"),
            // 新建查询到此连接：先按需连接再打开编辑器，和对象树里的同名入口走同一条续接。
            item("conn-new-query", "shell.command.newQuery"),
            danger("conn-delete", "connections.page.menuDelete"),
        ],
        MenuPlace::HistoryEntry => {
            if target == MenuTarget::Saved {
                vec![
                    item("history-load", "history.page.loadToEditor"),
                    item("history-copy", "history.preview.copy"),
                    item("history-rename", "history.page.rename"),
                    danger("saved-delete", "ui.history.delete"),
                ]
            } else {
                vec![
                    item("history-load", "history.page.loadToEditor"),
                    item("history-copy", "history.preview.copy"),
                    item("history-detail", "history.page.sqlPreview"),
                    item("history-filter-connection", "history.page.filterConnection"),
                    item("history-save-preview", "history.preview.saveQuery"),
                ]
            }
        }
    }
}

fn object_tree_items(target: MenuTarget, facts: Option<ObjectFacts>) -> Vec<MenuItem> {
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
            items.push(with_icon(item("open-admin", "ui.tool.admin"), IconName::Database));
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
                    disabled("compare-object", "shell.command.compare", "catalog.tree.relationalOnly"),
                    disabled("exchange-object", "shell.command.openExchange", "catalog.tree.relationalOnly"),
                ],
            ),
        ];
    }
    if target == MenuTarget::Connection {
        return vec![
            item("conn-connect", "connections.page.connect"),
            item("conn-disconnect", "connections.page.menuDisconnect"),
            item("conn-edit", "connections.page.menuEdit"),
            item("conn-test", "connections.editor.test.button"),
            item("conn-duplicate", "common.copy"),
            with_icon(item("open-admin", "ui.tool.admin"), IconName::Database),
            danger("conn-delete", "connections.page.menuDelete"),
        ];
    }
    let data = if target == MenuTarget::NonDataObject {
        disabled("tree-open-data", "catalog.tree.openData", "catalog.tree.noData")
    } else {
        with_icon(item("tree-open-data", "catalog.tree.openData"), IconName::Table)
    };
    let structure = if target == MenuTarget::NonDataObject {
        disabled("open-structure", "catalog.tree.viewStructure", "catalog.tree.noStructure")
    } else {
        with_icon(item("open-structure", "catalog.tree.viewStructure"), IconName::Table)
    };
    // 高频项最多七个：打开数据、打开结构、新建查询、查看/复制 DDL、刷新。
    let mut items = vec![
        data,
        structure,
        with_icon(item("tree-new-query", "catalog.tree.newQueryHere"), IconName::SquareTerminal),
    ];
    let relational = facts.is_some_and(|facts| facts.engine.is_relational());
    let table = facts.is_some_and(|facts| facts.kind == ObjectKind::Table);
    let view = facts.is_some_and(|facts| facts.kind == ObjectKind::View);
    if let Some(facts) = facts.filter(|facts| facts.engine.is_relational() && matches!(target, MenuTarget::Object | MenuTarget::NonDataObject)) {
        if ddl_available(facts.engine, facts.kind) {
            items.push(with_icon(item("tree-view-ddl", "catalog.tree.viewDdl"), IconName::Eye));
            items.push(with_icon(item("tree-copy-ddl", "catalog.tree.copyDdl"), IconName::Copy));
        } else {
            items.push(disabled("tree-view-ddl", "catalog.tree.viewDdl", "catalog.tree.noDefinition"));
            items.push(disabled("tree-copy-ddl", "catalog.tree.copyDdl", "catalog.tree.noDefinition"));
        }
    }
    items.push(with_icon(item("tree-refresh-node", "catalog.tree.refreshNode"), IconName::RefreshCw));
    let diagram = if target == MenuTarget::NonDataObject || !relational {
        disabled("open-diagram", "catalog.tree.diagram", "catalog.tree.noStructure")
    } else {
        with_icon(item("open-diagram", "catalog.tree.diagram"), IconName::ChartNetwork)
    };
    let compare = if relational {
        item("compare-object", "shell.command.compare")
    } else {
        disabled("compare-object", "shell.command.compare", "catalog.tree.relationalOnly")
    };
    let exchange = if relational {
        item("exchange-object", "shell.command.openExchange")
    } else {
        disabled("exchange-object", "shell.command.openExchange", "catalog.tree.relationalOnly")
    };
    let mut tools = vec![diagram, compare, exchange];
    if relational {
        tools.push(with_icon(item("open-admin", "ui.tool.admin"), IconName::Database));
    }
    if relational && target == MenuTarget::Object && (table || view) {
        let mut templates = vec![
            item("tree-template-insert", "catalog.tree.templateInsert"),
            item("tree-template-update", "catalog.tree.templateUpdate"),
            item("tree-template-delete", "catalog.tree.templateDelete"),
            item("tree-select-sql", "catalog.tree.templateSelect"),
        ];
        if let Some(first) = templates.first_mut() {
            first.separator_before = false;
        }
        let mut sql = branch("submenu-sql", "catalog.menu.sqlTemplates", IconName::Braces, templates);
        sql.separator_before = true;
        items.push(sql);
        items.push(branch(
            "submenu-insert",
            "catalog.menu.insertEditor",
            IconName::SquarePen,
            vec![
                item("tree-insert-object", "catalog.menu.insertQualified"),
                item("tree-insert-select", "catalog.tree.insertSelect"),
                item("tree-copy-name", "catalog.tree.copyFullName"),
            ],
        ));
    } else if matches!(target, MenuTarget::Object | MenuTarget::NonDataObject) {
        items.push(branch(
            "submenu-insert",
            "catalog.menu.insertEditor",
            IconName::SquarePen,
            vec![
                item("tree-insert-object", "catalog.tree.insertObject"),
                item("tree-insert-select", "catalog.tree.insertSelect"),
                item("tree-copy-name", "catalog.tree.copyFullName"),
            ],
        ));
    }
    items.push(branch("submenu-tools", "catalog.menu.tools", IconName::FolderOpen, tools));
    if relational && target == MenuTarget::Object && (table || view) {
        let mut danger_items = Vec::new();
        if table {
            danger_items.extend([
                danger("tree-rename-table", "catalog.tree.renameTable"),
                danger("tree-truncate-table", "catalog.tree.truncateTable"),
                danger("tree-drop-object", "catalog.tree.dropTable"),
            ]);
        } else {
            danger_items.push(danger("tree-drop-object", "catalog.tree.dropView"));
        }
        let mut danger_menu = branch("submenu-danger", "catalog.menu.danger", IconName::TriangleAlert, danger_items);
        danger_menu.danger = true;
        danger_menu.separator_before = true;
        items.push(danger_menu);
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
pub fn tree_menu_items(target: MenuTarget, facts: Option<ObjectFacts>) -> Vec<MenuItem> {
    object_tree_items(target, facts)
}


/// 带动态命令、静态文案的条目：折叠/展开、重命名这类目标写在命令里的动作。
fn targeted(id: &'static str, label_key: &'static str, action: String) -> MenuItem {
    MenuItem { action: Some(action), ..leaf(id, label_key, false, true, None) }
}

/// 资源管理器分组标题的菜单：新建、折叠/展开、重命名、删除（连接保留）。
pub fn group_menu_items(group_id: &str, collapsed: bool) -> Vec<MenuItem> {
    let toggle_key = if collapsed { "ui.group.expand" } else { "ui.group.collapse" };
    vec![
        with_icon(item("group-new", "ui.group.new"), IconName::Plus),
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
        let open = self.submenu.take().is_some();
        open
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
            // 表：删除/清空/重命名都在，并且都是危险项。
            let table = tree_menu_items(MenuTarget::Object, facts(engine, ObjectKind::Table));
            for id in DANGER {
                let item = flatten_items(&table).into_iter().find(|item| item.id == id).unwrap_or_else(|| panic!("{engine:?} {id}"));
                assert!(item.danger && item.enabled, "{engine:?} {id}");
            }
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
        assert!(table.iter().any(|item| item.id == "tree-view-ddl" && item.enabled));
        assert!(table.iter().any(|item| item.id == "tree-copy-ddl" && item.enabled));
        // 扩展读不到定义：两项禁用并说明原因。
        let extension = tree_menu_items(MenuTarget::NonDataObject, facts(Engine::Postgres, ObjectKind::Extension));
        for id in ["tree-view-ddl", "tree-copy-ddl"] {
            let item = extension.iter().find(|item| item.id == id).expect(id);
            assert!(!item.enabled);
            assert_eq!(item.reason_key, Some("catalog.tree.noDefinition"));
        }
        // 例程有定义：DDL 可用，但仍没有危险项。
        let routine = tree_menu_items(MenuTarget::NonDataObject, facts(Engine::Postgres, ObjectKind::Function));
        assert!(routine.iter().any(|item| item.id == "tree-view-ddl" && item.enabled));
        // SQLite 序列之类没有定义来源。
        let sqlite = tree_menu_items(MenuTarget::NonDataObject, facts(Engine::Sqlite, ObjectKind::Sequence));
        assert!(sqlite.iter().any(|item| item.id == "tree-view-ddl" && !item.enabled));
        // MongoDB 集合没有 DDL 项。
        let mongo = tree_menu_items(MenuTarget::DocumentObject, facts(Engine::Mongodb, ObjectKind::Table));
        assert!(!ids(&mongo).contains(&"tree-view-ddl"));
        // 范围行：自己的动作，加上关系型的新建表和管理页。
        assert_eq!(
            top_ids(&tree_menu_items(MenuTarget::Scope, None)),
            vec!["tree-new-query", "tree-copy-name", "tree-refresh-node", "tree-create-table", "open-admin"]
        );
        let redis_scope = tree_menu_items(MenuTarget::Scope, facts(Engine::Redis, ObjectKind::Table));
        assert!(!ids(&redis_scope).contains(&"open-admin"));
        // 表的一级高频项不超过 7，模板和危险操作在子菜单里。
        let top = top_ids(&table);
        let leaves: Vec<_> = table.iter().filter(|item| item.children.is_empty()).map(|item| item.id).collect();
        assert!(leaves.len() <= 7, "{leaves:?}");
        assert!(leaves.contains(&"tree-open-data"));
        assert!(leaves.contains(&"open-structure"));
        assert!(leaves.contains(&"tree-new-query"));
        assert!(leaves.contains(&"tree-view-ddl"));
        assert!(leaves.contains(&"tree-copy-ddl"));
        assert!(leaves.contains(&"tree-refresh-node"));
        assert!(top.contains(&"submenu-sql") && top.contains(&"submenu-danger"));
        assert!(ids(&table).contains(&"tree-select-sql"));
        assert!(ids(&table).contains(&"open-admin"));
        assert!(ids(&table).contains(&"open-diagram"));
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
        assert_eq!(menu.current().map(|item| item.id), Some("tree-template-insert"));
        menu.move_by(1);
        assert_eq!(menu.current().map(|item| item.id), Some("tree-template-update"));
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
}
