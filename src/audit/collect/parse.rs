use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::{Entry, LineOutcome};

fn opt_nonneg(v: &serde_json::Value, key: &str) -> Option<Option<i64>> {
    match v.get(key) {
        None => Some(None),
        Some(x) => x.as_i64().filter(|&n| n >= 0).map(Some),
    }
}

fn validate_repo_scope(
    v: &serde_json::Value,
    local_root: Option<&Path>,
    local_root_canon: Option<&Path>,
    same_root_cache: &mut HashMap<String, bool>,
) -> std::result::Result<PathBuf, LineOutcome> {
    let Some(repo_raw) = v
        .get("repo")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
    else {
        return Err(LineOutcome::DeadRepo);
    };
    if !crate::journal::liveness::repo_is_live(Some(repo_raw))
        || (local_root.is_none() && crate::journal::liveness::repo_is_ephemeral(repo_raw))
    {
        return Err(LineOutcome::DeadRepo);
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
            return Err(LineOutcome::OutsideScope);
        }
    }
    Ok(repo)
}

pub(super) fn parse_line(
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
    let repo = match validate_repo_scope(&v, local_root, local_root_canon, same_root_cache) {
        Ok(r) => r,
        Err(outcome) => return outcome,
    };
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

#[cfg(test)]
pub(super) fn canonical_tool(tool: &str) -> String {
    canonical_tool_inner(tool)
}

#[cfg(not(test))]
fn canonical_tool(tool: &str) -> String {
    canonical_tool_inner(tool)
}

fn canonical_tool_inner(tool: &str) -> String {
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
