//! 设置映射到版本化的 ShellSettings。密度保留 compact / standard / relaxed 三档。

use crate::app::{Density, Locale, Shell, ThemePreference};
use crate::state::ShellSettings;

pub const EDITOR_THEMES: &[&str] = &["auto", "contrast", "warm", "cool", "mono"];
pub const NULL_COPIES: &[&str] = &["NULL", "empty", "N"];
pub const TIMEOUTS: &[i64] = &[0, 15, 60];
pub const FONT_RANGE: (i64, i64) = (10, 24);
pub const PAGE_SIZE_RANGE: (i64, i64) = (1, 10_000);
pub const PREVIEW_RANGE: (i64, i64) = (1, 64);
pub const OBJECT_PAGE_RANGE: (i64, i64) = (1, 10_000);

/// 会改动本机设置、因而需要写盘反馈的命令：主题、密度、语言、编辑器字号与配色、
/// 读取上限、NULL 文本、查询超时与更新开关。分区导航与设置页本身不在此列。
pub fn is_settings_command(id: &str) -> bool {
    matches!(id, "font-up" | "font-down" | "editor-theme-next" | "editor-theme-prev" | "toggle-updates")
        || id.starts_with("theme-")
        || id.starts_with("density-")
        || id.starts_with("locale-")
        || id.starts_with("page-size-")
        || id.starts_with("preview-")
        || id.starts_with("object-page-")
        || id.starts_with("null-")
        || id.starts_with("timeout-")
}

/// 设置页的左侧分区导航：设计 S8.3 的三块，右侧只显示当前分区的表单。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsSection {
    /// 外观与编辑器：主题、密度、语言、编辑器字号与配色。
    Appearance,
    /// 数据读取：每页行数、预览上限、对象分页、NULL 文本与查询超时。
    Data,
    /// 本地数据：更新检查与本机状态说明。
    Local,
}

impl SettingsSection {
    pub const ALL: [SettingsSection; 3] = [SettingsSection::Appearance, SettingsSection::Data, SettingsSection::Local];

    pub fn label_key(self) -> &'static str {
        match self {
            SettingsSection::Appearance => "ui.settings.navAppearance",
            SettingsSection::Data => "ui.settings.navData",
            SettingsSection::Local => "ui.settings.navLocal",
        }
    }

    /// 分区按钮的命令 id：导航、键盘与命令面板共用同一个入口。
    pub fn command(self) -> &'static str {
        match self {
            SettingsSection::Appearance => "settings-section-appearance",
            SettingsSection::Data => "settings-section-data",
            SettingsSection::Local => "settings-section-local",
        }
    }

    pub fn parse(id: &str) -> Option<Self> {
        match id {
            "settings-section-appearance" => Some(SettingsSection::Appearance),
            "settings-section-data" => Some(SettingsSection::Data),
            "settings-section-local" => Some(SettingsSection::Local),
            _ => None,
        }
    }
}

/// 设置写盘结果。改动立即生效，但只有写成功才在重启后仍然成立，
/// 因此失败必须让用户看得见并能就地重试，而不是静默丢掉错误。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum SaveState {
    /// 本次会话还没改过设置。
    #[default]
    Idle,
    Saving,
    Saved,
    Failed {
        detail: String,
    },
}

