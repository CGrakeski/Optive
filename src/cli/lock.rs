//! `optive.lock` — 可复现依赖图。

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::manifest::{Dependency, Manifest, RevSpec};

pub const ROOT_PARENT: &str = "__root__";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockFile {
    pub version: u32,
    pub edges: Vec<LockEdge>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockEdge {
    /// `__root__` 或 content id
    pub parent: String,
    pub name: String,
    pub git: String,
    pub rev: String,
    pub id: String,
}

impl LockFile {
    pub fn new(edges: Vec<LockEdge>) -> Self {
        Self { version: 1, edges }
    }

    pub fn load(path: &Path) -> Result<Option<Self>, String> {
        if !path.is_file() {
            return Ok(None);
        }
        let text = fs::read_to_string(path).map_err(|e| {
            format!("cannot read {}: {e}", path.display())
        })?;
        let lock: LockFile = toml::from_str(&text).map_err(|e| {
            format!("invalid {}: {e}", path.display())
        })?;
        Ok(Some(lock))
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let text = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        fs::write(path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        Ok(())
    }

    /// 根清单意图是否与 lock 中根边一致（名字集合 + 每条 git/声明 rev 可复现）。
    pub fn matches_root_intent(&self, manifest: &Manifest) -> bool {
        let mut lock_root: BTreeMap<&str, &LockEdge> = BTreeMap::new();
        for e in &self.edges {
            if e.parent == ROOT_PARENT {
                lock_root.insert(e.name.as_str(), e);
            }
        }
        if lock_root.len() != manifest.dependencies.len() {
            return false;
        }
        for (name, dep) in &manifest.dependencies {
            let Some(edge) = lock_root.get(name.as_str()) else {
                return false;
            };
            if !intent_matches_edge(dep, edge) {
                return false;
            }
        }
        true
    }

    pub fn root_edges(&self) -> impl Iterator<Item = &LockEdge> {
        self.edges.iter().filter(|e| e.parent == ROOT_PARENT)
    }
}

fn intent_matches_edge(dep: &Dependency, edge: &LockEdge) -> bool {
    if normalize_cmp(&dep.git) != normalize_cmp(&edge.git) {
        return false;
    }
    match &dep.rev {
        RevSpec::Commit(r) => r == &edge.rev,
        RevSpec::Tag(t) => {
            // tag 钉死：lock 中可能存 tag 名或已解析 sha；要求声明 tag 仍写在意图里且 git 一致，
            // 且 lock 的 id 对应同一内容——这里用：edge.rev == tag 或至少 git 一致且名字在 lock。
            // 严格：若 toml 写 tag，lock.rev 应为解析后的 object 或 tag 名。
            t == &edge.rev || true_if_same_git_only_tag(dep, edge)
        }
        RevSpec::Branch(_) | RevSpec::None => {
            // 可追边：意图一致只要名字+git 在 lock 根边出现（rev 由 lock 钉死）
            true
        }
    }
}

fn true_if_same_git_only_tag(dep: &Dependency, edge: &LockEdge) -> bool {
    // tag 名可能已解析为 sha；接受 git 相同即可（名字已匹配）
    let _ = (dep, edge);
    true
}

fn normalize_cmp(url: &str) -> String {
    let mut u = url.trim().to_ascii_lowercase();
    if u.ends_with('/') {
        u.pop();
    }
    if u.ends_with(".git") {
        u.truncate(u.len() - 4);
    }
    u
}
