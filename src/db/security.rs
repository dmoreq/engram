//! Confinement hardening: owner-only POSIX modes and the git-provenance gate
//! for repo-local `.engram/` state. No write primitives live here — directory
//! creation stays in `db.rs`, and this module only adjusts modes and inspects
//! git-tracked status.

use super::*;

/// Restrict a confinement directory to its owner (`0700`) on Unix: the global
/// store holds private notes and telemetry, so a permissive umask must not
/// expose it to other local accounts.
#[cfg(unix)]
pub(crate) fn harden_dir_mode(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn harden_dir_mode(_: &Path) -> Result<()> {
    Ok(())
}

/// Restrict one confinement file (`0600`) on Unix: `usage.log` and `memory.db`
/// carry repository paths and agent-authored notes.
#[cfg(unix)]
pub(crate) fn harden_file_mode(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn harden_file_mode(_: &Path) -> Result<()> {
    Ok(())
}

/// Reject a symlinked path before any write or open: a symlink can redirect
/// every later operation outside the intended confinement root.
pub(crate) fn reject_symlink(path: &Path, label: &str) -> Result<()> {
    if let Ok(meta) = fs::symlink_metadata(path) {
        if meta.file_type().is_symlink() {
            return Err(EgError::Internal(format!(
                "refusing symlinked {label}: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

/// Reject a symlinked or non-regular journal file. `File::open` follows a
/// symlink; local usage/dashboard/monitor reads must not render another
/// repository's journal through one.
pub(crate) fn reject_unsafe_journal(root: &Path) -> Result<()> {
    let dir = eg_dir(root);
    reject_symlink(&dir, ".engram directory")?;
    for name in ["usage.log", "usage.log.1"] {
        let path = dir.join(name);
        if let Ok(meta) = fs::symlink_metadata(&path) {
            if meta.file_type().is_symlink() || !meta.is_file() {
                return Err(EgError::Internal(format!(
                    "refusing unsafe journal file: {}",
                    path.display()
                )));
            }
        }
    }
    reject_tracked_eg_dir(root)
}

/// Create or validate the machine-wide confinement directory and keep it
/// owner-only. The repo-local `.engram/` is left at the project's own mode:
/// several developers can share one working tree.
pub(crate) fn ensure_global_dir(path: &Path) -> Result<()> {
    ensure_dir_validated(path, "global .engram directory", true)?;
    harden_dir_mode(path)
}

/// Provenance gate for repo-local state. A repository that commits `.engram/`
/// hands every querying agent attacker-authored memories and index rows, so a
/// tracked confinement directory is rejected (exit 2) instead of trusted.
///
/// Uses a short-lived process-local cache (5s TTL, 64-entry cap) keyed by
/// canonicalized `root` and invalidated by `.git/index` mtime so `git add`,
/// checkout, or a branch switch takes effect immediately without spawning `git`
/// on every query. A failed probe still means "not tracked" (a broken git must
/// not brick local queries).
pub(crate) fn reject_tracked_eg_dir(root: &Path) -> Result<()> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};

    const TTL: Duration = Duration::from_secs(5);
    const MAX_ENTRIES: usize = 64;
    type CacheEntry = (Instant, Option<i64>, bool);
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, CacheEntry>>> = OnceLock::new();

    let dir = eg_dir(root);
    if !dir.exists() {
        return Ok(());
    }

    let key = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let idx_mtime = stat_mtime(&key.join(".git/index"));
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(guard) = cache.lock() {
        if let Some((at, cached_mtime, tracked)) = guard.get(&key) {
            if at.elapsed() < TTL && *cached_mtime == idx_mtime {
                return if *tracked {
                    Err(EgError::Security(tracked_detail()))
                } else {
                    Ok(())
                };
            }
        }
    }

    let tracked = git_tracked_engram(root).is_some();
    if let Ok(mut guard) = cache.lock() {
        if guard.len() >= MAX_ENTRIES && !guard.contains_key(&key) {
            guard.retain(|_, (at, _, _)| at.elapsed() < TTL);
            if guard.len() >= MAX_ENTRIES {
                guard.clear();
            }
        }
        guard.insert(key, (Instant::now(), idx_mtime, tracked));
    }

    if tracked {
        Err(EgError::Security(tracked_detail()))
    } else {
        Ok(())
    }
}

/// `git ls-files` over the confinement directory. The `:(icase)` pathspec is
/// required on case-insensitive filesystems: a tracked `.ENGRAM/index.db`
/// resolves to the same directory as `.engram/`, but a case-sensitive
/// pathspec returns no rows and would let the planted index through.
/// No git, no repository, or a failing git means "not tracked".
///
/// Every `GIT_*` variable is stripped from the child: a caller that already
/// runs inside a git context (a hook with `GIT_DIR` set, a nested checkout)
/// would otherwise hijack this probe and attribute another repository's
/// index to `root`, rejecting unrelated directories as tracked.
fn git_tracked_engram(root: &Path) -> Option<String> {
    let mut command = std::process::Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .args(["ls-files", "-z", "--", ":(icase).engram"]);
    for key in std::env::vars().map(|(k, _)| k).collect::<Vec<_>>() {
        if key.starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command.env("GIT_OPTIONAL_LOCKS", "0");
    let out = command.output().ok()?;
    if !out.status.success() || out.stdout.is_empty() {
        return None;
    }
    // `:(icase)` also matches an unrelated `.ENGRAM/` on a case-sensitive
    // filesystem. Confirm a case variant resolves to the same directory as
    // `.engram/` before rejecting it.
    let canonical_eg = fs::canonicalize(eg_dir(root)).ok();
    for entry in out
        .stdout
        .split(|byte| *byte == 0)
        .filter(|p| !p.is_empty())
    {
        let first = entry.split(|byte| *byte == b'/').next().unwrap_or(entry);
        if first == b".engram" {
            return Some(tracked_detail());
        }
        if first.eq_ignore_ascii_case(b".engram") {
            let variant = root.join(String::from_utf8_lossy(first).as_ref());
            if canonical_eg.is_some() && fs::canonicalize(&variant).ok() == canonical_eg {
                return Some(tracked_detail());
            }
        }
    }
    None
}

fn tracked_detail() -> String {
    ".engram/ contains git-tracked files — untrack and remove to prevent malicious memory injection"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{make_dir_all, remove_tree, stripped_git, temp_root, write_file};

    #[test]
    fn tracked_cache_hits_and_invalidates_when_git_index_changes() {
        let dir = temp_root("sec-cache");
        make_dir_all(dir.join(".engram")).unwrap();
        write_file(dir.join(".engram/index.db"), b"x").unwrap();
        let init = stripped_git()
            .arg("-C")
            .arg(&dir)
            .args(["init", "-q"])
            .status();
        if !matches!(init, Ok(status) if status.success()) {
            remove_tree(&dir);
            return;
        }
        reject_tracked_eg_dir(&dir).expect("initial untracked check");
        reject_tracked_eg_dir(&dir).expect("cached untracked check");

        let add = stripped_git()
            .arg("-C")
            .arg(&dir)
            .args(["add", "-f", ".engram/index.db"])
            .status()
            .unwrap();
        assert!(add.success());
        assert!(matches!(
            reject_tracked_eg_dir(&dir),
            Err(EgError::Security(_))
        ));
        remove_tree(&dir);
    }
}
