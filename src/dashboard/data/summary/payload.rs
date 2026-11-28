//! Dashboard payload assembly: field set, provenance markers, and the recent
//! graph window. Split out of `summary.rs` to stay inside the 500-LOC gate.

use super::*;

/// Assemble the dashboard payload: percentiles, reliability, token savings,
/// session recall, the graph event window, and the per-dimension rollups.
/// Field set and units are documented in `docs/agent-integration.md`; the
/// golden key-set test in `src/dashboard.rs` pins presence and absence.
/// One parsed usage entry: every field the record phases need, plus the
/// tool's dimension flags. Parsing lives beside the payload assembly so the
/// fallback defaults cannot drift between phases.
pub(super) struct EntryView<'a> {
    pub(super) tool: &'a str,
    pub(super) exit_code: i64,
    pub(super) shown: usize,
    pub(super) result_count: usize,
    pub(super) lat_us: Option<f64>,
    pub(super) ts: f64,
    pub(super) repo_name: &'a str,
    /// Aggregation key: the journaled absolute path when present, else the
    /// name. A path keeps two repositories with the same directory name apart.
    pub(super) repo_key: &'a str,
    /// Row label: the main repository's basename when this entry belongs to a
    /// linked worktree, else the journaled `repo_name`.
    pub(super) repo_display: String,
    /// Main repository root when the entry was recorded in a linked worktree
    /// whose `.git` pointer still resolves; the row folds into it.
    pub(super) repo_main: Option<String>,
    pub(super) is_worktree: bool,
    pub(super) agent_name: &'a str,
    pub(super) session_name: &'a str,
    pub(super) session_tagged: bool,
    /// `args.limit` for the tools that record one (`reuse`, `search`): a scan
    /// whose `result_count` reached the limit returned more than it showed.
    /// Journal `mode` for the tools that record one (`reuse`: scan/proposal).
    pub(super) mode: Option<&'a str>,
    /// Journaled truncation verdict: true when results were actually cut.
    pub(super) truncated: bool,
    pub(super) facts: EntryFacts,
}

