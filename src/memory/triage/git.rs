//! Bounded read-only git lineage enrichment for `mem_triage`.
//! Self-contained subprocess runner preserving `PATH`, stripping `GIT_*`,
//! disabling global/system git configs and hooks, and sanitizing all output.

use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::path::{Component, Path};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::{CommitContext, GitStats, TriageItem, TriageReason};
use crate::query::sanitize;

#[derive(Debug, Clone, Copy)]
pub struct GitBudget {
    pub max_refs: usize,
    pub per_cmd_timeout_ms: u64,
    pub total_budget_ms: u64,
    pub max_stdout_bytes: usize,
    pub max_siblings: usize,
}

impl Default for GitBudget {
    fn default() -> Self {
        Self {
            max_refs: 10,
            per_cmd_timeout_ms: 250,
            total_budget_ms: 1500,
            max_stdout_bytes: 65_536,
            max_siblings: 5,
        }
    }
}

enum GitRunOutcome {
    Ok(Vec<u8>),
    TimedOut,
    Failed,
}

#[cfg(unix)]
fn set_process_group(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
}

#[cfg(not(unix))]
fn set_process_group(_cmd: &mut Command) {}

#[cfg(unix)]
fn kill_group(child: &mut std::process::Child) {
    let pid = child.id().to_string();
    let _ = Command::new("kill")
        .args(["-9", &format!("-{pid}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = child.kill();
}

#[cfg(not(unix))]
fn kill_group(child: &mut std::process::Child) {
    let _ = child.kill();
}

fn run_git_bounded(root: &Path, args: &[&str], timeout_ms: u64, max_bytes: usize) -> GitRunOutcome {
    let mut cmd = Command::new("git");
    cmd.current_dir(root)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("GIT_") {
            cmd.env_remove(&k);
        }
    }
    cmd.env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0");
    set_process_group(&mut cmd);

    let Ok(mut child) = cmd.spawn() else {
        return GitRunOutcome::Failed;
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = child.wait();
        return GitRunOutcome::Failed;
    };

    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.take(max_bytes as u64 + 1).read_to_end(&mut buf);
        buf
    });

    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let buf = reader.join().unwrap_or_default();
                if !status.success() || buf.len() > max_bytes {
                    return GitRunOutcome::Failed;
                }
                return GitRunOutcome::Ok(buf);
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    kill_group(&mut child);
                    let _ = child.wait();
                    let _ = reader.join();
                    return GitRunOutcome::TimedOut;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => {
                kill_group(&mut child);
                let _ = child.wait();
                let _ = reader.join();
                return GitRunOutcome::Failed;
            }
        }
    }
}

fn is_safe_git_path(p: &str) -> bool {
    !p.is_empty()
        && !p.contains('\0')
        && !p.contains('\\')
        && !p.starts_with('-')
        && !p.starts_with(":(")
        && !Path::new(p)
            .components()
            .any(|c| matches!(c, Component::ParentDir))
}

fn sanitize_subject(raw: &[u8]) -> String {
    let s = sanitize(&String::from_utf8_lossy(raw));
    let trimmed = s.trim();
    if trimmed.chars().count() > 200 {
        let mut out: String = trimmed.chars().take(197).collect();
        out.push_str("...");
        out
    } else {
        trimmed.to_string()
    }
}

fn sanitize_path(raw: &[u8]) -> String {
    sanitize(&String::from_utf8_lossy(raw)).trim().to_string()
}

fn working_tree_renames(
    root: &Path,
    budget: GitBudget,
    stats: &mut GitStats,
) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let out = run_git_bounded(
        root,
        &[
            "--no-pager",
            "-c",
            "core.fsmonitor=false",
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=no",
        ],
        budget.per_cmd_timeout_ms,
        budget.max_stdout_bytes,
    );
    let bytes = match out {
        GitRunOutcome::Ok(b) => b,
        GitRunOutcome::TimedOut => {
            stats.timeouts += 1;
            stats.degraded = true;
            return map;
        }
        GitRunOutcome::Failed => {
            stats.failed += 1;
            return map;
        }
    };
    let parts: Vec<&[u8]> = bytes.split(|&b| b == 0).collect();
    let mut i = 0;
    while i < parts.len() {
        let rec = parts[i];
        if rec.len() >= 4 && (rec[0] == b'R' || rec[1] == b'R' || rec[0] == b'C' || rec[1] == b'C')
        {
            let is_rename = rec[0] == b'R' || rec[1] == b'R';
            let new_p = sanitize_path(&rec[3..]);
            if i + 1 < parts.len() {
                let old_p = sanitize_path(parts[i + 1]);
                if is_rename
                    && is_safe_git_path(&old_p)
                    && is_safe_git_path(&new_p)
                    && root.join(&new_p).exists()
                {
                    map.insert(old_p, new_p);
                }
                i += 2;
                continue;
            }
        }
        i += 1;
    }
    map
}

