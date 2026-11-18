//! Memory rows and revisioned writes: content dedup, revision chains,
//! supersession (same-key and cross-key), and the unix clock.

use super::*;

/// Unix seconds (writes stamp created_at/superseded_at).
pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// One memory note row as returned to the presentation layer. `superseded_by`
/// is read only for the `--include-superseded` listing tag; `superseded_at`
/// stays internal (the gc retention clock).
#[derive(Debug, Clone)]
pub struct MemRow {
    pub id: i64,
    pub key: String,
    pub rev: i64,
    pub kind: String,
    pub scope: String,
    pub session: String,
    pub body: String,
    pub tags: String,
    pub refs: String,
    pub source: String,
    pub created_at: i64,
    pub anchored_at: i64,
    pub expires_at: i64,
    pub superseded_by: Option<i64>,
}

pub(super) const MEM_ROW_SQL: &str =
    "id, key, rev, kind, scope, session, body, tags, refs, source, created_at, expires_at, superseded_by, anchored_at";

impl MemRow {
    /// The freshness anchor: a verified note uses `anchored_at`, otherwise it
    /// falls back to `created_at`.
    pub fn anchor(&self) -> i64 {
        self.anchored_at.max(self.created_at)
    }
}

pub(super) fn mem_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<MemRow> {
    Ok(MemRow {
        id: r.get(0)?,
        key: r.get(1)?,
        rev: r.get(2)?,
        kind: r.get(3)?,
        scope: r.get(4)?,
        session: r.get(5)?,
        body: r.get(6)?,
        tags: r.get(7)?,
        refs: r.get(8)?,
        source: r.get(9)?,
        created_at: r.get(10)?,
        expires_at: r.get(11)?,
        superseded_by: r.get(12)?,
        anchored_at: r.get(13)?,
    })
}

/// Validated note ready for the write transaction (built by the service layer
/// through `memory::normalize_note`). `expires_at` is 0 for durable notes and
/// the absolute unix deadline for session notes; `supersedes` names a
/// different key whose current note this write replaces.
pub struct MemNoteInput<'a> {
    pub key: &'a str,
    pub kind: &'a str,
    pub tags: &'a str,
    pub refs: &'a str,
    pub scope: &'a str,
    pub session: &'a str,
    pub body: &'a str,
    pub source: &'a str,
    pub expires_at: i64,
    /// Preserve the exported row's authorship date on import; `None` stamps
    /// the write time.
    pub created_at: Option<i64>,
    pub supersedes: Option<&'a str>,
    pub touch: bool,
}

/// Write outcome: a new revision, an idempotent duplicate, or a refreshed timestamp.
#[derive(Debug)]
pub enum MemWriteOutcome {
    Written {
        rev: i64,
        superseded_rev: Option<i64>,
    },
    Duplicate {
        rev: i64,
    },
    Touched {
        rev: i64,
    },
}

/// `execute_batch` reports statement failures as `SqlInputError`; `execute`
/// reports them as `SqliteFailure`. Both shapes must classify "already
/// exists" so a concurrent creator can fall through to the version gate.
pub(super) fn is_already_exists(e: &rusqlite::Error) -> bool {
    matches!(
        e,
        rusqlite::Error::SqliteFailure(_, Some(msg)) | rusqlite::Error::SqlInputError { msg, .. }
            if msg.contains("already exists")
    )
}

fn is_unique_violation(e: &rusqlite::Error) -> bool {
    matches!(
        e,
        rusqlite::Error::SqliteFailure(ffi, _)
            if ffi.code == rusqlite::ffi::ErrorCode::ConstraintViolation
    )
}

