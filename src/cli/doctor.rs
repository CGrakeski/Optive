//! `optive deps` / `deps doctor` / `cache gc` / `env`。

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use super::color;
use super::home;
use super::lock::{LockEdge, LockFile, ROOT_PARENT};
use super::manifest::{find_project, Project, RevSpec};
use super::store::Store;

pub fn print_env() {
    let home_path = home::optive_home();
    println!("OPTIVE_HOME (effective): {}", home_path.display());
    if let Ok(v) = std::env::var("OPTIVE_HOME") {
        println!("OPTIVE_HOME (env):       {v}");
    } else {
        println!("OPTIVE_HOME (env):       (unset, using default)");
    }
    println!("pack/:                   {}", home::pack_dir().display());
    println!("custom/:                 {}", home::custom_dir().display());
    println!(
        "Config.toml:             {}",
        home::global_config_path().display()
    );
    println!(
        "index.db:                {}",
        home::index_db_path().display()
    );
    println!(
        "index dir:               {}",
        super::registry::index_dir().display()
    );
    let index_json = super::registry::index_json_path();
    println!(
        "index.json:               {} ({})",
        index_json.display(),
        if index_json.is_file() {
            "present"
        } else {
            "missing"
        }
    );
    if let Some(origin) = super::git_ops::origin_fetch_url(&super::registry::index_dir()) {
        println!("index checkout origin:   {origin}");
    }
    println!(
        "index.url file:          {}",
        super::main_index::index_url_config_path().display()
    );
    match super::main_index::resolve_index_url() {
        Ok((u, src)) => {
            println!("index remote:            {u}");
            println!("index remote source:     {}", src.label());
        }
        Err(e) => println!("index remote:            (error: {e})"),
    }
    if let Ok(v) = std::env::var("OPTIVE_INDEX_URL") {
        println!("OPTIVE_INDEX_URL (env):  {v}");
    }
    println!(
        "index trust:             {}",
        super::index_trust::describe_policy()
    );
    println!(
        "OPTIVE_USE_LOCAL_DEPS:   {}",
        if home::use_local_deps() { "1" } else { "0" }
    );
    println!(
        "bytecode cache:          {}",
        optive::bc_cache::cache_dir().display()
    );
    if let Ok(v) = std::env::var("OPTIVE_BC_DIR") {
        println!("OPTIVE_BC_DIR (env):     {v}");
    }
    if let Ok(v) = std::env::var("OPTIVE_BC_CACHE") {
        println!("OPTIVE_BC_CACHE (env):   {v}");
    }
}

pub fn doctor(verbose: bool) -> Result<i32, Box<dyn std::error::Error>> {
    print_env();
    println!();

    let mut errors = 0;
    let mut warnings = 0;

    let project = match find_project(None) {
        Ok(p) => Some(p),
        Err(e) => {
            println!("project: (none) — {e}");
            None
        }
    };

    if let Some(ref project) = project {
        doctor_project(project, verbose, &mut errors, &mut warnings)?;
    }

    // 孤儿 pack
    if !home::use_local_deps() {
        if let Ok(store) = Store::open() {
            let orphans = store.list_orphans()?;
            let mut total: u64 = 0;
            for o in &orphans {
                total += dir_size(&o.path);
            }
            if orphans.is_empty() {
                println!("orphans: 0");
            } else {
                warnings += 1;
                println!(
                    "orphans: {} packages ({})",
                    orphans.len(),
                    format_size(total)
                );
                println!(
                    "  hint: Optive cache gc --dry-run  (preview)  or  Optive cache gc  (clean)"
                );
                if verbose {
                    for o in &orphans {
                        println!(
                            "  - {} {} ({})",
                            o.id,
                            o.path.display(),
                            format_size(dir_size(&o.path))
                        );
                    }
                }
            }
        }
    }

    if errors > 0 {
        Ok(1)
    } else {
        // warnings 不影响退出码（仅作提示）；与 errors 分开以保留语义可读性。
        let _ = warnings;
        Ok(0)
    }
}

