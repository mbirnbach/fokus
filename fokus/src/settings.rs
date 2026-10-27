//! App settings that are kept between runs (in the config folder as
//! `fokus/settings.json`). Machine-independent preferences only.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    /// Language code ("en", "de", or a community file); `None` follows the system.
    #[serde(default)]
    pub language: Option<String>,
    /// "light" or "dark"; `None` follows the system.
    #[serde(default)]
    pub theme: Option<String>,
}

impl Settings {
    fn path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("fokus").join("settings.json"))
    }

    /// Settings from disk, or the defaults if there are none (or they are unreadable).
    pub fn load() -> Self {
        Self::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(path) = Self::path() else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, serde_json::to_string_pretty(self).unwrap());
    }
}
