//! Call-graph queries: imports, callers, callees, path, context.

use super::*;
use crate::db::EDGE_SRC_FILE_JOIN;

mod context;
mod frame;
mod graph;
mod path;

pub use self::context::context;
pub use self::graph::{callees, callers, GraphQueryOpts};
pub use self::path::path;

pub fn imports(ctx: &QueryCtx<'_>, file: &str, limit: usize) -> Result<QueryOutput> {
    let QueryCtx { conn, .. } = *ctx;
    let limit = clamp_limit(limit)?;
    let rel = indexed_file(conn, file)?;
    let total: usize = conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM edges e
         {EDGE_SRC_FILE_JOIN}
         WHERE f.path = ?1 AND e.kind = '{}'",
            EdgeKind::Imports.as_str()
        ),
        [&rel],
        |r| r.get::<_, i64>(0),
    )? as usize;
    if total == 0 {
        return Err(EgError::NoResultsCaveated {
            caveat: IMPORTS_CAVEAT,
            detail: "0 imports found (external and unresolved imports are omitted)".to_string(),
        });
    }
    let sql = format!(
        "SELECT e.site_line, e.dst_name FROM edges e
         {EDGE_SRC_FILE_JOIN}
         WHERE f.path = ?1 AND e.kind = '{}'
         ORDER BY e.site_line, e.dst_name LIMIT ?2",
        EdgeKind::Imports.as_str()
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params![rel, limit as i64], |r| {
        Ok(Pointer {
            file: rel.clone(),
            line: r.get(0)?,
            tag: "",
            detail: r.get(1)?,
        })
    })?;
    let mut out = collect_output(ctx, rows, limit, total)?;
    prepend_header(&mut out, IMPORTS_CAVEAT);
    Ok(out)
}

/// Resolve a query name to target ids, failing on a missing or ambiguous
/// name. `role` labels the candidate list for the two `path` endpoints.
fn resolve_targets(
    conn: &Connection,
    name: &str,
    role: Option<&'static str>,
) -> Result<(Vec<i64>, Vec<NameCandidate>)> {
    let (ids, cands) = target_ids(conn, name)?;
    if ids.is_empty() {
        return Err(EgError::NoResults(symbol_not_found_msg(name)));
    }
    if cands.len() > 1 {
        // CUJ4: no merged-silent result; name the Class.method form. The
        // candidate list travels IN the error so non-CLI surfaces render it.
        return Err(EgError::Ambiguous {
            name: name.to_string(),
            total: ids.len(),
            candidates: cands,
            role,
        });
    }
    Ok((ids, cands))
}

fn symbol_not_found_msg(name: &str) -> String {
    let mut msg = format!("symbol '{name}' not in index");
    let trimmed = name.trim();
    if trimmed.contains('/')
        || [".py", ".rs", ".ts", ".tsx", ".js", ".jsx"]
            .iter()
            .any(|ext| trimmed.ends_with(ext))
    {
        msg.push_str(" (hint: for file symbols use 'eg symbols <path>')");
    } else if trimmed.contains(' ') {
        msg.push_str(" (hint: for keyword search use 'eg search \"<terms>\"')");
    }
    msg
}

#[cfg(test)]
mod tests {
    use super::symbol_not_found_msg;

    #[test]
    fn symbol_not_found_emits_boundary_hints() {
        assert!(symbol_not_found_msg("src/main.rs")
            .contains("hint: for file symbols use 'eg symbols <path>'"));
        assert!(symbol_not_found_msg("app.py")
            .contains("hint: for file symbols use 'eg symbols <path>'"));
        assert!(symbol_not_found_msg("parse json")
            .contains("hint: for keyword search use 'eg search \"<terms>\"'"));
        assert_eq!(
            symbol_not_found_msg("missing_sym"),
            "symbol 'missing_sym' not in index"
        );
    }
}
