//! Finalize a reuse report: confidence filter, sort/limit, 30 KiB cap,
//! rendering, and journal counters. Analysis owns the pool and freshness;
//! this module owns everything the user sees.

use super::*;

pub(super) fn finalize(
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
    super::sort_candidates(&mut report.pool);
    let below_threshold = report.counts.below_threshold + extra_below;
    let limit_omitted = report.pool.len().saturating_sub(opts.limit);
    report.pool.truncate(opts.limit);

    let (emitted, capped) = if report.pool.is_empty() {
        (Vec::new(), 0)
    } else {
        let blocks: Vec<(&ReuseCandidate, String)> = report
            .pool
            .iter()
            .map(|c| (c, c.format_text(opts.explain)))
            .collect();
        cap_blocks(blocks)
    };
    let shown = emitted.len();
    let omitted = if shown == 0 {
        0
    } else {
        limit_omitted + capped
    };
    let count_level = |level: ReuseConfidence| {
        emitted
            .iter()
            .filter(|(c, _)| c.confidence == level)
            .count()
    };
    report.counts = ReuseCounts {
        shown_provisional: count_level(ReuseConfidence::Provisional),
        shown_low: count_level(ReuseConfidence::Low),
        shown_medium: count_level(ReuseConfidence::Medium),
        shown_high: count_level(ReuseConfidence::High),
        suppressed: report.counts.suppressed,
        guarded_out: report.counts.guarded_out,
        below_threshold,
        omitted,
        capped,
        shown,
        total: report.counts.total,
        truncated: omitted > 0,
    };
    record_journal(&report, &emitted);

    let out = build_output_lines(&report, opts, &freshness_headers, &emitted);
    if opts.paths.is_empty() && omitted > 0 {
        report.stderr.push_str(
            "eg: tip: scope scans to edited files ('eg reuse <file>') for focused verdicts\n",
        );
    }
    if !opts.judge && emitted.iter().any(|(c, _)| judge::is_judgeable(c)) && judge::is_available() {
        report
            .stderr
            .push_str("eg: tip: rerun with --judge to arbitrate gray-band candidates locally\n");
    }
    let mut query = crate::query::LineQuery::new(out, report.stderr, shown);
    query.mode = report.mode;
    query.truncated = report.counts.truncated;
    Ok(query)
}

fn cap_blocks(blocks: Vec<(&ReuseCandidate, String)>) -> (Vec<(&ReuseCandidate, String)>, usize) {
    const OUTPUT_CAP: usize = 30 * 1024;
    let total_blocks = blocks.len();
    let mut emitted = Vec::new();
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
    (emitted, capped)
}

fn record_journal(report: &ReuseReport, emitted: &[(&ReuseCandidate, String)]) {
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
    if !emitted.is_empty() {
        let surfaced: Vec<String> = emitted
            .iter()
            .take(25)
            .map(|(c, _)| format!("{}->{}", sanitize(&c.symbol), sanitize(&c.canonical)))
            .collect();
        crate::journal::set_candidate_ids(&surfaced.join("\n"));
    }
    crate::journal::record_result(
        crate::journal::OutcomeCounts::of(report.counts.shown, report.counts.total),
        report.freshness.state == Freshness::Stale,
        0,
    );
}

fn format_summary_line(counts: &ReuseCounts, path_matches: Option<usize>) -> String {
    let mut summary = format!(
        "eg: reuse: {} provisional, {} low, {} medium, {} high, {} guarded out, {} below threshold",
        counts.shown_provisional,
        counts.shown_low,
        counts.shown_medium,
        counts.shown_high,
        counts.guarded_out,
        counts.below_threshold
    );
    if counts.omitted > 0 {
        summary.push_str(&format!("; {} omitted", counts.omitted));
    }
    if counts.capped > 0 {
        summary.push_str(&format!("; {} capped", counts.capped));
    }
    if counts.suppressed > 0 {
        summary.push_str(&format!("; {} suppressed", counts.suppressed));
    }
    if counts.omitted > 0 {
        summary.push_str(&format!("; showing {} of {}", counts.shown, counts.total));
    }
    if let Some(matches) = path_matches {
        summary.push_str(&format!("; path_matches={matches}"));
    }
    summary
}

fn build_output_lines(
    report: &ReuseReport,
    opts: &ReuseOpts,
    freshness_headers: &[String],
    emitted: &[(&ReuseCandidate, String)],
) -> Vec<String> {
    if emitted.is_empty() {
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
        lines.extend(freshness_headers.iter().cloned());
        if report.counts.below_threshold > 0
            || report.counts.guarded_out > 0
            || report.counts.suppressed > 0
        {
            lines.push(format_summary_line(&report.counts, None));
        }
        if !opts.paths.is_empty() {
            lines.push(format!("eg: reuse: path_matches={}", report.path_matches));
        }
        return lines;
    }

    let path_matches = (!opts.paths.is_empty()).then_some(report.path_matches);
    let mut out = vec![
        format!(
            "eg: reuse — {} duplication candidate(s) found",
            report.counts.shown
        ),
        format_summary_line(&report.counts, path_matches),
    ];
    out.extend(freshness_headers.iter().cloned());
    for (_, block) in emitted {
        out.extend(block.lines().map(str::to_string));
    }
    if report.counts.capped > 0 {
        out.push("eg: output capped at 30 KiB; narrow the path or raise the limit".to_string());
    }
    out
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
            line_end: 2,
            canonical: "pkg.Beta".into(),
            canonical_file: canonical_file.into(),
            canonical_line,
            canonical_line_end: canonical_line + 1,
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
        let query = finalize(report, &opts).unwrap();
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
        let query = finalize(report, &opts).unwrap();
        assert!(
            query.stderr.contains(
                "eg: tip: scope scans to edited files ('eg reuse <file>') for focused verdicts"
            ),
            "{}",
            query.stderr
        );
    }
}
