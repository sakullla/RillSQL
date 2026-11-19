//! 快捷键注册表（ADR-4 / R6）：全局按键绑定、命令面板与菜单里的组合键、快捷键面板的数据都从这一张表生成，
//! 不再分别维护。表里没有绑定函数的项由组件自己处理（表格、对象树）或由输入框内建（撤销、查找），只用于展示。
//!
//! 组合键用平台无关的记法：`Mod+` 在 macOS 是 ⌘、其它平台是 Ctrl；`Control+` 在任何平台都是真正的 Control 键。
//! 多个按法用 ` / ` 分隔。

use gpui_kit::component::input;
use gpui_kit::KeyBinding;

use super::{
    CloseTab, CommandPalette, CommitEdit, Dismiss, ExecuteCurrent, ExecuteQuery, ExecuteScript, ExplainSql, FormatSql, NewQuery,
    NextTab, OpenFile, OpenSettings, PrevTab, QuickOpen, RequestCancel, ShowAutocomplete, ShowHelp, ToggleComment, ToggleResults,
    ToggleSidebar,
};

/// 由组合键的一个按法（gpui 记法，如 `cmd-shift-f`）生成绑定；第二个参数是“是否按 macOS 生成”。
/// 返回 `None` 表示这个平台上不由应用绑定（例如由输入框内建）。
pub type BindFn = fn(&'static str, bool) -> Option<KeyBinding>;

/// 查询编辑器的按键上下文。
pub const EDITOR_CONTEXT: &str = "Input && mode == editor";

/// 快捷键面板的分组（顺序即面板左侧导航顺序），文案键为 `ui.help.<group>`。
pub const GROUPS: [&str; 4] = ["global", "editor", "table", "explorer"];

pub struct Shortcut {
    pub id: &'static str,
    pub group: &'static str,
    pub label_key: &'static str,
    /// macOS 上的组合键。
    pub mac: &'static str,
    /// Windows / Linux 上的组合键。
    pub other: &'static str,
    /// 这条快捷键执行的命令；命令面板与菜单按它显示组合键。
    pub command: Option<&'static str>,
    /// 由本表注册的按键绑定。
    pub bind: &'static [BindFn],
}

impl Shortcut {
    const fn new(id: &'static str, group: &'static str, chord: &'static str, label_key: &'static str) -> Self {
        Self { id, group, label_key, mac: chord, other: chord, command: None, bind: &[] }
    }

    /// 两个平台按法不同。
    const fn split(mut self, mac: &'static str, other: &'static str) -> Self {
        self.mac = mac;
        self.other = other;
        self
    }

    const fn command(mut self, command: &'static str) -> Self {
        self.command = Some(command);
        self
    }

    const fn binds(mut self, bind: &'static [BindFn]) -> Self {
        self.bind = bind;
        self
    }

    /// 指定平台的组合键。
    pub fn chord_for(&self, mac: bool) -> &'static str {
        if mac { self.mac } else { self.other }
    }

    /// 当前平台的组合键（平台无关记法）。
    pub fn chord(&self) -> &'static str {
        self.chord_for(cfg!(target_os = "macos"))
    }
}

/// 不带上下文的全局绑定。
macro_rules! global {
    ($action:expr) => {
        |key: &'static str, _: bool| Some(KeyBinding::new(key, $action, None))
    };
}

