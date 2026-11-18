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
