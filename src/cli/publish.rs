//! `Optive publish <version>` — annotated git tag (+ optional origin push / index insert).

use std::error::Error;
use std::fs;
use std::path::Path;

use super::git_ops;
use super::manifest::find_project;
use super::registry;
use super::repo_meta;
use super::semver;

pub fn publish(version_arg: &str) -> Result<(), Box<dyn Error>> {
    let project = find_project(None)?;
    let root = &project.root;
    let name = &project.manifest.package.name;

    if gix::open(root).is_err() {
        return Err(format!(
            "{} is not a git repository.\n\
             Initialize and commit first, then publish:\n\
               git init\n\
               git add .\n\
               git commit -m \"initial\"\n\
               Optive publish {version_arg}",
            root.display()
        )
        .into());
    }

    let ver = semver::parse_version(version_arg)
        .map_err(|e| format!("invalid version `{version_arg}`: {e}"))?;
    let tag = format!("v{ver}");
    let bare = ver.to_string();

    if repo_meta::is_worktree_dirty(root)? {
        return Err("worktree is dirty; commit or stash changes before publish".into());
    }

    let existing = repo_meta::tag_names_exist(root, &[&tag, &bare])?;
    if !existing.is_empty() {
        return Err(format!(
            "tag already exists: {}; refuse to overwrite",
            existing.join(", ")
        )
        .into());
    }

    if repo_meta::head_exact_tag(root).is_none() {
        println!("No tag at current commit.");
        println!("Recent commits:");
        match repo_meta::recent_commits(root, 5) {
            Ok(lines) if !lines.is_empty() => {
                for line in lines {
                    println!("  {line}");
                }
            }
            Ok(_) => println!("  (none)"),
            Err(e) => println!("  (could not list: {e})"),
        }
        println!();
    }

    let msg = format!("Release {tag}");
    repo_meta::create_annotated_tag(root, &tag, &msg)?;
    println!("Created annotated tag {tag}");

    match repo_meta::push_tag_origin(root, &tag) {
        Ok(true) => println!("Pushed {tag} to origin"),
        Ok(false) => {
            println!("No origin remote; tag is local only");
        }
        Err(e) => return Err(e),
    }

    let sha = repo_meta::short_head_sha(root).unwrap_or_else(|| "?".into());
    println!("Published {name} {tag} at {sha}");
    match maybe_insert_index(name, root) {
        Ok(Some(note)) => println!("{note}"),
        Ok(None) => {}
        Err(e) => println!("index: warning: {e}"),
    }
    Ok(())
}

fn maybe_insert_index(name: &str, root: &Path) -> Result<Option<String>, Box<dyn Error>> {
    let path = registry::index_json_path();
    if !path.is_file() {
        return Ok(Some(format!(
            "index: skipped (no {}); run `Optive index change` / `index sync` to register packs",
            path.display()
        )));
    }
    let mut map = registry::load_pack_index()?;
    let url = repo_meta::origin_url(root).unwrap_or_else(|| path_as_file_url(root));
    if let Some(existing) = map.get(name) {
        if git_ops::git_remotes_equivalent(existing, &url) {
            return Ok(Some(format!(
                "index: `{name}` already registered in {}",
                path.display()
            )));
        }
        let previous = existing.clone();
        map.insert(name.to_string(), url.clone());
        let text = serde_json::to_string_pretty(&map)?;
        fs::write(&path, format!("{text}\n"))
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        return Ok(Some(format!(
            "index: updated `{name}` → {url} (was {previous}) in {}\n  commit/push the index repo if it is shared",
            path.display()
        )));
    }
    map.insert(name.to_string(), url.clone());
    let text = serde_json::to_string_pretty(&map)?;
    fs::write(&path, format!("{text}\n"))
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(Some(format!(
        "index: added `{name}` → {url} in {}\n  commit/push the index repo if it is shared",
        path.display()
    )))
}

fn path_as_file_url(path: &Path) -> String {
    let abs = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let s = abs.to_string_lossy();
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s);
    let unixy = s.replace('\\', "/");
    if unixy.starts_with('/') {
        format!("file://{unixy}")
    } else {
        format!("file:///{unixy}")
    }
}
