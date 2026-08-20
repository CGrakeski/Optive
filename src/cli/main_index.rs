//! `Optive index sync` / `index change <url>`：同步或更换包索引 Git 仓库。
//!
//! 默认官方索引：`https://gitee.com/CGrakeski/optindex.git`。
//! 可用 `OPTIVE_INDEX_URL` / `index.url` 覆盖；本地 `OPTIVE_INDEX/index.json` 也可直接给 `search` / `add`。

use std::error::Error;
use std::fs;

use super::git_ops;
use super::home;
use super::registry;

/// 未配置时使用的官方包索引远程。
pub const DEFAULT_INDEX_URL: &str = "https://gitee.com/CGrakeski/optindex.git";

/// 持久化的索引远程 URL（`$OPTIVE_HOME/index.url`）。
pub fn index_url_config_path() -> std::path::PathBuf {
    home::optive_home().join("index.url")
}

/// 索引 URL 来源（供 `doctor` / `env` 展示）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexUrlSource {
    Env,
    File,
    Default,
}

impl IndexUrlSource {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Env => "OPTIVE_INDEX_URL",
            Self::File => "index.url",
            Self::Default => "default (gitee optindex)",
        }
    }
}

/// 解析当前要用的索引远程：`OPTIVE_INDEX_URL` > `index.url` 文件 > 官方默认。
pub fn resolve_index_url() -> Result<(String, IndexUrlSource), Box<dyn Error>> {
    if let Ok(u) = std::env::var("OPTIVE_INDEX_URL") {
        let t = u.trim();
        if !t.is_empty() {
            return Ok((t.to_string(), IndexUrlSource::Env));
        }
    }
    let path = index_url_config_path();
    if path.is_file() {
        let text = fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        if let Some(url) = text
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with('#'))
        {
            return Ok((url.to_string(), IndexUrlSource::File));
        }
        return Err(format!(
            "empty index URL in {}; run `Optive index change <url>` or remove the file",
            path.display()
        )
        .into());
    }
    Ok((DEFAULT_INDEX_URL.to_string(), IndexUrlSource::Default))
}

/// 当前索引远程 URL（忽略来源）。
#[allow(dead_code)] // 公开 API；doctor 等走 `resolve_index_url`
pub fn configured_index_url() -> Result<String, Box<dyn Error>> {
    Ok(resolve_index_url()?.0)
}

/// 校验并写入 `index.url`（不触发同步）。
pub fn set_index_url(url: &str) -> Result<(), Box<dyn Error>> {
    let url = url.trim();
    git_ops::validate_git_url(url)?;
    let path = index_url_config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    fs::write(&path, format!("{url}\n"))
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(())
}

fn sync_from(url: &str) -> Result<(), Box<dyn Error>> {
    let index_path = registry::index_dir();
    println!("Syncing index from {url}");
    println!("  -> {}", index_path.display());
    if git_ops::should_replace_checkout(&index_path, url)? {
        if let Some(old) = git_ops::origin_fetch_url(&index_path) {
            println!("Local index origin is {old}; replacing checkout…");
        } else {
            println!("Local index is not the configured remote; replacing checkout…");
        }
        git_ops::remove_checkout(&index_path)?;
    }
    git_ops::force_clone_or_sync(url, index_path.as_path())
}

pub fn sync_index() -> Result<(), Box<dyn Error>> {
    let (url, src) = resolve_index_url()?;
    if src == IndexUrlSource::Default {
        println!("Using default index remote ({})", src.label());
    }
    sync_from(&url)?;
    println!("Sync success!");
    Ok(())
}

/// 更换索引 Git 远程：写入配置后立即同步到本地 `index/`。
pub fn change_index(url: &str) -> Result<(), Box<dyn Error>> {
    let url = url.trim();
    set_index_url(url)?;
    println!("Index URL set to {url}");
    println!("  (saved: {})", index_url_config_path().display());
    sync_from(url)?;
    println!("Sync success!");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn set_and_read_index_url_roundtrip() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!(
            "optive_index_url_test_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        std::env::set_var("OPTIVE_HOME", &tmp);
        std::env::remove_var("OPTIVE_INDEX_URL");

        set_index_url("https://example.com/optindex.git").unwrap();
        assert_eq!(
            configured_index_url().unwrap(),
            "https://example.com/optindex.git"
        );
        assert_eq!(
            resolve_index_url().unwrap().1,
            IndexUrlSource::File
        );

        std::env::set_var("OPTIVE_INDEX_URL", "file:///tmp/other-index");
        assert_eq!(configured_index_url().unwrap(), "file:///tmp/other-index");
        assert_eq!(resolve_index_url().unwrap().1, IndexUrlSource::Env);

        std::env::remove_var("OPTIVE_INDEX_URL");
        std::env::remove_var("OPTIVE_HOME");
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn unset_falls_back_to_default() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!(
            "optive_index_url_default_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        std::env::set_var("OPTIVE_HOME", &tmp);
        std::env::remove_var("OPTIVE_INDEX_URL");

        let (url, src) = resolve_index_url().unwrap();
        assert_eq!(url, DEFAULT_INDEX_URL);
        assert_eq!(src, IndexUrlSource::Default);

        std::env::remove_var("OPTIVE_HOME");
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn reject_non_git_url() {
        let _g = ENV_LOCK.lock().unwrap();
        let err = set_index_url("not-a-url").unwrap_err();
        assert!(err.to_string().contains("unsupported git URL"));
    }
}
