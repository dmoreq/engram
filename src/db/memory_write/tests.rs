//! Write-gate regression tests: an inactive revision (superseded or expired)
//! cannot be touched, never swallows a re-write as a duplicate, and keeps its
//! supersede chain intact when a new revision is appended.

use super::*;
use crate::db::test_support::remove_tree;

fn note<'a>(key: &'a str, body: &'a str, expires_at: i64) -> MemNoteInput<'a> {
    MemNoteInput {
        key,
        kind: "fact",
        tags: "",
        refs: "",
        scope: if expires_at == 0 {
            "durable"
        } else {
            "session"
        },
        session: "",
        body,
        source: "cli",
        expires_at,
        created_at: None,
        supersedes: None,
        touch: false,
    }
}

#[test]
fn cross_key_superseded_note_rejects_touch_and_can_be_rewritten_with_same_body() {
    let root = temp_root("mem-wr-superseded");
    let mut conn = open_memory_store(&root).unwrap();
    memory_write(&mut conn, &note("k1", "same body", 0), 100).unwrap();
    memory_write(
        &mut conn,
        &MemNoteInput {
            supersedes: Some("k1"),
            ..note("k2", "replacement", 0)
        },
        200,
    )
    .unwrap();

    // A dead key is unreachable by touch even though the row still exists.
    assert_eq!(memory_touch(&mut conn, "k1", 300).unwrap(), None);

    // Identical content must append a revision (never Duplicate) and restore
    // the FTS rows the supersession deleted.
    let out = memory_write(&mut conn, &note("k1", "same body", 0), 400).unwrap();
    assert!(matches!(out, MemWriteOutcome::Written { rev: 2, .. }));
    assert!(memory_get(&conn, "k1").unwrap().is_some());
    let new_id: i64 = conn
        .query_row(
            "SELECT id FROM memories WHERE key = 'k1' AND rev = 2",
            [],
            |r| r.get(0),
        )
        .unwrap();
    for table in ["memories_fts", "memories_fts_v2"] {
        let hits: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE rowid = ?1"),
                rusqlite::params![new_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1, "{table} must carry the restored row");
    }

    // Chain preservation: the dead rev-1 row still points at k2's revision id,
    // never at the re-stamped same-key revision.
    let k2_rev_id: i64 = conn
        .query_row(
            "SELECT id FROM memories WHERE key = 'k2' AND rev = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let superseded_by: Option<i64> = conn
        .query_row(
            "SELECT superseded_by FROM memories WHERE key = 'k1' AND rev = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(superseded_by, Some(k2_rev_id));
    drop(conn);
    remove_tree(&root);
}

#[test]
fn cross_key_supersede_marks_highest_non_superseded_target_rev() {
    let root = temp_root("mem-wr-cross-expired");
    let mut conn = open_memory_store(&root).unwrap();
    // rev 1 expires at 500; the re-write at 600 appends rev 2 and (since the
    // supersede-on-expiry change) marks rev 1 superseded. Force rev 1 back to
    // the legacy shape binaries before that change left behind, so the probe
    // below still faces two unsuperseded rows for one key.
    memory_write(&mut conn, &note("k1", "body one", 500), 100).unwrap();
    memory_write(&mut conn, &note("k1", "body two", 900), 600).unwrap();
    conn.execute(
        "UPDATE memories SET superseded_by = NULL WHERE key = 'k1' AND rev = 1",
        [],
    )
    .unwrap();
    let null_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM memories WHERE key = 'k1' AND superseded_by IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        null_rows, 2,
        "legacy-shape expired rev 1 and live rev 2 are both current"
    );

    // The cross-key probe must pick the highest non-superseded revision,
    // stated in SQL with ORDER BY rev DESC rather than borrowed from the
    // `memories_current` partial index's scan order.
    memory_write(
        &mut conn,
        &MemNoteInput {
            supersedes: Some("k1"),
            ..note("k2", "replacement", 0)
        },
        700,
    )
    .unwrap();

    let (rev1_by, rev2_by): (Option<i64>, Option<i64>) = conn
        .query_row(
            "SELECT MAX(CASE WHEN rev = 1 THEN superseded_by END),
                    MAX(CASE WHEN rev = 2 THEN superseded_by END)
             FROM memories WHERE key = 'k1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        rev1_by, None,
        "the expired rev 1 chain stays untouched by the cross-key probe"
    );
    let k2_rev_id: i64 = conn
        .query_row(
            "SELECT id FROM memories WHERE key = 'k2' AND rev = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rev2_by, Some(k2_rev_id), "the live rev 2 carries the chain");

    // No live current row remains: the pick falls to the expired rev 1 and
    // the expiry gate rejects it, so `mem get k1` reports unknown.
    assert!(memory_get(&conn, "k1").unwrap().is_none());
    drop(conn);
    remove_tree(&root);
}

/// The revision-preconditioned touch enforces the same inactive-revision
/// predicate as `memory_touch`: an expired revision is never re-anchored,
/// at and after the deadline (the `>` boundary).
#[test]
fn touch_if_rev_refuses_expired_revision() {
    let root = temp_root("mem-wr-touch-rev-expiry");
    let mut conn = open_memory_store(&root).unwrap();
    memory_write(&mut conn, &note("k", "session body", 500), 100).unwrap();
    assert_eq!(
        memory_touch_if_rev(&mut conn, "k", 1, 499).unwrap(),
        Some(1)
    );
    assert_eq!(memory_touch_if_rev(&mut conn, "k", 1, 500).unwrap(), None);
    assert_eq!(memory_touch_if_rev(&mut conn, "k", 1, 600).unwrap(), None);
    drop(conn);
    remove_tree(&root);
}

#[test]
fn touch_updates_created_and_expires_not_anchored() {
    let root = temp_root("mem-wr-touch");
    let mut conn = open_memory_store(&root).unwrap();
    memory_write(&mut conn, &note("k", "session body", 500), 100).unwrap();

    // The `--touch`-equivalent input re-asserts the invocation's TTL policy:
    // created_at moves to now, expires_at follows the normalized input, and
    // anchored_at is untouched (mem touch owns it).
    let out = memory_write(
        &mut conn,
        &MemNoteInput {
            touch: true,
            expires_at: 800,
            ..note("k", "session body", 500)
        },
        200,
    )
    .unwrap();
    assert!(matches!(out, MemWriteOutcome::Touched { rev: 1 }));
    let (created_at, anchored_at, expires_at): (i64, i64, i64) = conn
        .query_row(
            "SELECT created_at, anchored_at, expires_at FROM memories WHERE key = 'k'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(created_at, 200);
    assert_eq!(expires_at, 800);
    assert_eq!(anchored_at, 0, "--touch must never move anchored_at");
    drop(conn);
    remove_tree(&root);
}

#[test]
fn expired_note_is_inactive_at_and_after_deadline() {
    let root = temp_root("mem-wr-expiry");
    let mut conn = open_memory_store(&root).unwrap();
    memory_write(&mut conn, &note("k", "ttl body", 500), 100).unwrap();

    // The `>` boundary makes exactly-at-deadline inactive; one second before
    // it, the note is still live.
    assert_eq!(memory_touch(&mut conn, "k", 500).unwrap(), None);
    assert_eq!(memory_touch(&mut conn, "k", 499).unwrap(), Some(1));

    // Re-write after expiry appends rev 2 even with identical content.
    let out = memory_write(&mut conn, &note("k", "ttl body", 900), 600).unwrap();
    assert!(matches!(out, MemWriteOutcome::Written { rev: 2, .. }));
    let revs: i64 = conn
        .query_row("SELECT COUNT(*) FROM memories WHERE key = 'k'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(revs, 2);
    drop(conn);
    remove_tree(&root);
}
