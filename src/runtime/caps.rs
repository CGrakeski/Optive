//! 运行时能力（capability）隔离：限制脚本可访问的宿主资源。
//!
//! 默认 `Capabilities::full()` 放开全部权限（与历史行为一致）。CLI 的
//! `--sandbox` / `--no-network` / `--allow-path` 构造受限 `Capabilities` 注入 VM，
//! 让不可信脚本 / 依赖在受控边界内运行。
//!
//! 受限文件访问会拒绝 `..`。配置的沙箱根前缀本身不做链接检查；打开根之后，
//! 对沙箱相对路径逐段拒绝符号链接（Windows 也拒绝 reparse point / junction）。
//! 实际文件 I/O 通过 `cap_std::fs::Dir` 根目录句柄及相对路径完成。
//! 这可阻止并发 symlink/rename 把访问重定向到根外，但它不是完整 OS 沙箱：同一
//! 可写根内的并发重命名仍可能令操作命中根内另一个对象；`remove_dir_all` 的对象
//! 身份也不保证跨并发 rename 原子稳定。不能接收已打开文件句柄的第三方 API
//! （当前为文件 SQLite 和动态库加载）在受限模式直接禁用。

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};

use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions};

use crate::error::RuntimeError;

/// 文件系统访问策略。
#[derive(Clone, Debug)]
pub enum FsPolicy {
    /// 不限制（默认）。
    Unrestricted,
    /// 仅允许落在给定可读写根目录之下，实际 I/O 相对目录句柄执行。
    Allow(Vec<PathBuf>),
    /// 项目/显式根可读写，锁定依赖根只读。
    Scoped {
        read_write: Vec<PathBuf>,
        read_only: Vec<PathBuf>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FsAccess {
    Read,
    Write,
}

enum FsTarget {
    Ambient(PathBuf),
    Restricted {
        dir: Dir,
        relative: PathBuf,
        root_path: PathBuf,
        host_path: PathBuf,
    },
}

#[derive(Clone, Copy)]
enum MetadataQuery {
    Exists,
    File,
    Dir,
}

/// 宿主对第三方依赖的显式授权。声明只能请求，授权来自 CLI / 嵌入方。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DepGrant {
    pub trust_all: bool,
    pub network: bool,
    pub env: bool,
    pub process: bool,
    pub ffi: bool,
}

impl DepGrant {
    #[must_use]
    pub const fn none() -> Self {
        Self {
            trust_all: false,
            network: false,
            env: false,
            process: false,
            ffi: false,
        }
    }
}

impl Default for DepGrant {
    fn default() -> Self {
        Self::none()
    }
}

/// 包清单上的能力请求（不能自行授予）。
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct CapabilityRequest {
    #[serde(default)]
    pub network: bool,
    #[serde(default)]
    pub process: bool,
    #[serde(default)]
    pub env: bool,
    #[serde(default)]
    pub ffi: bool,
    /// 请求可读的路径前缀（只是声明；授权来自 CLI/宿主）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read: Vec<String>,
    /// 请求可写的路径前缀（只是声明；授权来自 CLI/宿主）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub write: Vec<String>,
}

impl CapabilityRequest {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self.network
            && !self.process
            && !self.env
            && !self.ffi
            && self.read.is_empty()
            && self.write.is_empty()
    }
}

/// 脚本可用的宿主能力集合。
#[derive(Clone, Debug)]
pub struct Capabilities {
    pub network: bool,
    pub fs: FsPolicy,
    /// 是否允许 `std.os.setenv` / `chdir` 等改变进程环境的操作。
    pub env: bool,
    /// 是否允许 `std.os.run` / `capture` 拉起子进程。
    pub process: bool,
    /// 是否允许 `frompath` / `extern` 加载并调用本地动态库。
    pub ffi: bool,
    /// 对第三方依赖的授权；默认最小权限。
    pub dep_grant: DepGrant,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self::full()
    }
}

