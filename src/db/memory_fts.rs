//! Derived Unicode FTS index: completeness marker, tokenizer epoch, and the
//! chunked reconcile that repairs rows written by older binaries.

use super::*;
use rusqlite::OptionalExtension;

/// Tokenizer contract version stored in `memory_meta.fts_epoch`. Epoch 3
/// keeps combining marks inside Unicode tokens (NFD Vietnamese words stop
/// splitting at tone marks); the mismatch branch below drops and rebuilds
/// `memories_fts_v2` on the next write open, and a reverted binary rebuilds
/// back under the old tokenizer (self-healing in both directions).
const MEM_FTS_EPOCH: &str = "3";
/// Rows reconciled per transaction during a v2 backfill.
const MEM_FTS_CHUNK: usize = 5_000;

fn meta_get(conn: &Connection, key: &str) -> Result<Option<String>> {
    conn.query_row(
        "SELECT value FROM memory_meta WHERE key = ?1",
        rusqlite::params![key],
        |r| r.get(0),
    )
    .optional()
    .map_err(map_memory_err)
}

fn meta_set(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO memory_meta(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![key, value],
    )
    .map_err(map_memory_err)?;
    Ok(())
}

fn v2_tokenizer_ok(conn: &Connection) -> Result<bool> {
    let sql: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'memories_fts_v2'",
            [],
            |r| r.get(0),
        )
        .optional()
        .map_err(map_memory_err)?;
    Ok(sql.is_some_and(|s| s.contains("remove_diacritics 2")))
}

/// True only when v2 exists and a clean reconcile completed.
pub(crate) fn memory_fts_v2_usable(conn: &Connection) -> Result<bool> {
    if !super::query_state::object_exists_result(conn, "table", "memories_fts_v2")
        .map_err(map_memory_err)?
    {
        return Ok(false);
    }
    if !super::query_state::object_exists_result(conn, "table", "memory_meta")
        .map_err(map_memory_err)?
    {
        return Ok(false);
    }
    let epoch = meta_get(conn, "fts_epoch")?;
    let complete = meta_get(conn, "fts_v2_complete")?;
    Ok(epoch.as_deref() == Some(MEM_FTS_EPOCH) && complete.as_deref() == Some("1"))
}

fn fts_has_pending(conn: &Connection) -> Result<bool> {
    // The partial `memories_pending` index makes this probe O(1) on a healthy
    // store. Missing v2 rows after a crash are covered by the completeness
    // marker, which is cleared before the first repair chunk.
    let pending: i64 = conn
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM memories
                WHERE superseded_by IS NULL
                  AND (expires_at = 0 OR expires_at > ?1)
                  AND fts_version < 2
                LIMIT 1)",
            rusqlite::params![unix_now()],
            |r| r.get(0),
        )
        .map_err(map_memory_err)?;
    Ok(pending != 0)
}

