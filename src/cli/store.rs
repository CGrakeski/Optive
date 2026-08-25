//! 全局 CAS：`pack/<id>/` + `SQLite` `index.db`。

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};

use super::git_ops;
use super::home;

#[derive(Debug, Clone)]
pub struct PackRecord {
    pub id: String,
    pub path: PathBuf,
    pub source: String,
    pub commit: String,
    pub tree: String,
    pub content_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackIdentity {
    pub id: String,
    pub source: String,
    pub commit: String,
    pub tree: String,
    pub content_digest: String,
}

#[derive(Debug, Clone, Copy)]
pub struct ExpectedPack<'a> {
    pub tree: &'a str,
    pub content_digest: &'a str,
}

#[derive(Debug, Clone)]
pub struct MaterializedPack {
    pub identity: PackIdentity,
    pub path: PathBuf,
    pub fresh: bool,
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// 规范化 git URL 后计算 content id。
pub fn content_id(git_url: &str, effective_rev: &str) -> String {
    let norm = normalize_git_url(git_url);
    let mut hasher = Sha256::new();
    hasher.update(norm.as_bytes());
    hasher.update([0u8]);
    hasher.update(effective_rev.as_bytes());
    hex::encode(hasher.finalize())
}

pub fn normalize_git_url(url: &str) -> String {
    let mut u = url.trim().to_string();
    if u.ends_with('/') {
        u.pop();
    }
    if u.ends_with(".git") {
        u.truncate(u.len() - 4);
    }
    // file://：保留路径大小写（大小写敏感盘上 `/Home/Dep` ≠ `/home/Dep`）。
    if u.starts_with("file:") {
        return u;
    }
    // 网络 / scp-like：整段小写，保证 GitHub.com/Foo 与 github.com/foo 同一 CAS id。
    u.to_ascii_lowercase()
}

pub struct Store {
    conn: Connection,
    home: PathBuf,
}

impl Store {
    pub fn open() -> Result<Self, Box<dyn std::error::Error>> {
        let home = home::optive_home();
        fs::create_dir_all(home.join("pack"))?;
        let db_path = home.join("index.db");
        let conn = Connection::open(&db_path)?;
        conn.busy_timeout(std::time::Duration::from_secs(30))?;
        conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL;")?;
        conn.execute_batch(
            r"
            CREATE TABLE IF NOT EXISTS packs (
              id TEXT PRIMARY KEY,
              git_url TEXT NOT NULL,
              effective_rev TEXT NOT NULL,
              tree_id TEXT,
              content_digest TEXT,
              path TEXT NOT NULL,
              created_at INTEGER NOT NULL,
              last_access INTEGER
            );
            CREATE TABLE IF NOT EXISTS refs (
              project_key TEXT NOT NULL,
              pack_id TEXT NOT NULL REFERENCES packs(id) ON DELETE CASCADE,
              PRIMARY KEY (project_key, pack_id)
            );
            ",
        )?;
        // 旧 store 仅是本机缓存，可就地补列；缺失摘要的旧记录不会被信任，会要求重装。
        for sql in [
            "ALTER TABLE packs ADD COLUMN tree_id TEXT",
            "ALTER TABLE packs ADD COLUMN content_digest TEXT",
        ] {
            if let Err(err) = conn.execute(sql, []) {
                if !err
                    .to_string()
                    .to_ascii_lowercase()
                    .contains("duplicate column")
                {
                    return Err(err.into());
                }
            }
        }
        Ok(Self { conn, home })
    }

    pub fn pack_abs(&self, id: &str) -> PathBuf {
        self.home.join("pack").join(id)
    }