pub const SHORTCUTS: &[Shortcut] = &[
    Shortcut::new("command-palette", "global", "Mod+K", "shell.shortcuts.commandPalette").command("command-palette").binds(&[global!(CommandPalette)]),
    Shortcut::new("quick-open", "global", "Mod+P", "shell.shortcuts.quickOpen").command("quick-open").binds(&[global!(QuickOpen)]),
    Shortcut::new("new-query", "global", "Mod+T", "shell.shortcuts.newQuery").command("new-query").binds(&[global!(NewQuery)]),
    Shortcut::new("open-file", "global", "Mod+O", "shell.shortcuts.openFile").command("open-file").binds(&[global!(OpenFile)]),
    Shortcut::new("close-tab", "global", "Mod+W", "shell.shortcuts.closeTab").command("close-tab").binds(&[global!(CloseTab)]),
    Shortcut::new("toggle-sidebar", "global", "Mod+B", "shell.shortcuts.toggleSidebar").command("sidebar").binds(&[global!(ToggleSidebar)]),
    Shortcut::new("next-tab", "global", "Control+Tab", "shell.shortcuts.nextTab").command("next-tab").binds(&[global!(NextTab)]),
    Shortcut::new("prev-tab", "global", "Control+Shift+Tab", "shell.shortcuts.prevTab").command("prev-tab").binds(&[global!(PrevTab)]),
    Shortcut::new("nth-tab", "global", "Alt+1…9", "shell.shortcuts.nthTab"),
    Shortcut::new("open-settings", "global", "Mod+,", "shell.shortcuts.openSettings").command("settings").binds(&[global!(OpenSettings)]),
    // 文本框和编辑器里 ? 是要输入的字符，只在它们以外打开帮助。
    Shortcut::new("shortcut-help", "global", "?", "shell.shortcuts.shortcutHelp")
        .command("help")
        .binds(&[|key, _| Some(KeyBinding::new(key, ShowHelp, Some("Workbench && !Input")))]),
    Shortcut::new("dismiss-overlay", "global", "Esc", "shell.shortcuts.dismissOverlay").binds(&[|key, _| Some(KeyBinding::new(key, Dismiss, Some("Palette")))]),
    Shortcut::new("run-selection", "editor", "Mod+Enter", "shell.shortcuts.runSelection").command("execute-query").binds(&[global!(ExecuteQuery)]),
    Shortcut::new("run-current", "editor", "Mod+Shift+Enter", "shell.shortcuts.runCurrent").command("run-current").binds(&[global!(ExecuteCurrent)]),
    Shortcut::new("run-script", "editor", "Mod+Alt+Enter", "shell.shortcuts.runScript").command("run-script").binds(&[global!(ExecuteScript)]),
    Shortcut::new("request-cancel", "editor", "Mod+.", "shell.shortcuts.requestCancel").command("request-cancel").binds(&[global!(RequestCancel)]),
    Shortcut::new("undo", "editor", "Mod+Z", "shell.shortcuts.undo"),
    // gpui-base 只给 Windows/Linux 绑了 Ctrl+Y 重做；补上常见的 Ctrl+Shift+Z。
    Shortcut::new("redo", "editor", "", "shell.shortcuts.redo")
        .split("Mod+Shift+Z", "Ctrl+Y / Ctrl+Shift+Z")
        .binds(&[|key, mac| (!mac && key == "ctrl-shift-z").then(|| KeyBinding::new(key, input::Redo, Some("Input")))]),
    Shortcut::new("find", "editor", "Mod+F", "shell.shortcuts.find"),
    // gpui-base 在 macOS 编辑器里把 ⌘⇧F 绑成“替换”，这里把替换改到 ⌥⌘F（见 format）。其它平台用内建的 Ctrl+H。
    Shortcut::new("replace", "editor", "", "shell.shortcuts.replace")
        .split("Mod+Alt+F", "Ctrl+H")
        .binds(&[|key, mac| mac.then(|| KeyBinding::new(key, input::Replace, Some(EDITOR_CONTEXT)))]),
    // 补全：macOS 上 ⌃Space 切输入法、⌘Space 是聚焦搜索，都被系统占用，改用 ⌥Space。
    Shortcut::new("autocomplete", "editor", "", "shell.shortcuts.autocomplete")
        .split("Alt+Space", "Control+Space")
        .command("autocomplete")
        .binds(&[global!(ShowAutocomplete)]),
    Shortcut::new("toggle-comment", "editor", "Mod+/", "shell.shortcuts.toggleComment").command("toggle-comment").binds(&[global!(ToggleComment)]),
    Shortcut::new("toggle-results", "editor", "Mod+J", "shell.shortcuts.toggleResults").command("toggle-results").binds(&[global!(ToggleResults)]),
    // 同一上下文里后注册的绑定优先：macOS 编辑器里再绑一次，把 ⌘⇧F 从内建的“替换”还给格式化。
    Shortcut::new("format", "editor", "Mod+Shift+F", "shell.shortcuts.format")
        .command("format")
        .binds(&[global!(FormatSql), |key, mac| mac.then(|| KeyBinding::new(key, FormatSql, Some(EDITOR_CONTEXT)))]),
    Shortcut::new("explain", "editor", "Mod+Shift+E", "shell.shortcuts.explain").command("explain").binds(&[global!(ExplainSql)]),
    Shortcut::new("leave-editor", "editor", "Tab", "shell.shortcuts.leaveEditor"),
    Shortcut::new("move-cells", "table", "↑ ↓ ← →", "shell.shortcuts.moveCells"),
    Shortcut::new("select-all-cells", "table", "Mod+A", "shell.shortcuts.selectAllCells"),
    Shortcut::new("edit-cell", "table", "Enter / F2", "shell.shortcuts.editCell"),
    Shortcut::new("cancel-edit", "table", "Esc", "shell.shortcuts.cancelEdit"),
    Shortcut::new("mark-delete", "table", "Delete", "shell.shortcuts.markDelete"),
    Shortcut::new("commit", "table", "Mod+S", "shell.shortcuts.commit").command("commit").binds(&[global!(CommitEdit)]),
    Shortcut::new("copy-region", "table", "Mod+C", "shell.shortcuts.copyRegion"),
    Shortcut::new("copy-sql-value", "table", "Mod+Shift+C", "shell.shortcuts.copySqlValue"),
    Shortcut::new("paste-cells", "table", "Mod+V", "shell.shortcuts.pasteCells"),
    Shortcut::new("grid-undo", "table", "Mod+Z", "shell.shortcuts.undoStage"),
    // 结果区查找（R10）：网格（含其中的输入框）有焦点时展开查找栏并聚焦，编辑器里的 ⌘F 仍是编辑器自己的查找。
    Shortcut::new("grid-find", "table", "Mod+F", "grid.find.toggle")
        .binds(&[|key, _| Some(KeyBinding::new(key, crate::app::views::grid::GridFind, Some("Grid")))]),
    // 单元格设值（R7/R20）：三者是不同的值，编辑中与选中格上都可用；由网格按本表解析按键（条目 id 即网格命令）。
    Shortcut::new("grid-set-null", "table", "Mod+Shift+N", "grid.value.setNull"),
    Shortcut::new("grid-set-empty", "table", "Mod+Shift+Backspace", "grid.value.setEmpty"),
    Shortcut::new("grid-set-default", "table", "Mod+Shift+D", "grid.value.setDefault"),
    Shortcut::new("grid-next-cell", "table", "Tab / Shift+Tab", "shell.shortcuts.nextCell"),
    Shortcut::new("explorer-move", "explorer", "↑ ↓", "shell.shortcuts.explorerMove"),
    Shortcut::new("explorer-expand", "explorer", "← →", "shell.shortcuts.explorerExpand"),
    Shortcut::new("explorer-open", "explorer", "Enter", "shell.shortcuts.explorerOpen"),
    Shortcut::new("explorer-clear-search", "explorer", "Esc", "shell.shortcuts.explorerClearSearch"),
];

