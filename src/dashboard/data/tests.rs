//! Journal-ingest reader tests: the single live-event gate and the capped
//! line reader (I-12 line bounds). The ingest code lives in `data/ingest.rs`;
//! these tests exercise it through the parent's re-exports and the child
//! module's `pub(super)` items.

use super::ingest::{
    discover_repos, display_name_for, read_usage_entries_uncached, MAX_LINE_BYTES,
};
use super::*;
use crate::db::test_support::{append_file, make_dir_all, remove_tree, temp_root, write_file};
use serde_json::json;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

static STABLE_SEQ: AtomicUsize = AtomicUsize::new(0);

/// A writable repository root OUTSIDE the OS temp tree, under the gitignored
/// `target/` directory: discovery fixtures whose roots must survive the
/// scratch-root rules and stay distinct across parallel runs.
fn stable_root(name: &str) -> std::path::PathBuf {
    let seq = STABLE_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join(format!("eg-{name}-{}-{seq}", std::process::id()));
    remove_tree(&dir);
    dir
}

#[test]
fn discover_repos_keeps_same_basename_repos() {
    let a = stable_root("discover-repo-a");
    let b = stable_root("discover-repo-b");
    make_dir_all(a.join(".engram")).unwrap();
    make_dir_all(b.join(".engram")).unwrap();
    // Same directory name in two distinct roots: the basename key dropped the
    // second repository entirely; the path key keeps both. The cwd probe may
    // add this checkout's own store, so the assertions key on the fixtures.
    let a_dir = a.join("app");
    let b_dir = b.join("other").join("app");
    make_dir_all(a_dir.join(".engram")).unwrap();
    make_dir_all(b_dir.join(".engram")).unwrap();
    // Discovery checks for a real store file, not just the directory.
    write_file(a_dir.join(".engram/memory.db"), b"x").unwrap();
    write_file(b_dir.join(".engram/memory.db"), b"x").unwrap();
    let entries = vec![
        json!({"tool": "build", "repo": a_dir.to_string_lossy(), "repo_name": "app"}),
        json!({"tool": "build", "repo": b_dir.to_string_lossy(), "repo_name": "app"}),
    ];

    let mut repos = HashMap::new();
    discover_repos(None, &entries, "memory.db", &mut repos);
    let a_key = a_dir.to_string_lossy().into_owned();
    let b_key = b_dir.to_string_lossy().into_owned();
    assert!(repos.contains_key(&a_key), "{:?}", repos.keys());
    assert!(repos.contains_key(&b_key), "{:?}", repos.keys());

    // Labels are assigned over the sorted path set: one basename each until
    // they collide, then one ancestor distinguishes them.
    let mut paths: Vec<&String> = repos.keys().collect();
    paths.sort();
    let mut taken = std::collections::HashSet::new();
    let labels: HashMap<String, String> = paths
        .into_iter()
        .map(|p| {
            let label = display_name_for(Path::new(p), &mut taken);
            (p.clone(), label)
        })
        .collect();
    let a_label = &labels[&a_key];
    let b_label = &labels[&b_key];
    assert_ne!(a_label, b_label, "display names must disambiguate");
    assert!(a_label.ends_with("app") && b_label.ends_with("app"));

    remove_tree(&a);
    remove_tree(&b);
}

#[test]
fn live_event_keeps_only_live_repo_entries() {
    let live_root = crate::db::test_support::temp_root("dashboard-live-event");
    let live = live_root.to_string_lossy().into_owned();
    let kept = live_event(
        json!({ "tool": "build", "repo": live, "repo_name": "live-repo" })
            .to_string()
            .as_str(),
    )
    .unwrap();
    assert_eq!(kept["repo"], live);
    assert_eq!(kept["repo_name"], "live-repo");

    // Dead absolute root, legacy relative spelling, identityless line, and
    // garbage: all dropped by the same predicate (no unattributed bucket).
    for line in [
        json!({ "tool": "build", "repo": "/nonexistent-eg-dash/dead", "repo_name": "agent-skills" }).to_string(),
        json!({ "tool": "build", "repo": ".tmp/verify/repo1", "repo_name": "repo1" }).to_string(),
        json!({ "tool": "build" }).to_string(),
        "not json".to_string(),
    ] {
        assert!(live_event(&line).is_none(), "{line}");
    }
    crate::db::test_support::remove_tree(&live_root);
}

