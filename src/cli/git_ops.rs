use gix::index::File;
use gix::progress::{Count, Id, MessageLevel, NestedProgress, Progress, Step, StepShared, Unit};
use gix::worktree::state::checkout::Options;
use gix::Repository;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::error::Error;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use std::{fs, io, thread};

/// 将 gix/`prodash` 的分层进度桥接到 `indicatif` 进度条。
struct IndicatifProgress {
    multi: MultiProgress,
    bar: ProgressBar,
    name: String,
    id: Id,
    step: StepShared,
    max: Option<Step>,
    unit: Option<Unit>,
    /// 保持后台位置同步线程运行，直至本节点被 Drop。
    alive: Arc<AtomicBool>,
}

impl IndicatifProgress {
    fn new(title: impl Into<String>) -> Self {
        let multi = MultiProgress::new();
        let name = title.into();
        let bar = multi.add(ProgressBar::new_spinner());
        bar.set_style(spinner_style());
        bar.set_message(name.clone());
        bar.enable_steady_tick(Duration::from_millis(80));

        let step = Arc::new(AtomicUsize::new(0));
        let alive = Arc::new(AtomicBool::new(true));
        spawn_syncer(&bar, &step, &alive);

        Self {
            multi,
            bar,
            name,
            id: gix::progress::UNKNOWN,
            step,
            max: None,
            unit: None,
            alive,
        }
    }

    fn child(&self, name: impl Into<String>, id: Id) -> Self {
        let name = name.into();
        let bar = self.multi.add(ProgressBar::new_spinner());
        bar.set_style(spinner_style());
        bar.set_message(name.clone());
        bar.enable_steady_tick(Duration::from_millis(80));

        let step = Arc::new(AtomicUsize::new(0));
        let alive = Arc::new(AtomicBool::new(true));
        spawn_syncer(&bar, &step, &alive);

        Self {
            multi: self.multi.clone(),
            bar,
            name,
            id,
            step,
            max: None,
            unit: None,
            alive,
        }
    }

    fn apply_style(&self) {
        if let Some(max) = self.max {
            self.bar.set_length(max as u64);
            let style = if unit_is_bytes(&self.unit) {
                ProgressStyle::with_template(
                    "{spinner:.green} {msg} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, eta {eta})",
                )
                .unwrap_or_else(|_| ProgressStyle::default_bar())
                .progress_chars("=>-")
            } else {
                ProgressStyle::with_template(
                    "{spinner:.green} {msg} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})",
                )
                .unwrap_or_else(|_| ProgressStyle::default_bar())
                .progress_chars("=>-")
            };
            self.bar.set_style(style);
        } else {
            self.bar.unset_length();
            self.bar.set_style(spinner_style());
        }
        self.bar.set_message(self.name.clone());
    }

    fn sync_bar(&self) {
        self.bar
            .set_position(self.step.load(Ordering::Relaxed) as u64);
    }
}

impl Drop for IndicatifProgress {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Relaxed);
        if !self.bar.is_finished() {
            self.bar.finish_and_clear();
        }
    }
}

impl Count for IndicatifProgress {
    fn set(&self, step: Step) {
        self.step.store(step, Ordering::SeqCst);
        self.sync_bar();
    }

    fn step(&self) -> Step {
        self.step.load(Ordering::Relaxed)
    }

    fn inc_by(&self, step: Step) {
        self.step.fetch_add(step, Ordering::Relaxed);
        self.sync_bar();
    }

    fn counter(&self) -> StepShared {
        Arc::clone(&self.step)
    }
}

impl Progress for IndicatifProgress {
    fn init(&mut self, max: Option<Step>, unit: Option<Unit>) {
        self.max = max;
        self.unit = unit;
        self.step.store(0, Ordering::Relaxed);
        self.apply_style();
        self.sync_bar();
    }

    fn unit(&self) -> Option<Unit> {
        self.unit.clone()
    }

    fn max(&self) -> Option<Step> {
        self.max
    }

    fn set_max(&mut self, max: Option<Step>) -> Option<Step> {
        let prev = self.max;
        self.max = max;
        self.apply_style();
        prev
    }

    fn set_name(&mut self, name: String) {
        self.name = name;
        self.bar.set_message(self.name.clone());
    }

    fn name(&self) -> Option<String> {
        Some(self.name.clone())
    }

    fn id(&self) -> Id {
        self.id
    }

    fn message(&self, level: MessageLevel, message: String) {
        let prefix = match level {
            MessageLevel::Info => "info",
            MessageLevel::Success => "done",
            MessageLevel::Failure => "fail",
        };
        let _ = self.multi.println(format!("[{prefix}] {message}"));
    }
}

impl NestedProgress for IndicatifProgress {
    type SubProgress = Self;

    fn add_child(&mut self, name: impl Into<String>) -> Self::SubProgress {
        self.add_child_with_id(name, gix::progress::UNKNOWN)
    }

    fn add_child_with_id(&mut self, name: impl Into<String>, id: Id) -> Self::SubProgress {
        self.child(name, id)
    }
}

fn spinner_style() -> ProgressStyle {
    ProgressStyle::with_template("{spinner:.green} {msg} [{elapsed_precise}]")
        .unwrap_or_else(|_| ProgressStyle::default_spinner())
        .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ ")
}