impl SaveState {
    pub fn label_key(&self) -> Option<&'static str> {
        match self {
            SaveState::Idle => None,
            SaveState::Saving => Some("ui.settings.saving"),
            SaveState::Saved => Some("ui.settings.saved"),
            SaveState::Failed { .. } => Some("ui.settings.saveFailed"),
        }
    }

    pub fn detail(&self) -> &str {
        match self {
            SaveState::Failed { detail } => detail.as_str(),
            _ => "",
        }
    }

    pub fn failed(&self) -> bool {
        matches!(self, SaveState::Failed { .. })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsForm {
    pub theme: String,
    pub density: String,
    pub editor_font: i64,
    pub editor_theme: String,
    pub locale: String,
    pub page_size: i64,
    pub preview_kb: i64,
    pub object_page: i64,
    pub null_copy: String,
    pub query_timeout_sec: i64,
    pub auto_check_updates: bool,
}

impl Default for SettingsForm {
    fn default() -> Self {
        let settings = ShellSettings::default();
        Self::from_settings(&settings)
    }
}

impl SettingsForm {
    pub fn from_settings(settings: &ShellSettings) -> Self {
        Self {
            theme: normalize_theme(&settings.theme).to_owned(),
            density: normalize_density(&settings.density).to_owned(),
            editor_font: clamp(settings.editor_font, FONT_RANGE),
            editor_theme: normalize_editor_theme(&settings.editor_theme).to_owned(),
            locale: normalize_locale(&settings.locale).to_owned(),
            page_size: clamp(settings.page_size, PAGE_SIZE_RANGE),
            preview_kb: clamp(settings.preview_kb, PREVIEW_RANGE),
            object_page: clamp(settings.object_page, OBJECT_PAGE_RANGE),
            null_copy: normalize_null(&settings.null_copy).to_owned(),
            query_timeout_sec: settings.query_timeout_sec.max(0),
            auto_check_updates: settings.auto_check_updates,
        }
    }

    /// 存盘前再归一一次，直接改字段的调用方也不会写出非法值。
    pub fn normalized(&self) -> Self {
        Self {
            theme: normalize_theme(&self.theme).to_owned(),
            density: normalize_density(&self.density).to_owned(),
            editor_font: clamp(self.editor_font, FONT_RANGE),
            editor_theme: normalize_editor_theme(&self.editor_theme).to_owned(),
            locale: normalize_locale(&self.locale).to_owned(),
            page_size: clamp(self.page_size, PAGE_SIZE_RANGE),
            preview_kb: clamp(self.preview_kb, PREVIEW_RANGE),
            object_page: clamp(self.object_page, OBJECT_PAGE_RANGE),
            null_copy: normalize_null(&self.null_copy).to_owned(),
            query_timeout_sec: self.query_timeout_sec.max(0),
            auto_check_updates: self.auto_check_updates,
        }
    }

    pub fn to_settings(&self) -> ShellSettings {
        let form = self.normalized();
        ShellSettings {
            theme: form.theme,
            density: form.density,
            editor_font: form.editor_font,
            editor_theme: form.editor_theme,
            locale: form.locale,
            page_size: form.page_size,
            preview_kb: form.preview_kb,
            object_page: form.object_page,
            null_copy: form.null_copy,
            query_timeout_sec: form.query_timeout_sec,
            auto_check_updates: form.auto_check_updates,
        }
    }

    pub fn apply_shell(&self, shell: &mut Shell) {
        let form = self.normalized();
        shell.theme = match form.theme.as_str() {
            "light" => ThemePreference::Light,
            "dark" => ThemePreference::Dark,
            _ => ThemePreference::System,
        };
        shell.density = match form.density.as_str() {
            "standard" => Density::Standard,
            "relaxed" => Density::Relaxed,
            _ => Density::Compact,
        };
        shell.locale = if form.locale == "en" { Locale::En } else { Locale::ZhCn };
    }

    pub fn capture_shell(&mut self, shell: &Shell) {
        self.theme = match shell.theme {
            ThemePreference::Light => "light",
            ThemePreference::Dark => "dark",
            ThemePreference::System => "system",
        }
        .to_owned();
        self.density = match shell.density {
            Density::Compact => "compact",
            Density::Standard => "standard",
            Density::Relaxed => "relaxed",
        }
        .to_owned();
        self.locale = shell.locale.code().to_owned();
    }

    pub fn cycle_density(&mut self) {
        self.cycle_density_step(1);
    }

    pub fn cycle_density_step(&mut self, step: isize) {
        self.density = cycle_value(&["compact", "standard", "relaxed"], &self.density, step);
    }

    pub fn set_editor_font(&mut self, size: i64) {
        self.editor_font = clamp(size, FONT_RANGE);
    }

    pub fn step_editor_font(&mut self, delta: i64) {
        self.set_editor_font(self.editor_font.saturating_add(delta));
    }

    pub fn cycle_editor_theme(&mut self) {
        self.cycle_editor_theme_step(1);
    }

    pub fn cycle_editor_theme_step(&mut self, step: isize) {
        self.editor_theme = cycle_value(EDITOR_THEMES, &self.editor_theme, step);
    }

    pub fn set_page_size(&mut self, size: i64) {
        self.page_size = clamp(size, PAGE_SIZE_RANGE);
    }

    pub fn step_page_size(&mut self, delta: i64) {
        self.set_page_size(self.page_size.saturating_add(delta));
    }

    pub fn set_preview_kb(&mut self, size: i64) {
        self.preview_kb = clamp(size, PREVIEW_RANGE);
    }

    pub fn step_preview_kb(&mut self, delta: i64) {
        self.set_preview_kb(self.preview_kb.saturating_add(delta));
    }

    pub fn set_object_page(&mut self, size: i64) {
        self.object_page = clamp(size, OBJECT_PAGE_RANGE);
    }

    pub fn step_object_page(&mut self, delta: i64) {
        self.set_object_page(self.object_page.saturating_add(delta));
    }

    pub fn cycle_timeout(&mut self) {
        self.cycle_timeout_step(1);
    }

    pub fn cycle_timeout_step(&mut self, step: isize) {
        let current = TIMEOUTS.iter().position(|value| *value == self.query_timeout_sec).unwrap_or(0);
        let next = (current as isize + step).rem_euclid(TIMEOUTS.len() as isize) as usize;
        self.query_timeout_sec = TIMEOUTS[next];
    }

    pub fn cycle_null(&mut self) {
        self.cycle_null_step(1);
    }

    pub fn cycle_null_step(&mut self, step: isize) {
        self.null_copy = cycle_value(NULL_COPIES, &self.null_copy, step);
    }

    pub fn toggle_updates(&mut self) {
        self.auto_check_updates = !self.auto_check_updates;
    }
}

fn clamp(value: i64, (min, max): (i64, i64)) -> i64 {
    value.clamp(min, max)
}

fn cycle_value(values: &[&str], current: &str, step: isize) -> String {
    let index = values.iter().position(|item| *item == current).unwrap_or(0);
    let next = (index as isize + step).rem_euclid(values.len() as isize) as usize;
    values[next].to_owned()
}

pub fn normalize_density(value: &str) -> &'static str {
    match value {
        "compact" => "compact",
        "standard" | "comfortable" => "standard",
        "relaxed" => "relaxed",
        _ => "compact",
    }
}

