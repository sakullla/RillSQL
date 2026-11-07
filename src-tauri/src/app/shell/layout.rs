//! 1360 / 900 / 640 的外壳几何。侧栏在窄窗口改为覆盖，避免把内容挤出视口。

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarMode {
    Docked,
    Hidden,
    Overlay,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChromeLayout {
    pub width: i32,
    pub activity_width: i32,
    pub sidebar_width: i32,
    pub sidebar_mode: SidebarMode,
    pub content_width: i32,
    pub menu_height: i32,
    pub toolbar_height: i32,
    pub tab_height: i32,
    pub status_height: i32,
    pub overflow: i32,
}

pub fn layout_for(width: i32, sidebar_open: bool) -> ChromeLayout {
    let activity_width = if width <= 640 { 40 } else { 44 };
    let (sidebar_width, sidebar_mode) = if width <= 640 {
        if sidebar_open { (234, SidebarMode::Overlay) } else { (0, SidebarMode::Hidden) }
    } else if sidebar_open {
        (234, SidebarMode::Docked)
    } else {
        (0, SidebarMode::Hidden)
    };
    let docked = if sidebar_mode == SidebarMode::Docked { sidebar_width } else { 0 };
    let content_width = width - activity_width - docked;
    let mut overflow = 0;
    if content_width < 160 {
        overflow += 160 - content_width;
    }
    if sidebar_mode == SidebarMode::Overlay && sidebar_width > width {
        overflow += sidebar_width - width;
    }
    let menu_labels = 7 * 36 + 80;
    if menu_labels > width {
        overflow += menu_labels - width;
    }
    let toolbar = 8 * 32 + 80;
    if toolbar > width {
        overflow += toolbar - width;
    }
    ChromeLayout {
        width,
        activity_width,
        sidebar_width: if sidebar_mode == SidebarMode::Hidden { 0 } else { sidebar_width },
        sidebar_mode,
        content_width: content_width.max(0),
        menu_height: 26,
        toolbar_height: 36,
        tab_height: 30,
        status_height: 24,
        overflow,
    }
}

/// 中文按字折行，英文按空白折行。返回无法折开的最长片段宽度。
pub fn unbreakable_px(text: &str) -> i32 {
    let mut best = 0;
    let mut current = 0;
    for ch in text.chars() {
        if ch.is_whitespace() || !ch.is_ascii() {
            best = best.max(current);
            current = 0;
            if !ch.is_whitespace() && !ch.is_ascii() {
                best = best.max(14);
            }
        } else {
            current += 7;
        }
    }
    best.max(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_widths_do_not_overflow() {
        for width in [1360, 900, 640] {
            for open in [true, false] {
                let layout = layout_for(width, open);
                assert_eq!(layout.overflow, 0, "{width} open={open}");
                assert!(layout.content_width >= 160, "{width}");
                let right = layout.activity_width
                    + if layout.sidebar_mode == SidebarMode::Docked { layout.sidebar_width } else { 0 }
                    + layout.content_width;
                assert!(right <= width, "{width} right={right}");
            }
        }
        assert_eq!(layout_for(640, true).sidebar_mode, SidebarMode::Overlay);
        assert_eq!(layout_for(900, false).sidebar_mode, SidebarMode::Hidden);
        assert_eq!(layout_for(1360, true).sidebar_mode, SidebarMode::Docked);
    }
}
