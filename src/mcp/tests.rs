//! Root-resolution regression for the MCP surface (I-9): the active-root
//! fallback must agree with the shared `RepoRef` resolver instead of
//! re-deriving the git root with inverted priority.

use super::root_from_path;
use crate::db::test_support::{make_dir_all, remove_tree, temp_root};

#[test]
fn mcp_root_from_path_respects_nested_engram_store_inside_git_monorepo() {
    let outer = temp_root("mcp-monorepo");
    make_dir_all(outer.join(".git")).unwrap();
    let sub = outer.join("packages").join("sub");
    make_dir_all(sub.join(".engram")).unwrap();

    // The nested `.engram/` store wins over the enclosing git root, matching
    // the CLI's resolution inside monorepo sub-packages.
    let resolved = root_from_path(&sub).expect("nested store resolves");
    let expected = std::fs::canonicalize(&sub).unwrap_or_else(|_| sub.clone());
    assert_eq!(resolved, expected);

    // Without a nested store the enclosing git repository answers.
    let plain = outer.join("packages").join("plain");
    make_dir_all(&plain).unwrap();
    let resolved = root_from_path(&plain).expect("git repository resolves");
    let expected = std::fs::canonicalize(&outer).unwrap_or_else(|_| outer.clone());
    assert_eq!(resolved, expected);

    remove_tree(&outer);
}