pub fn normalize_theme(value: &str) -> &'static str {
    match value {
        "light" => "light",
        "dark" => "dark",
        "system" => "system",
        _ => "system",
    }
}

pub fn normalize_editor_theme(value: &str) -> &'static str {
    match value {
        "contrast" => "contrast",
        "warm" => "warm",
        "cool" => "cool",
        "mono" => "mono",
        _ => "auto",
    }
}

pub fn normalize_locale(value: &str) -> &'static str {
    if value == "en" { "en" } else { "zh-CN" }
}

pub fn normalize_null(value: &str) -> &'static str {
    match value {
        "empty" => "empty",
        "N" => "N",
        _ => "NULL",
    }
}

pub fn density_label_key(density: &str) -> &'static str {
    match density {
        "standard" => "shell.settings.densityStandard",
        "relaxed" => "shell.settings.densityRelaxed",
        _ => "shell.settings.densityCompact",
    }
}

pub fn editor_theme_key(theme: &str) -> &'static str {
    match theme {
        "contrast" => "shell.settings.editorThemeContrast",
        "warm" => "shell.settings.editorThemeWarm",
        "cool" => "shell.settings.editorThemeCool",
        "mono" => "shell.settings.editorThemeMono",
        _ => "shell.settings.editorThemeAuto",
    }
}

