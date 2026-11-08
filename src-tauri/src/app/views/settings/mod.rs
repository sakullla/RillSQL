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
}
