//! `optive add` / `remove` / `change`。

use super::deps;
use super::git_ops;
use super::manifest::{self, Dependency, Project, RevSpec};
use super::resolve::{EnsureOptions, ResolveMode};

pub struct AddOptions {
    pub url: String,
    pub name: Option<String>,
    pub branch: Option<String>,
    pub tag: Option<String>,
}

pub fn cmd_add(project: &Project, opts: AddOptions) -> Result<String, Box<dyn std::error::Error>> {
    let name = match opts.name {
        Some(n) => {
            git_ops::validate_dep_dir_name_pub(&n)?;
            n
        }
        None => git_ops::repo_name_from_url(&opts.url)?,
    };
    if opts.branch.is_some() && opts.tag.is_some() {
        return Err("--branch and --tag are mutually exclusive".into());
    }

    let dep = if let Some(tag) = opts.tag {
        Dependency::with_tag(&opts.url, tag)
    } else if let Some(branch) = opts.branch {
        Dependency::with_branch(&opts.url, branch)
    } else {
        // 默认钉死 tip commit
        let tip = git_ops::resolve_remote_tip(&opts.url, None)?;
        Dependency::pinned_commit(&opts.url, tip)
    };

    manifest::upsert_dependency(&project.manifest_path, &name, &dep)?;
    // 重新加载后 ensure
    let project = manifest::load_project(&project.manifest_path)?;
    let _ = deps::ensure_for_update(&project, None)?;
    let rev_desc = match &dep.rev {
        RevSpec::Commit(r) => format!("rev={r}"),
        RevSpec::Tag(t) => format!("tag={t}"),
        RevSpec::Branch(b) => format!("branch={b}"),
        RevSpec::None => "tip".into(),
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
    eprintln!("WARNING: track_latest={value} makes `Optive run` follow remote tips for trackable deps.");
    eprintln!("WARNING: Do not enable this in CI if you need reproducible builds; prefer Optive.lock.");
    manifest::set_track_latest(&project.manifest_path, value)?;
    Ok(format!("track_latest set to {value}"))
}