fn prepare_fts_v2(conn: &Connection, force: bool) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE")
        .map_err(map_memory_err)?;
    let step = (|| -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS memory_meta(key TEXT PRIMARY KEY, value TEXT NOT NULL)",
        )
        .map_err(map_memory_err)?;
        let epoch = meta_get(conn, "fts_epoch")?;
        let tokenizer_ok = v2_tokenizer_ok(conn)?;
        if force || epoch.as_deref() != Some(MEM_FTS_EPOCH) || !tokenizer_ok {
            meta_set(conn, "fts_v2_complete", "0")?;
            meta_set(conn, "fts_epoch", MEM_FTS_EPOCH)?;
            conn.execute_batch(
                "DROP TABLE IF EXISTS memories_fts_v2;
                 CREATE VIRTUAL TABLE memories_fts_v2 USING fts5(text, tokenize='unicode61 remove_diacritics 2');",
            )
            .map_err(map_memory_err)?;
            conn.execute("UPDATE memories SET fts_version = 0", [])
                .map_err(map_memory_err)?;
            let now = unix_now();
            for table in ["memories_fts", "memories_fts_v2"] {
                conn.execute(
                    &format!(
                        "DELETE FROM {table} WHERE rowid NOT IN (
                            SELECT id FROM memories
                            WHERE superseded_by IS NULL
                              AND (expires_at = 0 OR expires_at > ?1))"
                    ),
                    rusqlite::params![now],
                )
                .map_err(map_memory_err)?;
            }
        } else {
            meta_set(conn, "fts_v2_complete", "0")?;
            conn.execute_batch(
                "CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts_v2
                 USING fts5(text, tokenize='unicode61 remove_diacritics 2');",
            )
            .map_err(map_memory_err)?;
        }
        Ok(())
    })();
    match step {
        Ok(()) => conn.execute_batch("COMMIT").map_err(map_memory_err),
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// Pending-row selector arm A: rows never v2-indexed. `fts_version < 2` stays
/// an AND-ed term so the partial `memories_pending` index (defined on that
/// term alone) drives the scan; folding it into the old OR chain defeated the
/// index and forced a full table scan with two subqueries per chunk.
const FTS_PENDING_ARM: &str = "SELECT id, key, tags, body FROM memories
     WHERE superseded_by IS NULL
       AND (expires_at = 0 OR expires_at > ?1)
       AND fts_version < 2
     ORDER BY id LIMIT ?2";

/// Selector arm B: rows marked v2-indexed whose FTS rows went missing anyway
/// (a crash between the FTS insert and the marker update). Disjoint from arm
/// A (`fts_version` ranges do not overlap), so the UNION ALL batch semantics
/// match the single OR-chained selector it replaced.
const FTS_MISSING_ARM: &str = "SELECT id, key, tags, body FROM memories
     WHERE superseded_by IS NULL
       AND (expires_at = 0 OR expires_at > ?1)
       AND fts_version >= 2
       AND (id NOT IN (SELECT rowid FROM memories_fts)
            OR id NOT IN (SELECT rowid FROM memories_fts_v2))
     ORDER BY id LIMIT ?2";

/// Reconcile one batch. Returns the number of rows indexed; 0 means done.
fn reconcile_fts_chunk(conn: &Connection) -> Result<usize> {
    conn.execute_batch("BEGIN IMMEDIATE")
        .map_err(map_memory_err)?;
    let step = (|| -> Result<usize> {
        meta_set(conn, "fts_v2_complete", "0")?;
        let rows: Vec<(i64, String, String, String)> = {
            // Each arm is bounded by the chunk limit inside its subselect; the
            // outer select re-orders by id and re-limits to the chunk size.
            // Each arm is bounded by the chunk limit inside its own FROM
            // subselect (SQLite compound operands must be bare SELECTs); the
            // outer select re-orders by id and re-limits to the chunk size.
            let selector = format!(
                "SELECT id, key, tags, body FROM (
                    SELECT * FROM ({FTS_PENDING_ARM})
                    UNION ALL
                    SELECT * FROM ({FTS_MISSING_ARM})
                 ) ORDER BY id LIMIT ?2"
            );
            let mut stmt = conn.prepare(&selector).map_err(map_memory_err)?;
            let mapped = stmt
                .query_map(rusqlite::params![unix_now(), MEM_FTS_CHUNK as i64], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                })
                .map_err(map_memory_err)?;
            mapped
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(map_memory_err)?
        };
        if rows.is_empty() {
            return Ok(0);
        }
        // Fixed table-name SQL prepared once per chunk: the per-row
        // `execute(format!(...))` loop recompiled five statements per row.
        let mut del_legacy = conn
            .prepare("DELETE FROM memories_fts WHERE rowid = ?1")
            .map_err(map_memory_err)?;
        let mut del_v2 = conn
            .prepare("DELETE FROM memories_fts_v2 WHERE rowid = ?1")
            .map_err(map_memory_err)?;
        let mut ins_legacy = conn
            .prepare("INSERT INTO memories_fts(rowid, text) VALUES (?1, ?2)")
            .map_err(map_memory_err)?;
        let mut ins_v2 = conn
            .prepare("INSERT INTO memories_fts_v2(rowid, text) VALUES (?1, ?2)")
            .map_err(map_memory_err)?;
        let mut mark = conn
            .prepare_cached("UPDATE memories SET fts_version = 2 WHERE id = ?1")
            .map_err(map_memory_err)?;
        for (id, key, tags, body) in &rows {
            let legacy = crate::memory::mem_fts_text_legacy(key, tags, body);
            let unicode = crate::memory::mem_fts_text(key, tags, body);
            del_legacy
                .execute(rusqlite::params![id])
                .map_err(map_memory_err)?;
            del_v2
                .execute(rusqlite::params![id])
                .map_err(map_memory_err)?;
            ins_legacy
                .execute(rusqlite::params![id, legacy])
                .map_err(map_memory_err)?;
            ins_v2
                .execute(rusqlite::params![id, unicode])
                .map_err(map_memory_err)?;
            mark.execute(rusqlite::params![id])
                .map_err(map_memory_err)?;
        }
        Ok(rows.len())
    })();
    match step {
        Ok(n) => {
            conn.execute_batch("COMMIT").map_err(map_memory_err)?;
            Ok(n)
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

fn finish_fts_v2(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE")
        .map_err(map_memory_err)?;
    let step = meta_set(conn, "fts_v2_complete", "1");
    match step {
        Ok(()) => conn.execute_batch("COMMIT").map_err(map_memory_err),
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// Reconcile the Unicode index against current memory rows. Returns the number
/// of rows indexed in this call. `force` drops and recreates the index.
pub(crate) fn ensure_memory_fts_v2(conn: &Connection, force: bool) -> Result<u64> {
    if !force && memory_fts_v2_usable(conn)? && !fts_has_pending(conn)? {
        return Ok(0);
    }
    prepare_fts_v2(conn, force)?;
    let mut total = 0u64;
    loop {
        let n = reconcile_fts_chunk(conn)?;
        total += n as u64;
        if n == 0 {
            finish_fts_v2(conn)?;
            break;
        }
    }
    if total > MEM_FTS_CHUNK as u64 {
        eprintln!("eg: memory: reindexed {total} rows so far");
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::remove_tree;

    fn session_note<'a>(key: &'a str, body: &'a str) -> crate::db::MemNoteInput<'a> {
        crate::db::MemNoteInput {
            key,
            kind: "fact",
            tags: "",
            refs: "",
            scope: "durable",
            session: "",
            body,
            source: "cli",
            expires_at: 0,
            created_at: None,
            embedding: None,
            supersedes: None,
            touch: false,
        }
    }

    #[test]
    fn fts_epoch_mismatch_rebuilds_unicode_index() {
        let root = temp_root("mem-fts-epoch");
        let mut conn = open_memory_store(&root).unwrap();
        crate::db::memory_write(&mut conn, &session_note("k", "Bộ nhớ dự án"), unix_now()).unwrap();
        conn.execute(
            "UPDATE memory_meta SET value = '2' WHERE key = 'fts_epoch'",
            [],
        )
        .unwrap();
        drop(conn);
        // The next write open detects the stale epoch, drops and rebuilds
        // memories_fts_v2, and reconciles every row back to fts_version = 2.
        let conn = open_memory_store(&root).unwrap();
        let epoch: String = conn
            .query_row(
                "SELECT value FROM memory_meta WHERE key = 'fts_epoch'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(epoch, MEM_FTS_EPOCH);
        let stale: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memories WHERE fts_version < 2",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stale, 0);
        assert!(memory_fts_v2_usable(&conn).unwrap());
        drop(conn);
        remove_tree(&root);
    }

    #[test]
    fn nfd_body_matches_nfc_query_at_fts_layer() {
        let root = temp_root("mem-fts-nfd");
        let mut conn = open_memory_store(&root).unwrap();
        // NFD spellings (combining tone marks) and their NFC spellings.
        let nfd = "ti\u{0065}\u{0302}\u{0301}ng Vi\u{0065}\u{0302}\u{0301}t";
        let nfc = "tiếng Việt";
        crate::db::memory_write(&mut conn, &session_note("nfd-note", nfd), unix_now()).unwrap();
        crate::db::memory_write(&mut conn, &session_note("nfc-note", nfc), unix_now()).unwrap();
        let nfd_id: i64 = conn
            .query_row("SELECT id FROM memories WHERE key = 'nfd-note'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let nfc_id: i64 = conn
            .query_row("SELECT id FROM memories WHERE key = 'nfc-note'", [], |r| {
                r.get(0)
            })
            .unwrap();

        // The cross-match contract lives at the FTS5 fold: an NFC-spelled
        // query must reach the NFD note, and an NFD-spelled query the NFC note.
        for (query, expected) in [
            (crate::memory::mem_query_terms(nfc), nfd_id),
            (crate::memory::mem_query_terms(nfd), nfc_id),
        ] {
            let mut stmt = conn
                .prepare("SELECT rowid FROM memories_fts_v2 WHERE memories_fts_v2 MATCH ?1")
                .unwrap();
            let hits: Vec<i64> = stmt
                .query_map(rusqlite::params![query], |r| r.get(0))
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap();
            assert!(
                hits.contains(&expected),
                "query `{query}` missed {expected}"
            );
        }
        drop(conn);
        remove_tree(&root);
    }

    #[test]
    fn reconcile_uses_pending_index() {
        let root = temp_root("mem-fts-plan");
        let conn = open_memory_store(&root).unwrap();
        let plan: Vec<String> = {
            let mut stmt = conn
                .prepare(&format!("EXPLAIN QUERY PLAN {FTS_PENDING_ARM}"))
                .unwrap();
            stmt.query_map(rusqlite::params![unix_now(), MEM_FTS_CHUNK as i64], |r| {
                r.get::<_, String>(3)
            })
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap()
        };
        assert!(
            plan.iter().any(|d| d.contains("memories_pending")),
            "arm A must drive the scan through memories_pending: {plan:?}"
        );
        drop(conn);
        remove_tree(&root);
    }

    #[test]
    fn reconcile_repairs_missing_fts_rows() {
        let root = temp_root("mem-fts-repair");
        let mut conn = open_memory_store(&root).unwrap();
        for key in ["a", "b", "c"] {
            crate::db::memory_write(&mut conn, &session_note(key, "shared body"), unix_now())
                .unwrap();
        }
        // One row never v2-indexed (fts_version 0), one marked indexed but
        // missing only from the v2 table.
        conn.execute("UPDATE memories SET fts_version = 0 WHERE key = 'a'", [])
            .unwrap();
        let b_id: i64 = conn
            .query_row("SELECT id FROM memories WHERE key = 'b'", [], |r| r.get(0))
            .unwrap();
        conn.execute(
            "DELETE FROM memories_fts_v2 WHERE rowid = ?1",
            rusqlite::params![b_id],
        )
        .unwrap();

        loop {
            let n = reconcile_fts_chunk(&conn).unwrap();
            if n == 0 {
                break;
            }
        }
        let stale: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memories WHERE fts_version < 2",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stale, 0);
        for table in ["memories_fts", "memories_fts_v2"] {
            let hits: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(hits, 3, "{table} must carry every active row");
        }
        drop(conn);
        remove_tree(&root);
    }
}