fn doctor_project(
    project: &Project,
    verbose: bool,
    errors: &mut i32,
    warnings: &mut i32,
) -> Result<(), Box<dyn std::error::Error>> {
    let ver_label =
        super::repo_meta::project_version_label(&project.root).unwrap_or_else(|| "(no git)".into());
    println!(
        "project: {} {} ({})",
        project.manifest.package.name,
        ver_label,
        project.root.display()
    );

    if super::repo_meta::package_toml_has_legacy_version(&project.root) {
        *errors += 1;
        println!(
            "error: [package].version in Optive.toml is forbidden (tag-only); remove it and use `Optive publish <version>`"
        );
    }

    match super::repo_meta::head_exact_tag(&project.root) {
        Some(tag) => println!("git tag at HEAD: {tag}"),
        None => {
            *warnings += 1;
            println!("git tag at HEAD: (none) — release with `Optive publish <version>`");
        }
    }

    match super::repo_meta::is_worktree_dirty(&project.root) {
        Ok(true) => {
            *warnings += 1;
            println!("worktree: dirty (publish requires a clean worktree)");
        }
        Ok(false) => println!("worktree: clean"),
        Err(e) => {
            if verbose {
                println!("worktree: (could not check: {e})");
            }
        }
    }

    if let Some(lock) = LockFile::load(&project.lock_path())? {
        if lock.matches_root_intent(&project.manifest) {
            println!("lock: ok (matches Optive.toml)");
        } else {
            *errors += 1;
            println!("lock: OUT OF DATE — run `Optive update` or `Optive up`");
        }
        // 完整性：pack 是否存在
        if !home::use_local_deps() {
            let store = Store::open()?;
            for e in &lock.edges {
                let path = store.pack_abs(&e.package_id);
                if !path.is_dir() {
                    if let Some(rec) = store.lookup(&e.package_id)? {
                        if !rec.path.is_dir() {
                            *errors += 1;
                            println!("missing pack: {} ({})", e.name, e.package_id);
                        }
                    } else {
                        *errors += 1;
                        println!("missing pack index entry: {} ({})", e.name, e.package_id);
                    }
                } else if verbose {
                    println!("  pack ok: {} -> {}", e.name, path.display());
                }
            }
        }
    } else {
        println!("lock: (none)");
    }

    if !project.manifest.dependencies.is_empty() {
        *warnings += 1;
        println!(
            "sandbox: {} project dependenc{} default to read-only package roots without network/env/FFI; pass --trust-deps to grant host capabilities",
            project.manifest.dependencies.len(),
            if project.manifest.dependencies.len() == 1 { "y" } else { "ies" }
        );
    }
    Ok(())
}

pub fn cache_gc(dry_run: bool) -> Result<(), Box<dyn std::error::Error>> {
    let mut store = Store::open()?;
    let orphans = store.list_orphans()?;
    if orphans.is_empty() {
        println!("No orphan packs.");
        return Ok(());
    }
    let mut total: u64 = 0;
    for o in &orphans {
        let sz = dir_size(&o.path);
        total += sz;
        println!(
            "{}  {}  {}",
            if dry_run { "would remove" } else { "remove" },
            o.id,
            format_size(sz)
        );
        if !dry_run {
            store.delete_pack(&o.id)?;
        }
    }
    println!(
        "{} {} orphan(s), {}",
        if dry_run { "Would free" } else { "Freed" },
        orphans.len(),
        format_size(total)
    );
    Ok(())
}

fn dir_size(path: &Path) -> u64 {
    fn walk(p: &Path) -> u64 {
        let mut n = 0u64;
        let Ok(rd) = fs::read_dir(p) else {
            return 0;
        };
        for e in rd.flatten() {
            let path = e.path();
            if path.is_dir() {
                n += walk(&path);
            } else if let Ok(m) = e.metadata() {
                n += m.len();
            }
        }
        n
    }
    walk(path)
}

fn format_size(n: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if n >= GB {
        format!("{:.1}GB", n as f64 / GB as f64)
    } else if n >= MB {
        format!("{:.1}MB", n as f64 / MB as f64)
    } else if n >= KB {
        format!("{:.1}KB", n as f64 / KB as f64)
    } else {
        format!("{n}B")
    }
}

