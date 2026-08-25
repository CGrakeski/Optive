//! `Optive.lock` v2 — 可复现、可校验的依赖图。

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::manifest::{Dependency, Manifest, RevSpec};

pub const ROOT_PARENT: &str = "__root__";
pub const LOCK_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockFile {
    pub version: u32,
    pub edges: Vec<LockEdge>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockEdge {
    /// `__root__` 或父包的 `package_id`。
    pub parent: String,
    pub name: String,
    /// 规范化后的 Git 来源。
    pub source: String,
    /// 完整 commit object id；tag/branch 均已解析。
    pub commit: String,
    /// commit 对应的 Git tree object id。
    pub tree: String,
    /// 物化工作树内容摘要（SHA-256；排除 VCS/构建缓存）。
    pub content_digest: String,
    /// `sha256(source NUL commit)`，用于 CAS 路径和依赖边父节点。
    pub package_id: String,
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
    pub const fn new(edges: Vec<LockEdge>) -> Self {
        Self {
            version: LOCK_VERSION,
            edges,
        }
    }

    pub fn load(path: &Path) -> Result<Option<Self>, String> {
        if !path.is_file() {
            return Ok(None);
        }
        let text =
            fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let value: toml::Value = toml::from_str(&text)
            .map_err(|e| rebuild_error(path, &format!("invalid TOML: {e}")))?;
        let version = value
            .get("version")
            .and_then(toml::Value::as_integer)
            .and_then(|v| u32::try_from(v).ok());
        if version != Some(LOCK_VERSION) {
            return Err(format!(
                "unsupported {} lock format (found version {}, expected {LOCK_VERSION}); delete {} and regenerate it with `Optive update`",
                path.display(),
                version.map_or_else(|| "missing".into(), |v| v.to_string()),
                path.display()
            ));
        }
        let lock: Self = value
            .try_into()
            .map_err(|e| rebuild_error(path, &format!("invalid v{LOCK_VERSION} data: {e}")))?;
        lock.validate(path)?;
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

    fn validate(&self, path: &Path) -> Result<(), String> {
        let mut ids = std::collections::BTreeSet::new();
        let mut keys = std::collections::BTreeSet::new();
        for edge in &self.edges {
            if edge.source != super::store::normalize_git_url(&edge.source) {
                return Err(rebuild_error(
                    path,
                    &format!("edge `{}` source is not normalized", edge.name),
                ));
            }
            if !is_full_object_id(&edge.commit) {
                return Err(rebuild_error(
                    path,
                    &format!("edge `{}` commit is not a full object id", edge.name),
                ));
            }
            if !is_full_object_id(&edge.tree) {
                return Err(rebuild_error(
                    path,
                    &format!("edge `{}` tree is not a full object id", edge.name),
                ));
            }
            if !is_sha256(&edge.content_digest) {
                return Err(rebuild_error(
                    path,
                    &format!("edge `{}` content_digest is not SHA-256", edge.name),
                ));
            }
            let expected_id = super::store::content_id(&edge.source, &edge.commit);
            if edge.package_id != expected_id {
                return Err(rebuild_error(
                    path,
                    &format!(
                        "edge `{}` package_id {} does not match source+commit ({expected_id})",
                        edge.name, edge.package_id
                    ),
                ));
            }
            if !keys.insert((edge.parent.as_str(), edge.name.as_str())) {
                return Err(rebuild_error(
                    path,
                    &format!("duplicate dependency edge ({}, {})", edge.parent, edge.name),
                ));
            }
            ids.insert(edge.package_id.as_str());
        }
        for edge in &self.edges {
            if edge.parent != ROOT_PARENT && !ids.contains(edge.parent.as_str()) {
                return Err(rebuild_error(
                    path,
                    &format!(
                        "edge `{}` references unknown parent package_id {}",
                        edge.name, edge.parent
                    ),
                ));
            }
        }
        Ok(())
    }
}

fn intent_matches_edge(dep: &Dependency, edge: &LockEdge) -> bool {
    match &dep.rev {
        // 索引依赖：意图是「包名 + 版本」；lock 里的 git URL 来自当时的 index.json。
        RevSpec::IndexVersion(v) => {
            edge.tag.as_deref() == Some(v.as_str())
                && edge.branch.is_none()
                && !edge.pinned
                && (dep.git.is_empty() || normalize_cmp(&dep.git) == edge.source)
        }
        _ => {
            if normalize_cmp(&dep.git) != edge.source {
                return false;
            }
            match &dep.rev {
                RevSpec::Commit(r) => {
                    edge.pinned
                        && normalize_object_id(r) == edge.commit
                        && edge.branch.is_none()
                        && edge.tag.is_none()
                }
                RevSpec::Tag(t) => {
                    edge.tag.as_deref() == Some(t.as_str()) && edge.branch.is_none() && !edge.pinned
                }
                RevSpec::Branch(b) => {
                    edge.branch.as_deref() == Some(b.as_str()) && edge.tag.is_none() && !edge.pinned
                }
                RevSpec::None => !edge.pinned && edge.branch.is_none() && edge.tag.is_none(),
                RevSpec::IndexVersion(_) => unreachable!(),
            }
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

fn normalize_object_id(id: &str) -> String {
    id.trim().to_ascii_lowercase()
}

fn is_full_object_id(id: &str) -> bool {
    matches!(id.len(), 40 | 64) && id.bytes().all(|b| b.is_ascii_hexdigit())
}

fn is_sha256(digest: &str) -> bool {
    digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit())
}

fn rebuild_error(path: &Path, reason: &str) -> String {
    format!(
        "invalid {}: {reason}; this lock format is not migrated automatically—delete {} and regenerate it with `Optive update`",
        path.display(),
        path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::manifest::Dependency;

    fn edge(source: &str, tag: Option<&str>) -> LockEdge {
        let source = normalize_cmp(source);
        let commit = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string();
        LockEdge {
            parent: ROOT_PARENT.into(),
            name: "p".into(),
            package_id: super::super::store::content_id(&source, &commit),
            source,
            commit,
            tree: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
            content_digest: "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
                .into(),
            branch: None,
            tag: tag.map(str::to_string),
            pinned: false,
        }
    }

    #[test]
    fn index_version_with_git_must_match_url() {
        let dep = Dependency::from_git_version("https://example.com/a.git", "0.1.0");
        assert!(intent_matches_edge(
            &dep,
            &edge("https://example.com/a.git", Some("0.1.0"))
        ));
        assert!(!intent_matches_edge(
            &dep,
            &edge("https://example.com/b.git", Some("0.1.0"))
        ));
    }

    #[test]
    fn index_version_without_git_ignores_url() {
        let dep = Dependency::from_index_version("0.1.0");
        assert!(intent_matches_edge(
            &dep,
            &edge("https://anywhere.example/x.git", Some("0.1.0"))
        ));
    }

    #[test]
    fn old_lock_requires_delete_and_rebuild() {
        let dir = std::env::temp_dir().join(format!("optive_lock_v1_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Optive.lock");
        fs::write(&path, "version = 1\nedges = []\n").unwrap();
        let err = LockFile::load(&path).unwrap_err();
        assert!(err.contains("expected 2"), "{err}");
        assert!(err.contains("delete"), "{err}");
        assert!(err.contains("Optive update"), "{err}");
        let _ = fs::remove_dir_all(&dir);
    }
}