pub fn shortcut(id: &str) -> Option<&'static Shortcut> {
    SHORTCUTS.iter().find(|shortcut| shortcut.id == id)
}

/// 命令的组合键（当前平台）：命令面板、菜单与快速打开都从这里取。
pub fn command_chord(command: &str) -> Option<&'static str> {
    SHORTCUTS.iter().find(|shortcut| shortcut.command == Some(command)).map(Shortcut::chord)
}

/// 平台无关记法的组合键转成 gpui 的按键串（每个按法一项）。认不出的按法（如 `↑ ↓`、`Alt+1…9`）不产出。
pub fn keystrokes(chord: &str, mac: bool) -> Vec<String> {
    chord
        .split(" / ")
        .filter_map(|variant| {
            let variant = variant.trim();
            if variant.is_empty() || variant.contains(' ') || variant.contains('…') {
                return None;
            }
            // 末尾的按键本身可能是 '+'（例如 Mod++），这里没有这种组合，按 '+' 切分即可。
            let parts: Vec<&str> = variant.split('+').collect();
            let (key, modifiers) = parts.split_last()?;
            let mut out = Vec::new();
            for modifier in modifiers {
                out.push(match *modifier {
                    "Mod" => if mac { "cmd" } else { "ctrl" },
                    "Control" | "Ctrl" => "ctrl",
                    "Alt" => "alt",
                    "Shift" => "shift",
                    "Cmd" => "cmd",
                    _ => return None,
                });
            }
            let key = match *key {
                "Esc" => "escape".to_owned(),
                "Enter" => "enter".to_owned(),
                "Space" => "space".to_owned(),
                "Tab" => "tab".to_owned(),
                "Delete" => "delete".to_owned(),
                other => other.to_ascii_lowercase(),
            };
            out.push(&key);
            Some(out.join("-"))
        })
        .collect()
}

