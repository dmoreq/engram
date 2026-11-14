//! Usage-log parsing, aggregation, rendering, telemetry, and bounded reads.

use super::*;
use crate::repo::UNATTRIBUTED;

mod parse;
// KPI text assembly lives beside the other render submodules; `render.rs`
// keeps the re-exports so summary call sites and tests stay unchanged.
mod kpi;
mod read;
mod reuse;
mod row;
pub(crate) use parse::{parse_usage_line, UsageFields};
pub(crate) use read::*;
use kpi::kpi_section;
use reuse::ReuseUsage;
use row::tool_row;

#[derive(Default)]
struct RepoAgg {
    /// Row label: the journaled `repo_name`, or `(unattributed)`.
    display: String,
    calls: usize,
    errors: usize,
    worktree: bool,
}

struct ErrorRecord {
    ts: Option<i64>,
    tool: String,
    repo_name: Option<String>,
    exit_code: i64,
    agent: Option<String>,
}

#[derive(Default)]
struct ToolAgg {
    calls: usize,
    errors: usize,
    /// Successful calls only: empty (exit 1) and ambiguous (exit 2) results
    /// carry no savings — the dashboard summary's Ok-only accounting.
    ok_calls: usize,
    empty: usize,
    empty_ok: usize,
    empty_error: usize,
    stale: usize,
    ambiguous: usize,
    capped: usize,
    latencies: Vec<u64>,
    total_shown: usize,
    total_results: usize,
    reuse_usage: ReuseUsage,
    reuse_truncated: usize,
    reuse_modes: usize,
    reuse_scan_shown: usize,
    reuse_scan_results: usize,
    reuse_stale: usize,
    reuse_unknown: usize,
    judge_runs: usize,
    judge_reviewed: usize,
    judge_rejected: usize,
    judge_failed: usize,
}

/// Summarize `usage.log` (+ `.1`); `quality` adds the empty split and cap count.
pub fn summary(root: &Path, quality: bool) -> crate::error::Result<Vec<String>> {
    let lines = load_usage_lines(root)?;
    let aggs = aggregate_usage(&lines);
    Ok(render_summary(&aggs, quality))
}

/// Summarize machine-wide global journal (~/.engram/usage.log): cross-repo calls,
/// per-repository breakdown, error rate, token savings, and recent errors.
pub fn global_summary(quality: bool) -> crate::error::Result<Vec<String>> {
    let lines = load_global_usage_lines()?;
    let (aggs, repo_aggs, recent_errors) = aggregate_global_usage(&lines);
    Ok(render_global_summary(
        &aggs,
        &repo_aggs,
        &recent_errors,
        quality,
    ))
}

fn load_usage_lines_from_path(base_log: &Path) -> crate::error::Result<String> {
    let log = read_usage_log(base_log);
    let mut lines = String::new();
    for gen in log.generations {
        if gen.status != GenerationStatus::Read {
            continue;
        }
        lines.push_str(&gen.text);
        // A generation boundary is a line boundary. Without this, a rotated
        // file that does not end in `\n` splices its last record into the
        // current file's first record.
        if !lines.is_empty() && !lines.ends_with('\n') {
            lines.push('\n');
        }
    }
    if lines.trim().is_empty() {
        return Err(crate::error::EgError::NoResults(
            "no journal entries — usage.log is empty or absent".to_string(),
        ));
    }
    Ok(lines)
}

/// Read both journal generations, newest last (`""` is current, `.1` rotated).
fn load_usage_lines(root: &Path) -> crate::error::Result<String> {
    crate::db::reject_symlink(&crate::db::eg_dir(root), ".engram directory")?;
    crate::db::reject_unsafe_journal(root)?;
    crate::db::reject_tracked_eg_dir(root)?;
    load_usage_lines_from_path(&usage_log_path(root))
}

/// Read both global journal generations from ~/.engram/usage.log.
fn load_global_usage_lines() -> crate::error::Result<String> {
    let path = crate::db::global_usage_log_path()?;
    load_usage_lines_from_path(&path)
}

