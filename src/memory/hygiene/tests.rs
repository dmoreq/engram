use super::*;
use crate::db::test_support::{make_dir_all, remove_tree, temp_root, write_file};

#[test]
fn normalizes_repo_internal_abs_path_with_line_suffix() {
    let root = temp_root("hygiene-norm");
    make_dir_all(root.join("src")).unwrap();
    write_file(root.join("src/lib.rs"), b"pub fn f() {}\n").unwrap();
    let abs = format!("{}/src/lib.rs:42", root.display());
    let out = validate_and_normalize_refs(
        Some(&abs),
        Some(&root),
        None,
        MemScope::Durable,
        false,
        false,
    )
    .unwrap();
    assert!(out.changed);
    assert_eq!(out.normalized_count, 1);
    assert_eq!(out.refs.as_deref(), Some("src/lib.rs:42"));
    remove_tree(&root);
}

#[test]
fn allows_indexed_symbol_and_double_colon_qualname_blocks_extensionless_ignored_file() {
    let root = temp_root("hygiene-sym");
    write_file(root.join(".gitignore"), b"Dockerfile\n").unwrap();
    write_file(root.join("Dockerfile"), b"FROM scratch\n").unwrap();

    let sym = validate_and_normalize_refs(
        Some("crate::memory::hygiene"),
        Some(&root),
        None,
        MemScope::Durable,
        false,
        false,
    )
    .unwrap();
    assert_eq!(sym.refs.as_deref(), Some("crate::memory::hygiene"));

    let err = validate_and_normalize_refs(
        Some("Dockerfile"),
        Some(&root),
        None,
        MemScope::Durable,
        false,
        false,
    )
    .unwrap_err();
    assert_eq!(err.exit_code(), 2);
    remove_tree(&root);
}

#[cfg(unix)]
#[test]
fn blocks_relative_and_absolute_symlink_escape_and_handles_symlinked_root() {
    let root = temp_root("hygiene-sym-esc");
    let outside = temp_root("hygiene-outside");
    make_dir_all(root.join("src")).unwrap();
    write_file(outside.join("secret.txt"), b"x").unwrap();
    crate::db::test_support::make_symlink(&outside, root.join("src/escape")).unwrap();

    let err = validate_and_normalize_refs(
        Some("src/escape/secret.txt"),
        Some(&root),
        None,
        MemScope::Durable,
        false,
        false,
    )
    .unwrap_err();
    assert_eq!(err.exit_code(), 2);

    let abs_esc = format!("{}/src/escape/secret.txt", root.display());
    let err2 = validate_and_normalize_refs(
        Some(&abs_esc),
        Some(&root),
        None,
        MemScope::Durable,
        false,
        false,
    )
    .unwrap_err();
    assert_eq!(err2.exit_code(), 2);

    remove_tree(&root);
    remove_tree(&outside);
}

#[test]
fn loads_nested_gitignore_and_checks_nonexistent_parent_dirs() {
    let root = temp_root("hygiene-gi");
    make_dir_all(root.join("sub")).unwrap();
    write_file(root.join("sub/.gitignore"), b"build_out/\n").unwrap();

    let err = validate_and_normalize_refs(
        Some("sub/build_out/artifact.txt"),
        Some(&root),
        None,
        MemScope::Durable,
        false,
        false,
    )
    .unwrap_err();
    assert_eq!(err.exit_code(), 2);
    remove_tree(&root);
}

#[test]
fn blocks_tmp_engram_parent_dir_and_git_option_tokens() {
    let root = temp_root("hygiene-tok");
    for bad in [
        ".tmp/a.py",
        "./.tmp/a.py",
        ".engram/index.db",
        "./.engram/index.db",
        "../x",
        "--exec=sh",
        ":(glob)*",
    ] {
        let err = validate_and_normalize_refs(
            Some(bad),
            Some(&root),
            None,
            MemScope::Durable,
            false,
            false,
        )
        .unwrap_err();
        assert_eq!(err.exit_code(), 2, "expected exit 2 for {bad}");
    }
    assert!(validate_and_normalize_refs(
        Some(".tmp/a.py"),
        Some(&root),
        None,
        MemScope::Session,
        false,
        false,
    )
    .is_ok());
    assert!(validate_and_normalize_refs(
        Some(".tmp/a.py"),
        Some(&root),
        None,
        MemScope::Durable,
        false,
        true,
    )
    .is_ok());
    remove_tree(&root);
}

#[test]
fn global_durable_blocks_abs_path_without_confirm_and_skips_repo_discovery() {
    let err = validate_and_normalize_refs(
        Some("/abs/path/file.rs"),
        None,
        None,
        MemScope::Durable,
        true,
        false,
    )
    .unwrap_err();
    assert_eq!(err.exit_code(), 2);

    assert!(validate_and_normalize_refs(
        Some("/abs/path/file.rs"),
        None,
        None,
        MemScope::Durable,
        true,
        true,
    )
    .is_ok());
}
