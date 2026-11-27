use super::*;
use crate::db::test_support::remove_tree;

fn note<'a>(key: &'a str, body: &'a str, expected_rev: Option<i64>) -> MemNoteInput<'a> {
    MemNoteInput {
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
        expected_rev,
    }
}

#[test]
fn retire_purges_fts_and_restore_brings_note_back() {
    let root = temp_root("mem-retire-restore");
    let mut conn = open_memory_store(&root).unwrap();
    memory_write(&mut conn, &note("k1", "unique_retire_token", None), 100).unwrap();

    assert_eq!(
        memory_retire(&mut conn, "k1", Some(1), 200).unwrap(),
        CasOutcome::Ok(1)
    );
    assert!(memory_get(&conn, "k1").unwrap().is_none());
    assert!(memory_get_with_options(&conn, "k1", true)
        .unwrap()
        .is_some());
    let fts_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM memories_fts_v2 WHERE memories_fts_v2 MATCH 'unique_retire_token'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(fts_count, 0);

    // Retiring again fails with Conflict when expected_rev is given, NotFound without it.
    assert_eq!(
        memory_retire(&mut conn, "k1", Some(1), 250).unwrap(),
        CasOutcome::Conflict {
            expected_rev: 1,
            current_rev: None
        }
    );
    assert_eq!(
        memory_retire(&mut conn, "k1", None, 250).unwrap(),
        CasOutcome::NotFound
    );

    // Restore inserts rev + 1 and brings it back into active queries and FTS5.
    assert_eq!(memory_restore(&mut conn, "k1", 300).unwrap(), Some(2));
    assert!(memory_get(&conn, "k1").unwrap().is_some());
    let fts_restored: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM memories_fts_v2 WHERE memories_fts_v2 MATCH 'unique_retire_token'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(fts_restored, 1);

    // Restoring an active (non-retired) note exits 2 (Usage).
    let active_err = memory_restore(&mut conn, "k1", 310).unwrap_err();
    assert_eq!(active_err.exit_code(), 2);

    drop(conn);
    remove_tree(&root);
}

#[test]
fn expected_rev_cas_guards_write_touch_retire_and_delete() {
    let root = temp_root("mem-cas");
    let mut conn = open_memory_store(&root).unwrap();
    memory_write(&mut conn, &note("k", "v1", None), 100).unwrap();

    // Write with stale expected_rev=99 fails before modifying DB.
    let err = memory_write(&mut conn, &note("k", "v2", Some(99)), 200).unwrap_err();
    assert_eq!(err.exit_code(), 2);

    // Even identical content with wrong expected_rev must fail (not Duplicate).
    let err_dup = memory_write(&mut conn, &note("k", "v1", Some(99)), 200).unwrap_err();
    assert_eq!(err_dup.exit_code(), 2);

    // Matching expected_rev=1 succeeds and bumps to rev 2.
    let out = memory_write(&mut conn, &note("k", "v2", Some(1)), 200).unwrap();
    assert!(matches!(out, MemWriteOutcome::Written { rev: 2, .. }));

    // Touch and delete with stale rev=1 fail with Conflict.
    assert_eq!(
        memory_touch_if_rev(&mut conn, "k", 1, 300).unwrap(),
        CasOutcome::Conflict {
            expected_rev: 1,
            current_rev: Some(2)
        }
    );
    assert_eq!(
        memory_delete_if_rev(&mut conn, "k", Some(1), 300).unwrap(),
        CasOutcome::Conflict {
            expected_rev: 1,
            current_rev: Some(2)
        }
    );

    // Delete with rev=2 succeeds.
    assert_eq!(
        memory_delete_if_rev(&mut conn, "k", Some(2), 300).unwrap(),
        CasOutcome::Ok(2)
    );
    drop(conn);
    remove_tree(&root);
}