#[derive(Clone)]
enum PathLineage {
    Renamed {
        sha: String,
        subject: String,
        target: String,
    },
    Split {
        sha: String,
        subject: String,
        siblings: Vec<String>,
    },
    DeletedNoSiblings,
}

fn probe_deleted_or_renamed(
    root: &Path,
    path: &str,
    budget: GitBudget,
    stats: &mut GitStats,
) -> Option<PathLineage> {
    stats.attempted += 1;
    let out = run_git_bounded(
        root,
        &[
            "--no-pager",
            "-c",
            "core.fsmonitor=false",
            "--literal-pathspecs",
            "log",
            "--no-ext-diff",
            "--no-textconv",
            "--no-show-signature",
            "-n",
            "1",
            "--no-renames",
            "--diff-filter=D",
            "-z",
            "--format=%H%x00%s",
            "--",
            path,
        ],
        budget.per_cmd_timeout_ms,
        budget.max_stdout_bytes,
    );
    let bytes = match out {
        GitRunOutcome::Ok(b) => b,
        GitRunOutcome::TimedOut => {
            stats.timeouts += 1;
            stats.degraded = true;
            return None;
        }
        GitRunOutcome::Failed => {
            stats.failed += 1;
            return None;
        }
    };
    let parts: Vec<&[u8]> = bytes.split(|&b| b == 0).filter(|p| !p.is_empty()).collect();
    if parts.len() < 2 {
        return None;
    }
    let sha_full = String::from_utf8_lossy(parts[0]).trim().to_string();
    let sha: String = sha_full.chars().take(7).collect();
    let subject = sanitize_subject(parts[1]);
    probe_commit_changes(root, &sha_full, &sha, &subject, path, budget, stats)
}

fn probe_commit_changes(
    root: &Path,
    sha_full: &str,
    sha_short: &str,
    subject: &str,
    deleted_path: &str,
    budget: GitBudget,
    stats: &mut GitStats,
) -> Option<PathLineage> {
    let out = run_git_bounded(
        root,
        &[
            "--no-pager",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "diff.renameLimit=100",
            "show",
            "--no-ext-diff",
            "--no-textconv",
            "--no-show-signature",
            "-M",
            "--name-status",
            "-z",
            "--diff-filter=AR",
            "--format=",
            sha_full,
        ],
        budget.per_cmd_timeout_ms,
        budget.max_stdout_bytes,
    );
    let bytes = match out {
        GitRunOutcome::Ok(b) => b,
        GitRunOutcome::TimedOut => {
            stats.timeouts += 1;
            stats.degraded = true;
            return None;
        }
        GitRunOutcome::Failed => {
            stats.failed += 1;
            return None;
        }
    };
    let parent_dir = Path::new(deleted_path)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let parts: Vec<&[u8]> = bytes.split(|&b| b == 0).filter(|p| !p.is_empty()).collect();
    let mut same_dir: Vec<String> = Vec::new();
    let mut i = 0;
    while i < parts.len() {
        let st = String::from_utf8_lossy(parts[i]);
        if st.starts_with('R') && i + 2 < parts.len() {
            let old_p = sanitize_path(parts[i + 1]);
            let new_p = sanitize_path(parts[i + 2]);
            if old_p == deleted_path && is_safe_git_path(&new_p) && root.join(&new_p).exists() {
                return Some(PathLineage::Renamed {
                    sha: sha_short.to_string(),
                    subject: subject.to_string(),
                    target: new_p,
                });
            }
            if is_safe_git_path(&new_p)
                && root.join(&new_p).exists()
                && Path::new(&new_p)
                    .parent()
                    .is_some_and(|d| d.to_string_lossy() == parent_dir)
            {
                same_dir.push(new_p);
            }
            i += 3;
            continue;
        }
        if st == "A" && i + 1 < parts.len() {
            let p = sanitize_path(parts[i + 1]);
            if is_safe_git_path(&p)
                && root.join(&p).exists()
                && Path::new(&p)
                    .parent()
                    .is_some_and(|d| d.to_string_lossy() == parent_dir)
            {
                same_dir.push(p);
            }
            i += 2;
            continue;
        }
        i += 1;
    }
    same_dir.sort();
    same_dir.dedup();
    same_dir.truncate(budget.max_siblings);
    if same_dir.is_empty() {
        Some(PathLineage::DeletedNoSiblings)
    } else {
        Some(PathLineage::Split {
            sha: sha_short.to_string(),
            subject: subject.to_string(),
            siblings: same_dir,
        })
    }
}