    pub fn lookup(&self, id: &str) -> Result<Option<PackRecord>, Box<dyn std::error::Error>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, git_url, effective_rev, tree_id, content_digest, path FROM packs WHERE id = ?1",
            )?;
        let mut rows = stmt.query(params![id])?;
        if let Some(row) = rows.next()? {
            let path_s: String = row.get(5)?;
            let path = PathBuf::from(&path_s);
            let abs = if path.is_absolute() {
                path
            } else {
                self.home.join(path)
            };
            Ok(Some(PackRecord {
                id: row.get(0)?,
                path: abs,
                source: row.get(1)?,
                commit: row.get(2)?,
                tree: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                content_digest: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
            }))
        } else {
            Ok(None)
        }
    }

    pub fn upsert_pack(
        &self,
        id: &str,
        git_url: &str,
        effective_rev: &str,
        tree: &str,
        content_digest: &str,
        rel_or_abs: &Path,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let path_s = rel_or_abs.to_string_lossy().to_string();
        let ts = now_unix();
        self.conn.execute(
            r"
            INSERT INTO packs (
              id, git_url, effective_rev, tree_id, content_digest, path, created_at, last_access
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
            ON CONFLICT(id) DO UPDATE SET
              git_url=excluded.git_url,
              effective_rev=excluded.effective_rev,
              tree_id=excluded.tree_id,
              content_digest=excluded.content_digest,
              path=excluded.path,
              last_access=excluded.last_access
            ",
            params![id, git_url, effective_rev, tree, content_digest, path_s, ts],
        )?;
        Ok(())
    }

    pub fn touch_access(&self, id: &str) -> Result<(), Box<dyn std::error::Error>> {
        self.conn.execute(
            "UPDATE packs SET last_access = ?1 WHERE id = ?2",
            params![now_unix(), id],
        )?;
        Ok(())
    }

    pub fn set_project_refs(
        &mut self,
        project_key: &str,
        pack_ids: &[String],
    ) -> Result<(), Box<dyn std::error::Error>> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM refs WHERE project_key = ?1",
            params![project_key],
        )?;
        for id in pack_ids {
            tx.execute(
                "INSERT OR IGNORE INTO refs (project_key, pack_id) VALUES (?1, ?2)",
                params![project_key, id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn list_orphans(&self) -> Result<Vec<PackRecord>, Box<dyn std::error::Error>> {
        let mut stmt = self.conn.prepare(
            r"
            SELECT p.id, p.git_url, p.effective_rev, p.tree_id, p.content_digest, p.path
            FROM packs p
            WHERE NOT EXISTS (SELECT 1 FROM refs r WHERE r.pack_id = p.id)
            ",
        )?;
        let rows = stmt.query_map([], |row| {
            let path_s: String = row.get(5)?;
            Ok(PackRecord {
                id: row.get(0)?,
                path: PathBuf::from(path_s),
                source: row.get(1)?,
                commit: row.get(2)?,
                tree: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                content_digest: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            let mut rec = r?;
            if !rec.path.is_absolute() {
                rec.path = self.home.join(&rec.path);
            }
            out.push(rec);
        }
        Ok(out)
    }

    pub fn delete_pack(&mut self, id: &str) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(rec) = self.lookup(id)? {
            if rec.path.exists() {
                fs::remove_dir_all(&rec.path)
                    .map_err(|e| format!("cannot remove pack {}: {e}", rec.path.display()))?;
            }
        }
        self.conn
            .execute("DELETE FROM packs WHERE id = ?1", params![id])?;
        Ok(())
    }
}

/// 确保 `source+commit` 对应的 pack 存在，并在复用前后验证来源、tree 与内容摘要。
pub fn ensure_pack(
    store: &mut Store,
    source: &str,
    commit: &str,
    expected: Option<ExpectedPack<'_>>,
) -> Result<MaterializedPack, Box<dyn std::error::Error>> {
    let source = normalize_git_url(source);
    require_full_object_id("commit", commit)?;
    let commit = commit.to_ascii_lowercase();
    let id = content_id(&source, &commit);
    if let Some(rec) = store.lookup(&id)? {
        if rec.path.is_dir() {
            validate_record(&rec, &source, &commit, expected)?;
            validate_checkout_source(&rec.path, &source)?;
            // checkout 前先验证当前缓存内容，污染缓存不得由 checkout 静默“修好”。
            let before = inspect_pack(&rec.path, &source, &commit)?;
            validate_identity(&before, expected, Some(&rec))?;
            git_ops::checkout_rev(&rec.path, &commit)?;
            let after = inspect_pack(&rec.path, &source, &commit)?;
            validate_identity(&after, expected, Some(&rec))?;
            store.touch_access(&id)?;
            return Ok(MaterializedPack {
                identity: after,
                path: rec.path,
                fresh: false,
            });
        }
        // index 有、pack 无 → 重装
    }

    let target = store.pack_abs(&id);
    if target.exists() {
        return Err(format!(
            "untrusted cache path {} exists without a usable store record; remove it and retry",
            target.display()
        )
        .into());
    }
    // 唯一临时目录，避免并发 ensure 互删共享 `.tmp-{id}`。
    let tmp = store.home.join("pack").join(format!(
        ".tmp-{id}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    if tmp.exists() {
        fs::remove_dir_all(&tmp)?;
    }
    fs::create_dir_all(tmp.parent().expect("tempfile path always has a parent"))?;
    let install_result = (|| -> Result<PackIdentity, Box<dyn std::error::Error>> {
        git_ops::clone_into(&source, &tmp)?;
        validate_checkout_source(&tmp, &source)?;
        git_ops::checkout_rev(&tmp, &commit)?;
        let identity = inspect_pack(&tmp, &source, &commit)?;
        validate_identity(&identity, expected, None)?;
        Ok(identity)
    })();
    let identity = match install_result {
        Ok(identity) => identity,
        Err(err) => {
            let _ = fs::remove_dir_all(&tmp);
            return Err(err);
        }
    };
    // 并发下另一进程可能已装好：丢弃临时目录，但仍按同样规则验证赢家。
    if target.exists() {
        let _ = fs::remove_dir_all(&tmp);
        if let Some(rec) = store.lookup(&id)? {
            if rec.path.is_dir() {
                validate_record(&rec, &source, &commit, expected)?;
                validate_checkout_source(&rec.path, &source)?;
                let winner = inspect_pack(&rec.path, &source, &commit)?;
                validate_identity(&winner, expected, Some(&rec))?;
                store.touch_access(&id)?;
                return Ok(MaterializedPack {
                    identity: winner,
                    path: rec.path,
                    fresh: false,
                });
            }
        }
        return Err(format!(
            "cache race left untrusted pack at {}; remove it and retry",
            target.display()
        )
        .into());
    }
    fs::rename(&tmp, &target)?;
    let installed = inspect_pack(&target, &source, &commit)?;
    validate_identity(&installed, expected, None)?;
    let rel = PathBuf::from("pack").join(&id);
    store.upsert_pack(
        &id,
        &source,
        &commit,
        &identity.tree,
        &identity.content_digest,
        &rel,
    )?;
    Ok(MaterializedPack {
        identity: installed,
        path: target,
        fresh: true,
    })
}

/// `LOCAL_DEPS：装到项目` `deps/<name>/`。
pub fn ensure_local_pack(
    project_deps: &Path,
    name: &str,
    source: &str,
    commit: &str,
    expected: Option<ExpectedPack<'_>>,
) -> Result<MaterializedPack, Box<dyn std::error::Error>> {
    let source = normalize_git_url(source);
    require_full_object_id("commit", commit)?;
    let commit = commit.to_ascii_lowercase();
    let id = content_id(&source, &commit);
    git_ops::validate_dep_dir_name_pub(name)?;
    fs::create_dir_all(project_deps)?;
    let target = project_deps.join(name);
    let marker = target.join(".optive-id");
    if target.is_dir() {
        let ok = fs::read_to_string(&marker)
            .ok()
            .is_some_and(|s| s.trim() == id);
        if ok {
            // 有匹配标记：Git checkout 与手工 fixture 都必须重新验摘要。
            if target.join(".git").is_dir() {
                validate_checkout_source(&target, &source)?;
                let before = inspect_pack(&target, &source, &commit)?;
                validate_identity(&before, expected, None)?;
                git_ops::checkout_rev(&target, &commit)?;
                let after = inspect_pack(&target, &source, &commit)?;
                validate_identity(&after, expected, None)?;
                return Ok(MaterializedPack {
                    identity: after,
                    path: target,
                    fresh: false,
                });
            }
            let identity = inspect_fixture(&target, &source, &commit)?;
            validate_identity(&identity, expected, None)?;
            return Ok(MaterializedPack {
                identity,
                path: target,
                fresh: false,
            });
        }
        if target.join(".git").is_dir() {
            // 真 git 仓库但身份不符 → 重装，避免静默用错远程。
            fs::remove_dir_all(&target)?;
        } else if marker.is_file() {
            // 无 .git 却已有错误 marker：勿盲盖章复用，避免把错 fixture 当成目标 rev。
            return Err(format!(
                "deps/{name} exists with mismatched .optive-id (want {id}); remove the directory or fix the marker"
            )
            .into());
        } else {
            // 无 .git、无 marker 的本地 fixture（测试/手摆 deps/）：盖章后复用。
            fs::write(&marker, &id)?;
            let identity = inspect_fixture(&target, &source, &commit)?;
            validate_identity(&identity, expected, None)?;
            return Ok(MaterializedPack {
                identity,
                path: target,
                fresh: false,
            });
        }
    }
    git_ops::clone_into(&source, &target)?;
    validate_checkout_source(&target, &source)?;
    git_ops::checkout_rev(&target, &commit)?;
    fs::write(&marker, &id)?;
    let identity = inspect_pack(&target, &source, &commit)?;
    validate_identity(&identity, expected, None)?;
    Ok(MaterializedPack {
        identity,
        path: target,
        fresh: true,
    })
}

fn validate_record(
    record: &PackRecord,
    source: &str,
    commit: &str,
    expected: Option<ExpectedPack<'_>>,
) -> Result<(), Box<dyn std::error::Error>> {
    if record.source != source || record.commit != commit {
        return Err(format!(
            "cache source mismatch for {}: store has {}@{}, requested {}@{}",
            record.id, record.source, record.commit, source, commit
        )
        .into());
    }
    if record.tree.is_empty() || record.content_digest.is_empty() {
        return Err(format!(
            "cache record {} predates integrity metadata; remove the cached pack and retry",
            record.id
        )
        .into());
    }
    if let Some(expected) = expected {
        if record.tree != expected.tree || record.content_digest != expected.content_digest {
            return Err(format!(
                "cache metadata mismatch for {}: lock expects tree {} / digest {}, store has {} / {}",
                record.id,
                expected.tree,
                expected.content_digest,
                record.tree,
                record.content_digest
            )
            .into());
        }
    }
    Ok(())
}

fn validate_checkout_source(
    path: &Path,
    expected_source: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let origin = git_ops::origin_fetch_url(path)
        .ok_or_else(|| format!("cached checkout {} has no origin", path.display()))?;
    if !git_ops::git_remotes_equivalent(&origin, expected_source) {
        return Err(format!(
            "cache source mismatch at {}: origin is {origin}, expected {expected_source}",
            path.display()
        )
        .into());
    }
    Ok(())
}

fn inspect_pack(
    path: &Path,
    source: &str,
    commit: &str,
) -> Result<PackIdentity, Box<dyn std::error::Error>> {
    let repo = gix::open(path).map_err(|e| format!("open cached pack {}: {e}", path.display()))?;
    let actual_commit = repo.head_id()?.detach().to_string().to_ascii_lowercase();
    if actual_commit != commit {
        return Err(format!(
            "cache commit mismatch at {}: HEAD is {actual_commit}, expected {commit}",
            path.display()
        )
        .into());
    }
    let tree = repo
        .find_object(repo.head_id()?.detach())?
        .peel_to_tree()?
        .id
        .to_string()
        .to_ascii_lowercase();
    let content_digest = content_digest(path)?;
    Ok(PackIdentity {
        id: content_id(source, commit),
        source: source.to_string(),
        commit: commit.to_string(),
        tree,
        content_digest,
    })
}

fn inspect_fixture(
    path: &Path,
    source: &str,
    commit: &str,
) -> Result<PackIdentity, Box<dyn std::error::Error>> {
    let digest = content_digest(path)?;
    Ok(PackIdentity {
        id: content_id(source, commit),
        source: source.to_string(),
        commit: commit.to_string(),
        // 本地 fixture 没有 Git object database；使用内容 SHA-256 作为开发态 tree 身份。
        tree: digest.clone(),
        content_digest: digest,
    })
}

fn validate_identity(
    identity: &PackIdentity,
    expected: Option<ExpectedPack<'_>>,
    record: Option<&PackRecord>,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(expected) = expected {
        if identity.tree != expected.tree {
            return Err(format!(
                "Git tree mismatch for {}: materialized {}, lock expects {}",
                identity.id, identity.tree, expected.tree
            )
            .into());
        }
        if identity.content_digest != expected.content_digest {
            return Err(format!(
                "content digest mismatch for {}: materialized {}, lock expects {}; cached content may be polluted",
                identity.id, identity.content_digest, expected.content_digest
            )
            .into());
        }
    }
    if let Some(record) = record {
        if identity.tree != record.tree || identity.content_digest != record.content_digest {
            return Err(format!(
                "cache integrity mismatch for {}: materialized tree/digest differ from store metadata; remove the cached pack",
                identity.id
            )
            .into());
        }
    }
    Ok(())
}

fn require_full_object_id(kind: &str, id: &str) -> Result<(), Box<dyn std::error::Error>> {
    if matches!(id.len(), 40 | 64) && id.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(format!("{kind} must be a full 40- or 64-hex Git object id, got `{id}`").into())
    }
}

/// 对物化内容做稳定 SHA-256。忽略 VCS 元数据与 `.optive-id`；包含空目录与可执行位。
pub fn content_digest(root: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let mut files = Vec::new();
    collect_digest_entries(root, root, &mut files)?;
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut hasher = Sha256::new();
    hasher.update(b"optive-content-v2\0");
    for (relative, kind, path) in files {
        hash_field(&mut hasher, relative.as_bytes());
        hasher.update([kind]);
        match kind {
            b'D' => {}
            b'L' => {
                let target = fs::read_link(&path)?;
                let target = target
                    .to_str()
                    .ok_or_else(|| format!("non-UTF-8 symlink target at {}", path.display()))?
                    .replace('\\', "/");
                hash_field(&mut hasher, target.as_bytes());
            }
            _ => {
                let mut file = fs::File::open(&path)?;
                let meta = file.metadata()?;
                hasher.update([u8::from(file_is_executable(&meta))]);
                hasher.update(meta.len().to_le_bytes());
                let mut buffer = [0u8; 64 * 1024];
                loop {
                    let n = file.read(&mut buffer)?;
                    if n == 0 {
                        break;
                    }
                    hasher.update(&buffer[..n]);
                }
            }
        }
    }
    Ok(hex::encode(hasher.finalize()))
}

fn file_is_executable(meta: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        false
    }
}

fn collect_digest_entries(
    root: &Path,
    dir: &Path,
    out: &mut Vec<(String, u8, PathBuf)>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut entries = fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let relative = path
            .strip_prefix(root)?
            .to_str()
            .ok_or_else(|| format!("non-UTF-8 path in package: {}", path.display()))?
            .replace('\\', "/");
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        let ty = fs::symlink_metadata(&path)?.file_type();
        if ty.is_dir() {
            if should_ignore_dir(&name) {
                continue;
            }
            out.push((relative, b'D', path.clone()));
            collect_digest_entries(root, &path, out)?;
        } else if name != ".optive-id" {
            out.push((relative, if ty.is_symlink() { b'L' } else { b'F' }, path));
        }
    }
    Ok(())
}

fn should_ignore_dir(name: &str) -> bool {
    matches!(name, ".git" | ".hg" | ".svn")
}

fn hash_field(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_id_stable() {
        let a = content_id("https://GitHub.com/Foo/Bar.git", "abc");
        let b = content_id("https://github.com/foo/bar", "abc");
        assert_eq!(a, b);
        let c = content_id("https://github.com/foo/bar", "def");
        assert_ne!(a, c);
    }

    #[test]
    fn content_digest_ignores_vcs_not_build_cache() {
        let root = std::env::temp_dir().join(format!(
            "optive-digest-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(root.join("target")).unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.tive"), "print(1)\n").unwrap();
        fs::write(root.join(".git/config"), "noise").unwrap();
        fs::write(root.join("target/out"), "bin").unwrap();
        let first = content_digest(&root).unwrap();
        fs::write(root.join(".git/config"), "changed").unwrap();
        assert_eq!(first, content_digest(&root).unwrap());
        fs::write(root.join("target/out"), "other").unwrap();
        assert_ne!(first, content_digest(&root).unwrap());
        fs::write(root.join("src/main.tive"), "print(2)\n").unwrap();
        assert_ne!(first, content_digest(&root).unwrap());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn content_digest_includes_empty_directories() {
        let root = std::env::temp_dir().join(format!(
            "optive-digest-empty-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.tive"), "print(1)\n").unwrap();
        let first = content_digest(&root).unwrap();
        fs::create_dir_all(root.join("empty")).unwrap();
        assert_ne!(first, content_digest(&root).unwrap());
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn content_digest_includes_executable_bit() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!(
            "optive-digest-exec-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let file = root.join("script.sh");
        fs::write(&file, "echo hi\n").unwrap();
        let first = content_digest(&root).unwrap();
        let mut perms = fs::metadata(&file).unwrap().permissions();
        perms.set_mode(perms.mode() | 0o111);
        fs::set_permissions(&file, perms).unwrap();
        assert_ne!(first, content_digest(&root).unwrap());
        let _ = fs::remove_dir_all(&root);
    }
}
