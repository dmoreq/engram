//! Query-name resolution: exact and `Class.method` suffix lookups over the
//! symbol table, with the module-disambiguation and dedupe phases shared by
//! every name-addressed entry point.

use super::*;

/// One symbol row for name resolution: (id, qualname, path, line, kind).
type CandidateRow = (i64, String, String, i64, String);

fn candidate_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<CandidateRow> {
    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
}

/// SELECT/JOIN shared by both `target_ids` statements; the column order is
/// fixed by `candidate_row`.
const CANDIDATE_SELECT: &str = "SELECT s.id, s.qualname, f.path, s.line_start, s.kind
     FROM symbols s JOIN files f ON f.id = s.file_id";

/// Exact-identity predicate shared by the resolver (`target_ids`) and the
/// search pages: a symbol matches the query by bare name or full qualname,
/// bound as parameter ?1. Parenthesized so the `OR` cannot leak into a wider
/// `AND`.
pub(super) const EXACT_IDENTITY_PREDICATE: &str = "(s.name = ?1 OR s.qualname = ?1)";

/// Resolve the query name to target symbol ids. Multiple distinct symbols ->
/// exit 2 with a `path:line qualname` candidate list on stdout (CUJ4).
pub(super) fn target_ids(conn: &Connection, name: &str) -> Result<(Vec<i64>, Vec<NameCandidate>)> {
    // Agents paste the `path:line qualname` pointer strings this CLI prints.
    // Strip the locator and resolve the qualname; a bare `path:line` resolves
    // to the symbol spanning that line.
    if let Some(qualname) = pointer_qualname(name) {
        return target_ids(conn, qualname);
    }
    if let Some(ids) = pointer_span_ids(conn, name)? {
        return Ok((ids, Vec::new()));
    }
    let mut stmt = conn.prepare(&format!(
        "{CANDIDATE_SELECT}
         WHERE {EXACT_IDENTITY_PREDICATE}
         ORDER BY s.qualname, f.path, s.line_start",
    ))?;
    let rows = stmt.query_map(rusqlite::params![name], candidate_row)?;
    let mut all: Vec<_> = rows.collect::<std::result::Result<_, _>>()?;
    if all.is_empty() && name.contains('.') {
        all = suffix_candidates(conn, name)?;
    }
    Ok(disambiguate(all))
}

/// `src/main.rs:11 src.main` -> "src.main". A plain name (no space) is not a
/// pointer.
fn pointer_qualname(name: &str) -> Option<&str> {
    let (locator, rest) = name.split_once(' ')?;
    let (_, line) = locator.rsplit_once(':')?;
    line.trim().parse::<u64>().ok()?;
    let qualname = rest.trim();
    (!qualname.is_empty()).then_some(qualname)
}

/// `src/main.rs:11` -> the ids of the symbols spanning that line. The locator
/// must look like a path (contains '/'); a bare `name:line` is never a
/// pointer. `None` falls through to normal name resolution so the original
/// error text survives.
fn pointer_span_ids(conn: &Connection, name: &str) -> Result<Option<Vec<i64>>> {
    let Some((path, line_text)) = name.rsplit_once(':') else {
        return Ok(None);
    };
    let Ok(line) = line_text.trim().parse::<i64>() else {
        return Ok(None);
    };
    if !path.contains('/') {
        return Ok(None);
    }
    let mut stmt = conn.prepare(
        "SELECT id FROM symbols
         WHERE file_path = ?1 AND line_start <= ?2 AND line_end >= ?2
         ORDER BY line_start, qualname",
    )?;
    let ids = stmt
        .query_map(rusqlite::params![path, line], |r| r.get::<_, i64>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok((!ids.is_empty()).then_some(ids))
}

/// `Class.method` suffix form: the full qualname is `pkg.Class.method`, so
/// an exact name/qualname match misses it. Fall back to a suffix scan and
/// confirm the exact suffix in Rust (LIKE alone treats `_` and `%` as
/// wildcards). Multiple matches stay ambiguous through the caller.
fn suffix_candidates(conn: &Connection, name: &str) -> Result<Vec<CandidateRow>> {
    let pattern = format!("%.{name}");
    let mut stmt = conn.prepare(&format!(
        "{CANDIDATE_SELECT}
         WHERE s.qualname LIKE ?1
         ORDER BY s.qualname, f.path, s.line_start",
    ))?;
    let rows = stmt.query_map([&pattern], candidate_row)?;
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
