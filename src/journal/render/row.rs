//! Per-tool usage rows for the human summaries. Split from render.rs to
// stay under the module-size cap.

use super::ToolAgg;
use crate::journal::{percent, percentile};

pub(super) fn tool_row(tool: &str, agg: &ToolAgg, quality: bool) -> String {
    let mut lat = agg.latencies.clone();
    lat.sort_unstable();
    let p50 = percentile(&lat, 0.5);
    let p95 = percentile(&lat, 0.95);
    if quality {
        let mut row = format!(
            "  {tool}: {} calls, errors={} empty_ok={} ({}) empty_error={} stale={} ambiguous={} capped={} p50={p50} p95={p95}",
            agg.calls,
            agg.errors,
            agg.empty_ok,
            percent(agg.empty_ok, agg.calls),
            agg.empty_error,
            agg.stale,
            agg.ambiguous,
            agg.capped,
        );
        if agg.reuse_modes > 0 {
            row.push_str(&format!(
                " reuse_modes={} truncated={} provisional={} low={} medium={} high={} suppressed={} guarded={} below={} omitted={}",
                agg.reuse_modes,
                agg.reuse_truncated,
                agg.reuse_usage.provisional,
                agg.reuse_usage.low,
                agg.reuse_usage.medium,
                agg.reuse_usage.high,
                agg.reuse_usage.suppressed,
                agg.reuse_usage.guarded_out,
                agg.reuse_usage.below_threshold,
                agg.reuse_usage.omitted,
            ));
            if agg.reuse_stale > 0 || agg.reuse_unknown > 0 {
                row.push_str(&format!(
                    " stale={} unknown={}",
                    agg.reuse_stale, agg.reuse_unknown
                ));
            }
        }
        if tool == "reuse" && agg.judge_runs > 0 {
            row.push_str(&format!(
                " judge_reviewed={} judge_rejected={} judge_failed={}",
                agg.judge_reviewed, agg.judge_rejected, agg.judge_failed
            ));
        }
        row
    } else {
        format!(
            "  {tool}: {} calls, errors={} empty={} stale={} ambiguous={} p50={p50} p95={p95}",
            agg.calls, agg.errors, agg.empty, agg.stale, agg.ambiguous,
        )
    }
}
