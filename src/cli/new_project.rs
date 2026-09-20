//! `Optive new <ProjectName>` / `Optive init` — 创建项目骨架。

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

    fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    write_project_files(&root, &name)?;
    Ok(root)
}

/// 在一个空的既有目录中初始化项目，项目名取目录本身的名称。
pub fn init_project(root: &Path) -> Result<(PathBuf, String), String> {
    if root.join("Optive.toml").exists() {
        return Err(format!(
            "project is already initialized: {}",
            root.display()
        ));
    }

    let mut entries = fs::read_dir(root).map_err(|e| e.to_string())?;
    if entries
        .next()
        .transpose()
        .map_err(|e| e.to_string())?
        .is_some()
    {
        return Err(format!(
            "cannot initialize project: directory is not empty: {}",
            root.display()
        ));
    }
    drop(entries);

    let directory_name = root
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            format!(
                "current directory has no valid UTF-8 name: {}",
                root.display()
            )
        })?;
    let name = validate_project_name(directory_name)?;
    write_project_files(root, &name)?;
    Ok((root.to_path_buf(), name))
}

fn write_project_files(root: &Path, name: &str) -> Result<(), String> {
    fs::create_dir(root.join("src")).map_err(|e| e.to_string())?;

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

    Ok(())
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

    #[test]
    fn initializes_empty_existing_directory_with_its_name() {
        let parent = std::env::temp_dir().join(format!("optive_init_tpl_{}", std::process::id()));
        let root = parent.join("CurrentProject");
        let _ = fs::remove_dir_all(&parent);
        fs::create_dir_all(&root).unwrap();

        let (initialized, name) = init_project(&root).unwrap();
        assert_eq!(initialized, root);
        assert_eq!(name, "CurrentProject");
        assert!(root.join("Optive.toml").is_file());
        assert!(root.join("src/main.tive").is_file());
        assert!(root.join(".gitignore").is_file());

        let _ = fs::remove_dir_all(&parent);
    }

    #[test]
    fn init_rejects_existing_project_and_other_nonempty_directories() {
        let parent =
            std::env::temp_dir().join(format!("optive_init_reject_{}", std::process::id()));
        let project = parent.join("ExistingProject");
        let nonempty = parent.join("NonemptyProject");
        let _ = fs::remove_dir_all(&parent);
        fs::create_dir_all(&project).unwrap();
        fs::write(
            project.join("Optive.toml"),
            "[package]\nname = \"ExistingProject\"\n",
        )
        .unwrap();
        fs::create_dir_all(&nonempty).unwrap();
        fs::write(nonempty.join("notes.txt"), "keep me").unwrap();

        let existing_error = init_project(&project).unwrap_err();
        assert!(
            existing_error.contains("already initialized"),
            "{existing_error}"
        );
        let nonempty_error = init_project(&nonempty).unwrap_err();
        assert!(nonempty_error.contains("not empty"), "{nonempty_error}");
        assert_eq!(
            fs::read_to_string(nonempty.join("notes.txt")).unwrap(),
            "keep me"
        );

        let _ = fs::remove_dir_all(&parent);
    }
}
