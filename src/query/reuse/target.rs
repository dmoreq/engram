//! Target grammar for `eg reuse`: repo-relative path, directory prefix,
//! `file:line`, and unique symbol names. Validation happens before the corpus
//! loads so an invalid target cannot emit partial candidates.

use std::collections::HashSet;

use super::status::{ResolvedScope, ScopeKind};
use super::*;
use crate::query::{NameCandidate, QueryCtx, MAX_AMBIGUOUS_CANDIDATES};

#[derive(Default)]
struct ScopeAcc {
    paths: Vec<String>,
    symbols: HashSet<String>,
    matched_files: HashSet<String>,
    has_dir: bool,
}

pub(super) fn resolve_scope(ctx: &QueryCtx<'_>, inputs: &[String]) -> Result<ResolvedScope> {
    let mut acc = ScopeAcc::default();

    for input in inputs {
        if input.is_empty() {
            return Err(EgError::Usage("path must be non-empty".to_string()));
        }
        // Relativize contained workspace paths before validation; external
        // absolute paths stay absolute and are rejected by `validate_input`.
        let relativized = crate::query::relativize_path(ctx.root, input);
        // Validate the raw input before lexical normalization: dropping empty
        // components would otherwise turn `/etc` into a relative path.
        validate_input(&relativized)?;
        let normalized = normalize_target(&relativized);
        validate_input(&normalized)?;
        if normalized == "." {
            if inputs.len() > 1 {
                return Err(EgError::Usage(
                    "repository root cannot be combined with other targets".to_string(),
                ));
            }
            return Ok(ResolvedScope {
                kind: ScopeKind::Repo,
                paths: Vec::new(),
                symbols: HashSet::new(),
                files: Vec::new(),
                path_matches: 0,
            });
        }
        if let Some((file, line)) = split_file_line(ctx, &normalized) {
            let meta = std::fs::symlink_metadata(ctx.root.join(&file))
                .map_err(|err| EgError::Internal(format!("stat {file}: {err}")))?;
            if meta.file_type().is_symlink() {
                return Err(unsafe_path(&normalized));
            }
            resolve_file_line(ctx, &normalized, &file, line, &mut acc)?;
            continue;
        }
        let abs = ctx.root.join(&normalized);
        match std::fs::symlink_metadata(&abs) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(unsafe_path(&normalized));
            }
            Ok(meta) if meta.is_file() => resolve_file(ctx, &normalized, &mut acc)?,
            Ok(meta) if meta.is_dir() => {
                acc.has_dir = true;
                resolve_dir(ctx, &normalized, &mut acc)?
            }
            Ok(_) => {
                return Err(EgError::Usage(format!(
                    "path must be a file or directory: {}",
                    super::sanitize(&normalized)
                )));
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                resolve_symbol(ctx, &normalized, &mut acc)?;
            }
            Err(err) => {
                return Err(EgError::Internal(format!(
                    "stat {}: {err}",
                    super::sanitize(&normalized)
                )));
            }
        }
    }

    if !acc.paths.is_empty() && !acc.symbols.is_empty() {
        return Err(EgError::Usage(
            "mixed path and symbol targets are not supported".to_string(),
        ));
    }
    let kind = if acc.paths.is_empty() && acc.symbols.is_empty() {
        ScopeKind::Repo
    } else if !acc.paths.is_empty() {
        if acc.has_dir {
            ScopeKind::Dir
        } else {
            ScopeKind::Files
        }
    } else {
        ScopeKind::Symbols
    };
    let mut files: Vec<String> = acc.matched_files.into_iter().collect();
    files.sort();
    let path_matches = files.len();
    Ok(ResolvedScope {
        kind,
        paths: acc.paths,
        symbols: acc.symbols,
        files,
        path_matches,
    })
}

/// Collapse dot and empty components so `.`, `./`, `src/.`, and `./src/`
/// all resolve to a stable repo-relative path. `..` is preserved for
/// `validate_input` to reject.
fn normalize_target(input: &str) -> String {
    let mut parts = Vec::new();
    for part in input.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        parts.push(part);
    }
    if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    }
}

fn validate_input(input: &str) -> Result<()> {
    if input.is_empty() {
        return Err(EgError::Usage("path must be non-empty".to_string()));
    }
    if input.starts_with('/') || input.contains('\\') || input.split('/').any(|part| part == "..") {
        return Err(unsafe_path(input));
    }
    Ok(())
}

fn unsafe_path(input: &str) -> EgError {
    EgError::Usage(format!(
        "path must be repo-relative and contained: {}",
        super::sanitize(input)
    ))
}

/// Split `<file>:<positive line>`. The prefix must exist on disk so a normal
/// filename containing a colon is not misread as a line target.
fn split_file_line(ctx: &QueryCtx<'_>, input: &str) -> Option<(String, i64)> {
    let (file, line) = input.rsplit_once(':')?;
    if file.is_empty() || !line.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let line: i64 = line.parse().ok()?;
    if line <= 0 {
        return None;
    }
    std::fs::metadata(ctx.root.join(file))
        .ok()
        .filter(|m| m.is_file())?;
    Some((file.to_string(), line))
}

