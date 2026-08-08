//! 依赖图解析与安装（ensure / update）。

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;

use rustc_hash::{FxHashMap, FxHashSet};

use super::cache::ProjectCache;
use super::git_ops;
use super::home;
use super::lock::{self, LockEdge, LockFile, ROOT_PARENT};
use super::manifest::{
    read_deps_if_exists, Dependency, Project, RevSpec,
};
use super::store::{self, Store};

/// 包身份：根或 content id。
pub type PackageId = String;

#[derive(Debug, Clone)]
pub struct ResolvedEdge {
    pub parent: PackageId,
    pub name: String,
    pub git: String,
    pub effective_rev: String,
    pub id: String,
    /// toml 意图：branch 名（可追 tip）
    pub branch: Option<String>,
    /// toml 意图：tag 名（effective_rev 为剥皮 SHA）
    pub tag: Option<String>,
    /// toml 意图：`rev =` commit pin
    pub pinned: bool,
}

#[derive(Debug, Default)]
pub struct ResolveReport {
    pub installed: Vec<String>,
    pub reused: Vec<String>,
    pub edges: Vec<ResolvedEdge>,
}

/// `(parent_id, dep_name) → 绑定`
#[derive(Debug, Clone)]
pub struct DepBinding {
    pub path: PathBuf,
    pub id: PackageId,
}

pub type DepMap = FxHashMap<(PackageId, String), DepBinding>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveMode {
    /// `run`：有一致 lock 则复现；否则按 toml+cache；lock 不一致则失败
    Run,
    /// `update`：忽略旧 lock 版本，追可追 tip，成功写 lock
    Update,
    /// 预览 update，不写盘
    DryRun,
}

pub struct EnsureOptions {
    pub mode: ResolveMode,
    /// `update <name>` 时只更新该根依赖子树；`None` = 全部
    pub only_root_dep: Option<String>,
}

impl Default for EnsureOptions {
    fn default() -> Self {
        Self {
            mode: ResolveMode::Run,
            only_root_dep: None,
        }
    }
}

#[derive(Debug)]
pub struct EnsureResult {
    pub report: ResolveReport,
    pub dep_map: DepMap,
    pub wrote_lock: bool,
}