fn unit_is_bytes(unit: &Option<Unit>) -> bool {
    unit.as_ref().is_some_and(|u| {
        let mut buf = String::new();
        let _ = u.as_display_value().display_unit(&mut buf, 0);
        buf == "B" || buf.eq_ignore_ascii_case("bytes")
    })
}

/// 从原子计数器同步进度条（gix 通过 `counter()` 直接更新时使用）。
fn spawn_syncer(bar: &ProgressBar, step: &StepShared, alive: &Arc<AtomicBool>) {
    let bar = bar.clone();
    let step = Arc::clone(step);
    let alive = Arc::clone(alive);
    thread::spawn(move || {
        while alive.load(Ordering::Relaxed) {
            bar.set_position(step.load(Ordering::Relaxed) as u64);
            thread::sleep(Duration::from_millis(100));
        }
    });
}

/// 从 Git URL 中提取仓库名（支持 HTTPS 和 SSH 格式）。
/// 示例：
/// - "<https://github.com/user/repo.git>" -> "repo"
/// - "git@github.com:user/repo.git"   -> "repo"
/// - "<https://gitlab.com/group/sub/repo>" -> "repo"
///
/// 仅允许单段安全目录名（字母数字、`.`、`_`、`-`），拒绝路径穿越。
fn extract_repo_name(url: &str) -> Result<String, Box<dyn Error>> {
    let trimmed = url.trim_end_matches(".git");
    let name = if let Some(last_slash) = trimmed.rfind('/') {
        &trimmed[last_slash + 1..]
    } else if let Some(last_colon) = trimmed.rfind(':') {
        // 处理 SSH 风格：git@host:user/repo
        &trimmed[last_colon + 1..]
    } else {
        trimmed
    };
    let name = name.trim();
    if is_invalid_repo_name(name) {
        return Err(format!("invalid repository name derived from URL: {name:?}").into());
    }
    Ok(name.to_string())
}

fn is_invalid_repo_name(name: &str) -> bool {
    name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
}

/// 是否像 Git 远程 URL（`https://` / `file://` / `git@host:path` 等），而不是版本号。
pub fn looks_like_git_url(url: &str) -> bool {
    let u = url.trim();
    u.starts_with("https://")
        || u.starts_with("http://")
        || u.starts_with("ssh://")
        || u.starts_with("git://")
        || u.starts_with("file://")
        || (u.starts_with("git@") && u.contains(':'))
}

pub(crate) fn validate_git_url(url: &str) -> Result<(), Box<dyn Error>> {
    if looks_like_git_url(url) {
        Ok(())
    } else {
        Err(format!(
            "unsupported git URL scheme (allow https://, http://, ssh://, git://, file:///, git@host:path): {url}"
        )
        .into())
    }
}

/// 将 `file://` / `file:///` URL 转成文件系统路径。
///
/// Windows 上 gix 容易把 `file:///D:/repo` 误解析成 `file://D:\repo`（少一杠），
/// 因此克隆前应改走本地路径。
#[allow(clippy::manual_strip)] // `rest[1..]` 在 `#[cfg(windows)]` 块内，unix 回退需保留原 `rest` 的前导 `/`
pub fn file_url_to_path(url: &str) -> Option<PathBuf> {
    let u = url.trim();
    let rest = u.strip_prefix("file://")?;
    // file:///D:/x  → rest = "/D:/x"
    // file://D:/x   → rest = "D:/x"  (非标准，也尽量接受)
    // file:///home/x → rest = "/home/x"
    // file://localhost/D:/x → rest = "localhost/D:/x"
    let path_part = if let Some(after_host) = rest.strip_prefix("//") {
        // file:////unc/... rare; treat as path
        after_host
    } else if let Some(after) = rest
        .strip_prefix("localhost/")
        .or_else(|| rest.strip_prefix("localhost\\"))
    {
        after
    } else if rest.starts_with('/') {
        // "/D:/repo" or "/home/repo"
        #[cfg(windows)]
        {
            let bytes = rest.as_bytes();
            // "/C:/..." or "/C|/..."
            if bytes.len() >= 3
                && bytes[0] == b'/'
                && bytes[1].is_ascii_alphabetic()
                && (bytes[2] == b':' || bytes[2] == b'|')
            {
                let mut s = rest[1..].to_string();
                if s.as_bytes().get(1) == Some(&b'|') {
                    s.replace_range(1..2, ":");
                }
                return Some(PathBuf::from(s.replace('/', "\\")));
            }
        }
        rest
    } else if rest.len() >= 2
        && rest.as_bytes()[0].is_ascii_alphabetic()
        && rest.as_bytes()[1] == b':'
    {
        // "D:/repo" after file://
        rest
    } else {
        rest
    };

    let path = if cfg!(windows) {
        PathBuf::from(path_part.replace('/', "\\"))
    } else {
        PathBuf::from(path_part)
    };
    Some(path)
}

