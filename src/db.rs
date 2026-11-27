//! rusqlite layer: schema lifecycle, meta manifest, build lock (flock),
//! build inserts under one caller-owned transaction (Amendment 11b; 500-row
//! prepared batches), and the full-atomic-replacement build policy.
//!
//! Build semantics (spec, single rule): `eg build` always produces a full
//! atomic replacement — extract into `.engram/index.db.new` (fresh schema), close,
//! then rename(2) onto `index.db`. A concurrent reader sees the old or the new
//! DB, never a missing one. There is no delete step; orphan sidecars are
//! cleaned at build start and by `doctor`.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags};

use crate::error::{is_sqlite_corruption, EgError, Result};
use crate::extract::{
    CarriedIndex, CarriedSymbol, Confidence, EdgeKind, NewSymbol, SymbolKind, SCHEMA_VERSION,
};

/// Compile-time grammar identity, derived from the Cargo.lock-pinned grammar
/// crate versions by build.rs (R6d A8: a hand-maintained string had already
/// drifted from the lock). The identity gates the fast path.
pub const GRAMMAR_VERSION: &str = env!("ENGRAM_GRAMMAR_VERSION");
/// Extraction revision: older indexes must be re-extracted when capture or
/// resolution rules change. This does not change the table shape.
pub const SIGNATURE_VERSION: &str = "6";
/// Per-symbol metric-rule version (Amendment 19); a mismatch disables the fast path.
pub const METRICS_VERSION: &str = "2";

fn unsafe_lock(path: &Path) -> EgError {
    EgError::Internal(format!("refusing unsafe build lock: {}", path.display()))
}

/// Create or validate a directory, refusing a symlinked path. A symlink can
/// redirect every later write outside the intended root. `recursive` creates
/// missing parents (the global store may nest under `$HOME/.engram`); the
/// repo-local `.engram` must not. Creation race (two first-time writers, e.g.
/// concurrent mem writes — memory takes no build lock): the loser's create
/// reports AlreadyExists and re-validates the winner's directory. The single
/// implementation shared by the local and global `.engram` sides.
pub(crate) fn ensure_dir_validated(path: &Path, label: &str, recursive: bool) -> Result<()> {
    reject_symlink(path, label)?;
    match fs::symlink_metadata(path) {
        Ok(meta) if !meta.is_dir() => Err(EgError::Internal(format!(
            "{label} exists but is not a directory: {}",
            path.display()
        ))),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let created = if recursive {
                fs::create_dir_all(path)
            } else {
                fs::create_dir(path)
            };
            if let Err(create_err) = created {
                if create_err.kind() != std::io::ErrorKind::AlreadyExists {
                    return Err(create_err.into());
                }
            }
            reject_symlink(path, label)?;
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

/// Create or validate `.engram/`, refusing a symlinked directory.
pub fn ensure_eg_dir(root: &Path) -> Result<PathBuf> {
    let dir = eg_dir(root);
    ensure_dir_validated(&dir, ".engram directory", false)?;
    Ok(dir)
}

fn unexpected_gitignore(marker: &Path) -> EgError {
    EgError::Internal(format!(
        "unexpected .engram/.gitignore content: {}",
        marker.display()
    ))
}

/// Write the tool-owned `.engram/.gitignore` (`*`) on first build: the whole
/// directory disappears from `git status` without touching the repo's own
/// .gitignore (allowed write path per invariant 3's constant list).
/// Fail-closed: a symlink, non-regular file (FIFO/device — rejected before
/// any open), over-cap file, or wrong content is an error, never kept silent.
pub fn ensure_eg_gitignore(root: &Path) -> Result<()> {
    let dir = ensure_eg_dir(root)?;
    let marker = dir.join(".gitignore");
    reject_symlink(&marker, ".engram/.gitignore")?;
    match fs::symlink_metadata(&marker) {
        Ok(_) => {
            // An existing marker must still be the tool's own: `read_bounded`
            // rejects non-regular files before any open and caps the read.
            let bytes = crate::walk::read_bounded(&marker)?;
            if bytes != b"*\n" {
                return Err(unexpected_gitignore(&marker));
            }
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            publish_gitignore_marker(&dir, &marker)
        }
        Err(e) => Err(e.into()),
    }
}

/// Publish a missing marker atomically: write a private temp file, then
/// hard-link it into place. A concurrent creator either loses the link (and
/// validates the complete marker) or wins; no reader can observe a
/// half-written marker (the create-then-write race). Fail-closed: a symlink,
/// non-regular file, or wrong content is an error, never kept silent.
fn publish_gitignore_marker(dir: &Path, marker: &Path) -> Result<()> {
    let tmp = dir.join(format!(".gitignore.{}.tmp", std::process::id()));
    let write = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .and_then(|mut file| file.write_all(b"*\n"));
    if let Err(e) = write {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }
    let linked = fs::hard_link(&tmp, marker);
    match linked {
        Ok(()) => {
            let _ = fs::remove_file(&tmp);
            Ok(())
        }
        Err(link_err) if link_err.kind() == std::io::ErrorKind::AlreadyExists => {
            let _ = fs::remove_file(&tmp);
            let bytes = crate::walk::read_bounded(marker)?;
            if bytes == b"*\n" {
                Ok(())
            } else {
                Err(unexpected_gitignore(marker))
            }
        }
        // Filesystems without hard links (rare for a local .engram):
        // fall back to the atomic rename, which replaces the target.
        Err(link_err) if link_err.kind() == std::io::ErrorKind::Unsupported => {
            let renamed = fs::rename(&tmp, marker);
            let _ = fs::remove_file(&tmp);
            renamed.map_err(EgError::from)
        }
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e.into())
        }
    }
}

