//! Phase 3 opt-in allowlist. A current memory note with key
//! `reuse-allow/<target>-><canonical>` suppresses an intentional duplicate.
//! High-confidence pairs are never suppressed. Ref-bearing notes suppress
//! only while every ref names an indexed file that is unchanged on disk and
//! no newer than the note's anchor (`mem touch` refreshes the anchor);
//! rebuilding the index after a referenced file changed therefore re-reports
//! the duplicate until the note is touched.

use std::collections::HashSet;

use super::*;

pub(super) fn load(ctx: &QueryCtx<'_>) -> HashSet<(String, String)> {
    let Ok(conn) = crate::db::open_memory_query(ctx.root) else {
        return HashSet::new();
    };
    let now = crate::db::unix_now();
    let Ok(mut stmt) = conn.prepare(
        "SELECT key, refs, created_at, anchored_at FROM memories
         WHERE superseded_by IS NULL
           AND key >= 'reuse-allow/' AND key < 'reuse-allow0'
           AND (expires_at = 0 OR expires_at > ?1)",
    ) else {
        return HashSet::new();
    };
    let Ok(rows) = stmt.query_map([now], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
        ))
    }) else {
        return HashSet::new();
    };
    let mut pairs = HashSet::new();
    for row in rows {
        let Ok((key, refs_str, created_at, anchored_at)) = row else {
            continue;
        };
        let Some(pair) = key.strip_prefix("reuse-allow/") else {
            continue;
        };
        let Some((target, canonical)) = pair.split_once("->") else {
            continue;
        };
        let refs: Vec<&str> = refs_str.split_whitespace().collect();
        let anchor = anchored_at.max(created_at);
        if !refs.is_empty() && !refs.iter().all(|r| ref_is_fresh(ctx, r, anchor)) {
            continue;
        }
        pairs.insert((target.to_string(), canonical.to_string()));
    }
    pairs
}

/// Freshness for one `--refs` token, compared against the note's `anchor`.
/// Refs are consumed verbatim — no `path:line` splitting — so suppression and
/// the `mem query`/dashboard ref display always agree on the same string.
/// The shape branch resolves against the index instead of splitting on `/`:
/// a token with an exact `files` row is a file ref and needs that row plus an
/// unchanged on-disk file before the shared ref-status check compares the
/// indexed mtime against the anchor — bare filenames like `b.py` keep the
/// disk check, and the exact-row precondition keeps a moved file's old path
/// from failing open into the shared check's LIKE suffix arm (a slash-shaped
/// token with no exact row fails closed instead of matching
/// `vendor/src/old.py`). A token with no files row and no `/` is a symbol ref
/// and resolves through the shared check's symbol arm.
fn ref_is_fresh(ctx: &QueryCtx<'_>, raw: &str, anchor: i64) -> bool {
    if !crate::query::staleness::safe_index_path(raw) {
        return false;
    }
    let indexed: Option<i64> = ctx
        .conn
        .query_row("SELECT id FROM files WHERE path = ?1", [raw], |row| {
            row.get(0)
        })
        .ok();
    if indexed.is_none() && !raw.contains('/') {
        if crate::db::check_ref_status(ctx.conn, raw, anchor) != crate::db::RefStatus::Valid {
            return false;
        }
        let Ok(mut stmt) = ctx
            .conn
            .prepare("SELECT DISTINCT file_path FROM symbols WHERE qualname = ?1 OR name = ?1")
        else {
            return false;
        };
        let Ok(paths) = stmt.query_map([raw], |row| row.get::<_, String>(0)) else {
            return false;
        };
        let mut any = false;
        for path in paths {
            let Ok(file_path) = path else {
                return false;
            };
            any = true;
            if crate::query::staleness::file_changed(ctx, &file_path) {
                return false;
            }
        }
        return any;
    }
    indexed.is_some()
        && !crate::query::staleness::file_changed(ctx, raw)
        && crate::db::check_ref_status(ctx.conn, raw, anchor) == crate::db::RefStatus::Valid
}

pub(super) fn is_allowed(pairs: &HashSet<(String, String)>, candidate: &ReuseCandidate) -> bool {
    candidate.confidence != ReuseConfidence::High
        && pairs.contains(&(candidate.symbol.clone(), candidate.canonical.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(confidence: ReuseConfidence) -> ReuseCandidate {
        ReuseCandidate {
            detector: DetectorId::D2,
            symbol: "b.parse".into(),
            file: "b.py".into(),
            line: 1,
            line_end: 2,
            canonical: "a.parse".into(),
            canonical_file: "a.py".into(),
            canonical_line: 1,
            canonical_line_end: 2,
            canonical_callers: 0,
            overlap_pct: Some(100),
            shared_items: Vec::new(),
            raw_client: None,
            confidence,
            reasons: Vec::new(),
        }
    }

    #[test]
    fn high_confidence_is_never_suppressed() {
        let pairs = HashSet::from([("b.parse".into(), "a.parse".into())]);
        assert!(!is_allowed(&pairs, &candidate(ReuseConfidence::High)));
        assert!(is_allowed(&pairs, &candidate(ReuseConfidence::Medium)));
        assert!(is_allowed(&pairs, &candidate(ReuseConfidence::Low)));
    }

    #[test]
    fn symbol_ref_freshness_follows_check_ref_status() {
        use crate::db::test_support::{make_dir_all, remove_tree, temp_root, write_file};

        let root = temp_root("allowlist-sym-ref");
        make_dir_all(root.join("src")).unwrap();
        let file = root.join("src/util.py");
        write_file(&file, b"def normalize(x): return x\n").unwrap();
        let meta = std::fs::metadata(&file).unwrap();
        let mtime = crate::db::metadata_mtime(&meta).unwrap();
        let hash = crate::hash::fnv1a64(b"def normalize(x): return x\n") as i64;

        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("../../../schema/index.sql"))
            .unwrap();
        conn.execute(
            "INSERT INTO files(id, path, size, mtime, content_hash, parse_status, error_nodes)
             VALUES (NULL, 'src/util.py', ?1, ?2, ?3, 'ok', 0)",
            rusqlite::params![meta.len() as i64, mtime, hash],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO symbols(id, kind, name, qualname, file_id, file_path, line_start, line_end)
             VALUES (NULL, 'function', 'normalize', 'util.normalize', 1, 'src/util.py', 1, 2)",
            [],
        )
        .unwrap();
        let ctx = QueryCtx {
            conn: &conn,
            root: &root,
        };
        let mtime_secs = mtime / 1_000_000_000;
        assert!(ref_is_fresh(&ctx, "util.normalize", mtime_secs + 10));
        assert!(!ref_is_fresh(&ctx, "util.normalize", mtime_secs - 1));
        assert!(!ref_is_fresh(&ctx, "util.missing", mtime_secs + 10));

        // Modifying the backing file on disk must invalidate the symbol-ref
        // allowlist even before `eg build` updates `files.mtime`.
        write_file(&file, b"def normalize(x): return x + 1\n").unwrap();
        assert!(!ref_is_fresh(&ctx, "util.normalize", mtime_secs + 100));
        remove_tree(&root);
    }
}
