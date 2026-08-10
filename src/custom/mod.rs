//! 定制包（Custom Pack）：人读文案 + 排版；不影响语言身份与执行语义。

mod active;
mod keys;
mod pack;
mod paths;

pub use active::{active_pack, init_from_env_and_cwd, set_active_pack, ActivePack, TraceDirection};
pub use keys::{CliMsg, Diag, ErrorKindMsg, ParseMsg, ReplMsg};

/// 按当前激活包渲染人读消息。
#[must_use]
pub fn render(diag: &Diag) -> String {
    active_pack().render_diag(diag)
}
pub use pack::{
    list_installed_ids, load_pack_dir, load_pack_staging, CustomPack, Layout, MessageSpec,
    PackLoadError,
};
pub use paths::{custom_dir, global_config_path};

use std::path::{Path, PathBuf};

pub const PROJECT_CUSTOM_FILE: &str = "Custom.toml";
pub const PACK_MANIFEST_FILE: &str = "Custom.toml";
pub const GLOBAL_CONFIG_FILE: &str = "Config.toml";

pub fn parse_use_list(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(std::string::ToString::to_string)
        .collect()
}

/// 1. `cli_override`  2. `OPTIVE_CUSTOM`  3. 项目 Custom.toml  4. 全局 Config.toml
#[must_use]
pub fn resolve_use_chain(cli_override: Option<&str>) -> Vec<String> {
    if let Some(s) = cli_override {
        return parse_use_list(s);
    }
    if let Ok(s) = std::env::var("OPTIVE_CUSTOM") {
        if !s.trim().is_empty() {
            return parse_use_list(&s);
        }
    }
    if let Some(root) = find_project_root() {
        let p = root.join(PROJECT_CUSTOM_FILE);
        if let Ok(ids) = read_project_use(&p) {
            return ids;
        }
    }
    read_global_use().unwrap_or_default()
}

fn find_project_root() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        if dir.join("Optive.toml").is_file() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

mod project_custom_serde {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Default, Deserialize, Serialize)]
    pub struct File {
        #[serde(default, rename = "use")]
        pub use_list: Vec<String>,
    }
}

pub fn read_project_use(path: &Path) -> Result<Vec<String>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let f: project_custom_serde::File =
        toml::from_str(&text).map_err(|e| format!("invalid {}: {e}", path.display()))?;
    Ok(f.use_list)
}

pub fn write_project_use(path: &Path, ids: &[String]) -> Result<(), String> {
    let f = project_custom_serde::File {
        use_list: ids.to_vec(),
    };
    let text = toml::to_string_pretty(&f).map_err(|e| e.to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, text).map_err(|e| e.to_string())
}

#[derive(Debug, Default, serde::Deserialize, serde::Serialize)]
struct GlobalConfig {
    #[serde(default)]
    custom: GlobalCustomSection,
}

#[derive(Debug, Default, serde::Deserialize, serde::Serialize)]
struct GlobalCustomSection {
    #[serde(default, rename = "use")]
    use_list: Vec<String>,
}

pub fn read_global_use() -> Result<Vec<String>, String> {
    let path = global_config_path();
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let f: GlobalConfig =
        toml::from_str(&text).map_err(|e| format!("invalid {}: {e}", path.display()))?;
    Ok(f.custom.use_list)
}

pub fn write_global_use(ids: &[String]) -> Result<(), String> {
    let path = global_config_path();
    let mut f = if path.is_file() {
        let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        toml::from_str(&text).unwrap_or_default()
    } else {
        GlobalConfig::default()
    };
    f.custom.use_list = ids.to_vec();
    let text = toml::to_string_pretty(&f).map_err(|e| e.to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, text).map_err(|e| e.to_string())
}

pub fn build_active_from_ids(ids: &[String]) -> Result<ActivePack, String> {
    let mut pack = CustomPack::builtin_en_us();
    let mut chain = vec!["en-US".to_string()];
    for id in ids {
        if id == "en-US" {
            continue;
        }
        let dir = custom_dir().join(id);
        let overlay = load_pack_dir(&dir).map_err(|e| format!("{id}: {e}"))?;
        pack = pack.merged_with(&overlay);
        chain.push(id.clone());
    }
    Ok(ActivePack { pack, chain })
}

#[must_use]
pub fn project_custom_path() -> Option<PathBuf> {
    find_project_root().map(|r| r.join(PROJECT_CUSTOM_FILE))
}
