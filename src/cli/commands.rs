//! `optive add` / `remove` / `change`。

use super::deps;
use super::git_ops;
use super::manifest::{self, Dependency, Project, RevSpec};
use super::registry;
use super::resolve::{EnsureOptions, ResolveMode};

pub struct AddOptions {
    /// 位置参数：git URL，或 `name` / `name@version`。
    pub target: String,
    pub name: Option<String>,
    pub branch: Option<String>,
    pub tag: Option<String>,
}

/// 解析 `name` 或 `name@version`（第一个 `@` 分隔）。
pub fn parse_pack_spec(spec: &str) -> Result<(String, Option<String>), Box<dyn std::error::Error>> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Err("empty pack name".into());
    }
    if let Some((name, ver)) = spec.split_once('@') {
        let name = name.trim();
        let ver = ver.trim();
        if name.is_empty() {
            return Err("pack name before `@` is empty".into());
        }
        if ver.is_empty() {
            return Err("version after `@` is empty (use `name` alone for latest tag)".into());
        }
        git_ops::validate_dep_dir_name_pub(name)?;
        Ok((name.to_string(), Some(ver.to_string())))
    } else {
        git_ops::validate_dep_dir_name_pub(spec)?;
        Ok((spec.to_string(), None))
    }
}

pub fn cmd_add(project: &Project, opts: AddOptions) -> Result<String, Box<dyn std::error::Error>> {
    if opts.branch.is_some() && opts.tag.is_some() {
        return Err("--branch and --tag are mutually exclusive".into());
    }

    let (name, dep) = if git_ops::looks_like_git_url(&opts.target) {
        let url = opts.target;
        let name = match opts.name {
            Some(n) => {
                git_ops::validate_dep_dir_name_pub(&n)?;
                n
            }
            None => git_ops::repo_name_from_url(&url)?,
        };
        let dep = if let Some(tag) = opts.tag {
            Dependency::with_tag(&url, tag)
        } else if let Some(branch) = opts.branch {
            Dependency::with_branch(&url, branch)
        } else {
            let tip = git_ops::resolve_remote_tip(&url, None)?;
            Dependency::pinned_commit(&url, tip)
        };
        (name, dep)
    } else {
        if opts.branch.is_some() || opts.tag.is_some() {
            return Err(
                "pack-name add does not accept --branch/--tag; use `name@version` or a git URL"
                    .into(),
            );
        }
        if opts.name.is_some() {
            return Err(
                "pack-name add does not accept --name; the pack name is the dependency key".into(),
            );
        }
        let (pack_name, ver_opt) = parse_pack_spec(&opts.target)?;
        // 确认索引里有该包（resolve 时也会查；这里提前给出清晰错误）。
        let git_url = registry::lookup_pack_url(&pack_name)?;
        let version = match ver_opt {
            Some(v) => {
                // 校验约束语法；真正选 tag 在 ensure 时做。
                let _ = super::semver::parse_req(&v)
                    .map_err(|e| format!("invalid version constraint `{v}`: {e}"))?;
                v
            }
            None => git_ops::latest_semver_version(&git_url)?,
        };
        (pack_name, Dependency::from_index_version(version))
    };

    manifest::upsert_dependency(&project.manifest_path, &name, &dep)?;
    let project = manifest::load_project(&project.manifest_path)?;
    let _ = deps::ensure_for_update(&project, None)?;
    let rev_desc = match &dep.rev {
        RevSpec::Commit(r) => format!("rev={r}"),
        RevSpec::Tag(t) => format!("tag={t}"),
        RevSpec::Branch(b) => format!("branch={b}"),
        RevSpec::None => "tip".into(),
        RevSpec::IndexVersion(v) => format!("index {v}"),
    };
    Ok(format!("added {name} ({rev_desc})"))
}

pub fn cmd_remove(project: &Project, name: &str) -> Result<String, Box<dyn std::error::Error>> {
    let removed = manifest::remove_dependency(&project.manifest_path, name)?;
    if !removed {
        return Err(format!("dependency `{name}` not found in Optive.toml").into());
    }
    let project = manifest::load_project(&project.manifest_path)?;
    let _ = super::resolve::ensure_graph(
        &project,
        EnsureOptions {
            mode: ResolveMode::Update,
            only_root_dep: None,
        },
    )?;
    Ok(format!("removed {name}"))
}

pub fn cmd_change_track_latest(
    project: &Project,
    value: bool,
) -> Result<String, Box<dyn std::error::Error>> {
    eprintln!(
        "WARNING: track_latest={value} makes `Optive run` follow remote tips for trackable deps."
    );
    eprintln!(
        "WARNING: Do not enable this in CI if you need reproducible builds; prefer Optive.lock."
    );
    manifest::set_track_latest(&project.manifest_path, value)?;
    Ok(format!("track_latest set to {value}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pack_spec_name_only() {
        let (n, v) = parse_pack_spec("selfoptive").unwrap();
        assert_eq!(n, "selfoptive");
        assert!(v.is_none());
    }

    #[test]
    fn parse_pack_spec_with_version() {
        let (n, v) = parse_pack_spec("selfoptive@^0.2").unwrap();
        assert_eq!(n, "selfoptive");
        assert_eq!(v.as_deref(), Some("^0.2"));
    }

    #[test]
    fn parse_pack_spec_rejects_empty_version() {
        let err = parse_pack_spec("foo@").unwrap_err().to_string();
        assert!(err.contains("empty"), "{err}");
    }
}
