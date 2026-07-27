use gix::progress::{Count, Id, MessageLevel, NestedProgress, Progress, Step, StepShared, Unit};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
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
        self.bar.set_position(self.step.load(Ordering::Relaxed) as u64);
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
/// - "https://github.com/user/repo.git" -> "repo"
/// - "git@github.com:user/repo.git"   -> "repo"
/// - "https://gitlab.com/group/sub/repo" -> "repo"
///
/// 仅允许单段安全目录名（字母数字、`.`、`_`、`-`），拒绝路径穿越。
fn extract_repo_name(url: &str) -> Result<String, Box<dyn std::error::Error>> {
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
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
    {
        return Err(format!("invalid repository name derived from URL: {name:?}").into());
    }
    Ok(name.to_string())
}

fn validate_git_url(url: &str) -> Result<(), Box<dyn std::error::Error>> {
    let u = url.trim();
    let ok = u.starts_with("https://")
        || u.starts_with("http://")
        || u.starts_with("ssh://")
        || u.starts_with("git://")
        || u.starts_with("file://")
        || (u.starts_with("git@") && u.contains(':'));
    if !ok {
        return Err(format!(
            "unsupported git URL scheme (allow https://, http://, ssh://, git://, file:///, git@host:path): {url}"
        )
        .into());
    }
    Ok(())
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
    } else if rest.len() >= 2 && rest.as_bytes()[0].is_ascii_alphabetic() && rest.as_bytes()[1] == b':' {
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
pub fn normalize_clone_source(url: &str) -> Result<String, Box<dyn std::error::Error>> {
    let url = url.trim();
    if !url.starts_with("file://") {
        return Ok(url.to_string());
    }
    let path = file_url_to_path(url).ok_or_else(|| {
        format!("cannot parse file URL as path: {url}")
    })?;
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
pub fn validate_dep_dir_name_pub(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    validate_dep_dir_name(name)
}

/// 公开：从 URL 提取仓库名。
pub fn repo_name_from_url(url: &str) -> Result<String, Box<dyn std::error::Error>> {
    extract_repo_name(url)
}

/// 强制克隆到目标目录（目标必须不存在或已被调用方清掉）。
pub fn clone_into(url: &str, target_dir: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    validate_git_url(url)?;
    if target_dir.exists() {
        return Err(format!(
            "clone target already exists: {}",
            target_dir.display()
        )
        .into());
    }
    if let Some(parent) = target_dir.parent() {
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
                .map(|s| s.to_string()),
        },
    )?;
    Ok(())
}

/// 解析远程 tip commit sha（可选指定 branch）。
pub fn resolve_remote_tip(
    url: &str,
    branch: Option<&str>,
) -> Result<String, Box<dyn std::error::Error>> {
    validate_git_url(url)?;
    let tmp_root = std::env::temp_dir().join(format!(
        "optive_tip_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = fs::remove_dir_all(&tmp_root);
    // clone_into 要求目标路径尚不存在；只确保父目录在即可。
    if let Some(parent) = tmp_root.parent() {
        fs::create_dir_all(parent)?;
    }
    let outcome = (|| -> Result<String, Box<dyn std::error::Error>> {
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

/// 将 tag 剥皮为 commit SHA（用于 lock / CAS 不可变快照）。
pub fn resolve_tag_commit(
    url: &str,
    tag: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    validate_git_url(url)?;
    let tmp_root = std::env::temp_dir().join(format!(
        "optive_tag_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = fs::remove_dir_all(&tmp_root);
    if let Some(parent) = tmp_root.parent() {
        fs::create_dir_all(parent)?;
    }
    let outcome = (|| -> Result<String, Box<dyn std::error::Error>> {
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

fn validate_dep_dir_name(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
    {
        return Err(format!("invalid dependency directory name: {name:?}").into());
    }
    Ok(())
}

fn clone_git_repo(
    url: &str,
    target_dir: &std::path::Path,
    opts: CloneOptions,
) -> Result<CloneOutcome, Box<dyn std::error::Error>> {
    let display_name = opts
        .expected_name
        .clone()
        .unwrap_or_else(|| target_dir.file_name().and_then(|s| s.to_str()).unwrap_or("repo").into());

    if target_dir.exists() {
        if opts.skip_if_exists {
            println!("Dependency already present: {}", target_dir.display());
            return Ok(CloneOutcome::SkippedExisting);
        }
        if opts.interactive_overwrite {
            println!(
                "The target directory already exists: {:?}\nOverwrite it? [y/N]",
                target_dir
            );
            let mut input = String::new();
            io::stdin().read_line(&mut input)?;
            let input = input.trim().to_ascii_lowercase();
            if input == "y" || input == "yes" {
                println!("Removing existing directory: {:?}", target_dir);
                fs::remove_dir_all(target_dir)?;
                println!("Removed existing directory: {:?}", target_dir);
            } else {
                println!("Clone cancelled.");
                return Err("clone cancelled: target directory already exists".into());
            }
        } else {
            return Err(format!(
                "target directory already exists: {}",
                target_dir.display()
            )
            .into());
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
pub fn checkout_rev(
    repo_dir: &std::path::Path,
    rev: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
    use gix::refs::Target;

    let repo = gix::open(repo_dir)?;
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

    gix::worktree::state::checkout(
        &mut index,
        workdir,
        repo.objects.clone().into_arc()?,
        &gix::progress::Discard,
        &gix::progress::Discard,
        &gix::interrupt::IS_INTERRUPTED,
        opts,
    )?;
    index.write(Default::default())?;
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
    fn file_url_unix_absolute() {
        let p = file_url_to_path("file:///home/user/greeter").unwrap();
        assert_eq!(p, PathBuf::from("/home/user/greeter"));
    }
}

