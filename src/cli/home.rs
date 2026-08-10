//! Optive 全局数据根（`$OPTIVE_HOME` 或安装布局旁的 `data/`）。

use std::env;
use std::path::PathBuf;

/// 解析当前实际使用的全局安装根。
pub fn optive_home() -> PathBuf {
    if let Ok(home) = env::var("OPTIVE_HOME") {
        let p = PathBuf::from(home);
        if !p.as_os_str().is_empty() {
            return p;
        }
    }
    default_home()
}

fn default_home() -> PathBuf {
    // 相对当前可执行文件：`<exe_dir>/../data`（安装布局）或用户目录。
    // 安装布局判定：data 目录已存在，或当前可执行文件名与包名一致（大小写不敏感）。
    const PKG_NAME: &str = env!("CARGO_PKG_NAME");
    if let Ok(exe) = env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("data");
            let stem_matches = exe
                .file_stem()
                .is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(PKG_NAME));
            if candidate.is_dir() || stem_matches {
                return candidate;
            }
        }
    }
    if let Some(home) = env::var_os("HOME").or_else(|| env::var_os("USERPROFILE")) {
        return PathBuf::from(home).join(".optive");
    }
    // 无 HOME/USERPROFILE：退到系统临时目录，避免在任意 cwd 静默落盘。
    std::env::temp_dir().join(".optive")
}

pub fn pack_dir() -> PathBuf {
    optive_home().join("pack")
}

pub fn custom_dir() -> PathBuf {
    optive_home().join("custom")
}

pub fn global_config_path() -> PathBuf {
    optive_home().join("Config.toml")
}

pub fn index_db_path() -> PathBuf {
    optive_home().join("index.db")
}

pub fn use_local_deps() -> bool {
    matches!(
        env::var("OPTIVE_USE_LOCAL_DEPS").as_deref(),
        Ok("1" | "true" | "TRUE" | "yes" | "YES")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_respects_env() {
        env::set_var("OPTIVE_HOME", "/tmp/optive_test_home_xyz");
        assert_eq!(optive_home(), PathBuf::from("/tmp/optive_test_home_xyz"));
        env::remove_var("OPTIVE_HOME");
    }
}
