//! Audit collection: journal generations, strict line validation, window and
//! baseline split, one-pass metrics, and source discovery. No writes.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use crate::error::{EgError, ExitClass, Result};

use super::index::{self, RepoFacts};
use super::memory::{self, MemoryFacts};

pub enum Mode {
    Local(PathBuf),
    Global,
}

impl Mode {
    pub fn is_global(&self) -> bool {
        matches!(self, Mode::Global)
    }
}

pub struct Window {
    pub days: u32,
    pub start_ts: i64,
    pub end_ts: i64,
}

pub struct BaselineInfo {
    pub days: u32,
    pub start_ts: i64,
    pub end_ts: i64,
    pub sufficient: bool,
    pub successful_calls: usize,
    pub active_days: usize,
    pub clipped: bool,
}

pub struct GenInfo {
    pub name: &'static str,
    pub status: &'static str,
    pub bytes: u64,
    pub truncated: bool,
}

#[derive(Default)]
pub struct Coverage {
    pub window_entries: usize,
    pub baseline_entries: usize,
    pub lines: usize,
    pub malformed_lines: usize,
    pub invalid_lines: usize,
    pub legacy_lines: usize,
    pub dropped_dead_repos: usize,
    pub future_ts_lines: usize,
    pub outside_scope_lines: usize,
    pub oldest_ts: Option<i64>,
    pub newest_ts: Option<i64>,
    pub truncated: bool,
    pub generations: Vec<GenInfo>,
    pub repos_active: usize,
    pub repos_checked: usize,
    pub repos_truncated: bool,
    pub stores_checked: usize,
    pub stores_truncated: bool,
}

#[derive(Default, Clone)]
pub struct ToolAgg {
    pub calls: usize,
    pub successes: usize,
    pub empty: usize,
    pub latencies_ms: Vec<f64>,
}

impl ToolAgg {
    /// Nearest-rank p95, the same convention as the CLI and dashboard.
    pub fn p95(&self) -> f64 {
        if self.latencies_ms.is_empty() {
            return 0.0;
        }
        let mut sorted = self.latencies_ms.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        sorted[(0.95 * sorted.len() as f64).ceil() as usize - 1]
    }
}

#[derive(Default, Clone)]
pub struct Metrics {
    pub entries: usize,
    pub successes: usize,
    pub ops_critical: usize,
    pub lock_errors: usize,
    pub nav_successes: usize,
    pub prewrite_successes: usize,
    pub mem_reads: usize,
    pub mem_writes: usize,
    pub priced_successes: usize,
    pub savings: usize,
    pub nav_empty: usize,
    pub search_successes: usize,
    pub search_empty: usize,
    pub mem_query_successes: usize,
    pub mem_query_empty: usize,
    pub active_days: BTreeSet<i64>,
    pub repos: BTreeSet<PathBuf>,
    pub oldest_ts: Option<i64>,
    pub tools: BTreeMap<String, ToolAgg>,
}

impl Metrics {
    fn record_empty(&mut self, tool: &str) {
        if is_navigation(tool) {
            self.nav_empty += 1;
        } else if tool == "search" {
            self.search_empty += 1;
        } else if tool == "mem_query" {
            self.mem_query_empty += 1;
        }
    }