/// 交给 gix/git 的克隆源：file URL → 本地绝对路径，避免 Windows 下三斜杠被吃掉。
pub fn normalize_clone_source(url: &str) -> Result<String, Box<dyn Error>> {
    let url = url.trim();
    if !url.starts_with("file://") {
        return Ok(url.to_string());
    }
    let path =
        file_url_to_path(url).ok_or_else(|| format!("cannot parse file URL as path: {url}"))?;
    let path = if path.exists() {
        path.canonicalize().unwrap_or(path)
    } else {
        return Err(format!(
            "file URL path does not exist: {} (from {url})",
            path.display()
        )
        .into());
    };
    if !path.join(".git").exists() && gix::open(&path).is_err() {
        return Err(format!(
            "path is not a git repository: {} (from {url})",
            path.display()
        )
        .into());
    }
    Ok(path_for_gix_local(&path))
}

/// Windows `canonicalize` 会得到 `\\?\C:\...`，gix 会误当成 SCP URL；去掉此前缀。
fn path_for_gix_local(path: &std::path::Path) -> String {
    let mut s = path.to_string_lossy().into_owned();
    // `\\?\UNC\server\share\...` → `\\server\share\...`；普通 `\\?\C:\...` → `C:\...`
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        if let Some(unc) = rest.strip_prefix("UNC\\") {
            s = format!(r"\\{unc}");
        } else {
            s = rest.to_string();
        }
    }
    // 给 gix 用正斜杠的本地路径更稳（仍是本地 clone，不是 file URL）
    if cfg!(windows) {
        s = s.replace('\\', "/");
    }
    s
}

/// 公开：依赖目录名校验。
pub fn validate_dep_dir_name_pub(name: &str) -> Result<(), Box<dyn Error>> {
    validate_dep_dir_name(name)
}

/// 公开：从 URL 提取仓库名。
pub fn repo_name_from_url(url: &str) -> Result<String, Box<dyn Error>> {
    extract_repo_name(url)
}

/// 强制克隆到目标目录（目标必须不存在、为空目录，或已被调用方清掉）。
pub fn clone_into(url: &str, target_dir: &std::path::Path) -> Result<(), Box<dyn Error>> {
    validate_git_url(url)?;
    if target_dir.exists() {
        if target_dir.is_file() {
            return Err(format!(
                "clone target is a file, not a directory: {}",
                target_dir.display()
            )
            .into());
        }
        if !dir_is_empty(target_dir)? {
            return Err(format!("clone target already exists: {}", target_dir.display()).into());
        }
    } else if let Some(parent) = target_dir.parent() {
        fs::create_dir_all(parent)?;
    }
    clone_git_repo(
        url,
        target_dir,
        CloneOptions {
            skip_if_exists: false,
            interactive_overwrite: false,
            expected_name: target_dir
                .file_name()
                .and_then(|s| s.to_str())
                .map(ToString::to_string),
        },
    )?;
    Ok(())
}

/// 把 `url` 的仓库落到 `path` 本身（不套一层 repo 名目录）。
///
/// - `path` 不存在或为空目录：创建后直接克隆进去。
/// - `path` 已是 git 仓库：从 `url` fetch，能快进则快进，否则三路合并并更新工作区。
pub fn force_clone_or_sync(url: &str, path: &std::path::Path) -> Result<(), Box<dyn Error>> {
    validate_git_url(url)?;
    if should_clone_into(path)? {
        // 空目录直接克隆进去。Windows 上 `remove_dir` 常因句柄未释放报 os error 32。
        clone_into(url, path)?;
        return Ok(());
    }
    if gix::open(path).is_err() && !path.join(".git").exists() {
        return Err(format!(
            "path exists and is not an empty directory or git repository: {}",
            path.display()
        )
        .into());
    }
    sync_existing_repo(url, path)
}

/// 已有 checkout 的 `origin` fetch URL（没有 origin 则 `None`）。
pub fn origin_fetch_url(path: &std::path::Path) -> Option<String> {
    let repo = gix::open(path).ok()?;
    let remote = repo.find_remote("origin").ok()?;
    let url = remote.url(gix::remote::Direction::Fetch)?;
    Some(url.to_string())
}

fn remote_key(url: &str) -> String {
    let t = url.trim().trim_end_matches('/').trim_end_matches(".git");
    if t.starts_with("file://") {
        if let Some(p) = file_url_to_path(t) {
            let p = p.canonicalize().unwrap_or(p);
            return path_for_gix_local(&p).to_ascii_lowercase();
        }
    }
    let as_path = std::path::PathBuf::from(t);
    if as_path.exists() {
        let p = as_path.canonicalize().unwrap_or(as_path);
        return path_for_gix_local(&p).to_ascii_lowercase();
    }
    t.to_ascii_lowercase()
}

/// 两个 git 远程是否指向同一处（忽略 `.git` 后缀、file URL vs 本地路径）。
pub fn git_remotes_equivalent(a: &str, b: &str) -> bool {
    remote_key(a) == remote_key(b)
}

