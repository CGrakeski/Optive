//! `Optive new <ProjectName>` — 创建项目骨架。

use std::fs;
use std::path::{Path, PathBuf};

/// 校验并规范化项目名（目录名 / `[package].name`）。
pub fn validate_project_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("project name must not be empty".into());
    }
    if name == "." || name == ".." {
        return Err(format!("invalid project name: {name}"));
    }
    if name.contains('/') || name.contains('\\') || name.contains('\0') {
        return Err(format!(
            "project name must be a single path segment, got {name:?}"
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
    {
        return Err(format!(
            "project name may only contain letters, digits, `_`, `-`, `.` (got {name:?})"
        ));
    }
    if name.starts_with('.') {
        return Err("project name must not start with '.'".into());
    }
    Ok(name.to_string())
}

/// 在 `parent` 下创建 `name/` 项目（含 `Optive.toml`、`src/main.tive`）。
pub fn create_project(parent: &Path, name: &str) -> Result<PathBuf, String> {
    let name = validate_project_name(name)?;
    let root = parent.join(&name);
    if root.exists() {
        return Err(format!("directory already exists: {}", root.display()));
    }

    fs::create_dir_all(root.join("src")).map_err(|e| e.to_string())?;

    let manifest = format!(
        r#"[package]
name = "{name}"
entry = "src/main.tive"
# Version is defined by git tags (e.g. v0.1.0), not in this file.

[dependencies]
"#
    );
    fs::write(root.join("Optive.toml"), manifest).map_err(|e| e.to_string())?;

    let main_tive = format!(
        r#"// {name} — entry point
print("Hello from {name}!")
"#
    );
    fs::write(root.join("src/main.tive"), main_tive).map_err(|e| e.to_string())?;

    fs::write(root.join(".gitignore"), "Optive.cache\n/deps/\n.optive/\n")
        .map_err(|e| e.to_string())?;

    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bad_names() {
        assert!(validate_project_name("").is_err());
        assert!(validate_project_name("a/b").is_err());
        assert!(validate_project_name(".hidden").is_err());
        assert!(validate_project_name("ok_Name-1").is_ok());
    }

    #[test]
    fn template_has_no_version_field() {
        let parent = std::env::temp_dir().join(format!("optive_new_tpl_{}", std::process::id()));
        let _ = fs::remove_dir_all(&parent);
        fs::create_dir_all(&parent).unwrap();
        let root = create_project(&parent, "TplDemo").unwrap();
        let text = fs::read_to_string(root.join("Optive.toml")).unwrap();
        assert!(!text.lines().any(|l| l.trim_start().starts_with("version")));
        assert!(text.contains("git tags"));
        let _ = fs::remove_dir_all(&parent);
    }
}