    fn add(&mut self, e: &Entry) {
        self.entries += 1;
        self.active_days.insert(e.ts.div_euclid(86400));
        self.oldest_ts = Some(self.oldest_ts.map_or(e.ts, |v| v.min(e.ts)));
        self.repos.insert(e.repo.clone());
        let tool = self.tools.entry(e.tool.clone()).or_default();
        tool.calls += 1;
        match ExitClass::of(e.exit_code) {
            ExitClass::Error => {
                match e.exit_code {
                    4 => self.lock_errors += 1,
                    3 | 5 | 6 | 7 => self.ops_critical += 1,
                    _ => {}
                }
                return;
            }
            ExitClass::Ambiguous => return,
            ExitClass::Empty => {
                tool.empty += 1;
                self.record_empty(&e.tool);
                if matches!(e.tool.as_str(), "mem_query" | "mem_get" | "mem_export") {
                    self.mem_reads += 1;
                }
                return;
            }
            ExitClass::Ok => {}
        }
        if let Some(us) = e.latency_us {
            tool.latencies_ms.push(us as f64 / 1000.0);
        }
        self.successes += 1;
        tool.successes += 1;
        if e.result_count == 0 {
            tool.empty += 1;
            self.record_empty(&e.tool);
        }
        if is_navigation(&e.tool) {
            self.nav_successes += 1;
        } else if e.tool == "search" {
            self.search_successes += 1;
            self.prewrite_successes += 1;
        } else if e.tool == "reuse" {
            self.prewrite_successes += 1;
        } else if e.tool == "mem_query" {
            self.mem_query_successes += 1;
            self.mem_reads += 1;
        } else if matches!(e.tool.as_str(), "mem_get" | "mem_export") {
            self.mem_reads += 1;
        } else if matches!(
            e.tool.as_str(),
            "mem_write" | "mem_touch" | "mem_delete" | "mem_gc" | "mem_import" | "mem_reindex"
        ) {
            self.mem_writes += 1;
        }
        if is_priced(&e.tool) {
            self.priced_successes += 1;
            self.savings += crate::journal::estimated_tokens_saved(
                &e.tool,
                e.shown.max(0) as usize,
                e.result_count.max(0) as usize,
                1,
            );
        }
    }
}

pub struct AuditData {
    pub mode: &'static str,
    pub now: i64,
    pub window: Window,
    pub baseline: BaselineInfo,
    pub coverage: Coverage,
    pub window_metrics: Metrics,
    pub baseline_metrics: Metrics,
    pub recent7: Metrics,
    pub repos: Vec<RepoFacts>,
    pub memory: MemoryFacts,
}

struct Entry {
    tool: String,
    exit_code: i64,
    result_count: i64,
    shown: i64,
    latency_us: Option<i64>,
    ts: i64,
    repo: PathBuf,
}

enum LineOutcome {
    Valid(Entry),
    Malformed,
    Invalid,
    Legacy,
    DeadRepo,
    Future,
    OutsideScope,
}

