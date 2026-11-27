//! Query-name resolution: exact and `Class.method` suffix lookups over the
//! symbol table, with the module-disambiguation and dedupe phases shared by
//! every name-addressed entry point.

use super::*;

/// One symbol row for name resolution: (id, qualname, path, line, kind).
type CandidateRow = (i64, String, String, i64, String);

fn candidate_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<CandidateRow> {
    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
}

/// SELECT shared by `target_ids` statements; the column order is fixed by
/// `candidate_row`.
const CANDIDATE_SELECT: &str = "SELECT s.id, s.qualname, s.file_path, s.line_start, s.kind
     FROM symbols s";

/// Exact-identity predicate shared by the resolver (`target_ids`) and the
/// search pages: a symbol matches the query by bare name or full qualname,
/// bound as parameter ?1. Parenthesized so the `OR` cannot leak into a wider
/// `AND`.
pub(super) const EXACT_IDENTITY_PREDICATE: &str = "(s.name = ?1 OR s.qualname = ?1)";

/// Resolve the query name to target symbol ids. Multiple distinct symbols ->
/// exit 2 with a `path:line qualname` candidate list on stdout (CUJ4).
pub(super) fn target_ids(
    conn: &Connection,
    root: &Path,
    name: &str,
) -> Result<(Vec<i64>, Vec<NameCandidate>)> {
    // Agents paste the `path:line qualname` pointer strings this CLI prints.
    // Strip the locator and resolve the qualname; a bare `path:line` resolves
    // to the innermost symbol spanning that line.
    if let Some(qualname) = pointer_qualname(name) {
        return target_ids(conn, root, qualname);
    }
    if let Some(rows) = pointer_span_ids(conn, root, name)? {
        return Ok(disambiguate(rows));
    }
    let mut stmt = conn.prepare(&format!(
        "{CANDIDATE_SELECT}
         WHERE {EXACT_IDENTITY_PREDICATE}
         ORDER BY s.qualname, s.file_path, s.line_start",
    ))?;
    let rows = stmt.query_map(rusqlite::params![name], candidate_row)?;
    let mut all: Vec<_> = rows.collect::<std::result::Result<_, _>>()?;
    if all.is_empty() && name.contains('.') {
        all = suffix_candidates(conn, name)?;
    }
    Ok(disambiguate(all))
}

/// `src/main.rs:11 src.main` and `src/main.rs:11 function src.main` -> `"src.main"`.
/// A plain name (no space) is not a pointer.
fn pointer_qualname(name: &str) -> Option<&str> {
    let (locator, rest) = name.trim().split_once(' ')?;
    let (_, line) = locator.rsplit_once(':')?;
    line.trim().parse::<u64>().ok()?;
    let rest = rest.trim();
    let qualname = match rest.split_once(' ') {
        Some((kind, tail)) if SymbolKind::parse(kind).is_some() => tail.trim(),
        _ => rest,
    };
    (!qualname.is_empty()).then_some(qualname)
}

/// `src/main.rs:11` -> the innermost symbol spanning that line. An indexed
/// repo-relative or workspace-contained path is checked against `files.path`;
/// `None` falls through to normal name resolution so the original error text
/// survives.
fn pointer_span_ids(
    conn: &Connection,
    root: &Path,
    name: &str,
) -> Result<Option<Vec<CandidateRow>>> {
    let Some((raw_path, line_text)) = name.rsplit_once(':') else {
        return Ok(None);
    };
    let Ok(line) = line_text.trim().parse::<i64>() else {
        return Ok(None);
    };
    let norm = normalize_repo_path(root, raw_path);
    if norm.is_empty() || indexed_file_opt(conn, &norm)?.is_none() {
        return Ok(None);
    }
    let mut stmt = conn.prepare(
        "SELECT id, qualname, file_path, line_start, kind
         FROM symbols
         WHERE file_path = ?1 AND line_start <= ?2 AND line_end >= ?2
         ORDER BY CASE WHEN kind = 'module' THEN 1 ELSE 0 END,
                  (line_end - line_start) ASC,
                  line_start DESC,
                  qualname ASC
         LIMIT 1",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![norm, line], candidate_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok((!rows.is_empty()).then_some(rows))
}

/// `Class.method` suffix form: the full qualname is `pkg.Class.method`, so
/// an exact name/qualname match misses it. Narrow on `s.name = ?2` (using
/// `symbols_name`) and confirm the exact suffix in Rust (LIKE alone treats
/// `_` and `%` as wildcards). Multiple matches stay ambiguous through the
/// caller.
fn suffix_candidates(conn: &Connection, name: &str) -> Result<Vec<CandidateRow>> {
    let tail = name.rsplit_once('.').map(|(_, t)| t).unwrap_or(name);
    let pattern = format!("%.{name}");
    let mut stmt = conn.prepare(&format!(
        "{CANDIDATE_SELECT}
         WHERE s.qualname LIKE ?1 AND s.name = ?2
         ORDER BY s.qualname, s.file_path, s.line_start",
    ))?;
    let rows = stmt.query_map(rusqlite::params![&pattern, tail], candidate_row)?;
    let suffix = &pattern[1..];
    Ok(rows
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|r| r.1.ends_with(suffix))
        .collect())
}

