//! Process-local read-only `memory.db` connection cache for `eg mcp`.
//! Validated by `(PathBuf, dev, ino, mtime_ns, len)` via `symlink_metadata` so
//! repeated MCP `mem_query`/`context`/`impact` calls skip `MemoryDbProbe::read`.

use std::fs;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use rusqlite::Connection;

use super::super::open::{index_stamp, McpIndexStamp};
use super::{
    global_memory_store_path, memory_db_path, no_global_memory_error, no_memory_error,
    open_global_memory_query, open_memory_query,
};
use crate::db::paths::eg_dir;
use crate::db::security::reject_symlink;
use crate::error::Result;

const MAX_CACHED_MEMORY_CONNS: usize = 8;

struct CachedMemory {
    stamp: McpIndexStamp,
    conn: Connection,
}

static MCP_MEMORY_CACHE: OnceLock<Mutex<Vec<CachedMemory>>> = OnceLock::new();

#[allow(dead_code)]
pub struct CachedMemoryConnection {
    stamp: McpIndexStamp,
    conn: Option<Connection>,
}

impl std::ops::Deref for CachedMemoryConnection {
    type Target = Connection;
    fn deref(&self) -> &Self::Target {
        self.conn.as_ref().expect("connection taken only on drop")
    }
}

impl Drop for CachedMemoryConnection {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            let cell = MCP_MEMORY_CACHE.get_or_init(|| Mutex::new(Vec::new()));
            if let Ok(mut guard) = cell.lock() {
                guard.retain(|c| c.stamp.path != self.stamp.path);
                if guard.len() >= MAX_CACHED_MEMORY_CONNS {
                    guard.remove(0);
                }
                guard.push(CachedMemory {
                    stamp: self.stamp.clone(),
                    conn,
                });
            }
        }
    }
}

fn memory_stamp_for(root: Option<&Path>) -> Result<McpIndexStamp> {
    let path = match root {
        Some(r) => {
            reject_symlink(&eg_dir(r), ".engram directory")?;
            let p = memory_db_path(r);
            reject_symlink(&p, "memory.db")?;
            p
        }
        None => global_memory_store_path()?,
    };
    let meta = match fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(match root {
                Some(r) => no_memory_error(r),
                None => no_global_memory_error(&path),
            });
        }
        Err(e) => return Err(e.into()),
    };
    Ok(index_stamp(&path, &meta))
}

/// Process-local read-only `memory.db` connection cache for `eg mcp`.
/// `Some(root)` opens the local store; `None` opens the global store.
#[allow(dead_code)]
pub fn open_cached_memory_for_mcp(root: Option<&Path>) -> Result<CachedMemoryConnection> {
    let stamp = memory_stamp_for(root)?;
    let cell = MCP_MEMORY_CACHE.get_or_init(|| Mutex::new(Vec::new()));
    if let Ok(mut guard) = cell.lock() {
        if let Some(pos) = guard.iter().position(|c| c.stamp == stamp) {
            let cached = guard.remove(pos);
            return Ok(CachedMemoryConnection {
                stamp: cached.stamp,
                conn: Some(cached.conn),
            });
        }
    }
    let conn = match root {
        Some(r) => open_memory_query(r)?,
        None => open_global_memory_query()?,
    };
    Ok(CachedMemoryConnection {
        stamp,
        conn: Some(conn),
    })
}

/// Evict cached MCP memory connections (all when `None`, or local store for `Some(root)`).
#[allow(dead_code)]
pub fn evict_memory_mcp_cache(root: Option<&Path>) {
    let Some(cell) = MCP_MEMORY_CACHE.get() else {
        return;
    };
    if let Ok(mut guard) = cell.lock() {
        match root {
            None => guard.clear(),
            Some(r) => {
                let root_canon = fs::canonicalize(r).unwrap_or_else(|_| r.to_path_buf());
                let target_canon = memory_db_path(&root_canon);
                guard.retain(|c| c.stamp.path != target_canon);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_memory_cache_hits_on_same_stamp_and_evicts() {
        let root = crate::db::test_support::temp_root("mcp-mem-cache");
        let conn = crate::db::open_memory_store(&root).unwrap();
        conn.execute(
            "INSERT INTO memories (key, rev, kind, scope, session, body, tags, refs, content_hash, source, created_at)
             VALUES ('k1', 1, 'fact', 'durable', '', 'hello', '', '', 1, 'cli', 100)",
            [],
        )
        .unwrap();
        drop(conn);

        let first = open_cached_memory_for_mcp(Some(&root)).unwrap();
        let count1: i64 = first
            .query_row("SELECT COUNT(*) FROM memories", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count1, 1);
        drop(first);

        assert_eq!(MCP_MEMORY_CACHE.get().unwrap().lock().unwrap().len(), 1);
        let second = open_cached_memory_for_mcp(Some(&root)).unwrap();
        drop(second);

        evict_memory_mcp_cache(Some(&root));
        assert!(MCP_MEMORY_CACHE.get().unwrap().lock().unwrap().is_empty());

        evict_memory_mcp_cache(None);
        crate::db::test_support::remove_tree(&root);
    }
}