impl Capabilities {
    /// 全开（向后兼容：直接 `Optive xxx.tive` 的默认行为）。
    #[must_use]
    pub const fn full() -> Self {
        Self {
            network: true,
            fs: FsPolicy::Unrestricted,
            env: true,
            process: true,
            ffi: true,
            dep_grant: DepGrant::none(),
        }
    }

    /// 沙箱：禁网、禁改环境、禁 FFI、文件系统限制在 `roots` 之下。
    #[must_use]
    pub const fn sandbox(roots: Vec<PathBuf>) -> Self {
        Self {
            network: false,
            fs: FsPolicy::Scoped {
                read_write: roots,
                read_only: Vec::new(),
            },
            env: false,
            process: false,
            ffi: false,
            dep_grant: DepGrant::none(),
        }
    }

    /// 执行第三方包代码时使用的能力：默认只读包根，禁网 / 环境 / 进程 / FFI。
    /// `--trust-deps` 时依赖继承当前宿主能力。
    #[must_use]
    pub fn restrict_for_dependency(&self, package_root: &Path) -> Self {
        if self.dep_grant.trust_all {
            return self.clone();
        }
        Self {
            network: self.network && self.dep_grant.network,
            env: self.env && self.dep_grant.env,
            process: self.process && self.dep_grant.process,
            ffi: self.ffi && self.dep_grant.ffi,
            fs: FsPolicy::Scoped {
                read_write: Vec::new(),
                read_only: vec![package_root.to_path_buf()],
            },
            dep_grant: self.dep_grant,
        }
    }

    /// FFI 网关：`frompath` / `extern` 绑定时先过此关。
    pub fn check_ffi(&self, op: &str) -> Result<(), RuntimeError> {
        if self.ffi {
            Ok(())
        } else {
            Err(RuntimeError::io_err(format!(
                "{}: native FFI disabled (sandbox; pass --allow-ffi to enable)",
                crate::value::builtin_repr(op)
            )))
        }
    }

    /// 网络网关：`std.http` / `std.net` 调用前先过此关。
    pub fn check_network(&self, op: &str) -> Result<(), RuntimeError> {
        if self.network {
            Ok(())
        } else {
            Err(RuntimeError::io_err(format!(
                "{}: network access disabled (sandbox / --no-network)",
                crate::value::builtin_repr(op)
            )))
        }
    }

    /// 环境改变网关：`setenv` / `chdir` 调用前先过此关。
    pub fn check_env(&self, op: &str) -> Result<(), RuntimeError> {
        if self.env {
            Ok(())
        } else {
            Err(RuntimeError::io_err(format!(
                "{}: environment mutation disabled (sandbox)",
                crate::value::builtin_repr(op)
            )))
        }
    }

    /// 子进程网关：`std.os.run` / `capture`。依赖默认关闭。
    pub fn check_process(&self, op: &str) -> Result<(), RuntimeError> {
        if self.process {
            Ok(())
        } else {
            Err(RuntimeError::io_err(format!(
                "{}: process spawn disabled (sandbox)",
                crate::value::builtin_repr(op)
            )))
        }
    }

    /// 为项目执行补齐项目 RW、锁定依赖 RO 根。
    pub fn configure_project_fs(
        &mut self,
        project_root: &Path,
        dependency_roots: impl IntoIterator<Item = PathBuf>,
    ) {
        match &mut self.fs {
            FsPolicy::Unrestricted => {}
            FsPolicy::Allow(read_write) => {
                read_write.push(project_root.to_path_buf());
            }
            FsPolicy::Scoped {
                read_write,
                read_only,
            } => {
                read_write.push(project_root.to_path_buf());
                read_only.extend(dependency_roots);
            }
        }
    }

    #[must_use]
    pub fn writable_root(&self) -> Option<PathBuf> {
        match &self.fs {
            FsPolicy::Unrestricted => std::env::current_dir().ok(),
            FsPolicy::Allow(roots) => roots.first().cloned(),
            FsPolicy::Scoped { read_write, .. } => read_write.first().cloned(),
        }
    }

    #[must_use]
    pub const fn fs_restricted(&self) -> bool {
        !matches!(&self.fs, FsPolicy::Unrestricted)
    }

