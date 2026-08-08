//! 定制包路径（与 `cli/home` 对齐，供库内使用）。

use std::env;
use std::path::PathBuf;

use super::GLOBAL_CONFIG_FILE;

fn optive_home() -> PathBuf {
    if let Ok(home) = env::var("OPTIVE_HOME") {
        let p = PathBuf::from(home);
        if !p.as_os_str().is_empty() {
            return p;
        }
    }
    const PKG_NAME: &str = env!("CARGO_PKG_NAME");
    if let Ok(exe) = env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("data");
            let stem_matches = exe
                .file_stem()
                .map(|s| s.to_string_lossy().eq_ignore_ascii_case(PKG_NAME))
                .unwrap_or(false);
            if candidate.is_dir() || stem_matches {
                return candidate;
            }
        }
    }
    if let Some(home) = env::var_os("HOME").or_else(|| env::var_os("USERPROFILE")) {
        return PathBuf::from(home).join(".optive");
    }
    std::env::temp_dir().join(".optive")
}

pub fn custom_dir() -> PathBuf {
    optive_home().join("custom")
}

pub fn global_config_path() -> PathBuf {
    optive_home().join(GLOBAL_CONFIG_FILE)
}
