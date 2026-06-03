//! User settings persisted to RON at the OS-conventional config location.
//!
//! Settings are loaded on app start and saved when the user closes the
//! settings modal. A missing or invalid config file falls back to defaults
//! — there is no migration story yet, and unrecognized fields are tolerated
//! via `#[serde(default)]` on each top-level field.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Settings {
    /// Preferred theme. On startup the app applies this; the F10 theme picker
    /// can override transiently without changing this until settings are saved.
    #[serde(default)]
    pub theme: ThemePref,

    /// Render the emoji file-type glyph in front of each row name.
    #[serde(default = "default_true")]
    pub show_glyphs: bool,

    /// Alternating row-background tint for horizontal scanning.
    #[serde(default = "default_true")]
    pub row_striping: bool,

    /// On SFTP back-navigation, kick off a background refresh that replaces
    /// cached entries with fresh server-side data. When false, cached entries
    /// stay until the user manually refreshes (F2).
    #[serde(default = "default_true")]
    pub auto_refresh: bool,

    /// Per-extension overrides for the "open remote with mpv/vlc/ffplay"
    /// streaming path. Empty means the built-in probe order is used.
    #[serde(default)]
    pub media_handlers: Vec<MediaHandler>,

    /// Saved SFTP host bookmarks. Surfaced in the quick-jump dropdown so a
    /// labeled bookmark expands to (host, initial_path).
    #[serde(default)]
    pub bookmarks: Vec<Bookmark>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub enum ThemePref {
    #[default]
    Dark,
    Light,
    External(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MediaHandler {
    pub extension: String,
    pub command: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bookmark {
    pub label: String,
    pub host: String,
    pub path: String,
}

fn default_true() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemePref::Dark,
            show_glyphs: true,
            row_striping: true,
            auto_refresh: true,
            media_handlers: Vec::new(),
            bookmarks: Vec::new(),
        }
    }
}

/// Locate the OS-conventional config directory for fileman.
/// Linux: `$XDG_CONFIG_HOME/fileman` or `$HOME/.config/fileman`.
/// macOS: `$HOME/Library/Application Support/fileman`.
/// Windows: `%APPDATA%/fileman`.
pub fn config_dir() -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME")
            && !xdg.is_empty()
        {
            return Some(PathBuf::from(xdg).join("fileman"));
        }
        if let Ok(home) = std::env::var("HOME")
            && !home.is_empty()
        {
            return Some(PathBuf::from(home).join(".config").join("fileman"));
        }
        None
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(home) = std::env::var("HOME")
            && !home.is_empty()
        {
            return Some(
                PathBuf::from(home)
                    .join("Library")
                    .join("Application Support")
                    .join("fileman"),
            );
        }
        None
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA")
            && !appdata.is_empty()
        {
            return Some(PathBuf::from(appdata).join("fileman"));
        }
        None
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        None
    }
}

pub fn settings_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("settings.ron"))
}

/// Load settings from disk. Returns defaults if the file is missing, empty,
/// or fails to parse.
pub fn load() -> Settings {
    let Some(path) = settings_path() else {
        return Settings::default();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Settings::default();
    };
    match ron::from_str::<Settings>(&text) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("settings: parse error in {}: {e}", path.display());
            Settings::default()
        }
    }
}

/// Serialize settings to disk. Creates the config directory if needed.
pub fn save(settings: &Settings) -> anyhow::Result<()> {
    let dir = config_dir().ok_or_else(|| anyhow::anyhow!("no config dir available"))?;
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("settings.ron");
    let pretty = ron::ser::PrettyConfig::default();
    let text = ron::ser::to_string_pretty(settings, pretty)?;
    std::fs::write(&path, text)?;
    Ok(())
}