#[cfg(unix)]
fn link_count(meta: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.nlink()
}

#[cfg(not(unix))]
fn link_count(_: &fs::Metadata) -> u64 {
    1
}

/// Exclusive flock over `.engram/build.lock` for the whole build (CUJ6). This is
/// the only build lock: one POSIX flock identity for builds and (via the same
/// acquire) for `doctor --invalidate`.
pub struct BuildLock {
    file: File,
}

impl BuildLock {
    pub fn acquire(root: &Path) -> Result<BuildLock> {
        let dir = ensure_eg_dir(root)?;
        let lock_path = dir.join("build.lock");
        let mut file = open_lock_file(&lock_path)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                // A holder between set_len(0) and writeln! leaves the file
                // briefly empty; one bounded retry avoids reporting pid 0.
                let pid = match read_lock_pid(&lock_path) {
                    Some(pid) => pid,
                    None => {
                        std::thread::sleep(Duration::from_millis(5));
                        read_lock_pid(&lock_path).unwrap_or(0)
                    }
                };
                return Err(EgError::Locked(pid));
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(e.into()),
        }
        // Orphan sidecar cleanup AFTER the lock is held: deleting index.db.new
        // while another build is writing it would destroy its in-progress work.
        let new = db_new_path(root);
        if new.exists() {
            let _ = fs::remove_file(&new);
        }
        file.seek(SeekFrom::Start(0))?;
        file.set_len(0)?;
        writeln!(file, "{} {}", std::process::id(), env!("CARGO_PKG_VERSION"))?;
        Ok(BuildLock { file })
    }
}

/// Open (or create) the lock file, rejecting symlinks and hard-linked files
/// before and after the open.
fn open_lock_file(lock_path: &Path) -> Result<fs::File> {
    reject_symlink(lock_path, "build lock")?;
    let file = match fs::symlink_metadata(lock_path) {
        // != 1 also rejects nlink == 0 (an unlinked inode): two processes could
        // otherwise each flock a different unlinked inode and both "hold" the
        // build lock.
        Ok(meta) if !meta.is_file() || link_count(&meta) != 1 => {
            return Err(unsafe_lock(lock_path));
        }
        Ok(_) => fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(lock_path)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            match fs::OpenOptions::new()
                .create_new(true)
                .read(true)
                .write(true)
                .open(lock_path)
            {
                Ok(file) => file,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(lock_path)?,
                Err(e) => return Err(e.into()),
            }
        }
        Err(e) => return Err(e.into()),
    };
    reject_symlink(lock_path, "build lock")?;
    let meta = file.metadata()?;
    if !meta.is_file() || link_count(&meta) != 1 {
        return Err(unsafe_lock(lock_path));
    }
    Ok(file)
}

/// First line's leading integer of the lock file, when it parses.
fn read_lock_pid(lock_path: &Path) -> Option<u32> {
    fs::read_to_string(lock_path)
        .ok()?
        .lines()
        .next()?
        .split_whitespace()
        .next()?
        .parse::<u32>()
        .ok()
}

impl Drop for BuildLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// Fresh DB at `.engram/index.db.new` with tables, FTS, and user_version stamped.
/// Secondary indexes are created later by `create_secondary_indexes` (after
/// the bulk insert) so each index is sorted once instead of maintained per row.
pub fn create_fresh(root: &Path) -> Result<Connection> {
    reject_tracked_eg_dir(root)?;
    ensure_eg_gitignore(root)?;
    let path = db_new_path(root);
    let _ = fs::remove_file(&path);
    reject_symlink(&path, "index.db.new")?;
    let conn = Connection::open(&path)?;
    // Build-only pragmas (A2): `.new` is disposable until publish_new_index
    // renames it, so this connection drops the rollback journal (no double
    // writes) and per-commit fsyncs. publish_new_index fsyncs the finished file
    // before the atomic rename, so the published index is durable.
    conn.execute_batch(
        "PRAGMA journal_mode = OFF;
         PRAGMA synchronous = OFF;
         PRAGMA locking_mode = EXCLUSIVE;
         PRAGMA cache_size = -32768;",
    )?;
    conn.execute_batch(include_str!("../schema/index.sql"))?;
    // The version contract lives here alone: schema.sql declares no PRAGMA,
    // and this pragma_update stamps the compiled SCHEMA_VERSION.
    conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(conn)
}