/// gpui 的绑定要求按键串活到程序结束：同一个串只留一份，重复注册（测试里多次 bind_keys）不会越积越多。
fn intern(key: String) -> &'static str {
    static KEYS: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeSet<&'static str>>> = std::sync::OnceLock::new();
    let mut keys = KEYS.get_or_init(Default::default).lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(found) = keys.get(key.as_str()) {
        return found;
    }
    let leaked: &'static str = Box::leak(key.into_boxed_str());
    keys.insert(leaked);
    leaked
}

/// 一条快捷键在指定平台上生成的全部绑定。
pub fn bindings_of(shortcut: &Shortcut, mac: bool) -> Vec<KeyBinding> {
    let keys: Vec<&'static str> = keystrokes(shortcut.chord_for(mac), mac).into_iter().map(intern).collect();
    shortcut.bind.iter().flat_map(|bind| keys.iter().filter_map(move |key| bind(key, mac))).collect()
}

/// 指定平台上应用注册的全部按键绑定。
pub fn key_bindings_for(mac: bool) -> Vec<KeyBinding> {
    SHORTCUTS.iter().flat_map(|shortcut| bindings_of(shortcut, mac)).collect()
}

/// 当前平台的全部按键绑定：`bind_keys` 只注册这些。
pub fn key_bindings() -> Vec<KeyBinding> {
    key_bindings_for(cfg!(target_os = "macos"))
}

/// 组合键的显示：macOS 用 ⌃⌥⇧⌘ 符号，其它平台写 Ctrl/Alt/Shift。
pub fn display_chord_for(chord: &str, mac: bool) -> String {
    if mac {
        chord
            .replace("Control+", "⌃")
            .replace("Ctrl+", "⌃")
            .replace("Alt+", "⌥")
            .replace("Shift+", "⇧")
            .replace("Mod+", "⌘")
    } else {
        chord.replace("Control+", "Ctrl+").replace("Mod+", "Ctrl+")
    }
}

/// 当前平台的组合键显示。
pub fn display_chord(chord: &str) -> String {
    display_chord_for(chord, cfg!(target_os = "macos"))
}

/// 面板搜索：动作名（当前语言）或按键都能命中。按键可以输入符号（⌘/）、文字（cmd+/、ctrl /）或原记法（Mod+/）。
pub fn matches(shortcut: &Shortcut, label: &str, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return true;
    }
    label.to_lowercase().contains(&query) || key_matches(shortcut, &query)
}