#[test]
fn uncached_reader_skips_oversized_line() {
    let dir = temp_root("dashboard-oversized-line");
    let log = dir.join("usage.log");
    let repo = dir.to_string_lossy().into_owned();
    let valid = json!({"tool": "build", "repo": repo, "repo_name": "r"}).to_string();
    let oversized = format!(
        "{{\"tool\":\"build\",\"repo\":\"{repo}\",\"pad\":\"{}\"}}",
        "a".repeat(MAX_LINE_BYTES + 64 * 1024)
    );
    let mut body = oversized;
    body.push('\n');
    body.push_str(&valid);
    body.push('\n');
    write_file(&log, body.as_bytes()).unwrap();

    // The 300 KiB-class line is skipped without materializing it; only the
    // valid event parses.
    let entries = read_usage_entries_uncached(&log);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["repo_name"], "r");
    remove_tree(&dir);
}

#[test]
fn capped_reader_accepts_line_at_exact_cap() {
    // The fits boundary is the content length: a line of exactly
    // MAX_LINE_BYTES content bytes with its newline parses; one byte more is
    // skipped whole.
    let dir = temp_root("dashboard-cap-boundary");
    let log = dir.join("usage.log");
    let repo = dir.to_string_lossy().into_owned();
    let base = format!("{{\"tool\":\"build\",\"repo\":\"{repo}\",\"repo_name\":");
    // Tails: `"r","pad":"` is 11 bytes and the closing `"` + `}` is 2, so 13;
    // `"skip-me","pad":"` is 17 bytes plus the 2-byte close, so 19.
    let fitting = format!(
        "{base}\"r\",\"pad\":\"{}\"}}",
        "a".repeat(MAX_LINE_BYTES - base.len() - 13)
    );
    assert_eq!(fitting.len(), MAX_LINE_BYTES);
    let oversized = format!(
        "{base}\"skip-me\",\"pad\":\"{}\"}}",
        "a".repeat(MAX_LINE_BYTES + 1 - base.len() - 19)
    );
    assert_eq!(oversized.len(), MAX_LINE_BYTES + 1);
    write_file(&log, format!("{fitting}\n{oversized}\n").as_bytes()).unwrap();

    let entries = read_usage_entries_uncached(&log);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["repo_name"], "r");
    remove_tree(&dir);
}

#[test]
fn tail_poll_oversized_line_yields_reset_free_skip() {
    let dir = temp_root("dashboard-tail-oversized");
    let log = dir.join("usage.log");
    let repo = dir.to_string_lossy().into_owned();
    let valid = json!({"tool": "build", "repo": repo, "repo_name": "r"}).to_string();
    write_file(&log, format!("{valid}\n").as_bytes()).unwrap();
    let mut tail = Tail::new(&log);
    let mut lines: Vec<String> = Vec::new();
    tail.poll(&log, |e| {
        if let TailEvent::Line(l) = e {
            lines.push(l.to_string())
        }
    });
    assert!(lines.is_empty(), "start at EOF: no backlog");

    // Append an oversized line, then a valid one: only the valid line is
    // delivered; the oversized line is skipped whole.
    let oversized = format!(
        "{{\"tool\":\"build\",\"repo\":\"{repo}\",\"pad\":\"{}\"}}",
        "a".repeat(MAX_LINE_BYTES + 1024)
    );
    append_file(&log, format!("{oversized}\n{valid}\n").as_bytes()).unwrap();
    let mut lines: Vec<String> = Vec::new();
    tail.poll(&log, |e| {
        if let TailEvent::Line(l) = e {
            lines.push(l.to_string())
        }
    });
    assert_eq!(lines, vec![valid]);

    // The offset landed past both lines: a re-poll delivers nothing and no
    // reset (a sane offset, not a rewind onto the skipped line).
    let mut lines: Vec<String> = Vec::new();
    let mut resets = 0;
    tail.poll(&log, |e| match e {
        TailEvent::Reset => resets += 1,
        TailEvent::Line(l) => lines.push(l.to_string()),
    });
    assert!(lines.is_empty(), "no replay after the skip");
    assert_eq!(resets, 0, "no reset after the skip");
    remove_tree(&dir);
}
