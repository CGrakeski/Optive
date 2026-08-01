//! 运行时能力开关的 CLI 解析：`--sandbox[=DIR]` / `--no-network` / `--allow-path DIR`。
//!
//! 这些标志在 `Optive <script>` / `-c` / `run` / `up` / `debug` 等运行路径上生效，
//! 构造 [`optive::caps::Capabilities`] 注入 VM，让不可信脚本 / 依赖在受控边界内运行。
//! 其它子命令（`fmt` / `new` / `add` …）不运行用户代码，故不消费这些标志。

use std::path::PathBuf;

use optive::caps::{Capabilities, FsPolicy};

/// 从参数切片中剥离能力标志，返回（能力集，剩余参数）。
///
/// 支持：
/// - `--sandbox`：禁网 + 禁改环境 + 文件系统限制在 cwd
/// - `--sandbox=DIR`：同上，但限制在 DIR
/// - `--no-network`：仅禁网
/// - `--allow-path DIR`：把 DIR 加入文件系统允许根（可重复；不改变网络/环境）
pub fn parse_caps(args: &[String]) -> Result<(Capabilities, Vec<String>), Box<dyn std::error::Error>> {
    let mut no_network = false;
    let mut env_off = false;
    let mut roots: Vec<PathBuf> = Vec::new();
    let mut remaining: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--no-network" => no_network = true,
            "--sandbox" => {
                no_network = true;
                env_off = true;
                roots.push(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
            }
            s if s.starts_with("--sandbox=") => {
                no_network = true;
                env_off = true;
                let dir = &s["--sandbox=".len()..];
                roots.push(if dir.is_empty() {
                    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                } else {
                    PathBuf::from(dir)
                });
            }
            "--allow-path" => {
                let dir = args
                    .get(i + 1)
                    .ok_or("--allow-path requires a value")?;
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
    if !roots.is_empty() {
        caps.fs = FsPolicy::Allow(roots);
    }
    Ok((caps, remaining))
}
