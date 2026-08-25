//! `Optive.toml` 项目清单。

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const MANIFEST_NAMES: &[&str] = &["Optive.toml"];

/// 依赖修订声明种类（决定是否可被 `update` 追 tip）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevSpec {
    /// 钉死 commit sha
    Commit(String),
    /// 钉死 tag（当快照）
    Tag(String),
    /// 可追 branch
    Branch(String),
    /// 裸 URL：可追默认 tip
    None,
    /// 包名在 toml 键里，值是版本号；git URL 从 index.json 查
    IndexVersion(String),
}

impl RevSpec {
    pub const fn branch_name(&self) -> Option<&str> {
        match self {
            Self::Branch(s) => Some(s.as_str()),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Manifest {
    pub package: Package,
    #[serde(default)]
    pub track_latest: bool,
    #[serde(default)]
    pub dependencies: BTreeMap<String, Dependency>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Package {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 入口脚本；默认依次尝试 `src/main.tive`、`main.tive`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
    /// 本包声明需要的能力；不能自行授予，须由 CLI/宿主授权。
    #[serde(
        default,
        skip_serializing_if = "optive::caps::CapabilityRequest::is_empty"
    )]
    pub capabilities: optive::caps::CapabilityRequest,
    /// 语言语义版本，例如 `0.2`。缺省表示接受当前解释器语言版本。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language_version: Option<String>,
    /// 最低解释器版本，例如 `0.2.0` 或 `>=0.2.0`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_optive: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dependency {
    pub git: String,
    pub rev: RevSpec,
}

impl Dependency {
    fn with_rev(git: impl Into<String>, rev: RevSpec) -> Self {
        Self {
            git: git.into(),
            rev,
        }
    }

    pub fn pinned_commit(git: impl Into<String>, sha: impl Into<String>) -> Self {
        Self::with_rev(git, RevSpec::Commit(sha.into()))
    }

    pub fn with_branch(git: impl Into<String>, branch: impl Into<String>) -> Self {
        Self::with_rev(git, RevSpec::Branch(branch.into()))
    }

    pub fn with_tag(git: impl Into<String>, tag: impl Into<String>) -> Self {
        Self::with_rev(git, RevSpec::Tag(tag.into()))
    }

    pub fn from_index_version(version: impl Into<String>) -> Self {
        Self {
            git: String::new(),
            rev: RevSpec::IndexVersion(version.into()),
        }
    }

    pub fn from_git_version(git: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            git: git.into(),
            rev: RevSpec::IndexVersion(version.into()),
        }
    }
}

impl<'de> Deserialize<'de> for Dependency {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Url(String),
            Table {
                #[serde(default)]
                git: Option<String>,
                #[serde(default)]
                rev: Option<String>,
                #[serde(default)]
                branch: Option<String>,
                #[serde(default)]
                tag: Option<String>,
                #[serde(default)]
                version: Option<String>,
            },
        }
        match Raw::deserialize(deserializer)? {
            Raw::Url(s) => {
                if super::git_ops::looks_like_git_url(&s) {
                    Ok(Self {
                        git: s,
                        rev: RevSpec::None,
                    })
                } else {
                    Ok(Self::from_index_version(s))
                }
            }
            Raw::Table {
                git,
                rev,
                branch,
                tag,
                version,
            } => {
                let git = git.filter(|s| !s.trim().is_empty());
                let set = [
                    rev.is_some(),
                    branch.is_some(),
                    tag.is_some(),
                    version.is_some(),
                ]
                .into_iter()
                .filter(|b| *b)
                .count();
                if set > 1 {
                    return Err(serde::de::Error::custom(
                        "dependency may set only one of rev / branch / tag / version",
                    ));
                }
                match git {
                    None => {
                        if rev.is_some() || branch.is_some() || tag.is_some() {
                            return Err(serde::de::Error::custom(
                                "rev / branch / tag require a git URL",
                            ));
                        }
                        let Some(v) = version else {
                            return Err(serde::de::Error::custom(
                                "dependency table needs git or version",
                            ));
                        };
                        Ok(Self::from_index_version(v))
                    }
                    Some(git) => {
                        let rev = if let Some(r) = rev {
                            RevSpec::Commit(r)
                        } else if let Some(b) = branch {
                            RevSpec::Branch(b)
                        } else if let Some(t) = tag {
                            RevSpec::Tag(t)
                        } else if let Some(v) = version {
                            return Ok(Self::from_git_version(git, v));
                        } else {
                            RevSpec::None
                        };
                        Ok(Self { git, rev })
                    }
                }
            }
        }
    }
}