fn resolve_file_line(
    ctx: &QueryCtx<'_>,
    input: &str,
    file: &str,
    line: i64,
    acc: &mut ScopeAcc,
) -> Result<()> {
    let indexed: i64 = ctx.conn.query_row(
        "SELECT COUNT(*) FROM files WHERE path = ?1",
        [file],
        |row| row.get(0),
    )?;
    if indexed == 0 {
        return Err(EgError::NoResults(format!(
            "path not found/not indexed: {}",
            super::sanitize(input)
        )));
    }
    let qualname: Option<String> = ctx
        .conn
        .query_row(
            "SELECT s.qualname FROM symbols s JOIN files f ON f.id = s.file_id
             WHERE f.path = ?1 AND s.is_test = 0 AND s.kind IN ('function', 'method')
               AND s.line_start <= ?2 AND s.line_end >= ?2
             ORDER BY (s.line_end - s.line_start), s.qualname LIMIT 1",
            rusqlite::params![file, line],
            |row| row.get(0),
        )
        .ok();
    let Some(qualname) = qualname else {
        return Err(EgError::NoResults(format!(
            "no symbol at {}",
            super::sanitize(input)
        )));
    };
    acc.symbols.insert(qualname);
    acc.matched_files.insert(file.to_string());
    Ok(())
}

fn resolve_file(ctx: &QueryCtx<'_>, input: &str, acc: &mut ScopeAcc) -> Result<()> {
    let indexed: i64 = ctx.conn.query_row(
        "SELECT COUNT(*) FROM files WHERE path = ?1",
        [input],
        |row| row.get(0),
    )?;
    if indexed == 0 {
        return Err(EgError::NoResults(format!(
            "path is not indexed: {}; run eg build",
            super::sanitize(input)
        )));
    }
    acc.paths.push(input.to_string());
    acc.matched_files.insert(input.to_string());
    Ok(())
}

fn resolve_dir(ctx: &QueryCtx<'_>, input: &str, acc: &mut ScopeAcc) -> Result<()> {
    let prefix = format!("{}/%", crate::db::like_escape(input));
    let mut stmt = ctx
        .conn
        .prepare("SELECT path FROM files WHERE path = ?1 OR path LIKE ?2 ESCAPE '\\'")?;
    let rows = stmt.query_map(rusqlite::params![input, prefix], |row| {
        row.get::<_, String>(0)
    })?;
    let mut found = 0usize;
    for row in rows {
        let path = row?;
        if path_matches(&path, &[input.to_string()]) {
            acc.matched_files.insert(path);
            found += 1;
        }
    }
    if found == 0 {
        return Err(EgError::NoResults(format!(
            "no indexed files under path: {}",
            super::sanitize(input)
        )));
    }
    acc.paths.push(input.to_string());
    Ok(())
}

fn resolve_symbol(ctx: &QueryCtx<'_>, input: &str, acc: &mut ScopeAcc) -> Result<()> {
    let mut exact_stmt = ctx.conn.prepare(
        "SELECT file_path, line_start, qualname FROM symbols
         WHERE is_test = 0 AND kind IN ('function', 'method')
           AND (qualname = ?1 OR name = ?1)
         ORDER BY file_path, line_start, qualname",
    )?;
    let exact_rows = exact_stmt.query_map([input], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut all: Vec<NameCandidate> = exact_rows.collect::<rusqlite::Result<Vec<_>>>()?;
    if all.is_empty() {
        let escaped = crate::db::like_escape(input);
        let module_suffix = format!("%::{escaped}");
        let dot_suffix = format!("%.{escaped}");
        let mut suffix_stmt = ctx.conn.prepare(
            "SELECT file_path, line_start, qualname FROM symbols
             WHERE is_test = 0 AND kind IN ('function', 'method')
               AND (qualname LIKE ?1 ESCAPE '\\' OR qualname LIKE ?2 ESCAPE '\\')
             ORDER BY file_path, line_start, qualname",
        )?;
        let suffix_rows =
            suffix_stmt.query_map(rusqlite::params![module_suffix, dot_suffix], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
        all = suffix_rows.collect::<rusqlite::Result<Vec<_>>>()?;
    }
    let total = all.len();
    match total {
        0 => {
            let detail = if input.contains('/') || input.contains('.') {
                format!("path not found: {}", super::sanitize(input))
            } else {
                format!("symbol not in index: {}", super::sanitize(input))
            };
            Err(EgError::NoResults(detail))
        }
        1 => {
            let (file, _, qualname) = all.into_iter().next().unwrap();
            acc.matched_files.insert(file);
            acc.symbols.insert(qualname);
            Ok(())
        }
        _ => {
            let candidates = all
                .into_iter()
                .take(MAX_AMBIGUOUS_CANDIDATES)
                .map(|(path, line, qualname)| {
                    (super::sanitize(&path), line, super::sanitize(&qualname))
                })
                .collect();
            Err(EgError::Ambiguous {
                name: super::sanitize(input),
                candidates,
                total,
                role: None,
            })
        }
    }
}

/// A path is in scope when it equals a target file or sits below a target
/// directory. The boundary check keeps `src/query` from matching `src/query2`.
pub(super) fn path_matches(path: &str, targets: &[String]) -> bool {
    targets.iter().any(|target| {
        path == target
            || path
                .strip_prefix(target)
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{make_dir_all, remove_tree, temp_root, write_file};

    #[test]
    fn resolve_scope_relativizes_contained_absolute_path_and_rejects_external() {
        let root = temp_root("reuse-abs-scope");
        make_dir_all(root.join("src")).unwrap();
        write_file(root.join("src/a.py"), b"def f(): pass\n").unwrap();
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE files (id INTEGER PRIMARY KEY, path TEXT NOT NULL);
             INSERT INTO files (id, path) VALUES (1, 'src/a.py');",
        )
        .unwrap();
        let ctx = QueryCtx {
            conn: &conn,
            root: &root,
        };
        let abs = root.join("src/a.py").to_string_lossy().into_owned();
        let scope = resolve_scope(&ctx, &[abs]).unwrap();
        assert_eq!(scope.paths, vec!["src/a.py"]);

        let ext_err = resolve_scope(&ctx, &["/etc/passwd".to_string()])
            .err()
            .unwrap();
        assert_eq!(ext_err.exit_code(), 2);
        remove_tree(&root);
    }
}