/// Insert one note under BEGIN IMMEDIATE: the write lock is taken BEFORE the
/// current rev is read and held across read-compute-insert, so racing writers
/// serialize on busy_timeout (or exit 4) and never compute the same rev. The
/// FTS delta (delete superseded rowid, insert the new one) runs in the same
/// transaction. A retried write with identical content is a successful no-op.
pub fn memory_write(
    conn: &mut Connection,
    note: &MemNoteInput<'_>,
    now: i64,
) -> Result<MemWriteOutcome> {
    // One in-transaction retry on a UNIQUE(key, rev) failure (a defect by
    // construction — rev is computed inside the lock — but never exit 2/5).
    let mut last_error = None;
    for _ in 0..2 {
        match try_write_revision(conn, note, now)? {
            WriteAttempt::Written(outcome) => return Ok(outcome),
            WriteAttempt::Retry(e) => last_error = Some(e),
        }
    }
    match last_error {
        Some(e) => Err(map_memory_err(e)),
        None => Err(EgError::Internal(
            "memory write: no attempt completed".to_string(),
        )),
    }
}

/// Aggregate outcome of one import batch.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct BatchImportStats {
    pub imported: usize,
    pub skipped: usize,
    pub conflicts: usize,
    pub expired: usize,
}

/// Write many imported notes under ONE `BEGIN IMMEDIATE` and one commit.
/// Conflict policy: identical content (same hash) is a no-op; differing
/// content is skipped unless `force` opens a new revision; an already-expired
/// session note is counted and skipped. Any SQL error rolls the batch back.
pub fn memory_write_batch(
    conn: &mut Connection,
    notes: &[MemNoteInput<'_>],
    now: i64,
    force: bool,
) -> Result<BatchImportStats> {
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(map_memory_err)?;
    let mut stats = BatchImportStats::default();
    for note in notes {
        if note.expires_at != 0 && note.expires_at <= now {
            stats.expired += 1;
            continue;
        }
        let hash =
            crate::memory::mem_content_hash(note.kind, note.body, note.tags, note.refs, note.scope)
                as i64;
        let fts_legacy = crate::memory::mem_fts_text_legacy(note.key, note.tags, note.body);
        let fts_unicode = crate::memory::mem_fts_text(note.key, note.tags, note.body);
        let current = current_revision(&tx, note.key)?;
        if let Some(row) = &current {
            // An inactive (superseded or expired) row never blocks or dedups:
            // identical content still appends a revision so the key re-enters
            // the FTS index, and a conflict is only reported against a live row.
            if row.is_active(now) {
                // FNV-1a is not cryptographic: confirm the stored fields
                // before treating a hash match as a true duplicate.
                if row.content_hash == hash && stored_content_matches(&tx, row.id, note)? {
                    stats.skipped += 1;
                    continue;
                }
                if !force {
                    stats.conflicts += 1;
                    continue;
                }
            }
        }
        let rev = current.as_ref().map_or(1, |row| row.rev + 1);
        insert_revision(&tx, note, now, hash, rev).map_err(map_memory_err)?;
        let new_id = tx.last_insert_rowid();
        write_fts_and_supersede(&tx, &current, new_id, now, &fts_legacy, &fts_unicode)?;
        stats.imported += 1;
    }
    tx.commit().map_err(map_memory_err)?;
    Ok(stats)
}

enum WriteAttempt {
    Written(MemWriteOutcome),
    Retry(rusqlite::Error),
}

fn try_write_revision(
    conn: &mut Connection,
    note: &MemNoteInput<'_>,
    now: i64,
) -> Result<WriteAttempt> {
    let hash =
        crate::memory::mem_content_hash(note.kind, note.body, note.tags, note.refs, note.scope)
            as i64;
    let fts_legacy = crate::memory::mem_fts_text_legacy(note.key, note.tags, note.body);
    let fts_unicode = crate::memory::mem_fts_text(note.key, note.tags, note.body);
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(map_memory_err)?;
    let current = current_revision(&tx, note.key)?;
    if let Some(row) = &current {
        // An inactive (superseded or expired) row is live only for revision
        // numbering: identical content must fall through to `insert_revision`
        // so the key re-enters the FTS index, never return Duplicate/Touched.
        if row.is_active(now)
            && row.content_hash == hash
            && stored_content_matches(&tx, row.id, note)?
        {
            if note.touch {
                // The Touch contract owns `anchored_at`; `mem write --touch`
                // re-asserts the invocation's TTL policy instead: `created_at`
                // moves to now and `expires_at` comes from the normalized input.
                tx.execute(
                    "UPDATE memories SET created_at = ?1, expires_at = ?2 WHERE id = ?3",
                    rusqlite::params![now, note.expires_at, row.id],
                )
                .map_err(map_memory_err)?;
                tx.commit().map_err(map_memory_err)?;
                return Ok(WriteAttempt::Written(MemWriteOutcome::Touched {
                    rev: row.rev,
                }));
            }
            // Idempotent replay: nothing written, nothing to commit.
            drop(tx);
            return Ok(WriteAttempt::Written(MemWriteOutcome::Duplicate {
                rev: row.rev,
            }));
        }
    }
    let new_rev = current.as_ref().map_or(1, |row| row.rev + 1);
    if let Err(e) = insert_revision(&tx, note, now, hash, new_rev) {
        if is_unique_violation(&e) {
            return Ok(WriteAttempt::Retry(e));
        }
        return Err(map_memory_err(e));
    }
    let new_id = tx.last_insert_rowid();
    write_fts_and_supersede(&tx, &current, new_id, now, &fts_legacy, &fts_unicode)?;
    if let Some(target) = note.supersedes {
        supersede_cross_key(&tx, new_id, now, target)?;
    }
    tx.commit().map_err(map_memory_err)?;
    Ok(WriteAttempt::Written(MemWriteOutcome::Written {
        rev: new_rev,
        superseded_rev: current.as_ref().map(|row| row.rev),
    }))
}

/// Refresh `anchored_at = now` only when the active revision still equals
/// `expected_rev`, in one transaction. "Active" mirrors `Revision::is_active`:
/// nothing has superseded the revision and its TTL has not passed — an
/// expired revision must never be re-anchored into looking current, so the
/// dashboard touch cannot succeed where `mem touch` refuses. `Ok(None)` means
/// the key is gone, the revision moved, or the revision went inactive; the
/// caller maps that to 409/404.
pub fn memory_touch_if_rev(
    conn: &mut Connection,
    key: &str,
    expected_rev: i64,
    now: i64,
) -> Result<Option<i64>> {
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(map_memory_err)?;
    let updated = tx
        .execute(
            "UPDATE memories SET anchored_at = ?1 WHERE key = ?2 AND rev = ?3
             AND superseded_by IS NULL AND (expires_at = 0 OR expires_at > ?4)",
            rusqlite::params![now, key, expected_rev, now],
        )
        .map_err(map_memory_err)?;
    if updated == 0 {
        return Ok(None);
    }
    tx.commit().map_err(map_memory_err)?;
    Ok(Some(expected_rev))
}

/// Refresh `anchored_at = now` on the current active revision of `key`,
/// leaving `created_at` (the authorship date) untouched.
/// Returns `Ok(Some(rev))` if the active revision was updated, or `Ok(None)`
/// if the key does not exist or its current revision is inactive (superseded
/// or expired) — the same inactive-revision predicate
/// `memory_touch_if_rev` enforces in SQL; a dead key cannot be kept alive by
/// a touch.
pub fn memory_touch(conn: &mut Connection, key: &str, now: i64) -> Result<Option<i64>> {
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(map_memory_err)?;
    let current = current_revision(&tx, key)?;
    let Some(rev_info) = current else {
        return Ok(None);
    };
    if !rev_info.is_active(now) {
        return Ok(None);
    }
    tx.execute(
        "UPDATE memories SET anchored_at = ?1 WHERE id = ?2",
        rusqlite::params![now, rev_info.id],
    )
    .map_err(map_memory_err)?;
    tx.commit().map_err(map_memory_err)?;
    Ok(Some(rev_info.rev))
}

mod revision;

use revision::{
    current_revision, insert_revision, stored_content_matches, supersede_cross_key,
    write_fts_and_supersede,
};

#[cfg(test)]
mod tests;