    /// 兼容仅检查调用。实际文件操作必须使用本类型的 handle-based I/O 方法。
    pub fn check_fs(&self, op: &str, path: &str) -> Result<(), RuntimeError> {
        self.resolve_fs_path(op, path, FsAccess::Read).map(|_| ())
    }

    /// 路由路径并返回显示用宿主路径；不得把返回值用于受限模式的实际 I/O。
    pub fn resolve_fs_path(
        &self,
        op: &str,
        path: impl AsRef<Path>,
        access: FsAccess,
    ) -> Result<PathBuf, RuntimeError> {
        match self.fs_target(op, path, access)? {
            FsTarget::Ambient(path) => Ok(path),
            FsTarget::Restricted { host_path, .. } => Ok(host_path),
        }
    }

    pub fn read_to_string(&self, op: &str, path: impl AsRef<Path>) -> Result<String, RuntimeError> {
        match self.fs_target(op, path, FsAccess::Read)? {
            FsTarget::Ambient(path) => std::fs::read_to_string(path),
            FsTarget::Restricted { dir, relative, .. } => dir.read_to_string(relative),
        }
        .map_err(|e| fs_io_error(op, e))
    }

    pub fn read(&self, op: &str, path: impl AsRef<Path>) -> Result<Vec<u8>, RuntimeError> {
        match self.fs_target(op, path, FsAccess::Read)? {
            FsTarget::Ambient(path) => std::fs::read(path),
            FsTarget::Restricted { dir, relative, .. } => dir.read(relative),
        }
        .map_err(|e| fs_io_error(op, e))
    }

    pub fn write(
        &self,
        op: &str,
        path: impl AsRef<Path>,
        contents: impl AsRef<[u8]>,
    ) -> Result<(), RuntimeError> {
        match self.fs_target(op, path, FsAccess::Write)? {
            FsTarget::Ambient(path) => std::fs::write(path, contents),
            FsTarget::Restricted { dir, relative, .. } => dir.write(relative, contents),
        }
        .map_err(|e| fs_io_error(op, e))
    }

    pub fn append(
        &self,
        op: &str,
        path: impl AsRef<Path>,
        contents: &[u8],
    ) -> Result<(), RuntimeError> {
        match self.fs_target(op, path, FsAccess::Write)? {
            FsTarget::Ambient(path) => std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .and_then(|mut file| file.write_all(contents)),
            FsTarget::Restricted { dir, relative, .. } => {
                let mut options = OpenOptions::new();
                options.create(true).append(true);
                dir.open_with(relative, &options)
                    .and_then(|mut file| file.write_all(contents))
            }
        }
        .map_err(|e| fs_io_error(op, e))
    }

    pub fn open_read(
        &self,
        op: &str,
        path: impl AsRef<Path>,
    ) -> Result<std::fs::File, RuntimeError> {
        match self.fs_target(op, path, FsAccess::Read)? {
            FsTarget::Ambient(path) => std::fs::File::open(path),
            FsTarget::Restricted { dir, relative, .. } => {
                dir.open(relative).map(cap_std::fs::File::into_std)
            }
        }
        .map_err(|e| fs_io_error(op, e))
    }

    pub fn exists(&self, op: &str, path: impl AsRef<Path>) -> Result<bool, RuntimeError> {
        self.metadata_query(op, path, MetadataQuery::Exists)
    }

    pub fn is_file(&self, op: &str, path: impl AsRef<Path>) -> Result<bool, RuntimeError> {
        self.metadata_query(op, path, MetadataQuery::File)
    }

    /// 模块搜索用：沙箱根外的候选视为未命中（继续找），符号链接等策略拒绝仍报错。
    pub fn lookup_is_file(&self, op: &str, path: impl AsRef<Path>) -> Result<bool, RuntimeError> {
        match self.is_file(op, path) {
            Ok(found) => Ok(found),
            Err(err) if is_outside_sandbox_root_error(&err) => Ok(false),
            Err(err) => Err(err),
        }
    }

