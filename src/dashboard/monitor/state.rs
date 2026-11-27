//! In-memory state and metrics aggregation for the interactive TUI monitor.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::dashboard::data;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonitorTab {
    Graph,
    Reuse,
    Memory,
    Stream,
}

impl MonitorTab {
    pub fn index(&self) -> usize {
        match self {
            Self::Graph => 1,
            Self::Reuse => 2,
            Self::Memory => 3,
            Self::Stream => 4,
        }
    }

    pub fn title(&self) -> &'static str {
        match self {
            Self::Graph => "1 Graph",
            Self::Reuse => "2 Anti-Reinvent",
            Self::Memory => "3 Memory",
            Self::Stream => "4 Stream",
        }
    }

    pub fn next(&self) -> Self {
        match self {
            Self::Graph => Self::Reuse,
            Self::Reuse => Self::Memory,
            Self::Memory => Self::Stream,
            Self::Stream => Self::Graph,
        }
    }
}

pub struct TuiState {
    pub active_tab: MonitorTab,
    pub log_path: PathBuf,
    pub repo_filter: Option<String>,
    pub errors_only: bool,
    /// Global-journal mode: scratch stores hide from every event path, the
    /// same exclusion the linear mode applies. Set once at construction.
    pub exclude_ephemeral: bool,
    pub summary: Value,
    pub reuse_candidates: Vec<Value>,
    pub reuse_summary: Value,
    pub memories: Vec<Value>,
    pub recent_events: Vec<Value>,
    pub total_repos: usize,
    last_stamps: ((u64, u64), (u64, u64)),
}

fn file_stamp(path: &Path) -> (u64, u64) {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_file() && !m.file_type().is_symlink() => {
            let ns = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            (ns, m.len())
        }
        _ => (0, 0),
    }
}

fn log_stamps(path: &Path) -> ((u64, u64), (u64, u64)) {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    (file_stamp(&parent.join("usage.log.1")), file_stamp(path))
}

impl TuiState {
    pub fn new(
        root: Option<&Path>,
        log_path: PathBuf,
        repo_filter: Option<String>,
        errors_only: bool,
        exclude_ephemeral: bool,
    ) -> Self {
        let mut state = Self {
            active_tab: MonitorTab::Graph,
            log_path,
            repo_filter,
            errors_only,
            exclude_ephemeral,
            summary: json!({}),
            reuse_candidates: Vec::new(),
            reuse_summary: json!({}),
            memories: Vec::new(),
            recent_events: Vec::new(),
            total_repos: 0,
            last_stamps: ((0, 0), (0, 0)),
        };
        state.refresh(root);
        state
    }

    pub fn refresh_if_changed(&mut self, root: Option<&Path>) -> bool {
        let stamps = log_stamps(&self.log_path);
        if stamps == self.last_stamps {
            return false;
        }
        self.refresh(root);
        true
    }

    pub fn refresh(&mut self, root: Option<&Path>) {
        self.last_stamps = log_stamps(&self.log_path);
        let mut entries = data::read_usage_entries(&self.log_path);
        if self.exclude_ephemeral {
            entries.retain(|e| !crate::journal::liveness::entry_is_ephemeral(e));
        }
        self.summary = data::compute_summary(&entries);

        if let Ok((candidates, summary, _)) = data::collect_reuse_candidates(root, &entries) {
            self.reuse_candidates = candidates;
            self.reuse_summary = summary;
        }

        if let Ok(payload) = data::collect_memories(root, &entries) {
            self.memories = payload
                .get("notes")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
        }

        self.total_repos = self
            .summary
            .get("repo_aggs")
            .and_then(Value::as_object)
            .map(|m| {
                m.keys()
                    .filter(|key| key.as_str() != crate::repo::UNATTRIBUTED)
                    .count()
            })
            .unwrap_or(0);

        self.recent_events.clear();
        for e in entries.iter().rev().take(100) {
            if self.matches_filter(e) {
                self.recent_events.push(e.clone());
            }
        }
    }

    pub fn matches_filter(&self, val: &Value) -> bool {
        // One predicate for both monitor modes: the duplicated body lived here
        // and in `is_event_matched`, so `--repo` and `--errors` could drift.
        super::is_event_matched(
            val,
            self.repo_filter.as_deref(),
            self.errors_only,
            self.exclude_ephemeral,
        )
    }

    pub fn push_event(&mut self, val: Value) {
        if self.matches_filter(&val) {
            self.recent_events.insert(0, val);
            if self.recent_events.len() > 200 {
                self.recent_events.pop();
            }
        }
    }
}

pub fn classify_usp_pillar(tool: &str) -> (&'static str, &'static str) {
    let class = data::classify(tool);
    if class.memory {
        ("MEMORY", "\x1b[35m")
    } else if class.reuse {
        ("REUSE", "\x1b[32m")
    } else if class.graph {
        ("GRAPH", "\x1b[36m")
    } else {
        // stats/doctor/unresolved are diagnostics, not graph queries.
        ("DIAG", "\x1b[90m")
    }
}

pub fn format_timestamp(ts: i64) -> String {
    if ts <= 0 {
        return "--:--:--".to_string();
    }
    let secs = (ts % 86400) as u32;
    let hours = secs / 3600;
    let mins = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{hours:02}:{mins:02}:{s:02}")
}

pub fn truncate_chars(s: &str, max_chars: usize) -> &str {
    match s.char_indices().nth(max_chars) {
        Some((byte_offset, _)) => &s[..byte_offset],
        None => s,
    }
}
