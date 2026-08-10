//! 运行时能力（capability）隔离：限制脚本可访问的宿主资源。
//!
//! 默认 `Capabilities::full()` 放开全部权限（与历史行为一致）。CLI 的
//! `--sandbox` / `--no-network` / `--allow-path` 构造受限 `Capabilities` 注入 VM，
//! 让不可信脚本 / 依赖在受控边界内运行。
//!
//! 这是**尽力而为**的边界：文件路径用词法归一化（解析 `.` / `..`、相对 cwd），
//! 不解析符号链接；网络通过 `std.http` 网关拦截。足以挡住"普通脚本误触 / 依赖
//! 偷联网"，不构成对抗恶意构造路径的强隔离。

use std::path::{Path, PathBuf};

use crate::error::RuntimeError;

/// 文件系统访问策略。
#[derive(Clone, Debug)]
pub enum FsPolicy {
    /// 不限制（默认）。
    Unrestricted,
    /// 仅允许落在给定根目录之下（词法归一化后比较）。
    Allow(Vec<PathBuf>),
}

/// 脚本可用的宿主能力集合。
#[derive(Clone, Debug)]
pub struct Capabilities {
    pub network: bool,
    pub fs: FsPolicy,
    /// 是否允许 `std.os.setenv` / `chdir` 等改变进程环境的操作。
    pub env: bool,
    /// 是否允许 `C.frompath` / `extern` 加载并调用本地动态库。
    pub ffi: bool,
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
            ffi: true,
        }
    }

    /// 沙箱：禁网、禁改环境、禁 FFI、文件系统限制在 `roots` 之下。
    #[must_use]
    pub const fn sandbox(roots: Vec<PathBuf>) -> Self {
        Self {
            network: false,
            fs: FsPolicy::Allow(roots),
            env: false,
            ffi: false,
        }
    }

    /// FFI 网关：`C.frompath` / `extern` 绑定时先过此关。
    pub fn check_ffi(&self, op: &str) -> Result<(), RuntimeError> {
        if self.ffi {
            Ok(())
        } else {
            Err(RuntimeError::io_err(format!(
                "{op}: native FFI disabled (sandbox; pass --allow-ffi to enable)"
            )))
        }
    }

    /// 网络网关：`std.http.*` 调用前先过此关。
    pub fn check_network(&self, op: &str) -> Result<(), RuntimeError> {
        if self.network {
            Ok(())
        } else {
            Err(RuntimeError::io_err(format!(
                "{op}: network access disabled (sandbox / --no-network)"
            )))
        }
    }

    /// 环境改变网关：`setenv` / `chdir` 调用前先过此关。
    pub fn check_env(&self, op: &str) -> Result<(), RuntimeError> {
        if self.env {
            Ok(())
        } else {
            Err(RuntimeError::io_err(format!(
                "{op}: environment mutation disabled (sandbox)"
            )))
        }
    }

    /// 子进程网关：`std.os.run` / `capture`。沙箱默认关闭（与 `env` 同开同关）。
    pub fn check_process(&self, op: &str) -> Result<(), RuntimeError> {
        if self.env {
            Ok(())
        } else {
            Err(RuntimeError::io_err(format!(
                "{op}: process spawn disabled (sandbox)"
            )))
        }
    }

    /// 文件系统网关：所有 `std.fs` / `std.io` 文件操作调用前先过此关。
    /// `op` 为操作名（用于报错），`path` 为用户给出的路径（可相对）。
    pub fn check_fs(&self, op: &str, path: &str) -> Result<(), RuntimeError> {
        let roots = match &self.fs {
            FsPolicy::Unrestricted => return Ok(()),
            FsPolicy::Allow(r) => r,
        };
        if roots.is_empty() {
            return Err(RuntimeError::io_err(format!(
                "{op}: filesystem access disabled (sandbox has no allowed paths)"
            )));
        }
        let normalized = normalize_path(path);
        for root in roots {
            let root_n = normalize_path(root);
            if normalized == root_n || is_under(&normalized, &root_n) {
                return Ok(());
            }
        }
        Err(RuntimeError::io_err(format!(
            "{op}: path '{path}' outside sandbox (allowed roots: {})",
            roots
                .iter()
                .map(|r| r.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )))
    }
}

/// 词法归一化：相对 cwd 解析、折叠 `.` / `..`、保留驱动器前缀与根。
/// 不触碰磁盘（不解析符号链接），故对尚不存在的写路径也安全。
fn normalize_path(p: impl AsRef<Path>) -> PathBuf {
    use std::path::Component;
    let p = p.as_ref();
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(p)
    };
    let mut out: Vec<Component> = Vec::new();
    for comp in joined.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                // 只折叠紧邻的 Normal 段；不跨过 Prefix / RootDir。
                if matches!(out.last(), Some(Component::Normal(_))) {
                    out.pop();
                } else {
                    out.push(comp);
                }
            }
            c => out.push(c),
        }
    }
    let mut result = PathBuf::new();
    for c in out {
        result.push(c.as_os_str());
    }
    result
}

fn is_under(child: &Path, root: &Path) -> bool {
    child.starts_with(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_allows_everything() {
        let c = Capabilities::full();
        assert!(c.check_network("http.get").is_ok());
        assert!(c.check_fs("read_file", "/etc/passwd").is_ok());
        assert!(c.check_env("setenv").is_ok());
    }

    #[test]
    fn sandbox_blocks_network_and_env() {
        let c = Capabilities::sandbox(vec![PathBuf::from(".")]);
        assert!(c.check_network("http.get").is_err());
        assert!(c.check_env("setenv").is_err());
        assert!(c.check_ffi("C.frompath").is_err());
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
}
