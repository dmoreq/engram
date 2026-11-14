//! Data aggregation engine for the native dashboard.
//! Queries `.engram/usage.log`, `.engram/index.db`, and `.engram/memory.db` with sub-millisecond latency.

pub(crate) mod memories;
mod reuse;
mod summary;

pub use memories::{collect_memories, resolve_store, store_ids};
pub use reuse::collect_reuse_candidates;
pub use summary::compute_summary;
pub(crate) use summary::{annotate_worktree_main, classify, ToolClass};

// Journal ingest lives in the child module; the re-exports below keep every
// existing `data::` path and every child module's `super::` path unchanged.
mod ingest;

use ingest::{discover_repos, display_name_for, retain_untracked};
pub use ingest::{live_event, read_usage_entries, Tail, TailEvent};

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use serde_json::Value;

pub struct AggregatedData {
    pub summary: Value,
    pub reuse: Value,
    pub memories: Value,
}

/// Read one line window without materializing the whole file: skip to the
/// start line, then take at most `max_lines` lines. Lines are read through
/// the same capped reader as every journal ingest, so an oversized line
/// cannot grow the window; it shows as an empty numbered row.
pub fn read_snippet(file_path: &Path, line_start: i64, line_end: i64, max_lines: usize) -> String {
    if !file_path.is_file() {
        return String::new();
    }
    let Ok(file) = File::open(file_path) else {
        return String::new();
    };
    let mut reader = BufReader::new(file);
    let start = (line_start.max(1) - 1) as usize;
    let end = (line_end.max(line_start) as usize).max(start + 1);
    let mut out = Vec::new();
    let mut line_no = 0usize;
    loop {
        let text = match ingest::read_line_capped(&mut reader) {
            Ok(ingest::CappedLine::Line(text, _)) => text,
            // An oversized line still occupies a line number; its body is
            // elided so the window stays bounded.
            Ok(ingest::CappedLine::Skipped(_)) => String::new(),
            Ok(ingest::CappedLine::Torn) | Ok(ingest::CappedLine::Eof) | Err(_) => break,
        };
        line_no += 1;
        if line_no <= start {
            continue;
        }
        if out.len() >= max_lines {
            out.push(format!(
                " ... ({} more lines)",
                end.saturating_sub(start + out.len())
            ));
            break;
        }
        if start + out.len() >= end {
            break;
        }
        out.push(format!("{:4} | {}", start + out.len() + 1, text));
    }
    out.join("\n")
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod tail_tests {
    use super::*;
    use crate::db::test_support::{append_file, remove_tree, temp_root, write_file};

    #[test]
    fn tail_delivers_new_lines_and_signals_reset_on_shrink() {
        let dir = temp_root("dashboard-tail");
        let log = dir.join("usage.log");
        write_file(&log, b"{\"a\":1}\n").unwrap();
        let mut tail = Tail::new(&log);

        let mut lines: Vec<String> = Vec::new();
        tail.poll(&log, |e| {
            if let TailEvent::Line(l) = e {
                lines.push(l.to_string())
            }
        });
        assert!(lines.is_empty(), "start at EOF: no backlog");

        append_file(&log, b"{\"b\":2}\n").unwrap();
        tail.poll(&log, |e| {
            if let TailEvent::Line(l) = e {
                lines.push(l.to_string())
            }
        });
        assert_eq!(lines, vec!["{\"b\":2}"]);

        // In-place prune: same inode, smaller file -> Reset, no replay.
        write_file(&log, b"{\"c\":3}\n").unwrap();
        let mut resets = 0;
        let mut after: Vec<String> = Vec::new();
        tail.poll(&log, |e| match e {
            TailEvent::Reset => resets += 1,
            TailEvent::Line(l) => after.push(l.to_string()),
        });
        assert_eq!(resets, 1);
        assert!(after.is_empty(), "no replay after reset");
        remove_tree(&dir);
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;

    #[test]
    fn oversized_journal_lines_are_dropped_before_parsing() {
        let huge = format!(
            "{{\"tool\":\"build\",\"repo\":\"/x\",\"pad\":\"{}\"}}",
            "a".repeat(300 * 1024)
        );
        assert!(live_event(&huge).is_none());
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;
    use crate::db::test_support::{append_file, remove_tree, temp_root, write_file};
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn journal_parse_is_cached_until_the_stamp_changes() {
        let dir = temp_root("dashboard-entries-cache");
        let log = dir.join("usage.log");
        let repo = dir.to_string_lossy().into_owned();
        write_file(
            &log,
            format!("{{\"tool\":\"build\",\"repo\":\"{repo}\",\"repo_name\":\"r\"}}\n").as_bytes(),
        )
        .unwrap();
        assert_eq!(read_usage_entries(&log).len(), 1);
        // Second read hits the cache; an append changes the stamp and re-parses.
        assert_eq!(read_usage_entries(&log).len(), 1);
        append_file(
            &log,
            format!("{{\"tool\":\"build\",\"repo\":\"{repo}\",\"repo_name\":\"r\"}}\n").as_bytes(),
        )
        .unwrap();
        assert_eq!(read_usage_entries(&log).len(), 2);
        remove_tree(&dir);
    }

    #[test]
    fn retain_untracked_reports_security_errors() {
        let dir = temp_root("dashboard-retain-tracked");
        let repo = dir.join("repo");
        crate::db::test_support::make_dir_all(repo.join(".engram")).unwrap();
        write_file(repo.join(".engram/index.db"), b"x").unwrap();
        let init = crate::db::test_support::stripped_git()
            .arg("-C")
            .arg(&repo)
            .args(["init", "-q"])
            .status();
        if !matches!(init, Ok(status) if status.success()) {
            remove_tree(&dir);
            return;
        }
        let added = crate::db::test_support::stripped_git()
            .arg("-C")
            .arg(&repo)
            .args(["add", "-f", ".engram/index.db"])
            .status()
            .unwrap();
        assert!(added.success());
        let mut repos = HashMap::new();
        repos.insert("repo".to_string(), repo);
        let (errors, blocked) = retain_untracked(&mut repos, &HashMap::new());
        assert!(repos.is_empty());
        assert_eq!(blocked, 1);
        assert_eq!(errors[0]["stage"], "security");
        remove_tree(&dir);
    }

    #[test]
    fn tracked_repo_reuse_payload_is_degraded() {
        let dir = temp_root("dashboard-reuse-degraded");
        let repo = dir.join("repo");
        crate::db::test_support::make_dir_all(repo.join(".engram")).unwrap();
        write_file(repo.join(".engram/index.db"), b"x").unwrap();
        let init = crate::db::test_support::stripped_git()
            .arg("-C")
            .arg(&repo)
            .args(["init", "-q"])
            .status();
        if !matches!(init, Ok(status) if status.success()) {
            remove_tree(&dir);
            return;
        }
        let added = crate::db::test_support::stripped_git()
            .arg("-C")
            .arg(&repo)
            .args(["add", "-f", ".engram/index.db"])
            .status()
            .unwrap();
        assert!(added.success());
        let entries = vec![json!({
            "repo": repo.to_string_lossy(),
            "repo_name": "repo",
            "tool": "reuse",
        })];
        let (_, summary, _) = collect_reuse_candidates(Some(&repo), &entries).unwrap();
        assert_eq!(summary["degraded"], true, "{summary}");
        assert_eq!(summary["errors"][0]["stage"], "security", "{summary}");
        remove_tree(&dir);
    }
}