/// Module-filter and dedupe phase: a module shares the bare name of
/// classes/functions (`button` vs `Button`); when a callable candidate exists,
/// module symbols must not create a false ambiguity (R7 F1). Unknown kinds
/// stay non-module, as before the enum.
fn disambiguate(all: Vec<CandidateRow>) -> (Vec<i64>, Vec<NameCandidate>) {
    let parsed: Vec<_> = all
        .into_iter()
        .map(|(id, qualname, path, line, kind)| {
            let is_mod = SymbolKind::parse(&kind) == Some(SymbolKind::Module);
            (id, qualname, path, line, is_mod)
        })
        .collect();
    let has_callable = parsed.iter().any(|r| !r.4);
    let mut ids = Vec::new();
    let mut cands = Vec::new();
    let mut seen_ids: std::collections::HashSet<i64> = Default::default();
    for (id, qualname, path, line, is_mod) in parsed {
        if has_callable && is_mod {
            continue;
        }
        // Dedupe by id: a symbol whose bare name equals its qualname matches
        // both WHERE branches and must not create a false ambiguity. O(1)
        // membership (R6f: a Vec::contains scan was quadratic).
        if seen_ids.insert(id) {
            ids.push(id);
            // Render cap only (R6f): the error carries `total` separately and
            // the caller prints at most MAX_AMBIGUOUS_CANDIDATES lines.
            if cands.len() < MAX_AMBIGUOUS_CANDIDATES {
                cands.push((path, line, qualname));
            }
        }
    }
    (ids, cands)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_qualname_trims_and_strips_kind() {
        assert_eq!(
            pointer_qualname("src/main.rs:11 src.main"),
            Some("src.main")
        );
        assert_eq!(
            pointer_qualname("  src/main.rs:11 function src.main  "),
            Some("src.main")
        );
        assert_eq!(
            pointer_qualname("src/a.py:5 method MyClass.run"),
            Some("MyClass.run")
        );
        assert_eq!(pointer_qualname("src/main.rs:11   "), None);
        assert_eq!(
            pointer_qualname("src/main.rs:11 function"),
            Some("function")
        );
        assert_eq!(pointer_qualname("plain_name"), None);
    }

    #[test]
    fn pointer_span_ids_picks_innermost_non_module_and_populates_cands() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE files (id INTEGER PRIMARY KEY, path TEXT NOT NULL);
             CREATE TABLE symbols (
                 id INTEGER PRIMARY KEY, kind TEXT, name TEXT, qualname TEXT,
                 file_id INTEGER, file_path TEXT, line_start INTEGER, line_end INTEGER
             );
             INSERT INTO files (id, path) VALUES (1, 'src/app.py');
             INSERT INTO symbols VALUES (1, 'module', 'app', 'src.app', 1, 'src/app.py', 1, 30);
             INSERT INTO symbols VALUES (2, 'class', 'App', 'src.app.App', 1, 'src/app.py', 3, 25);
             INSERT INTO symbols VALUES (3, 'method', 'run', 'src.app.App.run', 1, 'src/app.py', 5, 10);",
        )
        .unwrap();
        let root = std::path::Path::new(".");
        let (ids, cands) = target_ids(&conn, root, "./src/app.py:7").unwrap();
        assert_eq!(ids, vec![3]);
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].2, "src.app.App.run");

        // Outside any class/method: falls back to module rather than panicking.
        let (ids_mod, cands_mod) = target_ids(&conn, root, "src/app.py:28").unwrap();
        assert_eq!(ids_mod, vec![1]);
        assert_eq!(cands_mod[0].2, "src.app");
    }
}
