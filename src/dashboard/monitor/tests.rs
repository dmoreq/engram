//! Regression tests for the monitor: security gates, log-path mode
//! resolution, and the single event-match predicate both modes share.

use super::*;

#[test]
fn monitor_gate_rejects_tracked_and_symlinked_journals() {
    let dir = crate::db::test_support::temp_root("monitor-gate");
    let repo = dir.join("repo");
    crate::db::test_support::make_dir_all(repo.join(".engram")).unwrap();
    crate::db::test_support::write_file(repo.join(".engram/usage.log"), b"{}\n").unwrap();
    assert!(monitor_gate(Some(&repo), false).is_ok());
    assert!(monitor_gate(Some(&repo), true).is_ok());
    let init = std::process::Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["init", "-q"])
        .status();
    if matches!(init, Ok(status) if status.success()) {
        let added = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["add", "-f", ".engram/usage.log"])
            .status()
            .unwrap();
        assert!(added.success());
        assert!(monitor_gate(Some(&repo), true).is_err());
    }
    crate::db::test_support::remove_tree(&dir);
}

#[cfg(unix)]
#[test]
fn monitor_gate_rejects_symlinked_journal_file() {
    let dir = crate::db::test_support::temp_root("monitor-gate-link");
    let repo = dir.join("repo");
    crate::db::test_support::make_dir_all(repo.join(".engram")).unwrap();
    crate::db::test_support::write_file(dir.join("outside.log"), b"{}\n").unwrap();
    std::os::unix::fs::symlink(dir.join("outside.log"), repo.join(".engram/usage.log")).unwrap();
    assert!(monitor_gate(Some(&repo), true).is_err());
    crate::db::test_support::remove_tree(&dir);
}

#[test]
fn resolve_monitor_log_path_explicit_root_returns_the_repo_journal() {
    let dir = crate::db::test_support::temp_root("monitor-log-path");
    let repo = dir.join("repo");
    crate::db::test_support::make_dir_all(repo.join(".engram")).unwrap();
    crate::db::test_support::write_file(repo.join(".engram/usage.log"), b"{}\n").unwrap();
    let (log, root) = resolve_monitor_log_path(Some(&repo), true).unwrap();
    assert_eq!(log, repo.join(".engram/usage.log"));
    assert_eq!(root.as_deref(), Some(repo.as_path()));
    crate::db::test_support::remove_tree(&dir);
}

/// I-16: one event-match predicate serves both modes, so a `--repo` filter
/// resolves an entry that carries only `repo` identically through the free
/// function and the TUI state.
#[test]
fn monitor_linear_and_tui_share_identical_event_filter() {
    let val = serde_json::json!({
        "tool": "callers", "exit_code": 3,
        "repo": "/workspace/engram",
    });
    // Linear mode and the TUI both match `--repo engram` via the basename
    // fallback (no `repo_name` on the entry).
    assert!(is_event_matched(&val, Some("engram"), false, false));
    let tui = TuiState::new(
        None,
        std::path::PathBuf::from("nonexistent-monitor-log"),
        Some("engram".to_string()),
        false,
        false,
    );
    assert!(tui.matches_filter(&val));

    // Errors-only rejects exit codes below 3 on both paths.
    let ok = serde_json::json!({
        "tool": "callers", "exit_code": 0, "repo": "/workspace/engram",
    });
    assert!(!is_event_matched(&ok, None, true, false));
    let tui_errors = TuiState::new(
        None,
        std::path::PathBuf::from("nonexistent-monitor-log"),
        None,
        true,
        false,
    );
    assert!(!tui_errors.matches_filter(&ok));
}

#[test]
fn global_monitor_hides_tmp_entries() {
    let scratch = serde_json::json!({
        "tool": "build", "exit_code": 0,
        "repo": "/tmp/eg-monitor-scratch", "repo_name": "eg-monitor-scratch",
    });
    // Global mode (exclude_ephemeral): invisible on both paths.
    assert!(!is_event_matched(&scratch, None, false, true));
    let tui_global = TuiState::new(
        None,
        std::path::PathBuf::from("nonexistent-monitor-log"),
        None,
        false,
        true,
    );
    assert!(!tui_global.matches_filter(&scratch));
    // Local mode: visible on both paths.
    assert!(is_event_matched(&scratch, None, false, false));
    let tui_local = TuiState::new(
        None,
        std::path::PathBuf::from("nonexistent-monitor-log"),
        None,
        false,
        false,
    );
    assert!(tui_local.matches_filter(&scratch));
}