/// 已有目录不能当作 `url` 的继续同步目标时（origin 不同、或不是 git），应删掉重克隆。
pub fn should_replace_checkout(path: &std::path::Path, url: &str) -> Result<bool, Box<dyn Error>> {
    if !path.exists() {
        return Ok(false);
    }
    if should_clone_into(path)? {
        return Ok(false);
    }
    if gix::open(path).is_err() {
        return Ok(true);
    }
    let Some(origin) = origin_fetch_url(path) else {
        return Ok(true);
    };
    if git_remotes_equivalent(&origin, url) {
        return Ok(false);
    }
    if let Ok(norm) = normalize_clone_source(url) {
        if git_remotes_equivalent(&origin, &norm) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// 删掉一次 checkout（索引仓换远程时用）。Windows 上偶发句柄占用则短等再试。
pub fn remove_checkout(path: &std::path::Path) -> Result<(), Box<dyn Error>> {
    if !path.exists() {
        return Ok(());
    }
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) => {
            std::thread::sleep(std::time::Duration::from_millis(250));
            fs::remove_dir_all(path)
                .map_err(|e2| format!("cannot replace {}: {e}; retry: {e2}", path.display()))?;
            Ok(())
        }
    }
}

fn should_clone_into(path: &std::path::Path) -> Result<bool, Box<dyn Error>> {
    if !path.exists() {
        return Ok(true);
    }
    if path.is_file() {
        return Err(format!(
            "clone target is a file, not a directory: {}",
            path.display()
        )
        .into());
    }
    Ok(dir_is_empty(path)?)
}

fn dir_is_empty(path: &std::path::Path) -> io::Result<bool> {
    Ok(path.read_dir()?.next().is_none())
}

fn sync_existing_repo(url: &str, path: &std::path::Path) -> Result<(), Box<dyn Error>> {
    use gix::bstr::BStr;

    let clone_source = normalize_clone_source(url)?;
    let mut repo = gix::open(path).map_err(|e| format!("open git repo {}: {e}", path.display()))?;
    repo.committer_or_set_generic_fallback()?;

    let display_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("repo");
    println!("Fetching '{clone_source}' into {}...", path.display());

    let mut remote = if let Ok(origin) = repo.find_remote("origin") {
        origin.with_url(clone_source.as_str())?
    } else {
        repo.remote_at(clone_source.as_str())?
    };
    if remote.refspecs(gix::remote::Direction::Fetch).is_empty() {
        remote = remote.with_refspecs(
            [BStr::new(b"+refs/heads/*:refs/remotes/origin/*")],
            gix::remote::Direction::Fetch,
        )?;
    }

    // 不要把 `HEAD` 当 extra refspec：本地 HEAD 若指向未出生的 `refs/heads/main`，
    // fetch 更新引用时会 follow 失败。
    let fetch_prep = {
        let progress = IndicatifProgress::new(format!("fetch {display_name}"));
        remote
            .connect(gix::remote::Direction::Fetch)?
            .prepare_fetch(progress, Default::default())?
    };
    if remote_has_no_commits(fetch_prep.ref_map()) {
        println!(
            "Already up to date: {} (remote has no commits yet)",
            path.display()
        );
        return Ok(());
    }
    // gix fetch 协商会 peel `refs/` 下所有引用。空克隆常留下
    // `refs/remotes/origin/HEAD` → `refs/heads/<branch>`（分支文件还不存在），
    // peel 失败。Git 会跳过这类引用；这里先删掉再 fetch，不删整个仓库。
    hide_unpeelable_local_refs(&repo)?;
    let fetch_outcome = match fetch_prep.receive(
        IndicatifProgress::new(format!("receive {display_name}")),
        &gix::interrupt::IS_INTERRUPTED,
    ) {
        Ok(outcome) => outcome,
        Err(gix::remote::fetch::Error::NoMapping { .. }) => {
            return Err(format!(
                "fetch from {url} produced no matching refs (local origin may still be a different repo).\n  \
                 remove {} and retry, or `Optive index change <url>`",
                path.display()
            )
            .into());
        }
        Err(err) => return Err(err.into()),
    };
    drop(remote);

    let theirs = remote_tip_id(&repo, &fetch_outcome.ref_map)?;
    let ours = match repo.head_id() {
        Ok(id) => id.detach(),
        Err(_) => {
            // 未出生 HEAD：创建当前分支（不要 deref 还不存在的 refs/heads/main）。
            checkout_commit(path, theirs, true)?;
            println!("Synced '{}' to {}", path.display(), theirs.to_hex());
            return Ok(());
        }
    };

    if ours == theirs {
        println!("Already up to date: {}", path.display());
        return Ok(());
    }

    let merge_base = repo.merge_base(ours, theirs).ok().map(|id| id.detach());
    if merge_base == Some(ours) {
        checkout_commit(path, theirs, true)?;
        println!("Fast-forwarded '{}' to {}", path.display(), theirs.to_hex());
        return Ok(());
    }
    if merge_base == Some(theirs) {
        println!("Already up to date: {}", path.display());
        return Ok(());
    }

    let labels = gix::merge::blob::builtin_driver::text::Labels {
        ancestor: Some("merge-base".into()),
        current: Some("HEAD".into()),
        other: Some("origin".into()),
    };
    let merge_opts: gix::merge::commit::Options = repo.tree_merge_options()?.into();
    let merge_opts = merge_opts.with_allow_missing_merge_base(true);
    let mut merge = repo.merge_commits(ours, theirs, labels, merge_opts)?;
    if merge
        .tree_merge
        .has_unresolved_conflicts(gix::merge::tree::TreatAsUnresolved::git())
    {
        return Err(format!("merge conflict while syncing {} from {url}", path.display()).into());
    }
    let tree_id = merge.tree_merge.tree.write()?.detach();
    // `commit()` 还要 author；机器上常只有 committer fallback。两侧都用 committer，避免 AuthorMissing。
    let sig = repo
        .committer()
        .ok_or("git committer is not configured")??;
    repo.commit_as(
        sig,
        sig,
        "HEAD",
        format!("Merge remote-tracking branch of {url}"),
        tree_id,
        [ours, theirs],
    )?;
    checkout_tree(path, tree_id)?;
    println!("Merged '{url}' into {}", path.display());
    Ok(())
}