/// Create the secondary indexes after every bulk insert. The final schema is
/// identical to the historical schema.sql layout; only the creation order moves.
pub fn create_secondary_indexes(conn: &Connection) -> Result<()> {
    conn.execute_batch(include_str!("../schema/indexes.sql"))?;
    Ok(())
}

/// Fsync the finished `.new` index and atomically publish it onto `index.db`.
/// The build connection must already be closed (SQLite flushes its pager on
/// close). A crash before the rename leaves only a disposable `.new`; the next
/// build's lock holder removes it, and the previous `index.db` stays readable.
pub fn publish_new_index(root: &Path) -> Result<()> {
    let new = db_new_path(root);
    // Write permission is required for FlushFileBuffers on Windows
    // (ERROR_ACCESS_DENIED on a read-only handle); POSIX is unaffected and
    // the published index stays immutable after the rename.
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&new)?
        .sync_all()?;
    fs::rename(&new, db_path(root))?;
    // Best-effort directory fsync: makes the rename durable. If it fails, a
    // crash can revert to the previous index, never expose a missing one.
    if let Ok(dir) = File::open(eg_dir(root)) {
        let _ = dir.sync_all();
    }
    Ok(())
}

/// Escape LIKE metacharacters so a ref containing `%` or `_` cannot match an
/// arbitrary path and report itself verified. Pair with `ESCAPE '\'` on every
/// LIKE term built from caller-controlled text.
pub(crate) fn like_escape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 2);
    for ch in raw.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Filesystem mutation helpers for db tests: every fs write performed by
    //! the db test modules is defined here, lexically inside `db.rs`, keeping
    //! the write-confinement invariant intact.

    use std::fs;
    use std::path::{Path, PathBuf};

    /// Fresh throwaway test root (`eg-{name}-{pid}`), wiped from any previous
    /// run; shared by journal, doctor, and walk tests via `crate::db::temp_root`.
    pub(crate) fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("eg-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    pub(crate) fn write_file(path: impl AsRef<Path>, bytes: &[u8]) -> std::io::Result<()> {
        fs::write(path, bytes)
    }

    /// Append bytes, mirroring a live journal writer in tail tests.
    pub(crate) fn append_file(path: impl AsRef<Path>, bytes: &[u8]) -> std::io::Result<()> {
        use std::io::Write;
        let mut f = fs::OpenOptions::new().append(true).open(path)?;
        f.write_all(bytes)
    }

    pub(crate) fn rename_file(from: impl AsRef<Path>, to: impl AsRef<Path>) -> std::io::Result<()> {
        fs::rename(from, to)
    }

    /// A git command for fixture repositories with every `GIT_*` variable
    /// stripped: a hook environment (`GIT_DIR` exported by `git push`) would
    /// otherwise redirect the fixture's git operations into the shared
    /// checkout's index.
    pub(crate) fn stripped_git() -> std::process::Command {
        let mut command = std::process::Command::new("git");
        for key in std::env::vars().map(|(k, _)| k).collect::<Vec<_>>() {
            if key.starts_with("GIT_") {
                command.env_remove(key);
            }
        }
        command
    }

    pub(crate) fn make_dir_all(path: impl AsRef<Path>) -> std::io::Result<()> {
        fs::create_dir_all(path)
    }

    /// Symlink fixture for liveness tests (`link` -> `target`). Unix-only;
    /// journal paths are POSIX filesystem identities.
    #[cfg(unix)]
    pub(crate) fn make_symlink(
        target: impl AsRef<Path>,
        link: impl AsRef<Path>,
    ) -> std::io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    pub(crate) fn remove_tree(path: impl AsRef<Path>) {
        let _ = fs::remove_dir_all(path);
    }
}

#[cfg(test)]
pub(crate) use test_support::temp_root;

mod inserts;
mod memory_fts;
#[cfg(test)]
mod memory_gc_tests;
mod memory_query;
mod memory_store;
#[cfg(test)]
mod memory_tests;
#[cfg(test)]
mod memory_v2_tests;
mod memory_write;
mod meta;
mod open;
mod paths;
mod query_state;
mod security;
mod sql;
#[cfg(test)]
mod tests;

pub use open::{check_schema_version, open_for_query, try_open_index, IndexOpenError};

pub(crate) use inserts::*;
pub(crate) use memory_fts::*;
pub(crate) use memory_query::*;
pub(crate) use memory_store::map_memory_err;
pub(crate) use memory_store::*;
pub(crate) use memory_write::*;
pub(crate) use meta::*;
pub(crate) use paths::file_size;
pub use paths::{db_new_path, db_path, eg_dir};
pub(crate) use query_state::*;
pub(crate) use security::*;
pub(crate) use sql::*;