/// 编辑器配色映射：背景与前景。auto 跟随工作区底色，其余档位是稳定可断言的
/// 代码配色，深色主题下再取一组更暗的底和更亮的字。
pub fn editor_theme_colors(theme: &str, dark: bool) -> (u32, u32) {
    match (normalize_editor_theme(theme), dark) {
        ("contrast", false) => (0xffffff, 0x000000),
        ("contrast", true) => (0x000000, 0xffffff),
        ("warm", false) => (0xfdf6e3, 0x3f3a34),
        ("warm", true) => (0x241f1a, 0xe8dfcf),
        ("cool", false) => (0xeef4fb, 0x1f2d3d),
        ("cool", true) => (0x131b24, 0xd7e3ef),
        ("mono", false) => (0xf4f4f4, 0x202020),
        ("mono", true) => (0x1e1e1e, 0xe6e6e6),
        (_, false) => (0xffffff, 0x16191f),
        (_, true) => (0x171a1f, 0xe8ebf0),
    }
}

/// TSV 里数据库 NULL 的显示文本；SQL 字面量仍由 sql_values 固定写成 NULL。
pub fn null_display(value: &str) -> &'static str {
    match normalize_null(value) {
        "empty" => "",
        "N" => "\\N",
        _ => "NULL",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comfortable_density_and_unknown_values_normalize() {
        assert_eq!(normalize_density("comfortable"), "standard");
        assert_eq!(normalize_density("nope"), "compact");
        assert_eq!(normalize_editor_theme("nope"), "auto");
        assert_eq!(normalize_locale("fr"), "zh-CN");
        let mut form = SettingsForm::default();
        assert_eq!(form.density, "compact");
        form.cycle_density();
        assert_eq!(form.density, "standard");
        form.cycle_density();
        assert_eq!(form.density, "relaxed");
        form.cycle_density();
        assert_eq!(form.density, "compact");
        form.set_editor_font(99);
        assert_eq!(form.editor_font, 24);
        form.editor_theme = "warm".into();
        form.theme = "dark".into();
        form.locale = "en".into();
        let mut shell = Shell::default();
        form.apply_shell(&mut shell);
        assert_eq!(shell.density, Density::Compact);
        assert_eq!(shell.theme, ThemePreference::Dark);
        assert_eq!(shell.locale, Locale::En);
        let settings = form.to_settings();
        assert_eq!(settings.editor_theme, "warm");
        assert!(!serde_json::to_string(&settings).unwrap().contains("password"));
    }

    #[test]
    fn every_field_moves_both_ways_and_illegal_stored_values_normalize() {
        let illegal = ShellSettings {
            theme: "neon".into(),
            density: "comfy".into(),
            editor_font: 999,
            editor_theme: "neon".into(),
            locale: "fr".into(),
            page_size: 0,
            preview_kb: 999,
            object_page: -3,
            null_copy: "bogus".into(),
            query_timeout_sec: -5,
            auto_check_updates: true,
        };
        let mut form = SettingsForm::from_settings(&illegal);
        assert_eq!(form.theme, "system");
        assert_eq!(form.density, "compact");
        assert_eq!(form.editor_font, 24);
        assert_eq!(form.editor_theme, "auto");
        assert_eq!(form.locale, "zh-CN");
        assert_eq!(form.page_size, 1);
        assert_eq!(form.preview_kb, 64);
        assert_eq!(form.object_page, 1);
        assert_eq!(form.null_copy, "NULL");
        assert_eq!(form.query_timeout_sec, 0);

        form.cycle_timeout();
        assert_eq!(form.query_timeout_sec, 15);
        form.cycle_timeout();
        assert_eq!(form.query_timeout_sec, 60);
        form.cycle_timeout_step(-1);
        assert_eq!(form.query_timeout_sec, 15);
        form.cycle_timeout_step(-1);
        assert_eq!(form.query_timeout_sec, 0);

        form.cycle_null();
        assert_eq!(form.null_copy, "empty");
        form.cycle_null();
        assert_eq!(form.null_copy, "N");
        form.cycle_null_step(-1);
        assert_eq!(form.null_copy, "empty");

        form.cycle_editor_theme_step(1);
        assert_eq!(form.editor_theme, "contrast");
        form.cycle_editor_theme_step(-1);
        assert_eq!(form.editor_theme, "auto");

        form.step_page_size(49);
        assert_eq!(form.page_size, 50);
        form.step_page_size(-49);
        assert_eq!(form.page_size, 1);
        form.set_page_size(99_999);
        assert_eq!(form.page_size, 10_000);
        form.step_preview_kb(1);
        assert_eq!(form.preview_kb, 64);
        form.step_preview_kb(-63);
        assert_eq!(form.preview_kb, 1);
        form.step_object_page(9);
        assert_eq!(form.object_page, 10);
        form.step_object_page(-9);
        assert_eq!(form.object_page, 1);
        form.set_editor_font(9);
        assert_eq!(form.editor_font, 10);
        form.step_editor_font(20);
        assert_eq!(form.editor_font, 24);

        let saved = form.clone();
        form.theme = "bogus".into();
        form.density = "bogus".into();
        form.editor_theme = "bogus".into();
        form.locale = "bogus".into();
        form.null_copy = "bogus".into();
        form.page_size = -1;
        let normalized = form.to_settings();
        assert_eq!(normalized.theme, "system");
        assert_eq!(normalized.density, "compact");
        assert_eq!(normalized.editor_theme, "auto");
        assert_eq!(normalized.locale, "zh-CN");
        assert_eq!(normalized.null_copy, "NULL");
        assert_eq!(normalized.page_size, 1);
        assert_eq!(saved.to_settings().preview_kb, 1);
    }

    #[test]
    fn public_settings_map_to_observable_editor_colors_and_tsv_nulls() {
        assert_eq!(null_display("NULL"), "NULL");
        assert_eq!(null_display("empty"), "");
        assert_eq!(null_display("N"), "\\N");
        assert_eq!(null_display("bogus"), "NULL");
        let auto = editor_theme_colors("auto", false);
        assert_ne!(auto, editor_theme_colors("warm", false));
        assert_ne!(auto, editor_theme_colors("contrast", true));
        assert_eq!(editor_theme_colors("NEON", false), auto, "未知档位归一为 auto");
        assert_ne!(editor_theme_colors("warm", false), editor_theme_colors("warm", true));
    }

    #[test]
    fn sections_have_stable_commands_and_labels() {
        assert_eq!(SettingsSection::ALL.len(), 3, "设计 S8.3 是三块分区");
        for section in SettingsSection::ALL {
            assert_eq!(SettingsSection::parse(section.command()), Some(section));
            assert_ne!(section.label_key(), section.command());
        }
        assert_eq!(SettingsSection::parse("settings-section-bogus"), None);
        assert_ne!(SettingsSection::Appearance.label_key(), SettingsSection::Data.label_key());
        assert_ne!(SettingsSection::Data.label_key(), SettingsSection::Local.label_key());
    }

    #[test]
    fn save_state_only_reports_copy_where_the_user_needs_it() {
        assert_eq!(SaveState::default(), SaveState::Idle);
        assert_eq!(SaveState::Idle.label_key(), None, "没改过设置时不打扰");
        assert!(!SaveState::Idle.failed());
        assert_eq!(SaveState::Saving.label_key(), Some("ui.settings.saving"));
        assert_eq!(SaveState::Saved.label_key(), Some("ui.settings.saved"));
        let failed = SaveState::Failed { detail: "磁盘只读".into() };
        assert!(failed.failed());
        assert_eq!(failed.label_key(), Some("ui.settings.saveFailed"));
        assert_eq!(failed.detail(), "磁盘只读");
        assert_eq!(SaveState::Saved.detail(), "", "只有失败才带底层原因");
    }

    #[test]
    fn only_commands_that_change_settings_use_the_feedback_path() {
        for id in [
            "theme-light",
            "theme-dark",
            "density-compact",
            "locale-en",
            "font-up",
            "editor-theme-next",
            "page-size-down",
            "preview-up",
            "object-page-down",
            "null-N",
            "timeout-15",
            "toggle-updates",
        ] {
            assert!(is_settings_command(id), "{id}");
        }
        // 分区导航、页面本身和历史预览按钮都不是设置写入，不能被当成保存失败反馈的来源。
        for id in [
            "settings",
            "settings-section-data",
            "settings-retry-save",
            "history-preview-open",
            "history-preview-copy",
            "history-preview-load",
            "new-query",
        ] {
            assert!(!is_settings_command(id), "{id}");
        }
    }
}