/// `Optive deps`：简洁列出当前项目依赖。
pub fn list_deps(verbose: bool) -> Result<(), Box<dyn std::error::Error>> {
    let project = find_project(None)?;
    let lock = LockFile::load(&project.lock_path())?;
    let lock_ok = lock
        .as_ref()
        .is_some_and(|l| l.matches_root_intent(&project.manifest));

    let n = project.manifest.dependencies.len();
    let title = format!(
        "{}  ·  {} direct {}",
        project.manifest.package.name,
        n,
        if n == 1 { "dependency" } else { "dependencies" }
    );
    println!("{}", color::green(&title));

    let lock_note = match (&lock, lock_ok) {
        (None, _) => color::dim("lock: none"),
        (Some(_), true) => color::dim("lock: ok"),
        (Some(_), false) => color::red("lock: stale — run Optive update"),
    };
    println!("  {lock_note}");
    println!();

    if n == 0 {
        println!("{}", color::dim("  (no dependencies)"));
        return Ok(());
    }

    let store = if home::use_local_deps() {
        None
    } else {
        Store::open().ok()
    };

    let root_edges: BTreeMap<&str, &LockEdge> = lock
        .as_ref()
        .map(|l| l.root_edges().map(|e| (e.name.as_str(), e)).collect())
        .unwrap_or_default();

    for (name, dep) in &project.manifest.dependencies {
        let edge = root_edges.get(name.as_str()).copied();
        let (path, present) = resolve_dep_path(&project, name, edge, store.as_ref());
        let effective = edge.map_or_else(|| "—".into(), |e| short_rev(&e.commit));
        let mode = rev_mode_label(&dep.rev);
        let path_disp = match (&path, present) {
            (Some(p), true) => display_path(p),
            (Some(p), false) => format!("{} {}", color::red("missing"), display_path(p)),
            (None, _) => color::red("not installed"),
        };

        println!("  {}  {}", color::purple(name), color::cyan(&effective));
        println!("    {}  {}", color::dim("mode"), color::dim(&mode));
        println!("    {}  {}", color::dim("git"), dep.git);
        println!("    {}  {}", color::dim("path"), path_disp);
        if verbose {
            if let Some(e) = edge {
                println!("    {}  {}", color::dim("id"), short_id(&e.package_id));
            }
        }
        println!();
    }

    if verbose {
        if let Some(lock) = &lock {
            let transitive: Vec<_> = lock
                .edges
                .iter()
                .filter(|e| e.parent != ROOT_PARENT)
                .collect();
            if !transitive.is_empty() {
                println!("{}", color::dim("Transitive"));
                for e in transitive {
                    let parent = short_id(&e.parent);
                    println!(
                        "  {} → {}  {}  {}",
                        color::dim(&parent),
                        color::purple(&e.name),
                        short_rev(&e.commit),
                        color::dim(&short_git(&e.source))
                    );
                }
                println!();
            }
        }
    }

    Ok(())
}

fn rev_mode_label(rev: &RevSpec) -> String {
    match rev {
        RevSpec::Commit(_) => "pinned".into(),
        RevSpec::Tag(t) => format!("tag {t}"),
        RevSpec::Branch(b) => format!("branch {b}"),
        RevSpec::None => "tip (trackable)".into(),
        RevSpec::IndexVersion(v) => format!("index {v}"),
    }
}

fn short_rev(rev: &str) -> String {
    if rev.len() >= 8 && rev.chars().all(|c| c.is_ascii_hexdigit()) {
        rev[..8].to_string()
    } else {
        rev.to_string()
    }
}

fn short_id(id: &str) -> String {
    if id.len() > 12 {
        format!("{}...", &id[..12])
    } else {
        id.to_string()
    }
}

fn short_git(git: &str) -> String {
    if git.len() > 48 {
        format!("{}...", &git[..45])
    } else {
        git.to_string()
    }
}

fn display_path(path: &Path) -> String {
    let mut s = path.to_string_lossy().into_owned();
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        s = rest.to_string();
    }
    s
}

/// 返回 (路径, 是否已落盘)。有 lock id 时总是给出预期 pack 路径。
fn resolve_dep_path(
    project: &Project,
    name: &str,
    edge: Option<&LockEdge>,
    store: Option<&Store>,
) -> (Option<PathBuf>, bool) {
    if home::use_local_deps() {
        let p = project.deps_dir().join(name);
        let ok = p.is_dir();
        return (Some(p), ok);
    }
    let Some(id) = edge.map(|e| e.package_id.as_str()) else {
        return (None, false);
    };
    if let Some(store) = store {
        if let Ok(Some(rec)) = store.lookup(id) {
            let ok = rec.path.is_dir();
            return (Some(rec.path), ok);
        }
        let p = store.pack_abs(id);
        let ok = p.is_dir();
        return (Some(p), ok);
    }
    (None, false)
}
