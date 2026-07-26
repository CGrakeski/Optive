//! `Optive.cache` — 本机 tip / id 小抄（不进 git）。

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectCache {
    #[serde(default)]
    pub entries: BTreeMap<String, CacheEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntry {
    pub git: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub commit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

impl ProjectCache {
    pub fn load(path: &Path) -> Self {
        if !path.is_file() {
            return Self::default();
        }
        fs::read_to_string(path)
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let text = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        fs::write(path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        Ok(())
    }

    fn key(git: &str, branch: Option<&str>) -> String {
        match branch {
            Some(b) => format!("{git}@{b}"),
            None => git.to_string(),
        }
    }

    pub fn get_commit(&self, git: &str, branch: Option<&str>) -> Option<&str> {
        let k = Self::key(git, branch);
        self.entries.get(&k).map(|e| e.commit.as_str()).or_else(|| {
            // 宽松：仅按 git 匹配
            self.entries
                .values()
                .find(|e| e.git == git && e.branch.as_deref() == branch)
                .map(|e| e.commit.as_str())
        })
    }

    pub fn put(
        &mut self,
        git: &str,
        branch: Option<&str>,
        commit: &str,
        id: Option<&str>,
    ) {
        let k = Self::key(git, branch);
        self.entries.insert(
            k,
            CacheEntry {
                git: git.to_string(),
                branch: branch.map(|s| s.to_string()),
                commit: commit.to_string(),
                id: id.map(|s| s.to_string()),
            },
        );
    }
}
