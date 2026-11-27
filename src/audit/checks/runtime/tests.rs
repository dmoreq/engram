//! Regression tests for the `performance.p95` baseline-regression rule (I-5).

use super::performance;
use crate::audit::checks::Status;
use crate::audit::collect::{collect, Mode};
use crate::db::test_support::{make_dir_all, remove_tree, temp_root, write_file};

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

/// One successful `callers` entry at an exact timestamp.
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

/// A sufficient baseline (24 fast `callers` entries over 4 distinct days,
/// plus pre-window history so the journal is not clipped) followed by 24
/// recent-window `callers` entries at `recent_latency_us`.
fn journal(root: &std::path::Path, now: i64, recent_latency_us: i64) -> Vec<String> {
    let b_start = now - 60 * 86_400;
    let mut lines = vec![line(root, b_start - 86_400, 1_000)];
    for i in 0..24 {
        lines.push(line(root, b_start + (i / 6) * 86_400 + 3_600, 1_000));
    }
    for _ in 0..24 {
        lines.push(line(root, now - 3_600, recent_latency_us));
    }
    lines
}

fn audit(
    name: &str,
    now: i64,
    recent_latency_us: i64,
) -> (crate::audit::collect::AuditData, std::path::PathBuf) {
    let root = temp_root(name);
    let lines = journal(&root, now, recent_latency_us);
    let root = seed(name, &lines);
    let data = collect(&Mode::Local(root.clone()), now, 30, 30).unwrap();
    (data, root)
}

#[test]
fn performance_critical_not_downgraded_by_baseline_regression() {
    let now = 1_800_000_000;
    // p95 = 20_000 ms, far above the 150 ms critical latency for callers.
    let (data, root) = audit("audit-p95-critical", now, 20_000_000);
    assert!(
        data.baseline.sufficient,
        "seeded baseline insufficient: {} successes over {} days (clipped: {})",
        data.baseline.successful_calls, data.baseline.active_days, data.baseline.clipped
    );
    let c = performance(&data);
    assert_eq!(c.status, Status::Critical, "{:?}", c.message);
    remove_tree(&root);
}

#[test]
fn performance_baseline_regression_escalates_pass_to_warn() {
    let now = 1_800_000_000;
    // p95 = 20 ms: statically a Pass (below the 50 ms warn latency), but
    // 20x the 1 ms baseline p95, so the 2x-baseline rule escalates to Warn.
    let (data, root) = audit("audit-p95-regression", now, 20_000);
    let c = performance(&data);
    assert_eq!(c.status, Status::Warn, "{:?}", c.message);
    remove_tree(&root);
}

#[test]
fn ops_errors_warns_on_high_mem_rejection_rate() {
    let now = 1_800_000_000;
    let root = temp_root("audit-ops-mut-reject");
    let mut lines = Vec::new();
    for i in 0..5 {
        let code = if i == 0 { 2 } else { 0 };
        lines.push(
            serde_json::json!({
                "ts": now - 60 - i,
                "tool": "mem_write",
                "exit_code": code,
                "result_count": 1,
                "shown": 1,
                "latency_us": 1_000,
                "repo": root.to_string_lossy(),
            })
            .to_string(),
        );
    }
    let root = seed("audit-ops-mut-reject", &lines);
    let data = collect(&Mode::Local(root.clone()), now, 30, 30).unwrap();
    let c = super::ops_errors(&data);
    assert_eq!(c.status, Status::Warn, "{:?}", c.message);
    assert!(c.message.contains("1 mem rejections"), "{}", c.message);
    remove_tree(&root);
}
