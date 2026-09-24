//! Shared module path resolution for `optive build` and the runtime importer.
//!
//! Callers supply their own existence / read probes so the CLI can use the
//! host filesystem while the VM keeps capability checks.

use std::path::{Path, PathBuf};

/// Top-level names that are never user source modules.
pub const BUILTIN_MODULE_ROOTS: &[&str] = &["std"];

#[must_use]
pub fn is_builtin_root(name: &str) -> bool {
    BUILTIN_MODULE_ROOTS.contains(&name)
}

#[must_use]
pub fn is_builtin_or_host_root(
    name: &str,
    host_builtins: impl IntoIterator<Item = impl AsRef<str>>,
) -> bool {
    if is_builtin_root(name) {
        return true;
    }
    host_builtins.into_iter().any(|item| item.as_ref() == name)
}

/// `{root}/{parts}.tive`, `{root}/{parts}/main.tive`, then the same under `src/`.
#[must_use]
pub fn package_module_candidates(root: &Path, parts: &[&str]) -> Vec<PathBuf> {
    if parts.is_empty() {
        return Vec::new();
    }
    let relative = parts.iter().collect::<PathBuf>();
    [root.to_path_buf(), root.join("src")]
        .into_iter()
        .flat_map(|base| {
            [
                base.join(&relative).with_extension("tive"),
                base.join(&relative).join("main.tive"),
            ]
        })
        .collect()
}

/// Declared `[package].entry`, then `src/main.tive`, `main.tive`, `{logical}.tive`.
#[must_use]
pub fn package_entry_candidates(
    root: &Path,
    logical: &str,
    declared_entry: Option<&str>,
) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(entry) = declared_entry {
        paths.push(root.join(entry));
    }
    paths.extend([
        root.join("src/main.tive"),
        root.join("main.tive"),
        root.join(format!("{logical}.tive")),
    ]);
    paths
}

pub fn declared_package_entry(manifest_text: &str) -> Result<Option<String>, String> {
    let document: toml::Value = manifest_text
        .parse()
        .map_err(|error| format!("invalid Optive.toml: {error}"))?;
    let Some(entry) = document
        .get("package")
        .and_then(|package| package.get("entry"))
    else {
        return Ok(None);
    };
    let entry = entry
        .as_str()
        .ok_or_else(|| "invalid [package].entry: expected a string".to_string())?;
    validate_package_entry(entry)?;
    Ok(Some(entry.to_string()))
}

fn validate_package_entry(entry: &str) -> Result<(), String> {
    use std::path::Component;

    if entry.is_empty() || entry.contains('\0') {
        return Err("invalid [package].entry: expected a non-empty relative path".to_string());
    }
    let path = Path::new(entry);
    // Treat both separators and Windows drive prefixes as unsafe on every host,
    // so a manifest cannot become an escape after moving between platforms.
    let bytes = entry.as_bytes();
    let portable_escape = matches!(bytes.first(), Some(b'/') | Some(b'\\'))
        || bytes.get(1) == Some(&b':')
        || entry.split(['/', '\\']).any(|part| part == "..");
    if portable_escape
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::Prefix(_) | Component::RootDir | Component::ParentDir
            )
        })
    {
        return Err(
            "invalid [package].entry: path must stay inside the package directory".to_string(),
        );
    }
    Ok(())
}

/// Relative file imports: the path as written, then `.tive` if it has no extension.
#[must_use]
pub fn file_import_candidates(base_dir: &Path, path: &str) -> Vec<PathBuf> {
    let candidate = base_dir.join(path);
    let mut paths = vec![candidate.clone()];
    if Path::new(path).extension().is_none() && !path.ends_with(".tive") {
        paths.push(candidate.with_extension("tive"));
    }
    paths
}

pub fn first_existing<E>(
    candidates: impl IntoIterator<Item = PathBuf>,
    mut is_file: impl FnMut(&Path) -> Result<bool, E>,
) -> Result<Option<PathBuf>, E> {
    for path in candidates {
        if is_file(&path)? {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

pub fn locate_package_module<E>(
    root: &Path,
    parts: &[&str],
    is_file: impl FnMut(&Path) -> Result<bool, E>,
) -> Result<Option<PathBuf>, E> {
    first_existing(package_module_candidates(root, parts), is_file)
}

pub fn locate_package_entry<E>(
    root: &Path,
    logical: &str,
    declared_entry: Option<&str>,
    is_file: impl FnMut(&Path) -> Result<bool, E>,
) -> Result<Option<PathBuf>, E> {
    first_existing(
        package_entry_candidates(root, logical, declared_entry),
        is_file,
    )
}

pub fn locate_file_import<E>(
    base_dir: &Path,
    path: &str,
    is_file: impl FnMut(&Path) -> Result<bool, E>,
) -> Result<Option<PathBuf>, E> {
    first_existing(file_import_candidates(base_dir, path), is_file)
}

pub fn import_bind_name(path: &str, path_is_string: bool, alias: Option<&str>) -> String {
    if let Some(alias) = alias {
        return alias.to_string();
    }
    if path_is_string {
        return Path::new(path)
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("module")
            .to_string();
    }
    path.split('.').next_back().unwrap_or(path).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_candidates_search_root_and_src() {
        let root = Path::new("/proj");
        let paths = package_module_candidates(root, &["constants"]);
        assert!(paths.iter().any(|path| path.ends_with("constants.tive")));
        assert!(paths
            .iter()
            .any(|path| path.ends_with(Path::new("src").join("constants.tive"))));
    }

    #[test]
    fn file_import_adds_tive_when_missing() {
        let paths = file_import_candidates(Path::new("src"), "helper");
        assert!(paths.iter().any(|path| path.ends_with("helper.tive")));
    }

    #[test]
    fn package_entry_must_be_a_safe_relative_path() {
        assert_eq!(
            declared_package_entry("[package]\nentry = \"src/main.tive\"\n").unwrap(),
            Some("src/main.tive".to_string())
        );
        for entry in ["../outside.tive", "/outside.tive", r"C:\outside.tive"] {
            let manifest = format!("[package]\nentry = {entry:?}\n");
            assert!(
                declared_package_entry(&manifest).is_err(),
                "unsafe entry was accepted: {entry}"
            );
        }
    }

    #[test]
    fn package_entry_rejects_non_string_values() {
        let error = declared_package_entry("[package]\nentry = 42\n").unwrap_err();
        assert!(error.contains("expected a string"));
    }
}
