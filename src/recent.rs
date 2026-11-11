use crate::error::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const MAX_RECENT: usize = 10;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct RecentRepos {
    pub paths: Vec<PathBuf>,
}

impl RecentRepos {
    pub fn load_from(file: &Path) -> RecentRepos {
        let mut r: RecentRepos = std::fs::read_to_string(file)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        r.paths.retain(|p| p.exists());
        r
    }

    pub fn save_to(&self, file: &Path) -> Result<()> {
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(file, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn touch(&mut self, path: PathBuf) {
        self.paths.retain(|p| *p != path);
        self.paths.insert(0, path);
        self.paths.truncate(MAX_RECENT);
    }

    pub fn default_file() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("codediff")
            .join("recent.json")
    }
}