/// 删掉无法 peel 到 object 的本地引用（例如空克隆留下的 `origin/HEAD`）。
fn hide_unpeelable_local_refs(repo: &Repository) -> Result<(), Box<dyn Error>> {
    use gix::refs::transaction::{Change, PreviousValue, RefEdit, RefLog};

    let mut deletes = Vec::new();
    {
        let platform = repo.references()?;
        for r in platform.all()? {
            let r = r.map_err(|e| -> Box<dyn Error> { e.to_string().into() })?;
            let name = r.name().to_owned();
            if r.into_fully_peeled_id().is_err() {
                deletes.push(RefEdit {
                    change: Change::Delete {
                        expected: PreviousValue::Any,
                        log: RefLog::AndReference,
                    },
                    name,
                    deref: false,
                });
            }
        }
    }
    if !deletes.is_empty() {
        repo.edit_references(deletes)?;
    }
    Ok(())
}

/// 将 HEAD 指到 `commit_id` 并检出工作区。`deref == true` 时更新 HEAD 指向的分支，避免变成 detached。
fn checkout_commit(
    repo_dir: &std::path::Path,
    commit_id: gix::ObjectId,
    deref: bool,
) -> Result<(), Box<dyn Error>> {
    use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
    use gix::refs::Target;

    let mut repo = gix::open(repo_dir)?;
    repo.committer_or_set_generic_fallback()?;
    let head = repo.head()?;
    let (ref_name, follow): (gix::refs::FullName, bool) = if head.is_unborn() {
        let name = head
            .referent_name()
            .ok_or("unborn HEAD has no branch name")?
            .to_owned();
        (name, false)
    } else {
        ("HEAD".try_into()?, deref)
    };
    if !head.is_unborn() {
        if let Ok(head_id) = repo.head_id() {
            if head_id == commit_id {
                return Ok(());
            }
        }
    }
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: "sync".into(),
            },
            expected: PreviousValue::Any,
            new: Target::Object(commit_id),
        },
        name: ref_name,
        deref: follow,
    })?;
    checkout_tree(repo_dir, commit_id)
}

fn checkout_tree(repo_dir: &std::path::Path, oid: gix::ObjectId) -> Result<(), Box<dyn Error>> {
    let repo = gix::open(repo_dir)?;
    let tree_id = repo.find_object(oid)?.peel_to_tree()?.id;
    let mut index = repo.index_from_tree(&tree_id)?;
    let workdir = repo
        .workdir()
        .ok_or_else(|| format!("repository at {} has no worktree", repo_dir.display()))?
        .to_owned();
    let opts = repo.checkout_options(gix::worktree::stack::state::attributes::Source::IdMapping)?;
    checkout_git_worktree(repo, &mut index, workdir, opts)?;
    index.write(Default::default())?;
    Ok(())
}

fn remote_has_no_commits(ref_map: &gix::remote::fetch::RefMap) -> bool {
    if ref_map.remote_refs.is_empty() {
        return true;
    }
    ref_map
        .remote_refs
        .iter()
        .all(|r| matches!(r, gix::protocol::handshake::Ref::Unborn { .. }))
}

fn remote_tip_id(
    repo: &Repository,
    ref_map: &gix::remote::fetch::RefMap,
) -> Result<gix::ObjectId, Box<dyn Error>> {
    for r in &ref_map.remote_refs {
        match r {
            gix::protocol::handshake::Ref::Symbolic {
                full_ref_name,
                object,
                ..
            }
            | gix::protocol::handshake::Ref::Direct {
                full_ref_name,
                object,
            } if full_ref_name == "HEAD" => {
                return Ok(object.to_owned());
            }
            gix::protocol::handshake::Ref::Unborn { full_ref_name, .. }
                if full_ref_name == "HEAD" =>
            {
                return Err("remote HEAD is unborn (empty repository)".into());
            }
            _ => {}
        }
    }

    // 跟踪分支 HEAD（clone 后通常是 symbolic → origin/<default>）
    if let Ok(r) = repo.find_reference("refs/remotes/origin/HEAD") {
        if let Ok(id) = r.into_fully_peeled_id() {
            return Ok(id.detach());
        }
    }
    for name in ["refs/remotes/origin/main", "refs/remotes/origin/master"] {
        if let Ok(r) = repo.find_reference(name) {
            if let Ok(id) = r.into_fully_peeled_id() {
                return Ok(id.detach());
            }
        }
    }

    Err("could not determine remote HEAD after fetch".into())
}

