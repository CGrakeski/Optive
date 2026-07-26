//! 全局 CAS：`pack/<id>/` + SQLite `index.db`。

use std::fs;
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
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
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
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS packs (
              id TEXT PRIMARY KEY,
              git_url TEXT NOT NULL,
              effective_rev TEXT NOT NULL,
              path TEXT NOT NULL,
              created_at INTEGER NOT NULL,
              last_access INTEGER
            );
            CREATE TABLE IF NOT EXISTS refs (
              project_key TEXT NOT NULL,
              pack_id TEXT NOT NULL REFERENCES packs(id) ON DELETE CASCADE,
              PRIMARY KEY (project_key, pack_id)
            );
            "#,
        )?;
        Ok(Self { conn, home })
    }

    pub fn pack_abs(&self, id: &str) -> PathBuf {
        self.home.join("pack").join(id)
    }

    pub fn lookup(&self, id: &str) -> Result<Option<PackRecord>, Box<dyn std::error::Error>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, git_url, effective_rev, path FROM packs WHERE id = ?1",
        )?;
        let mut rows = stmt.query(params![id])?;
        if let Some(row) = rows.next()? {
            let path_s: String = row.get(3)?;
            let path = PathBuf::from(&path_s);
            let abs = if path.is_absolute() {
                path
            } else {
                self.home.join(path)
            };
            Ok(Some(PackRecord {
                id: row.get(0)?,
                path: abs,
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
        rel_or_abs: &Path,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let path_s = rel_or_abs.to_string_lossy().to_string();
        let ts = now_unix();
        self.conn.execute(
            r#"
            INSERT INTO packs (id, git_url, effective_rev, path, created_at, last_access)
            VALUES (?1, ?2, ?3, ?4, ?5, ?5)
            ON CONFLICT(id) DO UPDATE SET
              git_url=excluded.git_url,
              effective_rev=excluded.effective_rev,
              path=excluded.path,
              last_access=excluded.last_access
            "#,
            params![id, git_url, effective_rev, path_s, ts],
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
        tx.execute("DELETE FROM refs WHERE project_key = ?1", params![project_key])?;
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
            r#"
            SELECT p.id, p.git_url, p.effective_rev, p.path
            FROM packs p
            WHERE NOT EXISTS (SELECT 1 FROM refs r WHERE r.pack_id = p.id)
            "#,
        )?;
        let rows = stmt.query_map([], |row| {
            let path_s: String = row.get(3)?;
            Ok(PackRecord {
                id: row.get(0)?,
                path: PathBuf::from(path_s),
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
                let _ = fs::remove_dir_all(&rec.path);
            }
        }
        self.conn
            .execute("DELETE FROM packs WHERE id = ?1", params![id])?;
        Ok(())
    }
}

/// 确保 `git+rev` 对应的 pack 存在；返回绝对包根路径。
pub fn ensure_pack(
    store: &mut Store,
    git_url: &str,
    effective_rev: &str,
) -> Result<(String, PathBuf, bool), Box<dyn std::error::Error>> {
    let id = content_id(git_url, effective_rev);
    if let Some(rec) = store.lookup(&id)? {
        if rec.path.is_dir() {
            store.touch_access(&id)?;
            // 仍对齐 rev（tag/commit）
            if let Err(e) = git_ops::checkout_rev(&rec.path, effective_rev) {
                // tip-only 裸 sha 应对齐；失败则继续使用现有树
                eprintln!("warning: checkout {effective_rev} in {}: {e}", rec.path.display());
            }
            return Ok((id, rec.path, false));
        }
        // index 有、pack 无 → 重装
    }

    let target = store.pack_abs(&id);
    if target.exists() {
        fs::remove_dir_all(&target)?;
    }
    // 克隆到临时目录再改名，避免半成品
    let tmp = store.home.join("pack").join(format!(".tmp-{id}"));
    if tmp.exists() {
        fs::remove_dir_all(&tmp)?;
    }
    fs::create_dir_all(tmp.parent().expect("tempfile path always has a parent"))?;
    git_ops::clone_into(git_url, &tmp)?;
    git_ops::checkout_rev(&tmp, effective_rev)?;
    fs::rename(&tmp, &target)?;
    let rel = PathBuf::from("pack").join(&id);
    store.upsert_pack(&id, git_url, effective_rev, &rel)?;
    Ok((id, target, true))
}

/// LOCAL_DEPS：装到项目 `deps/<name>/`。
pub fn ensure_local_pack(
    project_deps: &Path,
    name: &str,
    git_url: &str,
    effective_rev: &str,
) -> Result<(String, PathBuf, bool), Box<dyn std::error::Error>> {
    let id = content_id(git_url, effective_rev);
    git_ops::validate_dep_dir_name_pub(name)?;
    fs::create_dir_all(project_deps)?;
    let target = project_deps.join(name);
    if target.is_dir() {
        git_ops::checkout_rev(&target, effective_rev).ok();
        return Ok((id, target, false));
    }
    git_ops::clone_into(git_url, &target)?;
    git_ops::checkout_rev(&target, effective_rev)?;
    Ok((id, target, true))
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
}