pub fn collect(mode: &Mode, now: i64, days: u32, baseline_days: u32) -> Result<AuditData> {
    let log_path = match mode {
        Mode::Local(root) => {
            crate::db::reject_symlink(&crate::db::eg_dir(root), ".engram directory")?;
            crate::db::reject_unsafe_journal(root)?;
            crate::db::reject_tracked_eg_dir(root)?;
            crate::journal::usage_log_path(root)
        }
        Mode::Global => crate::db::global_usage_log_path()?,
    };
    let local_root = match mode {
        Mode::Local(r) => Some(r.as_path()),
        Mode::Global => None,
    };
    let local_root_canon =
        local_root.map(|r| std::fs::canonicalize(r).unwrap_or_else(|_| r.to_path_buf()));
    let mut same_root_cache: HashMap<String, bool> = HashMap::new();
    let (w_start, b_start, recent_start) = (
        now.saturating_sub(days as i64 * 86_400),
        now.saturating_sub((days + baseline_days) as i64 * 86_400),
        now.saturating_sub(7 * 86_400),
    );
    let (mut coverage, mut window_metrics, mut baseline_metrics, mut recent7) = (
        Coverage::default(),
        Metrics::default(),
        Metrics::default(),
        Metrics::default(),
    );
    let log = crate::journal::read_usage_log(&log_path);
    for gen in &log.generations {
        if gen.status == crate::journal::GenerationStatus::Error {
            return Err(EgError::Internal("audit: io-error".into()));
        }
        coverage.truncated |= gen.truncated;
        let status = match gen.status {
            crate::journal::GenerationStatus::Absent => "absent",
            crate::journal::GenerationStatus::Read => "read",
            crate::journal::GenerationStatus::Error => "error",
        };
        coverage.generations.push(GenInfo {
            name: gen.name,
            status,
            bytes: gen.bytes,
            truncated: gen.truncated,
        });
        if gen.status != crate::journal::GenerationStatus::Read {
            continue;
        }
        let mut segments: Vec<&str> = gen.text.split('\n').collect();
        if gen.name == "usage.log" && !gen.text.ends_with('\n') {
            segments.pop();
        }
        for raw in segments {
            let line = raw.trim_end_matches('\r').trim();
            if line.is_empty() {
                continue;
            }
            coverage.lines += 1;
            match parse_line(
                line,
                now,
                local_root,
                local_root_canon.as_deref(),
                &mut same_root_cache,
            ) {
                LineOutcome::Valid(e) => {
                    coverage.oldest_ts = Some(coverage.oldest_ts.map_or(e.ts, |v| v.min(e.ts)));
                    coverage.newest_ts = Some(coverage.newest_ts.map_or(e.ts, |v| v.max(e.ts)));
                    if e.ts >= recent_start {
                        recent7.add(&e);
                    }
                    if e.ts >= w_start {
                        coverage.window_entries += 1;
                        window_metrics.add(&e);
                    } else if e.ts >= b_start {
                        coverage.baseline_entries += 1;
                        baseline_metrics.add(&e);
                    } else {
                        coverage.outside_scope_lines += 1;
                    }
                }
                LineOutcome::Malformed => coverage.malformed_lines += 1,
                LineOutcome::Invalid => coverage.invalid_lines += 1,
                LineOutcome::Legacy => coverage.legacy_lines += 1,
                LineOutcome::DeadRepo => coverage.dropped_dead_repos += 1,
                LineOutcome::Future => coverage.future_ts_lines += 1,
                LineOutcome::OutsideScope => coverage.outside_scope_lines += 1,
            }
        }
    }

    // Clipped when oldest valid entry is newer than baseline start.
    let clipped = coverage.oldest_ts.map(|t| t > b_start).unwrap_or(false);
    let sufficient = baseline_metrics.successes >= 20
        && baseline_metrics.active_days.len() >= 3
        && !clipped
        && baseline_metrics.entries > 0;

    let mut repo_paths: Vec<PathBuf> = match mode {
        Mode::Local(root) => vec![root.clone()],
        Mode::Global => {
            let mut set = window_metrics.repos.clone();
            set.extend(baseline_metrics.repos.iter().cloned());
            set.into_iter().collect()
        }
    };
    repo_paths.sort();
    let repos_truncated = repo_paths.len() > 50;
    repo_paths.truncate(50);
    coverage.repos_active = repo_paths.len();
    let repos = index::collect(&repo_paths, now);
    coverage.repos_checked = repos.len();
    coverage.repos_truncated = repos_truncated;

    let memory = memory::collect(mode, &repo_paths, now);
    coverage.stores_checked = memory.stores_checked;
    coverage.stores_truncated = memory.stores_truncated;

    Ok(AuditData {
        mode: if mode.is_global() { "global" } else { "local" },
        now,
        window: Window {
            days,
            start_ts: w_start,
            end_ts: now + 1,
        },
        baseline: BaselineInfo {
            days: baseline_days,
            start_ts: b_start,
            end_ts: w_start,
            sufficient,
            successful_calls: baseline_metrics.successes,
            active_days: baseline_metrics.active_days.len(),
            clipped,
        },
        coverage,
        window_metrics,
        baseline_metrics,
        recent7,
        repos,
        memory,
    })
}

fn opt_nonneg(v: &serde_json::Value, key: &str) -> Option<Option<i64>> {
    match v.get(key) {
        None => Some(None),
        Some(x) => x.as_i64().filter(|&n| n >= 0).map(Some),
    }
}

