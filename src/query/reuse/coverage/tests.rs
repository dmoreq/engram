use super::*;

fn index_fixture() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(include_str!("../../../../schema/index.sql"))
        .unwrap();
    for path in ["a.py", "b.py", "c.py"] {
        conn.execute(
            "INSERT INTO files(id, path, size, mtime, content_hash, parse_status, error_nodes)
             VALUES (NULL, ?1, 10, 1, 1, 'ok', 0)",
            [path],
        )
        .unwrap();
    }
    for (id, name, qualname, file_id, file_path, line_end) in [
        (1, "fetch", "a.fetch", 1, "a.py", 2),
        (2, "send_data", "b.send_data", 2, "b.py", 4),
        (3, "unwrap", "c.unwrap", 3, "c.py", 2),
    ] {
        conn.execute(
            "INSERT INTO symbols(id, kind, name, qualname, file_id, file_path, line_start, line_end, signature)
             VALUES (?1, 'function', ?2, ?3, ?4, ?5, 1, ?6, '')",
            rusqlite::params![id, name, qualname, file_id, file_path, line_end],
        )
        .unwrap();
    }
    for (dst_name, site_line) in [("fetch", 2), ("unwrap", 3)] {
        conn.execute(
            "INSERT INTO unresolved_calls(file_id, dst_name, kind, site_line)
             VALUES (2, ?1, 'call', ?2)",
            rusqlite::params![dst_name, site_line],
        )
        .unwrap();
    }
    conn
}

#[test]
fn coverage_raw_client_still_matches_user_defined_symbol() {
    // review.md I-11.1 attributed D1L raw-client matches to the retain's
    // `|| is_raw_client` disjunct; that disjunct is unreachable dead code
    // (build_stoplist never stoplists a raw client, and the static
    // STOPLIST is disjoint from RAW_CLIENTS), and the same-name match
    // below is a true positive by design — a user-defined `fetch` wrapper
    // is exactly what D1L should surface, the same reason D4 keeps raw
    // clients on `all_callees`. Pin both directions of the retain: a
    // raw-client-named callee with a same-family defining symbol still
    // reaches unresolved_candidates, while a stoplisted callee does not.
    let conn = index_fixture();
    let pool = analyze(&conn, &[CoverageKind::Unresolved], None).unwrap();
    let matched: Vec<(String, String)> = pool
        .iter()
        .filter(|c| c.detector == DetectorId::D1L && c.symbol == "b.send_data")
        .map(|c| (c.canonical.clone(), c.shared_items.join(",")))
        .collect();
    assert!(
        matched.contains(&("a.fetch".into(), "fetch".into())),
        "user-defined fetch must stay a D1L match: {matched:?}"
    );
    assert!(
        !matched.iter().any(|(canonical, _)| canonical == "c.unwrap"),
        "stoplisted callees must be dropped from unresolved_callees: {matched:?}"
    );
}

#[test]
fn coverage_class_excludes_boilerplate_and_honors_scope() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(include_str!("../../../../schema/index.sql"))
        .unwrap();
    for (id, path) in [(1, "a.py"), (2, "b.py"), (3, "c.py")] {
        conn.execute(
            "INSERT INTO files(id, path, size, mtime, content_hash, parse_status, error_nodes)
             VALUES (?1, ?2, 10, 1, 1, 'ok', 0)",
            rusqlite::params![id, path],
        )
        .unwrap();
    }
    for (id, kind, name, qualname, file_id, file_path) in [
        (1, "class", "Widget", "a.Widget", 1, "a.py"),
        (2, "method", "__init__", "a.Widget.__init__", 1, "a.py"),
        (3, "class", "Widget", "b.Widget", 2, "b.py"),
        (4, "method", "__init__", "b.Widget.__init__", 2, "b.py"),
        (5, "class", "Widget", "c.Widget", 3, "c.py"),
        (6, "method", "render", "a.Widget.render", 1, "a.py"),
        (7, "method", "render", "c.Widget.render", 3, "c.py"),
    ] {
        conn.execute(
            "INSERT INTO symbols(id, kind, name, qualname, file_id, file_path, line_start, line_end, signature)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, 2, '')",
            rusqlite::params![id, kind, name, qualname, file_id, file_path],
        )
        .unwrap();
    }
    // Only __init__ shared between a.Widget and b.Widget -> must be excluded.
    let all = analyze(&conn, &[CoverageKind::Class], None).unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].symbol, "c.Widget");
    assert_eq!(all[0].canonical, "a.Widget");

    // Scope to b.py -> no matches because b.Widget only shared __init__.
    let scoped = analyze(&conn, &[CoverageKind::Class], Some(&["b.py".into()])).unwrap();
    assert!(scoped.is_empty(), "{scoped:?}");
}