    pub fn is_dir(&self, op: &str, path: impl AsRef<Path>) -> Result<bool, RuntimeError> {
        self.metadata_query(op, path, MetadataQuery::Dir)
    }

    pub fn create_dir(
        &self,
        op: &str,
        path: impl AsRef<Path>,
        recursive: bool,
    ) -> Result<(), RuntimeError> {
        match self.fs_target(op, path, FsAccess::Write)? {
            FsTarget::Ambient(path) if recursive => std::fs::create_dir_all(path),
            FsTarget::Ambient(path) => std::fs::create_dir(path),
            FsTarget::Restricted { dir, relative, .. } if recursive => dir.create_dir_all(relative),
            FsTarget::Restricted { dir, relative, .. } => dir.create_dir(relative),
        }
        .map_err(|e| fs_io_error(op, e))
    }

    pub fn remove_file(&self, op: &str, path: impl AsRef<Path>) -> Result<(), RuntimeError> {
        match self.fs_target(op, path, FsAccess::Write)? {
            FsTarget::Ambient(path) => std::fs::remove_file(path),
            FsTarget::Restricted { dir, relative, .. } => dir.remove_file(relative),
        }
        .map_err(|e| fs_io_error(op, e))
    }

    pub fn remove_dir_all(&self, op: &str, path: impl AsRef<Path>) -> Result<(), RuntimeError> {
        match self.fs_target(op, path, FsAccess::Write)? {
            FsTarget::Ambient(path) => std::fs::remove_dir_all(path),
            FsTarget::Restricted { dir, relative, .. } => dir.remove_dir_all(relative),
        }
        .map_err(|e| fs_io_error(op, e))
    }

    pub fn read_dir_names(
        &self,
        op: &str,
        path: impl AsRef<Path>,
    ) -> Result<Vec<OsString>, RuntimeError> {
        let entries: Box<dyn Iterator<Item = std::io::Result<OsString>>> =
            match self.fs_target(op, path, FsAccess::Read)? {
                FsTarget::Ambient(path) => Box::new(
                    std::fs::read_dir(path)
                        .map_err(|e| fs_io_error(op, e))?
                        .map(|entry| entry.map(|e| e.file_name())),
                ),
                FsTarget::Restricted { dir, relative, .. } => Box::new(
                    dir.read_dir(relative)
                        .map_err(|e| fs_io_error(op, e))?
                        .map(|entry| entry.map(|e| e.file_name())),
                ),
            };
        entries
            .collect::<std::io::Result<Vec<_>>>()
            .map_err(|e| fs_io_error(op, e))
    }

    pub fn rename(
        &self,
        op: &str,
        from: impl AsRef<Path>,
        to: impl AsRef<Path>,
    ) -> Result<(), RuntimeError> {
        let from = self.fs_target(op, from, FsAccess::Write)?;
        let to = self.fs_target(op, to, FsAccess::Write)?;
        match (from, to) {
            (FsTarget::Ambient(from), FsTarget::Ambient(to)) => std::fs::rename(from, to),
            (
                FsTarget::Restricted {
                    dir: from_dir,
                    relative: from,
                    ..
                },
                FsTarget::Restricted {
                    dir: to_dir,
                    relative: to,
                    ..
                },
            ) => from_dir.rename(from, &to_dir, to),
            _ => Err(std::io::Error::other(
                "cannot mix ambient and capability paths",
            )),
        }
        .map_err(|e| fs_io_error(op, e))
    }

    pub fn copy(
        &self,
        op: &str,
        from: impl AsRef<Path>,
        to: impl AsRef<Path>,
    ) -> Result<u64, RuntimeError> {
        let from = self.fs_target(op, from, FsAccess::Read)?;
        let to = self.fs_target(op, to, FsAccess::Write)?;
        match (from, to) {
            (FsTarget::Ambient(from), FsTarget::Ambient(to)) => std::fs::copy(from, to),
            (
                FsTarget::Restricted {
                    dir: from_dir,
                    relative: from,
                    ..
                },
                FsTarget::Restricted {
                    dir: to_dir,
                    relative: to,
                    ..
                },
            ) => from_dir.copy(from, &to_dir, to),
            _ => Err(std::io::Error::other(
                "cannot mix ambient and capability paths",
            )),
        }
        .map_err(|e| fs_io_error(op, e))
    }