impl Serialize for Dependency {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeMap;
        match &self.rev {
            RevSpec::None => serializer.serialize_str(&self.git),
            RevSpec::IndexVersion(v) => {
                if self.git.is_empty() {
                    serializer.serialize_str(v)
                } else {
                    let mut m = serializer.serialize_map(Some(2))?;
                    m.serialize_entry("git", &self.git)?;
                    m.serialize_entry("version", v)?;
                    m.end()
                }
            }
            RevSpec::Commit(r) => {
                let mut m = serializer.serialize_map(Some(2))?;
                m.serialize_entry("git", &self.git)?;
                m.serialize_entry("rev", r)?;
                m.end()
            }
            RevSpec::Tag(t) => {
                let mut m = serializer.serialize_map(Some(2))?;
                m.serialize_entry("git", &self.git)?;
                m.serialize_entry("tag", t)?;
                m.end()
            }
            RevSpec::Branch(b) => {
                let mut m = serializer.serialize_map(Some(2))?;
                m.serialize_entry("git", &self.git)?;
                m.serialize_entry("branch", b)?;
                m.end()
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct Project {
    pub root: PathBuf,
    pub manifest_path: PathBuf,
    pub manifest: Manifest,
}

impl Project {
    /// 解析入口 `.tive` 文件的绝对路径（相对 root join，便于显示时 `strip_prefix`）。
    pub fn entry_path_with_caps(
        &self,
        caps: &optive::caps::Capabilities,
    ) -> Result<PathBuf, String> {
        if let Some(entry) = &self.manifest.package.entry {
            use std::path::{Component, Path};
            let entry_path = Path::new(entry);
            if entry_path.is_absolute() {
                return Err(format!(
                    "entry must be relative to package root, got absolute: {entry} (from {})",
                    self.manifest_path.display()
                ));
            }
            if entry_path
                .components()
                .any(|c| matches!(c, Component::ParentDir))
            {
                return Err(format!(
                    "entry must not contain '..': {entry} (from {})",
                    self.manifest_path.display()
                ));
            }
            let p = self.root.join(entry);
            if !caps
                .is_file("project entry", &p)
                .map_err(|e| e.to_string())?
            {
                return Err(format!(
                    "entry not found: {} (from {})",
                    p.display(),
                    self.manifest_path.display()
                ));
            }
            // 纵深防御：能 canonicalize 时再确认未逃出包根（返回值仍用 join 路径，便于相对显示）。
            if !caps.fs_restricted() {
                if let (Ok(canon_root), Ok(canon)) = (self.root.canonicalize(), p.canonicalize()) {
                    if !canon.starts_with(&canon_root) {
                        return Err(format!(
                            "entry escapes package root: {entry} (from {})",
                            self.manifest_path.display()
                        ));
                    }
                }
            }
            return Ok(p);
        }
        for candidate in ["src/main.tive", "main.tive"] {
            let p = self.root.join(candidate);
            if caps
                .is_file("project entry", &p)
                .map_err(|e| e.to_string())?
            {
                return Ok(p);
            }
        }
        Err(format!(
            "no entry script: set [package].entry or add src/main.tive (project {})",
            self.root.display()
        ))
    }

    pub fn deps_dir(&self) -> PathBuf {
        if let Ok(custom) = std::env::var("OPTIVE_DEPS") {
            PathBuf::from(custom)
        } else {
            self.root.join("deps")
        }
    }

    pub fn lock_path(&self) -> PathBuf {
        self.root.join("Optive.lock")
    }

    pub fn cache_path(&self) -> PathBuf {
        self.root.join("Optive.cache")
    }
}

/// 读取包内依赖表。清单不存在 → 空表；存在但无法解析 → 硬错误。
pub fn read_deps_if_exists(package_root: &Path) -> Result<BTreeMap<String, Dependency>, String> {
    for name in MANIFEST_NAMES {
        let p = package_root.join(name);
        if p.is_file() {
            let text =
                fs::read_to_string(&p).map_err(|e| format!("cannot read {}: {e}", p.display()))?;
            let m: Manifest =
                toml::from_str(&text).map_err(|e| format!("invalid {}: {e}", p.display()))?;
            return Ok(m.dependencies);
        }
    }
    Ok(BTreeMap::new())
}

/// 用 `toml_edit` 增量写入单个依赖（尽量保留其它注释/格式）。
pub fn upsert_dependency(manifest_path: &Path, name: &str, dep: &Dependency) -> Result<(), String> {
    let text = fs::read_to_string(manifest_path)
        .map_err(|e| format!("cannot read {}: {e}", manifest_path.display()))?;
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| format!("invalid {}: {e}", manifest_path.display()))?;
    if doc.get("dependencies").is_none() {
        doc["dependencies"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    let deps = doc["dependencies"]
        .as_table_mut()
        .ok_or_else(|| format!("{}: [dependencies] is not a table", manifest_path.display()))?;
    deps.insert(name, dependency_to_item(dep));
    fs::write(manifest_path, doc.to_string())
        .map_err(|e| format!("cannot write {}: {e}", manifest_path.display()))?;
    Ok(())
}

pub fn remove_dependency(manifest_path: &Path, name: &str) -> Result<bool, String> {
    let text = fs::read_to_string(manifest_path)
        .map_err(|e| format!("cannot read {}: {e}", manifest_path.display()))?;
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| format!("invalid {}: {e}", manifest_path.display()))?;
    let Some(deps) = doc.get_mut("dependencies").and_then(|i| i.as_table_mut()) else {
        return Ok(false);
    };
    let removed = deps.remove(name).is_some();
    fs::write(manifest_path, doc.to_string())
        .map_err(|e| format!("cannot write {}: {e}", manifest_path.display()))?;
    Ok(removed)
}

pub fn set_track_latest(manifest_path: &Path, value: bool) -> Result<(), String> {
    let text = fs::read_to_string(manifest_path)
        .map_err(|e| format!("cannot read {}: {e}", manifest_path.display()))?;
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| format!("invalid {}: {e}", manifest_path.display()))?;
    doc["track_latest"] = toml_edit::value(value);
    fs::write(manifest_path, doc.to_string())
        .map_err(|e| format!("cannot write {}: {e}", manifest_path.display()))?;
    Ok(())
}

fn dependency_to_item(dep: &Dependency) -> toml_edit::Item {
    match &dep.rev {
        RevSpec::None => toml_edit::value(dep.git.as_str()),
        RevSpec::IndexVersion(v) => {
            if dep.git.is_empty() {
                toml_edit::value(v.as_str())
            } else {
                let mut t = toml_edit::InlineTable::new();
                t.insert("git", dep.git.as_str().into());
                t.insert("version", v.as_str().into());
                toml_edit::Item::Value(toml_edit::Value::InlineTable(t))
            }
        }
        RevSpec::Commit(r) => {
            let mut t = toml_edit::InlineTable::new();
            t.insert("git", dep.git.as_str().into());
            t.insert("rev", r.as_str().into());
            toml_edit::Item::Value(toml_edit::Value::InlineTable(t))
        }
        RevSpec::Tag(t) => {
            let mut table = toml_edit::InlineTable::new();
            table.insert("git", dep.git.as_str().into());
            table.insert("tag", t.as_str().into());
            toml_edit::Item::Value(toml_edit::Value::InlineTable(table))
        }
        RevSpec::Branch(b) => {
            let mut table = toml_edit::InlineTable::new();
            table.insert("git", dep.git.as_str().into());
            table.insert("branch", b.as_str().into());
            toml_edit::Item::Value(toml_edit::Value::InlineTable(table))
        }
    }
}

/// 从路径定位项目：目录、清单文件，或向上查找。
pub fn find_project(start: Option<&Path>) -> Result<Project, String> {
    let start = match start {
        Some(p) => {
            if p.is_file() {
                let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
                if name == "Optive.toml" {
                    return load_project(p);
                }
                return Err(format!(
                    "expected a project directory or Optive.toml, got file {}",
                    p.display()
                ));
            }
            p.to_path_buf()
        }
        None => std::env::current_dir().map_err(|e| e.to_string())?,
    };

    let mut dir = start.canonicalize().unwrap_or(start);
    loop {
        for name in MANIFEST_NAMES {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return load_project(&candidate);
            }
        }
        if !dir.pop() {
            break;
        }
    }
    Err("Optive.toml not found (searched current and parent directories)".into())
}

pub fn load_project(manifest_path: &Path) -> Result<Project, String> {
    let text = fs::read_to_string(manifest_path)
        .map_err(|e| format!("cannot read {}: {e}", manifest_path.display()))?;
    reject_forbidden_package_version(&text, manifest_path)?;
    let manifest: Manifest =
        toml::from_str(&text).map_err(|e| format!("invalid {}: {e}", manifest_path.display()))?;
    if manifest.package.name.trim().is_empty() {
        return Err(format!(
            "{}: [package].name must not be empty",
            manifest_path.display()
        ));
    }
    if let Some(lang) = &manifest.package.language_version {
        if !optive::versions::language_compatible(lang) {
            return Err(format!(
                "{}: language_version `{lang}` is not compatible with this interpreter ({})",
                manifest_path.display(),
                optive::versions::LANGUAGE_VERSION
            ));
        }
    }
    if let Some(req) = &manifest.package.requires_optive {
        match optive::versions::satisfies_requires_optive(req, env!("CARGO_PKG_VERSION")) {
            Ok(true) => {}
            Ok(false) => {
                return Err(format!(
                    "{}: requires_optive `{req}` is not satisfied by Optive {}",
                    manifest_path.display(),
                    env!("CARGO_PKG_VERSION")
                ))
            }
            Err(e) => {
                return Err(format!(
                    "{}: invalid requires_optive `{req}`: {e}",
                    manifest_path.display()
                ))
            }
        }
    }
    let root = manifest_path
        .parent()
        .map_or_else(|| PathBuf::from("."), std::path::Path::to_path_buf);
    let root = root.canonicalize().unwrap_or(root);
    let root = strip_windows_verbatim(root);
    Ok(Project {
        root,
        manifest_path: strip_windows_verbatim(manifest_path.to_path_buf()),
        manifest,
    })
}

/// `[package].version` 已废除：存在即失败（tag-only，无兼容期）。
fn reject_forbidden_package_version(text: &str, manifest_path: &Path) -> Result<(), String> {
    let val: toml::Value = text
        .parse()
        .map_err(|e| format!("invalid {}: {e}", manifest_path.display()))?;
    if val
        .get("package")
        .and_then(|p| p.as_table())
        .is_some_and(|t| t.contains_key("version"))
    {
        return Err(format!(
            "{}: [package].version is not supported.\n\
             Package version is defined only by git tags. Remove the field, then release with:\n\
               Optive publish <version>",
            manifest_path.display()
        ));
    }
    Ok(())
}

/// Windows `canonicalize` 会得到 `\\?\D:\...`；用户可见路径去掉此前缀。
fn strip_windows_verbatim(path: PathBuf) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        if let Some(unc) = rest.strip_prefix("UNC\\") {
            return PathBuf::from(format!(r"\\{unc}"));
        }
        return PathBuf::from(rest);
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_url_and_table_deps() {
        let src = r#"
[package]
name = "demo"
entry = "src/main.tive"

[dependencies]
helper = "https://github.com/example/helper.git"
other = { git = "https://github.com/example/other", rev = "abc123" }
branched = { git = "https://github.com/example/b", branch = "main" }
tagged = { git = "https://github.com/example/t", tag = "v1" }
indexed = "0.1.2"
indexed_table = { version = "1.2.3" }
git_version = { git = "https://github.com/example/gv.git", version = "^0.1.0" }
"#;
        let m: Manifest = toml::from_str(src).unwrap();
        assert_eq!(m.package.name, "demo");
        assert_eq!(m.package.entry.as_deref(), Some("src/main.tive"));
        assert_eq!(
            m.dependencies["helper"].git,
            "https://github.com/example/helper.git"
        );
        assert!(matches!(m.dependencies["helper"].rev, RevSpec::None));
        assert!(matches!(
            m.dependencies["other"].rev,
            RevSpec::Commit(ref s) if s == "abc123"
        ));
        assert!(matches!(
            m.dependencies["branched"].rev,
            RevSpec::Branch(ref s) if s == "main"
        ));
        assert!(matches!(
            m.dependencies["tagged"].rev,
            RevSpec::Tag(ref s) if s == "v1"
        ));
        assert!(matches!(
            m.dependencies["indexed"].rev,
            RevSpec::IndexVersion(ref s) if s == "0.1.2"
        ));
        assert!(m.dependencies["indexed"].git.is_empty());
        assert!(matches!(
            m.dependencies["indexed_table"].rev,
            RevSpec::IndexVersion(ref s) if s == "1.2.3"
        ));
        assert_eq!(
            m.dependencies["git_version"].git,
            "https://github.com/example/gv.git"
        );
        assert!(matches!(
            m.dependencies["git_version"].rev,
            RevSpec::IndexVersion(ref s) if s == "^0.1.0"
        ));
        let round = toml::to_string(&m).unwrap();
        assert!(
            round.contains("version") && round.contains("github.com/example/gv"),
            "git+version must round-trip git URL, got:\n{round}"
        );
    }

