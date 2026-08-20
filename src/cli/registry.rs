//! 包索引：`index/index.json`（`optive index sync` 拉下来的注册表）。

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::PathBuf;

/// 索引仓库本地目录（默认可执行文件旁的 `index/`；`OPTIVE_INDEX` 可覆盖）。
pub fn index_dir() -> PathBuf {
    if let Ok(home) = env::var("OPTIVE_INDEX") {
        let p = PathBuf::from(home);
        if !p.as_os_str().is_empty() {
            return p;
        }
    }
    env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("index")))
        .unwrap_or_else(|| PathBuf::from("index"))
}

pub fn index_json_path() -> PathBuf {
    index_dir().join("index.json")
}

/// `index.json`：`{ "pack_name": "git-url", ... }`
pub fn load_pack_index() -> Result<BTreeMap<String, String>, Box<dyn std::error::Error>> {
    let path = index_json_path();
    if !path.is_file() {
        return Err(format!(
            "index.json not found at {}; run `Optive index sync` (default: gitee.com/CGrakeski/optindex), or put one there / `Optive index change <url>`",
            path.display()
        )
        .into());
    }
    let text = fs::read_to_string(&path).map_err(|e| {
        format!("cannot read {}: {e}", path.display())
    })?;
    parse_pack_index(&text).map_err(|e| {
        format!("invalid {}: {e}", path.display()).into()
    })
}

pub fn parse_pack_index(text: &str) -> Result<BTreeMap<String, String>, String> {
    serde_json::from_str(text).map_err(|e| e.to_string())
}

/// 按包名查 git URL。
pub fn lookup_pack_url(name: &str) -> Result<String, Box<dyn std::error::Error>> {
    let map = load_pack_index()?;
    map.get(name)
        .cloned()
        .ok_or_else(|| {
            let known: Vec<&str> = map.keys().map(String::as_str).collect();
            let shown = if known.len() > 8 {
                format!("{}… ({} packs)", known[..8].join(", "), known.len())
            } else if known.is_empty() {
                "(empty index)".into()
            } else {
                known.join(", ")
            };
            format!(
                "pack `{name}` not found in {}\n  \
                 this file is the local checkout (not fetched on `up`/`add`).\n  \
                 run `Optive index sync` to refresh from the configured remote\n  \
                 (default: https://gitee.com/CGrakeski/optindex.git).\n  \
                 packs currently listed: {shown}",
                index_json_path().display()
            )
            .into()
        })
}

/// 按包名子串搜索（大小写不敏感）。`query` 为 `None` 或空则返回全部。
pub fn search_packs(
    query: Option<&str>,
) -> Result<Vec<(String, String)>, Box<dyn std::error::Error>> {
    let map = load_pack_index()?;
    let q = query.map(str::trim).filter(|s| !s.is_empty());
    let mut out: Vec<(String, String)> = match q {
        None => map.into_iter().collect(),
        Some(q) => {
            let ql = q.to_ascii_lowercase();
            map.into_iter()
                .filter(|(name, _)| name.to_ascii_lowercase().contains(&ql))
                .collect()
        }
    };
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_flat_name_to_url() {
        let map = parse_pack_index(
            r#"{ "selfoptive": "file:///E:/OptivePlayground/SelfOptive", "greeter": "https://example.com/greeter.git" }"#,
        )
        .unwrap();
        assert_eq!(
            map.get("selfoptive").map(String::as_str),
            Some("file:///E:/OptivePlayground/SelfOptive")
        );
        assert_eq!(
            map.get("greeter").map(String::as_str),
            Some("https://example.com/greeter.git")
        );
    }

    #[test]
    fn search_filters_case_insensitive() {
        let map = parse_pack_index(
            r#"{ "SelfOptive": "https://a.git", "greeter": "https://b.git", "other": "https://c.git" }"#,
        )
        .unwrap();
        let ql = "optive";
        let hits: Vec<_> = map
            .iter()
            .filter(|(n, _)| n.to_ascii_lowercase().contains(ql))
            .map(|(n, u)| (n.clone(), u.clone()))
            .collect();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, "SelfOptive");
    }
}
