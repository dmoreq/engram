//! Write-time ref hygiene for `mem write`: normalizes repo-internal absolute
//! paths into relative paths and blocks transient, gitignored, external, or
//! symlink-escaping refs on `durable` notes.

use std::path::{Component, Path, PathBuf};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use rusqlite::Connection;

use crate::error::{EgError, Result};
use crate::memory::MemScope;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HygieneOutcome {
    pub refs: Option<String>,
    pub changed: bool,
    pub normalized_count: usize,
    pub warnings: Vec<String>,
}

fn hygiene_disabled() -> bool {
    std::env::var("ENGRAM_MEM_HYGIENE").is_ok_and(|v| v == "0")
}

fn reject_hygiene(reason: &'static str, detail: String) -> EgError {
    EgError::Hygiene { reason, detail }
}

fn is_unsafe_ref_token(tok: &str) -> bool {
    tok.contains('\0')
        || tok.contains('\\')
        || tok.starts_with('-')
        || tok.starts_with(":(")
        || Path::new(tok)
            .components()
            .any(|c| matches!(c, Component::ParentDir))
}

fn split_path_and_suffix(tok: &str) -> (&str, &str) {
    let clean = crate::db::split_line_suffix(tok);
    (clean, &tok[clean.len()..])
}

fn is_symbol_ref(clean: &str, index_conn: Option<&Connection>) -> bool {
    if clean.contains("::") {
        return true;
    }
    if clean.contains('/') || Path::new(clean).is_absolute() {
        return false;
    }
    index_conn.is_some_and(|iconn| crate::db::lookup_symbol_ref(iconn, clean).is_some())
}

fn has_symlink_along(base: &Path, rel: &Path) -> bool {
    let mut cur = base.to_path_buf();
    for comp in rel.components() {
        cur.push(comp);
        match std::fs::symlink_metadata(&cur) {
            Ok(meta) if meta.file_type().is_symlink() => return true,
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(_) => return true,
        }
    }
    false
}

fn normalize_contained_path(
    clean: &str,
    root: &Path,
    root_canon: &Path,
) -> std::result::Result<(String, bool), (&'static str, String)> {
    let p = Path::new(clean);
    if p.is_absolute() {
        let mut ancestor = p;
        let mut tail_parts: Vec<&std::ffi::OsStr> = Vec::new();
        while !ancestor.exists() {
            let Some(name) = ancestor.file_name() else {
                break;
            };
            let Some(parent) = ancestor.parent() else {
                break;
            };
            tail_parts.push(name);
            ancestor = parent;
        }
        let and_canon = std::fs::canonicalize(ancestor).map_err(|_| {
            (
                "external_path",
                format!("absolute ref '{clean}' could not be resolved inside repository"),
            )
        })?;
        if !and_canon.starts_with(root_canon) {
            return Err((
                "external_path",
                format!(
                    "durable ref '{clean}' points outside repository root (durable is default; use --scope session or --confirm)"
                ),
            ));
        }
        let rel_and = and_canon.strip_prefix(root_canon).unwrap_or(Path::new(""));
        let mut rel = rel_and.to_path_buf();
        for part in tail_parts.iter().rev() {
            rel.push(part);
        }
        if has_symlink_along(root, &rel) {
            return Err((
                "symlink_escape",
                format!("ref '{clean}' traverses a symlink inside repository"),
            ));
        }
        let rel_str = rel.to_string_lossy().into_owned();
        if rel_str.is_empty() {
            return Err((
                "external_path",
                format!("ref '{clean}' resolves to repository root itself"),
            ));
        }
        Ok((rel_str, true))
    } else {
        if has_symlink_along(root, p) {
            return Err((
                "symlink_escape",
                format!("ref '{clean}' traverses a symlink"),
            ));
        }
        Ok((clean.to_string(), false))
    }
}

fn build_gitignore(root: &Path, rel: &Path) -> Option<Gitignore> {
    let mut builder = GitignoreBuilder::new(root);
    let root_gi = root.join(".gitignore");
    if root_gi.is_file() {
        let _ = builder.add(&root_gi);
    }
    let exclude = root.join(".git/info/exclude");
    if exclude.is_file() {
        let _ = builder.add(&exclude);
    }
    let mut cur = root.to_path_buf();
    if let Some(parent) = rel.parent() {
        for comp in parent.components() {
            cur.push(comp);
            let gi = cur.join(".gitignore");
            if gi.is_file() {
                let _ = builder.add(&gi);
            }
        }
    }
    builder.build().ok()
}

fn is_gitignored(root: &Path, rel_str: &str) -> bool {
    let rel = Path::new(rel_str);
    let Some(gi) = build_gitignore(root, rel) else {
        return false;
    };
    let full = root.join(rel);
    if let Ok(meta) = std::fs::symlink_metadata(&full) {
        return gi.matched(rel, meta.is_dir()).is_ignore();
    }
    if gi.matched(rel, false).is_ignore() {
        return true;
    }
    let mut and = PathBuf::new();
    if let Some(parent) = rel.parent() {
        for comp in parent.components() {
            and.push(comp);
            if gi.matched(&and, true).is_ignore() {
                return true;
            }
        }
    }
    false
}

