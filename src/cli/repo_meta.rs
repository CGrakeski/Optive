//! 本地仓库元数据：横幅版本展示、clean 检测、tag 查询（tag-only 模型）。

use std::error::Error;
use std::path::Path;
use std::process::Command;

use gix::bstr::ByteSlice;

/// 项目横幅用的版本段：精确 semver tag，或 `(unreleased) <short>`。
pub fn project_version_label(root: &Path) -> Option<String> {
    let repo = gix::open(root).ok()?;
    let head = repo.head_id().ok()?;
    let head_hex = head.to_string();
    let short = short_hex(&head_hex);

    if let Some(tag) = tag_exactly_at_head(&repo, &head_hex) {
        return Some(tag);
    }

    let dirty = is_worktree_dirty(root).unwrap_or(false);
    let suffix = if dirty { "-dirty" } else { "" };
    Some(format!("(unreleased) {short}{suffix}"))
}

/// HEAD 是否恰好被某个 tag 指向；返回 tag 名（优先 semver，否则任意）。
pub fn head_exact_tag(root: &Path) -> Option<String> {
    let repo = gix::open(root).ok()?;
    let head = repo.head_id().ok()?;
    tag_exactly_at_head(&repo, &head.to_string())
}

pub fn short_head_sha(root: &Path) -> Option<String> {
    let repo = gix::open(root).ok()?;
    let head = repo.head_id().ok()?;
    Some(short_hex(&head.to_string()))
}

fn short_hex(full: &str) -> String {
    full.chars().take(7).collect()
}

fn tag_exactly_at_head(repo: &gix::Repository, head_hex: &str) -> Option<String> {
    let platform = repo.references().ok()?;
    let mut semver_hit: Option<String> = None;
    let mut any_hit: Option<String> = None;
    for r in platform.all().ok()? {
        let Ok(r) = r else {
            continue;
        };
        let full = r.name().as_bstr().to_str_lossy();
        let Some(tag) = full.strip_prefix("refs/tags/") else {
            continue;
        };
        let tag = tag.strip_suffix("^{}").unwrap_or(tag);
        let Ok(obj) = repo.rev_parse_single(tag.as_bytes()) else {
            continue;
        };
        let peeled = obj
            .object()
            .ok()
            .and_then(|o| o.peel_to_commit().ok())
            .map(|c| c.id().to_string())
            .unwrap_or_else(|| obj.to_string());
        if peeled != head_hex {
            continue;
        }
        if super::semver::parse_version_from_tag(tag).is_some() {
            match &semver_hit {
                None => semver_hit = Some(tag.to_string()),
                Some(prev) if !prev.starts_with('v') && tag.starts_with('v') => {
                    semver_hit = Some(tag.to_string());
                }
                _ => {}
            }
        } else {
            any_hit.get_or_insert_with(|| tag.to_string());
        }
    }
    semver_hit.or(any_hit)
}

/// 工作区是否有未提交变更（含未跟踪）。优先 `git status --porcelain`。
pub fn is_worktree_dirty(root: &Path) -> Result<bool, Box<dyn Error>> {
    let out = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(root)
        .output()
        .map_err(|e| format!("git status failed: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git status exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        )
        .into());
    }
    Ok(!out.stdout.is_empty())
}

pub fn tag_names_exist(root: &Path, names: &[&str]) -> Result<Vec<String>, Box<dyn Error>> {
    let repo = gix::open(root).map_err(|e| format!("open git repo {}: {e}", root.display()))?;
    let platform = repo.references()?;
    let mut found = Vec::new();
    for r in platform.all()? {
        let r = r.map_err(|e| -> Box<dyn Error> { e.to_string().into() })?;
        let full = r.name().as_bstr().to_str_lossy();
        let Some(tag) = full.strip_prefix("refs/tags/") else {
            continue;
        };
        let tag = tag.strip_suffix("^{}").unwrap_or(tag);
        if names.iter().any(|n| *n == tag) {
            found.push(tag.to_string());
        }
    }
    found.sort();
    found.dedup();
    Ok(found)
}

/// 最近提交摘要（信息性打印）。
pub fn recent_commits(root: &Path, limit: usize) -> Result<Vec<String>, Box<dyn Error>> {
    let out = Command::new("git")
        .args([
            "log",
            &format!("-{limit}"),
            "--oneline",
            "--decorate",
            "--no-color",
        ])
        .current_dir(root)
        .output()
        .map_err(|e| format!("git log failed: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git log exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

pub fn create_annotated_tag(root: &Path, tag: &str, message: &str) -> Result<(), Box<dyn Error>> {
    let out = Command::new("git")
        .args(["tag", "-a", tag, "-m", message])
        .current_dir(root)
        .output()
        .map_err(|e| format!("git tag failed: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git tag -a {tag} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }
    Ok(())
}

/// 若存在 `origin` remote 则 push 指定 tag；返回是否执行了 push。
pub fn push_tag_origin(root: &Path, tag: &str) -> Result<bool, Box<dyn Error>> {
    let remotes = Command::new("git")
        .args(["remote"])
        .current_dir(root)
        .output()
        .map_err(|e| format!("git remote failed: {e}"))?;
    if !remotes.status.success() {
        return Err(format!(
            "git remote failed: {}",
            String::from_utf8_lossy(&remotes.stderr).trim()
        )
        .into());
    }
    let has_origin = String::from_utf8_lossy(&remotes.stdout)
        .lines()
        .any(|l| l.trim() == "origin");
    if !has_origin {
        return Ok(false);
    }
    let out = Command::new("git")
        .args(["push", "origin", tag])
        .current_dir(root)
        .output()
        .map_err(|e| format!("git push failed: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git push origin {tag} failed: {}\nTag exists locally; push manually with: git push origin {tag}",
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }
    Ok(true)
}

pub fn origin_url(root: &Path) -> Option<String> {
    let out = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// 原始 `Optive.toml` 的 `[package]` 是否仍含已弃用的 `version` 键。
pub fn package_toml_has_legacy_version(root: &Path) -> bool {
    let path = root.join("Optive.toml");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return false;
    };
    let Ok(val) = text.parse::<toml::Value>() else {
        return false;
    };
    val.get("package")
        .and_then(|p| p.get("version"))
        .is_some()
}
