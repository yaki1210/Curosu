//! 设置：serde 结构体 + %APPDATA%\Curosu\settings.json 读写。
//! 字段与原 C# 版一一对应。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const MIN_CURSOR_WIDTH: f64 = 16.0;
pub const MAX_CURSOR_WIDTH: f64 = 64.0;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub cursor_width: f64,
    pub auto_start: bool,
    pub tap_sound_enabled: bool,
    pub tap_sound_volume: f64,
    pub hover_sound_enabled: bool,
    pub hover_sound_volume: f64,
    pub hover_sound_as_resize_prompt: bool,
    /// 全屏时仍保留动画覆盖层的窗口规则，每行一条。
    #[serde(default)]
    pub fullscreen_overlay_exclusions: String,
    /// 用户从设置窗口选择的程序名；全屏时保留动画覆盖层。
    #[serde(default)]
    pub fullscreen_overlay_exclusion_executables: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            cursor_width: 30.0,
            auto_start: false,
            tap_sound_enabled: true,
            tap_sound_volume: 1.0,
            hover_sound_enabled: true,
            hover_sound_volume: 1.0,
            hover_sound_as_resize_prompt: false,
            fullscreen_overlay_exclusions: String::new(),
            fullscreen_overlay_exclusion_executables: Vec::new(),
        }
    }
}

/// 设置文件路径。
pub fn settings_path() -> PathBuf {
    let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
    let mut p = PathBuf::from(base);
    p.push("Curosu");
    p.push("settings.json");
    p
}

fn legacy_settings_path() -> PathBuf {
    let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
    let mut p = PathBuf::from(base);
    p.push("OsuCursorRs");
    p.push("settings.json");
    p
}

/// 设置文件是否存在（首次启动判断）。
pub fn exists() -> bool {
    settings_path().exists() || legacy_settings_path().exists()
}

impl Settings {
    pub fn load() -> Self {
        let contents = std::fs::read_to_string(settings_path())
            .or_else(|_| std::fs::read_to_string(legacy_settings_path()))
            .ok();
        match contents.and_then(|s| serde_json::from_str::<Settings>(&s).ok()) {
            Some(mut s) => {
                s.cursor_width = s.cursor_width.clamp(MIN_CURSOR_WIDTH, MAX_CURSOR_WIDTH);
                s
            }
            None => Settings::default(),
        }
    }

    pub fn save(&self) {
        let path = settings_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Settings;

    #[test]
    fn legacy_settings_default_the_fullscreen_exclusions() {
        let settings: Settings = serde_json::from_str(
            r#"{
                "cursor_width": 30.0,
                "auto_start": false,
                "tap_sound_enabled": true,
                "tap_sound_volume": 1.0,
                "hover_sound_enabled": true,
                "hover_sound_volume": 1.0,
                "hover_sound_as_resize_prompt": false
            }"#,
        )
        .expect("legacy settings should deserialize");

        assert!(settings.fullscreen_overlay_exclusions.is_empty());
        assert!(settings.fullscreen_overlay_exclusion_executables.is_empty());
    }
}
