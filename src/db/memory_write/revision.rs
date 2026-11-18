//! Revision-row mechanics for the memory store: the current-revision probe,
//! FNV collision confirmation, revision inserts, and same-key/cross-key
//! supersession with FTS row maintenance.

use super::*;
use rusqlite::OptionalExtension;

/// The current memory revision row for a key: row id, revision number, the
/// revision's content hash, and the two lifecycle columns that decide whether
/// the row can still answer writes and touches. Named so the i64 columns
/// cannot be swapped positionally in `try_write_revision`.
pub(super) struct Revision {
    pub(super) id: i64,
    pub(super) rev: i64,
    pub(super) content_hash: i64,
    pub(super) superseded_by: Option<i64>,
    pub(super) expires_at: i64,
}

impl Revision {
    /// A revision is active while nothing replaced it and its TTL has not
    /// passed (`expires_at == 0` is durable). An inactive row is read only for
    /// revision numbering: a re-write must append a revision and restore the
    /// FTS rows instead of reporting `Duplicate`/`Touched`, and a touch must
    /// refuse it.
    pub(super) fn is_active(&self, now: i64) -> bool {
        self.superseded_by.is_none() && (self.expires_at == 0 || self.expires_at > now)
    }
}

pub(super) fn write_fts_and_supersede(
    tx: &rusqlite::Transaction<'_>,
    current: &Option<Revision>,
    new_id: i64,
    now: i64,
    fts_legacy: &str,
    fts_unicode: &str,
) -> Result<()> {
    if let Some(row) = current {
        // Supersede every current row that nothing replaced yet, including
        // expired ones: the revision chain must show the replacement even
        // when the old revision's TTL already passed.
        if row.superseded_by.is_none() {
            supersede_current(tx, new_id, now, row.id)?;
        }
    }
    tx.execute(
        "INSERT INTO memories_fts(rowid, text) VALUES (?1, ?2)",
        rusqlite::params![new_id, fts_legacy],
    )
    .map_err(map_memory_err)?;
    tx.execute(
        "INSERT INTO memories_fts_v2(rowid, text) VALUES (?1, ?2)",
        rusqlite::params![new_id, fts_unicode],
    )
    .map_err(map_memory_err)?;
    Ok(())
}

pub(super) fn insert_revision(
    tx: &rusqlite::Transaction<'_>,
    note: &MemNoteInput<'_>,
    now: i64,
    hash: i64,
    rev: i64,
) -> rusqlite::Result<usize> {
    let created_at = note.created_at.unwrap_or(now);
    tx.execute(
        "INSERT INTO memories(key, rev, kind, scope, session, body, tags, refs, content_hash, source, created_at, expires_at, fts_version, embedding)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 2, ?13)",
        rusqlite::params![
            note.key,
            rev,
            note.kind,
            note.scope,
            note.session,
            note.body,
            note.tags,
            note.refs,
            hash,
            note.source,
            created_at,
            note.expires_at,
            note.embedding,
        ],
    )
}

/// Confirm a content-hash match against the stored fields before reporting a
/// duplicate: FNV-1a collisions must append a revision, not skip the write.
pub(super) fn stored_content_matches(
    tx: &rusqlite::Transaction<'_>,
    row_id: i64,
    note: &MemNoteInput<'_>,
) -> Result<bool> {
    let (kind, body, tags, refs, scope) = tx
        .query_row(
            "SELECT kind, body, tags, refs, scope FROM memories WHERE id = ?1",
            rusqlite::params![row_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            },
        )
        .map_err(map_memory_err)?;
    Ok(kind == note.kind
        && body == note.body
        && tags == note.tags
        && refs == note.refs
        && scope == note.scope)
}

pub(super) fn current_revision(
    tx: &rusqlite::Transaction<'_>,
    key: &str,
) -> Result<Option<Revision>> {
    tx.query_row(
        "SELECT id, rev, content_hash, superseded_by, expires_at FROM memories
         WHERE key = ?1 ORDER BY rev DESC LIMIT 1",
        rusqlite::params![key],
        |r| {
            Ok(Revision {
                id: r.get(0)?,
                rev: r.get(1)?,
                content_hash: r.get(2)?,
                superseded_by: r.get(3)?,
                expires_at: r.get(4)?,
            })
        },
    )
    .optional()
    .map_err(map_memory_err)
}

/// Mark one revision superseded and drop its FTS row (the caller inserts the
/// new revision's FTS row). Shared by same-key and cross-key supersession so
/// both paths apply the same invariant.
pub(super) fn supersede_current(
    tx: &rusqlite::Transaction<'_>,
    new_id: i64,
    now: i64,
    old_id: i64,
) -> Result<()> {
    tx.execute(
        "UPDATE memories SET superseded_by = ?1, superseded_at = ?2 WHERE id = ?3",
        rusqlite::params![new_id, now, old_id],
    )
    .map_err(map_memory_err)?;
    tx.execute(
        "DELETE FROM memories_fts WHERE rowid = ?1",
        rusqlite::params![old_id],
    )
    .map_err(map_memory_err)?;
    tx.execute(
        "DELETE FROM memories_fts_v2 WHERE rowid = ?1",
        rusqlite::params![old_id],
    )
    .map_err(map_memory_err)?;
    Ok(())
}

/// Cross-key supersession inside the caller's transaction: the target's
/// current revision becomes superseded and its FTS row leaves the index with
/// it. Same invariant as same-key supersession; without the FTS delete `mem
/// query` would return a note that `mem get` calls unknown. The pick is
/// `ORDER BY rev DESC`, stated in SQL like `current_revision` and `memory_get`
/// instead of borrowed from the `memories_current` partial index's scan
/// order — after an expired-key re-write, one key can carry two
/// `superseded_by IS NULL` rows and the highest revision must win under any
/// planner. An unknown target rolls the whole write back (exit 1). A
/// replayed identical write never reaches here, so the side effects do not
/// re-apply.
pub(super) fn supersede_cross_key(
    tx: &rusqlite::Transaction<'_>,
    new_id: i64,
    now: i64,
    target: &str,
) -> Result<()> {
    let target_id: Option<i64> = tx
        .query_row(
            "SELECT id FROM memories WHERE key = ?1 AND superseded_by IS NULL
             ORDER BY rev DESC LIMIT 1",
            rusqlite::params![target],
            |r| r.get(0),
        )
        .optional()
        .map_err(map_memory_err)?;
    let Some(target_id) = target_id else {
        return Err(EgError::NoResults(format!(
            "unknown memory key '{target}' — nothing superseded, write rolled back"
        )));
    };
    supersede_current(tx, new_id, now, target_id)
}