/// 建图并确保 pack 落盘；按模式读写 lock/cache。
pub fn ensure_graph(
    project: &Project,
    opts: EnsureOptions,
) -> Result<EnsureResult, Box<dyn std::error::Error>> {
    let lock_path = project.lock_path();
    let cache_path = project.cache_path();
    let existing_lock = LockFile::load(&lock_path)?;
    let mut cache = ProjectCache::load(&cache_path);

    if opts.mode == ResolveMode::Run {
        if let Some(ref lock) = existing_lock {
            if !lock.matches_root_intent(&project.manifest) {
                return Err(
                    "Optive.lock is out of date with Optive.toml; run `Optive update` or `Optive up`"
                        .into(),
                );
            }
            return ensure_from_lock(project, lock, &mut cache);
        }
    }

    // Update / DryRun / Run-without-lock
    let force_fetch_tips = matches!(opts.mode, ResolveMode::Update | ResolveMode::DryRun)
        || project.manifest.track_latest;

    let mut report = ResolveReport::default();
    let mut binding: FxHashMap<(String, String), String> = FxHashMap::default();
    let mut dep_map: DepMap = FxHashMap::default();
    let mut queue: VecDeque<(String, String, Dependency)> = VecDeque::new();

    let dry = opts.mode == ResolveMode::DryRun;
    let mut store = if dry || home::use_local_deps() {
        None
    } else {
        Some(Store::open()?)
    };
    // LOCAL_DEPS：检测同名不同 id
    let mut local_name_ids: FxHashMap<String, String> = FxHashMap::default();

    for (name, dep) in &project.manifest.dependencies {
        if let Some(ref only) = opts.only_root_dep {
            if name != only {
                // update <name>：其它根依赖整棵子树按 lock 原样物化，不 tip-fetch。
                if opts.mode == ResolveMode::Update {
                    if let Some(ref lock) = existing_lock {
                        if let Some(edge) = lock
                            .edges
                            .iter()
                            .find(|e| e.parent == ROOT_PARENT && e.name == *name)
                        {
                            if !lock::dependency_matches_lock_edge(dep, edge) {
                                return Err(format!(
                                    "Optive.lock is out of date for dependency `{name}` (intent changed); run `Optive update` without a name filter, or `Optive up`"
                                )
                                .into());
                            }
                            materialize_lock_subtree(
                                project,
                                lock,
                                edge,
                                dry,
                                &mut store,
                                &mut cache,
                                &mut binding,
                                &mut dep_map,
                                &mut report,
                                &mut local_name_ids,
                            )?;
                            continue;
                        }
                    }
                    // 有 only 过滤器但无 lock / 无该边：仍按 toml 解析该根（进 queue）。
                }
            }
        }
        queue.push_back((ROOT_PARENT.to_string(), name.clone(), dep.clone()));
    }

    while let Some((parent, name, dep)) = queue.pop_front() {
        let effective_rev = resolve_effective_rev(
            &dep,
            force_fetch_tips,
            &cache,
            opts.mode,
        )?;

        let id = store::content_id(&dep.git, &effective_rev);
        let key = (parent.clone(), name.clone());
        if let Some(prev) = binding.get(&key) {
            if prev != &id {
                return Err(format!(
                    "dependency conflict: ({parent}, {name}) maps to both {prev} and {id}"
                )
                .into());
            }
            continue;
        }
        binding.insert(key.clone(), id.clone());

        let path = if dry {
            // dry-run 不落盘，但给出 pack「将会占用」的真实 CAS 路径（由 id 决定，无需克隆）。
            home::pack_dir().join(&id)
        } else if home::use_local_deps() {
            if let Some(prev) = local_name_ids.get(&name) {
                if prev != &id {
                    return Err(format!(
                        "OPTIVE_USE_LOCAL_DEPS cannot express two versions of `{name}`; unset the env and use CAS"
                    )
                    .into());
                }
            }
            local_name_ids.insert(name.clone(), id.clone());
            let (got_id, path, fresh) = store::ensure_local_pack(
                &project.deps_dir(),
                &name,
                &dep.git,
                &effective_rev,
            )?;
            debug_assert_eq!(got_id, id);
            if fresh {
                report.installed.push(name.clone());
            } else {
                report.reused.push(name.clone());
            }
            path
        } else {
            let st = store
                .as_mut()
                .expect("store initialized unless dry-run (theoretically unreachable)");
            let (got_id, path, fresh) = store::ensure_pack(st, &dep.git, &effective_rev)?;
            debug_assert_eq!(got_id, id);
            if fresh {
                report.installed.push(format!("{name}@{effective_rev}"));
            } else {
                report.reused.push(name.clone());
            }
            path
        };

        if !dry {
            // 仅 tip/branch 写入 tip 缓存；tag/commit pin 不得污染 tip 槽。
            if matches!(dep.rev, RevSpec::Branch(_) | RevSpec::None) {
                cache.put(
                    &dep.git,
                    dep.rev.branch_name(),
                    &effective_rev,
                    Some(&id),
                );
            }
        }

        dep_map.insert(
            (parent.clone(), name.clone()),
            DepBinding {
                path: path.clone(),
                id: id.clone(),
            },
        );
        report.edges.push(ResolvedEdge {
            parent: parent.clone(),
            name: name.clone(),
            git: dep.git.clone(),
            effective_rev: effective_rev.clone(),
            id: id.clone(),
            branch: match &dep.rev {
                RevSpec::Branch(b) => Some(b.clone()),
                _ => None,
            },
            tag: match &dep.rev {
                RevSpec::Tag(t) => Some(t.clone()),
                _ => None,
            },
            pinned: matches!(dep.rev, RevSpec::Commit(_)),
        });

        // 子依赖：dry-run 也展开（读已有 pack 上的清单），以便 -v 能预览传递边；不落盘。
        let children = read_deps_if_exists(&path)?;
        for (cname, cdep) in children {
            queue.push_back((id.clone(), cname, cdep));
        }
    }

    let mut wrote_lock = false;

    if !dry {
        let lock = LockFile::new(
            report
                .edges
                .iter()
                .map(|e| LockEdge {
                    parent: e.parent.clone(),
                    name: e.name.clone(),
                    git: e.git.clone(),
                    rev: e.effective_rev.clone(),
                    id: e.id.clone(),
                    branch: e.branch.clone(),
                    tag: e.tag.clone(),
                    pinned: e.pinned,
                })
                .collect(),
        );
        // run 无 lock 时也生成；update 总是写
        if opts.mode == ResolveMode::Update
            || opts.mode == ResolveMode::Run
            || existing_lock.is_some()
        {
            lock.save(&lock_path)?;
            wrote_lock = true;
        }
        cache.save(&cache_path)?;

        if let Some(ref mut st) = store {
            let ids: Vec<String> = report.edges.iter().map(|e| e.id.clone()).collect();
            let uniq: FxHashSet<String> = ids.into_iter().collect();
            let ids: Vec<String> = uniq.into_iter().collect();
            let key = project_key(project);
            st.set_project_refs(&key, &ids)?;
        }
    }

    Ok(EnsureResult {
        report,
        dep_map,
        wrote_lock,
    })
}

