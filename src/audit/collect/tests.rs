//! Regression tests for the audit baseline sufficiency gate (C-1) and the
//! audit tool taxonomy (I-8).

use super::{collect, Metrics, Mode};
use crate::db::test_support::{make_dir_all, remove_tree, temp_root, write_file};

/// Seeds `.engram/usage.log` with JSONL lines, mirroring the live journal
/// writer's output. All fs writes go through `db::test_support`.
fn seed(name: &str, lines: &[String]) -> std::path::PathBuf {
    let root = temp_root(name);
    make_dir_all(root.join(".engram")).unwrap();
    write_file(
        root.join(".engram/usage.log"),
        format!("{}\n", lines.join("\n")).as_bytes(),
    )
    .unwrap();
    root
}

/// One successful journal entry at an exact timestamp.
fn line(root: &std::path::Path, ts: i64, latency_us: i64) -> String {
    serde_json::json!({
        "ts": ts,
        "tool": "callers",
        "exit_code": 0,
        "result_count": 1,
        "shown": 1,
        "latency_us": latency_us,
        "repo": root.to_string_lossy(),
    })
    .to_string()
}

/// 24 successful `callers` entries spread over 4 distinct days inside the
/// baseline window (`[b_start, w_start)`), satisfying the 20-success/3-day
/// floor.
fn baseline_window_lines(root: &std::path::Path, b_start: i64) -> Vec<String> {
    (0..24)
        .map(|i| {
            let ts = b_start + (i / 6) * 86_400 + 3_600;
            line(root, ts, 1_000)
        })
        .collect()
}

#[test]
fn baseline_sufficient_when_journal_precedes_baseline_start() {
    let now = 1_800_000_000;
    let b_start = now - 60 * 86_400;
    let root = temp_root("audit-baseline-ok");
    // Pre-window history proves the journal covers the window start. No
    // boundary entry on purpose: the baseline bucket only admits entries at
    // or after `b_start`, so seeding one there would let the pre-fix formula
    // (clipping from the bucket's own oldest) pass this test vacuously.
    let mut lines = vec![line(&root, b_start - 86_400, 1_000)];
    lines.extend(baseline_window_lines(&root, b_start));
    let root = seed("audit-baseline-ok", &lines);
    let data = collect(&Mode::Local(root.clone()), now, 30, 30).unwrap();
    assert!(
        !data.baseline.clipped,
        "clipped with journal history at {}",
        data.baseline.start_ts
    );
    assert!(
        data.baseline.sufficient,
        "insufficient: {} successes over {} days",
        data.baseline.successful_calls, data.baseline.active_days
    );
    remove_tree(&root);
}

#[test]
fn baseline_clipped_when_journal_starts_inside_baseline_window() {
    let now = 1_800_000_000;
    let b_start = now - 60 * 86_400;
    let root = temp_root("audit-baseline-clipped");
    // Same 24 entries, but nothing at or before `b_start`: the journal
    // cannot prove coverage of the window's start.
    let lines = baseline_window_lines(&root, b_start);
    let root = seed("audit-baseline-clipped", &lines);
    let data = collect(&Mode::Local(root.clone()), now, 30, 30).unwrap();
    assert!(
        data.baseline.clipped,
        "not clipped: oldest journal entry precedes the baseline start"
    );
    assert!(
        !data.baseline.sufficient,
        "sufficient despite clipping: {} successes over {} days",
        data.baseline.successful_calls, data.baseline.active_days
    );
    remove_tree(&root);
}

/// Strict-`>` boundary pin: an entry exactly at `b_start` is inside the
/// baseline bucket (`ts >= b_start` admission) and must not count as
/// clipping. Seeding nothing before `b_start` makes this journal fail under
/// both mutations of the clipping source: a `>=` comparison flags the
/// boundary entry itself, and reading the baseline bucket's own oldest
/// (`b_start + 3600` here instead of the coverage-wide `b_start`) reports
/// coverage loss that pre-window history alone proves absent.
#[test]
fn baseline_boundary_entry_at_b_start_is_not_clipping() {
    let now = 1_800_000_000;
    let b_start = now - 60 * 86_400;
    let root = temp_root("audit-baseline-boundary");
    let mut lines = vec![
        // The only coverage-wide signal: the journal starts exactly at
        // `b_start`, so nothing was rotated away from the window's start.
        line(&root, b_start, 1_000),
    ];
    lines.extend(baseline_window_lines(&root, b_start));
    let root = seed("audit-baseline-boundary", &lines);
    let data = collect(&Mode::Local(root.clone()), now, 30, 30).unwrap();
    assert!(
        !data.baseline.clipped,
        "clipped with the oldest entry exactly at the baseline start: {}",
        data.baseline.start_ts
    );
    assert!(
        data.baseline.sufficient,
        "insufficient: {} successes over {} days",
        data.baseline.successful_calls, data.baseline.active_days
    );
    remove_tree(&root);
}

/// I-8: the audit tool taxonomy tracks the journal writer's tool set, so a
/// newly classified tool can never fall back to `other` again.
#[test]
fn canonical_tool_recognizes_audit_and_mem_reindex() {
    use super::canonical_tool;
    assert_eq!(canonical_tool("audit"), "audit");
    assert_eq!(canonical_tool("mem_reindex"), "mem_reindex");
    // Every priced tool and every deliberately unpriced tool maps to itself.
    for tool in [
        "build",
        "callers",
        "callees",
        "context",
        "path",
        "impact",
        "imports",
        "search",
        "reuse",
        "symbols",
        "doctor",
        "usage",
        "unresolved",
        "stats",
        "mem_write",
        "mem_touch",
        "mem_query",
        "mem_get",
        "mem_delete",
        "mem_gc",
        "mem_export",
        "mem_import",
    ] {
        assert_eq!(canonical_tool(tool), tool, "{tool} must stay itself");
    }
    assert_eq!(canonical_tool("not-a-tool"), "other");
}

/// One successful entry for the classification assertions.
fn entry(tool: &str) -> super::Entry {
    super::Entry {
        tool: tool.to_string(),
        exit_code: 0,
        result_count: 1,
        shown: 1,
        latency_us: Some(100),
        ts: 1_800_000_000,
        repo: std::path::PathBuf::from("/tmp/eg-audit-taxonomy"),
    }
}

#[test]
fn metrics_routes_mem_export_and_mem_reindex() {
    let mut metrics = Metrics::default();
    metrics.add(&entry("mem_export"));
    assert_eq!(metrics.mem_reads, 1, "an export reads the store");
    assert_eq!(metrics.mem_writes, 0);

    let mut metrics = Metrics::default();
    metrics.add(&entry("mem_reindex"));
    assert_eq!(metrics.mem_writes, 1, "a reindex mutates derived state");
    assert_eq!(metrics.mem_reads, 0);
}
