//! 设置映射到版本化 `ShellSettings`。密度保留 compact / standard / relaxed 三档。

use crate::app::{Density, Locale, Shell, ThemePreference};
use crate::state::ShellSettings;

pub const EDITOR_THEMES: &[&str] = &["auto", "contrast", "warm", "cool", "mono"];
pub const NULL_COPIES: &[&str] = &["NULL", "empty", "N"];

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
            editor_font: settings.editor_font.clamp(10, 24),
            editor_theme: normalize_editor_theme(&settings.editor_theme).to_owned(),
            locale: normalize_locale(&settings.locale).to_owned(),
            page_size: settings.page_size.clamp(1, 10_000),
            preview_kb: settings.preview_kb.clamp(1, 64),
            object_page: settings.object_page.clamp(1, 10_000),
            null_copy: normalize_null(&settings.null_copy).to_owned(),
            query_timeout_sec: settings.query_timeout_sec.max(0),
            auto_check_updates: settings.auto_check_updates,
        }
    }

    pub fn to_settings(&self) -> ShellSettings {
        ShellSettings {
            theme: self.theme.clone(),
            density: self.density.clone(),
            editor_font: self.editor_font,
            editor_theme: self.editor_theme.clone(),
            locale: self.locale.clone(),
            page_size: self.page_size,
            preview_kb: self.preview_kb,
            object_page: self.object_page,
            null_copy: self.null_copy.clone(),
            query_timeout_sec: self.query_timeout_sec,
            auto_check_updates: self.auto_check_updates,
        }
    }

    pub fn apply_shell(&self, shell: &mut Shell) {
        shell.theme = match self.theme.as_str() {
            "light" => ThemePreference::Light,
            "dark" => ThemePreference::Dark,
            _ => ThemePreference::System,
        };
        shell.density = match self.density.as_str() {
            "standard" => Density::Standard,
            "relaxed" => Density::Relaxed,
            _ => Density::Compact,
        };
        shell.locale = if self.locale == "en" { Locale::En } else { Locale::ZhCn };
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
        self.density = match self.density.as_str() {
            "compact" => "standard",
            "standard" => "relaxed",
            _ => "compact",
        }
        .to_owned();
    }

    pub fn set_editor_font(&mut self, size: i64) {
        self.editor_font = size.clamp(10, 24);
    }

    pub fn cycle_editor_theme(&mut self) {
        let index = EDITOR_THEMES.iter().position(|item| *item == self.editor_theme).unwrap_or(0);
        self.editor_theme = EDITOR_THEMES[(index + 1) % EDITOR_THEMES.len()].to_owned();
    }
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
    if EDITOR_THEMES.contains(&value) { 
        match value {
            "contrast" => "contrast",
            "warm" => "warm",
            "cool" => "cool",
            "mono" => "mono",
            _ => "auto",
        }
    } else {
        "auto"
    }
}

pub fn normalize_locale(value: &str) -> &'static str {
    if value == "en" { "en" } else { "zh-CN" }
}

pub fn normalize_null(value: &str) -> &'static str {
    if NULL_COPIES.contains(&value) {
        match value {
            "empty" => "empty",
            "N" => "N",
            _ => "NULL",
        }
    } else {
        "NULL"
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
        form.set_editor_font(99);
        assert_eq!(form.editor_font, 24);
        form.editor_theme = "warm".into();
        form.theme = "dark".into();
        form.locale = "en".into();
        let mut shell = Shell::default();
        form.apply_shell(&mut shell);
        assert_eq!(shell.density, Density::Relaxed);
        assert_eq!(shell.theme, ThemePreference::Dark);
        assert_eq!(shell.locale, Locale::En);
        let settings = form.to_settings();
        assert_eq!(settings.editor_theme, "warm");
        assert!(!serde_json::to_string(&settings).unwrap().contains("password"));
    }
}
