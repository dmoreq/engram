//! Regression tests for the dashboard memories panel, moved out of
//! `memories.rs` to keep that file inside the 500-LOC hygiene gate. The
//! original inline test bodies are unchanged.

use super::*;

#[test]
fn global_store_is_not_nested_under_a_second_engram_dir() {
    let root = PathBuf::from("synthetic-global-root");
    let db = global_store_db(&root);
    assert_eq!(db, root.join("memory.db"));
    assert!(!db.to_string_lossy().contains(".engram/.engram"));
}

#[test]
fn tracked_store_is_reported_not_read() {
    let dir = crate::db::test_support::temp_root("tracked-dashboard-memory");
    let repo = dir.join("repo");
    crate::db::test_support::make_dir_all(repo.join(".engram")).unwrap();
    crate::db::test_support::write_file(repo.join(".engram/memory.db"), b"attacker").unwrap();
    let status = crate::db::test_support::stripped_git()
        .arg("-C")
        .arg(&repo)
        .args(["init", "-q"])
        .status();
    if !matches!(status, Ok(s) if s.success()) {
        crate::db::test_support::remove_tree(&dir);
        return;
    }
    let added = crate::db::test_support::stripped_git()
        .arg("-C")
        .arg(&repo)
        .args(["add", "-f", ".engram/memory.db"])
        .status()
        .unwrap();
    assert!(added.success());
    let store = Store {
        name: "repo".to_string(),
        repo_root: repo.clone(),
        db_path: repo.join(".engram/memory.db"),
        store_id: "tracked".to_string(),
        guard_root: Some(repo),
    };
    let mut notes = Vec::new();
    let err = collect_store_notes(&store, &HashMap::new(), &mut notes).unwrap_err();
    assert_eq!(err["stage"], "security", "{err}");
    assert!(err["message"].as_str().unwrap().contains("git-tracked"));
    assert!(notes.is_empty());
    crate::db::test_support::remove_tree(&dir);
}

#[test]
fn refs_without_an_index_are_unverified_not_broken() {
    let store = Store {
        name: "global".to_string(),
        repo_root: PathBuf::from("/nonexistent-eg-global"),
        db_path: PathBuf::from("/nonexistent-eg-global/memory.db"),
        store_id: "global".to_string(),
        guard_root: None,
    };
    let row = MemoryRow {
        id: 1,
        key: "k".to_string(),
        rev: 1,
        kind: "fact".to_string(),
        scope: "durable".to_string(),
        body: "b".to_string(),
        tags_raw: String::new(),
        refs_raw: "src/main.rs".to_string(),
        source: "cli".to_string(),
        created_at: 100,
        anchored_at: 0,
        is_current: true,
    };
    let note = format_note_json(&store, row, None, &HashMap::new());
    assert_eq!(note["ref_status"], "unverified");
    assert_eq!(note["resolved_refs"][0]["status"], "unverified");
}

#[test]
fn recall_counts_only_exact_key_reads() {
    let entries = vec![
        json!({ "tool": "mem_get", "args": { "key": "alpha" }, "ts": 10.0, "exit_code": 0, "result_count": 1 }),
        json!({ "tool": "mem_get", "args": { "key": "alpha" }, "ts": 12.0, "exit_code": 0, "result_count": 1 }),
        // A bare store-wide query cannot be attributed to one note.
        json!({ "tool": "mem_query", "args": { "terms": "" }, "ts": 13.0, "exit_code": 0, "result_count": 3 }),
        // A different key must not accrue.
        json!({ "tool": "mem_get", "args": { "key": "beta" }, "ts": 14.0, "exit_code": 0, "result_count": 1 }),
        // A failed or empty lookup delivered nothing: not a recall.
        json!({ "tool": "mem_get", "args": { "key": "alpha" }, "ts": 15.0, "exit_code": 1, "result_count": 0 }),
        json!({ "tool": "mem_get", "args": { "key": "alpha" }, "ts": 16.0, "exit_code": 0, "result_count": 0 }),
    ];
    let recalls = aggregate_recalls(&entries);
    assert_eq!(recalls.get("alpha").copied(), Some((2, 12.0)));
    assert_eq!(recalls.get("beta").copied(), Some((1, 14.0)));
    assert!(!recalls.contains_key(""));
}

