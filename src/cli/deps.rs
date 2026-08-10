//! 依赖安装入口（委托 [`resolve::ensure_graph`]）。

use super::manifest::Project;
use super::resolve::{self, EnsureOptions, EnsureResult, ResolveMode};

/// `run`：严 lock；返回 `DepMap`。
pub fn ensure_for_run(project: &Project) -> Result<EnsureResult, Box<dyn std::error::Error>> {
    resolve::ensure_graph(
        project,
        EnsureOptions {
            mode: ResolveMode::Run,
            only_root_dep: None,
        },
    )
}

/// `update`。
pub fn ensure_for_update(
    project: &Project,
    only: Option<&str>,
) -> Result<EnsureResult, Box<dyn std::error::Error>> {
    resolve::ensure_graph(
        project,
        EnsureOptions {
            mode: ResolveMode::Update,
            only_root_dep: only.map(std::string::ToString::to_string),
        },
    )
}