    pub fn canonical_host_path(
        &self,
        op: &str,
        path: impl AsRef<Path>,
    ) -> Result<PathBuf, RuntimeError> {
        match self.fs_target(op, path, FsAccess::Read)? {
            FsTarget::Ambient(path) => match std::fs::canonicalize(&path) {
                Ok(path) => Ok(path),
                Err(_) => ambient_absolute_lexical(&path).map_err(|e| fs_io_error(op, e)),
            },
            FsTarget::Restricted {
                dir,
                relative,
                root_path,
                host_path,
            } => match dir.canonicalize(relative) {
                Ok(relative) => Ok(root_path.join(relative)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(host_path),
                Err(e) => Err(fs_io_error(op, e)),
            },
        }
    }

    fn metadata_query(
        &self,
        op: &str,
        path: impl AsRef<Path>,
        query: MetadataQuery,
    ) -> Result<bool, RuntimeError> {
        match self.fs_target(op, path, FsAccess::Read)? {
            FsTarget::Ambient(path) => match std::fs::metadata(path) {
                Ok(metadata) => Ok(match query {
                    MetadataQuery::Exists => true,
                    MetadataQuery::File => metadata.is_file(),
                    MetadataQuery::Dir => metadata.is_dir(),
                }),
                Err(_) => Ok(false),
            },
            FsTarget::Restricted { dir, relative, .. } => match dir.metadata(relative) {
                Ok(metadata) => Ok(match query {
                    MetadataQuery::Exists => true,
                    MetadataQuery::File => metadata.is_file(),
                    MetadataQuery::Dir => metadata.is_dir(),
                }),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(e) => Err(fs_io_error(op, e)),
            },
        }
    }

    fn fs_target(
        &self,
        op: &str,
        path: impl AsRef<Path>,
        access: FsAccess,
    ) -> Result<FsTarget, RuntimeError> {
        let path = path.as_ref();
        let (read_write, read_only): (&[PathBuf], &[PathBuf]) = match &self.fs {
            FsPolicy::Unrestricted => return Ok(FsTarget::Ambient(path.to_path_buf())),
            FsPolicy::Allow(roots) => (roots, &[]),
            FsPolicy::Scoped {
                read_write,
                read_only,
            } => (read_write, read_only),
        };
        if read_write.is_empty() && read_only.is_empty() {
            return Err(RuntimeError::io_err(format!(
                "{}: filesystem access disabled (sandbox has no allowed paths)",
                crate::value::builtin_repr(op)
            )));
        }

        if path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(path_error(
                op,
                path,
                "outside sandbox roots: parent traversal '..' is not allowed",
            ));
        }
        let absolute = absolute_lexical(path)?;
        let mut best: Option<(usize, bool, PathBuf, PathBuf)> = None;
        for (root, writable) in read_write
            .iter()
            .map(|r| (r, true))
            .chain(read_only.iter().map(|r| (r, false)))
        {
            let root_abs = absolute_lexical(root)?;
            if access == FsAccess::Write
                && !writable
                && lexical_relative_under(&root_abs, &absolute).is_some()
            {
                return Err(path_error(
                    op,
                    path,
                    "path contains a read-only dependency root",
                ));
            }
            if let Some(relative) = lexical_relative_under(&absolute, &root_abs) {
                let specificity = root_abs.components().count();
                let replace = best
                    .as_ref()
                    .is_none_or(|(current, current_writable, _, _)| {
                        specificity > *current
                            || (specificity == *current && !writable && *current_writable)
                    });
                if replace {
                    best = Some((specificity, writable, root_abs, relative));
                }
            }
        }
        if let Some((_, writable, root, relative)) = best {
            if access == FsAccess::Write && !writable {
                return Err(path_error(
                    op,
                    path,
                    "path is under a read-only dependency root",
                ));
            }
            let root = root
                .canonicalize()
                .map_err(|e| path_error(op, path, &format!("cannot resolve sandbox root: {e}")))?;
            let dir = Dir::open_ambient_dir(&root, ambient_authority())
                .map_err(|e| path_error(op, path, &format!("cannot open sandbox root: {e}")))?;
            reject_links_relative_to(&dir, &relative)
                .map_err(|reason| path_error(op, path, &reason))?;
            let host_path = root.join(&relative);
            return Ok(FsTarget::Restricted {
                dir,
                relative,
                root_path: root,
                host_path,
            });
        }
        Err(path_error(op, path, "path is outside sandbox roots"))
    }
}

