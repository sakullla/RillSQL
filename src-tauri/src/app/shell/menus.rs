//! 六类右键菜单。条目 id 都进同一个 `WorkbenchModel::dispatch`。

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuPlace {
    ObjectTree,
    Grid,
    TabBar,
    Editor,
    ConnectionRow,
    HistoryEntry,
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
    pub id: &'static str,
    pub label_key: &'static str,
    pub danger: bool,
    pub enabled: bool,
    pub reason_key: Option<&'static str>,
}

fn item(id: &'static str, label_key: &'static str) -> MenuItem {
    MenuItem { id, label_key, danger: false, enabled: true, reason_key: None }
}

fn danger(id: &'static str, label_key: &'static str) -> MenuItem {
    MenuItem { id, label_key, danger: true, enabled: true, reason_key: None }
}

fn disabled(id: &'static str, label_key: &'static str, reason_key: &'static str) -> MenuItem {
    MenuItem { id, label_key, danger: false, enabled: false, reason_key: Some(reason_key) }
}

pub fn menu_items(place: MenuPlace, target: MenuTarget) -> Vec<MenuItem> {
    match place {
        MenuPlace::ObjectTree => {
            if target == MenuTarget::Column {
                return vec![
                    item("tree-copy-column", "catalog.tree.copyColumn"),
                    item("tree-insert-column", "catalog.tree.insertColumn"),
                ];
            }
            // 范围行（库/模式/种类分组）只绑定自己：开查询、复制名字，别的都没有。
            if target == MenuTarget::Scope {
                return vec![
                    item("tree-new-query", "catalog.tree.newQueryHere"),
                    item("tree-copy-name", "catalog.tree.copyFullName"),
                ];
            }
            if target == MenuTarget::DocumentObject {
                // 集合按文档处理：数据入口开集合浏览器，结构与关系图不适用。
                return vec![
                    item("tree-open-data", "catalog.tree.openData"),
                    disabled("open-structure", "catalog.tree.viewStructure", "catalog.tree.noStructure"),
                    disabled("open-diagram", "catalog.tree.diagram", "diagram.unsupported"),
                    item("tree-new-query", "catalog.tree.newQueryHere"),
                    item("tree-copy-name", "catalog.tree.copyFullName"),
                    disabled("compare-object", "shell.command.compare", "catalog.tree.relationalOnly"),
                    disabled("exchange-object", "shell.command.openExchange", "catalog.tree.relationalOnly"),
                ];
            }
            let data = if target == MenuTarget::NonDataObject {
                disabled("tree-open-data", "catalog.tree.openData", "catalog.tree.noData")
            } else {
                item("tree-open-data", "catalog.tree.openData")
            };
            // 触发器/例程/序列不是表：结构编辑器会误报“找不到表或视图”，
            // 关系图也不适用。入口保留但禁用并说明，避免把人带到错误页面。
            let structure = if target == MenuTarget::NonDataObject {
                disabled("open-structure", "catalog.tree.viewStructure", "catalog.tree.noStructure")
            } else {
                item("open-structure", "catalog.tree.viewStructure")
            };
            let diagram = if target == MenuTarget::NonDataObject {
                disabled("open-diagram", "catalog.tree.diagram", "catalog.tree.noStructure")
            } else {
                item("open-diagram", "catalog.tree.diagram")
            };
            let mut items = vec![
                data,
                structure,
                diagram,
                // 对象上下文里的“新建查询”绑定这一行，不是通用空查询。
                item("tree-new-query", "catalog.tree.newQueryHere"),
                // 插入只改当前草稿，不执行；复制与生成 SELECT 仍是独立入口。
                item("tree-insert-object", "catalog.tree.insertObject"),
                item("tree-insert-select", "catalog.tree.insertSelect"),
                item("tree-copy-name", "catalog.tree.copyFullName"),
                item("tree-select-sql", "catalog.tree.copyToQuery"),
                // 对比与导入导出不再占固定入口，但仍从对象上下文到达，并带上这个对象。
                item("compare-object", "shell.command.compare"),
                item("exchange-object", "shell.command.openExchange"),
            ];
            if target == MenuTarget::Connection {
                items.extend([
                    item("conn-connect", "connections.page.connect"),
                    item("conn-disconnect", "connections.page.menuDisconnect"),
                    item("conn-edit", "connections.page.menuEdit"),
                    item("conn-test", "connections.editor.test.button"),
                    item("conn-duplicate", "common.copy"),
                    danger("conn-delete", "connections.page.menuDelete"),
                ]);
            }
            items
        }
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
            item("new-query", "shell.command.newQuery"),
            danger("conn-delete", "connections.page.menuDelete"),
        ],
        MenuPlace::HistoryEntry => vec![
            item("history-load", "history.page.loadToEditor"),
            item("history-copy", "query.editor.menu.copy"),
            item("history-detail", "history.page.sqlPreview"),
            item("history-filter-connection", "history.page.filterConnection"),
        ],
    }
}

#[derive(Clone, Debug)]
pub struct OpenMenu {
    pub place: MenuPlace,
    pub items: Vec<MenuItem>,
    pub index: usize,
}

impl OpenMenu {
    pub fn new(place: MenuPlace, target: MenuTarget) -> Self {
        let items = menu_items(place, target);
        let index = items.iter().position(|item| item.enabled).unwrap_or(0);
        Self { place, items, index }
    }

    pub fn move_by(&mut self, step: isize) {
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
        if let Some(index) = self.items.iter().position(|item| item.enabled) {
            self.index = index;
        }
    }

    pub fn end(&mut self) {
        if let Some(index) = self.items.iter().rposition(|item| item.enabled) {
            self.index = index;
        }
    }

    pub fn current(&self) -> Option<&MenuItem> {
        self.items.get(self.index).filter(|item| item.enabled)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavKey {
    Escape,
    ArrowDown,
    ArrowUp,
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
        assert!(routine.items.iter().any(|item| item.id == "tree-insert-object"));

        // 列行只提供列自己的动作。
        let column = OpenMenu::new(MenuPlace::ObjectTree, MenuTarget::Column);
        let ids: Vec<&str> = column.items.iter().map(|item| item.id).collect();
        assert_eq!(ids, vec!["tree-copy-column", "tree-insert-column"]);
    }
}
