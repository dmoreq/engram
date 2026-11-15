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

#[test]
fn detector_counts_cover_every_detector_type() {
    // Coverage detectors (D5 class-shape, D1L unresolved, DT test helpers)
    // appear in candidate lists; the pills must count them like D1-D4.
    let candidates = vec![
        json!({ "detector": "D1" }),
        json!({ "detector": "D5" }),
        json!({ "detector": "D1L" }),
        json!({ "detector": "DT" }),
        json!({ "detector": "DT" }),
    ];
    let summary = summarize_reuse_metrics(&candidates, 5, 0);
    assert_eq!(summary["detector_counts"]["D1"], 1);
    assert_eq!(summary["detector_counts"]["D5"], 1);
    assert_eq!(summary["detector_counts"]["D1L"], 1);
    assert_eq!(summary["detector_counts"]["DT"], 2);
}

#[test]
fn prewrite_feed_keeps_only_search_and_reuse_newest_first() {
    let entries = vec![
        json!({ "tool": "search", "ts": 100.0, "repo": "/r", "repo_name": "r", "shown": 3, "latency_us": 100, "args": { "terms": "alpha" } }),
        // Memory lookups are not pre-write discovery.
        json!({ "tool": "mem_query", "ts": 150.0, "repo": "/r", "repo_name": "r", "shown": 9, "latency_us": 50, "args": { "terms": "x" } }),
        // Reuse runs are scanned feeds too; newest wins the first row.
        json!({ "tool": "reuse", "ts": 200.0, "repo": "/r", "repo_name": "r", "result_count": 2, "latency_us": 200, "args": {} }),
    ];
    let feed = collect_prewrite_searches(&entries);
    assert_eq!(feed.len(), 2, "{feed:?}");
    assert_eq!(feed[0]["tool"], "reuse");
    assert_eq!(feed[0]["shown"], 2);
    assert_eq!(feed[1]["tool"], "search");
    assert_eq!(feed[1]["shown"], 3);
}
