//! Soft-retire, restore, and CAS-guarded delete for memory notes.

use super::*;
use rusqlite::OptionalExtension;

/// Sentinel stored in `superseded_by` when a note is explicitly retired
/// (`eg mem delete --retire` / MCP `mem_retire`). Negative values distinguish
/// explicit retirement from positive row-id supersession.
pub const RETIRED_SENTINEL: i64 = -1;

/// Outcome of a revision-guarded (`--expected-rev`) memory mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CasOutcome {
    /// Mutation succeeded on revision `rev`.
    Ok(i64),
    /// Key does not exist (or has no active revision when unguarded).
    NotFound,
    /// `expected_rev` did not match the current active revision.
    Conflict {
        expected_rev: i64,
        current_rev: Option<i64>,
    },
}

/// Soft-retire the active revision of `key` inside `BEGIN IMMEDIATE`:
/// sets `superseded_by = RETIRED_SENTINEL`, `superseded_at = now`, and
/// purges its FTS5 rows so it drops out of `mem query` immediately while
/// remaining recoverable via `eg mem restore` or `mem get --include-superseded`.
pub fn memory_retire(
    conn: &mut Connection,
    key: &str,
    expected_rev: Option<i64>,
    now: i64,
) -> Result<CasOutcome> {
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(map_memory_err)?;
    let row = tx
        .query_row(
            "SELECT rev, superseded_by, expires_at FROM memories
             WHERE key = ?1 ORDER BY rev DESC LIMIT 1",
            rusqlite::params![key],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Option<i64>>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()
        .map_err(map_memory_err)?;
    let Some((rev, superseded_by, expires_at)) = row else {
        return Ok(CasOutcome::NotFound);
    };
    let is_active = superseded_by.is_none() && (expires_at == 0 || expires_at > now);
    if !is_active {
        return Ok(match expected_rev {
            Some(exp) => CasOutcome::Conflict {
                expected_rev: exp,
                current_rev: None,
            },
            None => CasOutcome::NotFound,
        });
    }
    if let Some(exp) = expected_rev {
        if rev != exp {
            return Ok(CasOutcome::Conflict {
                expected_rev: exp,
                current_rev: Some(rev),
            });
        }
    }
    tx.execute(
        "UPDATE memories SET superseded_by = ?1, superseded_at = ?2 WHERE key = ?3 AND superseded_by IS NULL",
        rusqlite::params![RETIRED_SENTINEL, now, key],
    )
    .map_err(map_memory_err)?;
    let unretired_ids: Vec<i64> = {
        let mut stmt = tx
            .prepare("SELECT id FROM memories WHERE key = ?1 AND superseded_by = ?2")
            .map_err(map_memory_err)?;
        let rows = stmt
            .query_map(rusqlite::params![key, RETIRED_SENTINEL], |r| {
                r.get::<_, i64>(0)
            })
            .map_err(map_memory_err)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_memory_err)?
    };
    for rid in unretired_ids {
        tx.execute(
            "DELETE FROM memories_fts WHERE rowid = ?1",
            rusqlite::params![rid],
        )
        .map_err(map_memory_err)?;
        tx.execute(
            "DELETE FROM memories_fts_v2 WHERE rowid = ?1",
            rusqlite::params![rid],
        )
        .map_err(map_memory_err)?;
    }
    tx.commit().map_err(map_memory_err)?;
    Ok(CasOutcome::Ok(rev))
}

