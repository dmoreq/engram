//! Regression tests for the reuse dashboard payload, moved out of reuse.rs
//! (498 LOC) to keep that file under the 500-LOC hygiene gate. The two
//! original inline modules (`tests`, `containment_tests`) are flattened into
//! this one sibling module; test bodies are unchanged.

use super::*;

#[test]
fn reuse_scan_cache_hits_until_the_index_changes() {
    let dir = crate::db::test_support::temp_root("reuse-scan-cache");
    let repo = dir.join("repo");
    crate::db::test_support::make_dir_all(repo.join(".engram")).unwrap();
    crate::db::test_support::write_file(repo.join(".engram").join("index.db"), b"").unwrap();
    let mut repos: HashMap<String, PathBuf> = HashMap::new();
    repos.insert("repo".to_string(), repo.clone());
    let labels = HashMap::new();

    let (_, first_hit, _) = cached_scan(&repos, &labels);
    let (_, second_hit, _) = cached_scan(&repos, &labels);
    assert!(!first_hit, "first scan must compute");
    assert!(second_hit, "second scan with an unchanged index must hit");

    // A changed index mtime invalidates the cache.
    std::thread::sleep(std::time::Duration::from_millis(5));
    crate::db::test_support::write_file(repo.join(".engram").join("index.db"), b"x").unwrap();
    let (_, third_hit, _) = cached_scan(&repos, &labels);
    assert!(!third_hit, "changed index must recompute");
    // An unreadable index measures nothing: the repo stays absent from the
    // pool map instead of reporting a fabricated zero.
    let ((_, _, pools), _, _) = cached_scan(&repos, &labels);
    assert!(pools.is_empty(), "{pools:?}");
    crate::db::test_support::remove_tree(&dir);
}

#[test]
fn summary_emits_sample_markers_and_estimate_flags() {
    let summary = summarize_reuse_metrics(&[], 0, 0);
    assert_eq!(summary["schema_version"], 4);
    assert_eq!(summary["total_candidates"], 0);
    assert_eq!(summary["prewrite_count"], 0);
    assert_eq!(summary["preventable_loc_estimated"], true);
    assert_eq!(summary["detector_counts_estimated"], true);
    // Judge totals are a structured block keyed `judge`; the retired scalar
    // `judged_calls` stays absent.
    assert_eq!(summary["judge"]["runs"], 0);
    assert_eq!(summary["judge"]["reviewed"], 0);
    assert_eq!(summary["judge"]["rejected"], 0);
    assert_eq!(summary["judge"]["failed"], 0);
    assert!(summary.get("judged_calls").is_none());
    assert_eq!(summary["avg_callers"], 0.0);
    assert_eq!(summary["avg_callers_estimated"], true);
    // Sample-only aggregate blocks no consumer read: retired with schema 4.
    assert!(summary.get("sample_strategy").is_none());
    assert!(summary.get("confidence_counts").is_none());
    assert!(summary.get("confidence_counts_estimated").is_none());
    assert!(summary.get("reason_counts").is_none());
}

#[test]
fn snippet_paths_stay_inside_the_repository_root() {
    let root = crate::db::test_support::temp_root("reuse-contain");
    crate::db::test_support::write_file(root.join("ok.rs"), b"fn a() {}\n").unwrap();
    crate::db::test_support::make_dir_all(root.join("src")).unwrap();
    crate::db::test_support::write_file(root.join("src").join("in.rs"), b"fn b() {}\n").unwrap();

    assert!(contained_path(&root, "ok.rs").is_some());
    assert!(contained_path(&root, "src/in.rs").is_some());
    // Escapes and absolute paths never resolve.
    assert!(contained_path(&root, "../outside.rs").is_none());
    assert!(contained_path(&root, "/etc/passwd").is_none());
    assert!(contained_path(&root, "src/../../outside.rs").is_none());
    crate::db::test_support::remove_tree(&root);
}