impl<'a> EntryView<'a> {
    /// `None` when the entry does not name a live repository: only live
    /// repositories contribute to the summary, so there is no unattributed
    /// bucket to fold into. The readers and the pruner already drop dead and
    /// identityless lines; this keeps direct callers honest.
    pub(super) fn parse(e: &'a Value) -> Option<EntryView<'a>> {
        let tool = e.get("tool").and_then(Value::as_str).unwrap_or("unknown");
        let repo_key = e
            .get("repo")
            .and_then(Value::as_str)
            .filter(|s| crate::journal::liveness::repo_is_live(Some(s)))?;
        let repo_name = e
            .get("repo_name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        let is_worktree = e.get("worktree").and_then(Value::as_bool).unwrap_or(false);
        let (bucket, folded) = crate::repo::canonical_repo_bucket(repo_key, is_worktree);
        let repo_main = folded.then_some(bucket);
        let repo_display = match &repo_main {
            Some(main) => main_display(main),
            None => repo_name.unwrap_or(UNATTRIBUTED).to_string(),
        };
        Some(EntryView {
            tool,
            exit_code: e.get("exit_code").and_then(Value::as_i64).unwrap_or(0),
            shown: e
                .get("shown")
                .and_then(Value::as_u64)
                .or_else(|| e.get("result_count").and_then(Value::as_u64))
                .unwrap_or(0) as usize,
            result_count: e.get("result_count").and_then(Value::as_u64).unwrap_or(0) as usize,
            lat_us: e.get("latency_us").and_then(Value::as_f64),
            ts: e.get("ts").and_then(Value::as_f64).unwrap_or(0.0),
            repo_name: repo_name.unwrap_or(UNATTRIBUTED),
            repo_key,
            repo_display,
            repo_main,
            is_worktree,
            agent_name: e
                .get("agent")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or("user"),
            session_name: e
                .get("session")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or("default"),
            session_tagged: e
                .get("session")
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty()),
            mode: e.get("mode").and_then(Value::as_str),
            truncated: e.get("truncated").and_then(Value::as_bool).unwrap_or(false),
            facts: {
                let class = classify(tool);
                EntryFacts {
                    is_graph: class.graph,
                    is_reuse: class.reuse,
                    is_search: tool == "search",
                    is_mem: class.memory,
                    is_mem_read: class.mem_read,
                    is_mem_write: class.mem_write,
                }
            },
        })
    }
}

pub(super) fn summary_json(mut state: SummaryState, entries: &[Value]) -> Value {
    // Only entries that survived the live-repository guard are counted; the
    // raw slice can carry skipped lines when a caller feeds it directly.
    let total_calls = state.recorded;
    let (p50, p95, avg) = compute_percentiles(&mut state.latencies_ms);
    let (graph_p50, graph_p95, graph_avg) = compute_percentiles(&mut state.graph_latencies_ms);
    let graph_recent_queries = recent_graph_window(entries);
    let tool_aggs_json = format_tool_aggs(state.tool_aggs);
    let repo_aggs_json = format_aggs(state.repo_aggs, true);
    let agent_aggs_json = format_aggs(state.agent_aggs, false);
    let reliability = rel_pct(state.errors, total_calls);
    let graph_reliability = rel_pct(state.graph_errors, state.graph_calls);
    let saved_dollars = state.saved_tokens as f64 * crate::journal::PRICE_PER_TOKEN;
    let graph_saved_dollars = state.graph_saved_tokens as f64 * crate::journal::PRICE_PER_TOKEN;
    let total_sessions = state.sessions.len();
    let memory_sessions = state.memory_sessions.len();
    let session_recall_pct = if total_sessions > 0 {
        100.0 * memory_sessions as f64 / total_sessions as f64
    } else {
        0.0
    };

    json!({
        "schema_version": 2,
        "generated_at": crate::db::unix_now(),
        // Readers cover the current log plus its one rotated generation, so a
        // lifetime label would overstate the window after rotation.
        "window": "retained_journal",
        "total_calls": total_calls, "error_count": state.errors,
        "reliability_pct": reliability, "p50_ms": p50, "p95_ms": p95, "avg_ms": avg,
        "saved_tokens": state.saved_tokens, "saved_dollars": saved_dollars,
        "saved_tokens_estimated": true, "saved_dollars_estimated": true,
        "graph_calls": state.graph_calls, "graph_errors": state.graph_errors,
        "graph_reliability_pct": graph_reliability, "graph_p50_ms": graph_p50,
        "graph_p95_ms": graph_p95, "graph_avg_ms": graph_avg,
        "graph_saved_tokens": state.graph_saved_tokens, "graph_saved_dollars": graph_saved_dollars,
        "graph_saved_tokens_estimated": true, "graph_saved_dollars_estimated": true,
        "session_count": total_sessions,
        "memory_session_count": memory_sessions, "session_recall_pct": session_recall_pct,
        "tool_aggs": tool_aggs_json, "repo_aggs": repo_aggs_json, "agent_aggs": agent_aggs_json,
        "graph_recent_queries": graph_recent_queries,
    })
}

fn format_tool_aggs(tool_aggs: HashMap<String, ToolAgg>) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    for (k, mut v) in tool_aggs {
        // No samples serialize as null: an unmeasured percentile is not an
        // instant tool.
        let (p50_t, p95_t, _) = compute_percentiles(&mut v.latencies);
        out.insert(
            k,
            json!({
                "calls": v.calls,
                "errors": v.errors,
                "shown": v.shown,
                "p50": p50_t,
                "p95": p95_t,
                "saved_tokens": v.saved_tokens,
            }),
        );
    }
    out
}

fn rel_pct(errors: usize, calls: usize) -> f64 {
    if calls > 0 {
        100.0 * (calls - errors) as f64 / calls as f64
    } else {
        100.0
    }
}

