//! Finalize a reuse report: confidence filter, sort/limit, 30 KiB cap,
//! rendering, and journal counters. Analysis owns the pool and freshness;
//! this module owns everything the user sees.

use super::*;

pub(super) fn finalize(
    _ctx: &QueryCtx<'_>,
    mut report: ReuseReport,
    opts: &ReuseOpts,
) -> Result<crate::query::LineQuery> {
    let freshness_headers = report.freshness.headers();
    crate::journal::set_freshness(crate::journal::ReuseFreshness {
        state: report.freshness.state.as_str(),
        changed_files: report.freshness.changed_files,
        checked_files: report.freshness.checked_files,
        unknown_count: report.freshness.unknown_count,
        unknown_reason: report.freshness.unknown_reason.map(|r| r.as_str()),
    });
    // Filter before sorting: only retained candidates need to be ordered.
    let extra_below = report
        .pool
        .iter()
        .filter(|c| c.confidence.rank() < opts.min_confidence.rank())
        .count();
    report
        .pool
        .retain(|c| c.confidence.rank() >= opts.min_confidence.rank());
    report.pool.sort_by(|a, b| {
        b.confidence
            .rank()
            .cmp(&a.confidence.rank())
            .then_with(|| b.sort_rank().cmp(&a.sort_rank()))
            .then_with(|| b.canonical_callers.cmp(&a.canonical_callers))
            .then_with(|| a.symbol.cmp(&b.symbol))
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.line.cmp(&b.line))
            .then_with(|| a.canonical.cmp(&b.canonical))
            .then_with(|| a.canonical_file.cmp(&b.canonical_file))
            .then_with(|| a.canonical_line.cmp(&b.canonical_line))
    });
    let below_threshold = report.counts.below_threshold + extra_below;
    let eligible = report.pool.len();
    let limit_omitted = eligible.saturating_sub(opts.limit);
    report.pool.truncate(opts.limit);

    if report.pool.is_empty() {
        report.counts = ReuseCounts {
            suppressed: report.counts.suppressed,
            guarded_out: report.counts.guarded_out,
            below_threshold,
            total: report.counts.total,
            ..ReuseCounts::default()
        };
        crate::journal::set_mode(report.mode);
        crate::journal::set_reuse_counts(crate::journal::ReuseJournalCounts {
            provisional: 0,
            low: 0,
            medium: 0,
            high: 0,
            suppressed: report.counts.suppressed,
            guarded_out: report.counts.guarded_out,
            below_threshold,
            omitted: 0,
            capped: 0,
            truncated: false,
        });
        crate::journal::record_result(
            crate::journal::OutcomeCounts::of(0, report.counts.total),
            report.freshness.state == Freshness::Stale,
            0,
        );
        // --limit 0 is the count-only convention: report the real total even
        // though nothing is displayed.
        let header = if opts.limit == 0 && report.counts.total > 0 {
            format!(
                "eg: reuse — {} duplication candidate(s) found (count-only: --limit 0)",
                report.counts.total
            )
        } else {
            "eg: reuse — 0 duplication candidate(s) found".to_string()
        };
        let mut lines = vec![header];
        lines.extend(freshness_headers);
        if below_threshold > 0 || report.counts.guarded_out > 0 || report.counts.suppressed > 0 {
            let mut summary = format!(
                "eg: reuse: 0 provisional, 0 low, 0 medium, 0 high, {} guarded out, {below_threshold} below threshold",
                report.counts.guarded_out
            );
            if report.counts.suppressed > 0 {
                summary.push_str(&format!("; {} suppressed", report.counts.suppressed));
            }
            lines.push(summary);
        }
        if !opts.paths.is_empty() {
            lines.push(format!("eg: reuse: path_matches={}", report.path_matches));
        }
        let mut query = crate::query::LineQuery::new(lines, report.stderr, 0);
        query.mode = report.mode;
        return Ok(query);
    }

    let blocks: Vec<(&ReuseCandidate, String)> = report
        .pool
        .iter()
        .map(|c| (c, c.format_text(opts.explain)))
        .collect();
    const OUTPUT_CAP: usize = 30 * 1024;
    let total_blocks = blocks.len();
    let mut emitted: Vec<(&ReuseCandidate, String)> = Vec::new();
    let mut bytes = 0usize;
    let mut capped = 0usize;
    for (index, (candidate, block)) in blocks.into_iter().enumerate() {
        let cost = block.len() + 1;
        if bytes + cost > OUTPUT_CAP {
            capped = total_blocks - index;
            break;
        }
        bytes += cost;
        emitted.push((candidate, block));
    }
    let shown = emitted.len();
    let count_level = |level: ReuseConfidence| {
        emitted
            .iter()
            .filter(|(c, _)| c.confidence == level)
            .count()
    };
    let provisional = count_level(ReuseConfidence::Provisional);
    let low = count_level(ReuseConfidence::Low);
    let medium = count_level(ReuseConfidence::Medium);
    let high = count_level(ReuseConfidence::High);
    let omitted = limit_omitted + capped;
    report.counts = ReuseCounts {
        shown_provisional: provisional,
        shown_low: low,
        shown_medium: medium,
        shown_high: high,
        suppressed: report.counts.suppressed,
        guarded_out: report.counts.guarded_out,
        below_threshold,
        omitted,
        capped,
        shown,
        total: report.counts.total,
        truncated: omitted > 0,
    };
    crate::journal::set_mode(report.mode);
    crate::journal::set_reuse_counts(crate::journal::ReuseJournalCounts {
        provisional: report.counts.shown_provisional,
        low: report.counts.shown_low,
        medium: report.counts.shown_medium,
        high: report.counts.shown_high,
        suppressed: report.counts.suppressed,
        guarded_out: report.counts.guarded_out,
        below_threshold: report.counts.below_threshold,
        omitted: report.counts.omitted,
        capped: report.counts.capped,
        truncated: report.counts.truncated,
    });
    let mut summary = format!(
        "eg: reuse: {} provisional, {} low, {} medium, {} high, {} guarded out, {} below threshold",
        report.counts.shown_provisional,
        report.counts.shown_low,
        report.counts.shown_medium,
        report.counts.shown_high,
        report.counts.guarded_out,
        report.counts.below_threshold
    );
    if report.counts.omitted > 0 {
        summary.push_str(&format!("; {} omitted", report.counts.omitted));
    }
    if report.counts.capped > 0 {
        summary.push_str(&format!("; {} capped", report.counts.capped));
    }
    if report.counts.suppressed > 0 {
        summary.push_str(&format!("; {} suppressed", report.counts.suppressed));
    }
    if report.counts.omitted > 0 {
        summary.push_str(&format!(
            "; showing {} of {}",
            report.counts.shown, report.counts.total
        ));
    }
    if !opts.paths.is_empty() {
        summary.push_str(&format!("; path_matches={}", report.path_matches));
    }
    let mut out = vec![
        format!(
            "eg: reuse — {} duplication candidate(s) found",
            report.counts.shown
        ),
        summary,
    ];
    out.extend(freshness_headers);
    for (_, block) in &emitted {
        for line in block.lines() {
            out.push(line.to_string());
        }
    }
    if report.counts.capped > 0 {
        out.push("eg: output capped at 30 KiB; narrow the path or raise the limit".to_string());
    }
    let surfaced: Vec<String> = emitted
        .iter()
        .take(25)
        .map(|(c, _)| format!("{}->{}", sanitize(&c.symbol), sanitize(&c.canonical)))
        .collect();
    crate::journal::set_candidate_ids(&surfaced.join("\n"));
    crate::journal::record_result(
        crate::journal::OutcomeCounts::of(report.counts.shown, report.counts.total),
        report.freshness.state == Freshness::Stale,
        0,
    );
    if opts.paths.is_empty() && omitted > 0 {
        report.stderr.push_str(
            "eg: tip: scope scans to edited files ('eg reuse <file>') for focused verdicts\n",
        );
    }
    if !opts.judge && (low > 0 || provisional > 0) && judge_is_available() {
        report
            .stderr
            .push_str("eg: tip: rerun with --judge to arbitrate gray-band candidates locally\n");
    }
    let mut query = crate::query::LineQuery::new(out, report.stderr, report.counts.shown);
    query.mode = report.mode;
    query.truncated = report.counts.truncated;
    Ok(query)
}