/// Fold one parsed line into its per-tool aggregate; one counter rule set.
fn fold_tool(fields: &UsageFields, agg: &mut ToolAgg) {
    agg.calls += 1;
    let shown = fields.shown.unwrap_or(fields.result_count).max(0) as usize;
    // One exit-code classification for the CLI and the dashboard: empty and
    // ambiguous results are not failures and price no savings — only a real
    // success bills rows and calls into the savings arms.
    match crate::error::ExitClass::of(fields.exit_code) {
        crate::error::ExitClass::Ok => {
            agg.ok_calls += 1;
            agg.total_shown += shown;
            agg.total_results += fields.result_count.max(0) as usize;
        }
        crate::error::ExitClass::Error => agg.errors += 1,
        crate::error::ExitClass::Empty | crate::error::ExitClass::Ambiguous => {}
    }
    if fields.result_count == 0 {
        agg.empty += 1;
        if fields.exit_code == 0 {
            agg.empty_ok += 1;
        } else {
            agg.empty_error += 1;
        }
    }
    let capped = if fields.tool == "reuse" {
        fields.reuse.is_some_and(|reuse| reuse.capped > 0)
    } else {
        fields.total > shown as i64
    };
    if capped {
        agg.capped += 1;
    }
    if fields.stale {
        agg.stale += 1;
    }
    agg.ambiguous += fields.ambiguous.max(0) as usize;
    if let Some(us) = fields.latency_us {
        agg.latencies.push(us.max(0) as u64);
    }
    reuse::fold(fields.reuse, fields.truncated, fields.tool == "reuse", agg);
    if fields.tool == "reuse" {
        if let Some(judge) = fields.judge {
            agg.judge_runs += 1;
            agg.judge_reviewed += judge.reviewed;
            agg.judge_rejected += judge.rejected;
            agg.judge_failed += judge.failed;
        }
    }
    // Ok-only: empty and ambiguous scans surfaced nothing (dashboard parity).
    if fields.tool == "reuse"
        && fields.exit_code == 0
        && fields.mode.as_deref().is_none_or(|mode| mode == "scan")
    {
        agg.reuse_scan_shown += shown;
        agg.reuse_scan_results += fields.result_count.max(0) as usize;
        if fields.freshness_state.as_deref() == Some("stale") {
            agg.reuse_stale += 1;
        }
        if fields.freshness_state.as_deref() == Some("unknown") {
            agg.reuse_unknown += fields.freshness_unknown.max(1);
        }
    }
}

/// Fold every parsed line into one `ToolAgg` per tool. Only calls against a
/// live repository contribute: dead roots and identityless legacy lines are
/// the same non-entities here as on every other surface.
fn aggregate_usage(lines: &str) -> std::collections::BTreeMap<String, ToolAgg> {
    let mut aggs: std::collections::BTreeMap<String, ToolAgg> = Default::default();
    for line in lines.lines().filter(|l| !l.trim().is_empty()) {
        let Some(fields) = parse_usage_line(line) else {
            continue;
        };
        if !liveness::repo_is_live(fields.repo.as_deref()) {
            continue;
        }
        fold_tool(&fields, aggs.entry(fields.tool.clone()).or_default());
    }
    aggs
}

fn render_summary(
    aggs: &std::collections::BTreeMap<String, ToolAgg>,
    quality: bool,
) -> Vec<String> {
    let total: usize = aggs.values().map(|a| a.calls).sum();
    let mut out = vec![format!("eg usage: {total} journaled calls")];
    out.extend(kpi_section(aggs, ""));

    let mut rows: Vec<(&String, &ToolAgg)> = aggs.iter().collect();
    rows.sort_by(|a, b| b.1.calls.cmp(&a.1.calls).then(a.0.cmp(b.0)));
    out.extend(
        rows.into_iter()
            .map(|(tool, agg)| tool_row(tool, agg, quality)),
    );
    out
}

fn aggregate_global_usage(
    lines: &str,
) -> (
    std::collections::BTreeMap<String, ToolAgg>,
    std::collections::BTreeMap<String, RepoAgg>,
    Vec<ErrorRecord>,
) {
    let mut aggs: std::collections::BTreeMap<String, ToolAgg> = Default::default();
    let mut repo_aggs: std::collections::BTreeMap<String, RepoAgg> = Default::default();
    let mut errors: Vec<ErrorRecord> = Vec::new();

    for line in lines.lines().filter(|l| !l.trim().is_empty()) {
        let Some(fields) = parse_usage_line(line) else {
            continue;
        };
        // One live-inventory rule for every surface: a row needs an absolute
        // root that still exists as a real directory. Dead roots and
        // identityless legacy lines are the same non-entities here as on the
        // dashboard, so both surfaces always list the same repositories.
        let Some(journaled) = fields
            .repo
            .clone()
            .filter(|repo| liveness::repo_is_live(Some(repo)))
        else {
            continue;
        };
        // Global telemetry lists durable repositories only: the same scratch
        // rule every dashboard panel applies, judged on a linked worktree's
        // main root so a scratch checkout of a real project survives.
        if liveness::repo_is_ephemeral(&journaled) {
            continue;
        }
        // A linked worktree contributes under its main repository's row,
        // keyed by the canonical main root; an unresolvable pointer keeps
        // its own row.
        let main = fields
            .worktree
            .then(|| crate::repo::cached_worktree_main_root(Path::new(&journaled)))
            .flatten();
        let (repo_key, display) = match &main {
            Some(main) => {
                let main = main.to_string_lossy().into_owned();
                let display = Path::new(&main)
                    .file_name()
                    .map_or_else(|| main.clone(), |n| n.to_string_lossy().into_owned());
                (main, display)
            }
            None => (
                journaled,
                fields
                    .repo_name
                    .clone()
                    .unwrap_or_else(|| UNATTRIBUTED.to_string()),
            ),
        };
        fold_tool(&fields, aggs.entry(fields.tool.clone()).or_default());

        let r_agg = repo_aggs.entry(repo_key).or_default();
        if r_agg.display.is_empty() {
            r_agg.display = display;
        }
        r_agg.calls += 1;
        if crate::error::ExitClass::of(fields.exit_code) == crate::error::ExitClass::Error {
            r_agg.errors += 1;
            errors.push(ErrorRecord {
                ts: fields.ts,
                tool: fields.tool,
                repo_name: fields.repo_name,
                exit_code: fields.exit_code,
                agent: fields.agent,
            });
        }
        // `[worktree]` marks an unresolved worktree row only; a folded entry
        // already shows under its main repository.
        if fields.worktree && main.is_none() {
            r_agg.worktree = true;
        }
    }
    errors.reverse();
    (aggs, repo_aggs, errors)
}