/// Write one session note with the given TTL deadline into a temp store.
fn write_session_note(root: &Path, key: &str, body: &str, expires_at: i64, now: i64) {
    let mut conn = crate::db::open_memory_store(root).unwrap();
    crate::db::memory_write(
        &mut conn,
        &crate::db::MemNoteInput {
            key,
            kind: "fact",
            tags: "",
            refs: "",
            scope: "session",
            session: "",
            body,
            source: "cli",
            expires_at,
            supersedes: None,
            touch: false,
        },
        now,
    )
    .unwrap();
}

/// The first note matching `key` (and `rev` when given); panics otherwise, so
/// a store that failed to contribute rows fails the test loudly.
fn find_note<'a>(payload: &'a Value, key: &str, rev: Option<i64>) -> &'a Value {
    payload["notes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| {
            n["key"].as_str() == Some(key) && rev.is_none_or(|r| n["rev"].as_i64() == Some(r))
        })
        .unwrap_or_else(|| panic!("missing note {key} (rev {rev:?}) in {}", payload["notes"]))
}

#[test]
fn dashboard_memories_marks_expired_session_notes_as_not_current() {
    let dir = crate::db::test_support::temp_root("dashboard-mem-expiry");
    let root = dir.join("repo");
    crate::db::test_support::make_dir_all(&root).unwrap();
    let now = crate::db::unix_now();
    write_session_note(
        &root,
        "live-session",
        "still inside its TTL",
        now + 3600,
        now,
    );
    write_session_note(
        &root,
        "expired-session",
        "TTL already passed",
        now - 3600,
        now,
    );

    let payload = collect_memories(Some(&root), &[]).unwrap();
    assert_eq!(
        find_note(&payload, "live-session", None)["is_current"],
        true
    );
    assert_eq!(
        find_note(&payload, "expired-session", None)["is_current"],
        false
    );
    crate::db::test_support::remove_tree(&dir);
}

#[test]
fn v1_store_without_expires_column_still_collects() {
    let dir = crate::db::test_support::temp_root("dashboard-mem-v1");
    let root = dir.join("repo");
    crate::db::test_support::make_dir_all(root.join(".engram")).unwrap();
    // Deterministic v1 fixture: the current schema text minus every line
    // naming `expires_at` (the column and its partial index) — no `DROP
    // COLUMN` dependency and no implementer choice in the fixture.
    let v1_schema = include_str!("../../../../schema/memory.sql")
        .lines()
        .filter(|line| !line.contains("expires_at"))
        .collect::<Vec<_>>()
        .join("\n");
    let db_path = root.join(".engram").join("memory.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute_batch(&v1_schema).unwrap();
    conn.execute_batch(
        "INSERT INTO memories(key, rev, kind, scope, body, content_hash, source, created_at)
         VALUES ('v1-live', 2, 'fact', 'durable', 'current row', 1, 'cli', 100),
                ('v1-live', 1, 'fact', 'durable', 'superseded row', 2, 'cli', 90);
         UPDATE memories
         SET superseded_by = (SELECT id FROM memories WHERE key = 'v1-live' AND rev = 2)
         WHERE key = 'v1-live' AND rev = 1;",
    )
    .unwrap();
    drop(conn);

    // No binding error: rows collect with `is_current` decided by the
    // supersession predicate alone (the v1 store has no TTL concept).
    let payload = collect_memories(Some(&root), &[]).unwrap();
    assert_eq!(find_note(&payload, "v1-live", Some(2))["is_current"], true);
    assert_eq!(find_note(&payload, "v1-live", Some(1))["is_current"], false);
    crate::db::test_support::remove_tree(&dir);
}
