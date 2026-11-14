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
    let filter = crate::db::MemFilter {
        kind: None,
        scope: None,
        now: crate::db::unix_now(),
        include_superseded: false,
        window: crate::memory::TimeWindow {
            since: None,
            until: None,
        },
    };
    let Ok(rows) = crate::db::memory_current_rows(&conn, &filter, None) else {
        return HashSet::new();
    };
    let mut pairs = HashSet::new();
    for row in rows {
        let Some(pair) = row.key.strip_prefix("reuse-allow/") else {
            continue;
        };
        let Some((target, canonical)) = pair.split_once("->") else {
            continue;
        };
        let refs: Vec<&str> = row.refs.split_whitespace().collect();
        let anchor = row.anchor();
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
        return crate::db::check_ref_status(ctx.conn, raw, anchor) == crate::db::RefStatus::Valid;
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
            canonical: "a.parse".into(),
            canonical_file: "a.py".into(),
            canonical_line: 1,
            canonical_callers: 0,
            overlap_pct: Some(100),
            shared_items: Vec::new(),
            raw_client: None,
            confidence,
            reasons: Vec::new(),
            freshness: Freshness::Fresh,
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
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("../../../schema/index.sql"))
            .unwrap();
        conn.execute(
            "INSERT INTO files(id, path, size, mtime, content_hash, parse_status, error_nodes)
             VALUES (NULL, 'src/util.py', 10, 10_000_000_000, 1, 'ok', 0)",
            [],
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
            root: std::path::Path::new("."),
        };
        // Symbol-shaped refs resolve through the shared ref-status check's
        // symbol arm: an anchor at or after the indexed mtime is fresh, an
        // anchor before it is modified, and an unresolvable name is never
        // fresh.
        assert!(ref_is_fresh(&ctx, "util.normalize", 20));
        assert!(!ref_is_fresh(&ctx, "util.normalize", 5));
        assert!(!ref_is_fresh(&ctx, "util.missing", 1_000_000_000));
    }
}
