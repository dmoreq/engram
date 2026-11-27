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
        match e.tool.as_str() {
            t if is_navigation(t) => self.nav_successes += 1,
            "search" => {
                self.search_successes += 1;
                self.prewrite_successes += 1;
            }
            "reuse" => self.prewrite_successes += 1,
            "mem_query" => {
                self.mem_query_successes += 1;
                self.mem_reads += 1;
            }
            "mem_get" | "mem_export" => self.mem_reads += 1,
            "mem_write" | "mem_touch" | "mem_delete" | "mem_gc" | "mem_import" | "mem_reindex" => {
                self.mem_writes += 1;
            }
            _ => {}
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

#[derive(Default)]
struct CollectAcc {
    coverage: Coverage,
    window: Metrics,
    baseline: Metrics,
    recent7: Metrics,
}

struct ScopeFilter<'a> {
    now: i64,
    w_start: i64,
    b_start: i64,
    recent_start: i64,
    local_root: Option<&'a Path>,
    local_root_canon: Option<&'a Path>,
    same_root_cache: HashMap<String, bool>,
}

fn ingest_generation(
    gen: &crate::journal::GenerationRead,
    acc: &mut CollectAcc,
    scope: &mut ScopeFilter<'_>,
) -> Result<()> {
    if gen.status == crate::journal::GenerationStatus::Error {
        return Err(EgError::Internal("audit: io-error".into()));
    }
    acc.coverage.truncated |= gen.truncated;
    let status = match gen.status {
        crate::journal::GenerationStatus::Absent => "absent",
        crate::journal::GenerationStatus::Read => "read",
        crate::journal::GenerationStatus::Error => "error",
    };
    acc.coverage.generations.push(GenInfo {
        name: gen.name,
        status,
        bytes: gen.bytes,
        truncated: gen.truncated,
    });
    if gen.status != crate::journal::GenerationStatus::Read {
        return Ok(());
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
        acc.coverage.lines += 1;
        match parse_line(
            line,
            scope.now,
            scope.local_root,
            scope.local_root_canon,
            &mut scope.same_root_cache,
        ) {
            LineOutcome::Valid(e) => {
                acc.coverage.oldest_ts = Some(acc.coverage.oldest_ts.map_or(e.ts, |v| v.min(e.ts)));
                acc.coverage.newest_ts = Some(acc.coverage.newest_ts.map_or(e.ts, |v| v.max(e.ts)));
                if e.ts >= scope.recent_start {
                    acc.recent7.add(&e);
                }
                if e.ts >= scope.w_start {
                    acc.coverage.window_entries += 1;
                    acc.window.add(&e);
                } else if e.ts >= scope.b_start {
                    acc.coverage.baseline_entries += 1;
                    acc.baseline.add(&e);
                } else {
                    acc.coverage.outside_scope_lines += 1;
                }
            }
            LineOutcome::Malformed => acc.coverage.malformed_lines += 1,
            LineOutcome::Invalid => acc.coverage.invalid_lines += 1,
            LineOutcome::Legacy => acc.coverage.legacy_lines += 1,
            LineOutcome::DeadRepo => acc.coverage.dropped_dead_repos += 1,
            LineOutcome::Future => acc.coverage.future_ts_lines += 1,
            LineOutcome::OutsideScope => acc.coverage.outside_scope_lines += 1,
        }
    }
    Ok(())
}

pub fn collect(mode: &Mode, now: i64, days: u32, baseline_days: u32) -> Result<AuditData> {
    let log_path = match mode {
        Mode::Local(root) => {
            crate::db::reject_unsafe_journal(root)?;
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
    let (w_start, b_start, recent_start) = (
        now.saturating_sub(days as i64 * 86_400),
        now.saturating_sub((days + baseline_days) as i64 * 86_400),
        now.saturating_sub(7 * 86_400),
    );
    let mut acc = CollectAcc::default();
    let mut scope = ScopeFilter {
        now,
        w_start,
        b_start,
        recent_start,
        local_root,
        local_root_canon: local_root_canon.as_deref(),
        same_root_cache: HashMap::new(),
    };
    let log = crate::journal::read_usage_log(&log_path);
    for gen in &log.generations {
        ingest_generation(gen, &mut acc, &mut scope)?;
    }

    let (mut coverage, window_metrics, baseline_metrics, recent7) =
        (acc.coverage, acc.window, acc.baseline, acc.recent7);

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

mod parse;
#[cfg(test)]
use parse::canonical_tool;
use parse::parse_line;
pub use parse::{is_navigation, is_priced};

#[cfg(test)]
mod tests;