    #[test]
    fn package_version_field_is_rejected() {
        let dir = std::env::temp_dir().join(format!("optive_forbid_ver_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Optive.toml");
        fs::write(
            &path,
            r#"
[package]
name = "demo"
version = "0.1.0"
entry = "src/main.tive"
"#,
        )
        .unwrap();
        let err = load_project(&path).unwrap_err();
        assert!(err.contains("[package].version is not supported"), "{err}");
        assert!(err.contains("Optive publish"), "{err}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn language_version_and_requires_optive_are_validated() {
        let dir = std::env::temp_dir().join(format!("optive_langver_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Optive.toml");
        fs::write(
            &path,
            r#"
[package]
name = "demo"
language_version = "0.1"
"#,
        )
        .unwrap();
        let err = load_project(&path).unwrap_err();
        assert!(err.contains("language_version"), "{err}");
        fs::write(
            &path,
            format!(
                r#"
[package]
name = "demo"
language_version = "{}"
requires_optive = ">=99.0.0"
"#,
                optive::versions::LANGUAGE_VERSION
            ),
        )
        .unwrap();
        let err = load_project(&path).unwrap_err();
        assert!(err.contains("requires_optive"), "{err}");
        fs::write(
            &path,
            format!(
                r#"
[package]
name = "demo"
language_version = "{}"
requires_optive = ">={}"
"#,
                optive::versions::LANGUAGE_VERSION,
                env!("CARGO_PKG_VERSION")
            ),
        )
        .unwrap();
        assert!(load_project(&path).is_ok());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_deps_missing_is_empty() {
        let dir = std::env::temp_dir().join(format!("optive_no_toml_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        assert!(read_deps_if_exists(&dir).unwrap().is_empty());
        let _ = fs::remove_dir_all(&dir);
    }
}