fn fs_io_error(op: &str, error: std::io::Error) -> RuntimeError {
    RuntimeError::io_err(format!(
        "{} failed: {error}",
        crate::value::builtin_repr(op)
    ))
}

fn path_error(op: &str, path: &Path, reason: &str) -> RuntimeError {
    RuntimeError::io_err(format!(
        "{}: path '{}' rejected by sandbox: {reason}",
        crate::value::builtin_repr(op),
        path.display()
    ))
}

fn is_outside_sandbox_root_error(err: &RuntimeError) -> bool {
    err.message()
        .ends_with("rejected by sandbox: path is outside sandbox roots")
}

fn absolute_lexical(path: &Path) -> Result<PathBuf, RuntimeError> {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| RuntimeError::io_err(format!("cannot read current directory: {e}")))?
            .join(path)
    };
    #[cfg(windows)]
    let joined = strip_windows_verbatim_path(joined);
    let mut result = PathBuf::new();
    for comp in joined.components() {
        match comp {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                return Err(RuntimeError::io_err("sandbox path contains '..'"));
            }
            c => result.push(c.as_os_str()),
        }
    }
    Ok(result)
}

#[cfg(windows)]
fn strip_windows_verbatim_path(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path
    }
}

fn ambient_absolute_lexical(path: &Path) -> std::io::Result<PathBuf> {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut result = PathBuf::new();
    for component in joined.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                result.pop();
            }
            other => result.push(other.as_os_str()),
        }
    }
    Ok(result)
}

#[cfg(not(windows))]
fn lexical_relative_under(path: &Path, root: &Path) -> Option<PathBuf> {
    path.strip_prefix(root).ok().map(Path::to_path_buf)
}

#[cfg(windows)]
fn lexical_relative_under(path: &Path, root: &Path) -> Option<PathBuf> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    fn wide_eq_ignore_ascii_case(a: u16, b: u16) -> bool {
        if a == b {
            return true;
        }
        let fold = |c: u16| {
            if (u16::from(b'A')..=u16::from(b'Z')).contains(&c) {
                c + 32
            } else {
                c
            }
        };
        fold(a) == fold(b)
    }

    fn os_eq_ignore_ascii_case(left: &OsStr, right: &OsStr) -> bool {
        let mut left = left.encode_wide();
        let mut right = right.encode_wide();
        loop {
            match (left.next(), right.next()) {
                (None, None) => return true,
                (Some(a), Some(b)) if wide_eq_ignore_ascii_case(a, b) => {}
                _ => return false,
            }
        }
    }

    let path_components: Vec<_> = path.components().collect();
    let root_components: Vec<_> = root.components().collect();
    if path_components.len() < root_components.len()
        || !path_components
            .iter()
            .zip(&root_components)
            .all(|(left, right)| os_eq_ignore_ascii_case(left.as_os_str(), right.as_os_str()))
    {
        return None;
    }
    let mut relative = PathBuf::new();
    for component in &path_components[root_components.len()..] {
        relative.push(component.as_os_str());
    }
    Some(relative)
}

