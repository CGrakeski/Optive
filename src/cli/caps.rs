//! 运行时能力开关的 CLI 解析：`--sandbox[=DIR]` / `--no-network` / `--allow-path DIR`。
//!
//! 这些标志在 `Optive <script>` / `-c` / `run` / `up` / `debug` / `test` 等运行路径上生效，
//! 构造 [`optive::caps::Capabilities`] 注入 VM，让不可信脚本 / 依赖在受控边界内运行。
//! 其它子命令（`fmt` / `new` / `add` …）不运行用户代码，故不消费这些标志。

use std::path::PathBuf;

use optive::caps::{Capabilities, DepGrant, FsPolicy};

/// 从参数切片中剥离能力标志，返回（能力集，剩余参数）。
///
/// 支持：
/// - `--sandbox`：禁网 + 禁改环境 + 禁 FFI + 文件系统限制在 cwd
/// - `--sandbox=DIR`：同上，但限制在 DIR
/// - `--no-network`：仅禁网
/// - `--allow-path DIR`：把 DIR 加入文件系统允许根（可重复；不改变网络/环境）
/// - `--allow-ffi`：仅在无文件系统限制时允许 FFI；受限模式仍拒绝路径 loader
/// - `--no-ffi`：禁止原生 FFI（即使非 sandbox）
/// - `--trust-deps`：第三方依赖继承入口能力（默认最小权限）
/// - `--allow-dep-network` / `--allow-dep-env` / `--allow-dep-process` / `--allow-dep-ffi`：精细授权依赖
pub fn parse_caps(
    args: &[String],
) -> Result<(Capabilities, Vec<String>), Box<dyn std::error::Error>> {
    let mut no_network = false;
    let mut env_off = false;
    let mut process_off = false;
    let mut ffi_off = false;
    let mut ffi_on = false;
    let mut sandbox = false;
    let mut dep_grant = DepGrant::none();
    let mut roots: Vec<PathBuf> = Vec::new();
    let mut remaining: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--no-network" => no_network = true,
            "--no-ffi" => ffi_off = true,
            "--allow-ffi" => ffi_on = true,
            "--trust-deps" => dep_grant.trust_all = true,
            "--allow-dep-network" => dep_grant.network = true,
            "--allow-dep-env" => dep_grant.env = true,
            "--allow-dep-process" => dep_grant.process = true,
            "--allow-dep-ffi" => dep_grant.ffi = true,
            "--sandbox" => {
                sandbox = true;
                no_network = true;
                env_off = true;
                process_off = true;
                ffi_off = true;
                roots.push(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
            }
            s if s.starts_with("--sandbox=") => {
                sandbox = true;
                no_network = true;
                env_off = true;
                process_off = true;
                ffi_off = true;
                let dir = &s["--sandbox=".len()..];
                roots.push(if dir.is_empty() {
                    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                } else {
                    PathBuf::from(dir)
                });
            }
            "--allow-path" => {
                let dir = args.get(i + 1).ok_or("--allow-path requires a value")?;
                roots.push(PathBuf::from(dir));
                i += 1;
            }
            other => remaining.push(other.to_string()),
        }
        i += 1;
    }

    let mut caps = Capabilities::full();
    if no_network {
        caps.network = false;
    }
    if env_off {
        caps.env = false;
    }
    if process_off {
        caps.process = false;
    }
    let cwd = std::env::current_dir()?;
    for root in &mut roots {
        if !root.is_absolute() {
            *root = cwd.join(&*root);
        }
    }
    for root in &roots {
        if !root.is_dir() {
            return Err(format!("path is not an existing directory: {}", root.display()).into());
        }
    }
    if !roots.is_empty() {
        caps.fs = if sandbox {
            FsPolicy::Scoped {
                read_write: roots,
                read_only: Vec::new(),
            }
        } else {
            FsPolicy::Allow(roots)
        };
    }
    // `--allow-ffi` 打开语言层开关；文件系统受限时 frompath 仍会拒绝路径 loader。
    if ffi_off {
        caps.ffi = false;
    }
    if ffi_on {
        caps.ffi = true;
    }
    caps.dep_grant = dep_grant;
    Ok((caps, remaining))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_and_allow_dep_flags() {
        let args = vec!["--trust-deps".into(), "script.tive".into()];
        let (caps, rest) = parse_caps(&args).unwrap();
        assert!(caps.dep_grant.trust_all);
        assert_eq!(rest, vec!["script.tive"]);

        let args = vec![
            "--allow-dep-network".into(),
            "--allow-dep-env".into(),
            "run".into(),
        ];
        let (caps, _) = parse_caps(&args).unwrap();
        assert!(caps.dep_grant.network);
        assert!(caps.dep_grant.env);
        assert!(!caps.dep_grant.ffi);
        assert!(!caps.dep_grant.process);
        assert!(!caps.dep_grant.trust_all);

        let args = vec!["--allow-dep-process".into(), "run".into()];
        let (caps, _) = parse_caps(&args).unwrap();
        assert!(caps.dep_grant.process);
        assert!(!caps.dep_grant.network);
    }
}