/// 按键比较时忽略空白与连接用的 '+'：Ctrl /、ctrl+/、Ctrl+/ 是同一个按法。
fn squeeze(text: &str) -> String {
    text.to_lowercase().chars().filter(|ch| !ch.is_whitespace() && *ch != '+').collect()
}

/// 查询词是否命中这条快捷键的按键（任一平台、任一写法）。
fn key_matches(shortcut: &Shortcut, query: &str) -> bool {
    let needle = squeeze(query);
    if needle.is_empty() {
        return false;
    }
    let mut haystacks = Vec::new();
    for mac in [true, false] {
        let chord = shortcut.chord_for(mac);
        haystacks.push(squeeze(chord));
        haystacks.push(squeeze(&display_chord_for(chord, mac)));
        // 文字写法：cmd/command、option、ctrl/control。
        let spelled = if mac {
            chord.replace("Mod+", "Cmd+").replace("Alt+", "Option+").replace("Control+", "Ctrl+")
        } else {
            chord.replace("Mod+", "Ctrl+").replace("Control+", "Ctrl+")
        };
        haystacks.push(squeeze(&spelled));
        haystacks.push(squeeze(&spelled.replace("Cmd+", "Command+").replace("Ctrl+", "Control+")));
    }
    haystacks.iter().any(|hay| hay.contains(&needle))
}

/// 动作名里命中查询词的一段（字节区间，忽略大小写）；用于面板高亮。
pub fn label_match(label: &str, query: &str) -> Option<(usize, usize)> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return None;
    }
    let bounds: Vec<usize> = label.char_indices().map(|(i, _)| i).chain(std::iter::once(label.len())).collect();
    for (n, &start) in bounds.iter().enumerate() {
        for &end in &bounds[n + 1..] {
            let lowered = label[start..end].to_lowercase();
            if lowered == query {
                return Some((start, end));
            }
            if !query.starts_with(&lowered) {
                break;
            }
        }
    }
    None
}

/// 面板上显示的组合键（指定平台，见 [`display_chord_for`]）里命中查询词的一段（字节区间）。
/// 只有按键确实命中时才返回；查询词用的是另一种写法（如 macOS 上输入 cmd+/）而对不上显示文字时，整段都算命中。
pub fn chord_match(shortcut: &Shortcut, query: &str, mac: bool) -> Option<(usize, usize)> {
    let query = query.trim().to_lowercase();
    if query.is_empty() || !key_matches(shortcut, &query) {
        return None;
    }
    let shown = display_chord_for(shortcut.chord_for(mac), mac);
    if shown.is_empty() {
        return None;
    }
    // 显示文字里参与比较的字符及其字节位置（同样忽略空白与 '+'）。
    let chars: Vec<(usize, String)> = shown
        .char_indices()
        .filter(|(_, ch)| !ch.is_whitespace() && *ch != '+')
        .map(|(i, ch)| (i, ch.to_lowercase().collect::<String>()))
        .collect();
    let flat: String = chars.iter().map(|(_, ch)| ch.as_str()).collect();
    // 把查询词换成显示用的写法再找：macOS 显示符号，其它平台显示 Ctrl/Alt/Shift。
    let words: &[(&str, &[&str])] = if mac {
        &[("command", &["⌘"]), ("cmd", &["⌘"]), ("mod", &["⌘"]), ("option", &["⌥"]), ("alt", &["⌥"]), ("shift", &["⇧"]), ("control", &["⌘", "⌃"]), ("ctrl", &["⌘", "⌃"])]
    } else {
        &[("command", &["ctrl"]), ("cmd", &["ctrl"]), ("mod", &["ctrl"]), ("control", &["ctrl"]), ("option", &["alt"]), ("⌘", &["ctrl"]), ("⌃", &["ctrl"]), ("⌥", &["alt"]), ("⇧", &["shift"])]
    };
    let mut needles = vec![squeeze(&query)];
    for (word, replacements) in words {
        let mut next = Vec::new();
        for needle in &needles {
            if needle.contains(word) {
                for replacement in *replacements {
                    next.push(needle.replace(word, replacement));
                }
            } else {
                next.push(needle.clone());
            }
        }
        needles = next;
    }
    for needle in needles.iter().filter(|needle| !needle.is_empty()) {
        let Some(at) = flat.find(needle.as_str()) else { continue };
        // flat 的字节位置换回显示文字的字符：逐个累加小写后的长度。
        let mut offset = 0;
        let mut first = None;
        let mut last = None;
        for (index, (_, ch)) in chars.iter().enumerate() {
            let end = offset + ch.len();
            if first.is_none() && end > at {
                first = Some(index);
            }
            if end >= at + needle.len() {
                last = Some(index);
                break;
            }
            offset = end;
        }
        if let (Some(first), Some(last)) = (first, last) {
            let start = chars[first].0;
            let end = chars[last].0 + shown[chars[last].0..].chars().next().map_or(0, char::len_utf8);
            return Some((start, end));
        }
    }
    Some((0, shown.len()))
}

