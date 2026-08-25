//! 工作区文档：解析 `import` / `use` 到源码，供跨文件跳转与补全。
//! 维护增量符号索引（打开的文档）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

use super::symbols::{index_source, FileIndex, Symbol};

static WORKSPACE_ROOTS: OnceLock<RwLock<Vec<PathBuf>>> = OnceLock::new();
static DOC_INDEX: OnceLock<RwLock<HashMap<String, FileIndex>>> = OnceLock::new();

pub fn configure_roots(params: &serde_json::Value) {
    let roots = roots_from_params(params);
    if let Ok(mut configured) = WORKSPACE_ROOTS
        .get_or_init(|| RwLock::new(Vec::new()))
        .write()
    {
        *configured = roots;
    }
}

fn roots_from_params(params: &serde_json::Value) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(uri) = params.get("rootUri").and_then(serde_json::Value::as_str) {
        if let Some(path) = uri_to_path(uri) {
            roots.push(path);
        }
    }
    if let Some(folders) = params
        .get("workspaceFolders")
        .and_then(serde_json::Value::as_array)
    {
        for folder in folders {
            if let Some(uri) = folder.get("uri").and_then(serde_json::Value::as_str) {
                if let Some(path) = uri_to_path(uri) {
                    roots.push(path);
                }
            }
        }
    }
    roots.sort();
    roots.dedup();
    roots
}

#[must_use]
pub fn is_std_spec(spec: &str) -> bool {
    spec == "std" || spec.starts_with("std.")
}

/// 从当前文档解析依赖模块：先查已打开的 `docs`，再读磁盘。
#[must_use]
pub fn load_module(
    from_uri: &str,
    spec: &str,
    docs: &HashMap<String, String>,
) -> Option<(String, String)> {
    if spec.is_empty() || is_std_spec(spec) {
        return None;
    }
    if let Some(joined) = join_uri(from_uri, spec) {
        if let Some(found) = lookup_docs(docs, &joined) {
            return Some(found);
        }
    }
    if let Some(path) = resolve_import_file(from_uri, spec) {
        let uri = path_to_uri(&path);
        if let Some(found) = lookup_docs(docs, &uri) {
            return Some(found);
        }
        if let Ok(text) = workspace_caps().read_to_string("LSP import", &path) {
            return Some((uri, text));
        }
    }
    None
}

#[must_use]
pub fn load_index(
    from_uri: &str,
    spec: &str,
    docs: &HashMap<String, String>,
) -> Option<(String, FileIndex)> {
    let (uri, src) = load_module(from_uri, spec, docs)?;
    if let Some(idx) = cached_index(&uri) {
        return Some((uri, idx));
    }
    Some((uri, index_source(&src)))
}

#[must_use]
pub fn find_export<'a>(idx: &'a FileIndex, name: &str) -> Option<&'a Symbol> {
    idx.exports()
        .into_iter()
        .find(|s| s.name == name)
        .or_else(|| idx.any_def(name))
}

pub fn join_uri(from_uri: &str, spec: &str) -> Option<String> {
    let prefix = "file://";
    let path_part = from_uri.strip_prefix(prefix)?;
    let parent = path_part.rsplit_once('/')?.0;
    let spec = spec.replace('\\', "/");
    let file = if spec.ends_with(".tive") {
        spec
    } else if spec.contains('/') {
        format!("{spec}.tive")
    } else if spec.contains('.') && !spec.starts_with('.') {
        format!("{}.tive", spec.replace('.', "/"))
    } else {
        format!("{spec}.tive")
    };
    Some(format!(
        "file://{}",
        normalize_slash_path(&format!("{parent}/{file}"))
    ))
}

fn normalize_slash_path(p: &str) -> String {
    let leading = p.starts_with('/');
    let mut out: Vec<&str> = Vec::new();
    for part in p.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            out.pop();
            continue;
        }
        out.push(part);
    }
    let body = out.join("/");
    if leading {
        format!("/{body}")
    } else {
        body
    }
}

/// 按 URI 取已打开文档；路径规范化后也能对上 Windows 盘符 / 百分号编码。
#[must_use]
pub fn resolve_doc(docs: &HashMap<String, String>, uri: &str) -> Option<(String, String)> {
    lookup_docs(docs, uri)
}

fn lookup_docs(docs: &HashMap<String, String>, uri: &str) -> Option<(String, String)> {
    if let Some(t) = docs.get(uri) {
        return Some((uri.to_string(), t.clone()));
    }
    let want = uri_to_path(uri)?;
    for (u, t) in docs {
        if let Some(p) = uri_to_path(u) {
            if same_path(&p, &want) {
                return Some((u.clone(), t.clone()));
            }
        }
    }
    None
}

