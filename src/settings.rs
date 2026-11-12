//! User preferences persisted next to recent.json.

use crate::error::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Color theme choice. `System` follows the macOS appearance.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

impl Appearance {
    /// Index used by the Slint UI: 0 system, 1 light, 2 dark.
    pub fn index(self) -> i32 {
        match self {
            Appearance::System => 0,
            Appearance::Light => 1,
            Appearance::Dark => 2,
        }
    }

    /// Inverse of `index`; unknown values fall back to `System`.
    pub fn from_index(i: i32) -> Appearance {
        match i {
            1 => Appearance::Light,
            2 => Appearance::Dark,
            _ => Appearance::System,
        }
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub appearance: Appearance,
}

impl Settings {
    /// Missing or unreadable file gives defaults.
    pub fn load_from(file: &Path) -> Settings {
        std::fs::read_to_string(file)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save_to(&self, file: &Path) -> Result<()> {
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(file, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn default_file() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("codediff")
            .join("settings.json")
    }
}
