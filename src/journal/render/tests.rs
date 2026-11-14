//! Unit tests for the global usage fold's live-repository and ephemeral
//! rules, Ok-only savings pricing, and the worktree fold.

use super::*;
use crate::db::test_support::{make_dir_all, remove_tree, temp_root, write_file};

/// A writable repository root OUTSIDE the OS temp tree: under the gitignored
/// `target/` directory. Temp-tree fixture roots are exactly the ephemeral
/// prefixes, so a global-fold fixture row that must survive the scratch rule
/// needs a durable location.
fn stable_root(name: &str) -> std::path::PathBuf {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join(format!("eg-{name}-{}", std::process::id()));
    remove_tree(&dir);
    dir
}

#[test]
fn global_rows_require_a_live_repo_root() {
    // The checkout itself is the durable live row (prune.rs fixture pattern).
    let live = env!("CARGO_MANIFEST_DIR").to_string();
    let lines = [
        format!(r#"{{"tool":"callers","exit_code":0,"repo":"{live}","repo_name":"live-repo"}}"#),
        // Purge policy: a dead absolute root is dropped before any fold.
        r#"{"tool":"callers","exit_code":0,"repo":"/nonexistent-eg-global/deleted","repo_name":"agent-skills"}"#.to_string(),
        // Identityless legacy lines (relative path, missing repo) too.
        r#"{"tool":"callers","exit_code":0,"repo":".tmp/verify/repo1","repo_name":"repo1"}"#.to_string(),
        r#"{"tool":"callers","exit_code":0}"#.to_string(),
    ]
    .join("\n");

    let (aggs, repo_aggs, errors) = aggregate_global_usage(&lines);
    assert_eq!(
        aggs["callers"].calls, 1,
        "dead and identityless lines dropped"
    );
    assert_eq!(repo_aggs[&live].display, "live-repo");
    assert_eq!(repo_aggs.len(), 1);
    assert!(errors.is_empty());

    let out = render_global_summary(&aggs, &repo_aggs, &errors, false).join("\n");
    assert!(out.contains("across 1 repositories"), "{out}");
    assert!(!out.contains("unattributed"), "{out}");
    assert!(out.contains("live-repo"), "{out}");
    assert!(!out.contains("agent-skills"), "{out}");
    assert!(!out.contains("repo1"), "{out}");
}

#[test]
fn live_repo_errors_render_and_dead_ones_vanish() {
    let live = env!("CARGO_MANIFEST_DIR").to_string();
    let lines = [
        format!(r#"{{"tool":"callers","exit_code":3,"repo":"{live}","repo_name":"live-repo"}}"#),
        r#"{"tool":"callers","exit_code":3,"repo":"/nonexistent-eg-global/deleted","repo_name":"agent-skills"}"#.to_string(),
        r#"{"tool":"callers","exit_code":3,"repo":".tmp/verify/repo1","repo_name":"repo1"}"#.to_string(),
    ]
    .join("\n");

    let (aggs, repo_aggs, errors) = aggregate_global_usage(&lines);
    assert_eq!(errors.len(), 1, "only the live repo's error survives");
    assert_eq!(errors[0].repo_name.as_deref(), Some("live-repo"));
    assert_eq!(repo_aggs.len(), 1);

    let out = render_global_summary(&aggs, &repo_aggs, &errors, false).join("\n");
    assert!(out.contains("live-repo"), "{out}");
    assert!(!out.contains("agent-skills"), "{out}");
    assert!(!out.contains("repo1"), "{out}");
}

#[test]
fn usage_cli_does_not_price_empty_or_ambiguous_exit_codes() {
    // Exit 1 (empty) and exit 2 (ambiguous) carry no savings: the priced arms
    // bill only successful calls, matching the dashboard summary. The rows
    // name a live root so the fold is not vacuous.
    let live = temp_root("usage-pricing-live");
    let repo = live.to_string_lossy().into_owned();
    let lines = [
        format!(r#"{{"ts":1700000000,"tool":"callers","exit_code":1,"result_count":0,"shown":0,"total":0,"stale":false,"ambiguous":0,"latency_us":100,"repo":"{repo}"}}"#),
        format!(r#"{{"ts":1700000001,"tool":"context","exit_code":2,"result_count":0,"shown":0,"total":0,"stale":false,"ambiguous":2,"latency_us":100,"repo":"{repo}"}}"#),
    ]
    .join("\n");
    let aggs = aggregate_usage(&lines);
    assert_eq!(aggs["callers"].calls, 1, "the fold is not vacuous");
    assert_eq!(aggs["context"].calls, 1);
    let (saved_tokens, _) = calculate_telemetry_totals(&aggs);
    assert_eq!(saved_tokens, 0, "empty and ambiguous calls are unpriced");
    remove_tree(&live);
}

#[test]
fn global_usage_folds_worktrees_and_drops_ephemeral() {
    // Durable main repository plus a linked worktree checked out under the
    // temp tree (liveness fixture pattern): the worktree contributes under
    // the main repo and never as its own row, and a plain scratch repo
    // disappears from the global totals entirely.
    let main_root = stable_root("usage-wt-main");
    make_dir_all(main_root.join(".git/worktrees/wt")).unwrap();
    make_dir_all(main_root.join(".engram")).unwrap();
    let wt_root = temp_root("usage-wt-checkout");
    let pointer = format!("gitdir: {}/.git/worktrees/wt\n", main_root.display());
    write_file(wt_root.join(".git"), pointer.as_bytes()).unwrap();
    let scratch = temp_root("usage-scratch-repo");

    let main = std::fs::canonicalize(&main_root)
        .unwrap_or_else(|_| main_root.clone())
        .to_string_lossy()
        .into_owned();
    let main_name = main_root
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let wt = wt_root.to_string_lossy().into_owned();
    let scratch_path = scratch.to_string_lossy().into_owned();
    let lines = [
        // Direct call in the main repository.
        format!(r#"{{"tool":"build","exit_code":0,"repo":"{main}","repo_name":"{main_name}","worktree":false}}"#),
        // Worktree call: folds onto the main repository's row.
        format!(r#"{{"tool":"callers","exit_code":0,"result_count":2,"shown":2,"total":2,"repo":"{wt}","repo_name":"wt-checkout","worktree":true}}"#),
        // Scratch identity: hidden from the global totals.
        format!(r#"{{"tool":"build","exit_code":0,"repo":"{scratch_path}","repo_name":"probe"}}"#),
    ]
    .join("\n");

    let (aggs, repo_aggs, _) = aggregate_global_usage(&lines);
    assert_eq!(aggs["build"].calls, 1, "scratch entry dropped from totals");
    assert_eq!(aggs["callers"].calls, 1, "worktree call priced once");
    assert_eq!(repo_aggs.len(), 1, "one row: the main repository");
    let row = repo_aggs.get(&main).expect("folded under the main root");
    assert_eq!(row.calls, 2, "main call + folded worktree call");
    assert_eq!(row.display, main_name, "row label is the main root");
    assert!(!row.worktree, "folded entries are not a worktree row");
    assert!(
        !repo_aggs.contains_key(&wt),
        "the worktree never forms its own row"
    );
    assert!(
        !repo_aggs.contains_key(&scratch_path),
        "scratch identity hidden"
    );

    remove_tree(&main_root);
    remove_tree(&wt_root);
    remove_tree(&scratch);
}

#[test]
fn quality_row_folds_structured_judge_counts() {
    // Two judged runs (2 pairs reviewed, 1 rejected, 1 failed) plus a legacy
    // reuse line without the judge object: the aggregate counts runs, not
    // lines, and the non-reuse tool stays judge-free.
    let live = env!("CARGO_MANIFEST_DIR").to_string();
    let lines = [
        format!(
            r#"{{"tool":"reuse","exit_code":0,"result_count":3,"shown":3,"total":3,"mode":"scan","judged":true,"judge":{{"reviewed":1,"rejected":0,"failed":1}},"repo":"{live}"}}"#
        ),
        format!(
            r#"{{"tool":"reuse","exit_code":0,"result_count":2,"shown":2,"total":2,"mode":"scan","judged":true,"judge":{{"reviewed":1,"rejected":1,"failed":0}},"repo":"{live}"}}"#
        ),
        format!(r#"{{"tool":"reuse","exit_code":0,"result_count":1,"shown":1,"total":1,"repo":"{live}"}}"#),
        format!(r#"{{"tool":"callers","exit_code":0,"result_count":1,"repo":"{live}"}}"#),
    ]
    .join("\n");

    let aggs = aggregate_usage(&lines);
    let row = tool_row("reuse", &aggs["reuse"], true);
    assert!(
        row.contains("judge_reviewed=2 judge_rejected=1 judge_failed=1"),
        "{row}"
    );
    let callers = tool_row("callers", &aggs["callers"], true);
    assert!(!callers.contains("judge_"), "{callers}");
}
