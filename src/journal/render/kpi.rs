//! KPI text assembly for the CLI and MCP usage summaries: latency
//! percentiles, the token-savings block with its basis breakdown, and the
//! anti-reinvention candidate total. Split out of `render.rs` so the module
//! stays inside the 500-line cap.

use super::{
    estimated_tokens_saved, format_number_with_commas, percentile, ToolAgg, PRICE_PER_TOKEN,
};

/// Shared KPI section for both summaries: overall latency percentiles,
/// reliability, and the token-savings block. `errors_note` carries the global
/// summary's " across all repos" wording into the otherwise-identical line.
pub(super) fn kpi_section(
    aggs: &std::collections::BTreeMap<String, ToolAgg>,
    errors_note: &str,
) -> Vec<String> {
    let mut total = 0;
    let mut total_errors = 0;
    let mut total_stale = 0;
    let mut all_lat = Vec::new();

    for a in aggs.values() {
        total += a.calls;
        total_errors += a.errors;
        total_stale += a.stale;
        all_lat.extend_from_slice(&a.latencies);
    }
    all_lat.sort_unstable();
    let p50 = percentile(&all_lat, 0.5);
    let p95 = percentile(&all_lat, 0.95);

    let reliability_pct = if total > 0 {
        100.0 * (total.saturating_sub(total_errors) as f64) / (total as f64)
    } else {
        100.0
    };

    let (saved_tokens, reuse_hits) = calculate_telemetry_totals(aggs);
    let dollars = (saved_tokens as f64) * PRICE_PER_TOKEN;
    let saved_tokens_formatted = format_number_with_commas(saved_tokens);
    let basis = savings_basis(aggs);

    let mut out = vec![
        format!("  ├─ Speed: p50 = {p50} | p95 = {p95}"),
        format!(
            "  ├─ Reliability: {reliability_pct:.1}% ok ({total_errors} errors{errors_note}, {total_stale} stale reads)"
        ),
        format!(
            "  ├─ Estimated Token Savings: ~{saved_tokens_formatted} tokens (~${dollars:.2})"
        ),
    ];
    if !basis.is_empty() {
        out.push(format!("  ├─ Savings basis: {basis}"));
    }
    out.push(format!(
        "  ├─ Anti-Reinvention: {reuse_hits} duplicate candidates surfaced"
    ));
    out
}

/// Which tool family the estimate comes from. The coefficient table prices a
/// reuse hit far above a graph row, so one line naming the mix keeps the total
/// from reading as measured savings.
fn savings_basis(aggs: &std::collections::BTreeMap<String, ToolAgg>) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut add = |label: &str, tools: &[&str]| {
        let sum: usize = tools
            .iter()
            .map(|tool| {
                aggs.get(*tool).map_or(0, |agg| {
                    estimated_tokens_saved(tool, agg.total_shown, agg.total_results, agg.ok_calls)
                })
            })
            .sum();
        if sum > 0 {
            parts.push(format!("{label} ~{}", format_number_with_commas(sum)));
        }
    };
    add(
        "graph",
        &["context", "callers", "callees", "path", "impact", "imports"],
    );
    add("search", &["search"]);
    add("reuse", &["reuse"]);
    add("symbols", &["symbols"]);
    add("build", &["build"]);
    parts.join(" · ")
}

/// Global savings plus the anti-reinvention candidate total. Both are Ok-only
/// by construction: `fold_tool` accumulates the priced inputs and the reuse
/// scan counters over successful calls alone, matching the dashboard.
pub(super) fn calculate_telemetry_totals(
    aggs: &std::collections::BTreeMap<String, ToolAgg>,
) -> (usize, usize) {
    let mut saved_tokens = 0;
    let mut reuse_hits = 0;

    for (tool, agg) in aggs {
        saved_tokens +=
            estimated_tokens_saved(tool, agg.total_shown, agg.total_results, agg.ok_calls);
        if tool == "reuse" {
            reuse_hits += agg.reuse_scan_results.max(agg.reuse_scan_shown);
        }
    }

    (saved_tokens, reuse_hits)
}
