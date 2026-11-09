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
            let mut items = vec![
                item("tree-open-data", "catalog.tree.openData"),
                item("open-structure", "catalog.tree.viewStructure"),
                item("open-diagram", "catalog.tree.diagram"),
                item("new-query", "catalog.tree.newQueryHere"),
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
                item("compare-object", "shell.command.compare"),
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
    }
}