fn project_key(project: &Project) -> String {
    project.root.to_string_lossy().to_string()
}

fn ensure_from_lock(
    project: &Project,
    lock: &LockFile,
    cache: &mut ProjectCache,
) -> Result<EnsureResult, Box<dyn std::error::Error>> {
    let mut report = ResolveReport::default();
    let mut dep_map: DepMap = FxHashMap::default();
    let mut store = if home::use_local_deps() {
        None
    } else {
        Some(Store::open()?)
    };
    let mut local_name_ids: FxHashMap<String, String> = FxHashMap::default();

    for edge in &lock.edges {
        let computed = store::content_id(&edge.git, &edge.rev);
        if computed != edge.id {
            return Err(format!(
                "corrupt Optive.lock: edge {} id {} != content_id({})",
                edge.name, edge.id, computed
            )
            .into());
        }
        let path = if home::use_local_deps() {
            if let Some(prev) = local_name_ids.get(&edge.name) {
                if prev != &edge.id {
                    return Err(
                        "OPTIVE_USE_LOCAL_DEPS cannot express two versions; unset env".into(),
                    );
                }
            }
            local_name_ids.insert(edge.name.clone(), edge.id.clone());
            let (_, path, fresh) =
                store::ensure_local_pack(&project.deps_dir(), &edge.name, &edge.git, &edge.rev)?;
            if fresh {
                report.installed.push(edge.name.clone());
            } else {
                report.reused.push(edge.name.clone());
            }
            path
        } else {
            let st = store
                .as_mut()
                .expect("store initialized unless dry-run (theoretically unreachable)");
            let (_, path, fresh) = store::ensure_pack(st, &edge.git, &edge.rev)?;
            if fresh {
                report.installed.push(format!("{}@{}", edge.name, edge.rev));
            } else {
                report.reused.push(edge.name.clone());
            }
            path
        };
        cache.put(&edge.git, None, &edge.rev, Some(&edge.id));
        dep_map.insert(
            (edge.parent.clone(), edge.name.clone()),
            DepBinding {
                path: path.clone(),
                id: edge.id.clone(),
            },
        );
        report.edges.push(ResolvedEdge {
            parent: edge.parent.clone(),
            name: edge.name.clone(),
            git: edge.git.clone(),
            effective_rev: edge.rev.clone(),
            id: edge.id.clone(),
            branch: edge.branch.clone(),
            tag: edge.tag.clone(),
            pinned: edge.pinned,
        });
    }

    cache.save(&project.cache_path())?;
    if let Some(ref mut st) = store {
        let ids: FxHashSet<String> = lock.edges.iter().map(|e| e.id.clone()).collect();
        st.set_project_refs(&project_key(project), &ids.into_iter().collect::<Vec<_>>())?;
    }

    Ok(EnsureResult {
        report,
        dep_map,
        wrote_lock: false,
    })
}

