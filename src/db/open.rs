//! Read-only index open path: the skew/corruption gate chain shared by the
//! CLI, MCP, audit, and tests. No write primitives live here (write-path
//! gate: only top-level `db.rs` may write).

use std::fs;
use std::path::Path;

use rusqlite::{Connection, OpenFlags};

use super::paths::{db_path, eg_dir};
use super::security::{reject_symlink, reject_tracked_eg_dir};
use super::SCHEMA_VERSION;
use crate::error::{EgError, Result};

/// Open the index for querying with the full skew/corruption gate chain:
/// missing -> exit 1 (CUJ7), corrupt -> exit 3 (CUJ3), skew -> exit 5 (CUJ5).
/// Amendment 11a: no `PRAGMA quick_check` here — it is a full B-tree scan,
/// linear in DB size, on every open (CLI and per-MCP-tool-call). Corruption
/// still exits 3: SQLITE_NOTADB surfaces on the first statement against a
/// non-SQLite file, SQLITE_CORRUPT when a query hits a malformed page, and
/// both map to the contract message via `EgError::from(rusqlite::Error)`.
/// Deep integrity remains `eg doctor` (`PRAGMA integrity_check`).
pub fn open_for_query(root: &Path) -> Result<Connection> {
    // A cloned repository can ship an attacker-authored index. Refuse tracked
    // state before reading any row, not only before writing `.engram/`.
    reject_tracked_eg_dir(root)?;
    reject_symlink(&eg_dir(root), ".engram directory")?;
    let path = db_path(root);
    reject_symlink(&path, "index.db")?;
    if !path.exists() {
        return Err(EgError::NoResults(format!(
            "no .engram/index.db under {} — run eg build",
            root.display()
        )));
    }
    let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    check_schema_version(&conn)?;
    let _ = conn.execute_batch("PRAGMA temp_store = MEMORY; PRAGMA cache_size = -16384;");
    Ok(conn)
}

pub fn check_schema_version(conn: &Connection) -> Result<()> {
    let have: i32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if have != SCHEMA_VERSION {
        return Err(EgError::SchemaMismatch {
            have,
            want: SCHEMA_VERSION,
        });
    }
    Ok(())
}

/// Non-aborting index open for read-only observers (`eg audit`). Unlike
/// [`open_for_query`], a missing, symlinked, corrupt, or skewed index is a
/// value the caller can report per repository instead of an exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexOpenError {
    Missing,
    Symlink,
    Corrupt,
    Schema { have: i32 },
}

pub fn try_open_index(root: &Path) -> std::result::Result<Connection, IndexOpenError> {
    let dir = eg_dir(root);
    let path = db_path(root);
    for candidate in [&dir, &path] {
        match fs::symlink_metadata(candidate) {
            Ok(meta) if meta.file_type().is_symlink() => return Err(IndexOpenError::Symlink),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(IndexOpenError::Missing)
            }
            Err(_) => return Err(IndexOpenError::Corrupt),
        }
    }
    let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| IndexOpenError::Corrupt)?;
    let have: i32 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|_| IndexOpenError::Corrupt)?;
    if have != SCHEMA_VERSION {
        return Err(IndexOpenError::Schema { have });
    }
    Ok(conn)
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct McpIndexStamp {
    pub(super) path: std::path::PathBuf,
    dev: u64,
    ino: u64,
    mtime_ns: i128,
    len: u64,
}

pub(super) fn index_stamp(path: &Path, meta: &fs::Metadata) -> McpIndexStamp {
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i128)
        .unwrap_or(0);
    #[cfg(unix)]
    let (dev, ino) = {
        use std::os::unix::fs::MetadataExt;
        (meta.dev(), meta.ino())
    };
    #[cfg(not(unix))]
    let (dev, ino) = (0u64, 0u64);
    McpIndexStamp {
        path: std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
        dev,
        ino,
        mtime_ns,
        len: meta.len(),
    }
}

struct CachedIndex {
    stamp: McpIndexStamp,
    conn: Connection,
}

