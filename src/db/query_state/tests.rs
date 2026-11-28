use super::*;

#[test]
fn resolve_ref_detail_handles_lines_symbols_dirs_and_escapes() {
    let root = crate::db::test_support::temp_root("resolve-ref-detail");
    crate::db::test_support::make_dir_all(root.join("src")).unwrap();
    crate::db::test_support::make_dir_all(root.join("docs/sub")).unwrap();
    crate::db::test_support::write_file(
        root.join("src/mod.py"),
        b"class Alpha:\n    def beta(self):\n        return 1\n",
    )
    .unwrap();
    crate::db::test_support::write_file(
        root.join("src/lib.rs"),
        b"pub mod db { pub fn helper() {} }\n",
    )
    .unwrap();
    crate::build::run_build(Some(&root), false, 0.05).unwrap();

    let conn = crate::db::open_for_query(&root).unwrap();
    let canon = std::fs::canonicalize(&root).unwrap();

    // :line and :start-end strip cleanly before file lookup
    let r1 = resolve_ref_detail_with_canon(
        Some(&conn),
        Some(&root),
        Some(&canon),
        "src/mod.py:2",
        i64::MAX,
    );
    assert_eq!(r1.status, RefStatus::Valid);
    assert_eq!(r1.kind, "file");

    let r2 = resolve_ref_detail_with_canon(
        Some(&conn),
        Some(&root),
        Some(&canon),
        "src/mod.py:1-3",
        i64::MAX,
    );
    assert_eq!(r2.status, RefStatus::Valid);
    assert_eq!(r2.kind, "file");

    // Class.method suffix lookup
    let r3 = resolve_ref_detail(Some(&conn), Some(&root), "Alpha.beta", i64::MAX);
    assert_eq!(r3.status, RefStatus::Valid);
    assert_eq!(r3.kind, "symbol");

    // Rust :: -> . normalization in symbol arm
    let r4 = resolve_ref_detail(Some(&conn), Some(&root), "db::helper", i64::MAX);
    assert_eq!(r4.status, RefStatus::Valid);
    assert_eq!(r4.kind, "symbol");

    // Contained directory is Valid
    let r5 =
        resolve_ref_detail_with_canon(Some(&conn), Some(&root), Some(&canon), "docs/sub", i64::MAX);
    assert_eq!(r5.status, RefStatus::Valid);
    assert_eq!(r5.kind, "path");

    // Out-of-root / absolute paths rejected as Broken
    let r6 = resolve_ref_detail(Some(&conn), Some(&root), "../outside", i64::MAX);
    assert_eq!(r6.status, RefStatus::Broken);
    let r7 = resolve_ref_detail(Some(&conn), Some(&root), "/etc/passwd", i64::MAX);
    assert_eq!(r7.status, RefStatus::Broken);

    drop(conn);
    crate::db::test_support::remove_tree(&root);
}