fn same_path(a: &Path, b: &Path) -> bool {
    if let (Ok(ca), Ok(cb)) = (a.canonicalize(), b.canonicalize()) {
        return ca == cb;
    }
    #[cfg(windows)]
    {
        a.to_string_lossy()
            .eq_ignore_ascii_case(&b.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        a == b
    }
}

pub fn resolve_import_file(doc_uri: &str, spec: &str) -> Option<PathBuf> {
    let spec_path = Path::new(spec);
    if spec_path.is_absolute()
        || spec_path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return None;
    }
    let doc = uri_to_path(doc_uri)?;
    let base = doc.parent().unwrap_or(Path::new("."));
    let cand = if spec.ends_with(".tive") {
        base.join(spec)
    } else {
        base.join(format!("{spec}.tive"))
    };
    if cand.is_file() {
        return workspace_checked(cand);
    }
    let with_slash = spec.replace('.', std::path::MAIN_SEPARATOR_STR);
    let cand2 = base.join(format!("{with_slash}.tive"));
    cand2.is_file().then(|| workspace_checked(cand2)).flatten()
}

fn workspace_checked(path: PathBuf) -> Option<PathBuf> {
    let roots = configured_roots()?;
    workspace_checked_with_roots(path, &roots)
}

fn configured_roots() -> Option<Vec<PathBuf>> {
    Some(
        WORKSPACE_ROOTS
            .get_or_init(|| RwLock::new(Vec::new()))
            .read()
            .ok()?
            .clone(),
    )
}

fn workspace_caps() -> crate::caps::Capabilities {
    let roots = configured_roots().unwrap_or_default();
    if roots.is_empty() {
        crate::caps::Capabilities::full()
    } else {
        crate::caps::Capabilities::sandbox(roots)
    }
}

fn workspace_checked_with_roots(path: PathBuf, roots: &[PathBuf]) -> Option<PathBuf> {
    if roots.is_empty() {
        return Some(path);
    }
    let gate = crate::caps::Capabilities::sandbox(roots.to_vec());
    gate.resolve_fs_path("LSP import", path, crate::caps::FsAccess::Read)
        .ok()
}

pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let decoded = percent_decode(rest);
    #[cfg(windows)]
    {
        // `file:///D:/x.tive` → `D:/x.tive`；`file:///tmp/x` 保持相对，避免误剥盘符。
        let path = if decoded.len() >= 3
            && decoded.as_bytes()[0] == b'/'
            && decoded.as_bytes()[1].is_ascii_alphabetic()
            && decoded.as_bytes()[2] == b':'
        {
            &decoded[1..]
        } else if let Some(unc) = decoded.strip_prefix("//") {
            unc
        } else {
            decoded.strip_prefix('/').unwrap_or(&decoded)
        };
        Some(PathBuf::from(path.replace('/', "\\")))
    }
    #[cfg(not(windows))]
    {
        Some(PathBuf::from(decoded))
    }
}

pub fn path_to_uri(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    if s.starts_with('/') {
        format!("file://{s}")
    } else {
        format!("file:///{s}")
    }
}

fn percent_decode(s: &str) -> String {
    let mut out = String::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) =
                u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or(""), 16)
            {
                out.push(v as char);
                i += 3;
                continue;
            }
        }
        out.push(b[i] as char);
        i += 1;
    }
    out
}

fn doc_index() -> &'static RwLock<HashMap<String, FileIndex>> {
    DOC_INDEX.get_or_init(|| RwLock::new(HashMap::new()))
}

pub fn upsert_index(uri: &str, source: &str) {
    let idx = index_source(source);
    if let Ok(mut map) = doc_index().write() {
        map.insert(uri.to_string(), idx);
    }
}

pub fn drop_index(uri: &str) {
    if let Ok(mut map) = doc_index().write() {
        map.remove(uri);
    }
}

#[must_use]
pub fn cached_index(uri: &str) -> Option<FileIndex> {
    doc_index().read().ok()?.get(uri).cloned()
}

#[must_use]
pub fn workspace_symbols(query: &str) -> Vec<(String, Symbol)> {
    let q = query.trim().to_ascii_lowercase();
    let Ok(map) = doc_index().read() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (uri, idx) in map.iter() {
        for s in &idx.symbols {
            if s.container.is_some() {
                continue;
            }
            if q.is_empty() || s.name.to_ascii_lowercase().contains(&q) {
                out.push((uri.clone(), s.clone()));
            }
        }
    }
    out.sort_by(|a, b| a.1.name.cmp(&b.1.name).then_with(|| a.0.cmp(&b.0)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disk_import_cannot_leave_workspace() {
        let base = std::env::temp_dir().join(format!("optive_lsp_gate_{}", std::process::id()));
        let root = base.join("workspace");
        let outside = base.join("outside.tive");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("inside.tive"), "export let x = 1\n").unwrap();
        std::fs::write(&outside, "export let secret = 1\n").unwrap();

        assert!(workspace_checked_with_roots(
            root.join("inside.tive"),
            std::slice::from_ref(&root)
        )
        .is_some());
        assert!(workspace_checked_with_roots(outside, &[root]).is_none());
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn initialize_reads_root_uri_and_workspace_folders() {
        let params = serde_json::json!({
            "rootUri": "file:///tmp/root",
            "workspaceFolders": [
                { "uri": "file:///tmp/other", "name": "other" }
            ]
        });
        let roots = roots_from_params(&params);
        assert_eq!(roots.len(), 2);
        assert!(roots.iter().any(|p| p.ends_with("root")));
        assert!(roots.iter().any(|p| p.ends_with("other")));
    }
}