/// 解析远程 tip commit sha（可选指定 branch）。
pub fn resolve_remote_tip(url: &str, branch: Option<&str>) -> Result<String, Box<dyn Error>> {
    validate_git_url(url)?;
    let tmp_root = std::env::temp_dir().join(format!(
        "optive_tip_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    let _ = fs::remove_dir_all(&tmp_root);
    // clone_into 要求目标路径尚不存在；只确保父目录在即可。
    if let Some(parent) = tmp_root.parent() {
        fs::create_dir_all(parent)?;
    }
    let outcome = (|| -> Result<String, Box<dyn Error>> {
        clone_into(url, &tmp_root)?;
        if let Some(b) = branch {
            checkout_rev(&tmp_root, b)?;
        }
        let repo = gix::open(&tmp_root)?;
        let id = repo.head_id()?;
        Ok(id.to_string())
    })();
    let _ = fs::remove_dir_all(&tmp_root);
    outcome
}

/// 列出远程仓库中最高的稳定 semver tag，返回精确三元组字符串（如 `1.2.3`）。
pub fn latest_semver_version(url: &str) -> Result<String, Box<dyn Error>> {
    validate_git_url(url)?;
    let tmp_root = std::env::temp_dir().join(format!(
        "optive_latest_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    let _ = fs::remove_dir_all(&tmp_root);
    if let Some(parent) = tmp_root.parent() {
        fs::create_dir_all(parent)?;
    }
    let outcome = (|| -> Result<String, Box<dyn Error>> {
        clone_into(url, &tmp_root)?;
        let repo = gix::open(&tmp_root)?;
        let mut best: Option<super::semver::Version> = None;
        for (ver, _) in list_semver_tags(&repo)? {
            if best.as_ref().is_none_or(|b| ver > *b) {
                best = Some(ver);
            }
        }
        let Some(ver) = best else {
            return Err(format!(
                "no semver git tags in {url}; cannot pick a default version for `Optive add <pack>`\n\
  tag a release (e.g. v0.1.0) or specify an explicit constraint: `Optive add <pack>@0.1.0`"
            )
            .into());
        };
        Ok(ver.to_string())
    })();
    let _ = fs::remove_dir_all(&tmp_root);
    outcome
}

/// 按版本号选 tag（`0.1.2` 或 `v0.1.2`）并剥皮为 commit SHA。
pub fn resolve_version_commit(url: &str, version: &str) -> Result<String, Box<dyn Error>> {
    validate_git_url(url)?;
    let tmp_root = std::env::temp_dir().join(format!(
        "optive_ver_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    let _ = fs::remove_dir_all(&tmp_root);
    if let Some(parent) = tmp_root.parent() {
        fs::create_dir_all(parent)?;
    }
    let outcome = (|| -> Result<String, Box<dyn Error>> {
        clone_into(url, &tmp_root)?;
        let req = super::semver::parse_req(version)
            .map_err(|e| format!("invalid version constraint `{version}`: {e}"))?;
        let repo = gix::open(&tmp_root)?;
        let tags = list_semver_tags(&repo)?;
        let mut best: Option<(super::semver::Version, String)> = None;
        for (ver, tag) in &tags {
            if !req.matches(ver) {
                continue;
            }
            if best.as_ref().is_none_or(|(b, _)| ver > b) {
                best = Some((*ver, tag.clone()));
            }
        }
        let Some((_, tag)) = best else {
            return Err(missing_version_tag_err(url, version, &tags).into());
        };
        checkout_rev(&tmp_root, &tag)?;
        let repo = gix::open(&tmp_root)?;
        let id = repo.head_id()?;
        Ok(id.to_string())
    })();
    let _ = fs::remove_dir_all(&tmp_root);
    outcome
}

fn missing_version_tag_err(
    url: &str,
    version: &str,
    available: &[(super::semver::Version, String)],
) -> String {
    let bare = version.trim_start_matches(['v', 'V']);
    let avail = if available.is_empty() {
        "this repository has no semver git tags.".to_string()
    } else {
        let names: Vec<&str> = available.iter().map(|(_, t)| t.as_str()).collect();
        format!("available tags: {}", names.join(", "))
    };
    format!(
        "no git tag matching `{version}` in {url}\n  {avail}\n\n\
note: index versions only match git tags (`{bare}` or `v{bare}`), not a field in Optive.toml.\n\
  publish the pack, then push the tag:\n\
      Optive publish {bare}\n\
      git push origin v{bare}\n\
  or pin a commit in the consumer:\n\
      <name> = {{ git = \"{url}\", rev = \"<commit>\" }}"
    )
}

fn list_semver_tags(
    repo: &Repository,
) -> Result<Vec<(super::semver::Version, String)>, Box<dyn Error>> {
    let platform = repo.references()?;
    let mut out = Vec::new();
    for r in platform.all()? {
        let r = r.map_err(|e| -> Box<dyn Error> { e.to_string().into() })?;
        let full = r.name().as_bstr().to_string();
        let Some(tag) = full.strip_prefix("refs/tags/") else {
            continue;
        };
        // 剥 annotated tag 后缀^{} 若出现
        let tag = tag.strip_suffix("^{}").unwrap_or(tag);
        if let Some(ver) = super::semver::parse_version_from_tag(tag) {
            out.push((ver, tag.to_string()));
        }
    }
    Ok(out)
}

/// 将 tag 剥皮为 commit SHA（用于 lock / CAS 不可变快照）。
pub fn resolve_tag_commit(url: &str, tag: &str) -> Result<String, Box<dyn Error>> {
    validate_git_url(url)?;
    let tmp_root = std::env::temp_dir().join(format!(
        "optive_tag_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    let _ = fs::remove_dir_all(&tmp_root);
    if let Some(parent) = tmp_root.parent() {
        fs::create_dir_all(parent)?;
    }
    let outcome = (|| -> Result<String, Box<dyn Error>> {
        clone_into(url, &tmp_root)?;
        checkout_rev(&tmp_root, tag)?;
        let repo = gix::open(&tmp_root)?;
        let id = repo.head_id()?;
        Ok(id.to_string())
    })();
    let _ = fs::remove_dir_all(&tmp_root);
    outcome
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloneOutcome {
    Cloned,
    SkippedExisting,
}

struct CloneOptions {
    skip_if_exists: bool,
    interactive_overwrite: bool,
    expected_name: Option<String>,
}

fn validate_dep_dir_name(name: &str) -> Result<(), Box<dyn Error>> {
    if is_invalid_repo_name(name) {
        return Err(format!("invalid dependency directory name: {name:?}").into());
    }
    Ok(())
}

fn clone_git_repo(
    url: &str,
    target_dir: &std::path::Path,
    opts: CloneOptions,
) -> Result<CloneOutcome, Box<dyn Error>> {
    let display_name = opts.expected_name.clone().unwrap_or_else(|| {
        target_dir
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("repo")
            .into()
    });

    let exists_non_empty =
        target_dir.exists() && !(target_dir.is_dir() && dir_is_empty(target_dir).unwrap_or(false));
    if exists_non_empty {
        if opts.skip_if_exists {
            println!("Dependency already present: {}", target_dir.display());
            return Ok(CloneOutcome::SkippedExisting);
        }
        if opts.interactive_overwrite {
            println!("The target directory already exists: {target_dir:?}\nOverwrite it? [y/N]");
            let mut input = String::new();
            io::stdin().read_line(&mut input)?;
            let input = input.trim().to_ascii_lowercase();
            if input == "y" || input == "yes" {
                println!("Removing existing directory: {target_dir:?}");
                fs::remove_dir_all(target_dir)?;
                println!("Removed existing directory: {target_dir:?}");
            } else {
                println!("Clone cancelled.");
                return Err("clone cancelled: target directory already exists".into());
            }
        } else {
            return Err(
                format!("target directory already exists: {}", target_dir.display()).into(),
            );
        }
    }

    println!("Cloning '{url}' into {target_dir:?}...");

    let clone_source = normalize_clone_source(url)?;
    if clone_source != url {
        println!("  (local path: {clone_source})");
    }

    let mut prepare_fetch = gix::prepare_clone(clone_source.as_str(), target_dir)?;

    let (mut prepare_checkout, _fetch_outcome) = {
        let progress = IndicatifProgress::new(format!("fetch {display_name}"));
        prepare_fetch.fetch_then_checkout(progress, &gix::interrupt::IS_INTERRUPTED)?
    };

    let (repo, _checkout_outcome) = {
        let progress = IndicatifProgress::new(format!("checkout {display_name}"));
        prepare_checkout.main_worktree(progress, &gix::interrupt::IS_INTERRUPTED)?
    };

    let workdir = repo
        .workdir()
        .ok_or("clone succeeded but worktree directory is missing")?;
    println!("Successfully cloned '{display_name}' into {workdir:?}");
    Ok(CloneOutcome::Cloned)
}

/// 将已存在仓库的工作区对齐到 `rev`（分支名 / tag / commit）。
pub fn checkout_rev(repo_dir: &std::path::Path, rev: &str) -> Result<(), Box<dyn Error>> {
    use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
    use gix::refs::Target;

    let mut repo = gix::open(repo_dir)?;
    repo.committer_or_set_generic_fallback()?;
    let target = repo.rev_parse_single(rev)?;
    let target_id = target.detach();

    if let Ok(head_id) = repo.head_id() {
        if head_id == target_id {
            return Ok(());
        }
    }

    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: format!("checkout {rev}").into(),
            },
            expected: PreviousValue::Any,
            new: Target::Object(target_id),
        },
        name: "HEAD".try_into()?,
        deref: false,
    })?;

    let tree_id = repo.find_object(target_id)?.peel_to_tree()?.id;
    let mut index = repo.index_from_tree(&tree_id)?;
    let workdir = repo
        .workdir()
        .ok_or_else(|| format!("repository at {} has no worktree", repo_dir.display()))?
        .to_owned();
    let opts = repo.checkout_options(gix::worktree::stack::state::attributes::Source::IdMapping)?;

    checkout_git_worktree(repo, &mut index, workdir, opts)?;
    index.write(Default::default())?;
    Ok(())
}

fn checkout_git_worktree(
    repo: Repository,
    index: &mut File,
    workdir: PathBuf,
    opts: Options,
) -> Result<(), Box<dyn Error>> {
    gix::worktree::state::checkout(
        index,
        workdir,
        repo.objects.clone().into_arc()?,
        &gix::progress::Discard,
        &gix::progress::Discard,
        &gix::interrupt::IS_INTERRUPTED,
        opts,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_url_windows_drive_keeps_third_slash_semantics() {
        let p = file_url_to_path("file:///D:/optive-playground/greeter").unwrap();
        let s = p.to_string_lossy();
        // 盘符路径，不能变成 file://D: 那种少杠形式
        assert!(
            s.contains("optive-playground") && s.contains("greeter"),
            "{s}"
        );
        #[cfg(windows)]
        assert!(s.starts_with("D:") || s.starts_with("d:"), "{s}");
    }

    #[test]
    fn git_remotes_equivalent_strips_git_suffix() {
        assert!(git_remotes_equivalent(
            "https://gitee.com/CGrakeski/optindex.git",
            "https://gitee.com/CGrakeski/optindex"
        ));
        assert!(!git_remotes_equivalent(
            "https://gitee.com/CGrakeski/optindex.git",
            r"D:\Optindex"
        ));
    }

    #[test]
    fn file_url_unix_absolute() {
        let p = file_url_to_path("file:///home/user/greeter").unwrap();
        assert_eq!(p, PathBuf::from("/home/user/greeter"));
    }

    #[test]
    fn force_clone_or_sync_empty_repo_twice() {
        let tmp = std::env::temp_dir().join(format!(
            "optive_empty_sync_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let src = tmp.join("src");
        let dst = tmp.join("dst");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&src).unwrap();
        gix::init(&src).unwrap();
        let url = if cfg!(windows) {
            format!("file:///{}", src.display().to_string().replace('\\', "/"))
        } else {
            format!("file://{}", src.display())
        };
        force_clone_or_sync(&url, &dst).expect("first clone");
        force_clone_or_sync(&url, &dst).expect("second sync of empty repo");
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn force_clone_or_sync_into_preexisting_empty_dir() {
        let tmp = std::env::temp_dir().join(format!(
            "optive_empty_dst_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let src = tmp.join("src");
        let dst = tmp.join("dst");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();
        gix::init(&src).unwrap();
        let url = if cfg!(windows) {
            format!("file:///{}", src.display().to_string().replace('\\', "/"))
        } else {
            format!("file://{}", src.display())
        };
        force_clone_or_sync(&url, &dst).expect("clone into existing empty directory");
        assert!(dst.join(".git").exists());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn force_clone_or_sync_empty_then_first_commit() {
        let tmp = std::env::temp_dir().join(format!(
            "optive_first_commit_sync_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let src = tmp.join("src");
        let dst = tmp.join("dst");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&src).unwrap();
        gix::init(&src).unwrap();
        let url = if cfg!(windows) {
            format!("file:///{}", src.display().to_string().replace('\\', "/"))
        } else {
            format!("file://{}", src.display())
        };
        force_clone_or_sync(&url, &dst).expect("clone empty");

        let status = std::process::Command::new("git")
            .current_dir(&src)
            .args([
                "-c",
                "user.email=test@example.com",
                "-c",
                "user.name=test",
                "commit",
                "--allow-empty",
                "-m",
                "init",
            ])
            .status()
            .expect("run git commit");
        assert!(status.success(), "git commit --allow-empty failed");

        force_clone_or_sync(&url, &dst).expect("sync first remote commit onto unborn local HEAD");
        let dst_repo = gix::open(&dst).unwrap();
        assert!(
            dst_repo.head_id().is_ok(),
            "local HEAD should be born after sync"
        );
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn resolve_version_missing_tag_mentions_hint() {
        let tmp = std::env::temp_dir().join(format!("optive_ver_hint_{}", std::process::id()));
        let src = tmp.join("src");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&src).unwrap();
        let status = std::process::Command::new("git")
            .current_dir(&src)
            .args(["init"])
            .status()
            .unwrap();
        assert!(status.success());
        let _ = std::process::Command::new("git")
            .current_dir(&src)
            .args([
                "-c",
                "user.email=t@e.com",
                "-c",
                "user.name=t",
                "commit",
                "--allow-empty",
                "-m",
                "i",
            ])
            .status();
        let url = if cfg!(windows) {
            format!("file:///{}", src.display().to_string().replace('\\', "/"))
        } else {
            format!("file://{}", src.display())
        };
        let err = resolve_version_commit(&url, "0.0.1")
            .unwrap_err()
            .to_string();
        assert!(err.contains("no git tag matching"), "{err}");
        assert!(err.contains("no semver git tags"), "{err}");
        assert!(err.contains("Optive publish"), "{err}");
        assert!(err.contains("rev"), "{err}");
        assert!(
            !err.contains("couldn't parse revision"),
            "must not dump gix rev-parse errors:\n{err}"
        );
        let _ = fs::remove_dir_all(&tmp);
    }
}