/// Restore a soft-retired note (`superseded_by == RETIRED_SENTINEL`) inside
/// `BEGIN IMMEDIATE` by inserting `rev + 1` and indexing it in FTS5.
/// Returns `Err(Usage)` if the key is already active, `Ok(None)` if missing
/// or not retired, and `Ok(Some(new_rev))` on restore.
pub fn memory_restore(conn: &mut Connection, key: &str, now: i64) -> Result<Option<i64>> {
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(map_memory_err)?;
    let row = tx
        .query_row(
            "SELECT rev, kind, scope, session, body, tags, refs, content_hash, source, expires_at, superseded_by, embedding
             FROM memories WHERE key = ?1 ORDER BY rev DESC LIMIT 1",
            rusqlite::params![key],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, i64>(7)?,
                    r.get::<_, String>(8)?,
                    r.get::<_, i64>(9)?,
                    r.get::<_, Option<i64>>(10)?,
                    r.get::<_, Option<Vec<u8>>>(11)?,
                ))
            },
        )
        .optional()
        .map_err(map_memory_err)?;
    let Some((
        rev,
        kind,
        scope,
        session,
        body,
        tags,
        refs,
        content_hash,
        source,
        expires_at,
        superseded_by,
        embedding,
    )) = row
    else {
        return Ok(None);
    };
    if superseded_by.is_none() && (expires_at == 0 || expires_at > now) {
        return Err(EgError::Usage(format!(
            "mem restore: '{key}' is already active at rev {rev}"
        )));
    }
    if superseded_by != Some(RETIRED_SENTINEL) || (expires_at != 0 && expires_at <= now) {
        return Ok(None);
    }
    let new_rev = rev + 1;
    let new_expires_at = if scope == "session" {
        now + 7 * 24 * 3600
    } else {
        0
    };
    tx.execute(
        "INSERT INTO memories(key, rev, kind, scope, session, body, tags, refs, content_hash, source, created_at, expires_at, fts_version, embedding)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 2, ?13)",
        rusqlite::params![
            key,
            new_rev,
            kind,
            scope,
            session,
            body,
            tags,
            refs,
            content_hash,
            source,
            now,
            new_expires_at,
            embedding,
        ],
    )
    .map_err(map_memory_err)?;
    let new_id = tx.last_insert_rowid();
    let fts_legacy = crate::memory::mem_fts_text_legacy(key, &tags, &body);
    let fts_unicode = crate::memory::mem_fts_text(key, &tags, &body);
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
    tx.commit().map_err(map_memory_err)?;
    Ok(Some(new_rev))
}

/// Hard-delete every revision of `key` (and its FTS rows), optionally guarded
/// by `expected_rev` on the current active revision.
pub fn memory_delete_if_rev(
    conn: &mut Connection,
    key: &str,
    expected_rev: Option<i64>,
    now: i64,
) -> Result<CasOutcome> {
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(map_memory_err)?;
    let latest = tx
        .query_row(
            "SELECT rev, superseded_by, expires_at FROM memories
             WHERE key = ?1 ORDER BY rev DESC LIMIT 1",
            rusqlite::params![key],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Option<i64>>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()
        .map_err(map_memory_err)?;
    let Some((latest_rev, superseded_by, expires_at)) = latest else {
        return Ok(CasOutcome::NotFound);
    };
    if let Some(exp) = expected_rev {
        let is_active = superseded_by.is_none() && (expires_at == 0 || expires_at > now);
        if !is_active {
            return Ok(CasOutcome::Conflict {
                expected_rev: exp,
                current_rev: None,
            });
        }
        if latest_rev != exp {
            return Ok(CasOutcome::Conflict {
                expected_rev: exp,
                current_rev: Some(latest_rev),
            });
        }
    }
    let ids: Vec<i64> = {
        let mut stmt = tx
            .prepare("SELECT id FROM memories WHERE key = ?1")
            .map_err(map_memory_err)?;
        let rows = stmt
            .query_map(rusqlite::params![key], |r| r.get::<_, i64>(0))
            .map_err(map_memory_err)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_memory_err)?
    };
    for id in &ids {
        tx.execute(
            "DELETE FROM memories_fts WHERE rowid = ?1",
            rusqlite::params![id],
        )
        .map_err(map_memory_err)?;
        tx.execute(
            "DELETE FROM memories_fts_v2 WHERE rowid = ?1",
            rusqlite::params![id],
        )
        .map_err(map_memory_err)?;
    }
    tx.execute(
        "DELETE FROM memories WHERE key = ?1",
        rusqlite::params![key],
    )
    .map_err(map_memory_err)?;
    tx.commit().map_err(map_memory_err)?;
    Ok(CasOutcome::Ok(latest_rev))
}

#[cfg(test)]
mod tests;
