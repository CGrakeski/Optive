//! `Optive.cache` — 本机 tip / id 小抄（不进 git）。

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::store;

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
        match fs::read_to_string(path) {
            Ok(t) => match toml::from_str(&t) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!(
                        "warning: ignoring corrupt {}: {e}",
                        path.display()
                    );
                    Self::default()
                }
            },
            Err(e) => {
                eprintln!(
                    "warning: cannot read {}: {e}",
                    path.display()
                );
                Self::default()
            }
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let text = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        let tmp = path.with_extension("cache.tmp");
        fs::write(&tmp, &text).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
        fs::rename(&tmp, path).map_err(|e| {
            let _ = fs::remove_file(&tmp);
            format!("cannot rename {} → {}: {e}", tmp.display(), path.display())
        })?;
        Ok(())
    }

    fn key(git: &str, branch: Option<&str>) -> String {
        let git = store::normalize_git_url(git);
        match branch {
            Some(b) => format!("{git}@{b}"),
            None => git,
        }
    }

    pub fn get_commit(&self, git: &str, branch: Option<&str>) -> Option<&str> {
        let norm = store::normalize_git_url(git);
        let k = Self::key(git, branch);
        self.entries.get(&k).map(|e| e.commit.as_str()).or_else(|| {
            // 宽松：规范化后按 git + branch 匹配（兼容旧缓存里未规范化的键）
            self.entries
                .values()
                .find(|e| {
                    store::normalize_git_url(&e.git) == norm && e.branch.as_deref() == branch
                })
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
        let norm = store::normalize_git_url(git);
        let k = Self::key(git, branch);
        self.entries.insert(
            k,
            CacheEntry {
                git: norm,
                branch: branch.map(|s| s.to_string()),
                commit: commit.to_string(),
                id: id.map(|s| s.to_string()),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tip_cache_key_normalizes_git_url() {
        let mut c = ProjectCache::default();
        c.put("https://GitHub.com/Foo/Bar.git", None, "abc", Some("id1"));
        assert_eq!(
            c.get_commit("https://github.com/foo/bar", None),
            Some("abc")
        );
    }
}