fn render_global_summary(
    aggs: &std::collections::BTreeMap<String, ToolAgg>,
    repo_aggs: &std::collections::BTreeMap<String, RepoAgg>,
    recent_errors: &[ErrorRecord],
    quality: bool,
) -> Vec<String> {
    let total: usize = aggs.values().map(|a| a.calls).sum();
    let repo_count = repo_aggs.len();
    let mut out = vec![format!(
        "eg global usage: {total} journaled calls across {repo_count} repositories"
    )];
    out.extend(kpi_section(aggs, " across all repos"));

    if repo_count > 0 {
        out.extend(active_repos_section(repo_aggs));
    }

    out.push("  ├─ Tool Breakdown:".to_string());
    let mut rows: Vec<(&String, &ToolAgg)> = aggs.iter().collect();
    rows.sort_by(|a, b| b.1.calls.cmp(&a.1.calls).then(a.0.cmp(b.0)));
    out.extend(
        rows.into_iter()
            .map(|(tool, agg)| tool_row(tool, agg, quality)),
    );

    out.extend(recent_errors_section(recent_errors));
    out
}

fn active_repos_section(repo_aggs: &std::collections::BTreeMap<String, RepoAgg>) -> Vec<String> {
    let mut out = vec!["  ├─ Active Repositories:".to_string()];
    let mut repos: Vec<(&String, &RepoAgg)> = repo_aggs.iter().collect();
    repos.sort_by(|a, b| b.1.calls.cmp(&a.1.calls).then(a.0.cmp(b.0)));
    for (_, r_agg) in repos {
        let rel = if r_agg.calls > 0 {
            100.0 * (r_agg.calls.saturating_sub(r_agg.errors) as f64) / (r_agg.calls as f64)
        } else {
            100.0
        };
        let wt_tag = if r_agg.worktree { " [worktree]" } else { "" };
        out.push(format!(
            "  │   ├─ {}{wt_tag}: {} calls ({rel:.1}% ok, {} errors)",
            r_agg.display, r_agg.calls, r_agg.errors
        ));
    }
    out
}

fn recent_errors_section(recent_errors: &[ErrorRecord]) -> Vec<String> {
    if recent_errors.is_empty() {
        return vec!["  └─ Recent Errors: None (100% clean)".to_string()];
    }
    let now = crate::db::unix_now();
    let mut out = vec!["  └─ Recent Errors (latest first):".to_string()];
    for err in recent_errors.iter().take(5) {
        let repo_label = err.repo_name.as_deref().unwrap_or(UNATTRIBUTED);
        let agent_label = err.agent.as_deref().unwrap_or("user");
        let age_str = err
            .ts
            .map(|t| format_age_secs(now.saturating_sub(t)))
            .unwrap_or_else(|| "-".to_string());
        out.push(format!(
            "      • [{age_str}] {} in {repo_label} [{agent_label}] -> exit {}",
            err.tool, err.exit_code
        ));
    }
    out
}

fn format_age_secs(diff: i64) -> String {
    if diff < 60 {
        format!("{diff}s ago")
    } else if diff < 3600 {
        format!("{}m ago", diff / 60)
    } else if diff < 86400 {
        format!("{}h ago", diff / 3600)
    } else {
        format!("{}d ago", diff / 86400)
    }
}

#[cfg(test)]
mod tests;