fn parse_line(
    line: &str,
    now: i64,
    local_root: Option<&Path>,
    local_root_canon: Option<&Path>,
    same_root_cache: &mut HashMap<String, bool>,
) -> LineOutcome {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
        return LineOutcome::Malformed;
    };
    let Some(tool_raw) = v.get("tool").and_then(|x| x.as_str()) else {
        return LineOutcome::Malformed;
    };
    if tool_raw.is_empty() || tool_raw.len() > 64 || tool_raw.chars().any(char::is_control) {
        return LineOutcome::Malformed;
    }
    let Some(ts) = v.get("ts").and_then(|x| x.as_i64()) else {
        return LineOutcome::Legacy;
    };
    if ts < 0 {
        return LineOutcome::Invalid;
    }
    if ts > now.saturating_add(300) {
        return LineOutcome::Future;
    }
    let Some(exit_code) = v
        .get("exit_code")
        .and_then(|x| x.as_i64())
        .filter(|c| (0..=7).contains(c))
    else {
        return LineOutcome::Invalid;
    };
    let (Some(rc_opt), Some(shown_opt), Some(latency_us)) = (
        opt_nonneg(&v, "result_count"),
        opt_nonneg(&v, "shown"),
        opt_nonneg(&v, "latency_us"),
    ) else {
        return LineOutcome::Invalid;
    };
    let result_count = rc_opt.unwrap_or(0);
    let shown = shown_opt.unwrap_or(result_count);
    let Some(repo_raw) = v
        .get("repo")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
    else {
        return LineOutcome::DeadRepo;
    };
    if !crate::journal::liveness::repo_is_live(Some(repo_raw))
        || (local_root.is_none() && crate::journal::liveness::repo_is_ephemeral(repo_raw))
    {
        return LineOutcome::DeadRepo;
    }
    let worktree = v.get("worktree").and_then(|x| x.as_bool()).unwrap_or(false);
    let (bucket, _) =
        crate::repo::canonical_repo_bucket(repo_raw, local_root.is_none() && worktree);
    let repo = PathBuf::from(bucket);
    if let (Some(root), Some(root_canon)) = (local_root, local_root_canon) {
        let same = *same_root_cache
            .entry(repo_raw.to_string())
            .or_insert_with(|| {
                std::fs::canonicalize(&repo).map_or_else(|_| repo == root, |c| c == root_canon)
            });
        if !same {
            return LineOutcome::OutsideScope;
        }
    }
    LineOutcome::Valid(Entry {
        tool: canonical_tool(tool_raw),
        exit_code,
        result_count,
        shown,
        latency_us,
        ts,
        repo,
    })
}

fn canonical_tool(tool: &str) -> String {
    if is_priced(tool) || crate::journal::UNPRICED_TOOLS.contains(&tool) {
        tool.to_string()
    } else {
        "other".to_string()
    }
}

pub fn is_priced(tool: &str) -> bool {
    is_navigation(tool) || matches!(tool, "search" | "reuse" | "symbols" | "build")
}

pub fn is_navigation(tool: &str) -> bool {
    matches!(
        tool,
        "callers" | "callees" | "context" | "path" | "impact" | "imports"
    )
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod stream3_tests {
    use super::*;
    use crate::db::test_support as ts;
    #[test]
    fn global_folds_worktree_and_drops_ephemeral() {
        let m = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/eg-wt-test");
        ts::remove_tree(&m);
        ts::make_dir_all(m.join(".git/worktrees/w")).unwrap();
        let (wt, s) = (ts::temp_root("awt"), ts::temp_root("asc"));
        ts::write_file(
            wt.join(".git"),
            format!("gitdir: {}/.git/worktrees/w\n", m.display()).as_bytes(),
        )
        .unwrap();
        let p = |d: &Path, w, r: Option<&Path>| {
            let mut cache = HashMap::new();
            let canon = r.map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()));
            let line = format!(
                r#"{{"ts":10,"tool":"callers","exit_code":0,"repo":"{}","worktree":{w}}}"#,
                d.display()
            );
            parse_line(&line, 20, r, canon.as_deref(), &mut cache)
        };
        let e = std::fs::canonicalize(&m).unwrap();
        assert!(matches!(p(&wt, true, None), LineOutcome::Valid(x) if x.repo == e));
        assert!(matches!(p(&s, false, None), LineOutcome::DeadRepo));
        assert!(matches!(p(&s, false, Some(&s)), LineOutcome::Valid(_)));
        [&m, &wt, &s].into_iter().for_each(ts::remove_tree);
    }

    #[test]
    fn exit_1_empty_tracks_counters_without_success_or_latency() {
        let mut m = Metrics::default();
        for (tool, latency) in [
            ("callers", Some(5000)),
            ("search", Some(4000)),
            ("mem_query", Some(3000)),
        ] {
            m.add(&Entry {
                tool: tool.into(),
                exit_code: 1,
                result_count: 0,
                shown: 0,
                latency_us: latency,
                ts: 100,
                repo: PathBuf::from("/tmp/r"),
            });
        }
        assert_eq!(
            (
                m.entries,
                m.successes,
                m.savings,
                m.nav_empty,
                m.search_empty,
                m.mem_query_empty,
                m.mem_reads
            ),
            (3, 0, 0, 1, 1, 1, 1)
        );
        assert!(m.tools["callers"].latencies_ms.is_empty() && m.tools["callers"].empty == 1);
    }
}