/// 将 lock 中以 `root_edge` 为根的整棵子树按钉死 rev 物化，不 tip-fetch、不重读 pack toml。
#[allow(clippy::too_many_arguments)]
fn materialize_lock_subtree(
    project: &Project,
    lock: &LockFile,
    root_edge: &LockEdge,
    dry: bool,
    store: &mut Option<Store>,
    cache: &mut ProjectCache,
    binding: &mut FxHashMap<(String, String), String>,
    dep_map: &mut DepMap,
    report: &mut ResolveReport,
    local_name_ids: &mut FxHashMap<String, String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut q: VecDeque<&LockEdge> = VecDeque::new();
    q.push_back(root_edge);
    // 按 (parent, name) 去重，保留菱形依赖的多条入边。
    let mut seen_keys: FxHashSet<(String, String)> = FxHashSet::default();

    while let Some(edge) = q.pop_front() {
        let key = (edge.parent.clone(), edge.name.clone());
        if !seen_keys.insert(key.clone()) {
            continue;
        }
        let computed = store::content_id(&edge.git, &edge.rev);
        if computed != edge.id {
            return Err(format!(
                "corrupt Optive.lock: edge {} id {} != content_id({})",
                edge.name, edge.id, computed
            )
            .into());
        }
        if let Some(prev) = binding.get(&key) {
            if prev != &edge.id {
                return Err(format!(
                    "dependency conflict: ({}, {}) maps to both {prev} and {}",
                    edge.parent, edge.name, edge.id
                )
                .into());
            }
        } else {
            binding.insert(key.clone(), edge.id.clone());
        }

        let path = if dry {
            home::pack_dir().join(&edge.id)
        } else if home::use_local_deps() {
            if let Some(prev) = local_name_ids.get(&edge.name) {
                if prev != &edge.id {
                    return Err(
                        "OPTIVE_USE_LOCAL_DEPS cannot express two versions; unset env".into(),
                    );
                }
            }
            local_name_ids.insert(edge.name.clone(), edge.id.clone());
            let (_, path, fresh) =
                store::ensure_local_pack(&project.deps_dir(), &edge.name, &edge.git, &edge.rev)?;
            if fresh {
                report.installed.push(edge.name.clone());
            } else {
                report.reused.push(edge.name.clone());
            }
            path
        } else {
            let st = store
                .as_mut()
                .expect("store initialized unless dry-run (theoretically unreachable)");
            let (_, path, fresh) = store::ensure_pack(st, &edge.git, &edge.rev)?;
            if fresh {
                report.installed.push(format!("{}@{}", edge.name, edge.rev));
            } else {
                report.reused.push(edge.name.clone());
            }
            path
        };

        if !dry && !edge.pinned && edge.tag.is_none() {
            cache.put(
                &edge.git,
                edge.branch.as_deref(),
                &edge.rev,
                Some(&edge.id),
            );
        }
        dep_map.insert(
            (edge.parent.clone(), edge.name.clone()),
            DepBinding {
                path,
                id: edge.id.clone(),
            },
        );
        report.edges.push(ResolvedEdge {
            parent: edge.parent.clone(),
            name: edge.name.clone(),
            git: edge.git.clone(),
            effective_rev: edge.rev.clone(),
            id: edge.id.clone(),
            branch: edge.branch.clone(),
            tag: edge.tag.clone(),
            pinned: edge.pinned,
        });

        for child in lock.edges.iter().filter(|e| e.parent == edge.id) {
            q.push_back(child);
        }
    }
    Ok(())
}

fn resolve_effective_rev(
    dep: &Dependency,
    force_fetch_tips: bool,
    cache: &ProjectCache,
    mode: ResolveMode,
) -> Result<String, Box<dyn std::error::Error>> {
    match &dep.rev {
        RevSpec::Commit(r) => Ok(r.clone()),
        // tag → 剥皮为 commit SHA，保证 CAS id / lock 可复现。
        RevSpec::Tag(t) => git_ops::resolve_tag_commit(&dep.git, t),
        RevSpec::Branch(b) => {
            if force_fetch_tips || mode == ResolveMode::DryRun {
                git_ops::resolve_remote_tip(&dep.git, Some(b))
            } else if let Some(c) = cache.get_commit(&dep.git, Some(b)) {
                Ok(c.to_string())
            } else {
                let tip = git_ops::resolve_remote_tip(&dep.git, Some(b))?;
                Ok(tip)
            }
        }
        RevSpec::None => {
            if force_fetch_tips || mode == ResolveMode::DryRun {
                git_ops::resolve_remote_tip(&dep.git, None)
            } else if let Some(c) = cache.get_commit(&dep.git, None) {
                Ok(c.to_string())
            } else {
                git_ops::resolve_remote_tip(&dep.git, None)
            }
        }
    }
}

/// 预览 update 变更（根依赖）。
pub fn dry_run_summary(
    project: &Project,
    verbose: bool,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let old = LockFile::load(&project.lock_path())?;
    let result = ensure_graph(
        project,
        EnsureOptions {
            mode: ResolveMode::DryRun,
            only_root_dep: None,
        },
    )?;
    let mut lines = Vec::new();
    let old_root: BTreeMap<String, String> = old
        .as_ref()
        .map(|l| {
            l.root_edges()
                .map(|e| (e.name.clone(), e.rev.clone()))
                .collect()
        })
        .unwrap_or_default();

    for e in &result.report.edges {
        if e.parent != ROOT_PARENT && !verbose {
            continue;
        }
        let prev = old_root.get(&e.name).map(|s| s.as_str()).unwrap_or("(none)");
        if e.parent == ROOT_PARENT {
            if prev != e.effective_rev.as_str() {
                lines.push(format!(
                    "{}: {prev} -> {}",
                    e.name, e.effective_rev
                ));
            } else {
                lines.push(format!("{}: {} (unchanged)", e.name, e.effective_rev));
            }
        } else if verbose {
            lines.push(format!(
                "  {} -> {} @ {}",
                e.parent, e.name, e.effective_rev
            ));
        }
    }
    if lines.is_empty() {
        lines.push("(no changes)".into());
    }
    Ok(lines)
}