fn judge_is_available() -> bool {
    let has_bin = std::env::var("EG_JUDGE_BIN")
        .ok()
        .is_some_and(|v| !v.is_empty())
        || std::env::var_os("PATH").is_some_and(|paths| {
            std::env::split_paths(&paths).any(|dir| dir.join("eg-judge").is_file())
        });
    if !has_bin {
        return false;
    }
    let model = match std::env::var("EG_JUDGE_MODEL") {
        Ok(value) if !value.is_empty() => std::path::PathBuf::from(value),
        _ => match std::env::var("HOME") {
            Ok(home) => std::path::PathBuf::from(home)
                .join(".engram")
                .join("judge")
                .join("model"),
            Err(_) => return false,
        },
    };
    model.join("lazy.json").exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tie_candidate(canonical_file: &str, canonical_line: i64) -> ReuseCandidate {
        ReuseCandidate {
            detector: DetectorId::D5,
            symbol: "pkg.Alpha".into(),
            file: "src/a.py".into(),
            line: 1,
            canonical: "pkg.Beta".into(),
            canonical_file: canonical_file.into(),
            canonical_line,
            canonical_callers: 0,
            overlap_pct: None,
            shared_items: Vec::new(),
            raw_client: None,
            confidence: ReuseConfidence::Medium,
            reasons: Vec::new(),
        }
    }

    #[test]
    fn finalize_sort_tie_breaks_on_canonical_file_and_line() {
        let conn = Connection::open_in_memory().unwrap();
        let ctx = QueryCtx {
            conn: &conn,
            root: std::path::Path::new("."),
        };
        let opts = ReuseOpts::new(Vec::new(), 10, false, ReuseConfidence::Low, false).unwrap();
        // Identical detector rank, symbol, file, and line; only the canonical
        // identity differs. Without the tie-breakers the stable sort would
        // keep the insertion order and the zeta candidate would stay first.
        let report = ReuseReport {
            mode: ReuseMode::Scan,
            pool: vec![
                tie_candidate("src/zeta.py", 9),
                tie_candidate("src/alpha.py", 2),
                tie_candidate("src/alpha.py", 1),
            ],
            counts: ReuseCounts::default(),
            freshness: freshness::FreshnessReport::default(),
            path_matches: 0,
            stderr: String::new(),
        };
        let query = finalize(&ctx, report, &opts).unwrap();
        let joined = query.lines.join("\n");
        let alpha1 = joined.find("in src/alpha.py:1)").unwrap();
        let alpha2 = joined.find("in src/alpha.py:2)").unwrap();
        let zeta = joined.find("in src/zeta.py:9)").unwrap();
        assert!(alpha1 < alpha2 && alpha2 < zeta, "{joined}");
        assert!(
            joined.contains("3 duplication candidate(s) found"),
            "{joined}"
        );
    }

    #[test]
    fn finalize_emits_scope_tip_when_repo_wide_scan_omits_candidates() {
        let conn = Connection::open_in_memory().unwrap();
        let ctx = QueryCtx {
            conn: &conn,
            root: std::path::Path::new("."),
        };
        let opts = ReuseOpts::new(Vec::new(), 1, false, ReuseConfidence::Low, false).unwrap();
        let report = ReuseReport {
            mode: ReuseMode::Scan,
            pool: vec![
                tie_candidate("src/alpha.py", 1),
                tie_candidate("src/beta.py", 2),
            ],
            counts: ReuseCounts::default(),
            freshness: freshness::FreshnessReport::default(),
            path_matches: 0,
            stderr: String::new(),
        };
        let query = finalize(&ctx, report, &opts).unwrap();
        assert!(
            query.stderr.contains(
                "eg: tip: scope scans to edited files ('eg reuse <file>') for focused verdicts"
            ),
            "{}",
            query.stderr
        );
    }
}
