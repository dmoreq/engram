//! Ref-status regression tests: LIKE metacharacters in a ref stay literals,
//! and the exact-or-suffix candidate pick is deterministic.

use super::*;
use rusqlite::Connection;

fn open_index() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(include_str!("../../../schema/index.sql"))
        .unwrap();
    conn
}

fn insert_file(conn: &Connection, path: &str, mtime: i64) {
    conn.execute(
        "INSERT INTO files(id, path, size, mtime, content_hash, parse_status, error_nodes)
         VALUES (NULL, ?1, 10, ?2, 1, 'ok', 0)",
        rusqlite::params![path, mtime],
    )
    .unwrap();
}

#[test]
fn check_ref_status_escapes_underscore_in_like_pattern() {
    let conn = open_index();
    // `fooXbar.rs` satisfies an unescaped `foo_bar` LIKE pattern; with the
    // escape it must not match, so the deleted `foo_bar.rs` ref reports broken.
    insert_file(&conn, "src/fooXbar.rs", 1_000);
    assert_eq!(
        check_ref_status(&conn, "foo_bar.rs", 2_000),
        RefStatus::Broken
    );
}

#[test]
fn check_ref_status_prefers_exact_over_suffix() {
    let conn = open_index();
    // The suffix candidate is fresher than the anchor (10 s > 5 s -> would be
    // Modified); the exact candidate predates it. Exact-match-first ordering
    // must make the stale exact row win deterministically.
    insert_file(&conn, "src/foo_bar.rs", 10_000_000_000);
    insert_file(&conn, "foo_bar.rs", 1_000);
    assert_eq!(check_ref_status(&conn, "foo_bar.rs", 5), RefStatus::Valid);
}

#[test]
fn check_ref_status_symbol_arm_anchors_on_the_newest_file() {
    let conn = open_index();
    // Two same-named symbols in different files with different mtimes. The
    // `qualname OR name` probe is a MULTI-INDEX OR, so without an ORDER BY
    // the picked row follows rowid order, which differs between fresh and
    // incremental builds; newest-mtime-first must make the verdict rest on
    // the newest match in both modes.
    insert_file(&conn, "a.py", 1_000);
    insert_file(&conn, "b.py", 10_000_000_000);
    conn.execute(
        "INSERT INTO symbols(id, kind, name, qualname, file_id, file_path, line_start, line_end, signature)
         VALUES (1, 'function', 'normalize', 'a.normalize', 1, 'a.py', 1, 2, ''),
                (2, 'function', 'normalize', 'b.normalize', 2, 'b.py', 1, 2, '')",
        [],
    )
    .unwrap();
    // Anchor between the two mtimes: only the newest file's mtime (10 s)
    // exceeds it. Picking the stale `a.py` row (0 s) would report Valid —
    // and which row wins must not depend on insertion order.
    assert_eq!(check_ref_status(&conn, "normalize", 5), RefStatus::Modified);
    // An anchor before both mtimes stays Modified; one after both stays
    // Valid, pinning that the verdict reads the picked row's mtime.
    assert_eq!(
        check_ref_status(&conn, "normalize", -1),
        RefStatus::Modified
    );
    assert_eq!(check_ref_status(&conn, "normalize", 11), RefStatus::Valid);
}