fn strip_cur_dir(p: &str) -> String {
    let mut out = PathBuf::new();
    for c in Path::new(p).components() {
        if !matches!(c, Component::CurDir) {
            out.push(c);
        }
    }
    out.to_string_lossy().into_owned()
}

fn check_durable_path_policy(
    root: Option<&Path>,
    rel_str: &str,
) -> std::result::Result<(), (&'static str, String)> {
    let normalized = strip_cur_dir(rel_str);
    let mut comps = Path::new(&normalized).components();
    if let Some(Component::Normal(first)) = comps.next() {
        if first == ".tmp" {
            return Err((
                "tmp_path",
                format!(
                    "durable ref '{rel_str}' points to '.tmp/' (durable is default; use --scope session or --confirm)"
                ),
            ));
        }
        if first == ".engram" {
            return Err((
                "internal_store_path",
                format!(
                    "durable ref '{rel_str}' points to '.engram/' (durable is default; use --scope session or --confirm)"
                ),
            ));
        }
    }
    if let Some(r) = root {
        if is_gitignored(r, &normalized) {
            return Err((
                "gitignored_path",
                format!(
                    "durable ref '{rel_str}' matches .gitignore (durable is default; use --scope session or --confirm)"
                ),
            ));
        }
    }
    Ok(())
}

/// Validate and normalize `--refs` for `mem write`.
pub fn validate_and_normalize_refs(
    refs: Option<&str>,
    root: Option<&Path>,
    index_conn: Option<&Connection>,
    scope: MemScope,
    global: bool,
    confirm: bool,
) -> Result<HygieneOutcome> {
    let Some(raw) = refs else {
        return Ok(HygieneOutcome {
            refs: None,
            changed: false,
            normalized_count: 0,
            warnings: Vec::new(),
        });
    };
    let tokens: Vec<&str> = raw.split_whitespace().collect();
    if tokens.is_empty() {
        return Ok(HygieneOutcome {
            refs: Some(String::new()),
            changed: false,
            normalized_count: 0,
            warnings: Vec::new(),
        });
    }

    let disabled = hygiene_disabled();
    let root_canon = root.and_then(|r| std::fs::canonicalize(r).ok());
    let mut out_tokens: Vec<String> = Vec::with_capacity(tokens.len());
    let mut normalized_count = 0usize;
    let mut warnings: Vec<String> = Vec::new();

    let mut handle_policy_err = |reason: &'static str, msg: String| -> Result<()> {
        if disabled {
            warnings.push(format!("eg: warning: hygiene ({reason}): {msg}"));
            Ok(())
        } else {
            Err(reject_hygiene(reason, msg))
        }
    };

    for tok in tokens {
        if is_unsafe_ref_token(tok) {
            return Err(reject_hygiene(
                "invalid_ref_token",
                format!("unsafe ref token '{tok}'"),
            ));
        }
        let (clean, suffix) = split_path_and_suffix(tok);
        if global {
            if scope == MemScope::Durable && !confirm && Path::new(clean).is_absolute() {
                handle_policy_err(
                    "external_path",
                    format!(
                        "global durable ref '{tok}' uses an absolute path (durable is default; use --scope session or --confirm)"
                    ),
                )?;
            }
            out_tokens.push(tok.to_string());
            continue;
        }
        if is_symbol_ref(clean, index_conn) {
            out_tokens.push(tok.to_string());
            continue;
        }

        let rel_clean = match (root, root_canon.as_deref()) {
            (Some(r), Some(rc)) => match normalize_contained_path(clean, r, rc) {
                Ok((norm, was_changed)) => {
                    if was_changed {
                        normalized_count += 1;
                    }
                    norm
                }
                Err(("symlink_escape", msg)) => return Err(reject_hygiene("symlink_escape", msg)),
                Err((reason, msg)) => {
                    if scope == MemScope::Durable && !confirm {
                        handle_policy_err(reason, msg)?;
                    }
                    clean.to_string()
                }
            },
            _ => {
                if Path::new(clean).is_absolute() && scope == MemScope::Durable && !confirm {
                    handle_policy_err(
                        "external_path",
                        format!(
                            "durable ref '{tok}' is an absolute path outside a repository (use --scope session or --confirm)"
                        ),
                    )?;
                }
                clean.to_string()
            }
        };

        if scope == MemScope::Durable && !confirm && !Path::new(&rel_clean).is_absolute() {
            if let Err((reason, msg)) = check_durable_path_policy(root, &rel_clean) {
                handle_policy_err(reason, msg)?;
            }
        }
        out_tokens.push(format!("{rel_clean}{suffix}"));
    }

    Ok(HygieneOutcome {
        refs: Some(out_tokens.join(" ")),
        changed: normalized_count > 0,
        normalized_count,
        warnings,
    })
}

#[cfg(test)]
mod tests;