/// 打开快捷键面板时按焦点所在区域预选的分组。
pub fn group_for_focus(focus: super::FocusTarget) -> &'static str {
    match focus {
        super::FocusTarget::Editor => "editor",
        super::FocusTarget::Grid => "table",
        super::FocusTarget::Sidebar => "explorer",
        _ => "global",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_registered_binding_parses_and_belongs_to_a_listed_shortcut() {
        for mac in [true, false] {
            let mut seen = std::collections::BTreeSet::new();
            for shortcut in SHORTCUTS {
                assert!(GROUPS.contains(&shortcut.group), "{} 的分组不在面板导航里", shortcut.id);
                for key in keystrokes(shortcut.chord_for(mac), mac) {
                    let key = intern(key);
                    assert!(gpui_kit::Keystroke::parse(key).is_ok(), "{} 的按键 {key} 解析失败", shortcut.id);
                }
                let bindings = bindings_of(shortcut, mac);
                if !shortcut.bind.is_empty() && !matches!(shortcut.id, "redo" | "replace") {
                    assert!(!bindings.is_empty(), "{} 声明了绑定却没生成（mac={mac}）", shortcut.id);
                }
                for binding in &bindings {
                    // 同一按键与上下文不重复绑定到不同动作（格式化在编辑器里重绑同一动作除外）。
                    let key = format!("{:?}", binding.keystrokes());
                    let context = binding.predicate().map(|p| format!("{p:?}")).unwrap_or_default();
                    assert!(seen.insert((key.clone(), context.clone())), "{key} 在 {context:?} 里重复绑定（{}）", shortcut.id);
                }
            }
            // 生成的全部绑定 = 各项生成的绑定之和：bind_keys 不另外手写绑定。
            assert_eq!(key_bindings_for(mac).len(), SHORTCUTS.iter().map(|s| bindings_of(s, mac).len()).sum::<usize>());
        }
    }

    #[test]
    fn keystrokes_follow_the_platform_modifier() {
        assert_eq!(keystrokes("Mod+Shift+Enter", true), ["cmd-shift-enter"]);
        assert_eq!(keystrokes("Mod+Shift+Enter", false), ["ctrl-shift-enter"]);
        assert_eq!(keystrokes("Control+Tab", true), ["ctrl-tab"]);
        assert_eq!(keystrokes("Ctrl+Y / Ctrl+Shift+Z", false), ["ctrl-y", "ctrl-shift-z"]);
        assert_eq!(keystrokes("Esc", true), ["escape"]);
        assert_eq!(keystrokes("?", true), ["?"]);
        assert!(keystrokes("↑ ↓ ← →", true).is_empty());
        assert!(keystrokes("Alt+1…9", false).is_empty());
    }

    #[test]
    fn mac_shows_symbols_and_other_platforms_spell_modifiers() {
        assert_eq!(display_chord_for("Mod+Alt+Shift+F", true), "⌘⌥⇧F");
        assert_eq!(display_chord_for("Mod+Shift+Enter", true), "⌘⇧Enter");
        assert_eq!(display_chord_for("Control+Tab", true), "⌃Tab");
        assert_eq!(display_chord_for("Mod+/", true), "⌘/");
        assert_eq!(display_chord_for("Mod+Shift+Enter", false), "Ctrl+Shift+Enter");
        assert_eq!(display_chord_for("Mod+Alt+Enter", false), "Ctrl+Alt+Enter");
        assert_eq!(display_chord_for("Control+Shift+Tab", false), "Ctrl+Shift+Tab");
        for shortcut in SHORTCUTS {
            let mac = display_chord_for(shortcut.chord_for(true), true);
            assert!(!mac.contains("Mod") && !mac.contains("Ctrl+") && !mac.contains("Alt+") && !mac.contains("Shift+"), "{}: {mac}", shortcut.id);
            let other = display_chord_for(shortcut.chord_for(false), false);
            assert!(!other.contains("Mod") && !other.contains('⌘') && !other.contains('⌥') && !other.contains('⇧'), "{}: {other}", shortcut.id);
        }
    }

    #[test]
    fn search_matches_action_names_and_keys_in_any_spelling() {
        let comment = shortcut("toggle-comment").unwrap();
        assert!(matches(comment, "注释 / 取消注释", "注释"));
        assert!(matches(comment, "Comment / uncomment", "COMMENT"));
        for query in ["⌘/", "Mod+/", "ctrl+/", "Ctrl /", "cmd+/", "command+/"] {
            assert!(matches(comment, "", query), "{query}");
        }
        let format = shortcut("format").unwrap();
        assert!(matches(format, "", "⌘⇧F"));
        assert!(matches(format, "", "ctrl+shift+f"));
        assert!(!matches(format, "格式化", "注释"));
        assert!(matches(format, "格式化", "  "), "空查询列出全部");
    }

    #[test]
    fn search_hits_are_located_for_highlighting() {
        let label = "注释 / 取消注释";
        assert_eq!(label_match(label, "注释"), Some((0, "注释".len())));
        assert_eq!(label_match("Comment / uncomment", "UNCOMMENT"), Some((10, 19)));
        assert_eq!(label_match(label, "  "), None);
        assert_eq!(label_match("格式化", "注释"), None);

        let comment = shortcut("toggle-comment").unwrap();
        assert_eq!(chord_match(comment, "注释", true), None, "按动作名命中时按键不高亮");
        // macOS 显示 ⌘/：符号、文字写法都定位到同一段。
        for query in ["⌘/", "cmd+/", "Mod+/", "ctrl /", "command+/"] {
            assert_eq!(chord_match(comment, query, true), Some((0, "⌘/".len())), "{query}");
        }
        assert_eq!(chord_match(comment, "/", true), Some(("⌘".len(), "⌘/".len())));
        // 其它平台显示 Ctrl+/。
        assert_eq!(chord_match(comment, "ctrl+/", false), Some((0, 6)));
        assert_eq!(chord_match(comment, "⌘/", false), Some((0, 6)));
        let format = shortcut("format").unwrap();
        assert_eq!(chord_match(format, "shift+f", false), Some((5, 12)), "Ctrl+Shift+F 里的 Shift+F");
        assert_eq!(chord_match(format, "⇧F", true), Some(("⌘".len(), "⌘⇧F".len())));
    }

    #[test]
    fn command_chords_come_from_the_registry() {
        assert_eq!(command_chord("open-file"), Some("Mod+O"));
        assert_eq!(command_chord("execute-query"), Some("Mod+Enter"));
        assert_eq!(command_chord("sidebar"), Some("Mod+B"));
        assert_eq!(command_chord("settings"), Some("Mod+,"));
        assert_eq!(command_chord("help"), Some("?"));
        assert_eq!(command_chord("history"), None);
    }
}