static MCP_INDEX_CACHE: std::sync::OnceLock<std::sync::Mutex<Option<CachedIndex>>> =
    std::sync::OnceLock::new();

pub struct CachedConnection {
    stamp: McpIndexStamp,
    conn: Option<Connection>,
}

impl std::ops::Deref for CachedConnection {
    type Target = Connection;
    fn deref(&self) -> &Self::Target {
        self.conn.as_ref().expect("connection taken only on drop")
    }
}

impl Drop for CachedConnection {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            let cell = MCP_INDEX_CACHE.get_or_init(|| std::sync::Mutex::new(None));
            if let Ok(mut guard) = cell.lock() {
                *guard = Some(CachedIndex {
                    stamp: self.stamp.clone(),
                    conn,
                });
            }
        }
    }
}

/// Process-local read-only `Connection` cache for `eg mcp`, validated by
/// `(PathBuf, dev, ino, mtime_ns, len)` via `symlink_metadata`.
pub fn open_cached_for_mcp(root: &Path) -> Result<CachedConnection> {
    reject_tracked_eg_dir(root)?;
    reject_symlink(&eg_dir(root), ".engram directory")?;
    let path = db_path(root);
    reject_symlink(&path, "index.db")?;
    let meta = match fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(EgError::NoResults(format!(
                "no .engram/index.db under {} — run eg build",
                root.display()
            )));
        }
        Err(e) => return Err(e.into()),
    };
    let stamp = index_stamp(&path, &meta);
    let cell = MCP_INDEX_CACHE.get_or_init(|| std::sync::Mutex::new(None));
    if let Ok(mut guard) = cell.lock() {
        if guard.as_ref().is_some_and(|c| c.stamp == stamp) {
            if let Some(cached) = guard.take() {
                return Ok(CachedConnection {
                    stamp: cached.stamp,
                    conn: Some(cached.conn),
                });
            }
        }
    }
    let conn = open_for_query(root)?;
    Ok(CachedConnection {
        stamp,
        conn: Some(conn),
    })
}

/// Evict the cached MCP index connection (all or for a specific root) before
/// `build` or memory mutations.
pub fn evict_mcp_cache(root: Option<&Path>) {
    let Some(cell) = MCP_INDEX_CACHE.get() else {
        return;
    };
    if let Ok(mut guard) = cell.lock() {
        match root {
            None => {
                *guard = None;
            }
            Some(r) => {
                let root_canon = fs::canonicalize(r).unwrap_or_else(|_| r.to_path_buf());
                let target_canon = db_path(&root_canon);
                if guard.as_ref().is_some_and(|c| c.stamp.path == target_canon) {
                    *guard = None;
                }
            }
        }
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;

    #[test]
    fn mcp_cache_hits_on_same_stamp_and_invalidates_on_build() {
        let root = crate::db::test_support::temp_root("mcp-conn-cache");
        crate::db::test_support::write_file(root.join("a.py"), b"def f():\n    return 1\n")
            .unwrap();
        crate::build::run_build(Some(&root), false, 0.05).unwrap();

        let first = open_cached_for_mcp(&root).unwrap();
        let count1: i64 = first
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count1, 1);
        drop(first);

        // Cache hit: connection is returned from MCP_INDEX_CACHE.
        {
            let cell = MCP_INDEX_CACHE.get().unwrap();
            assert!(cell.lock().unwrap().is_some());
        }
        let second = open_cached_for_mcp(&root).unwrap();
        drop(second);

        // Explicit eviction clears the cache; rebuild also changes stamp.
        evict_mcp_cache(Some(&root));
        assert!(MCP_INDEX_CACHE.get().unwrap().lock().unwrap().is_none());

        crate::db::test_support::write_file(root.join("b.py"), b"def g():\n    return 2\n")
            .unwrap();
        crate::build::run_build(Some(&root), true, 0.05).unwrap();
        let third = open_cached_for_mcp(&root).unwrap();
        let count2: i64 = third
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count2, 2);
        drop(third);
        evict_mcp_cache(None);
        crate::db::test_support::remove_tree(&root);
    }
}