fn reject_links_relative_to(dir: &Dir, path: &Path) -> std::result::Result<(), String> {
    let mut prefix = PathBuf::new();
    for component in path.components() {
        prefix.push(component.as_os_str());
        match dir.symlink_metadata(&prefix) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "symbolic link, junction, or reparse point is not allowed: {}",
                    prefix.display()
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(format!("cannot inspect {}: {error}", prefix.display())),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_allows_everything() {
        let c = Capabilities::full();
        assert!(c.check_network("get").is_ok());
        assert!(c.check_fs("read_file", "/etc/passwd").is_ok());
        assert!(c.check_env("setenv").is_ok());
    }

    #[test]
    fn sandbox_blocks_network_and_env() {
        let c = Capabilities::sandbox(vec![PathBuf::from(".")]);
        assert!(c.check_network("get").is_err());
        assert!(c.check_env("setenv").is_err());
        assert!(c.check_ffi("frompath").is_err());
    }

    #[test]
    fn full_allows_ffi() {
        assert!(Capabilities::full().check_ffi("extern").is_ok());
    }

    #[test]
    fn sandbox_allows_under_root() {
        let root = std::env::current_dir().unwrap();
        let c = Capabilities::sandbox(vec![root.clone()]);
        let inside = root.join("data.txt");
        assert!(c.check_fs("read_file", &inside.to_string_lossy()).is_ok());
        assert!(c.check_fs("read_file", "./data.txt").is_ok());
    }

    #[test]
    fn sandbox_blocks_dotdot_escape() {
        let root = std::env::current_dir().unwrap();
        let c = Capabilities::sandbox(vec![root.clone()]);
        let escape = root.join("..").join("secret.txt");
        assert!(c.check_fs("read_file", &escape.to_string_lossy()).is_err());
    }

    #[test]
    fn empty_roots_blocks_all_fs() {
        let c = Capabilities::sandbox(vec![]);
        assert!(c.check_fs("read_file", "anything.txt").is_err());
    }

    #[test]
    fn dependency_default_is_read_only_and_no_network() {
        let root = std::env::temp_dir().join("optive-dep-root");
        let _ = std::fs::create_dir_all(&root);
        let host = Capabilities::full();
        let dep = host.restrict_for_dependency(&root);
        assert!(dep.check_network("get").is_err());
        assert!(dep.check_ffi("frompath").is_err());
        assert!(dep.check_env("setenv").is_err());
        assert!(dep.check_process("os.run").is_err());
        assert!(dep
            .check_fs("read_file", &root.join("a.tive").to_string_lossy())
            .is_ok());
        let mut trusted = Capabilities::full();
        trusted.dep_grant.trust_all = true;
        assert!(trusted
            .restrict_for_dependency(&root)
            .check_network("get")
            .is_ok());
    }

    #[test]
    fn lookup_is_file_skips_outside_root_but_is_file_errors() {
        let root =
            std::env::temp_dir().join(format!("optive_lookup_outside_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&root);
        std::fs::write(root.join("yes.tive"), "ok").unwrap();
        let c = Capabilities::sandbox(vec![root.clone()]);
        assert!(c.is_file("is_file", "nope.tive").is_err());
        assert!(!c.lookup_is_file("module lookup", "nope.tive").unwrap());
        assert!(c
            .lookup_is_file("module lookup", root.join("yes.tive"))
            .unwrap());
        assert!(!c
            .lookup_is_file("module lookup", root.join("missing.tive"))
            .unwrap());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn lookup_is_file_rejects_symlink_inside_root() {
        use std::os::unix::fs::symlink;

        let pid = std::process::id();
        let root = std::env::temp_dir().join(format!("optive_lookup_link_{pid}"));
        let outside = std::env::temp_dir().join(format!("optive_lookup_link_out_{pid}"));
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.tive"), "export let x = 1\n").unwrap();
        let link = root.join("evil.tive");
        symlink(outside.join("secret.tive"), &link).unwrap();
        let c = Capabilities::sandbox(vec![root.clone()]);
        let err = c
            .lookup_is_file("module lookup", link)
            .expect_err("symlink inside sandbox root must not look like a regular file");
        assert!(err.message().contains("symbolic link"), "{}", err.message());
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }
}
