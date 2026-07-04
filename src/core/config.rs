use std::path::PathBuf;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TerminalApp {
    /// Try iTerm2, then Warp, then Terminal.app (original behaviour).
    #[default]
    Auto,
    Terminal,
    ITerm2,
    Warp,
    Ghostty,
}

fn default_sidebar_width() -> f32 { 200.0 }
fn default_git_panel_height() -> f32 { 260.0 }
fn default_git_panel_width() -> f32 { 380.0 }
fn default_terminal_panel_height() -> f32 { 220.0 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub show_hidden: bool,
    pub last_path: Option<PathBuf>,
    #[serde(default)]
    pub theme: Theme,
    #[serde(default)]
    pub custom_theme: Option<String>,
    #[serde(default)]
    pub terminal: TerminalApp,
    #[serde(default = "default_sidebar_width")]
    pub sidebar_width: f32,
    #[serde(default = "default_git_panel_height")]
    pub git_panel_height: f32,
    #[serde(default)]
    pub git_panel_right: bool,
    #[serde(default = "default_git_panel_width")]
    pub git_panel_width: f32,
    #[serde(default = "default_terminal_panel_height")]
    pub terminal_panel_height: f32,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            show_hidden: false,
            last_path: None,
            theme: Theme::System,
            custom_theme: None,
            terminal: TerminalApp::Auto,
            sidebar_width: default_sidebar_width(),
            git_panel_height: default_git_panel_height(),
            git_panel_right: false,
            git_panel_width: default_git_panel_width(),
            terminal_panel_height: default_terminal_panel_height(),
        }
    }
}

impl AppConfig {
    fn config_path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home)
            .join(".config")
            .join("file-explorer")
            .join("config.json")
    }

    pub fn load() -> Self {
        let path = Self::config_path();
        if let Ok(data) = std::fs::read_to_string(&path) {
            serde_json::from_str(&data).unwrap_or_default()
        } else {
            Self::default()
        }
    }

    pub fn save(&self) {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(data) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, data);
        }
    }
}
