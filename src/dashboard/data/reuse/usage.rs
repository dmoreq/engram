//! Per-candidate reuse usage from the retained journal.

use std::collections::HashMap;

use serde_json::{json, Value};

/// Per-candidate usage from the retained journal: how many `eg reuse` runs
/// surfaced this pair, and when the last one was. Legacy lines without
/// `args.candidates` contribute nothing (count 0 = never surfaced in window).
pub(super) fn aggregate_candidate_usage(entries: &[Value]) -> HashMap<String, (usize, f64)> {
    let mut usage: HashMap<String, (usize, f64)> = HashMap::new();
    for e in entries {
        if e.get("tool").and_then(Value::as_str) != Some("reuse") {
            continue;
        }
        let Some(repo_raw) = e.get("repo").and_then(Value::as_str) else {
            continue;
        };
        let worktree = e.get("worktree").and_then(Value::as_bool).unwrap_or(false);
        let (repo, _) = crate::repo::canonical_repo_bucket(repo_raw, worktree);
        let Some(list) = e
            .get("args")
            .and_then(|a| a.get("candidates"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        let ts = e.get("ts").and_then(Value::as_f64).unwrap_or(0.0);
        for pair in list.split('\n').filter(|p| !p.is_empty()) {
            let rec = usage.entry(format!("{repo}|{pair}")).or_insert((0, 0.0));
            rec.0 += 1;
            if ts > rec.1 {
                rec.1 = ts;
            }
        }
    }
    usage
}

pub(super) fn decorate_usage(candidates: &mut [Value], usage: &HashMap<String, (usize, f64)>) {
    for candidate in candidates.iter_mut() {
        let repo_path = candidate
            .get("repo_path")
            .and_then(Value::as_str)
            .unwrap_or("");
        let symbol = candidate
            .get("symbol")
            .and_then(Value::as_str)
            .unwrap_or("");
        let canonical = candidate
            .get("canonical")
            .and_then(Value::as_str)
            .unwrap_or("");
        let key = format!("{repo_path}|{symbol}->{canonical}");
        let (count, last_ts) = usage.get(&key).copied().unwrap_or((0, 0.0));
        candidate["reuse_count"] = json!(count);
        candidate["last_reused_ts"] = json!(last_ts);
        candidate["usage_window"] = json!("retained_journal");
    }
}

/// Judge totals over the retained journal. Only reuse lines carrying the
/// structured `judge` object count as runs; a missing key or a null (legacy
/// line, no-op run) contributes nothing. A run that reviewed nothing
/// (`{0,0,0}` — zero judgeable candidates) is not a run: counting it would
/// fabricate activity for a judge that was never exercised.
pub(super) struct JudgeTotals {
    pub(super) runs: usize,
    pub(super) reviewed: usize,
    pub(super) rejected: usize,
    pub(super) failed: usize,
    pub(super) rejected_loc: usize,
}

pub(super) fn aggregate_judge_usage(entries: &[Value]) -> JudgeTotals {
    let mut totals = JudgeTotals {
        runs: 0,
        reviewed: 0,
        rejected: 0,
        failed: 0,
        rejected_loc: 0,
    };
    for e in entries {
        if e.get("tool").and_then(Value::as_str) != Some("reuse") {
            continue;
        }
        let Some(judge) = e.get("judge").filter(|j| j.is_object()) else {
            continue;
        };
        let number = |key: &str| judge.get(key).and_then(Value::as_u64).unwrap_or(0) as usize;
        let (reviewed, rejected, failed) =
            (number("reviewed"), number("rejected"), number("failed"));
        if reviewed + rejected + failed == 0 {
            continue;
        }
        totals.runs += 1;
        totals.reviewed += reviewed;
        totals.rejected += rejected;
        totals.failed += failed;
        totals.rejected_loc += judge
            .get("rejected_loc")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
    }
    totals
}

/// Consolidation closure over the retained journal: pair every historically
/// surfaced twin against the current scan. A surfaced pair absent from the
/// current index was resolved (merged away); one still present is unresolved
/// recurrence. Without this, a successful fix removes the card and its own
/// evidence — the panel could only ever show failures.
///
/// Only repositories whose scan is COMPLETE count: `measured_pools` maps the
/// scanned root path to its pool, and a pool above the sample cap means the
/// 25-candidate sample cannot prove absence — such pairs are unmeasured, not
/// resolved. Repos absent from the map (gated, unreadable, deleted index)
/// are equally unmeasured.
#[derive(Debug)]
pub(super) struct ResolvedTotals {
    pub(super) resolved: usize,
    pub(super) surface_distinct: usize,
}

pub(super) fn resolved_vs_current(
    candidates: &[Value],
    usage: &HashMap<String, (usize, f64)>,
    measured_pools: &std::collections::BTreeMap<String, usize>,
) -> ResolvedTotals {
    let current: std::collections::HashSet<String> = candidates
        .iter()
        .filter_map(|c| {
            let repo = c.get("repo_path").and_then(Value::as_str)?;
            let symbol = c.get("symbol").and_then(Value::as_str)?;
            let canonical = c.get("canonical").and_then(Value::as_str)?;
            Some(format!("{repo}|{symbol}->{canonical}"))
        })
        .collect();
    let mut resolved = 0usize;
    let mut surface_distinct = 0usize;
    for key in usage.keys() {
        let Some(repo) = key.split('|').next() else {
            continue;
        };
        let Some(pool) = measured_pools.get(repo) else {
            continue;
        };
        // A pool above the sample cap means the 25-candidate sample cannot
        // prove absence — such pairs are unmeasured on BOTH sides of the
        // fraction: neither resolved nor part of the denominator.
        if *pool > super::SAMPLE_CAP {
            continue;
        }
        surface_distinct += 1;
        if !current.contains(key) {
            resolved += 1;
        }
    }
    ResolvedTotals {
        resolved,
        surface_distinct,
    }
}

#[cfg(test)]
mod usage_tests {
    use super::*;

    #[test]
    fn usage_aggregates_per_repo_and_candidate_pair() {
        let entries = vec![
            json!({
                "tool": "reuse", "repo": "/r/a", "ts": 100.0,
                "args": { "candidates": "f->g\nh->g" }
            }),
            json!({
                "tool": "reuse", "repo": "/r/a", "ts": 250.0,
                "args": { "candidates": "f->g" }
            }),
            // Same pair under another repo must not accrue.
            json!({
                "tool": "reuse", "repo": "/r/b", "ts": 300.0,
                "args": { "candidates": "f->g" }
            }),
            // Legacy line without candidate ids contributes nothing.
            json!({ "tool": "reuse", "repo": "/r/a", "ts": 400.0, "args": {} }),
            // Other tools never contribute.
            json!({
                "tool": "search", "repo": "/r/a", "ts": 500.0,
                "args": { "candidates": "f->g" }
            }),
        ];
        let usage = aggregate_candidate_usage(&entries);
        assert_eq!(usage.get("/r/a|f->g").copied(), Some((2, 250.0)));
        assert_eq!(usage.get("/r/a|h->g").copied(), Some((1, 100.0)));
        assert_eq!(usage.get("/r/b|f->g").copied(), Some((1, 300.0)));
    }

    #[test]
    fn decorate_usage_attaches_counts_and_window() {
        let mut candidates = vec![json!({
            "repo_path": "/r/a", "symbol": "f", "canonical": "g"
        })];
        let mut usage = HashMap::new();
        usage.insert("/r/a|f->g".to_string(), (3usize, 250.0f64));
        decorate_usage(&mut candidates, &usage);
        assert_eq!(candidates[0]["reuse_count"], 3);
        assert_eq!(candidates[0]["last_reused_ts"], 250.0);
        assert_eq!(candidates[0]["usage_window"], "retained_journal");
    }

    #[test]
    fn judge_totals_count_runs_not_lines() {
        let entries = vec![
            json!({ "tool": "reuse", "judged": true,
                    "judge": { "reviewed": 2, "rejected": 1, "failed": 0 } }),
            json!({ "tool": "reuse", "judged": true,
                    "judge": { "reviewed": 1, "rejected": 0, "failed": 1 } }),
            // No-op run: null judge object is not a run.
            json!({ "tool": "reuse", "judged": false, "judge": serde_json::Value::Null }),
            // Legacy line without the object is not a run either.
            json!({ "tool": "reuse" }),
            json!({ "tool": "search", "judge": { "reviewed": 9 } }),
            // Zero-judgeable run: {0,0,0} must not inflate the run count.
            json!({ "tool": "reuse", "judged": true,
                    "judge": { "reviewed": 0, "rejected": 0, "failed": 0 } }),
        ];
        let totals = aggregate_judge_usage(&entries);
        assert_eq!(totals.runs, 2);
        assert_eq!(totals.reviewed, 3);
        assert_eq!(totals.rejected, 1);
        assert_eq!(totals.failed, 1);
    }

    #[test]
    fn resolved_vs_current_closes_the_consolidation_loop() {
        // Two pairs surfaced historically in a sample-complete repo (pool 2
        // <= cap); the current scan still shows one. The absent pair counts
        // as resolved; the present one as recurrence.
        let candidates = vec![json!({
            "repo_path": "/r/a", "symbol": "f", "canonical": "g"
        })];
        let usage = HashMap::from([
            ("/r/a|f->g".to_string(), (2usize, 250.0f64)),
            ("/r/a|h->g".to_string(), (1, 100.0)),
        ]);
        let pools = std::collections::BTreeMap::from([("/r/a".to_string(), 2usize)]);
        let totals = resolved_vs_current(&candidates, &usage, &pools);
        assert_eq!(totals.surface_distinct, 2);
        assert_eq!(totals.resolved, 1);
    }

    #[test]
    fn resolved_skips_unmeasured_and_incomplete_samples() {
        // A pool above the sample cap cannot prove absence (the twin may be
        // ranked below the cut), and a repo missing from the measured set
        // (gated, unreadable, deleted index) measures nothing at all: both
        // must stay out of the resolved count instead of faking closure.
        let candidates = vec![json!({
            "repo_path": "/r/big", "symbol": "f", "canonical": "g"
        })];
        let usage = HashMap::from([
            ("/r/big|f->g".to_string(), (1usize, 100.0f64)),
            ("/r/gone|h->g".to_string(), (1, 100.0)),
        ]);
        let pools = std::collections::BTreeMap::from([
            ("/r/big".to_string(), 200usize),
            ("/r/other".to_string(), 1usize),
        ]);
        let totals = resolved_vs_current(&candidates, &usage, &pools);
        assert_eq!(totals.resolved, 0, "{totals:?}");
        // The over-cap repo is excluded on both sides of the fraction.
        assert_eq!(totals.surface_distinct, 0, "{totals:?}");
    }
}