fn format_aggs(aggs: HashMap<String, RepoAgg>, with_worktree: bool) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    for (k, v) in aggs {
        // `null` when the repository never ran a pre-write search: an
        // unmeasured rate must not render as a perfect one.
        let search_hit_rate_pct = if v.search_calls > 0 {
            Some(100.0 * v.search_hit_calls as f64 / v.search_calls as f64)
        } else {
            None
        };
        // Unmeasured (no reads) serializes as null, never a perfect score.
        let memory_read_hit_rate_pct = if v.memory_reads > 0 {
            Some(100.0 * v.memory_read_hits as f64 / v.memory_reads as f64)
        } else {
            None
        };
        let last_reuse_scan = v.last_reuse_scan.map(|s| {
            json!({
                "ts": s.ts,
                "result_count": s.result_count,
                "limit_capped": s.limit_capped,
                "prev_result_count": s.prev_result_count,
                "prev_ts": s.prev_ts,
            })
        });
        let mut entry = json!({
            "display": v.display,
            "calls": v.calls,
            "errors": v.errors,
            "graph_calls": v.graph_calls,
            "graph_errors": v.graph_errors,
            "last_ts": v.last_ts,
            "reliability_pct": rel_pct(v.errors, v.calls),
            "graph_reliability_pct": rel_pct(v.graph_errors, v.graph_calls),
            "saved_tokens": v.tokens_saved,
            "graph_saved_tokens": v.graph_tokens_saved,
            "reuse_calls": v.reuse_calls,
            "reuse_errors": v.reuse_errors,
            "reuse_reliability_pct": rel_pct(v.reuse_errors, v.reuse_calls),
            "search_calls": v.search_calls,
            "search_hit_rate_pct": search_hit_rate_pct,
            "last_reuse_scan": last_reuse_scan,
            "reuse_saved_tokens": v.reuse_tokens_saved,
            "memory_calls": v.memory_calls,
            "memory_errors": v.memory_errors,
            "memory_reliability_pct": rel_pct(v.memory_errors, v.memory_calls),
            "memory_reads": v.memory_reads,
            "memory_writes": v.memory_writes,
            "memory_read_hit_rate_pct": memory_read_hit_rate_pct,
        });
        if with_worktree {
            entry["worktree"] = json!(v.worktree);
            // Linked worktrees fold into their main repository's row; the
            // per-worktree breakdown feeds the dashboard's expandable
            // sub-rows. Agent rows carry no breakdown.
            if !v.worktrees.is_empty() {
                let worktrees: BTreeMap<&str, Value> = v
                    .worktrees
                    .iter()
                    .map(|(name, w)| {
                        (
                            name.as_str(),
                            json!({
                                "calls": w.calls,
                                "errors": w.errors,
                                "last_ts": w.last_ts,
                            }),
                        )
                    })
                    .collect();
                entry["worktrees"] = json!(worktrees);
            }
        }
        out.insert(k, entry);
    }
    out
}

/// The graph event window, newest first. Only graph tools are rendered, so no
/// all-tools twin is produced. Linked-worktree rows are stamped with their
/// main repository so the feed can note the provenance.
fn recent_graph_window(entries: &[Value]) -> Vec<Value> {
    entries
        .iter()
        .rev()
        .filter(|e| is_graph_tool(e.get("tool").and_then(Value::as_str).unwrap_or("")))
        .take(50)
        .map(|e| {
            let mut row = e.clone();
            crate::dashboard::data::annotate_worktree_main(&mut row);
            row
        })
        .collect()
}

/// Stamp one journal entry with `repo_main` and `repo_main_name` when it was
/// recorded in a linked worktree whose `.git` pointer still resolves. Feeds
/// show the worktree name plus the repository it belongs to, and the summary
/// window reuses the same stamp so both stay consistent.
pub(crate) fn annotate_worktree_main(entry: &mut Value) {
    let Some(repo) = entry.get("repo").and_then(Value::as_str) else {
        return;
    };
    if !entry
        .get("worktree")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return;
    }
    let Some(main) = crate::repo::cached_worktree_main_root(std::path::Path::new(repo)) else {
        return;
    };
    if let Some(name) = main.file_name() {
        let name = name.to_string_lossy().into_owned();
        entry["repo_main"] = json!(main.to_string_lossy());
        entry["repo_main_name"] = json!(name);
    }
}

pub(super) fn empty_summary() -> Value {
    json!({
        "schema_version": 2, "generated_at": crate::db::unix_now(),
        "window": "retained_journal",
        "total_calls": 0, "error_count": 0, "reliability_pct": 100.0,
        "p50_ms": Value::Null, "p95_ms": Value::Null, "avg_ms": Value::Null, "saved_tokens": 0, "saved_dollars": 0.0,
        "saved_tokens_estimated": true, "saved_dollars_estimated": true,
        "graph_calls": 0, "graph_errors": 0, "graph_reliability_pct": 100.0,
        "graph_p50_ms": Value::Null, "graph_p95_ms": Value::Null, "graph_avg_ms": Value::Null,
        "graph_saved_tokens": 0, "graph_saved_dollars": 0.0,
        "graph_saved_tokens_estimated": true, "graph_saved_dollars_estimated": true,
        "session_count": 0, "memory_session_count": 0, "session_recall_pct": 0.0,
        "tool_aggs": {}, "repo_aggs": {}, "agent_aggs": {},
        "graph_recent_queries": []
    })
}