pub fn enrich_with_git(
    root: Option<&Path>,
    items: &mut [TriageItem],
    no_git: bool,
    budget: GitBudget,
) -> GitStats {
    let mut stats = GitStats::default();
    let env_disabled = std::env::var("ENGRAM_TRIAGE_GIT").is_ok_and(|v| v == "0");
    let Some(repo) = root else {
        stats.disabled = true;
        return stats;
    };
    if no_git || env_disabled || !repo.join(".git").exists() {
        stats.disabled = true;
        return stats;
    }
    let start = Instant::now();
    let wt_renames = working_tree_renames(repo, budget, &mut stats);

    let mut unique_paths: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for item in items.iter() {
        for br in &item.broken_refs {
            if matches!(
                br.failure_kind,
                "missing_code_path" | "missing_doc_or_report"
            ) {
                let clean = crate::db::split_line_suffix(&br.token).to_string();
                if is_safe_git_path(&clean) && seen.insert(clean.clone()) {
                    unique_paths.push(clean);
                }
            }
        }
    }

    let mut cache: BTreeMap<String, Option<PathLineage>> = BTreeMap::new();
    for p in unique_paths {
        if wt_renames.contains_key(&p) {
            continue;
        }
        if cache.len() >= budget.max_refs
            || start.elapsed() >= Duration::from_millis(budget.total_budget_ms)
        {
            stats.capped += 1;
            continue;
        }
        let lin = probe_deleted_or_renamed(repo, &p, budget, &mut stats);
        cache.insert(p, lin);
    }

    for item in items.iter_mut() {
        let mut best: Option<(u8, TriageReason, CommitContext)> = None;
        for br in &item.broken_refs {
            let clean = crate::db::split_line_suffix(&br.token);
            if let Some(target) = wt_renames.get(clean) {
                let ctx = CommitContext {
                    sha: "working-tree".into(),
                    subject: "uncommitted rename".into(),
                    sibling_files: vec![target.clone()],
                };
                best = Some((3, TriageReason::UncommittedMove, ctx));
                break;
            }
            if let Some(Some(lin)) = cache.get(clean) {
                match lin {
                    PathLineage::Renamed {
                        sha,
                        subject,
                        target,
                    } => {
                        let rank = 2;
                        if best.as_ref().is_none_or(|b| rank > b.0) {
                            best = Some((
                                rank,
                                TriageReason::Renamed1To1,
                                CommitContext {
                                    sha: sha.clone(),
                                    subject: subject.clone(),
                                    sibling_files: vec![target.clone()],
                                },
                            ));
                        }
                    }
                    PathLineage::Split {
                        sha,
                        subject,
                        siblings,
                    } => {
                        let rank = 1;
                        if best.as_ref().is_none_or(|b| rank > b.0) {
                            best = Some((
                                rank,
                                TriageReason::FileSplitSameCommit,
                                CommitContext {
                                    sha: sha.clone(),
                                    subject: subject.clone(),
                                    sibling_files: siblings.clone(),
                                },
                            ));
                        }
                    }
                    PathLineage::DeletedNoSiblings => {}
                }
            }
        }
        if let Some((_, reason, ctx)) = best {
            item.triage_reason = reason;
            item.triage_label = reason.label();
            item.suggested_action = reason.action();
            item.commit_context = Some(ctx);
        }
    }

    stats.duration_ms = start.elapsed().as_millis() as u64;
    stats
}

#[cfg(test)]
mod tests;
