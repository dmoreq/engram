use super::*;

#[test]
fn memory_ref_candidate_rows_matches_normalized_tokens_and_respects_cap() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(include_str!("../../../schema/memory.sql"))
        .unwrap();
    conn.execute_batch(
        "INSERT INTO memories (key, rev, kind, scope, session, body, tags, refs, content_hash, source, created_at, expires_at)
         VALUES
           ('k1', 1, 'fact', 'durable', '', 'b1', '', './src/db/open.rs', 1, 'cli', 100, 0),
           ('k2', 1, 'fact', 'durable', '', 'b2', '', 'MyClass.helper', 2, 'cli', 200, 0),
           ('k3', 1, 'fact', 'durable', '', 'b3', '', 'db::resolve_ref', 3, 'cli', 300, 0),
           ('k4', 1, 'fact', 'durable', '', 'b4', '', 'unrelated.py', 4, 'cli', 400, 0);",
    )
    .unwrap();

    let win = crate::memory::TimeWindow::default();
    let rows = memory_ref_candidate_rows(
        &conn,
        &[
            "open.rs".to_string(),
            "helper".to_string(),
            "resolve_ref".to_string(),
        ],
        &win,
        1000,
        10,
    )
    .unwrap();
    let keys: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
    assert_eq!(keys, vec!["k3", "k2", "k1"]);

    let capped = memory_ref_candidate_rows(
        &conn,
        &["open.rs".to_string(), "helper".to_string()],
        &win,
        1000,
        1,
    )
    .unwrap();
    assert_eq!(capped.len(), 1);
    assert_eq!(capped[0].key, "k2");
}

#[test]
fn memory_ref_candidate_rows_matches_line_suffix_substring_and_orders_by_id_desc() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(include_str!("../../../schema/memory.sql"))
        .unwrap();
    conn.execute_batch(
        "INSERT INTO memories (key, rev, kind, scope, session, body, tags, refs, content_hash, source, created_at, expires_at)
         VALUES
           ('k1', 1, 'fact', 'durable', '', 'b1', '', 'src/db/open.rs:42', 1, 'cli', 500, 0),
           ('k2', 1, 'fact', 'durable', '', 'b2', '', 'resolve_ref_detail', 2, 'cli', 100, 0);",
    )
    .unwrap();
    let win = crate::memory::TimeWindow::default();
    let rows = memory_ref_candidate_rows(
        &conn,
        &["db/open.rs".to_string(), "resolve_ref".to_string()],
        &win,
        1000,
        10,
    )
    .unwrap();
    let keys: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
    assert_eq!(keys, vec!["k2", "k1"]);
}

#[test]
fn memory_embeddings_returns_empty_without_embedding_column() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE memories (id INTEGER PRIMARY KEY, key TEXT NOT NULL, body TEXT NOT NULL);
         INSERT INTO memories (id, key, body) VALUES (1, 'k1', 'hello');",
    )
    .unwrap();
    assert!(memory_embeddings(&conn, &[1]).unwrap().is_empty());
}
