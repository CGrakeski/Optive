//! `Optive.lock` — 可复现依赖图。

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
    /// 钉死的 object id（commit SHA）；tag 在解析时已剥皮。
    pub rev: String,
    pub id: String,
    /// 意图：toml 声明的 branch（可追 tip）；与 `tag` 互斥。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// 意图：toml 声明的 tag 名；`rev` 为其剥皮后的 commit。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// toml 声明了 `rev = "..."`（commit pin）。与 tip（`None`）区分：二者都无 branch/tag。
    /// 必须显式写出；缺省即格式错误（不做旧 lock 兼容）。
    pub pinned: bool,
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
        if lock.version != 1 {
            return Err(format!(
                "unsupported {}: version {} (expected 1)",
                path.display(),
                lock.version
            ));
        }
        Ok(Some(lock))
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let text = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        // 原子写：先写临时文件再 rename，避免崩溃截断锁文件。
        let tmp = path.with_extension("lock.tmp");
        fs::write(&tmp, &text).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
        fs::rename(&tmp, path).map_err(|e| {
            let _ = fs::remove_file(&tmp);
            format!("cannot rename {} → {}: {e}", tmp.display(), path.display())
        })?;
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
        RevSpec::Commit(r) => {
            // 钉死 commit：必须标记 pinned，且 rev 一致。
            edge.pinned && r == &edge.rev && edge.branch.is_none() && edge.tag.is_none()
        }
        RevSpec::Tag(t) => {
            edge.tag.as_deref() == Some(t.as_str())
                && edge.branch.is_none()
                && !edge.pinned
        }
        RevSpec::Branch(b) => {
            edge.branch.as_deref() == Some(b.as_str()) && edge.tag.is_none() && !edge.pinned
        }
        RevSpec::None => {
            // tip：无 branch/tag，且非 commit pin。
            !edge.pinned && edge.branch.is_none() && edge.tag.is_none()
        }
    }
}

/// 公开：供 `update <name>` 物化其它根前校验意图。
pub fn dependency_matches_lock_edge(dep: &Dependency, edge: &LockEdge) -> bool {
    intent_matches_edge(dep, edge)
}

fn normalize_cmp(url: &str) -> String {
    // 与 CAS 使用同一套规范化（file:// 保大小写，网络 URL 整段小写）。
    super::store::normalize_git_url(url)
}
